//! ZipNest native UI (egui). Same-process calls into `zipnest-ipc`; no webview.

mod i18n;

use eframe::egui;
use std::sync::{Arc, Mutex};
use zipnest_ipc::{CreateRequest, EntryDto, IpcService, SettingsPatch, ShellOptions};

const DESIGN_WIDTH: f32 = 900.0;

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
    title: String,
}

impl JobState {
    fn pct(&self) -> f32 {
        if self.total_items == 0 {
            return 0.0;
        }
        (self.done_items as f32 / self.total_items as f32).clamp(0.0, 1.0)
    }
}

#[derive(Clone)]
enum PreviewKind {
    Text(String),
    Hex(String),
    Image(egui::TextureHandle),
    None,
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([DESIGN_WIDTH, 620.0])
            .with_title("ZipNest"),
        ..Default::default()
    };
    eframe::run_native(
        "ZipNest",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    ctx: egui::Context,
    svc: Arc<IpcService>,
    settings: zipnest_ipc::Settings,
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
    // dialogs
    show_extract: bool,
    extract_dest: String,
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
    // assets
    qr_wechat: Option<egui::TextureHandle>,
    qr_alipay: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        i18n::install_fonts(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let jobs: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Default::default();
        let emit_jobs = Arc::clone(&jobs);
        let svc = Arc::new(IpcService::new(
            Arc::new(move |name: &str, payload: serde_json::Value| {
                emit_jobs.lock().unwrap().push((name.to_string(), payload));
            }),
            std::time::Duration::from_millis(100),
        ));
        let settings = svc.settings_get().unwrap_or_default();
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
        Self {
            ctx,
            svc,
            settings,
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
            show_extract: false,
            extract_dest: String::new(),
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
            qr_wechat,
            qr_alipay,
        }
    }

    fn t(&self, key: &str) -> String {
        i18n::tr(&self.lang, key)
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
                "job_finished" => {
                    let ok = payload["ok"].as_bool().unwrap_or(false);
                    let err_key = payload["error_key"]
                        .as_str()
                        .map(|s| s.to_string());
                    let j = self.job.get_or_insert_with(JobState::default);
                    j.ok = ok;
                    j.finished = true;
                    self.notice = if ok {
                        Some("job.ok".into())
                    } else {
                        err_key.map(|k| if k.starts_with("error.") { k } else { "error.engine".into() })
                    };
                }
                _ => {}
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
            return;
        };
        if entry.is_dir {
            self.preview = PreviewKind::None;
            return;
        }
        let max_bytes = self.settings.preview_max_bytes.max(1);
        let bytes = self
            .svc
            .read_entry_bytes(id, entry.path.clone(), max_bytes)
            .unwrap_or_default();
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
        ui.horizontal(|ui| {
            if ui.button(self.t("app.open")).clicked() {
                self.open_picker = true;
            }
            if ui.button(self.t("create.new")).clicked() {
                self.show_create = true;
                self.create_sources.clear();
                self.create_dest = self.current_archive_dir();
            }
            let extract_enabled = self.archive.is_some() && !self.selected.is_empty();
            if ui.add_enabled(extract_enabled, egui::Button::new(self.t("extract.start"))).clicked() {
                self.extract_dest = self.current_archive_dir();
                self.show_extract = true;
            }
            ui.separator();
            if let Some(a) = &self.archive {
                ui.label(a.format.clone());
            } else {
                ui.label(self.t("browser.empty"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.t("settings.donate")).clicked() {
                    self.show_donate = true;
                }
                if ui.button(if self.lang == "zh-CN" { "EN" } else { "中文" }).clicked() {
                    self.lang = if self.lang == "zh-CN" { "en-US" } else { "zh-CN" }.into();
                    self.settings.language = self.lang.clone();
                    let _ = self.svc.settings_set(SettingsPatch {
                        language: Some(self.lang.clone()),
                        ..Default::default()
                    });
                }
                if ui.button(self.t("settings.title")).clicked() {
                    self.show_settings = true;
                }
            });
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(self.t("browser.columns.name"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(self.t("browser.columns.encrypted"));
                ui.label(self.t("browser.columns.mtime"));
                ui.label(self.t("browser.columns.size"));
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut click_dir: Option<String> = None;
            let mut dbl: Option<String> = None;
            let mut click_file: Option<String> = None;
            let rows = self.rows.clone();
            for e in &rows {
                let selected = self.selected.contains(&e.path);
                let mut text = format!("{} {}", if e.is_dir { "📁" } else { "📄" }, e.name);
                let size = if e.is_dir {
                    String::new()
                } else {
                    format!("{:>10}", pretty_size(e.size))
                };
                let enc = if e.encrypted { "🔒" } else { "" };
                text = format!("{text}{size:>12}  {enc}");
                let resp = ui.selectable_label(selected, text);
                if resp.double_clicked() && e.is_dir {
                    dbl = Some(e.path.clone());
                } else if resp.clicked() {
                    if e.is_dir {
                        click_dir = Some(e.path.clone());
                    } else {
                        click_file = Some(e.path.clone());
                    }
                }
                ui.end_row();
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
    }

    fn statusbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                self.notice
                    .as_ref()
                    .map(|n| self.t(n))
                    .unwrap_or_else(|| {
                        if self.archive.is_some() {
                            format!("{}", self.rows.len())
                        } else {
                            String::new()
                        }
                    }),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.t("settings.donate")).clicked() {
                    self.show_donate = true;
                }
            });
        });
    }

    fn extract_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_extract;
        let arch_id = self.archive.as_ref().map(|a| a.id).unwrap_or(0);
        if self.archive.is_none() {
            self.show_extract = false;
            return;
        }
        let srcs = self.selected.clone();
        let svc = self.svc.clone();
        let t = self.t("extract.title");
        egui::Window::new(t).collapsible(false).open(&mut open).show(ctx, |ui| {
            ui.label(self.t("extract.dest"));
            ui.text_edit_singleline(&mut self.extract_dest);
            if ui.button(self.t("extract.browse")).clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.extract_dest = dir.to_str().unwrap_or("").to_string();
                }
            }
            let overwrite_label = self.t("extract.overwrite");
            ui.checkbox(&mut self.overwrite, overwrite_label);
            let can = !self.extract_dest.trim().is_empty();
            if ui.add_enabled(can, egui::Button::new(self.t("extract.start"))).clicked() {
                let dest = self.extract_dest.clone();
                let overwrite = self.overwrite;
                self.job = Some(JobState {
                    job_id: 0,
                    title: self.t("extract.title"),
                    ..Default::default()
                });
                let _ = svc.extract(
                    arch_id,
                    srcs.iter().cloned().collect(),
                    dest,
                    overwrite,
                    None,
                );
                self.show_extract = false;
            }
            ui.separator();
            if ui.button(self.t("extract.cancel")).clicked() {
                self.show_extract = false;
            }
        });
        self.show_extract = open && self.job.is_none();
    }

    fn create_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_create;
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
                if let Some(p) = rfd::FileDialog::new().save_file() {
                    self.create_dest = p.to_str().unwrap_or("").to_string();
                }
            }
            ui.horizontal(|ui| {
                ui.label(self.t("create.format"));
                for f in ["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
                    let sel = self.create_format == f;
                    if ui.selectable_label(sel, f).clicked() {
                        self.create_format = f.to_string();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(self.t("create.level"));
                for l in ["store", "fastest", "normal", "maximum", "ultra"] {
                    let sel = self.create_level == l;
                    if ui.selectable_label(sel, l).clicked() {
                        self.create_level = l.to_string();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(self.t("create.method"));
                for m in ["auto", "copy", "deflate", "lzma2", "bzip2"] {
                    let sel = self.create_method == m;
                    if ui.selectable_label(sel, m).clicked() {
                        self.create_method = m.to_string();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(self.t("create.password"));
                ui.text_edit_singleline(&mut self.create_password);
            });
            if self.create_format == "7z" {
                let lbl = self.t("create.encrypt_names");
                ui.checkbox(&mut self.create_encrypt_names, lbl);
            }
            ui.horizontal(|ui| {
                ui.label(self.t("create.volume"));
                for v in ["off", "10m", "100m", "1g", "custom"] {
                    let sel = self.create_volume == v;
                    if ui.selectable_label(sel, v).clicked() {
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
            if ui.add_enabled(can, egui::Button::new(self.t("create.start"))).clicked() {
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
                let _ = svc.create_archive(
                    self.create_sources.clone(),
                    self.create_dest.clone(),
                    req,
                );
                self.show_create = false;
            }
            ui.separator();
            if ui.button(self.t("extract.cancel")).clicked() {
                self.show_create = false;
            }
        });
        self.show_create = open && self.job.is_none();
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let svc = self.svc.clone();
        egui::Window::new(self.t("settings.title"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t("settings.language"));
                ui.horizontal(|ui| {
                    for (v, k) in [("system", "settings.language.system"), ("zh-CN", "settings.language.zh"), ("en-US", "settings.language.en")] {
                        let sel = self.settings.language == v;
                        if ui.selectable_label(sel, self.t(k)).clicked() {
                            self.settings.language = v.to_string();
                        }
                    }
                });
                ui.label(self.t("settings.default_dir"));
                ui.text_edit_singleline(&mut self.settings.default_extract_dir);
                if ui.button(self.t("extract.browse")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.settings.default_extract_dir = dir.to_str().unwrap_or("").to_string();
                    }
                }
                ui.label(self.t("settings.overwrite"));
                ui.horizontal(|ui| {
                    for (v, k) in [("ask", "settings.overwrite.ask"), ("overwrite", "settings.overwrite.overwrite"), ("skip", "settings.overwrite.skip"), ("rename", "settings.overwrite.rename")] {
                        let sel = self.settings.overwrite_policy == v;
                        if ui.selectable_label(sel, self.t(k)).clicked() {
                            self.settings.overwrite_policy = v.to_string();
                        }
                    }
                });
                ui.label(self.t("settings.shell.associate"));
                ui.checkbox(&mut self.settings.associate, "");
                ui.label(self.t("settings.shell.context_menu"));
                ui.checkbox(&mut self.settings.context_menu, "");
                ui.separator();
                let can = ui.button(self.t("settings.save")).clicked();
                if can {
                    let assoc = self.settings.associate;
                    let menu = self.settings.context_menu;
                    let exe = std::env::current_exe().unwrap_or_default();
                    let _ = svc.shell_register(&exe, ShellOptions { associate: assoc, context_menu: menu }, &zipnest_ipc::shell::WindowsRegistry);
                    let _ = svc.settings_set(SettingsPatch {
                        language: Some(self.settings.language.clone()),
                        default_extract_dir: Some(self.settings.default_extract_dir.clone()),
                        overwrite_policy: Some(self.settings.overwrite_policy.clone()),
                        preview_max_bytes: Some(self.settings.preview_max_bytes),
                        ..Default::default()
                    });
                    if self.settings.language != "system" {
                        self.lang = self.settings.language.clone();
                    }
                    self.show_settings = false;
                }
                if ui.button(self.t("extract.cancel")).clicked() {
                    self.show_settings = false;
                }
            });
        self.show_settings = open;
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

    fn error_window(&mut self, ctx: &egui::Context) {
        if let Some(err) = self.error.clone() {
            let mut open = true;
            egui::Window::new(self.t("error.title"))
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.colored_label(egui::Color32::from_rgb(200, 60, 40), err);
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
        if let Some(j) = &self.job {
            if j.finished {
                self.job = None;
                return;
            }
            let mut cancel = false;
            egui::Window::new(j.title.clone())
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.add(egui::ProgressBar::new(j.pct()).show_percentage());
                    let speed = format!("{} / {}", j.done_items, j.total_items);
                    ui.label(speed);
                    if ui.button(self.t("job.canceled")).clicked() {
                        cancel = true;
                    }
                });
            if cancel {
                if let Some(active) = self.job.clone() {
                    if active.job_id != 0 {
                        let _ = self.svc.cancel(active.job_id);
                    }
                }
                self.job = None;
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
        let scale = (ctx.screen_rect().width() / DESIGN_WIDTH).clamp(1.0, 1.7);
        ctx.set_zoom_factor(scale);
        self.poll_jobs();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            self.statusbar(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            // Two-pane: browser + preview
            egui::SidePanel::right("preview")
                .default_width(300.0)
                .resizable(true)
                .show_inside(ui, |ui| {
                    self.preview_panel(ui);
                });
            self.browser(ui);
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
        self.job_window(ctx);
        self.error_window(ctx);
    }
}