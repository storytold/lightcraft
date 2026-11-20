//! Photo Merge in the UI: the HDR / Panorama / HDR Panorama dialog (options + live preview) and
//! the background merge jobs.
//!
//! Previews and the final merge run [`lightcraft_engine::merge::MergeJob`]s on worker threads with
//! progress and cancellation; the window stays responsive. Changing an option cancels the running
//! preview and starts a new one. The final merge keeps running after the dialog closes (progress
//! in the activity stack); when it finishes, the result is written, imported and selected
//! ([`lightcraft_engine::Session::finish_merge`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use lightcraft_catalog::PhotoId;
use lightcraft_engine::activity::{Cancel, TaskGuard, Unit};
use lightcraft_engine::merge::{MergeJob, MergeOutput};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The dialog's options (also the parameters of the `merge.*` commands).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MergeDialog {
    /// "merge.hdr" | "merge.panorama" | "merge.hdrPanorama".
    pub command: String,
    pub align: bool,
    /// none | low | medium | high
    pub deghost: String,
    pub show_overlay: bool,
    pub auto_settings: bool,
    pub stack: bool,
    /// auto | spherical | cylindrical | perspective
    pub projection: String,
    pub boundary_warp: f64,
    pub auto_crop: bool,
    pub fill_edges: bool,
    /// HDR panorama: photos per bracket (0 = from EXIF).
    pub bracket: u32,
}

impl Default for MergeDialog {
    fn default() -> Self {
        MergeDialog {
            command: "merge.hdr".into(),
            align: true,
            deghost: "none".into(),
            show_overlay: false,
            auto_settings: true,
            stack: false,
            projection: "auto".into(),
            boundary_warp: 0.0,
            auto_crop: true,
            fill_edges: false,
            bracket: 0,
        }
    }
}

impl MergeDialog {
    pub fn for_command(command: &str) -> MergeDialog {
        MergeDialog { command: command.into(), ..Default::default() }
    }
    pub fn title(&self) -> &'static str {
        match self.command.as_str() {
            "merge.panorama" => "Panorama Merge Preview",
            "merge.hdrPanorama" => "HDR Panorama Merge Preview",
            _ => "HDR Merge Preview",
        }
    }
    pub fn is_hdr(&self) -> bool {
        self.command != "merge.panorama"
    }
    pub fn is_pano(&self) -> bool {
        self.command != "merge.hdr"
    }
    pub fn params(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// A merge running on a worker thread.
pub struct MergeTask {
    pub preview: bool,
    pub job: MergeJob,
    /// The dialog options it was started with.
    pub options: MergeDialog,
    /// The photos it merges.
    pub ids: Vec<PhotoId>,
    pub progress: Arc<Mutex<(f32, String)>>,
    pub cancel: Arc<AtomicBool>,
    rx: Receiver<Result<MergeOutput, String>>,
    /// The final merge's row in the activity stack (previews have none).
    guard: Option<TaskGuard>,
    /// The app's memory budget when it started ([`lightcraft_engine::memory::budget`]): the merge's
    /// memory is what the caches leave, so a preview's forecast holds only while it stays.
    budget: usize,
}

/// Merge state of the app.
#[derive(Default)]
pub struct MergeState {
    pub preview_task: Option<MergeTask>,
    pub final_task: Option<MergeTask>,
    /// The latest preview (texture, size, its options, stats) and the latest error.
    pub preview: Option<(egui::TextureHandle, [usize; 2], MergeDialog, Value)>,
    pub preview_pixels: Option<Arc<egui::ColorImage>>,
    pub error: Option<String>,
    /// The photos and options whose preview failed (not retried until either changes or the dialog
    /// opens again).
    pub failed_options: Option<(Vec<PhotoId>, MergeDialog)>,
    /// The preview zoomed in by a click: the point of it (0..1 across and down) at the middle of
    /// the box. `None` fits it in the box. Kept while the options change, to compare them there.
    pub zoom: Option<(f32, f32)>,
    /// The last finished merge (`{id, path, …}`).
    pub last_result: Option<Value>,
    /// Photos the dialog merges (the selection when it opened).
    pub ids: Vec<PhotoId>,
    /// HDR for Each Bracket Set: the sets still to merge after the running one.
    pub batch: std::collections::VecDeque<Vec<PhotoId>>,
    /// The user agreed to the smaller panorama the latest preview's forecast names
    /// ([`smaller_notice`]): the merge may then be made under 40 MP to fit memory
    /// (`fitMemory`). Never saved with the options, and cleared by each new preview and each time
    /// the dialog opens, so it answers only the size it was shown.
    pub smaller_agreed: bool,
    /// The app's memory budget the latest preview was made with ([`MergeTask::budget`]): a preview
    /// made with another is redone, since its forecast of the merge's size no longer holds.
    pub preview_budget: usize,
    /// The photos the latest preview merged ([`MergeTask::ids`]): its forecast is of them, not of
    /// photos the dialog merges now (HDR for Each Bracket Set moves on to its next set meanwhile).
    pub preview_ids: Vec<PhotoId>,
}

impl MergeState {
    /// A preview or a merge is running.
    pub fn busy(&self) -> bool {
        self.preview_task.is_some() || self.final_task.is_some()
    }
}

fn spawn(app: &LightcraftApp, options: &MergeDialog, ids: &[PhotoId], preview: bool, fit_memory: bool) -> Result<MergeTask, String> {
    let (kind, finish) = lightcraft_engine::merge::parse(&options.command, &options.params()).map_err(|e| e.to_string())?;
    let mut job = app.session.plan_merge(kind, finish, ids, preview).map_err(|e| e.to_string())?;
    job.fit_memory = fit_memory;
    let progress = Arc::new(Mutex::new((0.0, "Starting".to_string())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let (j, p, c) = (job.clone(), progress.clone(), cancel.clone());
    let work = move || {
        let run = || {
            j.run(&|f, stage| {
                if let Ok(mut g) = p.lock() {
                    *g = (f, stage.to_string());
                }
                !c.load(Ordering::Relaxed)
            })
        };
        // a panicking merge reports an error instead of leaving "Merging…" up forever
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| Err("the merge failed unexpectedly".into()));
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("photo-merge".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    let budget = lightcraft_engine::memory::budget();
    Ok(MergeTask { preview, job, options: options.clone(), ids: ids.to_vec(), progress, cancel, rx, guard: None, budget })
}

/// The options of the last merge of kind `command` started (kept in the library preferences), or
/// the defaults the first time. Fields missing from an older or hand-edited prefs.json take their
/// defaults; options that don't parse at all fall back to the defaults.
pub fn last_options(app: &LightcraftApp, command: &str) -> MergeDialog {
    let saved = app.session.last_merge.get(command).and_then(|v| serde_json::from_value::<MergeDialog>(v.clone()).ok());
    MergeDialog { command: command.into(), ..saved.unwrap_or_default() }
}

/// Open the merge dialog for the selection, with the options of the last merge of this kind.
pub fn open(app: &mut LightcraftApp, command: &str) -> Result<Value, String> {
    let ids = app.session.selected();
    if ids.len() < 2 {
        return Err("select at least 2 photos to merge".into());
    }
    app.merge.ids = ids;
    app.merge.error = None;
    // a preview that failed before is tried again, or the dialog would show neither a preview nor its error (#2708)
    app.merge.failed_options = None;
    app.merge.preview = None;
    app.merge.preview_pixels = None;
    app.merge.zoom = None;
    app.merge.smaller_agreed = false;
    if let Some(t) = app.merge.preview_task.take() {
        t.cancel.store(true, Ordering::Relaxed);
    }
    app.ui.dialog = Some(crate::state::Dialog::Merge { opts: last_options(app, command) });
    Ok(json!({"photos": app.merge.ids.len()}))
}

/// The dialog's Merge button / `ui.dialog.confirm`: the full-resolution merge, once the preview
/// has said what size it makes ([`holdup`]), made smaller than 40 MP to fit memory when the user
/// agreed to the size it named ([`MergeState::smaller_agreed`]).
pub fn confirm(app: &mut LightcraftApp, opts: &MergeDialog) -> Result<Value, String> {
    if let Some(why) = holdup(app, opts) {
        return Err(why);
    }
    // the floor the forecast was made at: under 40 MP only when that is what it said (and, when it
    // said smaller than the photos' resolution, the user agreed: `holdup`)
    let fit_memory = forecast(app, opts)["fitMemory"].as_bool() == Some(true);
    start(app, opts, fit_memory)
}

/// Start the full-resolution merge without the dialog's notice (Merge with Last Settings, HDR for
/// Each Bracket Set): never made smaller than 40 MP to fit memory, which nobody agreed to.
pub fn start_final(app: &mut LightcraftApp, opts: &MergeDialog) -> Result<Value, String> {
    start(app, opts, false)
}

fn start(app: &mut LightcraftApp, opts: &MergeDialog, fit_memory: bool) -> Result<Value, String> {
    if app.merge.final_task.is_some() {
        return Err("a merge is already running".into());
    }
    if let Some(t) = app.merge.preview_task.take() {
        t.cancel.store(true, Ordering::Relaxed);
    }
    let ids = app.merge.ids.clone();
    let mut task = spawn(app, opts, &ids, false, fit_memory)?;
    let guard = app.session.activity.start("merge", "Merging", Cancel::Flag(task.cancel.clone()));
    guard.set_unit(Unit::Percent);
    guard.progress(0, 1000);
    task.guard = Some(guard);
    app.merge.final_task = Some(task);
    app.session.last_merge.insert(opts.command.clone(), opts.params());
    if let Err(e) = app.session.save_prefs() {
        log::warn!("merge: {e}");
    }
    Ok(json!({"started": true, "photos": ids.len()}))
}

/// Merge the selection without the dialog, with the options last used for `command` (the
/// defaults the first time).
pub fn start_last(app: &mut LightcraftApp, command: &str) -> Result<Value, String> {
    let ids = app.session.selected();
    if ids.len() < 2 {
        return Err("select at least 2 photos to merge".into());
    }
    app.merge.ids = ids;
    let opts = last_options(app, command);
    start_final(app, &opts)
}

/// HDR for Each Bracket Set (`merge.hdrBatch`): merge each bracket set among the photos in the background, one after
/// the other, with the options last used for HDR (the defaults the first time).
pub fn start_batch(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    if app.merge.final_task.is_some() {
        return Err("a merge is already running".into());
    }
    let ids = app.session.targets(p).map_err(|e| e.to_string())?;
    let sets = app.session.catalog.bracket_sets(&ids);
    if sets.is_empty() {
        return Err(lightcraft_engine::merge::NO_BRACKET_SETS.into());
    }
    let n = sets.len();
    app.merge.batch = sets.into();
    next_in_batch(app)?;
    Ok(json!({"started": true, "sets": n}))
}

/// Start the next bracket set of a batch, if any.
fn next_in_batch(app: &mut LightcraftApp) -> Result<(), String> {
    let Some(ids) = app.merge.batch.pop_front() else { return Ok(()) };
    app.merge.ids = ids;
    let opts = last_options(app, "merge.hdr");
    start_final(app, &opts).map(|_| ())
}

/// Per frame: keep the preview in sync with the dialog's options, collect finished jobs.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    // preview for the open dialog
    let dialog_opts = match &app.ui.dialog {
        Some(crate::state::Dialog::Merge { opts }) => Some(opts.clone()),
        _ => None,
    };
    match &dialog_opts {
        Some(opts) => {
            let current = app.merge.preview_task.as_ref().map(|t| &t.options).or(app.merge.preview.as_ref().map(|p| &p.2));
            // the memory budget or the photos changed since the preview (Settings, an agent, a batch
            // moving on): its forecast no longer holds
            let outdated = app.merge.preview_task.is_none() && current == Some(opts) && !previewed(app, opts);
            let stale = (current != Some(opts) || outdated) && !last_failed_matches(app, opts);
            if stale {
                if let Some(t) = app.merge.preview_task.take() {
                    t.cancel.store(true, Ordering::Relaxed);
                }
                let ids = app.merge.ids.clone();
                match spawn(app, opts, &ids, true, false) {
                    Ok(t) => {
                        app.merge.error = None;
                        app.merge.preview_task = Some(t);
                    }
                    Err(e) => {
                        app.merge.error = Some(e);
                        app.merge.failed_options = Some((ids, opts.clone()));
                    }
                }
            }
        }
        None => {
            if let Some(t) = app.merge.preview_task.take() {
                t.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    if let Some(t) = &app.merge.preview_task {
        match t.rx.try_recv() {
            Ok(Ok(out)) => {
                let Some(t) = app.merge.preview_task.take() else { return };
                if let Some(mut img) = out.preview {
                    // shown through the monitor profile like every other preview
                    if let Some(d) = app.renderer.display()
                        && let Err(e) = d.profile.from_srgb(&mut img)
                    {
                        log::warn!("display profile: {e}");
                    }
                    let color = Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
                    let tex = ctx.load_texture("merge-preview", color.clone(), egui::TextureOptions::LINEAR);
                    app.merge.preview_pixels = Some(color);
                    app.merge.preview = Some((tex, [img.width, img.height], t.options, out.info));
                    app.merge.preview_budget = t.budget;
                    app.merge.preview_ids = t.ids;
                    // agreed to the size the last preview named, not this one's
                    app.merge.smaller_agreed = false;
                }
            }
            Ok(Err(e)) => {
                let Some(t) = app.merge.preview_task.take() else { return };
                if e != "cancelled" {
                    app.merge.error = Some(e);
                    app.merge.failed_options = Some((t.ids, t.options));
                }
            }
            Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
        }
    }
    // the final merge
    if let Some(t) = &app.merge.final_task {
        match t.rx.try_recv() {
            Ok(r) => {
                let Some(t) = app.merge.final_task.take() else { return };
                let what = crate::i18n::tr(if t.options.command == "merge.panorama" {
                    "Panorama"
                } else if t.options.command == "merge.hdrPanorama" {
                    "HDR panorama"
                } else {
                    "HDR"
                });
                match r.map_err(lightcraft_engine::EngineError::Other).and_then(|out| app.session.finish_merge(&t.job, out)) {
                    Ok(v) => {
                        let message = added_message(what, &v);
                        app.merge.last_result = Some(v);
                        app.toast(ctx, message);
                    }
                    Err(_) if t.cancel.load(Ordering::Relaxed) => {
                        // cancelling one merge of a batch stops the batch
                        app.merge.batch.clear();
                        app.toast(ctx, crate::i18n::tr_format!("{what} merge cancelled", what = what))
                    }
                    Err(e) => {
                        app.ui.status = e.to_string();
                        app.toast(ctx, crate::i18n::tr_format!("{what} merge failed: {e}", e = e, what = what));
                    }
                }
                // a failed set doesn't stop the others
                while !app.merge.batch.is_empty() {
                    match next_in_batch(app) {
                        Ok(()) => break,
                        Err(e) => app.toast(ctx, crate::i18n::tr_format!("{what} merge failed: {e}", e = e, what = what)),
                    }
                }
            }
            Err(_) => {
                let (f, stage) = t.progress.lock().map(|g| g.clone()).unwrap_or_default();
                if let Some(guard) = &t.guard {
                    let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
                    guard.progress((f * 1000.0) as u64, 1000);
                    guard.detail(crate::i18n::tr(&stage));
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// The message for a finished merge (`what`: "Panorama", …; `info`: its result): a panorama
/// smaller than its photos' resolution says why (issue #1455).
fn added_message(what: &str, info: &Value) -> String {
    let size = |k: &str| info[k].as_f64().map(|mp| format!("{mp:.0} MP"));
    match (info["limit"].as_str(), size("megapixels"), size("nativeMegapixels")) {
        (Some("memory"), Some(mp), Some(full)) => crate::i18n::tr_format!(
            "{what} merge added at {mp}: its full size, {full}, needs more memory than this computer has",
            what = what,
            mp = mp,
            full = full
        ),
        (Some("size"), Some(mp), Some(full)) => crate::i18n::tr_format!(
            "{what} merge added at {mp}, the largest a panorama can be (its full size is {full})",
            what = what,
            mp = mp,
            full = full
        ),
        _ => crate::i18n::tr_format!("{what} merge added", what = what),
    }
}

/// The latest preview's `forecast` of the full-size merge (`null` when none: an HDR, a preview of
/// other options, or a size that can't be told; [`lightcraft_engine::merge::MergeJob::fit_memory`]).
fn forecast<'a>(app: &'a LightcraftApp, opts: &MergeDialog) -> &'a Value {
    match &app.merge.preview {
        Some((_, _, _, info)) if previewed(app, opts) && opts.is_pano() => &info["forecast"],
        _ => &Value::Null,
    }
}

/// The latest preview is of `opts` and the photos the dialog merges, made with the memory budget
/// the app has now.
fn previewed(app: &LightcraftApp, opts: &MergeDialog) -> bool {
    app.merge.preview.as_ref().is_some_and(|p| &p.2 == opts)
        && app.merge.preview_ids == app.merge.ids
        && app.merge.preview_budget == lightcraft_engine::memory::budget()
}

/// What the dialog says under the preview when the panorama won't fit memory at its full size
/// (`forecast`: [`forecast`]): the size it will be made at, which the user agrees to before Merge
/// works ([`MergeState::smaller_agreed`]), or why it can't be made at all.
pub(crate) fn smaller_notice(forecast: &Value) -> Option<String> {
    if let Some(e) = forecast["refused"].as_str() {
        return Some(crate::i18n::tr_format!("This panorama can't be merged: {e}", e = e));
    }
    // whole megapixels, as the merge's own message names them; tenths under 10 MP, where a whole
    // one would hide much of the difference
    let size = |k: &str| forecast[k].as_f64().map(|mp| if mp < 10.0 { format!("{mp:.1} MP") } else { format!("{mp:.0} MP") });
    match (forecast["limit"].as_str(), size("megapixels"), size("nativeMegapixels")) {
        (Some("memory"), Some(mp), Some(full)) => Some(crate::i18n::tr_format!(
            "Not enough memory for the full size, {full}: the panorama will be made at about {mp}",
            mp = mp,
            full = full
        )),
        _ => None,
    }
}

/// Why the dialog's Merge doesn't work yet (`None`: it does): a panorama's preview is still
/// working out the size the merge makes, the merge can't be made, or it will be made smaller to
/// fit memory and the user hasn't agreed to that. Without a preview (it failed) the merge runs
/// as the command does, never made under 40 MP unasked.
pub(crate) fn holdup(app: &LightcraftApp, opts: &MergeDialog) -> Option<String> {
    if !opts.is_pano() {
        return None;
    }
    if !previewed(app, opts) && !last_failed_matches(app, opts) {
        return Some(crate::i18n::tr("wait for the preview: it works out the size the panorama is made at").to_owned());
    }
    let f = forecast(app, opts);
    match smaller_notice(f) {
        Some(e) if f["refused"].is_string() => Some(e),
        Some(_) if !app.merge.smaller_agreed => {
            Some(crate::i18n::tr("the panorama will be made smaller to fit memory: agree to its size first").to_owned())
        }
        _ => None,
    }
}

fn last_failed_matches(app: &LightcraftApp, opts: &MergeDialog) -> bool {
    app.merge.failed_options.as_ref().is_some_and(|(ids, o)| ids == &app.merge.ids && o == opts)
}

/// Where a preview of `size` pixels is drawn in `area`: fitted, or zoomed in on the point `zoom`
/// to one preview pixel per screen pixel (at least twice the fitted size), kept over the box
/// where it covers it. Returns the rectangle and the point actually at the middle.
fn preview_rect(area: egui::Rect, size: [usize; 2], ppp: f32, zoom: Option<(f32, f32)>) -> (egui::Rect, Option<(f32, f32)>) {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let fit = (area.width() / w).min(area.height() / h);
    let Some((u, v)) = zoom else {
        return (egui::Rect::from_center_size(area.center(), egui::vec2(w * fit, h * fit)), None);
    };
    let s = (1.0 / ppp.max(0.1)).max(fit * 2.0);
    let img = egui::vec2(w * s, h * s);
    // the point clicked to the middle, but no gap at an edge where the image covers the box
    let axis = |t: f32, len: f32, lo: f32, centre: f32, span: f32| -> (f32, f32) {
        if len <= span {
            return (centre - len / 2.0, 0.5);
        }
        let start = (centre - t.clamp(0.0, 1.0) * len).clamp(lo + span - len, lo);
        (start, (centre - start) / len)
    };
    let (x, u) = axis(u, img.x, area.left(), area.center().x, area.width());
    let (y, v) = axis(v, img.y, area.top(), area.center().y, area.height());
    (egui::Rect::from_min_size(egui::pos2(x, y), img), Some((u, v)))
}

/// Click on the preview: zoom in on the point clicked, or back out; drag pans while zoomed in.
fn preview_input(app: &mut LightcraftApp, ui: &egui::Ui, resp: &egui::Response, img: egui::Rect) {
    let zoomed = app.merge.zoom.is_some();
    if resp.hovered() {
        let cursor = match (zoomed, resp.dragged()) {
            (false, _) => egui::CursorIcon::ZoomIn,
            (true, false) => egui::CursorIcon::Grab,
            (true, true) => egui::CursorIcon::Grabbing,
        };
        ui.ctx().set_cursor_icon(cursor);
    }
    if resp.clicked() {
        app.merge.zoom = if zoomed {
            None
        } else {
            resp.interact_pointer_pos()
                .map(|q| (((q.x - img.left()) / img.width()).clamp(0.0, 1.0), ((q.y - img.top()) / img.height()).clamp(0.0, 1.0)))
        };
    } else if resp.dragged()
        && let Some((u, v)) = app.merge.zoom
    {
        let d = resp.drag_delta();
        app.merge.zoom = Some((u - d.x / img.width(), v - d.y / img.height()));
    }
}

/// The dialog body (options on the left, preview on the right).
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, opts: &mut MergeDialog) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal_top(|ui| {
        // preview
        ui.vertical(|ui| {
            let box_size = egui::vec2(520.0, 360.0);
            let (rect, resp) = ui.allocate_exact_size(box_size, egui::Sense::click_and_drag());
            ui.painter().rect_filled(rect, 4.0, t.canvas);
            crate::widgets::register(ui, "merge.preview", rect);
            if let Some((tex, size, _, _)) = &app.merge.preview {
                let (tex, size) = (tex.id(), *size);
                let (r, zoom) = preview_rect(rect, size, ui.ctx().pixels_per_point(), app.merge.zoom);
                app.merge.zoom = zoom;
                ui.painter_at(rect).image(tex, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                preview_input(app, ui, &resp, r);
            }
            if let Some(task) = &app.merge.preview_task {
                let (f, stage) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
                ui.painter().text(
                    rect.center_bottom() - egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    crate::i18n::tr_format!("{stage}… {:.0}%", f * 100.0, stage = stage),
                    t.font(12.0),
                    t.text,
                );
            }
            if let Some(e) = &app.merge.error {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, e, t.font(13.0), t.text);
            }
            let info = app.merge.preview.as_ref().map(|p| p.3.clone());
            if let Some(info) = info {
                let txt = if opts.is_pano() {
                    crate::i18n::tr_format!(
                        "{} photos · {} · {:.0}° × {:.0}°",
                        info["used"].as_array().map_or(0, Vec::len),
                        info["projection"].as_str().unwrap_or(""),
                        info["fov"][0].as_f64().unwrap_or(0.0),
                        info["fov"][1].as_f64().unwrap_or(0.0)
                    )
                } else {
                    let ev: Vec<String> =
                        info["ev"].as_array().map(|a| a.iter().filter_map(Value::as_f64).map(|v| format!("{v:+.1}")).collect()).unwrap_or_default();
                    crate::i18n::tr_format!("{} photos · exposures {} EV", ev.len(), ev.join(" / "))
                };
                ui.label(egui::RichText::new(txt).color(t.text_dim));
            }
            if opts.is_pano() && !previewed(app, opts) && !last_failed_matches(app, opts) {
                ui.label(egui::RichText::new(crate::i18n::tr("Merge waits for the preview, which works out the panorama's size")).color(t.text_dim));
            }
            if let Some(notice) = smaller_notice(forecast(app, opts)) {
                ui.label(egui::RichText::new(notice).color(t.text));
                if forecast(app, opts)["refused"].is_null() {
                    let r = ui.checkbox(&mut app.merge.smaller_agreed, crate::i18n::tr("Merge at the smaller size"));
                    crate::widgets::register_response("merge.fitMemory", r.rect, &r);
                }
            }
        });
        ui.add_space(12.0);
        // options
        ui.vertical(|ui| {
            ui.set_width(220.0);
            if opts.is_pano() {
                ui.label(egui::RichText::new(crate::i18n::tr("Projection")).color(t.text_label));
                for (v, l) in [("auto", "Auto Select"), ("spherical", "Spherical"), ("cylindrical", "Cylindrical"), ("perspective", "Perspective")] {
                    if ui.radio(opts.projection == v, crate::i18n::tr(l)).clicked() {
                        opts.projection = v.to_string();
                    }
                }
                ui.add_space(4.0);
                let mut bw = opts.boundary_warp;
                if num(ui, &BOUNDARY_WARP, &mut bw) {
                    opts.boundary_warp = bw.round();
                }
                ui.checkbox(&mut opts.fill_edges, crate::i18n::tr("Fill Edges"));
                ui.checkbox(&mut opts.auto_crop, crate::i18n::tr("Auto Crop"));
                ui.add_space(4.0);
            }
            if opts.is_hdr() {
                ui.checkbox(&mut opts.align, crate::i18n::tr("Auto Align"));
                ui.label(egui::RichText::new(crate::i18n::tr("Deghost Amount")).color(t.text_label));
                ui.horizontal(|ui| {
                    for (i, (v, l)) in [("none", "None"), ("low", "Low"), ("medium", "Med"), ("high", "High")].iter().enumerate() {
                        if crate::widgets::text_button(ui, &format!("mergeDeghost-{i}"), l, opts.deghost == *v).clicked() {
                            opts.deghost = v.to_string();
                        }
                    }
                });
                if opts.command == "merge.hdr" {
                    ui.add_enabled_ui(opts.deghost != "none", |ui| ui.checkbox(&mut opts.show_overlay, crate::i18n::tr("Show Deghost Overlay")));
                }
                if opts.command == "merge.hdrPanorama" {
                    let mut b = opts.bracket as f64;
                    if num(ui, &BRACKET, &mut b) {
                        opts.bracket = b.round() as u32;
                    }
                }
                ui.add_space(4.0);
            }
            ui.checkbox(&mut opts.auto_settings, crate::i18n::tr("Auto Settings"));
            ui.checkbox(&mut opts.stack, crate::i18n::tr("Create Stack"));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{} photos", app.merge.ids.len())).color(t.text_dim));
        });
    });
}

const BOUNDARY_WARP: lightcraft_develop::ControlSpec = lightcraft_develop::ControlSpec {
    id: "merge.boundaryWarp",
    label: "Boundary Warp",
    section: lightcraft_develop::Section::Light,
    min: 0.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: lightcraft_develop::Track::Plain,
};
const BRACKET: lightcraft_develop::ControlSpec = lightcraft_develop::ControlSpec {
    id: "merge.bracket",
    label: "Photos per bracket (0 = auto)",
    section: lightcraft_develop::Section::Light,
    min: 0.0,
    max: 9.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: lightcraft_develop::Track::Plain,
};

fn num(ui: &mut egui::Ui, spec: &lightcraft_develop::ControlSpec, v: &mut f64) -> bool {
    match crate::widgets::slider(ui, spec, *v, true, None).value {
        Some(n) => {
            *v = n;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(520.0, 360.0))
    }

    #[test]
    fn the_preview_fits_until_clicked() {
        let (r, zoom) = preview_rect(area(), [1024, 683], 1.0, None);
        assert_eq!(zoom, None);
        assert!((r.width() - 520.0).abs() < 0.5 && r.height() < 360.0 && (r.center() - area().center()).length() < 0.5, "{r:?}");
    }

    /// Like Lightroom's: a click zooms in to one preview pixel per screen pixel on the point clicked.
    #[test]
    fn a_click_zooms_in_on_the_point_clicked() {
        // a panorama preview, wide enough that one pixel per pixel is more than twice the fitted size
        let (r, zoom) = preview_rect(area(), [1600, 1000], 1.0, Some((0.4, 0.55)));
        assert_eq!(r.size(), egui::vec2(1600.0, 1000.0));
        let (u, v) = zoom.unwrap();
        assert!((u - 0.4).abs() < 1e-4 && (v - 0.55).abs() < 1e-4, "{u} {v}");
        let at = r.min + egui::vec2(r.width() * 0.4, r.height() * 0.55);
        assert!((at - area().center()).length() < 0.5, "the point clicked is in the middle: {at:?}");
        // on a Retina screen, one preview pixel per physical pixel, but always at least twice the fitted size
        let (r, _) = preview_rect(area(), [1024, 683], 2.0, Some((0.5, 0.5)));
        assert!((r.width() - 1040.0).abs() < 0.5, "{r:?}");
    }

    /// Zoomed in near a corner (or panned past it), the image still covers the box.
    #[test]
    fn a_zoomed_preview_stays_over_the_box() {
        for p in [(0.0, 0.0), (1.0, 1.0), (-3.0, 7.0)] {
            let (r, zoom) = preview_rect(area(), [1024, 683], 1.0, Some(p));
            assert!(r.contains_rect(area()), "{p:?}: {r:?}");
            let (u, v) = zoom.unwrap();
            assert!((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v), "{p:?}: {u} {v}");
        }
        // a wide panorama is shorter than the box: centred up and down
        let (r, _) = preview_rect(area(), [1024, 120], 1.0, Some((0.9, 0.0)));
        assert!((r.center().y - area().center().y).abs() < 0.5 && r.left() < area().left() && r.right() >= area().right(), "{r:?}");
    }

    /// Issue #1455: a panorama held to the computer's memory, or to the largest size, says so.
    #[test]
    fn a_smaller_panorama_says_why() {
        let added = |info: Value| added_message("Panorama", &info);
        assert_eq!(
            added(json!({"limit": "memory", "megapixels": 166.2, "nativeMegapixels": 194.0})),
            "Panorama merge added at 166 MP: its full size, 194 MP, needs more memory than this computer has"
        );
        assert_eq!(
            added(json!({"limit": "size", "megapixels": 512.0, "nativeMegapixels": 700.4})),
            "Panorama merge added at 512 MP, the largest a panorama can be (its full size is 700 MP)"
        );
        assert_eq!(added(json!({"limit": null, "megapixels": 90.0, "nativeMegapixels": 90.0})), "Panorama merge added");
        assert_eq!(added(json!({"limit": "asked", "megapixels": 20.0, "nativeMegapixels": 90.0})), "Panorama merge added");
        assert_eq!(added_message("HDR", &json!({"ev": [0.0]})), "HDR merge added");
    }
}
