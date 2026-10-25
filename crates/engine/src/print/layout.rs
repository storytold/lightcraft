//! The Print module's page layout model: paper, margins, layout styles and cells.
//!
//! Pure data and geometry, no rendering. Every length is in **inches** in page space: the origin
//! is the top-left corner of the page *as printed* (after orientation), x grows right and y down.
//! Rulers show other units ([`Unit`]) but convert at the edge; the model never stores them.
//!
//! Four layout styles ([`Style`]):
//! * `singleImage`: one photo fills the printable area of each page;
//! * `contactSheet`: a grid of identical cells ([`GridSpec`]), the photos paginated over pages;
//! * `picturePackage`: one photo at several cell sizes, packed onto pages by [`Layout::auto_layout`];
//! * `customPackage`: free placement of cells (overlap allowed, [`Stack`] order), each with its own photo.
//!
//! Package cells live in [`Layout::cells`] in stacking order (later = on top). Everything that
//! arrives from outside (commands, saved templates) passes through [`Layout::validate`]; all
//! setters reject NaN, infinite, negative and oversized numbers instead of storing them.
use serde::{Deserialize, Serialize};

/// Largest page edge accepted, inches.
pub const MAX_PAGE_IN: f64 = 60.0;
/// Smallest page edge accepted, inches.
pub const MIN_PAGE_IN: f64 = 1.0;
/// Smallest cell edge accepted, inches.
pub const MIN_CELL_IN: f64 = 0.1;
/// Package layouts hold at most this many cells.
pub const MAX_CELLS: usize = 200;
/// Package layouts span at most this many pages.
pub const MAX_PAGES: u32 = 50;
/// Largest margin accepted, inches.
pub const MAX_MARGIN_IN: f64 = 10.0;
/// The printable area must stay at least this wide and tall, inches.
pub const MIN_PRINTABLE_IN: f64 = 0.5;
/// Largest grid dimension of a contact sheet.
pub const MAX_GRID: usize = 20;
/// Geometry tolerance, inches (far below one printed pixel at any supported resolution).
pub const EPS: f64 = 1e-6;

/// Paper sizes by name: portrait width and height in millimetres (ISO) or inches (US/photo).
const PAPER: &[(&str, f64, f64)] = &[
    ("a3", 297.0 / 25.4, 420.0 / 25.4),
    ("a4", 210.0 / 25.4, 297.0 / 25.4),
    ("a5", 148.0 / 25.4, 210.0 / 25.4),
    ("letter", 8.5, 11.0),
    ("legal", 8.5, 14.0),
    ("tabloid", 11.0, 17.0),
    ("4x6", 4.0, 6.0),
    ("5x7", 5.0, 7.0),
    ("8x10", 8.0, 10.0),
    ("13x19", 13.0, 19.0),
    // Japanese photo printing and postcard sizes, at their exact metric dimensions (the US photo
    // sizes above are close but not identical: L is 89 x 127 mm, 5x7 in is 127 x 177.8 mm).
    ("l", 89.0 / 25.4, 127.0 / 25.4),
    ("2l", 127.0 / 25.4, 178.0 / 25.4),
    ("kg", 102.0 / 25.4, 152.0 / 25.4),
    ("postcard", 100.0 / 25.4, 148.0 / 25.4),
    ("double-postcard", 148.0 / 25.4, 200.0 / 25.4),
    ("6cut", 203.0 / 25.4, 254.0 / 25.4),
    ("4cut", 254.0 / 25.4, 305.0 / 25.4),
    ("business-card", 55.0 / 25.4, 91.0 / 25.4),
];

/// Japanese customary names (and a few English spellings) for the papers in [`PAPER`], matched
/// after [`normalize_paper`]: `L判` is `l`, `はがき` is `postcard`, `六切` is `6cut`, and so on.
const PAPER_ALIASES: &[(&str, &str)] = &[
    ("L判", "l"),
    ("Lサイズ", "l"),
    ("L-size", "l"),
    ("2L判", "2l"),
    ("2Lサイズ", "2l"),
    ("2L-size", "2l"),
    ("KG判", "kg"),
    ("KGサイズ", "kg"),
    ("KG-size", "kg"),
    ("はがき", "postcard"),
    ("ハガキ", "postcard"),
    ("葉書", "postcard"),
    ("ポストカード", "postcard"),
    ("往復はがき", "double-postcard"),
    ("往復ハガキ", "double-postcard"),
    ("六切", "6cut"),
    ("6切", "6cut"),
    ("四切", "4cut"),
    ("4切", "4cut"),
    ("名刺", "business-card"),
];

/// Names accepted by [`PageSetup::paper`] (besides `custom`).
pub fn paper_names() -> impl Iterator<Item = &'static str> {
    PAPER.iter().map(|p| p.0)
}

/// The form [`paper_name`] matches names in: lower case, letters and digits only, and `×`/`*` read
/// as `x`, so `L判`, `L判。`, `l-size` and `（l）` are one name and `4 × 6` is `4x6`.
fn normalize_paper(name: &str) -> String {
    name.chars()
        .filter_map(|c| match c {
            '×' | '*' => Some('x'),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect::<String>()
        .to_lowercase()
}

/// The canonical name of the paper `name` stands for, `None` for unknown names and `custom`.
/// `L判` is `l`, `はがき` is `postcard`, `六切` is `6cut`, and so on.
pub fn paper_name(name: &str) -> Option<&'static str> {
    let n = normalize_paper(name);
    if n.is_empty() {
        return None;
    }
    PAPER
        .iter()
        .find(|p| normalize_paper(p.0) == n)
        .map(|p| p.0)
        .or_else(|| PAPER_ALIASES.iter().find(|(alias, _)| normalize_paper(alias) == n).map(|(_, name)| *name))
}

/// Portrait size in inches of a named paper, `None` for unknown names and `custom`. Case, spaces
/// and punctuation don't matter, and the Japanese customary names (`L判`, `2L判`, `KG判`, `はがき`,
/// `往復はがき`, `六切`, `四切`, `名刺`) are aliases of the papers above.
pub fn paper_size(name: &str) -> Option<(f64, f64)> {
    let canonical = paper_name(name)?;
    PAPER.iter().find(|p| p.0 == canonical).map(|p| (p.1, p.2))
}

/// An axis-aligned rectangle in inches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn area(&self) -> f64 {
        self.w * self.h
    }
    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
    /// Whether the interiors overlap by more than [`EPS`] on both axes (touching edges do not count).
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.right() - EPS && o.x < self.right() - EPS && self.y < o.bottom() - EPS && o.y < self.bottom() - EPS
    }
    /// Whether `o` lies entirely inside `self`, within [`EPS`].
    pub fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x - EPS && o.y >= self.y - EPS && o.right() <= self.right() + EPS && o.bottom() <= self.bottom() + EPS
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

/// Ruler and dimension units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    #[serde(rename = "in")]
    Inches,
    #[serde(rename = "cm")]
    Centimeters,
    #[serde(rename = "mm")]
    Millimeters,
    #[serde(rename = "pt")]
    Points,
}

impl Unit {
    pub fn per_inch(self) -> f64 {
        match self {
            Unit::Inches => 1.0,
            Unit::Centimeters => 2.54,
            Unit::Millimeters => 25.4,
            Unit::Points => 72.0,
        }
    }
    pub fn from_inches(self, inches: f64) -> f64 {
        inches * self.per_inch()
    }
    pub fn to_inches(self, value: f64) -> f64 {
        value / self.per_inch()
    }
    pub fn label(self) -> &'static str {
        match self {
            Unit::Inches => "in",
            Unit::Centimeters => "cm",
            Unit::Millimeters => "mm",
            Unit::Points => "pt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Margins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Default for Margins {
    fn default() -> Self {
        Self::uniform(0.25)
    }
}

impl Margins {
    pub const fn uniform(v: f64) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }
}

/// Paper, orientation and margins (Lightroom's Page Setup + the Page margins).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PageSetup {
    /// A name from [`paper_names`] or `custom`.
    pub paper: String,
    /// Portrait size of a `custom` paper, inches.
    pub custom_width: f64,
    pub custom_height: f64,
    pub orientation: Orientation,
    pub margins: Margins,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self { paper: "letter".into(), custom_width: 8.5, custom_height: 11.0, orientation: Orientation::Portrait, margins: Margins::default() }
    }
}

fn check(v: f64, what: &str, min: f64, max: f64) -> Result<f64, String> {
    if v.is_finite() && v >= min && v <= max { Ok(v) } else { Err(format!("{what} must be a number from {min} to {max}")) }
}

impl PageSetup {
    /// Page width and height in inches after orientation.
    pub fn size(&self) -> Result<(f64, f64), String> {
        let (w, h) = if self.paper.eq_ignore_ascii_case("custom") {
            (
                check(self.custom_width, "custom width", MIN_PAGE_IN, MAX_PAGE_IN)?,
                check(self.custom_height, "custom height", MIN_PAGE_IN, MAX_PAGE_IN)?,
            )
        } else {
            paper_size(&self.paper)
                .ok_or_else(|| format!("unknown paper `{}` (one of {} or custom)", self.paper, paper_names().collect::<Vec<_>>().join(", ")))?
        };
        Ok(match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        })
    }

    /// The area inside the margins.
    pub fn printable(&self) -> Result<Rect, String> {
        let (w, h) = self.size()?;
        let m = &self.margins;
        let (t, r, b, l) = (
            check(m.top, "top margin", 0.0, MAX_MARGIN_IN)?,
            check(m.right, "right margin", 0.0, MAX_MARGIN_IN)?,
            check(m.bottom, "bottom margin", 0.0, MAX_MARGIN_IN)?,
            check(m.left, "left margin", 0.0, MAX_MARGIN_IN)?,
        );
        let (pw, ph) = (w - l - r, h - t - b);
        if pw < MIN_PRINTABLE_IN || ph < MIN_PRINTABLE_IN {
            return Err(format!("margins leave less than {MIN_PRINTABLE_IN} in of printable area"));
        }
        Ok(Rect::new(l, t, pw, ph))
    }

    /// The page size in PDF points (1/72 in).
    pub fn size_points(&self) -> Result<(f64, f64), String> {
        let (w, h) = self.size()?;
        Ok((w * 72.0, h * 72.0))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Style {
    #[default]
    SingleImage,
    ContactSheet,
    PicturePackage,
    CustomPackage,
}

impl Style {
    /// Styles whose cells are listed in [`Layout::cells`].
    pub fn is_package(self) -> bool {
        matches!(self, Style::PicturePackage | Style::CustomPackage)
    }
}

/// The contact-sheet grid. `singleImage` ignores it (one cell per page, no spacing).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct GridSpec {
    pub columns: usize,
    pub rows: usize,
    /// Gap between columns / rows, inches.
    pub spacing_x: f64,
    pub spacing_y: f64,
    /// Make the cells square (the grid is centred in the printable area).
    pub keep_square: bool,
}

impl Default for GridSpec {
    fn default() -> Self {
        Self { columns: 3, rows: 4, spacing_x: 0.1, spacing_y: 0.1, keep_square: false }
    }
}

/// One cell of a package layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Cell {
    pub id: u32,
    /// Zero-based page the cell is on.
    pub page: u32,
    pub rect: Rect,
    /// The photo printed in the cell (Custom Package); `None` uses the job's first photo.
    /// Not part of a saved template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo: Option<u64>,
}

/// Where a cell goes in the stacking order of its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stack {
    ToBack,
    Backward,
    Forward,
    ToFront,
}

/// How dragged cells snap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Snap {
    Off,
    /// To the edges of the other cells and of the printable area.
    #[default]
    Cells,
    /// To multiples of [`Guides::grid_size`] from the printable area's corner.
    Grid,
}

/// Rulers, grid and guides (Lightroom's Guides panel).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Guides {
    pub show_rulers: bool,
    pub units: Unit,
    pub snap: Snap,
    /// Grid pitch, inches.
    pub grid_size: f64,
    pub show_margins: bool,
    pub show_bleed: bool,
    pub show_page_grid: bool,
    pub show_cells: bool,
    pub show_dimensions: bool,
}

impl Default for Guides {
    fn default() -> Self {
        Self {
            show_rulers: true,
            units: Unit::Inches,
            snap: Snap::Cells,
            grid_size: 0.25,
            show_margins: true,
            show_bleed: false,
            show_page_grid: false,
            show_cells: true,
            show_dimensions: false,
        }
    }
}

impl Guides {
    pub fn validate(&self) -> Result<(), String> {
        check(self.grid_size, "grid size", 0.05, 10.0).map(|_| ())
    }
}

/// The page layout: setup, style and cells. See the module docs for the units and style rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Layout {
    pub style: Style,
    pub page: PageSetup,
    pub grid: GridSpec,
    /// Gap kept between cells by [`Layout::auto_layout`] and new cells, inches.
    pub spacing: f64,
    /// Pages of a package layout (at least 1 and past every cell).
    pub pages: u32,
    /// Package cells in stacking order, back to front.
    pub cells: Vec<Cell>,
    next_id: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            style: Style::SingleImage,
            page: PageSetup::default(),
            grid: GridSpec::default(),
            spacing: 0.1,
            pages: 1,
            cells: Vec::new(),
            next_id: 1,
        }
    }
}

/// Free space on one page as a set of maximal rectangles (the MaxRects scheme).
struct Free(Vec<Rect>);

impl Free {
    fn new(area: Rect) -> Self {
        Self(vec![area])
    }

    /// Removes `used` from the free space.
    fn occupy(&mut self, used: &Rect) {
        let mut next = Vec::with_capacity(self.0.len() + 4);
        for f in &self.0 {
            if !f.overlaps(used) {
                next.push(*f);
                continue;
            }
            if used.x > f.x + EPS {
                next.push(Rect::new(f.x, f.y, used.x - f.x, f.h));
            }
            if used.right() < f.right() - EPS {
                next.push(Rect::new(used.right(), f.y, f.right() - used.right(), f.h));
            }
            if used.y > f.y + EPS {
                next.push(Rect::new(f.x, f.y, f.w, used.y - f.y));
            }
            if used.bottom() < f.bottom() - EPS {
                next.push(Rect::new(f.x, used.bottom(), f.w, f.bottom() - used.bottom()));
            }
        }
        // Drop rectangles contained in another (the first of two equal ones survives).
        let mut keep: Vec<Rect> = Vec::with_capacity(next.len());
        for (i, r) in next.iter().enumerate() {
            let dominated = next.iter().enumerate().any(|(j, o)| i != j && o.contains(r) && (!r.contains(o) || j < i));
            if !dominated {
                keep.push(*r);
            }
        }
        self.0 = keep;
    }

    /// The best spot for a `w` x `h` block: least leftover on the short side, then the long side,
    /// then top-most and left-most. The block is turned a quarter (when `rotate`) only if it fits
    /// nowhere as given, so cells keep their shape whenever they can. Returns the spot and
    /// whether the block was turned.
    fn best_fit(&self, w: f64, h: f64, rotate: bool) -> Option<(Rect, bool)> {
        self.fit_block(w, h)
            .map(|r| (r, false))
            .or_else(|| if rotate && (w - h).abs() >= EPS { self.fit_block(h, w).map(|r| (r, true)) } else { None })
    }

    fn fit_block(&self, bw: f64, bh: f64) -> Option<Rect> {
        let mut best: Option<((f64, f64, f64, f64), Rect)> = None;
        let eq = |a: f64, b: f64| (a - b).abs() < EPS;
        for f in &self.0 {
            if bw > f.w + EPS || bh > f.h + EPS {
                continue;
            }
            let (dw, dh) = (f.w - bw, f.h - bh);
            let key = (dw.min(dh), dw.max(dh), f.y, f.x);
            let better = best.as_ref().is_none_or(|(k, _)| {
                if !eq(k.0, key.0) {
                    return key.0 < k.0;
                }
                if !eq(k.1, key.1) {
                    return key.1 < k.1;
                }
                if !eq(k.2, key.2) {
                    return key.2 < k.2;
                }
                key.3 < k.3 - EPS
            });
            if better {
                best = Some((key, Rect::new(f.x, f.y, bw, bh)));
            }
        }
        best.map(|(_, r)| r)
    }
}

/// Where each block of [`pack`] went: page index, rectangle in page space and whether it was turned.
pub type Placement = (u32, Rect, bool);

/// Packs blocks (width, height) onto pages of `area`, keeping `spacing` between them.
///
/// Deterministic: blocks are placed largest first (area, then longer side, then input order) with
/// the MaxRects best-short-side-fit rule, filling earlier pages before opening a new one. Blocks
/// may be turned a quarter when `rotate`. Fails when a block cannot fit an empty page or the
/// result would need more than [`MAX_PAGES`] pages. The result is in input order.
pub fn pack(sizes: &[(f64, f64)], area: Rect, spacing: f64, rotate: bool) -> Result<Vec<Placement>, String> {
    if !area.is_finite() || area.w <= 0.0 || area.h <= 0.0 {
        return Err("no printable area to pack into".into());
    }
    let spacing = check(spacing, "spacing", 0.0, MAX_PAGE_IN)?;
    if sizes.len() > MAX_CELLS {
        return Err(format!("at most {MAX_CELLS} cells"));
    }
    for (w, h) in sizes {
        check(*w, "cell width", MIN_CELL_IN, MAX_PAGE_IN)?;
        check(*h, "cell height", MIN_CELL_IN, MAX_PAGE_IN)?;
    }
    // Each block carries `spacing` on its right and bottom; the area grows by the same so a block
    // flush with the margin still fits.
    let padded = Rect::new(area.x, area.y, area.w + spacing, area.h + spacing);
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (sizes.get(a).copied().unwrap_or_default(), sizes.get(b).copied().unwrap_or_default());
        (sb.0 * sb.1).total_cmp(&(sa.0 * sa.1)).then((sb.0.max(sb.1)).total_cmp(&sa.0.max(sa.1))).then(a.cmp(&b))
    });
    let mut pages: Vec<Free> = Vec::new();
    let mut out: Vec<Option<Placement>> = vec![None; sizes.len()];
    for i in order {
        let (w, h) = sizes.get(i).copied().unwrap_or_default();
        let (bw, bh) = (w + spacing, h + spacing);
        let mut placed = None;
        for (p, free) in pages.iter().enumerate() {
            if let Some(hit) = free.best_fit(bw, bh, rotate) {
                placed = Some((p, hit));
                break;
            }
        }
        let (p, (spot, turned)) = match placed {
            Some(v) => v,
            None => {
                if pages.len() as u32 >= MAX_PAGES {
                    return Err(format!("the cells need more than {MAX_PAGES} pages"));
                }
                let fresh = Free::new(padded);
                let hit = fresh.best_fit(bw, bh, rotate).ok_or_else(|| format!("a {w:.2} x {h:.2} in cell does not fit the printable area"))?;
                pages.push(fresh);
                (pages.len() - 1, hit)
            }
        };
        if let Some(free) = pages.get_mut(p) {
            free.occupy(&spot);
        }
        let (cw, ch) = if turned { (h, w) } else { (w, h) };
        if let Some(slot) = out.get_mut(i) {
            *slot = Some((p as u32, Rect::new(spot.x, spot.y, cw, ch), turned));
        }
    }
    out.into_iter().map(|p| p.ok_or_else(|| "internal packing error".to_string())).collect()
}

/// Snaps a span (`start`, `len`) along one axis so its start or end lands on the nearest candidate
/// within `threshold`; unchanged when none is close.
fn snap_span(start: f64, len: f64, candidates: &[f64], threshold: f64) -> f64 {
    let mut best: Option<(f64, f64)> = None;
    for &c in candidates {
        for shift in [c - start, c - (start + len)] {
            if shift.abs() <= threshold && best.is_none_or(|(d, _)| shift.abs() < d) {
                best = Some((shift.abs(), shift));
            }
        }
    }
    start + best.map_or(0.0, |b| b.1)
}

/// Snaps one coordinate (a dragged edge) to the nearest candidate within `threshold`.
fn snap_edge(v: f64, candidates: &[f64], threshold: f64) -> f64 {
    candidates.iter().copied().filter(|c| (c - v).abs() <= threshold).min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs())).unwrap_or(v)
}

impl Layout {
    /// Page size in inches after orientation.
    pub fn page_size(&self) -> Result<(f64, f64), String> {
        self.page.size()
    }

    /// The printable area (inside the margins).
    pub fn printable(&self) -> Result<Rect, String> {
        self.page.printable()
    }

    /// Checks every number and cross-reference; also repairs `next_id` so ids stay unique.
    /// Run on anything that did not come from the setters (commands, templates).
    pub fn validate(&mut self) -> Result<(), String> {
        let area = self.page.printable()?;
        check(self.spacing, "spacing", 0.0, 10.0)?;
        if !(1..=MAX_GRID).contains(&self.grid.columns) || !(1..=MAX_GRID).contains(&self.grid.rows) {
            return Err(format!("columns and rows must be 1 to {MAX_GRID}"));
        }
        check(self.grid.spacing_x, "column spacing", 0.0, 10.0)?;
        check(self.grid.spacing_y, "row spacing", 0.0, 10.0)?;
        for r in self.grid_rects_for(area)? {
            if r.w < MIN_CELL_IN || r.h < MIN_CELL_IN {
                return Err("the grid leaves cells smaller than 0.1 in".into());
            }
        }
        if self.cells.len() > MAX_CELLS {
            return Err(format!("at most {MAX_CELLS} cells"));
        }
        if !(1..=MAX_PAGES).contains(&self.pages) {
            return Err(format!("pages must be 1 to {MAX_PAGES}"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.cells {
            if !seen.insert(c.id) {
                return Err(format!("duplicate cell id {}", c.id));
            }
            if c.page >= self.pages {
                return Err(format!("cell {} is on page {} but the layout has {} pages", c.id, c.page + 1, self.pages));
            }
            self.check_rect(&c.rect, area)?;
        }
        self.next_id = self.next_id.max(self.cells.iter().map(|c| c.id).max().unwrap_or(0).saturating_add(1)).max(1);
        Ok(())
    }

    fn check_rect(&self, r: &Rect, area: Rect) -> Result<(), String> {
        if !r.is_finite() || r.w < MIN_CELL_IN - EPS || r.h < MIN_CELL_IN - EPS {
            return Err(format!("cells must be at least {MIN_CELL_IN} in wide and tall"));
        }
        if !area.contains(r) {
            return Err("a cell lies outside the printable area".into());
        }
        Ok(())
    }

    /// The cells of one grid page in reading order (`singleImage`: one cell filling the area).
    pub fn grid_rects(&self) -> Result<Vec<Rect>, String> {
        self.grid_rects_for(self.page.printable()?)
    }

    fn grid_rects_for(&self, area: Rect) -> Result<Vec<Rect>, String> {
        if self.style == Style::SingleImage {
            return Ok(vec![area]);
        }
        let g = &self.grid;
        let (cols, rows) = (g.columns.clamp(1, MAX_GRID), g.rows.clamp(1, MAX_GRID));
        let gaps_x = g.spacing_x * (cols - 1) as f64;
        let gaps_y = g.spacing_y * (rows - 1) as f64;
        let mut cw = (area.w - gaps_x) / cols as f64;
        let mut ch = (area.h - gaps_y) / rows as f64;
        if !(cw > 0.0 && ch > 0.0) {
            return Err("the grid spacing leaves no room for cells".into());
        }
        let (mut ox, mut oy) = (area.x, area.y);
        if g.keep_square {
            let side = cw.min(ch);
            ox += (cw - side) * cols as f64 / 2.0;
            oy += (ch - side) * rows as f64 / 2.0;
            cw = side;
            ch = side;
        }
        let mut out = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                out.push(Rect::new(ox + c as f64 * (cw + g.spacing_x), oy + r as f64 * (ch + g.spacing_y), cw, ch));
            }
        }
        Ok(out)
    }

    /// Cells per page of a grid style; 1 for the package styles.
    pub fn cells_per_page(&self) -> usize {
        match self.style {
            Style::SingleImage => 1,
            Style::ContactSheet => self.grid.columns.clamp(1, MAX_GRID) * self.grid.rows.clamp(1, MAX_GRID),
            Style::PicturePackage | Style::CustomPackage => 1,
        }
    }

    /// Pages a print of `photos` photos needs: the photos over the grid for the grid styles, the
    /// layout's own pages for the packages.
    pub fn page_count(&self, photos: usize) -> usize {
        match self.style {
            Style::SingleImage | Style::ContactSheet => photos.max(1).div_ceil(self.cells_per_page()),
            Style::PicturePackage | Style::CustomPackage => self.pages.max(1) as usize,
        }
    }

    pub fn cell(&self, id: u32) -> Option<&Cell> {
        self.cells.iter().find(|c| c.id == id)
    }

    fn index(&self, id: u32) -> Result<usize, String> {
        self.cells.iter().position(|c| c.id == id).ok_or_else(|| format!("no cell {id}"))
    }

    /// Cells of one page, back to front.
    pub fn cells_on_page(&self, page: u32) -> impl Iterator<Item = &Cell> {
        self.cells.iter().filter(move |c| c.page == page)
    }

    /// Sets the layout style. Entering a package style from a grid style keeps existing cells.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn require_package(&self) -> Result<(), String> {
        if self.style.is_package() { Ok(()) } else { Err("cells exist only in the Picture Package and Custom Package styles".into()) }
    }

    /// Adds a `w` x `h` in cell at the first free spot of the earliest page with room (a new page
    /// when none has), and returns its id. A cell too large for the printable area is refused.
    pub fn add_cell(&mut self, w: f64, h: f64) -> Result<u32, String> {
        self.require_package()?;
        let area = self.printable()?;
        check(w, "cell width", MIN_CELL_IN, MAX_PAGE_IN)?;
        check(h, "cell height", MIN_CELL_IN, MAX_PAGE_IN)?;
        if self.cells.len() >= MAX_CELLS {
            return Err(format!("at most {MAX_CELLS} cells"));
        }
        if w > area.w + EPS || h > area.h + EPS {
            return Err(format!("a {w:.2} x {h:.2} in cell is larger than the printable area ({:.2} x {:.2} in)", area.w, area.h));
        }
        let padded = Rect::new(area.x, area.y, area.w + self.spacing, area.h + self.spacing);
        let spot = (0..self.pages).find_map(|p| {
            let mut free = Free::new(padded);
            for c in self.cells_on_page(p) {
                free.occupy(&Rect::new(c.rect.x, c.rect.y, c.rect.w + self.spacing, c.rect.h + self.spacing));
            }
            free.best_fit(w + self.spacing, h + self.spacing, false).map(|(r, _)| (p, r))
        });
        let (page, at) = match spot {
            Some(v) => v,
            None => {
                if self.pages >= MAX_PAGES {
                    return Err(format!("a layout holds at most {MAX_PAGES} pages"));
                }
                self.pages += 1;
                (self.pages - 1, Rect::new(area.x, area.y, w, h))
            }
        };
        let id = self.take_id();
        self.cells.push(Cell { id, page, rect: Rect::new(at.x, at.y, w, h), photo: None });
        Ok(id)
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        id
    }

    /// Moves a cell so its top-left corner is at (`x`, `y`), kept inside the printable area.
    pub fn move_cell(&mut self, id: u32, x: f64, y: f64) -> Result<Rect, String> {
        self.require_package()?;
        let area = self.printable()?;
        if !x.is_finite() || !y.is_finite() {
            return Err("position must be finite".into());
        }
        let i = self.index(id)?;
        let cell = self.cells.get_mut(i).ok_or("no such cell")?;
        let r = &mut cell.rect;
        r.x = x.clamp(area.x, (area.right() - r.w).max(area.x));
        r.y = y.clamp(area.y, (area.bottom() - r.h).max(area.y));
        Ok(*r)
    }

    /// Gives a cell a new rectangle (position and size), clamped into the printable area.
    pub fn resize_cell(&mut self, id: u32, rect: Rect) -> Result<Rect, String> {
        self.require_package()?;
        let area = self.printable()?;
        if !rect.is_finite() {
            return Err("rectangle must be finite".into());
        }
        let w = check(rect.w, "cell width", MIN_CELL_IN, MAX_PAGE_IN)?.min(area.w);
        let h = check(rect.h, "cell height", MIN_CELL_IN, MAX_PAGE_IN)?.min(area.h);
        let i = self.index(id)?;
        let cell = self.cells.get_mut(i).ok_or("no such cell")?;
        cell.rect = Rect::new(rect.x.clamp(area.x, (area.right() - w).max(area.x)), rect.y.clamp(area.y, (area.bottom() - h).max(area.y)), w, h);
        Ok(cell.rect)
    }

    pub fn delete_cell(&mut self, id: u32) -> Result<(), String> {
        let i = self.index(id)?;
        self.cells.remove(i);
        Ok(())
    }

    /// Removes every cell and extra page (Lightroom's Clear Layout).
    pub fn clear(&mut self) {
        self.cells.clear();
        self.pages = 1;
    }

    /// Appends an empty page to a package layout and returns its zero-based index.
    pub fn new_page(&mut self) -> Result<u32, String> {
        self.require_package()?;
        if self.pages >= MAX_PAGES {
            return Err(format!("a layout holds at most {MAX_PAGES} pages"));
        }
        self.pages += 1;
        Ok(self.pages - 1)
    }

    /// Moves a cell to another page, keeping its rectangle (clamped inside the printable area).
    pub fn set_cell_page(&mut self, id: u32, page: u32) -> Result<(), String> {
        self.require_package()?;
        if page >= self.pages {
            return Err(format!("the layout has {} pages", self.pages));
        }
        let i = self.index(id)?;
        if let Some(c) = self.cells.get_mut(i) {
            c.page = page;
        }
        Ok(())
    }

    /// Assigns (or with `None` clears) the photo printed in a cell.
    pub fn set_cell_photo(&mut self, id: u32, photo: Option<u64>) -> Result<(), String> {
        let i = self.index(id)?;
        if let Some(c) = self.cells.get_mut(i) {
            c.photo = photo;
        }
        Ok(())
    }

    /// Changes a cell's place in the stacking order of its page (Send to Back, Backward, Forward,
    /// Bring to Front). Cells on other pages are not moved past.
    pub fn restack(&mut self, id: u32, how: Stack) -> Result<(), String> {
        let i = self.index(id)?;
        let page = self.cells.get(i).map(|c| c.page).ok_or("no such cell")?;
        let same: Vec<usize> = self.cells.iter().enumerate().filter(|(_, c)| c.page == page).map(|(k, _)| k).collect();
        let pos = same.iter().position(|&k| k == i).ok_or("no such cell")?;
        let target = match how {
            Stack::ToBack => 0,
            Stack::Backward => pos.saturating_sub(1),
            Stack::Forward => (pos + 1).min(same.len().saturating_sub(1)),
            Stack::ToFront => same.len().saturating_sub(1),
        };
        if target == pos {
            return Ok(());
        }
        let cell = self.cells.remove(i);
        // Index in the shortened vector where the target neighbour now sits.
        let at = same.get(target).map(|&k| if k > i { k - 1 } else { k }).ok_or("no such cell")?;
        let at = if target > pos { at + 1 } else { at };
        self.cells.insert(at.min(self.cells.len()), cell);
        Ok(())
    }

    /// Re-packs every cell of the package onto as few pages as possible (Auto Layout). Cells keep
    /// their sizes (a cell may be turned a quarter to fit) and their stacking order; their
    /// photos are untouched. Fails, changing nothing, when a cell cannot fit the printable area.
    pub fn auto_layout(&mut self) -> Result<(), String> {
        self.require_package()?;
        let area = self.printable()?;
        let sizes: Vec<(f64, f64)> = self.cells.iter().map(|c| (c.rect.w, c.rect.h)).collect();
        let placed = pack(&sizes, area, self.spacing, true)?;
        let mut pages = 1;
        for (c, (page, rect, _)) in self.cells.iter_mut().zip(placed) {
            c.page = page;
            c.rect = rect;
            pages = pages.max(page + 1);
        }
        self.pages = pages;
        Ok(())
    }

    /// Pulls every cell back inside the printable area (shrinking one that is bigger than it), for
    /// after the paper, orientation or margins changed.
    pub fn fit_cells(&mut self) -> Result<(), String> {
        let area = self.printable()?;
        for c in &mut self.cells {
            let r = &mut c.rect;
            if !r.is_finite() {
                continue;
            }
            r.w = r.w.min(area.w);
            r.h = r.h.min(area.h);
            r.x = r.x.clamp(area.x, (area.right() - r.w).max(area.x));
            r.y = r.y.clamp(area.y, (area.bottom() - r.h).max(area.y));
        }
        Ok(())
    }

    /// Whether two cells on the same page overlap.
    pub fn has_overlaps(&self) -> bool {
        self.cells.iter().enumerate().any(|(i, a)| self.cells.iter().skip(i + 1).any(|b| a.page == b.page && a.rect.overlaps(&b.rect)))
    }

    /// Snap candidates along each axis for cell `skip` on `page`: edges of other cells and of the
    /// printable area, or grid lines.
    fn candidates(&self, page: u32, skip: Option<u32>, snap: Snap, grid: f64) -> Result<(Vec<f64>, Vec<f64>), String> {
        let area = self.printable()?;
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        match snap {
            Snap::Off => {}
            Snap::Cells => {
                xs.extend([area.x, area.right()]);
                ys.extend([area.y, area.bottom()]);
                for c in self.cells_on_page(page).filter(|c| Some(c.id) != skip) {
                    xs.extend([c.rect.x, c.rect.right()]);
                    ys.extend([c.rect.y, c.rect.bottom()]);
                }
            }
            Snap::Grid => {
                let grid = check(grid, "grid size", 0.05, 10.0)?;
                let n = |len: f64| (len / grid).floor().min(4000.0) as usize;
                xs.extend((0..=n(area.w)).map(|k| area.x + k as f64 * grid));
                ys.extend((0..=n(area.h)).map(|k| area.y + k as f64 * grid));
                xs.push(area.right());
                ys.push(area.bottom());
            }
        }
        Ok((xs, ys))
    }

    /// Snaps a cell being dragged: `rect` keeps its size, and its left or right edge (likewise top
    /// or bottom) lands on a candidate within `threshold` inches.
    pub fn snap_move(&self, id: u32, rect: Rect, snap: Snap, grid: f64, threshold: f64) -> Result<Rect, String> {
        if !rect.is_finite() || !threshold.is_finite() {
            return Err("rectangle must be finite".into());
        }
        let page = self.cell(id).map(|c| c.page).ok_or_else(|| format!("no cell {id}"))?;
        let (xs, ys) = self.candidates(page, Some(id), snap, grid)?;
        let area = self.printable()?;
        let x = snap_span(rect.x, rect.w, &xs, threshold.max(0.0));
        let y = snap_span(rect.y, rect.h, &ys, threshold.max(0.0));
        Ok(Rect::new(x.clamp(area.x, (area.right() - rect.w).max(area.x)), y.clamp(area.y, (area.bottom() - rect.h).max(area.y)), rect.w, rect.h))
    }

    /// Snaps a cell being resized by its bottom-right corner: the right and bottom edges land on
    /// candidates within `threshold`, the top-left corner stays.
    pub fn snap_resize(&self, id: u32, rect: Rect, snap: Snap, grid: f64, threshold: f64) -> Result<Rect, String> {
        if !rect.is_finite() || !threshold.is_finite() {
            return Err("rectangle must be finite".into());
        }
        let page = self.cell(id).map(|c| c.page).ok_or_else(|| format!("no cell {id}"))?;
        let (xs, ys) = self.candidates(page, Some(id), snap, grid)?;
        let right = snap_edge(rect.right(), &xs, threshold.max(0.0));
        let bottom = snap_edge(rect.bottom(), &ys, threshold.max(0.0));
        Ok(Rect::new(rect.x, rect.y, (right - rect.x).max(MIN_CELL_IN), (bottom - rect.y).max(MIN_CELL_IN)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Layout {
        Layout { style: Style::PicturePackage, ..Default::default() }
    }

    fn assert_clean(l: &Layout) {
        let area = l.printable().unwrap();
        for c in &l.cells {
            assert!(area.contains(&c.rect), "{c:?} outside {area:?}");
            assert!(c.page < l.pages);
        }
        assert!(!l.has_overlaps(), "{:#?}", l.cells);
    }

    #[test]
    fn papers_and_orientation() {
        let mut p = PageSetup::default();
        assert_eq!(p.size().unwrap(), (8.5, 11.0));
        p.orientation = Orientation::Landscape;
        assert_eq!(p.size().unwrap(), (11.0, 8.5));
        assert_eq!(p.size_points().unwrap(), (792.0, 612.0));
        p.paper = "A4".into();
        let (w, h) = p.size().unwrap();
        assert!((w - 11.693).abs() < 1e-3 && (h - 8.268).abs() < 1e-3);
        for n in paper_names() {
            p.paper = n.into();
            assert!(p.printable().is_ok(), "{n}");
        }
        p.paper = "custom".into();
        p.custom_width = 20.0;
        p.custom_height = 30.0;
        assert_eq!(p.size().unwrap(), (30.0, 20.0));
        p.paper = "poster".into();
        assert!(p.size().is_err());
    }

    /// Japanese photo and postcard papers: exact metric sizes to 0.1 pt, the customary names as
    /// aliases, and the ISO/US papers around them unchanged.
    #[test]
    fn japanese_photo_and_postcard_papers() {
        let pt = |name: &str| {
            let p = PageSetup { paper: name.into(), ..Default::default() };
            p.size_points().unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        for (name, w_mm, h_mm) in [
            ("L判", 89.0, 127.0),
            ("2L判", 127.0, 178.0),
            ("KG判", 102.0, 152.0),
            ("はがき", 100.0, 148.0),
            ("往復はがき", 148.0, 200.0),
            ("六切", 203.0, 254.0),
            ("四切", 254.0, 305.0),
            ("名刺", 55.0, 91.0),
        ] {
            let (w, h) = pt(name);
            let want = (w_mm / 25.4 * 72.0, h_mm / 25.4 * 72.0);
            assert!((w - want.0).abs() < 0.1 && (h - want.1).abs() < 0.1, "{name}: {w} x {h} pt, wanted {want:?}");
            assert!((w - h).abs() > 1e-6, "{name} is portrait");
        }
        // The customary and English spellings resolve to the same paper, including spaces and case.
        for (a, b) in [
            ("L判", "l"),
            ("L判", " L "),
            ("L判", "L-size"),
            ("2L判", "2l"),
            ("KG判", "kg"),
            ("はがき", "postcard"),
            ("はがき", "ハガキ"),
            ("往復はがき", "double-postcard"),
            ("六切", "6cut"),
            ("六切", "6切"),
            ("四切", "4cut"),
            ("名刺", "business-card"),
            ("A4", "a 4"),
            ("4 × 6", "4x6"),
        ] {
            assert_eq!(paper_size(a), paper_size(b), "{a} vs {b}");
        }
        // Punctuation is ignored, including the periods and Japanese marks a pasted name may carry.
        for (name, canonical) in [("L判。", "l"), ("L判、", "l"), ("（L判）", "l"), ("l-size.", "l"), ("はがき。", "postcard"), ("6切。", "6cut")]
        {
            assert_eq!(paper_name(name), Some(canonical), "{name}");
        }
        assert_eq!(paper_name("custom"), None);
        assert!(paper_size("poster").is_none());
        // The papers that were already there keep their sizes.
        assert_eq!(pt("letter"), (612.0, 792.0));
        assert_eq!(pt("4x6"), (288.0, 432.0));
        assert_eq!(pt("5x7"), (360.0, 504.0));
        assert_eq!(pt("8x10"), (576.0, 720.0));
        assert_eq!(pt("13x19"), (936.0, 1368.0));
        let (w, h) = pt("A4");
        assert!((w - 595.28).abs() < 0.01 && (h - 841.89).abs() < 0.01, "A4 {w} x {h}");
    }

    #[test]
    fn hostile_numbers_are_rejected() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e9] {
            let p = PageSetup { paper: "custom".into(), custom_width: bad, ..Default::default() };
            assert!(p.size().is_err(), "{bad}");
            let p = PageSetup { margins: Margins { left: bad, ..Margins::default() }, ..Default::default() };
            assert!(p.printable().is_err(), "{bad}");
            let mut l = package();
            assert!(l.add_cell(bad, 2.0).is_err(), "{bad}");
            assert!(l.add_cell(2.0, bad).is_err());
            let id = l.add_cell(2.0, 2.0).unwrap();
            assert!(bad.is_finite() || l.move_cell(id, bad, 0.0).is_err());
            assert!(l.resize_cell(id, Rect::new(0.0, 0.0, bad, 1.0)).is_err());
            assert!(bad.is_finite() || l.snap_move(id, Rect::new(bad, 0.0, 1.0, 1.0), Snap::Cells, 0.25, 0.1).is_err());
            let mut g = Layout { spacing: bad, ..package() };
            assert!(g.validate().is_err());
            assert!(pack(&[(1.0, 1.0)], l.printable().unwrap(), bad, true).is_err());
            assert!(pack(&[(bad, 1.0)], l.printable().unwrap(), 0.0, true).is_err());
        }
        // Margins that leave nothing.
        let p = PageSetup { margins: Margins::uniform(4.5), ..Default::default() };
        assert!(p.printable().is_err());
        let mut l = Layout { grid: GridSpec { columns: 0, ..Default::default() }, ..Default::default() };
        assert!(l.validate().is_err());
        l.grid.columns = 21;
        assert!(l.validate().is_err());
    }

    #[test]
    fn units_convert() {
        for u in [Unit::Inches, Unit::Centimeters, Unit::Millimeters, Unit::Points] {
            assert!((u.to_inches(u.from_inches(3.5)) - 3.5).abs() < 1e-12);
        }
        assert!((Unit::Millimeters.from_inches(1.0) - 25.4).abs() < 1e-12);
        assert_eq!(Unit::Points.from_inches(2.0), 144.0);
        assert_eq!(serde_json::to_string(&Unit::Centimeters).unwrap(), "\"cm\"");
    }

    #[test]
    fn contact_grid_fills_the_printable_area() {
        let l = Layout {
            style: Style::ContactSheet,
            grid: GridSpec { columns: 3, rows: 4, spacing_x: 0.2, spacing_y: 0.1, keep_square: false },
            ..Default::default()
        };
        let area = l.printable().unwrap();
        let cells = l.grid_rects().unwrap();
        assert_eq!(cells.len(), 12);
        let first = cells.first().unwrap();
        let last = cells.last().unwrap();
        assert!((first.x - area.x).abs() < 1e-9 && (first.y - area.y).abs() < 1e-9);
        assert!((last.right() - area.right()).abs() < 1e-9 && (last.bottom() - area.bottom()).abs() < 1e-9);
        for (i, a) in cells.iter().enumerate() {
            assert!((a.w - first.w).abs() < 1e-12 && (a.h - first.h).abs() < 1e-12);
            for b in cells.iter().skip(i + 1) {
                assert!(!a.overlaps(b));
            }
        }
        // Square cells are centred.
        let sq = Layout { grid: GridSpec { keep_square: true, ..l.grid }, ..l.clone() };
        let cells = sq.grid_rects().unwrap();
        assert!(cells.iter().all(|c| (c.w - c.h).abs() < 1e-12));
        let (lo, hi) = (cells.iter().map(|c| c.x).fold(f64::MAX, f64::min), cells.iter().map(|c| c.right()).fold(f64::MIN, f64::max));
        assert!(((lo - area.x) - (area.right() - hi)).abs() < 1e-9);
        // Single image ignores the grid.
        let one = Layout { style: Style::SingleImage, ..l };
        assert_eq!(one.grid_rects().unwrap(), vec![area]);
    }

    #[test]
    fn page_counts() {
        let mut l = Layout { style: Style::ContactSheet, grid: GridSpec { columns: 2, rows: 2, ..Default::default() }, ..Default::default() };
        assert_eq!(l.page_count(0), 1);
        assert_eq!(l.page_count(4), 1);
        assert_eq!(l.page_count(5), 2);
        l.style = Style::SingleImage;
        assert_eq!(l.page_count(3), 3);
        l.style = Style::PicturePackage;
        l.pages = 4;
        assert_eq!(l.page_count(99), 4);
    }

    #[test]
    fn packer_is_deterministic_and_clean() {
        let area = Layout::default().printable().unwrap();
        let sizes: Vec<(f64, f64)> =
            [(4.0, 6.0), (5.0, 7.0), (2.5, 3.5), (2.5, 3.5), (3.5, 5.0), (8.0, 10.0), (4.0, 6.0), (2.0, 2.0), (1.0, 1.5), (5.0, 7.0), (4.0, 6.0)]
                .to_vec();
        let a = pack(&sizes, area, 0.1, true).unwrap();
        assert_eq!(a, pack(&sizes, area, 0.1, true).unwrap());
        assert_eq!(a.len(), sizes.len());
        for (i, (p, r, turned)) in a.iter().enumerate() {
            assert!(area.contains(r), "{r:?}");
            let (w, h) = sizes[i];
            let expect = if *turned { (h, w) } else { (w, h) };
            assert!((r.w - expect.0).abs() < 1e-9 && (r.h - expect.1).abs() < 1e-9);
            for (j, (q, s, _)) in a.iter().enumerate().skip(i + 1) {
                if p == q {
                    // The spacing is kept between neighbours.
                    let grown = Rect::new(r.x, r.y, r.w + 0.1 - 2e-6, r.h + 0.1 - 2e-6);
                    assert!(!grown.overlaps(s), "{i} and {j}: {r:?} {s:?}");
                }
            }
        }
        // The input order does not hide a block that needs a page of its own.
        assert!(a.iter().map(|p| p.0).max().unwrap() >= 1);
    }

    #[test]
    fn packer_rotates_only_when_needed_and_refuses_oversize() {
        let area = Rect::new(0.25, 0.25, 8.0, 10.5);
        // 10 x 5 only fits a portrait page turned.
        let r = pack(&[(10.0, 5.0)], area, 0.0, true).unwrap();
        assert!(r[0].2 && (r[0].1.w - 5.0).abs() < 1e-9 && (r[0].1.h - 10.0).abs() < 1e-9);
        assert!(pack(&[(10.0, 5.0)], area, 0.0, false).is_err());
        let r = pack(&[(4.0, 6.0)], area, 0.0, true).unwrap();
        assert!(!r[0].2, "a block that fits as given is not turned");
        assert!(pack(&[(9.0, 11.0)], area, 0.0, true).is_err());
        // A block that exactly fills the area fits despite the spacing.
        assert!(pack(&[(8.0, 10.5)], area, 0.2, true).is_ok());
        assert!(pack(&vec![(1.0, 1.0); MAX_CELLS + 1], area, 0.0, true).is_err());
        // More pages than allowed.
        assert!(pack(&vec![(8.0, 10.5); 51], area, 0.0, true).is_err());
        assert_eq!(pack(&vec![(8.0, 10.5); 50], area, 0.0, true).unwrap().iter().map(|p| p.0).max(), Some(49));
    }

    #[test]
    fn packing_is_dense() {
        // Four 4x5.25 cells tile a letter page's 8 x 10.5 area exactly.
        let area = Rect::new(0.25, 0.25, 8.0, 10.5);
        let r = pack(&[(4.0, 5.25); 4], area, 0.0, false).unwrap();
        assert!(r.iter().all(|p| p.0 == 0));
        // Eight 4x6 photos fit two pages (four per page with the rotation trick is impossible: 4x6 x2 per column).
        let r = pack(&[(4.0, 6.0); 4], area, 0.0, true).unwrap();
        assert_eq!(r.iter().map(|p| p.0).max(), Some(1));
    }

    #[test]
    fn add_cell_fills_gaps_then_opens_pages() {
        let mut l = package();
        let a = l.add_cell(3.9, 6.0).unwrap();
        let b = l.add_cell(3.9, 6.0).unwrap();
        assert_eq!(l.cell(a).unwrap().rect.x, 0.25);
        assert_ne!(l.cell(a).unwrap().rect, l.cell(b).unwrap().rect);
        assert_clean(&l);
        // Keep adding until a second page appears.
        for _ in 0..6 {
            l.add_cell(3.9, 6.0).unwrap();
        }
        assert!(l.pages >= 2);
        assert_clean(&l);
        assert_eq!(l.cells.len(), 8);
        assert!(l.add_cell(9.0, 9.0).is_err(), "wider than the printable area");
        let mut grid = Layout::default();
        assert!(grid.add_cell(1.0, 1.0).is_err());
    }

    #[test]
    fn cell_cap() {
        let mut l = package();
        for _ in 0..MAX_CELLS {
            if l.add_cell(0.5, 0.5).is_err() {
                break;
            }
        }
        assert!(l.cells.len() <= MAX_CELLS);
        let n = l.cells.len();
        if n == MAX_CELLS {
            assert!(l.add_cell(0.5, 0.5).is_err());
        }
    }

    #[test]
    fn move_and_resize_stay_inside() {
        let mut l = package();
        let id = l.add_cell(2.0, 3.0).unwrap();
        let r = l.move_cell(id, 100.0, -5.0).unwrap();
        let area = l.printable().unwrap();
        assert!((r.right() - area.right()).abs() < 1e-9 && (r.y - area.y).abs() < 1e-9);
        let r = l.resize_cell(id, Rect::new(7.0, 9.0, 50.0, 50.0)).unwrap();
        assert!(area.contains(&r));
        assert!(l.resize_cell(id, Rect::new(0.0, 0.0, 0.0, 1.0)).is_err());
        assert!(l.move_cell(77, 0.0, 0.0).is_err());
        l.delete_cell(id).unwrap();
        assert!(l.delete_cell(id).is_err());
    }

    #[test]
    fn stacking_order() {
        let mut l = Layout { style: Style::CustomPackage, ..Default::default() };
        let ids: Vec<u32> = (0..4).map(|_| l.add_cell(1.0, 1.0).unwrap()).collect();
        let order = |l: &Layout| l.cells.iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(order(&l), ids);
        l.restack(ids[0], Stack::ToFront).unwrap();
        assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
        l.restack(ids[0], Stack::Backward).unwrap();
        assert_eq!(order(&l), vec![ids[1], ids[2], ids[0], ids[3]]);
        l.restack(ids[0], Stack::Forward).unwrap();
        assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
        l.restack(ids[0], Stack::Forward).unwrap();
        assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
        l.restack(ids[0], Stack::ToBack).unwrap();
        assert_eq!(order(&l), ids);
        l.restack(ids[0], Stack::Backward).unwrap();
        assert_eq!(order(&l), ids);
        assert!(l.restack(99, Stack::ToBack).is_err());
    }

    #[test]
    fn stacking_ignores_other_pages() {
        let mut l = Layout { style: Style::CustomPackage, pages: 2, ..Default::default() };
        let a = l.add_cell(1.0, 1.0).unwrap();
        let b = l.add_cell(1.0, 1.0).unwrap();
        let c = l.add_cell(1.0, 1.0).unwrap();
        l.set_cell_page(b, 1).unwrap();
        l.restack(a, Stack::ToFront).unwrap();
        // `a` goes past `c` (same page) and not necessarily past `b`.
        let pos = |id| l.cells.iter().position(|x| x.id == id).unwrap();
        assert!(pos(a) > pos(c));
    }

    #[test]
    fn overlap_is_allowed_by_move_and_reported() {
        let mut l = Layout { style: Style::CustomPackage, ..Default::default() };
        let a = l.add_cell(3.0, 3.0).unwrap();
        let b = l.add_cell(3.0, 3.0).unwrap();
        assert!(!l.has_overlaps());
        let r = l.cell(a).unwrap().rect;
        l.move_cell(b, r.x + 1.0, r.y + 1.0).unwrap();
        assert!(l.has_overlaps());
        // Touching edges are not an overlap.
        l.move_cell(b, r.right(), r.y).unwrap();
        assert!(!l.has_overlaps());
    }

    #[test]
    fn auto_layout_repacks_cleanly_and_keeps_photos() {
        let mut l = package();
        for (w, h) in [(4.0, 6.0), (5.0, 7.0), (2.5, 3.5), (8.0, 10.0), (4.0, 6.0), (3.5, 5.0)] {
            l.add_cell(w, h).unwrap();
        }
        let first = l.cells[0].id;
        l.set_cell_photo(first, Some(42)).unwrap();
        // Scramble positions to overlap, then auto layout.
        for c in &mut l.cells {
            c.rect.x = 0.25;
            c.rect.y = 0.25;
            c.page = 0;
        }
        l.pages = 1;
        assert!(l.has_overlaps());
        l.auto_layout().unwrap();
        assert_clean(&l);
        assert_eq!(l.cell(first).unwrap().photo, Some(42));
        assert_eq!(l.cells.len(), 6);
        let again = l.clone();
        let mut l2 = again.clone();
        l2.auto_layout().unwrap();
        l.auto_layout().unwrap();
        assert_eq!(l, l2);
    }

    #[test]
    fn auto_layout_failure_changes_nothing() {
        let mut l = package();
        l.add_cell(2.0, 2.0).unwrap();
        l.cells.push(Cell { id: 50, page: 0, rect: Rect::new(0.25, 0.25, 40.0, 40.0), photo: None });
        let before = l.clone();
        assert!(l.auto_layout().is_err());
        assert_eq!(l, before);
    }

    #[test]
    fn clear_and_new_page() {
        let mut l = package();
        l.add_cell(2.0, 2.0).unwrap();
        assert_eq!(l.new_page().unwrap(), 1);
        assert_eq!(l.pages, 2);
        l.clear();
        assert_eq!((l.cells.len(), l.pages), (0, 1));
        for _ in 1..MAX_PAGES {
            l.new_page().unwrap();
        }
        assert!(l.new_page().is_err());
        let mut g = Layout::default();
        assert!(g.new_page().is_err());
    }

    #[test]
    fn validate_catches_bad_cells() {
        let mut l = package();
        l.add_cell(2.0, 2.0).unwrap();
        let ok = l.clone();
        let mut bad = ok.clone();
        bad.cells[0].rect.w = f64::NAN;
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.cells[0].page = 3;
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.cells[0].rect.x = 50.0;
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.cells.push(bad.cells[0].clone());
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.pages = 0;
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.next_id = 0;
        bad.validate().unwrap();
        assert!(bad.next_id > bad.cells[0].id);
        assert!(serde_json::from_str::<Layout>(r#"{"surprise":1}"#).is_err());
        let mut round: Layout = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        round.validate().unwrap();
        assert_eq!(round, ok);
    }

    #[test]
    fn snapping_to_cells_grid_and_margins() {
        let mut l = Layout { style: Style::CustomPackage, spacing: 0.0, ..Default::default() };
        let a = l.add_cell(3.0, 3.0).unwrap();
        let b = l.add_cell(2.0, 2.0).unwrap();
        let ra = l.cell(a).unwrap().rect;
        // Left edge of b near the right edge of a.
        let m = l.snap_move(b, Rect::new(ra.right() + 0.04, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
        assert!((m.x - ra.right()).abs() < 1e-9);
        assert_eq!(m.y, 5.0);
        // Too far: untouched.
        let m = l.snap_move(b, Rect::new(ra.right() + 0.5, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
        assert!((m.x - (ra.right() + 0.5)).abs() < 1e-9);
        // Right edge snaps to the printable margin.
        let area = l.printable().unwrap();
        let m = l.snap_move(b, Rect::new(area.right() - 2.0 - 0.03, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
        assert!((m.right() - area.right()).abs() < 1e-9);
        // Off leaves everything.
        let m = l.snap_move(b, Rect::new(ra.right() + 0.04, 5.0, 2.0, 2.0), Snap::Off, 0.25, 0.1).unwrap();
        assert!((m.x - (ra.right() + 0.04)).abs() < 1e-9);
        // Grid snapping lands on multiples from the printable corner.
        let m = l.snap_move(b, Rect::new(1.37, 2.62, 2.0, 2.0), Snap::Grid, 0.5, 0.2).unwrap();
        assert!(((m.x - area.x) / 0.5).fract().abs() < 1e-9 || (((m.x - area.x) / 0.5).fract() - 1.0).abs() < 1e-9);
        // Resize snaps the dragged corner only.
        let r = l.snap_resize(b, Rect::new(5.0, 5.0, 2.1, 1.9), Snap::Grid, 0.5, 0.2).unwrap();
        assert_eq!((r.x, r.y), (5.0, 5.0));
        assert!(((r.right() - area.x) / 0.5 - ((r.right() - area.x) / 0.5).round()).abs() < 1e-9);
        // Never below the minimum size.
        let r = l.snap_resize(b, Rect::new(5.0, 5.0, 0.0, 0.0), Snap::Grid, 0.5, 0.2).unwrap();
        assert!(r.w >= MIN_CELL_IN && r.h >= MIN_CELL_IN);
        assert!(l.snap_move(99, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Cells, 0.25, 0.1).is_err());
        assert!(l.snap_move(b, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Grid, 0.0, 0.1).is_err());
        assert!(l.snap_move(b, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Grid, f64::NAN, 0.1).is_err());
    }

    #[test]
    fn fit_cells_follows_the_margins() {
        let mut l = package();
        let id = l.add_cell(7.0, 10.0).unwrap();
        l.page.margins = Margins::uniform(1.0);
        assert!(l.validate().is_err(), "the cell no longer fits");
        l.fit_cells().unwrap();
        l.validate().unwrap();
        let r = l.cell(id).unwrap().rect;
        assert!(l.printable().unwrap().contains(&r));
        assert!((r.w - 6.5).abs() < 1e-9 && (r.h - 9.0).abs() < 1e-9);
        l.page.paper = "poster".into();
        assert!(l.fit_cells().is_err());
    }

    #[test]
    fn cell_aspect_survives_auto_layout() {
        let mut l = package();
        let id = l.add_cell(2.5, 3.5).unwrap();
        l.auto_layout().unwrap();
        let r = l.cell(id).unwrap().rect;
        assert!((r.w / r.h - 2.5 / 3.5).abs() < 1e-9 || (r.h / r.w - 2.5 / 3.5).abs() < 1e-9);
    }

    #[test]
    fn guides_validate() {
        assert!(Guides::default().validate().is_ok());
        for bad in [f64::NAN, 0.0, -1.0, 1e9] {
            assert!(Guides { grid_size: bad, ..Default::default() }.validate().is_err());
        }
    }

    #[test]
    fn rect_helpers() {
        let a = Rect::new(0.0, 0.0, 2.0, 2.0);
        assert!(a.overlaps(&Rect::new(1.0, 1.0, 2.0, 2.0)));
        assert!(!a.overlaps(&Rect::new(2.0, 0.0, 1.0, 1.0)));
        assert!(a.contains(&Rect::new(0.5, 0.5, 1.0, 1.0)));
        assert!(!a.contains(&Rect::new(1.5, 0.5, 1.0, 1.0)));
        assert_eq!(a.area(), 4.0);
    }
}
