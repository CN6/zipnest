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

/// `ICryptoGetTextPassword` (IPassword.h) — single method after IUnknown.
#[repr(C)]
pub struct CryptoGetTextPasswordVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub get_text_password:
        unsafe extern "system" fn(this: *mut c_void, password: *mut *mut u16) -> Hresult,
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
