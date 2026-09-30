//! `Archive`: open/list entries through the 7z.dll `IInArchive` COM object.

use crate::com::callbacks::OpenCallbackOwner;
use crate::com::instream::FileStreamOwner;
use crate::com::propvariant::PropVariant;
use crate::com::vtables::InArchiveVt;
use crate::com::{
    CLSID_FORMAT_7Z, CLSID_FORMAT_BZIP2, CLSID_FORMAT_GZIP, CLSID_FORMAT_ISO, CLSID_FORMAT_RAR,
    CLSID_FORMAT_RAR5, CLSID_FORMAT_TAR, CLSID_FORMAT_XZ, CLSID_FORMAT_ZIP, Guid, S_OK,
};
use crate::dll;
use crate::error::ZipnestError;
use crate::types::{ArchiveEntry, ArchiveOpenOptions, ExtractOptions, ExtractProgress, ExtractStats};
use std::ffi::c_void;
use std::path::Path;

pub struct Archive {
    raw: *mut c_void,   // IInArchive (owned)
    stream: *mut c_void, // IInStream retained by the handler (owned)
    _not_send: *mut (),  // keep !Send/!Sync: IInArchive is not thread-safe
}

/// Candidate handler CLSIDs for a file path (first success wins).
fn clsid_candidates(path: &Path) -> Vec<Guid> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
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
        let dll = dll::load()?;
        let candidates = clsid_candidates(path);
        if candidates.is_empty() {
            return Err(ZipnestError::NotAnArchive);
        }
        let file = std::fs::File::open(path)?;

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
            if hr != S_OK || raw.is_null() {
                unsafe { stream.release_own() };
                last_err = crate::error::map_hresult(hr);
                continue;
            }

            let cb = OpenCallbackOwner::new(opts.password.clone());
            let state = std::sync::Arc::clone(cb.state());
            let hr = unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.open)(raw, stream.as_void(), std::ptr::null(), cb.as_void())
            };
            unsafe { cb.release_own() };

            if hr == S_OK {
                return Ok(Archive {
                    raw,
                    stream: stream.into_raw(),
                    _not_send: std::ptr::null_mut(),
                });
            }
            // Failed: drop both objects, remember error, try next candidate.
            unsafe {
                let vt = &**(raw as *const *const InArchiveVt);
                (vt.close)(raw);
                (vt.release)(raw);
                stream.release_own();
            }
            last_err = map_open_error(hr, &state);
        }
        Err(last_err)
    }

    pub fn len(&self) -> u32 {
        unsafe {
            let vt = &**(self.raw as *const *const InArchiveVt);
            let mut n: u32 = 0;
            if (vt.get_number_of_items)(self.raw, &mut n) != S_OK {
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
    pub fn entries(&self) -> Result<Vec<ArchiveEntry>, ZipnestError> {
        let count = self.len();
        let mut out = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut pv = PropVariant::empty();
            unsafe {
                let vt = &**(self.raw as *const *const InArchiveVt);
                // kpidPath
                (vt.get_property)(self.raw, index, crate::com::propvariant::KPID_PATH, &mut pv);
                let path = pv.take_bstr().unwrap_or_default();
                pv.clear();
                // kpidIsDir
                (vt.get_property)(self.raw, index, crate::com::propvariant::KPID_IS_DIR, &mut pv);
                let is_dir = pv.vt == crate::com::propvariant::VT_BOOL && pv.as_bool();
                pv.clear();
                // kpidSize
                (vt.get_property)(self.raw, index, crate::com::propvariant::KPID_SIZE, &mut pv);
                let size = if pv.vt == crate::com::propvariant::VT_UI8 { pv.as_u64() } else { 0 };
                pv.clear();
                // kpidCRC
                (vt.get_property)(self.raw, index, crate::com::propvariant::KPID_CRC, &mut pv);
                let crc = if pv.vt == crate::com::propvariant::VT_UI4 {
                    Some(pv.as_u64() as u32)
                } else {
                    None
                };
                pv.clear();
                // kpidMTime
                (vt.get_property)(self.raw, index, crate::com::propvariant::KPID_MTIME, &mut pv);
                let mtime = pv.as_system_time();
                pv.clear();
                // kpidEncrypted
                (vt.get_property)(
                    self.raw,
                    index,
                    crate::com::propvariant::KPID_ENCRYPTED,
                    &mut pv,
                );
                let encrypted = pv.vt == crate::com::propvariant::VT_BOOL && pv.as_bool();
                pv.clear();
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

fn map_open_error(hr: i32, state: &crate::com::callbacks::OpenState) -> ZipnestError {
    // E_ABORT and friends go through the single mapping table first.
    if !matches!(crate::error::map_hresult(hr), ZipnestError::Engine(_)) {
        return crate::error::map_hresult(hr);
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
            // Release the retained input stream last.
            crate::com::instream::release_void(self.stream);
        }
    }
}

