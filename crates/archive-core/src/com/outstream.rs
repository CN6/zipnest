//! `IOutStream` implementation backed by a real file on disk.
//!
//! The archive handler writes the archive body through this object, possibly
//! from worker threads, so the file handle and the byte counter both live
//! behind synchronization. Mirrors [`super::instream::FileStreamOwner`]'s
//! refcount / raw-pointer pattern.

use super::vtables::OutStreamVt;
use super::{
    qi_matches, Guid, Hresult, ComObject, E_FAIL, IID_IOUT_STREAM, IID_ISEQ_OUT_STREAM,
    IID_IUNKNOWN, S_OK,
};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Our `IOutStream` object: `[vtable ptr][refs][file][written]`.
#[repr(C)]
pub struct FileOutStream {
    pub obj: ComObject<OutStreamVt>,
    refs: AtomicU32,
    file: Mutex<File>,
    written: AtomicU64,
}

static OUT_STREAM_VT: OutStreamVt = OutStreamVt {
    query_interface: qi,
    add_ref,
    release,
    write,
    seek,
    set_size,
};

const KNOWN_IIDS: [Guid; 3] = [IID_IUNKNOWN, IID_ISEQ_OUT_STREAM, IID_IOUT_STREAM];

unsafe extern "system" fn qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if qi_matches(&*riid, &KNOWN_IIDS) {
        *ppv = this;
        add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        super::E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut FileOutStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let this = this as *mut FileOutStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn write(
    this: *mut c_void,
    data: *const c_void,
    size: u32,
    processed: *mut u32,
) -> Hresult {
    if processed.is_null() {
        return E_FAIL;
    }
    *processed = 0;
    if size == 0 {
        return S_OK;
    }
    let this = this as *mut FileOutStream;
    let buf = std::slice::from_raw_parts(data as *const u8, size as usize);
    let mut file = match (*this).file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    match file.write_all(buf) {
        Ok(()) => {
            (*this).written.fetch_add(size as u64, Ordering::Relaxed);
            *processed = size;
            S_OK
        }
        Err(_) => E_FAIL,
    }
}

unsafe extern "system" fn seek(
    this: *mut c_void,
    offset: i64,
    origin: u32,
    new_position: *mut u64,
) -> Hresult {
    let this = this as *mut FileOutStream;
    let whence = match origin {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return E_FAIL,
    };
    let mut file = match (*this).file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    match file.seek(whence) {
        Ok(pos) => {
            if !new_position.is_null() {
                *new_position = pos;
            }
            S_OK
        }
        Err(_) => E_FAIL,
    }
}

unsafe extern "system" fn set_size(this: *mut c_void, new_size: u64) -> Hresult {
    let this = this as *mut FileOutStream;
    let file = match (*this).file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    if file.set_len(new_size).is_ok() {
        S_OK
    } else {
        E_FAIL
    }
}

/// Create the destination file and wrap it in an `IOutStream` with an
/// initial reference count of 1 (owned by the caller). The pointer is what
/// the caller passes to `IOutArchive::UpdateItems`; release it afterwards
/// with [`release_void`].
pub fn new(path: &Path) -> std::io::Result<*mut c_void> {
    let file = File::create(path)?;
    let obj = Box::new(FileOutStream {
        obj: ComObject { vt: &OUT_STREAM_VT },
        refs: AtomicU32::new(1),
        file: Mutex::new(file),
        written: AtomicU64::new(0),
    });
    Ok(Box::into_raw(obj) as *mut c_void)
}

/// Release the caller's reference on a stream from [`new`].
///
/// # Safety
/// `p` must be null or a live pointer previously returned by [`new`].
pub unsafe fn release_void(p: *mut c_void) {
    if !p.is_null() {
        (OUT_STREAM_VT.release)(p);
    }
}
