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
const CURRENT_VERSION: &str = "0.4.1";
const UPDATE_API: &str = "https://api.github.com/repos/CN6/zipnest/releases/latest";
const UPDATE_UA: &str = "ZipNest-Updater/0.4.0";
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
        .set("User-Agent", UPDATE_UA)
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

fn open_in_browser(url: &str) {
    // Hand the URL to the default browser. `start` needs the empty "" title
    // argument before the URL when the URL could be quoted.
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

fn spawn_update_check(state: Arc<Mutex<UpdateState>>) {
    std::thread::spawn(move || {
        *state.lock().unwrap() = UpdateState::Checking;
        match fetch_latest() {
            Ok((tag, url)) => {
                let next = if is_newer(&tag) {
                    UpdateState::Found { tag, url }
                } else {
                    UpdateState::Idle
                };
                *state.lock().unwrap() = next;
            }
            Err(e) => *state.lock().unwrap() = UpdateState::Failed(e),
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

#[derive(Clone)]
enum PreviewKind {
    Text(String),
    Hex(String),
    Image(egui::TextureHandle),
    None,
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
        .with_title(single_instance::WINDOW_TITLE);
    // Stored geometry is in physical pixels. `ViewportBuilder` wants logical
    // points, so divide by the *effective* pixels-per-point: the monitor DPI
    // scale times the saved UI zoom, which is exactly what
    // `ctx.pixels_per_point()` returns on the side that wrote the value.
    // Dividing by the DPI scale alone made every restart grow the window by the
    // zoom factor and walk it down-right until it left the screen.
    let ppp = effective_pixels_per_point(system_dpi_scale(), saved.ui_zoom);
    if let (Some(w), Some(h)) = (saved.window_width, saved.window_height) {
        viewport = viewport.with_inner_size([
            physical_to_logical(w as f32, ppp),
            physical_to_logical(h as f32, ppp),
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
                physical_to_logical(x as f32, ppp),
                physical_to_logical(y as f32, ppp),
            ]);
        }
    }
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
    /// The window exists for a shell request, so it should disappear once its
    /// job is done instead of lingering.
    auto_close: bool,
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
    lang: String,
    open_picker: bool,
    // jobs / preview
    jobs: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    job: Option<JobState>,
    preview: PreviewKind,
    preview_for: String,
    /// The preview we are showing is only the head of a larger entry.
    preview_truncated: bool,
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
    show_donate: bool,
    /// A help hint the user clicked on: `(field label, explanation)`.
    help_popup: Option<(String, String)>,
    // assets
    qr_wechat: Option<egui::TextureHandle>,
    qr_alipay: Option<egui::TextureHandle>,
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
        // Theme: light, dark, or whatever the system is set to. Applied here
        // rather than next to install_fonts because it needs the settings.
        theme::apply_setting(&cc.egui_ctx, &settings.theme_mode);
        // Clean up a previously downloaded update installer that already ran
        // (it is only needed while the update is being applied).
        let _ = std::fs::remove_file(std::env::temp_dir().join("zipnest-update-setup.exe"));
        // Fixed zoom from settings, applied ONCE at startup. Never re-zoom per
        // frame — that feedback loop made fullscreen flicker badly.
        cc.egui_ctx.set_zoom_factor(settings.ui_zoom.clamp(100, 200) as f32 / 100.0);
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
        // Auto-check is user-controllable in Settings (on by default).
        if settings.auto_check_update {
            let check = Arc::clone(&update);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(1));
                spawn_update_check(check);
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
        Self {
            ctx,
            svc,
            settings,
            launch_open: None,
            launch_add: Vec::new(),
            pending_open: None,
            is_primary: false,
            published_busy: None,
            auto_close: false,
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
            lang,
            open_picker: false,
            jobs,
            job: None,
            preview: PreviewKind::None,
            preview_for: String::new(),
            preview_truncated: false,
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
            show_donate: false,
            help_popup: None,
            qr_wechat,
            qr_alipay,
            update,
        }
    }

fn t(&self, key: &str) -> String {
        i18n::tr(&self.lang, key)
    }

    /// Status text for the last job. `job.ok_skipped` carries the count as
    /// `{count}`; native i18n has no interpolator, so it is substituted here.
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
                theme::dialog_scale(ui);
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
                    // Done or cancelled: a window that was only here for the
                    // task disappears. A real failure stays open so its dialog
                    // can be read (and retried).
                    if self.auto_close && (ok || cancelled) {
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
        // Whoever asked us to do something (Explorer, a shortcut, the command
        // line) is not here to browse: this window should not outlive the job.
        self.auto_close = true;
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
            }
        }
    }

    fn open_archive(&mut self, path: &str) {
        match self.svc.open_archive(path.to_string(), None) {
            Ok(res) => {
                let id = res.id;
                self.archive = Some(res);
                self.archive_path = path.to_string();
                self.cwd = String::new();
                self.rows = self.svc.list_children(id, String::new()).unwrap_or_default();
                self.selected.clear();
                self.preview = PreviewKind::None;
                self.error = None;
            }
            Err(e) => self.error = Some(self.t(&e.key)),
        }
    }

    fn navigate(&mut self, dir: &str) {
        if let Some(a) = &self.archive {
            self.cwd = dir.to_string();
            self.rows = self.svc.list_children(a.id, dir.to_string()).unwrap_or_default();
            self.selected.clear();
            self.preview = PreviewKind::None;
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

    fn selected_one(&self) -> Option<&EntryDto> {
        if self.selected.len() == 1 {
            self.rows.iter().find(|e| self.selected.contains(&e.path))
        } else {
            None
        }
    }

    fn preview_selected(&mut self) {
        let id = self.archive.as_ref().map(|a| a.id).unwrap_or(0);
        let Some(entry) = self.selected_one().cloned() else {
            self.preview = PreviewKind::None;
            self.preview_truncated = false;
            return;
        };
        if entry.is_dir {
            self.preview = PreviewKind::None;
            self.preview_truncated = false;
            return;
        }
        let max_bytes = self.settings.preview_max_bytes.max(1);
        let read = self.svc.read_entry_bytes(id, entry.path.clone(), max_bytes);
        // The reader stops at the cap; say so instead of presenting the head of
        // an entry as if it were the whole thing.
        self.preview_truncated = read
            .as_ref()
            .map(|b| entry.size > b.len() as u64)
            .unwrap_or(false);
        let bytes = read.unwrap_or_default();
        let name_lower = entry.name.to_lowercase();
        if name_lower.ends_with(".png") || name_lower.ends_with(".jpg") || name_lower.ends_with(".jpeg") {
            if let Some(img) = i18n::load_image(&bytes) {
                let tex = self
                    .ctx
                    .load_texture("preview-img", img, egui::TextureOptions::LINEAR);
                self.preview = PreviewKind::Image(tex);
                return;
            }
            self.preview = PreviewKind::Hex(format_preview_hex(&bytes));
            return;
        }
        if bytes.contains(&0) {
            self.preview = PreviewKind::Hex(format_preview_hex(&bytes));
        } else {
            self.preview = PreviewKind::Text(String::from_utf8_lossy(&bytes).into_owned());
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

    fn browser(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            let full_w = ui.available_width();
            let size_x = (full_w - Self::COL_ENC - Self::COL_MTIME - Self::COL_SIZE).max(140.0);
            let mtime_x = size_x + Self::COL_SIZE;
            let enc_x = mtime_x + Self::COL_MTIME;
            let weak = ui.visuals().weak_text_color();
            let text_color = ui.visuals().text_color();
            let hairline = ui.visuals().widgets.inactive.bg_stroke.color;

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
                        paint_entry_icon(&painter, icon_rect, e, dark);

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
                        self.selected.clear();
                        self.selected.insert(f.clone());
                        self.preview_for = f;
                        self.preview_selected();
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
    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t("preview.title"));
        match &self.preview {
            PreviewKind::Text(s) => {
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.monospace(s);
                });
            }
            PreviewKind::Hex(h) => {
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.monospace(h);
                });
            }
            PreviewKind::Image(tex) => {
                let tex_handle = tex.clone();
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.image((tex_handle.id(), egui::Vec2::new(280.0, 280.0)));
                });
            }
            PreviewKind::None => {
                ui.weak(self.t("preview.no_preview"));
            }
        }
        if self.preview_truncated && !matches!(self.preview, PreviewKind::None) {
            ui.weak(self.t("preview.truncated"));
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
                            spawn_update_check(Arc::clone(&self.update));
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
            theme::dialog_scale(ui);
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
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("All", &["*"])
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
            }
            if ui.button(self.t("create.add_folder")).clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    if let Some(s) = dir.to_str() {
                        if !self.create_sources.contains(&s.to_string()) {
                            self.create_sources.push(s.to_string());
                        }
                    }
                }
            }
            for s in &self.create_sources.clone() {
                ui.label(format!("• {s}"));
            }
            ui.separator();
            ui.label(self.t("create.dest"));
            ui.text_edit_singleline(&mut self.create_dest);
            if ui.button(self.t("extract.browse")).clicked() {
                // The Save-As dialog used to open with an empty file name and an
                // "All files" filter, so 保存 did nothing until the user typed a
                // name themselves — which reads as "browse cannot select
                // anything". Pre-fill a sensible name and match the format.
                let format = self.create_format.clone();
                let (desc, exts) = format_filter(&format);
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
            let can = !self.create_sources.is_empty() && !self.create_dest.trim().is_empty();
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
                theme::dialog_scale(ui);
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

                        theme::section(ui, &self.t("settings.section.about"));
                        let update_label = self.t("settings.auto_update");
                        ui.checkbox(&mut self.settings.auto_check_update, update_label);
                        ui.horizontal(|ui| {
                            ui.label(self.t("settings.version"));
                            ui.label(egui::RichText::new(format!("v{CURRENT_VERSION}")).strong());
                        });
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
            ) {
                Ok(r) => {
                    // Reflect what the OS actually accepted: a hardened
                    // machine can deny one menu target and allow the
                    // others, so the checkboxes must not claim more than
                    // the registry holds.
                    self.settings.associate = r.associate;
                    self.settings.context_menu = r.context_menu;
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
                        theme::dialog_scale(ui);
                        ui.label(found);
                        ui.label(hint);
                        ui.horizontal(|ui| {
                            if ui.button(dl).clicked() {
                                open_in_browser(&url);
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
                theme::dialog_scale(ui);
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

fn format_preview_hex(bytes: &[u8]) -> String {
    let cap = bytes.len().min(4096);
    let mut s = String::new();
    for (i, chunk) in bytes[..cap].chunks(16).enumerate() {
        s.push_str(&format!("{i:04x}  "));
        for b in chunk {
            s.push_str(&format!("{b:02x} "));
        }
        s.push('\n');
    }
    if bytes.len() > cap {
        s.push_str(&format!("… {} more bytes", bytes.len() - cap));
    }
    s
}

/// rfd filter for a create format: `(description, extensions)`.
fn format_filter(format: &str) -> (&'static str, &'static [&'static str]) {
    match format {
        "7z" => ("7z archive", &["7z"]),
        "tar" => ("TAR archive", &["tar"]),
        "tar.gz" => ("TAR.GZ archive", &["gz"]),
        "tar.bz2" => ("TAR.BZ2 archive", &["bz2"]),
        "tar.xz" => ("TAR.XZ archive", &["xz"]),
        _ => ("ZIP archive", &["zip"]),
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

        // Remember the window geometry so the next launch opens where the user
        // left it. Debounced: dragging fires this every frame, and a maximised
        // window keeps the last normal size instead of overwriting it.
        let viewport = ctx.input(|i| i.viewport().clone());
        if viewport.maximized != Some(true) {
            if let Some(rect) = viewport.inner_rect {
                let ppp = ctx.pixels_per_point();
                let (size, fallback_pos) = physical_geometry(rect, rect.min, ppp);
                let pos = viewport
                    .outer_rect
                    .map(|r| physical_geometry(rect, r.min, ppp).1)
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
            }
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
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| theme::hint(ui, &self.t("browser.empty_dir")));
                });
            } else {
                self.browser(ui);
            }
        });

        if self.open_picker {
            self.open_picker = false;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Archives", &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"])
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

/// The pixels-per-point the app actually runs at: monitor DPI scale times the
/// saved UI zoom. Window creation happens before there is a context to ask, so
/// this mirrors what `set_zoom_factor` applies a moment later. It has to match
/// `ctx.pixels_per_point()` on the saving side, otherwise the stored physical
/// pixels are scaled by the wrong factor every time the app starts.
fn effective_pixels_per_point(dpi_scale: f32, ui_zoom_percent: u32) -> f32 {
    let dpi = if dpi_scale.is_finite() && dpi_scale > 0.0 {
        dpi_scale
    } else {
        1.0
    };
    dpi * (ui_zoom_percent.clamp(100, 200) as f32 / 100.0)
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
mod geometry_tests {
    use super::{effective_pixels_per_point, physical_geometry, physical_to_logical};

    #[test]
    fn the_effective_scale_includes_the_ui_zoom() {
        // 100% DPI with 125% UI zoom is the setup the window geometry used to
        // round-trip wrongly: saved at 1.25, restored at 1.0.
        assert_eq!(effective_pixels_per_point(1.0, 125), 1.25);
        assert_eq!(effective_pixels_per_point(1.5, 100), 1.5);
        assert_eq!(effective_pixels_per_point(1.5, 200), 3.0);
        // Nonsense values fall back instead of poisoning the window size.
        assert_eq!(effective_pixels_per_point(0.0, 125), 1.25);
        assert_eq!(effective_pixels_per_point(f32::NAN, 125), 1.25);
        assert_eq!(effective_pixels_per_point(1.0, 0), 1.0);
        assert_eq!(effective_pixels_per_point(1.0, 9999), 2.0);
    }

    #[test]
    fn a_stored_geometry_survives_a_round_trip() {
        // Save: points -> physical at 125% zoom.
        let inner = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(712.0, 608.0));
        let (size, pos) = physical_geometry(inner, egui::pos2(1177.6, 265.6), 1.25);
        assert_eq!(size, (890, 760));
        assert_eq!(pos, (1472, 332));
        // Restore: physical -> points must give the original window back.
        let ppp = effective_pixels_per_point(1.0, 125);
        assert_eq!(physical_to_logical(size.0 as f32, ppp), 712.0);
        assert_eq!(physical_to_logical(size.1 as f32, ppp), 608.0);
        assert_eq!(physical_to_logical(pos.0 as f32, ppp), 1177.6);
        assert_eq!(physical_to_logical(pos.1 as f32, ppp), 265.6);
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
    use super::{retarget_archive_ext, suggested_archive_name};

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