//! Icons drawn from SVG files (`assets/icons/`, see assets/ATTRIBUTION.md): each `<path>` is
//! filled with the non-zero rule and/or stroked (width, caps and joins), with the paint set on it
//! or inherited from the root `<svg>` (the forms Tabler's outline and filled files take; colours
//! are ignored). The result is rasterised with anti-aliasing at the size it is shown into a white
//! mask texture, kept per size, and painted tinted with the icon's colour.

use std::collections::HashMap;

use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2, vec2};
use kurbo::{BezPath, Cap, Join, PathEl, Point, Stroke, StrokeOpts};

/// An icon file: its name (the texture cache key), its SVG text, and whether it is drawn turned a
/// quarter turn clockwise (the file itself is unchanged).
#[derive(Clone, Copy, Debug)]
pub(super) struct Svg {
    pub name: &'static str,
    pub text: &'static str,
    pub turned: bool,
}

macro_rules! tabler {
    ($style:literal, $name:literal) => {
        Svg {
            name: concat!($style, "/", $name),
            text: include_str!(concat!("../../../../assets/icons/tabler/", $style, "/", $name, ".svg")),
            turned: false,
        }
    };
    ($style:literal, $name:literal, turned) => {
        Svg {
            name: concat!($style, "/", $name, " turned"),
            text: include_str!(concat!("../../../../assets/icons/tabler/", $style, "/", $name, ".svg")),
            turned: true,
        }
    };
}

pub(super) const PLUS: Svg = tabler!("outline", "plus");
pub(super) const CARET_DOWN: Svg = tabler!("filled", "caret-down");
pub(super) const CARET_RIGHT: Svg = tabler!("filled", "caret-right");
pub(super) const TAG: Svg = tabler!("outline", "tag");
pub(super) const TAG_FILL: Svg = tabler!("filled", "tag");
pub(super) const FOLDER: Svg = tabler!("outline", "folder");
pub(super) const FOLDER_FILL: Svg = tabler!("filled", "folder");
pub(super) const PHOTO: Svg = tabler!("outline", "photo");
pub(super) const PHOTO_FILL: Svg = tabler!("filled", "photo");
pub(super) const CLOCK: Svg = tabler!("outline", "clock");
pub(super) const CLOCK_FILL: Svg = tabler!("filled", "clock");
pub(super) const TRASH: Svg = tabler!("outline", "trash");
pub(super) const TRASH_FILL: Svg = tabler!("filled", "trash");
pub(super) const FLAG: Svg = tabler!("outline", "flag");
pub(super) const FLAG_FILL: Svg = tabler!("filled", "flag");
/// `layout-board` turned so its columns become rows: the Photo Grid's justified rows.
pub(super) const LAYOUT_BOARD_ROWS: Svg = tabler!("outline", "layout-board", turned);
pub(super) const LAYOUT_BOARD_ROWS_FILL: Svg = tabler!("filled", "layout-board", turned);
pub(super) const LAYOUT_GRID: Svg = tabler!("outline", "layout-grid");
pub(super) const LAYOUT_GRID_FILL: Svg = tabler!("filled", "layout-grid");
pub(super) const RECTANGLE: Svg = tabler!("outline", "rectangle");
pub(super) const RECTANGLE_FILL: Svg = tabler!("filled", "rectangle");
pub(super) const COLUMNS_2: Svg = tabler!("outline", "columns-2");
pub(super) const COLUMNS_2_FILL: Svg = tabler!("filled", "columns-2");
pub(super) const LAYOUT_DASHBOARD: Svg = tabler!("outline", "layout-dashboard");
pub(super) const LAYOUT_DASHBOARD_FILL: Svg = tabler!("filled", "layout-dashboard");
pub(super) const USER: Svg = tabler!("outline", "user");
pub(super) const USER_FILL: Svg = tabler!("filled", "user");
pub(super) const SORT_DESCENDING: Svg = tabler!("outline", "sort-descending");
pub(super) const STAR: Svg = tabler!("outline", "star");
pub(super) const STAR_FILL: Svg = tabler!("filled", "star");
pub(super) const CIRCLE: Svg = tabler!("outline", "circle");
pub(super) const SETTINGS: Svg = tabler!("outline", "settings");
pub(super) const SETTINGS_FILL: Svg = tabler!("filled", "settings");
pub(super) const LAYOUT_BOTTOMBAR: Svg = tabler!("outline", "layout-bottombar");
pub(super) const LAYOUT_BOTTOMBAR_FILL: Svg = tabler!("filled", "layout-bottombar");
pub(super) const CONTRAST: Svg = tabler!("outline", "contrast");
pub(super) const CONTRAST_FILL: Svg = tabler!("filled", "contrast");
pub(super) const FACE_ID: Svg = tabler!("outline", "face-id");

/// The largest mask drawn, in pixels a side (icons are small; this bounds the work and memory).
const MAX_PX: usize = 256;
/// Masks kept before the cache starts over (sizes change only with zoom and scale).
const MAX_CACHED: usize = 128;
/// Sample rows per pixel row; coverage across a row is exact.
const SUBROWS: usize = 5;
/// `<path>` elements read from one file (icons have a handful).
const MAX_ELEMENTS: usize = 64;

#[derive(Clone, Default)]
struct Cache(HashMap<(&'static str, usize), TextureHandle>);

/// Paint `svg` filling the square centred in `rect`, in `color`. A file that does not
/// parse paints nothing.
pub(super) fn paint(p: &Painter, rect: Rect, svg: Svg, color: Color32) {
    let ctx = p.ctx();
    let ppp = ctx.pixels_per_point();
    let side = rect.width().min(rect.height());
    if !(side.is_finite() && ppp.is_finite() && side > 0.0 && ppp > 0.0) {
        return;
    }
    let px = ((side * ppp).round() as usize).clamp(1, MAX_PX);
    let key = (svg.name, px);
    let cache_id = egui::Id::new("svg-icon-masks");
    let cached = ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(cache_id).0.get(&key).map(TextureHandle::id));
    let tex = match cached {
        Some(id) => id,
        None => {
            let image = ColorImage::new([px, px], mask_turned(svg.text, svg.turned, px).into_iter().map(Color32::from_white_alpha).collect());
            let tex = ctx.load_texture(format!("icon-{}-{px}", svg.name), image, TextureOptions::LINEAR);
            let id = tex.id();
            ctx.data_mut(|d| {
                let c = d.get_temp_mut_or_default::<Cache>(cache_id);
                if c.0.len() >= MAX_CACHED {
                    c.0.clear();
                }
                c.0.insert(key, tex);
            });
            id
        }
    };
    // on whole pixels, so the mask lands 1:1 and stays sharp
    let size = px as f32 / ppp;
    let min = ((rect.center().to_vec2() * ppp - vec2(px as f32, px as f32) / 2.0).round() / ppp).to_pos2();
    p.image(tex, Rect::from_min_size(min, vec2(size, size)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
}

/// The paint attributes of one tag; unset ones are inherited ([`Paint::over`]).
#[derive(Clone, Copy, Default)]
struct Paint<'a> {
    fill: Option<&'a str>,
    stroke: Option<&'a str>,
    width: Option<&'a str>,
    cap: Option<&'a str>,
    join: Option<&'a str>,
}

impl<'a> Paint<'a> {
    fn of(tag: &'a str) -> Self {
        Paint {
            fill: attr(tag, "fill"),
            stroke: attr(tag, "stroke"),
            width: attr(tag, "stroke-width"),
            cap: attr(tag, "stroke-linecap"),
            join: attr(tag, "stroke-linejoin"),
        }
    }

    /// These attributes, falling back on `parent`'s.
    fn over(self, parent: Paint<'a>) -> Self {
        Paint {
            fill: self.fill.or(parent.fill),
            stroke: self.stroke.or(parent.stroke),
            width: self.width.or(parent.width),
            cap: self.cap.or(parent.cap),
            join: self.join.or(parent.join),
        }
    }

    /// Filled unless `fill="none"` (SVG's default is a black fill).
    fn fills(&self) -> bool {
        self.fill.is_none_or(|f| f.trim() != "none")
    }

    /// The stroke, when there is one with a usable width (at most `max_width`).
    fn stroke(&self, max_width: f64) -> Option<Stroke> {
        if self.stroke.is_none_or(|s| s.trim() == "none") {
            return None;
        }
        let width = self.width.map_or(Some(1.0), |w| w.trim().parse::<f64>().ok())?;
        if !(width.is_finite() && width > 0.0) {
            return None;
        }
        let cap = match self.cap.map(str::trim) {
            Some("round") => Cap::Round,
            Some("square") => Cap::Square,
            _ => Cap::Butt,
        };
        let join = match self.join.map(str::trim) {
            Some("round") => Join::Round,
            Some("bevel") => Join::Bevel,
            _ => Join::Miter,
        };
        Some(Stroke::new(width.min(max_width)).with_start_cap(cap).with_end_cap(cap).with_join(join))
    }
}

/// The `viewBox` (x, y, width, height) and the regions an SVG paints: each `<path>`'s fill and
/// its stroke's outline, each to be filled with the non-zero rule.
fn parse(svg: &str) -> Option<([f64; 4], Vec<BezPath>)> {
    let vb: Vec<f64> = attr(svg, "viewBox")?.split([' ', ',']).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    let [x, y, w, h] = vb[..] else { return None };
    if !([x, y, w, h].iter().all(|v| v.is_finite()) && w > 0.0 && h > 0.0) {
        return None;
    }
    let root = Paint::of(svg.split("<svg").nth(1)?.split('>').next().unwrap_or(""));
    let tolerance = w.max(h) / 2000.0;
    let mut regions = Vec::new();
    for element in svg.split("<path").skip(1).take(MAX_ELEMENTS) {
        let element = element.split('>').next().unwrap_or("");
        let Some(d) = attr(element, "d") else { continue };
        let path = BezPath::from_svg(d).ok()?;
        let paint = Paint::of(element).over(root);
        if let Some(stroke) = paint.stroke(w.max(h)) {
            regions.push(kurbo::stroke(path.elements().iter().copied(), &stroke, &StrokeOpts::default(), tolerance));
        }
        if paint.fills() {
            regions.push(path);
        }
    }
    Some(([x, y, w, h], regions))
}

/// The value of the attribute `name` in `text` (the first one, double-quoted).
fn attr<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let mut from = 0;
    while let Some(at) = text.get(from..)?.find(&pat) {
        let start = from + at;
        // a whole attribute name, not the tail of another one
        let whole = text.get(..start)?.chars().next_back().is_some_and(char::is_whitespace);
        let value = start + pat.len();
        if whole {
            let rest = text.get(value..)?;
            return rest.find('"').and_then(|end| rest.get(..end));
        }
        from = value;
    }
    None
}

/// The coverage (0–255) of each pixel of a `px`×`px` mask the SVG `svg` paints, its `viewBox`
/// scaled to the whole mask. Empty when the file does not parse.
#[cfg(test)]
fn mask(svg: &str, px: usize) -> Vec<u8> {
    mask_turned(svg, false, px)
}

/// [`mask`], turned a quarter turn clockwise about the `viewBox`'s centre when `turned`.
fn mask_turned(svg: &str, turned: bool, px: usize) -> Vec<u8> {
    let Some(([vx, vy, vw, vh], mut regions)) = parse(svg) else { return vec![0; px * px] };
    if turned {
        let turn = kurbo::Affine::rotate_about(std::f64::consts::FRAC_PI_2, Point::new(vx + vw / 2.0, vy + vh / 2.0));
        for region in &mut regions {
            region.apply_affine(turn);
        }
    }
    let k = px as f64 / vw.max(vh);
    let mut total = vec![0.0f32; px * px];
    let mut cover = vec![0.0f32; px * px];
    for region in &regions {
        cover.fill(0.0);
        fill(&edges(region, k, (vx, vy)), px, &mut cover);
        // regions overlap (a stroke over its fill, strokes crossing): the union, not a sum
        for (t, c) in total.iter_mut().zip(&cover) {
            *t = t.max(*c);
        }
    }
    total.into_iter().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8).collect()
}

/// The outlines of `path` as straight edges in mask pixels (`k` pixels a unit, `origin` at 0, 0),
/// each subpath closed.
fn edges(path: &BezPath, k: f64, origin: (f64, f64)) -> Vec<[f64; 4]> {
    let to_px = |p: Point| ((p.x - origin.0) * k, (p.y - origin.1) * k);
    let mut edges: Vec<[f64; 4]> = Vec::new();
    let (mut start, mut last) = ((0.0, 0.0), (0.0, 0.0));
    kurbo::flatten(path.elements().iter().copied(), 0.1 / k, |el| match el {
        PathEl::MoveTo(p) => {
            if last != start {
                edges.push([last.0, last.1, start.0, start.1]);
            }
            start = to_px(p);
            last = start;
        }
        PathEl::LineTo(p) => {
            let p = to_px(p);
            edges.push([last.0, last.1, p.0, p.1]);
            last = p;
        }
        PathEl::ClosePath => {
            edges.push([last.0, last.1, start.0, start.1]);
            last = start;
        }
        // flattening leaves only lines
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
    });
    if last != start {
        edges.push([last.0, last.1, start.0, start.1]);
    }
    edges
}

/// Add the coverage of the region `edges` enclose (non-zero rule) to the `px`-wide `cover`.
fn fill(edges: &[[f64; 4]], px: usize, cover: &mut [f32]) {
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    let weight = 1.0 / SUBROWS as f32;
    for (row_y, row) in cover.chunks_mut(px).enumerate() {
        for sub in 0..SUBROWS {
            let y = row_y as f64 + (sub as f64 + 0.5) / SUBROWS as f64;
            crossings.clear();
            for &[x0, y0, x1, y1] in edges {
                let (up, down) = (y0 <= y && y < y1, y1 <= y && y < y0);
                let x = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
                if (up || down) && x.is_finite() {
                    crossings.push((x, if up { 1 } else { -1 }));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                let &[(a, wa), (b, _)] = pair else { continue };
                winding += wa;
                if winding != 0 {
                    add_span(row, a, b, weight);
                }
            }
        }
    }
}

/// Add `weight` × the part of each pixel of `row` that lies between `a` and `b`.
fn add_span(row: &mut [f32], a: f64, b: f64, weight: f32) {
    let w = row.len() as f64;
    let (a, b) = (a.clamp(0.0, w), b.clamp(0.0, w));
    if b <= a || a.is_nan() || b.is_nan() {
        return;
    }
    let (ia, ib) = (a.floor() as usize, b.floor() as usize);
    if ia == ib {
        if let Some(c) = row.get_mut(ia) {
            *c += (b - a) as f32 * weight;
        }
        return;
    }
    if let Some(c) = row.get_mut(ia) {
        *c += (ia as f64 + 1.0 - a) as f32 * weight;
    }
    for c in row.iter_mut().take(ib).skip(ia + 1) {
        *c += weight;
    }
    if let Some(c) = row.get_mut(ib) {
        *c += (b - ib as f64) as f32 * weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each outline icon and its filled variant.
    const PAIRS: [(Svg, Svg); 16] = [
        (TAG, TAG_FILL),
        (FOLDER, FOLDER_FILL),
        (PHOTO, PHOTO_FILL),
        (CLOCK, CLOCK_FILL),
        (TRASH, TRASH_FILL),
        (FLAG, FLAG_FILL),
        (LAYOUT_BOARD_ROWS, LAYOUT_BOARD_ROWS_FILL),
        (LAYOUT_GRID, LAYOUT_GRID_FILL),
        (RECTANGLE, RECTANGLE_FILL),
        (COLUMNS_2, COLUMNS_2_FILL),
        (LAYOUT_DASHBOARD, LAYOUT_DASHBOARD_FILL),
        (USER, USER_FILL),
        (STAR, STAR_FILL),
        (SETTINGS, SETTINGS_FILL),
        (LAYOUT_BOTTOMBAR, LAYOUT_BOTTOMBAR_FILL),
        (CONTRAST, CONTRAST_FILL),
    ];
    /// The icons without a pair.
    const SINGLES: [Svg; 6] = [PLUS, CARET_DOWN, CARET_RIGHT, SORT_DESCENDING, CIRCLE, FACE_ID];

    fn all() -> impl Iterator<Item = Svg> {
        PAIRS.into_iter().flat_map(|(a, b)| [a, b]).chain(SINGLES)
    }

    fn ink(svg: Svg, px: usize) -> f64 {
        mask_turned(svg.text, svg.turned, px).iter().map(|&a| f64::from(a) / 255.0).sum::<f64>() / (px * px) as f64
    }

    #[test]
    fn every_icon_file_parses_and_draws() {
        for svg in all() {
            let (vb, regions) = parse(svg.text).unwrap_or_else(|| panic!("{} parses", svg.name));
            assert_eq!(vb, [0.0, 0.0, 24.0, 24.0], "{}", svg.name);
            assert!(!regions.is_empty(), "{}", svg.name);
            let ink = ink(svg, 32);
            assert!(ink > 0.03 && ink < 0.8, "{} covers {ink}", svg.name);
        }
    }

    #[test]
    fn a_filled_variant_has_more_ink_than_its_outline() {
        for (outline, filled) in PAIRS {
            // (the filled trash keeps its two slots open and the filled bottom bar fills only the
            // bar, so the margin is modest)
            let ratio = ink(filled, 32) / ink(outline, 32);
            assert!(ratio > 1.1, "{} vs {}: {ratio}", filled.name, outline.name);
        }
    }

    #[test]
    fn strokes_take_their_width_caps_and_inherited_paint() {
        // Tabler's plus at one pixel a unit: 2-unit strokes from 5 to 19 with round caps
        let m = mask(PLUS.text, 24);
        let at = |x: usize, y: usize| m[y * 24 + x];
        assert_eq!(at(12, 8), 255, "on the upright, inside its width");
        assert_eq!(at(14, 8), 0, "beside the upright");
        assert!(at(12, 4) > 0, "the round cap reaches past the end");
        assert_eq!(at(12, 2), 0, "but no further than half the width");
        // a path may turn off the root's stroke and fill (Tabler's bounding-box path does)
        let m = mask(
            "<svg viewBox=\"0 0 10 10\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\">\
             <path stroke=\"none\" d=\"M0 0h10v10H0z\" fill=\"none\"/><path d=\"M5 0v10\"/></svg>",
            10,
        );
        assert_eq!(m[0], 0, "the unpainted box");
        assert_eq!(m[5 * 10 + 5], 255, "the stroked line");
        // SVG's defaults: filled, not stroked
        assert!(mask("<svg viewBox=\"0 0 10 10\"><path d=\"M0 0h10v10H0z\"/></svg>", 10).iter().all(|&a| a == 255));
    }

    #[test]
    fn a_turned_icon_has_its_columns_as_rows() {
        // layout-board's divider runs down the middle (x 11–13 at one pixel a unit); turned, across it
        let px = 24;
        let upright = mask(LAYOUT_BOARD_ROWS.text, px);
        let turned = mask_turned(LAYOUT_BOARD_ROWS.text, true, px);
        let row = |m: &[u8]| (6..18).all(|x| m[11 * px + x] == 255);
        let column = |m: &[u8]| (6..18).all(|y| m[y * px + 11] == 255);
        assert!(column(&upright) && !row(&upright), "the file's divider is upright");
        assert!(row(&turned) && !column(&turned), "turned, the divider lies across");
        // the Photo Grid's top row is split right of centre and its bottom row left of it
        assert_eq!(turned[7 * px + 15], 255, "top row split at x = 15");
        assert_eq!(turned[16 * px + 9], 255, "bottom row split at x = 9");
    }

    #[test]
    fn holes_stay_open_and_edges_are_smooth() {
        let px = 64;
        // the outline clock: a ring round an empty dial
        let m = mask(CLOCK.text, px);
        let at = |x: usize, y: usize| m[y * px + x];
        assert_eq!(at(20, 40), 0, "inside the ring, away from the hands");
        assert_eq!(at(32, 8), 255, "on the ring");
        assert_eq!(at(1, 1), 0, "outside");
        assert!(m.iter().any(|&a| a > 0 && a < 255), "anti-aliased edges");
        // the filled tag's eyelet: a hole the non-zero rule keeps open
        let m = mask(TAG_FILL.text, px);
        assert_eq!(m[20 * px + 20], 0, "the eyelet");
        assert_eq!(m[32 * px + 32], 255, "the body");
    }

    #[test]
    fn broken_or_hostile_files_draw_nothing() {
        for text in [
            "",
            "<svg>",
            "<svg viewBox=\"0 0 0 0\"><path d=\"M0,0L1,1Z\"/></svg>",
            "<svg viewBox=\"0 0 24 nan\"><path d=\"M0,0L1,1Z\"/></svg>",
            "<svg viewBox=\"0 0 24 24\"><path d=\"M0,0Q\"/></svg>",
            "<svg viewBox=\"0 0 24 24\"><path d=\"M1e308,0L-1e308,1e308Z\"/></svg>",
            "<svg viewBox=\"0 0 24 24\"><path id=\"x\" d=\"",
            "<svg viewBox=\"0 0 24 24\" stroke=\"red\" stroke-width=\"nan\"><path d=\"M0,0L1,1\"/></svg>",
            "<svg viewBox=\"0 0 24 24\" stroke=\"red\" stroke-width=\"-3\"><path d=\"M0,0L1,1\"/></svg>",
            "<svg viewBox=\"0 0 24 24\" stroke=\"red\" stroke-width=\"1e308\"><path d=\"M0,0L1,1\"/></svg>",
            "<svg viewBox=\"0 0 24 24\" stroke=\"red\" stroke-width=\"2\"><path d=\"M1e308,0L-1e308,1e308\"/></svg>",
            "<svg viewBox=\"0 0 24 24\" stroke=\"red\"><path d=\"M3,3\"/></svg>",
        ] {
            let m = mask(text, 8);
            assert_eq!(m.len(), 64, "{text}");
        }
        assert!(mask("<svg viewBox=\"0 0 24 24\"><path d=\"M0,0L1,1Z\"/></svg>", 8).iter().all(|&a| a == 0), "a line encloses nothing");
    }

    #[test]
    fn attributes_are_read_whole() {
        assert_eq!(attr("<path id=\"a\" d=\"M0\"/>", "d"), Some("M0"));
        assert_eq!(attr("<path kid=\"a\"/>", "d"), None);
        assert_eq!(attr("<path d=\"open", "d"), None);
        assert_eq!(attr("<svg stroke-width=\"2\" stroke=\"red\">", "stroke"), Some("red"));
    }
}
