//! Archive creation driver (M3).
//!
//! Task 1 implements the `Store` + `Copy` subset (no password, no volumes, no
//! SFX) for ZIP and 7Z. The guard below rejects the not-yet-wired options so
//! later tasks can fill them in; the callback/stream plumbing already carries
//! the fields they need.

use crate::com::callbacks;
use crate::com::outcallback::{self, SourceItem, UpdateState};
use crate::com::outstream;
use crate::com::vtables::OutArchiveVt;
use crate::com::{Guid, S_OK, CLSID_FORMAT_7Z, CLSID_FORMAT_ZIP, E_NOTIMPL, IID_IOUT_ARCHIVE};
use crate::dll;
use crate::error::ZipnestError;
use crate::types::{
    CompressionLevel, CompressionMethod, CreateFormat, CreateOptions, CreateProgress, CreateStats,
};
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::types::CreateSource;

/// Handler CLSID for a supported container, or `None` when creation is not
/// implemented yet (TAR family — later task).
fn clsid_for(format: CreateFormat) -> Option<Guid> {
    match format {
        CreateFormat::Zip => Some(CLSID_FORMAT_ZIP),
        CreateFormat::SevenZ => Some(CLSID_FORMAT_7Z),
        CreateFormat::Tar
        | CreateFormat::TarGz
        | CreateFormat::TarBz2
        | CreateFormat::TarXz => None,
    }
}

/// Stat every source and build the item table; also returns the pre-scanned
/// total of file bytes.
fn stat_sources(sources: &[CreateSource]) -> Result<(Vec<SourceItem>, u64), ZipnestError> {
    let mut items = Vec::with_capacity(sources.len());
    let mut total_bytes = 0u64;
    for s in sources {
        let meta = std::fs::metadata(&s.path)?;
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        if !is_dir {
            total_bytes += size;
        }
        items.push(SourceItem {
            path: s.path.clone(),
            node: s.node.clone(),
            is_dir,
            size,
            mtime: meta.modified().ok(),
        });
    }
    Ok((items, total_bytes))
}

/// Create an archive at `dest` from `sources`, reporting progress through
/// `progress` (return `false` to cancel).
///
/// `opts` carries the full creation surface; Task 1 honors only
/// `Store`/`Copy` with no password/volumes/SFX and returns `E_NOTIMPL`
/// otherwise.
#[allow(clippy::arc_with_non_send_sync)] // ProgressCell holds a stack-bound closure cell
pub fn create_archive(
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    if opts.level != CompressionLevel::Store
        || opts.method != CompressionMethod::Copy
        || opts.password.is_some()
        || opts.volume_bytes.is_some()
        || opts.sfx.is_some()
    {
        return Err(ZipnestError::Engine(E_NOTIMPL));
    }
    let clsid = clsid_for(opts.format).ok_or(ZipnestError::Engine(E_NOTIMPL))?;

    let (items, total_bytes) = stat_sources(sources)?;
    let files = items.iter().filter(|i| !i.is_dir).count() as u32;
    let num_items = items.len() as u32;

    let dll = dll::load()?;
    // Open the output stream first: if this fails, no handler ref exists yet.
    let out = outstream::new(dest)?;

    let mut raw: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { dll.create_object(&clsid, &IID_IOUT_ARCHIVE, &mut raw) };
    if hr != S_OK || raw.is_null() {
        unsafe { outstream::release_void(out) };
        return Err(crate::error::map_hresult(hr));
    }

    let state = Arc::new(UpdateState {
        done_bytes: AtomicU64::new(0),
        total_bytes: AtomicU64::new(total_bytes),
        cancelled: AtomicU8::new(0),
        last_path: Mutex::new(String::new()),
        progress: Mutex::new(outcallback::progress_cell(progress)),
        io_error: Mutex::new(None),
    });
    let crypto = callbacks::crypto_new(opts.password.clone());
    let cb = outcallback::new(items, Arc::clone(&state), crypto as *mut c_void);

    let hr = unsafe {
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.update_items)(raw, out, num_items, cb)
    };

    // Drop our references: the callback, the output stream, and the handler.
    unsafe {
        outcallback::release_void(cb);
        outstream::release_void(out);
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.release)(raw);
        // Free the boxed closure cell; no callbacks can fire after UpdateItems.
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

    // Post-check order: local flags win over the handler's HRESULT (a cancel
    // we raised must report its real cause).
    if state.cancelled.load(Ordering::SeqCst) == 1 {
        return Err(ZipnestError::Cancelled);
    }
    if let Some(e) = state.io_error.lock().ok().and_then(|mut s| s.take()) {
        return Err(ZipnestError::Io(e));
    }
    if hr != S_OK {
        return Err(crate::error::map_hresult(hr));
    }

    let bytes_out = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);

    Ok(CreateStats {
        files,
        bytes_in: total_bytes,
        bytes_out,
    })
}
