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
use crate::error::{seek_target_checked, E_NEGATIVE_SEEK};
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
    if data.is_null() {
        // Writing `size > 0` bytes out of a null buffer is UB; the engine never
        // asks for that, so a failure here means an ABI/layout mismatch.
        return E_FAIL;
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
    let mut file = match (*this).file.lock() {
        Ok(g) => g,
        Err(_) => return E_FAIL,
    };
    // Same rule as the input stream: resolve the origin here, so a negative
    // offset cannot wrap into a huge forward seek reported as S_OK
    // (vendor/7zip-sdk/IStream.h requires an error before the stream start).
    let base: u64 = match origin {
        0 => 0,
        1 => match file.stream_position() {
            Ok(pos) => pos,
            Err(_) => return E_FAIL,
        },
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
    if data.is_null() {
        // See `write`: a non-zero write from a null buffer is UB.
        return E_FAIL;
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

/// 列举 `<dest>.001`、`.002`、… 这条**连续序列**里的普通文件，按编号升序。
///
/// 判定边界（删旧卷、改名让位都以它为准，因此只写在这里一处）：
/// - 只认文件名形如 `<dest 的文件名>.<至少 3 位 ASCII 数字>`，文件名部分
///   大小写不敏感（Windows 文件系统本来就不区分）；
/// - 连续性从 `.001` 起算，遇到缺号立刻停，绝不去碰间隔之外的编号
///   （`.999` 之类可能是别的程序的卷）；
/// - 序列里出现非普通文件（同名目录、符号链接/junction）也立刻停：那不是
///   我们写出来的卷，一律当作边界。
///
/// `dest` 本身可以不存在（分卷时它从不生成）；`dest` 没有文件名时报错。
pub fn existing_volume_series(dest: &Path) -> std::io::Result<Vec<PathBuf>> {
    use std::ffi::OsString;
    let Some(name) = dest.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "volume destination has no file name",
        ));
    };
    // 前缀 = dest 的文件名 + "."，例如 `C:\out\vol.7z.`。
    let mut prefix = OsString::from(name);
    prefix.push(".");
    let dir = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };

    // 先把目录里符合「<文件名>.<3 位以上纯数字>」的条目标出来，再用编号顺序
    // 去走连续性。这样编号之外的文件（`.999`、`.0010`、`-001`）都不会被当成
    // 序列的一部分。文件名比较在 Windows 上天然大小写不敏感，但这里显式折叠
    // 一次，免得依赖文件系统的行为。
    let want = name.to_string_lossy().to_ascii_lowercase();
    let needle = format!("{want}.");
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let Some(suffix) = lower.strip_prefix(&needle) else {
            continue;
        };
        if suffix.len() < 3 || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        present.insert(suffix.to_string());
    }

    // 从 .001 起走连续编号（卷名是 `{:03}`，所以 001..=999 是三位，1000 起
    // 是四位 —— 用格式化后的字符串去查表，两种长度都对）。
    let mut out = Vec::new();
    let mut index = 1usize;
    loop {
        let key = format!("{index:03}");
        if !present.contains(&key) {
            break;
        }
        let mut vol_name = prefix.clone();
        vol_name.push(&key);
        let path = dir.join(&vol_name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => out.push(path),
            // 序列里出现非文件（目录/链接）或已被删掉：序列到此为止。
            _ => break,
        }
        index += 1;
    }
    Ok(out)
}

/// 删掉上一次分卷创建留下的 `<dest>.001`、`.002`、… 连续序列。
///
/// **创建流程不再调用它。** 上一版在写任何字节之前调用，被实测出四条数据丢失
/// 路径：取消/失败/磁盘满会把旧归档删了又没写出新的；删到一半被占用就放弃，
/// 留下打不开的半套；两个窗口互删对方正在写的 `.001`；分卷覆盖还不走用户确认。
/// 现在分卷创建走「先写暂存卷 → 成功后改名替换旧序列 → 最后删备份」，见
/// `crate::create` 里的 `commit_volume_series`。这个函数保留给「明确要清空一
/// 套旧卷」的工具场景，**绝不能在写入之前拿它当覆盖手段**。
///
/// 安全边界（每一条都必须守住，否则会误删用户的东西）：
/// - `dest` 必须是绝对路径，否则拒绝（相对路径下「同目录扫描」含义不明）；
/// - 只删 [`existing_volume_series`] 认出的那一串普通文件；
/// - 删除失败如实返回错误，不假装成功。
pub fn clear_volumes(dest: &Path) -> std::io::Result<()> {
    if !dest.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "volume destination must be an absolute path",
        ));
    }
    for path in existing_volume_series(dest)? {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // 另一个进程刚删掉：序列已经干净，继续下一个。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Create a multi-volume output stream writing to `<prefix>001`, `<prefix>002`,
/// ... (the caller passes a prefix that already ends in `.`) with each volume at
/// most `limit` bytes. `limit` must be non-zero.
///
/// 分卷创建**不再**先清旧卷：调用方（`create.rs` 的 `commit_volume_series`）
/// 让它写到暂存前缀上，全部写完并校验成功之后才动用户可见的 `<dest>.NNN`。
/// 这里仍然用 `create_new`，它是一把「别覆盖任何已有文件」的保险。
pub fn new_volumes(prefix: &Path, limit: u64) -> std::io::Result<*mut c_void> {
    if limit == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "volume size must be non-zero",
        ));
    }
    let mut prefix = prefix.as_os_str().to_os_string();
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

/// 写入分卷时用的**暂存前缀**：`<dest>.zipnest-part-<pid>-<seq>.`
///
/// 与最终卷名 `<dest>.NNN` 只差中间这一段，所以暂存卷的编号与最终编号一一
/// 对应，替换阶段只需逐卷改名，不需要再拆字节。`<seq>` 由进程内计数器给出，
/// 同一进程里两次创建绝不会撞名；跨进程由 pid 区分。
///
/// 组件长度：Windows 的单个文件名上限是 255 字节（UTF-16 code unit）。暂存卷
/// 的实际文件名还要再拼一个 `.NNN` 编号（最短 4 字节：`.001`），所以这里按
/// `上限 - 编号预留 - 后缀` 截短基名，保证拼完仍在 255 以内。`seq` 至多 20 位
/// 十进制，预留得下。最终路径一律再经 `os_path()`（调用方负责）以支持超过
/// MAX_PATH 的目录。
pub fn staging_volume_prefix(dest: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const MAX_COMPONENT: usize = 255;
    /// 编号后缀的预留：`.001`（三位）到 `.1000`（四位）都放得下。
    const VOLUME_SUFFIX_RESERVE: usize = 5;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let suffix = format!(
        ".zipnest-part-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut base = name;
    if base.len() + suffix.len() + VOLUME_SUFFIX_RESERVE > MAX_COMPONENT {
        let mut cut = MAX_COMPONENT
            .saturating_sub(suffix.len())
            .saturating_sub(VOLUME_SUFFIX_RESERVE);
        while cut > 0 && !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
    }
    let mut tmp = std::ffi::OsString::from(base);
    tmp.push(suffix);
    dest.with_file_name(tmp)
}

/// 把 `<prefix>.001`、`.002`、… 这条连续序列里实际存在、且是普通文件的那些
/// 列出来（升序）。暂存卷只有我们自己写，所以这里不套
/// [`existing_volume_series`] 那套「防误删别人文件」的严格判定 —— 暂存前缀
/// 带 pid 与序号，本来就不属于任何别的程序。
pub fn written_volume_series(prefix: &Path) -> Vec<PathBuf> {
    let dir = match prefix.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let base = prefix
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = Vec::new();
    for index in 1usize.. {
        let mut name = std::ffi::OsString::from(&base);
        name.push(".");
        name.push(format!("{:03}", index));
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => out.push(path),
            _ => break,
        }
    }
    out
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

// ---------------------------------------------------------------------------
// 分卷创建的进程间互斥（同一 dest 只允许一个任务在替换文件）
// ---------------------------------------------------------------------------

/// 分卷创建正在「写暂存卷 → 改名替换」的那个 dest 的进程间锁。
///
/// 为什么必须有：Windows 允许删除仍被其他进程打开的文件，所以两个窗口对同一
/// dest 各开一个分卷任务时，上一版的「先删旧卷再写」会让 A 删掉 B 正在写的
/// `.001`，B 照样返回成功、磁盘上只剩孤卷。UI 侧这条路径可达
/// （`single_instance::is_busy()` 只是「忙就另开一个窗口」）。
///
/// 实现跟 `apps/zipnest-native/src/single_instance.rs` 同一个模型：Win32
/// **命名互斥量**（`CreateMutexW`）。它不会像锁文件那样在进程崩溃后变陈旧 ——
/// 进程一死，句柄由内核释放。
///
/// 命名：`Local\ZipNest.VolumeCreate.<规范化 dest 的 FNV-1a 64 位哈希>`。
/// - `Local\` 把作用域限在登录会话内（与单实例锁一致；同一台机器上不同用户
///   本来就在各自的目录里，也没必要跨会话互斥）。
/// - 互斥量名字最长 260 个字符（不含 `Local\` 前缀），把任意长路径拼进去会
///   直接创建失败，所以只放一个哈希。哈希只用于**选择名字**，不参与安全判定：
///   撞名只会让两个无关的 dest 短暂互斥，不会让它们互相覆盖（替换阶段仍按
///   各自的真实路径操作）。
/// - key 用**规范化后的路径**：绝对化（相对路径按当前目录补全，不做符号链接
///   解析 —— `std::fs::canonicalize` 会把仅大小写不同的写法折成同一个，也会
///   把不存在的 dest 变成错误）后按 Windows 的规则折叠大小写，再统一分隔符。
///
/// 超时策略：**不无限等**。两层都拿不到就返回一个可映射成
/// `error.volume_busy` 的错误。选 10 秒是权衡后的结果：正常任务拿着这把锁的
/// 时间只有「暂存卷改名 + 删备份」这一段（文件数 = 卷数，通常几毫秒），10 秒
/// 足以吸收另一个任务收尾时的抖动；再长就会让用户在「开始创建」后面无提示地
/// 干等，而我们没有可中途取消的进度出口。
///
/// 实现分两层，两层都不能少：
/// 1. **进程内**（[`IN_PROCESS`]）：Win32 互斥量对同一线程是递归的，只有它的话
///    同一进程里的两个任务（UI 的任务线程）会双双拿到「锁」然后照样互删。
/// 2. **进程间**：上面那个命名互斥量。
pub struct VolumeCreateLock {
    /// 规范化后的 dest key，`Drop` 里用它归还进程内名额。
    key: String,
    /// `CreateMutexW` 的句柄。`Drop` 里先 `ReleaseMutex` 再 `CloseHandle`。
    ///
    /// 刻意**不实现** `Send`：互斥量所有权属于「拿到它的那个线程」，`ReleaseMutex`
    /// 也只有该线程调用才有效。我们的用法是同一线程里 `acquire → 替换文件 →
    /// drop`，编译器会替我们守住这条约束。
    ///
    /// 若所有权停在 `ReleaseMutex` 之外（崩溃），内核在下一次等待时以
    /// `WAIT_ABANDONED` 通知，我们把它当「拿到了」处理，因此不会死锁。
    handle: *mut c_void,
}

/// 进程内的占用表：`key -> (持有线程, 重入次数)`。
///
/// 记线程与次数而不是单纯一个集合：同一线程重复 acquire 同一把锁必须成功
/// （否则 `Drop` 的配对就乱了），别的线程必须被挡住。
static IN_PROCESS: Mutex<Option<std::collections::HashMap<String, (u64, u32)>>> = Mutex::new(None);

/// 当前线程的稳定标识（只在本进程的占用表里用）。
fn thread_id() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    hasher.finish()
}

/// 在当前线程上占住进程内名额。`None` = 别的线程已经占着。
fn claim_in_process(key: &str) -> Option<()> {
    let mut slot = match IN_PROCESS.lock() {
        Ok(g) => g,
        // 占用表被毒化（这里没有会 panic 的临界区，理论上不会发生）：宁可放行，
        // 也不让所有分卷创建永久失败。
        Err(_) => return Some(()),
    };
    let map = slot.get_or_insert_with(Default::default);
    let me = thread_id();
    match map.get_mut(key) {
        Some((owner, count)) if *owner == me => {
            *count += 1;
            Some(())
        }
        Some(_) => None,
        None => {
            map.insert(key.to_string(), (me, 1));
            Some(())
        }
    }
}

/// 归还进程内名额（与 [`claim_in_process`] 成对调用）。
fn release_in_process(key: &str) {
    let Ok(mut slot) = IN_PROCESS.lock() else {
        return;
    };
    let Some(map) = slot.as_mut() else {
        return;
    };
    let me = thread_id();
    if let Some((owner, count)) = map.get_mut(key) {
        if *owner != me {
            return;
        }
        if *count > 1 {
            *count -= 1;
        } else {
            map.remove(key);
        }
    }
}

impl Drop for VolumeCreateLock {
    fn drop(&mut self) {
        // 先归还进程内的名额，再放内核互斥量。
        //
        // 这一句曾经缺失：`acquire_volume_lock_with_timeout` 会 `claim_in_process`，
        // 但没有任何地方 `release_in_process` —— 于是**同一进程里第一次分卷创建
        // 之后就再也拿不到这个 dest 的锁**，用户第二次对同一个位置分卷压缩时
        // 会立刻收到「另一个任务正在写这个位置」，重启应用才能恢复。这条路径
        // 只有进程内占用表能挡住同进程的第二个线程，所以漏掉它不会在单测里
        // 表现为内核锁失败，只会表现为「第二次就忙」。
        release_in_process(&self.key);
        #[cfg(windows)]
        if !self.handle.is_null() {
            unsafe {
                ReleaseMutex(self.handle);
                CloseHandle(self.handle);
            }
        }
    }
}

/// 这把锁等多久（毫秒）。见 [`VolumeCreateLock`] 上的说明。
pub const VOLUME_CREATE_LOCK_TIMEOUT_MS: u32 = 10_000;

/// 拿不到锁时返回的 `std::io::ErrorKind`。调用方把它映射成
/// `ZipnestError::VolumeBusy`（i18n key `error.volume_busy`）。
fn volume_busy_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "another volume create is already running for this destination",
    )
}

/// 规范化 dest 路径，只用于给互斥量取名。
///
/// 绝对化（不要求文件存在，所以不能用 `canonicalize`）→ 统一 `\` 分隔符 →
/// 折叠为小写。Windows 的文件名比较不区分大小写，`C:\out\Vol.7z` 与
/// `c:\OUT\vol.7z` 是同一个目标，必须落到同一把锁上。
fn volume_lock_key(dest: &Path) -> String {
    let absolute = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(dest))
            .unwrap_or_else(|_| dest.to_path_buf())
    };
    let text = absolute.to_string_lossy().replace('/', "\\");
    // 连续分隔符（`C:\\a`、尾部 `\`）折成一个，避免同一路径的两种拼法落成两把锁。
    let mut folded = String::with_capacity(text.len());
    let mut last_sep = false;
    for ch in text.chars() {
        let sep = ch == '\\';
        if sep && last_sep {
            continue;
        }
        last_sep = sep;
        folded.push(if sep { '\\' } else { ch.to_ascii_lowercase() });
    }
    folded.trim_end_matches('\\').to_string()
}

/// FNV-1a 64 位。只用来把长路径压成互斥量名字里的一个短 token，不需要密码学
/// 强度 —— 见 [`VolumeCreateLock`] 关于撞名的说明。
fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 给 `dest` 拿分卷创建锁。拿不到（另一个任务正持有）返回
/// `ErrorKind::WouldBlock`，调用方据此报 `error.volume_busy`。
///
/// 非 Windows 平台退化为无锁（本 crate 只在 Windows 交付；测试只在 Windows
/// 上跑分卷用例）。
pub fn acquire_volume_lock(dest: &Path) -> std::io::Result<VolumeCreateLock> {
    acquire_volume_lock_with_timeout(dest, VOLUME_CREATE_LOCK_TIMEOUT_MS)
}

/// [`acquire_volume_lock`] 的可指定超时版本。
///
/// 生产路径固定用 [`VOLUME_CREATE_LOCK_TIMEOUT_MS`]；拆出这一层是为了让测试
/// 能在不打十秒空等的情况下验证「拿不到锁会返回 `WouldBlock`」。
pub fn acquire_volume_lock_with_timeout(
    dest: &Path,
    timeout_ms: u32,
) -> std::io::Result<VolumeCreateLock> {
    let key = volume_lock_key(dest);
    // 先拿进程内名额：它挡住同一进程里别的线程，也让「同一线程重入」成为
    // 合法操作。挡不住就立刻返回，不用等十秒。
    if claim_in_process(&key).is_none() {
        return Err(volume_busy_error());
    }
    let name = format!("Local\\ZipNest.VolumeCreate.{:016x}", fnv1a64(&key));
    match acquire_named_lock(&name, &key, timeout_ms) {
        Ok(lock) => Ok(lock),
        Err(e) => {
            // 进程间没拿到：把进程内的名额还回去，否则这个 dest 在本进程里
            // 会一直被视作「有人占着」。
            release_in_process(&key);
            Err(e)
        }
    }
}

#[cfg(windows)]
fn acquire_named_lock(name: &str, key: &str, timeout_ms: u32) -> std::io::Result<VolumeCreateLock> {
    use std::os::windows::ffi::OsStrExt;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x80;
    const WAIT_TIMEOUT: u32 = 0x102;

    let wide: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // 与 `single_instance.rs` 同一写法：`CreateMutexW` 先创建/打开，冲突与否
    // 再看 `GetLastError`。即使 `ERROR_ALREADY_EXISTS`，拿到的也是同一对象：
    // 真正判归属的是随后的等待。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        // 连互斥量都建不出来（极少见：内存不足/名字不合法）。这里不能假装拿到
        // 了锁 —— 并发保护正是为了避免互删，宁可让调用方看到失败。
        return Err(std::io::Error::last_os_error());
    }
    let lock = VolumeCreateLock {
        key: key.to_string(),
        handle,
    };
    let waited = unsafe { WaitForSingleObject(handle, timeout_ms) };
    match waited {
        // 拿到（`WAIT_ABANDONED` 也归这里：上一个持有者没来得及 `ReleaseMutex`
        // 就退出了。互斥量没有被破坏，只是要接手继续 —— 而且此前的写入都在
        // 暂存前缀上，用户可见的卷没被动过）。
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(lock),
        WAIT_TIMEOUT => {
            // 没等到：`lock` 会在这里被 drop，Drop 负责关句柄。若直接
            // `return` 而不经过它，这个内核句柄就泄漏了 —— 每失败一次漏一个。
            drop(lock);
            Err(volume_busy_error())
        }
        _ => {
            // 无效句柄之类：把真实原因交出去，不冒充「忙」。
            let err = std::io::Error::last_os_error();
            drop(lock);
            Err(err)
        }
    }
}

#[cfg(not(windows))]
fn acquire_named_lock(_name: &str, key: &str, _timeout_ms: u32) -> std::io::Result<VolumeCreateLock> {
    Ok(VolumeCreateLock {
        key: key.to_string(),
        handle: std::ptr::null_mut(),
    })
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn ReleaseMutex(handle: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zn-outstream-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plain_seek_rejects_a_position_before_the_start_of_the_stream() {
        let dir = temp_dir("neg");
        let p = new(&dir.join("out.bin")).unwrap();
        let mut processed = 0u32;
        let body = [1u8, 2, 3, 4];
        unsafe {
            assert_eq!(
                write(p, body.as_ptr() as *const c_void, 4, &mut processed),
                S_OK
            );
            assert_eq!(processed, 4);
            assert_eq!(seek(p, -1, 0, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            assert_eq!(seek(p, -5, 1, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            assert_eq!(seek(p, -5, 2, std::ptr::null_mut()), E_NEGATIVE_SEEK);
            let mut pos = u64::MAX;
            assert_eq!(seek(p, -2, 1, &mut pos), S_OK);
            assert_eq!(pos, 2);
            assert_eq!(seek(p, 0, 0, &mut pos), S_OK);
            assert_eq!(pos, 0);
            assert_eq!(seek(p, -4, 2, &mut pos), S_OK);
            assert_eq!(pos, 0);
            assert_eq!(seek(p, 0, 9, std::ptr::null_mut()), E_FAIL);
            release_void(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_rejects_a_null_buffer_but_allows_a_zero_size_write() {
        let dir = temp_dir("null");
        let p = new(&dir.join("out.bin")).unwrap();
        let mut processed = 9u32;
        unsafe {
            assert_eq!(write(p, std::ptr::null(), 4, &mut processed), E_FAIL);
            assert_eq!(processed, 0);
            // A zero-size write from a null buffer is legal.
            assert_eq!(write(p, std::ptr::null(), 0, &mut processed), S_OK);
            assert_eq!(processed, 0);
            release_void(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn volume_write_rejects_a_null_buffer_but_allows_a_zero_size_write() {
        let dir = temp_dir("volnull");
        let p = new_volumes(&dir.join("out.7z"), 100).unwrap();
        let mut processed = 9u32;
        unsafe {
            assert_eq!(vol_write(p, std::ptr::null(), 4, &mut processed), E_FAIL);
            assert_eq!(processed, 0);
            assert_eq!(vol_write(p, std::ptr::null(), 0, &mut processed), S_OK);
            release_void_volumes(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extending_past_end_creates_and_sizes_gap_volumes() {
        let dir = temp_dir("volstream");
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

    /// 锁名只由「规范化后的 dest」决定：大小写不同、分隔符不同的同一路径必须
    /// 落到同一把锁上，否则两个窗口还是能各拿各的锁去互删。
    #[test]
    fn the_lock_key_ignores_case_and_separator_spelling() {
        assert_eq!(
            volume_lock_key(Path::new(r"C:\Out\Vol.7z")),
            volume_lock_key(Path::new("c:/out/vol.7z"))
        );
        // 连续分隔符与尾部反斜杠是同一条路径的另一种写法。
        assert_eq!(
            volume_lock_key(Path::new(r"C:\out\\vol.7z\")),
            volume_lock_key(Path::new(r"C:\out\vol.7z"))
        );
        // 不同的 dest 必须落到不同的锁（哈希之后再比）。
        assert_ne!(fnv1a64(&volume_lock_key(Path::new(r"C:\out\a.7z"))),
            fnv1a64(&volume_lock_key(Path::new(r"C:\out\b.7z"))));
    }

    /// 互斥量真的互斥：持有期间**别的线程**拿不到同一 dest 的锁（返回
    /// `WouldBlock`），释放之后立刻能拿到。超时用 0 毫秒，测试不为此等待。
    ///
    /// 换线程是必须的：Win32 互斥量对同一线程是递归的，同线程重复 acquire 本来
    /// 就会成功（那正是 `create → 回滚 → 重试` 需要的语义）。挡住并发任务的
    /// 是进程内占用表这一层。
    #[test]
    fn the_volume_lock_is_exclusive_per_destination() {
        let dest = std::env::temp_dir().join(format!("zn-lock-{}.7z", std::process::id()));
        let held = acquire_volume_lock_with_timeout(&dest, 0).expect("first acquire");
        let probe = dest.clone();
        // 另一个线程来抢：必须被挡住。
        let other = std::thread::spawn(move || acquire_volume_lock_with_timeout(&probe, 0).map(|_| ()))
            .join()
            .unwrap();
        match other {
            Ok(()) => panic!("another thread must not acquire the lock while it is held"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock,
                "contention must be reported as WouldBlock, got {e:?}"),
        }
        drop(held);
        let again = acquire_volume_lock_with_timeout(&dest, 0);
        assert!(again.is_ok(), "the lock must be available after release: {:?}", again.err());
    }

    /// 不同 dest 互不影响：一个被占着，另一个照样能拿。
    #[test]
    fn the_volume_lock_is_scoped_to_one_destination() {
        let dir = std::env::temp_dir();
        let a = dir.join(format!("zn-lock-a-{}.7z", std::process::id()));
        let b = dir.join(format!("zn-lock-b-{}.7z", std::process::id()));
        let _held = acquire_volume_lock_with_timeout(&a, 0).expect("acquire a");
        let b_lock = acquire_volume_lock_with_timeout(&b, 0);
        assert!(b_lock.is_ok(), "a different destination must not be blocked: {:?}", b_lock.err());
    }

    /// 暂存前缀与最终卷名只差中间那一段，编号一一对应。
    #[test]
    fn the_staging_prefix_keeps_the_numbering_contract() {
        let dest = Path::new(r"C:\out\vol.7z");
        let staging = staging_volume_prefix(dest);
        let name = staging.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("vol.7z.zipnest-part-"), "got {name}");
        assert_eq!(staging.parent(), dest.parent());
        // 两个前缀不会撞名（同一进程连续两次创建）。
        assert_ne!(staging, staging_volume_prefix(dest));
    }

    /// 超长文件名：拼上暂存后缀后组件仍在 255 字节以内。
    ///
    /// 代价是这个已经很长的名字会被截掉尾部（包括扩展名）—— 暂存文件不需要
    /// 保留扩展名，真正要紧的是它留在 `dest` 所在目录里、且编号后缀还放得下。
    #[test]
    fn the_staging_prefix_trims_an_over_long_component() {
        let long = "x".repeat(250);
        let dest = PathBuf::from(format!(r"C:\out\{long}.7z"));
        let staging = staging_volume_prefix(&dest);
        let name = staging.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() <= 255, "component is {} bytes", name.len());
        assert!(name.contains(".zipnest-part-"), "the marker must survive: {name}");
        // 编号后缀（`.001`）还要加得进去，否则暂存卷的路径会超限。
        assert!(name.len() + 4 <= 255, "no room for the volume suffix: {} bytes", name.len());
        assert_eq!(staging.parent(), dest.parent());
    }
}
