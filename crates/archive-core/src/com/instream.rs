//! `IInStream` implementation backed by a real file on disk.
//!
//! The engine may retain and call this stream from decoder threads, so all
//! mutable state sits behind a mutex.

use super::vtables::InStreamVt;
use super::{as_void, Guid, Hresult, ComObject, E_FAIL, E_NOINTERFACE, IID_IIN_STREAM,
    IID_ISEQ_IN_STREAM, IID_IUNKNOWN, S_OK};
use crate::error::{seek_target_checked, E_NEGATIVE_SEEK};
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
    if data.is_null() {
        // Reading `size > 0` bytes into a null buffer is UB; the engine never
        // asks for that, so a failure here means an ABI/layout mismatch.
        return E_FAIL;
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
    let mut guard = match state.file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    let (file, pos) = &mut *guard;
    // Resolve the origin here instead of handing the raw `offset` to the OS:
    // `offset as u64` on a negative offset used to become a ~1.8e19 forward
    // seek that still reported S_OK. `IInStream::Seek` must reject a position
    // before the start of the stream (vendor/7zip-sdk/IStream.h).
    let base: u64 = match origin {
        0 => 0,
        1 => *pos,
        2 => match file.metadata() {
            Ok(meta) => meta.len(),
            Err(_) => return E_FAIL,
        },
        _ => return E_FAIL,
    };
    let target = match seek_target_checked(base, offset) {
        Some(t) => t,
        None => return E_NEGATIVE_SEEK,
    };
    match file.seek(SeekFrom::Start(target)) {
        Ok(new_pos) => {
            *pos = new_pos;
            if !new_position.is_null() {
                *new_position = new_pos;
            }
            S_OK
        }
        Err(_) => E_FAIL,
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

/// Release a stream through *its own* vtable. Needed because `Archive` may
/// hold either one of our `FileStreamOwner`s or a foreign `IInStream` returned
/// by `IInArchiveGetStream` (the engine's `CMultiStream`); both share the
/// IUnknown+Read+Seek layout, so dispatch must go through the object.
pub unsafe fn release_raw(p: *mut c_void) {
    if !p.is_null() {
        let vt = &**(p as *const *const InStreamVt);
        (vt.release)(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_stream(tag: &str, body: &[u8]) -> (FileStreamOwner, PathBuf) {
        let dir = std::env::temp_dir().join(format!("zn-instream-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("in.bin");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(body).unwrap();
        }
        (FileStreamOwner::new(File::open(&path).unwrap()).unwrap(), dir)
    }

    #[test]
    fn seek_rejects_a_position_before_the_start_of_the_stream() {
        let (owner, dir) = temp_stream("neg", b"abcdef");
        let p = owner.as_void();
        unsafe {
            assert_eq!(seek(p, -1, 0, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            assert_eq!(seek(p, -1, 1, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            assert_eq!(seek(p, -7, 2, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            // Valid seeks still work and report the new position.
            let mut pos = u64::MAX;
            assert_eq!(seek(p, 2, 0, &mut pos), S_OK);
            assert_eq!(pos, 2);
            assert_eq!(seek(p, -1, 1, &mut pos), S_OK);
            assert_eq!(pos, 1);
            assert_eq!(seek(p, -6, 2, &mut pos), S_OK);
            assert_eq!(pos, 0);
            assert_eq!(seek(p, 0, 9, std::ptr::null_mut()), E_FAIL);
        }
        drop(owner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_rejects_a_null_buffer_but_allows_a_zero_size_read() {
        let (owner, dir) = temp_stream("read", b"abcdef");
        let p = owner.as_void();
        let mut processed = 7u32;
        unsafe {
            assert_eq!(read(p, std::ptr::null_mut(), 4, &mut processed), E_FAIL);
            assert_eq!(processed, 0);
            // A zero-size read into a null buffer is legal.
            assert_eq!(read(p, std::ptr::null_mut(), 0, &mut processed), S_OK);
            let mut buf = [0u8; 3];
            assert_eq!(
                read(p, buf.as_mut_ptr() as *mut c_void, 3, &mut processed),
                S_OK
            );
            assert_eq!(processed, 3);
            assert_eq!(&buf, b"abc");
        }
        drop(owner);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

