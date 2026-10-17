//! Our own vector icons, drawn in code on a 20×20 design grid (1.5 px strokes, rounded caps).
//! Original work (see assets/ATTRIBUTION.md) — not derived from any third-party icon set.
//!
//! The library sidebar's and the bottom bar's icons (and their other uses) come from Tabler Icons
//! (MIT, files in `assets/icons/tabler/`, drawn by [`svg`]), most with a filled variant for the
//! selected row or mode ([`Icon::chosen`]; the `_Chosen` icons of issues #1591 and #1615).

mod svg;

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Sidebar,
    Back,
    Forward,
    Search,
    Filter,
    Bell,
    Share,
    Help,
    Cloud,
    Sliders,
    Crop,
    Eraser,
    Mask,
    Eye,
    /// Eye with a slash (hidden).
    EyeOff,
    Presets,
    Versions,
    Activity,
    More,
    /// Keyword (Tabler `tag`).
    Tag,
    /// [`Icon::Tag`] filled: a selected keyword.
    TagFilled,
    Info,
    /// Photo Grid view (Tabler `layout-board`, turned so its columns are rows).
    GridPhoto,
    /// [`Icon::GridPhoto`] filled: the Photo Grid view is on.
    GridPhotoFilled,
    /// Square Grid view (Tabler `layout-grid`).
    GridSquare,
    /// [`Icon::GridSquare`] filled.
    GridSquareFilled,
    /// Detail view (Tabler `rectangle`).
    Single,
    /// [`Icon::Single`] filled.
    SingleFilled,
    /// Compare view (Tabler `columns-2`).
    Compare,
    /// [`Icon::Compare`] filled.
    CompareFilled,
    /// Survey view (Tabler `layout-dashboard`).
    Survey,
    /// [`Icon::Survey`] filled.
    SurveyFilled,
    /// People view (Tabler `user`).
    People,
    /// [`Icon::People`] filled.
    PeopleFilled,
    /// Tabler `sort-descending`.
    Sort,
    /// Tabler `star`.
    Star,
    /// Tabler filled `star`: a rating star that is on.
    StarFilled,
    /// Unflagged (Tabler `circle`).
    Circle,
    /// Pick flag (Tabler `flag`).
    FlagPick,
    /// [`Icon::FlagPick`] filled: the selected Picks row.
    FlagPickFilled,
    /// The pick flag with a cross in it.
    FlagReject,
    /// Settings (Tabler `settings`).
    Gear,
    /// [`Icon::Gear`] filled.
    GearFilled,
    /// Filmstrip on/off (Tabler `layout-bottombar`).
    Filmstrip,
    /// [`Icon::Filmstrip`] filled: the filmstrip is shown.
    FilmstripFilled,
    /// Before/After (Tabler `contrast`).
    BeforeAfter,
    /// [`Icon::BeforeAfter`] filled: a before/after layout is on.
    BeforeAfterFilled,
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    ChevronUp,
    ChevronDoubleLeft,
    ChevronDoubleRight,
    /// Disclosure triangle of an open sidebar row (Tabler filled `caret-down`).
    CaretDown,
    /// Disclosure triangle of a closed sidebar row (Tabler filled `caret-right`).
    CaretRight,
    /// Tabler `plus`.
    Plus,
    Minus,
    Album,
    SmartAlbum,
    Stack,
    /// Tabler `folder`.
    Folder,
    /// [`Icon::Folder`] filled: a selected folder.
    FolderFilled,
    /// Edit (a pencil).
    Pencil,
    /// All Photos: a picture (Tabler `photo`).
    Photos,
    /// [`Icon::Photos`] filled: the selected All Photos row.
    PhotosFilled,
    /// Tabler `clock`.
    Clock,
    /// [`Icon::Clock`] filled: the selected Recently Added or date row.
    ClockFilled,
    Calendar,
    /// Tabler `trash`.
    Trash,
    /// [`Icon::Trash`] filled: the selected Recently Deleted row.
    TrashFilled,
    Curve,
    ProfileGrid,
    Close,
    Check,
    Brush,
    Linear,
    Radial,
    Sky,
    Subject,
    Picker,
    Rotate,
    /// A curved line with an arrow at both ends: drag to rotate, either way (the crop tool's pointer).
    RotateDrag,
    Flip,
    Invert,
    Dots,
    Video,
    Heart,
    Edited,
    /// Targeted adjustment: a ring with a centre dot and up/down arrows (drag vertically).
    Target,
    /// Community chat (opens the ArtCraft Discord): a speech bubble with three dots. Our own
    /// generic drawing, not any service's logo.
    Chat,
    /// Face boxes on/off (Tabler `face-id`).
    FaceBox,
    /// Play (a triangle pointing right).
    Play,
    /// Pause (two bars).
    Pause,
    /// Colour grading, all three wheels: three small circles.
    GradeAll,
    /// Colour grading, shadows wheel: a filled circle.
    GradeShadows,
    /// Colour grading, midtones wheel: a half-filled circle.
    GradeMidtones,
    /// Colour grading, highlights wheel: an open circle with rays.
    GradeHighlights,
    /// Appearance Mode ▸ Auto (follow the system): a monitor on its stand.
    Monitor,
    /// Appearance Mode ▸ Light: a sun (a disc with eight rays).
    Sun,
    /// Appearance Mode ▸ Dark: a crescent moon.
    Moon,
}

impl Icon {
    /// The icon shown when its row or item is selected: the filled variant where there is one.
    pub fn chosen(self) -> Icon {
        match self {
            Icon::Photos => Icon::PhotosFilled,
            Icon::Clock => Icon::ClockFilled,
            Icon::FlagPick => Icon::FlagPickFilled,
            Icon::Trash => Icon::TrashFilled,
            Icon::Folder => Icon::FolderFilled,
            Icon::Tag => Icon::TagFilled,
            Icon::GridPhoto => Icon::GridPhotoFilled,
            Icon::GridSquare => Icon::GridSquareFilled,
            Icon::Single => Icon::SingleFilled,
            Icon::Compare => Icon::CompareFilled,
            Icon::Survey => Icon::SurveyFilled,
            Icon::People => Icon::PeopleFilled,
            Icon::Star => Icon::StarFilled,
            Icon::Gear => Icon::GearFilled,
            Icon::Filmstrip => Icon::FilmstripFilled,
            Icon::BeforeAfter => Icon::BeforeAfterFilled,
            other => other,
        }
    }

    /// The SVG file this icon is drawn from, if it is not drawn in code.
    fn svg(self) -> Option<svg::Svg> {
        Some(match self {
            Icon::Plus => svg::PLUS,
            Icon::CaretDown => svg::CARET_DOWN,
            Icon::CaretRight => svg::CARET_RIGHT,
            Icon::Tag => svg::TAG,
            Icon::TagFilled => svg::TAG_FILL,
            Icon::Folder => svg::FOLDER,
            Icon::FolderFilled => svg::FOLDER_FILL,
            Icon::Photos => svg::PHOTO,
            Icon::PhotosFilled => svg::PHOTO_FILL,
            Icon::Clock => svg::CLOCK,
            Icon::ClockFilled => svg::CLOCK_FILL,
            Icon::Trash => svg::TRASH,
            Icon::TrashFilled => svg::TRASH_FILL,
            Icon::FlagPick => svg::FLAG,
            Icon::FlagPickFilled => svg::FLAG_FILL,
            Icon::GridPhoto => svg::LAYOUT_BOARD_ROWS,
            Icon::GridPhotoFilled => svg::LAYOUT_BOARD_ROWS_FILL,
            Icon::GridSquare => svg::LAYOUT_GRID,
            Icon::GridSquareFilled => svg::LAYOUT_GRID_FILL,
            Icon::Single => svg::RECTANGLE,
            Icon::SingleFilled => svg::RECTANGLE_FILL,
            Icon::Compare => svg::COLUMNS_2,
            Icon::CompareFilled => svg::COLUMNS_2_FILL,
            Icon::Survey => svg::LAYOUT_DASHBOARD,
            Icon::SurveyFilled => svg::LAYOUT_DASHBOARD_FILL,
            Icon::People => svg::USER,
            Icon::PeopleFilled => svg::USER_FILL,
            Icon::Sort => svg::SORT_DESCENDING,
            Icon::Star => svg::STAR,
            Icon::StarFilled => svg::STAR_FILL,
            Icon::Circle => svg::CIRCLE,
            Icon::Gear => svg::SETTINGS,
            Icon::GearFilled => svg::SETTINGS_FILL,
            Icon::Filmstrip => svg::LAYOUT_BOTTOMBAR,
            Icon::FilmstripFilled => svg::LAYOUT_BOTTOMBAR_FILL,
            Icon::BeforeAfter => svg::CONTRAST,
            Icon::BeforeAfterFilled => svg::CONTRAST_FILL,
            Icon::FaceBox => svg::FACE_ID,
            _ => return None,
        })
    }
}

struct Pen<'a> {
    p: &'a Painter,
    r: Rect,
    s: Stroke,
    k: f32,
    /// (cos, sin) of a turn about the grid's centre, clockwise on screen ([`paint_turned`]).
    turn: Option<(f32, f32)>,
}

impl Pen<'_> {
    fn pt(&self, x: f32, y: f32) -> Pos2 {
        let (x, y) = match self.turn {
            Some((c, s)) => (10.0 + (x - 10.0) * c - (y - 10.0) * s, 10.0 + (x - 10.0) * s + (y - 10.0) * c),
            None => (x, y),
        };
        pos2(self.r.min.x + x * self.k, self.r.min.y + y * self.k)
    }
    fn line(&self, pts: &[(f32, f32)]) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::line(v, self.s));
    }
    fn closed(&self, pts: &[(f32, f32)]) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::closed_line(v, self.s));
    }
    fn fill(&self, pts: &[(f32, f32)], c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.pt(*x, *y)).collect();
        self.p.add(Shape::convex_polygon(v, c, Stroke::NONE));
    }
    fn circle(&self, x: f32, y: f32, r: f32) {
        self.p.circle_stroke(self.pt(x, y), r * self.k, self.s);
    }
    fn dot(&self, x: f32, y: f32, r: f32) {
        self.p.circle_filled(self.pt(x, y), r * self.k, self.s.color);
    }
    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, round: f32) {
        self.p.rect_stroke(Rect::from_min_max(self.pt(x0, y0), self.pt(x1, y1)), round * self.k, self.s, egui::StrokeKind::Middle);
    }
    fn rect_fill(&self, x0: f32, y0: f32, x1: f32, y1: f32, round: f32) {
        self.p.rect_filled(Rect::from_min_max(self.pt(x0, y0), self.pt(x1, y1)), round * self.k, self.s.color);
    }
    fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
        let n = 24;
        let v: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.line(&v);
    }
}

/// Paint `icon` centred in `rect` (square area used) with `color`.
pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint_with(p, rect, icon, color, None);
}

/// [`paint`], turned `radians` clockwise about its centre. Only for icons drawn with lines, arcs
/// and circles (rectangles stay square to the screen; icons from SVG files are not turned).
pub fn paint_turned(p: &Painter, rect: Rect, icon: Icon, color: Color32, radians: f32) {
    paint_with(p, rect, icon, color, Some((radians.cos(), radians.sin())));
}

fn paint_with(p: &Painter, rect: Rect, icon: Icon, color: Color32, turn: Option<(f32, f32)>) {
    let side = rect.width().min(rect.height());
    // All disclosure and navigation chevrons share a 16-point size and stroke.
    let side = if matches!(
        icon,
        Icon::Back
            | Icon::Forward
            | Icon::ChevronDown
            | Icon::ChevronUp
            | Icon::ChevronLeft
            | Icon::ChevronRight
            | Icon::ChevronDoubleLeft
            | Icon::ChevronDoubleRight
    ) {
        side.min(16.0)
    } else {
        side
    };
    let r = Rect::from_center_size(rect.center(), vec2(side, side));
    let k = side / 20.0;
    if let Some(file) = icon.svg() {
        svg::paint(p, r, file, color);
        return;
    }
    let pen = Pen { p, r, s: Stroke::new((1.5 * k).max(1.0), color), k, turn };
    use Icon::*;
    match icon {
        // drawn from their files above
        Plus | CaretDown | CaretRight | Tag | TagFilled | Folder | FolderFilled | Photos | PhotosFilled | Clock | ClockFilled | Trash
        | TrashFilled | FlagPick | FlagPickFilled | GridPhoto | GridPhotoFilled | GridSquare | GridSquareFilled | Single | SingleFilled | Compare
        | CompareFilled | Survey | SurveyFilled | People | PeopleFilled | Sort | Star | StarFilled | Circle | Gear | GearFilled | Filmstrip
        | FilmstripFilled | BeforeAfter | BeforeAfterFilled | FaceBox => {}
        Sidebar => {
            pen.rect(3.0, 4.0, 17.0, 16.0, 1.5);
            pen.line(&[(8.0, 4.0), (8.0, 16.0)]);
        }
        Monitor => {
            pen.rect(2.5, 3.5, 17.5, 13.5, 1.5);
            pen.line(&[(10.0, 13.5), (10.0, 16.5)]);
            pen.line(&[(6.5, 16.5), (13.5, 16.5)]);
        }
        Sun => {
            pen.circle(10.0, 10.0, 3.2);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                let (c, s) = (a.cos(), a.sin());
                pen.line(&[(10.0 + 5.4 * c, 10.0 + 5.4 * s), (10.0 + 7.4 * c, 10.0 + 7.4 * s)]);
            }
        }
        Moon => {
            // outer arc of the disc, then the inner arc of the bite, meeting at the horns
            pen.arc(10.0, 10.0, 6.5, 2.9, 263.8);
            pen.arc(13.4, 6.4, 5.0, 214.9, 51.8);
        }
        Search => {
            pen.circle(8.5, 8.5, 5.0);
            pen.line(&[(12.2, 12.2), (16.5, 16.5)]);
        }
        Filter => pen.closed(&[(3.0, 4.0), (17.0, 4.0), (11.5, 10.5), (11.5, 16.0), (8.5, 14.5), (8.5, 10.5)]),
        Bell => {
            pen.line(&[(5.0, 14.0), (5.0, 9.0)]);
            pen.arc(10.0, 9.0, 5.0, 180.0, 360.0);
            pen.line(&[(15.0, 9.0), (15.0, 14.0)]);
            pen.line(&[(3.5, 14.0), (16.5, 14.0)]);
            pen.arc(10.0, 15.5, 1.8, 0.0, 180.0);
        }
        Share => {
            pen.line(&[(6.0, 8.0), (4.0, 8.0), (4.0, 17.0), (16.0, 17.0), (16.0, 8.0), (14.0, 8.0)]);
            pen.line(&[(10.0, 12.0), (10.0, 2.5)]);
            pen.line(&[(6.5, 6.0), (10.0, 2.5), (13.5, 6.0)]);
        }
        Help => {
            pen.circle(10.0, 10.0, 7.5);
            pen.arc(10.0, 8.0, 2.5, 180.0, 400.0);
            pen.line(&[(10.0, 10.5), (10.0, 11.8)]);
            pen.dot(10.0, 14.2, 0.9);
        }
        Cloud => {
            pen.arc(7.0, 11.5, 3.5, 90.0, 270.0);
            pen.arc(10.5, 8.5, 4.5, 190.0, 340.0);
            pen.arc(14.0, 11.8, 3.2, 270.0, 450.0);
            pen.line(&[(7.0, 15.0), (14.0, 15.0)]);
        }
        Sliders => {
            for (y, x) in [(5.0, 12.0), (10.0, 7.0), (15.0, 13.0)] {
                pen.line(&[(3.0, y), (x - 2.0, y)]);
                pen.line(&[(x + 2.0, y), (17.0, y)]);
                pen.circle(x, y, 2.0);
            }
        }
        Crop => {
            pen.line(&[(6.0, 2.5), (6.0, 14.0), (17.5, 14.0)]);
            pen.line(&[(2.5, 6.0), (14.0, 6.0), (14.0, 17.5)]);
            pen.line(&[(15.5, 2.0), (17.5, 4.0), (15.5, 6.0)]);
        }
        Eraser => {
            pen.closed(&[(3.0, 12.0), (10.0, 5.0), (16.0, 11.0), (11.0, 16.0), (7.0, 16.0)]);
            pen.line(&[(6.5, 8.5), (12.5, 14.5)]);
            pen.line(&[(7.0, 17.5), (17.0, 17.5)]);
        }
        Mask => {
            pen.circle(10.0, 10.0, 7.0);
            for i in 0..5 {
                for j in 0..5 {
                    let (x, y) = (5.0 + i as f32 * 2.5, 5.0 + j as f32 * 2.5);
                    if (x - 10.0).hypot(y - 10.0) < 5.8 {
                        pen.dot(x, y, 0.6);
                    }
                }
            }
        }
        Eye => {
            pen.arc(10.0, 16.0, 9.0, 212.0, 328.0);
            pen.arc(10.0, 4.0, 9.0, 32.0, 148.0);
            pen.circle(10.0, 10.0, 2.5);
        }
        GradeAll => {
            pen.circle(10.0, 6.0, 3.2);
            pen.circle(5.5, 13.5, 3.2);
            pen.circle(14.5, 13.5, 3.2);
        }
        GradeShadows => {
            pen.circle(10.0, 10.0, 6.0);
            pen.dot(10.0, 10.0, 6.0);
        }
        GradeMidtones => {
            pen.circle(10.0, 10.0, 6.0);
            let half: Vec<(f32, f32)> = (0..=12)
                .map(|i| {
                    let a = (90.0 + 180.0 * i as f32 / 12.0).to_radians();
                    (10.0 + 6.0 * a.cos(), 10.0 + 6.0 * a.sin())
                })
                .collect();
            pen.fill(&half, pen.s.color);
        }
        GradeHighlights => {
            pen.circle(10.0, 10.0, 4.0);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                pen.line(&[(10.0 + 6.0 * a.cos(), 10.0 + 6.0 * a.sin()), (10.0 + 8.0 * a.cos(), 10.0 + 8.0 * a.sin())]);
            }
        }
        EyeOff => {
            pen.arc(10.0, 16.0, 9.0, 212.0, 328.0);
            pen.arc(10.0, 4.0, 9.0, 32.0, 148.0);
            pen.circle(10.0, 10.0, 2.5);
            pen.line(&[(3.5, 3.5), (16.5, 16.5)]);
        }
        Presets => {
            pen.circle(8.0, 8.0, 5.0);
            pen.circle(12.0, 12.0, 5.0);
        }
        Versions => {
            pen.rect(5.0, 3.0, 17.0, 13.0, 1.5);
            pen.line(&[(3.0, 6.0), (3.0, 16.0), (14.0, 16.0)]);
        }
        Activity => {
            pen.circle(10.0, 10.0, 7.0);
            pen.line(&[(10.0, 6.0), (10.0, 10.0), (13.0, 12.0)]);
        }
        More | Dots => {
            for x in [5.0, 10.0, 15.0] {
                pen.dot(x, 10.0, 1.3);
            }
        }
        Info => {
            pen.circle(10.0, 10.0, 7.5);
            pen.line(&[(10.0, 9.0), (10.0, 14.0)]);
            pen.dot(10.0, 6.3, 0.9);
        }
        FlagReject => {
            // the pick flag, crossed out inside its cloth
            svg::paint(p, r, svg::FLAG, color);
            pen.line(&[(8.0, 6.0), (12.0, 10.0)]);
            pen.line(&[(12.0, 6.0), (8.0, 10.0)]);
        }
        ChevronDown => pen.line(&[(5.0, 7.5), (10.0, 12.5), (15.0, 7.5)]),
        Forward | ChevronRight => pen.line(&[(7.5, 5.0), (12.5, 10.0), (7.5, 15.0)]),
        Back | ChevronLeft => pen.line(&[(12.5, 5.0), (7.5, 10.0), (12.5, 15.0)]),
        ChevronDoubleLeft => {
            pen.line(&[(9.0, 5.0), (4.0, 10.0), (9.0, 15.0)]);
            pen.line(&[(16.0, 5.0), (11.0, 10.0), (16.0, 15.0)]);
        }
        ChevronDoubleRight => {
            pen.line(&[(4.0, 5.0), (9.0, 10.0), (4.0, 15.0)]);
            pen.line(&[(11.0, 5.0), (16.0, 10.0), (11.0, 15.0)]);
        }
        ChevronUp => pen.line(&[(5.0, 12.5), (10.0, 7.5), (15.0, 12.5)]),
        Minus => pen.line(&[(4.0, 10.0), (16.0, 10.0)]),
        Album => {
            pen.rect(3.0, 5.0, 15.0, 17.0, 1.0);
            pen.line(&[(6.0, 2.5), (17.5, 2.5), (17.5, 14.0)]);
        }
        Stack => {
            pen.rect(2.5, 7.0, 13.5, 17.5, 1.0);
            pen.line(&[(5.0, 4.5), (16.0, 4.5), (16.0, 14.5)]);
            pen.line(&[(7.5, 2.0), (18.5, 2.0), (18.5, 11.5)]);
        }
        SmartAlbum => {
            pen.rect(3.0, 5.0, 15.0, 17.0, 1.0);
            pen.line(&[(6.0, 2.5), (17.5, 2.5), (17.5, 14.0)]);
            // a small funnel: the album is a saved filter
            pen.closed(&[(5.5, 8.5), (12.5, 8.5), (10.0, 11.5), (10.0, 14.5), (8.0, 13.5), (8.0, 11.5)]);
        }
        Pencil => {
            pen.closed(&[(4.0, 16.0), (4.5, 12.5), (13.5, 3.5), (16.5, 6.5), (7.5, 15.5)]);
            pen.line(&[(11.5, 5.5), (14.5, 8.5)]);
        }
        Calendar => {
            // a page with a binding bar, two rings and a marked day
            pen.rect(3.0, 4.5, 17.0, 17.0, 1.5);
            pen.line(&[(3.0, 8.5), (17.0, 8.5)]);
            pen.line(&[(7.0, 2.5), (7.0, 6.0)]);
            pen.line(&[(13.0, 2.5), (13.0, 6.0)]);
            pen.rect(11.0, 11.5, 14.0, 14.5, 0.5);
        }
        Curve => {
            pen.rect(3.0, 3.0, 17.0, 17.0, 1.5);
            let pts: Vec<(f32, f32)> = (0..=12)
                .map(|i| {
                    let t = i as f32 / 12.0;
                    let y = t * t * (3.0 - 2.0 * t);
                    (4.5 + 11.0 * t, 15.5 - 11.0 * y)
                })
                .collect();
            pen.line(&pts);
        }
        ProfileGrid => {
            pen.rect_fill(3.5, 3.5, 9.0, 9.0, 0.5);
            pen.rect_fill(11.0, 3.5, 16.5, 9.0, 0.5);
            pen.rect_fill(3.5, 11.0, 9.0, 16.5, 0.5);
            pen.circle(14.0, 14.0, 2.5);
            pen.line(&[(15.8, 15.8), (17.5, 17.5)]);
        }
        Close => {
            pen.line(&[(5.0, 5.0), (15.0, 15.0)]);
            pen.line(&[(15.0, 5.0), (5.0, 15.0)]);
        }
        Check => pen.line(&[(4.0, 10.5), (8.5, 15.0), (16.0, 5.5)]),
        Brush => {
            pen.line(&[(16.5, 3.5), (9.0, 11.0)]);
            pen.closed(&[(9.0, 11.0), (6.0, 11.5), (4.0, 14.5), (3.5, 17.0), (6.0, 16.5), (8.5, 14.0)]);
        }
        Linear => {
            for (i, x) in [4.0, 7.5, 11.0, 14.5].iter().enumerate() {
                let a = 1.0 - i as f32 * 0.22;
                pen.p.line_segment([pen.pt(*x, 3.0), pen.pt(*x, 17.0)], Stroke::new(pen.s.width, color.gamma_multiply(a)));
            }
        }
        Radial => {
            pen.circle(10.0, 10.0, 7.0);
            pen.p.circle_stroke(pen.pt(10.0, 10.0), 3.5 * k, Stroke::new(pen.s.width, color.gamma_multiply(0.5)));
        }
        Sky => {
            pen.arc(8.0, 11.0, 3.2, 150.0, 300.0);
            pen.arc(12.0, 9.5, 3.8, 210.0, 360.0);
            pen.line(&[(5.0, 13.5), (15.5, 13.5)]);
            pen.line(&[(2.5, 17.0), (17.5, 17.0)]);
        }
        Subject => {
            pen.circle(10.0, 7.0, 3.0);
            pen.arc(10.0, 18.0, 6.5, 200.0, 340.0);
            pen.rect(2.5, 2.5, 17.5, 17.5, 2.0);
        }
        Play => pen.fill(&[(6.0, 4.0), (6.0, 16.0), (16.0, 10.0)], color),
        Pause => {
            pen.rect_fill(5.0, 4.0, 8.5, 16.0, 0.5);
            pen.rect_fill(11.5, 4.0, 15.0, 16.0, 0.5);
        }
        Picker => {
            pen.line(&[(4.0, 16.0), (11.5, 8.5)]);
            pen.closed(&[(11.0, 5.0), (15.0, 9.0), (17.0, 7.0), (13.0, 3.0)]);
        }
        Rotate => {
            pen.arc(10.0, 10.0, 6.5, 200.0, 470.0);
            pen.line(&[(3.0, 5.5), (4.0, 9.0), (7.5, 8.0)]);
        }
        RotateDrag => {
            // the upper arc of a circle, an arrowhead at each end pointing along it
            pen.arc(10.0, 14.0, 7.5, 200.0, 340.0);
            pen.line(&[(0.8, 9.6), (3.0, 11.9), (6.1, 11.0)]);
            pen.line(&[(19.2, 9.6), (17.0, 11.9), (13.9, 11.0)]);
        }
        Flip => {
            pen.line(&[(10.0, 2.5), (10.0, 17.5)]);
            pen.closed(&[(8.0, 5.0), (8.0, 15.0), (3.0, 15.0)]);
            pen.fill(&[(12.0, 5.0), (12.0, 15.0), (17.0, 15.0)], color);
        }
        Invert => {
            pen.circle(10.0, 10.0, 7.0);
            pen.p.add(Shape::convex_polygon(
                (0..=16)
                    .map(|i| {
                        pen.pt(
                            10.0 + 7.0 * ((90.0 + i as f32 * 180.0 / 16.0f32).to_radians()).cos(),
                            10.0 + 7.0 * ((90.0 + i as f32 * 180.0 / 16.0f32).to_radians()).sin(),
                        )
                    })
                    .collect(),
                color,
                Stroke::NONE,
            ));
        }
        Video => {
            pen.rect(2.5, 5.0, 13.0, 15.0, 1.5);
            pen.closed(&[(13.0, 8.5), (17.5, 6.0), (17.5, 14.0), (13.0, 11.5)]);
        }
        Heart => pen.closed(&[(10.0, 16.5), (3.5, 9.5), (4.0, 5.5), (7.5, 4.0), (10.0, 6.5), (12.5, 4.0), (16.0, 5.5), (16.5, 9.5)]),
        Edited => {
            pen.circle(10.0, 10.0, 7.0);
            pen.line(&[(7.0, 13.0), (13.0, 7.0)]);
        }
        Target => {
            pen.circle(8.0, 10.0, 5.0);
            pen.dot(8.0, 10.0, 1.3);
            pen.line(&[(15.5, 3.5), (15.5, 16.5)]);
            pen.line(&[(13.5, 5.5), (15.5, 3.5), (17.5, 5.5)]);
            pen.line(&[(13.5, 14.5), (15.5, 16.5), (17.5, 14.5)]);
        }
        Chat => {
            // rounded bubble with a tail at the lower left
            pen.closed(&[
                (4.5, 3.5),
                (15.5, 3.5),
                (17.0, 5.0),
                (17.0, 12.0),
                (15.5, 13.5),
                (9.0, 13.5),
                (5.5, 16.8),
                (5.5, 13.5),
                (4.5, 13.5),
                (3.0, 12.0),
                (3.0, 5.0),
            ]);
            for x in [6.8, 10.0, 13.2] {
                pen.dot(x, 8.5, 1.1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_arrowheads_have_both_halves() {
        for icon in [Icon::Back, Icon::Forward] {
            for scale in [1.0, 2.0] {
                let mut view = crate::headless::HeadlessView::new();
                let raw = crate::headless::HeadlessView::raw_input(vec2(64.0, 64.0), scale, 0.0, vec![]);
                view.run(raw, |ui| {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::BLACK);
                    paint(ui.painter(), Rect::from_center_size(pos2(32.0, 32.0), vec2(18.6, 18.6)), icon, Color32::WHITE);
                });
                let image = view.paint(&Default::default());
                let middle = image.pixels.len() / 2;
                let upper: u64 = image.pixels[..middle].iter().map(|p| u64::from(p.r())).sum();
                let lower: u64 = image.pixels[middle..].iter().map(|p| u64::from(p.r())).sum();
                assert!(upper > 0 && lower > 0, "the arrow must render");
                assert!(upper.abs_diff(lower) < upper / 20, "arrowhead must be symmetric at scale {scale}: {upper} above, {lower} below");
            }
        }
    }
}
