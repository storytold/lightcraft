//! Book text: the Type panel's style turned into `lcc-text` blocks, laid out in boxes with
//! columns and vertical alignment, and drawn into rasters (preview, JPEG) or PDF pages (vector,
//! embedded fonts). Also the built-in text style presets.

use lcc_raster::Rgba8;
use lcc_text::{CharStyle, ParagraphRun, ParagraphStyle, TextAlign, TextBlock, TextEngine, TextLayout, TextRun, TextShape, Xform};

use crate::{FontStyle, HAlign, KernPair, Kerning, TextPreset, TypeStyle, VAlign};

/// Built-in text style presets.
pub fn builtin_presets() -> Vec<TextPreset> {
    let p = |name: &str, style: TypeStyle| TextPreset { name: name.into(), style };
    vec![
        p("Caption", TypeStyle { size: 9.0, color: [90, 90, 90], ..TypeStyle::default() }),
        p("Body", TypeStyle { size: 11.0, leading: Some(15.0), align: HAlign::Justify, ..TypeStyle::default() }),
        p("Title", TypeStyle { size: 30.0, style: FontStyle::Bold, align: HAlign::Center, valign: VAlign::Middle, ..TypeStyle::default() }),
        p("Headline", TypeStyle { size: 18.0, style: FontStyle::Bold, tracking: 20.0, ..TypeStyle::default() }),
        p("Quote", TypeStyle { size: 16.0, style: FontStyle::Italic, align: HAlign::Center, color: [70, 70, 70], ..TypeStyle::default() }),
        p("Spaced Caps", TypeStyle { size: 10.0, tracking: 200.0, align: HAlign::Center, ..TypeStyle::default() }),
    ]
}

/// A built-in or user preset by name.
pub fn find_preset(book: &crate::Book, name: &str) -> Option<TypeStyle> {
    book.text_presets.iter().cloned().chain(builtin_presets()).find(|p| p.name.eq_ignore_ascii_case(name)).map(|p| p.style)
}

/// Manual kerning in laid-out text: (byte offset of the left character, 1/1000 em), sorted.
pub type Kerns = Vec<(usize, f32)>;

/// Expands the tokens of `template` like [`lcc_layout::tokens::expand`] and maps manual kerning
/// pairs (character indices in `template`) to byte offsets in the expanded text.
pub fn expand_kerned(template: &str, info: &lcc_layout::tokens::PhotoInfo, pairs: &[KernPair]) -> (String, Kerns) {
    if pairs.is_empty() {
        return (lcc_layout::tokens::expand(template, info), Vec::new());
    }
    let kern = |i: usize| u32::try_from(i).ok().and_then(|i| pairs.iter().find(|k| k.at == i)).map(|k| k.amount);
    let chars: Vec<(usize, char)> = template.char_indices().collect();
    let mut out = String::with_capacity(template.len());
    let mut kerns = Vec::new();
    let mut i = 0;
    while let Some(&(b, c)) = chars.get(i) {
        let next = chars.get(i + 1).map(|x| x.1);
        // a token `{…}` (no nested `{`) or an escape `{{` / `}}` spans several characters
        let span = if (c == '{' && next == Some('{')) || (c == '}' && next == Some('}')) {
            2
        } else if c == '{' {
            match chars.iter().skip(i + 1).position(|x| x.1 == '}' || x.1 == '{') {
                Some(n) if chars.get(i + 1 + n).is_some_and(|x| x.1 == '}') => n + 2,
                _ => 1,
            }
        } else {
            1
        };
        let end = chars.get(i + span).map_or(template.len(), |x| x.0);
        let piece = template.get(b..end).unwrap_or("");
        let start = out.len();
        if span == 1 {
            out.push(c);
        } else {
            out.push_str(&lcc_layout::tokens::expand(piece, info));
        }
        let amount = if span == 2 && piece.len() == 2 && piece != "{}" { kern(i).or_else(|| kern(i + 1)) } else { kern(i + span - 1) };
        if let Some(a) = amount
            && out.len() > start
        {
            let last = out.get(start..).and_then(|t| t.char_indices().last()).map_or(start, |(o, _)| start + o);
            kerns.push((last, a));
        }
        i += span;
    }
    (out, kerns)
}

fn block(text: &str, s: &TypeStyle, kerns: &[(usize, f32)], width_px: f32, height_px: f32) -> TextBlock {
    let color =
        lcc_text::Color { r: f32::from(s.color[0]) / 255.0, g: f32::from(s.color[1]) / 255.0, b: f32::from(s.color[2]) / 255.0, alpha: s.opacity };
    let style = CharStyle {
        font_family: s.font.clone(),
        weight: if s.style.bold() { 700 } else { 400 },
        italic: s.style.italic(),
        size_pt: s.size,
        color,
        tracking: s.tracking,
        leading_pt: s.leading,
        baseline_shift_pt: s.baseline,
        kerning: match s.kerning {
            Kerning::Metrics => lcc_text::Kerning::Metrics,
            Kerning::Optical => lcc_text::Kerning::Optical,
            Kerning::Off => lcc_text::Kerning::Off,
        },
        ..CharStyle::default()
    };
    let align = match s.align {
        HAlign::Left => TextAlign::Left,
        HAlign::Center => TextAlign::Center,
        HAlign::Right => TextAlign::Right,
        HAlign::Justify => TextAlign::JustifyLeft,
    };
    // one run per kerned character, the rest in runs of the plain style
    let mut runs = Vec::new();
    let mut at = 0;
    for &(o, k) in kerns {
        let Some(len) = text.get(o..).and_then(|t| t.chars().next()).map(char::len_utf8) else { continue };
        if o < at {
            continue;
        }
        if o > at {
            runs.push(TextRun { len: o - at, style: style.clone() });
        }
        runs.push(TextRun { len, style: CharStyle { kern: k, ..style.clone() } });
        at = o + len;
    }
    if at < text.len() || runs.is_empty() {
        runs.push(TextRun { len: text.len() - at, style });
    }
    TextBlock {
        text: text.to_string(),
        font_family: s.font.clone(),
        size_pt: s.size,
        color,
        runs,
        paragraphs: vec![ParagraphRun { len: text.len(), style: ParagraphStyle { align, ..ParagraphStyle::default() } }],
        shape: TextShape::Box { x: 0.0, y: 0.0, width: width_px.max(1.0), height: height_px.max(1.0) },
        ..TextBlock::default()
    }
}

/// Text laid out for one column of a box.
pub struct Placed {
    pub layout: TextLayout,
    pub text: String,
    /// Offset of the column's text origin from the box's top-left, pixels at the layout dpi.
    pub origin: (f32, f32),
}

/// Lays `text` out in a `w`×`h` point box at `dpi`: split over `style.columns` columns (lines
/// that don't fit the last column are dropped), single columns aligned vertically.
pub fn layout_box(engine: &mut TextEngine, text: &str, style: &TypeStyle, w: f32, h: f32, dpi: f32) -> Vec<Placed> {
    layout_box_kerned(engine, text, style, &[], w, h, dpi)
}

/// [`layout_box`] with manual kerning (byte offsets into `text`, see [`expand_kerned`]).
pub fn layout_box_kerned(engine: &mut TextEngine, text: &str, style: &TypeStyle, kerns: &[(usize, f32)], w: f32, h: f32, dpi: f32) -> Vec<Placed> {
    let px = dpi / 72.0;
    if text.trim().is_empty() || !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 && px.is_finite() && px > 0.0) || style.validate().is_err() {
        return Vec::new();
    }
    let n = usize::from(style.columns.clamp(1, 6));
    let col_w = ((w - style.gutter * (n as f32 - 1.0)) / n as f32).max(1.0);
    let (cw, hp) = (col_w * px, h * px);
    let full = engine.layout(&block(text, style, kerns, cw, hp * n as f32), dpi);
    if n == 1 {
        let (top, bottom) = full.line_bounds().map_or((0.0, 0.0), |b| (b[1], b[3]));
        let free = (hp - (bottom - top)).max(0.0);
        let dy = match style.valign {
            VAlign::Top => 0.0,
            VAlign::Middle => free / 2.0,
            VAlign::Bottom => free,
        } - top.min(0.0);
        return vec![Placed { layout: full, text: text.to_string(), origin: (0.0, dy) }];
    }
    // distribute whole lines over the columns
    let mut cols: Vec<(usize, usize)> = Vec::new();
    let mut col_top = None::<f32>;
    for line in &full.lines {
        let (top, bottom) = (line.baseline - line.ascent, line.baseline + line.descent);
        let start = *col_top.get_or_insert(top);
        if bottom - start > hp && cols.last().is_some_and(|c| c.1 > c.0) {
            if cols.len() >= n {
                break;
            }
            col_top = Some(top);
            cols.push((line.range.start, line.range.end));
        } else if let Some(c) = cols.last_mut() {
            c.1 = line.range.end;
        } else {
            cols.push((line.range.start, line.range.end));
        }
    }
    cols.truncate(n);
    cols.into_iter()
        .enumerate()
        .filter_map(|(i, (a, b))| {
            let raw = text.get(a..b)?;
            let part = raw.trim_start_matches(['\n', '\r']).to_string();
            let from = a + (raw.len() - part.len());
            let local: Kerns = kerns.iter().filter(|k| k.0 >= from && k.0 < b).map(|&(o, k)| (o - from, k)).collect();
            let layout = engine.layout(&block(&part, style, &local, cw, hp), dpi);
            Some(Placed { layout, text: part, origin: ((i as f32) * (col_w + style.gutter) * px, 0.0) })
        })
        .collect()
}

/// Draws laid-out text into `img`, its box's top-left at `(bx, by)` pixels, clipped to
/// `(bx, by, bw, bh)`.
pub fn draw(img: &mut Rgba8, placed: &[Placed], bx: f32, by: f32, bw: f32, bh: f32) {
    for p in placed {
        let xf = Xform([1.0, 0.0, 0.0, 1.0, f64::from(bx + p.origin.0), f64::from(by + p.origin.1)]);
        let r = lcc_text::render::rasterize(&p.layout, &xf, lcc_text::AntiAlias::Smooth);
        let (cx0, cy0, cx1, cy1) = (bx.floor() as i64, by.floor() as i64, (bx + bw).ceil() as i64, (by + bh).ceil() as i64);
        for row in 0..r.height {
            let y = i64::from(r.y0) + row as i64;
            if y < cy0.max(0) || y >= cy1.min(img.height as i64) {
                continue;
            }
            for col in 0..r.width {
                let x = i64::from(r.x0) + col as i64;
                if x < cx0.max(0) || x >= cx1.min(img.width as i64) {
                    continue;
                }
                let i = (row * r.width + col) * 4;
                let Some(s) = r.rgba.get(i..i + 4) else { continue };
                let a = s[3].clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let Some(d) = img.data.get_mut(y as usize * img.width + x as usize) else { continue };
                for (k, dv) in d.iter_mut().take(3).enumerate() {
                    let sv = s[k].clamp(0.0, 1.0) * 255.0;
                    *dv = (sv * a + f32::from(*dv) * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
                }
                d[3] = d[3].max((a * 255.0) as u8);
            }
        }
    }
}

/// Draws laid-out text (laid out at 72 dpi) on a PDF page, its box at `rect` (points), clipped.
pub fn draw_pdf(page: &mut lcc_pdf::Page, placed: &[Placed], rect: lcc_pdf::Rect) {
    page.save();
    page.clip(lcc_pdf::Path::rect(rect));
    for p in placed {
        page.text(&p.layout, &p.text, (rect.x + f64::from(p.origin.0), rect.y + f64::from(p.origin.1)), 1.0);
    }
    page.restore();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(img: &Rgba8, x0: usize, x1: usize) -> usize {
        (0..img.height).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] < 128).count()
    }

    #[test]
    fn text_draws_and_flows_into_columns() {
        let mut e = TextEngine::new();
        let style = TypeStyle { size: 12.0, color: [0, 0, 0], ..TypeStyle::default() };
        let words = "lorem ipsum dolor sit amet ".repeat(30);
        let one = layout_box(&mut e, &words, &style, 200.0, 60.0, 72.0);
        assert_eq!(one.len(), 1);
        let two = layout_box(&mut e, &words, &TypeStyle { columns: 2, gutter: 10.0, ..style.clone() }, 200.0, 60.0, 72.0);
        assert_eq!(two.len(), 2);
        assert!(two[1].origin.0 > 100.0);
        let mut img = Rgba8::filled(200, 60, [255; 4]);
        draw(&mut img, &two, 0.0, 0.0, 200.0, 60.0);
        assert!(ink(&img, 0, 95) > 50 && ink(&img, 105, 200) > 50);
        // hostile input
        assert!(layout_box(&mut e, "x", &TypeStyle { size: f32::NAN, ..style.clone() }, 10.0, 10.0, 72.0).is_empty());
        assert!(layout_box(&mut e, "x", &style, -1.0, 10.0, 72.0).is_empty());
        assert!(layout_box(&mut e, "   ", &style, 10.0, 10.0, 72.0).is_empty());
        draw(&mut img, &one, -500.0, 1e9, 10.0, 10.0);
    }

    #[test]
    fn manual_kerning_maps_through_tokens_and_moves_glyphs() {
        let info = lcc_layout::tokens::PhotoInfo { title: "Été".into(), ..Default::default() };
        let k = |at, amount| KernPair { at, amount };
        // "A{Title}B": 0 'A', 1..=7 the token, 8 'B'; kerns after A, after the token, inside it (ignored)
        let (t, kerns) = expand_kerned("A{Title}B", &info, &[k(0, 50.0), k(7, -20.0), k(3, 99.0)]);
        assert_eq!(t, "AÉtéB");
        assert_eq!(kerns, vec![(0, 50.0), ("AÉt".len(), -20.0)]);
        // escapes, a trailing pair, an index past the end and hostile templates
        assert_eq!(expand_kerned("{{x}}", &info, &[k(1, 10.0), k(4, 5.0), k(99, 1.0)]), ("{x}".into(), vec![(0, 10.0), (2, 5.0)]));
        assert_eq!(expand_kerned("{", &info, &[k(0, 1.0)]).0, "{");
        assert_eq!(expand_kerned("a{b{c}", &info, &[k(1, 1.0)]).0, lcc_layout::tokens::expand("a{b{c}", &info));

        let mut e = TextEngine::new();
        let style = TypeStyle { size: 100.0, ..TypeStyle::default() };
        let plain = layout_box(&mut e, "HOH", &style, 1000.0, 200.0, 72.0);
        let kerned = layout_box_kerned(&mut e, "HOH", &style, &[(0, 100.0)], 1000.0, 200.0, 72.0);
        let x = |p: &[Placed], i: usize| p[0].layout.glyphs[i].x;
        assert_eq!(x(&kerned, 0), x(&plain, 0));
        assert!((x(&kerned, 1) - x(&plain, 1) - 10.0).abs() < 1e-3, "{} vs {}", x(&kerned, 1), x(&plain, 1));
        // offsets that aren't character starts are skipped, not a panic
        let bad = layout_box_kerned(&mut e, "É", &style, &[(1, 100.0), (50, 1.0)], 1000.0, 200.0, 72.0);
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn set_kern_adds_replaces_removes_and_validates() {
        let mut s = TypeStyle::default();
        s.set_kern(3, 40.0).unwrap();
        s.set_kern(1, -10.0).unwrap();
        s.set_kern(3, 60.0).unwrap();
        assert_eq!(s.kerns, vec![KernPair { at: 1, amount: -10.0 }, KernPair { at: 3, amount: 60.0 }]);
        assert_eq!(s.kern_at(3), 60.0);
        s.set_kern(1, 0.0).unwrap();
        assert_eq!(s.kerns.len(), 1);
        assert!(s.set_kern(0, f32::NAN).is_err() && s.set_kern(0, 5000.0).is_err());
        s.validate().unwrap();
        s.kerns.push(KernPair { at: 0, amount: f32::INFINITY });
        assert!(s.validate().is_err());
    }

    #[test]
    fn presets_are_valid() {
        for p in builtin_presets() {
            p.style.validate().unwrap();
        }
        let b = crate::Book::default();
        assert!(find_preset(&b, "title").is_some());
    }
}
