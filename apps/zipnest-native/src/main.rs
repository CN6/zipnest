//! ZipNest native UI (egui). Same-process calls into `zipnest-ipc`; no webview.

mod i18n;

use eframe::egui;
use std::sync::Arc;
use zipnest_ipc::{EntryDto, IpcService, OpenArchiveResult, Settings};

const DESIGN_WIDTH: f32 = 900.0;

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
    svc: Arc<IpcService>,
    settings: Settings,
    archive: Option<OpenArchiveResult>,
    archive_path: String,
    cwd: String,
    rows: Vec<EntryDto>,
    selected: std::collections::HashSet<String>,
    error: Option<String>,
    notice: Option<String>,
    lang: String, // "zh-CN" | "en-US"
    open_picker: bool,
    // dialogs
    show_extract: bool,
    extract_dest: String,
    overwrite: bool,
    show_create: bool,
    show_settings: bool,
    show_donate: bool,
    // assets
    qr_wechat: Option<egui::TextureHandle>,
    qr_alipay: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Default to Chinese; settings will override after load.
        i18n::install_fonts(&cc.egui_ctx);
        let svc = Arc::new(IpcService::new(
            Arc::new(|_name, _payload| {}),
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
            show_extract: false,
            extract_dest: String::new(),
            overwrite: true,
            show_create: false,
            show_settings: false,
            show_donate: false,
            qr_wechat,
            qr_alipay,
        }
    }

    fn t(&self, key: &str) -> String {
        i18n::tr(&self.lang, key)
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
        }
    }

    fn current_archive_dir(&self) -> String {
        let p = self.archive_path.replace('\\', "/");
        match p.rfind('/') {
            Some(i) => p[..i].to_string(),
            None => String::new(),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(self.t("app.open")).clicked() {
                self.open_picker = true;
            }
            if ui.button(self.t("create.new")).clicked() {
                self.show_create = true;
            }
            let extract_enabled = self.archive.is_some() && !self.selected.is_empty();
            if ui.add_enabled(extract_enabled, egui::Button::new(self.t("extract.start"))).clicked() {
                self.extract_dest = self.current_archive_dir();
                self.show_extract = true;
            }
            ui.separator();
            if let Some(a) = &self.archive {
                ui.label(format!("{}", a.format));
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
                    let _ = self.svc.settings_set(zipnest_ipc::SettingsPatch {
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
            for e in &self.rows {
                let selected = self.selected.contains(&e.path);
                let text = format!("{} {}", if e.is_dir { "📁" } else { "📄" }, e.name);
                let resp = ui.selectable_label(selected, text);
                if resp.double_clicked() && e.is_dir {
                    dbl = Some(e.path.clone());
                } else if resp.clicked() {
                    if e.is_dir {
                        click_dir = Some(e.path.clone());
                    } else {
                        self.selected.clear();
                        self.selected.insert(e.path.clone());
                    }
                }
                ui.end_row();
            }
            if let Some(d) = dbl {
                self.navigate(&d);
            } else if let Some(d) = click_dir {
                self.navigate(&d);
            }
        });
    }

    fn statusbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(if let Some(n) = &self.notice {
                self.t(n)
            } else if self.archive.is_some() {
                format!("{}", self.rows.len())
            } else {
                String::new()
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.t("settings.donate")).clicked() {
                    self.show_donate = true;
                }
            });
        });
    }

    fn show_error(&mut self, ctx: &egui::Context) {
        if let Some(err) = self.error.clone() {
            egui::Window::new(self.t("error.title"))
                .collapsible(false)
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -20.0])
                .show(ctx, |ui| {
                    ui.colored_label(egui::Color32::from_rgb(200, 60, 40), err);
                    if ui.button(self.t("extract.cancel")).clicked() {
                        self.error = None;
                    }
                });
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // zoom to window width
        let scale = (ctx.screen_rect().width() / DESIGN_WIDTH).clamp(1.0, 1.7);
        ctx.set_zoom_factor(scale);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            self.statusbar(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.browser(ui);
        });

        // file open picker (native dialog)
        if self.open_picker {
            self.open_picker = false;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Archives", &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"])
                .pick_file()
            {
                self.open_archive(path.to_str().unwrap_or(""));
            }
        }

        // dialogs
        if self.show_extract {
            let mut open = true;
            let dest = self.extract_dest.clone();
            let selected = self.selected.clone();
            let svc = self.svc.clone();
            let t = self.t("extract.title");
            egui::Window::new(t)
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(self.t("extract.dest"));
                    ui.text_edit_singleline(&mut self.extract_dest);
                    if ui.button(self.t("extract.browse")).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.extract_dest = dir.to_str().unwrap_or("").to_string();
                        }
                    }
                    let overwrite_label = self.t("extract.overwrite");
                    ui.checkbox(&mut self.overwrite, overwrite_label);
                    let can_start = !self.extract_dest.trim().is_empty();
                    if ui.add_enabled(can_start, egui::Button::new(self.t("extract.start"))).clicked() {
                        let id = svc
                            .extract(
                                self.archive.as_ref().map(|a| a.id).unwrap_or(0),
                                selected.iter().cloned().collect(),
                                self.extract_dest.clone(),
                                self.overwrite,
                                None,
                            )
                            .unwrap_or(0);
                        self.notice = Some("extract.success".into());
                        self.show_extract = false;
                        let _ = id;
                    }
                    ui.separator();
                    if ui.button(self.t("extract.cancel")).clicked() {
                        self.show_extract = false;
                    }
                });
            if !open {
                self.show_extract = false;
            }
            let _ = dest;
        }
        if self.show_create {
            let mut open = true;
            egui::Window::new(self.t("create.title"))
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("TODO create");
                });
            if !open {
                self.show_create = false;
            }
        }
        if self.show_settings {
            let mut open = true;
            egui::Window::new(self.t("settings.title"))
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("TODO settings");
                });
            if !open {
                self.show_settings = false;
            }
        }
        if self.show_donate {
            let mut open = true;
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
            if !open {
                self.show_donate = false;
            }
        }

        self.show_error(ctx);
    }
}
