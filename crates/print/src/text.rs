//! Shaped text for layout cells: a [`TextRenderer`] over `dac-text` (HarfRust shaping, bidi,
//! line breaking, CJK fallbacks), and the same layout for vector PDF text, so a print preview
//! and its PDF wrap lines identically.

use std::sync::{Mutex, PoisonError};

use dac_layout::render::TextRenderer;
use dac_layout::{Align, TextStyle};
use dac_raster::Rgba8;
use dac_text::{CharStyle, Color, ParagraphRun, ParagraphStyle, TextAlign, TextBlock, TextEngine, TextLayout, TextRun, TextShape, Xform};

/// The text block for `text` in `style`, wrapped in a `w`×`h` box of text-space pixels.
pub fn block(text: &str, style: &TextStyle, w: f32, h: f32) -> TextBlock {
    let c = style.color;
    let color = Color { r: f32::from(c[0]) / 255.0, g: f32::from(c[1]) / 255.0, b: f32::from(c[2]) / 255.0, alpha: f32::from(c[3]) / 255.0 };
    let size = if style.size.is_finite() { style.size.clamp(1.0, 1000.0) } else { 10.0 };
    let family = style.family.clone().unwrap_or_default();
    let char_style = CharStyle {
        font_family: family.clone(),
        size_pt: size,
        color,
        weight: if style.bold { 700 } else { 400 },
        italic: style.italic,
        ..CharStyle::default()
    };
    let align = match style.align {
        Align::Left => TextAlign::Left,
        Align::Center => TextAlign::Center,
        Align::Right => TextAlign::Right,
    };
    let fin = |v: f32| if v.is_finite() { v.max(1.0) } else { 1.0 };
    TextBlock {
        text: text.to_string(),
        font_family: family,
        size_pt: size,
        color,
        runs: vec![TextRun { len: text.len(), style: char_style }],
        paragraphs: vec![ParagraphRun { len: text.len(), style: ParagraphStyle { align, ..ParagraphStyle::default() } }],
        shape: TextShape::Box { x: 0.0, y: 0.0, width: fin(w), height: fin(h) },
        ..TextBlock::default()
    }
}

/// A shared type engine. `TextRenderer::render` takes `&self`, so the engine (which caches
/// fonts and shaping state) sits behind a mutex.
pub struct ShapedText {
    engine: Mutex<TextEngine>,
}

impl Default for ShapedText {
    fn default() -> Self {
        Self::new()
    }
}

impl ShapedText {
    /// Bundled fonts (Inter, plus the craft-fonts CJK faces when built with them): the same
    /// output everywhere.
    pub fn new() -> Self {
        Self { engine: Mutex::new(TextEngine::new()) }
    }

    /// Bundled plus installed system fonts (desktop), so a family named in a template resolves.
    pub fn with_system_fonts() -> Self {
        Self { engine: Mutex::new(TextEngine::with_system_fonts()) }
    }

    /// Lays out `text` in a `w`×`h` point box (text space = points).
    pub fn layout_pt(&self, text: &str, style: &TextStyle, w: f32, h: f32) -> TextLayout {
        let mut e = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
        e.layout(&block(text, style, w, h), 72.0)
    }
}

impl TextRenderer for ShapedText {
    fn render(&self, text: &str, style: &TextStyle, w: usize, h: usize, px_per_pt: f32) -> Result<Rgba8, String> {
        if !(px_per_pt.is_finite() && px_per_pt > 0.0) {
            return Err("invalid text scale".into());
        }
        if w == 0 || h == 0 || w.saturating_mul(h) > dac_layout::render::MAX_PIXELS {
            return Err("invalid text box".into());
        }
        let dpi = px_per_pt * 72.0;
        let r = {
            let mut e = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
            let b = block(text, style, w as f32, h as f32);
            e.render(&b, dpi, &Xform::IDENTITY).1
        };
        let mut out = Rgba8::new(w, h);
        let px = r.to_rgba8();
        for y in 0..r.height {
            let ty = i64::from(r.y0) + y as i64;
            if ty < 0 || ty >= h as i64 {
                continue;
            }
            for x in 0..r.width {
                let tx = i64::from(r.x0) + x as i64;
                if tx < 0 || tx >= w as i64 {
                    continue;
                }
                let i = (y * r.width + x) * 4;
                let Some(s) = px.get(i..i + 4) else { continue };
                if let Some(d) = out.data.get_mut(ty as usize * w + tx as usize) {
                    *d = [s[0], s[1], s[2], s[3]];
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_ink_inside_the_box_and_aligns() {
        let t = ShapedText::new();
        let style = TextStyle { size: 20.0, ..TextStyle::default() };
        let img = t.render("Hello", &style, 200, 40, 1.0).unwrap();
        let ink: Vec<usize> = (0..200).filter(|&x| (0..40).any(|y| img.get(x, y)[3] > 128)).collect();
        assert!(!ink.is_empty());
        assert!(*ink.first().unwrap() < 20, "left aligned");
        let right = t.render("Hello", &TextStyle { align: Align::Right, ..style.clone() }, 200, 40, 1.0).unwrap();
        let rink: Vec<usize> = (0..200).filter(|&x| (0..40).any(|y| right.get(x, y)[3] > 128)).collect();
        assert!(*rink.last().unwrap() > 180, "right aligned");
        // double the scale: twice the ink width
        let big = t.render("Hello", &style, 400, 80, 2.0).unwrap();
        let bink = (0..400).filter(|&x| (0..80).any(|y| big.get(x, y)[3] > 128)).count();
        assert!(bink > ink.len() * 3 / 2);
    }

    #[test]
    fn hostile_sizes_are_errors_not_panics() {
        let t = ShapedText::new();
        let s = TextStyle { size: f32::NAN, ..TextStyle::default() };
        assert!(t.render("x", &s, 10, 10, f32::INFINITY).is_err());
        assert!(t.render("x", &s, 0, 10, 1.0).is_err());
        assert!(t.render("x", &s, 10, 10, 1.0).is_ok());
        let l = t.layout_pt("a b c", &TextStyle { size: 1e9, ..s }, f32::NAN, -5.0);
        let _ = l.glyphs.len();
    }
}
