//! `IArchiveUpdateCallback` implementation driving `IOutArchive::UpdateItems`.
//!
//! Each source item is reported as brand new (`newData = newProps = 1`,
//! `indexInArchive = -1`); directories hand out a null stream, files hand out
//! a [`FileStreamOwner`]. Progress / cancellation share state with the
//! `create_archive` driver through an `Arc<UpdateState>`.

use super::callbacks;
use super::instream::FileStreamOwner;
use super::propvariant::{PropVariant, KPID_IS_DIR, KPID_MTIME, KPID_PATH, KPID_SIZE};
use super::vtables::UpdateCallbackVt;
use super::{
    as_void, qi_matches, Guid, Hresult, E_ABORT, E_FAIL, E_INVALIDARG, E_NOINTERFACE,
    IID_IARCHIVE_UPDATE_CALLBACK, IID_IPROGRESS, IID_IUNKNOWN, S_OK,
};
use crate::types::CreateProgress;
use std::fs::File;
use std::os::raw::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// One item to write into the new archive.
pub struct SourceItem {
    pub path: PathBuf,
    pub node: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
}

/// Progress closure slot 鈥?the `Box<&mut dyn FnMut>` fat pointer, plus a
/// monomorphized trampoline to call it. Invoked while the `Mutex` is held.
pub struct ProgressCell {
    pub cell: *mut c_void,
    pub call: fn(*mut c_void, &CreateProgress) -> bool,
}

/// Shared state between the update callback and the `create_archive` driver.
pub struct UpdateState {
    pub done_bytes: AtomicU64,
    /// Pre-scanned total; also overwritten by the handler's `SetTotal`.
    pub total_bytes: AtomicU64,
    pub cancelled: AtomicU8,
    pub last_path: Mutex<String>,
    pub progress: Mutex<ProgressCell>,
    pub io_error: Mutex<Option<std::io::Error>>,
}

fn call_progress(raw: *mut c_void, p: &CreateProgress) -> bool {
    unsafe {
        let slot = raw as *mut &mut dyn FnMut(&CreateProgress) -> bool;
        let f = &mut *slot;
        (**f)(p)
    }
}

/// Build a [`ProgressCell`] from a borrowed progress closure.
pub fn progress_cell(progress: &mut dyn FnMut(&CreateProgress) -> bool) -> ProgressCell {
    ProgressCell {
        cell: Box::into_raw(Box::new(progress)) as *mut c_void,
        call: call_progress,
    }
}

#[repr(C)]
pub struct UpdateCallback {
    vt: *const UpdateCallbackVt,
    refs: AtomicU32,
    items: Vec<SourceItem>,
    state: Arc<UpdateState>,
    crypto: *mut c_void,
}

static UPDATE_CB_VT: UpdateCallbackVt = UpdateCallbackVt {
    query_interface: qi,
    add_ref,
    release,
    set_total,
    set_completed,
    get_update_item_info,
    get_property,
    get_stream,
    set_operation_result,
};

unsafe extern "system" fn qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let this = this as *mut UpdateCallback;
    if let Some(hr) = callbacks::qi_hand_out_crypto((*this).crypto, &*riid, ppv) {
        return hr;
    }
    if qi_matches(
        &*riid,
        &[IID_IUNKNOWN, IID_IPROGRESS, IID_IARCHIVE_UPDATE_CALLBACK],
    ) {
        *ppv = this as *mut c_void;
        add_ref(this as *mut c_void);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut UpdateCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let this = this as *mut UpdateCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        callbacks::crypto_release_void((*this).crypto);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn set_total(this: *mut c_void, total: u64) -> Hresult {
    let this = this as *mut UpdateCallback;
    let st = &(*this).state;
    st.total_bytes.store(total, Ordering::Relaxed);
    S_OK
}

unsafe extern "system" fn set_completed(this: *mut c_void, complete: *const u64) -> Hresult {
    let this = this as *mut UpdateCallback;
    let st = &(*this).state;
    if !complete.is_null() {
        st.done_bytes.store(*complete, Ordering::Relaxed);
    }
    if st.cancelled.load(Ordering::SeqCst) == 1 {
        return E_ABORT;
    }
    let keep_going = {
        let cell = match st.progress.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        let path = match st.last_path.lock() {
            Ok(p) => p.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let p = CreateProgress {
            done_bytes: st.done_bytes.load(Ordering::Relaxed),
            total_bytes: st.total_bytes.load(Ordering::Relaxed),
            current_path: path,
        };
        (cell.call)(cell.cell, &p)
    };
    if !keep_going {
        st.cancelled.store(1, Ordering::SeqCst);
        return E_ABORT;
    }
    S_OK
}

unsafe extern "system" fn get_update_item_info(
    this: *mut c_void,
    index: u32,
    new_data: *mut i32,
    new_props: *mut i32,
    index_in_archive: *mut u32,
) -> Hresult {
    if new_data.is_null() || new_props.is_null() || index_in_archive.is_null() {
        return E_FAIL;
    }
    let this = this as *mut UpdateCallback;
    let items: &[SourceItem] = &(*this).items;
    if (index as usize) >= items.len() {
        return E_INVALIDARG;
    }
    // Everything is new: request fresh data + properties, no archive slot.
    *new_data = 1;
    *new_props = 1;
    *index_in_archive = u32::MAX;
    S_OK
}

unsafe extern "system" fn get_property(
    this: *mut c_void,
    index: u32,
    prop_id: u32,
    value: *mut PropVariant,
) -> Hresult {
    if value.is_null() {
        return E_FAIL;
    }
    let this = this as *mut UpdateCallback;
    let items: &[SourceItem] = &(*this).items;
    let item = match items.get(index as usize) {
        Some(i) => i,
        None => return E_FAIL,
    };
    *value = match prop_id {
        KPID_PATH => PropVariant::from_bstr(&item.node),
        KPID_IS_DIR => PropVariant::from_bool(item.is_dir),
        KPID_SIZE => PropVariant::from_u64(item.size),
        KPID_MTIME => match item.mtime {
            Some(t) => PropVariant::from_filetime(t),
            None => PropVariant::empty(),
        },
        _ => PropVariant::empty(),
    };
    S_OK
}

unsafe extern "system" fn get_stream(
    this: *mut c_void,
    index: u32,
    in_stream: *mut *mut c_void,
) -> Hresult {
    if in_stream.is_null() {
        return E_FAIL;
    }
    *in_stream = std::ptr::null_mut();
    let this = this as *mut UpdateCallback;
    let items: &[SourceItem] = &(*this).items;
    let item = match items.get(index as usize) {
        Some(i) => i,
        None => return E_FAIL,
    };
    if item.is_dir {
        return S_OK;
    }
    let file = match File::open(&item.path) {
        Ok(f) => f,
        Err(e) => return record_io_error(&(*this).state, e),
    };
    match FileStreamOwner::new(file) {
        Ok(owner) => {
            *in_stream = owner.into_raw();
            S_OK
        }
        Err(e) => record_io_error(&(*this).state, e),
    }
}

fn record_io_error(state: &UpdateState, e: std::io::Error) -> Hresult {
    if let Ok(mut slot) = state.io_error.lock() {
        if slot.is_none() {
            *slot = Some(e);
        }
    }
    E_FAIL
}

unsafe extern "system" fn set_operation_result(_this: *mut c_void, _op_res: i32) -> Hresult {
    S_OK
}

/// Box an update callback with an initial reference count of 1 (owned by the
/// caller). Release with [`release_void`].
pub fn new(items: Vec<SourceItem>, state: Arc<UpdateState>, crypto: *mut c_void) -> *mut c_void {
    let obj = Box::new(UpdateCallback {
        vt: &UPDATE_CB_VT,
        refs: AtomicU32::new(1),
        items,
        state,
        crypto,
    });
    as_void(Box::into_raw(obj))
}

/// Release the caller's reference on a callback from [`new`].
///
/// # Safety
/// `p` must be null or a live pointer previously returned by [`new`].
pub unsafe fn release_void(p: *mut c_void) {
    if !p.is_null() {
        (UPDATE_CB_VT.release)(p);
    }
}
