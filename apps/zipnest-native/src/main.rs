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
const CURRENT_VERSION: &str = "0.4.11";
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
    Found { tag: String, url: String },
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
    let body = ureq::get(UPDATE_API)
        .set("User-Agent", &UPDATE_UA)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
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
        format!("Windows {}.{}.{} ({arch})", info.major, info.minor, info.build)
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

/// Open Windows' own Default apps page, pre-selecting ZipNest where the OS
/// supports it. This is the only supported way to hand ZipNest an extension
/// whose `UserChoice` already belongs to another program: Windows protects that
/// key (it cannot be written and cannot be deleted), so no amount of registry
/// work can take it back by itself.
fn open_default_apps_page() {
    open_shell_url(zipnest_ipc::shell::DEFAULT_APPS_URI);
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
fn integration_plan(
    settings: &zipnest_ipc::Settings,
    force: bool,
) -> Option<ShellOptions> {
    if !force && settings.integration_applied {
        return None;
    }
    Some(if settings.integration_applied {
        ShellOptions { associate: settings.associate, context_menu: settings.context_menu }
    } else {
        ShellOptions { associate: true, context_menu: true }
    })
}

/// Claim the Explorer integration. Called by `--register-integration` (the
/// installer runs it right after copying the files, `force = true`) and by the
/// first UI launch (`force = false`, which also covers the portable zip).
fn apply_integration(svc: &IpcService, force: bool) -> Option<zipnest_ipc::shell::ShellRegisterResult> {
    let settings = svc.settings_get().ok()?;
    let opts = integration_plan(&settings, force)?;
    let exe = std::env::current_exe().unwrap_or_default();
    svc.shell_register(
        &exe,
        opts,
        &zipnest_ipc::shell::WindowsRegistry,
        &zipnest_ipc::shell::WindowsRegistry,
    )
    .ok()
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
    /// Entries the last job could not write (unsafe name, skipped conflict,
    /// destination not creatable).
    skipped: u64,
    title: String,
}

impl JobState {
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
        let mut ops = zipnest_ipc::shell::context_menu_removals();
        ops.extend(zipnest_ipc::shell::assoc_removals());
        return match zipnest_ipc::shell::ShellApplier::run(
            &zipnest_ipc::shell::WindowsRegistry,
            &ops,
        ) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("ZipNest: shell cleanup failed: {e}");
                std::process::exit(1);
            }
        };
    }
    let mut launch_open: Option<String> = None;
    let mut launch_add: Vec<String> = Vec::new();
    // `--register-integration` is what the installer runs after copying the
    // files: it claims the archive extensions and the context menus, so a
    // machine where the app is never opened still has them. No UI starts.
    if args.iter().skip(1).any(|a| a == "--register-integration") {
        cap_cli_runtime();
        let svc = IpcService::new(
            Arc::new(|_: &str, _: serde_json::Value| {}),
            std::time::Duration::ZERO,
        );
        let already_applied = svc.settings_get().map(|s| s.integration_applied).unwrap_or(false);
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
        Some(single_instance::LaunchRequest::Open(path))
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
    if let (Some(w), Some(h)) = (saved.window_width, saved.window_height) {
        viewport = viewport.with_inner_size([
            physical_to_logical(w as f32, dpi),
            physical_to_logical(h as f32, dpi),
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
        Box::new(move |cc| Ok(Box::new(App::new(cc, launch_open, launch_add, is_primary)))),
    )
}

struct App {
    ctx: egui::Context,
    svc: Arc<IpcService>,
    settings: zipnest_ipc::Settings,
    launch_open: Option<String>,
    launch_add: Vec<String>,
    /// Archive a later launch asked for while a job was running; opened once
    /// the job lets go of the archive.
    pending_open: Option<String>,
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
    cwd: String,
    rows: Vec<EntryDto>,
    selected: std::collections::HashSet<String>,
    error: Option<String>,
    notice: Option<String>,
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
    overwrite: bool,
    show_create: bool,
    create_sources: Vec<String>,
    create_dest: String,
    create_format: String,
    create_level: String,
    create_method: String,
    create_password: String,
    create_encrypt_names: bool,
    create_volume: String,
    create_custom_volume: String,
    create_sfx: bool,
    show_settings: bool,
    /// Extensions Windows has already assigned to another program (its
    /// protected per-extension `UserChoice`). The writes succeeded, but the OS
    /// keeps the other handler until the user changes it in the system Default
    /// apps page, so the Settings dialog explains it and offers the deep link.
    assoc_blocked: Vec<String>,
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
    // auto-update
    update: Arc<Mutex<UpdateState>>,
}

impl App {
fn new(
        cc: &eframe::CreationContext<'_>,
        launch_open: Option<String>,
        launch_add: Vec<String>,
        is_primary: bool,
    ) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let mut app = Self::init(cc);
        app.launch_open = launch_open;
        app.launch_add = launch_add;
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
        let assoc_blocked = boot.map(|r| r.blocked).unwrap_or_default();
        // Theme: light, dark, or whatever the system is set to. Applied here
        // rather than next to install_fonts because it needs the settings.
        theme::apply_setting(&cc.egui_ctx, &settings.theme_mode);
        // Clean up a previously downloaded update installer that already ran
        // (it is only needed while the update is being applied).
        let _ = std::fs::remove_file(std::env::temp_dir().join("zipnest-update-setup.exe"));
        // Fixed zoom from settings, applied ONCE at startup. Never re-zoom per
        // frame — that feedback loop made fullscreen flicker badly.
        cc.egui_ctx.set_zoom_factor(settings.ui_zoom.clamp(100, 200) as f32 / 100.0);
        // One wheel notch should travel three rows, like Explorer. egui turns a
        // native `LineDelta` notch into `Options::line_scroll_speed` points (40
        // by default, about 1.5 rows here), which is why the list crawled.
        // Raising that option instead of rescaling
        // `InputState::smooth_scroll_delta` leaves the ScrollArea, scrollbar
        // dragging, keyboard navigation and selection untouched, and it only
        // affects real notches: touchpad / high-resolution `Point` deltas are
        // already proportional and stay as they are.
        cc.egui_ctx.options_mut(|o| o.line_scroll_speed = scroll_points_per_notch(Self::ROW_H));
        let lang = match settings.language.as_str() {
            "zh-CN" => "zh-CN",
            "en-US" => "en-US",
            _ => i18n::os_language(),
        }
        .to_string();
        let qr_wechat = i18n::load_image(include_bytes!("assets/donate/wechat.jpg"))
            .map(|img| cc.egui_ctx.load_texture("wechat", img, egui::TextureOptions::NEAREST));
        let qr_alipay = i18n::load_image(include_bytes!("assets/donate/alipay.jpg"))
            .map(|img| cc.egui_ctx.load_texture("alipay", img, egui::TextureOptions::NEAREST));
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
        // first frame. maximized_saved starts at the stored value so the first
        Self {
            ctx,
            svc,
            settings,
            launch_open: None,
            launch_add: Vec::new(),
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
            cwd: String::new(),
            rows: Vec::new(),
            selected: Default::default(),
            error: None,
            notice: None,
            copied_at: None,
            lang,
            open_picker: false,
            jobs,
            job: None,
            show_extract: false,
            extract_dest: String::new(),
            extract_skipped: 0,
            overwrite: true,
            show_create: false,
            create_sources: Vec::new(),
            create_dest: String::new(),
            create_format: "zip".into(),
            create_level: "normal".into(),
            create_method: "auto".into(),
            create_password: String::new(),
            create_encrypt_names: false,
            create_volume: "off".into(),
            create_custom_volume: String::new(),
            create_sfx: false,
            show_settings: false,
            assoc_blocked,
            show_donate: false,
            help_popup: None,
            qr_wechat,
            qr_alipay,
            icons: Default::default(),
            icon_order: Default::default(),
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
        open_shell_url(&zipnest_ipc::mailto_url(&feedback_email(), &subject, &report));
    }

    /// Status text for the last job. `job.ok_skipped` carries the count as
    /// `{count}`; native i18n has no interpolator, so it is substituted here.
    /// The one-line verdict for Settings -> Integration.
    ///
    /// Three states a user can be in, and each needs different words: we are not
    /// claiming anything (checkbox off), we are the default, or Windows has
    /// already given these extensions to another program and only the user can
    /// change that.
    fn assoc_status(&self) -> (&'static str, Option<String>) {
        if !self.settings.associate {
            return ("settings.shell.status_off", None);
        }
        if self.assoc_blocked.is_empty() {
            return ("settings.shell.status_ours", None);
        }
        let exts = self
            .assoc_blocked
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(" ");
        ("settings.shell.blocked", Some(exts))
    }

    /// Re-read what Windows currently resolves, so the verdict above is never a
    /// stale guess (the user may have changed the default in Windows Settings
    /// while ZipNest was open).
    fn refresh_assoc_status(&mut self) {
        self.assoc_blocked =
            zipnest_ipc::shell::blocked_extensions(&zipnest_ipc::shell::WindowsRegistry);
    }

    /// "One click to default": hand the user Windows' own default-programs page
    /// with ZipNest in it.
    ///
    /// Two routes, because only one of them is reliable: the classic
    /// `LaunchAdvancedAssociationUI` page is preselected to ZipNest but has been
    /// removed from recent Windows builds (it answers E_INVALIDARG for every app
    /// name there -- see `shell::launch_default_apps_ui`), so an error falls
    /// straight through to the Settings deep link, which on Windows 11 lands on
    /// ZipNest's own page and on Windows 10 opens Default apps.
    ///
    /// Off the UI thread: the classic page is modal and blocks until closed.
    fn set_default_now(&mut self) {
        self.notice = Some("settings.shell.set_default.opening".into());
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            if zipnest_ipc::shell::launch_default_apps_ui().is_err() {
                open_shell_url(zipnest_ipc::shell::DEFAULT_APPS_URI);
            }
            ctx.request_repaint();
        });
    }

    fn notice_text(&self) -> Option<String> {
        let key = self.notice.as_deref()?;
        let mut text = self.t(key);
        if key == "job.ok_skipped" {
            let n = self.job.as_ref().map(|j| j.skipped).unwrap_or(0);
            text = text.replace("{count}", &n.to_string());
        }
        Some(text)
    }

    /// Small "?" that explains a wizard field.
    ///
    /// Hovering shows the explanation right away; clicking pins the same text in
    /// a small popup, because a hover-only hint silently does nothing for anyone
    /// who (as most people do) clicks the mark instead of hovering it.
    fn help_hint(&mut self, ui: &mut egui::Ui, label: &str, text: &str) {
        let accent = ui.visuals().hyperlink_color;
        let fill = ui.visuals().widgets.inactive.weak_bg_fill;
        let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
        let resp = ui.add(
            egui::Button::new(egui::RichText::new("?").size(11.0).color(accent))
                .rounding(egui::Rounding::same(7.0))
                .min_size(egui::vec2(15.0, 15.0))
                .fill(fill)
                .stroke(egui::Stroke::new(1.0_f32, stroke)),
        );
        let resp = resp.on_hover_ui(|ui| {
            ui.set_max_width(380.0);
            ui.label(text);
        });
        if resp.clicked() {
            self.help_popup = Some((label.to_string(), text.to_string()));
        }
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
        let pending: Vec<_> = self.jobs.lock().unwrap().drain(..).collect();
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
                    if ok {
                        self.notice =
                            Some(if skipped > 0 { "job.ok_skipped" } else { "job.ok" }.into());
                    } else {
                        let key = err_key
                            .filter(|k| k.starts_with("error."))
                            .unwrap_or_else(|| "error.engine".into());
                        self.notice = Some(key.clone());
                        // Pop a dialog so a failure is never missed.
                        self.error = Some(self.t(&key));
                    }
                    // Done or cancelled: close only when the user asked for it.
                    // The window carries the "done" message (and the output
                    // path), so disappearing on its own hides the result.
                    if self.settings.auto_close_after_job && (ok || cancelled) {
                        self.close_at = Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(2),
                        );
                    }
                    self.extract_skipped = 0;
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
                if self.job_running() {
                    // Remember it and open it once the job is done.
                    self.pending_open = Some(path);
                } else if !self.same_archive(&path) {
                    self.open_archive(&path);
                }
                // Already showing this archive: the caller only wanted the
                // window raised, so the selection and any open dialog stay.
            }
            single_instance::LaunchRequest::Add(paths) => {
                self.create_sources = paths;
                self.create_dest = String::new();
                self.show_create = true;
                self.refresh_default_dest();
            }
        }
    }

    fn open_archive(&mut self, path: &str) {
        // Release whatever we are replacing first: the engine, the source file
        // handle and the session password all live inside that archive, and
        // nothing else closes it (see `IpcService::close_archive`).
        if let Some(old) = self.archive.take() {
            self.svc.close_archive(old.id);
        }
        match self.svc.open_archive(path.to_string(), None) {
            Ok(res) => {
                let id = res.id;
                self.archive = Some(res);
                self.archive_path = path.to_string();
                self.cwd = String::new();
                self.rows = self.svc.list_children(id, String::new()).unwrap_or_default();
                self.selected.clear();
                self.error = None;
            }
            Err(e) => self.error = Some(self.t(&e.key)),
        }
    }

    /// The one place that changes the browsed directory. Descending, the up and
    /// root buttons, a breadcrumb click and Backspace all funnel through here,
    /// so `cwd` and `rows` can never disagree about where we are.
    fn navigate(&mut self, dir: &str) {
        let dir = normalize_dir(dir);
        if let Some(a) = &self.archive {
            self.rows = self.svc.list_children(a.id, dir.clone()).unwrap_or_default();
            self.cwd = dir;
            self.selected.clear();
        }
    }

    /// Backspace and the "up" button. At the root there is nowhere to go, so
    /// this does nothing rather than clearing the current view.
    fn go_up(&mut self) {
        if let Some(up) = parent_dir(&self.cwd) {
            self.navigate(&up);
        }
    }

    /// Up / root / breadcrumb strip, drawn above the file list once we are
    /// inside the archive (at the root all three controls would be dead).
    fn nav_row(&mut self, ui: &mut egui::Ui) {
        if self.cwd.is_empty() {
            return;
        }
        // Compact: the breadcrumb is a row of chips, not a second toolbar.
        // Scoped to this child row so the list's own spacing is untouched.
        let mut target: Option<String> = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
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
        });
        if let Some(dir) = target {
            self.navigate(&dir);
        }
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
                    self.create_dest = self.current_archive_dir();
                }
let extract_enabled = self.archive.is_some();
                let extract_btn = egui::Button::new(
                    egui::RichText::new(self.t("extract.start"))
                        .color(egui::Color32::WHITE),
                )
                // Same accent as every other primary button, so the two blue
                // buttons match in dark mode too.
                .fill(ui.visuals().hyperlink_color)
                .stroke(egui::Stroke::NONE)
                .rounding(egui::Rounding::same(6.0));
                if ui
                    .add_enabled(extract_enabled, extract_btn)
                    .on_hover_text(if self.selected.is_empty() {
                        self.t("extract.all_hint")
                    } else {
                        String::new()
                    })
                    .clicked()
                {
                    self.extract_dest = self.default_extract_dest();
                    // The dialog checkbox mirrors the saved policy; "ask" starts
                    // from "do not overwrite", the safe default.
                    self.overwrite = self.settings.overwrite_policy == "overwrite";
                    // No selection → extract the whole archive (empty paths
                    // tells the engine to unpack everything).
                    self.show_extract = true;
                }
                ui.separator();
                if let Some(a) = &self.archive {
                    // The archive *name* is what the user needs to see; the
                    // format alone ("zip") told them nothing.
                    let name = std::path::Path::new(&self.archive_path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| self.archive_path.clone());
                    ui.add(
                        egui::Label::new(egui::RichText::new(name).size(13.0).strong())
                            .truncate(),
                    )
                    .on_hover_text(&self.archive_path);
                    ui.add(egui::Label::new(
                        egui::RichText::new(a.format.clone())
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    ));
                } else {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(self.t("browser.empty"))
                                .size(13.0)
                                .color(ui.visuals().weak_text_color()),
                        ),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::ghost_button(ui, &self.t("settings.donate")).clicked() {
                        self.show_donate = true;
                    }
                    if theme::ghost_button(ui, if self.lang == "zh-CN" { "EN" } else { "中文" }).clicked() {
                        self.lang = if self.lang == "zh-CN" { "en-US" } else { "zh-CN" }.into();
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
                });
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
            let weak = ui.visuals().weak_text_color();
            let text_color = ui.visuals().text_color();
            let hairline = ui.visuals().widgets.inactive.bg_stroke.color;

            self.trim_icons();
            self.nav_row(ui);

            // ---- header ----
            let (head, _) = ui.allocate_exact_size(egui::vec2(full_w, 20.0), egui::Sense::hover());
            {
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
            }
            let sep_y = ui.cursor().top();
            ui.painter()
                .hline(ui.max_rect().x_range(), sep_y, egui::Stroke::new(1.0, hairline));
            ui.add_space(4.0);

            // ---- rows ----
            let mut click_dir: Option<String> = None;
            let mut dbl: Option<String> = None;
            let mut click_file: Option<String> = None;
            // Shell icons cost a Win32 round trip each; resolve a handful per
            // frame and let the painted badge stand in until a row's turn
            // comes, so a 10 000-entry directory cannot stall the UI thread.
            let mut icon_budget = Self::ICONS_PER_FRAME;
            let rows = self.rows.clone();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, e) in rows.iter().enumerate() {
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
                        painter.rect_filled(rect, egui::Rounding::same(4.0), bg);

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

                        // Name: one line, ellipsised inside its column.
                        let name_rect = egui::Rect::from_min_max(
                            egui::pos2(icon_rect.right() + 8.0, rect.top()),
                            egui::pos2(size_x - 12.0, rect.bottom()),
                        );
                        paint_elided(
                            ui,
                            name_rect,
                            &e.name,
                            egui::FontId::proportional(13.5),
                            text_color,
                            egui::Align2::LEFT_CENTER,
                        );

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
                        // filled in before.
                        let mtime = e
                            .mtime_ms
                            .and_then(format_mtime_local)
                            .unwrap_or_else(|| "-".to_string());
                        paint_elided(
                            ui,
                            egui::Rect::from_min_max(
                                egui::pos2(mtime_x + 2.0, rect.top()),
                                egui::pos2(enc_x - 4.0, rect.bottom()),
                            ),
                            &mtime,
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
                        } else if resp.clicked() {
                            if e.is_dir {
                                click_dir = Some(e.path.clone());
                            } else {
                                click_file = Some(e.path.clone());
                            }
                        }
                    }
                    if let Some(d) = dbl.or(click_dir) {
                        self.navigate(&d);
                    } else if let Some(f) = click_file {
                        // Select only. This used to also run the preview reader,
                        // which pulled up to `preview_max_bytes` out of the
                        // archive on the UI thread for a panel that no longer
                        // exists.
                        self.selected.clear();
                        self.selected.insert(f);
                    }
                });
        });
    }

    /// Shown when nothing is open. The window has always accepted drops, but
    /// nothing told the user so — this is that signpost.
    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
        let border = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2((rect.width() - 90.0).max(240.0), (rect.height() - 90.0).max(180.0)),
        );
        let accent = ui.visuals().hyperlink_color;
        let weak = ui.visuals().weak_text_color();
        let text = ui.visuals().text_color();
        {
            let painter = ui.painter().clone();
            dashed_rect(&painter, border, ui.visuals().widgets.inactive.bg_stroke.color);
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
                .rounding(egui::Rounding::same(6.0))
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
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(status).color(ui.visuals().weak_text_color()),
                    ),
                );
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

    fn extract_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_extract;
        let mut close = false;
        let arch_id = self.archive.as_ref().map(|a| a.id).unwrap_or(0);
        if self.archive.is_none() {
            self.show_extract = false;
            return;
        }
        let srcs = self.selected.clone();
        let svc = self.svc.clone();
        let t = self.t("extract.title");
        let center = ctx.screen_rect().center() - egui::vec2(215.0, 110.0);
        egui::Window::new(t)
            .collapsible(false)
            .default_width(430.0)
            .default_pos(center)
            .open(&mut open)
            .show(ctx, |ui| {
            ui.label(self.t("extract.dest"));
            ui.text_edit_singleline(&mut self.extract_dest);
            if ui.button(self.t("extract.browse")).clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.extract_dest = dir.to_str().unwrap_or("").to_string();
                }
            }
            // The saved conflict policy decides what this dialog offers: "ask"
            // shows the checkbox, the other three are shown as a fixed setting
            // so a saved policy is never silently ignored.
            let policy = self.settings.overwrite_policy.clone();
            let ask = policy == "ask";
            if ask {
                let overwrite_label = self.t("extract.overwrite");
                ui.checkbox(&mut self.overwrite, overwrite_label);
            } else {
                ui.label(format!(
                    "{}: {}",
                    self.t("settings.overwrite"),
                    self.t(&format!("settings.overwrite.{policy}"))
                ));
            }
            let can = !self.extract_dest.trim().is_empty();
            let clicked = ui
                .add_enabled(can, egui::Button::new(self.t("extract.start")))
                .clicked();
            // Enter confirms (that is what a dialog is expected to do).
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                self.job = Some(JobState {
                    job_id: 0,
                    title: self.t("extract.title"),
                    ..Default::default()
                });
                match svc.extract(
                    arch_id,
                    srcs.iter().cloned().collect(),
                    dest,
                    Some(on_conflict),
                    None,
                ) {
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
                self.show_extract = false;
            }
            ui.separator();
            if ui.button(self.t("extract.cancel")).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                close = true;
            }
        });
        // Cancel used to set `show_extract` inside the closure, which the line
        // below then overwrote — the button did nothing at all.
        self.show_extract = open && !close && self.job.is_none();
    }

    fn create_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_create;
        let mut close = false;
        let svc = self.svc.clone();
        let t = self.t("create.title");
        egui::Window::new(t).collapsible(false).open(&mut open).show(ctx, |ui| {
            ui.label(self.t("create.sources"));
            if ui.button(self.t("create.add_files")).clicked() {
                let all = self.t("pick.all");
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter(all, &["*"])
                    .pick_files()
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
            for s in &self.create_sources.clone() {
                ui.label(format!("• {s}"));
            }
            ui.separator();
            let dest_label = self.t("create.dest");
            ui.label(&dest_label);
            // Read-only on purpose. A bare name typed here used to be handed to
            // the engine as-is, so the archive was written relative to whatever
            // working directory the shell happened to pass us and the user could
            // not find it. The path is now always picked with 浏览…, or filled in
            // next to the sources before the dialog opens.
            let dest_hint = self.t("create.dest_hint");
            ui.add(
                egui::TextEdit::singleline(&mut self.create_dest)
                    .interactive(false)
                    .desired_width(f32::INFINITY)
                    .hint_text(dest_hint),
            );
            if ui.button(self.t("extract.browse")).clicked() {
                // The Save-As dialog used to open with an empty file name and an
                // "All files" filter, so 保存 did nothing until the user typed a
                // name themselves — which reads as "browse cannot select
                // anything". Pre-fill a sensible name and match the format.
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
ui.horizontal(|ui| {
                let label = self.t("create.format");
                ui.label(&label);
                let hint = self.t("help.format");
                self.help_hint(ui, &label, &hint);
                for f in ["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
                    let sel = self.create_format == f;
                    if ui.selectable_label(sel, f).clicked() {
                        self.create_format = f.to_string();
                        self.create_dest = retarget_archive_ext(&self.create_dest, f);
                        self.refresh_default_dest();
                    }
                }
            });
            ui.horizontal(|ui| {
                let label = self.t("create.level");
                ui.label(&label);
                let hint = self.t("help.level");
                self.help_hint(ui, &label, &hint);
                for l in ["store", "fastest", "normal", "maximum", "ultra"] {
                    let sel = self.create_level == l;
                    let lbl = self.t(&format!("create.level.{l}"));
                    if ui.selectable_label(sel, lbl).clicked() {
                        self.create_level = l.to_string();
                    }
                }
            });
            ui.horizontal(|ui| {
                let label = self.t("create.method");
                ui.label(&label);
                let hint = self.t("help.method");
                self.help_hint(ui, &label, &hint);
                for m in ["auto", "copy", "deflate", "lzma2", "bzip2"] {
                    let sel = self.create_method == m;
                    let lbl = self.t(&format!("create.method.{m}"));
                    if ui.selectable_label(sel, lbl).clicked() {
                        self.create_method = m.to_string();
                    }
                }
            });
            ui.horizontal(|ui| {
                let label = self.t("create.password");
                ui.label(&label);
                let hint = self.t("help.password");
                self.help_hint(ui, &label, &hint);
                ui.text_edit_singleline(&mut self.create_password);
            });
            if self.create_format == "7z" {
                let lbl = self.t("create.encrypt_names");
                let hint = self.t("help.encrypt_names");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.create_encrypt_names, &lbl);
                    self.help_hint(ui, &lbl, &hint);
                });
            }
            ui.horizontal(|ui| {
                let label = self.t("create.volume");
                ui.label(&label);
                let hint = self.t("help.volume");
                self.help_hint(ui, &label, &hint);
for v in ["off", "10m", "100m", "1g", "custom"] {
                    let sel = self.create_volume == v;
                    let lbl = self.t(&format!("create.volume.{v}"));
                    if ui.selectable_label(sel, lbl).clicked() {
                        self.create_volume = v.to_string();
                    }
                }
                if self.create_volume == "custom" {
                    ui.text_edit_singleline(&mut self.create_custom_volume);
                }
            });
            if self.create_format == "7z" {
                let lbl = self.t("create.sfx");
                ui.checkbox(&mut self.create_sfx, lbl);
            }
            ui.separator();
            // The engine resolves a relative destination against the process
            // working directory, which for a shell-launched window is nowhere
            // the user would look, so only a full path may start a job.
            let dest_absolute = std::path::Path::new(self.create_dest.trim()).is_absolute();
            let can = !self.create_sources.is_empty() && dest_absolute;
            let clicked = ui
                .add_enabled(can, egui::Button::new(self.t("create.start")))
                .clicked();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if clicked || (can && enter) {
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
                    encrypt_names: if self.create_encrypt_names { Some(true) } else { None },
                    volume_bytes,
                    sfx: if self.create_sfx { Some("gui".into()) } else { None },
                };
                self.job = Some(JobState {
                    job_id: 0,
                    title: self.t("create.title"),
                    ..Default::default()
                });
                match svc.create_archive(
                    self.create_sources.clone(),
                    self.create_dest.clone(),
                    req,
                ) {
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
                self.show_create = false;
            }
            ui.separator();
            if ui.button(self.t("extract.cancel")).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                close = true;
            }
        });
        // Same overwrite-the-flag bug as the extract dialog: Cancel did nothing.
        self.show_create = open && !close && self.job.is_none();
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let svc = self.svc.clone();
        let mut save = false;
        let mut close = false;
        egui::Window::new(self.t("settings.title"))
            .collapsible(false)
            .resizable(false)
            .default_width(470.0)
            .open(&mut open)
            .show(ctx, |ui| {
                // The body scrolls and the buttons stay put, so adding a setting
                // can never push them off the bottom edge of the dialog.
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        theme::section(ui, &self.t("settings.section.general"));
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.language"));
                            for (v, k) in [("system", "settings.language.system"), ("zh-CN", "settings.language.zh"), ("en-US", "settings.language.en")] {
                                let sel = self.settings.language == v;
                                if ui.selectable_label(sel, self.t(k)).clicked() {
                                    self.settings.language = v.to_string();
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.theme"));
                            for (v, k) in [("system", "settings.theme.system"), ("light", "settings.theme.light"), ("dark", "settings.theme.dark")] {
                                let sel = self.settings.theme_mode == v;
                                if ui.selectable_label(sel, self.t(k)).clicked() {
                                    self.settings.theme_mode = v.to_string();
                                    // Apply at once so the choice is visible.
                                    theme::apply_setting(ctx, v);
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.ui_zoom"));
                            for z in [100u32, 125, 150, 175, 200] {
                                let sel = self.settings.ui_zoom == z;
                                if ui.selectable_label(sel, self.t("settings.ui_zoom.percent").replace("{percent}", &z.to_string())).clicked() {
                                    self.settings.ui_zoom = z;
                                    ctx.set_zoom_factor(z as f32 / 100.0);
                                }
                            }
                        });

                        // Whether a finished job takes its own window with it.
                        // Applies to create and extract alike.
                        let auto_close_label = self.t("settings.auto_close");
                        ui.checkbox(&mut self.settings.auto_close_after_job, auto_close_label);
                        theme::hint(ui, &self.t("settings.auto_close.hint"));

                        theme::section(ui, &self.t("settings.section.extract"));
                        ui.label(self.t("settings.default_dir"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings.default_extract_dir)
                                    .desired_width(250.0),
                            );
                            if ui.button(self.t("extract.browse")).clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.settings.default_extract_dir =
                                        dir.to_str().unwrap_or("").to_string();
                                }
                            }
                        });
                        theme::hint(ui, &self.t("settings.default_dir.hint"));
                        ui.horizontal(|ui| {
                            // 带语境：明确这是"文件已存在时"的处理方式
                            ui.label(self.t("settings.overwrite.hint"));
                            for (v, k) in [("ask", "settings.overwrite.ask"), ("overwrite", "settings.overwrite.overwrite"), ("skip", "settings.overwrite.skip"), ("rename", "settings.overwrite.rename")] {
                                let sel = self.settings.overwrite_policy == v;
                                if ui.selectable_label(sel, self.t(k)).clicked() {
                                    self.settings.overwrite_policy = v.to_string();
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            let label = self.t("settings.max_extract");
                            ui.label(&label);
                            let hint = self.t("settings.max_extract.hint");
                            self.help_hint(ui, &label, &hint);
                            let mut mb = self.settings.max_extract_bytes / (1024 * 1024);
                            if ui.add(egui::DragValue::new(&mut mb).speed(256.0)).changed() {
                                // Real clamping happens in Settings::apply on the way out.
                                self.settings.max_extract_bytes = mb.max(1).saturating_mul(1024 * 1024);
                            }
                            ui.label("MB");
                        });

                        theme::section(ui, &self.t("settings.section.integration"));
                        // The label belongs *in* the checkbox: an empty label left
                        // a stray ✓ floating above its own text.
                        let associate_label = self.t("settings.shell.associate");
                        ui.checkbox(&mut self.settings.associate, associate_label);
                        theme::hint(ui, &self.t("settings.shell.assoc_hint"));
                        let menu_label = self.t("settings.shell.context_menu");
                        ui.checkbox(&mut self.settings.context_menu, menu_label);
                        // What Windows currently resolves for archives, then the
                        // one-click way to change it. Windows protects a
                        // per-extension `UserChoice` against *programs* (it
                        // cannot be written and cannot be deleted), but the user
                        // can replace it in one click on the page below -- the
                        // same route Bandizip and 7-Zip send people to.
                        let (status_key, status_exts) = self.assoc_status();
                        match status_exts {
                            Some(exts) => {
                                let msg = self.t(status_key).replace("{exts}", &exts);
                                theme::hint(ui, &msg);
                            }
                            None => {
                                ui.label(self.t(status_key));
                            }
                        }
                        ui.horizontal(|ui| {
                            if theme::primary_button(ui, &self.t("settings.shell.set_default")).clicked()
                            {
                                self.set_default_now();
                            }
                            if !self.assoc_blocked.is_empty()
                                && ui.button(self.t("settings.shell.open_default_apps")).clicked()
                            {
                                open_default_apps_page();
                            }
                        });
                        theme::hint(ui, &self.t("settings.shell.set_default.hint"));

                        theme::section(ui, &self.t("settings.section.about"));
                        let update_label = self.t("settings.auto_update");
                        ui.checkbox(&mut self.settings.auto_check_update, update_label);
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.version"));
                            ui.label(egui::RichText::new(format!("v{CURRENT_VERSION}")).strong());
                        });

                        theme::section(ui, &self.t("settings.section.feedback"));
                        theme::hint(ui, &self.t("settings.feedback.hint"));
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
                                        egui::RichText::new(self.t("settings.feedback.copied"))
                                            .italics(),
                                    );
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.feedback.email"));
                            // Shown with the `@` broken up: a bare address in a UI
                            // string is a ready-made scrape target. Selectable so
                            // the user can copy it, with the real one only inside
                            // the mailto link.
                            let shown = format!(
                                "{FEEDBACK_EMAIL_USER} @ {FEEDBACK_EMAIL_DOMAIN}"
                            );
                            ui.add(egui::Label::new(egui::RichText::new(shown).strong()).selectable(true));
                            if ui.button(self.t("settings.feedback.copy_email")).clicked() {
                                ui.ctx().copy_text(feedback_email());
                                self.copied_at = Some(std::time::Instant::now());
                            }
                        });
                        if ui.button(self.t("settings.feedback.mail")).clicked() {
                            self.send_feedback_mail(ui.ctx());
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(self.t("settings.save")).clicked() {
                        save = true;
                    }
                    if ui.button(self.t("extract.cancel")).clicked() {
                        close = true;
                    }
                });
                // Enter saves, Escape closes — what a dialog is expected to do.
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        if close {
            self.show_settings = false;
        }
        if save {
            let assoc = self.settings.associate;
            let menu = self.settings.context_menu;
            let exe = std::env::current_exe().unwrap_or_default();
            match svc.shell_register(
                &exe,
                ShellOptions { associate: assoc, context_menu: menu },
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
                    // Extensions Windows keeps for another program stay
                    // visibly unresolved instead of looking registered.
                    self.assoc_blocked = r.blocked;
                    self.notice = r.warnings.first().cloned();
                }
                Err(_) => self.notice = Some("error.io".into()),
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
        } else {
            self.show_settings = open;
        }
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
                egui::Window::new(title)
                    .collapsible(false)
                    .show(ctx, |ui| {
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
                    .default_pos(ctx.screen_rect().center() - egui::vec2(190.0, 60.0))
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
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.colored_label(error_color(ui), err);
                    if ui.button(self.t("extract.cancel")).clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }
    }

    fn job_window(&mut self, ctx: &egui::Context) {
        let Some(j) = self.job.clone() else {
            return;
        };
        let mut cancel = false;
        let mut close = false;
        let center = ctx.screen_rect().center() - egui::vec2(190.0, 70.0);
        egui::Window::new(j.title.clone())
            .id(egui::Id::new("zipnest-job-window"))
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
    num.parse::<f64>().ok().map(|n| (n * mult as f64) as u64).filter(|n| *n > 0)
}

impl eframe::App for App {
fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
            self.apply_launch(single_instance::LaunchRequest::Open(path));
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
            if let Some(path) = self.pending_open.take() {
                self.open_archive(&path);
            }
        }

        // Drag & drop: files/folders dropped onto the window open the create
        // wizard pre-filled (pack them), archives open directly.
        let dropped: Vec<String> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        });
        if !dropped.is_empty() {
            let all_archives = dropped.iter().all(|p| {
                std::path::Path::new(p)
                    .extension()
                    .map(|e| {
                        let e = e.to_string_lossy().to_lowercase();
                        matches!(e.as_str(), "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "iso")
                    })
                    .unwrap_or(false)
            });
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
            && !ctx.wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Backspace))
        {
            self.go_up();
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            self.statusbar(ui);
        });
egui::CentralPanel::default().show(ctx, |ui| {
            // Browser only — the preview side panel was removed (it showed
            // raw hex/text that users found noisy; not worth the space).
            if self.archive.is_none() {
                self.empty_state(ui);
            } else if self.rows.is_empty() {
                theme::card(ui, |ui| {
                    // The navigation row stays: an empty folder is exactly
                    // where a user needs a way back to its parent.
                    self.nav_row(ui);
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| theme::hint(ui, &self.t("browser.empty_dir")));
                });
            } else {
                self.browser(ui);
            }
        });

        if self.open_picker {
            self.open_picker = false;
            let archives = self.t("pick.archives");
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(archives, &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"])
                .pick_file()
            {
                self.open_archive(path.to_str().unwrap_or(""));
            }
        }

        self.extract_dialog(ctx);
        self.create_dialog(ctx);
        self.settings_dialog(ctx);
        self.donate_dialog(ctx);
        self.help_window(ctx);
        self.job_window(ctx);
        self.update_window(ctx);
        self.error_window(ctx);
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
            egui::Rounding::same(2.0),
            amber,
        );
        p.rect_filled(
            egui::Rect::from_min_size(icon_rect.min + egui::vec2(3.0, 3.0), egui::vec2(6.0, 3.0)),
            egui::Rounding::same(1.0),
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
    let page = egui::Rect::from_min_size(icon_rect.min + egui::vec2(4.0, 2.0), egui::vec2(14.0, 16.0));
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
    p.rect_filled(page, egui::Rounding::same(2.0), fill);
    p.rect_stroke(page, egui::Rounding::same(2.0), egui::Stroke::new(1.0, outline));
    p.rect_filled(
        egui::Rect::from_min_size(page.min + egui::vec2(2.0, 2.0), egui::vec2(10.0, 4.0)),
        egui::Rounding::same(1.0),
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
        egui::Rounding::same(1.0),
        red,
    );
    p.circle_stroke(center + egui::vec2(0.0, -4.0), 3.0, egui::Stroke::new(1.5, red));
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
            byte_range: 0..text.len(),
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
    let galley = ui.fonts(|f| f.layout_job(job));
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
    let stroke = egui::Stroke::new(1.5, color);
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
    use super::{breadcrumb_segments, join_dir, normalize_dir, parent_dir};

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
            assert_eq!(physical_geometry(inner, egui::Pos2::ZERO, ppp).0, (800, 600));
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
        assert_eq!(retarget_archive_ext(r"C:\out\a.7z", "tar.gz"), r"C:\out\a.tar.gz");
        assert_eq!(retarget_archive_ext(r"C:\out\a.tar.gz", "zip"), r"C:\out\a.zip");
        assert_eq!(retarget_archive_ext(r"C:\out\a.TAR.XZ", "zip"), r"C:\out\a.zip");
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

    #[test]
    fn a_fresh_machine_is_claimed_even_if_the_flags_were_off() {
        // The v0.4.7 file on a real machine says `associate: false` because
        // that used to be the default. Treating it as a decision is exactly how
        // a fresh install stayed unclaimed; the first claim ignores it.
        let plan = integration_plan(&settings(false, false, false), false).unwrap();
        assert_eq!(plan, ShellOptions { associate: true, context_menu: true });
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
        assert_eq!(plan, ShellOptions { associate: true, context_menu: true });
        // ... and an explicit opt-out still wins over the installer.
        let off = integration_plan(&settings(true, false, false), true).unwrap();
        assert_eq!(off, ShellOptions { associate: false, context_menu: false });
        // Half-registered is a state the user can legitimately choose.
        let partial = integration_plan(&settings(true, true, false), true).unwrap();
        assert_eq!(partial, ShellOptions { associate: true, context_menu: false });
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
        assert!(!addr.contains(char::is_whitespace), "no spaces in an address");
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
        assert!(!shown.contains(&real), "split form must not contain the whole address");
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