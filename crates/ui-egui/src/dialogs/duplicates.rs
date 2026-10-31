//! Photo > Find Duplicates…: choose how close a copy has to be, find the groups (`library.findDuplicates`), and for each
//! group tick the photos to keep (the suggested one is ticked, with why) and mark the others Rejected
//! (`library.resolveDuplicates`). Nothing is deleted here.
//!
//! The scan reads the session (its catalog and signature cache), which only the UI thread owns, so it runs in steps: a
//! few milliseconds per frame ([`Scan::step`]), with a progress bar and a Cancel button, and a row in the activity stack.
//! The window keeps answering while a big library is read for the first time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::cmd::duplicates::Scan;
use serde_json::Value;

use super::{LightcraftApp, Tokens, json, list_height};

/// The width of one photo's cell in a group's rows.
const CELL: f32 = 120.0;
/// How long a frame reads signatures for.
const STEP_BUDGET: Duration = Duration::from_millis(12);
/// The thumbnail bucket requested for a 112 x 84 point cell (up to 2x scale), and its priority (as a print page's cells).
const THUMB_PX: usize = 256;
const THUMB_PRIORITY: u32 = 10;
/// Most groups laid out at once: the rest wait until some of these are decided. (A group is shown whole: the photos it
/// marks Rejected must all be in sight.)
const MAX_GROUPS_SHOWN: usize = 100;
/// The activity stack's row while the scan runs.
const LABEL: &str = "Finding duplicates";

/// A scan in progress (kept in `LightcraftApp::duplicates_run`).
pub struct DuplicateRun {
    scan: Scan,
    cancel: Arc<AtomicBool>,
    /// Its row in the activity stack (dropping the run removes it).
    _guard: TaskGuard,
}

/// The whole percent shown as "n% alike" for a copy that is not byte-identical: rounded down and never 100, so a near
/// copy cannot read as identical next to an "Exact copy" row.
fn alike_percent(similarity: f64) -> i32 {
    ((similarity * 100.0).floor() as i32).clamp(0, 99)
}

/// A reason id from the engine in words.
fn reason_words(id: &str) -> &'static str {
    match id {
        "picked" => "your pick",
        "rated" => "more stars",
        "raw" => "raw file",
        "edited" => "edited",
        "larger" => "more pixels",
        "sharper" => "sharper",
        _ => "",
    }
}

fn ids_of(group: &Value, key: &str) -> Vec<u64> {
    group["photos"].as_array().into_iter().flatten().filter_map(|p| p[key].as_u64()).collect()
}

/// Start finding the groups at the chosen level.
fn start(app: &mut LightcraftApp, near: bool) -> Result<(), String> {
    let scan = Scan::begin(&app.session, &json!({"level": if near { "near" } else { "exact" }})).map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = app.session.activity.start("duplicates", LABEL, Cancel::Flag(cancel.clone()));
    app.duplicates_run = Some(DuplicateRun { scan, cancel, _guard: guard });
    Ok(())
}

/// Read signatures for one frame's budget; the finished answer when the scan is through. A stopped scan is dropped.
fn pump(app: &mut LightcraftApp, ctx: &egui::Context) -> Option<Value> {
    let run = app.duplicates_run.as_mut()?;
    if run.cancel.load(Ordering::Relaxed) {
        app.duplicates_run = None;
        return None;
    }
    let done = run.scan.step(&mut app.session, Some(STEP_BUDGET));
    let (looked, total) = run.scan.progress();
    run._guard.progress(looked as u64, total as u64);
    if !done {
        ctx.request_repaint();
        return None;
    }
    let run = app.duplicates_run.take()?;
    Some(run.scan.finish(&app.session))
}

/// Drop a scan whose dialog was closed (called every frame).
pub fn drop_if_closed(app: &mut LightcraftApp) {
    if app.duplicates_run.is_some() && !matches!(app.ui.dialog, Some(crate::state::Dialog::Duplicates { .. })) {
        app.duplicates_run = None;
    }
}

/// Mark group `g`'s photos not ticked Rejected, and take the group out of the list.
fn resolve(app: &mut LightcraftApp, g: usize, result: &mut Option<Arc<Value>>, keep: &[u64]) -> Result<(), String> {
    let group = result.as_ref().and_then(|r| r["groups"].get(g)).cloned().ok_or("that group is gone")?;
    let ids = ids_of(&group, "id");
    let kept: Vec<u64> = ids.iter().copied().filter(|id| keep.contains(id)).collect();
    app.run("library.resolveDuplicates", json!({"ids": ids, "keep": kept}))?;
    if let Some(groups) = result.as_mut().and_then(|r| Arc::make_mut(r)["groups"].as_array_mut()) {
        groups.remove(g);
    }
    Ok(())
}

pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, near: &mut bool, result: &mut Option<Arc<Value>>, keep: &mut Vec<u64>) {
    let tr = crate::i18n::tr;
    // never wider than the window, however many photos a group has: its rows wrap inside this width
    ui.set_max_width((ui.ctx().content_rect().width() - 120.0).clamp(300.0, 1060.0));
    ui.label(
        egui::RichText::new(tr(
            "Finds photos that are copies of each other. Nothing is deleted: you choose which copy to keep, and the others are marked Rejected.",
        ))
        .color(t.text_label),
    );
    ui.add_space(6.0);
    for (is_near, id, label, help) in [
        (false, "exact", "Exact copies", "The very same file, found twice in the library."),
        (true, "near", "Same picture saved again", "Also copies that were resized or saved again, for example as a smaller JPEG."),
    ] {
        let r = ui.radio_value(near, is_near, tr(label));
        crate::widgets::register(ui, format!("radio:duplicates.{id}"), r.rect);
        ui.label(egui::RichText::new(tr(help)).color(t.text_dim));
    }
    ui.add_space(6.0);
    if let Some(found) = pump(app, ui.ctx()) {
        *keep = found["groups"].as_array().into_iter().flatten().filter_map(|g| g["keep"].as_u64()).collect();
        *result = Some(Arc::new(found));
    }
    let running = app.duplicates_run.is_some();
    ui.horizontal(|ui| {
        let r = ui.add_enabled(!running, egui::Button::new(tr("Find Duplicates")));
        crate::widgets::register(ui, "button:findDuplicates", r.rect);
        if r.clicked() {
            *result = None;
            if let Err(e) = start(app, *near) {
                app.toast_error(ui.ctx(), e);
            }
        }
        if let Some(run) = &app.duplicates_run {
            let (looked, total) = run.scan.progress();
            let fraction = if total == 0 { 0.0 } else { looked as f32 / total as f32 };
            let text = crate::i18n::tr_format!("Comparing {done} of {total} pictures…", done = looked, total = total);
            ui.add(egui::ProgressBar::new(fraction).desired_width(260.0).text(text));
            let stop = ui.button(tr("Cancel"));
            crate::widgets::register(ui, "button:duplicates.cancel", stop.rect);
            if stop.clicked() {
                run.cancel.store(true, Ordering::Relaxed);
            }
        }
    });
    let Some(found) = result.as_ref() else { return };
    let no_groups = Vec::new();
    let groups = found["groups"].as_array().unwrap_or(&no_groups);
    ui.add_space(8.0);
    if groups.is_empty() {
        ui.label(egui::RichText::new(tr("No duplicates found.")).color(t.text_label));
        return;
    }
    let photos: usize = groups.iter().map(|g| g["photos"].as_array().map_or(0, Vec::len)).sum();
    ui.label(egui::RichText::new(format!("{}: {}   {}: {}", tr("Groups"), groups.len(), tr("Photos"), photos)).color(t.text_label));
    ui.label(egui::RichText::new(tr("Tick the photos to keep.")).color(t.text_dim));
    let mut action: Option<usize> = None;
    egui::ScrollArea::vertical().max_height(list_height(ui, 420.0)).auto_shrink([false, true]).show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        for (gi, g) in groups.iter().enumerate().take(MAX_GROUPS_SHOWN) {
            ui.separator();
            let level = if g["level"] == "exact" { "Exact copies" } else { "Same picture saved again" };
            ui.label(egui::RichText::new(tr(level)).font(t.semibold(12.5)).color(t.text));
            let rows = g["photos"].as_array().unwrap_or(&no_groups);
            // rows of as many photos as fit (a wrapped layout does not break between nested blocks)
            let per_row = ((ui.available_width() / (CELL + ui.spacing().item_spacing.x)).floor() as usize).max(1);
            let cells: Vec<_> = rows.iter().enumerate().collect();
            for row in cells.chunks(per_row) {
                ui.horizontal(|ui| {
                    for &(pi, p) in row {
                        let id = lightcraft_catalog::PhotoId(p["id"].as_u64().unwrap_or(0));
                        ui.vertical(|ui| {
                            ui.set_width(CELL);
                            let size = egui::vec2(112.0, 84.0);
                            // photos the library grid does not show (scrolled away, filtered out) have no thumbnail yet
                            crate::panels::grid::request_thumb(app, id, THUMB_PX, THUMB_PRIORITY);
                            match app.renderer.thumb(id) {
                                Some(tex) => {
                                    let [w, h] = tex.size;
                                    let scale = (size.x / w.max(1) as f32).min(size.y / h.max(1) as f32);
                                    ui.add(egui::Image::new((tex.tex.id(), egui::vec2(w as f32 * scale, h as f32 * scale))));
                                }
                                None => {
                                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                    ui.painter().rect_filled(rect, 2.0, t.inset);
                                }
                            }
                            let mut on = keep.contains(&id.0);
                            let name = p["file"].as_str().unwrap_or_default().to_string();
                            let r = ui.checkbox(&mut on, tr("Keep")).on_hover_text(&name);
                            crate::widgets::register(ui, format!("check:duplicates.{gi}.{pi}"), r.rect);
                            if r.changed() {
                                if on {
                                    keep.push(id.0);
                                } else {
                                    keep.retain(|k| *k != id.0);
                                }
                            }
                            ui.label(egui::RichText::new(name).small().color(t.text_label));
                            ui.label(egui::RichText::new(format!("{} x {}", p["width"], p["height"])).small().color(t.text_dim));
                            if g["keep"] == p["id"] {
                                let why: Vec<&str> = g["reasons"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(reason_words)
                                    .filter(|w| !w.is_empty())
                                    .map(tr)
                                    .collect();
                                let text = if why.is_empty() {
                                    tr("Suggested").to_string()
                                } else {
                                    crate::i18n::tr_format!("Suggested: {why}", why = why.join(", "))
                                };
                                ui.label(egui::RichText::new(text).small().color(t.accent));
                            } else {
                                let how = if p["exact"] == true {
                                    tr("Exact copy").to_string()
                                } else {
                                    crate::i18n::tr_format!("{pct}% alike", pct = alike_percent(p["similarity"].as_f64().unwrap_or(0.0)))
                                };
                                ui.label(egui::RichText::new(how).small().color(t.text_dim));
                            }
                        });
                    }
                });
            }
            let any = rows.iter().filter_map(|p| p["id"].as_u64()).any(|id| keep.contains(&id));
            let r = ui
                .add_enabled(any, egui::Button::new(tr("Mark Others Rejected")))
                .on_disabled_hover_text(tr("Keep at least one photo of the group."));
            crate::widgets::register(ui, format!("button:duplicates.resolve.{gi}"), r.rect);
            if r.clicked() {
                action = Some(gi);
            }
        }
        if groups.len() > MAX_GROUPS_SHOWN {
            ui.separator();
            let note = crate::i18n::tr_format!(
                "Showing the first {shown} of {total} groups. Decide some of these to see the rest.",
                shown = MAX_GROUPS_SHOWN,
                total = groups.len()
            );
            ui.label(egui::RichText::new(note).color(t.text_dim));
        }
    });
    if let Some(g) = action
        && let Err(e) = resolve(app, g, result, keep)
    {
        app.toast_error(ui.ctx(), e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_that_is_not_exact_never_reads_100_percent() {
        assert_eq!(alike_percent(0.998), 99);
        assert_eq!(alike_percent(1.0), 99);
        assert_eq!(alike_percent(0.925), 92);
        assert_eq!(alike_percent(f64::NAN), 0);
        assert_eq!(alike_percent(-0.5), 0);
    }

    /// A stopped scan gives no answer, leaves no row in the activity stack and lets the next one start; one that is
    /// left running reads a few pictures per frame and ends with the answer.
    #[test]
    fn a_scan_runs_in_frames_and_cancel_drops_it() {
        let session = lightcraft_engine::Session::with_demo();
        let mut app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        start(&mut app, true).unwrap();
        assert!(app.duplicates_run.is_some() && app.session.activity.list().iter().any(|r| r.kind == "duplicates"));
        if let Some(run) = &app.duplicates_run {
            run.cancel.store(true, Ordering::Relaxed);
        }
        assert!(pump(&mut app, &ctx).is_none() && app.duplicates_run.is_none(), "cancelled: dropped, no answer");
        assert!(app.session.activity.list().iter().all(|r| r.kind != "duplicates"), "its row is gone");
        start(&mut app, true).unwrap();
        let mut frames = 0;
        let found = loop {
            frames += 1;
            assert!(frames < 10_000, "the scan never ended");
            if let Some(found) = pump(&mut app, &ctx) {
                break found;
            }
        };
        assert!(found["groups"].is_array() && app.duplicates_run.is_none());
    }
}
