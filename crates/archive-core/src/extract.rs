//! Extraction paths: `read_entry` (in-memory) and `extract_to_disk`.
//!
//! Both drive `IInArchive::Extract` with our own
//! `IArchiveExtractCallback` implementations.
//!
//! Threading contract (vendor README 搂9): `GetStream`/`PrepareOperation`/
//! `SetOperationResult` are serialized by the handler; `SetTotal`/
//! `SetCompleted` (IProgress) *may* run on another thread concurrently.
//! The user progress closure therefore lives behind a `Mutex` and is only
//! invoked while holding it; flags shared across threads are atomics.

use crate::archive::Archive;
use crate::com::callbacks;
use crate::com::propvariant::PropVariant;
use crate::com::vtables::{ArchiveExtractCallbackVt, InArchiveVt, SeqOutStreamVt};
use crate::com::{
    qi_matches, Guid, Hresult, E_ABORT, E_FAIL, E_NOINTERFACE, E_NOTIMPL, S_OK,
    IID_IARCHIVE_EXTRACT_CALLBACK, IID_IPROGRESS, IID_ISEQ_OUT_STREAM, IID_IUNKNOWN,
};
use crate::error::ZipnestError;
use crate::types::{
    ArchiveEntry, EntryFailure, ExtractOptions, ExtractProgress, ExtractStats, OnConflict,
    RenamedEntry,
};
use archive_security::{plan_entry_paths, EntryPathFix, ExtractQuota};
use std::ffi::c_void;
use std::os::raw::c_void as RawCVoid;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// `NExtract::NOperationResult` values we care about.
const OP_OK: i32 = 0;
const OP_DATA_ERROR: i32 = 2;
const OP_CRC_ERROR: i32 = 3;
const OP_WRONG_PASSWORD: i32 = 9;
/// `NExtract::NAskMode::kExtract`.
const ASK_EXTRACT: i32 = 0;
/// `NExtract::NAskMode::kTest` — what the handler asks for when `Extract` was
/// called with `testMode != 0` (IArchive.h: "test files without writing to
/// outStream"). It is still a real decode; handing it our sink keeps progress,
/// cancellation and the zip-bomb quota identical to a real extraction.
const ASK_TEST: i32 = 1;

/// Blocked-reason codes in [`DiskState::blocked`].
const BLOCK_NONE: u8 = 0;
const BLOCK_QUOTA: u8 = 2;

// ===========================================================================
// In-memory output stream (ISequentialOutStream) — used by read_entry
// ===========================================================================

struct SinkState {
    buf: Mutex<Vec<u8>>,
    limit: u64,
    /// Set once the cap above was hit. `mem_set_completed` turns this into
    /// `E_ABORT` so the handler stops decompressing instead of running the
    /// rest of a (possibly huge) entry into a discarded buffer.
    truncated: AtomicBool,
}

#[repr(C)]
struct MemOutStream {
    vt: *const SeqOutStreamVt,
    refs: AtomicU32,
    state: Arc<SinkState>,
}

static MEM_OUT_VT: SeqOutStreamVt = SeqOutStreamVt {
    query_interface: sink_qi,
    add_ref: sink_add_ref,
    release: sink_release,
    write: sink_write,
};

unsafe extern "system" fn sink_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if qi_matches(&*riid, &[IID_IUNKNOWN, IID_ISEQ_OUT_STREAM]) {
        *ppv = this;
        sink_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut MemOutStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let this = this as *mut MemOutStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn sink_write(
    this: *mut c_void,
    data: *const RawCVoid,
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
        return E_FAIL;
    }
    let this = this as *mut MemOutStream;
    let state = &(*this).state;
    let chunk = std::slice::from_raw_parts(data as *const u8, size as usize);
    let mut buf = match state.buf.lock() {
        Ok(b) => b,
        Err(_) => return E_FAIL,
    };
    let remaining = state.limit.saturating_sub(buf.len() as u64) as usize;
    let take = chunk.len().min(remaining);
    buf.extend_from_slice(&chunk[..take]);
    if take < chunk.len() {
        // Cap reached: keep what we have and let the next progress tick stop
        // the handler (see `mem_set_completed`). Reporting full consumption
        // here is what makes the handler wind down cleanly instead of erroring.
        state.truncated.store(true, Ordering::Relaxed);
    }
    *processed = size; // engine sees full consumption; overflow is dropped
    S_OK
}

// ===========================================================================
// Shared: one crypto companion object per Extract call (password support)
// ===========================================================================

// The QI helper (`callbacks::qi_hand_out_crypto`) is shared by both extract
// callbacks below and by the update callback in `outcallback.rs`.

// ===========================================================================
// read_entry: extract one index into a memory sink
// ===========================================================================

#[repr(C)]
struct MemExtractCallback {
    vt: *const ArchiveExtractCallbackVt,
    refs: AtomicU32,
    sink: *mut MemOutStream,
    crypto: *mut c_void,
    op_res: AtomicI32,
    state: Arc<SinkState>,
}

static MEM_CB_VT: ArchiveExtractCallbackVt = ArchiveExtractCallbackVt {
    query_interface: cb_qi,
    add_ref: cb_add_ref,
    release: cb_release,
    set_total: noop_set_total,
    set_completed: mem_set_completed,
    get_stream: mem_get_stream,
    prepare_operation: noop_prepare,
    set_operation_result: cb_op_result,
};

unsafe extern "system" fn cb_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let this = this as *mut MemExtractCallback;
    if let Some(hr) = callbacks::qi_hand_out_crypto((*this).crypto, &*riid, ppv) {
        return hr;
    }
    if qi_matches(&*riid, &[IID_IUNKNOWN, IID_IPROGRESS, IID_IARCHIVE_EXTRACT_CALLBACK]) {
        *ppv = this as *mut c_void;
        cb_add_ref(this as *mut c_void);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn cb_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut MemExtractCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn cb_release(this: *mut c_void) -> u32 {
    let this = this as *mut MemExtractCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        sink_release((*this).sink as *mut c_void);
        callbacks::crypto_release_void((*this).crypto);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn noop_set_total(_this: *mut c_void, _total: u64) -> Hresult {
    S_OK
}

/// Stop the handler once the sink cap is hit: without this the engine would
/// decompress the whole entry and throw the overflow away (a bomb entry would
/// pin a CPU core and churn the disk for minutes on a preview).
unsafe extern "system" fn mem_set_completed(this: *mut c_void, _complete: *const u64) -> Hresult {
    let this = this as *mut MemExtractCallback;
    let state = &(*this).state;
    if state.truncated.load(Ordering::Relaxed) {
        E_ABORT
    } else {
        S_OK
    }
}

unsafe extern "system" fn noop_prepare(_this: *mut c_void, _ask: i32) -> Hresult {
    S_OK
}

unsafe extern "system" fn mem_get_stream(
    this: *mut c_void,
    _index: u32,
    out_stream: *mut *mut c_void,
    ask_extract_mode: i32,
) -> Hresult {
    if out_stream.is_null() {
        return E_FAIL;
    }
    let this = this as *mut MemExtractCallback;
    if ask_extract_mode == ASK_EXTRACT {
        sink_add_ref((*this).sink as *mut c_void);
        *out_stream = (*this).sink as *mut c_void;
    } else {
        *out_stream = std::ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn cb_op_result(this: *mut c_void, op_res: i32) -> Hresult {
    let this = this as *mut MemExtractCallback;
    (*this).op_res.store(op_res, Ordering::SeqCst);
    S_OK
}

/// Map `NExtract::NOperationResult` to a `ZipnestError`.
///
/// 三种结果，处理方式必须不同：
///
/// * `kWrongPassword`(9) —— 引擎自己判定「密钥不对」，依据是条目里存的校验值
///   （WinZip AES 的 2 字节校验值、ZipCrypto 的检查字节）。直接回
///   [`ZipnestError::PasswordIncorrect`]，让 UI 再问一次密码。
/// * `kDataError`(2) / `kCRCError`(3) —— 解出来的内容对不上，**既可能是密钥错，
///   也可能是数据坏**。分两种情况：
///   - `key_checked` 为真（ZIP 家族，见 `Archive::key_verified`）：引擎在解码
///     每条数据前已经验过密钥，密钥不对时它早就报 9 了。能走到这里说明密钥是
///     对的，坏的是数据 → [`ZipnestError::Corrupt`]。这正是本缺陷要的效果：
///     密码正确却遇到坏数据，用户不会再被反复要求重输密码。
///   - `key_checked` 为假（7z、RAR 等没有校验值的格式）：错密钥同样会落到
///     2/3。实测 7z 用 Copy 存数据时错密码报 3、用 LZMA2 压缩时报 2，与真的
///     数据损坏完全同形。既然分不开，就按「可能是密码错」处理 —— 宁可让打错
///     密码的人多试一次，也不能把密码打错的包判成「已损坏」而让他再也不试。
///
/// 没提供密码时维持原来的泛化错误：那条路径不涉及密钥，不属本次改动。
fn map_op_res(op_res: i32, password_provided: bool, key_checked: bool) -> ZipnestError {
    match op_res {
        OP_WRONG_PASSWORD => ZipnestError::PasswordIncorrect,
        OP_DATA_ERROR | OP_CRC_ERROR if password_provided && key_checked => {
            // 条目列表由 `extract_to_disk` 补上：它手上才有这次没写出来的条目。
            ZipnestError::Corrupt(Vec::new())
        }
        OP_DATA_ERROR | OP_CRC_ERROR if password_provided => ZipnestError::PasswordIncorrect,
        _ => ZipnestError::Engine(E_FAIL),
    }
}

/// Extract one entry into memory.
///
/// `max_bytes` caps the payload: once the cap is reached the handler is asked
/// to stop and the bytes collected so far are returned. Callers that care about
/// truncation compare the byte count against `ArchiveEntry::size`.
pub(crate) fn read_entry(
    arc: &Archive,
    index: u32,
    password: Option<String>,
    max_bytes: Option<u64>,
) -> Result<Vec<u8>, ZipnestError> {
    let limit = max_bytes.unwrap_or(u64::MAX);
    let state = Arc::new(SinkState {
        buf: Mutex::new(Vec::new()),
        limit,
        truncated: AtomicBool::new(false),
    });
    let sink = Box::into_raw(Box::new(MemOutStream {
        vt: &MEM_OUT_VT,
        refs: AtomicU32::new(1),
        state: Arc::clone(&state),
    }));
    let password_provided = password.is_some();
    let crypto = callbacks::crypto_new(password);
    let cb = Box::into_raw(Box::new(MemExtractCallback {
        vt: &MEM_CB_VT,
        refs: AtomicU32::new(1),
        sink,
        crypto: crypto as *mut c_void,
        op_res: AtomicI32::new(-1),
        state: Arc::clone(&state),
    }));

    let indices = [index];
    let hr = unsafe {
        let vt = &**(arc.raw() as *const *const InArchiveVt);
        (vt.extract)(arc.raw(), indices.as_ptr(), 1, 0, cb as *mut c_void)
    };

    let op_res = unsafe { (*cb).op_res.load(Ordering::SeqCst) };
    unsafe {
        (MEM_CB_VT.release)(cb as *mut c_void);
    }

    // Truncated at the caller's cap: the handler was aborted deliberately (see
    // `mem_set_completed`), so the partial payload is the answer, not an error.
    if state.truncated.load(Ordering::Relaxed) {
        let buf = state.buf.lock().map_err(|_| ZipnestError::Engine(E_FAIL))?;
        return Ok(buf.clone());
    }
    // COM success is `hr >= 0`: S_FALSE means "some items failed", and the
    // per-item result below is the precise answer for that case.
    if hr < 0 {
        // map_hresult: E_ABORT -> Cancelled, everything else -> Engine.
        return Err(crate::error::map_hresult(hr));
    }
    if op_res != OP_OK {
        return Err(if op_res < 0 {
            ZipnestError::Engine(E_FAIL)
        } else {
            map_op_res(op_res, password_provided, arc.key_verified())
        });
    }

    let buf = state.buf.lock().map_err(|_| ZipnestError::Engine(E_FAIL))?;
    Ok(buf.clone())
}

// ===========================================================================
// extract_to_disk: secure extraction with quota / progress / cancellation
// ===========================================================================

/// Immutable per-entry lookup data captured before `Extract` starts.
struct EntryMeta {
    index: u32,
    raw_path: String,
    /// Safe relative path this entry is written to. Computed once for the whole
    /// selection by [`plan_entry_paths`], so a weird name costs the user a
    /// renamed file and never a missing file.
    plan: String,
    /// How `plan` differs from `raw_path` (empty when identical).
    fixes: Vec<EntryPathFix>,
    is_dir: bool,
    /// Size the archive claims for this entry. Only used to keep the progress
    /// total honest when the entry never gets a stream (skipped or unwritable):
    /// otherwise the bar would stop short of 100% for the rest of the run.
    size: u64,
    mtime: Option<std::time::SystemTime>,
}

/// Progress closure slot. Invoked only while the `Mutex` is held, which
/// serializes IProgress threads against the serialized handler methods.
///
/// The closure reference lives on the caller's stack for the whole
/// synchronous `Extract` call. We box the `&mut dyn FnMut` value itself
/// (a sized 16-byte cell) so the struct carries only thin pointers;
/// [`call_progress`] is the concrete trampoline re-typing it.
struct ProgressCell {
    cell: *mut c_void, // Box<&mut dyn FnMut(&ExtractProgress) -> bool>
    call: fn(*mut c_void, &ExtractProgress) -> bool,
    last_path: String,
    total_bytes: u64,
}

fn call_progress(raw: *mut c_void, p: &ExtractProgress) -> bool {
    // The closure is arbitrary caller code. A panic escaping it would unwind
    // through the `extern "system"` progress callback that called us, and Rust
    // aborts the process at that boundary — the user would just see the app
    // vanish mid-extraction. Treat a panic as "stop", which the callers map to
    // `E_ABORT`/`Cancelled`.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let slot = raw as *mut &mut dyn FnMut(&ExtractProgress) -> bool;
        let f = &mut *slot;
        (**f)(p)
    }))
    .unwrap_or(false)
}

struct DiskState {
    dest: PathBuf,
    on_conflict: OnConflict,
    /// Test mode: `GetStream` hands out a discarding sink and nothing touches
    /// the destination (see [`test_get_stream`]).
    test_mode: bool,
    /// The archive's own web mark, when the run was asked to propagate it and
    /// the archive has one. Copied onto every file the run writes.
    motw_mark: Option<Vec<u8>>,
    /// How many files could not hold the mark (their filesystem refused it).
    motw_unmarked: AtomicU32,
    /// How many files got it.
    motw_marked: AtomicU32,
    /// Test mode: the entry whose bytes the handler is decoding right now, so
    /// the verdict in `SetOperationResult` can name it. The handler serializes
    /// `GetStream`/`SetOperationResult`, so one slot is enough.
    test_in_flight: Mutex<Option<(String, u64)>>,
    entries: Vec<EntryMeta>, // sorted by index
    quota: Mutex<ExtractQuota>,
    blocked: AtomicU8,    // BLOCK_*
    cancelled: AtomicU8,  // 1 when the user closure asked to stop
    skipped: Mutex<Vec<String>>,
    /// Entries the destination already held and the run moved aside as
    /// `name (n).ext` (conflict policy `rename`). Reported apart from
    /// `renamed` (name cleaning); see `ExtractStats::conflict_renamed`.
    conflict_renamed: Mutex<Vec<RenamedEntry>>,
    /// Entries that could not be written. Non-empty ⇒ the run failed; the
    /// caller reports a failure instead of pretending the extraction worked.
    failed: Mutex<Vec<EntryFailure>>,
    io_error: Mutex<Option<std::io::Error>>,
    first_op_res: AtomicI32,
    done_bytes: AtomicU64,
    /// The file the handler is writing right now. It lives in a temp sibling
    /// and only replaces its destination once the handler reports the entry as
    /// finished, so a failed/cancelled run never truncates an existing file.
    pending: Mutex<Option<PendingWrite>>,
    files_committed: AtomicU32,
    bytes_committed: AtomicU64,
    progress: Mutex<ProgressCell>,
}

/// Temp file backing the entry being extracted right now.
struct PendingWrite {
    tmp: PathBuf,
    dest: PathBuf,
    mtime: Option<std::time::SystemTime>,
    /// The archive's web mark to copy onto the finished file, when the run was
    /// asked to propagate one.
    motw: Option<Vec<u8>>,
    /// The archive's own name for this entry, so a failed commit can be
    /// reported with the name the user sees in the archive.
    raw: String,
}

impl DiskState {
    fn entry(&self, index: u32) -> Option<&EntryMeta> {
        self.entries
            .binary_search_by_key(&index, |e| e.index)
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Record an entry the user's own conflict policy leaves alone (an existing
    /// destination under `skip`). Deliberate, so it is reported separately from
    /// a failure.
    fn record_skip(&self, raw: &str) {
        if let Ok(mut s) = self.skipped.lock() {
            s.push(raw.to_string());
        }
    }

    /// Charge an entry that will never write a byte: skipped, unwritable, or a
    /// directory.
    ///
    /// `done_bytes` counts what actually reached the disk, and `total_bytes` is
    /// summed from the listing, so an entry that is skipped or fails before its
    /// stream opens used to leave the bar permanently short of the total — a
    /// run that skipped 3 000 existing files over 1 GiB sat at 12% and then
    /// "finished", with the ETA extrapolating from a fraction that could never
    /// reach 1. Charging the listed size keeps the denominator and the running
    /// total in the same units.
    fn account_unwritten(&self, meta: &EntryMeta) {
        self.done_bytes.fetch_add(meta.size, Ordering::Relaxed);
    }

    /// Record a per-entry failure and charge its bytes, returning the callback
    /// result the caller must pass on (`S_OK`: one bad entry never aborts the
    /// run, the post-checks turn `failed` into a reported error).
    fn fail_entry(&self, meta: &EntryMeta, reason: &'static str, detail: Option<String>) -> i32 {
        self.record_failure(&meta.raw_path, reason, detail);
        self.account_unwritten(meta);
        S_OK
    }

    /// Same for an entry the conflict policy leaves alone (`skip`).
    fn skip_entry(&self, meta: &EntryMeta) -> i32 {
        self.record_skip(&meta.raw_path);
        self.account_unwritten(meta);
        S_OK
    }

    /// Record an entry that is **not on disk**. Every one of these turns the run
    /// into a failure: the UI lists them with their reason instead of showing a
    /// "done" message over missing files.
    fn record_failure(&self, raw: &str, reason: &'static str, detail: Option<String>) {
        if let Ok(mut f) = self.failed.lock() {
            f.push(EntryFailure {
                path: raw.to_string(),
                reason,
                detail,
            });
        }
    }

    /// Record a fatal write error (disk full, device failure, …). Only the
    /// first one is kept; per-entry open failures stay non-fatal and go to
    /// `failed` instead.
    fn record_io_error(&self, e: &std::io::Error) {
        if let Ok(mut slot) = self.io_error.lock() {
            if slot.is_none() {
                *slot = Some(std::io::Error::new(e.kind(), e.to_string()));
            }
        }
    }

    /// Invoke the user closure; `false` arms the cancellation flag.
    fn tick(&self) -> bool {
        let cell = match self.progress.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        let path = cell.last_path.clone();
        let p = ExtractProgress {
            done_bytes: self.done_bytes.load(Ordering::Relaxed),
            total_bytes: cell.total_bytes,
            current_path: path,
        };
        let keep_going = (cell.call)(cell.cell, &p);
        if !keep_going {
            self.cancelled.store(1, Ordering::SeqCst);
        }
        keep_going
    }
}

/// Per-file output stream writing into the destination filesystem.
#[repr(C)]
struct FileOutStream {
    vt: *const SeqOutStreamVt,
    refs: AtomicU32,
    file: Mutex<std::fs::File>,
    state: Arc<DiskState>,
}

static FILE_OUT_VT: SeqOutStreamVt = SeqOutStreamVt {
    query_interface: fos_qi,
    add_ref: fos_add_ref,
    release: fos_release,
    write: fos_write,
};

unsafe extern "system" fn fos_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if qi_matches(&*riid, &[IID_IUNKNOWN, IID_ISEQ_OUT_STREAM]) {
        *ppv = this;
        fos_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn fos_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut FileOutStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn fos_release(this: *mut c_void) -> u32 {
    let this = this as *mut FileOutStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn fos_write(
    this: *mut c_void,
    data: *const RawCVoid,
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
        return E_FAIL;
    }
    let this = this as *mut FileOutStream;
    let st = &(*this).state;

    // Already blocked/cancelled: swallow the bytes so the handler can wind
    // down without surfacing a data error; post-checks report the real cause.
    if st.blocked.load(Ordering::SeqCst) != BLOCK_NONE
        || st.cancelled.load(Ordering::SeqCst) == 1
    {
        *processed = size;
        return S_OK;
    }

    let chunk = std::slice::from_raw_parts(data as *const u8, size as usize);
    // Charge the quota BEFORE touching the disk (zip-bomb guard).
    {
        let mut quota = match st.quota.lock() {
            Ok(q) => q,
            Err(_) => return E_FAIL,
        };
        if quota.charge(chunk.len() as u64).is_err() {
            st.blocked.store(BLOCK_QUOTA, Ordering::SeqCst);
            *processed = size;
            return S_OK;
        }
    }
    let mut file = match (*this).file.lock() {
        Ok(f) => f,
        Err(_) => return E_FAIL,
    };
    match std::io::Write::write_all(&mut *file, chunk) {
        Ok(()) => {
            st.done_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            *processed = size;
            S_OK
        }
        Err(e) => {
            st.record_io_error(&e);
            E_FAIL
        }
    }
}

/// The disk extract callback.
#[repr(C)]
struct DiskExtractCallback {
    vt: *const ArchiveExtractCallbackVt,
    refs: AtomicU32,
    state: Arc<DiskState>,
    crypto: *mut c_void,
}

static DISK_CB_VT: ArchiveExtractCallbackVt = ArchiveExtractCallbackVt {
    query_interface: disk_qi,
    add_ref: disk_add_ref,
    release: disk_release,
    set_total: disk_set_total,
    set_completed: disk_set_completed,
    get_stream: disk_get_stream,
    prepare_operation: noop_prepare,
    set_operation_result: disk_op_result,
};

unsafe extern "system" fn disk_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let this = this as *mut DiskExtractCallback;
    if let Some(hr) = callbacks::qi_hand_out_crypto((*this).crypto, &*riid, ppv) {
        return hr;
    }
    if qi_matches(&*riid, &[IID_IUNKNOWN, IID_IPROGRESS, IID_IARCHIVE_EXTRACT_CALLBACK]) {
        *ppv = this as *mut c_void;
        disk_add_ref(this as *mut c_void);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn disk_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut DiskExtractCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn disk_release(this: *mut c_void) -> u32 {
    let this = this as *mut DiskExtractCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        callbacks::crypto_release_void((*this).crypto);
        drop(Box::from_raw(this));
    }
    left
}

/// IProgress: return `E_ABORT` once cancelled/blocked so the handler stops.
unsafe extern "system" fn disk_set_total(this: *mut c_void, _total: u64) -> Hresult {
    disk_progress_gate(this as *mut DiskExtractCallback)
}

unsafe extern "system" fn disk_set_completed(
    this: *mut c_void,
    _complete: *const u64,
) -> Hresult {
    disk_progress_gate(this as *mut DiskExtractCallback)
}

unsafe fn disk_progress_gate(cb: *mut DiskExtractCallback) -> Hresult {
    let st = &(*cb).state;
    if st.cancelled.load(Ordering::SeqCst) == 1 || st.blocked.load(Ordering::SeqCst) != BLOCK_NONE
    {
        return E_ABORT;
    }
    if !st.tick() {
        return E_ABORT;
    }
    S_OK
}

// ===========================================================================
// Atomic per-entry writes
// ===========================================================================
//
// Every entry is written to `<name>.zipnest-part-<pid>` next to its target and
// only renamed into place once the handler reports the entry as successful.
// That is what makes these guarantees hold:
//   * a cancelled, quota-blocked or failed run can never truncate a file that
//     was already there (`File::create` on the real path used to do just that);
//   * a half-written file never appears under the entry's real name.

/// The path to hand to the operating system.
///
/// Windows refuses any path past `MAX_PATH` (260) unless it carries the
/// verbatim `\\?\` prefix, which in turn requires an absolute, normalized path
/// with backslash separators. Archive entry names can be 255 bytes long and the
/// destination can be deep, so the limit is easy to cross — a 300-character
/// entry used to fail with `os error 123` ("the filename, directory name, or
/// volume label syntax is incorrect") and the file was silently never written.
#[cfg(windows)]
fn os_path(p: &std::path::Path) -> PathBuf {
    /// Below this length nothing needs the prefix; keeping short paths plain
    /// keeps error messages and the existing behaviour unchanged.
    const LIMIT: usize = 240;
    let s = p.as_os_str().to_string_lossy();
    if s.len() < LIMIT || !p.is_absolute() || s.starts_with(r"\\?\") {
        return p.to_path_buf();
    }
    let mut out = String::with_capacity(s.len() + 8);
    match s.strip_prefix(r"\\") {
        // A UNC path needs the `UNC\` form, not `\\?\` + `\\`.
        Some(rest) => {
            out.push_str(r"\\?\UNC\");
            out.push_str(rest);
        }
        None => {
            out.push_str(r"\\?\");
            out.push_str(&s);
        }
    }
    PathBuf::from(out.replace('/', "\\"))
}

#[cfg(not(windows))]
fn os_path(p: &std::path::Path) -> PathBuf {
    p.to_path_buf()
}

/// `Path::exists`, but able to see past `MAX_PATH` on Windows.
fn os_exists(p: &std::path::Path) -> bool {
    std::fs::metadata(os_path(p)).is_ok()
}

/// Is any directory strictly between `root` and `path` a symlink or directory
/// junction?
///
/// `path`'s own last component is deliberately not checked: replacing a symlink
/// with a real file is harmless, writing *through* one is what escapes the
/// destination. Walking up from the entry costs one `symlink_metadata` call per
/// path component and nothing at all for a flat archive (no parent to check).
fn has_link_ancestor(root: &std::path::Path, path: &std::path::Path) -> bool {
    let mut dir = path.parent();
    while let Some(current) = dir {
        if current == root {
            return false;
        }
        // `is_symlink` covers Windows symlinks *and* mount points (junctions):
        // both are name-surrogate reparse points, and both redirect a write.
        if std::fs::symlink_metadata(os_path(current))
            .map(|md| md.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
        dir = current.parent();
    }
    false
}

/// Temp sibling for `dest`: same directory, so the final rename stays on one
/// volume and is atomic.
///
/// The name is built from the raw `OsStr` (not a `&str`), so a destination that
/// is not valid UTF-8 still gets a temp file instead of being silently skipped;
/// and it carries a per-process counter so two entries in one archive whose
/// names differ only through this suffix cannot collide.
///
/// The *whole* component has to fit in 255 characters, so the base name is cut
/// when the suffix would push it over: an entry name may itself be 255 bytes
/// long, and `name + suffix` would then be rejected by the filesystem
/// (`os error 123`) and that file would never be written.
fn temp_sibling(dest: &std::path::Path) -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    const MAX_COMPONENT: usize = 255;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = dest.file_name()?;
    let suffix = format!(
        ".zipnest-part-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut base = name.to_string_lossy().into_owned();
    if base.len() + suffix.len() > MAX_COMPONENT {
        let mut cut = MAX_COMPONENT.saturating_sub(suffix.len());
        while cut > 0 && !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
    }
    let mut tmp = std::ffi::OsString::from(base);
    tmp.push(suffix);
    Some(dest.with_file_name(tmp))
}

/// First free `name (n).ext` next to `dest`; used by [`OnConflict::Rename`].
///
/// `None` when every candidate up to 9999 is taken. The old version fell back to
/// `dest` itself in that case, i.e. the one policy whose whole point is "never
/// overwrite" silently overwrote.
fn unique_dest(dest: &std::path::Path) -> Option<PathBuf> {
    let parent = dest.parent().map(PathBuf::from).unwrap_or_default();
    let stem = dest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for n in 2..10_000u32 {
        let cand = parent.join(format!("{stem} ({n}){ext}"));
        if !os_exists(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Move a finished temp file over its destination and restore the archived
/// modification time. Returns the number of bytes committed.
#[allow(clippy::permissions_set_readonly_false)]
fn commit_pending(pw: &PendingWrite) -> std::io::Result<u64> {
    // A read-only destination makes the replace fail; clear the flag first so
    // one read-only file cannot abort the whole extraction. Remember what we
    // changed: if the replace then fails (target locked by another process, a
    // directory, disk full), the user's file must not silently come out of it
    // with the read-only bit stripped while its content never changed.
    let dest = os_path(&pw.dest);
    let tmp = os_path(&pw.tmp);
    let was_readonly = std::fs::metadata(&dest)
        .map(|md| md.permissions().readonly())
        .unwrap_or(false);
    if was_readonly {
        let mut perm = std::fs::metadata(&dest)?.permissions();
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(&dest, perm);
    }
    let len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        if was_readonly {
            // Put the attribute back exactly as we found it.
            if let Ok(md) = std::fs::metadata(&dest) {
                let mut perm = md.permissions();
                perm.set_readonly(true);
                let _ = std::fs::set_permissions(&dest, perm);
            }
        }
        return Err(e);
    }
    if let Some(mtime) = pw.mtime {
        if let Ok(f) = std::fs::File::options().write(true).open(&dest) {
            let _ = f.set_modified(mtime);
        }
    }
    // The web mark goes on last, after the rename and the timestamp, so the
    // stream belongs to the finished file (and a mark write cannot be undone by
    // a later step failing).
    if let Some(mark) = &pw.motw {
        let marked = crate::motw::write_to(Path::new(&pw.dest), mark);
        PW_MARKED.store(marked, Ordering::Relaxed);
    }
    Ok(len)
}

/// Set by `commit_pending`: did the last commit manage to write the web mark?
/// (`commit_pending` has no channel back to `DiskState`, and the caller reads
/// this immediately after the call on the same thread — the handler serializes
/// `SetOperationResult`.)
static PW_MARKED: AtomicBool = AtomicBool::new(true);

// ===========================================================================
// Test mode: decode every selected entry, write nothing
// ===========================================================================

/// Output stream for test mode: charges the quota, counts the bytes, and
/// discards them.
///
/// The point of handing the engine a real stream instead of letting it use its
/// internal "no stream = just check" path is that the CRC/size checks and the
/// progress numbers stay exactly the same as a real extraction — the engine
/// decompresses the bytes either way. `io::sink` costs nothing, so the only
/// cost versus `testMode = 1` is one memcpy per chunk.
#[repr(C)]
struct SinkOutStream {
    vt: *const SeqOutStreamVt,
    refs: AtomicU32,
    state: Arc<DiskState>,
}

static SINK_OUT_VT: SeqOutStreamVt = SeqOutStreamVt {
    query_interface: sinkout_qi,
    add_ref: sinkout_add_ref,
    release: sinkout_release,
    write: sinkout_write,
};

unsafe extern "system" fn sinkout_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if qi_matches(&*riid, &[IID_IUNKNOWN, IID_ISEQ_OUT_STREAM]) {
        *ppv = this;
        sinkout_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn sinkout_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut SinkOutStream;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn sinkout_release(this: *mut c_void) -> u32 {
    let this = this as *mut SinkOutStream;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn sinkout_write(
    this: *mut c_void,
    data: *const RawCVoid,
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
        return E_FAIL;
    }
    let this = this as *mut SinkOutStream;
    let st = &(*this).state;
    if st.cancelled.load(Ordering::SeqCst) == 1 {
        *processed = size;
        return S_OK;
    }
    // Even though nothing is written, the bytes count against the zip-bomb
    // quota: a test run must not sit decompressing a 100 GB bomb for an hour.
    {
        let mut quota = match st.quota.lock() {
            Ok(q) => q,
            Err(_) => return E_FAIL,
        };
        if quota.charge(size as u64).is_err() {
            st.blocked.store(BLOCK_QUOTA, Ordering::SeqCst);
            *processed = size;
            return S_OK;
        }
    }
    st.done_bytes.fetch_add(size as u64, Ordering::Relaxed);
    *processed = size;
    S_OK
}

/// The disk callback's `GetStream`, for test mode: no destination, no temp
/// file, no conflict question — just a sink that counts and discards.
unsafe fn test_get_stream(
    cb: *mut DiskExtractCallback,
    index: u32,
    out_stream: *mut *mut c_void,
    ask_extract_mode: i32,
) -> Hresult {
    let st = &(*cb).state;
    if st.cancelled.load(Ordering::SeqCst) == 1 {
        return E_ABORT;
    }
    if st.blocked.load(Ordering::SeqCst) != BLOCK_NONE {
        return S_OK;
    }
    if ask_extract_mode != ASK_EXTRACT && ask_extract_mode != ASK_TEST {
        return S_OK;
    }
    let meta = match st.entry(index) {
        Some(m) => m,
        None => return E_FAIL,
    };
    if meta.is_dir {
        st.account_unwritten(meta);
        return S_OK;
    }
    {
        let mut cell = match st.progress.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        cell.last_path = meta.raw_path.clone();
    }
    if !st.tick() {
        return E_ABORT;
    }
    {
        let mut slot = match st.test_in_flight.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        *slot = Some((meta.raw_path.clone(), meta.size));
    }
    let stream = Box::into_raw(Box::new(SinkOutStream {
        vt: &SINK_OUT_VT,
        refs: AtomicU32::new(1),
        state: Arc::clone(st),
    }));
    *out_stream = stream as *mut c_void;
    S_OK
}

unsafe extern "system" fn disk_get_stream(
    this: *mut c_void,
    index: u32,
    out_stream: *mut *mut c_void,
    ask_extract_mode: i32,
) -> Hresult {
    if out_stream.is_null() {
        return E_FAIL;
    }
    *out_stream = std::ptr::null_mut();
    let cb = this as *mut DiskExtractCallback;
    let st = &(*cb).state;
    if st.test_mode {
        return test_get_stream(cb, index, out_stream, ask_extract_mode);
    }
    if st.cancelled.load(Ordering::SeqCst) == 1 {
        return E_ABORT;
    }
    if st.blocked.load(Ordering::SeqCst) != BLOCK_NONE {
        return S_OK; // skip; the post-check reports the real cause
    }
    if ask_extract_mode != ASK_EXTRACT {
        return S_OK;
    }

    let meta = match st.entry(index) {
        Some(m) => m,
        None => return E_FAIL,
    };

    // Progress tick for this item (also the first cancel point).
    {
        let mut cell = match st.progress.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        cell.last_path = meta.raw_path.clone();
    }
    if !st.tick() {
        return E_ABORT;
    }

    // The path was mapped from the RAW archive name before `Extract` started
    // (`plan_entry_paths`): odd names are rewritten, never dropped, so no entry
    // reaches this point without a safe relative path.
    let full: PathBuf = st.dest.join(&meta.plan);
    if !full.starts_with(&st.dest) {
        // Double insurance (symlink-style escapes cannot happen here, but
        // never write outside dest even if join semantics change).
        return st.fail_entry(meta, "error.entry_path", None);
    }

    if meta.is_dir {
        // Directories are idempotent and never conflict. One we cannot create
        // is a real failure: its files cannot land either.
        if let Err(e) = std::fs::create_dir_all(os_path(&full)) {
            return st.fail_entry(meta, "error.entry_io", Some(e.to_string()));
        }
        st.account_unwritten(meta);
        return S_OK;
    }

    // Existing destination: apply the conflict policy.
    let target = if os_exists(&full) {
        match st.on_conflict {
            OnConflict::Skip => {
                // Null stream → the engine leaves the file alone.
                return st.skip_entry(meta);
            }
            OnConflict::Rename => match unique_dest(&full) {
                Some(free) => {
                    // The file the user will find under a different name. The
                    // relative path is what the result window shows, next to
                    // the archive's own name for the entry.
                    let to = free
                        .strip_prefix(&st.dest)
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| free.to_string_lossy().into_owned());
                    if let Ok(mut list) = st.conflict_renamed.lock() {
                        list.push(RenamedEntry {
                            from: meta.raw_path.clone(),
                            to,
                        });
                    }
                    free
                }
                // Every `name (n).ext` up to 9999 exists. The policy promises
                // never to overwrite, so this entry cannot be written — which is
                // a failure to report, not something to hide behind "done".
                None => return st.fail_entry(meta, "error.entry_conflict", None),
            },
            OnConflict::Overwrite => full.clone(),
        }
    } else {
        full.clone()
    };

    // Per-entry failures below stay non-fatal for the *run*: one unwritable
    // path must not throw away every other file in the archive. They land in
    // `stats.failed`, which makes the job report a failure with these names.
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(os_path(parent)) {
            return st.fail_entry(meta, "error.entry_io", Some(e.to_string()));
        }
    }
    // The planner vets the entry *name* only. An archive can still ship a link
    // entry (`d` -> somewhere else) and then write through it: the lexical
    // `starts_with(dest)` check passes, `create_dir_all` is a no-op because the
    // link already exists, and the real file lands outside the destination.
    // Refuse to write through a symlink or directory junction anywhere between
    // the destination root and this entry. (On Windows creating a symlink needs
    // SeCreateSymbolicLinkPrivilege, so this is defence in depth rather than an
    // open door; junctions need no privilege.)
    if has_link_ancestor(&st.dest, &target) {
        return st.fail_entry(meta, "error.entry_link", None);
    }
    let Some(tmp) = temp_sibling(&target) else {
        return st.fail_entry(meta, "error.entry_path", None);
    };
    let file = match std::fs::File::create(os_path(&tmp)) {
        Ok(f) => f,
        Err(e) => return st.fail_entry(meta, "error.entry_io", Some(e.to_string())),
    };
    {
        // The handler serializes GetStream/SetOperationResult, so at most one
        // entry is in flight and a single slot is enough.
        let mut slot = match st.pending.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        // Defensive: a handler that opened a second stream for the same entry
        // must not leak the first temp file.
        if let Some(old) = slot.take() {
            let _ = std::fs::remove_file(os_path(&old.tmp));
        }
        *slot = Some(PendingWrite {
            tmp,
            dest: target,
            mtime: meta.mtime,
            motw: st.motw_mark.clone(),
            raw: meta.raw_path.clone(),
        });
    }

    let stream = Box::into_raw(Box::new(FileOutStream {
        vt: &FILE_OUT_VT,
        refs: AtomicU32::new(1),
        file: Mutex::new(file),
        state: Arc::clone(st),
    }));
    // The initial ref is the engine's: it owns the pointer we hand out and
    // releases it when the entry completes (COM caller-owns convention).
    *out_stream = stream as *mut c_void;
    S_OK
}

unsafe extern "system" fn disk_op_result(this: *mut c_void, op_res: i32) -> Hresult {
    let cb = this as *mut DiskExtractCallback;
    let st = &(*cb).state;
    if op_res != OP_OK {
        let _ = st.first_op_res.compare_exchange(0, op_res, Ordering::SeqCst, Ordering::SeqCst);
    }

    // Test mode: the handler serializes GetStream → SetOperationResult, so the
    // entry recorded by `test_get_stream` is the one this verdict is about.
    if st.test_mode {
        let in_flight = match st.test_in_flight.lock() {
            Ok(mut slot) => slot.take(),
            Err(p) => p.into_inner().take(),
        };
        if let Some((raw, size)) = in_flight {
            let clean = st.cancelled.load(Ordering::SeqCst) == 0
                && st.blocked.load(Ordering::SeqCst) == BLOCK_NONE;
            if op_res == OP_OK {
                // Verified: count it exactly like a written file, so a
                // successful test reports "N files verified" through the same
                // `files` field.
                st.files_committed.fetch_add(1, Ordering::Relaxed);
                st.bytes_committed.fetch_add(size, Ordering::Relaxed);
            } else if clean {
                // The engine decoded this entry to a bad CRC or a failed
                // decode. That entry is the reason the run fails, and it must
                // be named — "everything is fine" over corrupt data is the one
                // thing a test must never say.
                st.record_failure(&raw, "error.corrupt", None);
            }
        }
        if !st.tick() {
            return E_ABORT;
        }
        if st.cancelled.load(Ordering::SeqCst) == 1
            || st.blocked.load(Ordering::SeqCst) != BLOCK_NONE
        {
            return E_ABORT;
        }
        return S_OK;
    }

    // Finalize the entry that was just written. A complete entry that was not
    // cancelled or quota-blocked replaces its destination; anything else is
    // thrown away, leaving whatever the destination held before untouched.
    let pending = match st.pending.lock() {
        Ok(mut p) => p.take(),
        Err(p) => p.into_inner().take(),
    };
    if let Some(pw) = pending {
        let complete = op_res == OP_OK
            && st.cancelled.load(Ordering::SeqCst) == 0
            && st.blocked.load(Ordering::SeqCst) == BLOCK_NONE;
        if complete {
            // Assume a mark write will succeed; `commit_pending` flips this to
            // false when the filesystem refused it.
            PW_MARKED.store(true, Ordering::Relaxed);
            match commit_pending(&pw) {
                Ok(len) => {
                    st.files_committed.fetch_add(1, Ordering::Relaxed);
                    st.bytes_committed.fetch_add(len, Ordering::Relaxed);
                    if st.motw_mark.is_some() {
                        if PW_MARKED.load(Ordering::Relaxed) {
                            st.motw_marked.fetch_add(1, Ordering::Relaxed);
                        } else {
                            st.motw_unmarked.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => {
                    // The entry's bytes never reached their destination (target
                    // locked, path is a directory, disk full). Keep going with
                    // the remaining entries, but this run is a failure.
                    let _ = std::fs::remove_file(os_path(&pw.tmp));
                    st.record_failure(&pw.raw, "error.entry_io", Some(e.to_string()));
                }
            }
        } else {
            let _ = std::fs::remove_file(os_path(&pw.tmp));
            // 引擎说这个条目的数据不对（CRC/解码失败）或密码不对 —— 两种情况
            // 下这个文件都没写出来。记进 failed，`extract_to_disk` 才能把它们
            // 连同最终错误一起交给 UI，而不是在致命路径上丢掉这份名单。
            if op_res != OP_OK
                && st.cancelled.load(Ordering::SeqCst) == 0
                && st.blocked.load(Ordering::SeqCst) == BLOCK_NONE
            {
                st.record_failure(&pw.raw, "error.corrupt", None);
            }
        }
    }

    // Per-item cancel opportunity.
    if !st.tick() {
        return E_ABORT;
    }
    if st.cancelled.load(Ordering::SeqCst) == 1 || st.blocked.load(Ordering::SeqCst) != BLOCK_NONE
    {
        return E_ABORT;
    }
    S_OK
}

/// The entries an extraction request covers, sorted by archive index.
///
/// An empty selection means "the whole archive". This lives on its own — and
/// is shared with [`conflicts_on_disk`] — because the pre-flight question
/// ("does anything in the destination already have one of these names?") has to
/// be asked about *exactly* the same set the run will write. Two answers to
/// "what is selected" would eventually differ, and the user would be told
/// there is nothing in the way about files that were never checked.
fn select_entries_of(all: &[ArchiveEntry], indices: &[u32]) -> Vec<ArchiveEntry> {
    let mut selected: Vec<ArchiveEntry> = if indices.is_empty() {
        all.to_vec()
    } else {
        let want: std::collections::HashSet<u32> = indices.iter().copied().collect();
        all.iter().filter(|e| want.contains(&e.index)).cloned().collect()
    };
    selected.sort_by_key(|e| e.index);
    selected.dedup_by_key(|e| e.index);
    selected
}

/// Resolve every destination name before a single byte is written.
///
/// This is what makes the promise "success means every entry is on disk" hold:
/// a hostile or merely Windows-unfriendly name is rewritten here (and
/// reported), so the write path never has to give up on an entry.
///
/// [`conflicts_on_disk`] runs the same function, which is the point: the name
/// the pre-flight answer is about and the name the file actually lands under
/// are produced by one piece of code, so a change to the renaming rules cannot
/// make the question and the write disagree.
fn plan_entries(selected: &[ArchiveEntry]) -> Vec<EntryMeta> {
    let names: Vec<(String, bool)> = selected
        .iter()
        .map(|e| (e.path.clone(), e.is_dir))
        .collect();
    selected
        .iter()
        .zip(plan_entry_paths(&names))
        .map(|(e, p)| EntryMeta {
            index: e.index,
            raw_path: e.path.clone(),
            plan: p.path,
            fixes: p.fixes,
            is_dir: e.is_dir,
            size: e.size,
            mtime: e.mtime,
        })
        .collect()
}

/// How many of the planned files already exist under `opts.dest`.
///
/// See [`conflicts_in`] — this is the `Archive`-taking wrapper used by callers
/// that hold the archive open (tests, tooling).
pub fn conflicts_on_disk(arc: &Archive, opts: &ExtractOptions) -> Result<u64, ZipnestError> {
    let all = arc.entries()?;
    Ok(conflicts_in(&all, &opts.entries, &opts.dest))
}

/// How many of `indices` would land on a file that is already in `dest`.
///
/// This is the question the UI has to answer *before* it asks the user
/// "覆盖 / 跳过 / 自动重命名": asking when nothing is in the way is noise, and
/// v0.4.38 asked unconditionally (the dialog was opened by the button press,
/// never by what was on disk).
///
/// An empty `indices` means "the whole archive", exactly like
/// [`ExtractOptions::entries`]. Only file entries count — a directory is
/// idempotent (`create_dir_all`), and "replace this folder with another one"
/// is not an answer a user can meaningfully give.
///
/// **Every folder is listed once, not probed per entry.** `stat`-ing each name
/// costs ~17 µs per miss on this machine (measured), which is 1.7 s for a
/// 100 000-entry archive; one `read_dir` returns the whole folder for less than
/// that, and an archive's names cluster heavily into a handful of folders.
/// Whatever is not in the listing cannot exist, so this is exact, not an
/// approximation — and it is case-insensitive, which is what a Windows
/// filesystem is: `A.TXT` in the folder does block `a.txt`. (On a directory
/// with NTFS per-directory case sensitivity turned on this over-reports, which
/// asks the user a question rather than replacing a file unasked — the safe
/// direction.)
pub fn conflicts_in(entries: &[ArchiveEntry], indices: &[u32], dest: &std::path::Path) -> u64 {
    conflicts_in_counted(entries, indices, dest).0
}

/// [`conflicts_in`], also returning how many folders had to be read.
///
/// The second number is the property worth pinning in a test: it must be the
/// number of distinct folders in the selection, never the number of files.
/// A wall-clock assertion would measure the planner in a debug build instead of
/// the disk, so the test counts folders.
fn conflicts_in_counted(
    entries: &[ArchiveEntry],
    indices: &[u32],
    dest: &std::path::Path,
) -> (u64, usize) {
    if !os_exists(dest) {
        // Nothing there at all: every entry is a fresh write.
        return (0, 0);
    }
    let selected = select_entries_of(entries, indices);
    let mut listings: std::collections::HashMap<PathBuf, Listing> =
        std::collections::HashMap::new();
    let mut count = 0u64;
    for meta in plan_entries(&selected) {
        if meta.is_dir {
            continue;
        }
        let full = dest.join(&meta.plan);
        let Some(parent) = full.parent() else {
            continue;
        };
        let listing = listings
            .entry(parent.to_path_buf())
            .or_insert_with(|| Listing::read(parent));
        // A folder `create_dir_all` will make is empty, so nothing in it can be
        // in the way; `dest` itself is known to exist.
        if listing.contains(full.file_name(), dest == parent || listing.exists) {
            count += 1;
        }
    }
    (count, listings.len())
}

/// One directory's entries, or the fact that it is not there.
struct Listing {
    exists: bool,
    names: std::collections::HashSet<String>,
}

impl Listing {
    fn read(dir: &std::path::Path) -> Self {
        let Ok(rd) = std::fs::read_dir(os_path(dir)) else {
            return Self {
                exists: false,
                names: std::collections::HashSet::new(),
            };
        };
        Self {
            exists: true,
            // Lowercased: the destination is a Windows filesystem, where the
            // names are case-insensitive, and a pre-flight answer that ignored
            // that would promise a free name the run then has to rename.
            names: rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(|n| n.to_lowercase()))
                .collect(),
        }
    }

    /// `parent_known` is pass-through for the destination root, which is
    /// already known to exist; for any other folder an absent one answers "no".
    fn contains(&self, name: Option<&std::ffi::OsStr>, parent_known: bool) -> bool {
        if !parent_known || !self.exists {
            return false;
        }
        name.and_then(|n| n.to_str())
            .is_some_and(|n| self.names.contains(&n.to_lowercase()))
    }
}

// DiskState carries the progress-closure raw cell, so clippy sees a
// non-Send Arc. The cell lives exactly for this call: 7z invokes our
// callbacks synchronously on this thread, and it is freed before return.
#[allow(clippy::arc_with_non_send_sync)]
pub(crate) fn extract_to_disk(
    arc: &Archive,
    opts: &ExtractOptions,
    password: Option<&str>,
    progress: &mut dyn FnMut(&ExtractProgress) -> bool,
) -> Result<ExtractStats, ZipnestError> {
    // Resolve the selection once; handlers get a sorted, deduped index list.
    let all = arc.entries()?;
    let selected = select_entries_of(&all, &opts.entries);

    let total_bytes: u64 = selected.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let entries = plan_entries(&selected);
    let renamed: Vec<RenamedEntry> = entries
        .iter()
        .filter(|e| !e.fixes.is_empty())
        .map(|e| RenamedEntry {
            from: e.raw_path.clone(),
            to: e.plan.clone(),
        })
        .collect();

    // Read the archive's own web mark once, up front (A5). The source file is
    // still open and readable here; a filesystem that cannot answer is simply
    // "no mark to propagate".
    let motw_mark = if opts.preserve_motw && !opts.test_mode {
        let src = std::path::Path::new(arc.source_path());
        crate::motw::read_from(src)
    } else {
        None
    };
    let state = Arc::new(DiskState {
        motw_mark,
        motw_unmarked: AtomicU32::new(0),
        motw_marked: AtomicU32::new(0),
        dest: opts.dest.clone(),
        on_conflict: opts.on_conflict,
        test_mode: opts.test_mode,
        test_in_flight: Mutex::new(None),
        entries,
        quota: Mutex::new(ExtractQuota::new(opts.max_total_bytes)),
        blocked: AtomicU8::new(BLOCK_NONE),
        cancelled: AtomicU8::new(0),
        skipped: Mutex::new(Vec::new()),
        conflict_renamed: Mutex::new(Vec::new()),
        failed: Mutex::new(Vec::new()),
        io_error: Mutex::new(None),
        first_op_res: AtomicI32::new(0),
        done_bytes: AtomicU64::new(0),
        pending: Mutex::new(None),
        files_committed: AtomicU32::new(0),
        bytes_committed: AtomicU64::new(0),
        progress: Mutex::new(ProgressCell {
            cell: Box::into_raw(Box::new(progress)) as *mut c_void,
            call: call_progress,
            last_path: String::new(),
            total_bytes,
        }),
    });

    let crypto = callbacks::crypto_new(password.map(|s| s.to_string()));
    let cb = Box::into_raw(Box::new(DiskExtractCallback {
        vt: &DISK_CB_VT,
        refs: AtomicU32::new(1),
        state: Arc::clone(&state),
        crypto: crypto as *mut c_void,
    }));

    let indices: Vec<u32> = selected.iter().map(|e| e.index).collect();
    let hr = unsafe {
        let vt = &**(arc.raw() as *const *const InArchiveVt);
        (vt.extract)(
            arc.raw(),
            indices.as_ptr(),
            indices.len() as u32,
            // Test mode uses the engine's own "just verify" flag. The sink
            // still gets the bytes for the handlers that call `GetStream`
            // either way — see `test_get_stream` — so quota, progress and the
            // per-entry CRC failures behave the same as a real run; this flag
            // additionally tells the handlers not to look for a destination.
            i32::from(opts.test_mode),
            cb as *mut c_void,
        )
    };
    unsafe {
        (DISK_CB_VT.release)(cb as *mut c_void);
        // Free the boxed closure cell; no callbacks can fire after Extract.
        let cell = state.progress.lock().map(|c| c.cell).unwrap_or(std::ptr::null_mut());
        if !cell.is_null() {
            drop(Box::from_raw(
                cell as *mut &mut dyn FnMut(&ExtractProgress) -> bool,
            ));
        }
    }
    // Defensive: if the handler bailed out without a final SetOperationResult,
    // the temp file of that entry must not survive.
    {
        let leftover = match state.pending.lock() {
            Ok(mut p) => p.take(),
            Err(p) => p.into_inner().take(),
        };
        if let Some(pw) = leftover {
            let _ = std::fs::remove_file(os_path(&pw.tmp));
        }
    }

    // Post-check order matters: local flags win over whatever HRESULT the
    // handler chose to return (some handlers swallow our E_ABORT, and an
    // abort *we* triggered must report its real cause).
    if state.cancelled.load(Ordering::SeqCst) == 1 {
        return Err(ZipnestError::Cancelled);
    }
    if state.blocked.load(Ordering::SeqCst) == BLOCK_QUOTA {
        return Err(ZipnestError::QuotaExceeded);
    }
    if let Some(e) = state.io_error.lock().ok().and_then(|mut s| s.take()) {
        return Err(ZipnestError::Io(e));
    }
    if matches!(crate::error::map_hresult(hr), ZipnestError::Cancelled) {
        return Err(ZipnestError::Cancelled);
    }
    let op_res = state.first_op_res.load(Ordering::SeqCst);
    if op_res != 0 {
        let mut err = map_op_res(op_res, password.is_some(), arc.key_verified());
        // Test mode: no password is involved at all (an encrypted entry with
        // no password is refused before the job starts), so a failed decode is
        // the data being wrong — never a wrong key. Without this, CRC errors on
        // an unencrypted archive surfaced as the generic `error.engine`.
        if opts.test_mode && matches!(op_res, OP_DATA_ERROR | OP_CRC_ERROR) {
            err = ZipnestError::Corrupt(Vec::new());
        }
        // 条目名单一走致命路径就会丢：下面那些 `?` 之外没人再读 `state.failed`。
        // 「哪些条目没写出来」是用户唯一的线索，所以在这里把名单挂到错误上，
        // 让 UI 能像 `error.entries_failed` 那样逐条列出来。
        if let ZipnestError::Corrupt(ref mut entries) = err {
            if let Ok(mut f) = state.failed.lock() {
                entries.append(&mut f);
            }
        }
        return Err(err);
    }
    if hr < 0 {
        return Err(crate::error::map_hresult(hr));
    }

    let skipped = state
        .skipped
        .lock()
        .map(|mut s| std::mem::take(&mut *s))
        .unwrap_or_default();
    let conflict_renamed = state
        .conflict_renamed
        .lock()
        .map(|mut s| std::mem::take(&mut *s))
        .unwrap_or_default();
    let failed = state
        .failed
        .lock()
        .map(|mut f| std::mem::take(&mut *f))
        .unwrap_or_default();
    Ok(ExtractStats {
        files: state.files_committed.load(Ordering::Relaxed),
        bytes: state.bytes_committed.load(Ordering::Relaxed),
        skipped,
        // Only names that really changed. A planned path that happens to equal
        // the raw name (the overwhelmingly common case) is not a rename.
        renamed: renamed
            .into_iter()
            .filter(|r| r.from != r.to && r.from.replace('\\', "/") != r.to)
            .collect(),
        conflict_renamed,
        failed,
        motw: state.motw_mark.as_ref().map(|_| crate::types::MotwStats {
            marked: state.motw_marked.load(Ordering::Relaxed),
            unmarked: state.motw_unmarked.load(Ordering::Relaxed),
        }),
    })
}

// Keep the unused-import lint quiet for symbols wired in later tasks.
#[allow(unused)]
fn _future_use(_: &PropVariant, _: fn() -> ZipnestError) {
    let _ = E_NOTIMPL;
}

#[cfg(test)]
mod tests {
    use super::{conflicts_in, conflicts_on_disk, has_link_ancestor};
    use crate::archive::Archive;
    use crate::types::{ArchiveEntry, ArchiveOpenOptions, ExtractOptions, OnConflict};
    use std::path::{Path, PathBuf};

    /// A private scratch directory per test, removed at the end.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zipnest-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real archive built on the fly, so the tests below exercise the engine
    /// rather than hand-made entry structs. `zip` is already a dev-dependency.
    fn make_zip(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        for (entry, data) in entries {
            zip.start_file(*entry, opts).unwrap();
            use std::io::Write;
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn open(path: &Path) -> Archive {
        Archive::open(path, ArchiveOpenOptions::default()).expect("the fixture opens")
    }

    fn opts(dest: &Path) -> ExtractOptions {
        ExtractOptions {
            dest: dest.to_path_buf(),
            entries: Vec::new(),
            max_total_bytes: u64::MAX,
            on_conflict: OnConflict::Rename,
            test_mode: false,
            preserve_motw: false,
        }
    }

    fn entry(index: u32, path: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            index,
            path: path.to_string(),
            is_dir,
            size: 0,
            crc: None,
            mtime: None,
            encrypted: false,
        }
    }

    /// 目标目录里什么都没有的时候，绝不能报冲突 —— 这正是 v0.4.38 的缺陷：
    /// 弹窗由按钮触发，和磁盘无关。
    #[test]
    fn an_empty_destination_has_no_conflicts() {
        let dir = scratch("conflicts-empty");
        let zip = make_zip(&dir, "a.zip", &[("a.txt", b"one"), ("sub/b.txt", b"two")]);
        let arc = open(&zip);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        assert_eq!(conflicts_on_disk(&arc, &opts(&dest)).unwrap(), 0);

        // 目标目录本身还不存在时也一样：解压会把它建出来，没有东西可撞。
        let missing = dir.join("not-there");
        assert_eq!(conflicts_on_disk(&arc, &opts(&missing)).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同名文件真的在的时候要数出来，而且只数文件。
    #[test]
    fn existing_names_are_counted_file_by_file() {
        let dir = scratch("conflicts-count");
        let zip = make_zip(
            &dir,
            "a.zip",
            &[("a.txt", b"one"), ("b.txt", b"two"), ("sub/b.txt", b"three")],
        );
        let arc = open(&zip);
        let dest = dir.join("out");
        std::fs::create_dir_all(dest.join("sub")).unwrap();

        // 一个都没有。
        assert_eq!(conflicts_on_disk(&arc, &opts(&dest)).unwrap(), 0);

        std::fs::write(dest.join("a.txt"), b"old").unwrap();
        assert_eq!(conflicts_on_disk(&arc, &opts(&dest)).unwrap(), 1);

        // 子目录里的同名文件同样算：目标路径要保持归档里的结构。
        std::fs::write(dest.join("sub/b.txt"), b"old").unwrap();
        assert_eq!(conflicts_on_disk(&arc, &opts(&dest)).unwrap(), 2);

        // 目录条目本身从不计入：`sub/` 存在是正常的，不是冲突。
        let with_dirs = vec![entry(0, "sub", true), entry(1, "sub/b.txt", false)];
        assert_eq!(
            conflicts_in(&with_dirs, &[], &dest),
            1,
            "already-existing directories are not a conflict; only the file counts"
        );

        // 名字不同的文件不算：`b.txt` 在根上，`sub/b.txt` 在子目录里。
        std::fs::remove_file(dest.join("sub/b.txt")).unwrap();
        std::fs::write(dest.join("b.txt"), b"old").unwrap();
        assert_eq!(conflicts_on_disk(&arc, &opts(&dest)).unwrap(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 子目录整体不存在时，里面的文件不可能撞名 —— 判断不能只看文件名。
    #[test]
    fn a_file_under_a_missing_folder_cannot_conflict() {
        let dest = scratch("conflicts-missing-parent");
        // 同名文件放在根上，同时归档里有一个同名但位于不存在子目录里的条目。
        std::fs::write(dest.join("x.txt"), b"here").unwrap();
        // 建一个同名的 *目录*：`deep/x.txt` 的父目录叫 `deep`，这里它偏偏是文件。
        let entries = vec![entry(0, "deep/x.txt", false), entry(1, "x.txt", false)];
        assert_eq!(
            conflicts_in(&entries, &[], &dest),
            1,
            "deep/ does not exist, so deep/x.txt cannot be in the way; x.txt is"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 预检与解压必须对同一份文件名达成一致 —— 这是这个修复的全部意义。
    ///
    /// 用一个真实归档跑两遍：先问「有几个撞名」，再用 `rename` 解压到同一个
    /// 目录。答案 0 的那一次解压不能留下任何 `name (2).ext`（改名恰恰说明它撞上
    /// 了什么）；答案 N 的那一次必须正好留下 N 个。数的是磁盘上的文件，不是
    /// `stats.renamed` —— 那个字段记的是**归档名被清洗**，和冲突无关。
    #[test]
    fn the_precheck_agrees_with_the_run_that_follows_it() {
        fn renamed_copies(dest: &Path) -> u64 {
            fn walk(dir: &Path, out: &mut u64) {
                let Ok(rd) = std::fs::read_dir(dir) else {
                    return;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else if p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.contains(" (2)"))
                    {
                        *out += 1;
                    }
                }
            }
            let mut n = 0;
            walk(dest, &mut n);
            n
        }

        let dir = scratch("conflicts-agrees");
        let zip = make_zip(
            &dir,
            "a.zip",
            &[("a.txt", b"one"), ("c.txt", b"two"), ("sub/b.txt", b"three")],
        );
        let arc = open(&zip);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();

        // First run into an empty folder: nothing to be asked about.
        let none = conflicts_on_disk(&arc, &opts(&dest)).unwrap();
        assert_eq!(none, 0);
        arc.extract(&opts(&dest), None, &mut |_| true)
            .expect("the empty-destination run succeeds");
        assert_eq!(
            renamed_copies(&dest),
            0,
            "the precheck said 0, so the run may not have found anything to rename"
        );

        // Second run into the folder the first one just filled: every file is
        // now in the way, and that is exactly what the precheck says.
        let n = conflicts_on_disk(&arc, &opts(&dest)).unwrap();
        assert_eq!(n, 3, "all three files were just written there");
        arc.extract(&opts(&dest), None, &mut |_| true)
            .expect("the second run succeeds");
        assert_eq!(
            renamed_copies(&dest),
            n,
            "every name the precheck reported is one the run had to write elsewhere"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 被改写的归档名按**改写后**的名字比对：`CON` 会写成 `CON_`，目标目录里
    /// 有 `CON` 不算撞名，有 `CON_` 才算。
    #[test]
    fn a_rewritten_name_is_compared_under_its_written_name() {
        let dest = scratch("conflicts-rewritten");
        let entries = vec![entry(0, "CON", false)];
        assert_eq!(conflicts_in(&entries, &[], &dest), 0);
        // Sanity: the planner really does rewrite it, otherwise this test would
        // pass for the wrong reason.
        let planned = super::plan_entries(&entries);
        assert_ne!(planned[0].plan, "CON", "a device name must be rewritten");

        std::fs::write(dest.join(&planned[0].plan), b"x").unwrap();
        assert_eq!(
            conflicts_in(&entries, &[], &dest),
            1,
            "the file named the way the run will write it is a conflict"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 只问选中的那些条目：用户勾了一个子目录，问到的就该只是那一片。
    #[test]
    fn the_question_covers_only_the_selection() {
        let dest = scratch("conflicts-selection");
        std::fs::write(dest.join("a.txt"), b"old").unwrap();
        std::fs::write(dest.join("b.txt"), b"old").unwrap();
        let entries = vec![entry(0, "a.txt", false), entry(1, "b.txt", false)];
        // 空选择 = 整个归档。
        assert_eq!(conflicts_in(&entries, &[], &dest), 2);
        // 只选 a.txt。
        assert_eq!(conflicts_in(&entries, &[0], &dest), 1);
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 大小写不该骗过预检：Windows 上 `a.txt` 和 `A.TXT` 是同一个文件，
    /// 报「没有冲突」等于替用户决定覆盖。
    #[test]
    fn case_differences_still_count_as_a_conflict() {
        let dest = scratch("conflicts-case");
        std::fs::write(dest.join("A.TXT"), b"old").unwrap();
        let entries = vec![entry(0, "a.txt", false)];
        assert_eq!(
            conflicts_in(&entries, &[], &dest),
            1,
            "the destination filesystem would treat these as one file"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 代价不能随条目数线性堆 `stat`：一个文件夹只该读一次。
    ///
    /// 数的是「读了几次目录」，不是耗时 —— 调试构建里耗时主要由路径规划器
    /// 决定，用秒表去证明磁盘行为只会得到一个会飘的测试。
    #[test]
    fn a_wide_folder_is_listed_once_rather_than_probed_per_entry() {
        let dest = scratch("conflicts-wide");
        let entries: Vec<ArchiveEntry> = (0..5_000)
            .map(|i| entry(i, &format!("f{i}.txt"), false))
            .collect();
        let (count, folders) = super::conflicts_in_counted(&entries, &[], &dest);
        assert_eq!(count, 0, "the folder is empty");
        assert_eq!(
            folders, 1,
            "5000 files in one folder must cost one listing, not one probe per file"
        );

        // 两个子目录 → 两次列举，加上根目录一共三次。
        let nested = vec![
            entry(0, "a/one.txt", false),
            entry(1, "a/two.txt", false),
            entry(2, "b/three.txt", false),
            entry(3, "root.txt", false),
        ];
        let (_, folders) = super::conflicts_in_counted(&nested, &[], &dest);
        assert_eq!(folders, 3, "one listing per folder, including the root");
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// `has_link_ancestor` is the guard that stops an archive from writing
    /// *through* a link entry and out of the destination.
    ///
    /// Creating a symlink on Windows needs SeCreateSymbolicLinkPrivilege, so the
    /// test skips the positive case where the platform refuses (CI) instead of
    /// failing on an environment property; the negative case always runs.
    #[test]
    fn a_link_between_the_destination_and_the_entry_is_detected() {
        let root = std::env::temp_dir().join(format!("zipnest-link-{}", std::process::id()));
        let plain = root.join("plain");
        let linked = root.join("linked");
        let outside = root.join("outside");
        for dir in [&plain, &linked, &outside] {
            std::fs::create_dir_all(dir).unwrap();
        }

        // No link anywhere: the entry is written normally.
        assert!(!has_link_ancestor(&root, &plain.join("a.txt")));
        // The root itself is never treated as an escape.
        assert!(!has_link_ancestor(&root, &root.join("b.txt")));
        // A path outside the root is not "under a link" either -- the caller
        // already refuses those, and guessing here would be worse.
        assert!(!has_link_ancestor(&root, &outside.join("c.txt")));

        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &linked).is_ok();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &linked).is_ok();
        #[cfg(not(any(windows, unix)))]
        let made = false;

        if made {
            // `linked` now redirects to `outside`: a write to linked/x lands in
            // outside, even though the path starts with the destination root.
            assert!(has_link_ancestor(&root, &linked.join("x.txt")));
            assert!(has_link_ancestor(&root, &linked.join("deep/x.txt")));
        } else {
            eprintln!("note: this platform refused to create a symlink; positive case skipped");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Test mode must not create a single file, however many entries there are
    /// — and that is the answer to "will the test leave files behind?": the
    /// engine decodes each entry into a sink that only counts bytes, so there
    /// is no partial output to clean up on success, failure or cancel.
    #[test]
    fn test_mode_writes_nothing_into_the_destination() {
        let dir = scratch("test-mode-nothing");
        let zip = make_zip(
            &dir,
            "a.zip",
            &[("a.txt", b"one"), ("sub/b.txt", b"two"), ("sub/deep/c.bin", &[7u8; 4096])],
        );
        let arc = open(&zip);
        let dest = dir.join("out");
        // Deliberately does NOT pre-create the destination: a test that wrote
        // anything at all would have to create it first.
        let mut options = opts(&dest);
        options.test_mode = true;
        options.preserve_motw = true;

        let mut calls = 0u32;
        let stats = arc
            .extract(&options, None, &mut |_p| {
                calls += 1;
                true
            })
            .expect("a clean archive tests fine");

        assert_eq!(stats.files, 3, "all three files decoded and matched their CRC");
        assert!(stats.failed.is_empty());
        assert!(calls > 0, "progress was reported");
        assert!(
            !dest.exists(),
            "test mode must not even create the destination directory: {}",
            dest.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same on the cancel path: a test stopped midway leaves nothing behind
    /// because there was never anywhere for bytes to go.
    #[test]
    fn a_cancelled_test_mode_run_leaves_no_files() {
        let dir = scratch("test-mode-cancel");
        // One entry per chunk is not controllable from here, so use several
        // entries and stop on the first progress callback: enough to prove the
        // cancel path never touches the destination.
        let zip = make_zip(
            &dir,
            "a.zip",
            &[("a.txt", b"one"), ("b.txt", b"two"), ("c.txt", b"three")],
        );
        let arc = open(&zip);
        let dest = dir.join("out");
        let mut options = opts(&dest);
        options.test_mode = true;

        let err = arc
            .extract(&options, None, &mut |_p| false)
            .expect_err("a callback that returns false cancels the run");
        assert!(
            matches!(err, crate::ZipnestError::Cancelled),
            "cancel must surface as Cancelled, not a data error: {err:?}"
        );
        assert!(
            !dest.exists(),
            "a cancelled test must not create the destination either: {}",
            dest.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
