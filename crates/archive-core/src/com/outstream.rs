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
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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

// ---------------------------------------------------------------------------
// Multi-volume output stream (Task 7).
// ---------------------------------------------------------------------------

/// A virtual byte stream spanning `<prefix>001`, `<prefix>002`, ... where
/// `prefix` already ends in `.` (e.g. `C:\out\vol.7z.`). Every volume is
/// `limit` bytes except the last, which may be shorter.
///
/// This mirrors 7-Zip's client-side `CMultiOutStream`: the `IOutArchive`
/// handler only ever sees a single seekable stream and never knows the output
/// is split. Seeks are virtual-offset based, so the handler can rewrite earlier
/// volume bytes (ZIP local headers, the 7z start header) after writing the tail.
struct VolumeState {
    prefix: std::ffi::OsString,
    limit: u64,
    files: Vec<Option<File>>,
    length: u64,
    pos: u64,
}

impl VolumeState {
    /// Path of volume `index` (zero-based): `prefix` + `{:03}` of `index + 1`.
    fn path_for(&self, index: usize) -> PathBuf {
        let mut s = self.prefix.clone();
        s.push(format!("{:03}", index + 1));
        PathBuf::from(s)
    }

    fn ensure(&mut self, index: usize) -> std::io::Result<()> {
        while self.files.len() <= index {
            self.files.push(None);
        }
        if self.files[index].is_none() {
            let path = self.path_for(index);
            // `create_new` refuses to clobber an unrelated existing file.
            let f = OpenOptions::new().write(true).create_new(true).open(&path)?;
            self.files[index] = Some(f);
        }
        Ok(())
    }

    fn truncate(&mut self, index: usize, size: u64) -> std::io::Result<()> {
        if index < self.files.len() {
            if let Some(f) = self.files[index].as_mut() {
                f.set_len(size)?;
            }
        }
        Ok(())
    }

    fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        if self.pos > self.length {
            // A forward seek past the logical end: materialize the gap as
            // zero-filled volumes rather than leaving missing files behind.
            self.set_size(self.pos)?;
        }
        let total = buf.len();
        while !buf.is_empty() {
            let index = (self.pos / self.limit) as usize;
            let rel = self.pos % self.limit;
            let room = self.limit - rel;
            let n = std::cmp::min(room as usize, buf.len());
            self.ensure(index)?;
            {
                let f = self.files[index].as_mut().expect("volume was just ensured");
                f.seek(SeekFrom::Start(rel))?;
                f.write_all(&buf[..n])?;
            }
            self.pos += n as u64;
            if self.pos > self.length {
                self.length = self.pos;
            }
            buf = &buf[n..];
        }
        Ok(total)
    }

    fn seek(&mut self, offset: i64, origin: u32) -> std::io::Result<u64> {
        let base: i64 = match origin {
            0 => 0,
            1 => self.pos as i64,
            2 => self.length as i64,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bad seek origin",
                ))
            }
        };
        let target = base.checked_add(offset).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek overflow")
        })?;
        if target < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }

    fn set_size(&mut self, new_size: u64) -> std::io::Result<()> {
        self.length = new_size;
        let last = if new_size == 0 {
            0
        } else {
            ((new_size - 1) / self.limit) as usize
        };
        for i in 0..=last {
            let start = i as u64 * self.limit;
            let desired = std::cmp::min(self.limit, new_size - start);
            // Extending must materialize every volume in range, including gaps
            // that were never written: otherwise the logical length would cover
            // files that do not exist. `set_len` zero-fills an unopened volume.
            if desired > 0 {
                self.ensure(i)?;
            }
            self.truncate(i, desired)?;
        }
        for i in (last + 1)..self.files.len() {
            self.truncate(i, 0)?;
        }
        Ok(())
    }

    /// Truncate every volume to its logical size and remove trailing volumes
    /// that hold no data. Returns the total archive length.
    fn finalize(&mut self) -> std::io::Result<u64> {
        let total = if self.length == 0 {
            1
        } else {
            ((self.length - 1) / self.limit + 1) as usize
        };
        // Cover every logical volume in the loop so a gap that was never opened
        // still gets created and sized rather than silently missing.
        while self.files.len() < total {
            self.files.push(None);
        }
        for i in 0..self.files.len() {
            let start = i as u64 * self.limit;
            let desired = if i < total {
                std::cmp::min(self.limit, self.length - start)
            } else {
                0
            };
            if desired == 0 {
                let taken = self.files[i].take();
                if let Some(f) = taken {
                    drop(f);
                    let _ = std::fs::remove_file(self.path_for(i));
                }
            } else {
                self.ensure(i)?;
                self.truncate(i, desired)?;
            }
        }
        Ok(self.length)
    }
}

/// Our multi-volume `IOutStream`: `[vtable ptr][refs][commit][state]`.
#[repr(C)]
pub struct VolumeOutStream {
    pub obj: ComObject<OutStreamVt>,
    refs: AtomicU32,
    commit: AtomicBool,
    state: Mutex<VolumeState>,
}

static VOLUME_STREAM_VT: OutStreamVt = OutStreamVt {
    query_interface: vol_qi,
    add_ref: vol_add_ref,
    release: vol_release,
    write: vol_write,
    seek: vol_seek,
    set_size: vol_set_size,
};

const KNOWN_IIDS_VOL: [Guid; 3] = [IID_IUNKNOWN, IID_ISEQ_OUT_STREAM, IID_IOUT_STREAM];

unsafe extern "system" fn vol_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if qi_matches(&*riid, &KNOWN_IIDS_VOL) {
        *ppv = this;
        vol_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        super::E_NOINTERFACE
    }
}

unsafe extern "system" fn vol_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut VolumeOutStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn vol_release(this: *mut c_void) -> u32 {
    let this = this as *mut VolumeOutStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn vol_write(
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
    let this = this as *mut VolumeOutStream;
    let buf = std::slice::from_raw_parts(data as *const u8, size as usize);
    let mut state = match (*this).state.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    match state.write(buf) {
        Ok(n) => {
            *processed = n as u32;
            S_OK
        }
        Err(_) => E_FAIL,
    }
}

unsafe extern "system" fn vol_seek(
    this: *mut c_void,
    offset: i64,
    origin: u32,
    new_position: *mut u64,
) -> Hresult {
    let this = this as *mut VolumeOutStream;
    let mut state = match (*this).state.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    match state.seek(offset, origin) {
        Ok(pos) => {
            if !new_position.is_null() {
                *new_position = pos;
            }
            S_OK
        }
        Err(_) => E_FAIL,
    }
}

unsafe extern "system" fn vol_set_size(this: *mut c_void, new_size: u64) -> Hresult {
    let this = this as *mut VolumeOutStream;
    let mut state = match (*this).state.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    match state.set_size(new_size) {
        Ok(()) => S_OK,
        Err(_) => E_FAIL,
    }
}

impl Drop for VolumeOutStream {
    fn drop(&mut self) {
        // An uncommitted stream is a failed create: delete every volume that
        // was opened so no partial `.001`/`.002` litter survives.
        let keep = self.commit.load(Ordering::SeqCst);
        if let Ok(mut state) = self.state.lock() {
            for i in 0..state.files.len() {
                let taken = state.files[i].take();
                if let Some(f) = taken {
                    drop(f);
                    if !keep {
                        let _ = std::fs::remove_file(state.path_for(i));
                    }
                }
            }
        }
    }
}

/// Create a multi-volume output stream writing to `<dest>.001`, `<dest>.002`,
/// ... with each volume at most `limit` bytes. `limit` must be non-zero.
pub fn new_volumes(dest: &Path, limit: u64) -> std::io::Result<*mut c_void> {
    if limit == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "volume size must be non-zero",
        ));
    }
    let mut prefix = dest.as_os_str().to_os_string();
    prefix.push(".");
    let obj = Box::new(VolumeOutStream {
        obj: ComObject {
            vt: &VOLUME_STREAM_VT,
        },
        refs: AtomicU32::new(1),
        commit: AtomicBool::new(false),
        state: Mutex::new(VolumeState {
            prefix,
            limit,
            files: Vec::new(),
            length: 0,
            pos: 0,
        }),
    });
    Ok(Box::into_raw(obj) as *mut c_void)
}

/// Release the caller's reference on a stream from [`new_volumes`].
///
/// # Safety
/// `p` must be null or a live pointer previously returned by [`new_volumes`].
pub unsafe fn release_void_volumes(p: *mut c_void) {
    if !p.is_null() {
        (VOLUME_STREAM_VT.release)(p);
    }
}

/// Finish a successful multi-volume write: truncate each volume to its logical
/// size, drop trailing unused volumes, and mark the files as committed so the
/// destructor leaves them on disk. Returns the total length in bytes, or `None`
/// if truncation failed (the files are then deleted when the stream is
/// released).
///
/// # Safety
/// `p` must be null or a live pointer previously returned by [`new_volumes`].
pub unsafe fn finalize_volumes(p: *mut c_void) -> Option<u64> {
    if p.is_null() {
        return None;
    }
    let this = p as *mut VolumeOutStream;
    let mut state = (*this).state.lock().ok()?;
    match state.finalize() {
        Ok(n) => {
            (*this).commit.store(true, Ordering::SeqCst);
            Some(n)
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_past_end_creates_and_sizes_gap_volumes() {
        let dir = std::env::temp_dir().join(format!("zn-volstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.7z");

        let p = new_volumes(&dest, 100).unwrap();
        unsafe {
            // 10 bytes into volume 0 -> logical length 10.
            let d1 = [0xABu8; 10];
            let mut processed = 0u32;
            assert_eq!(
                vol_write(p, d1.as_ptr() as *const c_void, 10, &mut processed),
                S_OK
            );
            // Seek to virtual offset 350 (volume 3, offset 50). Volumes 1 and 2
            // were never written and must be materialized as zero-filled gaps.
            assert_eq!(vol_seek(p, 350, 0, std::ptr::null_mut()), S_OK);
            let d2 = [0xCDu8; 20];
            assert_eq!(
                vol_write(p, d2.as_ptr() as *const c_void, 20, &mut processed),
                S_OK
            );
            assert_eq!(finalize_volumes(p), Some(370));
            release_void_volumes(p);
        }

        for (index, expected) in [(1usize, 100u64), (2, 100), (3, 100), (4, 70)] {
            let path = PathBuf::from(format!("{}.{:03}", dest.display(), index));
            let len = std::fs::metadata(&path)
                .unwrap_or_else(|_| panic!("gap volume {} must exist", path.display()))
                .len();
            assert_eq!(len, expected, "volume {index} size");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
