//! Design tokens (colours, sizes, fonts). Values measured from black-box observation of the
//! reference app's dark theme (see plan/lightroom/10-observed-ui.md); everything is our own code.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

pub const FONT_SEMIBOLD: &str = "semibold";
const WEB_CHINESE_FONT: &str = "web Noto Sans CJK SC";

/// Shared colour-label palette for badges, thumbnail surrounds and feedback.
pub fn label_color(label: dac_catalog::ColorLabel) -> Color32 {
    use dac_catalog::ColorLabel;
    match label {
        ColorLabel::Red => Color32::from_rgb(222, 72, 72),
        ColorLabel::Yellow => Color32::from_rgb(232, 196, 58),
        ColorLabel::Green => Color32::from_rgb(88, 176, 92),
        ColorLabel::Blue => Color32::from_rgb(72, 130, 222),
        ColorLabel::Purple => Color32::from_rgb(158, 100, 210),
    }
}

/// A translucent label colour over thumbnail chrome; selection remains brighter.
pub fn label_background(base: Color32, label: Option<dac_catalog::ColorLabel>, selected: bool) -> Color32 {
    label.map_or(base, |l| base.lerp_to_gamma(label_color(l), if selected { 0.32 } else { 0.22 }))
}

/// Label confirmations use a pale colour and dark text, distinct from neutral/error HUDs.
pub fn label_toast_colors(label: dac_catalog::ColorLabel) -> (Color32, Color32) {
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

pub fn install_fonts(ctx: &egui::Context) {
    install_fonts_with_chinese(ctx, None);
}

/// Install the browser's separately downloaded Chinese face alongside the embedded faces.
pub fn install_fonts_with_chinese(ctx: &egui::Context, chinese: Option<&Arc<FontData>>) {
    ctx.set_fonts(font_definitions_with_chinese(dac_engine::CRAFT_FONTS, chinese));
}

/// Inter remains the default UI face. Egui's default faces and craft-fonts cover other scripts;
/// the browser's separate Chinese face precedes Japanese faces within the CJK fallback list.
/// Without craft-fonts or that separate face, CJK text shows boxes.
pub fn font_definitions(craft: &'static [dac_engine::CraftFont]) -> FontDefinitions {
    font_definitions_with_chinese(craft, None)
}

/// The web build keeps the large Simplified Chinese face outside the size-limited WASM module.
/// Prefer it for Chinese UI text, including Traditional Chinese until it has its own face.
pub fn font_definitions_with_chinese(craft: &'static [dac_engine::CraftFont], chinese: Option<&Arc<FontData>>) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("Inter-SemiBold".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"))));
    // Craft-fonts faces in preference order for a family drawn in `style`.
    let fallback = |style: &str| {
        dac_engine::fonts::cjk_fallback(craft, crate::i18n::language().script(), style).into_iter().map(craft_font_name).collect::<Vec<String>>()
    };
    let (mut regular, mut bold) = (fallback("Regular"), fallback("Bold"));
    for name in regular.iter().chain(bold.iter()) {
        if let Some(font) = craft.iter().find(|font| &craft_font_name(font) == name) {
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_static(font.bytes)));
        }
    }
    let script = crate::i18n::language().script();
    let prefer_chinese = chinese.is_some() && matches!(script, "Hans" | "Hant");
    if let Some(font) = chinese {
        fonts.font_data.insert(WEB_CHINESE_FONT.into(), font.clone());
        if prefer_chinese {
            regular.insert(0, WEB_CHINESE_FONT.into());
            bold.insert(0, WEB_CHINESE_FONT.into());
        } else {
            regular.push(WEB_CHINESE_FONT.into());
            bold.push(WEB_CHINESE_FONT.into());
        }
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
    let mut mono_fallback = fallback("Regular");
    if prefer_chinese {
        mono_fallback.insert(0, WEB_CHINESE_FONT.into());
    }
    if chinese.is_some() && !prefer_chinese {
        mono_fallback.push(WEB_CHINESE_FONT.into());
    }
    mono.extend(mono_fallback);
    fonts
}

/// The font families used by this app instance, including the browser's separately loaded face.
pub fn font_credits(chinese_loaded: bool) -> String {
    let mut families = vec!["Inter"];
    for f in dac_engine::CRAFT_FONTS {
        if !families.contains(&f.family) {
            families.push(f.family);
        }
    }
    if chinese_loaded && !families.contains(&"Noto Sans CJK SC") {
        families.push("Noto Sans CJK SC");
    }
    families.join(" / ")
}

fn craft_font_name(f: &dac_engine::CraftFont) -> String {
    format!("craft-fonts {} {}", f.family, f.style)
}

pub fn apply(ctx: &egui::Context) {
    let t = Tokens::default();
    ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, t));
    let mut v = Visuals::dark();
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
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(140) };
    v.window_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 30, spread: 0, color: Color32::from_black_alpha(160) };
    let w = &mut v.widgets;
    for (wv, fill) in [
        (&mut w.noninteractive, t.chrome),
        (&mut w.inactive, t.button),
        (&mut w.hovered, t.hover),
        (&mut w.active, t.pressed),
        (&mut w.open, t.hover),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.corner_radius = CornerRadius::same(4);
        wv.fg_stroke = Stroke::new(1.0, t.text_label);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.divider);
    w.inactive.bg_stroke = Stroke::new(1.0, t.button_border);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
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

    #[test]
    fn native_chinese_face_falls_back_after_default_faces() {
        static FACES: &[dac_engine::CraftFont] = &[
            dac_engine::CraftFont {
                family: "BIZ UDPGothic",
                style: "Regular",
                scripts: &["Jpan", "Latn"],
                bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
            },
            dac_engine::CraftFont {
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
    fn separate_web_chinese_face_is_available_in_every_ui_family() {
        // Simulate the WASM build, which embeds only the Japanese face. The Chinese face arrives
        // as a separate file and must precede that face when the UI uses Chinese.
        static JAPANESE: &[dac_engine::CraftFont] = &[dac_engine::CraftFont {
            family: "BIZ UDPGothic",
            style: "Regular",
            scripts: &["Jpan", "Latn"],
            bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
        }];
        let embedded = dac_engine::CRAFT_FONTS.iter().find(|font| font.family == "Noto Sans CJK SC" && font.style == "Regular");
        let fallback: &'static [u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let source = embedded.map_or(fallback, |font| font.bytes);
        let chinese = Arc::new(FontData::from_static(source));
        set_language(Locale::ZhHans);
        let definitions = font_definitions_with_chinese(JAPANESE, Some(&chinese));
        let default_count = FontDefinitions::default().families.get(&FontFamily::Proportional).map_or(0, Vec::len);
        for (family, primary) in [(FontFamily::Proportional, "Inter"), (FontFamily::Name(FONT_SEMIBOLD.into()), "Inter-SemiBold")] {
            let names = definitions.families.get(&family).expect("UI font family");
            assert_eq!(names.first().map(String::as_str), Some(primary));
            assert_eq!(names.get(1 + default_count).map(String::as_str), Some(WEB_CHINESE_FONT));
        }
        assert!(definitions.families.get(&FontFamily::Monospace).is_some_and(|names| names.contains(&WEB_CHINESE_FONT.to_string())));

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
        let japanese = font_definitions_with_chinese(JAPANESE, Some(&chinese));
        let names = japanese.families.get(&FontFamily::Proportional).expect("proportional fonts");
        assert_eq!(names.get(1 + default_count).map(String::as_str), Some("craft-fonts BIZ UDPGothic Regular"));
        assert_eq!(names.get(2 + default_count).map(String::as_str), Some(WEB_CHINESE_FONT));
        set_language(Locale::En);
    }
}
