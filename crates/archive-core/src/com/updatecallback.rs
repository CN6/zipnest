//! `IArchiveUpdateCallback` for updating an *existing* archive.
//!
//! Two different jobs use the same interface and the same callback shape:
//!
//! * creating a new archive from scratch (`create.rs`), where every item is
//!   brand new; and
//! * editing an existing one (`update.rs`), where most items are copied from
//!   the open archive and only some are added or dropped.
//!
//! This module is the second one: an item table of three kinds, mirroring what
//! `IArchiveUpdateCallback::GetUpdateItemInfo` accepts
//! (`vendor/7zip-sdk/Archive/IArchive.h`):
//!
//! * `newData = 0, newProps = 0` — copy both from the archive, the item that
//!   stays untouched at index `indexInArchive`;
//! * `newData = 1, newProps = 1` — take this item's data and properties from
//!   us, `indexInArchive = u32::MAX`;
//! * a **dropped** item is simply not in the table at all: 7-Zip's own client
//!   builds the new index list without it, and the handler rewrites the rest.
//!
//! Copying an item's data still goes through `GetStream`, and the handler
//! reads it from the *archive's* decoder, not from us: for `newData = 0` the
//! handler supplies the stream itself. Our `GetStream` is only asked for the
//! items we added.

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

/// One entry of the *new* archive, in the order the new archive will hold it.
#[derive(Debug, Clone)]
pub enum UpdateItem {
    /// Copy data and properties from the open archive's item `index`.
    Keep { index: u32 },
    /// Take this item's data and properties from the filesystem.
    Add {
        path: PathBuf,
        node: String,
        is_dir: bool,
        size: u64,
        mtime: Option<std::time::SystemTime>,
    },
}

/// Progress closure slot — see `outcallback::ProgressCell` (same shape, kept
/// separate because the two callbacks live in different modules).
pub struct ProgressCell {
    pub cell: *mut c_void,
    pub call: fn(*mut c_void, &CreateProgress) -> bool,
}

/// Shared state between the callback and the driver.
pub struct UpdateState {
    pub done_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
    pub cancelled: AtomicU8,
    pub last_path: Mutex<String>,
    pub progress: Mutex<ProgressCell>,
    pub io_error: Mutex<Option<std::io::Error>>,
}

fn call_progress(raw: *mut c_void, p: &CreateProgress) -> bool {
    // Same contract as the create-side trampoline: a panicking closure must
    // stop the run, not unwind through the COM boundary.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let slot = raw as *mut &mut dyn FnMut(&CreateProgress) -> bool;
        let f = &mut *slot;
        (**f)(p)
    }))
    .unwrap_or(false)
}

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
    items: Vec<UpdateItem>,
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
    let items: &[UpdateItem] = &(*this).items;
    let item = match items.get(index as usize) {
        Some(i) => i,
        None => return E_INVALIDARG,
    };
    match item {
        // Untouched: the handler copies both from the archive.
        UpdateItem::Keep { index: archive_index } => {
            *new_data = 0;
            *new_props = 0;
            *index_in_archive = *archive_index;
        }
        // New file (or folder): everything comes from us.
        UpdateItem::Add { .. } => {
            *new_data = 1;
            *new_props = 1;
            *index_in_archive = u32::MAX;
        }
    }
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
    let items: &[UpdateItem] = &(*this).items;
    let st = &(*this).state;
    let item = match items.get(index as usize) {
        Some(i) => i,
        None => return E_FAIL,
    };
    // Only the items we own are asked for properties: for `Keep` the handler
    // reads them from the archive itself. Answer empties rather than failing,
    // so a handler that asks anyway gets a clear "no value", not an error.
    let add = match item {
        UpdateItem::Keep { .. } => {
            *value = PropVariant::empty();
            return S_OK;
        }
        UpdateItem::Add { path, node, is_dir, size, mtime } => (path, node, is_dir, size, mtime),
    };
    let (path, node, is_dir, size, mtime) = add;
    if prop_id == KPID_PATH {
        if let Ok(mut p) = st.last_path.lock() {
            p.clone_from(node);
        }
    }
    *value = match prop_id {
        KPID_PATH => PropVariant::from_bstr(node),
        KPID_IS_DIR => PropVariant::from_bool(*is_dir),
        KPID_SIZE => PropVariant::from_u64(*size),
        KPID_MTIME => match mtime {
            Some(t) => PropVariant::from_filetime(*t),
            None => PropVariant::empty(),
        },
        _ => PropVariant::empty(),
    };
    let _ = path;
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
    let items: &[UpdateItem] = &(*this).items;
    let st = &(*this).state;
    let item = match items.get(index as usize) {
        Some(i) => i,
        None => return E_FAIL,
    };
    let add = match item {
        // The handler copies the data itself; no stream from us.
        UpdateItem::Keep { .. } => return S_OK,
        UpdateItem::Add { path, node, is_dir, .. } => (path, node, is_dir),
    };
    let (path, node, is_dir) = add;
    if let Ok(mut p) = st.last_path.lock() {
        p.clone_from(node);
    }
    if *is_dir {
        return S_OK;
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return record_io_error(st, e),
    };
    match FileStreamOwner::new(file) {
        Ok(owner) => {
            *in_stream = owner.into_raw();
            S_OK
        }
        Err(e) => record_io_error(st, e),
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

/// Box an update callback with an initial refcount of 1 (owned by the caller).
pub fn new(items: Vec<UpdateItem>, state: Arc<UpdateState>, crypto: *mut c_void) -> *mut c_void {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Same as `outcallback`'s test helper: the progress cell holds a raw
    // closure pointer, so the Arc is non-Send by construction.
    #[allow(clippy::arc_with_non_send_sync)]
    fn state() -> Arc<UpdateState> {
        Arc::new(UpdateState {
            done_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            cancelled: AtomicU8::new(0),
            last_path: Mutex::new(String::new()),
            progress: Mutex::new(ProgressCell {
                cell: std::ptr::null_mut(),
                call: |_, _| true,
            }),
            io_error: Mutex::new(None),
        })
    }

    /// The table `update.rs` builds must answer `GetUpdateItemInfo` exactly the
    /// way the SDK documents the three combinations: keep = (0,0,&index),
    /// add = (1,1,u32::MAX).
    #[test]
    fn the_item_table_reports_keep_and_add_the_way_the_sdk_expects() {
        let items = vec![
            UpdateItem::Keep { index: 3 },
            UpdateItem::Add {
                path: PathBuf::from(r"C:\x\a.txt"),
                node: "a.txt".into(),
                is_dir: false,
                size: 5,
                mtime: None,
            },
        ];
        let cb = new(items, state(), std::ptr::null_mut());
        unsafe {
            let mut nd = -1;
            let mut np = -1;
            let mut ia = 0u32;
            assert_eq!(get_update_item_info(cb, 0, &mut nd, &mut np, &mut ia), S_OK);
            assert_eq!((nd, np, ia), (0, 0, 3));
            assert_eq!(get_update_item_info(cb, 1, &mut nd, &mut np, &mut ia), S_OK);
            assert_eq!((nd, np), (1, 1));
            assert_eq!(ia, u32::MAX);
            // Out of range is an argument error, not a panic.
            assert_eq!(
                get_update_item_info(cb, 2, &mut nd, &mut np, &mut ia),
                E_INVALIDARG
            );
            release_void(cb);
        }
    }

    /// A `Keep` item must never open a file: the handler owns that data, and
    /// asking us for a stream would fail on a path that no longer exists (the
    /// archive's own name is not a filesystem path).
    #[test]
    fn a_kept_item_hands_out_no_stream_and_no_properties() {
        let items = vec![UpdateItem::Keep { index: 0 }];
        let cb = new(items, state(), std::ptr::null_mut());
        unsafe {
            let mut stream: *mut c_void = std::ptr::dangling_mut();
            assert_eq!(get_stream(cb, 0, &mut stream), S_OK);
            assert!(stream.is_null(), "the handler must supply its own stream");
            let mut pv = PropVariant::empty();
            assert_eq!(get_property(cb, 0, KPID_PATH, &mut pv), S_OK);
            assert_eq!(pv.vt, 0, "an empty value, not an error");
            release_void(cb);
        }
    }

    /// Cancellation travels the same way as the create path: `false` from the
    /// progress closure becomes E_ABORT and arms the cancel flag.
    ///
    /// The closure lives on this stack and the cell points at it for exactly
    /// the length of the test — the same contract `create.rs` gives the engine.
    #[test]
    #[allow(clippy::arc_with_non_send_sync)] // the cell holds a stack-bound closure
    fn a_cancelled_run_maps_to_e_abort() {
        let mut stop = |_: &CreateProgress| -> bool { false };
        let state = Arc::new(UpdateState {
            done_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            cancelled: AtomicU8::new(0),
            last_path: Mutex::new(String::new()),
            progress: Mutex::new(progress_cell(&mut stop)),
            io_error: Mutex::new(None),
        });
        let cb = new(Vec::new(), Arc::clone(&state), std::ptr::null_mut());
        unsafe {
            let done = 0u64;
            assert_eq!(set_completed(cb, &done), E_ABORT);
            assert_eq!(state.cancelled.load(Ordering::SeqCst), 1);
            release_void(cb);
            // Free the boxed closure cell, as production does after UpdateItems.
            let cell = state.progress.lock().unwrap().cell;
            if !cell.is_null() {
                drop(Box::from_raw(
                    cell as *mut &mut dyn FnMut(&CreateProgress) -> bool,
                ));
            }
        }
    }
}
