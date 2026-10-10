//! PDF 1.7 writer for print, books and contact sheets (L1).
//!
//! A [`Document`] holds pages, images and metadata; [`Document::to_bytes`] writes the file:
//!
//! * **pages** with media, bleed, trim, crop and art boxes ([`Page`]);
//! * **images**: JPEG passthrough (DCTDecode) and 8/16-bit samples with Flate, in Gray, RGB,
//!   CMYK or an ICC-based colour space, with an optional alpha soft mask ([`Image`]);
//! * **text** laid out by `dac-text` ([`Page::text`]): every face is subset to the glyphs used
//!   and embedded (TrueType and CFF outlines, CJK and vertical type included), with a ToUnicode
//!   map; faces that cannot be embedded are drawn as outlines;
//! * **vector paths** for guides, borders, crop and registration marks ([`Path`], [`Page::fill`],
//!   [`Page::stroke`], [`Page::crop_marks`]);
//! * **colour**: ICC-based image colour spaces and a document **OutputIntent** ([`OutputIntent`]);
//! * **metadata**: the Info dictionary and an XMP packet ([`Metadata`]).
//!
//! Coordinates are PDF points (1/72 in) with the origin at the **top-left** of the media box and
//! y pointing down, like the app's layout model; the writer flips them.
//!
//! Built on `pdf-writer` (object serialization) and `subsetter` (font subsetting), both
//! MIT/Apache-2.0, pure Rust. Sources: ISO 32000-1 (PDF 1.7); own design.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod font;
mod image;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, TextRenderingMode};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect as PRect, Ref, Str, TextStr};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

pub use image::{Image, ImageData, JpegInfo, jpeg_info};

use font::{FaceKey, FaceUse};

/// Errors from building or writing a document.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfError {
    /// A page size or box is not finite or outside PDF's limits.
    Page(String),
    /// An image is malformed or does not match its colour space.
    Image(String),
    /// An image id from another document.
    UnknownImage(usize),
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfError::Page(m) => write!(f, "PDF page: {m}"),
            PdfError::Image(m) => write!(f, "PDF image: {m}"),
            PdfError::UnknownImage(i) => write!(f, "PDF image #{i} does not exist in this document"),
        }
    }
}

impl std::error::Error for PdfError {}

/// Smallest and largest page side PDF allows (ISO 32000-1 Annex C: 3 to 14 400 units).
pub const MIN_SIDE: f64 = 3.0;
pub const MAX_SIDE: f64 = 14_400.0;

/// A rectangle in points, origin top-left of the page, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// `self` shrunk by `d` on every side.
    pub fn inset(self, d: f64) -> Self {
        Self { x: self.x + d, y: self.y + d, w: self.w - 2.0 * d, h: self.h - 2.0 * d }
    }

    fn is_finite(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite())
    }
}

/// A colour space for images (and the OutputIntent).
#[derive(Clone, Debug, PartialEq)]
pub enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// An ICC profile with `components` (1, 3 or 4) channels, e.g. the image's sRGB or Adobe RGB
    /// profile. Shared by pointer: pass the same `Arc` to write the profile once.
    Icc {
        profile: Arc<Vec<u8>>,
        components: u8,
    },
}

impl ColorSpace {
    pub fn components(&self) -> usize {
        match self {
            ColorSpace::Gray => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Icc { components, .. } => usize::from(*components),
        }
    }
}

/// A device colour for fills, strokes and text (components 0-1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Gray(f32),
    Rgb(f32, f32, f32),
    Cmyk(f32, f32, f32, f32),
}

impl Paint {
    pub const BLACK: Paint = Paint::Gray(0.0);
    pub const WHITE: Paint = Paint::Gray(1.0);
    /// Registration colour (100 % of every process ink), for crop and registration marks.
    pub const REGISTRATION: Paint = Paint::Cmyk(1.0, 1.0, 1.0, 1.0);

    fn set_fill(&self, c: &mut Content) {
        let f = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match *self {
            Paint::Gray(g) => c.set_fill_gray(f(g)),
            Paint::Rgb(r, g, b) => c.set_fill_rgb(f(r), f(g), f(b)),
            Paint::Cmyk(cy, m, y, k) => c.set_fill_cmyk(f(cy), f(m), f(y), f(k)),
        };
    }

    fn set_stroke(&self, c: &mut Content) {
        let f = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match *self {
            Paint::Gray(g) => c.set_stroke_gray(f(g)),
            Paint::Rgb(r, g, b) => c.set_stroke_rgb(f(r), f(g), f(b)),
            Paint::Cmyk(cy, m, y, k) => c.set_stroke_cmyk(f(cy), f(m), f(y), f(k)),
        };
    }
}

/// One segment of a [`Path`] (points, origin top-left, y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CubicTo(f64, f64, f64, f64, f64, f64),
    Close,
}

/// A vector path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path(pub Vec<Seg>);

impl Path {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn move_to(mut self, x: f64, y: f64) -> Self {
        self.0.push(Seg::MoveTo(x, y));
        self
    }
    pub fn line_to(mut self, x: f64, y: f64) -> Self {
        self.0.push(Seg::LineTo(x, y));
        self
    }
    pub fn cubic_to(mut self, x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) -> Self {
        self.0.push(Seg::CubicTo(x1, y1, x2, y2, x, y));
        self
    }
    pub fn close(mut self) -> Self {
        self.0.push(Seg::Close);
        self
    }
    /// A closed rectangle.
    pub fn rect(r: Rect) -> Self {
        Self::new().move_to(r.x, r.y).line_to(r.x + r.w, r.y).line_to(r.x + r.w, r.y + r.h).line_to(r.x, r.y + r.h).close()
    }
    /// A line from `a` to `b`.
    pub fn line(a: (f64, f64), b: (f64, f64)) -> Self {
        Self::new().move_to(a.0, a.1).line_to(b.0, b.1)
    }
    /// An ellipse inscribed in `r` (four cubic arcs).
    pub fn ellipse(r: Rect) -> Self {
        const K: f64 = 0.552_284_749_8;
        let (cx, cy, rx, ry) = (r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0, r.h / 2.0);
        Self::new()
            .move_to(cx + rx, cy)
            .cubic_to(cx + rx, cy + K * ry, cx + K * rx, cy + ry, cx, cy + ry)
            .cubic_to(cx - K * rx, cy + ry, cx - rx, cy + K * ry, cx - rx, cy)
            .cubic_to(cx - rx, cy - K * ry, cx - K * rx, cy - ry, cx, cy - ry)
            .cubic_to(cx + K * rx, cy - ry, cx + rx, cy - K * ry, cx + rx, cy)
            .close()
    }
}

/// Stroke style.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub paint: Paint,
    /// Line width in points.
    pub width: f64,
    /// Dash lengths (points) and phase; `None` = solid.
    pub dash: Option<(Vec<f64>, f64)>,
}

impl Stroke {
    pub fn solid(paint: Paint, width: f64) -> Self {
        Self { paint, width, dash: None }
    }
}

/// Handle of an image added with [`Document::add_image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageId(usize);

/// A glyph to show (resolved to a font and CID when the document is written).
#[derive(Clone, Debug)]
struct GlyphOp {
    face: FaceKey,
    gid: u16,
    size: f32,
    /// Glyph space (outline coordinates at `size`, y up) -> PDF user space (y up).
    m: [f64; 6],
    paint: Paint,
    /// Faux bold: stroke width in points.
    bold: Option<f64>,
    coords: Vec<i16>,
}

#[derive(Clone, Debug)]
enum Op {
    Image { id: ImageId, rect: Rect },
    Fill { path: Path, paint: Paint, even_odd: bool },
    Stroke { path: Path, stroke: Stroke },
    Glyph(Box<GlyphOp>),
    Save,
    Restore,
    Clip(Path),
}

/// One page. Boxes are in points, origin top-left of the media box.
#[derive(Clone, Debug)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    /// Area to which the content is clipped in production (sheet plus bleed).
    pub bleed: Option<Rect>,
    /// The finished page after trimming.
    pub trim: Option<Rect>,
    pub crop: Option<Rect>,
    pub art: Option<Rect>,
    ops: Vec<Op>,
    faces: Vec<(FaceKey, dac_text::FontData, bool)>,
    chars: Vec<(FaceKey, String)>,
}

impl Page {
    /// A blank page of `width` x `height` points.
    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height, bleed: None, trim: None, crop: None, art: None, ops: Vec::new(), faces: Vec::new(), chars: Vec::new() }
    }

    /// Draws `image` scaled into `rect`.
    pub fn image(&mut self, image: ImageId, rect: Rect) {
        if rect.is_finite() {
            self.ops.push(Op::Image { id: image, rect });
        }
    }

    pub fn fill(&mut self, path: Path, paint: Paint) {
        self.ops.push(Op::Fill { path, paint, even_odd: false });
    }

    pub fn fill_even_odd(&mut self, path: Path, paint: Paint) {
        self.ops.push(Op::Fill { path, paint, even_odd: true });
    }

    pub fn stroke(&mut self, path: Path, stroke: Stroke) {
        self.ops.push(Op::Stroke { path, stroke });
    }

    /// Saves the graphics state (pair with [`Page::restore`]).
    pub fn save(&mut self) {
        self.ops.push(Op::Save);
    }

    pub fn restore(&mut self) {
        self.ops.push(Op::Restore);
    }

    /// Intersects the clip with `path` (until the matching [`Page::restore`]).
    pub fn clip(&mut self, path: Path) {
        self.ops.push(Op::Clip(path));
    }

    /// Crop (trim) marks at the corners of `trim`: lines of `length` points starting `offset`
    /// points outside the trim edge, in registration colour.
    pub fn crop_marks(&mut self, trim: Rect, offset: f64, length: f64, width: f64) {
        let (x0, y0, x1, y1) = (trim.x, trim.y, trim.x + trim.w, trim.y + trim.h);
        let s = Stroke::solid(Paint::REGISTRATION, width);
        for (x, dx) in [(x0, -1.0), (x1, 1.0)] {
            for (y, dy) in [(y0, -1.0), (y1, 1.0)] {
                self.stroke(Path::line((x + dx * offset, y), (x + dx * (offset + length), y)), s.clone());
                self.stroke(Path::line((x, y + dy * offset), (x, y + dy * (offset + length))), s.clone());
            }
        }
    }

    /// A registration target (circle with cross hairs) of diameter `d` centred at `(cx, cy)`.
    pub fn registration_mark(&mut self, cx: f64, cy: f64, d: f64, width: f64) {
        let s = Stroke::solid(Paint::REGISTRATION, width);
        let r = d / 2.0;
        self.stroke(Path::ellipse(Rect::new(cx - r * 0.6, cy - r * 0.6, r * 1.2, r * 1.2)), s.clone());
        self.stroke(Path::line((cx - r, cy), (cx + r, cy)), s.clone());
        self.stroke(Path::line((cx, cy - r), (cx, cy + r)), s);
    }

    /// Draws a text layout: text space (pixels of `layout`) is mapped to the page by
    /// `pt_per_px` (72 / the layout's dpi) with the text origin at `origin` (points). `text` is
    /// the laid-out string (for searchable text). Colours come from the styles (RGB; alpha is
    /// ignored).
    pub fn text(&mut self, layout: &dac_text::TextLayout, text: &str, origin: (f64, f64), pt_per_px: f64) {
        if !(origin.0.is_finite() && origin.1.is_finite() && pt_per_px.is_finite() && pt_per_px > 0.0) {
            return;
        }
        // Text space (y down) -> page user space with y down; flipped when written.
        let place = dac_text::Xform([pt_per_px, 0.0, 0.0, pt_per_px, origin.0, origin.1]);
        for g in &layout.glyphs {
            let (Some(face), Some(st)) = (layout.faces.get(g.face as usize), layout.styles.get(g.style as usize)) else { continue };
            let Ok(gid) = u16::try_from(g.id) else { continue };
            let key = FaceKey { blob: face.font.data.id(), index: face.font.index };
            let variable = face.coords.iter().any(|&c| c != 0);
            if !self.faces.iter().any(|f| f.0 == key) {
                self.faces.push((key, face.font.clone(), variable));
            } else if variable && let Some(f) = self.faces.iter_mut().find(|f| f.0 == key) {
                f.2 = true;
            }
            let gx = dac_text::render::glyph_xform(layout, g, face.skew_deg);
            let m = place.mul(&gx).0;
            let bold = (st.faux_bold || face.embolden).then(|| f64::from(face.size_px * dac_text::render::FAUX_BOLD_RADIUS) * 2.0 * pt_per_px);
            let c = st.color;
            self.ops.push(Op::Glyph(Box::new(GlyphOp {
                face: key,
                gid,
                size: face.size_px,
                m,
                paint: Paint::Rgb(c.r, c.g, c.b),
                bold,
                coords: face.coords.clone(),
            })));
        }
        for d in &layout.decorations {
            let Some(st) = layout.styles.get(d.style as usize) else { continue };
            let pts = [(d.x0, d.y0), (d.x1, d.y0), (d.x1, d.y1), (d.x0, d.y1)].map(|(x, y)| place.apply(f64::from(x), f64::from(y)));
            let path =
                Path::new().move_to(pts[0].0, pts[0].1).line_to(pts[1].0, pts[1].1).line_to(pts[2].0, pts[2].1).line_to(pts[3].0, pts[3].1).close();
            self.fill(path, Paint::Rgb(st.color.r, st.color.g, st.color.b));
        }
        for f in layout.faces.iter().map(|f| FaceKey { blob: f.font.data.id(), index: f.font.index }) {
            if !self.chars.iter().any(|c| c.0 == f) {
                self.chars.push((f, text.to_string()));
            } else if let Some(c) = self.chars.iter_mut().find(|c| c.0 == f) {
                c.1.push_str(text);
            }
        }
    }
}

/// Document information (Info dictionary and XMP).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Vec<String>,
    /// The application that created the content.
    pub creator: Option<String>,
    /// The library that wrote the PDF.
    pub producer: Option<String>,
    pub created: Option<DateTime>,
    pub modified: Option<DateTime>,
}

/// A UTC date and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    fn pdf(&self) -> pdf_writer::Date {
        pdf_writer::Date::new(self.year.min(9999))
            .month(self.month)
            .day(self.day)
            .hour(self.hour)
            .minute(self.minute)
            .second(self.second)
            .utc_offset_hour(0)
            .utc_offset_minute(0)
    }

    fn iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year.min(9999),
            self.month.clamp(1, 12),
            self.day.clamp(1, 31),
            self.hour.min(23),
            self.minute.min(59),
            self.second.min(59)
        )
    }
}

/// The intended output condition (a print profile), written as `/OutputIntents`.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputIntent {
    /// Subtype name: `GTS_PDFX` (PDF/X), `GTS_PDFA1` (PDF/A) or another registered name.
    pub subtype: String,
    /// e.g. `"FOGRA39"` or a profile description.
    pub identifier: String,
    pub info: Option<String>,
    /// The destination ICC profile (output or display class) and its component count.
    pub profile: Arc<Vec<u8>>,
    pub components: u8,
}

/// A PDF document under construction.
#[derive(Clone, Debug, Default)]
pub struct Document {
    pages: Vec<Page>,
    images: Vec<Image>,
    pub metadata: Metadata,
    pub output_intent: Option<OutputIntent>,
}

/// Largest content we accept before refusing to write (guards absurd inputs).
const MAX_IMAGES: usize = 1 << 20;

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an image (validated now) and returns its handle.
    pub fn add_image(&mut self, image: Image) -> Result<ImageId, PdfError> {
        image.validate()?;
        if self.images.len() >= MAX_IMAGES {
            return Err(PdfError::Image("too many images".into()));
        }
        self.images.push(image);
        Ok(ImageId(self.images.len() - 1))
    }

    /// Appends a page.
    pub fn push_page(&mut self, page: Page) -> Result<(), PdfError> {
        let ok = |v: f64| v.is_finite() && (MIN_SIDE..=MAX_SIDE).contains(&v);
        if !ok(page.width) || !ok(page.height) {
            return Err(PdfError::Page(format!("{} x {} pt is outside {MIN_SIDE}-{MAX_SIDE} pt", page.width, page.height)));
        }
        for (name, b) in [("bleed", page.bleed), ("trim", page.trim), ("crop", page.crop), ("art", page.art)] {
            if let Some(b) = b
                && (!b.is_finite() || b.w <= 0.0 || b.h <= 0.0)
            {
                return Err(PdfError::Page(format!("{name} box {b:?}")));
            }
        }
        for op in &page.ops {
            if let Op::Image { id, .. } = op
                && id.0 >= self.images.len()
            {
                return Err(PdfError::UnknownImage(id.0));
            }
        }
        self.pages.push(page);
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Writes the PDF file.
    pub fn to_bytes(&self) -> Result<Vec<u8>, PdfError> {
        Writer::default().write(self)
    }
}

#[derive(Default)]
struct Writer {
    next: i32,
}

impl Writer {
    fn alloc(&mut self) -> Ref {
        self.next += 1;
        Ref::new(self.next)
    }

    fn write(mut self, doc: &Document) -> Result<Vec<u8>, PdfError> {
        if let Some(oi) = &doc.output_intent
            && !matches!(oi.components, 1 | 3 | 4)
        {
            return Err(PdfError::Image(format!("an output intent profile with {} components (1, 3 or 4 supported)", oi.components)));
        }
        let mut pdf = Pdf::new();
        pdf.set_version(1, 7);
        let catalog_id = self.alloc();
        let tree_id = self.alloc();

        // Fonts used anywhere, embedded once.
        let mut uses: BTreeMap<FaceKey, FaceUse> = BTreeMap::new();
        for p in &doc.pages {
            for (key, font, variable) in &p.faces {
                let u = uses.entry(*key).or_insert_with(|| FaceUse {
                    font: font.clone(),
                    glyphs: Default::default(),
                    chars: Default::default(),
                    outlines_only: false,
                });
                u.outlines_only |= *variable;
            }
            for (key, text) in &p.chars {
                if let Some(u) = uses.get_mut(key) {
                    u.chars.extend(text.chars());
                }
            }
            for op in &p.ops {
                if let Op::Glyph(g) = op
                    && let Some(u) = uses.get_mut(&g.face)
                {
                    u.glyphs.insert(g.gid);
                }
            }
        }
        let mut fonts: HashMap<FaceKey, (Ref, font::Embedded)> = HashMap::new();
        for (key, u) in &uses {
            if let Some(e) = u.embed() {
                let id = self.alloc();
                self.write_font(&mut pdf, id, &e);
                fonts.insert(*key, (id, e));
            }
        }

        // ICC profiles (by pointer) and images.
        let mut icc: HashMap<usize, Ref> = HashMap::new();
        let mut image_refs = Vec::with_capacity(doc.images.len());
        for img in &doc.images {
            let r = self.write_image(&mut pdf, img, &mut icc)?;
            image_refs.push(r);
        }

        // Pages.
        let mut page_ids = Vec::with_capacity(doc.pages.len());
        for p in &doc.pages {
            let page_id = self.alloc();
            let content_id = self.alloc();
            page_ids.push(page_id);
            let content = page_content(p, &fonts, &uses, &image_refs);
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
            pdf.stream(content_id, &compressed).filter(Filter::FlateDecode);
            let flip = |r: Rect| PRect::new(r.x as f32, (p.height - r.y - r.h) as f32, (r.x + r.w) as f32, (p.height - r.y) as f32);
            let mut page = pdf.page(page_id);
            page.parent(tree_id);
            page.media_box(PRect::new(0.0, 0.0, p.width as f32, p.height as f32));
            if let Some(b) = p.crop {
                page.crop_box(flip(b));
            }
            if let Some(b) = p.bleed {
                page.bleed_box(flip(b));
            }
            if let Some(b) = p.trim {
                page.trim_box(flip(b));
            }
            if let Some(b) = p.art {
                page.art_box(flip(b));
            }
            page.contents(content_id);
            let mut res = page.resources();
            {
                let mut fdict = res.fonts();
                for (i, key) in p.faces.iter().map(|f| f.0).enumerate() {
                    if let Some((id, _)) = fonts.get(&key) {
                        fdict.pair(Name(format!("F{i}").as_bytes()), *id);
                    }
                }
            }
            {
                let mut xo = res.x_objects();
                let mut seen = Vec::new();
                for op in &p.ops {
                    if let Op::Image { id, .. } = op
                        && !seen.contains(&id.0)
                        && let Some(r) = image_refs.get(id.0)
                    {
                        seen.push(id.0);
                        xo.pair(Name(format!("Im{}", id.0).as_bytes()), *r);
                    }
                }
            }
            res.finish();
            page.finish();
        }
        pdf.pages(tree_id).kids(page_ids.iter().copied()).count(page_ids.len() as i32);

        // Output intent profile.
        let intent = match &doc.output_intent {
            Some(oi) => {
                let id = self.alloc();
                let mut prof = pdf.icc_profile(id, &oi.profile);
                prof.n(i32::from(oi.components));
                prof.finish();
                Some((oi, id))
            }
            None => None,
        };

        // Metadata.
        let xmp_id = self.alloc();
        let xmp = xmp_packet(&doc.metadata);
        pdf.metadata(xmp_id, xmp.as_bytes());
        let info_id = self.alloc();
        {
            let m = &doc.metadata;
            let mut info = pdf.document_info(info_id);
            if let Some(t) = &m.title {
                info.title(TextStr(t));
            }
            if let Some(t) = &m.author {
                info.author(TextStr(t));
            }
            if let Some(t) = &m.subject {
                info.subject(TextStr(t));
            }
            if !m.keywords.is_empty() {
                info.keywords(TextStr(&m.keywords.join(", ")));
            }
            if let Some(t) = &m.creator {
                info.creator(TextStr(t));
            }
            if let Some(t) = &m.producer {
                info.producer(TextStr(t));
            }
            if let Some(d) = m.created {
                info.creation_date(d.pdf());
            }
            if let Some(d) = m.modified {
                info.modified_date(d.pdf());
            }
        }

        let mut cat = pdf.catalog(catalog_id);
        cat.pages(tree_id);
        cat.metadata(xmp_id);
        if let Some((oi, profile)) = intent {
            let mut arr = cat.output_intents();
            let mut o = arr.push();
            o.subtype(pdf_writer::types::OutputIntentSubtype::Custom(Name(oi.subtype.as_bytes())));
            o.output_condition_identifier(TextStr(&oi.identifier));
            if let Some(i) = &oi.info {
                o.info(TextStr(i));
            }
            o.dest_output_profile(profile);
        }
        cat.finish();

        // File identifier: a hash of the page count and metadata (stable for equal input).
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in xmp.bytes().chain((doc.pages.len() as u64).to_le_bytes()) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        let id = h.to_be_bytes().repeat(2);
        pdf.set_file_id((id.clone(), id));
        Ok(pdf.finish())
    }

    fn write_font(&mut self, pdf: &mut Pdf, type0_id: Ref, e: &font::Embedded) {
        let cid_id = self.alloc();
        let desc_id = self.alloc();
        let file_id = self.alloc();
        let cmap_id = self.alloc();
        let base = Name(e.base_font.as_bytes());
        pdf.type0_font(type0_id).base_font(base).encoding_predefined(Name(b"Identity-H")).descendant_font(cid_id).to_unicode(cmap_id);
        let mut cid = pdf.cid_font(cid_id);
        cid.subtype(if e.cff { CidFontType::Type0 } else { CidFontType::Type2 });
        cid.base_font(base);
        cid.system_info(SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"Identity"), supplement: 0 });
        cid.font_descriptor(desc_id);
        cid.default_width(0.0);
        if !e.cff {
            cid.cid_to_gid_map_predefined(Name(b"Identity"));
        }
        {
            let mut w = cid.widths();
            for (c, width) in &e.widths {
                w.consecutive(*c, [*width]);
            }
        }
        cid.finish();
        let mut flags = FontFlags::SYMBOLIC;
        if e.serif {
            flags |= FontFlags::SERIF;
        }
        if e.fixed {
            flags |= FontFlags::FIXED_PITCH;
        }
        if e.italic_angle != 0.0 {
            flags |= FontFlags::ITALIC;
        }
        let mut d = pdf.font_descriptor(desc_id);
        d.name(base)
            .flags(flags)
            .bbox(PRect::new(e.bbox[0], e.bbox[1], e.bbox[2], e.bbox[3]))
            .italic_angle(e.italic_angle)
            .ascent(e.ascent)
            .descent(e.descent)
            .cap_height(e.cap_height)
            .stem_v(80.0);
        if e.cff {
            d.font_file3(file_id);
        } else {
            d.font_file2(file_id);
        }
        d.finish();
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&e.data, 6);
        let mut s = pdf.stream(file_id, &compressed);
        s.filter(Filter::FlateDecode);
        if e.cff {
            s.pair(Name(b"Subtype"), Name(b"OpenType"));
        } else {
            s.pair(Name(b"Length1"), e.data.len() as i32);
        }
        s.finish();
        let mut cmap =
            pdf_writer::types::UnicodeCmap::<u16>::new(Name(b"Custom"), SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"UCS"), supplement: 0 });
        for (cid, ch) in &e.to_unicode {
            cmap.pair(*cid, *ch);
        }
        let cmap = cmap.finish();
        pdf.cmap(cmap_id, cmap.as_slice());
    }

    fn color_space(&mut self, pdf: &mut Pdf, cs: &ColorSpace, icc: &mut HashMap<usize, Ref>) -> Option<Ref> {
        let ColorSpace::Icc { profile, components } = cs else { return None };
        let key = Arc::as_ptr(profile) as usize;
        if let Some(r) = icc.get(&key) {
            return Some(*r);
        }
        let id = self.alloc();
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(profile, 6);
        let mut p = pdf.icc_profile(id, &compressed);
        p.filter(Filter::FlateDecode);
        p.n(i32::from(*components));
        match components {
            1 => p.alternate().device_gray(),
            4 => p.alternate().device_cmyk(),
            _ => p.alternate().device_rgb(),
        };
        p.finish();
        icc.insert(key, id);
        Some(id)
    }

    fn write_image(&mut self, pdf: &mut Pdf, img: &Image, icc: &mut HashMap<usize, Ref>) -> Result<Ref, PdfError> {
        let c = img.validate()?;
        let icc_ref = self.color_space(pdf, &img.color, icc);
        let id = self.alloc();
        let mask_id = img.alpha.as_ref().map(|_| self.alloc());
        let (bytes, filter) = match &img.data {
            ImageData::Jpeg(b) => (b.as_slice().to_vec(), Filter::DctDecode),
            ImageData::Samples { data, .. } => (miniz_oxide::deflate::compress_to_vec_zlib(data, 6), Filter::FlateDecode),
        };
        let mut x = pdf.image_xobject(id, &bytes);
        x.filter(filter);
        x.width(c.width as i32);
        x.height(c.height as i32);
        x.bits_per_component(i32::from(c.bits));
        x.interpolate(img.interpolate);
        match (&img.color, icc_ref) {
            (_, Some(r)) => x.color_space().icc_based(r),
            (ColorSpace::Gray, _) => x.color_space().device_gray(),
            (ColorSpace::Cmyk, _) => x.color_space().device_cmyk(),
            _ => x.color_space().device_rgb(),
        }
        if c.adobe_inverted {
            x.decode([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
        }
        if let Some(m) = mask_id {
            x.s_mask(m);
        }
        x.finish();
        if let (Some(m), Some(alpha)) = (mask_id, &img.alpha) {
            let a = miniz_oxide::deflate::compress_to_vec_zlib(alpha, 6);
            let mut s = pdf.image_xobject(m, &a);
            s.filter(Filter::FlateDecode);
            s.width(c.width as i32);
            s.height(c.height as i32);
            s.bits_per_component(8);
            s.color_space().device_gray();
            s.finish();
        }
        Ok(id)
    }
}

fn finite(v: f64) -> f32 {
    if v.is_finite() { v.clamp(-1e7, 1e7) as f32 } else { 0.0 }
}

/// Appends `path` (page coordinates, y down) to `c`, flipped to PDF space.
fn emit_path(c: &mut Content, path: &Path, h: f64) {
    for s in &path.0 {
        match *s {
            Seg::MoveTo(x, y) => {
                c.move_to(finite(x), finite(h - y));
            }
            Seg::LineTo(x, y) => {
                c.line_to(finite(x), finite(h - y));
            }
            Seg::CubicTo(x1, y1, x2, y2, x, y) => {
                c.cubic_to(finite(x1), finite(h - y1), finite(x2), finite(h - y2), finite(x), finite(h - y));
            }
            Seg::Close => {
                c.close_path();
            }
        }
    }
}

/// Records a glyph outline as path segments in PDF space.
struct GlyphPen<'a> {
    c: &'a mut Content,
    m: [f64; 6],
    /// Current point (glyph space), for quadratic-to-cubic conversion.
    last: (f32, f32),
}

impl GlyphPen<'_> {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.m;
        let (x, y) = (f64::from(x), f64::from(y));
        (finite(a * x + c * y + e), finite(b * x + d * y + f))
    }
}

impl OutlinePen for GlyphPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.c.move_to(p.0, p.1);
        self.last = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.c.line_to(p.0, p.1);
        self.last = (x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        // Degree elevation (exact): c1 = p0 + 2/3 (c - p0), c2 = p + 2/3 (c - p).
        let (x0, y0) = self.last;
        let a = self.map(x0 + 2.0 / 3.0 * (cx - x0), y0 + 2.0 / 3.0 * (cy - y0));
        let b = self.map(x + 2.0 / 3.0 * (cx - x), y + 2.0 / 3.0 * (cy - y));
        let p = self.map(x, y);
        self.c.cubic_to(a.0, a.1, b.0, b.1, p.0, p.1);
        self.last = (x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.c.cubic_to(a.0, a.1, b.0, b.1, p.0, p.1);
        self.last = (x, y);
    }
    fn close(&mut self) {
        self.c.close_path();
    }
}

fn page_content(p: &Page, fonts: &HashMap<FaceKey, (Ref, font::Embedded)>, uses: &BTreeMap<FaceKey, FaceUse>, images: &[Ref]) -> Vec<u8> {
    let mut c = Content::new();
    let h = p.height;
    // Page y-down -> PDF y-up for glyph matrices.
    let flip = |m: [f64; 6]| -> [f64; 6] {
        let [a, b, cc, d, e, f] = m;
        [a, -b, cc, -d, e, h - f]
    };
    let mut depth = 0usize;
    for op in &p.ops {
        match op {
            Op::Save => {
                depth += 1;
                c.save_state();
            }
            Op::Restore => {
                if depth > 0 {
                    depth -= 1;
                    c.restore_state();
                }
            }
            Op::Clip(path) => {
                emit_path(&mut c, path, h);
                c.clip_nonzero();
                c.end_path();
            }
            Op::Image { id, rect } => {
                if images.get(id.0).is_none() {
                    continue;
                }
                c.save_state();
                c.transform([finite(rect.w), 0.0, 0.0, finite(rect.h), finite(rect.x), finite(h - rect.y - rect.h)]);
                c.x_object(Name(format!("Im{}", id.0).as_bytes()));
                c.restore_state();
            }
            Op::Fill { path, paint, even_odd } => {
                paint.set_fill(&mut c);
                emit_path(&mut c, path, h);
                if *even_odd {
                    c.fill_even_odd();
                } else {
                    c.fill_nonzero();
                }
            }
            Op::Stroke { path, stroke } => {
                stroke.paint.set_stroke(&mut c);
                c.set_line_width(finite(stroke.width.max(0.0)));
                if let Some((dash, phase)) = &stroke.dash {
                    c.set_dash_pattern(dash.iter().map(|d| finite(d.max(0.0))), finite(*phase));
                } else {
                    c.set_dash_pattern([], 0.0);
                }
                emit_path(&mut c, path, h);
                c.stroke();
            }
            Op::Glyph(g) => {
                let m = flip(g.m);
                if !m.iter().all(|v| v.is_finite()) {
                    continue;
                }
                g.paint.set_fill(&mut c);
                let font = fonts.get(&g.face).and_then(|(_, e)| Some((e.remap.get(&g.gid)?, p.faces.iter().position(|f| f.0 == g.face)?)));
                match font {
                    Some((&cid, index)) => {
                        c.begin_text();
                        c.set_font(Name(format!("F{index}").as_bytes()), g.size);
                        if let Some(w) = g.bold {
                            g.paint.set_stroke(&mut c);
                            c.set_line_width(finite(w));
                            c.set_text_rendering_mode(TextRenderingMode::FillStroke);
                        }
                        // Tf scales normalized glyph units to the font size: the space the glyph
                        // transform starts from (outline coordinates at that size).
                        c.set_text_matrix(m.map(finite));
                        c.show(Str(&cid.to_be_bytes()));
                        c.end_text();
                    }
                    None => {
                        // Outline fallback (variable instances, fonts we may not embed).
                        let Some(u) = uses.get(&g.face) else { continue };
                        let Ok(f) = skrifa::FontRef::from_index(u.font.data.as_ref(), u.font.index) else { continue };
                        let Some(outline) = f.outline_glyphs().get(GlyphId::new(u32::from(g.gid))) else { continue };
                        let coords: Vec<NormalizedCoord> = g.coords.iter().map(|&v| NormalizedCoord::from_bits(v)).collect();
                        let settings = DrawSettings::unhinted(Size::new(g.size), LocationRef::new(&coords));
                        let mut pen = GlyphPen { c: &mut c, m, last: (0.0, 0.0) };
                        if outline.draw(settings, &mut pen).is_ok() {
                            if let Some(w) = g.bold {
                                g.paint.set_stroke(&mut c);
                                c.set_line_width(finite(w));
                                c.fill_nonzero_and_stroke();
                            } else {
                                c.fill_nonzero();
                            }
                        } else {
                            c.end_path();
                        }
                    }
                }
            }
        }
    }
    for _ in 0..depth {
        c.restore_state();
    }
    c.finish().to_vec()
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => o.push(c),
        }
    }
    o
}

/// The XMP packet mirroring the Info dictionary (Dublin Core, XMP basic, PDF schemas).
fn xmp_packet(m: &Metadata) -> String {
    let mut x = String::from("<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    x.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    x.push_str("<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n");
    x.push_str("<dc:format>application/pdf</dc:format>\n");
    if let Some(t) = &m.title {
        let _ = writeln!(x, "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>", xml_escape(t));
    }
    if let Some(a) = &m.author {
        let _ = writeln!(x, "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>", xml_escape(a));
    }
    if let Some(s) = &m.subject {
        let _ = writeln!(x, "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>", xml_escape(s));
    }
    if !m.keywords.is_empty() {
        let _ = writeln!(x, "<pdf:Keywords>{}</pdf:Keywords>", xml_escape(&m.keywords.join(", ")));
        x.push_str("<dc:subject><rdf:Bag>");
        for k in &m.keywords {
            let _ = write!(x, "<rdf:li>{}</rdf:li>", xml_escape(k));
        }
        x.push_str("</rdf:Bag></dc:subject>\n");
    }
    if let Some(c) = &m.creator {
        let _ = writeln!(x, "<xmp:CreatorTool>{}</xmp:CreatorTool>", xml_escape(c));
    }
    if let Some(p) = &m.producer {
        let _ = writeln!(x, "<pdf:Producer>{}</pdf:Producer>", xml_escape(p));
    }
    if let Some(d) = m.created {
        let _ = writeln!(x, "<xmp:CreateDate>{}</xmp:CreateDate>", d.iso());
    }
    if let Some(d) = m.modified {
        let _ = writeln!(x, "<xmp:ModifyDate>{}</xmp:ModifyDate>", d.iso());
    }
    x.push_str("</rdf:Description>\n</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>");
    x
}

#[cfg(test)]
mod tests;
