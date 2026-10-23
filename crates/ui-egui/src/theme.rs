//! Design tokens (colours, sizes, fonts). Values measured from black-box observation of the
//! reference app's dark theme (see plan/lightroom/10-observed-ui.md); everything is our own code.
//!
//! Themes come in two families, picked by the Appearance Mode setting (Settings ▸ Interface):
//! - dark: **Charcoal** (the default, the observed dark look) and **Midnight** (near black);
//! - light: **Silver** (light grey chrome) and **Paper** (near white).

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

pub const FONT_SEMIBOLD: &str = "semibold";

/// Shared colour-label palette for badges, thumbnail surrounds and feedback.
pub fn label_color(label: lightcraft_catalog::ColorLabel) -> Color32 {
    use lightcraft_catalog::ColorLabel;
    match label {
        ColorLabel::Red => Color32::from_rgb(222, 72, 72),
        ColorLabel::Yellow => Color32::from_rgb(232, 196, 58),
        ColorLabel::Green => Color32::from_rgb(88, 176, 92),
        ColorLabel::Blue => Color32::from_rgb(72, 130, 222),
        ColorLabel::Purple => Color32::from_rgb(158, 100, 210),
    }
}

/// A translucent label colour over thumbnail chrome; selection remains brighter.
pub fn label_background(base: Color32, label: Option<lightcraft_catalog::ColorLabel>, selected: bool) -> Color32 {
    label.map_or(base, |l| base.lerp_to_gamma(label_color(l), if selected { 0.32 } else { 0.22 }))
}

/// Label confirmations use a pale colour and dark text, distinct from neutral/error HUDs.
pub fn label_toast_colors(label: lightcraft_catalog::ColorLabel) -> (Color32, Color32) {
    (Color32::WHITE.lerp_to_gamma(label_color(label), 0.3), Color32::from_rgb(36, 24, 24))
}

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    /// Top bar, side panels, bottom bar, tool strip.
    pub chrome: Color32,
    /// Photo canvas (detail view) and filmstrip.
    pub canvas: Color32,
    /// Grid background behind the cells.
    pub grid_bg: Color32,
    pub cell: Color32,
    pub cell_selected: Color32,
    pub divider: Color32,
    pub inset: Color32,
    pub field: Color32,
    pub field_border: Color32,
    pub button: Color32,
    pub button_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub tool_active: Color32,
    pub text: Color32,
    pub text_label: Color32,
    pub text_dim: Color32,
    pub text_disabled: Color32,
    /// Icon glyphs in the chrome.
    pub icon: Color32,
    pub track: Color32,
    pub thumb: Color32,
    pub thumb_hover: Color32,
    pub accent: Color32,
    pub star: Color32,
    pub pick: Color32,
    pub reject: Color32,
    /// Cautionary notices (e.g. a raw shown from its embedded preview): a muted amber.
    pub caution: Color32,
    pub mask_overlay: Color32,
    // metrics (points)
    pub top_bar_h: f32,
    pub bottom_bar_h: f32,
    pub panel_w: f32,
    pub strip_w: f32,
    pub slider_row_h: f32,
    pub section_h: f32,
    pub film_h: f32,
}

/// An interface theme. Each belongs to the dark or the light family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    #[default]
    Charcoal,
    Midnight,
    Silver,
    Paper,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 4] = [ThemeKind::Charcoal, ThemeKind::Midnight, ThemeKind::Silver, ThemeKind::Paper];

    pub fn is_dark(self) -> bool {
        matches!(self, ThemeKind::Charcoal | ThemeKind::Midnight)
    }

    /// The theme's id (settings value and command suffix).
    pub fn id(self) -> &'static str {
        match self {
            ThemeKind::Charcoal => "charcoal",
            ThemeKind::Midnight => "midnight",
            ThemeKind::Silver => "silver",
            ThemeKind::Paper => "paper",
        }
    }

    /// The English display name.
    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Charcoal => "Charcoal",
            ThemeKind::Midnight => "Midnight",
            ThemeKind::Silver => "Silver",
            ThemeKind::Paper => "Paper",
        }
    }
}

impl Default for Tokens {
    fn default() -> Self {
        Tokens {
            chrome: Color32::from_rgb(0x2d, 0x2d, 0x2d),
            canvas: Color32::from_rgb(0x1c, 0x1c, 0x1c),
            grid_bg: Color32::from_rgb(0x0f, 0x0f, 0x0f),
            cell: Color32::from_rgb(0x1c, 0x1c, 0x1c),
            cell_selected: Color32::from_rgb(0x2d, 0x2d, 0x2d),
            divider: Color32::from_rgb(0x1c, 0x1c, 0x1c),
            inset: Color32::from_rgb(0x23, 0x23, 0x23),
            field: Color32::from_rgb(0x22, 0x22, 0x22),
            field_border: Color32::from_rgb(0x3c, 0x3c, 0x3c),
            button: Color32::from_rgb(0x24, 0x24, 0x24),
            button_border: Color32::from_rgb(0x4c, 0x4c, 0x4c),
            hover: Color32::from_rgb(0x3a, 0x3a, 0x3a),
            pressed: Color32::from_rgb(0x3f, 0x3f, 0x3f),
            tool_active: Color32::from_rgb(0x3f, 0x3f, 0x3f),
            text: Color32::from_rgb(0xe2, 0xe2, 0xe2),
            text_label: Color32::from_rgb(0xbc, 0xbc, 0xbc),
            text_dim: Color32::from_rgb(0x8e, 0x8e, 0x8e),
            text_disabled: Color32::from_rgb(0x5c, 0x5c, 0x5c),
            icon: Color32::from_rgb(0x9a, 0x9a, 0x9a),
            track: Color32::from_rgb(0x5a, 0x5a, 0x5a),
            thumb: Color32::from_rgb(0xa0, 0xa0, 0xa0),
            thumb_hover: Color32::from_rgb(0xe0, 0xe0, 0xe0),
            accent: Color32::from_rgb(0x01, 0x65, 0xdd),
            star: Color32::from_rgb(0xd8, 0xd8, 0xd8),
            pick: Color32::from_rgb(0xf0, 0xf0, 0xf0),
            reject: Color32::from_rgb(0xe0, 0x4a, 0x4a),
            caution: Color32::from_rgb(0xe3, 0xa8, 0x3c),
            mask_overlay: Color32::from_rgba_unmultiplied(0xe0, 0x20, 0x30, 110),
            top_bar_h: 42.0,
            bottom_bar_h: 48.0,
            panel_w: 270.0,
            strip_w: 48.0,
            slider_row_h: 45.0,
            section_h: 57.0,
            film_h: 142.0,
        }
    }
}

impl Tokens {
    /// The tokens of `kind`. Metrics are shared; only colours change.
    pub fn for_kind(kind: ThemeKind) -> Tokens {
        let rgb = |v: u32| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
        let base = Tokens::default();
        match kind {
            ThemeKind::Charcoal => base,
            ThemeKind::Midnight => Tokens {
                chrome: rgb(0x1e1e1e),
                canvas: rgb(0x121212),
                grid_bg: rgb(0x080808),
                cell: rgb(0x121212),
                cell_selected: rgb(0x262626),
                divider: rgb(0x0e0e0e),
                inset: rgb(0x181818),
                field: rgb(0x161616),
                field_border: rgb(0x333333),
                button: rgb(0x191919),
                button_border: rgb(0x404040),
                hover: rgb(0x2e2e2e),
                pressed: rgb(0x343434),
                tool_active: rgb(0x343434),
                ..base
            },
            ThemeKind::Silver => Tokens {
                chrome: rgb(0xe6e6e6),
                canvas: rgb(0xcfcfcf),
                grid_bg: rgb(0xd9d9d9),
                cell: rgb(0xcbcbcb),
                cell_selected: rgb(0xb4b4b4),
                divider: rgb(0xc4c4c4),
                inset: rgb(0xdcdcdc),
                field: rgb(0xf6f6f6),
                field_border: rgb(0xb8b8b8),
                button: rgb(0xf2f2f2),
                button_border: rgb(0xb0b0b0),
                hover: rgb(0xd2d2d2),
                pressed: rgb(0xc4c4c4),
                tool_active: rgb(0xcacaca),
                text: rgb(0x1c1c1c),
                text_label: rgb(0x383838),
                text_dim: rgb(0x6a6a6a),
                text_disabled: rgb(0xa2a2a2),
                icon: rgb(0x555555),
                track: rgb(0xa8a8a8),
                thumb: rgb(0x5e5e5e),
                thumb_hover: rgb(0x262626),
                accent: rgb(0x0a62d0),
                star: rgb(0x3a3a3a),
                pick: rgb(0x2a2a2a),
                reject: rgb(0xc83434),
                caution: rgb(0xb0761a),
                ..base
            },
            ThemeKind::Paper => Tokens {
                chrome: rgb(0xfafafa),
                canvas: rgb(0xececec),
                grid_bg: rgb(0xf2f2f2),
                cell: rgb(0xe8e8e8),
                cell_selected: rgb(0xd2d2d2),
                divider: rgb(0xe0e0e0),
                inset: rgb(0xf0f0f0),
                field: rgb(0xffffff),
                field_border: rgb(0xcccccc),
                button: rgb(0xffffff),
                button_border: rgb(0xc6c6c6),
                hover: rgb(0xebebeb),
                pressed: rgb(0xdedede),
                tool_active: rgb(0xe2e2e2),
                text: rgb(0x161616),
                text_label: rgb(0x343434),
                text_dim: rgb(0x707070),
                text_disabled: rgb(0xababab),
                icon: rgb(0x585858),
                track: rgb(0xbdbdbd),
                thumb: rgb(0x606060),
                thumb_hover: rgb(0x202020),
                accent: rgb(0x0a62d0),
                star: rgb(0x3a3a3a),
                pick: rgb(0x2a2a2a),
                reject: rgb(0xc83434),
                caution: rgb(0xb0761a),
                ..base
            },
        }
    }

    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::NULL)).unwrap_or_default()
    }
    pub fn font(&self, size: f32) -> FontId {
        FontId::proportional(size)
    }
    pub fn semibold(&self, size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(FONT_SEMIBOLD.into()))
    }
}

/// Install the craft-fonts faces this instance has: embedded, or loaded beside the WASM on the web.
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions(lightcraft_engine::craft_fonts()));
}

/// Inter remains the default UI face. Egui's default faces and craft-fonts cover other scripts.
/// Without craft-fonts, CJK text shows boxes.
pub fn font_definitions(craft: &'static [lightcraft_engine::CraftFont]) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("Inter-SemiBold".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"))));
    // Craft-fonts faces in preference order for a family drawn in `style`.
    let fallback = |style: &str| {
        lightcraft_engine::fonts::ui_fallback(craft, crate::i18n::language().script(), style)
            .into_iter()
            .map(craft_font_name)
            .collect::<Vec<String>>()
    };
    let (regular, mut bold) = (fallback("Regular"), fallback("Bold"));
    for name in regular.iter().chain(bold.iter()) {
        if let Some(font) = craft.iter().find(|font| &craft_font_name(font) == name) {
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_static(font.bytes)));
        }
    }
    // A variable face holds every weight in its one file: strong text draws it heavier, from the
    // same bytes. A face with one fixed weight stays as it is.
    for name in &mut bold {
        let Some(font) = craft.iter().find(|font| &craft_font_name(font) == name) else { continue };
        let Some(heavier) = strong_weight(font.bytes) else { continue };
        *name = format!("{name} strong");
        fonts.font_data.insert(name.clone(), Arc::new(heavier));
    }
    let defaults: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut prop = vec!["Inter".to_string()];
    prop.extend(defaults.iter().cloned());
    prop.extend(regular);
    fonts.families.insert(FontFamily::Proportional, prop);
    let mut semi = vec!["Inter-SemiBold".to_string()];
    semi.extend(defaults);
    semi.extend(bold);
    fonts.families.insert(FontFamily::Name(FONT_SEMIBOLD.into()), semi);
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.extend(fallback("Regular"));
    fonts
}

/// The font families used by this app instance, including the faces loaded beside the WASM.
pub fn font_credits() -> String {
    let mut families = vec!["Inter"];
    for f in lightcraft_engine::craft_fonts() {
        if !families.contains(&f.family) {
            families.push(f.family);
        }
    }
    families.join(" / ")
}

/// The weight of strong text in a variable face, to go with Inter SemiBold.
const STRONG_WEIGHT: f32 = 600.0;

/// `bytes` as a face drawn at [`STRONG_WEIGHT`], when it is a variable font whose weight axis goes
/// heavier than the weight it draws at by default. The bytes are borrowed, not copied.
fn strong_weight(bytes: &'static [u8]) -> Option<FontData> {
    let face = FontData::from_static(bytes);
    let axis = face.variation_axes().into_iter().find(|axis| axis.tag.to_be_bytes() == *b"wght")?;
    let weight = STRONG_WEIGHT.min(axis.range.max);
    if weight.partial_cmp(&axis.default) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let tweak = egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([(axis.tag, weight)]), ..Default::default() };
    Some(face.tweak(tweak))
}

fn craft_font_name(f: &lightcraft_engine::CraftFont) -> String {
    format!("craft-fonts {} {}", f.family, f.style)
}

/// Points one mouse-wheel notch (a line) scrolls.
pub const WHEEL_LINE_POINTS: f32 = 100.0;

/// Share of its visible size one mouse-wheel notch scrolls the photo grid and the filmstrip (Lightroom
/// Classic: three notches move a photo from one edge to the other, at any window size).
pub const WHEEL_STRIP_SHARE: f32 = 1.0 / 3.0;

/// Zoom per wheel point (`exp(speed × points)`): egui's 1/200 at its stock 40-point notch, rescaled so
/// one notch still zooms the same amount now that it scrolls [`WHEEL_LINE_POINTS`].
pub const WHEEL_ZOOM_PER_POINT: f32 = 40.0 / 200.0 / WHEEL_LINE_POINTS;

fn theme_id() -> egui::Id {
    egui::Id::new("lightcraft-theme")
}

/// The theme `ctx` is styled with (none before the first [`apply_kind`]).
pub fn current(ctx: &egui::Context) -> Option<ThemeKind> {
    ctx.data(|d| d.get_temp::<ThemeKind>(theme_id()))
}

/// Style `ctx` with the default theme.
pub fn apply(ctx: &egui::Context) {
    apply_kind(ctx, ThemeKind::default());
}

/// Style `ctx` with `kind`. The visuals go to both of egui's styles, so egui following the
/// system's dark/light setting on its own never swaps in its stock look.
pub fn apply_kind(ctx: &egui::Context, kind: ThemeKind) {
    // Keep photo zoom shortcuts from resizing egui itself.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    // A mouse wheel notch scrolls 100 points (egui's default 40 took 50+ notches through an open
    // Edit panel; Lightroom moves about this much per notch). Trackpads report points and are
    // unaffected.
    // A modifier-wheel (and the loupe's plain-wheel) zoom notch stays about 1.22x, as it was at 40 points.
    ctx.options_mut(|o| {
        o.input_options.line_scroll_speed = WHEEL_LINE_POINTS;
        o.input_options.scroll_zoom_speed = WHEEL_ZOOM_PER_POINT;
    });
    let t = Tokens::for_kind(kind);
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::NULL, t);
        d.insert_temp(theme_id(), kind);
    });
    let dark = kind.is_dark();
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.chrome;
    v.window_fill = t.chrome;
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.inset;
    v.window_stroke = Stroke::new(1.0, t.button_border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = t.accent.gamma_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.override_text_color = Some(t.text_label);
    let (popup, window) = if dark { (140, 160) } else { (50, 60) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(popup) };
    v.window_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 30, spread: 0, color: Color32::from_black_alpha(window) };
    let w = &mut v.widgets;
    craft_ui::theme::WidgetPalette { noninteractive: t.chrome, inactive: t.button, hovered: t.hover, active: t.pressed, open: t.hover }.apply_to(
        w,
        CornerRadius::same(4),
        Stroke::new(1.0, t.text_label),
    );
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.divider);
    w.inactive.bg_stroke = Stroke::new(1.0, t.button_border);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 22.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(13.0));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(13.0));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(egui::TextStyle::Heading, FontId::new(16.0, FontFamily::Name(FONT_SEMIBOLD.into())));
        s.animation_time = 0.08;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, set_language};

    /// Every theme scrolls 100 points a wheel notch, and a zoom notch stays egui's stock ~1.22x
    /// (`exp(40 / 200)`) instead of growing with the longer scroll.
    #[test]
    fn a_wheel_notch_scrolls_further_but_zooms_as_before_in_every_theme() {
        for kind in [ThemeKind::Charcoal, ThemeKind::Midnight, ThemeKind::Silver, ThemeKind::Paper] {
            let ctx = egui::Context::default();
            apply_kind(&ctx, kind);
            let (line, zoom) = ctx.options(|o| (o.input_options.line_scroll_speed, o.input_options.scroll_zoom_speed));
            assert_eq!(line, WHEEL_LINE_POINTS, "{kind:?}");
            assert!(((zoom * line).exp() - (40.0_f32 / 200.0).exp()).abs() < 1e-4, "{kind:?}: a notch zooms {}", (zoom * line).exp());
        }
    }

    /// #736: the release input must carry Thai, not silently skip the check with an older pin.
    /// Exercise the Thai characters of the reported folder name through every egui family.
    #[test]
    fn thai_folder_name_has_glyphs_in_every_ui_family() {
        if lightcraft_engine::CRAFT_FONTS.is_empty() {
            lightcraft_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR: no craft-fonts to check");
            return;
        }
        let thai = lightcraft_engine::CRAFT_FONTS.iter().find(|f| f.covers("Thai")).expect("font input includes Thai");
        set_language(Locale::En);
        let definitions = font_definitions(lightcraft_engine::CRAFT_FONTS);
        let name = craft_font_name(thai);
        assert!(definitions.font_data.contains_key(&name));
        // (The strong family has the face's heavier twin instead when it is a variable font.)
        let strong = format!("{name} strong");
        for family in [FontFamily::Proportional, FontFamily::Name(FONT_SEMIBOLD.into()), FontFamily::Monospace] {
            let names = definitions.families.get(&family).expect("UI font family");
            assert!(names.contains(&name) || (family == semibold_family() && names.contains(&strong)), "{family:?} falls back on {name}");
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        let mut frame = ctx.run_ui(egui::RawInput::default(), |_| {});
        frame.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            for family in [FontFamily::Proportional, FontFamily::Name(FONT_SEMIBOLD.into()), FontFamily::Monospace] {
                let id = FontId::new(13.0, family);
                // Check the Thai portion; egui 0.36's has_glyph reports false for characters
                // in the replacement glyph's own face (including normal monospace Latin).
                for ch in "Studio34 ถ่ายภาพงานแต่งงาน".chars().filter(|ch| !ch.is_ascii()) {
                    assert!(fonts.has_glyph(&id, ch), "{id:?} has {ch}");
                }
            }
        });
    }

    #[test]
    fn native_chinese_face_falls_back_after_default_faces() {
        static FACES: &[lightcraft_engine::CraftFont] = &[
            lightcraft_engine::CraftFont {
                family: "BIZ UDPGothic",
                style: "Regular",
                scripts: &["Jpan", "Latn"],
                bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
            },
            lightcraft_engine::CraftFont {
                family: "Noto Sans CJK SC",
                style: "Regular",
                scripts: &["Hans", "Latn"],
                bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
            },
        ];
        set_language(Locale::ZhHans);
        let definitions = font_definitions(FACES);
        let defaults = FontDefinitions::default();
        let default_prop = defaults.families.get(&FontFamily::Proportional).expect("default proportional fonts");
        let chinese = "craft-fonts Noto Sans CJK SC Regular";
        for (family, primary) in [(FontFamily::Proportional, "Inter"), (FontFamily::Name(FONT_SEMIBOLD.into()), "Inter-SemiBold")] {
            let names = definitions.families.get(&family).expect("UI font family");
            assert_eq!(names.first().map(String::as_str), Some(primary));
            assert_eq!(names.get(1), default_prop.first());
            assert!(names.iter().position(|name| name == chinese).is_some_and(|position| position > default_prop.len()));
        }
        let mono = definitions.families.get(&FontFamily::Monospace).expect("monospace font family");
        let default_mono = defaults.families.get(&FontFamily::Monospace).expect("default monospace fonts");
        assert_eq!(mono.first(), default_mono.first());
        assert!(mono.iter().position(|name| name == chinese).is_some_and(|position| position >= default_mono.len()));
        set_language(Locale::En);
    }

    #[test]
    fn chinese_face_leads_for_chinese_and_follows_the_japanese_face_for_japanese() {
        // The web build's faces (loaded beside the WASM, then joined to the embedded ones by
        // `lightcraft_engine::fonts::add_fonts`) are ordinary craft-fonts faces: the Chinese face
        // must precede the Japanese one when the UI uses Chinese, in every UI family.
        let embedded = lightcraft_engine::CRAFT_FONTS.iter().find(|font| font.family == "Noto Sans CJK SC" && font.style == "Regular");
        let inter: &'static [u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let faces: &'static [lightcraft_engine::CraftFont] = Box::leak(Box::new([
            lightcraft_engine::CraftFont { family: "BIZ UDPGothic", style: "Regular", scripts: &["Jpan", "Latn"], bytes: inter },
            lightcraft_engine::CraftFont {
                family: "Noto Sans CJK SC",
                style: "Regular",
                scripts: &["Hans", "Latn"],
                bytes: embedded.map_or(inter, |font| font.bytes),
            },
        ]));
        let chinese = "craft-fonts Noto Sans CJK SC Regular";
        set_language(Locale::ZhHans);
        let definitions = font_definitions(faces);
        let default_count = FontDefinitions::default().families.get(&FontFamily::Proportional).map_or(0, Vec::len);
        for (family, primary) in [(FontFamily::Proportional, "Inter"), (FontFamily::Name(FONT_SEMIBOLD.into()), "Inter-SemiBold")] {
            let names = definitions.families.get(&family).expect("UI font family");
            assert_eq!(names.first().map(String::as_str), Some(primary));
            assert_eq!(names.get(1 + default_count).map(String::as_str), Some(chinese));
        }
        assert!(definitions.families.get(&FontFamily::Monospace).is_some_and(|names| names.contains(&chinese.to_string())));

        if embedded.is_some() {
            let ctx = egui::Context::default();
            ctx.set_fonts(definitions);
            let mut frame = ctx.run_ui(egui::RawInput::default(), |_| {});
            frame.textures_delta.clear();
            ctx.fonts_mut(|fonts| {
                for family in [FontFamily::Proportional, FontFamily::Name(FONT_SEMIBOLD.into()), FontFamily::Monospace] {
                    let id = FontId::new(13.0, family);
                    assert!("简体中文字".chars().all(|ch| fonts.has_glyph(&id, ch)), "{id:?}");
                }
            });
        }

        set_language(Locale::Ja);
        let japanese = font_definitions(faces);
        let names = japanese.families.get(&FontFamily::Proportional).expect("proportional fonts");
        assert_eq!(names.get(1 + default_count).map(String::as_str), Some("craft-fonts BIZ UDPGothic Regular"));
        assert_eq!(names.get(2 + default_count).map(String::as_str), Some(chinese));
        set_language(Locale::En);
    }

    const ARABIC_WORD: &str = "الصور";

    fn semibold_family() -> FontFamily {
        FontFamily::Name(FONT_SEMIBOLD.into())
    }

    /// The width of `text` in `family` at the body size, and the ink its glyphs put in the font
    /// atlas (summed coverage), with `definitions` installed.
    fn width_and_ink(definitions: FontDefinitions, text: &str, family: FontFamily) -> (f32, u64) {
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        let mut frame = ctx.run_ui(egui::RawInput::default(), |_| {});
        frame.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            let galley = fonts.layout_no_wrap(text.into(), FontId::new(13.0, family), Color32::WHITE);
            let atlas = fonts.image();
            let mut ink = 0_u64;
            for glyph in galley.rows.iter().flat_map(|row| row.glyphs.iter()) {
                let (min, max) = (glyph.uv_rect.min, glyph.uv_rect.max);
                for y in min[1]..max[1] {
                    for x in min[0]..max[0] {
                        ink += u64::from(atlas[(usize::from(x), usize::from(y))].a());
                    }
                }
            }
            (galley.size().x, ink)
        })
    }

    /// The craft-fonts face for Arabic, when it is a variable font with a weight axis.
    fn variable_arabic_face() -> Option<&'static lightcraft_engine::CraftFont> {
        let face = lightcraft_engine::CRAFT_FONTS.iter().find(|font| font.covers("Arab"))?;
        FontData::from_static(face.bytes).variation_axes().iter().any(|axis| axis.tag.to_be_bytes() == *b"wght").then_some(face)
    }

    /// Strong text in a script whose fallback face is a variable font is really heavier: an Arabic
    /// tab name used to be drawn at regular weight beside a bold Latin one.
    #[test]
    fn strong_arabic_text_is_heavier_than_regular_with_a_variable_face() {
        // Given a build with craft-fonts whose Arabic face has a weight axis
        if variable_arabic_face().is_none() {
            eprintln!("skipped: built without a variable-weight craft-fonts face for Arabic");
            return;
        }
        set_language(Locale::En);
        // When an Arabic word is laid out regular and strong at the same size
        let (regular_width, regular_ink) = width_and_ink(font_definitions(lightcraft_engine::CRAFT_FONTS), ARABIC_WORD, FontFamily::Proportional);
        let (strong_width, strong_ink) = width_and_ink(font_definitions(lightcraft_engine::CRAFT_FONTS), ARABIC_WORD, semibold_family());
        eprintln!("{ARABIC_WORD}: regular {regular_width} px, ink {regular_ink}; strong {strong_width} px, ink {strong_ink}");
        // Then the strong one is wider and puts down more ink
        assert!(strong_width > regular_width, "strong Arabic is wider than regular: strong {strong_width} px, regular {regular_width} px");
        assert!(strong_ink as f64 > regular_ink as f64 * 1.05, "strong Arabic has more ink than regular: strong {strong_ink}, regular {regular_ink}");
    }

    /// The heavier face is for strong text only: regular and monospace text keep the face as the
    /// font file has it, and Latin text keeps Inter in both weights.
    #[test]
    fn regular_text_and_latin_are_unchanged_by_the_strong_variable_face() {
        // Given a build with craft-fonts whose Arabic face has a weight axis
        let Some(arabic) = variable_arabic_face() else {
            eprintln!("skipped: built without a variable-weight craft-fonts face for Arabic");
            return;
        };
        set_language(Locale::En);
        // When the UI builds its fonts
        let definitions = font_definitions(lightcraft_engine::CRAFT_FONTS);
        // Then the regular Arabic face is the font file untouched, and the only Arabic face of
        // the regular and monospace families
        let name = craft_font_name(arabic);
        assert_eq!(definitions.font_data.get(&name).map(|data| &data.tweak), Some(&egui::FontTweak::default()));
        let plain_weight = egui::epaint::text::VariationCoords::default();
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let names = definitions.families.get(&family).expect("UI font family");
            assert!(names.contains(&name), "{family:?} falls back on {name}");
            for other in names.iter().filter(|other| **other != name) {
                let coords = definitions.font_data.get(other).map(|data| data.tweak.coords.clone()).unwrap_or_default();
                assert_eq!(coords, plain_weight, "{family:?}: {other} is drawn at the weight its file has");
            }
        }
        // And regular Arabic text is as wide as with that face alone after Inter
        let mut plain = FontDefinitions::default();
        plain.font_data.insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
        plain.font_data.insert("Arabic".into(), Arc::new(FontData::from_static(arabic.bytes)));
        plain.families.insert(FontFamily::Proportional, vec!["Inter".into(), "Arabic".into()]);
        let (width, ink) = width_and_ink(definitions.clone(), ARABIC_WORD, FontFamily::Proportional);
        assert_eq!((width, ink), width_and_ink(plain, ARABIC_WORD, FontFamily::Proportional));
        // And Latin text, strong and regular, is as it is without craft-fonts
        for family in [FontFamily::Proportional, semibold_family()] {
            assert_eq!(
                width_and_ink(definitions.clone(), "Library", family.clone()),
                width_and_ink(font_definitions(&[]), "Library", family.clone()),
                "{family:?}"
            );
        }
        let (regular, _) = width_and_ink(definitions.clone(), "Library", FontFamily::Proportional);
        let (strong, _) = width_and_ink(definitions, "Library", semibold_family());
        assert!(strong > regular, "strong Latin stays wider than regular: {strong} px, {regular} px");
    }

    /// A face with one fixed weight is registered once: nothing heavier can be asked of it. Without
    /// craft-fonts there is nothing to register at all.
    #[test]
    fn a_static_face_has_no_strong_twin() {
        // Given a face without a weight axis for another script
        static FACES: &[lightcraft_engine::CraftFont] = &[lightcraft_engine::CraftFont {
            family: "Static Sans",
            style: "Regular",
            scripts: &["Thai"],
            bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
        }];
        set_language(Locale::En);
        for craft in [FACES, &[]] {
            // When the UI builds its fonts
            let definitions = font_definitions(craft);
            // Then no face is drawn at another weight than its file has, and the strong family
            // holds what the regular one does after Inter
            assert!(definitions.font_data.values().all(|data| data.tweak.coords == egui::epaint::text::VariationCoords::default()));
            let regular = definitions.families.get(&FontFamily::Proportional).expect("regular family");
            let strong = definitions.families.get(&semibold_family()).expect("strong family");
            assert_eq!(regular.get(1..), strong.get(1..));
            assert_eq!(definitions.font_data.len(), FontDefinitions::default().font_data.len() + 2 + craft.len());
        }
    }
}
