//! Win11 / Fluent-style theme for the egui shell: system fonts, rounded
//! corners, subtle borders, a blue accent — in light **or** dark.
//!
//! Colours are expressed through egui's own [`egui::Visuals`] so the whole UI
//! can follow the system theme: UI code asks `ui.visuals()` for a surface or a
//! text colour instead of hard-coding one. Only colours that must look the same
//! in both themes stay literal — the green "done" label, the red error text,
//! the amber folder and the file-type accents.

use eframe::egui;
use egui::{Color32, CornerRadius, Margin, Stroke};

/// Accent used for primary actions and links (Windows 11 blue).
const ACCENT_LIGHT: Color32 = Color32::from_rgb(0, 103, 192);
const ACCENT_DARK: Color32 = Color32::from_rgb(88, 166, 255);

/// Which palette the app paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// The Windows UI font stack, in the order a glyph is looked up.
///
/// This is the same choice Windows itself makes: `Microsoft YaHei UI` is the
/// face Windows draws its own dialogs and Explorer with (see
/// `HKCU\Control Panel\Desktop\WindowMetrics` → `MessageFont`/`CaptionFont`),
/// and it is face **1** of `msyh.ttc`, not face 0. Face 0 is plain
/// `Microsoft YaHei`, whose vertical metrics are looser (hhea 2167/-536 against
/// UI's 2080/-521 at the same em), which is why ordinary YaHei reads as airier
/// and slightly softer at UI sizes.
///
/// Latin stays `Segoe UI` rather than `Segoe UI Variable`. The variable font
/// is what Windows 11's own shell uses, but it carries only 2530 glyphs against
/// 5395, and its digit `one` is proportional (776 units against the static
/// face's 1104) — swapping it in would re-space every number in the list unless
/// `tnum` were forced on, which epaint does not expose. Measured, the two are
/// the same at 13 points for the letters this UI draws.
///
/// Deliberately NO color/emoji fonts — egui's glyph engine cannot parse them
/// and would render every glyph as a box.
const PROPORTIONAL_FONTS: &[(&str, u32)] = &[
    ("C:\\Windows\\Fonts\\segoeui.ttf", 0),
    ("C:\\Windows\\Fonts\\msyh.ttc", 1),
];
const MONOSPACE_FONTS: &[(&str, u32)] = &[
    ("C:\\Windows\\Fonts\\CascadiaMono.ttf", 0),
    ("C:\\Windows\\Fonts\\consola.ttf", 0),
    ("C:\\Windows\\Fonts\\msyh.ttc", 1),
];

/// The family name of the semibold/bold stack, for headings and emphasis.
///
/// egui has no synthetic bolding — `RichText::strong()` only darkens the colour,
/// which stopped being a distinction once body text itself went near-black. So
/// emphasis is a *face*, the way Windows does it: `Segoe UI Bold` for Latin and
/// `Microsoft YaHei UI Bold` (face 1 of `msyhbd.ttc`, the UI-metric face, same
/// as the regular stack) for CJK. `msyhbd` covers every non-ASCII character in
/// this crate's string literals (408 distinct code points, counted, not
/// estimated).
pub const BOLD: &str = "bold";

const BOLD_FONTS: &[(&str, u32)] = &[
    ("C:\\Windows\\Fonts\\segoeuib.ttf", 0),
    ("C:\\Windows\\Fonts\\msyhbd.ttc", 1),
];

/// How a system face is grid-fitted.
///
/// `symmetric_rendering: false` is the one that matters. With it on — egui's
/// default, and what every earlier ZipNest release used — ClearType-flavoured
/// TrueType programs (Microsoft YaHei ships a full `fpgm`/`prep`/`cvt ` set)
/// are run with GETINFO's "symmetric smoothing" bit set, which widens horizontal
/// stems so that they look right on a sub-pixel (RGB-striped) rasterizer. epaint
/// uses an analytic area rasterizer with one coverage channel, so those widened
/// stems have nothing to exploit and land as grey fringes instead.
///
/// Measured on this machine, 13-point Microsoft YaHei UI, `subpixel_binning`
/// off: turning this flag off moves fully-covered (solid) pixels from 4.0% to
/// 27.2% of the ink, while total ink is unchanged (1005 → 1001). The same text
/// in Segoe UI is byte-identical either way, so this only touches CJK — which
/// is exactly the text the user complained about.
///
/// `preserve_linear_metrics` is deliberately left at its default. It sounds
/// like it controls vertical grid-fitting but in skrifa 0.44 it only gates
/// `backward_compatibility` and INSTCTRL selector 3; measured across 12- and
/// 13-point CJK and Latin in both faces it produces byte-identical bitmaps.
/// Setting it would have been a no-op dressed up as a fix.
fn crisper_tweak() -> egui::FontTweak {
    use egui::epaint::text::{HintingTarget, SmoothHinting};
    egui::FontTweak {
        hinting_target: HintingTarget::Smooth(SmoothHinting {
            symmetric_rendering: false,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The face index to actually load: `index` when the file really holds a face
/// there, else face `0`, else `None` — do not register this file at all.
///
/// epaint has no missing-face fallback: `family::Family::new` calls
/// `panic!("Error parsing … TTF/OTF font file")` whenever a configured font
/// fails to parse, and with `windows_subsystem = "windows"` that panic is a
/// launch with no window and no message at all. The six paths above are system
/// paths, but a user can replace a font file and an OEM image can ship a stub,
/// so each file is checked before it is registered.
///
/// Counting the faces of a `ttcf` (the previous guard) is not enough: it says
/// nothing about whether the bytes parse — a truncated collection, or an
/// `.lnk` sitting where a font should be, went straight through to the panic.
/// So the check is the parser itself, `skrifa::FontRef::from_index`, which is
/// exactly the call `epaint::text::FontFace::new` makes. It is also the only
/// fallible step there — `metrics`, `charmap` and `outline_glyphs` cannot fail
/// once it has succeeded — so a file that passes here cannot panic epaint.
fn usable_face_index(bytes: &[u8], index: u32) -> Option<u32> {
    [index, 0]
        .into_iter()
        .find(|&face| skrifa::FontRef::from_index(bytes, face).is_ok())
}

pub fn install_fonts(ctx: &egui::Context) {
    use egui::FontFamily;

    let mut fonts = egui::FontDefinitions::default();
    // Each font file is read from disk at most once and registered once, then
    // referenced by key from every family that wants it — the bold family below
    // reuses the regular faces, and reading a 19 MB `.ttc` once per family was
    // three reads of it on every start.
    //
    // `index` picks the face inside a `.ttc` collection; 0 for a plain
    // `.ttf`/`.otf`.
    let mut add = |path: &str, index: u32, families: &[FontFamily]| {
        let file = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let key = format!("sys-{index}-{file}");
        // Read the file only if no family has registered it yet. The bold chain
        // below reuses the regular faces, and without this guard `msyh.ttc`
        // (18.8 MB) was read three times per launch and `segoeui.ttf` twice.
        if !fonts.font_data.contains_key(&key) {
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            // Parse-check first: a font file that cannot be parsed must never
            // reach epaint, which treats it as a panic rather than an error.
            // A file that fails is not registered at all, so the families below
            // fall through to the next (or the built-in) face instead.
            let Some(index) = usable_face_index(&bytes, index) else {
                return;
            };
            let data = egui::FontData::from_blob(std::sync::Arc::new(bytes), index)
                .tweak(crisper_tweak());
            fonts.font_data.insert(key.clone(), std::sync::Arc::new(data));
        }
        for family in families {
            let list = fonts.families.entry(family.clone()).or_default();
            if !list.contains(&key) {
                list.push(key.clone());
            }
        }
    };
    for (path, index) in PROPORTIONAL_FONTS {
        add(path, *index, &[FontFamily::Proportional]);
    }
    for (path, index) in MONOSPACE_FONTS {
        add(path, *index, &[FontFamily::Monospace]);
    }
    // The bold stack: its own family, so emphasis can be a real weight change
    // rather than a darker colour. The regular faces follow as the tail of the
    // chain, so a glyph the bold files happen to lack still renders (at regular
    // weight) instead of becoming tofu.
    let bold_family = FontFamily::Name(BOLD.into());
    for (path, index) in BOLD_FONTS {
        add(path, *index, std::slice::from_ref(&bold_family));
    }
    for (path, index) in PROPORTIONAL_FONTS {
        add(path, *index, std::slice::from_ref(&bold_family));
    }
    // ...and the *built-in* tail as well. A custom family starts with no entry
    // in `fonts.families`, so it never inherits `egui-icons` — the only face for
    // `egui::special_emojis`, whose private-use code points no platform font
    // has. Without this, an icon drawn in the bold family is a tofu box.
    if let Some(tail) = fonts.families.get(&FontFamily::Proportional).cloned() {
        let list = fonts.families.entry(bold_family.clone()).or_default();
        for key in tail.into_iter().filter(|k| !k.starts_with("sys-")) {
            if !list.contains(&key) {
                list.push(key);
            }
        }
    }
    // Move our system fonts ahead of the built-ins so they win.
    for fam in [
        FontFamily::Proportional,
        FontFamily::Monospace,
        bold_family.clone(),
    ] {
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

/// The family that draws at semibold/bold weight.
///
/// Use [`emphasis`] rather than `RichText::strong()`: strong only darkens the
/// colour, and against near-black body text that is a 1.2:1 change nobody can
/// see. This is the actual bold face.
pub fn bold_family() -> egui::FontFamily {
    egui::FontFamily::Name(BOLD.into())
}

/// A [`egui::RichText`] in the bold face. Chain `.size(…)`/`.color(…)` after it.
pub fn emphasis(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(bold_family())
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// `color` at `factor` of its own RGB, still fully opaque.
///
/// This is the palette's way to darken towards a light surface. It has to be
/// spelled out because [`Color32`] is *premultiplied*: `gamma_multiply(0.6)`
/// scales alpha along with RGB, so the result composites as
/// `0.6·color + 0.4·surface` — it moves the colour *towards* the surface it was
/// meant to stand out from, and the more so the smaller the factor. On the
/// file-type badge that made the three-letter label fainter than plain accent
/// (measured 1.66:1 light / 1.50:1 dark on the ZIP badge, against 2.35 / 1.93
/// at full accent). Multiplying `r`/`g`/`b` instead keeps alpha at 255.
pub fn shade(color: Color32, factor: f32) -> Color32 {
    let channel = |c: u8| (f32::from(c) * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(channel(color.r()), channel(color.g()), channel(color.b()))
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
    v.window_corner_radius = CornerRadius::same(10);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
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
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(6);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(6);
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
    // Body text. egui's defaults are gray(80) light / gray(140) dark, which on
    // this app's surfaces measured 7.3:1 — readable, but visibly fainter than
    // the title bar's own text a few points above it, and faint text is most of
    // what reads as "blurry". These are Windows 11's primary text values
    // (`TextFillColorPrimary`, i.e. black at 90% on a light surface, white at
    // 90% on a dark one), not pure black: on a 96-DPI LCD a pure-black stem
    // looks harsh.
    //
    // Emphasis is *not* expressed by a darker colour any more: at 27 the body
    // text is already within 1.2:1 of black, so `.strong()` had nothing left to
    // say. See [`emphasis`] — emphasis is a bold face now.
    let body_text = if dark { rgb(229, 229, 229) } else { rgb(27, 27, 27) };
    style.visuals.widgets.noninteractive.fg_stroke.color = body_text;
    style.visuals.widgets.inactive.fg_stroke.color = body_text;
    // Hover feedback. `ghost_button` and `crumb_button` draw no frame at all, so
    // the text colour is the *only* thing that changes under the pointer — and
    // body text is now 1.2:1 from black, which left hover almost invisible on
    // exactly the controls that have nothing else to show. The accent is
    // unmistakable against the body colour and still reads as text (5.7:1).
    style.visuals.widgets.hovered.fg_stroke.color = if dark { ACCENT_DARK } else { ACCENT_LIGHT };
    // Hint text inside a text field is egui's `weak_text_color()`, which is
    // `text_color().gamma_multiply(0.6)` and *then* painted at that alpha — two
    // separate attenuations, so it measured 2.96:1 on a white field and the
    // search box's placeholder was nearly invisible. Pinning an opaque value
    // ends the double decay: 4.95:1 light, 6.66:1 dark, both on the field's own
    // background. It stays lighter than body text, which is what a placeholder
    // should be.
    style.visuals.weak_text_color = Some(if dark {
        rgb(160, 160, 160)
    } else {
        rgb(112, 112, 112)
    });
    // Turn off sub-pixel binning. It caches each glyph at up to four horizontal
    // fractional offsets so kerning looks even, and epaint's own docs say what
    // it costs: "It also lead to text looking more blurry." CJK ideographs were
    // already excluded from it (`glyph_atlas.rs`, `is_cjk`), so the gain is
    // Latin's — and so is the full-width punctuation this UI uses a lot of
    // (`，。！？` are not `is_cjk`; they each held four atlas slots). Hinting
    // itself stays on: that is the one that sharpens.
    style.visuals.text_options.subpixel_binning = false;
    // Default control height. egui sizes a button from its text plus
    // `button_padding`, which at 13-point text comes to 26 — every control in
    // the app was a different height from the 28-point text fields beside it.
    style.spacing.interact_size.y = CONTROL_H;
    style.spacing.button_padding.y = (CONTROL_H - 20.0) / 2.0;
    // egui draws a few things from its own `text_styles` table rather than
    // through anything this file can see: a `Window` title and the error dialog
    // are `TextStyle::Heading`, a `ProgressBar` percentage and a `DragValue` are
    // `TextStyle::Button`, and a bare `ui.label` / `TextEdit` is `TextStyle::Body`.
    // Mapping the table to the same constants is what makes AGENTS.md's "dialogs
    // share the main window's text scale" a fact instead of two numbers that
    // happened to agree. `Small` was the one that disagreed (egui default 9).
    style.text_styles = [
        (egui::TextStyle::Small, egui::FontId::proportional(TEXT_SMALL)),
        (egui::TextStyle::Body, egui::FontId::proportional(TEXT_BODY)),
        (egui::TextStyle::Button, egui::FontId::proportional(TEXT_BODY)),
        (egui::TextStyle::Heading, egui::FontId::proportional(TEXT_TITLE)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(TEXT_BODY)),
    ]
    .into();
}

// ---------------------------------------------------------------------------
// Type scale
// ---------------------------------------------------------------------------
//
// Every size the UI draws now lives here and is a whole number, so the scale
// sits on one grid and adding a size is a deliberate act rather than a new
// literal.
//
// One honest note about why: these faces (Microsoft YaHei UI, Segoe UI) both
// set `FORCE_INTEGER_PPEM` in `head.flags`, so skrifa rounds the ppem before
// hinting and a 12.5-point request rasterizes *identically* to 13. The half
// point only ever moved layout, not strokes. The measured sharpness step is a
// whole size step: at 13 px the edge-pixel share of this text is 0.60 and at
// 14 px it is 0.556. Body text ships at 13 — the 0.60 tier — because 14 would
// have grown every row and control by a point, and the far larger wins turned
// out to be elsewhere (`crisper_tweak`, the text colours). See
// `docs/font-crispness-design.md`.

/// The smallest thing the UI says: format labels, the "?" help mark.
pub const TEXT_TINY: f32 = 11.0;
/// The three letters on the 14×16 file-type badge in the list. Drawn *inside* a
/// fixed 14-point page, so it is deliberately below [`TEXT_TINY`] and must not
/// be raised with the rest of the scale.
pub const TEXT_BADGE: f32 = 8.0;
/// Hints, footnotes, status-bar text, error lines under a field.
pub const TEXT_SMALL: f32 = 12.0;
/// Body text: captions, chips, row names, list content.
pub const TEXT_BODY: f32 = 13.0;
/// A settings or form section heading.
pub const TEXT_SECTION: f32 = 13.0;
/// The title row of a page (Settings, Create).
pub const TEXT_TITLE: f32 = 18.0;

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
///
/// The dark-mode label is near-black, not white: the dark accent is a *light*
/// blue (rgb 88,166,255) and white on it measures 2.53:1 — the toolbar's main
/// button was the least legible text in the whole dark theme. Near-black on the
/// same fill is 7.29:1.
pub fn primary_button_enabled(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (fill, ink) = if enabled {
        (ui.visuals().hyperlink_color, on_accent(ui))
    } else if dark {
        (rgb(52, 52, 52), rgb(120, 120, 120))
    } else {
        (rgb(238, 238, 238), rgb(150, 150, 150))
    };
    // A disabled button greys out; an enabled one keeps the readable pair above.
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(ink))
            .fill(fill)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::NONE)
            .sense(sense),
    )
}

/// The colour a label takes when it sits on the accent fill.
///
/// Light mode's accent is a deep blue, so white reads at 5.67:1. Dark mode's is
/// a pale blue (rgb 88,166,255) and white on it is only 2.53:1 — below the 3:1
/// floor for large text, let alone body. rgb(20,20,20) is 7.29:1 on the same
/// fill. Every accent-filled control in the app — the primary buttons, the
/// empty-state button, the selected chip and the progress bar's percentage —
/// reads it from here, so the rule lives in one place.
pub fn on_accent(ui: &egui::Ui) -> Color32 {
    on_accent_color(ui.visuals().dark_mode)
}

/// [`on_accent`] for code that has the theme but not a [`egui::Ui`] — the
/// contrast tests, and any painting done straight to a painter.
pub fn on_accent_color(dark: bool) -> Color32 {
    if dark {
        rgb(20, 20, 20)
    } else {
        Color32::WHITE
    }
}

/// How far the accent fill reaches the progress bar's percentage label.
///
/// egui paints that label one `item_spacing` inside the track's left edge
/// (~8 points) and at [`TEXT_BODY`] "100%" is ~34 points wide, so the fill has
/// to cover ~42 of the bar's 360 points — 0.12 — before the whole label is on
/// the accent. Below this the label is on the *track* and takes the track's
/// ink instead; the two inks are equally readable, and the switch is invisible
/// in normal use because the bar passes 15% within a few frames.
pub const PROGRESS_LABEL_ON_FILL: f32 = 0.15;

/// Subtle ghost button (no fill until hovered) for secondary actions.
///
/// The hover fill comes from the theme instead of being measured from the whole
/// available rect, so hovering one button no longer lights up the whole row.
pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ghost_button_enabled(ui, text, true)
}

/// [`ghost_button`] with an explicit enabled flag: a disabled one keeps its
/// seat and its label but neither highlights nor reports a click.
pub fn ghost_button_enabled(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        let dim = if enabled {
            None
        } else {
            Some(weak_text(ui))
        };
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        if let Some(dim) = dim {
            visuals.widgets.inactive.fg_stroke.color = dim;
        }
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        ui.add(egui::Button::new(text).frame(false).sense(sense))
    })
    .inner
}

fn weak_text(ui: &egui::Ui) -> Color32 {
    ui.visuals()
        .weak_text_color
        .unwrap_or_else(|| ui.visuals().text_color().gamma_multiply(0.6))
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
    let idle_fill = ui.visuals().widgets.inactive.weak_bg_fill;
    let idle_stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    let idle_text = ui.visuals().text_color();
    let (fill, text_color, stroke) = if selected {
        (accent, on_accent(ui), Stroke::NONE)
    } else {
        (idle_fill, idle_text, Stroke::new(1.0_f32, idle_stroke))
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(TEXT_BODY).color(text_color))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(6))
            .min_size(egui::vec2(0.0, CONTROL_H)),
    )
}

/// A toolbar row drawn as a card strip matching the current theme.
pub fn top_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(Margin::symmetric(14, 10))
        .corner_radius(CornerRadius::ZERO)
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
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(Margin::symmetric(14, 6))
        .show(ui, add_contents);
}

/// A free-floating card surface (browser table, empty state, drop zone).
pub fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = ui.visuals().window_fill;
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(Margin::symmetric(10, 8))
        .corner_radius(CornerRadius::same(8))
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
/// Windows uses 32 at its 14-point body text; this app's body is 13, so 28
/// keeps the same optical proportion on the same 4-point grid. It also bounds
/// the type scale from above: a Segoe UI line is 1.330× its point size, and
/// `button_padding.y` below derives from a 20-point text line, so any size past
/// ~15 would make a button taller than the 28-point field beside it.
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
        ui.label(egui::RichText::new(label).size(TEXT_BODY));
        if has_help {
            let accent = ui.visuals().hyperlink_color;
            let fill = ui.visuals().widgets.inactive.weak_bg_fill;
            let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
            mark = Some(
                ui.add(
                    egui::Button::new(egui::RichText::new("?").size(TEXT_TINY).color(accent))
                        .corner_radius(CornerRadius::same(7))
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
                    egui::Button::new(egui::RichText::new("?").size(TEXT_TINY).color(accent))
                        .corner_radius(CornerRadius::same(7))
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
    ui.label(egui::RichText::new(label).size(TEXT_BODY));
    ui.add_space(LABEL_GAP);
}

/// A settings group: the WinUI settings-card surface — a hairline, a 6-point
/// corner and a padded interior. A group is what tells the reader that the rows
/// inside belong together, so the gaps between rows can stay small and the
/// page never becomes a wall of evenly spaced, unrelated lines.
pub fn group(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let stroke = ui.visuals().widgets.inactive.bg_stroke.color;
    egui::Frame::NONE
        .fill(ui.visuals().window_fill)
        .inner_margin(Margin::symmetric(16, 14))
        .corner_radius(CornerRadius::same(6))
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
    ui.label(emphasis(title).size(TEXT_SECTION));
    ui.add_space(8.0);
}

/// Secondary text: captions, descriptions, helper lines.
///
/// A solid grey rather than black at 62% alpha — the same value either way, but
/// stated as one colour so every caller's contrast is checkable without knowing
/// which surface it lands on.
///
/// Both values are chosen against the *hardest* surface in their theme, which
/// is the selection fill (a row's size and mtime columns are drawn in this
/// colour and can be selected): rgb(84) on rgb(205,229,255) is 5.86:1 and
/// rgb(190) on rgb(38,79,120) is 4.57:1 — both clear of 4.5:1 at 12-point. They
/// also keep a real step below body text (ΔL* 25.9 light, 13.9 dark), because a
/// secondary colour that sits within a couple of L* of the body value stops
/// reading as secondary at all.
pub fn secondary(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        rgb(190, 190, 190)
    } else {
        rgb(84, 84, 84)
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
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(TEXT_SMALL)
                        .color(secondary(ui)),
                )
                .wrap(),
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
pub mod contrast {
    //! WCAG 2.x contrast, for tests only.
    //!
    //! Every colour decision in this file is a measurement, so the measuring
    //! stick lives next to them. Exposed to the crate's other test modules
    //! (`theme::contrast::ratio`), which is why this is not just a helper
    //! inside `mod tests`.
    use eframe::egui::Color32;

    /// WCAG 2.x relative luminance of an opaque colour.
    fn luminance(c: Color32) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    /// Contrast ratio between two opaque colours: 1.0 (identical) to 21.0
    /// (black on white). The app's floor for body-sized text is 4.5:1.
    pub fn ratio(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }
}

#[cfg(test)]
mod tests {
    use super::contrast::ratio;
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

    /// The progress bar's percentage is painted on the bar's accent fill. It
    /// used to inherit `selection.stroke.color` — the accent itself — which
    /// measured 4.39:1 in light mode and 3.36:1 in dark.
    #[test]
    fn the_progress_percentage_clears_4_5_to_1_on_the_accent_fill() {
        for (dark, accent, label) in [(false, ACCENT_LIGHT, "light"), (true, ACCENT_DARK, "dark")] {
            let measured = ratio(on_accent_color(dark), accent);
            assert!(
                measured >= 4.5,
                "{label}: on_accent on the accent fill is {measured:.2}:1"
            );
        }
    }

    /// The track is what the label sits on until the fill reaches it, so the
    /// body text colour has to clear 4.5:1 there too.
    #[test]
    fn the_progress_label_is_readable_on_the_track() {
        for (mode, label) in [(Mode::Light, "light"), (Mode::Dark, "dark")] {
            let v = visuals(mode);
            let measured = ratio(v.text_color(), v.extreme_bg_color);
            assert!(measured >= 4.5, "{label}: track ink is {measured:.2}:1");
        }
    }

    /// [`PROGRESS_LABEL_ON_FILL`] has to be past the point where the fill
    /// actually reaches the label's end: 360-point bar, label starts one
    /// `item_spacing` in, "100%" is ~34 points wide at [`TEXT_BODY`].
    #[test]
    fn the_threshold_puts_the_label_on_the_fill_not_the_track() {
        let label_reach = 8.0 + 34.0;
        assert!(PROGRESS_LABEL_ON_FILL * 360.0 > label_reach);
    }

    /// The font guard must reject anything epaint would panic on. `ttcf`
    /// headers are the trap: the old code trusted the face count and returned
    /// index 0 for a collection whose face table points nowhere.
    #[test]
    fn an_unparseable_font_is_never_registered() {
        assert_eq!(usable_face_index(b"", 0), None);
        assert_eq!(usable_face_index(b"not a font at all", 0), None);
        // A file that only *looks* like a collection: "ttcf", version 1.0,
        // numFonts = 1, then no face-offset table at all.
        let stub = [
            b't', b't', b'c', b'f', 0x00, 0x01, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x01,
        ];
        assert_eq!(usable_face_index(&stub, 0), None);
        // And a bare sfnt magic with no table directory behind it.
        assert_eq!(usable_face_index(&[0x00, 0x01, 0x00, 0x00], 0), None);
    }

    /// The positive control: the faces the app actually asks for resolve, and
    /// a request past the last face falls back to face 0 instead of `None`.
    #[test]
    fn the_real_system_faces_resolve() {
        let Ok(ttc) = std::fs::read("C:\\Windows\\Fonts\\msyh.ttc") else {
            return; // Not this machine — nothing to assert.
        };
        assert_eq!(usable_face_index(&ttc, 1), Some(1));
        // `msyh.ttc` holds one face? Then index 1 falls back; if it holds more,
        // index 1 is itself valid — either way the answer is a face that parses.
        let out_of_range = usable_face_index(&ttc, u32::MAX);
        assert_eq!(out_of_range, Some(0));

        let Ok(segoe) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") else {
            return;
        };
        assert_eq!(usable_face_index(&segoe, 0), Some(0));
        // A `.ttf` asked for face 1 has no such face: fall back to 0.
        assert_eq!(usable_face_index(&segoe, 1), Some(0));
    }
}
