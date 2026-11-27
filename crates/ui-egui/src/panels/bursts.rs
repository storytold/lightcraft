//! Window ▸ Bursts: Assisted Culling's bursts among the photos in view (with the hidden members of
//! their collapsed stacks), each a row of thumbnails with its pick first, to put right by hand.
//!
//! Drag a photo to another place in its burst (to the front, and it becomes the pick), into
//! another burst, onto *New burst* or onto *Not in a burst*; drag a burst by its title onto
//! another to merge them. Photos dragged from the grid drop here too. A drag is the grid's photo
//! drag ([`crate::state::UiState::dragging_photos`]: its label, Esc and release), and a drop is one
//! `photo.moveToBurst`, so one undo step.
//!
//! Widgets: `burst:{n}` (a burst's title, `n` its number), `burstPhoto:{id}`, `burstPick:{n}`,
//! `burst:new`, `burst:none`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::{Flag, PhotoId};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Thumbnail cells: side, gap, row inset.
const CELL: f32 = 68.0;
const GAP: f32 = 6.0;
const INSET: f32 = 20.0;
/// A burst's title row.
const TITLE_H: f32 = 30.0;

/// The bursts shown, by the view's generation (which follows every catalog change).
pub type Shown = Arc<Vec<(u32, Vec<PhotoId>)>>;

/// The bursts among the photos in view and the hidden members of their stacks, in view order.
fn bursts(app: &mut LightcraftApp) -> Shown {
    let (generation, ids) = app.session.visible_shared();
    if let Some((g, b)) = &app.caches.bursts
        && *g == generation
    {
        return b.clone();
    }
    let cat = &app.session.catalog;
    // each photo in view, then the hidden members of its stack: a burst whose photos are all
    // hidden in another burst's collapsed stack is listed too, after that stack's top
    let mut anchors: Vec<PhotoId> = Vec::with_capacity(ids.len());
    for id in ids.iter() {
        anchors.push(*id);
        if let Some(st) = cat.stack_of(*id) {
            anchors.extend(st.photos.iter().copied().filter(|p| p != id));
        }
    }
    let scope: HashSet<PhotoId> = anchors.iter().copied().collect();
    let b: Shown = Arc::new(cat.bursts_among(&anchors, Some(&scope)));
    app.caches.bursts = Some((generation, b.clone()));
    b
}

/// Every burst of the library: its number → its photos, pick first.
pub type All = Arc<HashMap<u32, Vec<PhotoId>>>;

/// Every burst of the library, built once per view generation (it follows every catalog change).
fn all_bursts(app: &mut LightcraftApp) -> All {
    let (generation, _) = app.session.visible_shared();
    if let Some((g, b)) = &app.caches.burst_index
        && *g == generation
    {
        return b.clone();
    }
    let b: All = Arc::new(app.session.catalog.burst_index());
    app.caches.burst_index = Some((generation, b.clone()));
    b
}

/// A [`Target::Burst`] slot after every photo of the burst: a drop on its title or after its
/// last photo.
const LAST: usize = usize::MAX;

/// Where a drop under the pointer would put the dragged photos.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    /// Burst `n` at `slot` (a place among its photos as shown, the dragged ones included;
    /// [`LAST`]: after every photo of the burst, those out of view too), dropped on the left half
    /// of the photo at `slot` (`true`: right before it) or the right half of the one before.
    Burst(u32, usize, bool),
    New,
    Nowhere,
}

/// Where the pointer is over the panel, and what a drop there would do (with the line marking it).
type Drop = Option<(Target, Option<Rect>)>;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::ScrollArea::vertical().id_salt("bursts-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        super::left::drag_auto_scroll(app, ui);
        super::right::header(ui, "Bursts");
        let shown = bursts(app);
        let dragged: Option<Vec<PhotoId>> = app.ui.dragging_photos.as_ref().map(|v| v.iter().copied().map(PhotoId).collect());
        let pointer = ui.input(|i| i.pointer.latest_pos()).filter(|p| ui.clip_rect().contains(*p));
        let mut target: Drop = None;
        let width = ui.available_width();
        let explain = if shown.is_empty() {
            "Photo → Assisted Culling… groups look-alike shots into bursts. They show here to put right by hand."
        } else {
            "Drag a photo to its place: the first of a burst is its pick. Drag one into another burst, or a burst by its title onto another to merge them."
        };
        egui::Frame::NONE.inner_margin(egui::Margin { left: INSET as i8, right: INSET as i8, top: 0, bottom: 10 }).show(ui, |ui| {
            ui.set_width(width - 2.0 * INSET);
            ui.label(egui::RichText::new(crate::i18n::tr(explain)).color(t.text_dim).size(12.0));
        });
        for (k, (g, photos)) in shown.iter().enumerate() {
            burst_rows(app, ui, &t, (k, *g, photos), pointer, dragged.is_some(), &mut target);
        }
        // drop places for a new burst and for none: there while there are bursts, or a drag
        if !shown.is_empty() || dragged.is_some() {
            drop_zones(ui, &t, pointer, dragged.is_some(), &mut target);
        }
        if let Some(ids) = dragged
            && let Some((tgt, mark)) = target
        {
            drop_on(app, ui, &shown, &ids, tgt, mark);
        }
        ui.add_space(12.0);
    });
}

/// Burst `g` (the `k`th shown): its title, then its photos in rows. Sets `target` when the pointer
/// is over it.
fn burst_rows(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    (k, g, photos): (usize, u32, &[PhotoId]),
    pointer: Option<egui::Pos2>,
    dragging: bool,
    target: &mut Drop,
) {
    let width = ui.available_width();
    let cols = (((width - 2.0 * INSET + GAP) / (CELL + GAP)).floor() as usize).max(1);
    let title = burst_title(app, ui, t, (k, g, photos.len()), pointer, dragging, target);
    let n = photos.len();
    // the photos, in rows
    let rows = n.div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * (CELL + GAP) + GAP), Sense::hover());
    let cell_at = |i: usize| {
        let (r, c) = (i / cols, i % cols);
        Rect::from_min_size(pos2(area.left() + INSET + c as f32 * (CELL + GAP), area.top() + r as f32 * (CELL + GAP)), vec2(CELL, CELL))
    };
    for (i, id) in photos.iter().enumerate() {
        let r = cell_at(i);
        if let Some(badge) = photo_cell(app, ui, *id, r) {
            register(ui, format!("burstPick:{g}"), badge);
        }
        // a drop on a photo's left half goes before it, on its right half after it
        if let Some(p) = pointer
            && r.expand(GAP / 2.0).contains(p)
        {
            let slot = if p.x < r.center().x { i } else { i + 1 };
            let edge = if slot == i { r.left() - GAP / 2.0 } else { r.right() + GAP / 2.0 };
            *target = Some((Target::Burst(g, slot, slot == i), Some(Rect::from_min_max(pos2(edge - 1.5, r.top()), pos2(edge + 1.5, r.bottom())))));
        }
    }
    if target.is_none()
        && let Some(p) = pointer
        && area.contains(p)
    {
        // the room after the last photo: last
        let last = cell_at(n.saturating_sub(1));
        *target = Some((
            Target::Burst(g, LAST, false),
            Some(Rect::from_min_max(pos2(last.right() + GAP / 2.0 - 1.5, last.top()), pos2(last.right() + GAP / 2.0 + 1.5, last.bottom()))),
        ));
    }
    if dragging && matches!(target, Some((Target::Burst(tg, ..), _)) if *tg == g) {
        ui.painter().rect_stroke(title.union(area).shrink2(vec2(8.0, 1.0)), 4.0, Stroke::new(1.0, t.accent.gamma_multiply(0.6)), StrokeKind::Inside);
    }
}

/// Burst `g`'s title (the `k`th shown, of `n` photos): a drop there puts the photos last;
/// dragged, it takes the burst along.
fn burst_title(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    (k, g, n): (usize, u32, usize),
    pointer: Option<egui::Pos2>,
    dragging: bool,
    target: &mut Drop,
) -> Rect {
    let width = ui.available_width();
    let (title, resp) = ui.allocate_exact_size(vec2(width, TITLE_H), Sense::click_and_drag());
    register(ui, format!("burst:{g}"), title);
    let p = ui.painter();
    let name = format!("{} {}", crate::i18n::tr("Burst"), k + 1);
    p.text(pos2(title.left() + INSET, title.center().y), Align2::LEFT_CENTER, name, t.semibold(12.5), t.text);
    let count = crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n);
    p.text(pos2(title.right() - INSET, title.center().y), Align2::RIGHT_CENTER, count, t.font(11.5), t.text_dim);
    if resp.hovered() && !dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    // the whole burst, also its photos out of view
    if resp.drag_started() {
        app.ui.dragging_photos = Some(all_bursts(app).get(&g).map(|m| m.iter().map(|p| p.0).collect()).unwrap_or_default());
    }
    if pointer.is_some_and(|p| title.contains(p)) {
        *target = Some((Target::Burst(g, LAST, false), None));
    }
    title
}

/// The drop places for a new burst and for none. Sets `target` when a drag is over one.
fn drop_zones(ui: &mut egui::Ui, t: &Tokens, pointer: Option<egui::Pos2>, dragging: bool, target: &mut Drop) {
    let width = ui.available_width();
    ui.add_space(8.0);
    for (key, label, tgt) in [("burst:new", "New burst", Target::New), ("burst:none", "Not in a burst", Target::Nowhere)] {
        let (row, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
        let zone = row.shrink2(vec2(INSET, 4.0));
        register(ui, key, zone);
        let over = dragging && pointer.is_some_and(|p| zone.contains(p));
        let stroke = if over { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.divider) };
        ui.painter().rect_stroke(zone, 6.0, stroke, StrokeKind::Inside);
        ui.painter().text(zone.center(), Align2::CENTER_CENTER, crate::i18n::tr(label), t.font(12.0), if over { t.text } else { t.text_dim });
        if over {
            *target = Some((tgt, None));
        }
    }
}

/// One photo of a burst: its thumbnail, the pick's badge (returned), the active photo's outline;
/// a click selects it, a drag takes it along.
fn photo_cell(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, r: Rect) -> Option<Rect> {
    let t = Tokens::get(ui.ctx());
    let photo = app.session.catalog.photo(id).cloned()?;
    let pick = photo.analysis.is_some_and(|a| a.best);
    let resp = ui.interact(r, egui::Id::unique(("burst-photo", id.0)), Sense::click_and_drag());
    register(ui, format!("burstPhoto:{}", id.0), r);
    // Any thumbnail will do at this size: one is asked for only when there is none, so the grid's
    // larger ones are never asked again at this size (and back) every frame. Above the grid's own
    // priorities: a member hidden in a collapsed stack isn't in the grid, which evicts the rest.
    if app.renderer.thumb(id).is_none() && !app.renderer.is_pending(crate::render::Slot::Thumb(id)) {
        let px = (CELL * ui.ctx().pixels_per_point()).ceil() as usize;
        super::grid::request_thumb(app, id, px, 12);
    }
    let p = ui.painter();
    p.rect_filled(r, 3.0, Color32::from_gray(30));
    let shown = match app.renderer.thumb(id) {
        Some(tex) => {
            let fit = super::detail::fit_texture_rect(r, tex.size);
            p.image(tex.tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            fit
        }
        None => r,
    };
    if photo.flag == Flag::Reject {
        p.rect_filled(shown, 0.0, Color32::from_black_alpha(110));
    }
    let state = app.session.selection.state_of(id);
    if state == lightcraft_engine::SelectionState::Active {
        p.rect_stroke(shown, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
    } else if state != lightcraft_engine::SelectionState::NotSelected {
        p.rect_stroke(shown, 0.0, Stroke::new(2.0, Color32::from_gray(170)), StrokeKind::Outside);
    }
    let badge = pick.then(|| {
        let g = p.layout_no_wrap(crate::i18n::tr("Pick").into(), t.semibold(9.5), Color32::WHITE);
        let badge = Rect::from_min_size(r.min + vec2(3.0, 3.0), vec2(g.size().x + 10.0, 14.0));
        p.rect_filled(badge, 7.0, t.accent);
        p.galley(pos2(badge.left() + 5.0, badge.center().y - g.size().y / 2.0), g, Color32::WHITE);
        badge
    });
    if photo.flag == Flag::Pick {
        crate::icons::paint(
            p,
            Rect::from_min_size(pos2(r.right() - 16.0, r.bottom() - 16.0), vec2(13.0, 13.0)),
            crate::icons::Icon::FlagPick,
            t.pick,
        );
    }
    let focus = photo.analysis.map(|a| a.sharpness.round() as i32).unwrap_or_default();
    let resp = resp.on_hover_text(format!("{} · {} {focus}", photo.file_name, crate::i18n::tr("Focus")));
    if resp.clicked() {
        let _ = app.run("library.select", json!({"ids": [id.0]}));
    }
    if resp.drag_started() {
        app.ui.dragging_photos = Some(vec![id.0]);
    }
    badge
}

/// Photos dragged over `tgt`: the mark where they would go, the label saying what a drop does,
/// and on release the move.
fn drop_on(app: &mut LightcraftApp, ui: &mut egui::Ui, shown: &Shown, ids: &[PhotoId], tgt: Target, mark: Option<Rect>) {
    let t = Tokens::get(ui.ctx());
    let released = ui.input(|i| i.pointer.any_released());
    // only photos culling scored can be in a burst
    if ids.iter().any(|id| app.session.catalog.photo(*id).is_none_or(|p| p.analysis.is_none())) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        let why = if ids.len() == 1 { "Run Assisted Culling on it first" } else { "Run Assisted Culling on these photos first" };
        super::grid::drag_hint(ui.ctx(), crate::i18n::tr(why).into());
        if released {
            app.toast(
                ui.ctx(),
                crate::i18n::tr("Only photos Assisted Culling scored can be in a burst: run Photo → Assisted Culling… on them first"),
            );
            app.ui.dragging_photos = None;
        }
        return;
    }
    let (params, hint) = match tgt {
        Target::Burst(g, slot, before) => {
            let all = all_bursts(app);
            let members = shown.iter().find(|(n, _)| *n == g).map(|(_, m)| m.as_slice()).unwrap_or(&[]);
            // the place in the whole burst (members the view leaves out included) once the dragged
            // photos have left theirs: right before the photo dropped on, or right after the one
            // whose right half it was (members out of view between them stay on their side)
            let rest: Vec<PhotoId> = all.get(&g).map(|m| m.iter().copied().filter(|p| !ids.contains(p)).collect()).unwrap_or_default();
            let at = |m: &PhotoId| rest.iter().position(|p| p == m);
            let next = members.get(slot..).and_then(|after| after.iter().find(|m| !ids.contains(m)));
            let index = if slot == LAST {
                rest.len()
            } else if let Some(i) = next.filter(|_| before).and_then(at) {
                i
            } else {
                members[..slot.min(members.len())]
                    .iter()
                    .rev()
                    .find(|m| !ids.contains(m))
                    .and_then(|m| rest.iter().position(|p| p == m))
                    .map_or(0, |i| i + 1)
            };
            // the dragged photos are a whole other burst (one lookup: the first one's burst)
            let from = ids.first().and_then(|id| app.session.catalog.photo(*id)).and_then(|p| p.analysis).and_then(|a| a.group);
            let whole = from
                .filter(|n| *n != g)
                .and_then(|n| all.get(&n))
                .is_some_and(|m| m.len() > 1 && m.len() == ids.len() && m.iter().all(|p| ids.contains(p)));
            let hint = if whole {
                "Merge into this burst"
            } else if index == 0 && slot < members.len() {
                if ids.len() == 1 { "Make it the pick" } else { "Put first: the first is the pick" }
            } else if ids.iter().all(|id| members.contains(id)) {
                "Move here"
            } else {
                "Move to this burst"
            };
            let index = if index >= rest.len() { Value::Null } else { json!(index) };
            (json!({"ids": ids.iter().map(|p| p.0).collect::<Vec<_>>(), "burst": g, "index": index}), hint)
        }
        Target::New => (json!({"ids": ids.iter().map(|p| p.0).collect::<Vec<_>>(), "burst": "new"}), "New burst"),
        Target::Nowhere => (json!({"ids": ids.iter().map(|p| p.0).collect::<Vec<_>>(), "burst": null}), "Take out of its burst"),
    };
    if let Some(mark) = mark {
        ui.painter().rect_filled(mark, 1.5, t.accent);
    }
    super::grid::drag_hint(ui.ctx(), crate::i18n::tr(hint).into());
    if released {
        if let Err(e) = app.run("photo.moveToBurst", params) {
            app.toast(ui.ctx(), e);
        }
        app.ui.dragging_photos = None;
    }
}
