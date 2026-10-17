//! Video in the loupe and the Edit panel (M14.3 / M14.4 / M14.6): the control bar under the image
//! (play / pause, time, scrub bar with trim handles and the poster marker, frame steps), the
//! grid's video badge, and greying out the develop controls video can't take.
//!
//! The bar is drawn from a plain [`VideoUiState`] and only *asks* for changes: it returns command
//! requests by id (`video.togglePlay`, `video.seek`, `video.setTrim`, `video.stepFrame`,
//! `video.setPoster`), which [`show_bar`] sends through [`LccApp::run`] like every other control.
//! Photos never reach any of this: everything is gated on the catalog kind being video.

use egui::{Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use lcc_catalog::{MediaKind, Photo, PhotoId};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{command_tooltip, icon_button_named, register};

/// The tooltip of a develop control that video can't take.
pub const NOT_FOR_VIDEO: &str = "Not available for video";

/// The bar's height in points.
pub const BAR_H: f32 = 44.0;

/// What the control bar shows. Times are in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoUiState {
    pub duration: f64,
    pub current: f64,
    pub playing: bool,
    pub trim_in: f64,
    /// `None`: the end of the clip.
    pub trim_out: Option<f64>,
    /// `None`: the default poster frame (10 % into the clip).
    pub poster: Option<f64>,
    /// Frames per second (for display; frame steps are the engine's).
    pub fps: f64,
}

impl Default for VideoUiState {
    fn default() -> Self {
        VideoUiState { duration: 0.0, current: 0.0, playing: false, trim_in: 0.0, trim_out: None, poster: None, fps: 30.0 }
    }
}

impl VideoUiState {
    /// The clip's length, never negative or NaN.
    fn len(&self) -> f64 {
        if self.duration.is_finite() { self.duration.max(0.0) } else { 0.0 }
    }
    fn clamp(&self, t: f64) -> f64 {
        if t.is_finite() { t.clamp(0.0, self.len()) } else { 0.0 }
    }
    /// The trim range in effect, `in ≤ out`.
    pub fn trim(&self) -> (f64, f64) {
        let a = self.clamp(self.trim_in);
        let b = self.clamp(self.trim_out.unwrap_or(self.len()));
        (a.min(b), a.max(b))
    }
    /// The poster time in effect.
    pub fn poster_time(&self) -> f64 {
        self.clamp(self.poster.unwrap_or(self.len() * 0.1))
    }

    /// Read `video.get`'s answer; unknown or missing fields keep `fallback`'s values.
    pub fn from_value(v: &Value, fallback: &VideoUiState) -> VideoUiState {
        let num = |keys: &[&str]| keys.iter().find_map(|k| v.get(*k).and_then(Value::as_f64));
        let opt = |keys: &[&str], d: Option<f64>| match keys.iter().find_map(|k| v.get(*k)) {
            Some(Value::Null) => None,
            Some(x) => x.as_f64().or(d),
            None => d,
        };
        VideoUiState {
            duration: num(&["duration"]).unwrap_or(fallback.duration),
            current: num(&["current", "time", "position"]).unwrap_or(fallback.current),
            playing: v.get("playing").and_then(Value::as_bool).unwrap_or(fallback.playing),
            trim_in: num(&["trimIn", "trim_in"]).unwrap_or(fallback.trim_in),
            trim_out: opt(&["trimOut", "trim_out"], fallback.trim_out),
            poster: opt(&["poster", "posterTime", "poster_time"], fallback.poster),
            fps: num(&["fps"]).filter(|f| *f > 0.0).unwrap_or(fallback.fps),
        }
    }
}

/// A command the bar asks for: its id and parameters.
pub type Request = (&'static str, Value);

/// Whether a catalog record is a video.
pub fn is_video(p: &Photo) -> bool {
    p.kind == MediaKind::Video
}

/// Whether the photo with this id is a video.
pub fn photo_is_video(app: &LccApp, id: PhotoId) -> bool {
    app.session.catalog.photo(id).is_some_and(|p| is_video(p))
}

/// A time as `m:ss` (`h:mm:ss` from an hour). Negative, NaN and infinite read `0:00`.
pub fn format_time(t: f64) -> String {
    let s = if t.is_finite() && t > 0.0 { t.floor().min(u32::MAX as f64) as u64 } else { 0 };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The bar's state for a video: `video.get` when the engine has it, else the catalog record.
pub fn state_of(app: &mut LccApp, id: PhotoId) -> Option<VideoUiState> {
    let photo = app.session.catalog.photo(id).filter(|p| is_video(p))?;
    let fallback = VideoUiState { duration: photo.duration.unwrap_or(0.0), ..Default::default() };
    if !has_command("video.get") {
        return Some(fallback);
    }
    match app.session.execute("video.get", &json!({"id": id.0})) {
        Ok(v) => Some(VideoUiState::from_value(&v, &fallback)),
        Err(_) => Some(fallback),
    }
}

fn has_command(id: &str) -> bool {
    lcc_engine::command_specs().iter().any(|c| c.id == id)
}

/// Draw the control bar in `rect` and send what it asks for through the command dispatch.
/// Returns the state after the requests ran (what the loupe shows).
pub fn show_bar(app: &mut LccApp, ui: &mut Ui, rect: Rect, id: PhotoId) -> Option<VideoUiState> {
    let state = state_of(app, id)?;
    let out = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| control_bar(ui, &state)).inner;
    for (cmd, params) in out.requests {
        if let Err(e) = app.run(cmd, params) {
            app.toast(ui.ctx(), e);
        }
    }
    state_of(app, id)
}

/// What [`control_bar`] drew and asked for.
pub struct BarOutput {
    pub requests: Vec<Request>,
    /// The scrub track (time 0 at its left edge, the duration at its right).
    pub track: Rect,
}

/// The track's x for a time.
pub fn time_x(track: Rect, s: &VideoUiState, t: f64) -> f32 {
    let len = s.len();
    if len <= 0.0 {
        return track.left();
    }
    track.left() + (s.clamp(t) / len) as f32 * track.width()
}

fn x_time(track: Rect, s: &VideoUiState, x: f32) -> f64 {
    if track.width() <= 0.0 {
        return 0.0;
    }
    s.clamp(((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * s.len())
}

/// The bar itself: buttons, times and the scrub track over the whole of `ui`'s width.
pub fn control_bar(ui: &mut Ui, s: &VideoUiState) -> BarOutput {
    let t = Tokens::get(ui.ctx());
    let mut requests: Vec<Request> = Vec::new();
    let full = ui.max_rect();
    let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H.min(full.height().max(0.0))));
    ui.painter().rect_filled(bar, 0.0, Color32::from_black_alpha(170));
    register(ui.ctx(), "video:bar", bar);
    let btn = vec2(30.0, 30.0);
    let y = bar.center().y;
    let mut x = bar.left() + 8.0;
    let mut place = |w: f32| {
        let r = Rect::from_center_size(pos2(x + w / 2.0, y), vec2(w, 30.0));
        x += w + 4.0;
        r
    };
    // the buttons, left to right
    let button = |ui: &mut Ui, r: Rect, key: &str, icon: Icon, name: &str, shortcut: Option<&str>| {
        let name_t = crate::i18n::tr(name).to_string();
        let tip = command_tooltip(ui, name, shortcut);
        ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| icon_button_named(ui, key, icon, btn, false, true, &name_t, &tip)).inner.clicked()
    };
    let r = place(30.0);
    if button(ui, r, "video.stepBack", Icon::StepBack, "Previous Frame", Some("Left")) {
        requests.push(("video.stepFrame", json!({"delta": -1})));
    }
    let r = place(30.0);
    let (icon, name) = if s.playing { (Icon::Pause, "Pause") } else { (Icon::Play, "Play") };
    if button(ui, r, "video.togglePlay", icon, name, Some("Space")) {
        requests.push(("video.togglePlay", json!({})));
    }
    let r = place(30.0);
    if button(ui, r, "video.stepForward", Icon::StepForward, "Next Frame", Some("Right")) {
        requests.push(("video.stepFrame", json!({"delta": 1})));
    }
    let time_text = format!("{} / {}", format_time(s.current), format_time(s.len()));
    let g = ui.painter().layout_no_wrap(time_text, t.font(12.0), t.text_label);
    let tr = place(g.size().x + 6.0);
    ui.painter().galley(pos2(tr.left() + 3.0, y - g.size().y / 2.0), g, t.text_label);
    register(ui.ctx(), "video:time", tr);
    // the poster button sits at the right end, the track fills what is between
    let poster_btn = Rect::from_center_size(pos2(bar.right() - 8.0 - 15.0, y), btn);
    if button(ui, poster_btn, "video.setPoster", Icon::Filmstrip, "Set Poster Frame", None) {
        requests.push(("video.setPoster", json!({"time": s.clamp(s.current)})));
    }
    let left = x + 8.0;
    let right = (poster_btn.left() - 14.0).max(left);
    let track = Rect::from_min_max(pos2(left, y - 3.0), pos2(right, y + 3.0));
    register(ui.ctx(), "video:track", track);
    let p = ui.painter().clone();
    p.rect_filled(track, 3.0, Color32::from_gray(70));
    // a trim handle being dragged: drawn where it is, committed (one undo step) on release
    let preview_id = egui::Id::new("video-trim-preview");
    let preview: Option<(f64, f64)> = ui.ctx().data(|d| d.get_temp(preview_id));
    let (a, b) = preview.unwrap_or_else(|| s.trim());
    let (xa, xb) = (time_x(track, s, a), time_x(track, s, b));
    p.rect_filled(Rect::from_min_max(pos2(xa, track.top()), pos2(xb, track.bottom())), 3.0, Color32::from_gray(150));
    // the scrub area: a click or drag seeks
    let hit = track.expand2(vec2(0.0, 10.0));
    let scrub = ui.interact(hit, egui::Id::new("video-scrub"), Sense::click_and_drag());
    crate::access::named(&scrub, egui::WidgetType::Slider, crate::i18n::tr("Playhead"));
    if (scrub.clicked() || scrub.dragged())
        && let Some(pos) = scrub.interact_pointer_pos()
    {
        requests.push(("video.seek", json!({"time": x_time(track, s, pos.x)})));
    }
    // poster marker: a small triangle above the track
    let xp = time_x(track, s, s.poster_time());
    p.add(egui::Shape::convex_polygon(
        vec![pos2(xp - 4.0, track.top() - 9.0), pos2(xp + 4.0, track.top() - 9.0), pos2(xp, track.top() - 3.0)],
        t.accent,
        Stroke::NONE,
    ));
    register(ui.ctx(), "video:poster", Rect::from_center_size(pos2(xp, track.top() - 6.0), vec2(8.0, 6.0)));
    // trim handles (drawn and hit-tested after the scrub area, so they win where they overlap)
    for (key, at, is_in) in [("video:trimIn", xa, true), ("video:trimOut", xb, false)] {
        let hr = Rect::from_center_size(pos2(at, y), vec2(8.0, 20.0));
        register(ui.ctx(), key, hr);
        let resp = ui.interact(hr.expand2(vec2(4.0, 0.0)), egui::Id::new(key), Sense::drag());
        crate::access::named(&resp, egui::WidgetType::Slider, crate::i18n::tr(if is_in { "Trim Start" } else { "Trim End" }));
        let resp = resp.on_hover_text(crate::i18n::tr(if is_in { "Drag to trim the start" } else { "Drag to trim the end" }));
        let color = if resp.hovered() || resp.dragged() { Color32::WHITE } else { Color32::from_rgb(240, 196, 64) };
        p.rect_filled(hr, 2.0, color);
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            // the handles stay at least a frame apart (the engine refuses in ≥ out)
            let gap = if s.fps > 0.0 { 1.0 / s.fps } else { 0.0 };
            let tt = x_time(track, s, pos.x);
            let (ni, no) = if is_in { ((tt.min(b - gap)).max(0.0), b) } else { (a, (tt.max(a + gap)).min(s.len())) };
            ui.ctx().data_mut(|d| d.insert_temp(preview_id, (ni, no)));
        }
        if resp.drag_stopped()
            && let Some((ni, no)) = ui.ctx().data_mut(|d| d.remove_temp::<(f64, f64)>(preview_id))
        {
            // the end at the clip's end is "no trim" (None)
            let out = if (no - s.len()).abs() < 1e-6 { Value::Null } else { json!(no) };
            requests.push(("video.setTrim", json!({"in": ni, "out": out})));
        }
    }
    // the playhead
    let xc = time_x(track, s, s.current);
    p.line_segment([pos2(xc, track.top() - 8.0), pos2(xc, track.bottom() + 8.0)], Stroke::new(2.0, Color32::WHITE));
    register(ui.ctx(), "video:playhead", Rect::from_center_size(pos2(xc, y), vec2(2.0, 22.0)));
    ui.allocate_rect(bar, Sense::hover());
    BarOutput { requests, track }
}

/// Space plays / pauses the active video and ←/→ step a frame while it's paused, in the loupe only.
/// Consumes the keys it uses, so they don't also reach their usual commands. Photos are untouched.
pub fn handle_keys(app: &mut LccApp, ctx: &egui::Context) {
    if app.ui.view != crate::state::ViewMode::Detail {
        return;
    }
    let Some(id) = app.session.active() else { return };
    if !photo_is_video(app, id) {
        return;
    }
    let playing = state_of(app, id).is_some_and(|s| s.playing);
    let none = egui::Modifiers::NONE;
    let (space, left, right) = ctx.input_mut(|i| {
        let space = i.consume_key(none, egui::Key::Space);
        let left = !playing && i.consume_key(none, egui::Key::ArrowLeft);
        let right = !playing && i.consume_key(none, egui::Key::ArrowRight);
        (space, left, right)
    });
    let mut run = |cmd: &str, params: Value| {
        if let Err(e) = app.run(cmd, params) {
            app.toast(ctx, e);
        }
    };
    if space {
        run("video.togglePlay", json!({}));
    }
    if left {
        run("video.stepFrame", json!({"delta": -1}));
    }
    if right {
        run("video.stepFrame", json!({"delta": 1}));
    }
}

/// The grid's video badge: a film icon and the clip's length, at the image's top left.
pub fn grid_badge(ui: &Ui, img: Rect, p: &Photo) {
    if !is_video(p) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let text = p.duration.map(format_time).unwrap_or_default();
    let g = ui.painter().layout_no_wrap(text, t.semibold(10.5), Color32::WHITE);
    let w = 6.0 + 14.0 + if g.size().x > 0.0 { 4.0 + g.size().x } else { 0.0 } + 6.0;
    let br = Rect::from_min_size(img.left_top() + vec2(6.0, 6.0), vec2(w, 20.0));
    if !img.contains_rect(br) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(br, 10.0, Color32::from_black_alpha(165));
    paint(painter, Rect::from_min_size(br.min + vec2(6.0, 3.0), vec2(14.0, 14.0)), Icon::Filmstrip, Color32::WHITE);
    painter.galley(pos2(br.left() + 24.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui.ctx(), format!("badge:video:{}", p.id.0), br);
}

/// The spoken suffix of a video's grid cell (", video, 1:05").
pub fn spoken_suffix(p: &Photo) -> String {
    if !is_video(p) {
        return String::new();
    }
    match p.duration {
        Some(d) => format!(", {}, {}", crate::i18n::tr("video"), format_time(d)),
        None => format!(", {}", crate::i18n::tr("video")),
    }
}

/// Edit panel sections video can take (global grading only; plan §7). The others are greyed out.
pub fn section_allowed(section: &str) -> bool {
    matches!(section, "light" | "color")
}

/// Develop controls (by spec id, or a panel row key) that video can't take: everything outside
/// the global Light and Color controls, plus HDR editing and Point Color.
pub fn control_allowed(id: &str) -> bool {
    let section = id.split('.').next().unwrap_or(id);
    matches!(section, "light" | "wb" | "color" | "curve" | "mixer" | "hsl" | "grading" | "profile")
}

/// UI and engine commands that a clip can't take: the Remove, Masking and Red Eye panels,
/// the local-adjustment and Upright tools, and the engine's photo-only commands.
pub fn photo_only(id: &str) -> bool {
    matches!(id, "panel.remove" | "panel.masking" | "panel.redeye" | "tool.brush" | "tool.linear" | "tool.radial" | "tool.guidedUpright")
        || lcc_engine::cmd::photo_only(id)
}

/// `id` is [`photo_only`] and the active photo is a clip.
pub fn blocked(app: &LccApp, id: &str) -> bool {
    photo_only(id) && app.session.active().is_some_and(|a| photo_is_video(app, a))
}

/// Draw `add` greyed out and inert when `blocked`, with the "Not available for video" tooltip.
pub fn video_gate<R>(ui: &mut Ui, blocked: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    if !blocked {
        return add(ui);
    }
    let r = ui.add_enabled_ui(false, |ui| {
        ui.multiply_opacity(0.45);
        add(ui)
    });
    let _ = r.response.on_hover_text(crate::i18n::tr(NOT_FOR_VIDEO));
    r.inner
}

#[cfg(test)]
#[path = "tests_video_ui.rs"]
mod tests;
