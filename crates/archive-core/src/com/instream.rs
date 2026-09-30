//! `IInStream` implementation backed by a real file on disk.
//!
//! The engine may retain and call this stream from decoder threads, so all
//! mutable state sits behind a mutex.

use super::vtables::InStreamVt;
use super::{as_void, Guid, Hresult, ComObject, E_FAIL, E_NOINTERFACE, IID_IIN_STREAM,
    IID_ISEQ_IN_STREAM, IID_IUNKNOWN, S_OK};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

pub struct FileStreamState {
    file: Mutex<(File, u64)>,
}

/// Our `IInStream` object: `[vtable ptr][refs][state ptr]`.
#[repr(C)]
pub struct InFileStream {
    pub obj: ComObject<InStreamVt>,
    refs: AtomicU32,
    state: *mut FileStreamState,
}

static IN_STREAM_VT: InStreamVt = InStreamVt {
    query_interface: qi,
    add_ref,
    release,
    read,
    seek,
};

const KNOWN_IIDS: [Guid; 3] = [IID_IUNKNOWN, IID_ISEQ_IN_STREAM, IID_IIN_STREAM];

unsafe extern "system" fn qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if super::qi_matches(&*riid, &KNOWN_IIDS) {
        *ppv = this;
        add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut InFileStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let this = this as *mut InFileStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        let state = Box::from_raw((*this).state);
        drop(state);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn read(
    this: *mut c_void,
    data: *mut c_void,
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
    let this = this as *mut InFileStream;
    let state = &*(*this).state;
    let buf = std::slice::from_raw_parts_mut(data as *mut u8, size as usize);
    let mut guard = match state.file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    let (file, pos) = &mut *guard;
    match file.read(buf) {
        Ok(n) => {
            *pos += n as u64;
            *processed = n as u32;
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
    let this = this as *mut InFileStream;
    let state = &*(*this).state;
    let whence = match origin {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return E_FAIL,
    };
    let mut guard = match state.file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    let (file, pos) = &mut *guard;
    match file.seek(whence) {
        Ok(new_pos) => {
            *pos = new_pos;
            if !new_position.is_null() {
                *new_position = new_pos;
            }
            S_OK
        }
        Err(_) => {
            // Negative seek etc.
            E_FAIL
        }
    }
}

pub struct FileStreamOwner {
    ptr: *mut InFileStream,
}

impl FileStreamOwner {
    pub fn new(file: File) -> std::io::Result<Self> {
        let state = Box::new(FileStreamState {
            file: Mutex::new((file, 0)),
        });
        let obj = Box::new(InFileStream {
            obj: ComObject { vt: &IN_STREAM_VT },
            refs: AtomicU32::new(1),
            state: Box::into_raw(state),
        });
        Ok(FileStreamOwner {
            ptr: Box::into_raw(obj),
        })
    }

    pub fn as_void(&self) -> *mut c_void {
        as_void(self.ptr)
    }

    /// Release our own reference (engine-held refs, if any, keep it alive).
    pub unsafe fn release_own(self) {
        let p = self.ptr;
        std::mem::forget(self);
        (IN_STREAM_VT.release)(p as *mut c_void);
    }

    /// Transfer ownership (engine object keeps it; `Archive` will release).
    pub fn into_raw(self) -> *mut c_void {
        let p = self.ptr;
        std::mem::forget(self);
        p as *mut c_void
    }
}

impl Drop for FileStreamOwner {
    fn drop(&mut self) {
        unsafe {
            (IN_STREAM_VT.release)(self.ptr as *mut c_void);
        }
    }
}

/// Release a raw stream pointer previously handed out by
/// [`FileStreamOwner::into_raw`] (used by `Archive::drop`).
pub unsafe fn release_void(p: *mut c_void) {
    if !p.is_null() {
        (IN_STREAM_VT.release)(p);
    }
}

