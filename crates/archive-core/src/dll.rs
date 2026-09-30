//! Dynamic loading of `7z.dll` (LGPL: load at runtime, never link).

use crate::com::{Guid, Hresult};
use crate::error::ZipnestError;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

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

static INSTANCE: OnceLock<Result<SevenZip, String>> = OnceLock::new();

/// Locate and load 7z.dll once for the process.
pub fn load() -> Result<&'static SevenZip, ZipnestError> {
    match INSTANCE.get_or_init(load_impl) {
        Ok(z) => Ok(z),
        Err(msg) => Err(ZipnestError::DllMissing(msg.clone())),
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
}

fn load_impl() -> Result<SevenZip, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut tried: Vec<String> = Vec::new();
    for path in candidate_paths() {
        let display = path.display().to_string();
        tried.push(display.clone());
        if !path.exists() {
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
                continue;
            }
            let sym = GetProcAddress(module, c"CreateObject".as_ptr());
            if sym.is_null() {
                continue;
            }
            let create: CreateObjectFn = std::mem::transmute(sym);
            return Ok(SevenZip { create });
        }
    }
    Err(format!("7z.dll not found; tried: {}", tried.join(" | ")))
}
