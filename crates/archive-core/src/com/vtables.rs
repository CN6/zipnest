//! Vtable shapes transcribed from vendor headers (notes §3-§4).
//!
//! We only *invoke* methods on engine objects through these types; objects
//! we *implement* live in `instream.rs` / `callbacks.rs` and define their
//! own matching vtables.

use super::propvariant::PropVariant;
use super::{Guid, Hresult};
use std::os::raw::c_void;

pub type QueryInterfaceFn = unsafe extern "system" fn(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult;
pub type AddRefFn = unsafe extern "system" fn(this: *mut c_void) -> u32;
pub type ReleaseFn = unsafe extern "system" fn(this: *mut c_void) -> u32;

/// IUnknown slots (0..=2) shared by every vtable.
#[repr(C)]
pub struct UnknownVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
}

/// `IInStream` = IUnknown + Read + Seek (vendor notes §4).
#[repr(C)]
pub struct InStreamVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub read: unsafe extern "system" fn(
        this: *mut c_void,
        data: *mut c_void,
        size: u32,
        processed: *mut u32,
    ) -> Hresult,
    pub seek: unsafe extern "system" fn(
        this: *mut c_void,
        offset: i64,
        origin: u32,
        new_position: *mut u64,
    ) -> Hresult,
}

/// `IInArchive` = IUnknown + 10 methods (vendor notes §4, exact order).
#[repr(C)]
pub struct InArchiveVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub open: unsafe extern "system" fn(
        this: *mut c_void,
        stream: *mut c_void,
        max_check_start: *const u64,
        open_callback: *mut c_void,
    ) -> Hresult,
    pub close: unsafe extern "system" fn(this: *mut c_void) -> Hresult,
    pub get_number_of_items:
        unsafe extern "system" fn(this: *mut c_void, num_items: *mut u32) -> Hresult,
    pub get_property: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        prop_id: u32,
        value: *mut PropVariant,
    ) -> Hresult,
    pub extract: unsafe extern "system" fn(
        this: *mut c_void,
        indices: *const u32,
        num_items: u32,
        test_mode: i32,
        extract_callback: *mut c_void,
    ) -> Hresult,
    pub get_archive_property: unsafe extern "system" fn(
        this: *mut c_void,
        prop_id: u32,
        value: *mut PropVariant,
    ) -> Hresult,
    pub get_number_of_properties:
        unsafe extern "system" fn(this: *mut c_void, num_props: *mut u32) -> Hresult,
    pub get_property_info: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        name: *mut *mut u16,
        prop_id: *mut u32,
        var_type: *mut u16,
    ) -> Hresult,
    pub get_number_of_archive_properties:
        unsafe extern "system" fn(this: *mut c_void, num_props: *mut u32) -> Hresult,
    pub get_archive_property_info: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        name: *mut *mut u16,
        prop_id: *mut u32,
        var_type: *mut u16,
    ) -> Hresult,
}

/// `IArchiveOpenCallback` = IUnknown + SetTotal + SetCompleted
/// (does NOT derive IProgress — IArchive.h line 181).
#[repr(C)]
pub struct ArchiveOpenCallbackVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_total: unsafe extern "system" fn(
        this: *mut c_void,
        files: *const u64,
        bytes: *const u64,
    ) -> Hresult,
    pub set_completed: unsafe extern "system" fn(
        this: *mut c_void,
        files: *const u64,
        bytes: *const u64,
    ) -> Hresult,
}

/// `IArchiveOpenVolumeCallback` = IUnknown + GetProperty + GetStream
/// (IArchive.h `Z7_IFACEM_IArchiveOpenVolumeCallback`, guid 6:0x30).
/// The `Split` handler queries this to discover and open the remaining
/// `.NNN` volumes.
#[repr(C)]
pub struct OpenVolumeCallbackVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub get_property: unsafe extern "system" fn(
        this: *mut c_void,
        prop_id: u32,
        value: *mut PropVariant,
    ) -> Hresult,
    pub get_stream: unsafe extern "system" fn(
        this: *mut c_void,
        name: *const u16,
        in_stream: *mut *mut c_void,
    ) -> Hresult,
}

/// `IInArchiveGetStream` = IUnknown + GetStream (guid 6:0x40). Exposes an
/// exposed handler entry as a raw stream (the `Split` handler uses it to hand
/// back the concatenated volume stream).
#[repr(C)]
pub struct InArchiveGetStreamVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub get_stream:
        unsafe extern "system" fn(this: *mut c_void, index: u32, stream: *mut *mut c_void) -> Hresult,
}

/// `ICryptoGetTextPassword` (IPassword.h) — single method after IUnknown.
#[repr(C)]
pub struct CryptoGetTextPasswordVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub get_text_password:
        unsafe extern "system" fn(this: *mut c_void, password: *mut *mut u16) -> Hresult,
}

/// `ICryptoGetTextPassword2` (IPassword.h) — single method after IUnknown.
///
/// The handlers obtain the create-time password through this interface
/// (`ZipHandlerOut.cpp`, `7zHandlerOut.cpp`), *not* through the
/// `ICryptoGetTextPassword` used by the open path.
#[repr(C)]
pub struct CryptoGetTextPassword2Vt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub get_text_password2: unsafe extern "system" fn(
        this: *mut c_void,
        password_is_defined: *mut i32,
        password: *mut *mut u16,
    ) -> Hresult,
}

/// `IProgress` = IUnknown + SetTotal(u64) + SetCompleted(*const u64).
#[repr(C)]
pub struct ProgressVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_total: unsafe extern "system" fn(this: *mut c_void, total: u64) -> Hresult,
    pub set_completed:
        unsafe extern "system" fn(this: *mut c_void, complete_value: *const u64) -> Hresult,
}

/// `IArchiveExtractCallback` = IProgress + GetStream + PrepareOperation +
/// SetOperationResult (base vtable first!).
#[repr(C)]
pub struct ArchiveExtractCallbackVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_total: unsafe extern "system" fn(this: *mut c_void, total: u64) -> Hresult,
    pub set_completed:
        unsafe extern "system" fn(this: *mut c_void, complete_value: *const u64) -> Hresult,
    pub get_stream: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        out_stream: *mut *mut c_void,
        ask_extract_mode: i32,
    ) -> Hresult,
    pub prepare_operation: unsafe extern "system" fn(
        this: *mut c_void,
        ask_extract_mode: i32,
    ) -> Hresult,
    pub set_operation_result:
        unsafe extern "system" fn(this: *mut c_void, op_res: i32) -> Hresult,
}

/// `ISequentialOutStream` = IUnknown + Write (for extract sinks).
#[repr(C)]
pub struct SeqOutStreamVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub write: unsafe extern "system" fn(
        this: *mut c_void,
        data: *const c_void,
        size: u32,
        processed: *mut u32,
    ) -> Hresult,
}

/// `IOutStream` = IUnknown + Write + Seek + SetSize (vendor `IStream.h`:
/// `IOutStream` derives `ISequentialOutStream`).
#[repr(C)]
pub struct OutStreamVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub write: unsafe extern "system" fn(
        this: *mut c_void,
        data: *const c_void,
        size: u32,
        processed: *mut u32,
    ) -> Hresult,
    pub seek: unsafe extern "system" fn(
        this: *mut c_void,
        offset: i64,
        origin: u32,
        new_position: *mut u64,
    ) -> Hresult,
    pub set_size: unsafe extern "system" fn(this: *mut c_void, new_size: u64) -> Hresult,
}

/// `IOutArchive` = IUnknown + UpdateItems + GetFileTimeType (IArchive.h 0xA0).
#[repr(C)]
pub struct OutArchiveVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub update_items: unsafe extern "system" fn(
        this: *mut c_void,
        out_stream: *mut c_void,
        num_items: u32,
        update_callback: *mut c_void,
    ) -> Hresult,
    pub get_file_time_type: unsafe extern "system" fn(this: *mut c_void, t: *mut u32) -> Hresult,
}

/// `ISetProperties` = IUnknown + SetProperties (IArchive.h 0x03).
#[repr(C)]
pub struct SetPropertiesVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_properties: unsafe extern "system" fn(
        this: *mut c_void,
        names: *const *const u16,
        values: *const PropVariant,
        num_props: u32,
    ) -> Hresult,
}

/// `IArchiveUpdateCallback` = IProgress (SetTotal/SetCompleted) +
/// GetUpdateItemInfo + GetProperty + GetStream + SetOperationResult
/// (IArchive.h 0x80, base vtable first).
#[repr(C)]
pub struct UpdateCallbackVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_total: unsafe extern "system" fn(this: *mut c_void, total: u64) -> Hresult,
    pub set_completed: unsafe extern "system" fn(this: *mut c_void, complete: *const u64) -> Hresult,
    pub get_update_item_info: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        new_data: *mut i32,
        new_props: *mut i32,
        index_in_archive: *mut u32,
    ) -> Hresult,
    pub get_property: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        prop_id: u32,
        value: *mut PropVariant,
    ) -> Hresult,
    pub get_stream: unsafe extern "system" fn(
        this: *mut c_void,
        index: u32,
        in_stream: *mut *mut c_void,
    ) -> Hresult,
    pub set_operation_result: unsafe extern "system" fn(this: *mut c_void, op_res: i32) -> Hresult,
}
