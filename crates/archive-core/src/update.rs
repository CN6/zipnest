//! Editing an existing ZIP / 7z in place: add files, drop entries.
//!
//! The mechanism is 7-Zip's own: open the old archive as an `IInArchive`, build
//! the new item list (kept entries by archive index, new entries by path), and
//! hand it to `IOutArchive::UpdateItems`. The handler reads kept items from the
//! archive's decoder and new items from us, and writes a fresh archive.
//!
//! Safety rules this module keeps (AGENTS.md, "Nothing is ever lost"):
//!
//! * the output goes to a temp sibling and only replaces the original after
//!   `UpdateItems` reported success, so a cancel, a disk-full or a bad source
//!   file leaves the original byte-for-byte intact;
//! * the old archive's password is reused (the UI re-prompts when it does not
//!   open), so an encrypted archive stays encrypted;
//! * only ZIP and 7z are editable. RAR is extract-only by licence and the
//!   single-stream formats (gz, bz2, xz) have no index to edit — the caller
//!   refuses them with `error.update_unsupported` instead of attempting a
//!   rewrite that would silently change their format.

use crate::archive::Archive;
use crate::com::callbacks;
use crate::com::outstream;
use crate::com::updatecallback::{self, UpdateItem, UpdateState};
use crate::com::vtables::OutArchiveVt;
use crate::com::{IID_IOUT_ARCHIVE, S_OK};
use crate::create::{apply_properties, os_path, temp_part_path, TempCleanup};
use crate::error::ZipnestError;
use crate::types::{CreateFormat, CreateOptions, CreateProgress, CreateSource, ExtractedEditStats};
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What to do with the open archive.
#[derive(Debug, Clone, Default)]
pub struct UpdatePlan {
    /// Archive indices (the old archive's own numbering) to drop.
    pub remove: Vec<u32>,
    /// Files or folders to add, each with the node name it takes inside the
    /// archive (relative, `/`-separated, already sanitized by the caller).
    pub add: Vec<CreateSource>,
}

impl UpdatePlan {
    /// Does this plan change anything? A no-op update would rewrite the whole
    /// archive for nothing — worse, it would touch the user's file — so the
    /// caller refuses it with `error.update_empty`.
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.add.is_empty()
    }
}

/// The two editable containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFormat {
    Zip,
    SevenZ,
}

impl UpdateFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateFormat::Zip => "zip",
            UpdateFormat::SevenZ => "7z",
        }
    }
}

/// Options for [`update_archive`].
#[derive(Clone)]
pub struct UpdateOptions {
    pub format: UpdateFormat,
    /// Compression level and method for the *new* archive. Kept items are
    /// recompressed by the handler at these settings — an "append" to a 7z
    /// solid archive rewrites the whole thing, so the level applies to all of
    /// it. `None` keeps the handler's defaults (normal / auto).
    pub level: Option<crate::types::CompressionLevel>,
    pub method: Option<crate::types::CompressionMethod>,
    /// The archive's password, when it has one. Reused so the rewritten file
    /// stays encrypted; never logged.
    pub password: Option<String>,
}

impl std::fmt::Debug for UpdateOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateOptions")
            .field("format", &self.format)
            .field("level", &self.level)
            .field("method", &self.method)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Rewrite `archive`'s source file with `plan` applied.
///
/// `archive` must be the open archive whose registry path is `dest`. The
/// caller holds the archive's mutex for the whole call (a job worker does
/// exactly that), which is also what makes it safe to read the old file while
/// writing a new one next to it.
#[allow(clippy::arc_with_non_send_sync)] // UpdateState holds a stack-bound closure cell
pub fn update_archive(
    archive: &Archive,
    dest: &Path,
    plan: &UpdatePlan,
    opts: &UpdateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<ExtractedEditStats, ZipnestError> {
    if plan.is_empty() {
        return Err(ZipnestError::Engine(crate::com::E_INVALIDARG));
    }

    // The old listing, in archive order: kept entries go into the new list in
    // this order, so the layout of an untouched archive does not shuffle.
    let old = archive.entries()?;
    let removed: std::collections::HashSet<u32> = plan.remove.iter().copied().collect();
    let mut items: Vec<UpdateItem> = Vec::new();
    let mut kept = 0u32;
    for e in &old {
        if removed.contains(&e.index) {
            continue;
        }
        items.push(UpdateItem::Keep { index: e.index });
        kept += 1;
    }

    // Expand the adds (directories recurse, links are reported by the caller's
    // collector) into concrete filesystem entries.
    let mut total_bytes: u64 = archive
        .entries()?
        .iter()
        .filter(|e| !e.is_dir && !removed.contains(&e.index))
        .map(|e| e.size)
        .sum();
    for src in &plan.add {
        let part = crate::create::collect_sources_detailed(std::slice::from_ref(&src.path), Path::new(""))
            .or_else(|_| {
                // A bare path with no parent: collect it relative to itself.
                let parent = src.path.parent().unwrap_or(Path::new("."));
                crate::create::collect_sources_detailed(std::slice::from_ref(&src.path), parent)
            })?;
        for item in part.sources {
            let meta = std::fs::metadata(&item.path)?;
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            if !is_dir {
                total_bytes += size;
            }
            items.push(UpdateItem::Add {
                path: item.path,
                node: item.node,
                is_dir,
                size,
                mtime: meta.modified().ok(),
            });
        }
    }
    if items.is_empty() {
        // Every entry dropped and nothing added: an archive with no items at
        // all is not something the handlers write coherently, and it is almost
        // certainly a mistake in the caller.
        return Err(ZipnestError::Engine(crate::com::E_INVALIDARG));
    }

    // Write to a temp sibling; the original is replaced only on success.
    let tmp = temp_part_path(dest);
    let _guard = TempCleanup(tmp.clone());
    let out = outstream::new(&tmp)?;

    // The OUT interface must come from the *open* archive, never from a fresh
    // CreateObject: 7-Zip's handlers implement IInArchive and IOutArchive on
    // the same object, and UpdateItems reads the kept items through the
    // archive it was opened with. A newly created object knows nothing about
    // that file, and the first `indexInArchive` copy dereferences its null
    // open state — measured as STATUS_ACCESS_VIOLATION on the first run of
    // these tests.
    let mut raw: *mut c_void = std::ptr::null_mut();
    let hr = unsafe {
        let in_vt = &**(archive.raw() as *const *const crate::com::vtables::InArchiveVt);
        (in_vt.query_interface)(archive.raw(), &IID_IOUT_ARCHIVE, &mut raw)
    };
    if hr != S_OK || raw.is_null() {
        unsafe { outstream::release_void(out) };
        return Err(crate::error::map_hresult(hr));
    }

    // Only the properties the caller set; an unset level/method leaves the
    // handler's own defaults, which is what a plain "add these files" means.
    let prop_opts = CreateOptions {
        format: match opts.format {
            UpdateFormat::Zip => CreateFormat::Zip,
            UpdateFormat::SevenZ => CreateFormat::SevenZ,
        },
        level: opts.level.unwrap_or(crate::types::CompressionLevel::Normal),
        method: opts.method.unwrap_or(crate::types::CompressionMethod::Auto),
        password: opts.password.clone(),
        encrypt_names: false,
        volume_bytes: None,
        sfx: None,
    };
    unsafe { apply_properties(raw, &prop_opts) };

    let state = Arc::new(UpdateState {
        done_bytes: AtomicU64::new(0),
        total_bytes: AtomicU64::new(total_bytes),
        cancelled: AtomicU8::new(0),
        last_path: Mutex::new(String::new()),
        progress: Mutex::new(updatecallback::progress_cell(progress)),
        io_error: Mutex::new(None),
    });
    let crypto = callbacks::crypto_new(opts.password.clone());
    let cb = updatecallback::new(items.clone(), Arc::clone(&state), crypto as *mut c_void);

    let num_items = items.len() as u32;
    let hr = unsafe {
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.update_items)(raw, out, num_items, cb)
    };

    let cancelled = state.cancelled.load(Ordering::SeqCst) == 1;
    let io_error = state.io_error.lock().ok().and_then(|mut s| s.take());
    let ok = hr == S_OK && !cancelled && io_error.is_none();

    unsafe {
        outstream::release_void(out);
        updatecallback::release_void(cb);
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.release)(raw);
        let cell = state
            .progress
            .lock()
            .map(|c| c.cell)
            .unwrap_or(std::ptr::null_mut());
        if !cell.is_null() {
            drop(Box::from_raw(
                cell as *mut &mut dyn FnMut(&CreateProgress) -> bool,
            ));
        }
    }

    if cancelled {
        return Err(ZipnestError::Cancelled);
    }
    if let Some(e) = io_error {
        return Err(ZipnestError::Io(e));
    }
    if !ok {
        return Err(crate::error::map_hresult(hr));
    }

    // Commit: the finished archive replaces the original. A failed rename
    // leaves the original untouched and the temp is cleaned by the guard.
    let bytes_out = std::fs::metadata(os_path(&tmp))
        .map(|m| m.len())
        .unwrap_or(0);
    std::fs::rename(os_path(&tmp), os_path(dest)).map_err(ZipnestError::Io)?;

    let added = (items.len() as u32).saturating_sub(kept);
    Ok(ExtractedEditStats {
        kept,
        added,
        removed: plan.remove.len() as u32,
        bytes_out,
    })
}

/// Translate a container name into the editable format, or `None` for the
/// formats this feature does not touch (RAR, TAR, the single streams, …).
pub fn update_format_for(container: &str) -> Option<UpdateFormat> {
    match container.to_ascii_lowercase().as_str() {
        "zip" => Some(UpdateFormat::Zip),
        "7z" | "7zip" => Some(UpdateFormat::SevenZ),
        _ => None,
    }
}

/// An empty `UpdatePlan` helper for callers that build one incrementally.
pub fn plan_with_removals(indices: &[u32]) -> UpdatePlan {
    UpdatePlan {
        remove: indices.to_vec(),
        add: Vec::new(),
    }
}

