//! Rasterizes a [`TextLayout`] into an RGBA buffer, and extracts glyph outlines. Ported from
//! PhotoCraft's `text::render` (MIT), without its document surfaces and warp.

use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::DrawSettings;
use skrifa::{GlyphId, MetadataProvider};

use crate::layout::{GlyphOrient, PlacedGlyph, TextLayout};
use crate::raster::{Bounds, Coverage, LineSink, Pen, Xform, rect_to};
use crate::style::{AntiAlias, Color};

/// Faux-italic slant in degrees (Photoshop-like).
pub const FAUX_ITALIC_DEG: f32 = 12.0;
/// Faux-bold dilation radius as a fraction of the font size.
pub const FAUX_BOLD_RADIUS: f32 = 0.018;
/// Largest raster we produce (pixels), as a guard against absurd sizes.
const MAX_PIXELS: u64 = 256 * 1024 * 1024;

/// Rendered text: straight-alpha RGBA (0-1, sRGB as given by the styles) covering the integer
/// pixel rectangle `[x0, y0, x0 + width, y0 + height)` of the target space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rendered {
    pub x0: i32,
    pub y0: i32,
    pub width: usize,
    pub height: usize,
    /// `width * height * 4` samples.
    pub rgba: Vec<f32>,
}

impl Rendered {
    /// 8-bit straight-alpha RGBA.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.rgba.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
    }
}

/// Draws every glyph and decoration of `layout` whose style colour is `color` (or all of them
/// when `color` is `None`) into `sink`, through `transform` (text space -> sink space).
fn draw(layout: &TextLayout, transform: &Xform, sink: &mut impl LineSink, only_color: Option<&Color>) {
    for g in &layout.glyphs {
        let Some(st) = layout.styles.get(g.style as usize) else { continue };
        if only_color.is_some_and(|c| c != &st.color) {
            continue;
        }
        let Some(face) = layout.faces.get(g.face as usize) else { continue };
        let Ok(font) = skrifa::FontRef::from_index(face.font.data.as_ref(), face.font.index) else {
            continue;
        };
        let Some(outline) = font.outline_glyphs().get(GlyphId::new(g.id)) else {
            continue;
        };
        let coords: Vec<NormalizedCoord> = face.coords.iter().map(|&c| NormalizedCoord::from_bits(c)).collect();
        let glyph = glyph_xform(layout, g, face.skew_deg);
        let bold = st.faux_bold || face.embolden;
        let r = (face.size_px * FAUX_BOLD_RADIUS) as f64;
        let offsets: &[(f64, f64)] = if bold {
            &[(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (0.7, 0.7), (-0.7, -0.7), (0.7, -0.7), (-0.7, 0.7)]
        } else {
            &[(0.0, 0.0)]
        };
        for &(ox, oy) in offsets {
            let local = Xform([1.0, 0.0, 0.0, 1.0, ox * r, oy * r]).mul(&glyph);
            let settings = DrawSettings::unhinted(Size::new(face.size_px), LocationRef::new(&coords));
            let mut pen = Pen::new(&mut *sink, transform.mul(&local));
            if outline.draw(settings, &mut pen).is_ok() {
                skrifa::outline::OutlinePen::close(&mut pen);
            }
        }
    }
    for d in &layout.decorations {
        let Some(st) = layout.styles.get(d.style as usize) else { continue };
        if only_color.is_some_and(|c| c != &st.color) {
            continue;
        }
        rect_to(sink, transform, d.x0 as f64, d.y0 as f64, d.x1 as f64, d.y1 as f64);
    }
}

/// Font units (scaled to px, y up) → text space for a placed glyph: scales, faux italic,
/// baseline shift, and the vertical-type orientation.
pub fn glyph_xform(layout: &TextLayout, g: &PlacedGlyph, face_skew_deg: f32) -> Xform {
    let Some(st) = layout.styles.get(g.style as usize) else { return Xform([1.0, 0.0, 0.0, -1.0, g.x as f64, g.y as f64]) };
    let hs = if st.horizontal_scale > 0.0 { st.horizontal_scale } else { 1.0 } as f64;
    let vs = if st.vertical_scale > 0.0 { st.vertical_scale } else { 1.0 } as f64;
    let skew_deg = if st.faux_italic { FAUX_ITALIC_DEG } else { 0.0 } + face_skew_deg;
    let skew = (skew_deg as f64).to_radians().tan() * vs;
    let shift = (st.baseline_shift_pt * layout.px_per_pt) as f64;
    let (x, y) = (g.x as f64, g.y as f64);
    match g.orient {
        GlyphOrient::Horizontal => Xform([hs, 0.0, skew, -vs, x, y - shift]),
        // Baseline shift moves upright glyphs in vertical type to the right.
        GlyphOrient::Upright => Xform([hs, 0.0, skew, -vs, x + shift, y]),
        // 90° clockwise: the glyph's up direction (and its baseline shift) points right.
        GlyphOrient::Rotated => Xform([0.0, 1.0, -1.0, 0.0, x, y]).mul(&Xform([hs, 0.0, skew, -vs, 0.0, -shift])),
    }
}

/// Integer pixel rectangle `[x0, y0, x1, y1]` around float bounds (1 px margin), or `None` when
/// empty, non-finite or larger than [`MAX_PIXELS`] on a side.
fn rect_from_bounds([x0, y0, x1, y1]: [f64; 4]) -> Option<[i32; 4]> {
    if ![x0, y0, x1, y1].into_iter().all(f64::is_finite) {
        return None;
    }
    let min = f64::from(i32::MIN);
    let max = f64::from(i32::MAX);
    let (x0, y0, x1, y1) = (
        (x0.floor() - 1.0).clamp(min, max),
        (y0.floor() - 1.0).clamp(min, max),
        (x1.ceil() + 1.0).clamp(min, max),
        (y1.ceil() + 1.0).clamp(min, max),
    );
    if x1 <= x0 || y1 <= y0 || x1 - x0 > MAX_PIXELS as f64 || y1 - y0 > MAX_PIXELS as f64 {
        return None;
    }
    Some([x0 as i32, y0 as i32, x1 as i32, y1 as i32])
}

/// Bounds of the drawn text through `transform` (integer pixel rectangle `[x0, y0, x1, y1]`).
pub fn ink_rect(layout: &TextLayout, transform: &Xform) -> Option<[i32; 4]> {
    let mut b = Bounds::default();
    draw(layout, transform, &mut b, None);
    b.rect.and_then(rect_from_bounds)
}

/// Rasterizes `layout` through `transform` (text space -> target pixels).
pub fn rasterize(layout: &TextLayout, transform: &Xform, antialias: AntiAlias) -> Rendered {
    let Some([x0, y0, x1, y1]) = ink_rect(layout, transform) else { return Rendered::default() };
    let (w, h) = ((i64::from(x1) - i64::from(x0)) as usize, (i64::from(y1) - i64::from(y0)) as usize);
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return Rendered::default();
    }
    // Premultiplied accumulation.
    let mut acc = vec![0.0f32; w * h * 4];
    let xf = Xform([1.0, 0.0, 0.0, 1.0, -f64::from(x0), -f64::from(y0)]).mul(transform);
    let mut colors: Vec<Color> = Vec::new();
    for st in &layout.styles {
        if !colors.contains(&st.color) {
            colors.push(st.color);
        }
    }
    for c in &colors {
        let mut cov = Coverage::new(w, h);
        draw(layout, &xf, &mut cov, Some(c));
        let cov = cov.finish();
        let comps = [c.r, c.g, c.b];
        let a = c.alpha.clamp(0.0, 1.0);
        for (px, &cv) in acc.chunks_exact_mut(4).zip(cov.iter()) {
            let cv = if antialias == AntiAlias::None { if cv >= 0.5 { 1.0 } else { 0.0 } } else { cv };
            let s = cv * a;
            if s <= 0.0 {
                continue;
            }
            let keep = 1.0 - s;
            for (v, comp) in px.iter_mut().zip(comps) {
                *v = comp * s + *v * keep;
            }
            if let Some(alpha) = px.get_mut(3) {
                *alpha = s + *alpha * keep;
            }
        }
    }
    // Unpremultiply.
    for px in acc.chunks_exact_mut(4) {
        let a = px.get(3).copied().unwrap_or(0.0);
        if a > 0.0 {
            for v in px.iter_mut().take(3) {
                *v = (*v / a).clamp(0.0, 1.0);
            }
        }
    }
    Rendered { x0, y0, width: w, height: h, rgba: acc }
}

/// One element of a glyph outline in document space (see [`outlines`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEl {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    QuadTo([f64; 2], [f64; 2]),
    CurveTo([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// Records outline commands through a text->target map.
struct Recorder {
    glyph: Xform,
    post: Xform,
    out: Vec<PathEl>,
}

impl Recorder {
    fn map(&self, x: f32, y: f32) -> [f64; 2] {
        let (tx, ty) = self.glyph.apply(f64::from(x), f64::from(y));
        let (dx, dy) = self.post.apply(tx, ty);
        [dx, dy]
    }
}

impl skrifa::outline::OutlinePen for Recorder {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.out.push(PathEl::MoveTo(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.out.push(PathEl::LineTo(p));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx0, cy0), self.map(x, y));
        self.out.push(PathEl::QuadTo(c, p));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.out.push(PathEl::CurveTo(a, b, p));
    }
    fn close(&mut self) {
        self.out.push(PathEl::Close);
    }
}

/// Glyph outlines of `layout` through `transform` (text space -> target), one element list per
/// glyph (vector output, e.g. text drawn as paths in a PDF). Faux bold is not applied; faux
/// italic and scaling are.
pub fn outlines(layout: &TextLayout, transform: &Xform) -> Vec<Vec<PathEl>> {
    let mut glyphs = Vec::new();
    for g in &layout.glyphs {
        let Some(face) = layout.faces.get(g.face as usize) else { continue };
        let Ok(font) = skrifa::FontRef::from_index(face.font.data.as_ref(), face.font.index) else {
            continue;
        };
        let Some(outline) = font.outline_glyphs().get(GlyphId::new(g.id)) else {
            continue;
        };
        let coords: Vec<NormalizedCoord> = face.coords.iter().map(|&c| NormalizedCoord::from_bits(c)).collect();
        let glyph = glyph_xform(layout, g, face.skew_deg);
        let mut rec = Recorder { glyph, post: *transform, out: Vec::new() };
        let settings = DrawSettings::unhinted(Size::new(face.size_px), LocationRef::new(&coords));
        if outline.draw(settings, &mut rec).is_ok() && !rec.out.is_empty() {
            glyphs.push(rec.out);
        }
    }
    glyphs
}

#[cfg(test)]
mod tests {
    use super::{MAX_PIXELS, rect_from_bounds};

    #[test]
    fn bounds_are_clipped_without_overflow_and_oversized_rectangles_rejected() {
        let min = f64::from(i32::MIN);
        let max = f64::from(i32::MAX);
        let cases = [
            ([min, 0.0, min + 8.0, 8.0], Some([i32::MIN, -1, i32::MIN + 9, 9])),
            ([max - 8.0, 0.0, max, 8.0], Some([i32::MAX - 9, -1, i32::MAX, 9])),
            ([-f64::MAX, 0.0, f64::MAX, 8.0], None),
            ([0.0, 0.0, MAX_PIXELS as f64, 8.0], None),
            ([0.25, -3.25, 10.1, 8.8], Some([-1, -5, 12, 10])),
        ];
        for (bounds, expected) in cases {
            assert_eq!(rect_from_bounds(bounds), expected, "bounds: {bounds:?}");
        }
    }
}
