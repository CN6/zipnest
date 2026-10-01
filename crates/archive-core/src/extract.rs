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
    IID_IARCHIVE_EXTRACT_CALLBACK, IID_ICRYPTO_GET_TEXT_PASSWORD, IID_IPROGRESS,
    IID_ISEQ_OUT_STREAM, IID_IUNKNOWN,
};
use crate::error::ZipnestError;
use crate::types::{ArchiveEntry, ExtractOptions, ExtractProgress, ExtractStats};
use archive_security::{sanitize_entry_path, ExtractQuota, SecurityViolation};
use std::ffi::c_void;
use std::os::raw::c_void as RawCVoid;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// `NExtract::NOperationResult` values we care about.
const OP_OK: i32 = 0;
const OP_DATA_ERROR: i32 = 2;
const OP_CRC_ERROR: i32 = 3;
const OP_WRONG_PASSWORD: i32 = 9;
/// `NExtract::NAskMode::kExtract`.
const ASK_EXTRACT: i32 = 0;

/// Blocked-reason codes in [`DiskState::blocked`].
const BLOCK_NONE: u8 = 0;
const BLOCK_SECURITY: u8 = 1;
const BLOCK_QUOTA: u8 = 2;

// ===========================================================================
// In-memory output stream (ISequentialOutStream) 鈥?used by read_entry
// ===========================================================================

struct SinkState {
    buf: Mutex<Vec<u8>>,
    limit: u64,
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
    *processed = size; // engine sees full consumption; overflow is dropped
    S_OK
}

// ===========================================================================
// Shared: one crypto companion object per Extract call (password support)
// ===========================================================================

/// QI helper used by both extract callbacks: returns `true` when `riid` is
/// `ICryptoGetTextPassword` and hands the companion object out.
/// Returns `false` for anything else.
///
/// # Safety
/// `crypto` must be a live object from [`callbacks::crypto_new`].
unsafe fn qi_hand_out_crypto(
    crypto: *mut c_void,
    riid: &Guid,
    ppv: *mut *mut c_void,
) -> Option<Hresult> {
    if *riid == IID_ICRYPTO_GET_TEXT_PASSWORD {
        callbacks::crypto_addref_void(crypto);
        *ppv = crypto;
        Some(S_OK)
    } else {
        None
    }
}

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
    set_completed: noop_set_completed,
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
    if let Some(hr) = qi_hand_out_crypto((*this).crypto, &*riid, ppv) {
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

unsafe extern "system" fn noop_set_completed(_this: *mut c_void, _complete: *const u64) -> Hresult {
    S_OK
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
/// `password_provided` matters: the 7z handler reports a wrong AES key as
/// `kDataError` (2), not `kWrongPassword` (9) — indistinguishable from real
/// corruption at this layer. For encrypted extracts we surface the data/CRC
/// classes as `password_incorrect` so the UI can offer a retry (matching
/// 7-Zip's re-prompt behavior); without a password they stay engine errors.
fn map_op_res(op_res: i32, password_provided: bool) -> ZipnestError {
    match op_res {
        OP_WRONG_PASSWORD => ZipnestError::PasswordIncorrect,
        OP_DATA_ERROR | OP_CRC_ERROR if password_provided => ZipnestError::PasswordIncorrect,
        _ => ZipnestError::Engine(E_FAIL),
    }
}

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

    if hr != S_OK {
        // map_hresult: E_ABORT -> Cancelled, everything else -> Engine.
        return Err(crate::error::map_hresult(hr));
    }
    if op_res != OP_OK {
        return Err(if op_res < 0 {
            ZipnestError::Engine(E_FAIL)
        } else {
            map_op_res(op_res, password_provided)
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
    is_dir: bool,
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
    unsafe {
        let slot = raw as *mut &mut dyn FnMut(&ExtractProgress) -> bool;
        let f = &mut *slot;
        (**f)(p)
    }
}

struct DiskState {
    dest: PathBuf,
    overwrite: bool,
    entries: Vec<EntryMeta>, // sorted by index
    quota: Mutex<ExtractQuota>,
    blocked: AtomicU8,    // BLOCK_*
    cancelled: AtomicU8,  // 1 when the user closure asked to stop
    security: Mutex<Option<SecurityViolation>>,
    io_error: Mutex<Option<std::io::Error>>,
    first_op_res: AtomicI32,
    done_bytes: AtomicU64,
    files_created: AtomicU32,
    progress: Mutex<ProgressCell>,
}

impl DiskState {
    fn entry(&self, index: u32) -> Option<&EntryMeta> {
        self.entries
            .binary_search_by_key(&index, |e| e.index)
            .ok()
            .map(|i| &self.entries[i])
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
            let mut slot = match st.io_error.lock() {
                Ok(s) => s,
                Err(_) => return E_FAIL,
            };
            if slot.is_none() {
                *slot = Some(e);
            }
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
    if let Some(hr) = qi_hand_out_crypto((*this).crypto, &*riid, ppv) {
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

    // Security: sanitize the RAW archive path before it touches the disk.
    let rel = match sanitize_entry_path(&meta.raw_path) {
        Ok(r) => r,
        Err(v) => {
            let mut slot = match st.security.lock() {
                Ok(s) => s,
                Err(_) => return E_FAIL,
            };
            if slot.is_none() {
                *slot = Some(v);
            }
            st.blocked.store(BLOCK_SECURITY, Ordering::SeqCst);
            return E_FAIL;
        }
    };
    let full: PathBuf = st.dest.join(&rel);
    if !full.starts_with(&st.dest) {
        // Double insurance (symlink-style escapes cannot happen here, but
        // never write outside dest even if join semantics change).
        let mut slot = match st.security.lock() {
            Ok(s) => s,
            Err(_) => return E_FAIL,
        };
        if slot.is_none() {
            *slot = Some(SecurityViolation::Traversal);
        }
        st.blocked.store(BLOCK_SECURITY, Ordering::SeqCst);
        return E_FAIL;
    }

    if meta.is_dir {
        if let Err(e) = std::fs::create_dir_all(&full) {
            let mut slot = match st.io_error.lock() {
                Ok(s) => s,
                Err(_) => return E_FAIL,
            };
            if slot.is_none() {
                *slot = Some(e);
            }
            return E_FAIL;
        }
        return S_OK;
    }

    if full.exists() && !st.overwrite {
        return S_OK; // keep existing file, hand out no stream
    }
    if let Some(parent) = full.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            let mut slot = match st.io_error.lock() {
                Ok(s) => s,
                Err(_) => return E_FAIL,
            };
            if slot.is_none() {
                *slot = Some(e);
            }
            return E_FAIL;
        }
    }
    let file = match std::fs::File::create(&full) {
        Ok(f) => f,
        Err(e) => {
            let mut slot = match st.io_error.lock() {
                Ok(s) => s,
                Err(_) => return E_FAIL,
            };
            if slot.is_none() {
                *slot = Some(e);
            }
            return E_FAIL;
        }
    };
    st.files_created.fetch_add(1, Ordering::Relaxed);

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
    let mut selected: Vec<ArchiveEntry> = if opts.entries.is_empty() {
        all
    } else {
        let want: std::collections::HashSet<u32> = opts.entries.iter().copied().collect();
        all.into_iter().filter(|e| want.contains(&e.index)).collect()
    };
    selected.sort_by_key(|e| e.index);
    selected.dedup_by_key(|e| e.index);

    let total_bytes: u64 = selected.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let entries: Vec<EntryMeta> = selected
        .iter()
        .map(|e| EntryMeta {
            index: e.index,
            raw_path: e.path.clone(),
            is_dir: e.is_dir,
        })
        .collect();

    let state = Arc::new(DiskState {
        dest: opts.dest.clone(),
        overwrite: opts.overwrite,
        entries,
        quota: Mutex::new(ExtractQuota::new(opts.max_total_bytes)),
        blocked: AtomicU8::new(BLOCK_NONE),
        cancelled: AtomicU8::new(0),
        security: Mutex::new(None),
        io_error: Mutex::new(None),
        first_op_res: AtomicI32::new(0),
        done_bytes: AtomicU64::new(0),
        files_created: AtomicU32::new(0),
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
            0, // testMode=0: write to the streams we hand out
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

    // Post-check order matters: local flags win over whatever HRESULT the
    // handler chose to return (some handlers swallow our E_ABORT, and an
    // abort *we* triggered must report its real cause).
    if state.cancelled.load(Ordering::SeqCst) == 1 {
        return Err(ZipnestError::Cancelled);
    }
    match state.blocked.load(Ordering::SeqCst) {
        BLOCK_SECURITY => {
            let v = state
                .security
                .lock()
                .ok()
                .and_then(|mut s| s.take())
                .unwrap_or(SecurityViolation::Traversal);
            return Err(ZipnestError::Security(v));
        }
        BLOCK_QUOTA => return Err(ZipnestError::QuotaExceeded),
        _ => {}
    }
    if let Some(e) = state.io_error.lock().ok().and_then(|mut s| s.take()) {
        return Err(ZipnestError::Io(e));
    }
    if matches!(crate::error::map_hresult(hr), ZipnestError::Cancelled) {
        return Err(ZipnestError::Cancelled);
    }
    let op_res = state.first_op_res.load(Ordering::SeqCst);
    if op_res != 0 {
        return Err(map_op_res(op_res, password.is_some()));
    }
    if hr != S_OK {
        return Err(crate::error::map_hresult(hr));
    }

    Ok(ExtractStats {
        files: state.files_created.load(Ordering::Relaxed),
        bytes: state.done_bytes.load(Ordering::Relaxed),
    })
}

// Keep the unused-import lint quiet for symbols wired in later tasks.
#[allow(unused)]
fn _future_use(_: &PropVariant, _: fn() -> ZipnestError) {
    let _ = E_NOTIMPL;
}
