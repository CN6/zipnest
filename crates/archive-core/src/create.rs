//! Archive creation driver (M3).
//!
//! Task 1 implemented the `Store` + `Copy` subset (no password, no volumes, no
//! SFX) for ZIP and 7Z. Task 2 wires `ISetProperties` so the requested
//! compression level (`x`) and method (`m`) reach the handler. Task 4 adds the
//! TAR family: plain TAR writes directly through `CLSID_FORMAT_TAR`, while
//! `TarGz`/`TarBz2`/`TarXz` first write a temporary `.tar` and then wrap it with
//! the GZIP/BZIP2/XZ handler. Task 5 adds AES-256 password creation: the
//! password is handed to the engine through `ICryptoGetTextPassword2` on the
//! update callback, and ZIP additionally forces AES-256 via the `em` property.
//! The guard below still rejects the not-yet-wired options (volumes/SFX); the
//! callback/stream plumbing already carries the fields they need.

use crate::com::callbacks;
use crate::com::outcallback::{self, SourceItem, UpdateState};
use crate::com::outstream;
use crate::com::propvariant::PropVariant;
use crate::com::vtables::{OutArchiveVt, SetPropertiesVt};
use crate::com::{
    Guid, S_OK, CLSID_FORMAT_7Z, CLSID_FORMAT_BZIP2, CLSID_FORMAT_GZIP, CLSID_FORMAT_TAR,
    CLSID_FORMAT_XZ, CLSID_FORMAT_ZIP, E_NOTIMPL, IID_IOUT_ARCHIVE, IID_ISET_PROPERTIES,
};
use crate::dll;
use crate::error::ZipnestError;
use crate::types::{CompressionMethod, CreateFormat, CreateOptions, CreateProgress, CreateStats};
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::types::CreateSource;

/// Expand `inputs` into a flat item table, walking directories recursively so a
/// directory tree can be added while preserving its structure.
///
/// `node` is each item's path relative to `base`, always `/`-separated.
/// Directory entries are emitted too (so empty directories stay visible).
/// Symbolic links — files or directories — are skipped rather than followed.
pub fn collect_sources(
    inputs: &[PathBuf],
    base: &Path,
) -> Result<Vec<CreateSource>, ZipnestError> {
    let mut out = Vec::new();
    for input in inputs {
        collect_one(input, base, &mut out)?;
    }
    Ok(out)
}

/// The safe archive node for `path`, relative to `base`.
///
/// Returns `Ok(None)` when `path` *is* `base` (no name of its own), and an
/// error when `path` is not lexically under `base` or the relative name is
/// unsafe (absolute/traversal/reserved). Callers must never write an
/// unsanitized name into the archive (zip-slip).
fn rel_node(path: &Path, base: &Path) -> Result<Option<String>, ZipnestError> {
    let rel = path.strip_prefix(base).map_err(|_| {
        ZipnestError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "source {} is not under base {}",
                path.display(),
                base.display()
            ),
        ))
    })?;
    if rel.as_os_str().is_empty() {
        return Ok(None);
    }
    let raw = rel.to_string_lossy().replace('\\', "/");
    Ok(Some(archive_security::sanitize_entry_path(&raw)?))
}

fn collect_one(
    path: &Path,
    base: &Path,
    out: &mut Vec<CreateSource>,
) -> Result<(), ZipnestError> {
    let meta = std::fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        if let Some(node) = rel_node(path, base)? {
            out.push(CreateSource {
                path: path.to_path_buf(),
                node,
            });
        }
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(path)?
            .collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            collect_one(&e.path(), base, out)?;
        }
    } else {
        let node = rel_node(path, base)?.ok_or(ZipnestError::Security(
            archive_security::SecurityViolation::Empty,
        ))?;
        out.push(CreateSource {
            path: path.to_path_buf(),
            node,
        });
    }
    Ok(())
}

/// Handler CLSID for the single-container formats written in one pass.
fn clsid_for(format: CreateFormat) -> Option<Guid> {
    match format {
        CreateFormat::Zip => Some(CLSID_FORMAT_ZIP),
        CreateFormat::SevenZ => Some(CLSID_FORMAT_7Z),
        CreateFormat::Tar => Some(CLSID_FORMAT_TAR),
        CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz => None,
    }
}

/// Wrapper handler CLSID for the compressed TAR variants.
fn wrapper_clsid(format: CreateFormat) -> Option<Guid> {
    match format {
        CreateFormat::TarGz => Some(CLSID_FORMAT_GZIP),
        CreateFormat::TarBz2 => Some(CLSID_FORMAT_BZIP2),
        CreateFormat::TarXz => Some(CLSID_FORMAT_XZ),
        _ => None,
    }
}

/// Deletes a temporary file when dropped, so cleanup covers both the success
/// and the error path of a two-pass create.
struct TempCleanup(PathBuf);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A unique scratch path for the intermediate `.tar`, placed beside the
/// destination: that directory is known writable (the destination lives there),
/// which keeps the write on one volume and lets tests observe cleanup.
fn temp_tar_path(dest: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    dest.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{name}.zipnest-{}-{nanos}-{seq}.tar", std::process::id()))
}

/// Inner node name for a compressed TAR: the destination filename with the
/// compression suffix removed (`out.tar.gz` -> `out.tar`). This is the name the
/// single compressed stream carries, so clients show `out.tar` rather than the
/// scratch filename.
fn inner_tar_node(dest: &Path) -> Result<String, ZipnestError> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().replace('\\', "/"))
        .ok_or(ZipnestError::Security(
            archive_security::SecurityViolation::Empty,
        ))?;
    let stripped = name
        .strip_suffix(".gz")
        .or_else(|| name.strip_suffix(".bz2"))
        .or_else(|| name.strip_suffix(".xz"))
        .unwrap_or(&name);
    Ok(archive_security::sanitize_entry_path(stripped)?)
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

/// Best-effort archive-level properties via `ISetProperties`. Handlers that
/// don't implement the interface are simply left at their defaults.
unsafe fn apply_properties(raw: *mut c_void, opts: &CreateOptions) {
    let mut sp: *mut c_void = std::ptr::null_mut();
    let oav = &**(raw as *const *const OutArchiveVt);
    if (oav.query_interface)(raw, &IID_ISET_PROPERTIES, &mut sp) != S_OK || sp.is_null() {
        return;
    }
    // Wide names must outlive the call; keep the backing Vec<u16> alive.
    let mut names_keep: Vec<Vec<u16>> = Vec::new();
    let mut names: Vec<*const u16> = Vec::new();
    let mut values: Vec<PropVariant> = Vec::new();

    fn add(
        name: &str,
        pv: PropVariant,
        keep: &mut Vec<Vec<u16>>,
        names: &mut Vec<*const u16>,
        values: &mut Vec<PropVariant>,
    ) {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        keep.push(w);
        names.push(keep.last().unwrap().as_ptr());
        values.push(pv);
    }

    add(
        "x",
        PropVariant::from_u32(opts.level.engine_x()),
        &mut names_keep,
        &mut names,
        &mut values,
    );
    if let Some(m) = opts.method.engine_name() {
        add(
            "m",
            PropVariant::from_bstr(m),
            &mut names_keep,
            &mut names,
            &mut values,
        );
    }
    if opts.password.is_some() && opts.format == CreateFormat::Zip {
        // ZipHandlerOut picks AES only when the `em` property says so; with a
        // password but no `em` the default is legacy ZipCrypto. "AES256" maps
        // to AES-256 key mode 3. (7z selects AES-256 automatically once the
        // password callback reports a password.)
        add(
            "em",
            PropVariant::from_bstr("AES256"),
            &mut names_keep,
            &mut names,
            &mut values,
        );
    }

    let sp_vt = &**(sp as *const *const SetPropertiesVt);
    (sp_vt.set_properties)(sp, names.as_ptr(), values.as_ptr(), names.len() as u32);
    (sp_vt.release)(sp);
    for v in values.iter_mut() {
        v.clear(); // release the BSTR payloads
    }
}

/// Create an archive at `dest` from `sources`, reporting progress through
/// `progress` (return `false` to cancel).
///
/// `opts` carries the full creation surface; the compression level and method
/// are honored via `ISetProperties`. A password encrypts the payload: ZIP via
/// AES-256 (`em=AES256` plus the `ICryptoGetTextPassword2` callback), 7Z via
/// its default AES-256. Plain TAR writes in one pass; the compressed TAR
/// variants write a temporary `.tar` and wrap it. A password on the TAR family
/// is rejected with [`ZipnestError::PasswordUnsupported`]; options not yet
/// wired (volumes, SFX) return `E_NOTIMPL`.
pub fn create_archive(
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    if opts.volume_bytes.is_some() || opts.sfx.is_some() {
        return Err(ZipnestError::Engine(E_NOTIMPL));
    }
    // The TAR family has no encryption; reject a password up front rather than
    // hand back an unencrypted archive the caller believes is protected.
    if opts.password.is_some()
        && matches!(
            opts.format,
            CreateFormat::Tar | CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz
        )
    {
        return Err(ZipnestError::PasswordUnsupported);
    }
    if matches!(
        opts.format,
        CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz
    ) {
        return create_compressed_tar(sources, dest, opts, progress);
    }
    let clsid = clsid_for(opts.format).ok_or(ZipnestError::Engine(E_NOTIMPL))?;
    write_archive(clsid, sources, dest, opts, progress)
}

/// Two-pass create for `TarGz`/`TarBz2`/`TarXz`: build a temporary `.tar`, then
/// wrap it with the matching compressor. The temp file is removed on both the
/// success and the error path via [`TempCleanup`].
fn create_compressed_tar(
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    let wrapper = wrapper_clsid(opts.format).ok_or(ZipnestError::Engine(E_NOTIMPL))?;
    let tar_path = temp_tar_path(dest);
    let _cleanup = TempCleanup(tar_path.clone());

    let inner_stats = write_archive(CLSID_FORMAT_TAR, sources, &tar_path, opts, progress)?;

    let inner = [CreateSource {
        path: tar_path,
        node: inner_tar_node(dest)?,
    }];
    // The compressors ignore `m`/solid but honor `x`; dropping `m` avoids an
    // accidental "Copy disables compression" interpretation.
    let mut wrapper_opts = opts.clone();
    wrapper_opts.method = CompressionMethod::Auto;
    let outer_stats = write_archive(wrapper, &inner, dest, &wrapper_opts, progress)?;

    Ok(CreateStats {
        files: inner_stats.files,
        bytes_in: inner_stats.bytes_in,
        bytes_out: outer_stats.bytes_out,
    })
}

/// Run one `IOutArchive::UpdateItems` pass for `clsid`, writing all `sources`
/// into the archive at `dest`.
#[allow(clippy::arc_with_non_send_sync)] // ProgressCell holds a stack-bound closure cell
fn write_archive(
    clsid: Guid,
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
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

    // Apply archive-level properties (compression level/method) before items.
    unsafe { apply_properties(raw, opts) };

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
