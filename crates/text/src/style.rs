//! Typed style model for a block of text: character runs, paragraph runs, point/box shape and
//! orientation. Ported from PhotoCraft's `doc::text` (MIT); pure data.
//!
//! Units follow Photoshop's Character/Paragraph panels: sizes, leading, indents and spacing are in
//! **points** (converted to pixels with the document resolution: `px = pt * dpi / 72`), tracking
//! is in 1/1000 em, and box bounds are in text-space **pixels** (before the layer transform).
//!
//! Runs cover the text by **UTF-8 byte length**, in order. A run list whose lengths don't add up
//! to `text.len()` is normalised by [`TextBlock::char_runs`] (the last run is stretched or
//! truncated), so edits never produce unstyled text.

use serde::{Deserialize, Serialize};

/// Kerning mode (Photoshop: Metrics, Optical, 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kerning {
    /// Font kerning (`kern` feature).
    #[default]
    Metrics,
    /// Photoshop's optical kerning: pair spacing computed from the glyph outlines.
    Optical,
    /// No automatic kerning (Photoshop's "0" / manual kerning).
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Caps {
    #[default]
    Normal,
    SmallCaps,
    AllCaps,
}

/// An OpenType feature setting, e.g. `{ tag: "ss01", value: 1 }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontFeature {
    pub tag: String,
    pub value: u16,
}

/// A variable-font axis setting, e.g. `{ axis: "wght", value: 650.0 }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontVariation {
    pub axis: String,
    pub value: f32,
}

/// Character style (Photoshop's Character panel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharStyle {
    /// Family name ("Inter"). Empty = the default family.
    pub font_family: String,
    /// Face style name as shown in the UI ("Bold Italic"); informational, `weight`/`italic` rule.
    pub font_style: String,
    /// PostScript name from a PSD (`ArialMT`), used to find the exact face when present.
    pub postscript_name: Option<String>,
    /// CSS-style weight, 100–900.
    pub weight: u16,
    pub italic: bool,
    pub size_pt: f32,
    pub color: Color,
    /// Letter spacing in 1/1000 em.
    pub tracking: f32,
    /// Baseline-to-baseline distance in points; `None` = auto (paragraph `auto_leading` × size).
    pub leading_pt: Option<f32>,
    /// Positive raises the text.
    pub baseline_shift_pt: f32,
    pub horizontal_scale: f32,
    pub vertical_scale: f32,
    pub underline: bool,
    pub strikethrough: bool,
    pub faux_bold: bool,
    pub faux_italic: bool,
    /// Automatic (pair) kerning mode.
    pub kerning: Kerning,
    /// Manual kerning in 1/1000 em between this character and the next one (Photoshop stores it
    /// per character run as `Kerning`). Added to the automatic kerning of the pair.
    pub kern: f32,
    pub caps: Caps,
    /// Standard ligatures (`liga`/`clig`).
    pub ligatures: bool,
    /// Discretionary ligatures (`dlig`).
    pub discretionary_ligatures: bool,
    /// Extra OpenType features (`ss01`, `onum`, `tnum`, …).
    pub features: Vec<FontFeature>,
    /// Variable-font axis values.
    pub variations: Vec<FontVariation>,
    /// BCP-47 language tag (affects shaping and line breaking).
    pub language: Option<String>,
}

impl Default for CharStyle {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_style: String::new(),
            postscript_name: None,
            weight: 400,
            italic: false,
            size_pt: 12.0,
            color: Color::BLACK,
            tracking: 0.0,
            leading_pt: None,
            baseline_shift_pt: 0.0,
            horizontal_scale: 1.0,
            vertical_scale: 1.0,
            underline: false,
            strikethrough: false,
            faux_bold: false,
            faux_italic: false,
            kerning: Kerning::Metrics,
            kern: 0.0,
            caps: Caps::Normal,
            ligatures: true,
            discretionary_ligatures: false,
            features: Vec::new(),
            variations: Vec::new(),
            language: None,
        }
    }
}

/// A character style applied to `len` UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    pub len: usize,
    pub style: CharStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    /// Justified, last line left.
    JustifyLeft,
    /// Justified, last line centered.
    JustifyCenter,
    /// Justified, last line right.
    JustifyRight,
    /// Justified including the last line.
    JustifyAll,
}

impl TextAlign {
    pub fn is_justified(self) -> bool {
        matches!(self, Self::JustifyLeft | Self::JustifyCenter | Self::JustifyRight | Self::JustifyAll)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextDirection {
    /// From the first strong character (Unicode bidi rules).
    #[default]
    Auto,
    Ltr,
    Rtl,
}

/// Paragraph style (Photoshop's Paragraph panel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParagraphStyle {
    pub align: TextAlign,
    pub first_line_indent_pt: f32,
    pub start_indent_pt: f32,
    pub end_indent_pt: f32,
    pub space_before_pt: f32,
    pub space_after_pt: f32,
    /// Auto leading as a multiple of the font size (Photoshop default 1.2).
    pub auto_leading: f32,
    pub direction: TextDirection,
    pub hyphenate: bool,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            align: TextAlign::Left,
            first_line_indent_pt: 0.0,
            start_indent_pt: 0.0,
            end_indent_pt: 0.0,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            auto_leading: 1.2,
            direction: TextDirection::Auto,
            hyphenate: false,
        }
    }
}

/// A paragraph style applied to `len` UTF-8 bytes (whole paragraphs, including the break).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphRun {
    pub len: usize,
    pub style: ParagraphStyle,
}

/// Point text grows from its anchor (text-space origin = first baseline at the anchor);
/// paragraph (box) text wraps inside a rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum TextShape {
    #[default]
    Point,
    /// Text-space pixels.
    Box { x: f32, y: f32, width: f32, height: f32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    #[default]
    Horizontal,
    /// Vertical type (tategaki): columns top to bottom, advancing right to left.
    Vertical,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntiAlias {
    None,
    Sharp,
    Crisp,
    Strong,
    #[default]
    Smooth,
    /// Photoshop's "Windows" (platform grayscale) anti-aliasing; rendered like `Sharp`.
    Windows,
    /// Photoshop's "Windows LCD" (platform subpixel) anti-aliasing; rendered like `Sharp` (we
    /// never produce colour-fringed subpixel text in document pixels).
    WindowsLcd,
}

/// Straight (non-premultiplied) sRGB colour with alpha, components 0–1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub alpha: f32,
}

impl Color {
    pub const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, alpha: 1.0 };
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, alpha: 1.0 };
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, alpha: 1.0 }
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::BLACK
    }
}

/// A block of styled text (PhotoCraft's `TextLayer` without the document parts): what the
/// layout engine lays out. Units: sizes in points, box in text-space pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextBlock {
    pub text: String,
    /// Summary style used when `runs` is empty.
    pub font_family: String,
    pub size_pt: f32,
    pub color: Color,
    /// Character style runs. Empty = one run built from `font_family`, `size_pt` and `color`.
    pub runs: Vec<TextRun>,
    /// Paragraph style runs. Empty = default paragraph style.
    pub paragraphs: Vec<ParagraphRun>,
    pub shape: TextShape,
    pub orientation: Orientation,
    pub antialias: AntiAlias,
}

impl Default for TextBlock {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: String::new(),
            size_pt: 12.0,
            color: Color::BLACK,
            runs: Vec::new(),
            paragraphs: Vec::new(),
            shape: TextShape::Point,
            orientation: Orientation::Horizontal,
            antialias: AntiAlias::Smooth,
        }
    }
}

impl TextBlock {
    /// One run of plain text.
    pub fn plain(text: impl Into<String>, font_family: impl Into<String>, size_pt: f32) -> Self {
        Self { text: text.into(), font_family: font_family.into(), size_pt, ..Default::default() }
    }

    /// Character runs covering exactly `text.len()` bytes, each on a char boundary.
    pub fn char_runs(&self) -> Vec<TextRun> {
        let base = CharStyle { font_family: self.font_family.clone(), size_pt: self.size_pt, color: self.color, ..Default::default() };
        normalize_runs(&self.text, self.runs.iter().map(|r| (r.len, r.style.clone())).collect(), base)
            .into_iter()
            .map(|(len, style)| TextRun { len, style })
            .collect()
    }

    /// Paragraph runs covering exactly `text.len()` bytes.
    pub fn paragraph_runs(&self) -> Vec<ParagraphRun> {
        normalize_runs(&self.text, self.paragraphs.iter().map(|r| (r.len, r.style.clone())).collect(), ParagraphStyle::default())
            .into_iter()
            .map(|(len, style)| ParagraphRun { len, style })
            .collect()
    }
}

/// Stretches/truncates `(len, style)` runs to cover `text` exactly, snapping to char boundaries
/// and dropping empty runs (keeps one run for empty text).
fn normalize_runs<S: Clone>(text: &str, runs: Vec<(usize, S)>, base: S) -> Vec<(usize, S)> {
    let total = text.len();
    if total == 0 {
        return vec![(0, runs.into_iter().next().map_or(base, |r| r.1))];
    }
    let mut out: Vec<(usize, S)> = Vec::new();
    let mut at = 0usize;
    for (len, style) in runs {
        if at >= total {
            break;
        }
        let mut end = at.saturating_add(len).min(total);
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end > at {
            out.push((end - at, style));
            at = end;
        }
    }
    if at < total {
        match out.last_mut() {
            Some(last) => last.0 += total - at,
            None => out.push((total, base.clone())),
        }
    }
    if out.is_empty() {
        out.push((0, base));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(len: usize, size: f32) -> TextRun {
        TextRun { len, style: CharStyle { size_pt: size, ..Default::default() } }
    }

    #[test]
    fn runs_are_normalized_to_the_text() {
        let base = TextBlock { text: "héllo".into(), size_pt: 9.0, ..Default::default() };
        let r = base.char_runs();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].len, r[0].style.size_pt), (6, 9.0));
        let t = TextBlock { runs: vec![run(2, 10.0), run(1, 20.0)], ..base.clone() };
        let r = t.char_runs();
        assert_eq!(r.iter().map(|r| r.len).collect::<Vec<_>>(), vec![3, 3]);
        let t = TextBlock { runs: vec![run(0, 1.0), run(100, 10.0), run(5, 20.0)], ..base.clone() };
        let r = t.char_runs();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].len, r[0].style.size_pt), (6, 10.0));
        let t = TextBlock { text: String::new(), runs: vec![run(0, 33.0)], ..base.clone() };
        assert_eq!(t.char_runs()[0].style.size_pt, 33.0);
        assert_eq!(t.paragraph_runs().len(), 1);
    }

    #[test]
    fn styles_serialize() {
        let s = CharStyle { features: vec![FontFeature { tag: "ss01".into(), value: 1 }], ..Default::default() };
        let v = serde_json::to_value(&s).unwrap();
        let back: CharStyle = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
        let p: ParagraphStyle = serde_json::from_str(r#"{"align":"Center"}"#).unwrap();
        assert_eq!((p.align, p.auto_leading), (TextAlign::Center, 1.2));
    }
}
