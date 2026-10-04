//! Dynamic loading of `7z.dll` (LGPL: load at runtime, never link).

use crate::com::{Guid, Hresult};
use crate::error::ZipnestError;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// `HRESULT (WINAPI *)(const GUID *clsID, const GUID *iid, void **outObject)`
pub type CreateObjectFn =
    unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> Hresult;

pub struct SevenZip {
    create: CreateObjectFn,
}

impl SevenZip {
    /// Call `CreateObject` on the loaded 7z.dll.
    ///
    /// # Safety
    /// `clsid`/`iid` must point to valid GUIDs and `out` to a writable
    /// pointer slot; on success `*out` holds a reference-counted interface
    /// pointer the caller must `Release`.
    pub unsafe fn create_object(
        &self,
        clsid: *const Guid,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> Hresult {
        (self.create)(clsid, iid, out)
    }
}

/// The successfully loaded library, cached for the process.
///
/// Only a *success* is cached. A failed attempt must stay retryable: the user
/// can install 7-Zip or fix `engines/7z.dll` while the app is running, and the
/// next open has to pick that up without a restart.
static INSTANCE: OnceLock<SevenZip> = OnceLock::new();

/// Serializes load attempts so concurrent first calls load the library once.
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Locate and load 7z.dll once for the process.
///
/// A failed attempt caches nothing: the next call probes every candidate path
/// again and reports a fresh diagnosis.
pub fn load() -> Result<&'static SevenZip, ZipnestError> {
    if let Some(z) = INSTANCE.get() {
        return Ok(z);
    }
    let _guard = LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(z) = INSTANCE.get() {
        return Ok(z);
    }
    match load_impl() {
        Ok(z) => Ok(INSTANCE.get_or_init(|| z)),
        Err(msg) => Err(ZipnestError::DllMissing(msg)),
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // 1) explicit override (dev/tests)
    if let Ok(p) = std::env::var("SEVENZIP_DLL_PATH") {
        if !p.is_empty() {
            v.push(PathBuf::from(p));
        }
    }
    // 2) shipped next to the exe (deployment layout)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("engines").join("7z.dll"));
        }
    }
    // 3) installed system locations
    v.push(PathBuf::from(r"C:\Program Files\7-Zip\7z.dll"));
    v.push(PathBuf::from(r"C:\Program Files (x86)\7-Zip\7z.dll"));
    v
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

/// Why one candidate path did not yield a usable `7z.dll`.
enum CandidateFailure {
    /// Nothing at that path.
    Missing,
    /// `LoadLibraryW` failed; the payload is `GetLastError()`.
    Load(u32),
    /// The library loaded but does not export `CreateObject` (a foreign or
    /// truncated `7z.dll`); the payload is `GetLastError()`.
    NoCreateObject(u32),
}

impl CandidateFailure {
    /// One-line reason. The Win32 error code is what separates "there is no
    /// file here" from "the file is there but the loader refused it".
    fn describe(&self, path: &str) -> String {
        match self {
            CandidateFailure::Missing => format!("{path} (no such file)"),
            CandidateFailure::Load(code) => {
                format!("{path} (LoadLibraryW failed, Win32 error {code})")
            }
            CandidateFailure::NoCreateObject(code) => {
                format!("{path} (no CreateObject export, Win32 error {code})")
            }
        }
    }
}

/// Build the `ZipnestError::DllMissing` payload from every failed candidate.
///
/// Pure so the wording can be unit-tested without a real DLL.
fn missing_message(failures: &[(String, CandidateFailure)]) -> String {
    if failures.is_empty() {
        return "7z.dll not found; no candidate paths".to_string();
    }
    let reasons: Vec<String> = failures
        .iter()
        .map(|(path, failure)| failure.describe(path))
        .collect();
    format!("7z.dll not found; tried: {}", reasons.join(" | "))
}

fn load_impl() -> Result<SevenZip, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut failures: Vec<(String, CandidateFailure)> = Vec::new();
    for path in candidate_paths() {
        let display = path.display().to_string();
        if !path.exists() {
            failures.push((display, CandidateFailure::Missing));
            continue;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let module = LoadLibraryW(wide.as_ptr());
            if module.is_null() {
                failures.push((display, CandidateFailure::Load(GetLastError())));
                continue;
            }
            let sym = GetProcAddress(module, c"CreateObject".as_ptr());
            if sym.is_null() {
                // Loaded something that is not the 7-Zip engine: hand the
                // module back instead of leaking the reference.
                failures.push((display, CandidateFailure::NoCreateObject(GetLastError())));
                FreeLibrary(module);
                continue;
            }
            // Success path intentionally keeps the module loaded for the
            // lifetime of the process (its function pointer stays cached).
            let create: CreateObjectFn = std::mem::transmute(sym);
            return Ok(SevenZip { create });
        }
    }
    Err(missing_message(&failures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_message_distinguishes_missing_from_unloadable() {
        let failures = vec![
            (
                r"C:\app\engines\7z.dll".to_string(),
                CandidateFailure::Missing,
            ),
            (
                r"C:\Program Files\7-Zip\7z.dll".to_string(),
                CandidateFailure::Load(193),
            ),
            (
                r"C:\regress\7z.dll".to_string(),
                CandidateFailure::NoCreateObject(127),
            ),
        ];
        let msg = missing_message(&failures);
        assert!(msg.starts_with("7z.dll not found; tried: "));
        assert!(msg.contains(r"C:\app\engines\7z.dll (no such file)"));
        assert!(msg.contains("LoadLibraryW failed, Win32 error 193"));
        assert!(msg.contains("no CreateObject export, Win32 error 127"));
    }

    #[test]
    fn missing_message_covers_an_empty_candidate_list() {
        assert_eq!(missing_message(&[]), "7z.dll not found; no candidate paths");
    }

    #[test]
    fn candidate_paths_always_offers_the_installed_locations() {
        // The override is opt-in, so the two standard install locations must
        // always be probed (order-independent assertion).
        let paths: Vec<String> = candidate_paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with(r"7-Zip\7z.dll")));
    }
}
