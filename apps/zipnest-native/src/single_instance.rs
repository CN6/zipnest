//! Single instance support.
//!
//! Explorer starts a fresh process for every double-click and every context
//! menu command, so opening a second archive while a window is already up used
//! to stack a second window — and the first one stayed there. The old Tauri
//! build avoided that with `tauri_plugin_single_instance`; this is the same
//! model for the native app, and what Explorer users expect from 7-Zip/WinRAR:
//! the later process hands its request to the running window and exits.
//!
//! Two mechanisms, deliberately separate:
//!
//! * a kernel **named mutex** decides who is the running instance. It cannot go
//!   stale — the OS releases it when the process dies, unlike a lock file.
//! * a **queue file** carries what the later process wanted (an archive to open
//!   or paths for the create wizard). A file is enough here and needs no window
//!   procedure hooking; the running instance drains it from its UI loop.
//!
//! `zipnest.exe --new-instance` skips the handoff for callers that really do
//! want a second window.

use std::io::Write;
use std::path::PathBuf;

/// Main window title. Shared with `ViewportBuilder::with_title` so the two can
/// never drift apart — [`focus_existing`] finds the window by this string.
pub const WINDOW_TITLE: &str = "ZipNest 解压缩";

/// What a later launch asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRequest {
    /// Show this archive in the running window.
    Open(String),
    /// Pre-fill the create wizard with these paths.
    Add(Vec<String>),
}

/// One JSON object per line, in the app data dir (the same folder the
/// uninstaller removes).
pub fn queue_path() -> PathBuf {
    zipnest_ipc::settings::default_path().with_file_name("launch-queue.jsonl")
}

fn encode(req: &LaunchRequest) -> String {
    let value = match req {
        LaunchRequest::Open(path) => serde_json::json!({ "open": path }),
        LaunchRequest::Add(paths) => serde_json::json!({ "add": paths }),
    };
    let mut line = value.to_string();
    line.push('\n');
    line
}

/// Parse whatever the queue holds. A half-written or unknown line is dropped
/// instead of failing the whole batch: requests are tiny, single-line, and
/// written by one process at a time, so a torn line can only be the last one.
pub fn parse_queue(text: &str) -> Vec<LaunchRequest> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(path) = value.get("open").and_then(|p| p.as_str()) {
            out.push(LaunchRequest::Open(path.to_string()));
        } else if let Some(paths) = value.get("add").and_then(|p| p.as_array()) {
            let paths: Vec<String> = paths
                .iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect();
            if !paths.is_empty() {
                out.push(LaunchRequest::Add(paths));
            }
        }
    }
    out
}

/// Append one request for the running instance. `None` means "nothing to say,
/// just raise the window".
pub fn enqueue(req: Option<&LaunchRequest>) -> std::io::Result<()> {
    let Some(req) = req else {
        return Ok(());
    };
    let path = queue_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(encode(req).as_bytes())
}

/// Take every queued request, truncating the file. Truncating before the
/// requests are acted on is what keeps them from being replayed.
pub fn take_queue() -> Vec<LaunchRequest> {
    let path = queue_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    if text.trim().is_empty() {
        return Vec::new();
    }
    let _ = std::fs::write(&path, "");
    parse_queue(&text)
}

/// Cheap probe for the watcher thread: is anything waiting?
pub fn queue_has_items() -> bool {
    std::fs::metadata(queue_path())
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// Marker the *primary* window writes while one of its jobs runs.
///
/// A later launch reads it to choose between "hand the request to the running
/// window" and "open another window so both jobs can go at once". Only the
/// primary publishes it, because only the primary is a reuse target.
///
/// Staleness after a crash is harmless: the marker is only consulted while the
/// single-instance mutex is held, i.e. while that window is alive, and every
/// window clears it when it takes the primary role with no job running.
pub fn busy_path() -> PathBuf {
    queue_path().with_file_name("busy")
}

/// Publish or clear the busy marker (best effort — never fails the caller).
pub fn set_busy(busy: bool) {
    let path = busy_path();
    if busy {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, "1");
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

/// Is the running (primary) window busy with a job?
pub fn is_busy() -> bool {
    busy_path().exists()
}

// ---------------------------------------------------------------------------
// Windows: the named mutex and "raise the other window"
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::WINDOW_TITLE;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const ERROR_ALREADY_EXISTS: u32 = 183;
    const SW_RESTORE: i32 = 9;
    /// `AllowSetForegroundWindow` argument that lets any process come forward.
    const ASFW_ANY: u32 = u32::MAX;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
        fn GetLastError() -> u32;
        fn FindWindowW(class: *const u16, title: *const u16) -> *mut c_void;
        fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Claim the single-instance mutex. `None` = another instance already holds
    /// it, so this process is only here to hand a request over.
    ///
    /// The handle is intentionally never closed: the mutex has to live exactly
    /// as long as this process, and the OS releases it on exit.
    pub fn claim() -> Option<()> {
        // `Local\` scopes it to the logon session; the app is per-user anyway.
        let name = wide(r"Local\ZipNest.SingleInstance");
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        let last_error = unsafe { GetLastError() };
        if handle.is_null() {
            // Could not even create it — running is better than refusing to.
            return Some(());
        }
        if last_error == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(())
    }

    /// Bring the running instance's window to the front. Best effort: the call
    /// can be refused if the user is typing in another app right now, in which
    /// case the queued request still lands.
    pub fn focus_existing() {
        let title = wide(WINDOW_TITLE);
        let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if hwnd.is_null() {
            return;
        }
        unsafe {
            // A shell launch is allowed to hand the foreground over.
            AllowSetForegroundWindow(ASFW_ANY);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(windows)]
pub use win::{claim, focus_existing};

#[cfg(not(windows))]
pub fn claim() -> Option<()> {
    Some(())
}

#[cfg(not(windows))]
pub fn focus_existing() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_the_queue_format() {
        let open = LaunchRequest::Open(r"D:\some folder\a b.zip".into());
        let add = LaunchRequest::Add(vec![r"D:\x".into(), r"D:\y.txt".into()]);
        let mut text = encode(&open);
        text.push_str(&encode(&add));
        assert_eq!(parse_queue(&text), vec![open, add]);
    }

    #[test]
    fn a_torn_or_unknown_line_is_dropped_not_fatal() {
        let text = concat!(
            "{\"open\":\"a.zip\"}\n",
            "{\"ope\n",
            "\n",
            "42\n",
            "[]\n",
            "{\"add\":[]}\n",
            "{\"add\":\"not a list\"}\n",
            "{\"open\":\"b.zip\"}\n",
        );
        assert_eq!(
            parse_queue(text),
            vec![
                LaunchRequest::Open("a.zip".into()),
                LaunchRequest::Open("b.zip".into()),
            ]
        );
    }

    #[test]
    fn an_empty_queue_parses_to_nothing() {
        assert!(parse_queue("").is_empty());
        assert!(parse_queue("\n \n").is_empty());
    }

    #[test]
    fn the_queue_lives_beside_the_settings_file() {
        let queue = queue_path();
        assert_eq!(queue.file_name().unwrap(), "launch-queue.jsonl");
        assert_eq!(
            queue.parent(),
            zipnest_ipc::settings::default_path().parent()
        );
    }

    #[test]
    fn the_busy_marker_sits_next_to_the_queue() {
        // Same folder: the uninstaller removes both, and neither can be
        // confused with the queue file itself.
        assert_eq!(busy_path().parent(), queue_path().parent());
        assert_eq!(busy_path().file_name().unwrap(), "busy");
        assert_ne!(busy_path(), queue_path());
    }
}
