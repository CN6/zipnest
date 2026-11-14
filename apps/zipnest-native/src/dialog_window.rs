//! Make the create wizard behave like a dialog of the main window.
//!
//! The wizard is an immediate eframe viewport, so it is a plain sibling
//! top-level window with no relationship to the window it belongs to. Two
//! things follow from that, both visible to users:
//!
//! * Windows places it wherever its own cascade rule says (measured: main
//!   window at 507,587 and wizard at 338,338 — two unrelated windows that drift
//!   apart), and clicking the main window drops the wizard behind it. Users read
//!   that as "the archive program opened a second, separate window".
//! * A sibling window takes a taskbar button of its own, so one app looked like
//!   two, and that stray button is the one that had no icon.
//!
//! eframe/egui have no way to declare an owner, so this sets it on the HWND
//! directly (`GWL_HWNDPARENT` on a top-level window means *owner*). Windows then
//! keeps the wizard above its owner, minimizes it together with the owner,
//! closes it with the owner, and stops giving it a taskbar button of its own —
//! ordinary dialog behaviour.
//!
//! Both windows are looked up among this process' own windows only, so a second
//! instance running beside this one can never be picked up by mistake.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    /// `GWL_HWNDPARENT`. On a top-level window this is the *owner*, not a
    /// parent (that is `GWLP_HWNDPARENT`'s only meaning for `WS_POPUP`s).
    const GWL_HWNDPARENT: i32 = -8;
    /// `GW_OWNER`, the `GetWindow` direction that reads the owner back.
    const GW_OWNER: u32 = 4;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(*mut c_void, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        fn GetWindowTextW(hwnd: *mut c_void, text: *mut u16, max: i32) -> i32;
        fn GetWindow(hwnd: *mut c_void, cmd: u32) -> *mut c_void;
        fn SetWindowLongPtrW(hwnd: *mut c_void, index: i32, value: isize) -> isize;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }

    /// One pass over this process' top-level windows, collecting the first
    /// window for each title we are looking for.
    struct Lookup {
        wanted: Vec<String>,
        found: Vec<Option<*mut c_void>>,
    }

    extern "system" fn visit(hwnd: *mut c_void, lparam: isize) -> i32 {
        // SAFETY: `lparam` is the `&mut Lookup` `find_titles` passed in, alive
        // for the duration of the synchronous `EnumWindows` call.
        let state = unsafe { &mut *(lparam as *mut Lookup) };
        unsafe {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != GetCurrentProcessId() {
                return 1;
            }
            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if len <= 0 {
                return 1;
            }
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            for (slot, want) in state.found.iter_mut().zip(&state.wanted) {
                if slot.is_none() && title == *want {
                    *slot = Some(hwnd);
                }
            }
            1
        }
    }

    fn find_titles(titles: &[&str]) -> Vec<Option<*mut c_void>> {
        let mut state = Lookup {
            wanted: titles.iter().map(|t| (*t).to_string()).collect(),
            found: vec![None; titles.len()],
        };
        unsafe { EnumWindows(visit, &mut state as *mut Lookup as isize) };
        state.found
    }

    /// Make the `child` window owned by the `owner` window. `false` when
    /// either is missing (the wizard exists only from its first frame on) or
    /// the owner did not stick.
    pub fn attach(child: &str, owner: &str) -> bool {
        let found = find_titles(&[child, owner]);
        let (Some(child_hwnd), Some(owner_hwnd)) = (found[0], found[1]) else {
            return false;
        };
        if child_hwnd == owner_hwnd {
            return false;
        }
        // A zero return means "had no owner before" *or* "failed", so the
        // outcome is read back instead of inferred.
        unsafe { SetWindowLongPtrW(child_hwnd, GWL_HWNDPARENT, owner_hwnd as isize) };
        unsafe { GetWindow(child_hwnd, GW_OWNER) == owner_hwnd }
    }
}

#[cfg(windows)]
pub fn attach(child_title: &str, owner_title: &str) -> bool {
    imp::attach(child_title, owner_title)
}

#[cfg(not(windows))]
pub fn attach(_child_title: &str, _owner_title: &str) -> bool {
    false
}
