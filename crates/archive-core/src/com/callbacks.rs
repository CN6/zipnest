//! `IArchiveOpenCallback` + `ICryptoGetTextPassword` pair passed to
//! `IInArchive::Open`.
//!
//! Two independently refcounted objects share one `Arc<OpenState>` because
//! the two interfaces have different vtables (slot 3 conflicts:
//! `SetTotal` vs `CryptoGetTextPassword`).

use super::instream::FileStreamOwner;
use super::propvariant::{alloc_bstr, PropVariant, VT_EMPTY, KPID_NAME};
use super::vtables::{
    ArchiveOpenCallbackVt, CryptoGetTextPassword2Vt, CryptoGetTextPasswordVt,
    OpenVolumeCallbackVt,
};
use super::{as_void, Guid, Hresult, ComObject, E_FAIL, E_NOINTERFACE,
    IID_ICRYPTO_GET_TEXT_PASSWORD, IID_ICRYPTO_GET_TEXT_PASSWORD2,
    IID_IARCHIVE_OPEN_CALLBACK, IID_IARCHIVE_OPEN_VOLUME_CALLBACK,
    IID_IUNKNOWN, S_FALSE, S_OK};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Password material + multi-volume lookup for one open operation — memory
/// only, never logged.
pub struct OpenState {
    pub password: Option<String>,
    /// Set when the engine actually asked for a password (via
    /// `ICryptoGetTextPassword`). Lets `Archive::open` distinguish
    /// "password required/incorrect" from "not an archive".
    pub asked: std::sync::atomic::AtomicU8,
    /// Directory of the file being opened; volume names are resolved here.
    pub volume_dir: PathBuf,
    /// File name of the first volume (e.g. `vol.7z.001`), reported through
    /// `IArchiveOpenVolumeCallback::GetProperty(kpidName)`.
    pub volume_name: String,
    /// First *real* I/O failure seen while resolving a sibling volume (sharing
    /// violation, ACL denial, unreadable file — anything but "not found").
    /// `GetStream` reports those as `E_FAIL`; the open path turns this into the
    /// user-visible error instead of opening a silently short archive.
    pub io_error: std::sync::Mutex<Option<std::io::Error>>,
}

/// Remember the first I/O failure of a volume lookup; later ones are reported
/// with the same `E_FAIL` but do not overwrite the original cause.
fn record_volume_io_error(state: &OpenState, e: std::io::Error) {
    let mut slot = match state.io_error.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if slot.is_none() {
        *slot = Some(e);
    }
}

#[repr(C)]
pub struct OpenCallback {
    obj: ComObject<ArchiveOpenCallbackVt>,
    refs: AtomicU32,
    state: Arc<OpenState>,
    crypto: *mut CryptoCallback,
    volume: *mut VolumeCallback,
}

#[repr(C)]
pub struct CryptoCallback {
    obj: ComObject<CryptoGetTextPasswordVt>,
    refs: AtomicU32,
    state: Arc<OpenState>,
}

/// Third interface of the open pair: `IArchiveOpenVolumeCallback`. It gets its
/// own object (and thus its own vtable) because its post-IUnknown slots
/// (`GetProperty`/`GetStream`) differ from `IArchiveOpenCallback`'s
/// (`SetTotal`/`SetCompleted`); one object cannot serve both.
#[repr(C)]
pub struct VolumeCallback {
    obj: ComObject<OpenVolumeCallbackVt>,
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

static VOLUME_VT: OpenVolumeCallbackVt = OpenVolumeCallbackVt {
    query_interface: volume_qi,
    add_ref: volume_add_ref,
    release: volume_release,
    get_property: volume_get_property,
    get_stream: volume_get_stream,
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
    } else if *iid == IID_IARCHIVE_OPEN_VOLUME_CALLBACK {
        // Multi-volume companion; may be absent when the caller chose not to
        // expose volume lookup (the re-open stage of a `.NNN` file).
        let volume = (*this).volume;
        if volume.is_null() {
            *ppv = std::ptr::null_mut();
            return E_NOINTERFACE;
        }
        *ppv = volume as *mut c_void;
        volume_add_ref(volume as *mut c_void);
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

// ---- IArchiveOpenVolumeCallback object ----

unsafe extern "system" fn volume_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_IARCHIVE_OPEN_VOLUME_CALLBACK {
        *ppv = this;
        volume_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn volume_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut VolumeCallback;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn volume_release(this: *mut c_void) -> u32 {
    let this = this as *mut VolumeCallback;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

/// `kpidName` → the first volume's file name (e.g. `vol.7z.001`), which the
/// `Split` handler parses to derive the rest of the sequence. Anything else is
/// reported as `VT_EMPTY`.
unsafe extern "system" fn volume_get_property(
    this: *mut c_void,
    prop_id: u32,
    value: *mut PropVariant,
) -> Hresult {
    if value.is_null() {
        return E_FAIL;
    }
    if prop_id == KPID_NAME {
        let obj: &VolumeCallback = &*(this as *mut VolumeCallback);
        *value = PropVariant::from_bstr(&obj.state.volume_name);
        S_OK
    } else {
        (*value).vt = VT_EMPTY;
        S_OK
    }
}

/// Open a sibling volume by name in the first volume's directory.
///
/// Only a volume that does *not exist* yields `S_FALSE` (the `Split` handler
/// reads that as "the sequence ends here"). Every other failure — sharing
/// violation, ACL denial, unreadable file — is recorded in `OpenState::io_error`
/// and reported as `E_FAIL`: answering `S_FALSE` there would let the handler
/// treat a truncated concatenation as a complete archive and open a short
/// archive without any error.
unsafe extern "system" fn volume_get_stream(
    this: *mut c_void,
    name: *const u16,
    in_stream: *mut *mut c_void,
) -> Hresult {
    if name.is_null() || in_stream.is_null() {
        return E_FAIL;
    }
    *in_stream = std::ptr::null_mut();
    let obj: &VolumeCallback = &*(this as *mut VolumeCallback);
    // The engine hands us a NUL-terminated string, but the scan is bounded so a
    // malformed pointer can never walk off the end of the process.
    const MAX_NAME_UNITS: usize = 32768;
    let mut len = 0usize;
    while len < MAX_NAME_UNITS && *name.add(len) != 0 {
        len += 1;
    }
    if len == MAX_NAME_UNITS {
        return E_FAIL;
    }
    let file_name = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
    let path = obj.state.volume_dir.join(file_name);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return S_FALSE;
            }
            record_volume_io_error(&obj.state, e);
            return E_FAIL;
        }
    };
    match FileStreamOwner::new(file) {
        Ok(owner) => {
            // Hand our single reference to the engine; it will Release it.
            *in_stream = owner.into_raw();
            S_OK
        }
        Err(e) => {
            // Unreachable today (the constructor cannot fail), but a local
            // failure is not "volume missing" either.
            record_volume_io_error(&obj.state, e);
            E_FAIL
        }
    }
}

// ---------------------------------------------------------------------------
// Standalone crypto companions for the extract/read and create callbacks.
//
// Two interfaces with the same slot layout but different method signatures are
// needed:
//   * `ICryptoGetTextPassword`  (v1) — queried by the open and extract paths.
//   * `ICryptoGetTextPassword2` (v2) — queried by the create handlers
//     (`ZipHandlerOut.cpp`, `7zHandlerOut.cpp`) to obtain the password plus an
//     explicit "is defined" flag.
// Because a single COM object can only carry one vtable pointer, the pair is a
// tiny holder that hands out whichever companion was asked for.
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct CryptoCallback2 {
    obj: ComObject<CryptoGetTextPassword2Vt>,
    refs: AtomicU32,
    state: Arc<OpenState>,
}

static CRYPTO2_VT: CryptoGetTextPassword2Vt = CryptoGetTextPassword2Vt {
    query_interface: crypto2_qi,
    add_ref: crypto2_add_ref,
    release: crypto2_release,
    get_text_password2,
};

/// Owns one v1 and one v2 companion sharing a password state.
#[repr(C)]
pub struct CryptoPair {
    v1: *mut CryptoCallback,
    v2: *mut CryptoCallback2,
}

unsafe extern "system" fn crypto2_qi(
    this: *mut c_void,
    riid: *const Guid,
    ppv: *mut *mut c_void,
) -> Hresult {
    if riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_ICRYPTO_GET_TEXT_PASSWORD2 {
        *ppv = this;
        crypto2_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn crypto2_add_ref(this: *mut c_void) -> u32 {
    let this = this as *mut CryptoCallback2;
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn crypto2_release(this: *mut c_void) -> u32 {
    let this = this as *mut CryptoCallback2;
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this));
    }
    left
}

unsafe extern "system" fn get_text_password2(
    this: *mut c_void,
    password_is_defined: *mut i32,
    password: *mut *mut u16,
) -> Hresult {
    if password_is_defined.is_null() || password.is_null() {
        return E_FAIL;
    }
    let this = this as *mut CryptoCallback2;
    let obj: &CryptoCallback2 = &*this;
    obj.state
        .asked
        .store(1, std::sync::atomic::Ordering::SeqCst);
    match &obj.state.password {
        Some(p) => {
            *password_is_defined = 1;
            *password = alloc_bstr(p);
            if (*password).is_null() {
                return E_FAIL;
            }
            S_OK
        }
        // v2 can report "no password" without failing; unlike v1's E_FAIL.
        None => {
            *password_is_defined = 0;
            *password = std::ptr::null_mut();
            S_OK
        }
    }
}

/// Create the standalone v1+v2 crypto companions, each with refcount 1 (ours).
pub(crate) fn crypto_new(password: Option<String>) -> *mut CryptoPair {
    let state = Arc::new(OpenState {
        password,
        asked: std::sync::atomic::AtomicU8::new(0),
        volume_dir: PathBuf::new(),
        volume_name: String::new(),
        io_error: std::sync::Mutex::new(None),
    });
    let v1 = Box::into_raw(Box::new(CryptoCallback {
        obj: ComObject { vt: &CRYPTO_VT },
        refs: AtomicU32::new(1),
        state: Arc::clone(&state),
    }));
    let v2 = Box::into_raw(Box::new(CryptoCallback2 {
        obj: ComObject { vt: &CRYPTO2_VT },
        refs: AtomicU32::new(1),
        state,
    }));
    Box::into_raw(Box::new(CryptoPair { v1, v2 }))
}

pub(crate) unsafe fn crypto_addref_void(p: *mut c_void) -> u32 {
    crypto_add_ref(p)
}

/// Release the pair's own references and free the holder. Any references the
/// engine still holds on a companion keep that companion alive on its own.
pub(crate) unsafe fn crypto_release_void(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    let pair = p as *mut CryptoPair;
    crypto_release((*pair).v1 as *mut c_void);
    crypto2_release((*pair).v2 as *mut c_void);
    drop(Box::from_raw(pair));
}

/// QI helper shared by the extract/update callbacks: when `riid` names either
/// crypto interface, hand out the matching companion (AddRef'd) and return
/// `Some(S_OK)`; anything else returns `None` so the caller can continue
/// matching its own IIDs.
///
/// # Safety
/// `crypto` must be a live pair from [`crypto_new`], and `ppv` writable.
pub(crate) unsafe fn qi_hand_out_crypto(
    crypto: *mut c_void,
    riid: &Guid,
    ppv: *mut *mut c_void,
) -> Option<Hresult> {
    if crypto.is_null() {
        return None;
    }
    let pair = crypto as *mut CryptoPair;
    if *riid == IID_ICRYPTO_GET_TEXT_PASSWORD {
        let v1 = (*pair).v1 as *mut c_void;
        crypto_addref_void(v1);
        *ppv = v1;
        Some(S_OK)
    } else if *riid == IID_ICRYPTO_GET_TEXT_PASSWORD2 {
        let v2 = (*pair).v2 as *mut c_void;
        crypto2_add_ref(v2);
        *ppv = v2;
        Some(S_OK)
    } else {
        None
    }
}

/// Owns the callback objects for one `Open` call.
pub struct OpenCallbackOwner {
    open: *mut OpenCallback,
    crypto: *mut CryptoCallback,
    volume: *mut VolumeCallback,
}

impl OpenCallbackOwner {
    /// `volume_dir`/`volume_name` describe the file being opened so the volume
    /// callback can resolve sibling `.NNN` volumes. Pass `with_volume = false`
    /// for a re-open stage that must not perform its own volume discovery
    /// (the combined stream already carries every volume).
    pub fn new(
        password: Option<String>,
        volume_dir: PathBuf,
        volume_name: String,
        with_volume: bool,
    ) -> Self {
        let state = Arc::new(OpenState {
            password,
            asked: std::sync::atomic::AtomicU8::new(0),
            volume_dir,
            volume_name,
            io_error: std::sync::Mutex::new(None),
        });
        let crypto = Box::new(CryptoCallback {
            obj: ComObject { vt: &CRYPTO_VT },
            refs: AtomicU32::new(1),
            state: Arc::clone(&state),
        });
        let crypto = Box::into_raw(crypto);
        let volume = if with_volume {
            let v = Box::new(VolumeCallback {
                obj: ComObject { vt: &VOLUME_VT },
                refs: AtomicU32::new(1),
                state: Arc::clone(&state),
            });
            Box::into_raw(v)
        } else {
            std::ptr::null_mut()
        };
        let open = Box::new(OpenCallback {
            obj: ComObject { vt: &OPEN_VT },
            refs: AtomicU32::new(1),
            state,
            crypto,
            volume,
        });
        OpenCallbackOwner {
            open: Box::into_raw(open),
            crypto,
            volume,
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
        let volume = self.volume;
        std::mem::forget(self);
        (OPEN_VT.release)(open as *mut c_void);
        (CRYPTO_VT.release)(crypto as *mut c_void);
        if !volume.is_null() {
            (VOLUME_VT.release)(volume as *mut c_void);
        }
    }
}

impl Drop for OpenCallbackOwner {
    fn drop(&mut self) {
        unsafe {
            (OPEN_VT.release)(self.open as *mut c_void);
            (CRYPTO_VT.release)(self.crypto as *mut c_void);
            if !self.volume.is_null() {
                (VOLUME_VT.release)(self.volume as *mut c_void);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::vtables::InStreamVt;

    fn name_units(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Ask the open callback for its volume interface and call `GetStream` on
    /// it, exactly like the engine's `Split` handler does.
    unsafe fn volume_get_stream_via_qi(
        owner: &OpenCallbackOwner,
        name: &[u16],
    ) -> (Hresult, *mut c_void) {
        let open = owner.as_void();
        let mut volume: *mut c_void = std::ptr::null_mut();
        let open_vt = &**(open as *const *const ArchiveOpenCallbackVt);
        let hr = (open_vt.query_interface)(open, &IID_IARCHIVE_OPEN_VOLUME_CALLBACK, &mut volume);
        assert_eq!(hr, S_OK);
        let vol_vt = &**(volume as *const *const OpenVolumeCallbackVt);
        let mut stream: *mut c_void = std::ptr::null_mut();
        let hr = (vol_vt.get_stream)(volume, name.as_ptr(), &mut stream);
        (vol_vt.release)(volume);
        (hr, stream)
    }

    unsafe fn release_stream(stream: *mut c_void) {
        let vt = &**(stream as *const *const InStreamVt);
        (vt.release)(stream);
    }

    #[test]
    fn volume_get_stream_reports_s_false_only_for_a_missing_volume() {
        let dir = std::env::temp_dir().join(format!("zn-callbacks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vol.7z.002"), b"second volume").unwrap();

        let owner = OpenCallbackOwner::new(None, dir.clone(), "vol.7z.001".to_string(), true);
        unsafe {
            // An existing sibling volume yields a live stream.
            let (hr, stream) = volume_get_stream_via_qi(&owner, &name_units("vol.7z.002"));
            assert_eq!(hr, S_OK);
            assert!(!stream.is_null());
            release_stream(stream);

            // An absent volume is the one case that means "sequence ends here".
            let (hr, stream) = volume_get_stream_via_qi(&owner, &name_units("vol.7z.003"));
            assert_eq!(hr, S_FALSE);
            assert!(stream.is_null());
        }
        assert!(
            owner.state().io_error.lock().unwrap().is_none(),
            "a missing volume is not an I/O failure"
        );

        unsafe {
            // Present but unreadable (opening a directory fails on Windows)
            // must be reported as a real error and recorded, never as S_FALSE:
            // S_FALSE would truncate the archive silently.
            let (hr, stream) = volume_get_stream_via_qi(&owner, &name_units("."));
            assert_eq!(hr, E_FAIL);
            assert!(stream.is_null());
        }
        assert!(owner.state().io_error.lock().unwrap().is_some());

        drop(owner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn volume_get_stream_rejects_an_unterminated_engine_name() {
        let dir = std::env::temp_dir().join(format!("zn-callbacks-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let owner = OpenCallbackOwner::new(None, dir.clone(), "vol.7z.001".to_string(), true);
        // No NUL terminator anywhere: the bounded scan must give up instead of
        // reading past the end of the buffer.
        let name = vec![b'x' as u16; 32768];
        unsafe {
            let (hr, stream) = volume_get_stream_via_qi(&owner, &name);
            assert_eq!(hr, E_FAIL);
            assert!(stream.is_null());
        }

        drop(owner);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

