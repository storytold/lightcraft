//! A person's page in the People view: only cropped faces, never whole photos. First the faces already named, then a
//! "More" section with the unnamed faces that look like them (best first), to be confirmed one at a time or in a
//! user-chosen batch.
//!
//! The engine does the looking (`faces.person`); this draws it. Only the rows on screen ask for a face render.

use std::sync::Arc;

use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

const PAD: f32 = 20.0;
const SECTION_H: f32 = 56.0;

#[derive(Clone, Debug, Default)]
pub struct Selection {
    name: String,
    active: bool,
    faces: std::collections::HashSet<(u64, usize)>,
    anchor: Option<(usize, bool)>,
    before_click: Option<BeforeClick>,
}

#[derive(Clone, Debug)]
struct BeforeClick {
    face: (u64, usize),
    faces: std::collections::HashSet<(u64, usize)>,
    anchor: Option<(usize, bool)>,
}

/// A double-click opens the photo without retaining the first click's range/anchor changes.
fn click_confirmed(selection: &mut Selection, faces: &[Face], target: &Face, shift: bool, double: bool) {
    if double {
        if let Some(before) = selection.before_click.take().filter(|before| before.face == (target.photo, target.index)) {
            selection.faces = before.faces;
            selection.anchor = before.anchor;
        }
    } else {
        selection.before_click = Some(BeforeClick { face: (target.photo, target.index), faces: selection.faces.clone(), anchor: selection.anchor });
        select_confirmed(selection, faces, target, shift);
    }
}

/// Repeat the anchor's include/exclude action over a range of confirmed faces.
fn select_confirmed(selection: &mut Selection, faces: &[Face], target: &Face, shift: bool) {
    let Some(index) = faces.iter().position(|f| f.photo == target.photo && f.index == target.index) else { return };
    let key = (target.photo, target.index);
    let (start, selecting) = selection.anchor.filter(|_| shift).unwrap_or((index, !selection.faces.contains(&key)));
    for face in faces.get(start.min(index)..=start.max(index)).unwrap_or_default() {
        if selecting {
            selection.faces.insert((face.photo, face.index));
        } else {
            selection.faces.remove(&(face.photo, face.index));
        }
    }
    if !shift || selection.anchor.is_none() {
        selection.anchor = Some((index, selecting));
    }
}

/// Clear the selected person's assignments atomically and hide them from More for this session.
fn remove_selected(app: &mut LightcraftApp, ctx: &egui::Context, name: &str) {
    let faces: Vec<_> = app.ui.person_selection.faces.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    match app.run("faces.nameFaces", json!({"faces": faces, "name": null, "expectedName": name})) {
        Ok(_) => {
            app.ui.dismissed_faces.extend(app.ui.person_selection.faces.drain());
            app.ui.person_selection.before_click = None;
            app.ui.person_selection.anchor = None;
        }
        Err(error) => app.toast(ctx, error),
    }
}

/// One face: which photo, which of its regions, the box to show it by, and (for the "More" faces) how alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub photo: u64,
    pub index: usize,
    pub rect: lightcraft_geom::Rect,
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

/// Parse a tile reference, preferring its detector-aligned display box over the catalog box.
fn face_of(v: &Value) -> Option<Face> {
    let rect = |r: &Value| Some(lightcraft_geom::Rect { x0: r["x0"].as_f64()?, y0: r["y0"].as_f64()?, x1: r["x1"].as_f64()?, y1: r["y1"].as_f64()? });
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

/// The page for `name`, asked for again only when the catalog changed, or (while the scan runs) at most once a second
/// as it learns more faces.
fn page_for(app: &mut LightcraftApp, name: &str, now: f64) -> Arc<PersonPage> {
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((n, r, i, at, page)) = &app.caches.person_page
        && n == name
        && *r == rev
        && (*i == indexed || now - *at < 1.0)
    {
        return page.clone();
    }
    let v = app.session.execute("faces.person", &json!({"name": name})).unwrap_or(Value::Null);
    let page = Arc::new(parse(&v));
    app.caches.person_page = Some((name.to_string(), rev, indexed, now, page.clone()));
    page
}

#[derive(PartialEq)]
enum Hit {
    Nothing,
    Open,
    OpenDouble,
    Confirm,
    ConfirmAbove,
    Dismiss,
    Remove,
}

/// The current, session-visible candidates at or above the selected score. The score, not the tile position, is the
/// boundary: a later tile with an equal score belongs to the batch too.
fn at_or_above(more: &[&Face], score: f32) -> Vec<(u64, usize)> {
    more.iter().filter(|f| f.score >= score).map(|f| (f.photo, f.index)).collect()
}

fn confirm_above(app: &mut LightcraftApp, ctx: &egui::Context, name: &str, more: &[&Face], score: f32) {
    let faces: Vec<Value> = at_or_above(more, score).into_iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    if !faces.is_empty()
        && let Err(error) = app.run("faces.nameFaces", json!({"faces": faces, "name": name}))
    {
        app.toast(ctx, error);
    }
}

/// The page-scoped A key takes precedence over View ▸ Visualize Spots only for a visible More tile. Called before the
/// general shortcut dispatcher, using the previous frame's registered rectangles as the pointer's target.
pub(crate) fn handle_bulk_key(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
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

/// Render confirmed faces and More suggestions with explicit selection and ignore actions.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, name: &str) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let page = page_for(app, name, now);
    let shown_name = if page.name.is_empty() { name.to_string() } else { page.name.clone() };
    if app.ui.person_selection.name != shown_name {
        app.ui.person_selection = Selection { name: shown_name.clone(), ..Default::default() };
    }
    if !app.ui.person_selection.faces.is_empty() {
        let valid: std::collections::HashSet<_> = page.confirmed.iter().map(|f| (f.photo, f.index)).collect();
        app.ui.person_selection.faces.retain(|key| valid.contains(key));
    }
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
    ui.horizontal_wrapped(|ui| {
        ui.add_space(PAD);
        let active = app.ui.person_selection.active;
        let button = ui.button(if active { crate::i18n::tr("Done selecting") } else { crate::i18n::tr("Select faces…") });
        register(ui.ctx(), "person:selectFaces", button.rect);
        if button.clicked() {
            app.ui.person_selection.active = !active;
            app.ui.person_selection.faces.clear();
            app.ui.person_selection.before_click = None;
            app.ui.person_selection.anchor = None;
        }
        if app.ui.person_selection.active {
            let all = ui.button(crate::i18n::tr("Select all"));
            register(ui.ctx(), "person:selectAll", all.rect);
            if all.clicked() {
                app.ui.person_selection.before_click = None;
                app.ui.person_selection.faces.extend(page.confirmed.iter().map(|f| (f.photo, f.index)));
            }
            let clear = ui.button(crate::i18n::tr("Clear selection"));
            register(ui.ctx(), "person:clearSelection", clear.rect);
            if clear.clicked() {
                app.ui.person_selection.faces.clear();
                app.ui.person_selection.before_click = None;
                app.ui.person_selection.anchor = None;
            }
            let count = app.ui.person_selection.faces.len();
            let remove = ui
                .add_enabled(count > 0, egui::Button::new(format!("Remove {count} faces from {shown_name}")))
                .on_hover_text("Return these faces to unnamed faces. Photos and face boxes are kept. Undo restores the assignment.");
            register(ui.ctx(), "person:removeSelected", remove.rect);
            if remove.clicked() {
                remove_selected(app, ui.ctx(), &shown_name);
            }
        }
    });
    if app.ui.person_selection.active {
        ui.label(crate::i18n::tr("Click faces to select or deselect. Shift-click extends that action to a range. Double-click opens a photo."));
    }
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
    let recognising = app.caches.faces_active || page.ready;
    // with a model installed the button switches recognition on; without one it opens Settings ▸ Faces
    let step = if recognising { super::faces::Setup::Running } else { super::faces::setup(app, ui.ctx()) };
    let note: Option<String> = if step == super::faces::Setup::TurnOn {
        Some(format!("Face recognition is off. Turn it on to find more photos of {shown_name}."))
    } else if !recognising {
        Some(format!("Face recognition needs a model, in Settings, to find more photos of {shown_name}."))
    } else if more.is_empty() && page.pending > 0 {
        Some(format!("Looking through your photos… {} left", page.pending))
    } else if more.is_empty() {
        Some(format!("No other faces look like {shown_name} yet. Faces appear here as your photos are scanned."))
    } else {
        None
    };
    let ppp = ui.ctx().pixels_per_point();
    let edge = super::people::tile_edge(app.ui.thumb_size);
    let mut hit: Option<(Hit, Face)> = None;
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
                let action = tile(app, ui, Rect::from_min_size(min, vec2(edge, edge)), f, ppp, false, &shown_name, 0);
                if action == Hit::Remove {
                    hit = Some((action, f.clone()));
                } else if matches!(action, Hit::Open | Hit::OpenDouble) {
                    let double = action == Hit::OpenDouble;
                    if app.ui.person_selection.active {
                        click_confirmed(&mut app.ui.person_selection, &page.confirmed, f, ui.input(|i| i.modifiers.shift), double);
                    }
                    if !app.ui.person_selection.active || double {
                        hit = Some((Hit::Open, f.clone()));
                    }
                }
                if app.ui.person_selection.active && app.ui.person_selection.faces.contains(&(f.photo, f.index)) {
                    let rect = Rect::from_min_size(min, vec2(edge, edge));
                    ui.painter().rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(3.0, t.accent), StrokeKind::Inside);
                    ui.painter().rect_filled(Rect::from_min_size(min, vec2(24.0, 24.0)), 3.0, t.accent);
                    ui.painter().text(min + vec2(12.0, 12.0), Align2::CENTER_CENTER, "✓", t.font(16.0), Color32::WHITE);
                }
            }
        }
        // the "More" section
        let heading = pos2(area.left() + PAD, area.top() + more_top + 14.0);
        ui.painter().text(heading, Align2::LEFT_CENTER, "More", t.semibold(15.0), t.text);
        let sub = match &note {
            Some(n) => n.clone(),
            None => format!("Faces that look like {shown_name}. Click to confirm one, ↑ for all above, or × to hide."),
        };
        ui.painter().text(pos2(area.left() + PAD, area.top() + more_top + 36.0), Align2::LEFT_CENTER, sub, t.font(12.0), t.text_dim);
        if !recognising {
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
                match tile(app, ui, Rect::from_min_size(min, vec2(edge, edge)), f, ppp, true, &shown_name, above_count) {
                    Hit::Confirm => hit = Some((Hit::Confirm, (*f).clone())),
                    Hit::ConfirmAbove => hit = Some((Hit::ConfirmAbove, (*f).clone())),
                    Hit::Dismiss => hit = Some((Hit::Dismiss, (*f).clone())),
                    _ => {}
                }
            }
        }
        // the picture cache keeps every face on screen
        app.renderer.want_variants(shown);
    });
    if let Some((what, f)) = hit {
        match what {
            Hit::Open | Hit::OpenDouble => {
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
            Hit::Remove => {
                match app.run("faces.nameFaces", json!({"faces": [{"photo": f.photo, "index": f.index}], "name": null, "expectedName": shown_name})) {
                    Ok(_) => {
                        app.ui.dismissed_faces.insert((f.photo, f.index));
                    }
                    Err(error) => app.toast(ui.ctx(), error),
                }
            }
            Hit::Nothing => {}
        }
    }
}

/// One face tile. A confirmed one opens its photo when clicked; a "More" one is confirmed when clicked, and has a × that
/// hides it.
fn tile(app: &mut LightcraftApp, ui: &mut egui::Ui, r: Rect, f: &Face, ppp: f32, more: bool, name: &str, above_count: usize) -> Hit {
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
    if more {
        register(ui.ctx(), format!("more-face-above:{}:{}", f.photo, f.index), above_rect);
    }
    let hovered = resp.hovered() || x_resp.as_ref().is_some_and(|x| x.hovered()) || above_resp.as_ref().is_some_and(|x| x.hovered());
    let p = ui.painter();
    p.rect_filled(r, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lightcraft_catalog::PhotoId(f.photo), f.rect, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if hovered {
        p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
    }
    let mut hit = Hit::Nothing;
    resp.context_menu(|ui| {
        if !more {
            let button = ui.button(format!("Remove from {name}"));
            register(ui.ctx(), "person:removeFace", button.rect);
            if button.clicked() {
                hit = Hit::Remove;
                ui.close();
            }
        }
    });
    if more && hovered {
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
        let _ = resp.clone().on_hover_text(tip);
        if let Some(above) = above_resp.as_ref() {
            let _ = above.clone().on_hover_text(format!(
                "All above: confirm {above_count} faces as {name} (this face and every shown face with equal or higher similarity). Press A."
            ));
        }
        if x_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::Dismiss;
        } else if above_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::ConfirmAbove;
        } else if resp.clicked() {
            hit = Hit::Confirm;
        }
    } else {
        let _ = resp.clone().on_hover_text(format!("{name} — click to open this photo"));
        if resp.double_clicked() {
            hit = Hit::OpenDouble;
        } else if resp.clicked() {
            hit = Hit::Open;
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Double-click navigation restores the whole pre-click range and anchor on return.
    #[test]
    fn double_clicking_a_confirmed_face_preserves_selection_and_anchor() {
        let timeout = std::time::Duration::from_secs(15);
        for case in 0..3 {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
            let ids: Vec<_> = app.session.catalog.photos().take(4).map(|p| p.id).collect();
            for id in ids {
                let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
                meta.regions = vec![lightcraft_meta::Region {
                    rect: lightcraft_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.4, y1: 0.4 },
                    kind: lightcraft_meta::RegionKind::Face,
                    name: Some("Person".into()),
                    description: None,
                }];
                app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
            }
            let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
            h.app.run("view.person", json!({"name": "Person"})).unwrap();
            h.settle(timeout);
            let faces = parse(&h.app.session.execute("faces.person", &json!({"name": "Person"})).unwrap()).confirmed;
            assert_eq!(faces.len(), 4);
            h.app.ui.person_selection.active = true;
            select_confirmed(&mut h.app.ui.person_selection, &faces, &faces[0], false);
            if case == 2 {
                select_confirmed(&mut h.app.ui.person_selection, &faces, &faces[3], true);
                select_confirmed(&mut h.app.ui.person_selection, &faces, &faces[1], false);
            }
            let before = h.app.ui.person_selection.clone();
            // Keep these two clicks separate from setup/navigation click bursts.
            for _ in 0..60 {
                h.step();
            }
            let widget = format!("person-face:{}:{}", faces[3].photo, faces[3].index);
            let params = json!({"id": widget, "shift": case != 0});
            assert_eq!(h.request("ui.clickWidget", params.clone(), timeout)["ok"], true);
            h.step();
            assert_ne!(h.app.ui.person_selection.faces, before.faces);
            assert_eq!(h.request("ui.clickWidget", params, timeout)["ok"], true);
            h.step();
            assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail, "case {case}");
            h.app.run("view.back", json!({})).unwrap();
            h.step();
            assert_eq!(h.app.ui.person_page.as_deref(), Some("Person"));
            assert_eq!(h.app.ui.person_selection.faces, before.faces, "case {case}");
            assert_eq!(h.app.ui.person_selection.anchor, before.anchor, "case {case}");
        }
    }

    /// Batch and context-menu corrections preserve boxes and restore assignments with one undo.
    #[test]
    fn selected_faces_can_be_removed_from_a_person_and_restored_with_one_undo() {
        use std::time::Duration;
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let mut ids: Vec<_> = app.session.catalog.photos().map(|p| p.id).collect();
        ids.sort();
        ids.truncate(3);
        let rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.6, y1: 0.6 };
        for id in &ids {
            let mut meta = app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions =
                vec![lightcraft_meta::Region { rect, kind: lightcraft_meta::RegionKind::Face, name: Some("Person".into()), description: None }];
            app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
        }
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        h.step();
        h.app.run("view.person", json!({"name": "Person"})).unwrap();
        h.settle(Duration::from_secs(15));
        let click = |h: &mut crate::headless::Headless, id: &str| {
            let r = h.request("ui.clickWidget", json!({"id": id}), Duration::from_secs(15));
            assert_eq!(r["ok"], true, "{r}");
            h.step();
        };
        click(&mut h, "person:selectFaces");
        click(&mut h, &format!("person-face:{}:0", ids[0].0));
        click(&mut h, &format!("person-face:{}:0", ids[1].0));
        assert_eq!(h.app.ui.person_selection.faces.len(), 2);
        let undo = h.app.session.undo.len();
        click(&mut h, "person:removeSelected");
        assert_eq!(h.app.session.undo.len(), undo + 1);
        for id in &ids[..2] {
            let photo = h.app.session.catalog.photo(*id).unwrap();
            assert_eq!(photo.meta.regions.len(), 1);
            assert_eq!(photo.meta.regions[0].rect, rect);
            assert!(photo.meta.regions[0].name.is_none());
        }
        assert_eq!(h.app.session.catalog.photo(ids[2]).unwrap().meta.regions[0].name.as_deref(), Some("Person"));
        h.app.run("edit.undo", json!({})).unwrap();
        assert!(ids.iter().all(|id| h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.as_deref() == Some("Person")));
        if let Some(path) = std::env::var_os("LIGHTCRAFT_PERSON_SCREENSHOT") {
            h.step();
            click(&mut h, "person:selectAll");
            let shot = h.snapshot(Duration::from_secs(15));
            let image = lightcraft_raster::Rgba8::from_fn(shot.size[0], shot.size[1], |x, y| shot.pixels[y * shot.size[0] + x].to_array());
            let png = lightcraft_engine::export::encode_image(
                &image,
                &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
            )
            .unwrap();
            std::fs::write(path, png).unwrap();
        }
        h.step();
        let menu = h.request("ui.clickWidget", json!({"id": format!("person-face:{}:0", ids[2].0), "button": "right"}), Duration::from_secs(15));
        assert_eq!(menu["ok"], true);
        h.step();
        click(&mut h, "person:removeFace");
        assert!(h.app.session.catalog.photo(ids[2]).unwrap().meta.regions[0].name.is_none());
        assert!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.is_some());
    }

    /// Shift ranges repeat the anchor action for both inclusion and exclusion.
    #[test]
    fn confirmed_face_selection_extends_include_and_exclude_ranges() {
        let faces: Vec<_> =
            (0..4).map(|photo| Face { photo, index: 0, rect: lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }, score: 0.0 }).collect();
        let mut selected = Selection::default();
        select_confirmed(&mut selected, &faces, &faces[0], false);
        select_confirmed(&mut selected, &faces, &faces[3], true);
        assert_eq!(selected.faces.len(), 4);
        select_confirmed(&mut selected, &faces, &faces[1], false);
        select_confirmed(&mut selected, &faces, &faces[3], true);
        assert_eq!(selected.faces, std::collections::HashSet::from([(0, 0)]));
    }

    #[test]
    fn all_above_uses_score_including_later_ties_and_only_visible_faces() {
        let face = |photo, score| Face { photo, index: 0, rect: lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }, score };
        // The tie is deliberately after a lower-score tile: an index slice would miss it.
        let candidates = [face(1, 0.95), face(2, 0.80), face(3, 0.70), face(4, 0.80), face(5, 0.60)];
        let dismissed = (2_u64, 0_usize);
        let shown: Vec<&Face> = candidates.iter().filter(|f| (f.photo, f.index) != dismissed).collect();
        assert_eq!(at_or_above(&shown, 0.80), vec![(1, 0), (4, 0)], "the dismissed face is excluded and the late tie included");
        assert_eq!(at_or_above(&shown, 0.95), vec![(1, 0)], "the highest choice names only itself");
        assert_eq!(at_or_above(&shown, 0.60), vec![(1, 0), (3, 0), (4, 0), (5, 0)], "the lowest choice names the full displayed range");
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
