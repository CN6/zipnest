//! COM primitives: GUIDs, HRESULTs, vtable shapes.
//!
//! All layout data is transcribed from the vendored 7-Zip 26.03 headers —
//! see `vendor/7zip-sdk/README.md` ("Rust FFI notes").

pub mod callbacks;
pub mod instream;
pub mod propvariant;
pub mod vtables;

use std::os::raw::c_void;

/// COM GUID (binary layout as used by `CreateObject`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

pub type Hresult = i32;

pub const S_OK: Hresult = 0;
pub const S_FALSE: Hresult = 1;
pub const E_NOTIMPL: Hresult = 0x8000_4001u32 as i32;
pub const E_NOINTERFACE: Hresult = 0x8000_4002u32 as i32;
pub const E_ABORT: Hresult = 0x8000_4004u32 as i32;
pub const E_FAIL: Hresult = 0x8000_4005u32 as i32;
pub const E_INVALIDARG: Hresult = 0x8007_0057u32 as i32;

/// 7-Zip interface GUID: `{23170F69-40C1-278A-0000-00GG00SS0000}`.
const fn guid7(group: u8, sub: u8) -> Guid {
    Guid {
        data1: 0x2317_0F69,
        data2: 0x40C1,
        data3: 0x278A,
        data4: [0, 0, 0, group, 0, sub, 0, 0],
    }
}

/// 7-Zip handler CLSID: `{23170F69-40C1-278A-1000-000110xx0000}`.
const fn handler_guid(xx: u8) -> Guid {
    Guid {
        data1: 0x2317_0F69,
        data2: 0x40C1,
        data3: 0x278A,
        data4: [0x10, 0, 0, 1, 0x10, xx, 0, 0],
    }
}

pub const IID_IUNKNOWN: Guid = Guid {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
pub const IID_IIN_STREAM: Guid = guid7(3, 0x03);
pub const IID_ISEQ_IN_STREAM: Guid = guid7(3, 0x01);
pub const IID_ISEQ_OUT_STREAM: Guid = guid7(3, 0x02);
pub const IID_IPROGRESS: Guid = guid7(0, 5);
pub const IID_ICRYPTO_GET_TEXT_PASSWORD: Guid = guid7(5, 0x10);
pub const IID_IARCHIVE_OPEN_CALLBACK: Guid = guid7(6, 0x10);
pub const IID_IARCHIVE_EXTRACT_CALLBACK: Guid = guid7(6, 0x20);
pub const IID_IIN_ARCHIVE: Guid = guid7(6, 0x60);

pub const CLSID_FORMAT_ZIP: Guid = handler_guid(0x01);
pub const CLSID_FORMAT_BZIP2: Guid = handler_guid(0x02);
pub const CLSID_FORMAT_RAR: Guid = handler_guid(0x03);
pub const CLSID_FORMAT_XZ: Guid = handler_guid(0x0C);
pub const CLSID_FORMAT_7Z: Guid = handler_guid(0x07);
pub const CLSID_FORMAT_RAR5: Guid = handler_guid(0xCC);
pub const CLSID_FORMAT_ISO: Guid = handler_guid(0xE7);
pub const CLSID_FORMAT_TAR: Guid = handler_guid(0xEE);
pub const CLSID_FORMAT_GZIP: Guid = handler_guid(0xEF);

/// Object header: every COM object we hand to the engine starts with a
/// vtable pointer.
#[repr(C)]
pub struct ComObject<V> {
    pub vt: *const V,
}

/// IUnknown-style `QueryInterface` helper shared by our objects:
/// match `iid` against the provided list, else `E_NOINTERFACE`.
pub unsafe fn qi_matches(iid: &Guid, known: &[Guid]) -> bool {
    known.iter().any(|k| k == iid)
}

/// Convenience: cast any of our COM object structs to the engine-facing
/// `void*` (all start with a vtable pointer).
pub fn as_void<V>(p: *mut V) -> *mut c_void {
    p as *mut c_void
}
