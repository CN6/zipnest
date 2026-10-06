//! Win11 / Fluent-style theme for the egui shell: system fonts, rounded
//! corners, subtle borders, a blue accent — in light **or** dark.
//!
//! Colours are expressed through egui's own [`egui::Visuals`] so the whole UI
//! can follow the system theme: UI code asks `ui.visuals()` for a surface or a
//! text colour instead of hard-coding one. Only colours that must look the same
//! in both themes stay literal — the green "done" label, the red error text,
//! the amber folder and the file-type accents.

use eframe::egui;
use egui::{Color32, Margin, Rounding, Stroke};

/// Accent used for primary actions and links (Windows 11 blue).
const ACCENT_LIGHT: Color32 = Color32::from_rgb(0, 103, 192);
const ACCENT_DARK: Color32 = Color32::from_rgb(88, 166, 255);

/// Which palette the app paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

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

impl Mode {
    pub fn is_dark(self) -> bool {
        self == Mode::Dark
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn visuals(mode: Mode) -> egui::Visuals {
    let dark = mode.is_dark();
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let accent = if dark { ACCENT_DARK } else { ACCENT_LIGHT };

    v.panel_fill = if dark { rgb(32, 32, 32) } else { rgb(250, 250, 250) };
    v.window_fill = if dark { rgb(43, 43, 43) } else { rgb(255, 255, 255) };
    v.window_stroke = Stroke::new(1.0, if dark { rgb(62, 62, 62) } else { rgb(230, 230, 230) });
    v.window_rounding = Rounding::same(10.0);
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 120 } else { 40 }),
    };
    v.faint_bg_color = if dark { rgb(45, 45, 45) } else { rgb(245, 245, 245) };
    v.extreme_bg_color = if dark { rgb(26, 26, 26) } else { rgb(255, 255, 255) };
    v.selection.bg_fill = if dark { rgb(38, 79, 120) } else { rgb(205, 229, 255) };
    v.selection.stroke = Stroke::new(1.0, accent);
    v.hyperlink_color = accent;
    v
}

/// Apply the stored `"system" | "light" | "dark"` preference.
///
/// Both theme slots are filled with our palette, so whichever one egui selects
/// is ours. egui keeps one `Style` per theme and picks between them with the
/// preference, while eframe feeds it the system theme on every frame — writing
/// only "the current style" is therefore bypassed as soon as the preference
/// resolves to the other slot, which is exactly how a dark palette once ended
/// up on a light window.
pub fn apply_setting(ctx: &egui::Context, setting: &str) {
    fill(ctx, Mode::Light);
    fill(ctx, Mode::Dark);
    let system = if system_prefers_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    let (preference, fallback) = match setting {
        "dark" => (egui::ThemePreference::Dark, egui::Theme::Dark),
        "light" => (egui::ThemePreference::Light, egui::Theme::Light),
        // Follow Windows live; our own detection is the fallback for the rare
        // case where the platform reports no theme at all.
        _ => (egui::ThemePreference::System, system),
    };
    ctx.options_mut(|options| {
        options.theme_preference = preference;
        options.fallback_theme = fallback;
    });
}

/// Write our palette into one theme's style slot.
fn fill(ctx: &egui::Context, mode: Mode) {
    let theme = match mode {
        Mode::Dark => egui::Theme::Dark,
        Mode::Light => egui::Theme::Light,
    };
    let mut style = (*ctx.style_of(theme)).clone();
    style.visuals = visuals(mode);
    tune(&mut style, mode);
    ctx.set_style_of(theme, style);
}

/// Spacing and widget shapes shared by both palettes.
fn tune(style: &mut egui::Style, mode: Mode) {
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    // Show tooltips at once: the default half-second delay made the "?" hints
    // look like they did nothing at all when the pointer was only passing over.
    style.interaction.tooltip_delay = 0.0;
    style.spacing.window_margin = Margin::same(14.0);
    style.spacing.menu_margin = Margin::same(6.0);
    style.visuals.widgets.inactive.rounding = Rounding::same(6.0);
    style.visuals.widgets.hovered.rounding = Rounding::same(6.0);
    style.visuals.widgets.active.rounding = Rounding::same(6.0);
    let dark = mode.is_dark();
    style.visuals.widgets.inactive.bg_stroke =
        Stroke::new(1.0, if dark { rgb(78, 78, 78) } else { rgb(222, 222, 222) });
    style.visuals.widgets.hovered.bg_fill =
        if dark { rgb(58, 58, 58) } else { rgb(238, 242, 248) };
    style.visuals.widgets.hovered.bg_stroke =
        Stroke::new(1.0, if dark { ACCENT_DARK } else { ACCENT_LIGHT });
    style.visuals.widgets.inactive.weak_bg_fill =
        if dark { rgb(48, 48, 48) } else { rgb(255, 255, 255) };
    style.visuals.widgets.hovered.weak_bg_fill =
        if dark { rgb(60, 60, 60) } else { rgb(238, 242, 248) };
}

/// Primary/accent button (filled accent, white text) for the toolbar.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let accent = ui.visuals().hyperlink_color;
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE))
            .fill(accent)
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::NONE),
    )
}

/// Subtle ghost button (no fill until hovered) for secondary actions.
///
/// The hover fill comes from the theme instead of being measured from the whole
/// available rect, so hovering one button no longer lights up the whole row.
pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        ui.add(egui::Button::new(text).frame(false))
    })
    .inner
}

/// A toolbar row drawn as a card strip matching the current theme.
pub fn top_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .rounding(Rounding::ZERO)
        .stroke(Stroke::NONE)
        .show(ui, add_contents);
}

/// Bottom status bar strip.
pub fn status_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().faint_bg_color;
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(14.0, 6.0))
        .stroke(Stroke::new(1.0, stroke))
        .show(ui, add_contents);
}

/// A free-floating card surface (browser table, empty state, drop zone).
pub fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(8.0, 6.0))
        .rounding(Rounding::same(8.0))
        .stroke(Stroke::new(1.0, stroke))
        .show(ui, add_contents);
}


/// A small section heading used inside dialogs, with a hairline under it.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(title)
            .size(12.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

/// Dimmed helper text (hints, footnotes).
pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
    );
}

// ---------------------------------------------------------------------------
// System theme preference
// ---------------------------------------------------------------------------

/// Windows' "apps use light theme" flag. Missing value => light, which is what
/// Windows itself assumes for a fresh profile.
#[cfg(windows)]
fn apps_use_light_theme() -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: isize = 0x8000_0001;
    const KEY_READ: u32 = 0x2_0019;
    const REG_DWORD: u32 = 4;
    const ERROR_SUCCESS: i32 = 0;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            hkey: isize,
            subkey: *const u16,
            options: u32,
            sam: u32,
            result: *mut isize,
        ) -> i32;
        fn RegQueryValueExW(
            hkey: isize,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: isize) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let mut hkey = 0isize;
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let name = wide("AppsUseLightTheme");
    let mut kind = 0u32;
    let mut data = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            &mut data as *mut u32 as *mut u8,
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if rc != ERROR_SUCCESS || kind != REG_DWORD {
        return None;
    }
    Some(data)
}

#[cfg(not(windows))]
fn apps_use_light_theme() -> Option<u32> {
    None
}

/// Does the system prefer a dark UI?
///
/// `ZIPNEST_THEME=dark|light` overrides the detection — used for screenshots
/// and by anyone who wants a fixed look while testing.
pub fn system_prefers_dark() -> bool {
    match std::env::var("ZIPNEST_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    apps_use_light_theme() == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_have_distinct_surfaces() {
        let light = visuals(Mode::Light);
        let dark = visuals(Mode::Dark);
        assert!(!light.dark_mode && dark.dark_mode);
        assert_ne!(light.window_fill, dark.window_fill);
        assert_ne!(light.panel_fill, dark.panel_fill);
        // The border must stay visible against its own surface: brighter than
        // the fill when light, lighter grey than the fill when dark.
        assert!(light.window_fill.r() > light.window_stroke.color.r());
        assert!(dark.window_fill.r() < dark.window_stroke.color.r());
        // Text and canvas must contrast with the surface they sit on.
        assert!(light.panel_fill.r() > light.window_stroke.color.r());
        assert!(dark.panel_fill.r() < dark.window_fill.r());
    }
}
