//! Raster rendering of one page.
//!
//! Photos come from a [`PhotoSource`] (the engine implements it with its render/export pipeline,
//! so prints match exports); text from a [`TextRenderer`]. Until the shaping `text` crate is
//! plugged in, [`PlaceholderText`] draws each line as a bar of the text's colour, sized from the
//! font size and character count, which is enough for layout previews and tests.
//!
//! Resolution independence: everything is placed in points and scaled by `dpi / 72`, so a preview
//! at 50 dpi and a print at 300 dpi show the same layout.

use dac_raster::Rgba8;

use crate::tokens::{PhotoInfo, expand};
use crate::{Align, Cell, CellKind, Color, Fit, GraphicCell, LayoutError, Page, PageLayout, PhotoCell, Rect, Result, Shape, Stroke, TextStyle};

/// Largest rendered page (pixels): 600 MP, a 40×50 in print at 300 dpi.
pub const MAX_PIXELS: usize = 600_000_000;

/// Supplies developed photos and their metadata.
pub trait PhotoSource {
    /// The developed photo, oriented, in sRGB, with its long side at least `long_side` pixels when
    /// the original is that large (a smaller image is upscaled by the renderer).
    fn image(&self, photo: &str, long_side: usize) -> std::result::Result<Rgba8, String>;
    /// Metadata for token text.
    fn info(&self, _photo: &str) -> PhotoInfo {
        PhotoInfo::default()
    }
}

/// Draws a text cell's (already expanded) text.
pub trait TextRenderer {
    /// A `w`×`h` RGBA image (straight alpha) of `text` in `style`, `px_per_pt` pixels per point;
    /// transparent where there's no ink.
    fn render(&self, text: &str, style: &TextStyle, w: usize, h: usize, px_per_pt: f32) -> std::result::Result<Rgba8, String>;
}

/// Bars in place of glyphs (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaceholderText;

impl TextRenderer for PlaceholderText {
    fn render(&self, text: &str, style: &TextStyle, w: usize, h: usize, k: f32) -> std::result::Result<Rgba8, String> {
        let mut img = Rgba8::new(w, h);
        let line_h = (style.size * 1.2 * k).max(1.0);
        let bar_h = (style.size * 0.55 * k).max(1.0);
        let ink = [style.color[0], style.color[1], style.color[2], (style.color[3] as f32 * 0.7) as u8];
        for (i, line) in text.lines().enumerate() {
            let top = i as f32 * line_h + (line_h - bar_h) / 2.0;
            if top >= h as f32 {
                break;
            }
            let chars = line.chars().filter(|c| !c.is_control()).count() as f32;
            let bw = (chars * style.size * 0.5 * k).min(w as f32);
            if bw <= 0.0 {
                continue;
            }
            let x0 = match style.align {
                Align::Left => 0.0,
                Align::Center => (w as f32 - bw) / 2.0,
                Align::Right => w as f32 - bw,
            };
            fill_rect(&mut img, x0, top, x0 + bw, top + bar_h, ink);
        }
        Ok(img)
    }
}

/// Render settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// Pixels per inch.
    pub dpi: f32,
    /// Render the bleed area too (otherwise the trim box only).
    pub include_bleed: bool,
    /// Draw margins, cell outlines and guides (on-screen preview).
    pub show_guides: bool,
    /// Page number (1-based) and count, for `{Page}`/`{Pages}`.
    pub page: usize,
    pub pages: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { dpi: 72.0, include_bleed: false, show_guides: false, page: 1, pages: 1 }
    }
}

/// A rendered page and what went wrong on the way (a missing photo is drawn as a grey slot, not
/// an error).
#[derive(Clone, Debug)]
pub struct Rendered {
    pub image: Rgba8,
    pub warnings: Vec<String>,
}

const EMPTY_SLOT: Color = [200, 200, 200, 255];

/// Renders `layout` on `page`.
pub fn render_page(page: &Page, layout: &PageLayout, photos: &dyn PhotoSource, text: &dyn TextRenderer, opt: &RenderOptions) -> Result<Rendered> {
    page.validate()?;
    layout.validate()?;
    if !(opt.dpi.is_finite() && opt.dpi > 0.0 && opt.dpi <= 2400.0) {
        return Err(LayoutError::Invalid("dpi must be between 0 and 2400".into()));
    }
    let k = opt.dpi / crate::PT_PER_INCH;
    let area = if opt.include_bleed { page.bleed_box() } else { page.trim() };
    let w = (area.w * k).round().max(1.0) as usize;
    let h = (area.h * k).round().max(1.0) as usize;
    if w.saturating_mul(h) > MAX_PIXELS {
        return Err(LayoutError::Render(format!("{w}×{h} pixels is too large")));
    }
    // page point → canvas pixel
    let map = |r: &Rect| Rect::new((r.x - area.x) * k, (r.y - area.y) * k, r.w * k, r.h * k);
    let mut img = Rgba8::filled(w, h, page.background);
    let mut warnings = Vec::new();
    let mut infos: Vec<Option<PhotoInfo>> = Vec::with_capacity(layout.cells.len());
    for c in &layout.cells {
        infos.push(c.as_photo().and_then(|p| p.photo.as_deref()).map(|id| {
            let mut i = photos.info(id);
            i.page = opt.page;
            i.pages = opt.pages;
            i
        }));
    }
    for (idx, cell) in layout.cells.iter().enumerate() {
        let r = map(&cell.rect);
        match &cell.kind {
            CellKind::Photo(p) => draw_photo(&mut img, r, p, photos, k, &mut warnings),
            CellKind::Graphic(g) => draw_graphic(&mut img, r, g, k),
            CellKind::Text(t) => {
                let src = t.source.and_then(|s| infos.get(s).cloned().flatten()).or_else(|| infos.iter().flatten().next().cloned());
                let info = src.unwrap_or_else(|| PhotoInfo { page: opt.page, pages: opt.pages, ..PhotoInfo::default() });
                let s = expand(&t.text, &info);
                let (tw, th) = (r.w.round().max(0.0) as usize, r.h.round().max(0.0) as usize);
                if tw == 0 || th == 0 || s.is_empty() {
                    continue;
                }
                match text.render(&s, &t.style, tw, th, k) {
                    Ok(t) => blit(&mut img, &t, r.x.round() as isize, r.y.round() as isize),
                    Err(e) => warnings.push(format!("cell {idx}: text: {e}")),
                }
            }
        }
    }
    if opt.show_guides {
        let guide = [0, 170, 255, 200];
        stroke_rect(&mut img, map(&page.content()), 1.0, guide);
        for c in &layout.cells {
            stroke_rect(&mut img, map(&c.rect), 1.0, [128, 128, 128, 160]);
        }
        for g in &layout.guides {
            let (gx, gy) = match *g {
                crate::Guide::Vertical(x) => (Some((x - area.x) * k), None),
                crate::Guide::Horizontal(y) => (None, Some((y - area.y) * k)),
            };
            if let Some(x) = gx {
                fill_rect(&mut img, x, 0.0, x + 1.0, h as f32, [255, 0, 170, 200]);
            }
            if let Some(y) = gy {
                fill_rect(&mut img, 0.0, y, w as f32, y + 1.0, [255, 0, 170, 200]);
            }
        }
    }
    Ok(Rendered { image: img, warnings })
}

/// Where the photo lands in a cell: (scale, left, top) in pixels, for an `iw`×`ih` image.
pub fn photo_placement(cell: Rect, iw: f32, ih: f32, p: &PhotoCell) -> (f32, f32, f32) {
    let base = match p.fit {
        Fit::Fit => (cell.w / iw).min(cell.h / ih),
        Fit::Fill => (cell.w / iw).max(cell.h / ih),
    };
    let zoom = if p.zoom.is_finite() { p.zoom.clamp(1.0, 20.0) } else { 1.0 };
    let s = base * zoom;
    let (dw, dh) = (iw * s, ih * s);
    let pan = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
    // pan −1 shows the left/top end of an overflowing photo; letterboxed axes stay centred
    let ox = cell.x + (cell.w - dw) / 2.0 - pan(p.pan[0]) * (dw - cell.w).max(0.0) / 2.0;
    let oy = cell.y + (cell.h - dh) / 2.0 - pan(p.pan[1]) * (dh - cell.h).max(0.0) / 2.0;
    (s, ox, oy)
}

fn draw_photo(img: &mut Rgba8, r: Rect, p: &PhotoCell, photos: &dyn PhotoSource, k: f32, warnings: &mut Vec<String>) {
    let Some(id) = p.photo.as_deref() else {
        fill_rect(img, r.x, r.y, r.right(), r.bottom(), EMPTY_SLOT);
        return;
    };
    let zoom = if p.zoom.is_finite() { p.zoom.clamp(1.0, 20.0) } else { 1.0 };
    let long = ((r.w.max(r.h) * zoom).ceil().max(1.0) as usize).min(16_384);
    let src = match photos.image(id, long) {
        Ok(i) if i.width > 0 && i.height > 0 && i.data.len() == i.width.saturating_mul(i.height) => i,
        Ok(_) => {
            warnings.push(format!("photo {id}: empty image"));
            fill_rect(img, r.x, r.y, r.right(), r.bottom(), EMPTY_SLOT);
            return;
        }
        Err(e) => {
            warnings.push(format!("photo {id}: {e}"));
            fill_rect(img, r.x, r.y, r.right(), r.bottom(), EMPTY_SLOT);
            return;
        }
    };
    let mut turns = p.rotate % 4;
    if p.rotate_to_fit && ((src.width > src.height) != (r.w > r.h)) && src.width != src.height {
        turns = (turns + 1) % 4;
    }
    let src = match turns {
        1 => src.rotate_cw(),
        2 => src.rotate_180(),
        3 => src.rotate_180().rotate_cw(),
        _ => src,
    };
    let (iw, ih) = (src.width as f32, src.height as f32);
    let (s, ox, oy) = photo_placement(r, iw, ih, p);
    if !(s.is_finite() && s > 0.0) {
        return;
    }
    // visible = displayed photo ∩ cell ∩ canvas
    let vx0 = r.x.max(ox).max(0.0);
    let vy0 = r.y.max(oy).max(0.0);
    let vx1 = r.right().min(ox + iw * s).min(img.width as f32);
    let vy1 = r.bottom().min(oy + ih * s).min(img.height as f32);
    let (x0, y0, x1, y1) = (vx0.round() as usize, vy0.round() as usize, vx1.round().max(0.0) as usize, vy1.round().max(0.0) as usize);
    for y in y0..y1 {
        let v = (y as f32 + 0.5 - oy) / s;
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - ox) / s;
            let px = sample(&src, u, v);
            blend(img, x, y, px);
        }
    }
    if let Some(st) = p.stroke {
        draw_stroke(img, Rect::new(vx0, vy0, vx1 - vx0, vy1 - vy0), st, k);
    }
}

fn draw_stroke(img: &mut Rgba8, r: Rect, st: Stroke, k: f32) {
    if st.width > 0.0 {
        stroke_rect(img, r, (st.width * k).max(1.0), st.color);
    }
}

fn draw_graphic(img: &mut Rgba8, r: Rect, g: &GraphicCell, k: f32) {
    match g.shape {
        Shape::Rect => {
            if let Some(f) = g.fill {
                fill_rect(img, r.x, r.y, r.right(), r.bottom(), f);
            }
            if let Some(st) = g.stroke {
                draw_stroke(img, r, st, k);
            }
        }
        Shape::Ellipse => {
            let (cx, cy) = r.center();
            let (rx, ry) = (r.w / 2.0, r.h / 2.0);
            if rx <= 0.0 || ry <= 0.0 {
                return;
            }
            let sw = g.stroke.map(|s| (s.width * k).max(1.0)).unwrap_or(0.0);
            let (x0, y0) = (r.x.max(0.0) as usize, r.y.max(0.0) as usize);
            let (x1, y1) = ((r.right().min(img.width as f32)).max(0.0) as usize, (r.bottom().min(img.height as f32)).max(0.0) as usize);
            for y in y0..y1 {
                for x in x0..x1 {
                    let dx = (x as f32 + 0.5 - cx) / rx;
                    let dy = (y as f32 + 0.5 - cy) / ry;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d > 1.0 {
                        continue;
                    }
                    // distance to the edge in pixels, roughly
                    let edge = (1.0 - d) * rx.min(ry);
                    match (g.stroke, g.fill) {
                        (Some(s), _) if edge < sw => blend(img, x, y, s.color),
                        (_, Some(f)) => blend(img, x, y, f),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Bilinear sample of an 8-bit image at continuous coordinates (edge-clamped).
fn sample(src: &Rgba8, u: f32, v: f32) -> Color {
    let fx = u - 0.5;
    let fy = v - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let (tx, ty) = (fx - x0, fy - y0);
    let wmax = src.width.saturating_sub(1) as isize;
    let hmax = src.height.saturating_sub(1) as isize;
    let at = |x: isize, y: isize| -> [f32; 4] {
        let (x, y) = (x.clamp(0, wmax) as usize, y.clamp(0, hmax) as usize);
        let p = src.data.get(y * src.width + x).copied().unwrap_or([0; 4]);
        p.map(f32::from)
    };
    let (x0, y0) = (x0 as isize, y0 as isize);
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bot = c[i] + (d[i] - c[i]) * tx;
        (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8
    })
}

/// Straight-alpha "over" of `c` onto pixel (x, y).
fn blend(img: &mut Rgba8, x: usize, y: usize, c: Color) {
    if x >= img.width {
        return;
    }
    let Some(d) = img.data.get_mut(y.saturating_mul(img.width).saturating_add(x)) else { return };
    let a = c[3] as u32;
    if a == 255 {
        *d = c;
        return;
    }
    if a == 0 {
        return;
    }
    let da = d[3] as u32;
    let out_a = a + da * (255 - a) / 255;
    if out_a == 0 {
        return;
    }
    for i in 0..3 {
        let v = (c[i] as u32 * a + d[i] as u32 * da * (255 - a) / 255) / out_a;
        d[i] = v.min(255) as u8;
    }
    d[3] = out_a.min(255) as u8;
}

fn fill_rect(img: &mut Rgba8, x0: f32, y0: f32, x1: f32, y1: f32, c: Color) {
    let cl = |v: f32, max: usize| if v.is_finite() { v.round().clamp(0.0, max as f32) as usize } else { 0 };
    let (x0, x1) = (cl(x0, img.width), cl(x1, img.width));
    let (y0, y1) = (cl(y0, img.height), cl(y1, img.height));
    for y in y0..y1 {
        for x in x0..x1 {
            blend(img, x, y, c);
        }
    }
}

/// An outline `width` pixels wide, inside `r`.
fn stroke_rect(img: &mut Rgba8, r: Rect, width: f32, c: Color) {
    let wd = width.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    fill_rect(img, r.x, r.y, r.right(), r.y + wd, c);
    fill_rect(img, r.x, r.bottom() - wd, r.right(), r.bottom(), c);
    fill_rect(img, r.x, r.y + wd, r.x + wd, r.bottom() - wd, c);
    fill_rect(img, r.right() - wd, r.y + wd, r.right(), r.bottom() - wd, c);
}

fn blit(dst: &mut Rgba8, src: &Rgba8, ox: isize, oy: isize) {
    for y in 0..src.height {
        let dy = oy + y as isize;
        if dy < 0 || dy >= dst.height as isize {
            continue;
        }
        for x in 0..src.width {
            let dx = ox + x as isize;
            if dx < 0 || dx >= dst.width as isize {
                continue;
            }
            if let Some(&p) = src.data.get(y * src.width + x) {
                blend(dst, dx as usize, dy as usize, p);
            }
        }
    }
}

/// Renders every page of a document.
pub fn render_document(doc: &crate::Document, photos: &dyn PhotoSource, text: &dyn TextRenderer, opt: &RenderOptions) -> Result<Vec<Rendered>> {
    let n = doc.pages.len();
    doc.pages.iter().enumerate().map(|(i, p)| render_page(&doc.page, p, photos, text, &RenderOptions { page: i + 1, pages: n, ..*opt })).collect()
}

/// Convenience for a cell list without a [`PageLayout`].
pub fn render_cells(page: &Page, cells: &[Cell], photos: &dyn PhotoSource, opt: &RenderOptions) -> Result<Rendered> {
    render_page(page, &PageLayout { cells: cells.to_vec(), guides: Vec::new() }, photos, &PlaceholderText, opt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Insets, Size};

    struct Solid;
    impl PhotoSource for Solid {
        fn image(&self, photo: &str, _: usize) -> std::result::Result<Rgba8, String> {
            match photo {
                // 2:1 landscape: left half red, right half blue
                "wide" => Ok(Rgba8::from_fn(200, 100, |x, _| if x < 100 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })),
                "empty" => Ok(Rgba8::new(0, 0)),
                _ => Err("not found".into()),
            }
        }
    }

    fn page() -> Page {
        Page::new(Size::new(100.0, 100.0))
    }

    fn cell(photo: &str, fit: Fit) -> Cell {
        let mut c = Cell::photo(Rect::new(0.0, 0.0, 100.0, 100.0));
        if let Some(p) = c.as_photo_mut() {
            p.photo = Some(photo.into());
            p.fit = fit;
        }
        c
    }

    #[test]
    fn fit_letterboxes_and_fill_crops() {
        let opt = RenderOptions::default();
        let fit = render_cells(&page(), &[cell("wide", Fit::Fit)], &Solid, &opt).unwrap().image;
        assert_eq!(fit.get(50, 5), [255, 255, 255, 255]); // letterbox
        assert_eq!(fit.get(10, 50), [255, 0, 0, 255]);
        assert_eq!(fit.get(90, 50), [0, 0, 255, 255]);
        let fill = render_cells(&page(), &[cell("wide", Fit::Fill)], &Solid, &opt).unwrap().image;
        assert_eq!(fill.get(2, 2), [255, 0, 0, 255]); // no letterbox: the photo is cropped
        assert_eq!(fill.get(97, 97), [0, 0, 255, 255]);
    }

    #[test]
    fn pan_rotate_and_failures() {
        let opt = RenderOptions::default();
        let mut c = cell("wide", Fit::Fill);
        c.as_photo_mut().unwrap().pan = [-1.0, 0.0];
        let left = render_cells(&page(), &[c.clone()], &Solid, &opt).unwrap().image;
        assert_eq!(left.get(95, 50), [255, 0, 0, 255]); // panned fully left: only red visible
        c.as_photo_mut().unwrap().pan = [1.0, 0.0];
        assert_eq!(render_cells(&page(), &[c.clone()], &Solid, &opt).unwrap().image.get(5, 50), [0, 0, 255, 255]);
        c.as_photo_mut().unwrap().pan = [f32::NAN, f32::INFINITY];
        assert!(render_cells(&page(), &[c.clone()], &Solid, &opt).is_err());
        let mut r = cell("wide", Fit::Fit);
        r.as_photo_mut().unwrap().rotate = 1;
        let rot = render_cells(&page(), &[r], &Solid, &opt).unwrap().image;
        assert_eq!(rot.get(50, 10), [255, 0, 0, 255]); // after a CW turn the left half is on top
        let miss = render_cells(&page(), &[cell("nope", Fit::Fit), cell("empty", Fit::Fit)], &Solid, &opt).unwrap();
        assert_eq!(miss.warnings.len(), 2);
        assert_eq!(miss.image.get(50, 50), EMPTY_SLOT);
        let huge = RenderOptions { dpi: 2400.0, ..opt };
        assert!(render_cells(&Page::new(Size::new(14_000.0, 14_000.0)), &[], &Solid, &huge).is_err());
        assert!(render_cells(&page(), &[], &Solid, &RenderOptions { dpi: f32::NAN, ..opt }).is_err());
    }

    #[test]
    fn bleed_and_margins_scale_with_dpi() {
        let p = Page { bleed: 9.0, ..page().with_margins(Insets::all(10.0)) };
        let lo = render_cells(&p, &[], &Solid, &RenderOptions { include_bleed: true, ..Default::default() }).unwrap().image;
        assert_eq!((lo.width, lo.height), (118, 118));
        let hi = render_cells(&p, &[], &Solid, &RenderOptions { dpi: 144.0, ..Default::default() }).unwrap().image;
        assert_eq!((hi.width, hi.height), (200, 200));
    }
}
