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
    /// The face chosen as their picture in People (photo, region index), if one is.
    pub cover: Option<(u64, usize)>,
    /// How many faces are set aside as "not them" (never offered in "More" again).
    pub dismissed: usize,
}

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
        cover: v["cover"]["photo"].as_u64().zip(v["cover"]["index"].as_u64().and_then(|i| usize::try_from(i).ok())),
        dismissed: v["dismissed"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0)),
    }
}

/// What a person page is made from, as a number: every face of the library (where, which box, which name), and whether
/// its photo is in the library. A catalog change that leaves it alone (a rating, an edit, a photo opened and checked)
/// does not need the page made again.
pub(crate) fn faces_signature(cat: &lightcraft_catalog::Catalog) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in cat.photos() {
        let faces = p.meta.regions.iter().filter(|r| r.kind == lightcraft_meta::RegionKind::Face);
        for r in faces {
            (p.id.0, p.in_library(), r.name.as_deref()).hash(&mut h);
            [r.rect.x0, r.rect.y0, r.rect.x1, r.rect.y1].map(f64::to_bits).hash(&mut h);
        }
    }
    h.finish()
}

/// The page for `name`, asked for again only when a face of the library or a chosen picture changed, or, while the scan learns more
/// faces, at most once a second (and no more than a tenth of the time, when making it takes long: a person page in a
/// large library takes seconds).
fn page_for(app: &mut LightcraftApp, name: &str, now: f64) -> Arc<PersonPage> {
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    let (made_from, cost) = app.caches.person_page_made;
    let mut keep = None;
    if let Some((n, r, i, at, page)) = &app.caches.person_page
        && n == name
        && app.caches.person_page_gen == app.session.people_gen
        && (*i == indexed || now - *at < (cost * 10.0).max(1.0))
    {
        // another catalog change than a face's: the page as it is
        if *r == rev || faces_signature(&app.session.catalog) == made_from {
            keep = Some(page.clone());
        }
    }
    if let Some(page) = keep {
        if let Some(cached) = app.caches.person_page.as_mut() {
            cached.1 = rev;
        }
        return page;
    }
    let started = web_time::Instant::now();
    let v = app.session.execute("faces.person", &json!({"name": name})).unwrap_or(Value::Null);
    let page = Arc::new(parse(&v));
    app.caches.person_page = Some((name.to_string(), rev, indexed, now, page.clone()));
    app.caches.person_page_gen = app.session.people_gen;
    app.caches.person_page_made = (faces_signature(&app.session.catalog), started.elapsed().as_secs_f64());
    page
}

/// Whether `f` was set aside for the person named `name` this session (hidden before the engine's next page says so).
fn hidden(set: &std::collections::HashSet<(String, u64, usize)>, name: &str, f: &Face) -> bool {
    set.contains(&(name.to_lowercase(), f.photo, f.index))
}

#[derive(PartialEq)]
enum Hit {
    Nothing,
    Open,
    Confirm,
    ConfirmAbove,
    Dismiss,
    /// Show the person by this face in People.
    MakeCover,
    /// Show them by their largest face again.
    AutoCover,
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
        .filter(|f| !hidden(&app.ui.dismissed_faces, &name, f))
        .find(|f| {
            let widget = format!("more-face:{}:{}", f.photo, f.index);
            app.widgets.iter().any(|(id, rect)| id == &widget && pointer.is_some_and(|p| rect.contains(p)))
        })
        .or_else(|| {
            page.more.iter().filter(|f| !hidden(&app.ui.dismissed_faces, &name, f)).find(|f| {
                ctx.memory(|m| {
                    m.has_focus(egui::Id::unique(("more-face", f.photo, f.index)))
                        || m.has_focus(egui::Id::unique(("more-face-above", f.photo, f.index)))
                })
            })
        });
    let Some(target) = target else { return false };
    let more: Vec<&Face> = page.more.iter().filter(|f| !hidden(&app.ui.dismissed_faces, &name, f)).collect();
    confirm_above(app, ctx, &name, &more, target.score);
    true
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, name: &str) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let page = page_for(app, name, now);
    let shown_name = if page.name.is_empty() { name.to_string() } else { page.name.clone() };
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(PAD - 4.0);
        let back = crate::widgets::arrow_button(ui, crate::i18n::tr("People"), crate::icons::Icon::ChevronLeft, true);
        register(ui, "person:back", back.rect);
        if back.clicked() {
            let _ = app.run("view.people", json!({}));
        }
        ui.label(RichText::new(&shown_name).font(t.semibold(15.0)).color(t.text));
        let rename = ui.button(crate::i18n::tr("Rename Person…"));
        register(ui, "person:rename", rename.rect);
        if rename.clicked() {
            super::people::open_rename(app, &shown_name);
        }
        // straight to the faces that look like them, below hundreds of their own
        let jump = ui.button(crate::i18n::tr("Go to Suggestions")).on_hover_text(crate::i18n::tr("The faces that look like them, below theirs"));
        register(ui, "person:jumpMore", jump.rect);
        if jump.clicked() {
            app.ui.person_jump_more = true;
        }
        let count = if page.total == 1 { "1 face".to_string() } else { format!("{} faces", page.total) };
        ui.label(RichText::new(count).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(PAD - 4.0);
            if page.dismissed > 0 {
                let label = format!("{} ({})", crate::i18n::tr("Bring Back Set-Aside Faces"), page.dismissed);
                let back = ui.button(label).on_hover_text(crate::i18n::tr("The faces you set aside with × are offered again"));
                register(ui, "person:restoreDismissed", back.rect);
                if back.clicked() {
                    match app.run("faces.dismiss", json!({"name": shown_name, "restore": true})) {
                        Ok(_) => {
                            // they are offered again: the page is asked for anew
                            let key = shown_name.to_lowercase();
                            app.ui.dismissed_faces.retain(|(n, ..)| *n != key);
                            app.caches.person_page = None;
                        }
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
            let photos = ui.button(crate::i18n::tr("Show photos")).on_hover_text(crate::i18n::tr("Their photos in the grid"));
            register(ui, "person:photos", photos.rect);
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
    let more: Vec<&Face> = page.more.iter().filter(|f| !hidden(&app.ui.dismissed_faces, &shown_name, f)).collect();
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
        if std::mem::take(&mut app.ui.person_jump_more) {
            let target = Rect::from_min_size(pos2(area.left(), area.top() + more_top), vec2(width, viewport.height().max(1.0)));
            ui.scroll_to_rect(target, Some(egui::Align::TOP));
        }
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
                let spec = TileSpec {
                    r: Rect::from_min_size(min, vec2(edge, edge)),
                    f,
                    ppp,
                    more: false,
                    name: &shown_name,
                    above_count: 0,
                    cover: page.cover == Some((f.photo, f.index)),
                    chosen: page.cover.is_some(),
                };
                if let h @ (Hit::Open | Hit::MakeCover | Hit::AutoCover) = tile(app, ui, &spec) {
                    hit = Some((h, f.clone()));
                }
            }
        }
        // the "More" section
        let heading = pos2(area.left() + PAD, area.top() + more_top + 14.0);
        ui.painter().text(heading, Align2::LEFT_CENTER, "More", t.semibold(15.0), t.text);
        let sub = match &note {
            Some(n) => n.clone(),
            None => format!("Faces that look like {shown_name}. Click to confirm one, ↑ for all above, or × if it is not them."),
        };
        ui.painter().text(pos2(area.left() + PAD, area.top() + more_top + 36.0), Align2::LEFT_CENTER, sub, t.font(12.0), t.text_dim);
        if !recognising {
            let r = Rect::from_min_size(pos2(area.left() + PAD + 330.0, area.top() + more_top + 8.0), vec2(120.0, 24.0));
            let label = if step == super::faces::Setup::TurnOn { "Turn on" } else { "Open Settings" };
            let b = ui.put(r, egui::Button::new(label));
            register(ui, "faces:setup", b.rect);
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
                let spec = TileSpec {
                    r: Rect::from_min_size(min, vec2(edge, edge)),
                    f,
                    ppp,
                    more: true,
                    name: &shown_name,
                    above_count,
                    cover: false,
                    chosen: false,
                };
                match tile(app, ui, &spec) {
                    Hit::Open => hit = Some((Hit::Open, (*f).clone())),
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
                // hidden at once; kept as "not them" for good (#3249), so a later page leaves it out too
                let key = (shown_name.to_lowercase(), f.photo, f.index);
                app.ui.dismissed_faces.insert(key.clone());
                match app.run("faces.dismiss", json!({"name": shown_name, "photo": f.photo, "index": f.index})) {
                    Ok(v) => {
                        if let Some((n, _, _, _, page)) = app.caches.person_page.as_mut()
                            && *n == name
                        {
                            let dismissed = v["dismissed"].as_u64().map_or(page.dismissed, |n| usize::try_from(n).unwrap_or(0));
                            *page = Arc::new(PersonPage { dismissed, ..(**page).clone() });
                            // the page as patched is current (the face is already hidden): not asked for again
                            app.caches.person_page_gen = app.session.people_gen;
                        }
                    }
                    Err(e) => {
                        // not kept: shown again, to be set aside once it can be
                        app.ui.dismissed_faces.remove(&key);
                        app.toast(ui.ctx(), e);
                    }
                }
            }
            Hit::MakeCover | Hit::AutoCover => {
                let photo = if what == Hit::MakeCover { json!(f.photo) } else { Value::Null };
                match app.run("faces.setCover", json!({"name": shown_name, "photo": photo, "index": f.index})) {
                    Ok(_) => {
                        // the star moves on the page as shown: asking `faces.person` again would also redo its "More"
                        // faces, which takes long in a large library and changes nothing here
                        if let Some((n, _, _, _, page)) = app.caches.person_page.as_mut()
                            && *n == name
                        {
                            let cover = (what == Hit::MakeCover).then_some((f.photo, f.index));
                            *page = Arc::new(PersonPage { cover, ..(**page).clone() });
                            // the page as patched is current: a change made elsewhere (a script) is asked for again
                            app.caches.person_page_gen = app.session.people_gen;
                        }
                        let done = if what == Hit::MakeCover {
                            "This face is now their picture in People"
                        } else {
                            "Their largest face is their picture again"
                        };
                        app.toast(ui.ctx(), crate::i18n::tr(done));
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            Hit::Nothing => {}
        }
    }
}

/// What one face tile shows.
struct TileSpec<'a> {
    /// where it is drawn
    r: Rect,
    f: &'a Face,
    /// pixels per point, for the size of the face picture
    ppp: f32,
    /// a "More" tile (a face that looks like the person) rather than a confirmed one
    more: bool,
    /// the person's name
    name: &'a str,
    /// how many shown faces look at least as much like the person (for a "More" tile)
    above_count: usize,
    /// the face chosen as their picture in People
    cover: bool,
    /// a face is chosen as their picture (so the largest one can be asked for again)
    chosen: bool,
}

/// One face tile. A confirmed one opens its photo when clicked; a "More" one is confirmed when clicked, and has a × that
/// sets it aside as not them.
fn tile(app: &mut LightcraftApp, ui: &mut egui::Ui, spec: &TileSpec) -> Hit {
    let TileSpec { r, f, ppp, more, name, above_count, cover, chosen } = *spec;
    let t = Tokens::get(ui.ctx());
    let salt = if more { "more-face" } else { "person-face" };
    let resp = ui.interact(r, egui::Id::unique((salt, f.photo, f.index)), Sense::click());
    crate::widgets::announce(&resp, name.to_string());
    register(ui, format!("{salt}:{}:{}", f.photo, f.index), r);
    // the × is added after the tile, so it wins where they overlap
    let x_rect = Rect::from_min_size(pos2(r.right() - 24.0, r.top() + 4.0), vec2(20.0, 20.0));
    let x_resp = more.then(|| ui.interact(x_rect, egui::Id::unique(("more-face-x", f.photo, f.index)), Sense::click()));
    let above_rect = Rect::from_min_size(pos2(r.left() + 4.0, r.top() + 4.0), vec2(24.0, 20.0));
    let above_resp = more.then(|| ui.interact(above_rect, egui::Id::unique(("more-face-above", f.photo, f.index)), Sense::click()));
    if more {
        register(ui, format!("more-face-above:{}:{}", f.photo, f.index), above_rect);
        register(ui, format!("more-face-x:{}:{}", f.photo, f.index), x_rect);
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
    if cover {
        // their picture in People: a star in the corner
        let badge = Rect::from_min_size(pos2(r.left() + 4.0, r.bottom() - 24.0), vec2(20.0, 20.0));
        p.circle_filled(badge.center(), 10.0, Color32::from_rgba_unmultiplied(30, 30, 30, 220));
        crate::icons::paint(p, badge.shrink(4.0), crate::icons::Icon::StarFilled, Color32::WHITE);
    }
    let mut hit = Hit::Nothing;
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
        let tip = format!("Looks like {name} (similarity {:.2}). Click to confirm, right-click to open the photo.", f.score);
        let _ = resp.clone().on_hover_text(tip);
        if let Some(x) = x_resp.as_ref() {
            let _ = x.clone().on_hover_text(format!("Not {name}: no longer offered for them (the button at the top brings these faces back)"));
        }
        if let Some(above) = above_resp.as_ref() {
            let _ = above.clone().on_hover_text(format!(
                "All above: confirm {above_count} faces as {name} (this face and every shown face with equal or higher similarity). Press A."
            ));
        }
        // a right click offers to open the photo before deciding
        crate::widgets::context_menu(&resp, |ui| {
            let item = ui.button(crate::i18n::tr("Open Photo"));
            register(ui, "more-face-menu:open", item.rect);
            if item.clicked() {
                hit = Hit::Open;
            }
        });
        if hit == Hit::Open {
            // the menu's choice wins
        } else if x_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::Dismiss;
        } else if above_resp.as_ref().is_some_and(|x| x.clicked()) {
            hit = Hit::ConfirmAbove;
        } else if resp.clicked() {
            hit = Hit::Confirm;
        }
    } else {
        let tip = if cover {
            format!("{name}'s picture in People — click to open this photo, right-click to change it")
        } else {
            format!("{name} — click to open this photo, right-click to make it their picture in People")
        };
        let _ = resp.clone().on_hover_text(tip);
        if resp.clicked() {
            hit = Hit::Open;
        }
        crate::widgets::context_menu(&resp, |ui| {
            let use_it = ui.add_enabled(!cover, egui::Button::new(crate::i18n::tr("Use as Person's Picture")));
            register(ui, "person-face-menu:cover", use_it.rect);
            if use_it.clicked() {
                hit = Hit::MakeCover;
            }
            if chosen {
                let auto = ui.button(crate::i18n::tr("Use Their Largest Face"));
                register(ui, "person-face-menu:autoCover", auto.rect);
                if auto.clicked() {
                    hit = Hit::AutoCover;
                }
            }
        });
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

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
