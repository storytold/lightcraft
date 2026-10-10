//! Print job settings and how they become a [`Document`] of pages.
//!
//! The three layout styles of a print module:
//! * **Single Image / Contact Sheet**: a [`Grid`] of equal cells, photos in order, pages repeated
//!   until every photo is placed (or one photo repeated in every cell, one page per photo);
//! * **Picture Package**: fixed cell sizes (e.g. one 5×7 and four 2.5×3.5 in) laid out
//!   automatically, the same photo in every cell, one package per photo;
//! * **Custom Package**: free cells placed by the user on one or more pages; photos fill the empty
//!   cells in order, and cells that already name a photo keep it.
//!
//! Page options (photo info, page numbers, page info, identity plate) become text cells on each
//! page, so the preview, JPEG and PDF all draw them through the same path.

use dac_layout::grid::Grid;
use dac_layout::{Align, Cell, CellKind, Color, CreationKind, Document, Fit, Insets, Page, PageLayout, Rect, Size, Stroke, TextCell, TextStyle};
use serde::{Deserialize, Serialize};

use crate::{PrintError, Result};

/// Most photos in one job.
pub const MAX_PHOTOS: usize = 5_000;

/// A paper size preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paper {
    pub key: &'static str,
    pub label: &'static str,
    pub size: Size,
}

const fn inch(w: f32, h: f32) -> Size {
    Size::new(w * 72.0, h * 72.0)
}
const fn mm(w: f32, h: f32) -> Size {
    Size::new(w * 72.0 / 25.4, h * 72.0 / 25.4)
}

/// Paper sizes offered by Page Setup (portrait; Page Setup turns them).
pub const PAPERS: &[Paper] = &[
    Paper { key: "letter", label: "US Letter (8.5 × 11 in)", size: inch(8.5, 11.0) },
    Paper { key: "legal", label: "US Legal (8.5 × 14 in)", size: inch(8.5, 14.0) },
    Paper { key: "tabloid", label: "Tabloid (11 × 17 in)", size: inch(11.0, 17.0) },
    Paper { key: "super-b", label: "Super B (13 × 19 in)", size: inch(13.0, 19.0) },
    Paper { key: "4x6", label: "4 × 6 in", size: inch(4.0, 6.0) },
    Paper { key: "5x7", label: "5 × 7 in", size: inch(5.0, 7.0) },
    Paper { key: "8x10", label: "8 × 10 in", size: inch(8.0, 10.0) },
    Paper { key: "a5", label: "A5 (148 × 210 mm)", size: mm(148.0, 210.0) },
    Paper { key: "a4", label: "A4 (210 × 297 mm)", size: mm(210.0, 297.0) },
    Paper { key: "a3", label: "A3 (297 × 420 mm)", size: mm(297.0, 420.0) },
    Paper { key: "a3+", label: "A3+ (329 × 483 mm)", size: mm(329.0, 483.0) },
];

/// The paper preset by key.
pub fn paper(key: &str) -> Option<Paper> {
    PAPERS.iter().copied().find(|p| p.key.eq_ignore_ascii_case(key))
}

/// Picture-package cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageSpec {
    /// Cell sizes in points.
    pub cells: Vec<Size>,
    /// Gap between cells (points).
    #[serde(default)]
    pub gap: f32,
}

/// The layout style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "style", rename_all = "camelCase")]
pub enum LayoutStyle {
    /// Single Image / Contact Sheet.
    SingleImage {
        grid: Grid,
    },
    PicturePackage(PackageSpec),
    /// Custom Package: the pages' cells as placed.
    CustomPackage {
        pages: Vec<PageLayout>,
    },
}

impl LayoutStyle {
    pub fn label(&self) -> &'static str {
        match self {
            LayoutStyle::SingleImage { .. } => "Single Image / Contact Sheet",
            LayoutStyle::PicturePackage(_) => "Picture Package",
            LayoutStyle::CustomPackage { .. } => "Custom Package",
        }
    }
}

/// Image settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageSettings {
    /// Fill each cell, cropping the photo.
    pub zoom_to_fill: bool,
    /// Turn photos a quarter when that matches the cell better.
    pub rotate_to_fit: bool,
    /// The same photo in every cell of a page (one page per photo).
    pub repeat_one: bool,
    /// Border around each photo.
    pub stroke: Option<Stroke>,
}

/// The identity plate: a line of text on the page or on every image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdentityPlate {
    pub text: String,
    pub size: f32,
    pub color: Color,
    /// 0–1.
    pub opacity: f32,
    /// On every photo cell instead of once per page.
    pub every_image: bool,
}

impl Default for IdentityPlate {
    fn default() -> Self {
        IdentityPlate { text: String::new(), size: 18.0, color: [40, 40, 40, 255], opacity: 1.0, every_image: false }
    }
}

/// Page options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PageOptions {
    pub background: Color,
    pub identity_plate: Option<IdentityPlate>,
    /// "Page 2 of 5" at the bottom right.
    pub page_numbers: bool,
    /// The job's settings (paper, resolution, profile) at the bottom left.
    pub page_info: bool,
    /// Crop (trim) marks around the page (PDF output).
    pub crop_marks: bool,
    /// Token template drawn under each photo (`{Filename}`, `{Title}`…); `None` = off.
    pub photo_info: Option<String>,
    /// Font size of photo info, page numbers and page info (points).
    pub font_size: f32,
}

impl Default for PageOptions {
    fn default() -> Self {
        PageOptions {
            background: dac_layout::WHITE,
            identity_plate: None,
            page_numbers: false,
            page_info: false,
            crop_marks: false,
            photo_info: None,
            font_size: 8.0,
        }
    }
}

/// Where the job goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Destination {
    #[default]
    Pdf,
    Jpeg,
    Printer,
}

/// The print job.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JobOptions {
    /// Pixels per inch of photos (PDF) and pages (JPEG).
    pub dpi: f32,
    /// Draft mode: previews instead of full renders (the caller's photo source decides).
    pub draft: bool,
    /// JPEG quality 1–100.
    pub jpeg_quality: u8,
}

impl Default for JobOptions {
    fn default() -> Self {
        JobOptions { dpi: 240.0, draft: false, jpeg_quality: 92 }
    }
}

/// Everything a print needs except the photos.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PrintSettings {
    pub page: Page,
    pub layout: LayoutStyle,
    pub image: ImageSettings,
    pub options: PageOptions,
    pub job: JobOptions,
    pub destination: Destination,
}

impl Default for PrintSettings {
    fn default() -> Self {
        PrintSettings {
            page: Page::new(inch(8.5, 11.0)).with_margins(Insets::all(36.0)),
            layout: LayoutStyle::SingleImage { grid: Grid::new(1, 1) },
            image: ImageSettings::default(),
            options: PageOptions::default(),
            job: JobOptions::default(),
            destination: Destination::Pdf,
        }
    }
}

impl PrintSettings {
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| PrintError::Invalid(e.to_string()))
    }

    pub fn from_json(s: &str) -> Result<PrintSettings> {
        let p: PrintSettings = serde_json::from_str(s).map_err(|e| PrintError::Invalid(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        self.page.validate()?;
        let j = &self.job;
        if !(j.dpi.is_finite() && (36.0..=1200.0).contains(&j.dpi)) {
            return Err(PrintError::Invalid("print resolution must be 36–1200 ppi".into()));
        }
        let fs = self.options.font_size;
        if !(fs.is_finite() && (2.0..=72.0).contains(&fs)) {
            return Err(PrintError::Invalid("font size must be 2–72 pt".into()));
        }
        if let Some(p) = &self.options.identity_plate
            && !(p.size.is_finite() && (2.0..=400.0).contains(&p.size) && p.opacity.is_finite())
        {
            return Err(PrintError::Invalid("identity plate size must be 2–400 pt".into()));
        }
        Ok(())
    }

    /// Page Setup: `paper` turned to `landscape` or portrait, margins kept.
    pub fn set_paper(&mut self, paper: Size, landscape: bool) {
        let (a, b) = (paper.w.min(paper.h), paper.w.max(paper.h));
        self.page.size = if landscape { Size::new(b, a) } else { Size::new(a, b) };
    }

    /// The one-line summary used by Page Info.
    pub fn info_line(&self) -> String {
        let s = self.page.size;
        let paper = PAPERS
            .iter()
            .find(|p| (p.size.w - s.w.min(s.h)).abs() < 0.5 && (p.size.h - s.w.max(s.h)).abs() < 0.5)
            .map(|p| p.label.to_string())
            .unwrap_or_else(|| format!("{:.2} × {:.2} in", s.w / 72.0, s.h / 72.0));
        format!("{} · {} · {} ppi{}", paper, self.layout.label(), self.job.dpi.round(), if self.job.draft { " · draft" } else { "" })
    }

    /// The pages for `photos` (catalog ids as text).
    pub fn build(&self, photos: &[String]) -> Result<Document> {
        self.validate()?;
        if photos.len() > MAX_PHOTOS {
            return Err(PrintError::Invalid(format!("at most {MAX_PHOTOS} photos per print job")));
        }
        let fit = if self.image.zoom_to_fill { Fit::Fill } else { Fit::Fit };
        let mut pages: Vec<PageLayout> = Vec::new();
        match &self.layout {
            LayoutStyle::SingleImage { grid } => {
                let mut grid = grid.clone();
                if self.options.photo_info.is_some() {
                    grid.caption_height = grid.caption_height.max(self.options.font_size * 1.8);
                    grid.caption = None;
                }
                let base = grid.layout(&self.page, fit)?;
                pages = fill(&[base], photos, self.image.repeat_one);
            }
            LayoutStyle::PicturePackage(spec) => {
                let package = dac_layout::package::package_pages(&self.page, &spec.cells, spec.gap)?;
                if photos.is_empty() {
                    pages = package;
                } else {
                    for photo in photos {
                        for p in &package {
                            let mut p = p.clone();
                            p.cells.iter_mut().filter_map(Cell::as_photo_mut).for_each(|c| c.photo = Some(photo.clone()));
                            pages.push(p);
                            if pages.len() > dac_layout::MAX_PAGES {
                                return Err(PrintError::Invalid("too many pages".into()));
                            }
                        }
                    }
                }
            }
            LayoutStyle::CustomPackage { pages: custom } => {
                if custom.is_empty() {
                    return Err(PrintError::Invalid("a custom package needs a page".into()));
                }
                pages = fill(custom, photos, self.image.repeat_one);
            }
        }
        if pages.len() > dac_layout::MAX_PAGES {
            return Err(PrintError::Invalid("too many pages".into()));
        }
        for p in &mut pages {
            for c in p.cells.iter_mut().filter_map(Cell::as_photo_mut) {
                if !matches!(self.layout, LayoutStyle::CustomPackage { .. }) {
                    c.fit = fit;
                }
                c.rotate_to_fit |= self.image.rotate_to_fit;
                if self.image.stroke.is_some() {
                    c.stroke = self.image.stroke;
                }
            }
            self.decorate(p);
        }
        let mut page = self.page.clone();
        page.background = self.options.background;
        let doc = Document { pages, ..Document::new(CreationKind::Print, page) };
        doc.validate()?;
        Ok(doc)
    }

    /// Page options as text cells.
    fn decorate(&self, p: &mut PageLayout) {
        let o = &self.options;
        let style = TextStyle { size: o.font_size, color: [60, 60, 60, 255], ..TextStyle::default() };
        let line = o.font_size * 1.6;
        let photo_cells: Vec<(usize, Rect)> =
            p.cells.iter().enumerate().filter(|(_, c)| matches!(c.kind, CellKind::Photo(_))).map(|(i, c)| (i, c.rect)).collect();
        if let Some(t) = o.photo_info.as_deref().filter(|t| !t.trim().is_empty()) {
            for (i, r) in &photo_cells {
                p.cells.push(Cell {
                    rect: Rect::new(r.x, r.bottom() + o.font_size * 0.2, r.w, line),
                    kind: CellKind::Text(TextCell { text: t.into(), style: TextStyle { align: Align::Center, ..style.clone() }, source: Some(*i) }),
                });
            }
        }
        let content = self.page.content();
        let m = self.page.margins;
        // footer: inside the bottom margin when there is room, else just inside the content
        let fy = if m.bottom >= line { self.page.size.h - m.bottom + (m.bottom - line) / 2.0 } else { content.bottom() - line };
        if o.page_info {
            let info = format!("{} · page {{Page}}", self.info_line());
            p.cells.push(Cell::text(Rect::new(content.x, fy, content.w, line), info, style.clone()));
        }
        if o.page_numbers {
            p.cells.push(Cell::text(
                Rect::new(content.x, fy, content.w, line),
                "Page {Page} of {Pages}",
                TextStyle { align: Align::Right, ..style.clone() },
            ));
        }
        if let Some(plate) = o.identity_plate.as_ref().filter(|p| !p.text.trim().is_empty()) {
            let a = (f32::from(plate.color[3]) * plate.opacity.clamp(0.0, 1.0)).round() as u8;
            let ps = TextStyle {
                size: plate.size,
                color: [plate.color[0], plate.color[1], plate.color[2], a],
                align: Align::Center,
                ..TextStyle::default()
            };
            let h = plate.size * 1.5;
            if plate.every_image {
                for (_, r) in &photo_cells {
                    p.cells.push(Cell::text(Rect::new(r.x, r.bottom() - h - r.h * 0.04, r.w, h), plate.text.clone(), ps.clone()));
                }
            } else {
                let y = if m.top >= h { (m.top - h) / 2.0 } else { content.y };
                p.cells.push(Cell::text(Rect::new(content.x, y, content.w, h), plate.text.clone(), ps));
            }
        }
    }
}

/// `templates` filled with `photos`: empty photo cells take the photos in order and the pages
/// repeat until all are placed; with `repeat_one`, each page holds one photo in every cell.
fn fill(templates: &[PageLayout], photos: &[String], repeat_one: bool) -> Vec<PageLayout> {
    let slots: usize = templates.iter().map(|p| p.cells.iter().filter(|c| c.as_photo().is_some_and(|p| p.photo.is_none())).count()).sum();
    if photos.is_empty() || slots == 0 {
        return templates.to_vec();
    }
    let mut out = Vec::new();
    if repeat_one {
        for photo in photos {
            for t in templates {
                let mut p = t.clone();
                p.cells.iter_mut().filter_map(Cell::as_photo_mut).filter(|c| c.photo.is_none()).for_each(|c| c.photo = Some(photo.clone()));
                out.push(p);
            }
        }
        return out;
    }
    let mut next = photos.iter();
    let mut left = photos.len();
    while left > 0 && out.len() <= dac_layout::MAX_PAGES {
        for t in templates {
            if left == 0 {
                break;
            }
            let mut p = t.clone();
            for c in p.cells.iter_mut().filter_map(Cell::as_photo_mut).filter(|c| c.photo.is_none()) {
                if let Some(ph) = next.next() {
                    c.photo = Some(ph.clone());
                    left -= 1;
                }
            }
            out.push(p);
        }
    }
    out
}

/// Built-in print templates (own designs): (name, settings).
pub fn builtin_templates() -> Vec<(String, PrintSettings)> {
    let letter = PrintSettings::default();
    let with = |name: &str, f: &dyn Fn(&mut PrintSettings)| {
        let mut s = letter.clone();
        f(&mut s);
        (name.to_string(), s)
    };
    let grid = |rows, cols, gutter| LayoutStyle::SingleImage { grid: Grid::new(rows, cols).with_gutter(gutter) };
    vec![
        with("1 Large with Stroke", &|s| {
            s.image.stroke = Some(Stroke { width: 1.0, color: dac_layout::BLACK });
        }),
        with("Fine Art Mat", &|s| {
            s.page.margins = Insets { top: 90.0, right: 90.0, bottom: 150.0, left: 90.0 };
            s.options.photo_info = Some("{Title}".into());
            s.options.font_size = 11.0;
        }),
        with("Maximize Size", &|s| {
            s.page.margins = Insets::all(18.0);
            s.image.rotate_to_fit = true;
        }),
        with("2×2 Cells", &|s| {
            s.layout = grid(2, 2, 18.0);
            s.image.zoom_to_fill = true;
        }),
        with("4 Wide", &|s| {
            s.set_paper(inch(8.5, 11.0), true);
            s.layout = grid(1, 4, 9.0);
            s.image.zoom_to_fill = true;
        }),
        with("4×5 Contact Sheet", &|s| {
            s.layout = grid(5, 4, 9.0);
            s.options.photo_info = Some("{Filename}".into());
            s.options.page_numbers = true;
        }),
        with("5×8 Contact Sheet", &|s| {
            s.layout = grid(8, 5, 6.0);
            s.options.photo_info = Some("{Filename}".into());
            s.options.font_size = 6.0;
            s.options.page_numbers = true;
        }),
        with("Triptych", &|s| {
            s.set_paper(inch(8.5, 11.0), true);
            s.layout = grid(1, 3, 12.0);
            s.image.zoom_to_fill = true;
        }),
        with("(1) 4×6, (6) 2×3", &|s| {
            s.layout = LayoutStyle::PicturePackage(PackageSpec {
                cells: package_cells(&[(4.0, 6.0), (2.0, 3.0), (2.0, 3.0), (2.0, 3.0), (2.0, 3.0), (2.0, 3.0), (2.0, 3.0)]),
                gap: 9.0,
            });
            s.image.zoom_to_fill = true;
            s.options.crop_marks = true;
        }),
        with("(2) 5×7", &|s| {
            s.layout = LayoutStyle::PicturePackage(PackageSpec { cells: package_cells(&[(7.0, 5.0), (7.0, 5.0)]), gap: 9.0 });
            s.image.zoom_to_fill = true;
        }),
        with("Custom Centered", &|s| {
            let c = s.page.content();
            let big = Rect::new(c.x + c.w * 0.1, c.y + c.h * 0.05, c.w * 0.8, c.h * 0.55);
            let small = |i: f32| Rect::new(c.x + c.w * (0.1 + 0.275 * i), c.y + c.h * 0.65, c.w * 0.25, c.h * 0.25);
            s.layout = LayoutStyle::CustomPackage {
                pages: vec![PageLayout {
                    cells: vec![Cell::photo(big), Cell::photo(small(0.0)), Cell::photo(small(1.0)), Cell::photo(small(2.0))],
                    guides: Vec::new(),
                }],
            };
        }),
    ]
}

fn package_cells(inches: &[(f32, f32)]) -> Vec<Size> {
    inches.iter().map(|&(w, h)| inch(w, h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("p{i}")).collect()
    }

    #[test]
    fn contact_sheet_paginates_and_adds_info() {
        let (_, s) = builtin_templates().into_iter().find(|(n, _)| n == "4×5 Contact Sheet").unwrap();
        let doc = s.build(&ids(45)).unwrap();
        assert_eq!(doc.pages.len(), 3);
        assert_eq!(doc.photos().len(), 45);
        // 20 photos + 20 captions + page numbers
        assert_eq!(doc.pages[0].cells.len(), 41);
        assert_eq!(doc.pages[2].photo_cells().filter(|c| c.photo.is_some()).count(), 5);
    }

    #[test]
    fn package_repeats_one_photo_per_package() {
        let (_, s) = builtin_templates().into_iter().find(|(n, _)| n.starts_with("(1) 4×6")).unwrap();
        let doc = s.build(&ids(2)).unwrap();
        assert!(doc.pages.len() >= 2);
        assert!(doc.pages[0].photo_cells().all(|c| c.photo.as_deref() == Some("p0")));
        assert_eq!(
            doc.pages[0].photo_cells().count()
                + doc.pages.get(1).filter(|p| p.photo_cells().all(|c| c.photo.as_deref() == Some("p0"))).map_or(0, |p| p.photo_cells().count()),
            7
        );
    }

    #[test]
    fn custom_package_keeps_assigned_photos() {
        let (_, mut s) = builtin_templates().into_iter().find(|(n, _)| n == "Custom Centered").unwrap();
        if let LayoutStyle::CustomPackage { pages } = &mut s.layout {
            pages[0].cells[0].as_photo_mut().unwrap().photo = Some("fixed".into());
        }
        let doc = s.build(&ids(4)).unwrap();
        assert_eq!(doc.pages.len(), 2); // 3 free cells per page
        assert!(doc.pages.iter().all(|p| p.cells[0].as_photo().unwrap().photo.as_deref() == Some("fixed")));
    }

    #[test]
    fn settings_round_trip_and_reject_hostile_values() {
        for (_, s) in builtin_templates() {
            let back = PrintSettings::from_json(&s.to_json().unwrap()).unwrap();
            assert_eq!(back, s);
            s.build(&[]).unwrap();
        }
        let mut s = PrintSettings::default();
        s.job.dpi = f32::NAN;
        assert!(s.build(&ids(1)).is_err());
        assert!(PrintSettings::from_json(r#"{"job":{"dpi":1e9}}"#).is_err());
        assert!(PrintSettings::from_json("{").is_err());
        let mut s = PrintSettings { layout: LayoutStyle::SingleImage { grid: Grid::new(0, 3) }, ..PrintSettings::default() };
        assert!(s.build(&ids(1)).is_err());
        s.layout = LayoutStyle::PicturePackage(PackageSpec { cells: vec![Size::new(1e6, 10.0)], gap: 0.0 });
        assert!(s.build(&ids(1)).is_err());
        assert!(PrintSettings::default().build(&ids(MAX_PHOTOS + 1)).is_err());
    }
}
