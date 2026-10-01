//! PROPVARIANT layout + helpers (Windows ABI, x64) — vendor notes §5.

use super::Hresult;
use std::os::raw::c_void;

/// VARTYPE constants we need.
pub const VT_EMPTY: u16 = 0;
pub const VT_BSTR: u16 = 8;
pub const VT_BOOL: u16 = 11;
pub const VT_UI4: u16 = 19;
pub const VT_I8: u16 = 20;
pub const VT_UI8: u16 = 21;
pub const VT_FILETIME: u16 = 64;

/// PROPID values (vendor notes §6; PropID.h enum counted).
pub const KPID_PATH: u32 = 3;
/// `kpidName` (PropID.h, enum counted from `kpidNoProperty = 0`): the 7-Zip
/// `Split` handler asks the volume callback for this to learn the first
/// volume's file name (e.g. `vol.7z.001`).
pub const KPID_NAME: u32 = 4;
pub const KPID_IS_DIR: u32 = 6;
pub const KPID_SIZE: u32 = 7;
pub const KPID_MTIME: u32 = 12;
pub const KPID_ENCRYPTED: u32 = 15;
pub const KPID_CRC: u32 = 19;
pub const KPID_PHY_SIZE: u32 = 44;
pub const KPID_ERROR_FLAGS: u32 = 71;

/// `kpv_ErrorFlags` bits (vendor notes §6).
pub const ERRFLAGS_IS_NOT_ARC: u32 = 1 << 0;
pub const ERRFLAGS_HEADERS_ERROR: u32 = 1 << 1;
pub const ERRFLAGS_ENCRYPTED_HEADERS_ERROR: u32 = 1 << 2;

/// 100ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH_DELTA: u64 = 11_644_473_600_000_000;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropVariant {
    pub vt: u16,
    pub reserved1: u16,
    pub reserved2: u16,
    pub reserved3: u16,
    pub data: u64,
    /// The Windows `PROPVARIANT` union is 16 bytes on x64 (it contains
    /// `DECIMAL`), so `sizeof(PROPVARIANT)` is 24. Arrays handed to the engine
    /// (e.g. `ISetProperties`) are indexed with that stride; this tail keeps
    /// our array stride correct and gives the engine room to write.
    pub reserved4: u64,
}

impl PropVariant {
    /// `vt = VT_EMPTY`, everything else zero — required before every
    /// `GetProperty` call (callee may `VariantClear` the input).
    pub fn empty() -> Self {
        PropVariant {
            vt: VT_EMPTY,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: 0,
        }
    }

    /// UI8 payload (also valid for VT_I8).
    pub fn as_u64(&self) -> u64 {
        self.data
    }

    /// VT_BOOL: `-1i32` (VARIANT_TRUE) is non-zero.
    pub fn as_bool(&self) -> bool {
        (self.data as u32 as i32) != 0
    }

    /// VT_FILETIME → SystemTime.
    pub fn as_system_time(&self) -> Option<std::time::SystemTime> {
        if self.vt != VT_FILETIME {
            return None;
        }
        let ticks = self.data;
        if ticks < FILETIME_UNIX_EPOCH_DELTA {
            return None;
        }
        let unix_100ns = ticks - FILETIME_UNIX_EPOCH_DELTA;
        std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_nanos(unix_100ns.checked_mul(100)?))
    }

    /// VT_BSTR → owned UTF-16 string, freeing the engine-allocated BSTR.
    pub unsafe fn take_bstr(&mut self) -> Option<String> {
        if self.vt != VT_BSTR || self.data == 0 {
            return None;
        }
        let ptr = self.data as *mut u16;
        // BSTR length prefix lives at ptr-4 (byte length); use it, then NUL-terminate fallback.
        let byte_len = *(ptr.wrapping_sub(2) as *const u32) as usize;
        let n_units = byte_len / 2;
        let slice = std::slice::from_raw_parts(ptr, n_units);
        let s = String::from_utf16_lossy(slice);
        sys_free_string(ptr);
        self.vt = VT_EMPTY;
        self.data = 0;
        Some(s)
    }

    /// Release any engine-held payload (must be called on every variant
    /// returned from `GetProperty`).
    pub unsafe fn clear(&mut self) {
        if self.vt != VT_EMPTY {
            prop_variant_clear(self);
            self.vt = VT_EMPTY;
            self.data = 0;
        }
    }

    /// BSTR-valued property (e.g. `kpidPath`): the BSTR is owned by the
    /// PropVariant and released by [`PropVariant::clear`].
    pub fn from_bstr(s: &str) -> Self {
        PropVariant {
            vt: VT_BSTR,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: alloc_bstr(s) as u64,
        }
    }

    /// `VT_UI4` payload (7-Zip's numeric handler properties such as `x`).
    pub fn from_u32(v: u32) -> Self {
        PropVariant {
            vt: VT_UI4,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: v as u64,
        }
    }

    /// `VT_UI8` payload (also used for `kpidSize`).
    pub fn from_u64(v: u64) -> Self {
        PropVariant {
            vt: VT_UI8,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: v,
        }
    }

    /// `VT_BOOL` (VARIANT_TRUE is `-1i32`).
    pub fn from_bool(v: bool) -> Self {
        PropVariant {
            vt: VT_BOOL,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: (if v { -1i32 } else { 0 }) as u32 as u64,
        }
    }

    /// `VT_FILETIME` from a `SystemTime` (100ns ticks since 1601-01-01).
    pub fn from_filetime(t: std::time::SystemTime) -> Self {
        let ticks = t
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 / 100 + FILETIME_UNIX_EPOCH_DELTA)
            .unwrap_or(FILETIME_UNIX_EPOCH_DELTA);
        PropVariant {
            vt: VT_FILETIME,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
            data: ticks,
        }
    }
}

#[link(name = "ole32")]
extern "system" {
    fn PropVariantClear(pv: *mut PropVariant) -> Hresult;
}

#[link(name = "oleaut32")]
extern "system" {
    fn SysFreeString(bstr: *mut u16);
}

unsafe fn prop_variant_clear(pv: &mut PropVariant) {
    PropVariantClear(pv as *mut PropVariant);
}

unsafe fn sys_free_string(b: *mut u16) {
    SysFreeString(b);
}

/// Allocate a BSTR from a Rust string (for passwords we hand to the engine).
pub fn alloc_bstr(s: &str) -> *mut u16 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    sys_alloc_string(&wide)
}

#[link(name = "oleaut32")]
extern "system" {
    fn SysAllocStringLen(strin: *const u16, uint: u32) -> *mut u16;
}

fn sys_alloc_string(wide: &[u16]) -> *mut u16 {
    unsafe { SysAllocStringLen(wide.as_ptr(), wide.len() as u32) }
}

/// `VariantClear` equivalent for a `*mut c_void` payload we no longer need.
pub fn _unused(_: *mut c_void) {}
