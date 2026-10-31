//! Design tokens (colours, sizes, fonts). Values measured from black-box observation of the
//! reference app's dark theme (see plan/lightroom/10-observed-ui.md); everything is our own code.
//!
//! Themes come in two families, picked by the Appearance Mode setting (Settings ▸ Interface):
//! - dark: **Charcoal** (the default, the observed dark look) and **Midnight** (near black);
//! - light: **Silver** (light grey chrome) and **Paper** (near white).

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

pub const FONT_SEMIBOLD: &str = "semibold";

/// The sizes text is set in, one per job. The steps are a point or more, so two sizes on a screen
/// are told apart; a panel names the size its text has (`t.caption()`, `t.body_strong()`, …)
/// instead of writing a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSize {
    /// A badge or a pill on a thumbnail or an icon.
    Micro,
    /// An explanation, the caption over a field, a line of status; in semibold, a small button or
    /// a choice.
    Caption,
    /// What stands by something larger: a count, the title of a tab, a control of the filter bar.
    Label,
    /// Controls, rows, values and menus.
    Body,
    /// The header of a section in a panel.
    Title,
    /// The name of a panel or of a window.
    PanelTitle,
    /// The title over the photos, and what an empty screen says.
    Display,
}

impl TextSize {
    /// Smallest first.
    pub const ALL: [TextSize; 7] =
        [TextSize::Micro, TextSize::Caption, TextSize::Label, TextSize::Body, TextSize::Title, TextSize::PanelTitle, TextSize::Display];

    /// The size in points.
    pub const fn points(self) -> f32 {
        match self {
            TextSize::Micro => 10.0,
            TextSize::Caption => 11.0,
            TextSize::Label => 12.0,
            TextSize::Body => 13.0,
            TextSize::Title => 14.0,
            TextSize::PanelTitle => 15.0,
            TextSize::Display => 18.0,
        }
    }

    /// The size nearest a number of points; of two equally near, the one nearer [`TextSize::Body`]
    /// (12.5 is 13 and so is 13.5). Anything past the ends is the end: a huge number or infinity
    /// the largest, a negative one the smallest. Not a number: the size of a control.
    pub fn nearest(points: f32) -> TextSize {
        // (far from the scale every size is as near as the next in an f32)
        let points = points.clamp(0.0, 1000.0);
        let body = TextSize::Body.points();
        let mut best = TextSize::Body;
        let mut least = f32::INFINITY;
        for size in TextSize::ALL {
            let off = (size.points() - points).abs();
            if off < least || (off == least && (size.points() - body).abs() < (best.points() - body).abs()) {
                (best, least) = (size, off);
            }
        }
        best
    }
}

const WEB_CHINESE_FONT: &str = "web Noto Sans CJK SC";

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
    /// A thumbnail's place while its picture is still being made: static, apart from the grid, the
    /// cells and the canvas in every theme.
    pub skeleton: Color32,
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
            skeleton: Color32::from_rgb(0x25, 0x25, 0x25),
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
        let t = match kind {
            ThemeKind::Charcoal => base,
            ThemeKind::Midnight => Tokens {
                chrome: rgb(0x1e1e1e),
                canvas: rgb(0x121212),
                grid_bg: rgb(0x080808),
                cell: rgb(0x121212),
                cell_selected: rgb(0x262626),
                skeleton: rgb(0x1d1d1d),
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
                skeleton: rgb(0xa6a6a6),
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
                skeleton: rgb(0xbcbcbc),
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
        };
        // The dim text of captions, counts and explanations is read like any other: it stands
        // 4.5:1 against every surface it is written on (as observed it was 4.20:1 on a panel in
        // Charcoal, and 3.3 to 4.3:1 on the light themes' surfaces).
        Tokens { text_dim: t.on_surfaces(t.text_dim), ..t }
    }

    /// The surfaces text is written on at rest: panels, the canvas, the grid and its cells,
    /// insets, text boxes and buttons. (A hovered, pressed or selected surface is passing or
    /// marked otherwise: text there is made legible against it where it is drawn.)
    fn surfaces(&self) -> [Color32; 7] {
        [self.chrome, self.canvas, self.grid_bg, self.cell, self.inset, self.field, self.button]
    }

    /// `color`, brought as far toward the text colour as it takes to stand 4.5:1 against every
    /// surface of [`Self::surfaces`]: a colour that says something as text.
    fn on_surfaces(&self, color: Color32) -> Color32 {
        self.surfaces().into_iter().fold(color, |c, on| self.legible(c, on, 4.5))
    }

    /// What is written or marked over a picture: a picture has no theme, so this is the same
    /// light ink in every theme, on [`Self::photo_shade`].
    pub const fn on_photo() -> Color32 {
        Color32::from_rgb(0xf2, 0xf2, 0xf2)
    }

    /// The flag of a rejected photo over a picture: a red light enough to stand 3:1 on the shade
    /// whatever is under it (a theme's own red is made for its panels: 2.3 to 3:1 here).
    pub const fn on_photo_reject() -> Color32 {
        Color32::from_rgb(0xff, 0x6b, 0x6b)
    }

    /// The shade under what is written or marked over a picture.
    /// (Black at 200 of 255: over white it is a grey the ink stands about 10.6:1 against, and
    /// [`Self::on_photo_reject`] 4:1.)
    pub fn photo_shade() -> Color32 {
        Color32::from_black_alpha(200)
    }

    /// The accent as text: a link, "in use", what Auto Sync is doing. (The accent itself is
    /// for fills and outlines: as text it is 2.56:1 on a panel in Charcoal.)
    pub fn text_link(&self) -> Color32 {
        self.on_surfaces(self.accent)
    }

    /// The caution colour as text. (3.09:1 on a panel in Silver as it is.)
    pub fn text_caution(&self) -> Color32 {
        self.on_surfaces(self.caution)
    }

    /// The colour of a rejection or an error as text.
    pub fn text_error(&self) -> Color32 {
        self.on_surfaces(self.reject)
    }

    /// What is written on a filled mark (a count on the accent, "!" on the error colour): black
    /// or white, whichever stands out more.
    pub fn ink_on(&self, fill: Color32) -> Color32 {
        let white = self.legible(Color32::WHITE, fill, 4.5) == Color32::WHITE;
        let black = self.legible(Color32::BLACK, fill, 4.5) == Color32::BLACK;
        match (white, black) {
            (true, false) => Color32::WHITE,
            (false, true) => Color32::BLACK,
            // both stand 4.5:1 or neither does: the farther from the fill
            _ if Self::lightness(fill) < 0.18 => Color32::WHITE,
            _ => Color32::BLACK,
        }
    }

    /// Relative luminance (WCAG), 0 black to 1 white.
    fn lightness(c: Color32) -> f32 {
        let linear = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    }

    /// The fill behind what is chosen (a chosen chip): the accent mixed into the
    /// panel as strongly as leaves the label colour 4.5:1 on it.
    pub fn selection_fill(&self) -> Color32 {
        const STEPS: u8 = 12;
        (0..=STEPS)
            .rev()
            .map(|step| self.chrome.lerp_to_gamma(self.accent, 0.6 * f32::from(step) / f32::from(STEPS)))
            .find(|fill| self.legible(self.text_label, *fill, 4.5) == self.text_label)
            .unwrap_or(self.chrome)
    }

    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::NULL)).unwrap_or_default()
    }
    /// The regular face at the size of the scale nearest `size` ([`TextSize::nearest`]): text
    /// asked for by a number is on the scale too. New code names the size (`t.body()`, …).
    pub fn font(&self, size: f32) -> FontId {
        self.regular(TextSize::nearest(size))
    }
    /// The semibold face at the size of the scale nearest `size`.
    pub fn semibold(&self, size: f32) -> FontId {
        self.strong(TextSize::nearest(size))
    }
    /// The regular face at a size of the scale.
    pub fn regular(&self, size: TextSize) -> FontId {
        FontId::proportional(size.points())
    }
    /// The semibold face at a size of the scale.
    pub fn strong(&self, size: TextSize) -> FontId {
        FontId::new(size.points(), FontFamily::Name(FONT_SEMIBOLD.into()))
    }
    /// Micro: a badge or a pill on a thumbnail or an icon.
    pub fn micro(&self) -> FontId {
        self.regular(TextSize::Micro)
    }
    /// [`Self::micro`] in semibold.
    pub fn micro_strong(&self) -> FontId {
        self.strong(TextSize::Micro)
    }
    /// Caption: an explanation, the caption over a field, a line of status; in semibold, a small button.
    pub fn caption(&self) -> FontId {
        self.regular(TextSize::Caption)
    }
    /// [`Self::caption`] in semibold.
    pub fn caption_strong(&self) -> FontId {
        self.strong(TextSize::Caption)
    }
    /// Label: what stands by something larger: a count, the title of a tab.
    pub fn label(&self) -> FontId {
        self.regular(TextSize::Label)
    }
    /// [`Self::label`] in semibold.
    pub fn label_strong(&self) -> FontId {
        self.strong(TextSize::Label)
    }
    /// Body: controls, rows, values and menus.
    pub fn body(&self) -> FontId {
        self.regular(TextSize::Body)
    }
    /// [`Self::body`] in semibold.
    pub fn body_strong(&self) -> FontId {
        self.strong(TextSize::Body)
    }
    /// Title: the header of a section in a panel (set strong; plain is for a value at its size).
    pub fn title(&self) -> FontId {
        self.regular(TextSize::Title)
    }
    /// [`Self::title`] in semibold.
    pub fn title_strong(&self) -> FontId {
        self.strong(TextSize::Title)
    }
    /// PanelTitle: the name of a panel or of a window (set strong).
    pub fn panel_title(&self) -> FontId {
        self.regular(TextSize::PanelTitle)
    }
    /// [`Self::panel_title`] in semibold.
    pub fn panel_title_strong(&self) -> FontId {
        self.strong(TextSize::PanelTitle)
    }
    /// Display: the title over the photos, and what an empty screen says (set strong).
    pub fn display(&self) -> FontId {
        self.regular(TextSize::Display)
    }
    /// [`Self::display`] in semibold.
    pub fn display_strong(&self) -> FontId {
        self.strong(TextSize::Display)
    }
}

pub fn install_fonts(ctx: &egui::Context) {
    install_fonts_with_chinese(ctx, None);
}

/// Install the browser's separately downloaded Chinese face alongside the embedded faces.
pub fn install_fonts_with_chinese(ctx: &egui::Context, chinese: Option<&Arc<FontData>>) {
    ctx.set_fonts(font_definitions_with_chinese(lightcraft_engine::CRAFT_FONTS, chinese));
}

/// Inter remains the default UI face. Egui's default faces and craft-fonts cover other scripts;
/// the browser's separate Chinese face precedes Japanese faces within the CJK fallback list.
/// Without craft-fonts or that separate face, CJK text shows boxes.
pub fn font_definitions(craft: &'static [lightcraft_engine::CraftFont]) -> FontDefinitions {
    font_definitions_with_chinese(craft, None)
}

/// The web build keeps the large Simplified Chinese face outside the size-limited WASM module.
/// Prefer it for Chinese UI text, including Traditional Chinese until it has its own face.
pub fn font_definitions_with_chinese(craft: &'static [lightcraft_engine::CraftFont], chinese: Option<&Arc<FontData>>) -> FontDefinitions {
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
    let (mut regular, mut bold) = (fallback("Regular"), fallback("Bold"));
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
    for f in lightcraft_engine::CRAFT_FONTS {
        if !families.contains(&f.family) {
            families.push(f.family);
        }
    }
    if chinese_loaded && !families.contains(&"Noto Sans CJK SC") {
        families.push("Noto Sans CJK SC");
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
    egui::Id::unique("lightcraft-theme")
}

impl Tokens {
    /// `color` as it is where it stands `ratio`:1 or more against `on`, as WCAG measures it (4.5
    /// for text, 3 for a line or a mark that says where something is); else moved toward the text
    /// colour, which stands furthest from every surface, in small steps until it does, and the
    /// text colour itself when nothing on the way does (or `ratio` is no number).
    ///
    /// For text and marks drawn in a token chosen for its hue or its quietness (the dim text, the
    /// accent, the caution and reject colours, a border) on a surface where a theme's value for it
    /// is too close to read: the token keeps its value for everything else that uses it.
    pub fn legible(&self, color: Color32, on: Color32, ratio: f32) -> Color32 {
        fn luminance(c: Color32) -> f32 {
            let linear = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        }
        let stands = |c: Color32| {
            let (a, b) = (luminance(c), luminance(on));
            (a.max(b) + 0.05) / (a.min(b) + 0.05) >= ratio
        };
        const STEPS: u8 = 32;
        (0..STEPS).map(|step| color.lerp_to_gamma(self.text, f32::from(step) / f32::from(STEPS))).find(|c| stands(*c)).unwrap_or(self.text)
    }
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
    v.selection.bg_fill = t.selection_fill();
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.override_text_color = Some(t.text_label);
    // (egui's own: the label colour at 60 %, under 4.1:1 in every theme, and a bright blue)
    v.weak_text_color = Some(t.text_dim);
    v.hyperlink_color = t.text_link();
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
        s.text_styles.insert(egui::TextStyle::Body, t.body());
        s.text_styles.insert(egui::TextStyle::Button, t.body());
        s.text_styles.insert(egui::TextStyle::Small, t.caption());
        s.text_styles.insert(egui::TextStyle::Heading, t.panel_title_strong());
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
    fn separate_web_chinese_face_is_available_in_every_ui_family() {
        // Simulate the WASM build, which embeds only the Japanese face. The Chinese face arrives
        // as a separate file and must precede that face when the UI uses Chinese.
        static JAPANESE: &[lightcraft_engine::CraftFont] = &[lightcraft_engine::CraftFont {
            family: "BIZ UDPGothic",
            style: "Regular",
            scripts: &["Jpan", "Latn"],
            bytes: include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
        }];
        let embedded = lightcraft_engine::CRAFT_FONTS.iter().find(|font| font.family == "Noto Sans CJK SC" && font.style == "Regular");
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
