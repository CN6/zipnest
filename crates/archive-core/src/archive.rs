//! `Archive`: open/list entries through the 7z.dll `IInArchive` COM object.

use crate::com::callbacks::OpenCallbackOwner;
use crate::com::instream::{self, FileStreamOwner};
use crate::com::propvariant::{
    PropVariant, ERRFLAGS_ENCRYPTED_HEADERS_ERROR, ERRFLAGS_IS_NOT_ARC, KPID_CRC, KPID_ENCRYPTED,
    KPID_ERROR_FLAGS, KPID_IS_DIR, KPID_MTIME, KPID_PATH, KPID_SIZE, VT_BOOL, VT_UI4, VT_UI8,
};
use crate::com::vtables::{InArchiveGetStreamVt, InArchiveVt, InStreamVt};
use crate::com::{
    CLSID_FORMAT_7Z, CLSID_FORMAT_BZIP2, CLSID_FORMAT_GZIP, CLSID_FORMAT_ISO, CLSID_FORMAT_RAR,
    CLSID_FORMAT_RAR5, CLSID_FORMAT_SPLIT, CLSID_FORMAT_TAR, CLSID_FORMAT_XZ, CLSID_FORMAT_ZIP,
    Guid, IID_IIN_ARCHIVE_GET_STREAM, S_OK,
};
use crate::dll;
use crate::error::{hr_failed, ZipnestError};
use crate::types::{ArchiveEntry, ArchiveOpenOptions, ExtractOptions, ExtractProgress, ExtractStats};
use std::ffi::c_void;
use std::path::Path;

/// Cap on the initial `Vec` capacity used by [`Archive::entries`]: the item
/// count comes from the archive header and must never drive an allocation
/// proportional to a hostile number.
const MAX_PREALLOC_ENTRIES: u32 = 4096;

pub struct Archive {
    raw: *mut c_void,   // IInArchive (owned)
    stream: *mut c_void, // IInStream retained by the handler (owned)
    _not_send: *mut (),  // keep !Send/!Sync: IInArchive is not thread-safe
}

/// Strip a numeric multi-volume suffix (`.001`, `.0001`, ...). 7-Zip's
/// `Split` handler needs at least two digits, so we require the same.
fn strip_volume_suffix(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    let suffix = &name[dot + 1..];
    if suffix.len() >= 2 && suffix.bytes().all(|b| b.is_ascii_digit()) {
        Some(name[..dot].to_string())
    } else {
        None
    }
}

/// Path of the first volume's logical archive: `<base>.7z.001` → `<base>.7z`.
/// `None` when `path` is not a numbered volume.
fn volume_base(path: &Path) -> Option<std::path::PathBuf> {
    let name = path.file_name()?.to_str()?;
    let stem = strip_volume_suffix(name)?;
    if stem.is_empty() || !stem.contains('.') {
        return None;
    }
    Some(path.with_file_name(stem))
}

/// Candidate handler CLSIDs for a file path (first success wins). A trailing
/// volume suffix is ignored so `<base>.7z.001` selects the 7z handler.
fn clsid_candidates(path: &Path) -> Vec<Guid> {
    let name = path
        .file_name()
        .map(|n| {
            let lower = n.to_string_lossy().to_lowercase();
            strip_volume_suffix(&lower).unwrap_or(lower)
        })
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".gz") {
        return vec![CLSID_FORMAT_GZIP];
    }
    if name.ends_with(".7z") {
        return vec![CLSID_FORMAT_7Z];
    }
    if name.ends_with(".rar") {
        // RAR5 handler first; older RARs fall back to the legacy handler.
        return vec![CLSID_FORMAT_RAR5, CLSID_FORMAT_RAR];
    }
    if name.ends_with(".tar") {
        return vec![CLSID_FORMAT_TAR];
    }
    if name.ends_with(".zip") {
        return vec![CLSID_FORMAT_ZIP];
    }
    if name.ends_with(".bz2") {
        return vec![CLSID_FORMAT_BZIP2];
    }
    if name.ends_with(".xz") {
        return vec![CLSID_FORMAT_XZ];
    }
    if name.ends_with(".iso") {
        return vec![CLSID_FORMAT_ISO];
    }
    Vec::new()
}

impl Archive {
    /// Open an archive (decode headers; may prompt for a password via
    /// `opts.password`).
    pub fn open(path: &Path, opts: ArchiveOpenOptions) -> Result<Archive, ZipnestError> {
        // A numbered volume (`<base>.7z.001`) must first be assembled by the
        // `Split` handler, then re-opened with the real format handler.
        if volume_base(path).is_some() && !clsid_candidates(path).is_empty() {
            return Self::open_volumes(path, opts);
        }

        let dll = dll::load()?;
        let candidates = clsid_candidates(path);
        if candidates.is_empty() {
            return Err(ZipnestError::NotAnArchive);
        }
        let file = std::fs::File::open(path)?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut last_err = ZipnestError::NotAnArchive;
        for clsid in &candidates {
            let stream = FileStreamOwner::new(file.try_clone()?)?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            let hr = unsafe {
                dll.create_object(
                    clsid,
                    &crate::com::IID_IIN_ARCHIVE,
                    &mut raw,
                )
            };
            if hr_failed(hr) || raw.is_null() {
                unsafe { stream.release_own() };
                last_err = crate::error::map_hresult(hr);
                continue;
            }

            let cb = OpenCallbackOwner::new(
                opts.password.clone(),
                dir.clone(),
                file_name.clone(),
                true,
            );
            let state = std::sync::Arc::clone(cb.state());
            let hr = unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.open)(raw, stream.as_void(), std::ptr::null(), cb.as_void())
            };
            unsafe { cb.release_own() };

            // `Open` is the one method where S_FALSE is *not* a soft success:
            // 7-Zip's handlers return it for "this is not my format" (cf.
            // `OpenForSize`: "S_FALSE - is not archive", IArchive.h), which is
            // what makes the RAR5 -> legacy-RAR fallback work. Accepting
            // S_FALSE here would return an Archive for a corrupt file.
            if hr == S_OK {
                return Ok(Archive {
                    raw,
                    stream: stream.into_raw(),
                    _not_send: std::ptr::null_mut(),
                });
            }
            // Failed: read the engine's error flags while the object is still
            // alive (they carry the real reason), then drop both objects,
            // remember the error, try next candidate.
            let error_flags = unsafe { archive_error_flags(raw) };
            unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.close)(raw);
                (vt.release)(raw);
                stream.release_own();
            }
            last_err = map_open_error(hr, &state, error_flags);
        }
        Err(last_err)
    }

    /// Open `<base>.7z.001` / `<base>.zip.001`: let the engine's `Split`
    /// handler read every volume through our `IArchiveOpenVolumeCallback`,
    /// take the concatenated stream it exposes via `IInArchiveGetStream`, and
    /// decode that stream with the real handler chosen from `<base>`.
    fn open_volumes(path: &Path, opts: ArchiveOpenOptions) -> Result<Archive, ZipnestError> {
        let dll = dll::load()?;
        let candidates = clsid_candidates(path);
        if candidates.is_empty() {
            return Err(ZipnestError::NotAnArchive);
        }
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        // --- Stage 1: assemble the volumes with the `Split` handler. ---
        let file = std::fs::File::open(path)?;
        let stream = FileStreamOwner::new(file)?;
        let mut split_raw: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            dll.create_object(&CLSID_FORMAT_SPLIT, &crate::com::IID_IIN_ARCHIVE, &mut split_raw)
        };
        if hr_failed(hr) || split_raw.is_null() {
            unsafe { stream.release_own() };
            return Err(crate::error::map_hresult(hr));
        }
        let cb = OpenCallbackOwner::new(opts.password.clone(), dir.clone(), file_name.clone(), true);
        let state = std::sync::Arc::clone(cb.state());
        let hr = unsafe {
            let vt = &**(split_raw as *const *const InArchiveVt);
            (vt.open)(split_raw, stream.as_void(), std::ptr::null(), cb.as_void())
        };
        // Same S_FALSE rule as above, so only read the flags on a failure.
        let error_flags = if hr == S_OK {
            0
        } else {
            unsafe { archive_error_flags(split_raw) }
        };
        unsafe {
            cb.release_own();
            stream.release_own();
        }
        let close_split = |raw: *mut c_void| unsafe {
            let vt = &**(raw as *const *const InArchiveVt);
            (vt.close)(raw);
            (vt.release)(raw);
        };
        if hr != S_OK {
            close_split(split_raw);
            // This is where a volume that exists but cannot be read (locked,
            // ACL-denied) surfaces: the callback recorded the real cause.
            return Err(map_open_error(hr, &state, error_flags));
        }

        // --- Stage 2: fetch the combined, seekable stream. ---
        let mut combined: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            let qi_vt = &**(split_raw as *const *const InArchiveVt);
            let mut gs_iface: *mut c_void = std::ptr::null_mut();
            let qhr =
                (qi_vt.query_interface)(split_raw, &IID_IIN_ARCHIVE_GET_STREAM, &mut gs_iface);
            // QueryInterface must answer S_OK or an error (`S_FALSE` is not a
            // success for it), so the strict test is deliberate here.
            if qhr != S_OK || gs_iface.is_null() {
                (qi_vt.close)(split_raw);
                (qi_vt.release)(split_raw);
                return Err(ZipnestError::NotAnArchive);
            }
            let gs_vt = &**(gs_iface as *const *const InArchiveGetStreamVt);
            let ghr = (gs_vt.get_stream)(gs_iface, 0, &mut combined);
            (gs_vt.release)(gs_iface);
            (qi_vt.close)(split_raw);
            (qi_vt.release)(split_raw);
            ghr
        };
        if hr_failed(hr) || combined.is_null() {
            return Err(ZipnestError::NotAnArchive);
        }

        // --- Stage 3: decode the combined stream with the real handler. ---
        let mut last_err = ZipnestError::NotAnArchive;
        for clsid in &candidates {
            // A failed attempt may have moved the shared stream; rewind.
            unsafe {
                let vt = &**(combined as *const *const InStreamVt);
                (vt.seek)(combined, 0, 0, std::ptr::null_mut());
            }
            let mut raw: *mut c_void = std::ptr::null_mut();
            let hr = unsafe { dll.create_object(clsid, &crate::com::IID_IIN_ARCHIVE, &mut raw) };
            if hr_failed(hr) || raw.is_null() {
                last_err = crate::error::map_hresult(hr);
                continue;
            }
            let cb = OpenCallbackOwner::new(
                opts.password.clone(),
                dir.clone(),
                file_name.clone(),
                false,
            );
            let state = std::sync::Arc::clone(cb.state());
            let hr = unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.open)(raw, combined, std::ptr::null(), cb.as_void())
            };
            unsafe { cb.release_own() };
            if hr == S_OK {
                return Ok(Archive {
                    raw,
                    stream: combined,
                    _not_send: std::ptr::null_mut(),
                });
            }
            let error_flags = unsafe { archive_error_flags(raw) };
            unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.close)(raw);
                (vt.release)(raw);
            }
            last_err = map_open_error(hr, &state, error_flags);
        }
        unsafe { instream::release_raw(combined) };
        Err(last_err)
    }

    /// Item count as reported by the handler, or `0` when the handler fails to
    /// report one. This is a display helper (`len`/`is_empty`/`Debug`); it has
    /// no error channel, so callers that need the count to be truthful must use
    /// [`Self::entries`], which re-reads it and surfaces the engine's error.
    pub fn len(&self) -> u32 {
        unsafe {
            let vt = &**(self.raw as *const *const InArchiveVt);
            let mut n: u32 = 0;
            if hr_failed((vt.get_number_of_items)(self.raw, &mut n)) {
                0
            } else {
                n
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Enumerate all entries with metadata (Task 6).
    ///
    /// A property read the engine reports as failed becomes an error: turning it
    /// into an empty path / `size = 0` / "no timestamp" would hand the caller a
    /// list that quietly lies about the archive.
    pub fn entries(&self) -> Result<Vec<ArchiveEntry>, ZipnestError> {
        let count = unsafe {
            let vt = &**(self.raw as *const *const InArchiveVt);
            let mut n: u32 = 0;
            let hr = (vt.get_number_of_items)(self.raw, &mut n);
            if hr_failed(hr) {
                return Err(crate::error::map_hresult(hr));
            }
            n
        };
        if count == 0 {
            // A handler with a weak signature check can report "opened" for a
            // foreign file; its own error flags decide. This mirrors what
            // 7-Zip's client does with `kpidErrorFlags` after `Open`.
            let flags = unsafe { archive_error_flags(self.raw) };
            if let Some(e) = error_flags_error(flags, false) {
                return Err(e);
            }
        }
        // The item count is attacker-controlled: a header claiming 4 billion
        // entries must not turn into a 4-billion-element allocation. Grow on
        // demand past the cap instead.
        let mut out = Vec::with_capacity(count.min(MAX_PREALLOC_ENTRIES) as usize);
        for index in 0..count {
            unsafe {
                let vt = &**(self.raw as *const *const InArchiveVt);
                // kpidPath
                let mut path_pv = read_property(vt, self.raw, index, KPID_PATH)?;
                let path = path_pv.take_bstr().unwrap_or_default();
                path_pv.clear();
                // kpidIsDir
                let dir_pv = read_property(vt, self.raw, index, KPID_IS_DIR)?;
                let is_dir = dir_pv.vt == VT_BOOL && dir_pv.as_bool();
                // kpidSize
                let size_pv = read_property(vt, self.raw, index, KPID_SIZE)?;
                let size = if size_pv.vt == VT_UI8 {
                    size_pv.as_u64()
                } else {
                    0
                };
                // kpidCRC
                let crc_pv = read_property(vt, self.raw, index, KPID_CRC)?;
                let crc = if crc_pv.vt == VT_UI4 {
                    Some(crc_pv.as_u64() as u32)
                } else {
                    None
                };
                // kpidMTime
                let mtime_pv = read_property(vt, self.raw, index, KPID_MTIME)?;
                let mtime = mtime_pv.as_system_time();
                // kpidEncrypted
                let enc_pv = read_property(vt, self.raw, index, KPID_ENCRYPTED)?;
                let encrypted = enc_pv.vt == VT_BOOL && enc_pv.as_bool();
                out.push(ArchiveEntry {
                    index,
                    path,
                    is_dir,
                    size,
                    crc,
                    mtime,
                    encrypted,
                });
            }
        }
        Ok(out)
    }

    /// Read one entry fully into memory (Task 7).
    pub fn read_entry(
        &self,
        index: u32,
        opts: &ArchiveOpenOptions,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>, ZipnestError> {
        crate::extract::read_entry(self, index, opts.password.clone(), max_bytes)
    }

    /// Extract entries to disk with security/progress/cancel (Task 8).
    pub fn extract(
        &self,
        opts: &ExtractOptions,
        password: Option<&str>,
        progress: &mut dyn FnMut(&ExtractProgress) -> bool,
    ) -> Result<ExtractStats, ZipnestError> {
        crate::extract::extract_to_disk(self, opts, password, progress)
    }

    pub(crate) fn raw(&self) -> *mut c_void {
        self.raw
    }
}

/// Read one `IInArchive::GetProperty` value, or the engine's error.
///
/// `hr < 0` is reported instead of being turned into a default value; `hr >= 0`
/// with `VT_EMPTY` is the engine's way of saying "that property is not set".
unsafe fn read_property(
    vt: &InArchiveVt,
    raw: *mut c_void,
    index: u32,
    prop_id: u32,
) -> Result<PropVariant, ZipnestError> {
    let mut pv = PropVariant::empty();
    let hr = (vt.get_property)(raw, index, prop_id, &mut pv);
    if hr_failed(hr) {
        pv.clear();
        return Err(crate::error::map_hresult(hr));
    }
    Ok(pv)
}

/// `IInArchive::GetArchiveProperty(kpidErrorFlags)` → its `VT_UI4` bits, or `0`
/// when the handler does not report them.
unsafe fn archive_error_flags(raw: *mut c_void) -> u32 {
    let vt = &**(raw as *const *const InArchiveVt);
    let mut pv = PropVariant::empty();
    let hr = (vt.get_archive_property)(raw, KPID_ERROR_FLAGS, &mut pv);
    let flags = if !hr_failed(hr) && pv.vt == VT_UI4 {
        pv.as_u64() as u32
    } else {
        0
    };
    pv.clear();
    flags
}

/// Map the hard bits of `kpidErrorFlags` onto the errors the UI knows.
///
/// Only the two unambiguous cases are mapped; the other bits (header warnings,
/// `UnexpectedEnd`, CRC, …) leave the archive usable, exactly as 7-Zip's own
/// client treats them after `Open`.
fn error_flags_error(flags: u32, password_supplied: bool) -> Option<ZipnestError> {
    if flags & ERRFLAGS_ENCRYPTED_HEADERS_ERROR != 0 {
        return Some(if password_supplied {
            ZipnestError::PasswordIncorrect
        } else {
            ZipnestError::PasswordRequired
        });
    }
    if flags & ERRFLAGS_IS_NOT_ARC != 0 {
        return Some(ZipnestError::NotAnArchive);
    }
    None
}

fn map_open_error(
    hr: i32,
    state: &crate::com::callbacks::OpenState,
    error_flags: u32,
) -> ZipnestError {
    // E_ABORT and friends go through the single mapping table first.
    if !matches!(crate::error::map_hresult(hr), ZipnestError::Engine(_)) {
        return crate::error::map_hresult(hr);
    }
    // A callback-level I/O failure (a volume that exists but could not be
    // opened) is the actionable cause: report it rather than "not an archive".
    let volume_io_error = match state.io_error.lock() {
        Ok(mut slot) => slot.take(),
        Err(p) => p.into_inner().take(),
    };
    if let Some(e) = volume_io_error {
        return ZipnestError::Io(e);
    }
    // The engine asked for a password during Open:
    //  - none supplied  -> password required
    //  - supplied but Open still failed -> it was rejected
    // (header-encrypted 7z asks before decoding anything; the ask flag is
    // the reliable discriminator across handlers.)
    let asked = state.asked.load(std::sync::atomic::Ordering::SeqCst) == 1;
    if asked {
        return if state.password.is_some() {
            ZipnestError::PasswordIncorrect
        } else {
            ZipnestError::PasswordRequired
        };
    }
    // Handlers that never ask still report encrypted headers through
    // `kpidErrorFlags`; without the flags that case is indistinguishable from
    // "not an archive".
    if let Some(e) = error_flags_error(error_flags, state.password.is_some()) {
        return e;
    }
    ZipnestError::NotAnArchive
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("items", &self.len())
            .finish()
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        unsafe {
            let vt = &**(self.raw as *const *const InArchiveVt);
            (vt.close)(self.raw);
            (vt.release)(self.raw);
            // Release the retained input stream last. Dispatch through the
            // object's own vtable: it may be ours (single file) or the engine's
            // `CMultiStream` (assembled volumes).
            instream::release_raw(self.stream);
        }
    }
}

