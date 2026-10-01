//! `IArchiveOpenCallback` + `ICryptoGetTextPassword` pair passed to
//! `IInArchive::Open`.
//!
//! Two independently refcounted objects share one `Arc<OpenState>` because
//! the two interfaces have different vtables (slot 3 conflicts:
//! `SetTotal` vs `CryptoGetTextPassword`).

use super::propvariant::alloc_bstr;
use super::vtables::{ArchiveOpenCallbackVt, CryptoGetTextPasswordVt};
use super::{as_void, Guid, Hresult, ComObject, E_FAIL, E_NOINTERFACE,
    IID_ICRYPTO_GET_TEXT_PASSWORD, IID_IARCHIVE_OPEN_CALLBACK, IID_IUNKNOWN, S_OK};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Password material for one open operation — memory only, never logged.
pub struct OpenState {
    pub password: Option<String>,
    /// Set when the engine actually asked for a password (via
    /// `ICryptoGetTextPassword`). Lets `Archive::open` distinguish
    /// "password required/incorrect" from "not an archive".
    pub asked: std::sync::atomic::AtomicU8,
}

#[repr(C)]
pub struct OpenCallback {
    obj: ComObject<ArchiveOpenCallbackVt>,
    refs: AtomicU32,
    state: Arc<OpenState>,
    crypto: *mut CryptoCallback,
}

#[repr(C)]
pub struct CryptoCallback {
    obj: ComObject<CryptoGetTextPasswordVt>,
    refs: AtomicU32,
    state: Arc<OpenState>,
}

static OPEN_VT: ArchiveOpenCallbackVt = ArchiveOpenCallbackVt {
    query_interface: open_qi,
    add_ref: open_add_ref,
    release: open_release,
    set_total,
    set_completed,
};

static CRYPTO_VT: CryptoGetTextPasswordVt = CryptoGetTextPasswordVt {
    query_interface: crypto_qi,
    add_ref: crypto_add_ref,
    release: crypto_release,
    get_text_password,
};

// ---- IArchiveOpenCallback object ----

unsafe extern "system" fn open_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let this = this as *mut OpenCallback;
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_IARCHIVE_OPEN_CALLBACK {
        *ppv = this as *mut c_void;
        open_add_ref(this as *mut c_void);
        S_OK
    } else if *iid == IID_ICRYPTO_GET_TEXT_PASSWORD {
        // Hand out the companion object (refcounted separately).
        let crypto = (*this).crypto;
        *ppv = crypto as *mut c_void;
        crypto_add_ref(crypto as *mut c_void);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn open_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut OpenCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn open_release(this: *mut c_void) -> u32 {
    let this = this as *mut OpenCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        // Drop our Arc; companion crypto object has its own lifecycle.
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn set_total(
    _this: *mut c_void,
    _files: *const u64,
    _bytes: *const u64,
) -> Hresult {
    S_OK
}

unsafe extern "system" fn set_completed(
    _this: *mut c_void,
    _files: *const u64,
    _bytes: *const u64,
) -> Hresult {
    S_OK
}

// ---- ICryptoGetTextPassword object ----

unsafe extern "system" fn crypto_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_ICRYPTO_GET_TEXT_PASSWORD {
        *ppv = this;
        crypto_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn crypto_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut CryptoCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn crypto_release(this: *mut c_void) -> u32 {
    let this = this as *mut CryptoCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn get_text_password(
    this: *mut c_void,
    password: *mut *mut u16,
) -> Hresult {
    if password.is_null() {
        return E_FAIL;
    }
    let this = this as *mut CryptoCallback;
    let obj: &CryptoCallback = &*this;
    obj.state
        .asked
        .store(1, std::sync::atomic::Ordering::SeqCst);
    match &obj.state.password {
        Some(p) => {
            *password = alloc_bstr(p);
            if (*password).is_null() {
                return E_FAIL;
            }
            S_OK
        }
        None => {
            *password = std::ptr::null_mut();
            E_FAIL
        }
    }
}

// ---------------------------------------------------------------------------
// Standalone crypto objects for the extract/read callbacks (Task 8/9).
// The extract callback QIs `IID_ICRYPTO_GET_TEXT_PASSWORD` on itself and
// hands out this companion object, exactly like the open path above.
// ---------------------------------------------------------------------------

/// Create a standalone `ICryptoGetTextPassword` with refcount 1 (ours).
pub(crate) fn crypto_new(password: Option<String>) -> *mut CryptoCallback {
    let state = Arc::new(OpenState {
        password,
        asked: std::sync::atomic::AtomicU8::new(0),
    });
    Box::into_raw(Box::new(CryptoCallback {
        obj: ComObject { vt: &CRYPTO_VT },
        refs: AtomicU32::new(1),
        state,
    }))
}

pub(crate) unsafe fn crypto_addref_void(p: *mut c_void) -> u32 {
    crypto_add_ref(p)
}

pub(crate) unsafe fn crypto_release_void(p: *mut c_void) -> u32 {
    crypto_release(p)
}

/// QI helper shared by the extract/update callbacks: when `riid` is
/// `ICryptoGetTextPassword`, hand out the companion object (AddRef'd) and
/// return `Some(S_OK)`; anything else returns `None` so the caller can
/// continue matching its own IIDs.
///
/// # Safety
/// `crypto` must be a live object from [`crypto_new`], and `ppv` writable.
pub(crate) unsafe fn qi_hand_out_crypto(
    crypto: *mut c_void,
    riid: &Guid,
    ppv: *mut *mut c_void,
) -> Option<Hresult> {
    if *riid == IID_ICRYPTO_GET_TEXT_PASSWORD {
        crypto_addref_void(crypto);
        *ppv = crypto;
        Some(S_OK)
    } else {
        None
    }
}

/// Owns the callback pair for one `Open` call.
pub struct OpenCallbackOwner {
    open: *mut OpenCallback,
    crypto: *mut CryptoCallback,
}

impl OpenCallbackOwner {
    pub fn new(password: Option<String>) -> Self {
        let state = Arc::new(OpenState {
            password,
            asked: std::sync::atomic::AtomicU8::new(0),
        });
        let crypto = Box::new(CryptoCallback {
            obj: ComObject { vt: &CRYPTO_VT },
            refs: AtomicU32::new(1),
            state: Arc::clone(&state),
        });
        let crypto = Box::into_raw(crypto);
        let open = Box::new(OpenCallback {
            obj: ComObject { vt: &OPEN_VT },
            refs: AtomicU32::new(1),
            state,
            crypto,
        });
        OpenCallbackOwner {
            open: Box::into_raw(open),
            crypto,
        }
    }

    pub fn as_void(&self) -> *mut c_void {
        as_void(self.open)
    }

    /// Shared open state; survives `release_own` when cloned, so callers
    /// can classify an `Open` failure after the callback is gone.
    pub fn state(&self) -> &Arc<OpenState> {
        // Safe: the owner keeps the OpenCallback object alive.
        unsafe { &(*self.open).state }
    }

    /// Release our own references after the engine call returns. Any
    /// balanced AddRef/Release pairs the engine made have already cancelled
    /// out; unbalanced engine refs degrade to a safe leak, never a UAF.
    pub unsafe fn release_own(self) {
        let open = self.open;
        let crypto = self.crypto;
        std::mem::forget(self);
        (OPEN_VT.release)(open as *mut c_void);
        (CRYPTO_VT.release)(crypto as *mut c_void);
    }
}

impl Drop for OpenCallbackOwner {
    fn drop(&mut self) {
        unsafe {
            (OPEN_VT.release)(self.open as *mut c_void);
            (CRYPTO_VT.release)(self.crypto as *mut c_void);
        }
    }
}

