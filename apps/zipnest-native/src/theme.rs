//! Win11 / Fluent-style theme for the egui shell: system fonts, light
//! surfaces, rounded corners, subtle borders, a blue accent.

use eframe::egui;
use egui::{Margin, Rounding, Stroke};

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |path: &str, family: egui::FontFamily| {
        if let Ok(bytes) = std::fs::read(path) {
            let key = format!("sys-{}", std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy());
            fonts.font_data.insert(key.clone(), egui::FontData::from_owned(bytes));
            fonts.families.entry(family).or_default().push(key);
        }
    };
    // Latin: Segoe UI (Win11 default). CJK: Microsoft YaHei fallback.
    // Deliberately NO color/emoji fonts — egui's glyph engine cannot parse
    // them and would render every glyph as a box.
    add("C:\\Windows\\Fonts\\segoeui.ttf", egui::FontFamily::Proportional);
    add("C:\\Windows\\Fonts\\msyh.ttc", egui::FontFamily::Proportional);
    add("C:\\Windows\\Fonts\\consola.ttf", egui::FontFamily::Monospace);
    add("C:\\Windows\\Fonts\\msyh.ttc", egui::FontFamily::Monospace);
    // Move our system fonts ahead of the built-ins so they win.
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&fam) {
            let sys: Vec<String> = list.iter().filter(|k| k.starts_with("sys-")).cloned().collect();
            let rest: Vec<String> = list.iter().filter(|k| !k.starts_with("sys-")).cloned().collect();
            *list = sys.into_iter().chain(rest).collect();
        }
    }
    ctx.set_fonts(fonts);
}

fn visual() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = egui::Color32::from_rgb(250, 250, 250); // app canvas
    v.window_fill = egui::Color32::from_rgb(255, 255, 255);
    v.window_stroke = Stroke::new(1.0, egui::Color32::from_rgb(230, 230, 230));
    v.window_rounding = Rounding::same(10.0);
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: egui::Color32::from_black_alpha(40),
    };
    v.faint_bg_color = egui::Color32::from_rgb(245, 245, 245);
    v.extreme_bg_color = egui::Color32::from_rgb(255, 255, 255);
    v.selection.bg_fill = egui::Color32::from_rgb(205, 229, 255);
    v.selection.stroke = Stroke::new(1.0, egui::Color32::from_rgb(0, 103, 192));
    v.hyperlink_color = egui::Color32::from_rgb(0, 103, 192);
    v
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = visual();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.window_margin = Margin::same(14.0);
    style.spacing.menu_margin = Margin::same(6.0);
    style.visuals.widgets.inactive.rounding = Rounding::same(6.0);
    style.visuals.widgets.hovered.rounding = Rounding::same(6.0);
    style.visuals.widgets.active.rounding = Rounding::same(6.0);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, egui::Color32::from_rgb(222, 222, 222));
    style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(238, 242, 248);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, egui::Color32::from_rgb(0, 103, 192));
    ctx.set_style(style);
}

/// Primary/accent button (filled blue, white text) for the toolbar.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE))
            .fill(egui::Color32::from_rgb(0, 103, 192))
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::NONE),
    )
}

/// Subtle ghost button (no fill until hover) for secondary actions.
pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let fill = if ui.rect_contains_pointer(ui.max_rect()) {
        egui::Color32::from_rgb(238, 242, 248)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.add(
        egui::Button::new(text)
            .fill(fill)
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::NONE),
    )
}

/// A toolbar row drawn as a white "card" strip with a hairline bottom border.
pub fn top_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(255, 255, 255))
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .rounding(Rounding::ZERO)
        .stroke(Stroke::NONE)
        .show(ui, add_contents);
}

/// Bottom status bar strip.
pub fn status_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(245, 246, 248))
        .inner_margin(Margin::symmetric(14.0, 6.0))
        .stroke(Stroke::new(1.0, egui::Color32::from_rgb(232, 232, 232)))
        .show(ui, add_contents);
}

/// Enlarge text + spacing inside a dialog so it is easier to read.
pub fn dialog_scale(ui: &mut egui::Ui) {
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
    ui.style_mut().spacing.item_spacing = egui::vec2(10.0, 10.0);
    ui.style_mut().spacing.button_padding = egui::vec2(14.0, 8.0);
    ui.style_mut().spacing.interact_size.y = 26.0;
}
