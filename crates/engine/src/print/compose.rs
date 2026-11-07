//! Composes print pages: places each cell's photo (fit or Zoom to Fill, optionally Rotate to Fit),
//! draws Photo Border and Inner Stroke, fills the page background, adds the Page Options (crop
//! marks, page number, page info) and returns [`PageRaster`]s.
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
/// A crop mark's length and its gap from the photo's corner, inches.
pub const CROP_MARK_IN: f64 = 0.25;
pub const CROP_MARK_GAP_IN: f64 = 0.0625;
/// A crop mark's weight, points.
pub const CROP_MARK_PT: f64 = 0.5;
/// Size of the page number and page info text, points.
pub const PAGE_TEXT_PT: f64 = 9.0;

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
    /// Page Options (Single Image and Contact Sheet, as in Lightroom; the packages ignore them):
    /// the page number at the bottom right of each page.
    pub page_numbers: bool,
    /// The print settings (sharpening, profile, resolution) at the bottom left of each page.
    pub page_info: bool,
    /// Marks at the corners of each photo to cut along.
    pub crop_marks: bool,
}

impl Default for PageStyle {
    fn default() -> Self {
        Self { background: [255; 3], page_numbers: false, page_info: false, crop_marks: false }
    }
}

/// An sRGB colour as 16-bit Rec. 2020 samples (the encoding of [`PageRaster`]).
pub fn rec2020_from_srgb8(c: [u8; 3]) -> [u16; 3] {
    let lin = SRGB.to_space(&REC2020).apply_f32(c.map(|v| srgb_to_linear(f32::from(v) / 255.0)));
    let trc = OutputSpace::Rec2020.trc();
    lin.map(|v| (trc.encode(v) * 65535.0 + 0.5) as u16)
}

/// Whether a layout style prints the Page Options (page numbers, page info, crop marks):
/// Lightroom offers them for Single Image and Contact Sheet; its packages have Cut Guides instead.
pub fn has_page_options(style: Style) -> bool {
    !style.is_package()
}

/// The colour of crop marks and page text on `background`: black, or white on a dark page.
pub fn mark_ink(background: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = background.map(f64::from);
    if 0.2126 * r + 0.7152 * g + 0.0722 * b < 128.0 { [255; 3] } else { [0; 3] }
}

/// The Page Info line: print sharpening, profile and resolution (Lightroom names the printer
/// where the resolution is; this print goes to a PDF).
pub fn page_info_text(settings: &PrintSettings) -> String {
    use crate::print_color::{ManagedSpace, PrintMedia, PrintProfile, PrintSharpen};
    let c = &settings.job.color;
    let media = match c.media {
        PrintMedia::Matte => "Matte",
        PrintMedia::Glossy => "Glossy",
    };
    let sharpen = match c.sharpen {
        PrintSharpen::Off => "Off".to_string(),
        PrintSharpen::Low => format!("Low ({media})"),
        PrintSharpen::Standard => format!("Standard ({media})"),
        PrintSharpen::High => format!("High ({media})"),
    };
    let profile = match &c.profile {
        PrintProfile::Managed { space: ManagedSpace::Srgb } => "Managed by Printer (sRGB)".to_string(),
        PrintProfile::Managed { space: ManagedSpace::AdobeRgb } => "Managed by Printer (Adobe RGB)".to_string(),
        PrintProfile::Icc { path } => path.file_stem().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
    };
    format!("Print Sharpening: {sharpen} · Profile: {profile} · {:.0} ppi", settings.job.effective_ppi())
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

/// A rectangular image region used as a source or destination for `Image16::blit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlitRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
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
    pub fn blit(&mut self, src: &Image16, source: BlitRect, destination: BlitRect) {
        let BlitRect { x: sx, y: sy, .. } = source;
        let BlitRect { x: dx, y: dy, .. } = destination;
        let w = source.w.min(destination.w).min(src.w.saturating_sub(sx)).min(self.w.saturating_sub(dx));
        let h = source.h.min(destination.h).min(src.h.saturating_sub(sy)).min(self.h.saturating_sub(dy));
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
/// asked), then the inner stroke on top of it. Returns where the photo landed.
pub fn draw_cell(page: &mut Image16, photo: &Image16, cell: PxRect, style: &CellStyle) -> Result<PxRect, String> {
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
    page.blit(&img, BlitRect { x: sx, y: sy, w, h }, BlitRect { x: dx, y: dy, w, h });
    let s = style.stroke_px.min(w.min(h) / 2);
    if s > 0 {
        page.fill_rect(dx, dy, w, s, style.stroke);
        page.fill_rect(dx, dy + h - s, w, s, style.stroke);
        page.fill_rect(dx, dy + s, s, h - 2 * s, style.stroke);
        page.fill_rect(dx + w - s, dy + s, s, h - 2 * s, style.stroke);
    }
    Ok(PxRect { x: dx, y: dy, w, h })
}

/// What the Page Options draw on every page, in page pixels.
struct Marks {
    /// Crop mark length, gap from the corner and weight.
    crop: Option<(usize, usize, usize)>,
    numbers: bool,
    info: Option<String>,
    ink: [u16; 3],
    text_px: f32,
    /// The printable area: the text sits in the margin below it, aligned with its sides.
    area: Option<PxRect>,
}

impl Marks {
    fn of(settings: &PrintSettings, ppi: f64, pw: usize, ph: usize) -> Self {
        let on = has_page_options(settings.layout.style);
        let page = &settings.page;
        let px = |inches: f64| (inches * ppi).round() as usize;
        Marks {
            crop: (on && page.crop_marks).then(|| (px(CROP_MARK_IN), px(CROP_MARK_GAP_IN), px(CROP_MARK_PT / 72.0).max(1))),
            numbers: on && page.page_numbers,
            info: (on && page.page_info).then(|| page_info_text(settings)),
            ink: rec2020_from_srgb8(mark_ink(page.background)),
            text_px: (PAGE_TEXT_PT / 72.0 * ppi) as f32,
            area: settings.layout.printable().ok().and_then(|a| px_rect(&a, ppi, pw, ph)),
        }
    }

    /// Draws the marks of page `number` (one-based) around `photos`, the photos on it.
    fn draw(&self, page: &mut Image16, photos: &[PxRect], number: usize) {
        if let Some((len, gap, weight)) = self.crop {
            for r in photos {
                crop_marks(page, r, len, gap, weight, photos, self.ink);
            }
        }
        let area = self.area.unwrap_or(PxRect { x: 0, y: 0, w: page.w, h: page.h });
        let (area_bottom, page_h) = ((area.y + area.h) as f32, page.h as f32);
        // centred in the bottom margin, and on the paper when the margin is thinner than the text
        let top = |h: f32| (area_bottom + (page_h - area_bottom - h) / 2.0).clamp(0.0, (page_h - h).max(0.0));
        if self.numbers
            && let Some(t) = crate::text::TextLayout::new(&number.to_string(), self.text_px, false, crate::fonts::CRAFT_FONTS)
        {
            let x = ((area.x + area.w) as f32 - t.width).max(0.0);
            draw_text(page, &t, x, top(t.height), self.ink);
        }
        if let Some(info) = &self.info
            && let Some(t) = crate::text::TextLayout::new(info, self.text_px, false, crate::fonts::CRAFT_FONTS)
        {
            draw_text(page, &t, area.x as f32, top(t.height), self.ink);
        }
    }
}

/// Crop marks for the photo at `r`: at each corner, a line along each of its edges that starts
/// `gap` past the corner and runs `len` outward, `weight` thick. Marks never cover a photo.
fn crop_marks(page: &mut Image16, r: &PxRect, len: usize, gap: usize, weight: usize, photos: &[PxRect], ink: [u16; 3]) {
    let (x0, y0, x1, y1) = (r.x as i64, r.y as i64, (r.x + r.w) as i64, (r.y + r.h) as i64);
    let (l, g, t) = (len as i64, gap as i64, weight.min(r.w).min(r.h) as i64);
    let segments = [
        (x0 - g - l, y0, l, t),
        (x1 + g, y0, l, t),
        (x0 - g - l, y1 - t, l, t),
        (x1 + g, y1 - t, l, t),
        (x0, y0 - g - l, t, l),
        (x0, y1 + g, t, l),
        (x1 - t, y0 - g - l, t, l),
        (x1 - t, y1 + g, t, l),
    ];
    let on_photo = |x: usize, y: usize| photos.iter().any(|p| x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h);
    for (sx, sy, sw, sh) in segments {
        for y in sy.max(0)..(sy + sh).min(page.h as i64) {
            for x in sx.max(0)..(sx + sw).min(page.w as i64) {
                let (x, y) = (x as usize, y as usize);
                if !on_photo(x, y) {
                    page.fill_rect(x, y, 1, 1, ink);
                }
            }
        }
    }
}

/// Blends text in `ink` into `page` with its top-left corner at (`x`, `y`).
fn draw_text(page: &mut Image16, text: &crate::text::TextLayout, x: f32, y: f32, ink: [u16; 3]) {
    let (w, h) = (page.w, page.h);
    text.draw_in(x, y, w as f32, h as f32, |gx, gy, cov| {
        let (Ok(gx), Ok(gy)) = (usize::try_from(gx), usize::try_from(gy)) else { return };
        if gx >= w || gy >= h {
            return;
        }
        let i = (gy * w + gx) * 3;
        if let Some(px) = page.rgb.get_mut(i..i + 3) {
            let k = cov.clamp(0.0, 1.0);
            for (p, c) in px.iter_mut().zip(ink) {
                *p = (f32::from(*p) + (f32::from(c) - f32::from(*p)) * k).round().clamp(0.0, 65535.0) as u16;
            }
        }
    });
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
    marks: Marks,
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
            let mut placed = Vec::new();
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
                placed.push(draw_cell(&mut page, photo, item.cell, &self.style).map_err(|e| format!("{}: {e}", item.name))?);
            }
            self.marks.draw(&mut page, &placed, index + 1);
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
        marks: Marks::of(&settings, ppi, pw, ph),
        color: settings.job.color.clone(),
    })
}

#[cfg(test)]
mod tests;
