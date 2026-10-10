//! The book document.

use serde::{Deserialize, Serialize};

use crate::{BookError, MAX_CELLS, MAX_PAGES, Result};
use dac_layout::{PT_PER_INCH, PT_PER_MM, Size};

/// The current book format.
pub const BOOK_VERSION: u32 = 1;

/// What the book is exported as (ordering printed books from a service is out of scope).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BookKind {
    #[default]
    Pdf,
    Jpeg,
}

/// Page sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "size")]
pub enum BookSize {
    /// 7 × 7 in.
    SmallSquare,
    /// 10 × 8 in.
    #[default]
    StandardLandscape,
    /// 8 × 10 in.
    StandardPortrait,
    /// 13 × 11 in.
    LargeLandscape,
    /// 8.5 × 11 in.
    Letter,
    /// 210 × 297 mm.
    A4,
    /// Any size, in inches (2–40 per side).
    Custom { w: f32, h: f32 },
}

impl BookSize {
    pub const PRESETS: [BookSize; 6] =
        [BookSize::SmallSquare, BookSize::StandardLandscape, BookSize::StandardPortrait, BookSize::LargeLandscape, BookSize::Letter, BookSize::A4];

    /// Trim size in points.
    pub fn points(self) -> Size {
        match self {
            BookSize::SmallSquare => Size::inches(7.0, 7.0),
            BookSize::StandardLandscape => Size::inches(10.0, 8.0),
            BookSize::StandardPortrait => Size::inches(8.0, 10.0),
            BookSize::LargeLandscape => Size::inches(13.0, 11.0),
            BookSize::Letter => Size::inches(8.5, 11.0),
            BookSize::A4 => Size::mm(210.0, 297.0),
            BookSize::Custom { w, h } => Size::inches(w, h),
        }
    }

    pub fn label(self) -> String {
        match self {
            BookSize::SmallSquare => "Small Square (7 × 7 in)".into(),
            BookSize::StandardLandscape => "Standard Landscape (10 × 8 in)".into(),
            BookSize::StandardPortrait => "Standard Portrait (8 × 10 in)".into(),
            BookSize::LargeLandscape => "Large Landscape (13 × 11 in)".into(),
            BookSize::Letter => "Letter (8.5 × 11 in)".into(),
            BookSize::A4 => "A4 (210 × 297 mm)".into(),
            BookSize::Custom { w, h } => format!("Custom ({w} × {h} in)"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            BookSize::SmallSquare => "smallSquare",
            BookSize::StandardLandscape => "standardLandscape",
            BookSize::StandardPortrait => "standardPortrait",
            BookSize::LargeLandscape => "largeLandscape",
            BookSize::Letter => "letter",
            BookSize::A4 => "a4",
            BookSize::Custom { .. } => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<BookSize> {
        BookSize::PRESETS.into_iter().find(|b| b.key().eq_ignore_ascii_case(s))
    }

    fn validate(self) -> Result<()> {
        if let BookSize::Custom { w, h } = self
            && !(w.is_finite() && h.is_finite() && (2.0..=40.0).contains(&w) && (2.0..=40.0).contains(&h))
        {
            return Err(BookError::Invalid("a custom size must be 2–40 inches per side".into()));
        }
        Ok(())
    }
}

/// The cover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoverType {
    #[default]
    Hardcover,
    Softcover,
    None,
}

/// Output sharpening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sharpening {
    Off,
    Low,
    #[default]
    Standard,
    High,
}

/// Export settings and the paper note.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BookSettings {
    pub kind: BookKind,
    pub size: BookSize,
    pub cover: CoverType,
    /// The paper the book will be printed on (a note for the printer).
    pub paper: String,
    /// 1–100.
    pub jpeg_quality: u8,
    /// Output colour profile (`sRGB` is the one written for now).
    pub color_profile: String,
    /// Pixels per inch of photos and JPEG pages (72–600).
    pub resolution: u32,
    pub sharpening: Sharpening,
    /// Bleed around every page, points (0–36).
    pub bleed: f32,
}

impl Default for BookSettings {
    fn default() -> Self {
        BookSettings {
            kind: BookKind::Pdf,
            size: BookSize::StandardLandscape,
            cover: CoverType::Hardcover,
            paper: String::new(),
            jpeg_quality: 90,
            color_profile: "sRGB".into(),
            resolution: 240,
            sharpening: Sharpening::Standard,
            bleed: 9.0,
        }
    }
}

/// A rectangle in normalized trim coordinates (0–1, top-left origin).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct NRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NRect {
    pub const FULL: NRect = NRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> NRect {
        NRect { x, y, w, h }
    }
    /// In points on a `size` page.
    pub fn to_points(self, size: Size) -> dac_layout::Rect {
        dac_layout::Rect::new(self.x * size.w, self.y * size.h, self.w * size.w, self.h * size.h)
    }
    fn is_valid(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite() && (-0.5..=1.5).contains(v)) && self.w > 0.0 && self.h > 0.0
    }
}

/// Cell padding in points; `linked` = one value for every side (the UI's link toggle).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Padding {
    pub linked: bool,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Padding {
    pub fn all(v: f32) -> Padding {
        Padding { linked: true, top: v, right: v, bottom: v, left: v }
    }
    pub fn insets(&self) -> dac_layout::Insets {
        dac_layout::Insets { top: self.top, right: self.right, bottom: self.bottom, left: self.left }
    }
    fn is_valid(&self) -> bool {
        [self.top, self.right, self.bottom, self.left].iter().all(|v| v.is_finite() && (0.0..=720.0).contains(v))
    }
}

/// Font weight/slant pick of the Type panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub const ALL: [FontStyle; 4] = [FontStyle::Regular, FontStyle::Bold, FontStyle::Italic, FontStyle::BoldItalic];
    pub fn bold(self) -> bool {
        matches!(self, FontStyle::Bold | FontStyle::BoldItalic)
    }
    pub fn italic(self) -> bool {
        matches!(self, FontStyle::Italic | FontStyle::BoldItalic)
    }
    pub fn label(self) -> &'static str {
        match self {
            FontStyle::Regular => "Regular",
            FontStyle::Bold => "Bold",
            FontStyle::Italic => "Italic",
            FontStyle::BoldItalic => "Bold Italic",
        }
    }
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Vertical alignment in the text box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// Automatic kerning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kerning {
    #[default]
    Metrics,
    Optical,
    Off,
}

/// The Type panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TypeStyle {
    /// Font family (empty = the UI font, Inter).
    pub font: String,
    pub style: FontStyle,
    /// Points (1–500).
    pub size: f32,
    /// 0–1.
    pub opacity: f32,
    /// sRGB.
    pub color: [u8; 3],
    /// Letter spacing, 1/1000 em (−500 … 2000).
    pub tracking: f32,
    /// Baseline shift, points (positive raises).
    pub baseline: f32,
    /// Line spacing, points (`None` = auto, 1.2 × size).
    pub leading: Option<f32>,
    pub kerning: Kerning,
    /// Text columns (1–6) and the gap between them (points).
    pub columns: u8,
    pub gutter: f32,
    pub align: HAlign,
    pub valign: VAlign,
}

impl Default for TypeStyle {
    fn default() -> Self {
        TypeStyle {
            font: String::new(),
            style: FontStyle::Regular,
            size: 11.0,
            opacity: 1.0,
            color: [40, 40, 40],
            tracking: 0.0,
            baseline: 0.0,
            leading: None,
            kerning: Kerning::Metrics,
            columns: 1,
            gutter: 12.0,
            align: HAlign::Left,
            valign: VAlign::Top,
        }
    }
}

impl TypeStyle {
    pub fn sized(size: f32) -> TypeStyle {
        TypeStyle { size, ..TypeStyle::default() }
    }
    pub fn validate(&self) -> Result<()> {
        let fin = |v: f32| v.is_finite();
        let ok = fin(self.size)
            && (1.0..=500.0).contains(&self.size)
            && fin(self.opacity)
            && (0.0..=1.0).contains(&self.opacity)
            && fin(self.tracking)
            && (-500.0..=2000.0).contains(&self.tracking)
            && fin(self.baseline)
            && self.baseline.abs() <= 500.0
            && self.leading.is_none_or(|l| fin(l) && (0.5..=1000.0).contains(&l))
            && (1..=6).contains(&self.columns)
            && fin(self.gutter)
            && (0.0..=200.0).contains(&self.gutter)
            && self.font.len() <= 256;
        if ok { Ok(()) } else { Err(BookError::Invalid("type style out of range".into())) }
    }
}

/// Where photo text sits relative to its photo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PhotoTextPos {
    Above,
    #[default]
    Below,
    /// Over the photo, at its bottom.
    Over,
}

/// A photo's caption: token text (`{Title}`, `{Caption}`, `{Filename}`…) or custom text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PhotoText {
    pub text: String,
    pub position: PhotoTextPos,
    /// Distance from the photo, points.
    pub offset: f32,
    pub style: TypeStyle,
}

impl Default for PhotoText {
    fn default() -> Self {
        PhotoText { text: "{Title}".into(), position: PhotoTextPos::Below, offset: 6.0, style: TypeStyle::sized(9.0) }
    }
}

/// Where page text sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageTextPos {
    Top,
    #[default]
    Bottom,
}

/// Text across the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PageText {
    pub text: String,
    pub position: PageTextPos,
    /// From the page edge, points.
    pub offset: f32,
    pub style: TypeStyle,
}

impl Default for PageText {
    fn default() -> Self {
        PageText {
            text: String::new(),
            position: PageTextPos::Bottom,
            offset: 36.0,
            style: TypeStyle { align: HAlign::Center, ..TypeStyle::sized(12.0) },
        }
    }
}

/// What a cell holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CellContent {
    #[serde(rename_all = "camelCase")]
    Photo {
        /// Catalog id as text; `None` = an empty slot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        photo: Option<String>,
        /// Zoom to fill the cell (true) or fit the whole photo (false).
        #[serde(default = "yes")]
        fill: bool,
        /// Extra magnification (1–10).
        #[serde(default = "one")]
        zoom: f32,
        /// −1 … 1 per axis, 0 = centred.
        #[serde(default)]
        pan: [f32; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<PhotoText>,
    },
    #[serde(rename_all = "camelCase")]
    Text {
        #[serde(default)]
        text: String,
        #[serde(default)]
        style: TypeStyle,
    },
}

fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}

/// A cell on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookCell {
    pub rect: NRect,
    #[serde(default)]
    pub padding: Padding,
    #[serde(flatten)]
    pub content: CellContent,
}

impl BookCell {
    pub fn photo(rect: NRect) -> BookCell {
        BookCell {
            rect,
            padding: Padding::default(),
            content: CellContent::Photo { photo: None, fill: true, zoom: 1.0, pan: [0.0, 0.0], text: None },
        }
    }
    pub fn text(rect: NRect, style: TypeStyle) -> BookCell {
        BookCell { rect, padding: Padding::default(), content: CellContent::Text { text: String::new(), style } }
    }
    pub fn is_photo(&self) -> bool {
        matches!(self.content, CellContent::Photo { .. })
    }
    pub fn photo_id(&self) -> Option<&str> {
        match &self.content {
            CellContent::Photo { photo, .. } => photo.as_deref(),
            CellContent::Text { .. } => None,
        }
    }
    fn validate(&self, i: usize) -> Result<()> {
        if !self.rect.is_valid() || !self.padding.is_valid() {
            return Err(BookError::Invalid(format!("cell {i}: rectangle or padding out of range")));
        }
        match &self.content {
            CellContent::Photo { zoom, pan, text, photo, .. } => {
                if !(zoom.is_finite() && (1.0..=10.0).contains(zoom) && pan.iter().all(|v| v.is_finite() && v.abs() <= 1.0)) {
                    return Err(BookError::Invalid(format!("cell {i}: zoom or pan out of range")));
                }
                if photo.as_ref().is_some_and(|p| p.len() > 256) {
                    return Err(BookError::Invalid(format!("cell {i}: photo id too long")));
                }
                if let Some(t) = text {
                    t.style.validate()?;
                    if !(t.offset.is_finite() && t.offset.abs() <= 720.0) || t.text.len() > 4096 {
                        return Err(BookError::Invalid(format!("cell {i}: photo text out of range")));
                    }
                }
            }
            CellContent::Text { text, style } => {
                style.validate()?;
                if text.len() > 64 * 1024 {
                    return Err(BookError::Invalid(format!("cell {i}: text too long")));
                }
            }
        }
        Ok(())
    }
}

/// A decorative graphic drawn behind the page content (our own vector designs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Graphic {
    #[default]
    None,
    /// A thin frame inset from the trim.
    Frame,
    /// L-shaped corner marks.
    Corners,
    /// A band across the bottom third.
    Band,
    /// Fine horizontal lines.
    Lines,
}

impl Graphic {
    pub const ALL: [Graphic; 5] = [Graphic::None, Graphic::Frame, Graphic::Corners, Graphic::Band, Graphic::Lines];
    pub fn label(self) -> &'static str {
        match self {
            Graphic::None => "None",
            Graphic::Frame => "Frame",
            Graphic::Corners => "Corners",
            Graphic::Band => "Band",
            Graphic::Lines => "Lines",
        }
    }
}

/// A page background: colour, then a photo, then a graphic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Background {
    pub color: [u8; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
    /// 0–1.
    pub photo_opacity: f32,
    pub graphic: Graphic,
    pub graphic_color: [u8; 3],
    /// 0–1.
    pub graphic_opacity: f32,
}

impl Default for Background {
    fn default() -> Self {
        Background {
            color: [255, 255, 255],
            photo: None,
            photo_opacity: 0.3,
            graphic: Graphic::None,
            graphic_color: [120, 120, 120],
            graphic_opacity: 0.5,
        }
    }
}

impl Background {
    fn validate(&self) -> Result<()> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if unit(self.photo_opacity) && unit(self.graphic_opacity) && self.photo.as_ref().is_none_or(|p| p.len() <= 256) {
            Ok(())
        } else {
            Err(BookError::Invalid("background opacity out of range".into()))
        }
    }
}

/// Page number placement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NumberPos {
    TopCorner,
    TopCenter,
    #[default]
    BottomCorner,
    BottomCenter,
    /// On the outer edge, vertically centred.
    Side,
}

impl NumberPos {
    pub const ALL: [NumberPos; 5] = [NumberPos::TopCorner, NumberPos::TopCenter, NumberPos::BottomCorner, NumberPos::BottomCenter, NumberPos::Side];
    pub fn label(self) -> &'static str {
        match self {
            NumberPos::TopCorner => "Top Corner",
            NumberPos::TopCenter => "Top Center",
            NumberPos::BottomCorner => "Bottom Corner",
            NumberPos::BottomCenter => "Bottom Center",
            NumberPos::Side => "Side",
        }
    }
}

/// Page numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PageNumbers {
    pub show: bool,
    pub position: NumberPos,
    /// Distance from the trim edge, points.
    pub offset: f32,
    pub style: TypeStyle,
}

impl Default for PageNumbers {
    fn default() -> Self {
        PageNumbers { show: false, position: NumberPos::BottomCorner, offset: 24.0, style: TypeStyle::sized(9.0) }
    }
}

/// Guides drawn in the editor (never exported).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Guides {
    pub show: bool,
    pub bleed: bool,
    pub text_safe: bool,
    pub photo_cells: bool,
    /// Placeholder text in empty text cells.
    pub filler_text: bool,
}

impl Default for Guides {
    fn default() -> Self {
        Guides { show: true, bleed: true, text_safe: true, photo_cells: true, filler_text: true }
    }
}

/// Distance of the text-safe area from the trim, points (a quarter inch).
pub const TEXT_SAFE: f32 = 18.0;

/// One page (or a cover).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookPage {
    /// The template it was made from.
    pub template: String,
    pub cells: Vec<BookCell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<PageText>,
    /// Overrides the book background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    /// Hide this page's number.
    pub hide_number: bool,
}

impl BookPage {
    pub fn photo_slots(&self) -> usize {
        self.cells.iter().filter(|c| c.is_photo()).count()
    }
    pub fn photos(&self) -> impl Iterator<Item = &str> {
        self.cells.iter().filter_map(BookCell::photo_id)
    }
    fn validate(&self) -> Result<()> {
        if self.cells.len() > MAX_CELLS {
            return Err(BookError::Invalid(format!("more than {MAX_CELLS} cells on a page")));
        }
        for (i, c) in self.cells.iter().enumerate() {
            c.validate(i)?;
        }
        if let Some(t) = &self.text {
            t.style.validate()?;
            if !(t.offset.is_finite() && (0.0..=720.0).contains(&t.offset)) || t.text.len() > 64 * 1024 {
                return Err(BookError::Invalid("page text out of range".into()));
            }
        }
        if let Some(b) = &self.background {
            b.validate()?;
        }
        Ok(())
    }
}

/// A named text style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextPreset {
    pub name: String,
    pub style: TypeStyle,
}

/// Which page: the front cover, the back cover or a numbered page (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageRef {
    Front,
    Back,
    Page(usize),
}

impl PageRef {
    /// `"front"`, `"back"` or a 1-based page number.
    pub fn parse(v: &serde_json::Value) -> Option<PageRef> {
        match v {
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("front") || s.eq_ignore_ascii_case("cover") => Some(PageRef::Front),
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("back") => Some(PageRef::Back),
            serde_json::Value::String(s) => s.parse::<usize>().ok().filter(|n| *n >= 1).map(|n| PageRef::Page(n - 1)),
            v => v.as_u64().filter(|n| *n >= 1).and_then(|n| usize::try_from(n - 1).ok()).map(PageRef::Page),
        }
    }
    pub fn label(self) -> String {
        match self {
            PageRef::Front => "Front Cover".into(),
            PageRef::Back => "Back Cover".into(),
            PageRef::Page(i) => format!("Page {}", i + 1),
        }
    }
}

/// A photo book.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Book {
    pub version: u32,
    pub name: String,
    pub settings: BookSettings,
    pub front: BookPage,
    pub back: BookPage,
    pub pages: Vec<BookPage>,
    pub background: Background,
    pub numbers: PageNumbers,
    pub guides: Guides,
    /// Favourite template ids.
    pub favorites: Vec<String>,
    /// User text style presets (built-ins are in [`crate::text::builtin_presets`]).
    pub text_presets: Vec<TextPreset>,
}

impl Default for Book {
    fn default() -> Self {
        Book {
            version: BOOK_VERSION,
            name: "Untitled Book".into(),
            settings: BookSettings::default(),
            front: crate::templates::page("cover-photo-title").unwrap_or_default(),
            back: crate::templates::page("blank").unwrap_or_default(),
            pages: Vec::new(),
            background: Background::default(),
            numbers: PageNumbers::default(),
            guides: Guides::default(),
            favorites: Vec::new(),
            text_presets: Vec::new(),
        }
    }
}

impl Book {
    /// Trim size in points.
    pub fn trim(&self) -> Size {
        self.settings.size.points()
    }

    pub fn has_cover(&self) -> bool {
        self.settings.cover != CoverType::None
    }

    pub fn page(&self, r: PageRef) -> Option<&BookPage> {
        match r {
            PageRef::Front => self.has_cover().then_some(&self.front),
            PageRef::Back => self.has_cover().then_some(&self.back),
            PageRef::Page(i) => self.pages.get(i),
        }
    }

    pub fn page_mut(&mut self, r: PageRef) -> Option<&mut BookPage> {
        match r {
            PageRef::Front => self.has_cover().then_some(&mut self.front),
            PageRef::Back => self.has_cover().then_some(&mut self.back),
            PageRef::Page(i) => self.pages.get_mut(i),
        }
    }

    /// Every page in book order: front cover, pages, back cover.
    pub fn all_pages(&self) -> Vec<PageRef> {
        let mut v = Vec::with_capacity(self.pages.len() + 2);
        if self.has_cover() {
            v.push(PageRef::Front);
        }
        v.extend((0..self.pages.len()).map(PageRef::Page));
        if self.has_cover() {
            v.push(PageRef::Back);
        }
        v
    }

    /// Every photo placed, in book order (backgrounds excluded).
    pub fn photos(&self) -> Vec<&str> {
        self.all_pages().into_iter().filter_map(|r| self.page(r)).flat_map(BookPage::photos).collect()
    }

    /// The background a page shows.
    pub fn background_of(&self, r: PageRef) -> &Background {
        self.page(r).and_then(|p| p.background.as_ref()).unwrap_or(&self.background)
    }

    /// Spreads as shown in the spread view: page 1 alone on the right, then 2–3, 4–5, …
    /// (`None` = the empty side).
    pub fn spreads(&self) -> Vec<[Option<usize>; 2]> {
        let n = self.pages.len();
        let mut out = Vec::new();
        if n == 0 {
            return out;
        }
        out.push([None, Some(0)]);
        let mut i = 1;
        while i < n {
            out.push([Some(i), (i + 1 < n).then_some(i + 1)]);
            i += 2;
        }
        out
    }

    pub fn validate(&self) -> Result<()> {
        if self.version > BOOK_VERSION {
            return Err(BookError::Invalid(format!("book format {} is newer than this build reads ({BOOK_VERSION})", self.version)));
        }
        self.settings.size.validate()?;
        let s = &self.settings;
        if !((1..=100).contains(&s.jpeg_quality) && (72..=600).contains(&s.resolution) && s.bleed.is_finite() && (0.0..=36.0).contains(&s.bleed)) {
            return Err(BookError::Invalid("JPEG quality 1–100, resolution 72–600 ppi and bleed 0–36 pt".into()));
        }
        if s.paper.len() > 1024 || self.name.len() > 1024 || s.color_profile.len() > 256 {
            return Err(BookError::Invalid("name, paper or profile too long".into()));
        }
        if self.pages.len() > MAX_PAGES {
            return Err(BookError::Invalid(format!("more than {MAX_PAGES} pages")));
        }
        self.front.validate()?;
        self.back.validate()?;
        self.pages.iter().try_for_each(BookPage::validate)?;
        self.background.validate()?;
        self.numbers.style.validate()?;
        if !(self.numbers.offset.is_finite() && (0.0..=720.0).contains(&self.numbers.offset)) {
            return Err(BookError::Invalid("page number offset out of range".into()));
        }
        if self.favorites.len() > 1000 || self.text_presets.len() > 1000 {
            return Err(BookError::Invalid("too many favourites or presets".into()));
        }
        self.text_presets.iter().try_for_each(|p| p.style.validate())
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| BookError::Json(e.to_string()))
    }

    /// Parses and validates.
    pub fn from_json(s: &str) -> Result<Book> {
        let b: Book = serde_json::from_str(s).map_err(|e| BookError::Json(e.to_string()))?;
        b.validate()?;
        Ok(b)
    }
}

/// Points per inch and per millimetre (re-exported for the UI's unit fields).
pub const PT_IN: f32 = PT_PER_INCH;
pub const PT_MM: f32 = PT_PER_MM;
