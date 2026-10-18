//! Composes print pages: places each cell's photo (fit or Zoom to Fill, optionally Rotate to Fit),
//! draws Photo Border and Inner Stroke, fills the page background and returns [`PageRaster`]s.
//!
//! Work is split like an export: [`prepare`] reads the session (which photos go in which cell, one
//! render job per cell) and [`Prepared::run`] does the heavy part without it, so it can run on a
//! background thread with progress and cancellation.
//!
//! Photos are rendered with the normal export pipeline into Rec. 2020 at 16 bits, at exactly the
//! pixel size the cell needs at the job's resolution; colours given in sRGB (background, border,
//! stroke) are converted to the same encoding.
use super::layout::{Layout, Rect, Style};
use super::template::PrintSettings;
use super::{MAX_JOB_PAGES, MAX_JOB_PIXELS, MAX_PAGE_PIXELS, PageRaster};
use crate::Session;
use crate::export::{ExportOptions, output_size};
use crate::media::RenderJob;
use lightcraft_catalog::PhotoId;
use lightcraft_color::transfer::srgb_to_linear;
use lightcraft_color::{REC2020, SRGB};
use lightcraft_pipeline::{DeepSamples, OutputDepth, OutputSpace};
use serde::{Deserialize, Serialize};

/// Most photos one print job takes.
pub const MAX_PHOTOS: usize = 1000;
/// Widest Photo Border, inches.
pub const MAX_BORDER_IN: f64 = 2.0;
/// Widest Inner Stroke, points.
pub const MAX_STROKE_PT: f64 = 36.0;
/// Renders kept for cells that print the same photo at the same size.
const RENDER_CACHE: usize = 4;

/// Lightroom's Image Settings panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageSettings {
    /// Fill each cell with the photo and crop what overflows (centred); off: fit the whole photo.
    pub zoom_to_fill: bool,
    /// Turn a photo a quarter when that makes it print larger (fit) or crop less (fill).
    pub rotate_to_fit: bool,
    /// A border around the photo inside its cell; Picture Package and Custom Package only.
    pub border_enabled: bool,
    /// Border width, inches.
    pub border_width: f64,
    /// Border colour, sRGB.
    pub border_color: [u8; 3],
    /// A stroke drawn inside the photo's edge, every layout style.
    pub stroke_enabled: bool,
    /// Stroke width, points.
    pub stroke_width_pt: f64,
    /// Stroke colour, sRGB.
    pub stroke_color: [u8; 3],
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            zoom_to_fill: false,
            rotate_to_fit: false,
            border_enabled: false,
            border_width: 0.25,
            border_color: [255; 3],
            stroke_enabled: false,
            stroke_width_pt: 1.0,
            stroke_color: [0; 3],
        }
    }
}

impl ImageSettings {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f64, max: f64| v.is_finite() && (0.0..=max).contains(&v);
        if !ok(self.border_width, MAX_BORDER_IN) {
            return Err(format!("border width must be 0 to {MAX_BORDER_IN} in"));
        }
        if !ok(self.stroke_width_pt, MAX_STROKE_PT) {
            return Err(format!("stroke width must be 0 to {MAX_STROKE_PT} pt"));
        }
        Ok(())
    }
}

/// Lightroom's Page panel (the parts that are not margins or overlays).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PageStyle {
    /// Page background colour, sRGB.
    pub background: [u8; 3],
}

impl Default for PageStyle {
    fn default() -> Self {
        Self { background: [255; 3] }
    }
}

/// An sRGB colour as 16-bit Rec. 2020 samples (the encoding of [`PageRaster`]).
pub fn rec2020_from_srgb8(c: [u8; 3]) -> [u16; 3] {
    let lin = SRGB.to_space(&REC2020).apply_f32(c.map(|v| srgb_to_linear(f32::from(v) / 255.0)));
    let trc = OutputSpace::Rec2020.trc();
    lin.map(|v| (trc.encode(v) * 65535.0 + 0.5) as u16)
}

// ---- placement ------------------------------------------------------------------------------

/// How a photo of `iw` x `ih` goes into an `aw` x `ah` area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Turned a quarter (clockwise).
    pub rotated: bool,
    /// Scaled size after turning: at most the area when fitting, at least the area when filling.
    pub w: usize,
    pub h: usize,
}

impl Placement {
    /// The scaled size before turning (the size to render at).
    pub fn unturned(&self) -> (usize, usize) {
        if self.rotated { (self.h, self.w) } else { (self.w, self.h) }
    }
}

/// Decides turning and scale. `None` for an empty area or a photo without size.
pub fn place(iw: f64, ih: f64, aw: usize, ah: usize, fill: bool, rotate: bool) -> Option<Placement> {
    if !(iw.is_finite() && ih.is_finite() && iw >= 1.0 && ih >= 1.0) || aw == 0 || ah == 0 {
        return None;
    }
    let (af, bf) = (aw as f64, ah as f64);
    let scale = |w: f64, h: f64| if fill { (af / w).max(bf / h) } else { (af / w).min(bf / h) };
    let (s0, s1) = (scale(iw, ih), scale(ih, iw));
    let rotated = rotate && if fill { s1 < s0 * (1.0 - 1e-9) } else { s1 > s0 * (1.0 + 1e-9) };
    let (w, h) = if rotated { (ih * s1, iw * s1) } else { (iw * s0, ih * s0) };
    let (w, h) = if fill {
        (((w - 1e-6).ceil() as usize).max(aw), ((h - 1e-6).ceil() as usize).max(ah))
    } else {
        ((w.round() as usize).clamp(1, aw), (h.round() as usize).clamp(1, ah))
    };
    Some(Placement { rotated, w, h })
}

/// A rectangle in page pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// The pixel rectangle of an inch rectangle on a `pw` x `ph` page at `ppi`, clipped to the page.
pub fn px_rect(r: &Rect, ppi: f64, pw: usize, ph: usize) -> Option<PxRect> {
    if !r.is_finite() {
        return None;
    }
    let edge = |v: f64, max: usize| (v * ppi).round().clamp(0.0, max as f64) as usize;
    let (x0, y0, x1, y1) = (edge(r.x, pw), edge(r.y, ph), edge(r.right(), pw), edge(r.bottom(), ph));
    (x1 > x0 && y1 > y0).then_some(PxRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// The cells of every page with the photo each prints, by layout style: Single Image and Contact
/// Sheet lay `photos` out in sequence over as many pages as needed; Picture Package prints the
/// first photo in every cell; Custom Package prints each cell's own photo (else the first).
pub fn plan_cells(layout: &Layout, photos: &[PhotoId]) -> Result<Vec<Vec<(Rect, PhotoId)>>, String> {
    let first = photos.first().copied().ok_or("select at least one photo to print")?;
    if photos.len() > MAX_PHOTOS {
        return Err(format!("a print takes at most {MAX_PHOTOS} photos"));
    }
    match layout.style {
        Style::SingleImage | Style::ContactSheet => {
            let grid = layout.grid_rects()?;
            let per_page = grid.len().max(1);
            let pages = photos.len().div_ceil(per_page);
            if pages > MAX_JOB_PAGES {
                return Err(format!("a print is limited to {MAX_JOB_PAGES} pages; use a bigger grid or fewer photos"));
            }
            Ok(photos.chunks(per_page).map(|chunk| chunk.iter().zip(&grid).map(|(id, r)| (*r, *id)).collect()).collect())
        }
        Style::PicturePackage | Style::CustomPackage => {
            let pages = layout.pages.max(1) as usize;
            if pages > MAX_JOB_PAGES {
                return Err(format!("a print is limited to {MAX_JOB_PAGES} pages"));
            }
            if layout.cells.is_empty() {
                return Err("the layout has no cells; add some (print.cell.add) or use Auto Layout".into());
            }
            let mut out: Vec<Vec<(Rect, PhotoId)>> = vec![Vec::new(); pages];
            for c in &layout.cells {
                let photo = match layout.style {
                    Style::CustomPackage => c.photo.map_or(first, PhotoId),
                    _ => first,
                };
                out.get_mut(c.page as usize).ok_or("a cell is on a page the layout does not have")?.push((c.rect, photo));
            }
            Ok(out)
        }
    }
}

// ---- pixels ----------------------------------------------------------------------------------

/// 16-bit interleaved RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct Image16 {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u16>,
}

impl Image16 {
    pub fn filled(w: usize, h: usize, c: [u16; 3]) -> Result<Self, String> {
        let n = w.checked_mul(h).and_then(|n| n.checked_mul(3)).ok_or("image too large")?;
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(n).map_err(|_| "not enough memory for the page")?;
        for _ in 0..w * h {
            rgb.extend_from_slice(&c);
        }
        Ok(Self { w, h, rgb })
    }

    fn px(&self, x: usize, y: usize) -> Option<[u16; 3]> {
        let i = y.checked_mul(self.w)?.checked_add(x)?.checked_mul(3)?;
        let s = self.rgb.get(i..i.checked_add(3)?)?;
        Some([*s.first()?, *s.get(1)?, *s.get(2)?])
    }

    /// Fills a rectangle (clipped to the image).
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u16; 3]) {
        let x1 = x.saturating_add(w).min(self.w);
        for row in y..y.saturating_add(h).min(self.h) {
            let Some(line) = self.rgb.get_mut((row * self.w + x.min(x1)) * 3..(row * self.w + x1) * 3) else { continue };
            for px in line.as_chunks_mut::<3>().0 {
                *px = c;
            }
        }
    }

    /// Copies the `w` x `h` block of `src` at (`sx`, `sy`) to (`dx`, `dy`), clipped to both images.
    pub fn blit(&mut self, src: &Image16, sx: usize, sy: usize, dx: usize, dy: usize, w: usize, h: usize) {
        let w = w.min(src.w.saturating_sub(sx)).min(self.w.saturating_sub(dx));
        let h = h.min(src.h.saturating_sub(sy)).min(self.h.saturating_sub(dy));
        for row in 0..h {
            let from = src.rgb.get(((sy + row) * src.w + sx) * 3..((sy + row) * src.w + sx + w) * 3);
            let to = self.rgb.get_mut(((dy + row) * self.w + dx) * 3..((dy + row) * self.w + dx + w) * 3);
            if let (Some(from), Some(to)) = (from, to) {
                to.copy_from_slice(from);
            }
        }
    }

    /// A quarter turn clockwise.
    pub fn rotated_cw(&self) -> Result<Image16, String> {
        let mut out = Image16::filled(self.h, self.w, [0; 3])?;
        for y in 0..self.h {
            for x in 0..self.w {
                let Some(p) = self.px(x, y) else { continue };
                // (x, y) lands at (h - 1 - y, x).
                let i = (x * out.w + (self.h - 1 - y)) * 3;
                if let Some(d) = out.rgb.get_mut(i..i + 3) {
                    d.copy_from_slice(&p);
                }
            }
        }
        Ok(out)
    }

    /// Scales to `nw` x `nh`: area-weighted when shrinking an axis, linear when growing it.
    pub fn resized(&self, nw: usize, nh: usize) -> Result<Image16, String> {
        if nw == 0 || nh == 0 || self.w == 0 || self.h == 0 {
            return Err("empty image".into());
        }
        if (nw, nh) == (self.w, self.h) {
            return Ok(self.clone());
        }
        let wx = axis_weights(self.w, nw);
        let wy = axis_weights(self.h, nh);
        // Horizontal pass into 16-bit rows, then the vertical pass.
        let mut mid = Image16::filled(nw, self.h, [0; 3])?;
        for y in 0..self.h {
            let Some(src) = self.rgb.get(y * self.w * 3..(y + 1) * self.w * 3) else { continue };
            let Some(dst) = mid.rgb.get_mut(y * nw * 3..(y + 1) * nw * 3) else { continue };
            for (x, taps) in wx.iter().enumerate() {
                for c in 0..3 {
                    let v: f32 = taps.iter().map(|&(i, k)| src.get(i * 3 + c).map_or(0.0, |s| f32::from(*s) * k)).sum();
                    if let Some(d) = dst.get_mut(x * 3 + c) {
                        *d = v.round().clamp(0.0, 65535.0) as u16;
                    }
                }
            }
        }
        let mut out = Image16::filled(nw, nh, [0; 3])?;
        for (y, taps) in wy.iter().enumerate() {
            let Some(dst) = out.rgb.get_mut(y * nw * 3..(y + 1) * nw * 3) else { continue };
            for (i, d) in dst.iter_mut().enumerate() {
                let v: f32 = taps.iter().map(|&(row, k)| mid.rgb.get(row * nw * 3 + i).map_or(0.0, |s| f32::from(*s) * k)).sum();
                *d = v.round().clamp(0.0, 65535.0) as u16;
            }
        }
        Ok(out)
    }
}

/// For each of `new` output samples, the `(source index, weight)` taps that make it (weights sum to 1).
fn axis_weights(old: usize, new: usize) -> Vec<Vec<(usize, f32)>> {
    let (o, n) = (old as f64, new as f64);
    (0..new)
        .map(|i| {
            if new < old {
                let step = o / n;
                let (a, b) = (i as f64 * step, (i as f64 + 1.0) * step);
                let first = a.floor() as usize;
                let last = (b.ceil() as usize).min(old);
                (first..last)
                    .filter_map(|j| {
                        let overlap = (b.min(j as f64 + 1.0) - a.max(j as f64)).max(0.0);
                        (overlap > 0.0).then_some((j, (overlap / step) as f32))
                    })
                    .collect()
            } else {
                let pos = (i as f64 + 0.5) * o / n - 0.5;
                let j0 = pos.floor();
                let f = (pos - j0) as f32;
                let at = |j: f64| j.clamp(0.0, o - 1.0) as usize;
                vec![(at(j0), 1.0 - f), (at(j0 + 1.0), f)]
            }
        })
        .collect()
}

/// Everything about a cell's look that does not depend on the photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellStyle {
    pub fill: bool,
    pub rotate: bool,
    pub border_px: usize,
    pub border: [u16; 3],
    pub stroke_px: usize,
    pub stroke: [u16; 3],
}

impl CellStyle {
    /// The style for `settings` at `ppi`; the border applies to the package styles only.
    pub fn of(settings: &PrintSettings, ppi: f64) -> Self {
        let img = &settings.image;
        let border_px = if img.border_enabled && settings.layout.style.is_package() { (img.border_width * ppi).round() as usize } else { 0 };
        let stroke_px =
            if img.stroke_enabled && img.stroke_width_pt > 0.0 { ((img.stroke_width_pt / 72.0 * ppi).round() as usize).max(1) } else { 0 };
        Self {
            fill: img.zoom_to_fill,
            rotate: img.rotate_to_fit,
            border_px,
            border: rec2020_from_srgb8(img.border_color),
            stroke_px,
            stroke: rec2020_from_srgb8(img.stroke_color),
        }
    }
}

/// Draws `photo` into `cell` of `page`: border frame, the photo fitted or filled (and turned when
/// asked), then the inner stroke on top of it.
pub fn draw_cell(page: &mut Image16, photo: &Image16, cell: PxRect, style: &CellStyle) -> Result<(), String> {
    // The border may not eat the cell: at least one pixel of photo area stays.
    let border = style.border_px.min(cell.w.min(cell.h).saturating_sub(1) / 2);
    let (ax, ay) = (cell.x + border, cell.y + border);
    let (aw, ah) = (cell.w - 2 * border, cell.h - 2 * border);
    let p = place(photo.w as f64, photo.h as f64, aw, ah, style.fill, style.rotate).ok_or("a photo has no size")?;
    let (uw, uh) = p.unturned();
    let mut img = photo.resized(uw, uh)?;
    if p.rotated {
        img = img.rotated_cw()?;
    }
    // Where the photo lands: centred in the area, cropped to it when it overflows.
    let (sx, sy) = (img.w.saturating_sub(aw) / 2, img.h.saturating_sub(ah) / 2);
    let (w, h) = (img.w.min(aw), img.h.min(ah));
    let (dx, dy) = (ax + (aw - w) / 2, ay + (ah - h) / 2);
    if border > 0 {
        page.fill_rect(dx - border, dy - border, w + 2 * border, h + 2 * border, style.border);
    }
    page.blit(&img, sx, sy, dx, dy, w, h);
    let s = style.stroke_px.min(w.min(h) / 2);
    if s > 0 {
        page.fill_rect(dx, dy, w, s, style.stroke);
        page.fill_rect(dx, dy + h - s, w, s, style.stroke);
        page.fill_rect(dx, dy + s, s, h - 2 * s, style.stroke);
        page.fill_rect(dx + w - s, dy + s, s, h - 2 * s, style.stroke);
    }
    Ok(())
}

// ---- the job ---------------------------------------------------------------------------------

struct Item {
    page: usize,
    name: String,
    cell: PxRect,
    key: (u64, usize, usize),
    job: RenderJob,
}

/// A print job set up from the session: one render per cell, ready to run on another thread.
pub struct Prepared {
    pages: Vec<(usize, usize)>,
    items: Vec<Item>,
    ppi: f32,
    background: [u16; 3],
    style: CellStyle,
    /// Print sharpening, applied to each photo once it's rendered at its print size.
    color: crate::print_color::PrintColorSettings,
}

impl Prepared {
    /// Renders (progress steps) the job will run.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Pages the job will produce.
    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// Renders every photo and composes the pages. `progress(done, name)` is called before each
    /// render with the photo's file name; returning `false` cancels (no partial result).
    pub fn run(self, progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Vec<PageRaster>, String> {
        let mut cache: Vec<((u64, usize, usize), Image16)> = Vec::new();
        let mut done = 0usize;
        let mut items = self.items.into_iter().peekable();
        let mut out = Vec::with_capacity(self.pages.len());
        for (index, &(w, h)) in self.pages.iter().enumerate() {
            let mut page = Image16::filled(w, h, self.background)?;
            while let Some(item) = items.next_if(|i| i.page == index) {
                if !progress(done, &item.name) {
                    return Err("Print cancelled".into());
                }
                done += 1;
                let at = cache.iter().position(|(k, _)| *k == item.key);
                let at = match at {
                    Some(at) => at,
                    None => {
                        let img = render(item.job, &self.color).map_err(|e| format!("{}: {e}", item.name))?;
                        if cache.len() >= RENDER_CACHE {
                            cache.remove(0);
                        }
                        cache.push((item.key, img));
                        cache.len() - 1
                    }
                };
                let photo = &cache.get(at).ok_or("render cache lost an entry")?.1;
                draw_cell(&mut page, photo, item.cell, &self.style).map_err(|e| format!("{}: {e}", item.name))?;
            }
            out.push(PageRaster { width: w as u32, height: h as u32, ppi: self.ppi, rgb: page.rgb });
        }
        if !progress(done, "") {
            return Err("Print cancelled".into());
        }
        Ok(out)
    }
}

fn render(job: RenderJob, color: &crate::print_color::PrintColorSettings) -> Result<Image16, String> {
    let rendered = job.run().rendered?;
    let mut deep = rendered.deep.ok_or("the render has no 16-bit samples")?;
    // print sharpening for the paper, at the size the photo prints
    color.sharpen_photo(&mut deep);
    let DeepSamples::U16(rgb) = deep.samples else { return Err("the render is not 16-bit".into()) };
    if deep.space != OutputSpace::Rec2020 || rgb.len() != deep.width.saturating_mul(deep.height).saturating_mul(3) {
        return Err("the render is not Rec. 2020 RGB".into());
    }
    Ok(Image16 { w: deep.width, h: deep.height, rgb })
}

/// Sets a print job up: the cells for `photos` under `settings.layout`, one render job per cell.
pub fn prepare(session: &mut Session, settings: &PrintSettings, photos: &[PhotoId]) -> Result<Prepared, String> {
    let mut settings = settings.clone();
    settings.validate()?;
    let ppi = f64::from(settings.job.effective_ppi());
    let (pw_in, ph_in) = settings.layout.page_size()?;
    let (pw, ph) = ((pw_in * ppi).round() as usize, (ph_in * ppi).round() as usize);
    let page_px = (pw as u64).saturating_mul(ph as u64);
    if pw == 0 || ph == 0 || page_px > MAX_PAGE_PIXELS {
        return Err(format!("a page of {pw} x {ph} pixels is too large; lower the print resolution"));
    }
    let plan = plan_cells(&settings.layout, photos)?;
    if page_px.saturating_mul(plan.len() as u64) > MAX_JOB_PIXELS {
        return Err("the print is too large for memory; lower the print resolution or print fewer pages".into());
    }
    let style = CellStyle::of(&settings, ppi);
    let mut items = Vec::new();
    for (page, cells) in plan.iter().enumerate() {
        for (rect, id) in cells {
            let Some(cell) = px_rect(rect, ppi, pw, ph) else { continue };
            let photo = session.catalog.photo(*id).ok_or_else(|| format!("no photo {}", id.0))?;
            let name = photo.file_name.clone();
            let (nw, nh) = output_size(photo, &ExportOptions::default());
            let border = style.border_px.min(cell.w.min(cell.h).saturating_sub(1) / 2);
            let (aw, ah) = (cell.w - 2 * border, cell.h - 2 * border);
            let p = place(nw as f64, nh as f64, aw, ah, style.fill, style.rotate).ok_or("a photo has no size")?;
            let (rw, rh) = p.unturned();
            let job = session.export_job(*id, rw, rh, OutputSpace::Rec2020, OutputDepth::U16)?;
            items.push(Item { page, name, cell, key: (id.0, rw, rh), job });
        }
    }
    Ok(Prepared {
        pages: vec![(pw, ph); plan.len()],
        items,
        ppi: ppi as f32,
        background: rec2020_from_srgb8(settings.page.background),
        style,
        color: settings.job.color.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::layout::{GridSpec, Margins};

    const RED: [u16; 3] = [60000, 1000, 1000];
    const BLUE: [u16; 3] = [1000, 1000, 60000];
    const WHITE: [u16; 3] = [65535; 3];

    /// A `w` x `h` image whose left half is red and right half blue, with a green top row.
    fn halves(w: usize, h: usize) -> Image16 {
        let mut img = Image16::filled(w, h, RED).unwrap();
        img.fill_rect(w / 2, 0, w - w / 2, h, BLUE);
        img.fill_rect(0, 0, w, 1, [1000, 60000, 1000]);
        img
    }

    fn style() -> CellStyle {
        CellStyle { fill: false, rotate: false, border_px: 0, border: [0; 3], stroke_px: 0, stroke: [0; 3] }
    }

    fn at(img: &Image16, x: usize, y: usize) -> [u16; 3] {
        img.px(x, y).unwrap()
    }

    fn page() -> Image16 {
        Image16::filled(100, 100, WHITE).unwrap()
    }

    #[test]
    fn placement_fit_fill_and_rotation() {
        // 4:3 photo into a 2:1 area.
        let fit = place(400.0, 300.0, 200, 100, false, false).unwrap();
        assert_eq!((fit.w, fit.h, fit.rotated), (133, 100, false));
        let fill = place(400.0, 300.0, 200, 100, true, false).unwrap();
        assert_eq!((fill.w, fill.h), (200, 150));
        // A portrait photo in a landscape area: turning prints it larger.
        assert_eq!(place(300.0, 400.0, 400, 300, false, false).unwrap(), Placement { rotated: false, w: 225, h: 300 });
        let turned = place(300.0, 400.0, 400, 300, false, true).unwrap();
        assert_eq!((turned.rotated, turned.w, turned.h), (true, 400, 300));
        assert_eq!(turned.unturned(), (300, 400));
        // Filling a landscape cell with a portrait photo crops less when turned.
        let f = place(300.0, 400.0, 400, 300, true, true).unwrap();
        assert!(f.rotated && f.w >= 400 && f.h >= 300);
        // Matching orientation never turns.
        assert!(!place(400.0, 300.0, 400, 300, false, true).unwrap().rotated);
        assert!(!place(300.0, 300.0, 100, 100, false, true).unwrap().rotated);
        // Hostile input.
        for bad in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
            assert!(place(bad, 10.0, 10, 10, false, false).is_none());
            assert!(place(10.0, bad, 10, 10, true, true).is_none());
        }
        assert!(place(10.0, 10.0, 0, 10, false, false).is_none());
    }

    #[test]
    fn fit_centres_the_photo() {
        let mut p = page();
        draw_cell(&mut p, &halves(40, 20), PxRect { x: 10, y: 10, w: 80, h: 80 }, &style()).unwrap();
        // 40x20 scaled to 80x40, centred vertically at y 30..70.
        assert_eq!(at(&p, 10, 50), RED);
        assert_eq!(at(&p, 89, 50), BLUE);
        assert_eq!(at(&p, 50, 20), WHITE, "above the photo");
        assert_eq!(at(&p, 50, 75), WHITE, "below the photo");
        assert_eq!(at(&p, 5, 50), WHITE, "outside the cell");
    }

    #[test]
    fn zoom_to_fill_crops_the_centre() {
        let mut p = page();
        let s = CellStyle { fill: true, ..style() };
        draw_cell(&mut p, &halves(40, 20), PxRect { x: 10, y: 10, w: 80, h: 80 }, &s).unwrap();
        // The photo scales to 160x80 and the centre 80 columns remain: the colour edge is mid-cell.
        for y in [10, 50, 89] {
            assert_ne!(at(&p, 10, y), WHITE);
            assert_ne!(at(&p, 89, y), WHITE);
        }
        assert_eq!(at(&p, 20, 50), RED);
        assert_eq!(at(&p, 80, 50), BLUE);
        // Nothing leaks outside the cell.
        assert_eq!(at(&p, 9, 50), WHITE);
        assert_eq!(at(&p, 90, 50), WHITE);
        assert_eq!(at(&p, 50, 9), WHITE);
        assert_eq!(at(&p, 50, 90), WHITE);
    }

    #[test]
    fn rotate_to_fit_turns_clockwise() {
        // A portrait 20x40 photo (red left, blue right) in a landscape 80x40 cell.
        let mut p = page();
        let cell = PxRect { x: 10, y: 10, w: 80, h: 40 };
        draw_cell(&mut p, &halves(20, 40), cell, &style()).unwrap();
        assert_eq!(at(&p, 20, 30), WHITE, "unturned the photo is a narrow strip in the middle");
        assert_eq!(at(&p, 45, 30), RED);
        let mut q = page();
        draw_cell(&mut q, &halves(20, 40), cell, &CellStyle { rotate: true, ..style() }).unwrap();
        // Turned clockwise the left half (red) becomes the top half and the photo spans the cell.
        assert_eq!(at(&q, 12, 12), RED, "{:?}", at(&q, 12, 12));
        assert_eq!(at(&q, 12, 48), BLUE);
        assert_eq!(at(&q, 50, 20), RED);
        assert_eq!(at(&q, 50, 40), BLUE);
        assert_eq!(at(&q, 11, 30), at(&q, 11, 30));
    }

    #[test]
    fn border_frames_the_photo_inside_the_cell() {
        let mut p = page();
        let green = [100, 60000, 100];
        let s = CellStyle { border_px: 5, border: green, fill: true, ..style() };
        draw_cell(&mut p, &halves(60, 60), PxRect { x: 10, y: 10, w: 60, h: 60 }, &s).unwrap();
        assert_eq!(at(&p, 10, 40), green);
        assert_eq!(at(&p, 14, 40), green);
        assert_eq!(at(&p, 15, 40), RED);
        assert_eq!(at(&p, 69, 40), green);
        assert_eq!(at(&p, 64, 40), BLUE);
        assert_eq!(at(&p, 40, 10), green);
        assert_eq!(at(&p, 40, 69), green);
        // A fitted photo gets the border hugging it, not the whole cell.
        let mut q = page();
        let s = CellStyle { border_px: 5, border: green, ..style() };
        draw_cell(&mut q, &halves(60, 30), PxRect { x: 0, y: 0, w: 100, h: 100 }, &s).unwrap();
        // The 90x90 area fits a 90x45 photo at y 27..72 inside a frame 5 px thick.
        assert_eq!(at(&q, 50, 23), green);
        assert_eq!(at(&q, 50, 22), green);
        assert_eq!(at(&q, 50, 21), WHITE);
        // A border wider than the cell leaves the photo visible.
        let mut r = page();
        let s = CellStyle { border_px: 500, border: green, ..style() };
        draw_cell(&mut r, &halves(10, 10), PxRect { x: 10, y: 10, w: 20, h: 20 }, &s).unwrap();
        assert_ne!(at(&r, 20, 20), WHITE);
    }

    #[test]
    fn stroke_is_drawn_inside_the_photo_edge() {
        let mut p = page();
        let black = [0, 0, 0];
        let s = CellStyle { stroke_px: 3, stroke: black, fill: true, ..style() };
        draw_cell(&mut p, &halves(60, 60), PxRect { x: 20, y: 20, w: 60, h: 60 }, &s).unwrap();
        for (x, y) in [(20, 40), (22, 40), (77, 40), (79, 40), (40, 20), (40, 22), (40, 77), (40, 79)] {
            assert_eq!(at(&p, x, y), black, "({x}, {y})");
        }
        assert_eq!(at(&p, 23, 40), RED);
        assert_eq!(at(&p, 76, 40), BLUE);
        assert_eq!(at(&p, 19, 40), WHITE, "the stroke does not grow the photo");
        // Corners are covered exactly once and a huge stroke stays within the photo.
        let mut q = page();
        let s = CellStyle { stroke_px: 1000, stroke: black, ..style() };
        draw_cell(&mut q, &halves(10, 10), PxRect { x: 20, y: 20, w: 40, h: 40 }, &s).unwrap();
        assert_eq!(at(&q, 40, 40), black);
        assert_eq!(at(&q, 19, 40), WHITE);
        assert_eq!(at(&q, 60, 40), WHITE);
    }

    #[test]
    fn colours_are_rec2020_encoded() {
        assert_eq!(rec2020_from_srgb8([255; 3]), [65535; 3]);
        assert_eq!(rec2020_from_srgb8([0; 3]), [0; 3]);
        let grey = rec2020_from_srgb8([128; 3]);
        assert!(grey[0].abs_diff(grey[1]) < 3 && grey[1].abs_diff(grey[2]) < 3);
        // sRGB red sits inside the Rec. 2020 gamut: its green and blue are not zero.
        let red = rec2020_from_srgb8([255, 0, 0]);
        assert!(red[0] < 65535 && red[1] > 0);
    }

    #[test]
    fn resize_preserves_flat_colour_and_edges() {
        let img = halves(64, 32);
        for (w, h) in [(32, 16), (128, 64), (50, 70), (1, 1), (64, 31)] {
            let r = img.resized(w, h).unwrap();
            assert_eq!((r.w, r.h), (w, h));
            assert_eq!(r.rgb.len(), w * h * 3);
        }
        let flat = Image16::filled(40, 30, [1234, 5678, 9999]).unwrap().resized(17, 61).unwrap();
        assert!(flat.rgb.chunks(3).all(|p| p == [1234, 5678, 9999]));
        assert!(img.resized(0, 5).is_err());
        let r = img.resized(32, 16).unwrap();
        assert_eq!(at(&r, 4, 8), RED);
        assert_eq!(at(&r, 28, 8), BLUE);
    }

    #[test]
    fn rotate_and_blit_are_clipped() {
        let img = halves(3, 2);
        let r = img.rotated_cw().unwrap();
        assert_eq!((r.w, r.h), (2, 3));
        let mut p = page();
        p.blit(&img, 0, 0, 99, 99, 10, 10);
        assert_eq!(at(&p, 99, 99), at(&img, 0, 0));
        p.blit(&img, 5, 5, 0, 0, 3, 3);
        p.fill_rect(95, 95, 50, 50, RED);
        assert_eq!(at(&p, 99, 99), RED);
        p.fill_rect(usize::MAX, usize::MAX, usize::MAX, usize::MAX, RED);
    }

    fn settings(style: Style) -> PrintSettings {
        let mut s = PrintSettings::default();
        s.layout.style = style;
        s.layout.page.paper = "4x6".into();
        s.layout.page.margins = Margins::uniform(0.5);
        s.job.ppi = 100.0;
        s
    }

    #[test]
    fn plans_follow_the_layout_style() {
        let ids: Vec<PhotoId> = (1..=5).map(PhotoId).collect();
        let mut s = settings(Style::ContactSheet);
        s.layout.grid = GridSpec { columns: 2, rows: 2, ..Default::default() };
        let plan = plan_cells(&s.layout, &ids).unwrap();
        assert_eq!(plan.iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 1]);
        assert_eq!(plan[1][0].1, PhotoId(5));
        let one = plan_cells(&settings(Style::SingleImage).layout, &ids).unwrap();
        assert_eq!(one.len(), 5);
        assert!(plan_cells(&s.layout, &[]).is_err());
        assert!(plan_cells(&s.layout, &vec![PhotoId(1); MAX_PHOTOS + 1]).is_err());

        let mut pkg = settings(Style::PicturePackage);
        assert!(plan_cells(&pkg.layout, &ids).is_err(), "no cells yet");
        pkg.layout.add_cell(1.0, 1.0).unwrap();
        pkg.layout.add_cell(2.0, 2.0).unwrap();
        let plan = plan_cells(&pkg.layout, &ids).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].iter().all(|(_, p)| *p == PhotoId(1)), "picture package repeats the first photo");

        let mut custom = settings(Style::CustomPackage);
        let a = custom.layout.add_cell(1.0, 1.0).unwrap();
        custom.layout.add_cell(1.0, 1.0).unwrap();
        custom.layout.set_cell_photo(a, Some(3)).unwrap();
        let plan = plan_cells(&custom.layout, &ids).unwrap();
        assert_eq!((plan[0][0].1, plan[0][1].1), (PhotoId(3), PhotoId(1)));
        let blank_page = {
            custom.layout.new_page().unwrap();
            plan_cells(&custom.layout, &ids).unwrap()
        };
        assert_eq!(blank_page.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 0]);
    }

    fn first_ids(s: &Session, n: usize) -> Vec<PhotoId> {
        s.catalog.photos().take(n).map(|p| p.id).collect()
    }

    #[test]
    fn composes_pages_from_real_photos() {
        let mut s = Session::with_demo();
        let ids = first_ids(&s, 3);
        let mut set = settings(Style::ContactSheet);
        set.layout.grid = GridSpec { columns: 1, rows: 2, spacing_x: 0.0, spacing_y: 0.0, keep_square: false };
        set.page.background = [200, 30, 30];
        set.image.zoom_to_fill = true;
        set.image.stroke_enabled = true;
        set.image.stroke_width_pt = 2.0;
        set.image.stroke_color = [0, 0, 0];
        let work = prepare(&mut s, &set, &ids).unwrap();
        assert_eq!((work.len(), work.pages()), (3, 2));
        let mut steps = Vec::new();
        let pages = work
            .run(&mut |i, name| {
                steps.push((i, name.to_string()));
                true
            })
            .unwrap();
        assert_eq!(steps.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(pages.len(), 2);
        let first = &pages[0];
        assert_eq!((first.width, first.height, first.ppi), (400, 600, 100.0));
        assert_eq!(first.rgb.len(), 400 * 600 * 3);
        let bg = rec2020_from_srgb8([200, 30, 30]);
        let px = |p: &PageRaster, x: usize, y: usize| {
            let i = (y * p.width as usize + x) * 3;
            [p.rgb[i], p.rgb[i + 1], p.rgb[i + 2]]
        };
        assert_eq!(px(first, 5, 5), bg, "margin shows the page background");
        assert_eq!(px(first, 395, 595), bg);
        // Zoom to fill: the top-left photo pixel (inside the 2 pt = 3 px stroke) is the stroke colour.
        assert_eq!(px(first, 50, 50), [0, 0, 0]);
        let centre = px(first, 200, 150);
        assert_ne!(centre, bg);
        assert_ne!(centre, [0, 0, 0]);
        // The second page holds the third photo in the first cell only: the second cell is bare.
        let second = &pages[1];
        assert_ne!(px(second, 200, 150), bg);
        assert_eq!(px(second, 200, 450), bg);
        // Cancelling gives no pages.
        let work = prepare(&mut s, &set, &ids).unwrap();
        assert!(work.run(&mut |i, _| i < 1).is_err());
    }

    #[test]
    fn package_renders_each_size_and_border() {
        let mut s = Session::with_demo();
        let ids = first_ids(&s, 1);
        let mut set = settings(Style::PicturePackage);
        set.layout.add_cell(1.5, 1.0).unwrap();
        set.layout.add_cell(1.5, 1.0).unwrap();
        set.image.border_enabled = true;
        set.image.border_width = 0.1;
        set.image.border_color = [10, 200, 10];
        let pages = prepare(&mut s, &set, &ids).unwrap().run(&mut |_, _| true).unwrap();
        assert_eq!(pages.len(), 1);
        let green = rec2020_from_srgb8([10, 200, 10]);
        let cell = set.layout.cells[0].rect;
        let (x0, y0) = ((cell.x * 100.0).round() as usize, (cell.y * 100.0).round() as usize);
        let at = |x: usize, y: usize| {
            let i = (y * 400 + x) * 3;
            pages[0].rgb[i..i + 3].to_vec()
        };
        // The frame touches the cell edge on the photo's limiting axis: 10 px (0.1 in) of border all round.
        let mid = (x0 + 75, y0 + 50);
        assert_ne!(at(mid.0, mid.1), green.to_vec(), "the photo, not the border, is in the middle");
        let green_px = (y0..y0 + 100).flat_map(|y| (x0..x0 + 150).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) == green.to_vec()).count();
        assert!(green_px > 1000, "the border frames the photo inside its cell: {green_px} border pixels");
    }

    #[test]
    fn caps_and_hostile_settings_are_refused() {
        let mut s = Session::with_demo();
        let ids = first_ids(&s, 1);
        let mut set = settings(Style::SingleImage);
        set.layout.page.paper = "custom".into();
        set.layout.page.custom_width = 60.0;
        set.layout.page.custom_height = 60.0;
        set.job.ppi = 720.0;
        assert!(prepare(&mut s, &set, &ids).is_err(), "a 60 in page at 720 ppi is far beyond the page cap");
        let mut set = settings(Style::SingleImage);
        set.job.ppi = f32::NAN;
        assert!(prepare(&mut s, &set, &ids).is_err());
        let mut set = settings(Style::SingleImage);
        set.image.border_width = f64::NAN;
        assert!(prepare(&mut s, &set, &ids).is_err());
        set.image.border_width = 0.1;
        set.image.stroke_width_pt = -1.0;
        assert!(prepare(&mut s, &set, &ids).is_err());
        assert!(prepare(&mut s, &settings(Style::SingleImage), &[PhotoId(999_999)]).is_err());
        assert!(prepare(&mut s, &settings(Style::SingleImage), &[]).is_err());
        let set = settings(Style::SingleImage);
        assert!(prepare(&mut s, &set, &vec![ids[0]; 101]).is_err(), "more than 100 pages");
        // 100 pages of 4x6 at 100 ppi is 24 Mpx: allowed; at 720 ppi it would be over the job cap.
        let mut big = settings(Style::SingleImage);
        big.job.ppi = 720.0;
        assert!(prepare(&mut s, &big, &vec![ids[0]; 60]).is_err());
    }
}
