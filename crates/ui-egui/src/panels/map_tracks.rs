//! Track logs on the Map view (Lightroom Classic's Map ▸ Tracklog): the loaded GPX file's tracks
//! drawn as lines, the Tracklog button and its menu (load, pick the track, its time range, the
//! camera's time zone offset, Auto-Tag the selected photos, fit, clear), and small marks where
//! the selected photos' capture times fall on the track.
//!
//! The track log lives in the session (`map.trackLog`, [`lightcraft_engine::tracklog`]): parsed,
//! projected and ranked for simplification once when loaded. Here each frame only takes the
//! points kept at the view's zoom ([`lightcraft_map::Level`], rebuilt when the whole zoom changes)
//! and clips them to the view ([`lightcraft_map::Level::screen_lines`], recomputed when the view
//! moves). Segments (logging gaps) are separate lines; a line crossing the antimeridian is cut
//! there and drawn the short way. With a track picked in the menu the others are drawn dimmed.
//!
//! The marks of the selected photos come from the same reading of capture times and the same
//! track the menu's Auto-Tag uses ([`lightcraft_engine::tracklog::positions`]), so a mark is
//! exactly where Auto-Tag would put that photo; they are recomputed when the selection, the catalog or the track changes, for
//! at most [`MAX_TICKS`] selected photos.

use egui::{Color32, Pos2, Rect, Stroke, pos2};
use lightcraft_geom::Point;
use lightcraft_map::{Bounds, LatLon, Level, Viewport, fit_bounds};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The track's colour (a pink that stands out from the tiles, the orange pins and the blue saved
/// locations).
pub const TRACK: Color32 = Color32::from_rgb(0xff, 0x3d, 0x8b);
const CASING: Color32 = Color32::from_rgba_premultiplied(0x10, 0x10, 0x18, 0xb0);
/// Selected photos marked on the track, at most.
pub const MAX_TICKS: usize = 5000;
/// Tracks listed in the menu (the rest are picked with `map.trackLog {active}`).
const MAX_LISTED: usize = 40;
/// Room drawn around the view so a line's width never shows its clipped end.
const CLIP_MARGIN: f64 = 8.0;

/// What the map draws of the track log.
#[derive(Default)]
pub struct TrackState {
    /// Each track simplified for one zoom, and what for (track generation, zoom).
    levels: Vec<Level>,
    levels_for: Option<(u64, u8)>,
    /// Each track's lines on screen (canvas coordinates), and what for (generation, zoom, view).
    lines: Vec<Vec<Vec<Point>>>,
    lines_for: Option<(u64, u8, [u64; 5])>,
    /// Fit the track on the next frame (it was loaded before the map had a size).
    pub fit_pending: bool,
    /// The selected photos' positions on the track, and what for (generation, catalog revision,
    /// selection).
    ticks: Vec<(u64, LatLon)>,
    ticks_for: Option<(u64, u64, u64)>,
    /// Marks drawn last frame (screen points).
    shown_ticks: Vec<(u64, Pos2)>,
    /// Points handed to the painter last frame, the time to simplify (last rebuild) and to clip
    /// (last view change), ms.
    pub drawn_points: usize,
    pub level_ms: f64,
    pub clip_ms: f64,
}

/// The zoom whose simplification the view uses: the next whole zoom (finer than needed between).
fn level_zoom(vp: &Viewport) -> u8 {
    let z = vp.zoom.ceil();
    if z.is_finite() { z.clamp(0.0, f64::from(lightcraft_map::MAX_ZOOM)) as u8 } else { 0 }
}

/// Draw the track log on the map (after the tiles and saved locations, under the pins).
pub fn draw(app: &mut LightcraftApp, painter: &egui::Painter, t: &Tokens, canvas: Rect, vp: &Viewport) {
    let generation = app.session.map.track_generation;
    let Some(track) = app.session.map.track.as_ref() else {
        let s = &mut app.map.trk;
        if s.levels_for.is_some() {
            *s = TrackState { fit_pending: s.fit_pending, ..Default::default() };
        }
        return;
    };
    let shapes = track.shapes.clone();
    let active = track.active;
    let z = level_zoom(vp);
    let s = &mut app.map.trk;
    if s.levels_for != Some((generation, z)) {
        let t0 = crate::now_ms();
        s.levels = shapes.iter().map(|sh| sh.level(z)).collect();
        s.levels_for = Some((generation, z));
        s.level_ms = crate::now_ms() - t0;
        s.lines_for = None;
    }
    let key = (generation, z, [vp.center.lat.to_bits(), vp.center.lon.to_bits(), vp.zoom.to_bits(), vp.width.to_bits(), vp.height.to_bits()]);
    if s.lines_for != Some(key) {
        let t0 = crate::now_ms();
        s.lines = s.levels.iter().map(|l| l.screen_lines(vp, CLIP_MARGIN)).collect();
        s.lines_for = Some(key);
        s.clip_ms = crate::now_ms() - t0;
    }
    let at = |p: &Point| pos2(canvas.min.x + p.x as f32, canvas.min.y + p.y as f32);
    let mut drawn = 0;
    // the other tracks first (dimmed when one is picked), the active one on top
    let mut order: Vec<usize> = (0..s.lines.len()).filter(|i| Some(*i) != active).collect();
    order.extend(active.filter(|a| *a < s.lines.len()));
    for i in order {
        let on = active.is_none_or(|a| a == i);
        let (w, color) = if on { (3.0, TRACK) } else { (2.0, TRACK.gamma_multiply(0.45)) };
        for line in s.lines.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            let pts: Vec<Pos2> = line.iter().map(at).filter(|p| p.x.is_finite() && p.y.is_finite()).collect();
            drawn += pts.len();
            match pts.as_slice() {
                [] => {}
                [one] => {
                    painter.circle(*one, w + 1.0, color, Stroke::new(1.5, CASING));
                }
                _ => {
                    if on {
                        painter.add(egui::Shape::line(pts.clone(), Stroke::new(w + 2.5, CASING)));
                    }
                    painter.add(egui::Shape::line(pts, Stroke::new(w, color)));
                }
            }
        }
    }
    s.drawn_points = drawn;
    // the selected photos' places on the track
    refresh_ticks(app);
    let mut shown = Vec::new();
    for (id, pos) in &app.map.trk.ticks {
        let p = vp.to_screen(*pos);
        let q = pos2(canvas.min.x + p.x as f32, canvas.min.y + p.y as f32);
        if !canvas.expand(4.0).contains(q) {
            continue;
        }
        painter.circle(q, 4.0, Color32::WHITE, Stroke::new(2.0, t.accent));
        shown.push((*id, q));
    }
    app.map.trk.shown_ticks = shown;
}

/// Recompute the selected photos' positions on the track when the selection, the catalog or the
/// track changed.
fn refresh_ticks(app: &mut LightcraftApp) {
    let sel = &app.session.selection.ids;
    let hash = sel.iter().fold(sel.len() as u64, |h, id| h.rotate_left(7) ^ id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let key = (app.session.map.track_generation, app.session.catalog.revision, hash);
    if app.map.trk.ticks_for == Some(key) {
        return;
    }
    app.map.trk.ticks_for = Some(key);
    app.map.trk.ticks.clear();
    if sel.is_empty() || sel.len() > MAX_TICKS || app.session.map.track.is_none() {
        return;
    }
    let ids = sel.clone();
    app.map.trk.ticks = lightcraft_engine::tracklog::positions(&app.session, &ids)
        .into_iter()
        .filter_map(|(id, lat, lon)| Some((id, LatLon::new(lat, lon)?)))
        .collect();
}

/// The box of the active track (all tracks when none is picked).
fn track_bounds(app: &LightcraftApp) -> Option<Bounds> {
    let t = app.session.map.track.as_ref()?;
    match t.active.and_then(|i| t.shapes.get(i)) {
        Some(sh) => sh.bounds(),
        None => {
            let corners: Vec<(f64, f64)> =
                t.shapes.iter().filter_map(|sh| sh.bounds()).flat_map(|b| [(b.south, b.west), (b.north, b.east)]).collect();
            Bounds::of(corners)
        }
    }
}

/// Fit the map to the track (now if the map has a size, else on its next frame).
pub fn fit(app: &mut LightcraftApp) -> bool {
    let Some(b) = track_bounds(app) else { return false };
    match app.map.rect {
        Some(r) if r.width() > 0.0 && r.height() > 0.0 => {
            let v = fit_bounds(&b, f64::from(r.width()), f64::from(r.height()), super::FIT_MARGIN, super::FIT_MAX_ZOOM.min(super::max_zoom(app)));
            app.map.camera = Some(super::Camera { center: v.center, zoom: v.zoom.floor().max(0.0) });
            app.map.fit_pending = false;
            app.map.trk.fit_pending = false;
            super::apply_view_area(app);
        }
        _ => app.map.trk.fit_pending = true,
    }
    true
}

/// "2026-05-01T12:00:00" → "2026-05-01 12:00:00".
fn local(v: &Value) -> String {
    v.as_str().unwrap_or("").replacen('T', " ", 1)
}

/// "2 h 05 min", "4 min 10 s".
fn duration(secs: f64) -> String {
    let s = if secs.is_finite() { secs.max(0.0).round() as u64 } else { 0 };
    let (h, m, sec) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {sec:02} s")
    } else {
        format!("{sec} s")
    }
}

/// The Tracklog button at `rect` (top right of the map) and its menu.
pub fn button(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let loaded = app.session.map.track.is_some();
    let fill = if loaded { t.tool_active } else { t.chrome.gamma_multiply(0.92) };
    let b = ui.put(rect, egui::Button::new(egui::RichText::new(crate::i18n::tr("Tracklog")).font(t.font(12.5))).fill(fill).selected(loaded));
    register(ui, "map:tracklog", b.rect);
    let hover = match app.session.map.track.as_ref() {
        Some(tr) => {
            let off = tr.offset_minutes.unwrap_or(0);
            let range = tr.span().map(|(a, b)| {
                let (a, b) = (lightcraft_engine::tracklog::iso(a, off), lightcraft_engine::tracklog::iso(b, off));
                format!(" · {} – {}", a.replacen('T', " ", 1), b.replacen('T', " ", 1))
            });
            format!("{}{}", tr.name, range.unwrap_or_default())
        }
        None => crate::i18n::tr("Load a GPS track log (GPX) to show it on the map and tag photos from it").to_string(),
    };
    let b = b.on_hover_text(hover);
    // the menu's contents only while it is open
    egui::Popup::menu(&b).show(|ui| {
        let r = report(app);
        menu(app, ui, &r);
    });
}

fn report(app: &LightcraftApp) -> Value {
    lightcraft_engine::tracklog::report(app.session.map.track.as_ref(), app.session.map.track_generation)
}

fn item(ui: &mut egui::Ui, id: &str, enabled: bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    let b = ui.add_enabled(enabled, egui::Button::new(text));
    register(ui, format!("map:tracklog:{id}"), b.rect);
    b
}

fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, r: &Value) {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(300.0);
    let run = |app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, p: Value| {
        if let Err(e) = app.run(id, p) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    };
    let can_load = app.services.pick_tracklog.is_some() || app.services.picker.is_some();
    if r["loaded"] != true {
        if item(ui, "load", can_load, crate::i18n::tr("Load Tracklog…")).clicked() {
            run(app, ui, "map.loadTrackLog", json!({}));
        }
        return;
    }
    let dim = |s: String| egui::RichText::new(s).font(t.font(11.5)).color(t.text_label);
    let head = ui.label(egui::RichText::new(r["name"].as_str().unwrap_or("")).font(t.semibold(12.5)).color(t.text));
    register(ui, "map:tracklog:name", head.rect);
    // the tracks: all, or one (what Auto-Tag uses and the map highlights)
    let tracks = r["tracks"].as_array().cloned().unwrap_or_default();
    let active = r["active"].as_u64();
    if tracks.len() > 1 {
        let all = ui.add(egui::Button::selectable(active.is_none(), crate::i18n::tr_format!("All Tracks ({n})", n = tracks.len())));
        register(ui, "map:tracklog:track:all", all.rect);
        if all.clicked() && active.is_some() {
            run(app, ui, "map.trackLog", json!({"active": "all"}));
        }
    }
    let off = r["offset"].as_str().map(str::to_string);
    for tr in tracks.iter().take(MAX_LISTED) {
        let i = tr["index"].as_u64().unwrap_or(0);
        let name =
            tr["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| crate::i18n::tr_format!("Track {n}", n = i + 1));
        let text = format!("{name} · {} {}", tr["points"], crate::i18n::tr("points"));
        let on = active == Some(i) || (tracks.len() == 1 && active.is_none());
        let b = ui.add(egui::Button::selectable(on, text));
        register(ui, format!("map:tracklog:track:{i}"), b.rect);
        if b.clicked() && active != Some(i) {
            run(app, ui, "map.trackLog", json!({"active": i}));
        }
    }
    if tracks.len() > MAX_LISTED {
        ui.label(dim(crate::i18n::tr_format!("{n} more tracks", n = tracks.len() - MAX_LISTED)));
    }
    ui.separator();
    // the time range, on the camera's clock (the offset tagging uses)
    let zone = match &off {
        Some(o) => crate::i18n::tr_format!("camera time, UTC{o}", o = o),
        None => crate::i18n::tr("UTC: no time zone offset set").to_string(),
    };
    let info = ui.label(dim(format!(
        "{}  {}\n{}  {}\n{}  {}  ({zone})",
        crate::i18n::tr("Start:"),
        local(&r["startLocal"]),
        crate::i18n::tr("End:"),
        local(&r["endLocal"]),
        crate::i18n::tr("Duration:"),
        duration(r["duration"].as_f64().unwrap_or(0.0)),
    )));
    register(ui, "map:tracklog:info", info.rect);
    let offset_label = match &off {
        Some(o) => crate::i18n::tr_format!("Set Time Zone Offset… (UTC{o})", o = o),
        None => crate::i18n::tr("Set Time Zone Offset…").to_string(),
    };
    if item(ui, "offset", true, offset_label).clicked() {
        run(app, ui, "map.trackLog.setOffset", json!({}));
    }
    ui.separator();
    let n = app.session.targets(&json!({})).unwrap_or_default().len();
    let on_track = app.map.trk.ticks.len();
    let label =
        if n == 1 { crate::i18n::tr("Auto-Tag Selected Photo").to_string() } else { crate::i18n::tr_format!("Auto-Tag {n} Selected Photos", n = n) };
    let b = item(ui, "autoTag", n > 0, label);
    let b = if n > 0 && n <= MAX_TICKS {
        b.on_hover_text(crate::i18n::tr_format!("{m} of them fall within the track's time range", m = on_track))
    } else {
        b
    };
    if b.clicked() {
        run(app, ui, "map.trackLog.autoTag", json!({}));
    }
    if item(ui, "fit", true, crate::i18n::tr("Fit Map to Tracklog")).clicked() {
        run(app, ui, "map.trackLog.fit", json!({}));
    }
    ui.separator();
    if item(ui, "load", can_load, crate::i18n::tr("Load Another Tracklog…")).clicked() {
        run(app, ui, "map.loadTrackLog", json!({}));
    }
    if item(ui, "clear", true, crate::i18n::tr("Clear Tracklog")).clicked() {
        run(app, ui, "map.trackLog", json!({"clear": true}));
    }
}

/// The message after tagging (`photo.autoTagTracklog`'s answer).
pub fn tagged_message(v: &Value) -> String {
    let n = v["tagged"].as_u64().unwrap_or(0);
    let sk = &v["skipped"];
    let mut msg = crate::i18n::tr_format!("Tagged {n} photo{} from the tracklog", if n == 1 { "" } else { "s" }, n = n);
    let outside = sk["outside"].as_u64().unwrap_or(0);
    if outside > 0 {
        msg += &crate::i18n::tr_format!("; {outside} outside its time range", outside = outside);
    }
    let kept = sk["hasGps"].as_u64().unwrap_or(0);
    if kept > 0 {
        msg += &crate::i18n::tr_format!("; {kept} already had a location", kept = kept);
    }
    msg
}

/// The map's track log UI commands (`None`: not one of them).
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    let r = match id {
        "map.trackLog" => {
            // the session's command, then the map fits a newly loaded (or picked) track
            let r = app.session.execute(id, p).map_err(|e| e.to_string());
            let moved = ["path", "gpx", "active"].iter().any(|k| p.get(*k).is_some());
            if r.is_ok() && moved && p.get("fit") != Some(&Value::Bool(false)) {
                fit(app);
            }
            r
        }
        "map.loadTrackLog" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let req = crate::pick::PickRequest::file(crate::i18n::tr("Load Tracklog"), crate::i18n::tr("GPS Track Log"), &["gpx"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_tracklog.as_mut().map(|f| f())) {
                        crate::pick::Picked::Now(v) => v.into_iter().next(),
                        crate::pick::Picked::Later | crate::pick::Picked::Unavailable => None,
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let r = command(app, "map.trackLog", &json!({"path": path}))?;
            if let Err(e) = &r {
                app.toast_error(&ctx, e.clone());
            }
            r
        }
        "map.trackLog.fit" => {
            if fit(app) {
                Ok(super::state(app))
            } else {
                Err("map.trackLog.fit: no track log is loaded".into())
            }
        }
        "map.trackLog.setOffset" => {
            if app.session.map.track.is_none() {
                return Some(Err("map.trackLog.setOffset: no track log is loaded".into()));
            }
            match p.get("offset") {
                Some(o) => app.session.execute("map.trackLog", &json!({"offset": o})).map_err(|e| e.to_string()),
                None => {
                    let cur = app
                        .session
                        .map
                        .track
                        .as_ref()
                        .and_then(|t| t.offset_minutes)
                        .map(lightcraft_engine::tracklog::fmt_offset)
                        .unwrap_or_default();
                    super::super::dialogs::prompt(
                        app,
                        "Set Time Zone Offset",
                        "Camera time zone, e.g. -07:00 (empty: UTC)",
                        &cur,
                        "map.trackLog.setOffset",
                        json!({}),
                        "offset",
                    );
                    Ok(Value::Null)
                }
            }
        }
        "map.trackLog.autoTag" => {
            let Some(track) = app.session.map.track.as_ref() else {
                return Some(Err("map.trackLog.autoTag: no track log is loaded (Tracklog ▸ Load Tracklog…)".into()));
            };
            let has_offset = track.offset_minutes.is_some();
            let mut params = p.as_object().cloned().unwrap_or_default();
            match params.remove("offset") {
                // the camera's zone, asked once for the track (kept with it, so the marks follow)
                Some(o) => {
                    if let Err(e) = app.session.execute("map.trackLog", &json!({"offset": o})) {
                        return Some(Err(e.to_string()));
                    }
                }
                None if !has_offset => {
                    super::super::dialogs::prompt(
                        app,
                        "Auto-Tag from Tracklog",
                        "Camera time zone, e.g. -07:00 (empty: UTC)",
                        "",
                        "map.trackLog.autoTag",
                        Value::Object(params),
                        "offset",
                    );
                    return Some(Ok(Value::Null));
                }
                None => {}
            }
            params.insert("loaded".into(), json!(true));
            let r = app.run("photo.autoTagTracklog", Value::Object(params));
            if let Ok(v) = &r {
                app.toast(&ctx, tagged_message(v));
            }
            r
        }
        _ => return None,
    };
    Some(r)
}

/// `map.state` → `trackLog`: what is loaded and drawn.
pub fn state(app: &mut LightcraftApp, s: &mut Value) {
    let mut r = report(app);
    if r["loaded"] == true {
        let tr = &app.map.trk;
        r["drawn"] = json!({
            "lines": tr.lines.iter().map(|l| l.len()).collect::<Vec<_>>(),
            "points": tr.drawn_points,
            "levelPoints": tr.levels.iter().map(|l| l.points).collect::<Vec<_>>(),
            "zoom": tr.levels_for.map(|(_, z)| z),
            "levelMs": tr.level_ms,
            "clipMs": tr.clip_ms,
        });
        r["ticks"] = json!(tr.shown_ticks.iter().map(|(id, p)| json!({"id": id, "x": p.x, "y": p.y})).collect::<Vec<_>>());
        r["onTrack"] = json!(tr.ticks.len());
    }
    s["trackLog"] = r;
}
