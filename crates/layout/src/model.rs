//! The page model: sizes, pages, cells and documents.

use serde::{Deserialize, Serialize};

use crate::{LayoutError, MAX_CELLS, MAX_PAGE_PT, MAX_PAGES, PT_PER_INCH, PT_PER_MM, Result};

/// A size in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Size {
        Size { w, h }
    }
    pub fn inches(w: f32, h: f32) -> Size {
        Size { w: w * PT_PER_INCH, h: h * PT_PER_INCH }
    }
    pub fn mm(w: f32, h: f32) -> Size {
        Size { w: w * PT_PER_MM, h: h * PT_PER_MM }
    }
    /// The same size turned 90°.
    pub fn landscape(self) -> Size {
        Size { w: self.w.max(self.h), h: self.w.min(self.h) }
    }
}

/// A rectangle in points (top-left origin, y down).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    /// Shrunk by `i` on each side (never negative).
    pub fn inset(&self, i: &Insets) -> Rect {
        Rect { x: self.x + i.left, y: self.y + i.top, w: (self.w - i.left - i.right).max(0.0), h: (self.h - i.top - i.bottom).max(0.0) }
    }
    fn is_valid(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite() && v.abs() <= MAX_PAGE_PT * 2.0) && self.w >= 0.0 && self.h >= 0.0
    }
}

/// Margins in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Insets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Insets {
    pub const fn all(v: f32) -> Insets {
        Insets { top: v, right: v, bottom: v, left: v }
    }
}

/// An sRGB colour with alpha.
pub type Color = [u8; 4];

pub const WHITE: Color = [255, 255, 255, 255];
pub const BLACK: Color = [0, 0, 0, 255];

/// Paper/page setup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// Trim size.
    pub size: Size,
    #[serde(default)]
    pub margins: Insets,
    /// Bleed beyond the trim on every side (books, full-bleed prints).
    #[serde(default)]
    pub bleed: f32,
    #[serde(default = "white")]
    pub background: Color,
}

fn white() -> Color {
    WHITE
}

impl Page {
    pub fn new(size: Size) -> Page {
        Page { size, margins: Insets::default(), bleed: 0.0, background: WHITE }
    }
    pub fn with_margins(mut self, m: Insets) -> Page {
        self.margins = m;
        self
    }
    /// The trim box.
    pub fn trim(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size.w, self.size.h)
    }
    /// The area inside the margins.
    pub fn content(&self) -> Rect {
        self.trim().inset(&self.margins)
    }
    /// The trim box grown by the bleed.
    pub fn bleed_box(&self) -> Rect {
        Rect::new(-self.bleed, -self.bleed, self.size.w + 2.0 * self.bleed, self.size.h + 2.0 * self.bleed)
    }
    pub fn validate(&self) -> Result<()> {
        let s = self.size;
        if !(s.w.is_finite() && s.h.is_finite() && s.w >= 1.0 && s.h >= 1.0 && s.w <= MAX_PAGE_PT && s.h <= MAX_PAGE_PT) {
            return Err(LayoutError::Invalid(format!("page size {}×{} pt is out of range", s.w, s.h)));
        }
        let m = self.margins;
        let ok = |v: f32| v.is_finite() && v >= 0.0;
        if ![m.top, m.right, m.bottom, m.left].into_iter().all(ok) || m.left + m.right >= s.w || m.top + m.bottom >= s.h {
            return Err(LayoutError::Invalid("margins must be non-negative and leave room on the page".into()));
        }
        if !ok(self.bleed) || self.bleed > 144.0 {
            return Err(LayoutError::Invalid("bleed must be between 0 and 2 inches".into()));
        }
        Ok(())
    }
}

/// How a photo meets its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// The whole photo, letterboxed.
    #[default]
    Fit,
    /// The cell filled, the photo cropped ("zoom to fill").
    Fill,
}

/// A stroke (border) around a cell or photo.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    /// Width in points.
    pub width: f32,
    pub color: Color,
}

/// A photo cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct PhotoCell {
    /// The photo (catalog id as text); `None` = an empty slot of a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
    #[serde(default)]
    pub fit: Fit,
    /// Extra magnification over `fit` (1 = none; clamped to 1..=20).
    #[serde(default = "one")]
    pub zoom: f32,
    /// Where the visible window sits when the photo overflows the cell: −1 (left/top) … 1
    /// (right/bottom), 0 = centred.
    #[serde(default)]
    pub pan: [f32; 2],
    /// Clockwise quarter turns of the photo in the cell (0..=3).
    #[serde(default)]
    pub rotate: u8,
    /// Turn the photo a quarter when that matches the cell's orientation better.
    #[serde(default)]
    pub rotate_to_fit: bool,
    /// Border drawn around the visible photo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
}

fn one() -> f32 {
    1.0
}

/// Horizontal text alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// How a text cell's text looks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    /// Font family name (resolved by the text renderer; `None` = the UI font).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Size in points.
    pub size: f32,
    #[serde(default = "black")]
    pub color: Color,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

fn black() -> Color {
    BLACK
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { family: None, size: 10.0, color: BLACK, align: Align::Left, bold: false, italic: false }
    }
}

/// A text cell: a token template expanded against the page's photo ([`crate::tokens`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct TextCell {
    pub text: String,
    #[serde(default)]
    pub style: TextStyle,
    /// The photo cell whose photo the tokens describe (index into the page's cells); `None` =
    /// the first photo cell on the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<usize>,
}

/// Graphic shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Rect,
    Ellipse,
}

/// A graphic cell (backgrounds, rules, mats, identity-plate boxes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct GraphicCell {
    #[serde(default)]
    pub shape: Shape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
}

/// What a cell holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CellKind {
    Photo(PhotoCell),
    Text(TextCell),
    Graphic(GraphicCell),
}

/// A cell on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Position on the page (trim coordinates; may reach into the bleed).
    pub rect: Rect,
    #[serde(flatten)]
    pub kind: CellKind,
}

impl Cell {
    pub fn photo(rect: Rect) -> Cell {
        Cell { rect, kind: CellKind::Photo(PhotoCell { zoom: 1.0, ..PhotoCell::default() }) }
    }
    pub fn text(rect: Rect, text: impl Into<String>, style: TextStyle) -> Cell {
        Cell { rect, kind: CellKind::Text(TextCell { text: text.into(), style, source: None }) }
    }
    pub fn graphic(rect: Rect, g: GraphicCell) -> Cell {
        Cell { rect, kind: CellKind::Graphic(g) }
    }
    pub fn as_photo(&self) -> Option<&PhotoCell> {
        match &self.kind {
            CellKind::Photo(p) => Some(p),
            _ => None,
        }
    }
    pub fn as_photo_mut(&mut self) -> Option<&mut PhotoCell> {
        match &mut self.kind {
            CellKind::Photo(p) => Some(p),
            _ => None,
        }
    }
}

/// A guide line placed by the user.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "axis", content = "at", rename_all = "snake_case")]
pub enum Guide {
    /// A vertical line at this x.
    Vertical(f32),
    /// A horizontal line at this y.
    Horizontal(f32),
}

/// The cells and guides of one page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct PageLayout {
    #[serde(default)]
    pub cells: Vec<Cell>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<Guide>,
}

impl PageLayout {
    pub fn photo_cells(&self) -> impl Iterator<Item = &PhotoCell> {
        self.cells.iter().filter_map(Cell::as_photo)
    }
    pub fn validate(&self) -> Result<()> {
        if self.cells.len() > MAX_CELLS {
            return Err(LayoutError::Invalid(format!("more than {MAX_CELLS} cells on a page")));
        }
        for (i, c) in self.cells.iter().enumerate() {
            if !c.rect.is_valid() {
                return Err(LayoutError::Invalid(format!("cell {i} has an invalid rectangle")));
            }
            match &c.kind {
                CellKind::Photo(p) => {
                    if !(p.zoom.is_finite() && p.pan.iter().all(|v| v.is_finite())) || p.rotate > 3 {
                        return Err(LayoutError::Invalid(format!("cell {i}: zoom, pan or rotate out of range")));
                    }
                    check_stroke(p.stroke, i)?;
                }
                CellKind::Text(t) => {
                    if !(t.style.size.is_finite() && t.style.size > 0.0 && t.style.size <= 1000.0) {
                        return Err(LayoutError::Invalid(format!("cell {i}: text size out of range")));
                    }
                    if t.text.len() > 64 * 1024 {
                        return Err(LayoutError::Invalid(format!("cell {i}: text too long")));
                    }
                }
                CellKind::Graphic(g) => check_stroke(g.stroke, i)?,
            }
        }
        if self.guides.len() > MAX_CELLS || self.guides.iter().any(|g| !matches!(g, Guide::Vertical(v) | Guide::Horizontal(v) if v.is_finite())) {
            return Err(LayoutError::Invalid("invalid guides".into()));
        }
        Ok(())
    }
}

fn check_stroke(s: Option<Stroke>, i: usize) -> Result<()> {
    match s {
        Some(s) if !(s.width.is_finite() && (0.0..=144.0).contains(&s.width)) => {
            Err(LayoutError::Invalid(format!("cell {i}: stroke width out of range")))
        }
        _ => Ok(()),
    }
}

/// What a document is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationKind {
    Print,
    Book,
    Slideshow,
    Web,
}

impl CreationKind {
    pub fn label(self) -> &'static str {
        match self {
            CreationKind::Print => "Print",
            CreationKind::Book => "Book",
            CreationKind::Slideshow => "Slideshow",
            CreationKind::Web => "Web Gallery",
        }
    }
}

/// The current layout document format.
pub const DOCUMENT_VERSION: u32 = 1;

/// A layout document: a page setup and its pages (a print job, a book, a slideshow, a web
/// gallery). Saved creations store this as JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    #[serde(default = "doc_version")]
    pub version: u32,
    pub kind: CreationKind,
    pub page: Page,
    #[serde(default)]
    pub pages: Vec<PageLayout>,
    /// The module's own settings for a creation that isn't a page layout (a saved slideshow's or
    /// web gallery's settings); its photos are the collection's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
}

fn doc_version() -> u32 {
    DOCUMENT_VERSION
}

impl Document {
    pub fn new(kind: CreationKind, page: Page) -> Document {
        Document { version: DOCUMENT_VERSION, kind, page, pages: Vec::new(), settings: None }
    }
    pub fn validate(&self) -> Result<()> {
        if self.version > DOCUMENT_VERSION {
            return Err(LayoutError::Invalid(format!("layout format {} is newer than this build reads ({DOCUMENT_VERSION})", self.version)));
        }
        self.page.validate()?;
        if self.pages.len() > MAX_PAGES {
            return Err(LayoutError::Invalid(format!("more than {MAX_PAGES} pages")));
        }
        self.pages.iter().try_for_each(PageLayout::validate)
    }
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| LayoutError::Json(e.to_string()))
    }
    /// Parses and validates.
    pub fn from_json(s: &str) -> Result<Document> {
        let d: Document = serde_json::from_str(s).map_err(|e| LayoutError::Json(e.to_string()))?;
        d.validate()?;
        Ok(d)
    }
    /// Every photo placed in the document, in page and cell order.
    pub fn photos(&self) -> Vec<&str> {
        self.pages.iter().flat_map(|p| p.photo_cells()).filter_map(|c| c.photo.as_deref()).collect()
    }
}
