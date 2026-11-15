//! A person's page in the People view: only cropped faces, never whole photos. First the faces already named, then a
//! "More" section with the unnamed faces that look like them (best first), to be confirmed one at a time or in a
//! user-chosen batch. Ctrl/⌘-click (or Shift-click for a range, or the tick at a tile's corner) selects "More" faces;
//! a bar then assigns them all to someone else (typing offers the people already named, or creates a new one) or
//! rejects them (`faces.nameFaces`, `faces.ignore`: one undo step each). A tile's context menu does the same for one face.
//!
//! The engine does the looking (`faces.person`); this draws it. Only the rows on screen ask for a face render.

use std::sync::Arc;

use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

const PAD: f32 = 20.0;
const SECTION_H: f32 = 56.0;

/// One face: which photo, which of its regions, the box to show it by, and (for the "More" faces) how alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub photo: u64,
    pub index: usize,
    pub rect: lcc_geom::Rect,
    pub score: f32,
}

/// What `faces.person` said about one person.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonPage {
    pub name: String,
    pub total: usize,
    pub confirmed: Vec<Face>,
    pub more: Vec<Face>,
    /// Recognition is on, so an empty "More" means "none yet" and not "not looking".
    pub ready: bool,
    pub pending: usize,
}

fn face_of(v: &Value) -> Option<Face> {
    let rect = |r: &Value| Some(lcc_geom::Rect { x0: r["x0"].as_f64()?, y0: r["y0"].as_f64()?, x1: r["x1"].as_f64()?, y1: r["y1"].as_f64()? });
    Some(Face {
        photo: v["photo"].as_u64()?,
        index: usize::try_from(v["index"].as_u64()?).ok()?,
        // shown by the detector's box when the scan has one: every face equally close
        rect: rect(&v["view"]).or_else(|| rect(&v["rect"]))?,
        score: v["score"].as_f64().unwrap_or(0.0) as f32,
    })
}

/// The engine's answer as a page (anything that does not make sense is left out).
pub fn parse(v: &Value) -> PersonPage {
    let faces = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(face_of).collect()).unwrap_or_default();
    PersonPage {
        name: v["name"].as_str().unwrap_or("").to_string(),
        total: v["total"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0)),
        confirmed: faces("confirmed"),
        more: faces("more"),
        ready: v["ready"] == true,
        pending: v["pendingPhotos"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0)),
    }
}

/// The library filter as the face lists use it: what the sidebar picked (a year, a folder, an album), any person
/// filter aside, since a person's page and the unnamed faces are about people already.
pub(crate) fn face_filter(f: &lcc_catalog::Filter) -> lcc_catalog::Filter {
    lcc_catalog::Filter { person: None, people: Vec::new(), ..f.clone() }
}

/// Forgets the cached face lists when the library filter changed since they were asked for, so they follow the sidebar.
pub(crate) fn follow_filter(app: &mut LccApp) {
    let now = face_filter(&app.session.filter);
    match &app.caches.faces_filter {
        Some(was) if *was == now => {}
        Some(_) => {
            app.caches.unnamed = None;
            app.caches.person_page = None;
            app.caches.faces_filter = Some(now);
        }
        None => app.caches.faces_filter = Some(now),
    }
}

/// The page for `name`, asked for again only when the catalog changed, or (while the scan runs) at most once a second
/// as it learns more faces.
fn page_for(app: &mut LccApp, name: &str, now: f64) -> Arc<PersonPage> {
    follow_filter(app);
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((n, r, i, at, page)) = &app.caches.person_page
        && n == name
        && *r == rev
        && (*i == indexed || now - *at < 1.0)
    {
        return page.clone();
    }
    let v = app.session.execute("faces.person", &json!({"name": name, "scope": "filter"})).unwrap_or(Value::Null);
    let page = Arc::new(parse(&v));
    app.caches.person_page = Some((name.to_string(), rev, indexed, now, page.clone()));
    page
}

#[derive(PartialEq)]
enum Hit {
    Nothing,
    Open,
    Confirm,
    ConfirmAbove,
    Dismiss,
    /// Ctrl/⌘-click or the tick: add or take away from the selection; `true` with Shift: a range.
    Select(bool),
    /// The context menu's "Assign to Someone Else…": select this face alone and type a name.
    AssignOne,
    /// The context menu's "Reject".
    RejectOne,
}

/// What the name box offers for `typed`: the people already named whose name holds it (those starting with it first,
/// at most eight, not `current`), and the name to create when nobody has that name yet.
pub(crate) fn assign_offers(people: &[String], typed: &str, current: &str) -> (Vec<String>, Option<String>) {
    let typed = typed.trim();
    if typed.is_empty() {
        return (Vec::new(), None);
    }
    let low = typed.to_lowercase();
    let current = current.trim().to_lowercase();
    let mut hits: Vec<&String> = people.iter().filter(|p| p.to_lowercase().contains(&low) && p.trim().to_lowercase() != current).collect();
    hits.sort_by_key(|p| !p.to_lowercase().starts_with(&low));
    let offers = hits.into_iter().take(8).cloned().collect();
    let exists = people.iter().any(|p| p.trim().to_lowercase() == low);
    (offers, (!exists && low != current).then(|| typed.to_string()))
}

/// The name Return assigns for `typed`: an existing person's own spelling when the name matches one, else the typed
/// name (a new person). `None` for nothing, or for the page's own person.
pub(crate) fn assign_target(people: &[String], typed: &str, current: &str) -> Option<String> {
    let typed = typed.trim();
    let low = typed.to_lowercase();
    if typed.is_empty() || low == current.trim().to_lowercase() {
        return None;
    }
    Some(people.iter().find(|p| p.trim().to_lowercase() == low).cloned().unwrap_or_else(|| typed.to_string()))
}

fn faces_json(faces: &[(u64, usize)]) -> Value {
    Value::Array(faces.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect())
}

fn clear_selection(app: &mut LccApp) {
    app.ui.person_selected.clear();
    app.ui.person_assign.clear();
    app.ui.person_anchor = None;
    app.ui.person_assign_focus = false;
}

/// Toggle face `i` of `more`, or with `range` select every face from the last one clicked.
fn select(app: &mut LccApp, more: &[&Face], i: usize, range: bool) {
    let Some(f) = more.get(i) else { return };
    match app.ui.person_anchor.filter(|_| range) {
        Some(a) => {
            for f in more.get(a.min(i)..=a.max(i)).unwrap_or_default() {
                app.ui.person_selected.insert((f.photo, f.index));
            }
        }
        None => {
            let key = (f.photo, f.index);
            if !app.ui.person_selected.remove(&key) {
                app.ui.person_selected.insert(key);
            }
            app.ui.person_anchor = Some(i);
        }
    }
}

/// The bar above the faces while some "More" faces are selected: how many, a name box with the matching people and
/// "+ Create", Assign, Reject and Clear.
fn batch_bar(app: &mut LccApp, ui: &mut egui::Ui, current: &str) {
    if app.ui.person_selected.is_empty() {
        app.ui.person_assign.clear();
        return;
    }
    let t = Tokens::get(ui.ctx());
    let mut selected: Vec<(u64, usize)> = app.ui.person_selected.iter().copied().collect();
    selected.sort_unstable();
    let people = app.caches.person_names(&app.session.catalog);
    let target = assign_target(&people, &app.ui.person_assign, current);
    let (mut assign, mut reject, mut clear) = (None::<String>, false, false);
    ui.horizontal(|ui| {
        ui.add_space(PAD - 4.0);
        let n = selected.len();
        ui.label(RichText::new(if n == 1 { "1 face selected".to_string() } else { format!("{n} faces selected") }).color(t.text));
        let field = crate::text_field::TextField::singleline("field:personAssign", &mut app.ui.person_assign)
            .hint(crate::i18n::tr("Assign to…"))
            .width(220.0)
            .show(ui);
        if std::mem::take(&mut app.ui.person_assign_focus) {
            field.response.request_focus();
        }
        if field.ending == Some(crate::text_field::Ending::Return) {
            assign = target.clone();
        }
        let label = if n == 1 { "Assign face".to_string() } else { format!("Assign {n} faces") };
        let b = ui.add_enabled(target.is_some(), egui::Button::new(label));
        register(ui.ctx(), "person:assign", b.rect);
        if b.clicked() {
            assign = target.clone();
        }
        let r = ui.button(crate::i18n::tr("Reject")).on_hover_text(crate::i18n::tr("Leave these faces out of People; they are not suggested again"));
        register(ui.ctx(), "person:reject", r.rect);
        reject = r.clicked();
        let c = ui.button(crate::i18n::tr("Clear"));
        register(ui.ctx(), "person:clearSelection", c.rect);
        clear = c.clicked() || field.cancelled();
    });
    let (offers, create) = assign_offers(&people, &app.ui.person_assign, current);
    if !offers.is_empty() || create.is_some() {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(PAD - 4.0);
            for p in offers {
                let b = ui.add(egui::Button::new(RichText::new(p.as_str()).color(t.text_label)).frame(false));
                register(ui.ctx(), format!("person:pick:{p}"), b.rect);
                if b.clicked() {
                    assign = Some(p);
                }
            }
            if let Some(new) = create {
                let b =
                    ui.add(egui::Button::new(RichText::new(crate::i18n::tr_format!("+ Create “{new}”", new = new)).color(t.text_label)).frame(false));
                register(ui.ctx(), "person:create", b.rect);
                if b.clicked() {
                    assign = Some(new);
                }
            }
        });
    }
    if let Some(who) = assign {
        if let Err(error) = app.run("faces.nameFaces", json!({"faces": faces_json(&selected), "name": who})) {
            app.toast(ui.ctx(), error);
        }
        clear_selection(app);
    } else if reject {
        if let Err(error) = app.run("faces.ignore", json!({"faces": faces_json(&selected)})) {
            app.toast(ui.ctx(), error);
        }
        clear_selection(app);
    } else if clear {
        clear_selection(app);
    }
}

/// The current, session-visible candidates at or above the selected score. The score, not the tile position, is the
/// boundary: a later tile with an equal score belongs to the batch too.
fn at_or_above(more: &[&Face], score: f32) -> Vec<(u64, usize)> {
    more.iter().filter(|f| f.score >= score).map(|f| (f.photo, f.index)).collect()
}

fn confirm_above(app: &mut LccApp, ctx: &egui::Context, name: &str, more: &[&Face], score: f32) {
    let faces: Vec<Value> = at_or_above(more, score).into_iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    if !faces.is_empty()
        && let Err(error) = app.run("faces.nameFaces", json!({"faces": faces, "name": name}))
    {
        app.toast(ctx, error);
    }
}

/// The page-scoped A key takes precedence over View ▸ Visualize Spots only for a visible More tile. Called before the
/// general shortcut dispatcher, using the previous frame's registered rectangles as the pointer's target.
pub(crate) fn handle_bulk_key(app: &mut LccApp, ctx: &egui::Context) -> bool {
    if app.ui.view != crate::state::ViewMode::People || app.ui.dialog.is_some() || ctx.text_edit_focused() {
        return false;
    }
    let Some(name) = app.ui.person_page.clone() else { return false };
    if !ctx.input(|i| i.modifiers == egui::Modifiers::NONE && i.key_pressed(egui::Key::A)) {
        return false;
    }
    let Some((cached_name, cached_revision, _, _, page)) = &app.caches.person_page else { return false };
    if *cached_name != name || *cached_revision != app.session.catalog.revision {
        return false;
    }
    let page = page.clone();
    let pointer = ctx.pointer_hover_pos();
    let target = page
        .more
        .iter()
        .filter(|f| !app.ui.dismissed_faces.contains(&(f.photo, f.index)))
        .find(|f| {
            let widget = format!("more-face:{}:{}", f.photo, f.index);
            app.widgets.iter().any(|(id, rect)| id == &widget && pointer.is_some_and(|p| rect.contains(p)))
        })
        .or_else(|| {
            page.more.iter().filter(|f| !app.ui.dismissed_faces.contains(&(f.photo, f.index))).find(|f| {
                ctx.memory(|m| {
                    m.has_focus(egui::Id::new(("more-face", f.photo, f.index))) || m.has_focus(egui::Id::new(("more-face-above", f.photo, f.index)))
                })
            })
        });
    let Some(target) = target else { return false };
    let more: Vec<&Face> = page.more.iter().filter(|f| !app.ui.dismissed_faces.contains(&(f.photo, f.index))).collect();
    confirm_above(app, ctx, &name, &more, target.score);
    true
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui, name: &str) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let page = page_for(app, name, now);
    let shown_name = if page.name.is_empty() { name.to_string() } else { page.name.clone() };
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(PAD - 4.0);
        let back = crate::widgets::arrow_button(ui, crate::i18n::tr("People"), crate::icons::Icon::ChevronLeft, true);
        register(ui.ctx(), "person:back", back.rect);
        if back.clicked() {
            let _ = app.run("view.people", json!({}));
        }
        ui.label(RichText::new(&shown_name).font(t.semibold(15.0)).color(t.text));
        let rename = ui.button(crate::i18n::tr("Rename Person…"));
        register(ui.ctx(), "person:rename", rename.rect);
        if rename.clicked() {
            super::people::open_rename(app, &shown_name);
        }
        let count = if page.total == 1 { "1 face".to_string() } else { format!("{} faces", page.total) };
        ui.label(RichText::new(count).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(PAD - 4.0);
            let photos = ui.button(crate::i18n::tr("Show photos")).on_hover_text(crate::i18n::tr("Their photos in the grid"));
            register(ui.ctx(), "person:photos", photos.rect);
            if photos.clicked() {
                let _ = app.run("library.filter", json!({"person": shown_name}));
                let _ = app.run("view.photoGrid", json!({}));
            }
        });
    });
    ui.add_space(6.0);
    if page.total == 0 {
        super::empty_message(
            ui,
            ui.available_rect_before_wrap(),
            "No faces by that name",
            "They may have been renamed or removed. Go back to People",
        );
        return;
    }
    let dismissed = &app.ui.dismissed_faces;
    let more: Vec<&Face> = page.more.iter().filter(|f| !dismissed.contains(&(f.photo, f.index))).collect();
    // what the "More" section says when it has no faces to show
    let recognising = app.caches.faces_active || !more.is_empty();
    // with a model installed the button switches recognition on; without one it opens Settings ▸ Faces
    let step = if recognising { super::faces::Setup::Running } else { super::faces::setup(app, ui.ctx()) };
    let note: Option<String> = if step == super::faces::Setup::TurnOn {
        Some(format!("Face recognition is off. Turn it on to find more photos of {shown_name}."))
    } else if matches!(step, super::faces::Setup::GetModel | super::faces::Setup::Unavailable) {
        Some(format!("Face recognition needs a model, in Settings, to find more photos of {shown_name}."))
    } else if more.is_empty() && page.pending > 0 {
        Some(format!("Looking through your photos… {} left", page.pending))
    } else if more.is_empty() {
        Some(format!("No other faces look like {shown_name} yet. Faces appear here as your photos are scanned."))
    } else {
        None
    };
    // what was selected and has since been named, rejected or hidden is no longer selected
    let present: std::collections::HashSet<(u64, usize)> = more.iter().map(|f| (f.photo, f.index)).collect();
    app.ui.person_selected.retain(|k| present.contains(k));
    if app.ui.person_selected.is_empty() {
        app.ui.person_anchor = None;
    }
    batch_bar(app, ui, &shown_name);
    let dismissed = &app.ui.dismissed_faces;
    let more: Vec<&Face> = page.more.iter().filter(|f| !dismissed.contains(&(f.photo, f.index))).collect();
    let selecting = !app.ui.person_selected.is_empty();
    let ppp = ui.ctx().pixels_per_point();
    let edge = super::people::tile_edge(app.ui.thumb_size);
    let mut hit: Option<(Hit, Face, usize)> = None;
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let cols = super::people::columns(width, edge);
        let row_h = edge + super::people::TILE_GAP;
        let confirmed_rows = page.confirmed.len().div_ceil(cols);
        let more_rows = more.len().div_ceil(cols);
        let more_top = PAD + confirmed_rows as f32 * row_h + 12.0;
        let total_h = more_top + SECTION_H + more_rows as f32 * row_h + PAD;
        let (area, _) = ui.allocate_exact_size(vec2(width, total_h), Sense::hover());
        let visible = |top: f32, rows: usize| {
            let first = ((viewport.top() - top) / row_h).floor().max(0.0) as usize;
            let last = (((viewport.bottom() - top) / row_h).ceil().max(0.0) as usize).min(rows);
            first..last
        };
        let mut shown = 0;
        for row in visible(PAD, confirmed_rows) {
            for col in 0..cols {
                let Some(f) = page.confirmed.get(row * cols + col) else { break };
                shown += 1;
                let min = area.min + vec2(PAD + col as f32 * (edge + super::people::TILE_GAP), PAD + row as f32 * row_h);
                if let Hit::Open = tile(app, ui, Rect::from_min_size(min, vec2(edge, edge)), f, ppp, false, &shown_name, 0, false) {
                    hit = Some((Hit::Open, f.clone(), 0));
                }
            }
        }
        // the "More" section
        let heading = pos2(area.left() + PAD, area.top() + more_top + 14.0);
        ui.painter().text(heading, Align2::LEFT_CENTER, "More", t.semibold(15.0), t.text);
        let sub = match &note {
            Some(n) => n.clone(),
            None => format!(
                "Faces that look like {shown_name}. Click to confirm one, ↑ for all above, × to hide, or Ctrl/⌘-click to select some and assign or reject them."
            ),
        };
        ui.painter().text(pos2(area.left() + PAD, area.top() + more_top + 36.0), Align2::LEFT_CENTER, sub, t.font(12.0), t.text_dim);
        if matches!(step, super::faces::Setup::TurnOn | super::faces::Setup::GetModel) {
            let r = Rect::from_min_size(pos2(area.left() + PAD + 330.0, area.top() + more_top + 8.0), vec2(120.0, 24.0));
            let label = if step == super::faces::Setup::TurnOn { "Turn on" } else { "Open Settings" };
            let b = ui.put(r, egui::Button::new(label));
            register(ui.ctx(), "faces:setup", b.rect);
            if b.clicked() {
                if step == super::faces::Setup::TurnOn {
                    super::faces::take_step(app, ui.ctx(), step);
                } else {
                    let _ = app.run("app.settings", json!({"tab": "faces"}));
                }
            }
        }
        let grid_top = more_top + SECTION_H;
        for row in visible(grid_top, more_rows) {
            for col in 0..cols {
                let Some(f) = more.get(row * cols + col) else { break };
                shown += 1;
                let min = area.min + vec2(PAD + col as f32 * (edge + super::people::TILE_GAP), grid_top + row as f32 * row_h);
                let above_count = more.iter().filter(|candidate| candidate.score >= f.score).count();
                let i = row * cols + col;
                let mut h = tile(app, ui, Rect::from_min_size(min, vec2(edge, edge)), f, ppp, true, &shown_name, above_count, selecting);
                // while some are selected a plain click adds to (or takes from) the selection
                if selecting && h == Hit::Confirm {
                    h = Hit::Select(false);
                }
                if !matches!(h, Hit::Nothing | Hit::Open) {
                    hit = Some((h, (*f).clone(), i));
                }
            }
        }
        // the picture cache keeps every face on screen
        app.renderer.want_variants(shown);
    });
    if let Some((what, f, i)) = hit {
        match what {
            Hit::Open => {
                let _ = app.run("library.select", json!({"ids": [f.photo], "active": f.photo}));
                let _ = app.run("view.detail", json!({}));
                // Escape comes back to this page; the People button to everyone, with this person next to its title
                app.ui.person_from = Some((Some(shown_name.clone()), f.photo));
                app.ui.last_person = Some(shown_name.clone());
                app.ui.person_page = None;
            }
            Hit::Confirm => {
                let _ = app.run("faces.setName", json!({"id": f.photo, "index": f.index, "name": shown_name}));
            }
            Hit::ConfirmAbove => confirm_above(app, ui.ctx(), &shown_name, &more, f.score),
            Hit::Dismiss => {
                app.ui.dismissed_faces.insert((f.photo, f.index));
            }
            Hit::Select(range) => select(app, &more, i, range),
            Hit::AssignOne => {
                clear_selection(app);
                app.ui.person_selected.insert((f.photo, f.index));
                app.ui.person_anchor = Some(i);
                app.ui.person_assign_focus = true;
            }
            Hit::RejectOne => {
                if let Err(error) = app.run("faces.ignore", json!({"faces": faces_json(&[(f.photo, f.index)])})) {
                    app.toast(ui.ctx(), error);
                }
                app.ui.person_selected.remove(&(f.photo, f.index));
            }
            Hit::Nothing => {}
        }
    }
}

/// One face tile. A confirmed one opens its photo when clicked; a "More" one is confirmed when clicked, and has a × that
/// hides it.
#[allow(clippy::too_many_arguments)]
fn tile(app: &mut LccApp, ui: &mut egui::Ui, r: Rect, f: &Face, ppp: f32, more: bool, name: &str, above_count: usize, selecting: bool) -> Hit {
    let t = Tokens::get(ui.ctx());
    let salt = if more { "more-face" } else { "person-face" };
    let resp = ui.interact(r, egui::Id::new((salt, f.photo, f.index)), Sense::click());
    crate::widgets::announce(&resp, name.to_string());
    register(ui.ctx(), format!("{salt}:{}:{}", f.photo, f.index), r);
    // the × is added after the tile, so it wins where they overlap
    let x_rect = Rect::from_min_size(pos2(r.right() - 24.0, r.top() + 4.0), vec2(20.0, 20.0));
    let x_resp = more.then(|| ui.interact(x_rect, egui::Id::new(("more-face-x", f.photo, f.index)), Sense::click()));
    let above_rect = Rect::from_min_size(pos2(r.left() + 4.0, r.top() + 4.0), vec2(24.0, 20.0));
    let above_resp = more.then(|| ui.interact(above_rect, egui::Id::new(("more-face-above", f.photo, f.index)), Sense::click()));
    let tick_rect = Rect::from_min_size(pos2(r.left() + 4.0, r.bottom() - 26.0), vec2(22.0, 22.0));
    let tick_resp = more.then(|| ui.interact(tick_rect, egui::Id::new(("more-face-tick", f.photo, f.index)), Sense::click()));
    if more {
        register(ui.ctx(), format!("more-face-above:{}:{}", f.photo, f.index), above_rect);
        register(ui.ctx(), format!("more-face-tick:{}:{}", f.photo, f.index), tick_rect);
    }
    let picked = more && app.ui.person_selected.contains(&(f.photo, f.index));
    let hovered = resp.hovered()
        || x_resp.as_ref().is_some_and(|x| x.hovered())
        || above_resp.as_ref().is_some_and(|x| x.hovered())
        || tick_resp.as_ref().is_some_and(|x| x.hovered());
    let p = ui.painter();
    p.rect_filled(r, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lcc_catalog::PhotoId(f.photo), f.rect, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // which face this tile is about, when another shares it (#1147)
    super::face_preview::outline(app, p, r, f.photo, f.rect);
    if hovered {
        p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
    }
    if picked {
        p.rect_stroke(r, 3.0, Stroke::new(3.0, t.accent), StrokeKind::Inside);
    }
    if more && (hovered || picked || selecting) {
        // the tick that selects: filled when selected
        let c = tick_rect.center();
        let fill = if picked { t.accent } else { Color32::from_rgba_unmultiplied(30, 30, 30, 200) };
        p.circle_filled(c, 10.0, fill);
        p.circle_stroke(c, 10.0, Stroke::new(1.2, Color32::WHITE));
        if picked {
            p.line_segment([pos2(c.x - 4.5, c.y), pos2(c.x - 1.0, c.y + 3.5)], Stroke::new(1.8, Color32::WHITE));
            p.line_segment([pos2(c.x - 1.0, c.y + 3.5), pos2(c.x + 4.5, c.y - 3.5)], Stroke::new(1.8, Color32::WHITE));
        }
    }
    let mut hit = Hit::Nothing;
    if more && hovered && !selecting {
        // a check at the bottom right says a click confirms; × hides and ↑ names all above this score
        let badge = Rect::from_min_size(pos2(r.right() - 28.0, r.bottom() - 28.0), vec2(24.0, 24.0));
        p.circle_filled(badge.center(), 12.0, Color32::from_rgba_unmultiplied(30, 30, 30, 220));
        let c = badge.center();
        let check = [pos2(c.x - 5.0, c.y), pos2(c.x - 1.5, c.y + 4.0), pos2(c.x + 5.5, c.y - 4.0)];
        p.line_segment([check[0], check[1]], Stroke::new(2.0, Color32::WHITE));
        p.line_segment([check[1], check[2]], Stroke::new(2.0, Color32::WHITE));
        p.circle_filled(x_rect.center(), 10.0, Color32::from_rgba_unmultiplied(30, 30, 30, 220));
        let xc: Pos2 = x_rect.center();
        p.line_segment([pos2(xc.x - 3.5, xc.y - 3.5), pos2(xc.x + 3.5, xc.y + 3.5)], Stroke::new(1.6, Color32::WHITE));
        p.line_segment([pos2(xc.x - 3.5, xc.y + 3.5), pos2(xc.x + 3.5, xc.y - 3.5)], Stroke::new(1.6, Color32::WHITE));
        p.rect_filled(above_rect, 4.0, Color32::from_rgba_unmultiplied(30, 30, 30, 220));
        let ac = above_rect.center();
        let arrow = Stroke::new(2.0, Color32::WHITE);
        p.line_segment([pos2(ac.x, ac.y + 5.0), pos2(ac.x, ac.y - 5.0)], arrow);
        p.line_segment([pos2(ac.x - 4.0, ac.y - 1.0), pos2(ac.x, ac.y - 5.0)], arrow);
        p.line_segment([pos2(ac.x, ac.y - 5.0), pos2(ac.x + 4.0, ac.y - 1.0)], arrow);
    }
    if more {
        let tip = format!("Looks like {name} (similarity {:.2}). Click to confirm.", f.score);
        super::face_preview::on_hover(app, &resp, f.photo, f.rect, Some(tip));
        if let Some(above) = above_resp.as_ref() {
            let _ = above.clone().on_hover_text(format!(
                "All above: confirm {above_count} faces as {name} (this face and every shown face with equal or higher similarity). Press A."
            ));
        }
        resp.context_menu(|ui| {
            if ui.button(crate::i18n::tr("Assign to Someone Else…")).clicked() {
                hit = Hit::AssignOne;
                ui.close();
            }
            if ui.button(crate::i18n::tr("Reject")).clicked() {
                hit = Hit::RejectOne;
                ui.close();
            }
        });
        let modifiers = ui.input(|i| i.modifiers);
        if hit != Hit::Nothing {
            // chosen from the menu
        } else if tick_resp.as_ref().is_some_and(|x| x.clicked()) || (resp.clicked() && (modifiers.command || modifiers.shift)) {
            hit = Hit::Select(modifiers.shift);
        } else if x_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::Dismiss;
        } else if above_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::ConfirmAbove;
        } else if resp.clicked() {
            hit = Hit::Confirm;
        }
    } else {
        let tip = crate::i18n::tr_format!("{name}: click to open this photo", name = name);
        super::face_preview::on_hover(app, &resp, f.photo, f.rect, Some(tip));
        if resp.clicked() {
            hit = Hit::Open;
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_above_uses_score_including_later_ties_and_only_visible_faces() {
        let face = |photo, score| Face { photo, index: 0, rect: lcc_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }, score };
        // The tie is deliberately after a lower-score tile: an index slice would miss it.
        let candidates = [face(1, 0.95), face(2, 0.80), face(3, 0.70), face(4, 0.80), face(5, 0.60)];
        let dismissed = (2_u64, 0_usize);
        let shown: Vec<&Face> = candidates.iter().filter(|f| (f.photo, f.index) != dismissed).collect();
        assert_eq!(at_or_above(&shown, 0.80), vec![(1, 0), (4, 0)], "the dismissed face is excluded and the late tie included");
        assert_eq!(at_or_above(&shown, 0.95), vec![(1, 0)], "the highest choice names only itself");
        assert_eq!(at_or_above(&shown, 0.60), vec![(1, 0), (3, 0), (4, 0), (5, 0)], "the lowest choice names the full displayed range");
    }

    #[test]
    fn the_name_box_offers_people_and_creates_a_new_one_only_when_nobody_has_the_name() {
        let people: Vec<String> = ["Alex Smith", "Jane Doe", "Malex", "alexandra"].iter().map(|s| s.to_string()).collect();
        let (offers, create) = assign_offers(&people, " ale", "Jane Doe");
        assert_eq!(offers.first().map(String::as_str), Some("Alex Smith"), "a name starting with what is typed comes first");
        assert!(offers.contains(&"Malex".to_string()), "a name holding it is offered too");
        assert_eq!(create.as_deref(), Some("ale"));
        let (offers, create) = assign_offers(&people, "ALEX SMITH", "Jane Doe");
        assert_eq!((offers.len(), create), (1, None), "an existing name is not created again");
        assert_eq!(assign_target(&people, "alex smith", "Jane Doe").as_deref(), Some("Alex Smith"), "the existing spelling is kept");
        assert_eq!(assign_target(&people, "Alex", "Jane Doe").as_deref(), Some("Alex"), "a new name makes a new person");
        assert!(assign_offers(&people, "jane", "Jane Doe").0.is_empty(), "the page's own person is not offered");
        // nothing, the page's own person, or odd text: no target and no crash
        for odd in ["", "   ", " jane doe ", "\u{0}", "\u{1F600}\u{0301}"] {
            let _ = assign_offers(&people, odd, "Jane Doe");
            let _ = assign_offers(&[], odd, "");
        }
        assert_eq!(assign_target(&people, "   ", "Jane Doe"), None);
        assert_eq!(assign_target(&people, " JANE doe", "Jane Doe"), None);
    }

    #[test]
    fn the_engines_answer_becomes_a_page_and_odd_entries_are_left_out() {
        let v = json!({
            "name": "Jane Doe", "total": 2, "ready": true, "pendingPhotos": 7,
            "confirmed": [
                {"photo": 3, "index": 0, "rect": {"x0": 0.1, "y0": 0.2, "x1": 0.3, "y1": 0.5}},
                {"photo": 4, "index": 1},
                {"photo": "x", "index": 0, "rect": {"x0": 0.1, "y0": 0.2, "x1": 0.3, "y1": 0.5}},
            ],
            "more": [{"photo": 9, "index": 2, "score": 0.5, "rect": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}}],
        });
        let page = parse(&v);
        assert_eq!((page.name.as_str(), page.total, page.ready, page.pending), ("Jane Doe", 2, true, 7));
        assert_eq!(page.confirmed.len(), 1, "a face without a box or a photo is dropped");
        assert_eq!((page.confirmed[0].photo, page.confirmed[0].index), (3, 0));
        assert_eq!((page.more.len(), page.more[0].score), (1, 0.5));
        // nothing, or nonsense, is an empty page and not a crash
        for bad in [Value::Null, json!([]), json!({"confirmed": "x", "more": 5, "total": -1})] {
            assert_eq!(parse(&bad), PersonPage::default(), "{bad}");
        }
    }
}
