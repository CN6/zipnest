//! ZipNest native UI (egui). Same-process calls into `zipnest-ipc`; no webview.
//! Windows GUI subsystem: no console window pops up during normal use.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod i18n;
mod single_instance;
mod theme;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use eframe::egui;
use std::sync::{Arc, Mutex};
use zipnest_ipc::{CreateRequest, EntryDto, IpcService, SettingsPatch, ShellOptions};

const DESIGN_WIDTH: f32 = 900.0;

/// Smallest window that still shows the toolbar without its controls colliding.
///
/// Below this the two right-hand clusters (打开/新建/开始解压 and 设置/EN) ran
/// out of room and the archive name was painted straight through them. The
/// list itself would survive a narrower window; the chrome around it does not.
const MIN_WIDTH: f32 = 720.0;
const MIN_HEIGHT: f32 = 420.0;

/// The same floor in the units the viewport asks for.
///
/// `MIN_WIDTH` is in *logical* points — the size the UI actually lays out in —
/// while `ViewportBuilder::with_min_inner_size` is in native points. At 200%
/// zoom those differ by 2×, so a fixed floor let the window shrink until the
/// zoomed toolbar no longer fitted and 设置 / 加密 were cut off again. Scaling
/// the floor by the zoom is what keeps the promise at every zoom level.
///
/// Capped to the desktop: at 200% zoom the scaled floor is 1440×840, which no
/// longer fits a 1366-wide laptop screen, and a floor larger than the screen
/// would make the window impossible to resize or even to see whole. On such a
/// screen the toolbar itself is the thing that has to give — see `toolbar`.
fn min_inner_size(zoom: f32) -> egui::Vec2 {
    let zoom = zoom.clamp(1.0, 2.0);
    let want = egui::vec2(MIN_WIDTH, MIN_HEIGHT) * zoom;
    let (_, _, vw, vh) = virtual_screen_bounds();
    let dpi = system_dpi_scale().max(0.5);
    egui::vec2(
        want.x.min(vw as f32 / dpi - 40.0).max(320.0),
        want.y.min(vh as f32 / dpi - 80.0).max(240.0),
    )
}

// Windows: CREATE_NO_WINDOW — spawn helpers (e.g. the updater installer)
// without any console flash.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Bounds of the whole virtual desktop `(x, y, width, height)`. Every monitor
/// counts, so a window parked on a second screen is still accepted, while one
/// that ends up past the right or bottom edge is not.
#[cfg(windows)]
fn virtual_screen_bounds() -> (i32, i32, i32, i32) {
    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
    }
    // SM_XVIRTUALSCREEN / SM_YVIRTUALSCREEN / SM_CXVIRTUALSCREEN /
    // SM_CYVIRTUALSCREEN.
    unsafe {
        (
            GetSystemMetrics(76),
            GetSystemMetrics(77),
            GetSystemMetrics(78),
            GetSystemMetrics(79),
        )
    }
}

#[cfg(not(windows))]
fn virtual_screen_bounds() -> (i32, i32, i32, i32) {
    (0, 0, 1920, 1080)
}

/// Format an archive timestamp (Unix ms, UTC) the way Explorer shows it:
/// local time, minute precision. Windows does the DST-aware conversion, so no
/// date library is needed.
fn format_mtime_local(ms_since_epoch: u64) -> Option<String> {
    #[cfg(windows)]
    {
        // FILETIME counts 100ns ticks from 1601-01-01.
        const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;

        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn FileTimeToLocalFileTime(ft: *const FileTime, out: *mut FileTime) -> i32;
            fn FileTimeToSystemTime(ft: *const FileTime, out: *mut SystemTime) -> i32;
        }

        let ticks = ms_since_epoch
            .checked_mul(10_000)?
            .checked_add(EPOCH_DIFF_100NS)?;
        let utc = FileTime {
            low: ticks as u32,
            high: (ticks >> 32) as u32,
        };
        let mut local = FileTime { low: 0, high: 0 };
        if unsafe { FileTimeToLocalFileTime(&utc, &mut local) } == 0 {
            return None;
        }
        let mut st = SystemTime::default();
        if unsafe { FileTimeToSystemTime(&local, &mut st) } == 0 {
            return None;
        }
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            st.year, st.month, st.day, st.hour, st.minute
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = ms_since_epoch;
        None
    }
}

// --- auto-update ---
// Single source of truth: the workspace version in the root Cargo.toml, which
// packaging/check-versions.ps1 compares against the installer, the MSIX manifest,
// the README and the CHANGELOG. It used to be a hand-typed literal here, and it
// sat at 0.4.0 for ten releases.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const UPDATE_API: &str = "https://api.github.com/repos/CN6/zipnest/releases/latest";
/// Derived from [`CURRENT_VERSION`] on purpose: a hand-typed string here sat at
/// `0.4.0` for ten releases (the release checklist asked for it every time and
/// nobody could tell). `packaging\check-versions.ps1` fails the build if a
/// literal version comes back.
static UPDATE_UA: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("ZipNest-Updater/{CURRENT_VERSION}"));
const RELEASES_PAGE: &str = "https://github.com/CN6/zipnest/releases/latest";

#[derive(Clone, Default)]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    Found {
        tag: String,
        url: String,
    },
    Failed(String),
}

fn parse_version(tag: &str) -> Vec<u32> {
    tag.trim_start_matches('v')
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect()
}

fn is_newer(tag: &str) -> bool {
    parse_version(CURRENT_VERSION) < parse_version(tag)
}

fn fetch_latest() -> Result<(String, String), String> {
    // ureq 3 takes the timeout on the agent, not on the request, so every call
    // builds one. Without a timeout the check waits for Windows' TCP timeouts —
    // on a machine whose network drops packets instead of refusing them, the
    // spinner in the status bar never stopped. Both bounds are short because
    // this is a background courtesy, never something to wait on.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let body = agent
        .get(UPDATE_API)
        .header("User-Agent", UPDATE_UA.as_str())
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
    let url = v["html_url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(RELEASES_PAGE)
        .to_string();
    Ok((tag, url))
}

/// Hand a URL (web page, or `mailto:`) to whatever Windows has registered for
/// it. `start` needs the empty "" title argument before a quoted URL.
fn open_shell_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Show a folder in Explorer.
fn open_path_in_explorer(path: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Hand a file to whatever Windows has registered for its type — the same
/// thing a double-click in Explorer does.
///
/// `explorer <path>` is not that: given a file it opens the *containing
/// folder* instead of the file, which is why a double-clicked entry looked
/// like it did nothing. `ShellExecuteW` is the shell's own "open" verb, so it
/// honours the user's file associations, and it takes a wide string, so a
/// path with `&` or spaces needs no quoting.
fn open_with_default_app(path: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
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
        /// `SW_SHOWNORMAL`.
        const SW_SHOWNORMAL: i32 = 1;
        let wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let op = wide("open");
        let file = wide(path);
        // Safe: two NUL-terminated buffers that outlive the call, and a null
        // parent window, which is what "no owner" means for the shell.
        unsafe {
            ShellExecuteW(
                0,
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Where feedback goes. Kept as two halves so the Settings dialog can show it
/// with the `@` broken up (`… @ …`): the joined address only ever exists at
/// runtime and inside the `mailto:` link, so a static scan of the exe does not
/// find a ready-made `user@domain` string to scrape.
///
/// Changing the channel is this one line: put the new mailbox in
/// [`FEEDBACK_EMAIL_USER`] / [`FEEDBACK_EMAIL_DOMAIN`]. It must be a mailbox the
/// maintainer actually reads — see the address checks in `feedback_tests`.
const FEEDBACK_EMAIL_USER: &str = "zipnest";
const FEEDBACK_EMAIL_DOMAIN: &str = "foxmail.com";

fn feedback_email() -> String {
    format!("{FEEDBACK_EMAIL_USER}@{FEEDBACK_EMAIL_DOMAIN}")
}

/// `Windows 10.0.26100 (x64)`. The build number is what identifies a Windows
/// version — the marketing name lies (Windows 11 reports 10.0) — and
/// `RtlGetVersion` is used because `GetVersionExW` has been deprecated and
/// reports a fixed 6.2 since Windows 8.1.
#[cfg(windows)]
fn windows_version_string() -> String {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }

    let arch = arch_label();
    let mut info = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform_id: 0,
        csd: [0; 128],
    };
    if unsafe { RtlGetVersion(&mut info) } == 0 {
        format!(
            "Windows {}.{}.{} ({arch})",
            info.major, info.minor, info.build
        )
    } else {
        format!("Windows (build unknown) ({arch})")
    }
}

#[cfg(not(windows))]
fn windows_version_string() -> String {
    format!("{} ({})", std::env::consts::OS, arch_label())
}

fn arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => other,
    }
}

/// Hard cap for the command-line modes (`--register-integration`,
/// `--unregister-shell`).
///
/// The uninstaller runs `--unregister-shell` through `nsExec::ExecToLog`, which
/// blocks until this process exits (and the installer did the same until the
/// stuck-install report). Anything that wedged the process would therefore hang
/// the installer with nothing on screen to explain it. The real work is a
/// handful of registry writes, so a deadline costs nothing. Detached, so a
/// normal exit never waits for it.
fn cap_cli_runtime() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(20));
        eprintln!("ZipNest: command-line mode did not finish within 20s, exiting");
        std::process::exit(0);
    });
}

/// Which integration to apply when somebody asks for it.
///
/// Split out from the registry work because it is the part that is easy to get
/// wrong, and it carries the whole "is a fresh install the default?" answer:
///
/// - never claimed before (`integration_applied == false`): claim everything,
///   whatever the two flags happen to hold. On a fresh profile they default to
///   true, and an old settings file's `false` was the *old* default rather than
///   a decision — honouring it is exactly how v0.4.7 machines stayed unclaimed.
/// - decision already recorded: apply that decision. `force` (the installer's
///   `--register-integration`) re-applies it, so a lost registration comes back
///   on reinstall; without `force` (a normal launch) nothing is touched, so a
///   deliberate opt-out is never undone.
fn integration_plan(settings: &zipnest_ipc::Settings, force: bool) -> Option<ShellOptions> {
    if !force && settings.integration_applied {
        return None;
    }
    Some(if settings.integration_applied {
        ShellOptions {
            associate: settings.associate,
            context_menu: settings.context_menu,
            // The user's per-extension ticks; the default is all nine.
            extensions: settings.assoc_extensions.clone(),
        }
    } else {
        // Never claimed before: everything we ship a handler for.
        ShellOptions {
            associate: true,
            context_menu: true,
            extensions: zipnest_ipc::shell::SUPPORTED_EXTENSIONS
                .iter()
                .map(|e| (*e).to_string())
                .collect(),
        }
    })
}

/// Claim the Explorer integration, then take the archive formats over as the
/// default handler. Called by `--register-integration` (the installer runs it
/// right after copying the files, `force = true`) and by the first UI launch
/// (`force = false`, which also covers the portable zip).
///
/// The takeover is what finally makes a fresh install the default extractor:
/// writing our `ProgId` into `HKCU\Software\Classes` is not enough once Windows
/// has recorded somebody else's handler for `.zip`, so
/// [`zipnest_ipc::user_choice::claim_all`] replaces that record with a valid one.
fn apply_integration(
    svc: &IpcService,
    force: bool,
) -> Option<zipnest_ipc::shell::ShellRegisterResult> {
    let settings = svc.settings_get().ok()?;
    let opts = integration_plan(&settings, force)?;
    let exe = std::env::current_exe().unwrap_or_default();
    let result = svc
        .shell_register(
            &exe,
            opts.clone(),
            &zipnest_ipc::shell::WindowsRegistry,
            &zipnest_ipc::shell::WindowsRegistry,
        )
        .ok()?;
    if opts.associate {
        claim_default_formats(&opts.extensions);
    }
    Some(result)
}

/// The extensions a [`ShellOptions`] actually claims: an empty list means
/// "everything we ship a handler for".
fn claimed_extensions(list: &[String]) -> Vec<String> {
    let normalised = zipnest_ipc::shell::normalise_extensions(list);
    if normalised.is_empty() {
        zipnest_ipc::shell::SUPPORTED_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_string())
            .collect()
    } else {
        normalised
    }
}

/// Make ZipNest the default handler for every claimed archive format.
///
/// Returns the extensions that could not be taken over. Kept separate from
/// [`apply_integration`] so the Settings button can run exactly the same work
/// and report it.
fn claim_default_formats(list: &[String]) -> Vec<String> {
    let exts = claimed_extensions(list);
    zipnest_ipc::user_choice::claim_all(exts.iter().map(String::as_str))
        .into_iter()
        .map(|(ext, reason)| {
            eprintln!("ZipNest: could not take over .{ext}: {reason}");
            ext
        })
        .collect()
}

/// Hand every claimed archive format back to whatever was the default before
/// ZipNest took it over.
///
/// This is the other half of [`claim_default_formats`]: claiming only rewrote
/// the `FileExts\<ext>\UserChoice` record, so uninstalling without releasing it
/// left Windows pointed at a ProgID whose registry tree (and exe) the uninstaller
/// was about to delete. Double-clicking a .zip afterwards did nothing at all.
///
/// Formats the user has since given to another program are left alone — see
/// [`zipnest_ipc::shell::release_defaults_for`]. Returns the extensions that
/// could not be released.
fn release_default_formats(list: &[String]) -> Vec<String> {
    let exts = claimed_extensions(list);
    zipnest_ipc::shell::release_defaults_for(exts.iter().map(String::as_str))
        .into_iter()
        .map(|(ext, reason)| {
            eprintln!("ZipNest: could not release .{ext}: {reason}");
            ext
        })
        .collect()
}

/// Look for a newer release.
///
/// `manual` marks the Settings/toolbar button. Only that path may show a state:
/// the automatic check on startup runs **silently** and touches nothing the
/// user can see unless a genuinely newer release exists. `Checking` used to be
/// set unconditionally, so every launch flashed an "正在检查更新" window even
/// when there was nothing to report, and a failed check (no network) popped an
/// error dialog nobody asked for. The background thread therefore leaves the
/// state alone while it works, and only writes `Found`; "no update" and "the
/// request failed" are both non-events on that path.
fn spawn_update_check(state: Arc<Mutex<UpdateState>>, manual: bool) {
    std::thread::spawn(move || {
        if manual {
            *state.lock().unwrap() = UpdateState::Checking;
        }
        match fetch_latest() {
            Ok((tag, url)) => {
                if is_newer(&tag) {
                    *state.lock().unwrap() = UpdateState::Found { tag, url };
                } else if manual {
                    *state.lock().unwrap() = UpdateState::Idle;
                }
            }
            Err(e) => {
                if manual {
                    *state.lock().unwrap() = UpdateState::Failed(e);
                }
            }
        }
    });
}

#[derive(Default, Clone)]
struct JobState {
    job_id: u64,
    done_items: u64,
    total_items: u64,
    done_bytes: u64,
    total_bytes: u64,
    speed_bps: u64,
    eta_secs: u64,
    ok: bool,
    finished: bool,
    error_key: Option<String>,
    /// Entries the last job left alone because the user's own conflict policy
    /// said so (an existing file under "skip").
    skipped: u64,
    /// Entries written under a rewritten name. All of them are on disk.
    renamed: u64,
    /// This extract asked the IPC layer to remove its source on success.
    delete_archive: bool,
    title: String,
    /// Where this job writes. Shown next to the result so the user does not
    /// have to guess the folder the app picked for them.
    dest: String,
}

impl JobState {
    /// `还剩 1 分 20 秒`, or empty while there is no honest estimate yet.
    ///
    /// The engine reports a rate; for the first moments of a job, and once it
    /// is all but done, the arithmetic divides by noise — so the estimate is
    /// suppressed at both ends rather than shown wrong.
    fn eta_text(&self, lang: &str) -> Option<String> {
        // Under 2% the rate is measured over a handful of milliseconds, so a
        // 300 MB archive can read "还剩 43 分钟" on work that takes ten
        // seconds. Suppress it until there is a real sample to divide by —
        // a wrong number is worse than none.
        if self.finished || self.eta_secs == 0 || self.total_bytes == 0 {
            return None;
        }
        if !(0.02..=0.995).contains(&self.pct()) {
            return None;
        }
        let secs = self.eta_secs;
        let text = if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{secs}s")
        };
        Some(if lang == "en-US" {
            format!("{text} left")
        } else {
            format!("还剩 {text}")
        })
    }

    fn pct(&self) -> f32 {
        if self.total_bytes > 0 {
            (self.done_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        } else if self.total_items > 0 {
            (self.done_items as f32 / self.total_items as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

fn load_app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("assets/icon.png")).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width: w,
        height: h,
    })
}

fn main() -> Result<(), eframe::Error> {
    // Launch intent from Explorer/CLI:
    //   zipnest.exe <archive>        → open that archive
    //   zipnest.exe --add <path>...  → open the create wizard pre-filled
    let args: Vec<String> = std::env::args().collect();
    // `--unregister-shell` is what the uninstaller runs before deleting files:
    // it drops every HKCU integration this app ever wrote (context menus,
    // ProgIDs and the extension defaults we pointed at them) without starting
    // the UI. A failure is reported through the exit code.
    if args.iter().skip(1).any(|a| a == "--unregister-shell") {
        cap_cli_runtime();
        // Release the recorded defaults *first*, while the ProgIDs they point
        // at still exist: `release_default` reads the current handler to decide
        // whether it is ours to give back. Doing it after `assoc_removals` on
        // the same pass would still work (the ops are only applied below) but
        // ordering it first keeps the intent obvious.
        let unreleased = release_default_formats(&[]);
        let mut ops = zipnest_ipc::shell::context_menu_removals();
        ops.extend(zipnest_ipc::shell::assoc_removals());
        let cleaned =
            match zipnest_ipc::shell::ShellApplier::run(&zipnest_ipc::shell::WindowsRegistry, &ops)
            {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("ZipNest: shell cleanup failed: {e}");
                    false
                }
            };
        // A leftover `FileExts` record the user does not own is exactly the
        // dangling-ProgID bug this fixes, so surface it as a failure too.
        return if cleaned && unreleased.is_empty() {
            Ok(())
        } else {
            std::process::exit(1);
        };
    }
    let mut launch_open: Option<String> = None;
    let mut launch_add: Vec<String> = Vec::new();
    // Set by the "解压到…" context-menu verb: open the archive and bring the
    // extract dialog up with it, so the destination can be picked immediately.
    let mut launch_extract = false;
    // `--register-integration` is what the installer runs after copying the
    // files: it claims the archive extensions and the context menus, so a
    // machine where the app is never opened still has them. No UI starts.
    if args.iter().skip(1).any(|a| a == "--register-integration") {
        cap_cli_runtime();
        let svc = IpcService::new(
            Arc::new(|_: &str, _: serde_json::Value| {}),
            std::time::Duration::ZERO,
        );
        let already_applied = svc
            .settings_get()
            .map(|s| s.integration_applied)
            .unwrap_or(false);
        let applied = apply_integration(&svc, true);
        return if already_applied || applied.is_some() {
            Ok(())
        } else {
            eprintln!("ZipNest: shell integration registration failed");
            std::process::exit(1);
        };
    }
    {
        let mut add_mode = false;
        for a in args.iter().skip(1) {
            if a == "--add" {
                add_mode = true;
                continue;
            }
            if a == "--extract" {
                launch_extract = true;
                continue;
            }
            if a.starts_with('-') {
                continue;
            }
            if add_mode {
                if std::path::Path::new(a).exists() {
                    launch_add.push(a.clone());
                }
            } else if launch_open.is_none() && std::path::Path::new(a).is_file() {
                launch_open = Some(a.clone());
            }
        }
    }
    // Single instance. Explorer starts a new process for every double-click and
    // every context-menu command, so without this a second right-click while a
    // window is open stacks a second window instead of reusing the first one.
    //
    // The running window is the reuse target — unless it is busy: while it is
    // extracting, a new request opens its own window instead of queueing behind
    // the job, so both can make progress. `--new-instance` opts out entirely.
    let force_new_instance = args.iter().any(|a| a == "--new-instance");
    let request = if let Some(path) = launch_open.clone() {
        Some(if launch_extract {
            single_instance::LaunchRequest::Extract(path)
        } else {
            single_instance::LaunchRequest::Open(path)
        })
    } else if launch_add.is_empty() {
        None
    } else {
        Some(single_instance::LaunchRequest::Add(launch_add.clone()))
    };
    let mut is_primary = false;
    if !force_new_instance {
        if single_instance::claim().is_none() {
            // Nothing to ask for (or the window is idle): hand it over and go.
            // A bare second launch only raises the window, busy or not.
            if request.is_none() || !single_instance::is_busy() {
                let _ = single_instance::enqueue(request.as_ref());
                single_instance::focus_existing();
                return Ok(());
            }
        } else {
            is_primary = true;
        }
    }
    // Read the settings once before the window exists: the saved geometry has
    // to be applied while the viewport is created, otherwise the window would
    // visibly jump on the first frame.
    let saved = zipnest_ipc::SettingsStore::new(zipnest_ipc::settings::default_path())
        .get()
        .clone();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([DESIGN_WIDTH, 620.0])
        // Below this the toolbar's own controls start fighting for the same
        // pixels and the archive name was drawn straight through them. The
        // list itself would survive a narrower window, the chrome does not:
        // 打开/新建/开始解压 plus 设置/EN/支持我们 is already ~600 points wide.
        // Scaled by the saved zoom — see `min_inner_size`.
        .with_min_inner_size(min_inner_size(saved.ui_zoom as f32 / 100.0))
        .with_title(single_instance::WINDOW_TITLE)
        // Created hidden and shown on the first frame. A window that is shown
        // before its size is applied sits as a 16x16 square in the corner for a
        // moment, which reads as a flash before the real window appears.
        .with_visible(false);
    // Stored geometry is in physical pixels. `ViewportBuilder` wants logical
    // points, so divide by the monitor DPI scale — the same factor the saving
    // side multiplies by. Note that egui's viewport rects are native points:
    // the UI zoom does not enter them, so using `pixels_per_point()` on either
    // side scaled the stored value by the zoom factor as well, and the window
    // grew by that factor on every start until it walked off the screen.
    let dpi = system_dpi_scale();
    // The floor this launch will enforce. A geometry saved under a smaller one
    // (an older build, a lower zoom) must be raised to it here: Windows does not
    // re-check `min_inner_size` against a window that is created at its restored
    // size, so the saved 400×400 came back 400×400 and the zoomed toolbar was
    // still cut off.
    let floor = min_inner_size(saved.ui_zoom as f32 / 100.0);
    if let (Some(w), Some(h)) = (saved.window_width, saved.window_height) {
        viewport = viewport.with_inner_size([
            physical_to_logical(w as f32, dpi).max(floor.x),
            physical_to_logical(h as f32, dpi).max(floor.y),
        ]);
    }
    if let (Some(x), Some(y)) = (saved.window_x, saved.window_y) {
        // Only trust a position that lands the whole window inside the virtual
        // desktop (every monitor): a window restored off-screen looks exactly
        // like "the app does not start". Both sides are physical pixels, so they
        // compare directly with the screen metrics.
        let (vx, vy, vw, vh) = virtual_screen_bounds();
        let w = saved.window_width.unwrap_or(0) as i32;
        let h = saved.window_height.unwrap_or(0) as i32;
        if x >= vx - 8 && y >= vy - 8 && x + w <= vx + vw && y + h <= vy + vh {
            viewport = viewport.with_position([
                physical_to_logical(x as f32, dpi),
                physical_to_logical(y as f32, dpi),
            ]);
        }
    }
    // Restore a window that was left maximized; without this the window came
    // back at its old normal size every launch.

    // Same icon embedded in the exe via build.rs: window and taskbar then match
    // the Explorer icon and shortcuts.
    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "ZipNest",
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                cc,
                launch_open,
                launch_add,
                launch_extract,
                is_primary,
            )))
        }),
    )
}

/// What to do once a password has been typed in.
///
/// Opening an archive and extracting from one take the same password, but they
/// are different operations: the first has to run `open_archive` again, the
/// second has to start the extract job that was refused. Keeping the intent in
/// the prompt means the user types the password once and the retry happens
/// automatically, instead of having to walk back through the dialog they came
/// from.
enum PasswordThen {
    /// Re-open the archive the prompt is for.
    Open,
    /// Re-run an extract that was refused for lack of a password.
    Extract(ExtractRequest),
}

/// One extract request, kept whole so it can be replayed.
///
/// A password refusal arrives either before the job starts or part-way into
/// it, and both need the same retry. Carrying the request itself — rather than
/// scattered fields — means the retry runs exactly what the user asked for.
#[derive(Clone)]
struct ExtractRequest {
    archive_id: u64,
    /// Empty for "the whole archive", the entry paths otherwise.
    sources: Vec<String>,
    dest: String,
    on_conflict: String,
    /// Delete the source only after the whole extraction succeeds.
    delete_archive: bool,
    /// Unpacking a double-clicked entry into a scratch folder, not a real
    /// extract: no dialog asked for a destination, so the result must not be
    /// reported as "extracted to <somewhere the user never chose>".
    silent: bool,
}

/// A password prompt that is on screen right now.
struct PasswordPrompt {
    /// The archive the password belongs to, shown so the user knows which of
    /// several open archives is asking.
    path: String,
    /// What has been typed so far.
    text: String,
    /// A password was already refused, so the field starts empty and says so.
    retry: bool,
    /// Put the caret in the field on the first frame; cleared once done so
    /// typing is not stolen back on every repaint.
    focus: bool,
    then: PasswordThen,
}

/// Which column the browser sorts by. There is no "unsorted": the engine's
/// order was the entry index, which looked arbitrary to a user.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Mtime,
}

/// Compare two names the way a file manager does: case-insensitively, and with
/// digit runs compared as numbers, so `file2` sorts before `file10` and
/// `Chapter 9` before `Chapter 10`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut a_chars = a.chars().peekable();
    let mut b_chars = b.chars().peekable();
    loop {
        match (a_chars.peek().copied(), b_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a_ch), Some(b_ch)) => {
                if a_ch.is_ascii_digit() && b_ch.is_ascii_digit() {
                    let mut a_run = String::new();
                    while let Some(c) = a_chars.peek().copied() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        a_run.push(c);
                        a_chars.next();
                    }
                    let mut b_run = String::new();
                    while let Some(c) = b_chars.peek().copied() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        b_run.push(c);
                        b_chars.next();
                    }
                    // Leading zeros are padding, not magnitude: compare the
                    // stripped values numerically (longer = larger, then the
                    // digits themselves), and fall back to the raw length so
                    // "07" and "7" still order deterministically.
                    let a_val = a_run.trim_start_matches('0');
                    let b_val = b_run.trim_start_matches('0');
                    let ord = a_val
                        .len()
                        .cmp(&b_val.len())
                        .then_with(|| a_val.cmp(b_val))
                        .then_with(|| a_run.len().cmp(&b_run.len()));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    a_chars.next();
                    b_chars.next();
                    let ord = a_ch.to_ascii_lowercase().cmp(&b_ch.to_ascii_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
        }
    }
}

/// Sort a directory listing in place. Directories always lead, as in Explorer
/// and 7-Zip; the chosen column only orders within each group.
fn sort_rows(rows: &mut [EntryDto], key: SortKey, ascending: bool) {
    rows.sort_by(|a, b| {
        let dirs_first = b.is_dir.cmp(&a.is_dir);
        if dirs_first != std::cmp::Ordering::Equal {
            return dirs_first;
        }
        let ord = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name).then_with(|| a.name.cmp(&b.name)),
            SortKey::Size => a.size.cmp(&b.size),
            // An entry the engine gave no timestamp for counts as the oldest,
            // so a directory full of them does not jump to the top.
            SortKey::Mtime => a.mtime_ms.unwrap_or(0).cmp(&b.mtime_ms.unwrap_or(0)),
        };
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

struct App {
    ctx: egui::Context,
    svc: Arc<IpcService>,
    settings: zipnest_ipc::Settings,
    launch_open: Option<String>,
    launch_add: Vec<String>,
    /// Our own command line carried `--extract` ("解压到…"), so the archive it
    /// names opens with the extract dialog up. Only the first request uses it.
    launch_extract: bool,
    /// Archive a later launch asked for while a job was running; opened once
    /// the job lets go of the archive. The flag is the `--extract` intent, so a
    /// "解压到…" click that had to wait for a job still ends in the dialog.
    pending_open: Option<(String, bool)>,
    /// This window is the one later launches hand their requests to.
    is_primary: bool,
    /// Last published busy-marker state; `None` forces one write on becoming
    /// primary so a marker left by a crash cannot make us look busy.
    published_busy: Option<bool>,
    /// The window is created hidden; the first frame is what shows it.
    show_on_first_frame: bool,
    /// The window exists for a shell request rather than a user session; it is
    /// closed once its job is done when `settings.auto_close_after_job` is on.
    /// When to close after a finished job (short delay so the result is read).
    close_at: Option<std::time::Instant>,
    /// Next attempt to take the primary role once the current holder is gone.
    primary_retry_at: std::time::Instant,
    /// Last window geometry written to settings (size + position) and when, so
    /// dragging the window does not rewrite the settings file every frame.
    saved_geometry: Option<((u32, u32), (i32, i32))>,
    geometry_saved_at: Option<std::time::Instant>,

    archive: Option<zipnest_ipc::OpenArchiveResult>,
    archive_path: String,
    /// The password the open archive was unlocked with, when the user had to
    /// supply one. The service also keeps it for preview; this copy is what
    /// the extract dialog hands over so an encrypted archive extracts without
    /// asking twice.
    archive_password: Option<String>,
    /// The password prompt on screen, if any.
    password_prompt: Option<PasswordPrompt>,
    /// The extract request the running or last job belongs to, so a password
    /// refused after the job started can be answered without losing what the
    /// user picked.
    last_extract: Option<ExtractRequest>,
    /// Set when a double-clicked entry is being unpacked so it can be handed to
    /// its own program: the entry path inside the archive. Consumed by the job
    /// result, which launches the file instead of drawing a progress window.
    open_after_extract: Option<String>,
    /// What the archive-wide filter box holds. Non-empty means `rows` is a
    /// search result, not the contents of `cwd`.
    search: String,
    cwd: String,
    rows: Vec<EntryDto>,
    selected: std::collections::HashSet<String>,
    /// Which column the list is sorted by and in which direction. Directories
    /// always lead; this only orders rows inside a group.
    sort_key: SortKey,
    sort_asc: bool,
    /// Row index the last plain or Ctrl click landed on, for Shift ranges. It
    /// indexes the *sorted* list, so sorting or navigating clears it.
    anchor: Option<usize>,
    error: Option<String>,
    notice: Option<String>,
    /// Backup path of a settings file that could not be parsed at startup, so
    /// the `settings.corrupt` notice can name it. `None` on a normal start.
    settings_backup: Option<String>,
    /// When the diagnostics report was last copied, so the Settings dialog can
    /// show a short "已复制" confirmation instead of a silent button.
    copied_at: Option<std::time::Instant>,
    lang: String,
    open_picker: bool,
    // jobs
    jobs: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    job: Option<JobState>,
    // dialogs
    show_extract: bool,
    extract_dest: String,
    extract_skipped: u64,
    /// `(name in the archive, name on disk)` for entries the last extraction
    /// had to rewrite so they could be written at all.
    renamed_entries: Vec<(String, String)>,
    /// Extra lines under an error message: which entries failed and why. Kept
    /// apart from `error` so a failure dialog can list them.
    error_detail: Vec<String>,
    overwrite: bool,
    /// "Also delete the source archive" — a checkbox in the extract dialog, not
    /// a third button beside the primary action: a destructive action sitting
    /// next to the one the user meant to click is one slip away from data loss.
    delete_source: bool,
    show_create: bool,
    create_sources: Vec<String>,
    create_dest: String,
    create_format: String,
    create_level: String,
    create_method: String,
    create_password: String,
    /// Second entry of the same password. Typing an encrypted archive's key
    /// wrong is not recoverable — the data is on disk and nobody holds it — so
    /// the wizard asks twice before it starts.
    create_password_confirm: String,
    /// Set when 开始创建 was pressed on a destination that already exists: the
    /// replacement is irreversible, so it takes a second, explicit click.
    create_confirm_overwrite: bool,
    create_encrypt_names: bool,
    create_volume: String,
    create_custom_volume: String,
    create_sfx: bool,
    /// Whether this opening of the wizard window has already asked for the
    /// front. Windows creates a new OS window behind the main one; without
    /// this it would appear underneath the app it belongs to.
    create_raised: bool,
    show_settings: bool,
    /// Per-extension state for the Settings status line (is Windows' recorded
    /// handler ours?), re-read whenever the page opens, after a save, and
    /// after a takeover.
    assoc_rows: Vec<zipnest_ipc::shell::AssocState>,
    show_donate: bool,
    /// A help hint the user clicked on: `(field label, explanation)`.
    help_popup: Option<(String, String)>,
    // assets
    qr_wechat: Option<egui::TextureHandle>,
    qr_alipay: Option<egui::TextureHandle>,
    /// Windows shell icons for the file list, keyed by [`shell_icon_key`]. A
    /// key the shell could not answer for is cached as `None` too, so the
    /// shell is asked at most once per key.
    icons: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// Insertion order of `icons`, oldest first, so eviction can free the
    /// oldest texture instead of letting the cache grow without bound.
    icon_order: std::collections::VecDeque<String>,
    /// Listings already fetched for a directory of the open archive, keyed by
    /// the normalized directory. Every navigation used to rescan the whole
    /// entry list; walking back and forth into the same folder should not.
    /// Cleared when a different archive opens.
    dir_cache: std::collections::HashMap<String, Vec<EntryDto>>,
    /// Local-time text for an entry's modification time, keyed by the raw
    /// timestamp. Producing it costs two Win32 calls plus a `format!`, and a
    /// visible row is redrawn every frame, so the same timestamp was converted
    /// ~60 times a second for as long as the folder stayed on screen.
    mtime_cache: std::collections::HashMap<u64, String>,
    // auto-update
    update: Arc<Mutex<UpdateState>>,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        launch_open: Option<String>,
        launch_add: Vec<String>,
        launch_extract: bool,
        is_primary: bool,
    ) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let mut app = Self::init(cc);
        app.launch_open = launch_open;
        app.launch_add = launch_add;
        app.launch_extract = launch_extract;
        app.is_primary = is_primary;
        app
    }

    fn init(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let jobs: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Default::default();
        let emit_jobs = Arc::clone(&jobs);
        let repaint = cc.egui_ctx.clone();
        let svc = Arc::new(IpcService::new(
            Arc::new(move |name: &str, payload: serde_json::Value| {
                emit_jobs.lock().unwrap().push((name.to_string(), payload));
                // Events arrive from job worker threads. Wake the UI so progress
                // actually advances on screen (otherwise egui stays idle until
                // the user moves the mouse — looks like "no progress bar").
                repaint.request_repaint();
            }),
            std::time::Duration::from_millis(100),
        ));
        let settings = svc.settings_get().unwrap_or_default();
        // First launch on a machine that has never recorded an integration
        // decision: claim the archive handlers and the context menus now. On an
        // installed copy the installer already ran this, so in practice this
        // path is the portable zip. A failure (a locked-down machine) must not
        // stop the app from starting.
        let boot = apply_integration(&svc, false);
        let mut settings = settings;
        if let Some(r) = &boot {
            settings.associate = r.associate;
            settings.context_menu = r.context_menu;
            settings.integration_applied = true;
        }
        let assoc_rows = zipnest_ipc::shell::assoc_states(&zipnest_ipc::shell::WindowsRegistry);
        // Theme: light, dark, or whatever the system is set to. Applied here
        // rather than next to install_fonts because it needs the settings.
        theme::apply_setting(&cc.egui_ctx, &settings.theme_mode);
        // Clean up a previously downloaded update installer that already ran
        // (it is only needed while the update is being applied).
        let _ = std::fs::remove_file(std::env::temp_dir().join("zipnest-update-setup.exe"));
        // Fixed zoom from settings, applied ONCE at startup. Never re-zoom per
        // frame — that feedback loop made fullscreen flicker badly.
        cc.egui_ctx
            .set_zoom_factor(settings.ui_zoom.clamp(100, 200) as f32 / 100.0);
        // One wheel notch should travel three rows, like Explorer. egui turns a
        // native `LineDelta` notch into `Options::line_scroll_speed` points (40
        // by default, about 1.5 rows here), which is why the list crawled.
        // Raising that option instead of rescaling
        // `InputState::smooth_scroll_delta` leaves the ScrollArea, scrollbar
        // dragging, keyboard navigation and selection untouched, and it only
        // affects real notches: touchpad / high-resolution `Point` deltas are
        // already proportional and stay as they are.
        cc.egui_ctx.options_mut(|o| {
            o.input_options.line_scroll_speed = scroll_points_per_notch(Self::ROW_H)
        });
        let lang = match settings.language.as_str() {
            "zh-CN" => "zh-CN",
            "en-US" => "en-US",
            _ => i18n::os_language(),
        }
        .to_string();
        let qr_wechat = i18n::load_image(include_bytes!("assets/donate/wechat.jpg")).map(|img| {
            cc.egui_ctx
                .load_texture("wechat", img, egui::TextureOptions::NEAREST)
        });
        let qr_alipay = i18n::load_image(include_bytes!("assets/donate/alipay.jpg")).map(|img| {
            cc.egui_ctx
                .load_texture("alipay", img, egui::TextureOptions::NEAREST)
        });
        let update: Arc<Mutex<UpdateState>> = Default::default();
        // Auto-check is user-controllable in Settings (on by default). It is
        // silent: no window, no spinner, no error dialog — only a real newer
        // release opens anything (see spawn_update_check).
        if settings.auto_check_update {
            let check = Arc::clone(&update);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(1));
                spawn_update_check(check, false);
            });
        }
        // Wake the UI when a later launch leaves a request in the queue: egui
        // sleeps while the window is idle, so nothing would read it otherwise.
        {
            let wake = cc.egui_ctx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if single_instance::queue_has_items() {
                    wake.request_repaint();
                }
            });
        }
        // A settings file that could not be parsed has already been moved
        // aside by the store; say so once, with the path the old file went to.
        // Without this the app simply started with every preference reset and
        // no explanation of why.
        let settings_backup = svc
            .corrupt_settings()
            .and_then(|c| c.backup)
            .map(|p| p.display().to_string());
        // first frame. maximized_saved starts at the stored value so the first
        Self {
            ctx,
            svc,
            settings,
            launch_open: None,
            launch_add: Vec::new(),
            launch_extract: false,
            pending_open: None,
            is_primary: false,
            published_busy: None,
            show_on_first_frame: true,
            close_at: None,
            primary_retry_at: std::time::Instant::now(),
            saved_geometry: None,
            geometry_saved_at: None,

            archive: None,
            archive_path: String::new(),
            open_after_extract: None,
            search: String::new(),
            cwd: String::new(),
            rows: Vec::new(),
            archive_password: None,
            password_prompt: None,
            last_extract: None,
            selected: Default::default(),
            sort_key: SortKey::Name,
            sort_asc: true,
            anchor: None,
            error: None,
            notice: settings_backup
                .as_ref()
                .map(|_| "settings.corrupt".to_string()),
            settings_backup,
            copied_at: None,
            lang,
            open_picker: false,
            jobs,
            job: None,
            show_extract: false,
            extract_dest: String::new(),
            extract_skipped: 0,
            renamed_entries: Vec::new(),
            error_detail: Vec::new(),
            overwrite: true,
            delete_source: false,
            show_create: false,
            create_sources: Vec::new(),
            create_dest: String::new(),
            create_format: "zip".into(),
            create_level: "normal".into(),
            create_method: "auto".into(),
            create_password: String::new(),
            create_password_confirm: String::new(),
            create_confirm_overwrite: false,
            create_encrypt_names: false,
            create_volume: "off".into(),
            create_custom_volume: String::new(),
            create_sfx: false,
            create_raised: false,
            show_settings: false,
            assoc_rows,
            show_donate: false,
            help_popup: None,
            qr_wechat,
            qr_alipay,
            icons: Default::default(),
            icon_order: Default::default(),
            dir_cache: Default::default(),
            mtime_cache: Default::default(),
            update,
        }
    }

    fn t(&self, key: &str) -> String {
        i18n::tr(&self.lang, key)
    }

    /// The text behind 设置 → 帮助与反馈. One source for the clipboard button and
    /// the mail body, so what a user sends is exactly what the button showed.
    ///
    /// Built by [`zipnest_ipc::diagnostics`], which is where the privacy rules
    /// live: version, OS build, the exe path with the account name masked, the
    /// integration flags, the engine state and the last error *key* — never a
    /// file name, an archive entry or anything else about the user's work.
    fn diagnostics_report(&self) -> String {
        zipnest_ipc::diagnostics::render(&zipnest_ipc::Diagnostics {
            version: CURRENT_VERSION.to_string(),
            os: windows_version_string(),
            exe: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            profile_dir: std::env::var("USERPROFILE").ok(),
            engine_ok: zipnest_ipc::engine_available(),
            settings: self.settings.clone(),
            last_error: self.error.clone().or_else(|| self.notice.clone()),
        })
    }

    /// Copy the report and open the user's mail client with it prefilled.
    ///
    /// The clipboard comes first on purpose: plenty of Windows installs have no
    /// mail client configured, in which case the `mailto:` does nothing at all —
    /// and the user still has the text ready to paste into webmail.
    fn send_feedback_mail(&mut self, ctx: &egui::Context) {
        let report = self.diagnostics_report();
        ctx.copy_text(report.clone());
        self.copied_at = Some(std::time::Instant::now());
        let subject = format!(
            "[ZipNest v{CURRENT_VERSION}] {}",
            self.t("settings.feedback.mail_subject")
        );
        open_shell_url(&zipnest_ipc::mailto_url(
            &feedback_email(),
            &subject,
            &report,
        ));
    }

    /// Status text for the last job. `job.ok_skipped` carries the count as
    /// `{count}`; native i18n has no interpolator, so it is substituted here.
    /// The one-line verdict for Settings -> Integration.
    ///
    /// Three states a user can be in, and each needs different words: we are not
    /// claiming anything (checkbox off), we are the default, or Windows has
    /// already given these extensions to another program and only the user can
    /// change that.
    /// What the takeover would have to fix: the formats Windows still records
    /// as somebody else's.
    fn assoc_status(&self) -> (&'static str, Option<String>) {
        if !self.settings.associate {
            return ("settings.shell.status_off", None);
        }
        let blocked: Vec<String> = self
            .assoc_rows
            .iter()
            .filter(|row| row.needs_user_choice())
            .map(|row| format!(".{}", row.ext))
            .collect();
        if blocked.is_empty() {
            return ("settings.shell.status_ours", None);
        }
        ("settings.shell.blocked", Some(blocked.join(" ")))
    }

    /// Re-read what Windows currently resolves, so the verdict above is never a
    /// stale guess (the user may have changed the default in Windows Settings
    /// while ZipNest was open).
    fn refresh_assoc_status(&mut self) {
        self.assoc_rows = zipnest_ipc::shell::assoc_states(&zipnest_ipc::shell::WindowsRegistry);
    }

    /// Take the archive formats over as the default handler, right now.
    ///
    /// This is the whole "one click to default" feature, and it needs no help
    /// from Windows Settings: the recorded handler lives in a `UserChoice` key
    /// that Windows denies *value writes* to but lets the owner delete, and the
    /// hash that protects it is derivable — see [`zipnest_ipc::user_choice`].
    /// So the work is quiet, instant, and cannot leave the user staring at a
    /// page wondering what to click.
    ///
    /// Windows still wins the long game: if the user later picks another
    /// archiver in "打开方式", that becomes the new default, and this button (or
    /// a reinstall) takes it back.
    fn force_default_now(&mut self) {
        let failed = claim_default_formats(&self.settings.assoc_extensions);
        self.refresh_assoc_status();
        self.notice = Some(if failed.is_empty() {
            "settings.shell.claim_ok".into()
        } else {
            "settings.shell.claim_partial".into()
        });
    }

    fn notice_text(&self) -> Option<String> {
        let key = self.notice.as_deref()?;
        let mut text = self.t(key);
        if key == "job.ok_skipped" {
            let n = self.job.as_ref().map(|j| j.skipped).unwrap_or(0);
            text = text.replace("{count}", &n.to_string());
        }
        if key == "job.ok_renamed" {
            let n = self.job.as_ref().map(|j| j.renamed).unwrap_or(0);
            text = text.replace("{count}", &n.to_string());
        }
        if key == "settings.corrupt" {
            let path = self.settings_backup.clone().unwrap_or_default();
            text = text.replace("{path}", &path);
        }
        Some(text)
    }

    /// Attach an explanation to a field's help mark.
    ///
    /// The mark itself is drawn by `theme::field`, beside the caption. Hovering
    /// shows the text at once; clicking pins it in a small popup, because a
    /// hover-only hint silently does nothing for anyone who (as most people do)
    /// clicks the mark instead of hovering it.
    fn help_hint(&mut self, ui: &egui::Ui, mark: Option<egui::Response>, label: &str, text: &str) {
        let Some(mark) = mark else { return };
        let mark = mark.on_hover_ui(|ui| {
            ui.set_max_width(380.0);
            ui.label(text);
        });
        if mark.clicked() {
            self.help_popup = Some((label.to_string(), text.to_string()));
        }
        let _ = ui;
    }

    /// The clicked help hint, shown until it is closed.
    fn help_window(&mut self, ctx: &egui::Context) {
        let Some((label, text)) = self.help_popup.clone() else {
            return;
        };
        let title = format!("{} · {}", self.t("help.title"), label);
        let close_label = self.t("job.close");
        let mut open = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(400.0);
                ui.label(text);
                ui.separator();
                if ui.button(close_label).clicked() {
                    self.help_popup = None;
                }
            });
        if !open {
            self.help_popup = None;
        }
    }

    /// Keep the output path sensible while the user has not chosen one: beside
    /// the first source, named after it. A path the user browsed for is never
    /// overwritten.
    fn refresh_default_dest(&mut self) {
        if self.create_dest.trim().is_empty() {
            if let Some(path) = default_archive_path(&self.create_sources, &self.create_format) {
                self.create_dest = path;
            }
        }
    }

    /// Drain job events emitted by the service into UI state.
    fn poll_jobs(&mut self) {
        let pending: Vec<_> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for (name, payload) in pending {
            match name.as_str() {
                "job_progress" => {
                    let j = self.job.get_or_insert_with(|| JobState {
                        job_id: payload["job_id"].as_u64().unwrap_or(0),
                        total_items: payload["total_items"].as_u64().unwrap_or(0),
                        total_bytes: payload["total_bytes"].as_u64().unwrap_or(0),
                        title: String::new(),
                        ..Default::default()
                    });
                    j.done_items = payload["done_items"].as_u64().unwrap_or(0);
                    j.total_items = payload["total_items"].as_u64().unwrap_or(0);
                    j.done_bytes = payload["done_bytes"].as_u64().unwrap_or(0);
                    j.total_bytes = payload["total_bytes"].as_u64().unwrap_or(0);
                    j.speed_bps = payload["speed_bps"].as_u64().unwrap_or(0);
                    j.eta_secs = payload["eta_secs"].as_u64().unwrap_or(0);
                }
                "extract_skipped" => {
                    self.extract_skipped = payload["count"].as_u64().unwrap_or(0);
                }
                "extract_renamed" => {
                    // The entries are on disk under a different name. Keep the
                    // pairs so the job window can show exactly what changed.
                    let pairs: Vec<(String, String)> = payload["entries"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|e| {
                                    (
                                        e["from"].as_str().unwrap_or_default().to_string(),
                                        e["to"].as_str().unwrap_or_default().to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let count = payload["count"].as_u64().unwrap_or(0);
                    if let Some(j) = self.job.as_mut() {
                        j.renamed = count;
                    }
                    self.renamed_entries = pairs;
                }
                "extract_failed" => {
                    // Files are missing. Build the lines the error dialog shows:
                    // the OS's own words (when it had any) plus one line per
                    // entry with the reason it could not be written.
                    let mut lines: Vec<String> = Vec::new();
                    if let Some(d) = payload["detail"].as_str() {
                        if !d.trim().is_empty() {
                            lines.push(d.to_string());
                        }
                    }
                    for e in payload["entries"].as_array().into_iter().flatten() {
                        let path = e["path"].as_str().unwrap_or_default();
                        let reason = e["reason"].as_str().unwrap_or("error.io");
                        let reason_text = self.t(reason);
                        let detail = e["detail"].as_str().unwrap_or_default();
                        // The OS's own words go on their own line: "(被占用 /
                        // 无权限)" followed by "(os error 183)" reads badly.
                        lines.push(match (path.is_empty(), detail.trim().is_empty()) {
                            (true, true) => reason_text,
                            (true, false) => format!("{reason_text}\n    {detail}"),
                            (false, true) => format!("{path}    {reason_text}"),
                            (false, false) => format!("{path}    {reason_text}\n    {detail}"),
                        });
                    }
                    self.error_detail = lines;
                }
                "job_finished" => {
                    let ok = payload["ok"].as_bool().unwrap_or(false);
                    let err_key = payload["error_key"].as_str().map(|s| s.to_string());
                    let skipped = self.extract_skipped;
                    {
                        let j = self.job.get_or_insert_with(JobState::default);
                        j.ok = ok;
                        j.finished = true;
                        j.error_key = err_key.clone();
                        j.skipped = skipped;
                    }
                    let cancelled = !ok && err_key.as_deref() == Some("error.cancelled");
                    let delete_archive =
                        self.job.as_ref().map(|j| j.delete_archive).unwrap_or(false);
                    let archive_released = (ok && delete_archive)
                        || (!ok && err_key.as_deref() == Some("error.archive_delete"));
                    // The tab closes on success, on cancel, and on any real
                    // failure (the box is drawn and then left behind).
                    let silent = self
                        .last_extract
                        .as_ref()
                        .map(|r| r.silent)
                        .unwrap_or(false);
                    if ok && silent {
                        // A double-clicked entry was unpacked only so its own
                        // program could open it. There is nothing to report:
                        // hand the file over and leave no window behind.
                        let name = self.open_after_extract.take();
                        let dest = self
                            .job
                            .as_ref()
                            .map(|j| j.dest.clone())
                            .unwrap_or_default();
                        self.job = None;
                        self.notice = None;
                        self.last_extract = None;
                        if let Some(name) = name {
                            // A name Windows could not store was rewritten on
                            // the way out; the pair list says what it became.
                            let name = self
                                .renamed_entries
                                .iter()
                                .find(|(from, _)| *from == name)
                                .map(|(_, to)| to.clone())
                                .unwrap_or(name);
                            let file = std::path::Path::new(&dest).join(name);
                            open_with_default_app(&file.to_string_lossy());
                        }
                        self.extract_skipped = 0;
                        if archive_released {
                            self.clear_archive_view();
                        }
                        continue;
                    }
                    self.open_after_extract = None;
                    if ok {
                        let renamed = self.job.as_ref().map(|j| j.renamed).unwrap_or(0);
                        self.notice = Some(
                            if delete_archive {
                                "job.ok_deleted"
                            } else if renamed > 0 {
                                "job.ok_renamed"
                            } else if skipped > 0 {
                                "job.ok_skipped"
                            } else {
                                "job.ok"
                            }
                            .into(),
                        );
                    } else if cancelled {
                        // The user pressed 取消. Reporting it through the error
                        // dialog told them the app had hit a problem when it had
                        // done exactly what they asked, and the progress window
                        // they had just dismissed came back with it.
                        self.notice = Some("error.cancelled".into());
                        self.job = None;
                    } else {
                        let key = err_key
                            .filter(|k| k.starts_with("error."))
                            .unwrap_or_else(|| "error.engine".into());
                        // A password refused mid-job is not a failure to report:
                        // it is the same question the pre-flight check asks, and
                        // the request is still around to replay it. This is the
                        // ZIP-with-AES case, where the names are readable but
                        // the contents are not.
                        match (is_password_key(&key), self.last_extract.clone()) {
                            (true, Some(req)) => {
                                self.password_prompt = Some(PasswordPrompt {
                                    path: self.archive_path.clone(),
                                    text: String::new(),
                                    retry: true,
                                    focus: true,
                                    then: PasswordThen::Extract(req),
                                });
                                // The retry replaces it; leaving the failed
                                // progress window up would talk over the prompt.
                                self.job = None;
                                self.notice = None;
                            }
                            _ => {
                                self.notice = Some(key.clone());
                                // Pop a dialog so a failure is never missed.
                                self.error = Some(self.t(&key));
                            }
                        }
                    }
                    // Done or cancelled: close only when the user asked for it.
                    // The window carries the "done" message (and the output
                    // path), so disappearing on its own hides the result.
                    if self.settings.auto_close_after_job && (ok || cancelled) {
                        self.close_at =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                    }
                    self.extract_skipped = 0;
                    if archive_released {
                        self.clear_archive_view();
                    }
                }
                _ => {}
            }
        }
    }

    /// A heavy job still owns the archive mutex, so opening another archive
    /// while one runs would block the UI thread until it finishes.
    fn job_running(&self) -> bool {
        self.job.as_ref().map(|j| !j.finished).unwrap_or(false)
    }

    /// The same file reaches us with mixed separators and case depending on who
    /// launched us (Explorer, a shortcut, the command line).
    fn same_archive(&self, path: &str) -> bool {
        fn norm(p: &str) -> String {
            p.replace('/', "\\").to_lowercase()
        }
        !self.archive_path.is_empty() && norm(&self.archive_path) == norm(path)
    }

    /// Act on a launch request — our own command line, or one a later launch
    /// forwarded through the single-instance queue.
    fn apply_launch(&mut self, request: single_instance::LaunchRequest) {
        match request {
            single_instance::LaunchRequest::Open(path) => {
                self.apply_open(path, false);
            }
            single_instance::LaunchRequest::Extract(path) => {
                self.apply_open(path, true);
            }
            single_instance::LaunchRequest::Add(paths) => {
                self.create_sources = paths;
                self.create_dest = String::new();
                self.show_create = true;
                self.refresh_default_dest();
            }
        }
    }

    /// Open an archive for a launch request, queueing it while a job runs.
    /// `then_extract` brings the destination dialog up afterwards — the
    /// "解压到…" context-menu verb, which has to survive the queue too.
    fn apply_open(&mut self, path: String, then_extract: bool) {
        if self.job_running() {
            // Remember it and open it once the job is done.
            self.pending_open = Some((path, then_extract));
            return;
        }
        // Already showing this archive: the caller only wanted the window
        // raised, so the selection and any open dialog stay.
        if !self.same_archive(&path) {
            self.open_archive(&path);
        }
        if then_extract && self.archive.is_some() {
            self.begin_extract_dialog();
        }
    }

    /// The IPC layer already released this source after a successful
    /// extract-and-delete (or a deletion that failed). Forget its UI state;
    /// the job result window stays visible.
    fn clear_archive_view(&mut self) {
        self.archive = None;
        self.archive_path.clear();
        self.archive_password = None;
        self.password_prompt = None;
        self.last_extract = None;
        self.cwd.clear();
        self.search.clear();
        self.rows.clear();
        self.selected.clear();
        self.anchor = None;
        self.dir_cache.clear();
        self.mtime_cache.clear();
        self.show_extract = false;
    }

    fn open_archive(&mut self, path: &str) {
        self.open_archive_with_password(path, None);
    }

    /// Open an archive, optionally with a password.
    ///
    /// An archive whose *names* are encrypted cannot even be listed without the
    /// password: the engine refuses during `Open`, which surfaces as
    /// `error.password_required`. That is not a failure to report — it is a
    /// question to ask — so it opens the password prompt with this path queued
    /// for a retry. Any other error is a real one and goes to the error dialog.
    fn open_archive_with_password(&mut self, path: &str, password: Option<String>) {
        // Release whatever we are replacing first: the engine, the source file
        // handle and the session password all live inside that archive, and
        // nothing else closes it (see `IpcService::close_archive`). The cached
        // listings belong to that archive too.
        if let Some(old) = self.archive.take() {
            self.svc.close_archive(old.id);
        }
        self.dir_cache.clear();
        // The previous archive's password dies with it; a successful open below
        // sets the one this archive was unlocked with.
        self.archive_password = None;
        match self.svc.open_archive(path.to_string(), password.clone()) {
            Ok(res) => {
                // The open already computed the root listing; reusing it here
                // saves a second full scan of every entry in the archive.
                let root = res.entries.clone();
                self.archive = Some(res);
                self.archive_path = path.to_string();
                self.archive_password = password;
                self.password_prompt = None;
                self.cwd = String::new();
                // The old archive's filter means nothing for this one.
                self.search.clear();
                self.dir_cache.insert(String::new(), root.clone());
                self.rows = root;
                self.selected.clear();
                self.resort();
                self.error = None;
            }
            Err(e) => {
                if is_password_key(&e.key) {
                    // A password was tried and refused, or none was given at
                    // all: either way the answer is the same prompt, with the
                    // "wrong password" line shown only in the first case.
                    self.password_prompt = Some(PasswordPrompt {
                        path: path.to_string(),
                        text: String::new(),
                        retry: password.is_some() || e.key == "error.password_incorrect",
                        focus: true,
                        then: PasswordThen::Open,
                    });
                    self.error = None;
                } else {
                    self.error = Some(self.t(&e.key));
                }
            }
        }
    }

    /// Apply the current sort to `rows`. Called wherever `rows` is replaced and
    /// when a column header is clicked; the Shift anchor indexes the sorted
    /// list, so it cannot survive either.
    fn resort(&mut self) {
        sort_rows(&mut self.rows, self.sort_key, self.sort_asc);
        self.anchor = None;
    }

    /// The one place that changes the browsed directory. Descending, the up and
    /// root buttons, a breadcrumb click and Backspace all funnel through here,
    /// so `cwd` and `rows` can never disagree about where we are.
    fn navigate(&mut self, dir: &str) {
        let dir = normalize_dir(dir);
        let Some(id) = self.archive.as_ref().map(|a| a.id) else {
            return;
        };
        // Walking into a folder from a search result means the user is done
        // searching: leave the box empty so the breadcrumb comes back and the
        // listing below is that folder, not a stale set of hits.
        self.search.clear();
        self.rows = match self.dir_cache.get(&dir) {
            Some(cached) => cached.clone(),
            None => {
                let rows = self.svc.list_children(id, dir.clone()).unwrap_or_default();
                // Bounded: clearing on overflow costs one re-scan later, and
                // keeps a deep walk through a huge archive from caching every
                // listing it passed through.
                if self.dir_cache.len() >= Self::DIR_CACHE_MAX {
                    self.dir_cache.clear();
                }
                self.dir_cache.insert(dir.clone(), rows.clone());
                rows
            }
        };
        self.cwd = dir;
        self.selected.clear();
        self.resort();
    }

    /// Local-time text for an entry's modification time, memoized by the
    /// timestamp. Every row is repainted each frame, and formatting one needs
    /// two Win32 calls, so a folder left on screen was re-converting the same
    /// timestamps sixty times a second.
    fn mtime_text(&mut self, mtime_ms: Option<u64>) -> &str {
        const DASH: &str = "-";
        let Some(ms) = mtime_ms else {
            return DASH;
        };
        if !self.mtime_cache.contains_key(&ms) {
            let text = format_mtime_local(ms).unwrap_or_else(|| DASH.to_string());
            // A directory can hold more distinct timestamps than are worth
            // remembering; start over rather than grow without bound.
            if self.mtime_cache.len() >= 8192 {
                self.mtime_cache.clear();
            }
            self.mtime_cache.insert(ms, text);
        }
        self.mtime_cache
            .get(&ms)
            .map(String::as_str)
            .unwrap_or(DASH)
    }

    /// Backspace and the "up" button. At the root there is nowhere to go, so
    /// this does nothing rather than clearing the current view.
    fn go_up(&mut self) {
        if let Some(up) = parent_dir(&self.cwd) {
            self.navigate(&up);
        }
    }

    /// Up / root / breadcrumb strip, drawn above the file list once we are
    /// inside the archive (at the root all three controls would be dead). The
    /// archive-wide search box shares the row, right-aligned, because it is
    /// available everywhere — including the root, where this row otherwise
    /// draws nothing.
    fn nav_row(&mut self, ui: &mut egui::Ui) {
        // Compact: the breadcrumb is a row of chips, not a second toolbar.
        // Scoped to this child row so the list's own spacing is untouched.
        let mut target: Option<String> = None;
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if !self.cwd.is_empty() && !self.searching() {
                if theme::ghost_button(ui, &self.t("browser.up"))
                    .on_hover_text(self.t("browser.up_hint"))
                    .clicked()
                {
                    target = parent_dir(&self.cwd);
                }
                if theme::ghost_button(ui, &self.t("browser.root")).clicked() {
                    target = Some(String::new());
                }
                ui.separator();
                if crumb_button(ui, &self.t("browser.root")).clicked() {
                    target = Some(String::new());
                }
                for (label, path) in breadcrumb_segments(&self.cwd) {
                    ui.label(egui::RichText::new("/").weak());
                    if crumb_button(ui, &label).clicked() {
                        target = Some(path);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.searching() && ui.small_button("×").clicked() {
                    clear = true;
                }
                let hint = self.t("browser.search.hint");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .desired_width(170.0)
                        .hint_text(hint),
                );
                if resp.changed() {
                    self.run_search();
                }
            });
        });
        if let Some(dir) = target {
            self.navigate(&dir);
        }
        if clear {
            self.search.clear();
            self.run_search();
        }
    }

    /// A search is showing instead of a directory listing.
    fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }

    /// Recompute the list for the current query. An empty query restores the
    /// directory the user was in.
    fn run_search(&mut self) {
        let Some(id) = self.archive.as_ref().map(|a| a.id) else {
            return;
        };
        if !self.searching() {
            let cwd = self.cwd.clone();
            self.navigate(&cwd);
            return;
        }
        // A search is a flat list from the whole archive, so it has no
        // directory to be "in": the breadcrumb goes away with it.
        self.rows = self
            .svc
            .search_entries(id, self.search.clone(), Self::SEARCH_MAX)
            .unwrap_or_default();
        self.selected.clear();
        self.resort();
    }

    fn current_archive_dir(&self) -> String {
        let p = self.archive_path.replace('\\', "/");
        match p.rfind('/') {
            Some(i) => p[..i].to_string(),
            None => String::new(),
        }
    }

    /// Default extract location: a subfolder named after the archive, so files
    /// don't scatter into the archive's own folder (matches WinRAR/7-Zip).
    /// `settings.default_extract_dir` replaces "next to the archive" as the
    /// parent folder when the user configured one.
    fn default_extract_dest(&self) -> String {
        let custom = self.settings.default_extract_dir.trim();
        let dir = if custom.is_empty() {
            self.current_archive_dir()
        } else {
            custom.trim_end_matches(['/', '\\']).to_string()
        };
        let stem = std::path::Path::new(&self.archive_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if stem.is_empty() {
            dir
        } else if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        theme::top_bar(ui, |ui| {
            ui.horizontal(|ui| {
                if theme::primary_button(ui, &self.t("app.open")).clicked() {
                    self.open_picker = true;
                }
                if theme::ghost_button(ui, &self.t("create.new")).clicked() {
                    self.show_create = true;
                    self.create_sources.clear();
                    // Empty, not the current archive's folder: the field holds
                    // a whole archive path and `refresh_default_dest` only
                    // fills it while it is blank. Seeding it with a bare
                    // directory left "开始创建" enabled (a directory is
                    // absolute) and the job failing for certain — and the
                    // field is read-only, so the user could not even repair it
                    // by typing a file name.
                    self.create_dest = String::new();
                }
                let extract_enabled = self.archive.is_some();
                if theme::primary_button_enabled(ui, &self.t("extract.start"), extract_enabled)
                    .on_hover_text(if self.selected.is_empty() {
                        self.t("extract.all_hint")
                    } else {
                        String::new()
                    })
                    .clicked()
                {
                    self.begin_extract_dialog();
                }
                if self.archive.is_some() {
                    // Space only: `ui.separator()` in a horizontal row draws a
                    // vertical hairline sized to the row's cursor, which here
                    // was a stub a few points tall hanging off the top edge.
                    ui.add_space(12.0);
                }
                // The archive name gets what the right-hand cluster leaves,
                // measured here rather than after that cluster is drawn: a
                // `Label::truncate()` only shortens itself against the space it
                // already knows about, so a name added before the right-hand
                // group was painted put "nested.zip" straight through 设置 / EN /
                // 支持我们 in a narrow window. `TOOLBAR_RIGHT_W` is that cluster's
                // own width (three ghost buttons plus their spacing).
                // Only a reserve for measuring the title, not a layout
                // constraint: the cluster itself is placed against the right
                // edge (see below), so this number being a few points generous
                // costs nothing but a slightly shorter name.
                const TOOLBAR_RIGHT_W: f32 = 210.0;
                // The archive title gets a child `ui` of the width the
                // right-hand cluster leaves. `Label::truncate()` shortens
                // itself against the space it is given, so giving it exactly
                // that space is what keeps a long name off 设置 / EN / 支持我们.
                // Measuring it against the whole row instead does not: the
                // label was placed before the cluster and painted through it.
                let left_w = (ui.available_width() - TOOLBAR_RIGHT_W).max(0.0);
                if let Some(a) = &self.archive {
                    let format = a.format.clone();
                    let path = self.archive_path.clone();
                    let name = std::path::Path::new(&self.archive_path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| self.archive_path.clone());
                    let name_w = left_w.min(280.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(name_w, 22.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // The archive *name* is what the user needs to
                            // see; the format alone ("zip") told them nothing.
                            ui.add(
                                egui::Label::new(egui::RichText::new(name).size(13.0).strong())
                                    .truncate(),
                            )
                            .on_hover_text(path);
                            ui.add(egui::Label::new(
                                egui::RichText::new(format)
                                    .size(11.0)
                                    .color(theme::secondary(ui)),
                            ));
                        },
                    );
                }
                // The right-hand cluster is laid out against the edge itself
                // instead of being pushed there by a spacer. The spacer used the
                // cluster's *reserved* width, and the reserve has to be larger
                // than the buttons really are (translations change their width),
                // so every millimetre the reserve overshot showed up as a strip
                // of empty toolbar to the right of 支持我们.
                let rest = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(rest, theme::CONTROL_H),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Added in reverse, because the layout runs right to
                        // left: the row still reads 设置 · EN · 支持我们.
                        if theme::ghost_button(ui, &self.t("settings.donate")).clicked() {
                            self.show_donate = true;
                        }
                        if theme::ghost_button(ui, if self.lang == "zh-CN" { "EN" } else { "中文" })
                            .clicked()
                        {
                            self.lang = if self.lang == "zh-CN" {
                                "en-US"
                            } else {
                                "zh-CN"
                            }
                            .into();
                            self.settings.language = self.lang.clone();
                            let _ = self.svc.settings_set(SettingsPatch {
                                language: Some(self.lang.clone()),
                                ..Default::default()
                            });
                        }
                        if theme::ghost_button(ui, &self.t("settings.title")).clicked() {
                            self.show_settings = true;
                            // The user may have changed the default in Windows
                            // Settings since we last looked; refresh instead of
                            // showing a stale verdict.
                            self.refresh_assoc_status();
                        }
                    },
                );
            });
        });
    }

    /// Column widths (points) shared by the table header and every row, so the
    /// two can never drift apart — the old header laid itself out right-to-left
    /// with ad-hoc paddings while rows used another mechanism, and the lock
    /// icon was painted at a fixed offset from the row start.
    const COL_SIZE: f32 = 92.0;
    const COL_MTIME: f32 = 150.0;
    const COL_ENC: f32 = 54.0;
    const ROW_H: f32 = 26.0;

    /// Shell icons resolved per frame (see `entry_icon`).
    const ICONS_PER_FRAME: usize = 4;
    /// Hard cap on cached icons. Textures are freed as their key is evicted,
    /// so browsing a huge archive cannot leak GPU memory.
    const ICON_CACHE_MAX: usize = 256;
    /// Directories remembered per open archive. Old enough to cover normal
    /// back-and-forth browsing, small enough that a deep walk through a huge
    /// archive cannot hold a copy of every listing it ever showed.
    const DIR_CACHE_MAX: usize = 64;
    /// Rows a search may return. A one-letter query in a huge archive matches
    /// thousands of entries; drawing all of them costs more than the filter is
    /// worth, and no one scrolls past the first screenful anyway.
    const SEARCH_MAX: usize = 1000;

    /// Drop the oldest cached icons. Runs at the start of the list, before any
    /// row is painted: egui frees a texture when its last handle goes, so
    /// evicting in the middle of the row loop could free one that a row has
    /// already queued for this frame. A key evicted here is simply reloaded if
    /// a later row asks for it again.
    fn trim_icons(&mut self) {
        while self.icon_order.len() > Self::ICON_CACHE_MAX {
            if let Some(oldest) = self.icon_order.pop_front() {
                self.icons.remove(&oldest);
            }
        }
    }

    /// The shell icon for one row, resolving at most `budget` new keys per
    /// frame. `None` means "not resolved yet, or the shell has no icon for it"
    /// — the caller paints the fallback badge then.
    fn entry_icon(
        &mut self,
        ctx: &egui::Context,
        e: &EntryDto,
        budget: &mut usize,
    ) -> Option<egui::TextureHandle> {
        let key = shell_icon_key(e);
        if let Some(cached) = self.icons.get(&key) {
            return cached.clone();
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let tex = load_shell_icon(ctx, &key);
        self.icons.insert(key.clone(), tex.clone());
        self.icon_order.push_back(key);
        tex
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            let full_w = ui.available_width();
            let size_x = (full_w - Self::COL_ENC - Self::COL_MTIME - Self::COL_SIZE).max(140.0);
            let mtime_x = size_x + Self::COL_SIZE;
            let enc_x = mtime_x + Self::COL_MTIME;
            let weak = theme::secondary(ui);
            let text_color = ui.visuals().text_color();
            let hairline = ui.visuals().widgets.inactive.bg_stroke.color;

            self.trim_icons();
            self.nav_row(ui);

            // ---- header ----
            // Each of the three sortable column titles is a button: clicking it
            // sorts by that column, clicking it again flips the direction, the
            // way Explorer and 7-Zip behave.
            let (head, _) = ui.allocate_exact_size(egui::vec2(full_w, 20.0), egui::Sense::hover());
            let mut sort_click: Option<SortKey> = None;
            let mut arrow_at: Option<egui::Pos2> = None;
            {
                // The name column owns everything left of the size column, which
                // is why its hit box starts at the card edge and not at its
                // label.
                let columns = [
                    (
                        egui::Rect::from_min_max(
                            head.left_top(),
                            egui::pos2(size_x, head.bottom()),
                        ),
                        0usize,
                        SortKey::Name,
                    ),
                    (
                        egui::Rect::from_min_max(
                            egui::pos2(size_x, head.top()),
                            egui::pos2(mtime_x, head.bottom()),
                        ),
                        1usize,
                        SortKey::Size,
                    ),
                    (
                        egui::Rect::from_min_max(
                            egui::pos2(mtime_x, head.top()),
                            egui::pos2(enc_x, head.bottom()),
                        ),
                        2usize,
                        SortKey::Mtime,
                    ),
                ];
                for (rect, slot, key) in columns {
                    let clicked = ui
                        .interact(
                            rect,
                            ui.scope_id().with(("sort-column", slot)),
                            egui::Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked();
                    if clicked {
                        sort_click = Some(key);
                    }
                    if key == self.sort_key {
                        // Park the triangle where it cannot touch the label: the
                        // far side of the name column, the left edge of the size
                        // column (its label is right-aligned), and the far side
                        // of the time column.
                        let x = match key {
                            SortKey::Name => size_x - 8.0,
                            SortKey::Size => size_x + 8.0,
                            SortKey::Mtime => enc_x - 8.0,
                        };
                        arrow_at = Some(egui::pos2(x, rect.center().y));
                    }
                }
                let p = ui.painter();
                let f = egui::FontId::proportional(12.0);
                p.text(
                    egui::pos2(head.left() + 30.0, head.center().y),
                    egui::Align2::LEFT_CENTER,
                    self.t("browser.columns.name"),
                    f.clone(),
                    weak,
                );
                p.text(
                    egui::pos2(mtime_x - 10.0, head.center().y),
                    egui::Align2::RIGHT_CENTER,
                    self.t("browser.columns.size"),
                    f.clone(),
                    weak,
                );
                p.text(
                    egui::pos2(mtime_x + 2.0, head.center().y),
                    egui::Align2::LEFT_CENTER,
                    self.t("browser.columns.mtime"),
                    f.clone(),
                    weak,
                );
                p.text(
                    egui::pos2(enc_x + Self::COL_ENC * 0.5, head.center().y),
                    egui::Align2::CENTER_CENTER,
                    self.t("browser.columns.encrypted"),
                    f,
                    weak,
                );
                if let Some(pos) = arrow_at {
                    // Up means ascending (A to Z, small to large), down is the
                    // reverse. An 8-point-wide, 6-point-tall triangle.
                    let base = if self.sort_asc { 3.0 } else { -3.0 };
                    p.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(pos.x - 4.0, pos.y + base),
                            egui::pos2(pos.x + 4.0, pos.y + base),
                            egui::pos2(pos.x, pos.y - base),
                        ],
                        text_color,
                        egui::Stroke::NONE,
                    ));
                }
            }
            if let Some(key) = sort_click {
                if self.sort_key == key {
                    self.sort_asc = !self.sort_asc;
                } else {
                    self.sort_key = key;
                    self.sort_asc = true;
                }
                self.resort();
            }
            let sep_y = ui.cursor().top();
            ui.painter().hline(
                ui.max_rect().x_range(),
                sep_y,
                egui::Stroke::new(1.0_f32, hairline),
            );
            ui.add_space(4.0);

            // ---- rows ----
            let mut click_dir: Option<String> = None;
            let mut dbl: Option<String> = None;
            // A double-clicked file: unpack it into a temp folder and let the
            // OS open it, instead of opening the extract dialog.
            let mut dbl_file: Option<String> = None;
            // A flat list of hits shows the folder beside each name; a
            // directory listing does not need to repeat where it is.
            let search_hit = self.searching();
            // Shell icons cost a Win32 round trip each; resolve a handful per
            // frame and let the painted badge stand in until a row's turn
            // comes, so a 10 000-entry directory cannot stall the UI thread.
            let mut icon_budget = Self::ICONS_PER_FRAME;
            // The loop needs `&mut self` (icons) *and* the list that
            // `self.rows` owns. Copying the whole list every frame was the old
            // way out: a 10 000-entry directory cloned ~1 MB of paths and
            // strings on every repaint. Move it out for the duration of the
            // loop instead and put it back before the actions below run, so
            // they still see a whole `self`.
            let rows = std::mem::take(&mut self.rows);
            // `show_rows` touches only the rows inside the viewport: the old
            // code allocated a rect and laid out four text runs for every
            // entry in the archive on every frame, which is what made a big
            // directory feel heavy.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                // egui keeps each scroll area's offset under its own id, so
                // salting the id with the folder gives every folder its own
                // remembered position: walking down and back up no longer
                // lands at the top of a listing the user had scrolled.
                .id_salt(("zipnest-rows", &self.cwd, self.searching()))
                .show_rows(ui, Self::ROW_H, rows.len(), |ui, range| {
                    for i in range {
                        let e = &rows[i];
                        let selected = self.selected.contains(&e.path);
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(full_w, Self::ROW_H),
                            egui::Sense::click(),
                        );
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                        // Selection beats hover beats the zebra stripe.
                        let visuals = ui.visuals();
                        let bg = if selected {
                            visuals.selection.bg_fill
                        } else if resp.hovered() {
                            visuals.widgets.hovered.weak_bg_fill
                        } else if i % 2 == 1 {
                            visuals.faint_bg_color
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let dark = visuals.dark_mode;
                        let painter = ui.painter().clone();
                        painter.rect_filled(rect, egui::CornerRadius::same(4), bg);

                        let icon_rect = egui::Rect::from_min_size(
                            rect.min + egui::vec2(6.0, 3.0),
                            egui::vec2(22.0, 20.0),
                        );
                        // The real Explorer-style icon when Windows gives us
                        // one; the painted badge is the fallback, never a blank
                        // square. Icons are RGBA and drawn untinted, so they
                        // read on both themes.
                        match self.entry_icon(ui.ctx(), e, &mut icon_budget) {
                            Some(tex) => {
                                painter.image(
                                    tex.id(),
                                    egui::Rect::from_center_size(
                                        icon_rect.center(),
                                        egui::Vec2::splat(icon_rect.height() - 2.0),
                                    ),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                            }
                            None => paint_entry_icon(&painter, icon_rect, e, dark),
                        }

                        // Name: one line, ellipsised inside its column. In a
                        // search result the name alone would not say where the
                        // file lives, so the folder follows it in weak text.
                        let name_rect = egui::Rect::from_min_max(
                            egui::pos2(icon_rect.right() + 8.0, rect.top()),
                            egui::pos2(size_x - 12.0, rect.bottom()),
                        );
                        let dir_of = search_hit
                            .then(|| {
                                std::path::Path::new(&e.path)
                                    .parent()
                                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                                    .unwrap_or_default()
                            })
                            .filter(|d| !d.is_empty());
                        match dir_of {
                            Some(dir) => {
                                let font = egui::FontId::proportional(13.5);
                                let name_w = ui
                                    .painter()
                                    .layout_no_wrap(e.name.clone(), font.clone(), text_color)
                                    .size()
                                    .x
                                    .min(name_rect.width());
                                paint_elided(
                                    ui,
                                    egui::Rect::from_min_max(
                                        name_rect.min,
                                        egui::pos2(
                                            name_rect.left() + name_w + 1.0,
                                            name_rect.bottom(),
                                        ),
                                    ),
                                    &e.name,
                                    font.clone(),
                                    text_color,
                                    egui::Align2::LEFT_CENTER,
                                );
                                paint_elided(
                                    ui,
                                    egui::Rect::from_min_max(
                                        egui::pos2(
                                            name_rect.left() + name_w + 8.0,
                                            name_rect.top(),
                                        ),
                                        name_rect.max,
                                    ),
                                    &format!("· {dir}"),
                                    egui::FontId::proportional(11.5),
                                    weak,
                                    egui::Align2::LEFT_CENTER,
                                );
                            }
                            None => paint_elided(
                                ui,
                                name_rect,
                                &e.name,
                                egui::FontId::proportional(13.5),
                                text_color,
                                egui::Align2::LEFT_CENTER,
                            ),
                        }

                        // Size: right-aligned against its column.
                        if !e.is_dir {
                            paint_elided(
                                ui,
                                egui::Rect::from_min_max(
                                    egui::pos2(size_x - 10.0, rect.top()),
                                    egui::pos2(mtime_x - 8.0, rect.bottom()),
                                ),
                                &pretty_size(e.size),
                                egui::FontId::proportional(12.5),
                                weak,
                                egui::Align2::RIGHT_CENTER,
                            );
                        }

                        // Modification time: the column existed but was never
                        // filled in before. Memoized — see `mtime_text`.
                        let mtime = self.mtime_text(e.mtime_ms);
                        paint_elided(
                            ui,
                            egui::Rect::from_min_max(
                                egui::pos2(mtime_x + 2.0, rect.top()),
                                egui::pos2(enc_x - 4.0, rect.bottom()),
                            ),
                            mtime,
                            egui::FontId::proportional(12.5),
                            weak,
                            egui::Align2::LEFT_CENTER,
                        );

                        if e.encrypted {
                            paint_lock(
                                &painter,
                                egui::pos2(enc_x + Self::COL_ENC * 0.5, rect.center().y),
                            );
                        }

                        if resp.double_clicked() && e.is_dir {
                            dbl = Some(e.path.clone());
                        } else if resp.double_clicked() {
                            dbl_file = Some(e.path.clone());
                        } else if resp.clicked() {
                            // Ctrl toggles one row, Shift extends from the last
                            // clicked one, a plain click replaces the selection.
                            // A plain click on a directory still descends into
                            // it, which is how this list has always behaved;
                            // Ctrl and Shift on a directory only select it.
                            let mods = ui.input(|i| i.modifiers);
                            if mods.shift {
                                let start = match self.anchor {
                                    Some(a) if a < rows.len() => a,
                                    _ => i,
                                };
                                let (lo, hi) = if start <= i { (start, i) } else { (i, start) };
                                if !mods.command {
                                    self.selected.clear();
                                }
                                for row in &rows[lo..=hi] {
                                    self.selected.insert(row.path.clone());
                                }
                                if self.anchor.is_none() {
                                    self.anchor = Some(i);
                                }
                            } else if mods.command {
                                if !self.selected.remove(&e.path) {
                                    self.selected.insert(e.path.clone());
                                }
                                self.anchor = Some(i);
                            } else if e.is_dir {
                                click_dir = Some(e.path.clone());
                            } else {
                                self.selected.clear();
                                self.selected.insert(e.path.clone());
                                self.anchor = Some(i);
                            }
                        }
                    }
                });
            // Put the list back before acting on a click: `navigate` replaces
            // `self.rows` itself, and selecting needs the field intact.
            self.rows = rows;
            if let Some(d) = dbl.or(click_dir) {
                self.navigate(&d);
            }
            if let Some(path) = dbl_file {
                self.open_entry(path);
            }
        });
    }

    /// Double-click on a file inside the archive: unpack just that entry into a
    /// scratch folder and hand the result to its own program.
    ///
    /// The alternative — the extract dialog — asks the user where to put a file
    /// they only wanted to look at. WinRAR and 7-Zip both do it this way.
    fn open_entry(&mut self, path: String) {
        let Some(id) = self.archive.as_ref().map(|a| a.id) else {
            return;
        };
        if self.job_running() {
            return;
        }
        // One folder per archive, so opening a second file from the same
        // archive reuses the folder instead of scattering a new one per click.
        let stem = std::path::Path::new(&self.archive_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = std::env::temp_dir()
            .join("ZipNest-open")
            .join(if stem.is_empty() {
                "archive".to_string()
            } else {
                stem
            });
        let dir = dir.to_string_lossy().into_owned();
        let req = ExtractRequest {
            archive_id: id,
            sources: vec![path.clone()],
            dest: dir,
            // Looking at a file twice must not stop on a "file exists"
            // question: the copy in a scratch folder is ours to replace.
            on_conflict: "overwrite".into(),
            delete_archive: false,
            // Not a real extract: no dialog asked for a destination, so the
            // result window would report a place the user never chose.
            silent: true,
        };
        // The archive's own path for the entry, not its file name: extraction
        // keeps the folder structure, so `docs/notes.txt` lands at
        // `<dest>/docs/notes.txt` and joining only the name finds nothing.
        self.open_after_extract = Some(path);
        match self.begin_extract(req.clone(), self.archive_password.clone()) {
            None => {}
            Some(key) if is_password_key(&key) => {
                // Encrypted archive, no password yet: ask, and replay this.
                self.open_after_extract = None;
                self.password_prompt = Some(PasswordPrompt {
                    path: self.archive_path.clone(),
                    text: String::new(),
                    retry: key == "error.password_incorrect",
                    focus: true,
                    then: PasswordThen::Extract(req),
                });
            }
            Some(key) => {
                self.open_after_extract = None;
                self.job = None;
                self.error = Some(self.t(&key));
            }
        }
    }

    /// Shown when nothing is open. The window has always accepted drops, but
    /// nothing told the user so — this is that signpost.
    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
        let border = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(
                (rect.width() - 90.0).max(240.0),
                (rect.height() - 90.0).max(180.0),
            ),
        );
        let accent = ui.visuals().hyperlink_color;
        let weak = theme::secondary(ui);
        let text = ui.visuals().text_color();
        {
            let painter = ui.painter().clone();
            dashed_rect(
                &painter,
                border,
                ui.visuals().widgets.inactive.bg_stroke.color,
            );
            painter.text(
                border.center() - egui::vec2(0.0, 30.0),
                egui::Align2::CENTER_CENTER,
                self.t("browser.drop_title"),
                egui::FontId::proportional(17.0),
                text,
            );
            painter.text(
                border.center() - egui::vec2(0.0, 2.0),
                egui::Align2::CENTER_CENTER,
                self.t("browser.drop_hint"),
                egui::FontId::proportional(13.0),
                weak,
            );
        }
        let button = egui::Rect::from_center_size(
            border.center() + egui::vec2(0.0, 46.0),
            egui::vec2(150.0, 34.0),
        );
        let clicked = ui
            .put(
                button,
                egui::Button::new(
                    egui::RichText::new(self.t("app.open")).color(egui::Color32::WHITE),
                )
                .fill(accent)
                .corner_radius(egui::CornerRadius::same(6))
                .stroke(egui::Stroke::NONE),
            )
            .clicked();
        if clicked {
            self.open_picker = true;
        }
    }

    fn statusbar(&mut self, ui: &mut egui::Ui) {
        theme::status_bar(ui, |ui| {
            ui.horizontal(|ui| {
                let status = self.notice_text().unwrap_or_else(|| {
                    if self.archive.is_none() {
                        return String::new();
                    }
                    // A bare number told the user nothing; say what it counts.
                    if self.selected.is_empty() {
                        self.t("browser.count")
                            .replace("{count}", &self.rows.len().to_string())
                    } else {
                        self.t("browser.count_selected")
                            .replace("{selected}", &self.selected.len().to_string())
                            .replace("{count}", &self.rows.len().to_string())
                    }
                });
                ui.add(egui::Label::new(
                    egui::RichText::new(status).color(theme::secondary(ui)),
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let upd = self.update.lock().unwrap().clone();
                    if matches!(upd, UpdateState::Idle) {
                        if theme::ghost_button(ui, &self.t("update.check")).clicked() {
                            spawn_update_check(Arc::clone(&self.update), true);
                        }
                    } else if matches!(upd, UpdateState::Checking) {
                        ui.add(egui::Spinner::new());
                    }
                    if theme::ghost_button(ui, &self.t("settings.donate")).clicked() {
                        self.show_donate = true;
                    }
                });
            });
        });
    }

    /// Open the extract dialog with its defaults filled in. Shared by the
    /// toolbar button and the "解压到…" context-menu verb so the two cannot
    /// drift: the destination and the overwrite default are decided here once.
    fn begin_extract_dialog(&mut self) {
        self.extract_dest = self.default_extract_dest();
        // The dialog checkbox mirrors the saved policy; "ask" starts from "do
        // not overwrite", the safe default.
        self.overwrite = self.settings.overwrite_policy == "overwrite";
        // Deleting the source is destructive, so it never starts ticked: the
        // answer to "which folder" must not also authorise a deletion the user
        // did not ask for.
        self.delete_source = false;
        // No selection → extract the whole archive (empty paths tells the
        // engine to unpack everything).
        self.show_extract = true;
    }

    fn extract_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_extract;
        let mut close = false;
        let arch_id = self.archive.as_ref().map(|a| a.id).unwrap_or(0);
        if self.archive.is_none() {
            self.show_extract = false;
            return;
        }
        let srcs = self.selected.clone();
        let t = self.t("extract.title");
        let center = ctx.content_rect().center() - egui::vec2(215.0, 110.0);
        egui::Window::new(t)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .default_pos(center)
            .open(&mut open)
            .show(ctx, |ui| {
                // Selection is now a scope, not just a highlight: say so before the
                // fields, because "Extract" with rows selected means "these rows".
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(if srcs.is_empty() {
                            self.t("extract.scope_all")
                        } else {
                            self.t("extract.scope_selected")
                                .replace("{n}", &srcs.len().to_string())
                        })
                        .size(12.5)
                        .color(theme::secondary(ui)),
                    )
                    .wrap(),
                );
                ui.add_space(12.0);
                // Caption above the field, field across the whole width: a
                // destination path is the one thing here the user has to read in
                // full, and a caption column beside it spent a third of the dialog
                // on empty space and still cut long paths off.
                let label = self.t("extract.dest");
                theme::caption(ui, &label);
                // Right-to-left: the button is placed first and the field takes
                // the rest, so a long path can never squeeze it out. Laid out
                // in a box exactly one control tall, because `with_layout`
                // otherwise claims every point the window has and the dialog
                // grows a hand's width of empty space below the form.
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), theme::CONTROL_H),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add_sized(
                                [72.0, theme::CONTROL_H],
                                egui::Button::new(self.t("extract.browse")),
                            )
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.extract_dest = dir.to_str().unwrap_or("").to_string();
                            }
                        }
                        let field_w = (ui.available_width() - 8.0).max(80.0);
                        ui.add_sized(
                            [field_w, theme::CONTROL_H],
                            egui::TextEdit::singleline(&mut self.extract_dest),
                        );
                    },
                );
                let can = std::path::Path::new(self.extract_dest.trim()).is_absolute();
                // A relative path is refused by the service too, but saying so here
                // is the only place the user can act on it: a bare folder name used
                // to extract into the process's own working directory and still
                // report 完成.
                if !self.extract_dest.trim().is_empty() && !can {
                    // Red, not the usual dim hint: extracting into the wrong folder
                    // is a mistake the user can still fix, not a footnote.
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(self.t("extract.dest_absolute"))
                                .size(11.5)
                                .color(error_color(ui)),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(12.0);
                // The saved conflict policy decides what this dialog offers: "ask"
                // shows the checkbox, the other three are shown as a fixed setting
                // so a saved policy is never silently ignored.
                let policy = self.settings.overwrite_policy.clone();
                let ask = policy == "ask";
                if ask {
                    let overwrite_label = self.t("extract.overwrite");
                    ui.checkbox(&mut self.overwrite, overwrite_label);
                } else {
                    // A saved policy is a fact, not a choice: say it on one line
                    // instead of spending a caption and a value line on it.
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(self.t(&format!("settings.overwrite.{policy}")))
                                .color(theme::secondary(ui)),
                        );
                        ui.label(self.t("extract.policy_saved"));
                    });
                }
                ui.add_space(theme::FIELD_GAP);
                // Deleting the source is only safe when everything in it is being
                // extracted; a partial selection would lose the rest. It is a
                // checkbox rather than a third button: a destructive action
                // sitting next to the one the user meant to press is one slip
                // away from losing the archive.
                let whole_archive = srcs.is_empty();
                if !whole_archive {
                    // A partially selected extract cannot delete the archive:
                    // the rest of it would go with it.
                    self.delete_source = false;
                }
                let delete_label = self.t("extract.delete");
                ui.add_enabled(
                    can && whole_archive,
                    egui::Checkbox::new(&mut self.delete_source, delete_label),
                )
                .on_hover_text(self.t("extract.delete_hint"));
                ui.add_space(24.0);
                // Windows' own order, left to right: the affirmative action
                // first, Cancel last. The group sits against the dialog's right
                // edge, which is where every Win11 dialog puts its actions.
                // No separator: 24 points of air already say "these are the
                // actions", and the old hairline also carried [取消] away on its
                // own line below the form.
                let clicked = ui
                    .allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), theme::CONTROL_H),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.button(self.t("extract.cancel")).clicked() {
                                close = true;
                            }
                            theme::primary_button_enabled(ui, &self.t("extract.start"), can)
                                .clicked()
                        },
                    )
                    .inner;
                // Enter confirms (that is what a dialog is expected to do) — but not
                // while the password prompt is up: that prompt owns the keyboard,
                // and a shared Enter would start the extract behind its back.
                let prompt_open = self.password_prompt.is_some();
                let enter = !prompt_open && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if clicked || (can && enter) {
                    let dest = self.extract_dest.clone();
                    let on_conflict = if ask {
                        if self.overwrite {
                            "overwrite"
                        } else {
                            "skip"
                        }
                    } else {
                        policy.as_str()
                    }
                    .to_string();
                    let req = ExtractRequest {
                        archive_id: arch_id,
                        sources: srcs.iter().cloned().collect(),
                        dest,
                        on_conflict,
                        delete_archive: self.delete_source,
                        silent: false,
                    };
                    // The password the archive was opened with, if any. The service
                    // also remembers it per archive, but passing it here keeps a
                    // re-opened archive (new id) working the same way.
                    match self.begin_extract(req.clone(), self.archive_password.clone()) {
                        None => {}
                        Some(key) if is_password_key(&key) => {
                            // Encrypted contents, no password yet: keep everything
                            // this dialog collected and ask for the password rather
                            // than making the user redo the dialog afterwards.
                            self.password_prompt = Some(PasswordPrompt {
                                path: self.archive_path.clone(),
                                text: String::new(),
                                retry: key == "error.password_incorrect",
                                focus: true,
                                then: PasswordThen::Extract(req),
                            });
                        }
                        Some(key) => self.error = Some(self.t(&key)),
                    }
                    self.show_extract = false;
                }
                if !prompt_open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        // Cancel used to set `show_extract` inside the closure, which the line
        // below then overwrote — the button did nothing at all.
        self.show_extract = open && !close && self.job.is_none();
    }

    /// Start an extract job and point the progress window at it.
    ///
    /// Returns the error key when the job was refused. A refusal for want of a
    /// password is left to the caller: that caller has the dialog's inputs and
    /// can reopen the password prompt with them intact, instead of showing an
    /// error the user can do nothing about.
    fn begin_extract(&mut self, req: ExtractRequest, password: Option<String>) -> Option<String> {
        self.last_extract = Some(req.clone());
        self.job = Some(JobState {
            job_id: 0,
            title: self.t("extract.title"),
            delete_archive: req.delete_archive,
            dest: req.dest.clone(),
            ..Default::default()
        });
        // Last job's rename/failure lists belong to the last job.
        self.renamed_entries.clear();
        self.error_detail.clear();
        match self.svc.extract_with_delete(
            req.archive_id,
            req.sources,
            req.dest,
            Some(req.on_conflict),
            password,
            req.delete_archive,
        ) {
            Ok(jid) => {
                if let Some(job) = self.job.as_mut() {
                    job.job_id = jid;
                }
                None
            }
            Err(e) => {
                self.job = None;
                Some(e.key)
            }
        }
    }

    /// The password prompt.
    ///
    /// Shown whenever the engine needs a password to open an archive or to
    /// extract from one. It remembers what the user was doing, so one password
    /// is enough: the open or the extract that triggered it runs again as soon
    /// as the field is submitted.
    fn password_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.password_prompt.take() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        let title = self.t("password.title");
        let center = ctx.content_rect().center() - egui::vec2(190.0, 60.0);
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .default_pos(center)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(egui::Label::new(self.t("password.prompt")).wrap());
                // Which archive is asking. Names are the only thing the user
                // can match against the window they dragged in.
                let name = prompt
                    .path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&prompt.path)
                    .to_string();
                ui.label(
                    egui::RichText::new(name)
                        .size(11.5)
                        .color(theme::secondary(ui)),
                );
                if prompt.retry {
                    ui.colored_label(ui.visuals().error_fg_color, self.t("password.wrong"));
                }
                let field = ui.add_sized(
                    [ui.available_width(), theme::CONTROL_H],
                    egui::TextEdit::singleline(&mut prompt.text)
                        .password(true)
                        .hint_text(self.t("password.hint")),
                );
                if prompt.focus {
                    field.request_focus();
                    prompt.focus = false;
                }
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can = !prompt.text.is_empty();
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if theme::primary_button_enabled(ui, &self.t("password.ok"), can).clicked() {
                        submit = true;
                    }
                    if ui.button(self.t("password.cancel")).clicked() {
                        cancel = true;
                    }
                });
                if entered && can {
                    submit = true;
                }
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if submit {
            let password = std::mem::take(&mut prompt.text);
            let then = std::mem::replace(&mut prompt.then, PasswordThen::Open);
            match then {
                PasswordThen::Open => {
                    // A refusal re-arms the prompt from inside here, so nothing
                    // is restored afterwards.
                    let path = prompt.path.clone();
                    self.open_archive_with_password(&path, Some(password));
                }
                PasswordThen::Extract(req) => {
                    self.show_extract = false;
                    match self.begin_extract(req.clone(), Some(password)) {
                        None => {}
                        // The job path reports a bad password as an event, but
                        // a refusal before the job starts comes back here.
                        Some(key) if is_password_key(&key) => {
                            self.password_prompt = Some(PasswordPrompt {
                                path: prompt.path.clone(),
                                text: String::new(),
                                retry: true,
                                focus: true,
                                then: PasswordThen::Extract(req),
                            });
                        }
                        Some(key) => self.error = Some(self.t(&key)),
                    }
                }
            }
            return;
        }
        // Cancelled, closed with the window button, or simply repainted: only
        // the first two drop the prompt.
        if cancel || !open {
            return;
        }
        self.password_prompt = Some(prompt);
    }

    /// The create wizard, as a window of its own.
    ///
    /// It used to be an `egui::Window` inside the main window, which made it a
    /// prisoner of that window's rect: in a small window the bottom rows and
    /// the start button were clipped by the edge, and dragging it outside was
    /// impossible. A real OS window can be moved anywhere, resized, and put on
    /// a second monitor — and the clipping problem stops existing.
    ///
    /// `show_viewport_immediate` falls back to an embedded window on a backend
    /// without multi-viewport support, so the wizard still opens there.
    fn create_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_create {
            return;
        }
        let title = self.t("create.title");
        let builder = egui::ViewportBuilder::default()
            .with_title(title.clone())
            // Fits the wizard at its natural height, plus room for the
            // password confirmation and the overwrite warning when they
            // appear. The window is resizable and scrolls, so a smaller or a
            // larger one both work; this is only where it starts.
            .with_inner_size([470.0, 520.0])
            // The chip rows are the floor: six formats on one line need ~400
            // logical points, and below that they wrap and the window turns
            // into a column of half-rows. Scrolling still covers a screen too
            // short for the whole form. Scaled by the UI zoom for the same
            // reason as the main window's floor.
            .with_min_inner_size(
                egui::vec2(430.0, 340.0) * (self.settings.ui_zoom.clamp(100, 200) as f32 / 100.0),
            )
            .with_resizable(true);
        let finished = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("zipnest-create-wizard"),
            builder,
            |ui, class| {
                // The callback hands us a `&mut Ui` since 0.34; the window
                // commands and the nested containers still take a `&Context`.
                let ctx = ui.ctx().clone();
                let embedded = class == egui::ViewportClass::EmbeddedWindow;
                // Windows creates the new window *behind* the main one, so the
                // first frame of each opening asks for the front. A fresh
                // window has never been seen, and appearing underneath the
                // app is the same occlusion this window exists to escape.
                if !self.create_raised && !embedded {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.create_raised = true;
                }
                // The window's own close button.
                if ctx.input(|i| i.viewport().close_requested()) {
                    return true;
                }
                if !embedded {
                    // eframe clears an immediate viewport with `[0,0,0,0]`
                    // rather than the app's clear colour, so anything not
                    // painted here would be transparent and keep whatever the
                    // compositor put behind it.
                    let rect = ui.max_rect();
                    let fill = ctx.global_style().visuals.panel_fill;
                    ctx.layer_painter(egui::LayerId::background())
                        .rect_filled(rect, 0.0, fill);
                }
                let body = |ui: &mut egui::Ui| self.create_wizard_ui(ui);
                if embedded {
                    egui::Window::new(title.clone())
                        .collapsible(false)
                        .resizable(true)
                        .vscroll(true)
                        .default_width(460.0)
                        .show(&ctx, body)
                        .and_then(|r| r.inner)
                        .unwrap_or(false)
                } else {
                    // A scroll area, not a bare panel: the wizard grows when
                    // the password confirmation or the overwrite warning
                    // appears, and on a short window the start button must stay
                    // reachable rather than falling off the bottom edge — the
                    // exact complaint that made this a separate window.
                    egui::CentralPanel::default()
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, body)
                                .inner
                        })
                        .inner
                }
            },
        );
        if finished {
            self.show_create = false;
            // The next opening is a brand-new window and must raise itself.
            self.create_raised = false;
            // An immediate viewport is destroyed on the *next* frame, and egui
            // only draws when something asks it to. Clicking 取消 therefore
            // left the window on screen until an unrelated event woke the
            // loop. Ask for the frame that closes it.
            ctx.request_repaint();
        }
    }

    /// The wizard's contents. Returns true when it is done — started, or
    /// cancelled — so the caller can close the window around it.
    fn create_wizard_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let svc = self.svc.clone();
        let mut finished = false;
        {
            ui.label(self.t("create.sources"));
            // Side by side, and the list scrolls on its own: a hundred-file
            // source list must not push the format and password rows out of
            // reach.
            ui.horizontal(|ui| {
                if ui.button(self.t("create.add_files")).clicked() {
                    let all = self.t("pick.all");
                    if let Some(files) = rfd::FileDialog::new().add_filter(all, &["*"]).pick_files()
                    {
                        for f in files {
                            if let Some(s) = f.to_str() {
                                if !self.create_sources.contains(&s.to_string()) {
                                    self.create_sources.push(s.to_string());
                                }
                            }
                        }
                    }
                    self.refresh_default_dest();
                }
                if ui.button(self.t("create.add_folder")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        if let Some(s) = dir.to_str() {
                            if !self.create_sources.contains(&s.to_string()) {
                                self.create_sources.push(s.to_string());
                            }
                        }
                    }
                    self.refresh_default_dest();
                }
            });
            if !self.create_sources.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("zipnest-create-sources")
                    .max_height(72.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for s in &self.create_sources.clone() {
                            ui.label(format!("• {s}"));
                        }
                    });
            }
            ui.add_space(12.0);
            let dest_label = self.t("create.dest");
            // Read-only on purpose. A bare name typed here used to be handed to
            // the engine as-is, so the archive was written relative to whatever
            // working directory the shell happened to pass us and the user could
            // not find it. The path is now always picked with 浏览…, or filled in
            // next to the sources before the dialog opens.
            let dest_hint = self.t("create.dest_hint");
            theme::caption(ui, &dest_label);
            // Right-to-left, in a box exactly one control tall: egui's
            // `with_layout` takes every point left in the ui, and inside a
            // field that meant the row grew to fill the window. The button is
            // placed first and the field takes the rest, so no translated
            // label can push the button off the edge ("Browse…" in English
            // used to).
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), theme::CONTROL_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    let browse_w = 72.0;
                    if ui
                        .add_sized(
                            [browse_w, theme::CONTROL_H],
                            egui::Button::new(self.t("extract.browse")),
                        )
                        .clicked()
                    {
                        // The Save-As dialog used to open with an empty file name and
                        // an "All files" filter, so 保存 did nothing until the user
                        // typed a name themselves — which reads as "browse cannot
                        // select anything". Pre-fill a sensible name, match the
                        // format.
                        let format = self.create_format.clone();
                        let (desc_key, exts) = format_filter(&format);
                        let desc = self.t(desc_key);
                        let picked = rfd::FileDialog::new()
                            .set_file_name(suggested_archive_name(&self.create_sources, &format))
                            .add_filter(desc, exts)
                            .save_file();
                        if let Some(p) = picked {
                            self.create_dest = p.to_str().unwrap_or("").to_string();
                        }
                    }
                    let field_w = (ui.available_width() - 8.0).max(80.0);
                    ui.add_sized(
                        [field_w, theme::CONTROL_H],
                        egui::TextEdit::singleline(&mut self.create_dest)
                            .interactive(false)
                            .hint_text(dest_hint),
                    );
                },
            );
            ui.add_space(theme::FIELD_GAP);
            // From here down every row is `theme::field`: caption, then the
            // control across the full width. `horizontal_wrapped` is what lets
            // the six format chips wrap onto a second line in a narrow window
            // instead of running off the right edge — tar.xz used to be cut in
            // half at the wizard's own minimum width.
            let label = self.t("create.format");
            let hint = self.t("help.format");
            let (_, mark) = theme::field(ui, &label, true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for f in ["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
                        let sel = self.create_format == f;
                        if theme::chip(ui, sel, f).clicked() {
                            self.create_format = f.to_string();
                            self.create_dest = retarget_archive_ext(&self.create_dest, f);
                            self.refresh_default_dest();
                        }
                    }
                });
            });
            self.help_hint(ui, mark, &label, &hint);
            let label = self.t("create.level");
            let hint = self.t("help.level");
            let (_, mark) = theme::field(ui, &label, true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for l in ["store", "fastest", "normal", "maximum", "ultra"] {
                        let sel = self.create_level == l;
                        let lbl = self.t(&format!("create.level.{l}"));
                        if theme::chip(ui, sel, &lbl).clicked() {
                            self.create_level = l.to_string();
                        }
                    }
                });
            });
            self.help_hint(ui, mark, &label, &hint);
            let label = self.t("create.method");
            let hint = self.t("help.method");
            let (_, mark) = theme::field(ui, &label, true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for m in ["auto", "copy", "deflate", "lzma2", "bzip2"] {
                        let sel = self.create_method == m;
                        let lbl = self.t(&format!("create.method.{m}"));
                        if theme::chip(ui, sel, &lbl).clicked() {
                            self.create_method = m.to_string();
                        }
                    }
                });
            });
            self.help_hint(ui, mark, &label, &hint);
            let label = self.t("create.password");
            let hint = self.t("help.password");
            let (_, mark) = theme::field(ui, &label, true, |ui| {
                // 220 rather than the full width: a password field wider than
                // the eye can follow reads as a text area, and the row would
                // stretch to the window edge for nothing.
                ui.add_sized(
                    [220.0, theme::CONTROL_H],
                    egui::TextEdit::singleline(&mut self.create_password).password(true),
                );
            });
            self.help_hint(ui, mark, &label, &hint);
            if !self.create_password.is_empty() {
                // A typo here produces an archive nobody can open, and the
                // mistake stays invisible until someone tries. The password
                // itself is never stored anywhere, so there is no way to
                // recover it after the fact.
                let confirm = self.t("create.password_confirm");
                theme::caption(ui, &confirm);
                ui.add_sized(
                    [220.0, theme::CONTROL_H],
                    egui::TextEdit::singleline(&mut self.create_password_confirm).password(true),
                );
                if self.create_password != self.create_password_confirm
                    && !self.create_password_confirm.is_empty()
                {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new(self.t("create.password_mismatch"))
                            .size(11.5)
                            .color(error_color(ui)),
                    ));
                }
                ui.add_space(theme::FIELD_GAP);
            }
            if self.create_format == "7z" {
                let lbl = self.t("create.encrypt_names");
                let hint = self.t("help.encrypt_names");
                let mark = theme::check_field(ui, &lbl, &mut self.create_encrypt_names, true);
                self.help_hint(ui, mark, &lbl, &hint);
            }
            let label = self.t("create.volume");
            let hint = self.t("help.volume");
            let (_, mark) = theme::field(ui, &label, true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for v in ["off", "10m", "100m", "1g", "custom"] {
                        let sel = self.create_volume == v;
                        let lbl = self.t(&format!("create.volume.{v}"));
                        if theme::chip(ui, sel, &lbl).clicked() {
                            self.create_volume = v.to_string();
                        }
                    }
                    if self.create_volume == "custom" {
                        ui.add_sized(
                            [80.0, theme::CONTROL_H],
                            egui::TextEdit::singleline(&mut self.create_custom_volume),
                        );
                    }
                });
            });
            self.help_hint(ui, mark, &label, &hint);
            if self.create_format == "7z" {
                let lbl = self.t("create.sfx");
                theme::check_field(ui, &lbl.clone(), &mut self.create_sfx, false);
            }
            ui.add_space(6.0);
            // The engine resolves a relative destination against the process
            // working directory, which for a shell-launched window is nowhere
            // the user would look, so only a full path may start a job.
            let dest = self.create_dest.trim().to_string();
            let dest_absolute = std::path::Path::new(&dest).is_absolute();
            let password_ok = self.create_password.is_empty()
                || self.create_password == self.create_password_confirm;
            let can = !self.create_sources.is_empty() && dest_absolute && password_ok;
            // Writing over an existing archive replaces it atomically and
            // irreversibly: `create.rs` renames the finished temp file onto the
            // target, so the old one is gone whatever the user meant. The
            // destination is pre-filled next to the sources, which is exactly
            // where an earlier archive of the same name usually sits, so this
            // is a realistic accident rather than a corner case.
            let replaces = dest_absolute && std::path::Path::new(&dest).is_file();
            // A destination that vanished between frames must not leave the
            // question standing.
            if !replaces {
                self.create_confirm_overwrite = false;
            }
            // Pressing 开始创建 onto an existing archive swaps the button for
            // the confirmation row rather than adding a row below it: this
            // wizard is already tall enough to run past the window edge, and a
            // confirmation whose own buttons fall outside the dialog is worse
            // than no confirmation at all.
            let mut proceed = false;
            let mut start_clicked = false;
            let mut cancel_clicked = false;
            if self.create_confirm_overwrite {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            self.t("create.overwrite_warning").replace("{path}", &dest),
                        )
                        .size(12.0)
                        .color(error_color(ui)),
                    )
                    .wrap(),
                );
                let (yes, no) = ui
                    .horizontal(|ui| {
                        (
                            ui.button(self.t("create.overwrite_confirm")).clicked(),
                            ui.button(self.t("create.overwrite_cancel")).clicked(),
                        )
                    })
                    .inner;
                if no {
                    // Only the question is dismissed. Closing the wizard would
                    // throw away the source list, and reopening it clears that
                    // list too — so the answer to "not here" must leave the
                    // work in place for the user to point somewhere else.
                    self.create_confirm_overwrite = false;
                }
                proceed = can && yes;
            } else {
                // Actions against the right edge, in Windows' order: the
                // affirmative one first, then Cancel. The reason the main button
                // is grey sits to their left, on the same line, so the user does
                // not have to look somewhere else to find out why.
                ui.horizontal(|ui| {
                    // Why the button is grey, said where it is grey rather than
                    // left for the user to work out.
                    if !can {
                        let why = if self.create_sources.is_empty() {
                            "create.need_sources"
                        } else if !dest_absolute {
                            "create.need_dest"
                        } else {
                            "create.need_password"
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(self.t(why))
                                    .size(11.5)
                                    .color(theme::secondary(ui)),
                            )
                            .truncate(),
                        );
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), theme::CONTROL_H),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            cancel_clicked = ui.button(self.t("extract.cancel")).clicked();
                            start_clicked =
                                theme::primary_button_enabled(ui, &self.t("create.start"), can)
                                    .clicked();
                        },
                    );
                });
                let enter = can && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if replaces && (start_clicked || enter) {
                    self.create_confirm_overwrite = true;
                } else {
                    proceed = start_clicked || enter;
                }
            }
            if proceed {
                self.create_confirm_overwrite = false;
                let volume_bytes = match self.create_volume.as_str() {
                    "off" => None,
                    "10m" => Some(10 * 1024 * 1024),
                    "100m" => Some(100 * 1024 * 1024),
                    "1g" => Some(1024 * 1024 * 1024),
                    "custom" => parse_volume(&self.create_custom_volume),
                    _ => None,
                };
                let req = CreateRequest {
                    format: self.create_format.clone(),
                    level: self.create_level.clone(),
                    method: self.create_method.clone(),
                    password: if self.create_password.is_empty() {
                        None
                    } else {
                        Some(self.create_password.clone())
                    },
                    encrypt_names: if self.create_encrypt_names {
                        Some(true)
                    } else {
                        None
                    },
                    volume_bytes,
                    sfx: if self.create_sfx {
                        Some("gui".into())
                    } else {
                        None
                    },
                };
                self.job = Some(JobState {
                    job_id: 0,
                    title: self.t("create.title"),
                    // The archive itself is the result, but the button next to
                    // it opens a folder, so remember the folder it lands in.
                    dest: std::path::Path::new(&self.create_dest)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    ..Default::default()
                });
                match svc.create_archive(self.create_sources.clone(), self.create_dest.clone(), req)
                {
                    Ok(jid) => {
                        if let Some(job) = self.job.as_mut() {
                            job.job_id = jid;
                        }
                    }
                    Err(e) => {
                        self.error = Some(self.t(&e.key));
                        self.job = None;
                    }
                }
                // The job owns the window from here: the progress window
                // takes over and the wizard has said everything it has to say.
                finished = true;
            }
            if cancel_clicked || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                finished = true;
            }
        }
        finished
    }

    /// The scrolling half of the Settings page.
    ///
    /// It draws nothing else: the header and the Save/Cancel row are separate
    /// panels of the same window, so this body may take every point left over
    /// without pushing the footer out of sight.
    fn settings_body(&mut self, ui: &mut egui::Ui) {
        // Padding only: the page owns the whole window now, so it needs the
        // gutters the card used to supply, but not its border or rounding.
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(24, 8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // The column follows the window, and when the window is
                        // wider than a readable measure it is *centred* rather
                        // than pinned left. Pinned left, every point past the cap
                        // piled up on the right as a dead strip (at 1500 wide
                        // that was a third of the page); centred, the same gap is
                        // split evenly and reads as a margin.
                        let avail = ui.available_width();
                        let col = avail.min(1064.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(avail, f32::INFINITY),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(col, f32::INFINITY),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                theme::section(ui, &self.t("settings.section.general"));
                                theme::group(ui, |ui| {
                                    let label = self.t("settings.language");
                                    theme::field(ui, &label, false, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            for (v, k) in [
                                                ("system", "settings.language.system"),
                                                ("zh-CN", "settings.language.zh"),
                                                ("en-US", "settings.language.en"),
                                            ] {
                                                let sel = self.settings.language == v;
                                                if theme::chip(ui, sel, &self.t(k)).clicked() {
                                                    self.settings.language = v.to_string();
                                                }
                                            }
                                        });
                                    });
                                    let label = self.t("settings.theme");
                                    theme::field(ui, &label, false, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            for (v, k) in [
                                                ("system", "settings.theme.system"),
                                                ("light", "settings.theme.light"),
                                                ("dark", "settings.theme.dark"),
                                            ] {
                                                let sel = self.settings.theme_mode == v;
                                                if theme::chip(ui, sel, &self.t(k)).clicked() {
                                                    self.settings.theme_mode = v.to_string();
                                                    // Apply at once so the choice is visible.
                                                    theme::apply_setting(&self.ctx.clone(), v);
                                                }
                                            }
                                        });
                                    });
                                    let label = self.t("settings.ui_zoom");
                                    theme::field(ui, &label, false, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            for z in [100u32, 125, 150, 175, 200] {
                                                let sel = self.settings.ui_zoom == z;
                                                let text = self
                                                    .t("settings.ui_zoom.percent")
                                                    .replace("{percent}", &z.to_string());
                                                if theme::chip(ui, sel, &text).clicked() {
                                                    self.settings.ui_zoom = z;
                                                    self.ctx.set_zoom_factor(z as f32 / 100.0);
                                                    // The floor is in native points, so a zoom
                                                    // change moves it: without this, raising the
                                                    // zoom left the old floor behind and the
                                                    // toolbar could be squeezed past its minimum.
                                                    self.ctx.send_viewport_cmd(
                                                        egui::ViewportCommand::MinInnerSize(
                                                            min_inner_size(z as f32 / 100.0),
                                                        ),
                                                    );
                                                }
                                            }
                                        });
                                    });
                                    // Whether a finished job takes its own window with it.
                                    // Applies to create and extract alike.
                                    let auto_close_label = self.t("settings.auto_close");
                                    ui.checkbox(
                                        &mut self.settings.auto_close_after_job,
                                        auto_close_label,
                                    );
                                    ui.add_space(4.0);
                                    theme::hint(ui, &self.t("settings.auto_close.hint"));
                                });

                                theme::section(ui, &self.t("settings.section.extract"));
                                theme::group(ui, |ui| {
                                    let label = self.t("settings.default_dir");
                                    theme::field(ui, &label, false, |ui| {
                                        // Right-to-left, same reason as the wizard's
                                        // destination row: the button is placed first
                                        // and the field takes what is left, so no
                                        // translated label can push the button off the
                                        // edge.
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let browse_w = 72.0;
                                                if ui
                                                    .add_sized(
                                                        [browse_w, theme::CONTROL_H],
                                                        egui::Button::new(self.t("extract.browse")),
                                                    )
                                                    .clicked()
                                                {
                                                    if let Some(dir) =
                                                        rfd::FileDialog::new().pick_folder()
                                                    {
                                                        self.settings.default_extract_dir =
                                                            dir.to_str().unwrap_or("").to_string();
                                                    }
                                                }
                                                let field_w =
                                                    (ui.available_width() - 8.0).max(80.0);
                                                ui.add_sized(
                                                    [field_w, theme::CONTROL_H],
                                                    egui::TextEdit::singleline(
                                                        &mut self.settings.default_extract_dir,
                                                    ),
                                                );
                                            },
                                        );
                                    });
                                    theme::hint(ui, &self.t("settings.default_dir.hint"));
                                    ui.add_space(theme::FIELD_GAP);
                                    // 带语境：明确这是"文件已存在时"的处理方式
                                    let label = self.t("settings.overwrite.hint");
                                    theme::field(ui, &label, false, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            for (v, k) in [
                                                ("ask", "settings.overwrite.ask"),
                                                ("overwrite", "settings.overwrite.overwrite"),
                                                ("skip", "settings.overwrite.skip"),
                                                ("rename", "settings.overwrite.rename"),
                                            ] {
                                                let sel = self.settings.overwrite_policy == v;
                                                if theme::chip(ui, sel, &self.t(k)).clicked() {
                                                    self.settings.overwrite_policy = v.to_string();
                                                }
                                            }
                                        });
                                    });
                                    let label = self.t("settings.max_extract");
                                    let hint = self.t("settings.max_extract.hint");
                                    let (_, mark) = theme::field(ui, &label, true, |ui| {
                                        let mut mb =
                                            self.settings.max_extract_bytes / (1024 * 1024);
                                        if ui
                                            .add(egui::DragValue::new(&mut mb).speed(256.0))
                                            .changed()
                                        {
                                            // Real clamping happens in Settings::apply on the way out.
                                            self.settings.max_extract_bytes =
                                                mb.max(1).saturating_mul(1024 * 1024);
                                        }
                                        ui.label("MB");
                                    });
                                    self.help_hint(ui, mark, &label, &hint);
                                });

                                theme::section(ui, &self.t("settings.section.integration"));
                                theme::group(ui, |ui| {
                                    // The label belongs *in* the checkbox: an empty label left
                                    // a stray tick floating above its own text.
                                    let associate_label = self.t("settings.shell.associate");
                                    ui.checkbox(&mut self.settings.associate, associate_label);
                                    ui.add_space(4.0);
                                    theme::hint(ui, &self.t("settings.shell.assoc_hint"));
                                    // The default-handler status and its button belong to
                                    // the association box above, so they sit directly under
                                    // it. They used to come after 右键菜单, which made the
                                    // line read as a claim about the context menu and sent
                                    // anyone whose default was still another app looking in
                                    // the wrong place.
                                    if self.settings.associate {
                                        // The button is the whole feature: it rewrites the
                                        // OS's own recorded handler for every format we
                                        // claim -- hash and all -- so nothing here sends the
                                        // user off to a Settings page they would have to
                                        // work out by themselves.
                                        ui.add_space(8.0);
                                        let (status_key, status_exts) = self.assoc_status();
                                        match status_exts {
                                            Some(exts) => {
                                                let msg =
                                                    self.t(status_key).replace("{exts}", &exts);
                                                theme::hint(ui, &msg);
                                            }
                                            None => {
                                                ui.label(self.t(status_key));
                                            }
                                        }
                                        ui.add_space(8.0);
                                        if theme::primary_button(
                                            ui,
                                            &self.t("settings.shell.set_default"),
                                        )
                                        .clicked()
                                        {
                                            self.force_default_now();
                                        }
                                        ui.add_space(6.0);
                                        theme::hint(ui, &self.t("settings.shell.set_default.hint"));
                                    }
                                    ui.add_space(theme::FIELD_GAP);
                                    let menu_label = self.t("settings.shell.context_menu");
                                    ui.checkbox(&mut self.settings.context_menu, menu_label);
                                });

                                theme::section(ui, &self.t("settings.section.about"));
                                theme::group(ui, |ui| {
                                    let update_label = self.t("settings.auto_update");
                                    ui.checkbox(&mut self.settings.auto_check_update, update_label);
                                    ui.add_space(theme::FIELD_GAP);
                                    let label = self.t("settings.version");
                                    theme::field(ui, &label, false, |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("v{CURRENT_VERSION}"))
                                                .strong(),
                                        );
                                    });
                                });

                                theme::section(ui, &self.t("settings.section.feedback"));
                                theme::group(ui, |ui| {
                                    theme::hint(ui, &self.t("settings.feedback.hint"));
                                    ui.add_space(theme::FIELD_GAP);
                                    ui.horizontal(|ui| {
                                        if ui.button(self.t("settings.feedback.copy")).clicked() {
                                            // Everything the maintainer asks for first, and
                                            // nothing about what the user was working on.
                                            let report = self.diagnostics_report();
                                            ui.ctx().copy_text(report);
                                            self.copied_at = Some(std::time::Instant::now());
                                        }
                                        if let Some(at) = self.copied_at {
                                            if at.elapsed() < std::time::Duration::from_secs(4) {
                                                ui.label(
                                                    egui::RichText::new(
                                                        self.t("settings.feedback.copied"),
                                                    )
                                                    .italics(),
                                                );
                                            }
                                        }
                                    });
                                    ui.add_space(theme::FIELD_GAP);
                                    let label = self.t("settings.feedback.email");
                                    theme::field(ui, &label, false, |ui| {
                                        // Shown with the `@` broken up: a bare address in a UI
                                        // string is a ready-made scrape target. Selectable so
                                        // the user can copy it, with the real one only inside
                                        // the mailto link.
                                        let shown = format!(
                                            "{FEEDBACK_EMAIL_USER} @ {FEEDBACK_EMAIL_DOMAIN}"
                                        );
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(shown).strong())
                                                .selectable(true),
                                        );
                                        if ui
                                            .button(self.t("settings.feedback.copy_email"))
                                            .clicked()
                                        {
                                            ui.ctx().copy_text(feedback_email());
                                            self.copied_at = Some(std::time::Instant::now());
                                        }
                                    });
                                    if ui.button(self.t("settings.feedback.mail")).clicked() {
                                        self.send_feedback_mail(ui.ctx());
                                    }
                                });
                                ui.add_space(16.0);
                            },
                        );
                            },
                        );
                    });
            });
    }

    /// The Settings page's header row: a Back button and the page title, drawn
    /// in the same seat the toolbar occupies on the archive page.
    ///
    /// Returns `true` when the user asked to leave.
    fn settings_header(&mut self, ui: &mut egui::Ui) -> bool {
        let mut back = false;
        theme::top_bar(ui, |ui| {
            ui.horizontal(|ui| {
                if theme::ghost_button(ui, &self.t("settings.back")).clicked() {
                    back = true;
                }
                ui.add(egui::Label::new(
                    egui::RichText::new(self.t("settings.title"))
                        .size(18.0)
                        .strong(),
                ));
            });
        });
        back
    }

    /// The Settings page's footer: Save and Cancel, plus whatever the last save
    /// had to report. Returns `(save, close)`.
    fn settings_footer(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let mut save = false;
        let mut close = false;
        theme::status_bar(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button(self.t("settings.save")).clicked() {
                    save = true;
                }
                if ui.button(self.t("extract.cancel")).clicked() {
                    close = true;
                }
                // Always right-aligned, even with nothing to say: that is what
                // makes the strip span the whole panel like the archive page's
                // status bar, instead of shrinking to a box around the buttons.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(text) = self.notice_text() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).color(theme::secondary(ui)))
                                .truncate(),
                        );
                    }
                });
            });
        });
        (save, close)
    }

    /// Write what the page edits back to the service and to Windows.
    ///
    /// Kept apart from the rendering so the Save button and the Enter key reach
    /// exactly the same code.
    fn save_settings(&mut self) {
        let svc = self.svc.clone();
        let assoc = self.settings.associate;
        let menu = self.settings.context_menu;
        let exe = std::env::current_exe().unwrap_or_default();
        match svc.shell_register(
            &exe,
            ShellOptions {
                associate: assoc,
                context_menu: menu,
                // The ticked extensions; unticked ones are released.
                extensions: self.settings.assoc_extensions.clone(),
            },
            &zipnest_ipc::shell::WindowsRegistry,
            &zipnest_ipc::shell::WindowsRegistry,
        ) {
            Ok(r) => {
                // Reflect what the OS actually accepted: a hardened
                // machine can deny one menu target and allow the
                // others, so the checkboxes must not claim more than
                // the registry holds.
                self.settings.associate = r.associate;
                self.settings.context_menu = r.context_menu;
                self.notice = r.warnings.first().cloned();
                // Registering `Classes` is only half of it: where Windows
                // has already recorded a handler for a format, the OS keeps
                // using that until the record itself is replaced.
                if assoc {
                    let failed = claim_default_formats(&self.settings.assoc_extensions);
                    if !failed.is_empty() && self.notice.is_none() {
                        self.notice = Some("settings.shell.claim_partial".into());
                    }
                } else {
                    // Associations switched off entirely: give every format we
                    // still own back, including the `FileExts` record, so Windows
                    // stops opening archives with a ProgID we just removed.
                    let _ = release_default_formats(&[]);
                }
                // The rows are a snapshot of the registry: re-read them so
                // the status line reflects this very save.
                self.refresh_assoc_status();
            }
            Err(e) => self.notice = Some(e.key),
        }
        let _ = svc.settings_set(SettingsPatch {
            language: Some(self.settings.language.clone()),
            default_extract_dir: Some(self.settings.default_extract_dir.clone()),
            overwrite_policy: Some(self.settings.overwrite_policy.clone()),
            preview_max_bytes: Some(self.settings.preview_max_bytes),
            max_extract_bytes: Some(self.settings.max_extract_bytes),
            theme_mode: Some(self.settings.theme_mode.clone()),
            ui_zoom: Some(self.settings.ui_zoom),
            auto_check_update: Some(self.settings.auto_check_update),
            auto_close_after_job: Some(self.settings.auto_close_after_job),
            ..Default::default()
        });
        if self.settings.language != "system" {
            self.lang = self.settings.language.clone();
        }
        self.show_settings = false;
    }

    /// Draw the Settings page, in the main window, instead of the archive view.
    ///
    /// It is a page rather than a dialog: the toolbar becomes the Back row, the
    /// archive list gives way to the settings body, and the status bar becomes
    /// the Save/Cancel footer. The application therefore keeps showing exactly
    /// one window, and the page gets the whole frame instead of being clipped by
    /// or floating above an archive list — which is what an embedded dialog did
    /// wrong, and why it briefly grew a window of its own before this.
    ///
    /// Returns `(save, close)`.
    fn settings_page(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let ctx = ui.ctx().clone();
        // The panel ids are the archive page's own ("toolbar" / "status"): only
        // one of the two branches draws per frame, so the page simply takes the
        // seats over instead of stacking extra bars underneath them.
        let back = egui::Panel::top("toolbar")
            .show(ui, |ui| {
                ui.add_space(4.0);
                let back = self.settings_header(ui);
                ui.add_space(4.0);
                back
            })
            .inner;
        let (save, close) = egui::Panel::bottom("status")
            .show(ui, |ui| self.settings_footer(ui))
            .inner;
        egui::CentralPanel::default().show(ui, |ui| {
            // No card: Settings fills the window edge to edge, like a page of
            // the app rather than a panel floating inside it. The rounded,
            // bordered surface it used to sit on read as "a second window
            // opened", which is the impression v0.4.14 set out to remove and
            // only half did.
            self.settings_body(ui);
        });
        // Escape leaves, Enter saves — what a settings surface is expected to do.
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        (save || enter, close || back || escape)
    }

    fn donate_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_donate;
        egui::Window::new(self.t("settings.donate.title"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t("settings.donate.body"));
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.qr_wechat {
                        ui.image((tex.id(), egui::Vec2::new(240.0, 240.0)));
                    }
                    if let Some(tex) = &self.qr_alipay {
                        ui.image((tex.id(), egui::Vec2::new(240.0, 240.0)));
                    }
                });
                if ui.button(self.t("extract.cancel")).clicked() {
                    self.show_donate = false;
                }
            });
        self.show_donate = open;
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        let st = self.update.lock().unwrap().clone();
        match st {
            UpdateState::Idle => {}
            UpdateState::Checking => {
                let title = self.t("update.title");
                let msg = self.t("update.checking");
                egui::Window::new(title).collapsible(false).show(ctx, |ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(msg);
                });
            }
            UpdateState::Found { tag, url } => {
                let title = self.t("update.title");
                let found = format!("{} ({})", self.t("update.found"), tag);
                let hint = self.t("update.hint");
                let dl = self.t("update.download");
                let cancel = self.t("extract.cancel");
                let upd = Arc::clone(&self.update);
                let mut open = true;
                egui::Window::new(title)
                    .collapsible(false)
                    .default_width(380.0)
                    .default_pos(ctx.content_rect().center() - egui::vec2(190.0, 60.0))
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(found);
                        ui.label(hint);
                        ui.horizontal(|ui| {
                            if ui.button(dl).clicked() {
                                open_shell_url(&url);
                            }
                            if ui.button(cancel).clicked() {
                                *upd.lock().unwrap() = UpdateState::Idle;
                            }
                        });
                    });
                if !open {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                }
            }
            UpdateState::Failed(msg) => {
                let title = self.t("update.title");
                let failed = format!("{}: {}", self.t("update.failed"), msg);
                let cancel = self.t("extract.cancel");
                let upd = Arc::clone(&self.update);
                let mut open = true;
                egui::Window::new(title)
                    .collapsible(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(failed);
                        if ui.button(cancel).clicked() {
                            *upd.lock().unwrap() = UpdateState::Idle;
                        }
                    });
                if !open {
                    *self.update.lock().unwrap() = UpdateState::Idle;
                }
            }
        }
    }

    fn error_window(&mut self, ctx: &egui::Context) {
        if let Some(err) = self.error.clone() {
            let mut open = true;
            egui::Window::new(self.t("error.title"))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .default_width(430.0)
                .show(ctx, |ui| {
                    ui.colored_label(error_color(ui), err);
                    // Which entries were not written, and why. A failure that
                    // only says "something failed" is not good enough when files
                    // are missing.
                    if !self.error_detail.is_empty() {
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("zipnest-error-detail")
                            .max_height(220.0)
                            .show(ui, |ui| {
                                for line in &self.error_detail {
                                    ui.label(line);
                                }
                            });
                    }
                    if ui.button(self.t("job.close")).clicked() {
                        self.error = None;
                        self.error_detail.clear();
                    }
                });
            if !open {
                self.error = None;
                self.error_detail.clear();
            }
        }
    }

    fn job_window(&mut self, ctx: &egui::Context) {
        let Some(j) = self.job.clone() else {
            return;
        };
        let mut cancel = false;
        let mut close = false;
        let center = ctx.content_rect().center() - egui::vec2(190.0, 70.0);
        egui::Window::new(j.title.clone())
            .id(egui::Id::unique("zipnest-job-window"))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .default_pos(center)
            .show(ctx, |ui| {
                let bar = if j.finished { 1.0 } else { j.pct() };
                ui.add_sized([360.0, 22.0], egui::ProgressBar::new(bar).show_percentage());
                if j.finished {
                    if j.ok {
                        ui.colored_label(
                            ok_color(ui),
                            self.notice_text().unwrap_or_else(|| self.t("job.ok")),
                        );
                        // Where the files went. The app picks the folder, so it
                        // is the one thing the user cannot work out themselves.
                        if !j.dest.is_empty() {
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(&j.dest)
                                        .size(11.5)
                                        .color(theme::secondary(ui)),
                                );
                            });
                            if ui.button(self.t("job.open_folder")).clicked() {
                                open_path_in_explorer(&j.dest);
                            }
                        }
                        // Entries whose archived name could not be stored as-is.
                        // They *are* on disk — under the name on the right — so
                        // this is information, not a warning.
                        if !self.renamed_entries.is_empty() {
                            ui.separator();
                            ui.label(self.t("job.renamed.header"));
                            egui::ScrollArea::vertical()
                                .id_salt("zipnest-renamed-list")
                                .max_height(140.0)
                                .show(ui, |ui| {
                                    for (from, to) in &self.renamed_entries {
                                        ui.label(format!("{from}  →  {to}"));
                                    }
                                });
                        }
                    } else {
                        let k = j.error_key.clone().unwrap_or_else(|| "error.engine".into());
                        ui.colored_label(error_color(ui), self.t(&k));
                    }
                    if ui.button(self.t("job.close")).clicked() {
                        close = true;
                    }
                } else {
                    let text = if j.total_bytes > 0 {
                        format!(
                            "{} / {}  ·  {}/s",
                            pretty_size(j.done_bytes),
                            pretty_size(j.total_bytes),
                            pretty_size(j.speed_bps)
                        )
                    } else {
                        format!("{} / {}", j.done_items, j.total_items)
                    };
                    ui.label(text);
                    if let Some(eta) = j.eta_text(&self.lang) {
                        ui.label(
                            egui::RichText::new(eta)
                                .size(12.0)
                                .color(theme::secondary(ui)),
                        );
                    }
                    if ui.button(self.t("job.canceled")).clicked() {
                        cancel = true;
                    }
                }
            });
        if cancel {
            if j.job_id != 0 {
                let _ = self.svc.cancel(j.job_id);
            }
            self.job = None;
        }
        if close {
            self.job = None;
            if self.settings.auto_close_after_job {
                self.close_at = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                // Do not make the user wait for the next scheduled frame: an
                // idle egui loop only wakes up on its own timer, which is what
                // made a click on 关闭 look like a two-second hang.
                ctx.request_repaint();
            }
        }
    }
}

fn pretty_size(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if b >= GB {
        format!("{:.1}G", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1}M", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.0}K", b as f64 / KB as f64)
    } else {
        format!("{b}B")
    }
}

/// Does this path name an archive ZipNest can open?
///
/// A numeric suffix counts: the create wizard can split its output into
/// numbered volumes, and the engine opens the set from any member. Recognising
/// only the format extension meant dragging in a `.7z.001` the app had just
/// produced offered to pack it into a new archive instead.
fn looks_like_archive(path: &str) -> bool {
    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(
        ext.as_str(),
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "iso"
    ) {
        return true;
    }
    // `.001`, `.0001`, … — 7-Zip's Split handler needs at least two digits.
    if ext.len() < 2 || !ext.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // `<base>.<format>.001`: the part before the number has to be a format we
    // know, otherwise `photo.001` (a raw photo sequence) would be swallowed.
    p.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .and_then(|s| s.rsplit('.').next().map(str::to_string))
        .is_some_and(|inner| matches!(inner.as_str(), "zip" | "7z" | "rar" | "tar"))
}

/// Color + short label used on the painted file icon, by extension.
fn entry_accent(ext: &str) -> (egui::Color32, Option<&'static str>) {
    match ext {
        "exe" | "msi" | "bat" | "cmd" => (egui::Color32::from_rgb(0, 103, 192), Some("EXE")),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" => {
            (egui::Color32::from_rgb(56, 142, 98), Some("IMG"))
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "mid" => {
            (egui::Color32::from_rgb(121, 85, 196), Some("AU"))
        }
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" => {
            (egui::Color32::from_rgb(198, 66, 110), Some("VI"))
        }
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "iso" | "tgz" => {
            (egui::Color32::from_rgb(236, 143, 60), Some("ZIP"))
        }
        "pdf" => (egui::Color32::from_rgb(210, 60, 60), Some("PDF")),
        "doc" | "docx" | "txt" | "md" => (egui::Color32::from_rgb(90, 110, 220), Some("TXT")),
        "xls" | "xlsx" | "csv" => (egui::Color32::from_rgb(46, 125, 90), Some("XL")),
        _ => (egui::Color32::from_rgb(121, 128, 140), None),
    }
}

/// rfd filter for a create format: `(i18n key of the description, extensions)`.
///
/// The description is shown verbatim in the Windows file dialog, so it is a
/// translation key the caller resolves -- it used to be English-only.
fn format_filter(format: &str) -> (&'static str, &'static [&'static str]) {
    match format {
        "7z" => ("format.7z", &["7z"]),
        "tar" => ("format.tar", &["tar"]),
        "tar.gz" => ("format.tar.gz", &["gz"]),
        "tar.bz2" => ("format.tar.bz2", &["bz2"]),
        "tar.xz" => ("format.tar.xz", &["xz"]),
        _ => ("format.zip", &["zip"]),
    }
}

/// The name to pre-fill in the Save-As dialog: the single source's own name, or
/// the folder holding several sources, plus the format's extension. The create
/// formats are also their extensions (`zip`, `tar.gz`, ...).
fn suggested_archive_name(sources: &[String], format: &str) -> String {
    let stem = match sources {
        [] => "archive".to_string(),
        [one] => std::path::Path::new(one)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string()),
        [first, ..] => std::path::Path::new(first)
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string()),
    };
    format!("{stem}.{format}")
}

/// Suggested output path: alongside the first source, named after it (or after
/// the folder holding several). `None` when there is nothing to derive the
/// location from, so the field stays empty and the hint asks for 浏览… instead.
fn default_archive_path(sources: &[String], format: &str) -> Option<String> {
    let first = sources.first()?;
    let dir = std::path::Path::new(first).parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(
        dir.join(suggested_archive_name(sources, format))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Swap a known archive extension in `dest` for the one `format` needs, so
/// switching the format after browsing does not leave `out.zip` behind while
/// writing 7z data. Paths without a known extension are left untouched.
fn retarget_archive_ext(dest: &str, format: &str) -> String {
    const KNOWN: [&str; 6] = ["tar.bz2", "tar.gz", "tar.xz", "zip", "7z", "tar"];
    let lower = dest.to_ascii_lowercase();
    for ext in KNOWN {
        let suffix = format!(".{ext}");
        if lower.ends_with(&suffix) {
            return format!("{}.{}", &dest[..dest.len() - suffix.len()], format);
        }
    }
    dest.to_string()
}

fn parse_volume(input: &str) -> Option<u64> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = if let Some(rest) = s.strip_suffix('k') {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix('g') {
        (rest, 1024 * 1024 * 1024)
    } else {
        (s.as_str(), 1u64)
    };
    num.parse::<f64>()
        .ok()
        .map(|n| (n * mult as f64) as u64)
        .filter(|n| *n > 0)
}

impl eframe::App for App {
    // 0.34 split the old `update` into `logic` (no ui, may run while hidden)
    // and `ui`. Everything this app does before drawing — polling jobs, taking
    // launch requests, syncing the window geometry — belongs in `logic` so it
    // keeps running while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_jobs();

        if self.show_on_first_frame {
            self.show_on_first_frame = false;
            // The size is applied by now, so the window can appear at its final
            // geometry instead of flashing a small default first.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }

        // Remember the window geometry so the next launch opens where the user
        // left it. Debounced: dragging fires this every frame, and a maximised
        // window keeps the last normal size instead of overwriting it.
        let viewport = ctx.input(|i| i.viewport().clone());
        // Deliberately NOT remembered: restoring a maximized window makes it
        // appear small for a moment before it is maximized, which reads as a
        // flash. So this keeps the last *normal* size, and every launch opens as
        // a normal window the user can maximize themselves.
        if viewport.maximized != Some(true) {
            if let Some(rect) = viewport.inner_rect {
                // Native DPI scale, *not* `ctx.pixels_per_point()`: the viewport
                // rects are native points, so the zoom must not be applied here
                // or the value written back is the window size times the zoom.
                let dpi = system_dpi_scale();
                let (size, fallback_pos) = physical_geometry(rect, rect.min, dpi);
                let pos = viewport
                    .outer_rect
                    .map(|r| physical_geometry(rect, r.min, dpi).1)
                    .unwrap_or(fallback_pos);
                {
                    let due = self
                        .geometry_saved_at
                        .map(|t| t.elapsed().as_secs() >= 2)
                        .unwrap_or(true);
                    if self.saved_geometry != Some((size, pos)) && due {
                        self.saved_geometry = Some((size, pos));
                        self.geometry_saved_at = Some(std::time::Instant::now());
                        let _ = self.svc.settings_set(SettingsPatch {
                            window_width: Some(size.0),
                            window_height: Some(size.1),
                            window_x: Some(pos.0),
                            window_y: Some(pos.1),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        // Take the primary role if it is free: the window that holds it is the
        // one later launches reuse, so an existing window should pick it up
        // after the previous holder closed instead of forcing a new window.
        if !self.is_primary && std::time::Instant::now() >= self.primary_retry_at {
            self.primary_retry_at = std::time::Instant::now() + std::time::Duration::from_secs(1);
            if single_instance::claim().is_some() {
                self.is_primary = true;
            }
        }
        // Publish "busy" on edges only: later launches read the marker to pick
        // between reusing this window and opening their own.
        if self.is_primary {
            let busy_now = self.job_running();
            if self.published_busy != Some(busy_now) {
                single_instance::set_busy(busy_now);
                self.published_busy = Some(busy_now);
            }
        }
        // A window opened for a shell request leaves once its job is over, after
        // keeping the result on screen long enough to read.
        if let Some(at) = self.close_at {
            if std::time::Instant::now() >= at {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        // Consume launch intents: our own command line plus anything a later
        // launch forwarded through the single-instance queue (only the primary
        // window owns that queue).
        if let Some(path) = self.launch_open.take() {
            // Our own command line: `--extract` is the "解压到…" verb, and it
            // has to open the dialog too — not just the archive.
            let then_extract = std::mem::take(&mut self.launch_extract);
            self.apply_launch(if then_extract {
                single_instance::LaunchRequest::Extract(path)
            } else {
                single_instance::LaunchRequest::Open(path)
            });
        }
        if !self.launch_add.is_empty() {
            let paths = std::mem::take(&mut self.launch_add);
            self.apply_launch(single_instance::LaunchRequest::Add(paths));
        }
        if self.is_primary {
            for request in single_instance::take_queue() {
                self.apply_launch(request);
            }
        }
        // A request that arrived while a job was running waits for it.
        if !self.job_running() {
            if let Some((path, then_extract)) = self.pending_open.take() {
                self.apply_open(path, then_extract);
            }
        }

        // Drag & drop: files/folders dropped onto the window open the create
        // wizard pre-filled (pack them), archives open directly.
        let dropped: Vec<String> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        });
        if !dropped.is_empty() {
            let all_archives = dropped.iter().all(|p| looks_like_archive(p));
            if all_archives && dropped.len() == 1 {
                self.open_archive(&dropped[0]);
            } else {
                self.create_sources = dropped;
                self.create_dest = String::new();
                self.show_create = true;
                self.refresh_default_dest();
            }
        }

        // Backspace is the keyboard twin of the "up" button. Skipped while a
        // text field owns the keyboard (a dialog is open), so editing a path
        // never navigates the browser behind the dialog.
        if self.archive.is_some()
            && !self.cwd.is_empty()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Backspace))
        {
            self.go_up();
        }

        // Ctrl+A selects every entry in the directory, the shortcut half of the
        // new multi-selection. Skipped for the same reason as Backspace: a text
        // field (or the settings page) owns the keyboard then.
        if self.archive.is_some()
            && !self.show_settings
            // An open dialog decides what gets extracted (or created) from the
            // selection, so it must not change under it: Ctrl+A while the
            // extract dialog is up would silently widen the scope.
            && !self.show_extract
            && !self.show_create
            && self.password_prompt.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
        {
            self.selected = self.rows.iter().map(|e| e.path.clone()).collect();
            self.anchor = if self.rows.is_empty() { None } else { Some(0) };
        }

    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The panels below are shown *inside* the root ui, which is what 0.34
        // changed them to take. The floating windows still want the context.
        let ctx = ui.ctx().clone();
        let ctx = &ctx;

        if self.show_settings {
            // Settings is a page of this window, so it draws *instead of* the
            // archive view rather than on top of it.
            let (save, close) = self.settings_page(ui);
            if save {
                self.save_settings();
            } else if close {
                self.show_settings = false;
            }
        } else {
            egui::Panel::top("toolbar").show(ui, |ui| {
                ui.add_space(4.0);
                self.toolbar(ui);
                ui.add_space(4.0);
            });
            egui::Panel::bottom("status").show(ui, |ui| {
                self.statusbar(ui);
            });
            egui::CentralPanel::default().show(ui, |ui| {
                // Browser only — the preview side panel was removed (it showed
                // raw hex/text that users found noisy; not worth the space).
                if self.archive.is_none() {
                    self.empty_state(ui);
                } else if self.rows.is_empty() {
                    theme::card(ui, |ui| {
                        // The navigation row stays: an empty folder — or a
                        // search with no hits — is exactly where a user needs
                        // a way back out.
                        self.nav_row(ui);
                        ui.add_space(20.0);
                        let empty = if self.searching() {
                            self.t("browser.search.none")
                        } else {
                            self.t("browser.empty_dir")
                        };
                        ui.vertical_centered(|ui| theme::hint(ui, &empty));
                    });
                } else {
                    self.browser(ui);
                }
            });
        }

        if self.open_picker {
            self.open_picker = false;
            let archives = self.t("pick.archives");
            if let Some(path) = rfd::FileDialog::new()
                // The numeric volume suffix is there because the create wizard
                // can produce split archives: its second volume is
                // `photos.7z.002`, and the engine opens the set from any
                // member. Without these filters the app could not reopen what
                // it had just written.
                .add_filter(
                    archives,
                    &[
                        "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso", "001", "002",
                        "003",
                    ],
                )
                .pick_file()
            {
                self.open_archive(path.to_str().unwrap_or(""));
            }
        }

        self.extract_dialog(ctx);
        self.password_dialog(ctx);
        self.create_dialog(ctx);
        self.donate_dialog(ctx);
        self.help_window(ctx);
        self.job_window(ctx);
        self.update_window(ctx);
        self.error_window(ctx);
    }
}

/// Whether an error key means "a password is needed", rather than a failure
/// worth an error dialog.
///
/// The engine separates the two cases: `required` when nothing was supplied,
/// `incorrect` when what was supplied did not work. Both are answered the same
/// way — ask again — so every place that has to choose between asking and
/// reporting shares this rule. `unsupported` is deliberately not here: the
/// engine cannot handle that encryption at all, and asking again would only
/// waste the user's time.
fn is_password_key(key: &str) -> bool {
    key == "error.password_required" || key == "error.password_incorrect"
}

#[cfg(test)]
mod password_key_tests {
    use super::is_password_key;

    #[test]
    fn only_the_two_password_keys_ask_again() {
        assert!(is_password_key("error.password_required"));
        assert!(is_password_key("error.password_incorrect"));
        // Asking again cannot fix an encryption the engine does not support,
        // and the remaining keys are ordinary failures.
        assert!(!is_password_key("error.password_unsupported"));
        assert!(!is_password_key("error.engine"));
        assert!(!is_password_key("error.cancelled"));
        assert!(!is_password_key("error.not_an_archive"));
    }
}

// ---------------------------------------------------------------------------
// Archive paths
// ---------------------------------------------------------------------------

/// Archive paths are `/`-separated and carry no leading or trailing separator.
/// Entry paths do not always arrive in that shape (a stored name may use `\`,
/// a directory entry is often written with a trailing `/`), so every path the
/// UI shows or compares against is normalised here first.
fn normalize_dir(dir: &str) -> String {
    dir.replace('\\', "/").trim_matches('/').to_string()
}

/// The parent of an archive directory, or `None` when it is the root — the
/// caller then leaves the view alone instead of navigating off the tree.
fn parent_dir(cwd: &str) -> Option<String> {
    let cwd = normalize_dir(cwd);
    if cwd.is_empty() {
        return None;
    }
    Some(match cwd.rsplit_once('/') {
        Some((head, _)) => head.to_string(),
        // One level below the root: the parent *is* the root.
        None => String::new(),
    })
}

/// One level deeper. Used to build breadcrumb targets, so the path a crumb
/// jumps to is exactly the path descending would have produced.
fn join_dir(parent: &str, name: &str) -> String {
    let parent = normalize_dir(parent);
    let name = normalize_dir(name);
    match (parent.is_empty(), name.is_empty()) {
        (_, true) => parent,
        (true, _) => name,
        (false, _) => format!("{parent}/{name}"),
    }
}

/// `(label, path)` for every level of `cwd`, root first, so each crumb can jump
/// to the level it names. Empty at the root, where there is nothing to show.
fn breadcrumb_segments(cwd: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut path = String::new();
    for segment in normalize_dir(cwd).split('/').filter(|s| !s.is_empty()) {
        path = join_dir(&path, segment);
        out.push((segment.to_string(), path.clone()));
    }
    out
}

/// A breadcrumb chip: text only, like the toolbar's ghost buttons, but with a
/// smaller font because a deep path puts many of them in one row.
fn crumb_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(text).size(12.0)).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// How far one wheel notch should travel: three rows, the Windows default.
/// egui's own default (`Options::line_scroll_speed`, 40 points) is about one
/// and a half rows of this list.
fn scroll_points_per_notch(row_height: f32) -> f32 {
    row_height * 3.0
}

// ---------------------------------------------------------------------------
// Table painting helpers
// ---------------------------------------------------------------------------

/// Paints the folder / file badge for one row.
fn paint_entry_icon(p: &egui::Painter, icon_rect: egui::Rect, e: &EntryDto, dark: bool) {
    if e.is_dir {
        let amber = egui::Color32::from_rgb(236, 184, 70);
        p.rect_filled(
            egui::Rect::from_min_size(icon_rect.min + egui::vec2(3.0, 5.0), egui::vec2(16.0, 12.0)),
            egui::CornerRadius::same(2),
            amber,
        );
        p.rect_filled(
            egui::Rect::from_min_size(icon_rect.min + egui::vec2(3.0, 3.0), egui::vec2(6.0, 3.0)),
            egui::CornerRadius::same(1),
            amber,
        );
        return;
    }
    let ext = e
        .name
        .rsplit_once('.')
        .map(|(_, s)| s.to_lowercase())
        .unwrap_or_default();
    let (accent, label) = entry_accent(&ext);
    let page =
        egui::Rect::from_min_size(icon_rect.min + egui::vec2(4.0, 2.0), egui::vec2(14.0, 16.0));
    let (fill, outline) = if dark {
        (
            egui::Color32::from_rgb(226, 228, 234),
            egui::Color32::from_rgb(120, 124, 132),
        )
    } else {
        (
            egui::Color32::from_rgb(250, 250, 252),
            egui::Color32::from_rgb(160, 160, 170),
        )
    };
    p.rect_filled(page, egui::CornerRadius::same(2), fill);
    p.rect_stroke(
        page,
        egui::CornerRadius::same(2),
        egui::Stroke::new(1.0_f32, outline),
        egui::StrokeKind::Inside,
    );
    p.rect_filled(
        egui::Rect::from_min_size(page.min + egui::vec2(2.0, 2.0), egui::vec2(10.0, 4.0)),
        egui::CornerRadius::same(1),
        accent,
    );
    if let Some(txt) = label {
        p.text(
            page.center() + egui::vec2(1.0, 5.0),
            egui::Align2::CENTER_CENTER,
            txt,
            egui::FontId::proportional(8.0),
            accent,
        );
    }
}

// ---------------------------------------------------------------------------
// Windows shell icons
// ---------------------------------------------------------------------------

/// Cache key for a row's shell icon.
///
/// An entry lives *inside* the archive, so the shell can never inspect the real
/// file's embedded resources: `SHGFI_USEFILEATTRIBUTES` is the only mode that
/// works here, and it resolves icons by file *type*. Keying by extension is
/// therefore both correct and cheap — two files with the same extension share
/// one texture, and all directories share the folder icon. A real `.exe` or
/// `.lnk` would need its own path looked up on disk, which an archive entry
/// does not have, so those fall back to the per-type icon as well.
fn shell_icon_key(e: &EntryDto) -> String {
    if e.is_dir {
        return "dir".to_string();
    }
    let ext = e
        .name
        .rsplit_once('.')
        .map(|(_, s)| s.to_lowercase())
        .unwrap_or_default();
    format!("ext:{ext}")
}

/// Resolve a [`shell_icon_key`] into a texture. `None` on non-Windows builds
/// and whenever the shell has no icon of its own to offer.
fn load_shell_icon(ctx: &egui::Context, key: &str) -> Option<egui::TextureHandle> {
    let (size, rgba) = shell_icon_rgba(key)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([size.0, size.1], &rgba);
    Some(ctx.load_texture(
        format!("shell-icon:{key}"),
        image,
        egui::TextureOptions::LINEAR,
    ))
}

/// Ask the Windows shell for the small icon of a directory, or of a file whose
/// extension follows `ext:`. Must be called from the UI thread — the texture
/// upload needs the egui context anyway, and `SHGetFileInfoW` is not documented
/// as safe to call concurrently.
#[cfg(windows)]
fn shell_icon_rgba(key: &str) -> Option<((usize, usize), Vec<u8>)> {
    use std::mem::zeroed;

    const SHGFI_ICON: u32 = 0x0000_0100;
    const SHGFI_SMALLICON: u32 = 0x0000_0001;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

    #[repr(C)]
    struct ShFileInfoW {
        hicon: isize,
        iicon: i32,
        attributes: u32,
        display_name: [u16; 260],
        type_name: [u16; 80],
    }

    #[repr(C)]
    struct IconInfo {
        is_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        mask: isize,
        color: isize,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetFileInfoW(
            path: *const u16,
            attributes: u32,
            info: *mut ShFileInfoW,
            info_size: u32,
            flags: u32,
        ) -> usize;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetIconInfo(icon: isize, info: *mut IconInfo) -> i32;
        fn DestroyIcon(icon: isize) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        // The two bitmaps behind an HICON are ours to release.
        fn DeleteObject(object: isize) -> i32;
    }

    // Nothing is looked up on disk: we hand the shell a plausible *name* plus
    // the attribute it should pretend the file has. That is what keeps this
    // cheap on a directory with thousands of entries.
    let (name, attributes) = if key == "dir" {
        ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY)
    } else {
        let ext = key.strip_prefix("ext:").unwrap_or("");
        let name = if ext.is_empty() {
            "x".to_string()
        } else {
            format!("x.{ext}")
        };
        (name, FILE_ATTRIBUTE_NORMAL)
    };
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let mut info: ShFileInfoW = zeroed();
        let found = SHGetFileInfoW(
            wide.as_ptr(),
            attributes,
            &mut info,
            std::mem::size_of::<ShFileInfoW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        );
        if found == 0 || info.hicon == 0 {
            return None;
        }
        let hicon = info.hicon;
        let mut icon_info: IconInfo = zeroed();
        let rgba = if GetIconInfo(hicon, &mut icon_info) == 0 {
            None
        } else {
            let rgba = icon_bitmaps_to_rgba(icon_info.color, icon_info.mask);
            // Both bitmaps belong to us once `GetIconInfo` returned.
            if icon_info.color != 0 {
                DeleteObject(icon_info.color);
            }
            if icon_info.mask != 0 {
                DeleteObject(icon_info.mask);
            }
            rgba
        };
        // The HICON is ours as well (SHGFI_ICON hands over a copy).
        DestroyIcon(hicon);
        rgba
    }
}

/// Non-Windows: there is no shell to ask, so the caller keeps painting the
/// coloured badge and nothing renders blank.
#[cfg(not(windows))]
fn shell_icon_rgba(_key: &str) -> Option<((usize, usize), Vec<u8>)> {
    None
}

/// Fold one icon's colour bitmap and 1bpp AND mask into straight RGBA. Modern
/// icons carry a real alpha channel, legacy ones only the mask, so which of the
/// two is authoritative is decided per icon.
#[cfg(windows)]
unsafe fn icon_bitmaps_to_rgba(color: isize, mask: isize) -> Option<((usize, usize), Vec<u8>)> {
    use std::ffi::c_void;
    use std::mem::zeroed;

    const BI_RGB: u32 = 0;
    const DIB_RGB_COLORS: u32 = 0;
    // No icon is anywhere near this big; a bogus bitmap size must not allocate.
    const MAX_SIDE: i32 = 256;

    #[repr(C)]
    struct Bitmap {
        kind: u32,
        width: i32,
        height: i32,
        width_bytes: u32,
        planes: u16,
        bits_per_pixel: u16,
        bits: *mut u8,
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_pels_per_meter: i32,
        y_pels_per_meter: i32,
        colors_used: u32,
        colors_important: u32,
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: isize) -> isize;
        fn DeleteDC(dc: isize) -> i32;
        fn GetDIBits(
            dc: isize,
            bitmap: isize,
            start: u32,
            lines: u32,
            bits: *mut u8,
            header: *mut BitmapInfoHeader,
            usage: u32,
        ) -> i32;
        fn GetObjectW(object: isize, size: i32, out: *mut c_void) -> i32;
    }

    let mut bitmap: Bitmap = zeroed();
    if color == 0
        || GetObjectW(
            color,
            std::mem::size_of::<Bitmap>() as i32,
            &mut bitmap as *mut Bitmap as *mut c_void,
        ) == 0
    {
        return None;
    }
    let (w, h) = (bitmap.width, bitmap.height);
    if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let (w, h) = (w as usize, h as usize);

    // GetDIBits needs a device context, and any one will do: the screen DC's
    // compatible context keeps us from having to release a shared handle.
    let dc = CreateCompatibleDC(0);
    if dc == 0 {
        return None;
    }
    let header = |w: usize, h: usize| BitmapInfoHeader {
        size: std::mem::size_of::<BitmapInfoHeader>() as u32,
        width: w as i32,
        // Positive height: GDI hands the rows back bottom-up, flipped below.
        height: h as i32,
        planes: 1,
        bit_count: 32,
        compression: BI_RGB,
        size_image: 0,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        colors_used: 0,
        colors_important: 0,
    };
    let mut color_px = vec![0u8; w * h * 4];
    let mut mask_px = vec![0u8; w * h * 4];
    let mut color_header = header(w, h);
    let got_color = GetDIBits(
        dc,
        color,
        0,
        h as u32,
        color_px.as_mut_ptr(),
        &mut color_header,
        DIB_RGB_COLORS,
    );
    let mut got_mask = 0;
    if mask != 0 {
        // A 1bpp mask asked for as 32bpp comes back as black/white, which is
        // easier to combine than the padded bit rows.
        let mut mask_header = header(w, h);
        got_mask = GetDIBits(
            dc,
            mask,
            0,
            h as u32,
            mask_px.as_mut_ptr(),
            &mut mask_header,
            DIB_RGB_COLORS,
        );
    }
    DeleteDC(dc);
    if got_color == 0 {
        return None;
    }

    let mut out = vec![0u8; w * h * 4];
    // An icon either carries a real alpha channel (modern) or leans on its
    // legacy 1bpp AND mask. Which one is decided once per icon: a 32bpp icon
    // may legitimately have alpha 0 on a pixel whose mask bit is clear, and
    // reading that as "opaque" paints those pixels black.
    let has_alpha = color_px.iter().skip(3).step_by(4).any(|a| *a != 0);
    // GDI hands the rows back bottom-up; walk them in reverse so the texture
    // comes out top-down like every other image we upload.
    for y in (0..h).rev() {
        let row = y * w * 4;
        for x in 0..w {
            let s = row + x * 4;
            let alpha = if has_alpha {
                color_px[s + 3]
            } else if got_mask != 0 && mask_px[s] != 0 {
                0
            } else {
                255
            };
            // GDI stores BGRA.
            out[row + x * 4] = color_px[s + 2];
            out[row + x * 4 + 1] = color_px[s + 1];
            out[row + x * 4 + 2] = color_px[s];
            out[row + x * 4 + 3] = alpha;
        }
    }
    Some(((w, h), out))
}

/// The little red padlock for encrypted entries, centred in its column.
fn paint_lock(p: &egui::Painter, center: egui::Pos2) {
    let red = egui::Color32::from_rgb(200, 60, 60);
    p.rect_filled(
        egui::Rect::from_center_size(center, egui::vec2(7.0, 6.0)),
        egui::CornerRadius::same(1),
        red,
    );
    p.circle_stroke(
        center + egui::vec2(0.0, -4.0),
        3.0,
        egui::Stroke::new(1.5_f32, red),
    );
}

/// One line of text inside `rect`, ellipsised when it does not fit.
fn paint_elided(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    align: egui::Align2,
) {
    if text.is_empty() || rect.width() <= 2.0 {
        return;
    }
    let job = egui::text::LayoutJob {
        text: text.to_string(),
        sections: vec![egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(text.len()),
            format: egui::TextFormat {
                font_id: font,
                color,
                ..Default::default()
            },
        }],
        wrap: egui::text::TextWrapping {
            max_width: rect.width(),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        },
        ..Default::default()
    };
    // 0.37 hands out a shared `FontsView`, so layout through the painter,
    // which takes the mutable borrow egui already holds for this frame.
    let galley = ui.painter().layout_job(job);
    // `anchor_size` anchors around a point, so pick the point the alignment
    // refers to (left edge, right edge or centre of the cell).
    let anchor = if align == egui::Align2::LEFT_CENTER {
        rect.left_center()
    } else if align == egui::Align2::RIGHT_CENTER {
        rect.right_center()
    } else {
        rect.center()
    };
    let pos = align.anchor_size(anchor, galley.size()).min;
    ui.painter().galley(pos, galley, color);
}

/// A dashed rectangle: egui has no dashed stroke, so the edges are drawn as
/// short segments.
fn dashed_rect(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5_f32, color);
    let (dash, gap) = (7.0_f32, 5.0_f32);
    for (from, to) in [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ] {
        let len = (to - from).length();
        if len <= 0.0 {
            continue;
        }
        let dir = (to - from) / len;
        let mut t = 0.0;
        while t < len {
            let end = (t + dash).min(len);
            p.line_segment([from + dir * t, from + dir * end], stroke);
            t = end + gap;
        }
    }
}

/// "Done" green — brightened for dark mode so it stays readable.
fn ok_color(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(94, 200, 126)
    } else {
        egui::Color32::from_rgb(40, 140, 60)
    }
}

/// Error red — brightened for dark mode for the same reason.
fn error_color(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(255, 122, 102)
    } else {
        egui::Color32::from_rgb(200, 60, 40)
    }
}

/// Window geometry in **physical pixels** — the one unit with no ambiguity.
///
/// egui reports points; multiplying by its pixels-per-point gives pixels, which
/// is also what the OS (`GetSystemMetrics`, `GetWindowRect`) uses. Persisting
/// points instead made the value depend on the UI zoom: at 125% every launch
/// stored a window a fifth smaller than reality, and the window shrank on each
/// start until it hit the minimum size.
fn physical_geometry(
    inner: egui::Rect,
    outer_min: egui::Pos2,
    pixels_per_point: f32,
) -> ((u32, u32), (i32, i32)) {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let size = (
        (inner.width() * ppp).round().max(1.0) as u32,
        (inner.height() * ppp).round().max(1.0) as u32,
    );
    let pos = (
        (outer_min.x * ppp).round() as i32,
        (outer_min.y * ppp).round() as i32,
    );
    (size, pos)
}

/// Stored physical pixels -> the logical points `ViewportBuilder` wants,
/// using the monitor DPI scale.
fn physical_to_logical(physical: f32, dpi_scale: f32) -> f32 {
    let scale = if dpi_scale.is_finite() && dpi_scale > 0.0 {
        dpi_scale
    } else {
        1.0
    };
    physical / scale
}

/// DPI scale of the primary monitor, read straight from Windows so the window
/// can be created at the right size on the very first frame (no visible jump).
#[cfg(windows)]
fn system_dpi_scale() -> f32 {
    #[link(name = "user32")]
    extern "system" {
        fn GetDpiForSystem() -> u32;
    }
    let dpi = unsafe { GetDpiForSystem() };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

#[cfg(not(windows))]
fn system_dpi_scale() -> f32 {
    1.0
}

#[cfg(test)]
mod archive_path_tests {
    use super::{breadcrumb_segments, join_dir, looks_like_archive, normalize_dir, parent_dir};

    #[test]
    fn a_format_extension_is_an_archive() {
        for p in ["a.zip", "A.7Z", r"C:\x\b.tar", r"C:\x\c.tar.gz", "d.iso"] {
            assert!(looks_like_archive(p), "{p} should be an archive");
        }
    }

    #[test]
    fn a_numbered_volume_is_an_archive() {
        // The create wizard writes these, so the app has to be able to open
        // what it produced.
        assert!(looks_like_archive(r"D:\out\photos.7z.001"));
        assert!(looks_like_archive(r"D:\out\photos.7z.0002"));
    }

    #[test]
    fn a_numbered_file_that_is_not_a_volume_is_not_an_archive() {
        // `photo.001` is a raw camera frame, and plain `.txt` is not anything.
        assert!(!looks_like_archive(r"D:\shots\photo.001"));
        assert!(!looks_like_archive(r"D:\docs\notes.txt"));
        assert!(!looks_like_archive(r"D:\docs\no-extension"));
    }

    #[test]
    fn one_separator_shape_is_used_everywhere() {
        assert_eq!(normalize_dir(""), "");
        assert_eq!(normalize_dir("/"), "");
        assert_eq!(normalize_dir("photos/"), "photos");
        // Entries stored by other tools carry backslashes.
        assert_eq!(normalize_dir(r"\photos\2024\"), "photos/2024");
    }

    #[test]
    fn the_parent_of_a_top_level_folder_is_the_root() {
        assert_eq!(normalize_dir("photos"), "photos");
        assert_eq!(parent_dir("photos").as_deref(), Some(""));
        assert_eq!(parent_dir("photos/2024/a").as_deref(), Some("photos/2024"));
        // Same answer whatever shape the entry path arrived in.
        assert_eq!(parent_dir(r"photos\2024\").as_deref(), Some("photos"));
        // At the root there is nowhere to go up to.
        assert_eq!(parent_dir(""), None);
        assert_eq!(parent_dir("/"), None);
    }

    #[test]
    fn joining_builds_the_path_the_browser_would_list() {
        assert_eq!(join_dir("", "photos"), "photos");
        assert_eq!(join_dir("photos", "2024"), "photos/2024");
        assert_eq!(join_dir("photos/", "/2024"), "photos/2024");
        assert_eq!(join_dir("photos", ""), "photos");
        assert_eq!(join_dir("", ""), "");
    }

    #[test]
    fn every_breadcrumb_names_a_level_it_can_jump_to() {
        let crumbs = breadcrumb_segments("photos/2024/a b");
        assert_eq!(
            crumbs,
            vec![
                ("photos".to_string(), "photos".to_string()),
                ("2024".to_string(), "photos/2024".to_string()),
                ("a b".to_string(), "photos/2024/a b".to_string()),
            ]
        );
        // Each crumb path is exactly what joining the labels gives, which is
        // also what descending into that level would set as `cwd`.
        assert_eq!(crumbs.last().unwrap().1, join_dir("photos/2024", "a b"));
        // At the root there is nothing to show.
        assert!(breadcrumb_segments("").is_empty());
        assert!(breadcrumb_segments("/").is_empty());
    }
}

#[cfg(test)]
mod shell_icon_tests {
    use super::{shell_icon_key, EntryDto};

    fn entry(name: &str, is_dir: bool) -> EntryDto {
        EntryDto {
            path: name.to_string(),
            name: name.to_string(),
            is_dir,
            size: 0,
            mtime_ms: None,
            encrypted: false,
        }
    }

    #[test]
    fn the_icon_cache_key_is_the_file_type() {
        // Every directory shares one icon, whatever it is called.
        assert_eq!(shell_icon_key(&entry("photos", true)), "dir");
        assert_eq!(shell_icon_key(&entry("photos", false)), "ext:");
        // Case is irrelevant to the shell, so it must be irrelevant to the key:
        // otherwise the same icon is cached and uploaded twice.
        assert_eq!(shell_icon_key(&entry("A.ZIP", false)), "ext:zip");
        assert_eq!(shell_icon_key(&entry("b.zip", false)), "ext:zip");
        // Only the last dot counts.
        assert_eq!(shell_icon_key(&entry("a.tar.gz", false)), "ext:gz");
    }

    /// The ROP chain (HICON -> DIBs -> straight RGBA) is the part that silently
    /// produces garbage when a constant or a row order is wrong, and it is the
    /// part no unit test of the key can see. Windows only — elsewhere the
    /// loader is compiled out and the painted badge is the icon.
    #[cfg(windows)]
    #[test]
    fn a_real_shell_icon_comes_back_as_rgba() {
        for key in ["dir", "ext:txt"] {
            let (size, rgba) = super::shell_icon_rgba(key).expect("shell icon");
            assert!(size.0 > 0 && size.1 > 0 && size.0 <= 64 && size.1 <= 64);
            assert_eq!(rgba.len(), size.0 * size.1 * 4);
            // An icon is mostly opaque; an all-transparent buffer would mean
            // the AND mask was folded the wrong way round.
            let opaque = rgba.iter().skip(3).step_by(4).filter(|a| **a > 0).count();
            assert!(
                opaque * 2 > size.0 * size.1,
                "{key}: only {opaque} of {} pixels opaque",
                size.0 * size.1
            );
        }
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::{scroll_points_per_notch, App};

    #[test]
    fn one_wheel_notch_travels_three_rows() {
        // Explorer's default is three lines per notch. egui would move the
        // list `Options::line_scroll_speed` points (40 on native), about a row
        // and a half, so the option has to be raised to this value.
        assert_eq!(scroll_points_per_notch(App::ROW_H), 78.0);
        assert!(scroll_points_per_notch(App::ROW_H) > 40.0);
        // Degenerate row heights must not produce a nonsense speed.
        assert_eq!(scroll_points_per_notch(0.0), 0.0);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::{physical_geometry, physical_to_logical};

    #[test]
    fn a_stored_geometry_survives_a_round_trip_at_any_dpi() {
        for dpi in [1.0_f32, 1.25, 1.5, 2.0] {
            // Save: native points -> physical pixels.
            let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 620.0));
            let (size, pos) = physical_geometry(inner, egui::pos2(120.0, 80.0), dpi);
            assert_eq!(size.0 as f32, 900.0 * dpi);
            assert_eq!(size.1 as f32, 620.0 * dpi);
            // Restore: physical pixels -> native points gives the window back.
            assert_eq!(physical_to_logical(size.0 as f32, dpi), 900.0);
            assert_eq!(physical_to_logical(size.1 as f32, dpi), 620.0);
            assert_eq!(physical_to_logical(pos.0 as f32, dpi), 120.0);
            assert_eq!(physical_to_logical(pos.1 as f32, dpi), 80.0);
        }
    }

    #[test]
    fn egui_points_become_physical_pixels() {
        // Measured on a 125%-zoom, 100%-DPI setup: egui reports 720x608 points
        // while the window really is 900x760 pixels.
        let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(720.0, 608.0));
        let (size, _) = physical_geometry(inner, egui::Pos2::ZERO, 1.25);
        assert_eq!(size, (900, 760));
    }

    #[test]
    fn unzoomed_geometry_is_unchanged() {
        let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 650.0));
        let (size, pos) = physical_geometry(inner, egui::pos2(120.0, 80.0), 1.0);
        assert_eq!(size, (900, 650));
        assert_eq!(pos, (120, 80));
    }

    #[test]
    fn a_broken_scale_does_not_corrupt_the_geometry() {
        let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        for ppp in [0.0, -1.0, f32::NAN] {
            assert_eq!(
                physical_geometry(inner, egui::Pos2::ZERO, ppp).0,
                (800, 600)
            );
        }
        assert_eq!(physical_to_logical(900.0, 1.25), 720.0);
        for scale in [0.0, -1.0, f32::NAN] {
            assert_eq!(physical_to_logical(900.0, scale), 900.0);
        }
    }

    #[test]
    fn a_saved_size_round_trips_through_both_conversions() {
        let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(720.0, 608.0));
        let (size, _) = physical_geometry(inner, egui::Pos2::ZERO, 1.25);
        let logical = (
            physical_to_logical(size.0 as f32, 1.0),
            physical_to_logical(size.1 as f32, 1.0),
        );
        assert_eq!((logical.0 as u32, logical.1 as u32), size);
    }
}

#[cfg(test)]
mod create_form_tests {
    use super::{default_archive_path, retarget_archive_ext, suggested_archive_name};

    #[test]
    fn the_suggested_output_path_sits_beside_the_source() {
        let dir = vec![r"C:\tmp\my folder".to_string()];
        assert_eq!(
            default_archive_path(&dir, "zip").as_deref(),
            Some(r"C:\tmp\my folder.zip")
        );
        let file = vec![r"C:\tmp\photo.png".to_string()];
        assert_eq!(
            default_archive_path(&file, "7z").as_deref(),
            Some(r"C:\tmp\photo.7z")
        );
        // Several sources are named after the folder that holds them.
        let many = vec![r"C:\tmp\a.txt".to_string(), r"D:\other\b.txt".to_string()];
        assert_eq!(
            default_archive_path(&many, "zip").as_deref(),
            Some(r"C:\tmp\tmp.zip")
        );
        // Nothing to derive a location from, so the field stays empty and the
        // hint tells the user to browse.
        assert_eq!(default_archive_path(&[], "zip"), None);
        assert_eq!(default_archive_path(&["bare.txt".to_string()], "zip"), None);
    }

    #[test]
    fn the_suggested_name_comes_from_the_source() {
        let file = vec![r"C:\tmp\photos.zip".to_string()];
        assert_eq!(suggested_archive_name(&file, "7z"), "photos.7z");
        let dir = vec![r"C:\tmp\my folder".to_string()];
        assert_eq!(suggested_archive_name(&dir, "zip"), "my folder.zip");
        // Several sources: name it after the folder that holds them.
        let many = vec![r"C:\tmp\a.txt".to_string(), r"C:\tmp\b.txt".to_string()];
        assert_eq!(suggested_archive_name(&many, "tar.gz"), "tmp.tar.gz");
        // Nothing picked yet: still a usable name, never an empty dialog box.
        assert_eq!(suggested_archive_name(&[], "zip"), "archive.zip");
    }

    #[test]
    fn the_extension_follows_the_chosen_format() {
        assert_eq!(retarget_archive_ext(r"C:\out\a.zip", "7z"), r"C:\out\a.7z");
        assert_eq!(
            retarget_archive_ext(r"C:\out\a.7z", "tar.gz"),
            r"C:\out\a.tar.gz"
        );
        assert_eq!(
            retarget_archive_ext(r"C:\out\a.tar.gz", "zip"),
            r"C:\out\a.zip"
        );
        assert_eq!(
            retarget_archive_ext(r"C:\out\a.TAR.XZ", "zip"),
            r"C:\out\a.zip"
        );
        // A name the user typed themselves is left alone.
        assert_eq!(retarget_archive_ext(r"C:\out\notes", "7z"), r"C:\out\notes");
        assert_eq!(retarget_archive_ext("", "zip"), "");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::{integration_plan, ShellOptions};

    fn settings(applied: bool, associate: bool, context_menu: bool) -> zipnest_ipc::Settings {
        zipnest_ipc::Settings {
            integration_applied: applied,
            associate,
            context_menu,
            ..Default::default()
        }
    }

    /// Every extension we ship a handler for: what a fresh profile claims.
    fn all_exts() -> Vec<String> {
        zipnest_ipc::shell::SUPPORTED_EXTENSIONS
            .iter()
            .map(|e| (*e).to_string())
            .collect()
    }

    #[test]
    fn a_fresh_machine_is_claimed_even_if_the_flags_were_off() {
        // The v0.4.7 file on a real machine says `associate: false` because
        // that used to be the default. Treating it as a decision is exactly how
        // a fresh install stayed unclaimed; the first claim ignores it.
        let plan = integration_plan(&settings(false, false, false), false).unwrap();
        assert_eq!(
            plan,
            ShellOptions {
                associate: true,
                context_menu: true,
                extensions: all_exts()
            }
        );
    }

    #[test]
    fn a_normal_launch_never_touches_a_recorded_decision() {
        // Once the decision exists, launching the app must not re-register:
        // that is what makes unticking the box stick.
        assert!(integration_plan(&settings(true, true, true), false).is_none());
        assert!(integration_plan(&settings(true, false, false), false).is_none());
    }

    #[test]
    fn the_installer_reapplies_a_recorded_decision() {
        // `--register-integration` (force) restores a registration that went
        // missing, using the settings the user already has. Without this, a
        // reinstall could never bring the associations back.
        let plan = integration_plan(&settings(true, true, true), true).unwrap();
        assert_eq!(
            plan,
            ShellOptions {
                associate: true,
                context_menu: true,
                extensions: all_exts()
            }
        );
        // ... and an explicit opt-out still wins over the installer.
        let off = integration_plan(&settings(true, false, false), true).unwrap();
        assert_eq!(
            off,
            ShellOptions {
                associate: false,
                context_menu: false,
                extensions: all_exts()
            }
        );
        // Half-registered is a state the user can legitimately choose.
        let partial = integration_plan(&settings(true, true, false), true).unwrap();
        assert_eq!(
            partial,
            ShellOptions {
                associate: true,
                context_menu: false,
                extensions: all_exts()
            }
        );
    }

    #[test]
    fn a_per_extension_selection_is_reapplied_verbatim() {
        // Someone who keeps .tar with their own tools must get exactly that
        // back after a reinstall, not "all archives".
        let mut s = settings(true, true, true);
        s.assoc_extensions = vec!["zip".to_string(), "7z".to_string()];
        let plan = integration_plan(&s, true).unwrap();
        assert_eq!(plan.extensions, vec!["zip".to_string(), "7z".to_string()]);
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::{feedback_email, FEEDBACK_EMAIL_DOMAIN, FEEDBACK_EMAIL_USER};

    #[test]
    fn the_feedback_address_is_a_usable_mailbox() {
        // A typo here (a space, a doubled `@`, an empty half) is a channel that
        // silently swallows every report, so it is worth asserting.
        let addr = feedback_email();
        assert_eq!(addr.matches('@').count(), 1, "exactly one @: {addr}");
        assert!(
            !addr.contains(char::is_whitespace),
            "no spaces in an address"
        );
        let (user, domain) = addr.split_once('@').unwrap();
        assert!(!user.is_empty(), "local part is empty");
        assert!(domain.contains('.'), "domain has no dot: {domain}");
        assert!(!addr.contains(".."), "doubled dot: {addr}");
    }

    #[test]
    fn the_ui_never_prints_the_joined_address() {
        // The Settings dialog shows `user @ domain`; only the `mailto:` link and
        // the copy button build the real thing. This is what keeps a scrape of
        // the UI/executable strings from getting a working address.
        let shown = format!("{FEEDBACK_EMAIL_USER} @ {FEEDBACK_EMAIL_DOMAIN}");
        let real = feedback_email();
        assert_ne!(shown, real);
        assert!(
            !shown.contains(&real),
            "split form must not contain the whole address"
        );
    }

    #[test]
    fn the_mail_link_targets_that_mailbox() {
        let url = zipnest_ipc::mailto_url(&feedback_email(), "[ZipNest v0.4.9] test", "body");
        assert!(
            url.starts_with(&format!("mailto:{}?", feedback_email())),
            "mailto must point at the feedback mailbox: {url}"
        );
        assert!(url.contains("subject=") && url.contains("&body="));
    }
}

#[cfg(test)]
mod eta_tests {
    use super::JobState;

    fn job(done: u64, total: u64, eta: u64) -> JobState {
        JobState {
            done_bytes: done,
            total_bytes: total,
            eta_secs: eta,
            ..Default::default()
        }
    }

    #[test]
    fn the_eta_is_hidden_until_there_is_a_real_sample() {
        // A 300 MB job one megabyte in has a "rate" measured over a few
        // milliseconds; showing it produced "还剩 43 分钟" on ten seconds of
        // work.
        assert!(job(1 << 20, 300 << 20, 2600).eta_text("zh-CN").is_none());
        // Nothing to divide by, or nothing left to do.
        assert!(job(0, 100, 30).eta_text("zh-CN").is_none());
        assert!(job(100, 100, 1).eta_text("zh-CN").is_none());
        assert!(job(50, 100, 0).eta_text("zh-CN").is_none());
    }

    #[test]
    fn the_eta_reads_as_time_and_goes_away_when_finished() {
        assert_eq!(
            job(50, 100, 45).eta_text("zh-CN").as_deref(),
            Some("还剩 45s")
        );
        assert_eq!(
            job(50, 100, 95).eta_text("zh-CN").as_deref(),
            Some("还剩 1m 35s")
        );
        assert_eq!(
            job(50, 100, 3725).eta_text("zh-CN").as_deref(),
            Some("还剩 1h 2m")
        );
        assert_eq!(
            job(50, 100, 45).eta_text("en-US").as_deref(),
            Some("45s left")
        );
        let mut done = job(50, 100, 45);
        done.finished = true;
        assert!(done.eta_text("zh-CN").is_none());
    }
}

#[cfg(test)]
mod sort_tests {
    use super::{natural_cmp, sort_rows, EntryDto, SortKey};
    use std::cmp::Ordering;

    fn entry(name: &str, is_dir: bool, size: u64, mtime: Option<u64>) -> EntryDto {
        EntryDto {
            path: name.to_string(),
            name: name.to_string(),
            is_dir,
            size,
            mtime_ms: mtime,
            encrypted: false,
        }
    }

    fn names(rows: &[EntryDto]) -> Vec<&str> {
        rows.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn names_sort_numerically_not_lexically() {
        // The whole reason this is not a plain `str::cmp`: a file manager has to
        // put `file2` before `file10`, and `Chapter 2` before `Chapter 10`.
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("file10", "file2"), Ordering::Greater);
        assert_eq!(natural_cmp("报告2", "报告10"), Ordering::Less);
        // Padding zeros are not magnitude.
        assert_eq!(natural_cmp("a0007", "a8"), Ordering::Less);
        // Case does not decide the order of two otherwise equal names.
        assert_eq!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        // A name that is a prefix of another sorts first, and a digit run in the
        // middle of a name counts as a number too.
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
        assert_eq!(natural_cmp("x2y", "x10y"), Ordering::Less);
        // Zero padding is only a tie-break, so the ordering is deterministic
        // instead of depending on which one the sort happened to see first.
        assert_eq!(natural_cmp("007", "7"), Ordering::Greater);
    }

    #[test]
    fn directories_lead_whatever_the_column_says() {
        let mut rows = vec![
            entry("b.txt", false, 300, Some(3)),
            entry("zz", true, 0, None),
            entry("a.txt", false, 100, Some(1)),
        ];
        sort_rows(&mut rows, SortKey::Size, true);
        assert_eq!(names(&rows), ["zz", "a.txt", "b.txt"]);
        // Descending reverses the files but the directory stays on top.
        sort_rows(&mut rows, SortKey::Size, false);
        assert_eq!(names(&rows), ["zz", "b.txt", "a.txt"]);
    }

    #[test]
    fn a_missing_timestamp_counts_as_the_oldest() {
        let mut rows = vec![
            entry("b.txt", false, 0, Some(20)),
            entry("a.txt", false, 0, None),
            entry("c.txt", false, 0, Some(10)),
        ];
        sort_rows(&mut rows, SortKey::Mtime, true);
        assert_eq!(names(&rows), ["a.txt", "c.txt", "b.txt"]);
    }

    #[test]
    fn the_name_order_is_stable_for_names_differing_only_in_case() {
        let mut rows = vec![
            entry("b.txt", false, 0, None),
            entry("B.txt", false, 0, None),
        ];
        sort_rows(&mut rows, SortKey::Name, true);
        // `natural_cmp` calls these equal, so the tie-break has to decide, and
        // it has to decide the same way every time: uppercase sorts first.
        assert_eq!(names(&rows), ["B.txt", "b.txt"]);
    }
}
