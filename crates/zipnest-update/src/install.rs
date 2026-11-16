//! Deciding whether an in-app install is possible here, and starting the
//! installer correctly.
//!
//! ## Why the installer cannot simply be spawned
//!
//! `packaging/installer.nsi` sets `RequestExecutionLevel admin`, so the setup
//! exe's manifest says `requireAdministrator` (verified by reading the embedded
//! manifest of the shipped v0.4.39 setup). `CreateProcess` — i.e.
//! `std::process::Command::spawn` — cannot elevate: it fails with
//! **`ERROR_ELEVATION_REQUIRED` (740)** and never shows a UAC prompt. That, not
//! a file lock, is very likely what broke the v0.2.x updater on users' machines
//! while working on the maintainer's elevated dev box: the error was swallowed by
//! `let _ =` and the app called `process::exit(0)` one second later, so the
//! window closed and the version never changed.
//!
//! `ShellExecuteExW` with `lpVerb = "runas"` is the one call that both raises the
//! UAC prompt and hands back a process handle (`SEE_MASK_NOCLOSEPROCESS`) to wait
//! on and read the exit code from.
//!
//! ## Why the app must quit first, and not wait
//!
//! In silent mode (`/S`) the installer's forced-close question is pre-answered
//! "no" (`ZN_FORCE_ASK_SD = IDNO`): if `zipnest.exe` is still running after ten
//! polite `taskkill` attempts it **aborts** with exit code 2 and writes nothing.
//! So the app exits before the installer starts, and a small detached watcher
//! process — a copy of this exe, running from `%LOCALAPPDATA%` — waits for the
//! app's PID to disappear and then starts the installer. The copy is
//! load-bearing: a watcher running from `$INSTDIR\zipnest.exe` would make the
//! installer's `Delete` + `SetOverwrite try` silently skip the locked file and
//! reproduce exactly the bug above.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How this copy of ZipNest was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMode {
    /// Installed by the NSIS installer into `install_dir`; the in-app update can
    /// replace it.
    Installed { install_dir: PathBuf },
    /// Running from a portable extraction (or a dev build): there is no
    /// `Uninstall\ZipNest\InstallLocation` pointing at this exe.
    Portable,
}

/// Registry key the installer writes (`installer.nsi`, `InstallLocation`).
#[cfg_attr(not(windows), allow(dead_code))]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ZipNest";

/// Classify an exe path against the recorded install location.
///
/// Split out from the registry read so it can be tested without touching the
/// registry: the interesting logic is the comparison, not the lookup.
pub fn classify(recorded: Option<&str>, exe: &Path) -> InstallMode {
    let Some(recorded) = recorded else {
        return InstallMode::Portable;
    };
    let recorded = recorded.trim().trim_end_matches(['\\', '/']);
    if recorded.is_empty() {
        return InstallMode::Portable;
    }
    let dir = exe.parent().unwrap_or_else(|| Path::new(""));
    if same_dir(Path::new(recorded), dir) {
        InstallMode::Installed {
            install_dir: dir.to_path_buf(),
        }
    } else {
        InstallMode::Portable
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    !norm(a).is_empty() && norm(a) == norm(b)
}

/// Read `InstallLocation` from HKCU and classify this exe against it.
///
/// A dev build under `target\` gets `Portable`, which is correct: installing it
/// would drop a second copy into Program Files while the user keeps launching
/// the dev one.
pub fn detect(exe: &Path) -> InstallMode {
    classify(recorded_install_dir().as_deref(), exe)
}

/// The recorded install directory, if any.
#[cfg(windows)]
pub fn recorded_install_dir() -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            ty: *mut u32,
            data: *mut std::ffi::c_void,
            data_len: *mut u32,
        ) -> i32;
    }
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const ERROR_SUCCESS: i32 = 0;

    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let sub_key = wide(UNINSTALL_KEY);
    let value = wide("InstallLocation");

    // Two calls: ask for the size, then read. A single fixed buffer would
    // silently truncate a long install path, and a truncated path is worse than
    // no path at all -- it fails the comparison in `classify` and makes a real
    // install look like a portable copy, so the update is refused for no
    // visible reason.
    const ERROR_MORE_DATA: i32 = 234;
    // Room for a NUL plus the terminating character.
    let mut wide_len: u32 = 0;
    let mut ty = 0u32;
    let probe = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut ty,
            std::ptr::null_mut(),
            &mut wide_len,
        )
    };
    if probe != ERROR_SUCCESS && probe != ERROR_MORE_DATA {
        return None;
    }
    // `wide_len` is in bytes. Allow for the NUL the first call may not count.
    let chars = (wide_len as usize / 2) + 1;
    let mut buf = vec![0u16; chars.max(2)];

    let mut len = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut ty,
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if rc != ERROR_SUCCESS || len < 2 {
        return None;
    }
    // `len` counts bytes *including* the terminating NUL, so the string is
    // `len / 2` wide characters. The previous code dropped one more, which
    // truncated the final character -- `D:\ZipNest` read back as `D:\ZipNes`
    // and classified as portable.
    let taken = (len as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..taken]);
    let text = text.trim_matches('\0').trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(not(windows))]
pub fn recorded_install_dir() -> Option<String> {
    None
}

/// Outcome of asking Windows to run the installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// `SHELLEXECUTEINFO::hProcess` came back non-null.
    Started,
    /// The user declined the UAC prompt (`ERROR_CANCELLED` 1223). A normal
    /// answer, not a failure: keep the download and offer the button again.
    Cancelled,
    /// ShellExecuteExW itself failed with this `GetLastError` value.
    Failed(u32),
}

impl LaunchOutcome {
    /// Map a `ShellExecuteExW` result + `GetLastError` to an outcome.
    ///
    /// Split out for testing: 1223 has to be "cancelled", not "failed", or the
    /// UI tells the user something went wrong when they simply pressed 否.
    pub fn from_result(started: bool, last_error: u32) -> Self {
        if started {
            LaunchOutcome::Started
        } else if last_error == 1223 {
            LaunchOutcome::Cancelled
        } else {
            LaunchOutcome::Failed(last_error)
        }
    }
}

/// What the installer's exit code means.
///
/// `installer.nsi` defines 2 = ZipNest still running, 3 = files busy, 4 = the
/// Win11 context menu could not be registered. 4 is not a failed update — the
/// app files were written; only the shell package needs a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallResult {
    Ok,
    StillRunning,
    FilesBusy,
    ShellPackageOnly,
    Other(i32),
}

impl InstallResult {
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            0 => InstallResult::Ok,
            2 => InstallResult::StillRunning,
            3 => InstallResult::FilesBusy,
            4 => InstallResult::ShellPackageOnly,
            other => InstallResult::Other(other),
        }
    }

    /// Whether the app files on disk are the new version now.
    pub fn applied(self) -> bool {
        matches!(self, InstallResult::Ok | InstallResult::ShellPackageOnly)
    }

    /// i18n key describing a non-`Ok` outcome.
    pub fn message_key(self) -> Option<&'static str> {
        match self {
            InstallResult::Ok => None,
            InstallResult::ShellPackageOnly => Some("update.installed.shell_failed"),
            InstallResult::StillRunning => Some("update.failed.running"),
            InstallResult::FilesBusy => Some("update.failed.busy"),
            InstallResult::Other(_) => Some("update.failed.installer"),
        }
    }
}

/// `/S` plus an explicit install directory.
///
/// `/D=` must be the **last** parameter and must **not** be quoted (spaces are
/// fine unquoted). Passing it means a user who moved the install still gets the
/// update in the right place, instead of a second copy in the default location.
pub fn silent_params(install_dir: &Path) -> String {
    format!("/S /D={}", install_dir.display())
}

/// Where downloads and the updater copy live.
///
/// `%LOCALAPPDATA%\ZipNest`, not `%TEMP%`: Disk Cleanup and third-party
/// "optimisers" delete TEMP while a transfer is running, and
/// "unsigned exe downloaded to TEMP, then run to write into Program Files" is
/// the textbook antivirus heuristic.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    data_dir_in(&base)
}

/// The layout under whichever base: `%LOCALAPPDATA%/ZipNest` in production, a
/// temp directory in a test that must not touch the real one.
///
/// Reading the environment once, at the outermost function, is what lets the
/// tests be pure: `set_var` is process-global and this crate's tests run on
/// parallel threads, so a test that redirects `LOCALAPPDATA` would otherwise
/// redirect another test's writes.
fn data_dir_in(base: &std::path::Path) -> PathBuf {
    base.join("ZipNest")
}

pub fn updates_dir(version: &str) -> PathBuf {
    updates_dir_in(&data_dir(), version)
}

fn updates_dir_in(dir: &std::path::Path, version: &str) -> PathBuf {
    dir.join("updates").join(version)
}

pub fn updater_dir() -> PathBuf {
    data_dir().join("updater")
}

/// Delete leftovers from earlier update attempts.
///
/// Keeps the `updates/<version>` directories named in `keep`, and nothing else.
/// That has to include the version being *downloaded*, not just the version
/// currently running: the download lives in `updates/<target>`, which by
/// definition is a different directory from `updates/<installed>`. Keeping only
/// the latter deleted the in-progress `.part` on every launch and threw away the
/// resume the downloader is built around — and worse, a relaunch while the
/// detached watcher was still waiting on the installer could delete the setup
/// exe out from under it.
pub fn clean_stale(keep: &[&str]) {
    let updates = data_dir().join("updates");
    let Ok(entries) = std::fs::read_dir(&updates) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if keep.contains(&name.as_str()) {
            continue;
        }
        // A directory touched in the last hour belongs to a download that is
        // probably still running in *another* ZipNest process — the single
        // instance mutex only covers the normal window, not a second user
        // session or a portable copy launched elsewhere. Deleting it would
        // remove a live `.part` (and, during an install, the setup exe the
        // elevated installer is reading). Stale by age instead of by name.
        let recently_used = std::fs::metadata(entry.path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(3600));
        if recently_used {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
    // A previous run's `.old` exe, from the portable replace path.
    if let Ok(exe) = std::env::current_exe() {
        let mut old = exe.as_os_str().to_owned();
        old.push(".old");
        let _ = std::fs::remove_file(PathBuf::from(old));
    }
}

/// Files the watcher and the app exchange to report the outcome of an install.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingInstall {
    /// Version the update was for.
    pub version: String,
    /// Installer exit code, once known. Absent while the install is running.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// `LaunchOutcome` described as a string, for the "user said no" case where
    /// there is no exit code at all.
    #[serde(default)]
    pub launch_error: Option<String>,
    /// SHA-256 the setup exe had when it was verified after download.
    ///
    /// Carried into the watcher so the file can be re-hashed immediately before
    /// `ShellExecuteExW("runas")`. The download verified one path — its own
    /// `.part` — and then the watcher waited up to a minute before elevating
    /// whatever was at that path; anything running as the same user could
    /// replace it in between and have its own bytes launched as administrator.
    #[serde(default)]
    pub sha256: Option<String>,
}

pub fn pending_path() -> PathBuf {
    pending_path_in(&data_dir())
}

fn pending_path_in(dir: &std::path::Path) -> PathBuf {
    dir.join("update-pending.json")
}

pub fn write_pending(p: &PendingInstall) -> std::io::Result<()> {
    write_pending_to(&pending_path(), p)
}

/// Written whole, then renamed into place: a crash mid-write must not leave a
/// truncated JSON that the next launch cannot parse, because a record it cannot
/// read is indistinguishable from "no update was in progress".
fn write_pending_to(path: &std::path::Path, p: &PendingInstall) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    if std::fs::rename(&tmp, path).is_err() {
        // Replacing an existing file can fail while something holds it open;
        // remove-then-rename is better than losing the record entirely.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

pub fn read_pending() -> Option<PendingInstall> {
    read_pending_from(&pending_path())
}

fn read_pending_from(path: &std::path::Path) -> Option<PendingInstall> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn clear_pending() {
    clear_pending_at(&pending_path());
}

/// How long an "install started" record may sit without a result before it is
/// treated as a dead attempt.
///
/// The watcher writes its result within ten minutes at the outside (its own
/// installer wait is capped at 600 s, plus up to 60 s for the app to exit). A
/// record older than this means the watcher was killed — a reboot, Task Manager
/// — and no result is ever coming, so the next launch must say so instead of
/// showing "更新正在进行" forever.
pub const PENDING_STALE: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Age of the pending record, if there is one.
pub fn pending_age() -> Option<std::time::Duration> {
    std::fs::metadata(pending_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

fn clear_pending_at(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// Copy this exe into `%LOCALAPPDATA%\ZipNest\updater\` and return the copy's
/// path.
///
/// The copy is what the watcher runs. It has to live outside the install
/// directory, or the installer would try to delete a running image and
/// `SetOverwrite try` would skip it without a word.
pub fn stage_watcher() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = updater_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join("zipnest-updater.exe");
    // Retry the replace: the previous watcher may still be exiting, and Defender
    // may still be scanning the copy.
    let mut delay = std::time::Duration::from_millis(500);
    let mut last = String::new();
    for attempt in 0..4 {
        match std::fs::copy(&me, &dest) {
            Ok(_) => return Ok(dest),
            Err(e) => {
                last = e.to_string();
                if attempt < 3 {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
        }
    }
    Err(last)
}

/// Start the installer elevated and return a handle to wait on.
///
/// `ShellExecuteExW` + `"runas"` is the only way from a normal user process:
/// `CreateProcess` on a `requireAdministrator` image fails with 740 instead of
/// prompting. `SEE_MASK_NOCLOSEPROCESS` is what makes the handle (and therefore
/// the exit code) available at all.
#[cfg(windows)]
pub fn launch_installer(
    setup: &Path,
    install_dir: &Path,
    expect_sha256: Option<&str>,
) -> Result<LaunchOutcome, String> {
    use std::os::windows::ffi::OsStrExt;

    // Re-hash immediately before elevating. `download` verified the file it
    // wrote, but the watcher then waited up to a minute for the app to exit
    // and possibly for a UAC answer; anything running as this user could
    // have replaced the file at that path in the meantime and had its own
    // bytes launched as administrator. The expected digest comes from the
    // Ed25519-signed manifest, not from the file.
    if let Some(want) = expect_sha256 {
        match crate::verify::sha256_file(setup) {
            Ok(got) if got.eq_ignore_ascii_case(want) => {}
            Ok(got) => {
                return Err(format!(
                    "the installer changed after it was verified ({got} != {want}); refusing to run it"
                ))
            }
            Err(e) => return Err(format!("could not re-read the installer: {e}")),
        }
    }

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: isize,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: isize,
        lp_id_list: *mut std::ffi::c_void,
        lp_class: *const u16,
        hkey_class: isize,
        dw_hot_key: u32,
        h_icon_or_monitor: isize,
        h_process: isize,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> u32;
    }

    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;
    /// `SW_SHOWNORMAL`; the installer draws its own progress window in silent
    /// mode, and the UAC dialog is Windows', not ours.
    const SW_SHOWNORMAL: i32 = 1;

    let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    };
    let verb: Vec<u16> = wide(std::ffi::OsStr::new("runas"));
    let file = wide(setup.as_os_str());
    let params = wide(std::ffi::OsStr::new(&silent_params(install_dir)));
    let dir = wide(install_dir.as_os_str());

    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        f_mask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        hwnd: 0,
        lp_verb: verb.as_ptr(),
        lp_file: file.as_ptr(),
        lp_parameters: params.as_ptr(),
        lp_directory: dir.as_ptr(),
        n_show: SW_SHOWNORMAL,
        h_inst_app: 0,
        lp_id_list: std::ptr::null_mut(),
        lp_class: std::ptr::null(),
        hkey_class: 0,
        dw_hot_key: 0,
        h_icon_or_monitor: 0,
        h_process: 0,
    };
    // SAFETY: `info` is a correctly sized, fully initialised struct whose
    // pointer fields all refer to live, NUL-terminated wide strings for the
    // duration of the call.
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    let err = if ok { 0 } else { unsafe { GetLastError() } };
    let outcome = LaunchOutcome::from_result(ok && info.h_process != 0, err);
    if let LaunchOutcome::Started = outcome {
        INSTALLER_HANDLE.store(info.h_process, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(outcome)
}

/// Handle of the installer started by [`launch_installer`], so [`await_installer`]
/// can wait on it without the caller threading an `isize` through the UI.
#[cfg(windows)]
static INSTALLER_HANDLE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Wait for the installer to finish and return its exit code.
///
/// The cap is generous on purpose: the installer runs
/// `powershell … install.ps1` for the Win11 shell package through
/// `nsExec::ExecToLog`, which blocks and can stall on a broken AppX store. A
/// watcher that gave up at 60 s would report failure on an install that was
/// still working.
#[cfg(windows)]
pub fn await_installer(timeout: std::time::Duration) -> Result<i32, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn WaitForSingleObject(handle: isize, millis: u32) -> u32;
        fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    const STILL_ACTIVE: u32 = 259;

    let handle = INSTALLER_HANDLE.load(std::sync::atomic::Ordering::SeqCst);
    if handle == 0 {
        return Err("no installer was started".into());
    }
    let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
    // SAFETY: `handle` came from ShellExecuteExW with SEE_MASK_NOCLOSEPROCESS
    // and has not been closed; the out-pointer is a valid stack `u32`.
    let wait = unsafe { WaitForSingleObject(handle, millis) };
    let mut code: u32 = 0;
    let got = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
    unsafe { CloseHandle(handle) };
    INSTALLER_HANDLE.store(0, std::sync::atomic::Ordering::SeqCst);
    if wait == WAIT_TIMEOUT {
        return Err("timeout".into());
    }
    if wait != WAIT_OBJECT_0 {
        return Err(format!("wait failed ({wait})"));
    }
    if !got {
        return Err("exit code unavailable".into());
    }
    if code == STILL_ACTIVE {
        return Err("still running".into());
    }
    Ok(code as i32)
}

/// Block until `pid` exits, up to `timeout`. Used by the watcher to let the app
/// quit before the installer starts — waiting on the PID, never on the exe path,
/// because the installer is about to replace that path.
#[cfg(windows)]
pub fn wait_for_process_exit(pid: u32, timeout: std::time::Duration) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn WaitForSingleObject(handle: isize, millis: u32) -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    // SAFETY: OpenProcess with SYNCHRONIZE on an arbitrary pid; a failure
    // returns null and is handled below.
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        // Already gone, or not ours to wait on. Either way, do not block the
        // install on it.
        return true;
    }
    let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
    let wait = unsafe { WaitForSingleObject(handle, millis) };
    unsafe { CloseHandle(handle) };
    wait == WAIT_OBJECT_0
}

/// Start the installed exe again, unelevated, once the installer is done.
#[cfg(windows)]
pub fn relaunch(exe: &Path) {
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    };
    let file = wide(exe.as_os_str());
    let op = wide(std::ffi::OsStr::new("open"));
    // SAFETY: all pointers refer to live NUL-terminated wide strings.
    unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }
}

#[cfg(not(windows))]
pub fn launch_installer(
    _setup: &Path,
    _install_dir: &Path,
    _expect_sha256: Option<&str>,
) -> Result<LaunchOutcome, String> {
    Err("in-app installation is Windows-only".into())
}

#[cfg(not(windows))]
pub fn await_installer(_timeout: std::time::Duration) -> Result<i32, String> {
    Err("in-app installation is Windows-only".into())
}

#[cfg(not(windows))]
pub fn wait_for_process_exit(_pid: u32, _timeout: std::time::Duration) -> bool {
    true
}

#[cfg(not(windows))]
pub fn relaunch(_exe: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_path_keeps_its_last_character() {
        // The bug this guards: the returned byte count includes the NUL, and
        // the old code dropped one character past it, so `D:\\ZipNest` came back
        // as `D:\\ZipNes` and a real install classified as portable. The parsing
        // is inline in a Win32 call, so this reproduces the shape: 8 chars + NUL
        // = 18 bytes.
        let utf16: Vec<u16> = "D:\\ZipNest\0".encode_utf16().collect();
        let len_bytes = (utf16.len() * 2) as u32;
        let taken = (len_bytes as usize / 2).min(utf16.len());
        let text = String::from_utf16_lossy(&utf16[..taken]);
        let text = text.trim_matches('\0').trim().to_string();
        assert_eq!(text, "D:\\ZipNest");
        assert!(matches!(
            classify(Some(&text), Path::new("D:\\ZipNest\\zipnest.exe")),
            InstallMode::Installed { .. }
        ));
    }

    #[test]
    fn an_install_directory_that_matches_is_installed() {
        let m = classify(
            Some(r"D:\ZipNest"),
            Path::new(r"D:\ZipNest\zipnest.exe"),
        );
        assert_eq!(
            m,
            InstallMode::Installed {
                install_dir: PathBuf::from(r"D:\ZipNest")
            }
        );
    }

    #[test]
    fn the_comparison_ignores_case_and_a_trailing_separator() {
        for recorded in [r"d:\zipnest", r"D:\ZipNest\", r"D:/ZipNest"] {
            assert!(
                matches!(
                    classify(Some(recorded), Path::new(r"D:\ZipNest\zipnest.exe")),
                    InstallMode::Installed { .. }
                ),
                "{recorded}"
            );
        }
    }

    #[test]
    fn a_dev_build_under_target_is_portable() {
        // This is what keeps `cargo run` from offering to install itself into
        // Program Files while the user keeps launching the dev exe.
        let m = classify(
            Some(r"D:\ZipNest"),
            Path::new(r"D:\claude\zipnest\target\release\zipnest-native.exe"),
        );
        assert_eq!(m, InstallMode::Portable);
    }

    #[test]
    fn no_registry_entry_means_portable() {
        assert_eq!(
            classify(None, Path::new(r"C:\anywhere\zipnest.exe")),
            InstallMode::Portable
        );
        assert_eq!(
            classify(Some("  "), Path::new(r"C:\anywhere\zipnest.exe")),
            InstallMode::Portable
        );
    }

    #[test]
    fn a_different_directory_on_the_same_drive_is_portable() {
        assert_eq!(
            classify(Some(r"D:\ZipNest"), Path::new(r"D:\Other\zipnest.exe")),
            InstallMode::Portable
        );
    }

    #[test]
    fn uac_refusal_is_cancelled_not_failed() {
        assert_eq!(
            LaunchOutcome::from_result(false, 1223),
            LaunchOutcome::Cancelled
        );
        assert_eq!(
            LaunchOutcome::from_result(false, 5),
            LaunchOutcome::Failed(5)
        );
        assert_eq!(
            LaunchOutcome::from_result(true, 0),
            LaunchOutcome::Started
        );
    }

    #[test]
    fn exit_codes_map_to_the_installer_s_definitions() {
        assert_eq!(InstallResult::from_exit_code(0), InstallResult::Ok);
        assert_eq!(
            InstallResult::from_exit_code(2),
            InstallResult::StillRunning
        );
        assert_eq!(InstallResult::from_exit_code(3), InstallResult::FilesBusy);
        assert_eq!(
            InstallResult::from_exit_code(4),
            InstallResult::ShellPackageOnly
        );
        assert_eq!(InstallResult::from_exit_code(1), InstallResult::Other(1));
    }

    #[test]
    fn a_shell_package_failure_still_counts_as_an_applied_update() {
        assert!(InstallResult::ShellPackageOnly.applied());
        assert!(InstallResult::Ok.applied());
        assert!(!InstallResult::FilesBusy.applied());
        assert!(!InstallResult::StillRunning.applied());
        assert!(!InstallResult::Other(1).applied());
    }

    #[test]
    fn every_non_ok_result_has_a_message_and_ok_has_none() {
        assert!(InstallResult::Ok.message_key().is_none());
        for r in [
            InstallResult::StillRunning,
            InstallResult::FilesBusy,
            InstallResult::ShellPackageOnly,
            InstallResult::Other(9),
        ] {
            assert!(r.message_key().is_some(), "{r:?}");
        }
    }

    #[test]
    fn silent_params_put_d_equals_last_and_unquoted() {
        let p = silent_params(Path::new(r"C:\Program Files\ZipNest"));
        assert!(p.starts_with("/S "));
        assert!(p.contains("/D=C:\\Program Files\\ZipNest"));
        assert!(!p.contains('"'), "quotes would break /D=: {p}");
        assert!(p.trim_end().ends_with("ZipNest"));
    }

    #[test]
    fn the_pending_record_round_trips() {
        // Pure: the base directory is passed in, never taken from the
        // environment. `set_var` is process-global and these tests run on
        // parallel threads.
        let dir = tempfile::tempdir().unwrap();
        let path = pending_path_in(dir.path());

        assert!(read_pending_from(&path).is_none(), "nothing written yet");

        let p = PendingInstall {
            version: "0.5.0".into(),
            exit_code: Some(3),
            launch_error: None,
            sha256: Some("ab".repeat(32)),
        };
        write_pending_to(&path, &p).unwrap();
        let back = read_pending_from(&path).unwrap();
        assert_eq!(back.version, "0.5.0");
        assert_eq!(back.exit_code, Some(3));
        assert_eq!(back.launch_error, None);
        assert_eq!(back.sha256.as_deref(), Some("ab".repeat(32).as_str()));

        // A second write replaces the first rather than appending.
        let p2 = PendingInstall {
            version: "0.5.1".into(),
            exit_code: None,
            launch_error: Some("cancelled".into()),
            sha256: None,
        };
        write_pending_to(&path, &p2).unwrap();
        let back = read_pending_from(&path).unwrap();
        assert_eq!(back.version, "0.5.1");
        assert_eq!(back.launch_error.as_deref(), Some("cancelled"));

        // No half-written temp file is left behind.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!std::path::Path::new(&tmp).exists());

        clear_pending_at(&path);
        assert!(read_pending_from(&path).is_none());

        let data = data_dir_in(dir.path());
        assert_eq!(data, dir.path().join("ZipNest"));
        assert_eq!(
            updates_dir_in(&data, "0.5.0"),
            dir.path().join("ZipNest").join("updates").join("0.5.0")
        );
    }
}
