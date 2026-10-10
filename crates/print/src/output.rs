//! Print output: layout pages to PDF (photos as JPEG images, shaped vector text with embedded
//! subset fonts, graphics, crop marks) and to JPEG files (one raster per page).

use std::collections::HashMap;
use std::sync::Arc;

use dac_codecs::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
use dac_layout::render::{PhotoSource, RenderOptions, photo_placement, render_page};
use dac_layout::tokens::{PhotoInfo, expand};
use dac_layout::{CellKind, Color, Document, Shape};
use dac_pdf::{ColorSpace, Image, Paint, Path, Rect as PRect, Stroke as PStroke};
use dac_raster::Rgba8;

use crate::text::ShapedText;
use crate::{PrintError, PrintSettings, Result};

/// Crop marks: gap between the trim edge and the mark, mark length, line width (points).
pub const MARK_OFFSET: f64 = 9.0;
pub const MARK_LENGTH: f64 = 18.0;
pub const MARK_WIDTH: f64 = 0.5;

/// Longest photo side rendered for a print (pixels).
pub const MAX_PHOTO_PX: usize = 16_384;

/// Extra PDF options.
#[derive(Clone, Debug, Default)]
pub struct PdfOptions {
    pub metadata: dac_pdf::Metadata,
    /// The printer/paper profile written as the document's OutputIntent.
    pub output_intent: Option<dac_pdf::OutputIntent>,
}

/// Called before each page (0-based) with the page count; return `false` to cancel.
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize) -> bool;

/// A finished output and anything that went wrong on the way (missing photos are drawn as grey
/// slots and reported here).
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub files: Vec<Vec<u8>>,
    pub pages: usize,
    pub warnings: Vec<String>,
}

fn paint(c: Color) -> Paint {
    Paint::Rgb(f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0)
}

fn prect(r: &dac_layout::Rect, dx: f64, dy: f64) -> PRect {
    PRect::new(f64::from(r.x) + dx, f64::from(r.y) + dy, f64::from(r.w), f64::from(r.h))
}

/// RGB samples of `img` composited over white.
fn rgb_over_white(img: &Rgba8) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.data.len().saturating_mul(3));
    for p in &img.data {
        let a = u32::from(p[3]);
        for c in &p[..3] {
            out.push(((u32::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8);
        }
    }
    out
}

/// JPEG bytes of `img` (alpha composited over white).
pub fn jpeg(img: &Rgba8, quality: u8, ppi: Option<u16>) -> Result<Vec<u8>> {
    let rgb = rgb_over_white(img);
    let (w, h) = (u32::try_from(img.width).unwrap_or(u32::MAX), u32::try_from(img.height).unwrap_or(u32::MAX));
    encode_jpeg(
        &EncodeImage::new(w, h, 3, Samples::U8(&rgb)),
        quality.clamp(1, 100),
        ChromaSubsampling::S444,
        &EncodeMeta { ppi, ..Default::default() },
    )
    .map_err(|e| PrintError::Output(e.to_string()))
}

fn page_infos(layout: &dac_layout::PageLayout, photos: &dyn PhotoSource, page: usize, pages: usize) -> Vec<Option<PhotoInfo>> {
    layout
        .cells
        .iter()
        .map(|c| {
            c.as_photo().and_then(|p| p.photo.as_deref()).map(|id| {
                let mut i = photos.info(id);
                i.page = page;
                i.pages = pages;
                i
            })
        })
        .collect()
}

fn text_of(t: &dac_layout::TextCell, infos: &[Option<PhotoInfo>], page: usize, pages: usize) -> String {
    let src = t.source.and_then(|s| infos.get(s).cloned().flatten()).or_else(|| infos.iter().flatten().next().cloned());
    let info = src.unwrap_or(PhotoInfo { page, pages, ..PhotoInfo::default() });
    expand(&t.text, &info)
}

/// Writes `doc` as a PDF. Photos are requested at `settings.job.dpi`.
pub fn to_pdf(
    doc: &Document,
    settings: &PrintSettings,
    photos: &dyn PhotoSource,
    text: &ShapedText,
    opt: &PdfOptions,
    progress: Progress,
) -> Result<Output> {
    doc.validate()?;
    let dpi = f64::from(settings.job.dpi.clamp(36.0, 1200.0));
    let page = &doc.page;
    let pad = if settings.options.crop_marks { MARK_OFFSET + MARK_LENGTH + 4.0 } else { 0.0 };
    let bleed = f64::from(page.bleed.max(0.0));
    let margin = pad.max(bleed);
    let (pw, ph) = (f64::from(page.size.w) + 2.0 * margin, f64::from(page.size.h) + 2.0 * margin);
    let mut pdf = dac_pdf::Document::new();
    pdf.metadata = opt.metadata.clone();
    pdf.output_intent = opt.output_intent.clone();
    let mut warnings = Vec::new();
    let mut images: HashMap<(String, usize, u8), (dac_pdf::ImageId, usize, usize)> = HashMap::new();
    let n = doc.pages.len();
    for (pi, layout) in doc.pages.iter().enumerate() {
        if !progress(pi, n) {
            return Err(PrintError::Cancelled);
        }
        let mut out = dac_pdf::Page::new(pw, ph);
        let trim = PRect::new(margin, margin, f64::from(page.size.w), f64::from(page.size.h));
        out.trim = Some(trim);
        if bleed > 0.0 {
            out.bleed = Some(trim.inset(-bleed));
        }
        let bg = if bleed > 0.0 { trim.inset(-bleed) } else { trim };
        if page.background != dac_layout::WHITE {
            out.fill(Path::rect(bg), paint(page.background));
        }
        let infos = page_infos(layout, photos, pi + 1, n);
        for (ci, cell) in layout.cells.iter().enumerate() {
            let r = prect(&cell.rect, margin, margin);
            match &cell.kind {
                CellKind::Photo(p) => {
                    let Some(id) = p.photo.as_deref() else {
                        out.fill(Path::rect(r), Paint::Gray(0.8));
                        continue;
                    };
                    let zoom = if p.zoom.is_finite() { f64::from(p.zoom.clamp(1.0, 20.0)) } else { 1.0 };
                    let long = ((r.w.max(r.h) * zoom * dpi / 72.0).ceil().max(1.0) as usize).min(MAX_PHOTO_PX);
                    let mut turns = p.rotate % 4;
                    let key = (id.to_string(), long, turns);
                    let found = match images.get(&key) {
                        Some(v) => Some(*v),
                        None => match photos.image(id, long) {
                            Ok(img) if img.width > 0 && img.height > 0 && img.data.len() == img.width.saturating_mul(img.height) => {
                                let img = match turns {
                                    1 => img.rotate_cw(),
                                    2 => img.rotate_180(),
                                    3 => img.rotate_180().rotate_cw(),
                                    _ => img,
                                };
                                let bytes = jpeg(&img, settings.job.jpeg_quality.max(80), None)?;
                                let iid =
                                    pdf.add_image(Image::jpeg(Arc::new(bytes), ColorSpace::Rgb)).map_err(|e| PrintError::Output(e.to_string()))?;
                                let v = (iid, img.width, img.height);
                                images.insert(key, v);
                                Some(v)
                            }
                            Ok(_) => {
                                warnings.push(format!("photo {id}: empty image"));
                                None
                            }
                            Err(e) => {
                                warnings.push(format!("photo {id}: {e}"));
                                None
                            }
                        },
                    };
                    let Some((iid, mut iw, mut ih)) = found else {
                        out.fill(Path::rect(r), Paint::Gray(0.8));
                        continue;
                    };
                    // rotate to fit: a quarter turn more when orientations disagree
                    let mut iid = iid;
                    if p.rotate_to_fit && iw != ih && ((iw > ih) != (r.w > r.h)) {
                        turns = (turns + 1) % 4;
                        let k2 = (id.to_string(), long, turns);
                        if let Some(v) = images.get(&k2) {
                            (iid, iw, ih) = *v;
                        } else if let Ok(img) = photos.image(id, long) {
                            let img = match turns {
                                1 => img.rotate_cw(),
                                2 => img.rotate_180(),
                                3 => img.rotate_180().rotate_cw(),
                                _ => img,
                            };
                            if img.width > 0 && img.height > 0 && img.data.len() == img.width.saturating_mul(img.height) {
                                let bytes = jpeg(&img, settings.job.jpeg_quality.max(80), None)?;
                                iid = pdf.add_image(Image::jpeg(Arc::new(bytes), ColorSpace::Rgb)).map_err(|e| PrintError::Output(e.to_string()))?;
                                (iw, ih) = (img.width, img.height);
                                images.insert(k2, (iid, iw, ih));
                            }
                        }
                    }
                    let cell_pt = dac_layout::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
                    let (s, ox, oy) = photo_placement(cell_pt, iw as f32, ih as f32, p);
                    if !(s.is_finite() && s > 0.0) {
                        continue;
                    }
                    let shown = PRect::new(f64::from(ox), f64::from(oy), iw as f64 * f64::from(s), ih as f64 * f64::from(s));
                    out.save();
                    out.clip(Path::rect(r));
                    out.image(iid, shown);
                    out.restore();
                    if let Some(st) = p.stroke.filter(|s| s.width > 0.0 && s.width.is_finite()) {
                        let x0 = r.x.max(shown.x);
                        let y0 = r.y.max(shown.y);
                        let x1 = (r.x + r.w).min(shown.x + shown.w);
                        let y1 = (r.y + r.h).min(shown.y + shown.h);
                        let w = f64::from(st.width);
                        if x1 - x0 > w && y1 - y0 > w {
                            out.stroke(Path::rect(PRect::new(x0, y0, x1 - x0, y1 - y0).inset(w / 2.0)), PStroke::solid(paint(st.color), w));
                        }
                    }
                }
                CellKind::Graphic(g) => {
                    let path = |r: PRect| match g.shape {
                        Shape::Rect => Path::rect(r),
                        Shape::Ellipse => Path::ellipse(r),
                    };
                    if let Some(f) = g.fill {
                        out.fill(path(r), paint(f));
                    }
                    if let Some(st) = g.stroke.filter(|s| s.width > 0.0 && s.width.is_finite()) {
                        let w = f64::from(st.width);
                        out.stroke(path(r.inset(w / 2.0)), PStroke::solid(paint(st.color), w));
                    }
                }
                CellKind::Text(t) => {
                    let s = text_of(t, &infos, pi + 1, n);
                    if s.is_empty() || cell.rect.w <= 0.0 || cell.rect.h <= 0.0 {
                        continue;
                    }
                    let l = text.layout_pt(&s, &t.style, cell.rect.w, cell.rect.h);
                    if l.glyphs.is_empty() && !s.trim().is_empty() {
                        warnings.push(format!("page {} cell {ci}: no glyphs for text", pi + 1));
                    }
                    out.text(&l, &s, (r.x, r.y), 1.0);
                }
            }
        }
        if settings.options.crop_marks {
            out.crop_marks(trim, MARK_OFFSET.max(bleed), MARK_LENGTH, MARK_WIDTH);
        }
        pdf.push_page(out).map_err(|e| PrintError::Output(e.to_string()))?;
    }
    let bytes = pdf.to_bytes().map_err(|e| PrintError::Output(e.to_string()))?;
    Ok(Output { files: vec![bytes], pages: n, warnings })
}

/// Renders every page of `doc` to a JPEG at `settings.job.dpi`.
pub fn to_jpeg(doc: &Document, settings: &PrintSettings, photos: &dyn PhotoSource, text: &ShapedText, progress: Progress) -> Result<Output> {
    doc.validate()?;
    let n = doc.pages.len();
    let mut out = Output { pages: n, ..Output::default() };
    let ppi = settings.job.dpi.round().clamp(1.0, 65_535.0) as u16;
    for (i, p) in doc.pages.iter().enumerate() {
        if !progress(i, n) {
            return Err(PrintError::Cancelled);
        }
        let opt = RenderOptions { dpi: settings.job.dpi, include_bleed: false, show_guides: false, page: i + 1, pages: n };
        let r = render_page(&doc.page, p, photos, text, &opt)?;
        out.warnings.extend(r.warnings.into_iter().map(|w| format!("page {}: {w}", i + 1)));
        out.files.push(jpeg(&r.image, settings.job.jpeg_quality, Some(ppi))?);
    }
    Ok(out)
}

/// A photo source over images already rendered (e.g. by the engine on a worker thread).
#[derive(Default)]
pub struct MapSource {
    pub images: HashMap<String, Rgba8>,
    pub infos: HashMap<String, PhotoInfo>,
}

impl PhotoSource for MapSource {
    fn image(&self, photo: &str, _long_side: usize) -> std::result::Result<Rgba8, String> {
        self.images.get(photo).cloned().ok_or_else(|| "not rendered".to_string())
    }
    fn info(&self, photo: &str) -> PhotoInfo {
        self.infos.get(photo).cloned().unwrap_or_default()
    }
}

/// The longest side (pixels) each photo of `doc` is needed at for `dpi`.
pub fn needed_sizes(doc: &Document, dpi: f32) -> HashMap<String, usize> {
    let mut m: HashMap<String, usize> = HashMap::new();
    let k = if dpi.is_finite() { dpi.clamp(1.0, 1200.0) / 72.0 } else { 1.0 };
    for p in &doc.pages {
        for c in &p.cells {
            if let Some(ph) = c.as_photo()
                && let Some(id) = &ph.photo
            {
                let zoom = if ph.zoom.is_finite() { ph.zoom.clamp(1.0, 20.0) } else { 1.0 };
                let long = ((c.rect.w.max(c.rect.h) * zoom * k).ceil().max(1.0) as usize).min(MAX_PHOTO_PX);
                let e = m.entry(id.clone()).or_insert(0);
                *e = (*e).max(long);
            }
        }
    }
    m
}
