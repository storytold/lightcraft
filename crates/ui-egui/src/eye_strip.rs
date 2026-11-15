//! The eye focus strip (`view.eyeFocus`): across the top of the loupe, each person's sharper eye at
//! 100 %, left to right as they stand, with a dot saying how sharp it is (see
//! [`lcc_engine::eyes`]). A click shows that eye at 100 % in the loupe.
//!
//! The eyes are found on the eye finder's own thread for the photo on screen first, then for the
//! next ones in the direction of travel, and kept per photo, so stepping never waits for them:
//! the strip appears when its photo's result is there.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_engine::eyes::{EyeReport, EyeScanner, Focus, Lids, Verdict};

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Results kept (a few crops each: about 50 kB a photo).
const KEEP: usize = 64;
/// The type size (points) of the verdict under each crop, read at a glance from culling distance
/// rather than fine print.
const LABEL_PT: f32 = 13.5;
/// Height of the verdict line under each crop.
const LABEL_H: f32 = 20.0;
/// The smallest a crop is drawn (points wide) before the group wraps into another row; a big
/// group (more than [`BIG_GROUP`] people) goes down to [`MIN_THUMB_W_BIG`] first.
const MIN_THUMB_W: f32 = 96.0;
const MIN_THUMB_W_BIG: f32 = 76.0;
const BIG_GROUP: usize = 24;

/// The order a big group is shown in: the people to look at first (closed, blurry, half closed,
/// soft), then those whose eyes can't be seen, then the sharp and open ones.
pub fn look_first(v: Verdict) -> u8 {
    match v {
        Verdict::Closed => 0,
        Verdict::Blurry => 1,
        Verdict::HalfClosed => 2,
        Verdict::Soft => 3,
        Verdict::NotVisible => 4,
        Verdict::Sharp => 5,
    }
}

/// The most photos after the one on screen whose eyes are found ahead (how many is set in
/// Settings ▸ Performance ▸ Prepare photos in the background: `prep::ModePrep::eye_ahead`).
pub const AHEAD: usize = lcc_engine::prep::MAX_AHEAD as usize;

/// What the strip keeps between frames.
#[derive(Default)]
pub struct EyeStrip {
    scanner: Option<EyeScanner>,
    /// Result per job key, and when it was last wanted.
    results: HashMap<u64, (Result<Arc<EyeReport>, String>, u64)>,
    tick: u64,
    /// The keys last handed to the scanner, in order.
    wanted: Vec<u64>,
    /// The crops of the photo on screen, as textures.
    textures: Option<(u64, Vec<egui::TextureHandle>)>,
    /// A big group scrolled down this many rows (for the photo of this key).
    scrolled: (u64, usize),
}

impl std::fmt::Debug for EyeStrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EyeStrip").field("results", &self.results.len()).field("wanted", &self.wanted).finish()
    }
}

/// What the strip shows for a photo.
#[derive(Debug, PartialEq)]
pub enum Shown {
    Pending,
    Failed(String),
    NoFaces,
    /// Faces, but none with an eye that can be judged.
    NoEyes(usize),
    Eyes(usize),
}

impl EyeStrip {
    /// The finished report for an eye job's key, when there is one.
    pub fn report(&self, key: u64) -> Option<Arc<EyeReport>> {
        self.results.get(&key).and_then(|(r, _)| r.as_ref().ok()).cloned()
    }

    /// The state for `ui.inspect`.
    pub fn to_json(&self, key: Option<u64>) -> serde_json::Value {
        let r = key.and_then(|k| self.results.get(&k)).map(|(r, _)| r);
        serde_json::json!({
            "pending": key.is_some_and(|k| !self.results.contains_key(&k)),
            "error": r.and_then(|r| r.as_ref().err()),
            "faces": r.and_then(|r| r.as_ref().ok()).map(|r| r.faces),
            "ms": r.and_then(|r| r.as_ref().ok()).map(|r| r.ms),
            "eyes": r.and_then(|r| r.as_ref().ok()).map(|r| r.eyes.iter().map(|e| serde_json::json!({
                "center": e.center, "sharpness": e.sharpness, "focus": e.focus, "bothEyes": e.both_eyes,
                "lids": e.lids, "openness": e.openness, "good": e.good(), "verdict": e.verdict(), "relative": e.relative,
            })).collect::<Vec<_>>()),
            "blinks": r.and_then(|r| r.as_ref().ok()).map(|r| r.blinks()),
            "cached": self.results.len(),
        })
    }

    /// Take in finished results and ask for `jobs` (the photo on screen first). `model` is where
    /// the model is installed.
    fn update(&mut self, model: std::path::PathBuf, jobs: Vec<lcc_engine::eyes::EyeJob>) {
        if self.scanner.as_ref().is_none_or(|s| s.model_dir() != model) {
            self.scanner = EyeScanner::start(model);
            self.wanted.clear();
        }
        self.tick += 1;
        if let Some(s) = &self.scanner {
            for (key, _, r) in s.finished() {
                self.results.insert(key, (r, self.tick));
            }
        }
        for j in &jobs {
            if let Some(e) = self.results.get_mut(&j.key) {
                e.1 = self.tick;
            }
        }
        let todo: Vec<_> = jobs.into_iter().filter(|j| !self.results.contains_key(&j.key)).collect();
        let keys: Vec<u64> = todo.iter().map(|j| j.key).collect();
        if keys != self.wanted
            && let Some(s) = &self.scanner
        {
            s.want(todo);
            self.wanted = keys;
        }
        self.trim();
    }

    /// Keep the [`KEEP`] results wanted last.
    fn trim(&mut self) {
        if self.results.len() > KEEP {
            let mut ages: Vec<(u64, u64)> = self.results.iter().map(|(k, v)| (v.1, *k)).collect();
            ages.sort_unstable();
            for (_, k) in ages.into_iter().take(self.results.len() - KEEP) {
                self.results.remove(&k);
            }
        }
    }

    fn shown(&self, key: u64) -> Shown {
        match self.results.get(&key).map(|(r, _)| r) {
            None => Shown::Pending,
            Some(Err(e)) => Shown::Failed(e.clone()),
            Some(Ok(r)) if r.eyes.is_empty() && r.faces == 0 => Shown::NoFaces,
            Some(Ok(r)) if r.eyes.is_empty() => Shown::NoEyes(r.no_eyes.max(1)),
            Some(Ok(r)) => Shown::Eyes(r.eyes.len()),
        }
    }
}

/// The strip's colour for a focus.
pub fn focus_colour(f: Focus) -> Color32 {
    match f {
        Focus::Sharp => Color32::from_rgb(0x3f, 0xb9, 0x50),
        Focus::Soft => Color32::from_rgb(0xe3, 0xa0, 0x08),
        Focus::Blurry => Color32::from_rgb(0xf8, 0x51, 0x49),
    }
}

/// A person's frame colour: the worse of lids and focus (closed is red however sharp), grey when
/// no eye shows (sunglasses).
pub fn verdict_colour(v: Verdict) -> Color32 {
    match v {
        Verdict::Sharp => focus_colour(Focus::Sharp),
        Verdict::Soft | Verdict::HalfClosed => focus_colour(Focus::Soft),
        Verdict::Blurry | Verdict::Closed => focus_colour(Focus::Blurry),
        Verdict::NotVisible => Color32::from_gray(150),
    }
}

/// The closed-eye marker's word, `None` for an open eye (or one whose lids couldn't be measured).
fn lids_word(l: Lids) -> Option<&'static str> {
    match l {
        Lids::Closed => Some(crate::i18n::tr("Closed")),
        Lids::Half => Some(crate::i18n::tr("Half closed")),
        Lids::Open | Lids::Unknown | Lids::Hidden => None,
    }
}

/// A small closed eye (a lid's curve with lashes) at `c`, `w` wide.
fn closed_eye_icon(p: &egui::Painter, c: egui::Pos2, w: f32, colour: Color32) {
    let pts: Vec<egui::Pos2> = (0..=8)
        .map(|i| {
            let t = i as f32 / 8.0;
            pos2(c.x - w / 2.0 + t * w, c.y - (std::f32::consts::PI * t).sin() * w * 0.22 + w * 0.08)
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.4, colour)));
    for t in [0.25f32, 0.5, 0.75] {
        let x = c.x - w / 2.0 + t * w;
        let y = c.y - (std::f32::consts::PI * t).sin() * w * 0.22 + w * 0.08;
        p.line_segment([pos2(x, y), pos2(x, y + w * 0.2)], Stroke::new(1.2, colour));
    }
}

fn focus_word(f: Focus) -> &'static str {
    match f {
        Focus::Sharp => crate::i18n::tr("Sharp"),
        Focus::Soft => crate::i18n::tr("Soft"),
        Focus::Blurry => crate::i18n::tr("Blurry"),
    }
}

/// Lay out `n` thumbnails `w × h` (points) with `gap` between them in `width`: their size, scaled
/// down (never up) so they fit.
pub fn fit_thumbs(n: usize, w: f32, h: f32, gap: f32, width: f32) -> (f32, f32) {
    if n == 0 || !(w > 0.0 && h > 0.0) || !width.is_finite() {
        return (w.max(0.0), h.max(0.0));
    }
    let need = n as f32 * w + (n as f32 + 1.0) * gap;
    let s = ((width - (n as f32 + 1.0) * gap).max(1.0) / (n as f32 * w)).min(1.0);
    if need <= width { (w, h) } else { (w * s, h * s) }
}

/// How the strip lays out `n` crops: `cols` across, each `w × h`, and how many are `shown`.
#[derive(Debug, PartialEq)]
pub struct StripGrid {
    pub cols: usize,
    pub w: f32,
    pub h: f32,
    pub shown: usize,
}

/// Lay out `n` crops of `w × h` (points, at 100 %) with `gap` between them and a `label_h` line
/// under each, in `width` × `max_h`: one row while each crop can stay [`MIN_THUMB_W`] wide, else
/// rows of crops that size, as many rows as fit (a group of a hundred people still shows the people
/// to look at; the rest are counted).
pub fn strip_grid(n: usize, w: f32, h: f32, gap: f32, label_h: f32, width: f32, max_h: f32) -> StripGrid {
    if n == 0 || !(w > 0.0 && h > 0.0 && width.is_finite() && max_h.is_finite()) {
        return StripGrid { cols: n.max(1), w: w.max(0.0), h: h.max(0.0), shown: n };
    }
    let (fw, fh) = fit_thumbs(n, w, h, gap, width);
    if fw >= (if n > BIG_GROUP { MIN_THUMB_W_BIG } else { MIN_THUMB_W }).min(w) {
        return StripGrid { cols: n, w: fw, h: fh, shown: n };
    }
    let tw = (if n > BIG_GROUP { MIN_THUMB_W_BIG } else { MIN_THUMB_W }).min(w);
    let th = h * tw / w;
    let cols = (((width - gap) / (tw + gap)).floor().max(1.0)) as usize;
    let rows_fit = (((max_h - gap * 0.5) / (th + label_h + gap)).floor().max(1.0)) as usize;
    let shown = n.min(cols.saturating_mul(rows_fit));
    StripGrid { cols, w: tw, h: th, shown }
}

/// The scroll bar of a band that holds more rows than it shows: along its right edge.
fn scroll_track(band: Rect) -> Rect {
    Rect::from_min_max(pos2(band.right() - 9.0, band.top() + 6.0), pos2(band.right() - 3.0, band.bottom() - 6.0))
}

/// The first row shown after a drag of `moved` rows from row `from`, kept within `0..=max_first`.
pub fn rows_after(from: usize, moved: f32, max_first: usize) -> usize {
    if !moved.is_finite() {
        return from.min(max_first);
    }
    let to = from as f32 + moved;
    if to <= 0.0 { 0 } else { (to as usize).min(max_first) }
}

/// The first row shown when the bar `track` is pressed at `y`: the rows shown centred there.
pub fn row_at(track: Rect, y: f32, rows_all: usize, rows_shown: usize) -> usize {
    let max_first = rows_all.saturating_sub(rows_shown);
    if track.height() <= 0.0 || !y.is_finite() || rows_all == 0 {
        return 0;
    }
    let f = ((y - track.top()) / track.height()).clamp(0.0, 1.0) * rows_all as f32 - rows_shown as f32 / 2.0;
    rows_after(0, f.round(), max_first)
}

/// Draw the strip across the top of `canvas` for photo `id` (`upcoming`: the photos likely next).
pub fn show(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, id: PhotoId, upcoming: &[PhotoId]) {
    let t = Tokens::get(ui.ctx());
    // the messages sit under the activity stack's cards ("Finding faces"), never behind them
    let top = message_top(canvas, app.activity_bottom);
    let Some(model) = app.session.eye_model_dir() else {
        needs_model(app, ui, canvas, top);
        return;
    };
    let Some(job) = app.session.eye_job(id) else { return };
    let key = job.key;
    let mut jobs = vec![job];
    jobs.extend(upcoming.iter().take(AHEAD).filter_map(|p| app.session.eye_job(*p)));
    app.eye_strip.update(model, jobs);
    app.eye_key = Some(key);
    let shown = app.eye_strip.shown(key);
    let pill = |ui: &egui::Ui, text: String| {
        let p = ui.painter_at(canvas);
        let g = p.layout_no_wrap(text, t.font(12.0), Color32::from_gray(225));
        let r = Rect::from_center_size(pos2(canvas.center().x, top + 20.0), g.size() + vec2(20.0, 8.0));
        p.rect_filled(r, 10.0, Color32::from_black_alpha(170));
        p.galley(r.min + vec2(10.0, 4.0), g, Color32::from_gray(225));
        register(ui.ctx(), "eyes:status", r);
    };
    match shown {
        Shown::Pending => {
            pill(ui, crate::i18n::tr("Finding eyes…").to_string());
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        }
        Shown::Failed(e) => pill(ui, crate::i18n::tr_format!("Eye focus: {}", e)),
        Shown::NoFaces => pill(ui, crate::i18n::tr("No faces").to_string()),
        Shown::NoEyes(_) => pill(ui, crate::i18n::tr("No eye to judge (faces turned away)").to_string()),
        Shown::Eyes(_) => thumbs(app, ui, canvas, key),
    }
}

/// Find the eyes of photo `id` in the background with the strip off (Space zooms to them:
/// `panels::detail::focus_pan`). Nothing without the face model; never waited for.
pub fn find_quietly(app: &mut LccApp, id: PhotoId) {
    let Some(model) = app.session.eye_model_dir() else { return };
    let Some(job) = app.session.eye_job(id) else { return };
    let key = job.key;
    app.eye_strip.update(model, vec![job]);
    // (a frame now and then picks up the result)
    if app.eye_strip.shown(key) == Shown::Pending
        && let Some(ctx) = app.tasks.repaint.as_ref()
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// Reject Closed and Blurry Eyes running in the background (`photo.rejectEyes`): when it ends, its
/// rejects are flagged in one undo step and its summary shown. Cheap every frame.
pub fn reject_pump(app: &mut LccApp) {
    let Ok(v) = app.session.execute("photo.rejectEyesPump", &serde_json::json!({})) else { return };
    if v["done"] != true {
        return;
    }
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    let text = if n("rejected") == 0 {
        crate::i18n::tr("No closed or blurry eyes to reject").to_string()
    } else {
        crate::i18n::tr_format!(
            "Rejected {n}: {closed} with closed eyes, {blurry} with blurry eyes",
            n = n("rejected"),
            closed = n("closedEyes"),
            blurry = n("blurryEyes")
        )
    };
    let now = app.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(app.last_time);
    app.ui.show_toast((text, now + crate::TOAST_SECONDS, None));
}

/// The top the strip's messages are laid out from: the loupe's top, or under the activity stack.
pub fn message_top(canvas: Rect, activity_bottom: Option<f32>) -> f32 {
    match activity_bottom {
        Some(b) if b.is_finite() && b + 4.0 > canvas.top() => (b + 4.0).min(canvas.bottom() - 40.0).max(canvas.top()),
        _ => canvas.top(),
    }
}

/// The "needs the model" strip, with its button.
fn needs_model(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, top: f32) {
    let t = Tokens::get(ui.ctx());
    let text = crate::i18n::tr("Eye focus needs the face AI model");
    let g = ui.painter().layout_no_wrap(text.to_string(), t.font(12.0), Color32::from_gray(225));
    let r = Rect::from_center_size(pos2(canvas.center().x, top + 22.0), vec2(g.size().x + 200.0, 30.0));
    ui.painter_at(canvas).rect_filled(r, 8.0, Color32::from_black_alpha(170));
    ui.painter_at(canvas).galley(pos2(r.left() + 12.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    let b = Rect::from_min_max(pos2(r.right() - 176.0, r.top() + 4.0), pos2(r.right() - 6.0, r.bottom() - 4.0));
    register(ui.ctx(), "eyes:getModel", b);
    if ui.put(b, egui::Button::new(crate::i18n::tr("Get the AI Model…")).small()).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
    }
}

fn thumbs(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, key: u64) {
    let t = Tokens::get(ui.ctx());
    let Some(Ok(report)) = app.eye_strip.results.get(&key).map(|(r, _)| r.clone()) else { return };
    if app.eye_strip.textures.as_ref().is_none_or(|(k, _)| *k != key) {
        let tex = report
            .eyes
            .iter()
            .enumerate()
            .map(|(i, e)| {
                ui.ctx().load_texture(
                    format!("eye-{key}-{i}"),
                    egui::ColorImage::from_rgba_unmultiplied([e.crop.width, e.crop.height], &e.crop.as_bytes()),
                    egui::TextureOptions::NEAREST,
                )
            })
            .collect();
        app.eye_strip.textures = Some((key, tex));
    }
    let Some((_, tex)) = app.eye_strip.textures.clone() else { return };
    let ppp = ui.ctx().pixels_per_point().max(0.5);
    let n = report.eyes.len();
    let (gap, label_h) = (8.0, LABEL_H);
    // no summary text beside the crops: the photographer glances at the band, and each crop's
    // colour and closed-eye marker already say what matters
    let room = (canvas.width() - 24.0).max(1.0);
    let max_h = canvas.height() * 0.45;
    // a big group wraps into rows of smaller crops; past what fits, the people to look at come
    // first and the rest scroll (wheel, drag, or the bar at the band's right edge)
    let grid = strip_grid(n, lcc_engine::eyes::CROP_W as f32 / ppp, lcc_engine::eyes::CROP_H as f32 / ppp, gap, label_h, room, max_h);
    let (w, h) = (grid.w, grid.h);
    let cols = grid.cols.max(1);
    let row_h = h + label_h + gap;
    let mut order: Vec<usize> = (0..n).collect();
    let scrollable = grid.shown < n;
    let rows_all = n.div_ceil(cols).max(1);
    let rows_shown = if scrollable { grid.shown.div_ceil(cols).max(1) } else { rows_all };
    let max_first = rows_all.saturating_sub(rows_shown);
    let mut first_row = 0;
    if scrollable {
        order.sort_by_key(|i| (report.eyes.get(*i).map_or(9, |e| look_first(e.verdict())), *i));
        first_row = if app.eye_strip.scrolled.0 == key { app.eye_strip.scrolled.1.min(max_first) } else { 0 };
    }
    let thumbs_w = cols.min(n.max(1)) as f32 * (w + gap) + gap;
    let band_w = thumbs_w.min(canvas.width() - 12.0).max(1.0);
    let band_h = rows_shown as f32 * row_h + gap * 0.5;
    let band = Rect::from_min_size(pos2(canvas.center().x - band_w / 2.0, message_top(canvas, app.activity_bottom) + 6.0), vec2(band_w, band_h));
    if scrollable {
        // the wheel over the band, a drag on it, or a press / drag on its bar move the rows
        let wheel = ui.input(|i| {
            if i.pointer.hover_pos().is_some_and(|p| band.contains(p)) {
                i.events.iter().map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 }).sum::<f32>()
            } else {
                0.0
            }
        });
        if wheel < 0.0 {
            first_row = (first_row + 1).min(max_first);
        } else if wheel > 0.0 {
            first_row = first_row.saturating_sub(1);
        }
        let drag = ui.interact(band, egui::Id::new(("eye-strip-drag", key)), Sense::drag());
        let start_id = egui::Id::new(("eye-strip-drag-from", key));
        if drag.drag_started() {
            ui.ctx().data_mut(|d| d.insert_temp(start_id, first_row));
        }
        if drag.dragged() {
            let from = ui.ctx().data(|d| d.get_temp::<usize>(start_id)).unwrap_or(first_row);
            let moved = (-drag.total_drag_delta().map_or(0.0, |d| d.y) / row_h.max(1.0)).round();
            first_row = rows_after(from, moved, max_first);
        }
        let track = scroll_track(band);
        let bar = ui.interact(track.expand2(vec2(4.0, 0.0)), egui::Id::new(("eye-strip-bar", key)), Sense::click_and_drag());
        register(ui.ctx(), "eyes:scrollbar", track);
        if (bar.clicked() || bar.dragged())
            && let Some(p) = bar.interact_pointer_pos()
        {
            first_row = row_at(track, p.y, rows_all, rows_shown);
        }
        app.eye_strip.scrolled = (key, first_row);
        order = order.into_iter().skip(first_row.saturating_mul(cols)).take(rows_shown.saturating_mul(cols)).collect();
    }
    let first_shown = order.first().copied().unwrap_or(0);
    let p = ui.painter_at(canvas);
    p.rect_filled(band, 8.0, Color32::from_black_alpha(175));
    register(ui.ctx(), "eyes:strip", band);
    if scrollable {
        // a bar at the band's right edge: where these rows are in the group
        let track = scroll_track(band);
        let a = track.top() + track.height() * first_row as f32 / rows_all as f32;
        let b = (a + track.height() * rows_shown as f32 / rows_all as f32).min(track.bottom());
        p.rect_filled(track, 3.0, Color32::from_white_alpha(30));
        p.rect_filled(Rect::from_x_y_ranges(track.x_range(), a..=b), 3.0, Color32::from_white_alpha(170));
    }
    for (slot, i) in order.into_iter().enumerate() {
        let (Some(e), Some(tx)) = (report.eyes.get(i), tex.get(i)) else { continue };
        let (col, row) = (slot % cols, slot / cols);
        let r =
            Rect::from_min_size(pos2(band.left() + gap + col as f32 * (w + gap), band.top() + gap + row as f32 * (h + label_h + gap)), vec2(w, h));
        // the crop is shown whole (smaller at the picture's edges: centred in its place)
        let [cw, ch] = tx.size();
        let s = (w / (cw as f32 / ppp)).min(h / (ch as f32 / ppp)).min(1.0);
        let img = Rect::from_center_size(r.center(), vec2(cw as f32 / ppp * s, ch as f32 / ppp * s));
        p.image(tx.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let c = verdict_colour(e.verdict());
        p.rect_stroke(r, 3.0, Stroke::new(2.0, c), StrokeKind::Outside);
        p.circle_filled(pos2(r.right() - 7.0, r.top() + 7.0), 4.5, c);
        p.circle_stroke(pos2(r.right() - 7.0, r.top() + 7.0), 4.5, Stroke::new(1.0, Color32::from_black_alpha(160)));
        p.text(
            pos2(r.center().x, r.bottom() + label_h / 2.0 + 1.0),
            Align2::CENTER_CENTER,
            if e.lids == Lids::Hidden {
                crate::i18n::tr("Eyes not visible").to_string()
            } else {
                format!("{} {:.0}", focus_word(e.focus), e.sharpness)
            },
            t.font(LABEL_PT),
            Color32::from_gray(230),
        );
        // closed or half closed: a marker of its own, apart from the sharpness colour
        if let Some(word) = lids_word(e.lids) {
            let g = p.layout_no_wrap(word.to_string(), t.semibold(11.5), Color32::WHITE);
            let pill = Rect::from_min_size(pos2(r.left() + 3.0, r.bottom() - 21.0), vec2(g.size().x + 28.0, 18.0));
            let fill = if e.lids == Lids::Closed {
                Color32::from_rgba_unmultiplied(120, 40, 160, 230)
            } else {
                Color32::from_rgba_unmultiplied(90, 60, 140, 210)
            };
            p.rect_filled(pill, 7.0, fill);
            closed_eye_icon(&p, pos2(pill.left() + 11.0, pill.center().y), 13.0, Color32::WHITE);
            p.galley(pos2(pill.left() + 21.0, pill.center().y - g.size().y / 2.0), g, Color32::WHITE);
            register(ui.ctx(), format!("eyes:closed:{i}"), pill);
        }
        // the person's verdict: sharp and open is a keeper
        let v = pos2(r.left() + 8.0, r.top() + 8.0);
        if e.good() {
            p.circle_filled(v, 6.0, Color32::from_black_alpha(150));
            p.line_segment([v + vec2(-3.0, 0.0), v + vec2(-1.0, 2.5)], Stroke::new(1.6, focus_colour(Focus::Sharp)));
            p.line_segment([v + vec2(-1.0, 2.5), v + vec2(3.5, -2.5)], Stroke::new(1.6, focus_colour(Focus::Sharp)));
        }
        let id = format!("eyes:thumb:{i}");
        register(ui.ctx(), &id, r);
        let resp = ui.interact(r, egui::Id::new(("eye-thumb", i)), Sense::click()).on_hover_text(crate::i18n::tr("Click to see this eye at 100 %"));
        if resp.clicked() {
            zoom_to(app, e.center);
        }
    }
    controls(app, ui, canvas, band, first_shown);
}

/// The strip's controls, shown while the pointer is over it: ✕ hides the strip (and the quick
/// settings bar flashes where it turns back on: `view.dismissEyeFocus`), the eye shows the first
/// eye shown at 100 % (`view.zoomToEye`). Beside the band's top-right corner, or inside it when the
/// band fills the loupe.
fn controls(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, band: Rect, first: usize) {
    const SIDE: f32 = 22.0;
    let (close, eye) = if band.right() + 6.0 + SIDE <= canvas.right() - 4.0 {
        let close = Rect::from_min_size(pos2(band.right() + 6.0, band.top()), vec2(SIDE, SIDE));
        (close, close.translate(vec2(0.0, SIDE + 4.0)))
    } else {
        let close = Rect::from_min_size(pos2(band.right() - SIDE - 4.0, band.top() + 4.0), vec2(SIDE, SIDE));
        (close, close.translate(vec2(-(SIDE + 4.0), 0.0)))
    };
    let hovered = ui.rect_contains_pointer(band.union(close).union(eye).expand(2.0));
    let a = ui.ctx().animate_bool_with_time(egui::Id::new("eye-strip-controls"), hovered, crate::quick_bar::FADE_SECS);
    if a <= 0.0 {
        return;
    }
    let p = ui.painter_at(canvas);
    let mut run = None;
    for (rect, id, label) in [(close, "eyes:dismiss", "Hide Eye Focus Strip"), (eye, "eyes:zoom", "Zoom to Eye")] {
        register(ui.ctx(), id, rect);
        let resp = ui.interact(rect, egui::Id::new(id), if a >= 0.5 { Sense::click() } else { Sense::hover() });
        crate::access::button(&resp, label);
        let fill = if resp.hovered() { 220.0 } else { 170.0 };
        p.circle_filled(rect.center(), SIDE / 2.0, Color32::from_black_alpha((fill * a) as u8));
        let ink = Color32::from_gray(235).gamma_multiply(a);
        if id == "eyes:dismiss" {
            let k = SIDE * 0.2;
            let c = rect.center();
            p.line_segment([c + vec2(-k, -k), c + vec2(k, k)], Stroke::new(1.6, ink));
            p.line_segment([c + vec2(k, -k), c + vec2(-k, k)], Stroke::new(1.6, ink));
        } else {
            crate::icons::paint(&p, Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), crate::icons::Icon::Eye, ink);
        }
        if resp.on_hover_text(crate::i18n::tr(label)).clicked() {
            run = Some(id);
        }
    }
    if a < 1.0 {
        ui.ctx().request_repaint();
    }
    let r = match run {
        Some("eyes:dismiss") => app.run("view.dismissEyeFocus", serde_json::json!({})),
        Some(_) => app.run("view.zoomToEye", serde_json::json!({"index": first})),
        None => return,
    };
    if let Err(e) = r {
        app.ui.status = e;
    }
}

/// Show the point `center` (normalised to the picture) at 100 % in the loupe.
pub fn zoom_to(app: &mut LccApp, center: [f32; 2]) {
    if center.iter().all(|v| v.is_finite()) {
        app.ui.zoom = crate::state::Zoom::Percent(100.0);
        app.ui.pan = (center[0].clamp(0.0, 1.0), center[1].clamp(0.0, 1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_sit_under_the_activity_stack() {
        let canvas = Rect::from_min_size(pos2(0.0, 40.0), vec2(800.0, 600.0));
        assert_eq!(message_top(canvas, None), 40.0);
        assert_eq!(message_top(canvas, Some(10.0)), 40.0);
        let top = message_top(canvas, Some(120.0));
        assert!(top >= 120.0, "{top}");
        // never pushed off the loupe, junk ignored
        assert!(message_top(canvas, Some(5000.0)) < canvas.bottom());
        assert_eq!(message_top(canvas, Some(f32::NAN)), 40.0);
    }

    #[test]
    fn thumbnails_shrink_to_fit_and_never_grow() {
        assert_eq!(fit_thumbs(3, 160.0, 100.0, 8.0, 2000.0), (160.0, 100.0));
        let (w, h) = fit_thumbs(8, 160.0, 100.0, 8.0, 800.0);
        assert!(w < 160.0 && (w / h - 1.6).abs() < 0.01);
        assert!(8.0 * w + 9.0 * 8.0 <= 800.5);
        assert_eq!(fit_thumbs(0, 160.0, 100.0, 8.0, 10.0), (160.0, 100.0));
        let (w, _) = fit_thumbs(4, 160.0, 100.0, 8.0, f32::NAN);
        assert_eq!(w, 160.0);
    }

    /// A big group wraps into rows of readable crops; past what fits, the rest are counted.
    #[test]
    fn a_big_group_wraps_and_is_capped_to_the_room() {
        let one = strip_grid(4, 160.0, 100.0, 8.0, LABEL_H, 1400.0, 400.0);
        assert_eq!((one.cols, one.shown, one.w), (4, 4, 160.0));
        let big = strip_grid(100, 160.0, 100.0, 8.0, LABEL_H, 1200.0, 400.0);
        assert!(big.w >= MIN_THUMB_W_BIG - 0.01 && big.cols >= 12, "{big:?}");
        let rows = big.shown.div_ceil(big.cols);
        assert!(rows >= 2 && rows as f32 * (big.h + LABEL_H + 8.0) <= 400.0 + 8.0, "{big:?}");
        assert!(big.shown < 100);
        let junk = strip_grid(30, 160.0, 100.0, 8.0, LABEL_H, f32::NAN, 10.0);
        assert_eq!(junk.shown, 30);
        let narrow = strip_grid(30, 160.0, 100.0, 8.0, LABEL_H, 50.0, 10.0);
        assert!(narrow.cols >= 1 && narrow.shown >= 1);
    }

    /// A group of 60 (notes item 19): the crops fit the loupe at every width, and a bar or a drag
    /// scrolls the rows. No summary text sits beside them (test notes 2026-10-22, note 2).
    #[test]
    fn a_big_group_never_spills_off_the_loupe() {
        for canvas_w in [700.0f32, 1000.0, 1400.0, 2400.0] {
            let room = canvas_w - 24.0;
            let g = strip_grid(60, 160.0, 100.0, 8.0, LABEL_H, room, 400.0);
            let thumbs_w = g.cols.min(60) as f32 * (g.w + 8.0) + 8.0;
            assert!(thumbs_w <= room + 0.5, "{canvas_w}: {thumbs_w} > {room} ({g:?})");
        }
        // a drag moves whole rows, kept in range
        assert_eq!(rows_after(2, 1.0, 5), 3);
        assert_eq!(rows_after(2, -4.0, 5), 0);
        assert_eq!(rows_after(2, 9.0, 5), 5);
        assert_eq!(rows_after(2, f32::NAN, 5), 2);
        // the bar: its top shows the first rows, its bottom the last
        let track = Rect::from_min_max(pos2(0.0, 0.0), pos2(6.0, 100.0));
        assert_eq!(row_at(track, 0.0, 10, 2), 0);
        assert_eq!(row_at(track, 100.0, 10, 2), 8);
        assert_eq!(row_at(track, 50.0, 10, 2), 4);
        assert_eq!(row_at(track, f32::NAN, 10, 2), 0);
        assert_eq!(row_at(Rect::NOTHING, 5.0, 10, 2), 0);
    }

    /// The strip has its own switch while importing (on at first: culling) and in the library
    /// (off at first: editing); the button and Shift+T switch the one where the user is.
    #[test]
    fn the_strip_is_on_while_importing_and_off_in_the_library_until_switched() {
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        assert!(!app.importing() && !app.eye_focus_on(), "off in the library at first");
        app.session.source = lcc_engine::LibrarySource::Folder;
        assert!(app.importing() && app.eye_focus_on(), "on while importing at first");
        assert_eq!(app.run("view.eyeFocus", serde_json::json!({})).unwrap()["on"], false);
        assert!(!app.ui.eye_focus_import);
        // (a library view is the Library tab: browsed photos show only in the Import tab)
        app.run("library.source", serde_json::json!({"kind": "all"})).unwrap();
        assert!(!app.importing());
        assert_eq!(app.run("view.eyeFocus", serde_json::json!({})).unwrap()["on"], true);
        assert!(app.ui.eye_focus && !app.ui.eye_focus_import, "each side keeps its own");
    }

    /// In a big group the people to look at come first: closed, blurry, half closed, soft, then
    /// eyes not visible, then the sharp and open.
    #[test]
    fn a_big_group_shows_the_people_to_look_at_first() {
        let mut v = [Verdict::Sharp, Verdict::NotVisible, Verdict::Soft, Verdict::Closed, Verdict::HalfClosed, Verdict::Blurry];
        v.sort_by_key(|x| look_first(*x));
        assert_eq!(v, [Verdict::Closed, Verdict::Blurry, Verdict::HalfClosed, Verdict::Soft, Verdict::NotVisible, Verdict::Sharp]);
        // 120 people on a 1,300-point loupe: crops at least 76 points wide, several rows
        let g = strip_grid(120, 160.0, 100.0, 8.0, LABEL_H, 1276.0, 360.0);
        assert!(g.w >= MIN_THUMB_W_BIG - 0.01 && g.cols >= 14 && g.shown >= 50 && g.shown < 120, "{g:?}");
    }

    #[test]
    fn a_click_zooms_to_the_eye_and_junk_is_ignored() {
        let mut app = crate::LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        zoom_to(&mut app, [0.25, 0.75]);
        assert_eq!((app.ui.zoom, app.ui.pan), (crate::state::Zoom::Percent(100.0), (0.25, 0.75)));
        zoom_to(&mut app, [f32::NAN, 0.1]);
        assert_eq!(app.ui.pan, (0.25, 0.75));
    }

    #[test]
    fn results_are_kept_per_photo_and_bounded() {
        let mut s = EyeStrip::default();
        assert_eq!(s.shown(1), Shown::Pending);
        for k in 0..(KEEP as u64 + 10) {
            s.tick += 1;
            let r = EyeReport {
                photo: PhotoId(k),
                eyes: Vec::new(),
                faces: (k % 2) as usize,
                no_eyes: (k % 2) as usize,
                size: (1, 1),
                ms: 0.0,
                boxes: Vec::new(),
            };
            s.results.insert(k, (Ok(Arc::new(r)), s.tick));
        }
        s.trim();
        assert_eq!(s.results.len(), KEEP);
        assert!(!s.results.contains_key(&0), "the oldest go first");
        assert_eq!(s.shown(KEEP as u64 + 8), Shown::NoFaces);
        assert_eq!(s.shown(KEEP as u64 + 9), Shown::NoEyes(1));
        s.results.insert(7, (Err("no model".into()), s.tick));
        assert_eq!(s.shown(7), Shown::Failed("no model".into()));
    }
}
