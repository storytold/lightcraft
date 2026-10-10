//! The Classic Slideshow module: the Template Browser and saved slideshows on the left, the slide
//! preview in the centre, Options / Layout / Overlays / Backdrop / Titles / Playback / Music on the
//! right, and the play / preview / export buttons in the toolbar. The look, timing and export come
//! from `dac-slideshow`; the preview and the show are painted with egui using the same geometry, the
//! JPEG-sequence export composes on the CPU.
//!
//! Every action is a `slideshow.*` command (listed in `module::SHELL_COMMANDS`). View ▸ Slideshow
//! (the impromptu slideshow) is unchanged.

use std::cell::RefCell;

use dac_catalog::{Flag, PhotoId};
use dac_slideshow::compose::{Geometry, geometry};
use dac_slideshow::settings::{Anchor, Aspect, TextOverlay, TitleScreen};
use dac_slideshow::timeline::{Plan, Segment, pan_zoom};
use dac_slideshow::tokens::{SlideInfo, TOKENS, expand};
use dac_slideshow::{SavedSlideshow, Settings, Template, builtin_templates};
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Which photos the slideshow shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsePhotos {
    /// Every photo in the filmstrip.
    #[default]
    All,
    Selected,
    Flagged,
}

/// A show in progress (not saved).
#[derive(Clone, Debug)]
pub struct Playing {
    pub plan: Plan,
    pub photos: Vec<PhotoId>,
    /// egui time the show started (shifted on pause / resume / steps).
    pub start: f64,
    pub paused_at: Option<f64>,
    /// Full screen, else in the preview area.
    pub full: bool,
}

/// The module's state (saved with `ui.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SlideshowState {
    pub settings: Settings,
    /// The user's templates (the built-in ones come from `dac-slideshow`).
    pub templates: Vec<Template>,
    /// Saved slideshows as older builds kept them here; they move into the catalog as saved
    /// creations (collections of kind `slideshow`) the first time a slideshow command runs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<SavedSlideshow>,
    /// The template last applied or saved.
    pub template: String,
    pub use_photos: UsePhotos,
    /// The name of the saved slideshow open (its photos are the show), if any.
    pub open: Option<String>,
    /// Its collection (saved creation).
    pub open_id: Option<u64>,
    #[serde(skip)]
    pub playing: Option<Playing>,
    /// The last export's result, for the toolbar.
    #[serde(skip)]
    pub export_status: Option<String>,
    /// An export is running on a worker thread.
    #[serde(skip)]
    pub export_busy: bool,
    /// The last export's command result (or `{error}`).
    #[serde(skip)]
    pub export_last: Option<Value>,
}

thread_local! {
    /// The music player of the running show (UI thread only; dropping it stops the sound).
    static PLAYER: RefCell<Option<dac_slideshow::music::Player>> = const { RefCell::new(None) };
}

/// The commands of the module (`module::SHELL_COMMANDS` lists them for menus and parity).
pub fn is_command(id: &str) -> bool {
    id.starts_with("slideshow.")
}

/// The photos the show uses.
pub fn photos(app: &mut DacApp) -> Vec<PhotoId> {
    if let Some(id) = app.ui.slides.open_id
        && let Some(a) = app.session.catalog.album(dac_catalog::AlbumId(id)).filter(|a| a.creation.is_some())
    {
        let ids: Vec<PhotoId> = a.photos.iter().copied().filter(|i| app.session.catalog.photo(*i).is_some()).collect();
        return ids;
    }
    let visible = app.session.visible_cloned();
    match app.ui.slides.use_photos {
        UsePhotos::All => visible,
        UsePhotos::Selected => {
            let sel = app.session.selection.ids.clone();
            let v: Vec<PhotoId> = visible.iter().copied().filter(|i| sel.contains(i)).collect();
            if v.is_empty() { visible } else { v }
        }
        UsePhotos::Flagged => visible.into_iter().filter(|i| app.session.catalog.photo(*i).is_some_and(|p| p.flag == Flag::Pick)).collect(),
    }
}

fn plate_text(app: &DacApp) -> String {
    let t = app.ui.identity_plate.text.trim();
    if t.is_empty() { dac_brand::DISPLAY_NAME.to_string() } else { t.to_string() }
}

fn all_templates(app: &DacApp) -> Vec<Template> {
    let mut v = builtin_templates();
    v.extend(app.ui.slides.templates.iter().cloned());
    v
}

fn name_param(p: &Value) -> Result<String, String> {
    let n = p.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("missing name")?;
    Ok(n.chars().take(80).collect())
}

/// The saved slideshows: saved creations of kind `slideshow` in the catalog.
fn saved_list(app: &DacApp) -> Vec<dac_engine::creations::SavedCreation> {
    dac_engine::creations::creations_of(&app.session, Some(dac_layout::CreationKind::Slideshow))
}

/// Moves saved slideshows an older build kept in `ui.json` into the catalog (once).
fn migrate_saved(app: &mut DacApp) {
    if app.ui.slides.saved.is_empty() {
        return;
    }
    for old in std::mem::take(&mut app.ui.slides.saved) {
        let exists = dac_engine::creations::find_creation(&app.session, dac_layout::CreationKind::Slideshow, &old.name).is_some();
        if exists || old.name.trim().is_empty() {
            continue;
        }
        let r = app.run("creation.save", json!({"kind": "slideshow", "name": old.name, "settings": old.settings, "ids": old.photos}));
        if let Err(e) = r {
            log::warn!("saved slideshow {:?} could not move into the catalog: {e}", old.name);
        }
    }
}

/// Opens a saved slideshow (`id`: its collection; else by `name`): its settings, and its photos
/// are the show.
fn open_saved(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let list = saved_list(app);
    let found = match p.get("id").and_then(Value::as_u64) {
        Some(id) => list.into_iter().find(|c| c.id.0 == id),
        None => {
            let name = name_param(p)?;
            list.into_iter().find(|c| c.name.eq_ignore_ascii_case(&name))
        }
    };
    let c = found.ok_or("no such saved slideshow")?;
    let settings: Settings = serde_json::from_value(c.settings.clone()).unwrap_or_default();
    app.ui.slides.settings = settings.sanitized();
    app.ui.slides.open = Some(c.name);
    app.ui.slides.open_id = Some(c.id.0);
    Ok(state_json(app))
}

fn state_json(app: &mut DacApp) -> Value {
    let photos = photos(app).len();
    let s = &app.ui.slides;
    json!({
        "settings": s.settings,
        "template": s.template,
        "templates": all_templates(app).iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "saved": saved_list(app).iter().map(|x| json!({"id": x.id.0, "name": x.name, "photos": x.photos.len()})).collect::<Vec<_>>(),
        "open": s.open,
        "openId": s.open_id,
        "usePhotos": s.use_photos,
        "photos": photos,
        "playing": s.playing.as_ref().map(|p| json!({"full": p.full, "paused": p.paused_at.is_some(), "slides": p.plan.segments.len()})),
        "canPlayMusic": dac_slideshow::music::CAN_PLAY,
    })
}

fn now(app: &DacApp) -> f64 {
    app.tasks.repaint.as_ref().map_or(0.0, |c| c.input(|i| i.time))
}

fn stop(app: &mut DacApp) {
    app.ui.slides.playing = None;
    PLAYER.with(|p| p.borrow_mut().take());
}

fn start_music(app: &mut DacApp, repeat: bool) -> Option<String> {
    let m = app.ui.slides.settings.music.clone();
    if !m.enabled || m.tracks.is_empty() {
        return None;
    }
    if !dac_slideshow::music::CAN_PLAY {
        return Some("this build plays slideshows without sound".into());
    }
    let mut all = dac_slideshow::music::Audio::default();
    for t in &m.tracks {
        match dac_slideshow::music::decode(std::path::Path::new(t)) {
            Ok(a) if all.rate == 0 || a.rate == all.rate => {
                all.rate = a.rate;
                all.samples.extend(a.samples);
            }
            Ok(_) => return Some(format!("{t}: a different sample rate than the first track (skipped)")),
            Err(e) => return Some(e),
        }
    }
    dac_slideshow::music::mix(&mut all.samples, m.volume, m.balance);
    match dac_slideshow::music::Player::play(all, repeat) {
        Ok(p) => {
            PLAYER.with(|slot| *slot.borrow_mut() = Some(p));
            None
        }
        Err(e) => Some(e),
    }
}

fn play(app: &mut DacApp, full: bool) -> Result<Value, String> {
    let ids = photos(app);
    if ids.is_empty() {
        return Err("no photos to show".into());
    }
    let s = app.ui.slides.settings.clone();
    let music = if s.music.enabled && s.music.fit_to_music { Some(dac_slideshow::music::total_duration(&s.music.tracks).0) } else { None };
    let seed = (now(app) * 1000.0) as u64 ^ ids.len() as u64;
    let plan = Plan::new(ids.len(), &s, seed, music);
    stop(app);
    let warning = start_music(app, s.playback.repeat);
    let n = plan.segments.len();
    app.ui.slides.playing = Some(Playing { plan, photos: ids, start: now(app), paused_at: None, full });
    Ok(json!({"slides": n, "full": full, "music": warning}))
}

/// Move the show by `delta` segments (manual playback, arrow keys).
fn step(app: &mut DacApp, delta: i64) -> Result<Value, String> {
    let t = now(app);
    let p = app.ui.slides.playing.as_mut().ok_or("no slideshow is playing")?;
    let at = p.paused_at.unwrap_or(t) - p.start;
    let n = p.plan.segments.len() as i64;
    let k = p.plan.frame_at(at).map_or(0, |f| f.index as i64);
    let to = if p.plan.repeat { (k + delta).rem_euclid(n.max(1)) } else { (k + delta).clamp(0, (n - 1).max(0)) };
    let target = p.plan.start_of(to as usize);
    p.start = p.paused_at.unwrap_or(t) - target;
    Ok(json!({"slide": to}))
}

/// Run a `slideshow.*` command; `None`: not one.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !is_command(id) {
        return None;
    }
    Some(run_inner(app, id, p))
}

fn run_inner(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    migrate_saved(app);
    match id {
        "slideshow.get" => Ok(state_json(app)),
        "slideshow.set" => {
            // {settings patch…, usePhotos?}
            if let Some(u) = p.get("usePhotos") {
                app.ui.slides.use_photos = serde_json::from_value(u.clone()).map_err(|_| "usePhotos: all|selected|flagged")?;
            }
            let mut patch = p.clone();
            if let Some(o) = patch.as_object_mut() {
                o.remove("usePhotos");
            }
            app.ui.slides.settings = app.ui.slides.settings.patched(&patch)?;
            Ok(state_json(app))
        }
        "slideshow.reset" => {
            app.ui.slides.settings = Settings::default();
            app.ui.slides.template = "Default".into();
            Ok(state_json(app))
        }
        "slideshow.applyTemplate" => {
            let name = name_param(p)?;
            let t = all_templates(app).into_iter().find(|t| t.name == name).ok_or_else(|| format!("no template named {name}"))?;
            app.ui.slides.settings = t.settings.sanitized();
            app.ui.slides.template = name;
            Ok(state_json(app))
        }
        "slideshow.saveTemplate" => {
            let name = name_param(p)?;
            if builtin_templates().iter().any(|t| t.name == name) {
                return Err(format!("{name} is a built-in template; choose another name"));
            }
            let settings = app.ui.slides.settings.clone();
            let list = &mut app.ui.slides.templates;
            match list.iter_mut().find(|t| t.name == name) {
                Some(t) => t.settings = settings,
                None => list.push(Template { name: name.clone(), settings }),
            }
            app.ui.slides.template = name;
            Ok(state_json(app))
        }
        "slideshow.deleteTemplate" => {
            let name = name_param(p)?;
            let before = app.ui.slides.templates.len();
            app.ui.slides.templates.retain(|t| t.name != name);
            if before == app.ui.slides.templates.len() {
                return Err(format!("no user template named {name}"));
            }
            Ok(state_json(app))
        }
        "slideshow.saveSlideshow" => {
            // a saved slideshow: the settings and the photos in use now
            let name = name_param(p)?;
            let ids: Vec<u64> = photos(app).into_iter().map(|i| i.0).collect();
            if ids.is_empty() {
                return Err("no photos to save in the slideshow".into());
            }
            // a saved creation in the catalog: replaces the one of the same name
            let settings = serde_json::to_value(&app.ui.slides.settings).map_err(|e| e.to_string())?;
            let existing = dac_engine::creations::find_creation(&app.session, dac_layout::CreationKind::Slideshow, &name);
            let cid = match existing {
                Some(c) => {
                    app.run("creation.update", json!({"id": c.id.0, "settings": settings, "ids": ids}))?;
                    c.id.0
                }
                None => app
                    .run("creation.save", json!({"kind": "slideshow", "name": name, "settings": settings, "ids": ids}))?
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or("the slideshow was not saved")?,
            };
            app.ui.slides.open = Some(name);
            app.ui.slides.open_id = Some(cid);
            Ok(state_json(app))
        }
        "slideshow.openSaved" => open_saved(app, p),
        "slideshow.closeSaved" => {
            app.ui.slides.open = None;
            app.ui.slides.open_id = None;
            Ok(state_json(app))
        }
        "slideshow.deleteSaved" => {
            let c = match p.get("id").and_then(Value::as_u64) {
                Some(id) => saved_list(app).into_iter().find(|c| c.id.0 == id),
                None => {
                    let name = name_param(p)?;
                    saved_list(app).into_iter().find(|c| c.name.eq_ignore_ascii_case(&name))
                }
            };
            let c = c.ok_or("no such saved slideshow")?;
            app.run("album.delete", json!({"id": c.id.0}))?;
            if app.ui.slides.open_id == Some(c.id.0) {
                app.ui.slides.open = None;
                app.ui.slides.open_id = None;
            }
            Ok(state_json(app))
        }
        "slideshow.play" => play(app, true),
        "slideshow.preview" => play(app, false),
        "slideshow.stop" => {
            stop(app);
            Ok(Value::Null)
        }
        "slideshow.pause" => {
            let t = now(app);
            let pl = app.ui.slides.playing.as_mut().ok_or("no slideshow is playing")?;
            match pl.paused_at.take() {
                Some(at) => pl.start += t - at,
                None => pl.paused_at = Some(t),
            }
            Ok(json!({"paused": pl.paused_at.is_some()}))
        }
        "slideshow.next" => step(app, 1),
        "slideshow.previous" => step(app, -1),
        "slideshow.addMusic" => {
            let path = p.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let d = dac_slideshow::music::duration(std::path::Path::new(path))?;
            let m = &mut app.ui.slides.settings.music;
            if m.tracks.len() >= 64 {
                return Err("a slideshow takes at most 64 tracks".into());
            }
            m.tracks.push(path.to_string());
            m.enabled = true;
            Ok(json!({"seconds": d, "tracks": m.tracks}))
        }
        "slideshow.clearMusic" => {
            app.ui.slides.settings.music.tracks.clear();
            Ok(state_json(app))
        }
        "slideshow.exportJpeg" => export(app, p, false),
        "slideshow.exportPdf" => export(app, p, true),
        _ => Err(format!("unknown slideshow command: {id}")),
    }
}

/// `slideshow.exportJpeg {dir, width?, height?, quality?, name?}`: the JPEG sequence;
/// `slideshow.exportPdf {…}`: one PDF, a page per slide. The slides are set up here and rendered,
/// composed and written on a worker thread (Activity "export"); the result lands in
/// `export_status` / `export_last`. Returns `{background: true, frames}`.
fn export(app: &mut DacApp, p: &Value, pdf: bool) -> Result<Value, String> {
    let id = if pdf { "slideshow.exportPdf" } else { "slideshow.exportJpeg" };
    let Some(dir) = p.get("dir").and_then(Value::as_str).filter(|d| !d.trim().is_empty()) else {
        let req = crate::pick::PickRequest::folder(crate::i18n::tr("Export Slideshow"));
        return match crate::pick::ask(app, id, p, "dir", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
            crate::pick::Picked::Now(v) => match v.into_iter().next() {
                Some(d) => export(app, &crate::pick::with_answer(p, "dir", crate::pick::PickKind::Folder, &[d]), pdf),
                None => Ok(Value::Null),
            },
            crate::pick::Picked::Later => Ok(Value::Null),
            crate::pick::Picked::Unavailable => Err(format!("no folder dialog here: run {id} {{dir}}")),
        };
    };
    if app.ui.slides.export_busy {
        return Err("a slideshow export is already running".into());
    }
    let num = |k: &str, d: u64| p.get(k).and_then(Value::as_u64).unwrap_or(d);
    let (w, h) = (num("width", 1920) as usize, num("height", 1080) as usize);
    let quality = num("quality", 90).clamp(1, 100) as u8;
    let name = p.get("name").and_then(Value::as_str).map(str::to_string).or_else(|| app.ui.slides.open.clone()).unwrap_or_else(|| "Slideshow".into());
    let ids = photos(app);
    let settings = app.ui.slides.settings.clone();
    let plate = plate_text(app);
    let prep = if pdf { dac_slideshow::export::prepare_pdf } else { dac_slideshow::export::prepare };
    let job = prep(&mut app.session, &ids, &settings, w, h, quality, std::path::Path::new(dir), &name, &plate)?;
    let frames = job.len();
    let dir = dir.to_string();
    app.ui.slides.export_busy = true;
    app.ui.slides.export_status = Some(crate::i18n::tr("Exporting…").to_string());
    let started = crate::tasks::spawn(
        app,
        if pdf { "Exporting PDF slideshow" } else { "Exporting JPEG slideshow" },
        Some("export"),
        move || job.run(&mut |_, _| true),
        move |app, ctx, r: Result<Vec<String>, String>| {
            app.ui.slides.export_busy = false;
            match r {
                Ok(files) => {
                    let msg = if pdf {
                        crate::i18n::tr_format!("PDF in {dir}", dir = dir)
                    } else {
                        crate::i18n::tr_format!("{n} JPEGs in {dir}", n = files.len(), dir = dir)
                    };
                    app.toast(ctx, msg.clone());
                    app.ui.slides.export_status = Some(msg);
                    app.ui.slides.export_last = Some(json!({"frames": frames, "files": files}));
                }
                Err(e) => {
                    app.ui.slides.export_status = Some(e.clone());
                    app.ui.slides.export_last = Some(json!({"error": e}));
                    app.toast_error(ctx, e);
                }
            }
        },
    );
    if let Err(e) = started {
        app.ui.slides.export_busy = false;
        return Err(e);
    }
    Ok(json!({"background": true, "frames": frames}))
}

// ---------------------------------------------------------------- painting

fn c32(c: [u8; 3], a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn to_rect(r: dac_slideshow::compose::Rect, at: Pos2) -> Rect {
    Rect::from_min_max(pos2(at.x + r.x0, at.y + r.y0), pos2(at.x + r.x1, at.y + r.y1))
}

fn align(a: Anchor) -> Align2 {
    match a {
        Anchor::TopLeft => Align2::LEFT_TOP,
        Anchor::Top => Align2::CENTER_TOP,
        Anchor::TopRight => Align2::RIGHT_TOP,
        Anchor::Left => Align2::LEFT_CENTER,
        Anchor::Center => Align2::CENTER_CENTER,
        Anchor::Right => Align2::RIGHT_CENTER,
        Anchor::BottomLeft => Align2::LEFT_BOTTOM,
        Anchor::Bottom => Align2::CENTER_BOTTOM,
        Anchor::BottomRight => Align2::RIGHT_BOTTOM,
    }
}

fn anchored_text(p: &egui::Painter, area: Rect, a: Anchor, inset: f32, text: &str, px: f32, col: Color32, shadow: bool) {
    if text.trim().is_empty() || px < 1.0 {
        return;
    }
    let al = align(a);
    let inner = area.shrink(inset);
    let x = match al.x() {
        egui::Align::Min => inner.left(),
        egui::Align::Center => inner.center().x,
        egui::Align::Max => inner.right(),
    };
    let y = match al.y() {
        egui::Align::Min => inner.top(),
        egui::Align::Center => inner.center().y,
        egui::Align::Max => inner.bottom(),
    };
    let font = egui::FontId::proportional(px);
    if shadow {
        p.text(pos2(x + px * 0.06, y + px * 0.06), al, text, font.clone(), Color32::from_black_alpha((col.a() as f32 * 0.6) as u8));
    }
    p.text(pos2(x, y), al, text, font, col);
}

fn star_shape(c: Pos2, r: f32, col: Color32) -> Vec<egui::Shape> {
    // a star as five triangles around a pentagon (convex pieces for egui)
    let pt = |i: usize, rr: f32| {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        pos2(c.x + rr * a.cos(), c.y + rr * a.sin())
    };
    let inner: Vec<Pos2> = (0..5).map(|k| pt(2 * k + 1, r * 0.42)).collect();
    let mut v = vec![egui::Shape::convex_polygon(inner.clone(), col, Stroke::NONE)];
    for k in 0..5 {
        let tip = pt(2 * k, r);
        let (a, b) = (inner[(k + 4) % 5], inner[k]);
        v.push(egui::Shape::convex_polygon(vec![a, tip, b], col, Stroke::NONE));
    }
    v
}

/// Paint one slide (or title) into `frame` at opacity `alpha`.
fn paint_segment(app: &mut DacApp, ui: &egui::Ui, frame: Rect, seg: Segment, photos: &[PhotoId], progress: f32, alpha: f32, guides: bool) {
    let s = app.ui.slides.settings.clone();
    let p = ui.painter().with_clip_rect(frame);
    let short = frame.width().min(frame.height()).max(1.0);
    let title = |t: &TitleScreen, plate: &str| {
        p.rect_filled(frame, 0.0, c32(t.color, alpha));
        let text = if !t.text.trim().is_empty() {
            t.text.clone()
        } else if t.plate {
            plate.to_string()
        } else {
            String::new()
        };
        anchored_text(&p, frame, Anchor::Center, 0.0, &text, t.size * short, c32(t.text_color, alpha), false);
    };
    let plate = plate_text(app);
    let i = match seg {
        Segment::Intro => return title(&s.titles.intro, &plate),
        Segment::Ending => return title(&s.titles.ending, &plate),
        Segment::Slide(i) => i,
    };
    let Some(&id) = photos.get(i) else { return };
    let ppp = ui.ctx().pixels_per_point();
    let g0 = geometry(frame.width() as usize, frame.height() as usize, 3, 2, &s);
    crate::panels::grid::request_thumb(app, id, (g0.cell.w().max(g0.cell.h()) * ppp) as usize, 9);
    let tex = app.renderer.thumb(id).map(|t| (t.tex.id(), t.tex.size()));
    let (pw, ph) = tex.map_or((3, 2), |(_, sz)| (sz[0], sz[1]));
    let g: Geometry = geometry(frame.width().round() as usize, frame.height().round() as usize, pw, ph, &s);
    let at = frame.min;
    let slide = to_rect(g.slide, at);
    let cell = to_rect(g.cell, at);
    let photo = to_rect(g.photo, at);
    let visible = photo.intersect(cell);
    // backdrop
    let b = &s.backdrop;
    p.rect_filled(slide, 0.0, c32(b.color, alpha));
    if b.wash && b.wash_opacity > 0.0 {
        let a = b.wash_angle.to_radians();
        let (dx, dy) = (a.cos(), -a.sin());
        let mut mesh = egui::Mesh::default();
        let col_at = |q: Pos2| {
            let half = ((slide.width() * dx).abs() + (slide.height() * dy).abs()).max(1.0) / 2.0;
            let t = ((q.x - slide.center().x) * dx + (q.y - slide.center().y) * dy) / half;
            c32(b.wash_color, ((t + 1.0) / 2.0).clamp(0.0, 1.0) * b.wash_opacity * alpha)
        };
        for q in [slide.left_top(), slide.right_top(), slide.right_bottom(), slide.left_bottom()] {
            mesh.colored_vertex(q, col_at(q));
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(egui::Shape::mesh(mesh));
    }
    let o = &s.options;
    if o.shadow && o.shadow_opacity > 0.0 {
        let a = o.shadow_angle.to_radians();
        let off = vec2(a.cos(), -a.sin()) * o.shadow_offset * short;
        let blur = (o.shadow_radius * short).clamp(0.0, 255.0) as u8;
        let shadow = egui::epaint::Shadow {
            offset: [off.x as i8, off.y as i8],
            blur,
            spread: 0,
            color: Color32::from_black_alpha((o.shadow_opacity * alpha * 255.0) as u8),
        };
        p.add(shadow.as_shape(visible, 0.0));
    }
    if o.stroke && o.stroke_width > 0.0 {
        let w = (o.stroke_width * short).max(1.0);
        p.rect_filled(visible.expand(w), 0.0, c32(o.stroke_color, alpha));
    }
    match tex {
        Some((tid, _)) => {
            let (zoom, cx, cy) = if s.playback.pan_zoom { pan_zoom(i, progress, s.playback.pan_zoom_amount) } else { (1.0, 0.5, 0.5) };
            // the visible part of the photo, in UV, after pan and zoom
            let uv_of = |q: Pos2| {
                let u = (q.x - photo.left()) / photo.width().max(1e-3);
                let v = (q.y - photo.top()) / photo.height().max(1e-3);
                pos2(cx + (u - 0.5) / zoom, cy + (v - 0.5) / zoom)
            };
            let uv = Rect::from_min_max(uv_of(visible.min), uv_of(visible.max));
            p.image(tid, visible, uv, Color32::from_white_alpha((alpha * 255.0) as u8));
        }
        None => {
            p.rect_filled(visible, 0.0, Color32::from_gray(60).gamma_multiply(alpha));
        }
    }
    // overlays
    let info = app.session.catalog.photo(id).map(|ph| SlideInfo::from_photo(ph, i + 1, photos.len())).unwrap_or_default();
    let v = &s.overlays;
    if v.identity_plate {
        let text = if v.plate_text.trim().is_empty() { plate.clone() } else { v.plate_text.clone() };
        anchored_text(
            &p,
            slide,
            v.plate_anchor,
            0.02 * short,
            &text,
            v.plate_size * short,
            Color32::from_white_alpha((v.plate_opacity * alpha * 235.0) as u8),
            false,
        );
    }
    if v.rating && info.rating > 0 {
        let r = v.rating_size * short / 2.0;
        let pad = 0.015 * short + r;
        let n = info.rating.min(5);
        for k in 0..n {
            let c = pos2(visible.right() - pad - (n - 1 - k) as f32 * r * 2.2, visible.bottom() - pad);
            for sh in star_shape(c, r, c32(v.rating_color, v.rating_opacity * alpha)) {
                p.add(sh);
            }
        }
    }
    if !v.watermark.trim().is_empty() {
        anchored_text(
            &p,
            visible,
            Anchor::BottomRight,
            0.02 * short,
            &expand(&v.watermark, &info),
            0.025 * short,
            Color32::from_white_alpha((0.6 * alpha * 255.0) as u8),
            true,
        );
    }
    for t in &v.texts {
        let area = if t.on_photo { visible } else { slide };
        anchored_text(&p, area, t.anchor, 0.02 * short, &expand(&t.text, &info), t.size * short, c32(t.color, t.opacity * alpha), t.shadow);
    }
    if guides && s.layout.show_guides {
        let st = Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 170, 255, 160));
        p.rect_stroke(cell, 0.0, st, egui::StrokeKind::Inside);
    }
}

/// Paint the show at its current time into `frame`; returns false when it is over.
fn paint_show(app: &mut DacApp, ui: &egui::Ui, frame: Rect) -> bool {
    let t = ui.ctx().input(|i| i.time);
    let Some(pl) = app.ui.slides.playing.clone() else { return false };
    let at = pl.paused_at.unwrap_or(t) - pl.start;
    let Some(f) = pl.plan.frame_at(at) else { return false };
    let s = app.ui.slides.settings.clone();
    ui.painter().rect_filled(frame, 0.0, Color32::BLACK);
    paint_segment(app, ui, frame, f.current, &pl.photos, f.progress, 1.0, false);
    if let Some((next, k)) = f.next {
        if s.playback.color_fade {
            let c = s.playback.fade_color;
            if k < 0.5 {
                ui.painter().rect_filled(frame, 0.0, c32(c, k * 2.0));
            } else {
                paint_segment(app, ui, frame, next, &pl.photos, 0.0, 1.0, false);
                ui.painter().rect_filled(frame, 0.0, c32(c, (1.0 - k) * 2.0));
            }
        } else {
            paint_segment(app, ui, frame, next, &pl.photos, 0.0, k, false);
        }
    }
    // manual playback holds each slide until a key moves on
    if s.playback.manual
        && pl.paused_at.is_none()
        && f.next.is_some()
        && let Some(p) = app.ui.slides.playing.as_mut()
    {
        p.paused_at = Some(t);
    }
    if f.done {
        return false;
    }
    if pl.paused_at.is_none() {
        ui.ctx().request_repaint();
    }
    true
}

/// Keys while a show plays: Esc ends, Space pauses, ← / → step.
fn play_keys(app: &mut DacApp, ctx: &egui::Context) {
    let (esc, space, left, right) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Space),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
        )
    });
    if esc {
        stop(app);
    } else if space {
        let _ = app.run("slideshow.pause", json!({}));
    } else if right {
        let _ = app.run("slideshow.next", json!({}));
    } else if left {
        let _ = app.run("slideshow.previous", json!({}));
    }
}

/// The full-screen show over everything (when playing full screen).
fn full_screen_show(app: &mut DacApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("slideshow-full")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        crate::access::button(&resp, "End Slideshow");
        register(ui.ctx(), "view:slideshow:play".to_string(), r);
        if !paint_show(app, ui, r) {
            stop(app);
        }
    });
}

// ---------------------------------------------------------------- the module

/// The module's toolbar: photo use, play and preview, export.
pub fn toolbar(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("slideshow-toolbar")
        .exact_size(t.bottom_bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 4)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let n = photos(app).len();
                ui.label(crate::i18n::tr("Use:"));
                let cur = app.ui.slides.use_photos;
                let label = |u: UsePhotos| match u {
                    UsePhotos::All => "All Filmstrip Photos",
                    UsePhotos::Selected => "Selected Photos",
                    UsePhotos::Flagged => "Flagged Photos",
                };
                egui::ComboBox::from_id_salt("slideshow-use").selected_text(crate::i18n::tr(label(cur))).show_ui(ui, |ui| {
                    for u in [UsePhotos::All, UsePhotos::Selected, UsePhotos::Flagged] {
                        if ui.selectable_label(cur == u, crate::i18n::tr(label(u))).clicked() {
                            let _ = app.run("slideshow.set", json!({"usePhotos": u}));
                        }
                    }
                });
                ui.label(crate::i18n::tr_format!("{n} photos", n = n));
                if let Some(name) = app.ui.slides.open.clone() {
                    ui.label(format!("· {name}"));
                    if ui.small_button("✕").clicked() {
                        let _ = app.run("slideshow.closeSaved", json!({}));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let play = ui.button(crate::i18n::tr("Play"));
                    register(ui.ctx(), "button:slideshow.play".to_string(), play.rect);
                    if play.clicked() {
                        run_reporting(app, ui.ctx(), "slideshow.play");
                    }
                    let prev = ui.button(crate::i18n::tr("Preview"));
                    register(ui.ctx(), "button:slideshow.preview".to_string(), prev.rect);
                    if prev.clicked() {
                        run_reporting(app, ui.ctx(), "slideshow.preview");
                    }
                    if app.ui.slides.playing.is_some() && ui.button(crate::i18n::tr("Stop")).clicked() {
                        let _ = app.run("slideshow.stop", json!({}));
                    }
                    let ex = ui.button(crate::i18n::tr("Export JPEG…"));
                    register(ui.ctx(), "button:slideshow.exportJpeg".to_string(), ex.rect);
                    if ex.clicked() {
                        run_reporting(app, ui.ctx(), "slideshow.exportJpeg");
                    }
                    let ex = ui.button(crate::i18n::tr("Export PDF…"));
                    register(ui.ctx(), "button:slideshow.exportPdf".to_string(), ex.rect);
                    if ex.clicked() {
                        run_reporting(app, ui.ctx(), "slideshow.exportPdf");
                    }
                    if let Some(s) = &app.ui.slides.export_status {
                        ui.label(egui::RichText::new(s.clone()).color(t.text_dim));
                    }
                });
            });
        });
}

fn run_reporting(app: &mut DacApp, ctx: &egui::Context, id: &str) {
    match app.run(id, json!({})) {
        Ok(v) => {
            if let Some(w) = v.get("music").and_then(Value::as_str) {
                app.toast(ctx, w.to_string());
            }
        }
        Err(e) => app.toast_error(ctx, e),
    }
}

/// The centre: the side columns (when their edges are shown) and the slide preview.
pub fn center(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    if app.ui.slides.playing.is_some() {
        play_keys(app, &ctx);
    }
    if app.ui.slides.playing.as_ref().is_some_and(|p| p.full) {
        full_screen_show(app, &ctx);
    }
    let frame = egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 8));
    if crate::module::edge_visible(app, crate::module::Edge::Left) {
        egui::Panel::left("slideshow-left").resizable(false).exact_size(220.0).frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("slideshow-left-scroll").show(ui, |ui| left_column(app, ui));
        });
    }
    if crate::module::edge_visible(app, crate::module::Edge::Right) {
        egui::Panel::right("slideshow-right").resizable(false).exact_size(290.0).frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("slideshow-right-scroll").show(ui, |ui| right_column(app, ui));
        });
    }
    let mut area = ui.available_rect_before_wrap();
    if crate::module::edge_visible(app, crate::module::Edge::Bottom) {
        let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
        area.max.y = film.top();
        crate::panels::detail::filmstrip(app, ui, film);
    }
    app.canvas_rect = Some(area);
    ui.allocate_rect(area, Sense::hover());
    register(ui.ctx(), "view:module:slideshow".to_string(), area);
    ui.painter().rect_filled(area, 0.0, t.canvas);
    // the slide: the layout aspect (Screen = the window's) fitted in the canvas
    let aspect = app.ui.slides.settings.layout.aspect.ratio().unwrap_or_else(|| {
        let s = ctx.content_rect();
        s.width() / s.height().max(1.0)
    });
    let avail = area.shrink(24.0);
    let (w, h) = if avail.width() / avail.height().max(1.0) > aspect {
        (avail.height() * aspect, avail.height())
    } else {
        (avail.width(), avail.width() / aspect)
    };
    let frame_r = Rect::from_center_size(avail.center(), vec2(w.max(1.0), h.max(1.0)));
    register(ui.ctx(), "view:slideshow:preview".to_string(), frame_r);
    let previewing = app.ui.slides.playing.as_ref().is_some_and(|p| !p.full);
    if previewing {
        if !paint_show(app, ui, frame_r) {
            stop(app);
        }
        return;
    }
    let ids = photos(app);
    let shown = app.session.active().and_then(|a| ids.iter().position(|x| *x == a)).unwrap_or(0);
    if ids.is_empty() {
        ui.painter().rect_filled(frame_r, 0.0, c32(app.ui.slides.settings.backdrop.color, 1.0));
        ui.painter().text(frame_r.center(), Align2::CENTER_CENTER, crate::i18n::tr("No photos"), t.font(14.0), t.text_dim);
        return;
    }
    paint_segment(app, ui, frame_r, Segment::Slide(shown), &ids, 0.0, 1.0, true);
    ui.painter().text(
        pos2(frame_r.center().x, frame_r.bottom() + 12.0),
        Align2::CENTER_CENTER,
        format!("{} / {}", shown + 1, ids.len()),
        t.font(11.0),
        t.text_dim,
    );
}

fn heading(ui: &mut egui::Ui, text: &str) -> bool {
    let id = ui.make_persistent_id(("slideshow-section", text));
    let r = egui::CollapsingHeader::new(egui::RichText::new(crate::i18n::tr(text)).strong()).id_salt(id).default_open(true).show(ui, |_| ());
    r.fully_open() || r.openness > 0.0
}

fn left_column(app: &mut DacApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(crate::i18n::tr("Template Browser")).strong());
    let current = app.ui.slides.template.clone();
    let builtin: Vec<String> = builtin_templates().into_iter().map(|t| t.name).collect();
    let user: Vec<String> = app.ui.slides.templates.iter().map(|t| t.name.clone()).collect();
    for (name, own) in builtin.iter().map(|n| (n, false)).chain(user.iter().map(|n| (n, true))) {
        // built-in templates show in the UI language, the user's own as named
        let r = ui.selectable_label(*name == current, if own { name.as_str() } else { crate::i18n::tr(name) });
        register(ui.ctx(), format!("slideshowTemplate:{name}"), r.rect);
        if r.clicked() {
            let _ = app.run("slideshow.applyTemplate", json!({"name": name}));
        }
        if own {
            r.context_menu(|ui| {
                if ui.button(crate::i18n::tr("Delete")).clicked() {
                    let _ = app.run("slideshow.deleteTemplate", json!({"name": name}));
                }
            });
        }
    }
    let draft_id = egui::Id::new("slideshow-template-name");
    let mut draft: String = ui.data(|d| d.get_temp(draft_id)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(120.0).hint_text(crate::i18n::tr("Name")));
        crate::access::label(&r, "Template name");
        if ui.button(crate::i18n::tr("Save Template")).clicked()
            && let Err(e) = app.run("slideshow.saveTemplate", json!({"name": draft}))
        {
            app.toast_error(ui.ctx(), e);
        }
    });
    ui.add_space(10.0);
    ui.label(egui::RichText::new(crate::i18n::tr("Saved Slideshows")).strong());
    migrate_saved(app);
    let saved: Vec<(u64, String, usize)> = saved_list(app).into_iter().map(|s| (s.id.0, s.name, s.photos.len())).collect();
    let count = saved.len();
    let open = app.ui.slides.open_id;
    for (id, name, n) in saved {
        let r = ui.selectable_label(open == Some(id), format!("{name} ({n})"));
        if r.clicked() {
            let _ = app.run("slideshow.openSaved", json!({"id": id}));
        }
        r.context_menu(|ui| {
            if ui.button(crate::i18n::tr("Delete")).clicked() {
                let _ = app.run("slideshow.deleteSaved", json!({"id": id}));
            }
        });
    }
    if ui.button(crate::i18n::tr("Create Saved Slideshow")).clicked() {
        let name = if draft.trim().is_empty() { crate::i18n::tr_format!("Slideshow {n}", n = count + 1) } else { draft.clone() };
        if let Err(e) = app.run("slideshow.saveSlideshow", json!({"name": name})) {
            app.toast_error(ui.ctx(), e);
        }
    }
    ui.data_mut(|d| d.insert_temp(draft_id, draft));
}

fn rgb(ui: &mut egui::Ui, c: &mut [u8; 3]) -> bool {
    let r = ui.color_edit_button_srgb(c);
    crate::access::label(&r, "Color");
    r.changed()
}

fn pct(ui: &mut egui::Ui, label: &str, v: &mut f32, max: f32) -> bool {
    ui.add(egui::Slider::new(v, 0.0..=max).text(crate::i18n::tr(label))).changed()
}

fn anchor_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, a: &mut Anchor) -> bool {
    const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];
    let name = |x: Anchor| {
        crate::i18n::tr(match x {
            Anchor::TopLeft => "Top Left",
            Anchor::Top => "Top",
            Anchor::TopRight => "Top Right",
            Anchor::Left => "Left",
            Anchor::Center => "Center",
            Anchor::Right => "Right",
            Anchor::BottomLeft => "Bottom Left",
            Anchor::Bottom => "Bottom",
            Anchor::BottomRight => "Bottom Right",
        })
    };
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(name(*a)).show_ui(ui, |ui| {
        for x in ALL {
            if ui.selectable_label(*a == x, name(x)).clicked() {
                *a = x;
                changed = true;
            }
        }
    });
    changed
}

/// Options, Layout, Overlays, Backdrop, Titles, Playback, Music — edited on a copy, applied with
/// `slideshow.set` when anything changed (so the control channel sees the same path).
fn right_column(app: &mut DacApp, ui: &mut egui::Ui) {
    let before = app.ui.slides.settings.clone();
    let mut s = before.clone();
    let mut ch = false;
    if heading(ui, "Options") {
        ch |= ui.checkbox(&mut s.options.zoom_to_fill, crate::i18n::tr("Zoom to Fill Frame")).changed();
        ui.horizontal(|ui| {
            ch |= ui.checkbox(&mut s.options.stroke, crate::i18n::tr("Stroke Border")).changed();
            ch |= rgb(ui, &mut s.options.stroke_color);
        });
        if s.options.stroke {
            ch |= pct(ui, "Width", &mut s.options.stroke_width, 0.05);
        }
        ch |= ui.checkbox(&mut s.options.shadow, crate::i18n::tr("Cast Shadow")).changed();
        if s.options.shadow {
            ch |= pct(ui, "Opacity", &mut s.options.shadow_opacity, 1.0);
            ch |= pct(ui, "Offset", &mut s.options.shadow_offset, 0.1);
            ch |= pct(ui, "Radius", &mut s.options.shadow_radius, 0.1);
            ch |= ui.add(egui::Slider::new(&mut s.options.shadow_angle, -180.0..=180.0).text(crate::i18n::tr("Angle"))).changed();
        }
    }
    if heading(ui, "Layout") {
        ch |= ui.checkbox(&mut s.layout.show_guides, crate::i18n::tr("Show Guides")).changed();
        ch |= ui.checkbox(&mut s.layout.linked, crate::i18n::tr("Link All")).changed();
        let names = ["Left", "Top", "Right", "Bottom"];
        for k in 0..4 {
            let mut v = s.layout.margins[k];
            if pct(ui, names[k], &mut v, 0.45) {
                ch = true;
                if s.layout.linked {
                    s.layout.margins = [v; 4];
                } else {
                    s.layout.margins[k] = v;
                }
            }
        }
        let aspect = |a: Aspect| {
            crate::i18n::tr(match a {
                Aspect::Screen => "Screen",
                Aspect::Wide16x9 => "Wide (16:9)",
                Aspect::Classic4x3 => "Classic (4:3)",
            })
        };
        egui::ComboBox::from_id_salt("slideshow-aspect").selected_text(aspect(s.layout.aspect)).show_ui(ui, |ui| {
            for a in [Aspect::Screen, Aspect::Wide16x9, Aspect::Classic4x3] {
                if ui.selectable_label(s.layout.aspect == a, aspect(a)).clicked() {
                    s.layout.aspect = a;
                    ch = true;
                }
            }
        });
    }
    if heading(ui, "Overlays") {
        let o = &mut s.overlays;
        ch |= ui.checkbox(&mut o.identity_plate, crate::i18n::tr("Identity Plate")).changed();
        if o.identity_plate {
            let r = ui.add(egui::TextEdit::singleline(&mut o.plate_text).hint_text(crate::i18n::tr("Identity plate text")));
            crate::access::label(&r, "Identity plate text");
            ch |= r.changed();
            ch |= anchor_combo(ui, "slideshow-plate-anchor", &mut o.plate_anchor);
            ch |= pct(ui, "Opacity", &mut o.plate_opacity, 1.0);
            ch |= pct(ui, "Scale", &mut o.plate_size, 0.3);
        }
        ui.horizontal(|ui| {
            ch |= ui.checkbox(&mut o.rating, crate::i18n::tr("Rating Stars")).changed();
            ch |= rgb(ui, &mut o.rating_color);
        });
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Watermark"));
            ch |= ui.text_edit_singleline(&mut o.watermark).changed();
        });
        let mut remove = None;
        for (k, t) in o.texts.iter_mut().enumerate() {
            ui.separator();
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut t.text).desired_width(170.0));
                crate::access::label(&r, "Text");
                ch |= r.changed();
                ch |= rgb(ui, &mut t.color);
                if ui.small_button("✕").clicked() {
                    remove = Some(k);
                }
            });
            ui.horizontal(|ui| {
                ch |= anchor_combo(ui, ("slideshow-text-anchor", k), &mut t.anchor);
                ch |= ui.checkbox(&mut t.on_photo, crate::i18n::tr("On photo")).changed();
                ch |= ui.checkbox(&mut t.shadow, crate::i18n::tr("Shadow")).changed();
            });
            ch |= pct(ui, "Size", &mut t.size, 0.3);
            ch |= pct(ui, "Opacity", &mut t.opacity, 1.0);
        }
        if let Some(k) = remove {
            o.texts.remove(k);
            ch = true;
        }
        ui.horizontal(|ui| {
            if ui.button(crate::i18n::tr("Add Text")).clicked() {
                o.texts.push(TextOverlay::default());
                ch = true;
            }
            ui.menu_button(crate::i18n::tr("Tokens"), |ui| {
                for tok in TOKENS {
                    if ui.button(*tok).clicked() {
                        match o.texts.last_mut() {
                            Some(t) => t.text.push_str(&format!("{{{tok}}}")),
                            None => o.texts.push(TextOverlay { text: format!("{{{tok}}}"), ..TextOverlay::default() }),
                        }
                        ch = true;
                    }
                }
            });
        });
    }
    if heading(ui, "Backdrop") {
        let b = &mut s.backdrop;
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Background Color"));
            ch |= rgb(ui, &mut b.color);
        });
        ui.horizontal(|ui| {
            ch |= ui.checkbox(&mut b.wash, crate::i18n::tr("Color Wash")).changed();
            ch |= rgb(ui, &mut b.wash_color);
        });
        if b.wash {
            ch |= pct(ui, "Opacity", &mut b.wash_opacity, 1.0);
            ch |= ui.add(egui::Slider::new(&mut b.wash_angle, -180.0..=180.0).text(crate::i18n::tr("Angle"))).changed();
        }
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Background Image"));
            let r = ui.add(egui::TextEdit::singleline(&mut b.image).hint_text(crate::i18n::tr("path (export)")));
            crate::access::label(&r, "Background Image");
            ch |= r.changed();
        });
        ch |= pct(ui, "Opacity", &mut b.image_opacity, 1.0);
    }
    if heading(ui, "Titles") {
        for (label, tt) in [("Intro Screen", &mut s.titles.intro), ("Ending Screen", &mut s.titles.ending)] {
            ui.horizontal(|ui| {
                ch |= ui.checkbox(&mut tt.enabled, crate::i18n::tr(label)).changed();
                ch |= rgb(ui, &mut tt.color);
            });
            if tt.enabled {
                ch |= ui.checkbox(&mut tt.plate, crate::i18n::tr("Add Identity Plate")).changed();
                let r = ui.add(egui::TextEdit::singleline(&mut tt.text).hint_text(crate::i18n::tr("Text")));
                crate::access::label(&r, "Text");
                ch |= r.changed();
            }
        }
    }
    if heading(ui, "Playback") {
        let p = &mut s.playback;
        ch |= ui.checkbox(&mut p.manual, crate::i18n::tr("Manual Slideshow")).changed();
        ch |= ui.add(egui::Slider::new(&mut p.slide_secs, 0.5..=30.0).text(crate::i18n::tr("Slides"))).changed();
        ch |= ui.add(egui::Slider::new(&mut p.fade_secs, 0.0..=10.0).text(crate::i18n::tr("Fades"))).changed();
        ui.horizontal(|ui| {
            ch |= ui.checkbox(&mut p.color_fade, crate::i18n::tr("Color")).changed();
            ch |= rgb(ui, &mut p.fade_color);
        });
        ch |= ui.checkbox(&mut p.random, crate::i18n::tr("Random Order")).changed();
        ch |= ui.checkbox(&mut p.repeat, crate::i18n::tr("Repeat Slideshow")).changed();
        ch |= ui.checkbox(&mut p.pan_zoom, crate::i18n::tr("Pan and Zoom")).changed();
        if p.pan_zoom {
            ch |= pct(ui, "Amount", &mut p.pan_zoom_amount, 1.0);
        }
        ch |= ui.checkbox(&mut p.draft, crate::i18n::tr("Draft Quality")).changed();
    }
    if heading(ui, "Music") {
        let m = &mut s.music;
        ch |= ui.checkbox(&mut m.enabled, crate::i18n::tr("Soundtrack")).changed();
        if !dac_slideshow::music::CAN_PLAY {
            ui.label(egui::RichText::new(crate::i18n::tr("This build plays without sound")).small());
        }
        for tr in &m.tracks {
            ui.label(egui::RichText::new(std::path::Path::new(tr).file_name().and_then(|f| f.to_str()).unwrap_or(tr)).small());
        }
        ch |= ui.checkbox(&mut m.fit_to_music, crate::i18n::tr("Fit to Music")).changed();
        ch |= pct(ui, "Volume", &mut m.volume, 1.0);
        ch |= ui.add(egui::Slider::new(&mut m.balance, -1.0..=1.0).text(crate::i18n::tr("Balance"))).changed();
        let path_id = egui::Id::new("slideshow-music-path");
        let mut path: String = ui.data(|d| d.get_temp(path_id)).unwrap_or_default();
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut path).desired_width(160.0).hint_text("track.flac"));
            crate::access::label(&r, "Music");
            if ui.button(crate::i18n::tr("Add")).clicked() {
                match app.run("slideshow.addMusic", json!({"path": path})) {
                    Ok(_) => path.clear(),
                    Err(e) => app.toast_error(ui.ctx(), e),
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(path_id, path));
        if !m.tracks.is_empty() && ui.button(crate::i18n::tr("Clear Music")).clicked() {
            m.tracks.clear();
            ch = true;
        }
    }
    // addMusic changed the settings under us: keep its tracks
    if app.ui.slides.settings.music.tracks != before.music.tracks {
        s.music.tracks = app.ui.slides.settings.music.tracks.clone();
        s.music.enabled = app.ui.slides.settings.music.enabled;
    }
    if ch && s != before {
        match serde_json::to_value(&s) {
            Ok(v) => {
                if let Err(e) = app.run("slideshow.set", v) {
                    app.toast_error(ui.ctx(), e);
                }
            }
            Err(e) => log::warn!("slideshow settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::headless::Headless;
    use crate::{DacApp, Services};

    const T: Duration = Duration::from_secs(20);

    fn demo() -> Headless {
        let services = Services { png: None, ..Default::default() };
        let app = DacApp::new(dac_engine::Session::with_demo(), services);
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        h.settle(Duration::from_secs(120));
        h
    }

    fn exec(h: &mut Headless, id: &str, params: Value) -> Value {
        let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
        h.step();
        r
    }

    fn ok(h: &mut Headless, id: &str, params: Value) -> Value {
        let r = exec(h, id, params);
        assert_eq!(r["ok"], true, "{id}: {r}");
        r["result"].clone()
    }

    fn wait_export(h: &mut Headless) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while h.app.ui.slides.export_busy && std::time::Instant::now() < deadline {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!h.app.ui.slides.export_busy, "export did not finish");
        h.app.ui.slides.export_last.take().unwrap()
    }

    fn has_widget(h: &Headless, id: &str) -> bool {
        h.app.widgets.iter().any(|(w, _)| w == id)
    }

    #[test]
    fn the_module_previews_plays_and_saves() {
        let mut h = demo();
        ok(&mut h, "module.slideshow", json!({}));
        h.settle(Duration::from_secs(60));
        assert!(has_widget(&h, "view:module:slideshow") && has_widget(&h, "view:slideshow:preview"));
        assert!(has_widget(&h, "button:slideshow.play"));
        assert!(!has_widget(&h, "panel:left_panel"), "the Library sources stay away");
        // settings: a partial patch, clamped; bad values are errors
        let r = ok(&mut h, "slideshow.set", json!({"options": {"zoomToFill": true}, "playback": {"slideSecs": 9999}}));
        assert_eq!(r["settings"]["options"]["zoomToFill"], true);
        assert_eq!(r["settings"]["playback"]["slideSecs"], 600.0);
        assert_eq!(exec(&mut h, "slideshow.set", json!({"layout": {"margins": "wide"}}))["ok"], false);
        // templates
        ok(&mut h, "slideshow.applyTemplate", json!({"name": "Exif Metadata"}));
        assert_eq!(h.app.ui.slides.settings.overlays.texts.len(), 3);
        ok(&mut h, "slideshow.saveTemplate", json!({"name": "Mine"}));
        assert_eq!(exec(&mut h, "slideshow.saveTemplate", json!({"name": "Default"}))["ok"], false);
        assert_eq!(exec(&mut h, "slideshow.applyTemplate", json!({"name": "Nope"}))["ok"], false);
        h.step();
        assert!(h.app.ui.slides.templates.iter().any(|t| t.name == "Mine"));
        // a saved slideshow keeps its photos
        let n = ok(&mut h, "slideshow.get", json!({}))["photos"].as_u64().unwrap();
        assert!(n > 0);
        ok(&mut h, "slideshow.saveSlideshow", json!({"name": "Trip"}));
        ok(&mut h, "slideshow.closeSaved", json!({}));
        let r = ok(&mut h, "slideshow.openSaved", json!({"name": "Trip"}));
        assert_eq!(r["photos"].as_u64(), Some(n));
        // the state survives ui.json
        let saved: crate::state::UiState = serde_json::from_value(serde_json::to_value(&h.app.ui).unwrap()).unwrap();
        assert_eq!(saved.slides.templates.len(), 1);
        // the saved slideshow is a saved creation in the catalog, not ui.json
        assert!(saved.slides.saved.is_empty());
        let list = ok(&mut h, "creation.list", json!({"kind": "slideshow"}));
        assert_eq!(list["creations"][0]["name"], "Trip", "{list}");
        assert_eq!(list["creations"][0]["photos"].as_u64(), Some(n));
        // play full screen, step, pause, stop
        let r = ok(&mut h, "slideshow.play", json!({}));
        assert!(r["slides"].as_u64().unwrap() > 0);
        h.step();
        assert!(has_widget(&h, "view:slideshow:play"));
        ok(&mut h, "slideshow.next", json!({}));
        assert_eq!(ok(&mut h, "slideshow.pause", json!({}))["paused"], true);
        ok(&mut h, "slideshow.stop", json!({}));
        assert!(h.app.ui.slides.playing.is_none());
        assert_eq!(exec(&mut h, "slideshow.next", json!({}))["ok"], false);
        // preview in place
        ok(&mut h, "slideshow.preview", json!({}));
        assert!(h.app.ui.slides.playing.as_ref().is_some_and(|p| !p.full));
        ok(&mut h, "slideshow.stop", json!({}));
        // music: unknown formats are errors, not tracks
        assert_eq!(exec(&mut h, "slideshow.addMusic", json!({"path": "/no/such/song.mp3"}))["ok"], false);
        assert!(h.app.ui.slides.settings.music.tracks.is_empty());
        // JPEG sequence
        let dir = std::env::temp_dir().join(format!("dac-slideshow-ui-{}", std::process::id()));
        ok(&mut h, "slideshow.closeSaved", json!({}));
        ok(&mut h, "slideshow.set", json!({"usePhotos": "selected"}));
        let first = h.app.session.visible_cloned()[0].0;
        ok(&mut h, "library.select", json!({"ids": [first]}));
        let r = ok(&mut h, "slideshow.exportJpeg", json!({"dir": dir.display().to_string(), "width": 320, "height": 200}));
        assert_eq!(r["background"], true, "{r}");
        assert!(exec(&mut h, "slideshow.exportPdf", json!({"dir": dir.display().to_string()}))["ok"] == false, "one export at a time");
        let r = wait_export(&mut h);
        assert_eq!(r["files"].as_array().map(Vec::len), Some(1), "{r}");
        // PDF: one file, a page per slide
        ok(&mut h, "slideshow.exportPdf", json!({"dir": dir.display().to_string(), "width": 320, "height": 200, "name": "Show"}));
        let r = wait_export(&mut h);
        let pdf = r["files"][0].as_str().unwrap_or_default().to_string();
        assert!(pdf.ends_with("Show.pdf"), "{r}");
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
        let _ = std::fs::remove_dir_all(dir);
        let img = h.snapshot(T);
        assert!(img.width() > 0);
    }
}
