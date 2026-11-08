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
    use egui::FontFamily;

    let mut fonts = egui::FontDefinitions::default();
    // Each font file is read once and registered once, then referenced from
    // every family that wants it. `msyh.ttc` used to be read twice (once for
    // Proportional, once for Monospace): the key collided, so memory was not
    // duplicated, but the ~19 MB file was read from disk and dropped a second
    // time on every start.
    let mut add = |path: &str, families: &[FontFamily]| {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let file = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let key = format!("sys-{file}");
        fonts
            .font_data
            .insert(key.clone(), egui::FontData::from_owned(bytes));
        for family in families {
            let list = fonts.families.entry(family.clone()).or_default();
            if !list.contains(&key) {
                list.push(key.clone());
            }
        }
    };
    // Latin: Segoe UI (Win11 default). CJK: Microsoft YaHei fallback.
    // Deliberately NO color/emoji fonts — egui's glyph engine cannot parse
    // them and would render every glyph as a box.
    add(
        "C:\\Windows\\Fonts\\segoeui.ttf",
        &[FontFamily::Proportional],
    );
    add(
        "C:\\Windows\\Fonts\\msyh.ttc",
        &[FontFamily::Proportional, FontFamily::Monospace],
    );
    add("C:\\Windows\\Fonts\\consola.ttf", &[FontFamily::Monospace]);
    // Move our system fonts ahead of the built-ins so they win.
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&fam) {
            let sys: Vec<String> = list
                .iter()
                .filter(|k| k.starts_with("sys-"))
                .cloned()
                .collect();
            let rest: Vec<String> = list
                .iter()
                .filter(|k| !k.starts_with("sys-"))
                .cloned()
                .collect();
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

    v.panel_fill = if dark {
        rgb(32, 32, 32)
    } else {
        rgb(243, 243, 243)
    };
    v.window_fill = if dark {
        rgb(43, 43, 43)
    } else {
        rgb(255, 255, 255)
    };
    v.window_stroke = Stroke::new(
        1.0_f32,
        if dark {
            rgb(62, 62, 62)
        } else {
            rgb(230, 230, 230)
        },
    );
    v.window_rounding = Rounding::same(10.0);
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 120 } else { 40 }),
    };
    // Zebra stripes and the status strip. In light mode this has to be darker
    // than the white card it stripes: at 245 against a 255 card the rows were
    // only 4% apart and the alternation was invisible, which is most of why the
    // list looked washed out.
    v.faint_bg_color = if dark {
        rgb(37, 37, 37)
    } else {
        rgb(247, 247, 247)
    };
    v.extreme_bg_color = if dark {
        rgb(26, 26, 26)
    } else {
        rgb(255, 255, 255)
    };
    v.selection.bg_fill = if dark {
        rgb(38, 79, 120)
    } else {
        rgb(205, 229, 255)
    };
    v.selection.stroke = Stroke::new(1.0_f32, accent);
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
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(
        1.0_f32,
        if dark {
            rgb(78, 78, 78)
        } else {
            rgb(222, 222, 222)
        },
    );
    style.visuals.widgets.hovered.bg_fill = if dark {
        rgb(58, 58, 58)
    } else {
        rgb(238, 242, 248)
    };
    style.visuals.widgets.hovered.bg_stroke =
        Stroke::new(1.0_f32, if dark { ACCENT_DARK } else { ACCENT_LIGHT });
    style.visuals.widgets.inactive.weak_bg_fill = if dark {
        rgb(48, 48, 48)
    } else {
        rgb(255, 255, 255)
    };
    style.visuals.widgets.hovered.weak_bg_fill = if dark {
        rgb(60, 60, 60)
    } else {
        rgb(238, 242, 248)
    };
    // Default control height. egui sizes a button from its text plus
    // `button_padding`, which at 12.5-point text comes to 26 — every control in
    // the app was a different height from the 28-point text fields beside it.
    style.spacing.interact_size.y = CONTROL_H;
    style.spacing.button_padding.y = (CONTROL_H - 20.0) / 2.0;
}

/// Primary/accent button (filled accent, white text) for the toolbar.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    primary_button_enabled(ui, text, true)
}

/// The same button, but a disabled one actually looks disabled.
///
/// `ui.add_enabled(false, …)` does not grey this button, it *fades* it towards
/// the panel fill: an accent-filled control became a pale ghost of itself and
/// its label all but vanished, which read as "the main action is here, and it
/// did nothing when I clicked it". The fill is chosen from `enabled` instead,
/// and the sense is dropped to hover-only so a dead button neither highlights
/// nor reports a click.
pub fn primary_button_enabled(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (fill, ink) = if enabled {
        (ui.visuals().hyperlink_color, Color32::WHITE)
    } else if dark {
        (rgb(52, 52, 52), rgb(120, 120, 120))
    } else {
        (rgb(238, 238, 238), rgb(150, 150, 150))
    };
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(ink))
            .fill(fill)
            .rounding(Rounding::same(6.0))
            .stroke(Stroke::NONE)
            .sense(sense),
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

/// One option in a set of mutually exclusive choices — format, compression
/// level, theme, and so on.
///
/// Every one of these used to be an `egui::selectable_label`, which paints a
/// grey slab on *every* unselected option; five of them in a row turned the
/// wizard into a wall of boxes, and the selected one was only faintly bluer.
/// A chip is quiet until chosen and unmistakable when chosen.
pub fn chip(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let accent = ui.visuals().hyperlink_color;
    // The dark accent is a pale blue: white on it is unreadable, near-black is
    // not.
    let on_accent = if ui.visuals().dark_mode {
        rgb(20, 20, 20)
    } else {
        Color32::WHITE
    };
    let idle_fill = ui.visuals().widgets.inactive.weak_bg_fill;
    let idle_stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    let idle_text = ui.visuals().text_color();
    let (fill, text_color, stroke) = if selected {
        (accent, on_accent, Stroke::NONE)
    } else {
        (idle_fill, idle_text, Stroke::new(1.0_f32, idle_stroke))
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(12.5).color(text_color))
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(6.0))
            .min_size(egui::vec2(0.0, CONTROL_H)),
    )
}

/// A toolbar row drawn as a card strip matching the current theme.
pub fn top_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .rounding(Rounding::ZERO)
        .stroke(Stroke::NONE)
        .show(ui, |ui| {
            // A frame is only as wide as what it holds, so a toolbar whose
            // contents happened to be narrower than the window left a strip of
            // the panel's own fill to the right of it — visible as a toolbar
            // that stopped short of the window edge.
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
}

/// Bottom status bar strip.
///
/// No border: the `TopBottomPanel` it lives in already draws the hairline that
/// separates it from the list. The frame's own stroke drew the other three
/// sides too, which boxed the strip in and left a second line a few points from
/// the card's border.
pub fn status_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(14.0, 6.0))
        .show(ui, add_contents);
}

/// A free-floating card surface (browser table, empty state, drop zone).
pub fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(10.0, 8.0))
        .rounding(Rounding::same(8.0))
        .stroke(Stroke::new(1.0_f32, stroke))
        .show(ui, add_contents);
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------

// A form is laid out on ONE alignment axis: everything — caption, control,
// helper text, buttons — starts at the same left edge, and the only vertical
// rhythm is the 4-point ramp below.
//
// It used to be a fixed 84-point caption column with its controls beside it and
// the helper text indented under them. That put two left edges on the page (the
// section headings flush at the margin, the captions right-aligned 80 points
// in), made every hint a ragged block floating in the middle of the page, and
// spent a third of a 430-point dialog on the caption column. Windows' own
// settings pages, Material's text fields and Apple's inspectors all caption a
// control *above* it for exactly this reason: one axis cannot misalign.

/// Space between a caption and the control it names.
const LABEL_GAP: f32 = 4.0;
/// Space after a whole field, before the next one.
pub const FIELD_GAP: f32 = 16.0;
/// Height of every form control — a text field, a button, a chip.
///
/// Windows uses 32 at its 14-point body text; this app's body is 12.5, so 28
/// keeps the same optical proportion on the same 4-point grid.
pub const CONTROL_H: f32 = 28.0;

/// One labelled control: caption, optional help mark, then the control itself
/// stretched to the full width.
///
/// `has_help` draws the "?" on the caption's own line, at its right end, so it
/// can never shift the control and never leaves a reserved hole on rows without
/// one. Returns the control's own result and the mark, which the caller
/// decorates (see `help_hint`).
pub fn field<R>(
    ui: &mut egui::Ui,
    label: &str,
    has_help: bool,
    add_control: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, Option<egui::Response>) {
    let mut mark = None;
    ui.horizontal(|ui| {
        // Primary text, not the dim colour: this caption is the row's title.
        // Dim caption + primary checkbox label made two rows of the same form
        // look like different kinds of thing.
        ui.label(egui::RichText::new(label).size(12.5));
        if has_help {
            let accent = ui.visuals().hyperlink_color;
            let fill = ui.visuals().widgets.inactive.weak_bg_fill;
            let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
            mark = Some(
                ui.add(
                    egui::Button::new(egui::RichText::new("?").size(10.5).color(accent))
                        .rounding(Rounding::same(7.0))
                        .min_size(egui::vec2(15.0, 15.0))
                        .fill(fill)
                        .stroke(Stroke::new(1.0_f32, stroke)),
                )
                .on_hover_cursor(egui::CursorIcon::Help),
            );
        }
    });
    ui.add_space(LABEL_GAP);
    let out = ui.horizontal(|ui| add_control(ui)).inner;
    ui.add_space(FIELD_GAP);
    (out, mark)
}

/// A checkbox row that may carry a help mark.
///
/// The tick and its caption are one control, so the mark goes after the
/// caption rather than on a line of its own: `field` with an empty label left
/// an invisible caption line above the tick, which pushed every such row 20
/// points away from the one above it for no visible reason.
pub fn check_field(
    ui: &mut egui::Ui,
    text: &str,
    checked: &mut bool,
    has_help: bool,
) -> Option<egui::Response> {
    let mut mark = None;
    ui.horizontal(|ui| {
        ui.checkbox(checked, text);
        if has_help {
            let accent = ui.visuals().hyperlink_color;
            let fill = ui.visuals().widgets.inactive.weak_bg_fill;
            let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
            mark = Some(
                ui.add(
                    egui::Button::new(egui::RichText::new("?").size(10.5).color(accent))
                        .rounding(Rounding::same(7.0))
                        .min_size(egui::vec2(15.0, 15.0))
                        .fill(fill)
                        .stroke(Stroke::new(1.0_f32, stroke)),
                )
                .on_hover_cursor(egui::CursorIcon::Help),
            );
        }
    });
    ui.add_space(FIELD_GAP);
    mark
}

/// A caption that sits immediately above a control drawn by the caller, for
/// rows whose control owns its own line.
pub fn caption(ui: &mut egui::Ui, label: &str) {
    ui.label(egui::RichText::new(label).size(12.5));
    ui.add_space(LABEL_GAP);
}

/// A settings group: the WinUI settings-card surface — a hairline, a 6-point
/// corner and a padded interior. A group is what tells the reader that the rows
/// inside belong together, so the gaps between rows can stay small and the
/// page never becomes a wall of evenly spaced, unrelated lines.
pub fn group(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::none()
        .fill(ui.visuals().window_fill)
        .inner_margin(Margin::symmetric(16.0, 14.0))
        .rounding(Rounding::same(6.0))
        .stroke(Stroke::new(1.0_f32, stroke))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
    ui.add_space(4.0);
}

/// A small section heading above a group.
pub fn section(ui: &mut egui::Ui, title: &str) {
    // 24 above, 8 below: the heading has to belong to the group under it more
    // than to the group over it, and that distance is what says so.
    ui.add_space(20.0);
    ui.label(egui::RichText::new(title).size(13.0).strong());
    ui.add_space(8.0);
}

/// Secondary text: captions, descriptions, helper lines.
///
/// Windows expresses emphasis as alpha over the surface — 62% black on light,
/// 77% white on dark — rather than a hand-picked grey, so it holds up on any
/// background and in either theme. egui's `weak_text_color` blends towards the
/// panel fill instead, which on a dark card came out almost invisible.
pub fn secondary(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgba_unmultiplied(255, 255, 255, 196)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 158)
    }
}

/// Dimmed helper text (hints, footnotes).
///
/// Capped at [`HINT_MAX_W`]: a hint running the full width of a wide window is
/// a 120-character line, and the eye loses its place between lines. The cap is
/// also what makes the text wrap at all — inside a `horizontal` ui a label takes
/// its unwrapped width and paints past the window edge.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    let w = ui.available_width().min(HINT_MAX_W);
    ui.allocate_ui_with_layout(
        egui::vec2(w, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(text).size(11.5).color(secondary(ui))).wrap(),
            );
        },
    );
}

/// The width a hint is allowed to reach. Comfortable line length, not the
/// window's width.
pub const HINT_MAX_W: f32 = 560.0;

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
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) };
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
