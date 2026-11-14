//! Give every window this process owns the same icon, both sizes, and give the
//! Win11 shell package a logo worth drawing.
//!
//! Why this exists at all, measured on Windows 11 26H1 (28020):
//!
//! * winit's `ViewportBuilder::with_icon` sets only `ICON_SMALL`. A window with
//!   no `ICON_BIG` has nothing for the taskbar to draw, so it falls back to the
//!   window *class* icon — and winit registers its class with no icon at all —
//!   and the taskbar button comes out as Windows' generic grey square. That is
//!   exactly what a launch through 添加到压缩包… looked like: the create wizard
//!   is created first and takes the focus, eframe's own icon setter (which only
//!   ever touches `GetActiveWindow`) gave *it* both sizes, and the main window
//!   was left with `ICON_BIG == 0` — read back with `WM_GETICON` on both
//!   top-level windows.
//! * So the icons are set here for real: both sizes, loaded from this exe's own
//!   `ICON` resource (the same `.ico` Explorer draws), on every top-level window
//!   of the process.
//! * The window *class* gets them too, because Windows hands a new window its
//!   class icon when it has none of its own. That covers the create wizard,
//!   which is an immediate viewport created after this call, and anything else
//!   this process opens later.
//!
//! One more thing icons cannot fix from here, kept as a note because it is the
//! other half of the same bug (measured the same way):
//!
//! * A launch from the Win11 shell extension inherits MSIX **package identity**
//!   (`GetPackageFullName` → `ZipNest.Shell_…`). For a packaged process the
//!   taskbar draws the package's `Square44x44Logo`, not the window icon — and
//!   with `WM_GETICON` reporting both sizes set, the button still came out grey,
//!   because `packaging\win11-shell\Assets\Square44x44Logo.png` was a nearly
//!   transparent 44×44 image. Those assets now come from the app's own icon;
//!   `SetCurrentProcessExplicitAppUserModelID` is *not* a workaround — Windows
//!   ignores it for a process that has package identity (read back with
//!   `GetApplicationUserModelId` after setting it: still the package's).
//!
//! The window icons are applied once on the first frame, and again when the
//! wizard opens.

/// Resource id of the icon in `src/assets/app.rc` (`1 ICON ...`).
#[cfg(windows)]
const ICON_RESOURCE_ID: usize = 1;

#[cfg(windows)]
mod imp {
    use super::ICON_RESOURCE_ID;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};

    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTCOLOR: u32 = 0;
    /// `WM_SETICON` writes the window's icon. It is *not* `WM_GETICON`
    /// (0x7F, the adjacent value): writing with 0x7F silently reads instead,
    /// and the window keeps no icon at all.
    const WM_SETICON: u32 = 0x80;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    /// `GCLP_HICON` / `GCLP_HICONSM`: the class's default icons, which is what
    /// Windows falls back to for a window created after this call.
    const GCLP_HICON: i32 = -14;
    const GCLP_HICONSM: i32 = -34;
    const SM_CXICON: i32 = 11;
    const SM_CYICON: i32 = 12;
    const SM_CXSMICON: i32 = 49;
    const SM_CYSMICON: i32 = 50;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(*mut c_void, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetSystemMetrics(index: i32) -> i32;
        fn LoadImageW(
            hinst: *mut c_void,
            name: *const u16,
            kind: u32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> *mut c_void;
        fn SetClassLongPtrW(hwnd: *mut c_void, index: i32, value: isize) -> isize;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    /// The two loaded icons. Kept for the life of the process instead of being
    /// reloaded per window: `LoadImageW` hands back a new handle each time, and
    /// a handle nobody destroys is a leak.
    static BIG: AtomicIsize = AtomicIsize::new(0);
    static SMALL: AtomicIsize = AtomicIsize::new(0);

    /// Give every top-level window of this process both icon sizes, and make
    /// them the class defaults so windows created afterwards inherit them.
    pub fn apply_to_all_windows() {
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            if hinst.is_null() {
                return;
            }
            let (big, small) = load_icons(hinst);
            if big == 0 || small == 0 {
                // Half an icon pair would look worse than none; try again on
                // the next call rather than caching a failure.
                return;
            }
            BIG.store(big, Ordering::Relaxed);
            SMALL.store(small, Ordering::Relaxed);
            EnumWindows(each_window, 0);
        }
    }

    /// Load both sizes once; a cached pair is reused across windows.
    unsafe fn load_icons(hinst: *mut c_void) -> (isize, isize) {
        let cached = (BIG.load(Ordering::Relaxed), SMALL.load(Ordering::Relaxed));
        if cached.0 != 0 && cached.1 != 0 {
            return cached;
        }
        // `MAKEINTRESOURCE(1)`: the numeric id reinterpreted as a pointer.
        let name = ICON_RESOURCE_ID as *const u16;
        let cx = GetSystemMetrics(SM_CXICON).max(32);
        let cy = GetSystemMetrics(SM_CYICON).max(32);
        let big = LoadImageW(hinst, name, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) as isize;
        let sx = GetSystemMetrics(SM_CXSMICON).max(16);
        let sy = GetSystemMetrics(SM_CYSMICON).max(16);
        let small = LoadImageW(hinst, name, IMAGE_ICON, sx, sy, LR_DEFAULTCOLOR) as isize;
        (big, small)
    }

    extern "system" fn each_window(hwnd: *mut c_void, _lparam: isize) -> i32 {
        unsafe {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != GetCurrentProcessId() {
                return 1;
            }
            let big = BIG.load(Ordering::Relaxed);
            let small = SMALL.load(Ordering::Relaxed);
            SetClassLongPtrW(hwnd, GCLP_HICON, big);
            SetClassLongPtrW(hwnd, GCLP_HICONSM, small);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, big);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small);
            1
        }
    }
}

#[cfg(windows)]
pub fn apply_to_all_windows() {
    imp::apply_to_all_windows();
}

#[cfg(not(windows))]
pub fn apply_to_all_windows() {}
