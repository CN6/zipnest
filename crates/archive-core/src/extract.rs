//! Extraction paths: `read_entry` (in-memory) and `extract_to_disk`.
//!
//! Both drive `IInArchive::Extract` with our own
//! `IArchiveExtractCallback` implementations.

use crate::archive::Archive;
use crate::com::propvariant::PropVariant;
use crate::com::vtables::{ArchiveExtractCallbackVt, InArchiveVt, SeqOutStreamVt};
use crate::com::{
    as_void, qi_matches, Guid, Hresult, E_FAIL, E_NOINTERFACE, IID_IARCHIVE_EXTRACT_CALLBACK,
    IID_IPROGRESS, IID_ISEQ_OUT_STREAM, IID_IUNKNOWN, S_OK,
};
use crate::error::ZipnestError;
use crate::types::{ExtractOptions, ExtractProgress, ExtractStats};
use std::ffi::c_void;
use std::os::raw::c_void as RawCVoid;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// `NExtract::NOperationResult` values we care about.
const OP_OK: i32 = 0;
const OP_WRONG_PASSWORD: i32 = 9;
/// `NExtract::NAskMode::kExtract`.
const ASK_EXTRACT: i32 = 0;

// ---------------------------------------------------------------------------
// In-memory output stream (ISequentialOutStream)
// ---------------------------------------------------------------------------

struct SinkState {
    buf: Mutex<Vec<u8>>,
    limit: u64,
    truncated: AtomicU8,
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

const SINK_IIDS: [Guid; 3] = [IID_IUNKNOWN, IID_ISEQ_OUT_STREAM, IID_IPROGRESS /*unused*/];

unsafe extern "system" fn sink_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let known = [IID_IUNKNOWN, IID_ISEQ_OUT_STREAM];
    if qi_matches(&*riid, &known) {
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
    if take < chunk.len() {
        state.truncated.store(1, Ordering::Relaxed);
    }
    *processed = size; // engine sees full consumption; overflow is dropped
    S_OK
}

// ---------------------------------------------------------------------------
// Extract callback driving IInArchive::Extract (memory mode)
// ---------------------------------------------------------------------------

#[repr(C)]
struct MemExtractCallback {
    vt: *const ArchiveExtractCallbackVt,
    refs: AtomicU32,
    sink: *mut MemOutStream,
    op_res: AtomicI32,
    state: Arc<SinkState>,
}

static MEM_CB_VT: ArchiveExtractCallbackVt = ArchiveExtractCallbackVt {
    query_interface: cb_qi,
    add_ref: cb_add_ref,
    release: cb_release,
    set_total: cb_set_total,
    set_completed: cb_set_completed,
    get_stream: cb_get_stream,
    prepare_operation: cb_prepare,
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
    let known = [IID_IUNKNOWN, IID_IPROGRESS, IID_IARCHIVE_EXTRACT_CALLBACK];
    if qi_matches(&*riid, &known) {
        *ppv = this;
        cb_add_ref(this);
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
        // drop sink (our ref) together with the callback
        sink_release((*this).sink as *mut c_void);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn cb_set_total(_this: *mut c_void, _total: u64) -> Hresult {
    S_OK
}

unsafe extern "system" fn cb_set_completed(
    _this: *mut c_void,
    _complete: *const u64,
) -> Hresult {
    S_OK
}

unsafe extern "system" fn cb_get_stream(
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
        // Hand the engine our memory sink (engine will AddRef/Release).
        sink_add_ref((*this).sink as *mut c_void);
        *out_stream = (*this).sink as *mut c_void;
    } else {
        *out_stream = std::ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn cb_prepare(_this: *mut c_void, _ask: i32) -> Hresult {
    S_OK
}

unsafe extern "system" fn cb_op_result(this: *mut c_void, op_res: i32) -> Hresult {
    let this = this as *mut MemExtractCallback;
    (*this).op_res.store(op_res, Ordering::SeqCst);
    S_OK
}

fn map_op_res(op_res: i32) -> ZipnestError {
    match op_res {
        OP_OK => unreachable!("op_ok handled by caller"),
        OP_WRONG_PASSWORD => ZipnestError::PasswordIncorrect,
        _ => ZipnestError::Engine(E_FAIL),
    }
}

// ---------------------------------------------------------------------------
// Public entry: read one entry into memory
// ---------------------------------------------------------------------------

pub(crate) fn read_entry(
    arc: &Archive,
    index: u32,
    max_bytes: Option<u64>,
) -> Result<Vec<u8>, ZipnestError> {
    let limit = max_bytes.unwrap_or(u64::MAX);
    let state = Arc::new(SinkState {
        buf: Mutex::new(Vec::new()),
        limit,
        truncated: AtomicU8::new(0),
    });
    let sink = Box::into_raw(Box::new(MemOutStream {
        vt: &MEM_OUT_VT,
        refs: AtomicU32::new(1),
        state: Arc::clone(&state),
    }));
    let cb = Box::into_raw(Box::new(MemExtractCallback {
        vt: &MEM_CB_VT,
        refs: AtomicU32::new(1),
        sink,
        op_res: AtomicI32::new(-1),
        state: Arc::clone(&state),
    }));

    let indices = [index];
    let hr = unsafe {
        let vt = &**(arc.raw() as *const *const InArchiveVt);
        (vt.extract)(
            arc.raw(),
            indices.as_ptr(),
            1,
            0, // testMode=0: stream data to the provided out-stream
            cb as *mut c_void,
        )
    };

    let op_res = unsafe { (*cb).op_res.load(Ordering::SeqCst) };
    unsafe {
        (MEM_CB_VT.release)(cb as *mut c_void);
    }

    if hr == crate::com::E_ABORT {
        return Err(ZipnestError::Cancelled);
    }
    if hr != S_OK {
        return Err(crate::error::map_hresult(hr));
    }
    if op_res != OP_OK {
        // -1 = callback never completed (handler skipped the item)
        return Err(if op_res < 0 {
            ZipnestError::Engine(E_FAIL)
        } else {
            map_op_res(op_res)
        });
    }

    let buf = state.buf.lock().map_err(|_| ZipnestError::Engine(E_FAIL))?;
    Ok(buf.clone())
}

// ===========================================================================
// extract_to_disk — Task 8 (scaffold body; implemented next task)
// ===========================================================================

#[allow(unused_variables)]
pub(crate) fn extract_to_disk(
    arc: &Archive,
    opts: &ExtractOptions,
    password: Option<&str>,
    progress: &mut dyn FnMut(&ExtractProgress) -> bool,
) -> Result<ExtractStats, ZipnestError> {
    Err(ZipnestError::Engine(crate::com::E_NOTIMPL))
}

// silence "never used" until Task 8 wires these
#[allow(unused)]
fn _keep(p: &PropVariant) {
    let _ = p;
}
