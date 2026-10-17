//! The overlay a thumbnail wears, drawn the same way in the justified grid, the square grid and the
//! filmstrip: the stack badge (top left), the virtual-copy tag (top right), the failure mark, the
//! preview-only pill and the marks row along the photo's lower edge (rating, flag, edited, colour
//! label).
//!
//! Everything is laid out against the picture itself, not its cell, and painted clipped to it: on
//! a narrow tile (a portrait photo at the smallest thumbnail size) the marks degrade instead of
//! running over the photo's edge (see [`bar_layout`], [`copy_tag_layout`], [`stack_layout`] and
//! [`preview_pill_place`], pure functions of the room on offer).

use egui::{Align2, Color32, Painter, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::{Flag, Photo, PhotoId, StackId};

use crate::{
    LightcraftApp,
    icons::{Icon, paint},
    theme::Tokens,
    widgets::register,
};

/// Where a thumbnail is drawn: its marks keep the proportions of the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    /// The justified and the square grid.
    Grid,
    /// The strip under the loupe.
    Film,
}

/// The sizes, in points, of a surface's marks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    /// A star's side and the distance between two, and the shared rating drawing of that size (its
    /// side and step are the same numbers).
    star: f32,
    star_pitch: f32,
    rating: crate::rating::RatingSize,
    /// The room a digit takes in a compact rating.
    digit: f32,
    /// The flag's icon, and the room it takes (a gap to what is left of it included).
    flag_icon: f32,
    flag: f32,
    /// The edited mark's icon and room.
    edited_icon: f32,
    edited: f32,
    /// The label dot's radius and room.
    dot: f32,
    label: f32,
    /// The marks row's height, its padding at each end and the gap between its two groups.
    bar: f32,
    pad: f32,
    gap: f32,
    /// How far the corner badges sit from the photo's corner, and how high they are.
    inset: f32,
    badge: f32,
    /// What a stack badge adds to its text (icon and padding), and what a copy tag adds to its
    /// name; a copy tag without its name is `copy_icon` wide.
    stack_extra: f32,
    copy_extra: f32,
    copy_icon: f32,
    /// What the preview pill adds to its text; without its text it is `pill_icon` wide.
    pill_extra: f32,
    pill_icon: f32,
    font: f32,
    micro: f32,
    /// Whether a band darkens the row under any marks (the filmstrip's), not only under the pointer
    /// and on a chosen photo (the grid's).
    band: bool,
}

impl Surface {
    pub(crate) fn metrics(self) -> Metrics {
        match self {
            Surface::Grid => Metrics {
                star: 12.0,
                star_pitch: 13.0,
                rating: crate::rating::RatingSize::Medium,
                digit: 7.0,
                flag_icon: 14.0,
                flag: 18.0,
                edited_icon: 14.0,
                edited: 20.0,
                dot: 5.0,
                label: 14.0,
                bar: 24.0,
                pad: 4.0,
                gap: 6.0,
                inset: 5.0,
                badge: 20.0,
                stack_extra: 30.0,
                copy_extra: 26.0,
                copy_icon: 20.0,
                pill_extra: 24.0,
                pill_icon: 16.0,
                font: 11.0,
                micro: 9.5,
                band: false,
            },
            Surface::Film => Metrics {
                star: 8.0,
                star_pitch: 9.0,
                rating: crate::rating::RatingSize::Small,
                digit: 5.0,
                flag_icon: 10.0,
                flag: 12.0,
                edited_icon: 10.0,
                edited: 14.0,
                dot: 3.5,
                label: 10.0,
                bar: 15.0,
                pad: 3.0,
                gap: 4.0,
                inset: 3.0,
                badge: 15.0,
                stack_extra: 22.0,
                copy_extra: 20.0,
                copy_icon: 16.0,
                pill_extra: 20.0,
                pill_icon: 14.0,
                font: 9.5,
                micro: 8.5,
                band: true,
            },
        }
    }

    /// The widget ids of a surface: the grid's keep their names, the filmstrip's start with `film:`.
    fn widget(self, kind: Mark, id: PhotoId) -> String {
        let (grid, film) = match kind {
            Mark::Stack => ("stack", "film:stack"),
            Mark::Copy => ("copy", "film:copy"),
            Mark::Preview => ("badge:previewOnly", "film:previewOnly"),
            Mark::Failed => ("badge:failed", "film:failed"),
        };
        format!("{}:{}", if self == Surface::Grid { grid } else { film }, id.0)
    }
}

#[derive(Clone, Copy)]
enum Mark {
    Stack,
    Copy,
    Preview,
    Failed,
}

// ------------------------------------------------------------------------ the marks row

/// What a photo carries along its lower edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Marks {
    pub rating: u8,
    pub flag: bool,
    pub edited: bool,
    pub label: bool,
}

impl Marks {
    pub(crate) fn of(photo: &Photo) -> Self {
        Marks { rating: photo.rating.min(5), flag: photo.flag != Flag::None, edited: photo.is_edited(), label: photo.label.is_some() }
    }
}

/// How the rating is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stars {
    #[default]
    None,
    /// One star per point of the rating.
    Row(u8),
    /// One star and the number: "★3".
    Compact(u8),
}

/// Which marks a row draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BarLayout {
    pub stars: Stars,
    pub flag: bool,
    pub edited: bool,
    pub label: bool,
}

impl BarLayout {
    pub(crate) fn is_empty(&self) -> bool {
        *self == BarLayout::default()
    }

    /// The width the row takes, its padding included; 0 for an empty row.
    pub(crate) fn width(&self, m: &Metrics) -> f32 {
        let stars = match self.stars {
            Stars::None => 0.0,
            Stars::Row(n) => f32::from(n) * m.star_pitch,
            Stars::Compact(_) => m.star + 1.0 + m.digit,
        };
        let left = stars + if self.flag { m.flag } else { 0.0 };
        let right = if self.edited { m.edited } else { 0.0 } + if self.label { m.label } else { 0.0 };
        if left + right == 0.0 {
            return 0.0;
        }
        left + right + if left > 0.0 && right > 0.0 { m.gap } else { 0.0 } + 2.0 * m.pad
    }

    fn fits(&self, width: f32, m: &Metrics) -> bool {
        self.width(m) <= width
    }
}

/// The marks a photo `width` points wide has room for, as the tile narrows: first all of them;
/// then without the edited mark; then the stars as one star and the number; then only what fits
/// whole, by importance: the flag, the rating, the label. Where the surface has no surround to
/// tint (`label_first`) the label is the only sign of the label, so it comes first.
pub(crate) fn bar_layout(width: f32, marks: Marks, m: &Metrics, label_first: bool) -> BarLayout {
    let stars = if marks.rating > 0 { Stars::Row(marks.rating) } else { Stars::None };
    let full = BarLayout { stars, flag: marks.flag, edited: marks.edited, label: marks.label };
    if full.fits(width, m) {
        return full;
    }
    let plain = BarLayout { edited: false, ..full };
    if plain.fits(width, m) {
        return plain;
    }
    // (one star is already as small as a rating gets)
    let compact = if marks.rating > 1 { Stars::Compact(marks.rating) } else { stars };
    let squeezed = BarLayout { stars: compact, ..plain };
    if squeezed.fits(width, m) {
        return squeezed;
    }
    #[derive(Clone, Copy)]
    enum Item {
        Flag,
        Stars,
        Label,
    }
    let order = if label_first { [Item::Label, Item::Flag, Item::Stars] } else { [Item::Flag, Item::Stars, Item::Label] };
    let mut out = BarLayout::default();
    for item in order {
        let next = match item {
            Item::Flag if marks.flag => BarLayout { flag: true, ..out },
            Item::Stars if compact != Stars::None => BarLayout { stars: compact, ..out },
            Item::Label if marks.label => BarLayout { label: true, ..out },
            _ => continue,
        };
        if next.fits(width, m) {
            out = next;
        }
    }
    out
}

// ------------------------------------------------------------------------ the corner badges

/// How the virtual-copy tag is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyTag {
    /// The icon and the copy's name.
    Full,
    /// The icon alone.
    IconOnly,
    /// No room: the stack badge, or the photo's edge, is in the way.
    Hidden,
}

/// The copy tag for a photo `width` wide whose name is `name_w` wide, next to a stack badge
/// `stack_w` wide (if it has one): the tag gives up its name before it would run into the badge
/// or over the photo's edge, and then gives up its icon.
pub(crate) fn copy_tag_layout(width: f32, stack_w: Option<f32>, name_w: f32, m: &Metrics) -> CopyTag {
    let room = width - 2.0 * m.inset - stack_w.map_or(0.0, |w| w + m.gap);
    if name_w + m.copy_extra <= room {
        CopyTag::Full
    } else if m.copy_icon <= room {
        CopyTag::IconOnly
    } else {
        CopyTag::Hidden
    }
}

/// Where the preview-only pill goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PillPlace {
    pub rect: Rect,
    /// With its word, else the icon alone.
    pub full: bool,
}

/// Where the preview-only pill goes on `photo`: above the marks row if that is clear, else below
/// the stack badge `stack`, else beside it as the icon alone. `hard` are what it must never cover
/// (the stack badge, the copy tag), `soft` what it may sit over when nothing else is clear (the
/// marks strip, whose marks are drawn at its ends). Order: a spot clear of both, then one clear of
/// `hard`, then, as it changes what editing does and so is never hidden, the icon at the first
/// spot. The word goes before the icon.
pub(crate) fn preview_pill_place(photo: Rect, text_w: f32, stack: Option<Rect>, hard: &[Rect], soft: &[Rect], m: &Metrics) -> PillPlace {
    let h = m.pill_icon;
    let x = photo.left() + m.inset;
    let above = (photo.bottom() - m.bar - 4.0 - h).max(photo.top() + m.inset);
    let mut tries = vec![(pos2(x, above), true), (pos2(x, above), false)];
    if let Some(s) = stack {
        let below = s.bottom() + 3.0;
        tries.extend([(pos2(x, below), true), (pos2(x, below), false), (pos2(s.right() + 3.0, s.top()), false)]);
    }
    let overlap = |a: Rect, b: Rect| a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y;
    let spots: Vec<PillPlace> = tries
        .into_iter()
        .map(|(at, full)| PillPlace { rect: Rect::from_min_size(at, vec2(if full { text_w + m.pill_extra } else { h }, h)), full })
        .filter(|p| {
            let r = p.rect;
            r.left() >= photo.left() && r.right() <= photo.right() - m.inset && r.top() >= photo.top() && r.bottom() <= photo.bottom()
        })
        .collect();
    let clear = |p: &&PillPlace, of: &[&[Rect]]| !of.iter().any(|set| set.iter().any(|o| overlap(p.rect, *o)));
    spots
        .iter()
        .find(|p| clear(p, &[hard, soft]))
        .or_else(|| spots.iter().find(|p| clear(p, &[hard])))
        .copied()
        .unwrap_or(PillPlace { rect: Rect::from_min_size(pos2(x, above), vec2(h, h)), full: false })
}

/// The strip the marks row covers: `None` when it draws nothing (no marks that fit, and no band
/// under the pointer or on a chosen photo), so that nothing else keeps clear of it.
pub(crate) fn marks_extent(rect: Rect, marks: Marks, hot: bool, only_label: bool, label_first: bool, m: &Metrics) -> Option<Rect> {
    let layout = bar_layout(rect.width(), marks, m, label_first);
    (!layout.is_empty() || (!only_label && hot)).then(|| marks_bar(rect, m))
}

/// How the stack badge is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StackShape {
    /// Icon and `i/n` (or the count on a collapsed stack's top).
    Full,
    /// `i/n` without the icon.
    NoIcon,
    /// The count alone.
    CountOnly,
}

/// The stack badge for `room` points (the photo's width less its two insets), whose text is
/// `full_w` wide and its count alone `count_w`: the shape and the width. The count alone is the
/// last shape; when even it is wider than the room the badge is drawn clipped to the photo.
pub(crate) fn stack_layout(room: f32, full_w: f32, count_w: f32, m: &Metrics) -> (StackShape, f32) {
    let pad = 2.0 * m.pad + 2.0;
    let full = full_w + m.stack_extra;
    let bare = full_w + pad;
    if full <= room {
        (StackShape::Full, full)
    } else if bare <= room {
        (StackShape::NoIcon, bare)
    } else {
        (StackShape::CountOnly, count_w + pad)
    }
}

/// The marks row's strip along the photo's lower edge.
fn marks_bar(rect: Rect, m: &Metrics) -> Rect {
    Rect::from_min_max(pos2(rect.left(), (rect.bottom() - m.bar).max(rect.top())), rect.right_bottom())
}

/// The overlay's painter: nothing is painted outside the photo.
fn clipped(ui: &egui::Ui, th: &Thumb) -> Painter {
    let p = ui.painter();
    p.with_clip_rect(th.rect.intersect(p.clip_rect()))
}

// ------------------------------------------------------------------------ texts

/// What a screen reader says of a rating: the number follows its label, so that it reads right in
/// every language whatever its plural forms (no "star"/"stars" suffix to pick).
pub(crate) fn spoken_rating(rating: u8) -> String {
    crate::i18n::tr_format!(", rating {} of 5", rating)
}

/// What the failure mark says on hover: why the photo has no thumbnail, in plain words, where to
/// go from there, and the error itself. (The loupe's pill uses the same two headlines.)
pub(crate) fn failure_text(offline: bool, error: &str) -> String {
    let (head, hint) = if offline {
        (crate::i18n::tr("Original offline · no smart preview"), crate::i18n::tr("Right-click ▸ Locate Missing File… to find it again."))
    } else {
        (crate::i18n::tr("This photo can't be opened"), crate::i18n::tr("Right-click ▸ Reload from Disk to try again."))
    };
    format!("{head}\n{hint}\n{error}")
}

fn stack_tip(collapsed: bool) -> &'static str {
    crate::i18n::tr(if collapsed { "Stack — click to expand (S)" } else { "Stack — click to collapse (S)" })
}

// ------------------------------------------------------------------------ placeholder

/// Where a photo of `aspect` (width / height) sits in `area`, when its thumbnail is not drawn yet.
pub(crate) fn placeholder_rect(area: Rect, aspect: f32) -> Rect {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.5 };
    let (w, h) = if area.width() / aspect <= area.height() { (area.width(), area.width() / aspect) } else { (area.height() * aspect, area.height()) };
    Rect::from_center_size(area.center(), vec2(w, h))
}

/// The picture's stand-in: the theme's skeleton color (firmer than the cell on the light themes).
pub(crate) fn paint_placeholder(p: &Painter, rect: Rect, t: &Tokens) {
    p.rect_filled(rect, 0.0, t.skeleton);
}

// ------------------------------------------------------------------------ the overlay

/// One thumbnail to put the overlay on.
pub(crate) struct Thumb<'a> {
    pub surface: Surface,
    pub photo: &'a Photo,
    /// The picture (or the placeholder standing for it) on screen.
    pub rect: Rect,
    /// The photo's stack and its place in it.
    pub stack: Option<(StackId, usize)>,
    /// Whether the marks row is drawn (Settings → Interface).
    pub marks: bool,
    /// The pointer is over the cell, or it is chosen: the row's band shows even without marks.
    pub hot: bool,
    /// Why the thumbnail could not be drawn, when it could not.
    pub failure: Option<String>,
    /// Whether the surface tints the cell around the photo with its colour label (the square grid
    /// and the filmstrip do; the justified grid has no surround).
    pub surround: bool,
}

/// Draws the overlay; returns the stack badge's response, which covers part of the cell, so that
/// its caller can give it the cell's context menu.
pub(crate) fn overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, th: &Thumb) -> Option<egui::Response> {
    let t = Tokens::get(ui.ctx());
    let m = th.surface.metrics();
    let photo = th.photo;
    let p = clipped(ui, th);
    let rect = th.rect;
    // (a rejected photo is dimmed under its marks, not over them)
    if photo.flag == Flag::Reject {
        p.rect_filled(rect, 0.0, Color32::from_black_alpha(110));
    }
    if let Some(error) = &th.failure {
        failure_mark(app, ui, th, &t, error);
    }
    let bar = if th.marks {
        marks_row(&p, &t, &m, th, false)
    } else if !th.surround && photo.label.is_some() {
        // the setting is about ratings and flags: where no tinted surround says the label, its dot stays
        marks_row(&p, &t, &m, th, true)
    } else {
        None
    };
    let stack = th.stack.and_then(|(sid, pos)| stack_badge(app, ui, th, &t, sid, pos));
    let stack_rect = stack.as_ref().map(|(r, _)| *r);
    let mut hard: Vec<Rect> = stack_rect.into_iter().collect();
    if let Some(name) = &photo.copy_name {
        hard.extend(copy_tag(ui, th, &t, name, stack_rect.map(|r| r.width())));
    }
    if let Some(why) = &photo.preview_only {
        preview_pill(ui, th, &t, why, stack_rect, &hard, bar.as_slice());
    }
    stack.map(|(_, r)| r)
}

fn failure_mark(app: &LightcraftApp, ui: &mut egui::Ui, th: &Thumb, t: &Tokens, error: &str) {
    let id = th.photo.id;
    let side = (th.rect.width().min(th.rect.height()) * 0.4).clamp(12.0, 22.0);
    let icon = Rect::from_center_size(th.rect.center(), vec2(side, side));
    paint(&clipped(ui, th), icon, Icon::Warning, t.caution);
    let target = icon.expand(4.0).intersect(th.rect);
    register(ui, th.surface.widget(Mark::Failed, id), target);
    let offline = matches!(&th.photo.source, lightcraft_catalog::Source::File { path } if app.session.media.availability.is_offline(path));
    let text = failure_text(offline, error);
    let resp = ui.interact(target, egui::Id::unique((th.surface, "thumb-failed", id.0)), Sense::hover()).on_hover_text(&text);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text.replace('\n', ": ")));
}

/// Draws the marks row; returns the strip it covers, if it drew anything.
fn marks_row(p: &Painter, t: &Tokens, m: &Metrics, th: &Thumb, only_label: bool) -> Option<Rect> {
    let (photo, rect) = (th.photo, th.rect);
    let marks = if only_label { Marks { label: true, ..Marks::default() } } else { Marks::of(photo) };
    let layout = bar_layout(rect.width(), marks, m, !th.surround);
    let bar = marks_extent(rect, marks, th.hot, only_label, !th.surround, m)?;
    if !only_label && (th.hot || m.band) {
        p.rect_filled(bar, 0.0, Color32::from_black_alpha(120));
    }
    let y = bar.center().y;
    let mut x = bar.left() + m.pad;
    match layout.stars {
        Stars::None => {}
        Stars::Row(n) => {
            x += crate::rating::paint_rating(p, pos2(x, y), n, m.rating, t.star);
        }
        Stars::Compact(n) => {
            paint(p, Rect::from_min_size(pos2(x, y - m.star / 2.0), vec2(m.star, m.star)), Icon::StarFilled, t.star);
            p.text(pos2(x + m.star + 1.0, y), Align2::LEFT_CENTER, n.to_string(), t.semibold(m.font), t.star);
            x += m.star + 1.0 + m.digit;
        }
    }
    if layout.flag {
        let at = Rect::from_min_size(pos2(x + m.flag - m.flag_icon, y - m.flag_icon / 2.0), vec2(m.flag_icon, m.flag_icon));
        match photo.flag {
            Flag::Pick => paint(p, at, Icon::FlagPickFilled, t.pick),
            Flag::Reject => paint(p, at, Icon::FlagReject, t.reject),
            Flag::None => {}
        }
    }
    let mut right = bar.right() - m.pad;
    if layout.label
        && let Some(l) = photo.label
    {
        p.circle(pos2(right - m.label / 2.0, y), m.dot, crate::theme::label_color(l), Stroke::new(1.0, Color32::from_black_alpha(120)));
        right -= m.label;
    }
    if layout.edited {
        let at = Rect::from_center_size(pos2(right - m.edited / 2.0, y), vec2(m.edited_icon, m.edited_icon));
        paint(p, at, Icon::Sliders, t.text_label);
    }
    Some(bar)
}

/// The stack badge at the photo's top left: the number of photos on a collapsed stack's top,
/// `i/n` on the members of an expanded one; a click collapses or expands the stack. It gives up
/// its icon, then the `i/` of `i/n`, on a narrow photo, and is never painted outside the photo.
/// Returns the badge's rectangle (as laid out, before clipping) and its response.
fn stack_badge(app: &mut LightcraftApp, ui: &mut egui::Ui, th: &Thumb, t: &Tokens, sid: StackId, pos: usize) -> Option<(Rect, egui::Response)> {
    let m = th.surface.metrics();
    let st = app.session.catalog.stack(sid)?;
    let (n, collapsed) = (st.photos.len(), st.collapsed);
    let id = th.photo.id;
    let p = clipped(ui, th);
    let count = p.layout_no_wrap(n.to_string(), t.semibold(m.font), Color32::WHITE);
    let full = if collapsed { count.clone() } else { p.layout_no_wrap(format!("{}/{n}", pos + 1), t.semibold(m.font), Color32::WHITE) };
    let (shape, w) = stack_layout(th.rect.width() - 2.0 * m.inset, full.size().x, count.size().x, &m);
    let br = Rect::from_min_size(th.rect.min + vec2(m.inset, m.inset), vec2(w, m.badge));
    let hit = br.intersect(th.rect);
    let resp = ui.interact(hit, egui::Id::unique((th.surface, "stack-badge", id.0)), Sense::click()).on_hover_text(stack_tip(collapsed));
    register(ui, th.surface.widget(Mark::Stack, id), hit);
    let fill = if resp.hovered() { Color32::from_black_alpha(220) } else { Color32::from_black_alpha(165) };
    p.rect_filled(br, m.badge / 2.0, fill);
    if pos == 0 && !collapsed {
        p.rect_stroke(br, m.badge / 2.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    }
    let text = if shape == StackShape::CountOnly { count } else { full };
    match shape {
        StackShape::Full => {
            let icon = m.badge - 6.0;
            paint(&p, Rect::from_min_size(br.min + vec2(m.badge * 0.3, 3.0), vec2(icon, icon)), Icon::Stack, Color32::WHITE);
            p.galley(pos2(br.min.x + m.stack_extra - 6.0, br.center().y - text.size().y / 2.0), text, Color32::WHITE);
        }
        StackShape::NoIcon | StackShape::CountOnly => {
            p.galley(br.center() - text.size() / 2.0, text, Color32::WHITE);
        }
    }
    // a double-click (Lightroom's way) toggles once: its second click would undo the first
    if resp.clicked() && !resp.double_clicked() {
        crate::panels::grid::toggle_stack(app, ui.ctx(), id);
    }
    Some((br, resp))
}

/// The virtual-copy tag at the photo's top right, so that a copy and its master tell apart.
/// Returns where it is, when it is drawn.
fn copy_tag(ui: &mut egui::Ui, th: &Thumb, t: &Tokens, name: &str, stack_w: Option<f32>) -> Option<Rect> {
    let m = th.surface.metrics();
    let p = clipped(ui, th);
    let g = p.layout_no_wrap(name.to_string(), t.semibold(m.micro + 1.0), Color32::WHITE);
    let mode = copy_tag_layout(th.rect.width(), stack_w, g.size().x, &m);
    let w = match mode {
        CopyTag::Full => g.size().x + m.copy_extra,
        CopyTag::IconOnly => m.copy_icon,
        CopyTag::Hidden => return None,
    };
    let br = Rect::from_min_size(pos2(th.rect.right() - m.inset - w, th.rect.top() + m.inset), vec2(w, m.badge));
    p.rect_filled(br, m.badge / 2.0, Color32::from_black_alpha(165));
    let icon = m.badge * 0.55;
    let at = if mode == CopyTag::Full { pos2(br.left() + m.badge * 0.65, br.center().y) } else { br.center() };
    paint(&p, Rect::from_center_size(at, vec2(icon, icon)), Icon::VirtualCopy, Color32::WHITE);
    if mode == CopyTag::Full {
        p.galley(pos2(br.left() + m.copy_extra - 4.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    let id = th.photo.id;
    register(ui, th.surface.widget(Mark::Copy, id), br);
    let tip = format!("{}: {name}", crate::i18n::tr("Virtual copy"));
    ui.interact(br, egui::Id::unique((th.surface, "copy-tag", id.0)), Sense::hover())
        .on_hover_text(&tip)
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &tip));
    Some(br)
}

/// A raw shown from its embedded JPEG: a small amber "Preview" pill above the marks row, always
/// shown (it changes what editing does) wherever it fits clear of the other marks.
fn preview_pill(ui: &mut egui::Ui, th: &Thumb, t: &Tokens, why: &str, stack: Option<Rect>, hard: &[Rect], soft: &[Rect]) {
    let m = th.surface.metrics();
    let p = clipped(ui, th);
    let g = p.layout_no_wrap(crate::i18n::tr("Preview").into(), t.semibold(m.micro), Color32::WHITE);
    let place = preview_pill_place(th.rect, g.size().x, stack, hard, soft, &m);
    let (br, full, h) = (place.rect, place.full, place.rect.height());
    p.rect_filled(br, h / 2.0, Color32::from_black_alpha(170));
    let icon = h - 4.0;
    let centre = if full { pos2(br.left() + h * 0.55, br.center().y) } else { br.center() };
    paint(&p, Rect::from_center_size(centre, vec2(icon, icon)), Icon::Info, t.caution);
    if full {
        p.galley(pos2(br.left() + h * 0.55 + icon / 2.0 + 2.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    let id = th.photo.id;
    register(ui, th.surface.widget(Mark::Preview, id), br);
    ui.interact(br, egui::Id::unique((th.surface, "preview-only-badge", id.0)), Sense::hover())
        .on_hover_text(crate::i18n::tr_format!("Preview only — {}", crate::widgets::preview_only_explanation(why)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_marks() -> Vec<Marks> {
        let mut out = Vec::new();
        for rating in 0..=5 {
            for bits in 0..8u8 {
                out.push(Marks { rating, flag: bits & 1 != 0, edited: bits & 2 != 0, label: bits & 4 != 0 });
            }
        }
        out
    }

    #[test]
    fn a_wide_tile_draws_every_mark() {
        let m = Surface::Grid.metrics();
        let marks = Marks { rating: 5, flag: true, edited: true, label: true };
        let l = bar_layout(300.0, marks, &m, false);
        assert_eq!(l, BarLayout { stars: Stars::Row(5), flag: true, edited: true, label: true });
    }

    /// As the photo narrows, the edited mark goes first, then the stars shrink to one star and the
    /// number, and the rest is kept only as far as it fits whole.
    #[test]
    fn marks_degrade_in_order_as_the_tile_narrows() {
        for surface in [Surface::Grid, Surface::Film] {
            let m = surface.metrics();
            let marks = Marks { rating: 5, flag: true, edited: true, label: true };
            let full = bar_layout(1000.0, marks, &m, false);
            let w_full = full.width(&m);
            let no_edit = bar_layout(w_full - 0.5, marks, &m, false);
            assert_eq!(no_edit, BarLayout { edited: false, ..full }, "{surface:?}: the edited mark goes first");
            let w_plain = no_edit.width(&m);
            let compact = bar_layout(w_plain - 0.5, marks, &m, false);
            assert_eq!(compact.stars, Stars::Compact(5), "{surface:?}: then the stars collapse");
            assert!(compact.flag && compact.label && !compact.edited, "{surface:?}: and nothing else is lost yet: {compact:?}");
            let w_compact = compact.width(&m);
            let fewer = bar_layout(w_compact - 0.5, marks, &m, false);
            assert!(fewer.width(&m) <= w_compact - 0.5, "{surface:?}: what is drawn fits");
            assert!(fewer.flag, "{surface:?}: the flag outlasts the label: {fewer:?}");
            assert!(!fewer.label, "{surface:?}: the label is the first to go: {fewer:?}");
        }
    }

    /// Whatever is drawn is drawn whole: the row never needs more than the photo's width.
    #[test]
    fn the_marks_row_never_outgrows_the_photo() {
        for surface in [Surface::Grid, Surface::Film] {
            let m = surface.metrics();
            for marks in all_marks() {
                for w in 0..400 {
                    let first = bar_layout(w as f32, marks, &m, true);
                    assert!(first.width(&m) <= w as f32, "{surface:?} {marks:?} at {w} (label first): {first:?}");
                    let l = bar_layout(w as f32, marks, &m, false);
                    assert!(l.width(&m) <= w as f32, "{surface:?} {marks:?} at {w}: {l:?} needs {}", l.width(&m));
                    if l.edited {
                        assert!(marks.edited && l == bar_layout(1000.0, marks, &m, false), "the edited mark only shows alongside all the others");
                    }
                }
            }
        }
    }

    /// A portrait photo at the smallest thumbnail size with five stars and a flag: the case that
    /// ran over the photo's edge.
    #[test]
    fn a_narrow_portrait_with_five_stars_and_a_flag_keeps_what_fits() {
        let m = Surface::Grid.metrics();
        let marks = Marks { rating: 5, flag: true, edited: false, label: false };
        // justified rows 90 pt high: a 2:3 portrait is 60 wide
        let l = bar_layout(60.0, marks, &m, false);
        assert_eq!(l, BarLayout { stars: Stars::Compact(5), flag: true, edited: false, label: false });
        // a square cell 90 pt wide leaves a 70 × 56 area for the photo: 37 wide
        let narrow = bar_layout(56.0 * 2.0 / 3.0, marks, &m, false);
        assert!(narrow.width(&m) <= 56.0 * 2.0 / 3.0 && narrow.flag, "{narrow:?}");
        assert_ne!(narrow.stars, Stars::Row(5), "five whole stars (65 pt) cannot fit 37");
    }

    #[test]
    fn one_star_stays_one_star() {
        let m = Surface::Grid.metrics();
        let marks = Marks { rating: 1, flag: false, edited: false, label: false };
        assert_eq!(bar_layout(m.star_pitch + 2.0 * m.pad, marks, &m, false).stars, Stars::Row(1));
    }

    #[test]
    fn the_copy_tag_gives_up_its_name_then_its_icon_before_it_meets_the_stack_badge() {
        for surface in [Surface::Grid, Surface::Film] {
            let m = surface.metrics();
            let (stack_w, name_w) = (44.0, 50.0);
            let mut seen = Vec::new();
            for w in (0..300).step_by(1) {
                let width = w as f32;
                let mode = copy_tag_layout(width, Some(stack_w), name_w, &m);
                let tag_w = match mode {
                    CopyTag::Full => name_w + m.copy_extra,
                    CopyTag::IconOnly => m.copy_icon,
                    CopyTag::Hidden => 0.0,
                };
                if mode != CopyTag::Hidden {
                    // stack badge from the left inset, the tag to the right inset: they leave a gap
                    let (stack_right, tag_left) = (m.inset + stack_w, width - m.inset - tag_w);
                    assert!(stack_right < tag_left, "{surface:?} at {width}: {mode:?} meets the badge");
                    assert!(tag_left >= m.inset, "{surface:?} at {width}: {mode:?} leaves the photo");
                }
                if seen.last() != Some(&mode) {
                    seen.push(mode);
                }
            }
            assert_eq!(seen, [CopyTag::Hidden, CopyTag::IconOnly, CopyTag::Full], "{surface:?}: as the tile widens");
        }
    }

    #[test]
    fn without_a_stack_badge_the_copy_tag_keeps_its_name_longer() {
        let m = Surface::Grid.metrics();
        let width = 2.0 * m.inset + 50.0 + m.copy_extra + 1.0;
        assert_eq!(copy_tag_layout(width, None, 50.0, &m), CopyTag::Full);
        assert_eq!(copy_tag_layout(width, Some(30.0), 50.0, &m), CopyTag::IconOnly);
    }

    #[test]
    fn the_preview_pill_shrinks_to_its_icon_on_a_narrow_tile() {
        let m = Surface::Grid.metrics();
        let photo = |w: f32| Rect::from_min_size(pos2(0.0, 0.0), vec2(w, 80.0));
        assert!(preview_pill_place(photo(120.0), 40.0, None, &[], &[], &m).full);
        let narrow = preview_pill_place(photo(60.0), 40.0, None, &[], &[], &m);
        assert!(!narrow.full);
    }

    /// A wide stacked photo in the filmstrip, a 3:2 one in the square grid at 90 pt: the pill's
    /// usual place meets the stack badge, so it moves below it, beside it, or goes.
    #[test]
    fn the_preview_pill_keeps_clear_of_the_stack_badge() {
        for (surface, w, h, badge_w) in [(Surface::Film, 100.0, 50.0, 30.0), (Surface::Grid, 70.0, 47.0, 40.0)] {
            let m = surface.metrics();
            let photo = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
            let badge = Rect::from_min_size(pos2(m.inset, m.inset), vec2(badge_w, m.badge));
            let bar = marks_bar(photo, &m);
            let p = preview_pill_place(photo, 30.0, Some(badge), &[badge], &[bar], &m);
            let free = |r: Rect| !(r.min.x < badge.max.x && badge.min.x < r.max.x && r.min.y < badge.max.y && badge.min.y < r.max.y);
            assert!(
                free(p.rect) && photo.contains_rect(p.rect) && p.rect.bottom() <= bar.top() + 0.01,
                "{surface:?}: {p:?} meets the badge {badge:?}"
            );
            // without the badge it sits where it always did
            let alone = preview_pill_place(photo, 30.0, None, &[], &[bar], &m);
            assert_eq!(alone.rect.top(), (h - m.bar - 4.0 - m.pill_icon).max(m.inset), "{surface:?}");
        }
        // a photo too small for the pill anywhere clear of the badge: nothing else fits, so the
        // icon is drawn at its first spot, over the badge, rather than not at all
        let m = Surface::Film.metrics();
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 30.0));
        let badge = Rect::from_min_size(pos2(m.inset, m.inset), vec2(34.0, m.badge));
        let last = preview_pill_place(tiny, 30.0, Some(badge), &[badge], &[marks_bar(tiny, &m)], &m);
        assert!(!last.full, "the icon alone");
        assert_eq!(last.rect.min, pos2(m.inset, (tiny.bottom() - m.bar - 4.0 - m.pill_icon).max(m.inset)), "at its first spot");
        assert!(last.rect.intersects(badge), "over the badge: the last resort");
    }

    /// A short photo with a collapsed stack's badge and a marks band (hovered, or rated): the spot
    /// beside the badge overlaps the band, which is allowed (its marks sit at its ends), the badge
    /// is not.
    #[test]
    fn the_pill_may_sit_over_the_marks_band_but_never_over_the_badge() {
        let m = Surface::Grid.metrics();
        let photo = Rect::from_min_size(pos2(0.0, 0.0), vec2(70.0, 39.0));
        let badge = Rect::from_min_size(pos2(m.inset, m.inset), vec2(34.0, m.badge));
        let band = marks_bar(photo, &m);
        let p = preview_pill_place(photo, 30.0, Some(badge), &[badge], &[band], &m);
        let overlap = |a: Rect, b: Rect| a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y;
        assert!(!overlap(p.rect, badge), "{p:?} is over the badge {badge:?}");
        assert!(photo.contains_rect(p.rect), "{p:?}");
        assert!(p.rect.left() >= badge.right(), "beside the badge: {p:?}");
    }

    /// A square-grid 16:9 photo at 90 pt is 70 x 39: with no marks to draw the row covers nothing,
    /// and the pill sits where it always did instead of dodging a strip that is not there.
    #[test]
    fn a_short_photo_without_marks_leaves_the_pill_its_place() {
        for (surface, w, h) in [(Surface::Grid, 70.0, 39.0), (Surface::Film, 100.0, 30.0)] {
            let m = surface.metrics();
            let photo = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
            let bar = marks_extent(photo, Marks::default(), false, false, false, &m);
            assert_eq!(bar, None, "{surface:?}: nothing drawn, nothing to avoid");
            let obstacles: Vec<Rect> = bar.into_iter().collect();
            let place = preview_pill_place(photo, 30.0, None, &[], &obstacles, &m);
            assert!(photo.contains_rect(place.rect) && place.full, "{surface:?}: {place:?}");
            // under the pointer the band is there, and the row covers its strip
            assert_eq!(marks_extent(photo, Marks::default(), true, false, false, &m), Some(marks_bar(photo, &m)));
        }
    }

    /// "12/120" on a 60 pt photo, "3/12" on a 42 pt filmstrip photo: the badge sheds its icon, then
    /// the `i/`, and what is left is drawn clipped to the photo rather than onto its neighbour.
    #[test]
    fn the_stack_badge_degrades_on_a_narrow_photo() {
        for surface in [Surface::Grid, Surface::Film] {
            let m = surface.metrics();
            let (full_w, count_w) = (34.0, 20.0);
            let room = |w: f32| w - 2.0 * m.inset;
            let (s, w) = stack_layout(room(300.0), full_w, count_w, &m);
            assert_eq!((s, w), (StackShape::Full, full_w + m.stack_extra));
            let (s, w) = stack_layout(room(full_w + m.stack_extra + 2.0 * m.inset - 1.0), full_w, count_w, &m);
            assert_eq!(s, StackShape::NoIcon, "{surface:?}: the icon goes first");
            assert!(w <= full_w + m.stack_extra);
            let (s, w) = stack_layout(room(w + 2.0 * m.inset - 1.0), full_w, count_w, &m);
            assert_eq!(s, StackShape::CountOnly, "{surface:?}: then the i/");
            assert!(w < full_w);
            // nothing fits: still the count, and the caller clips it
            assert_eq!(stack_layout(0.0, full_w, count_w, &m).0, StackShape::CountOnly);
        }
    }

    /// Where the label has no surround to say it, it outlasts the stars and the flag.
    #[test]
    fn on_a_surfaceless_tile_the_label_is_kept_before_the_stars() {
        let m = Surface::Grid.metrics();
        let marks = Marks { rating: 5, flag: true, edited: false, label: true };
        let with_surround = bar_layout(60.0, marks, &m, false);
        assert!(!with_surround.label, "{with_surround:?}");
        let without = bar_layout(60.0, marks, &m, true);
        assert!(without.label && without.width(&m) <= 60.0, "{without:?}");
    }

    #[test]
    fn the_placeholder_has_the_photo_s_shape() {
        let area = Rect::from_min_size(pos2(10.0, 10.0), vec2(70.0, 56.0));
        let portrait = placeholder_rect(area, 2.0 / 3.0);
        assert!(
            (portrait.width() / portrait.height() - 2.0 / 3.0).abs() < 1e-4 && area.contains_rect(portrait) && portrait.center() == area.center()
        );
        let wide = placeholder_rect(area, 3.0);
        assert!((wide.width() - 70.0).abs() < 1e-4 && wide.height() < 56.0);
        let unknown = placeholder_rect(area, f32::NAN);
        assert!((unknown.width() / unknown.height() - 1.5).abs() < 1e-4);
    }

    /// Italian says "valutazione: 1 su 5", "valutazione: 2 su 5"…: no English plural suffix to
    /// pick, so no number reads wrong.
    #[test]
    fn a_spoken_rating_needs_no_plural_suffix() {
        crate::i18n::set_language(crate::i18n::Locale::It);
        let said: Vec<String> = (1..=5).map(spoken_rating).collect();
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert!(said.iter().all(|s| !s.contains("star")), "{said:?}");
        assert_eq!(said[1], said[0].replacen('1', "2", 1), "only the number changes: {said:?}");
        assert_eq!(spoken_rating(1), ", rating 1 of 5");
    }

    #[test]
    fn a_missing_original_points_at_locate_and_anything_else_at_reload() {
        let missing = failure_text(true, "No such file");
        assert!(missing.contains("Original offline") && missing.contains("Locate Missing File") && missing.ends_with("No such file"), "{missing}");
        let broken = failure_text(false, "bad header");
        assert!(broken.contains("can't be opened") && broken.contains("Reload from Disk") && !broken.contains("Locate"), "{broken}");
    }
}
