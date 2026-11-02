//! The activity stack (issue #345): every long-running task in one place, floating in the top-left corner under the
//! top bar while something runs — a row each with its name, a progress bar, what it is working on, and ✕ for the ones
//! that can be cancelled. The tasks come from the engine's registry ([`lightcraft_engine::activity`]); ✕ runs
//! `activity.cancel` like any other frontend would.
//!
//! A batch (AI Denoise or an export over many photos) is one row: "37 of 200 · about
//! 6 min left · 2 failed". Its per-photo rows fold under it ("Show each photo"), and when it ends its summary shows
//! once as a message ("AI Denoise: 198 done, 2 failed: why").

use egui::{Align, Layout, Margin, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_engine::activity::{BatchSummary, TaskInfo, Unit};
use serde_json::json;

use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

/// A task shows once it has run this long, so quick ones don't flash.
pub const SHOW_AFTER_MS: u64 = 500;
/// Rows shown before "+N more".
pub const MAX_ROWS: usize = 3;
/// The stack's width (points).
pub const WIDTH: f32 = 280.0;

const MARGIN: i8 = 8;
const CROSS: f32 = 16.0;
/// How far a batch's per-photo rows are set in.
const INDENT: f32 = 12.0;

/// "about 6 min left": the time a batch has left, roughly.
pub fn eta_text(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        tr("less than a minute left").to_string()
    } else if secs < 90 * 60 {
        tr_format!("about {n} min left", n = secs.div_ceil(60))
    } else {
        let mins = secs.div_ceil(60);
        tr_format!("about {h} h {m} min left", h = mins / 60, m = mins % 60)
    }
}

/// What a batch did, for the message when it ends: "AI Denoise: 198 done, 2 failed: IMG_2.CR3: …".
pub fn summary_text(s: &BatchSummary) -> String {
    let mut m = format!("{}: {}", tr(&s.label), tr_format!("{n} done", n = s.done.saturating_sub(s.failed)));
    if s.failed > 0 {
        m += &tr_format!(", {n} failed", n = s.failed);
        if let Some(why) = s.reasons.first() {
            m += &format!(": {why}");
        }
    }
    if s.cancelled {
        m += &format!(" · {}", tr("cancelled"));
    }
    m
}

/// "3 of 25", "12 of 340 MB", "40 %"; empty while the amount of work isn't known.
pub fn count_text(t: &TaskInfo) -> String {
    if t.total == 0 {
        return String::new();
    }
    match t.unit {
        Unit::Count => tr_format!("{done} of {total}", done = t.done, total = t.total),
        Unit::Bytes => tr_format!("{done} of {total} MB", done = megabytes(t.done), total = megabytes(t.total)),
        Unit::Percent => tr_format!("{pct} %", pct = t.done.saturating_mul(100) / t.total),
    }
}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb < 10.0 { format!("{mb:.1}") } else { format!("{:.0}", mb.floor()) }
}

#[cfg(test)]
pub(crate) fn detail_line_for_tests(t: &TaskInfo) -> String {
    detail_line(t)
}

/// The row's second line: "Stopping…" once cancelled, else the count and what is being worked on.
fn detail_line(t: &TaskInfo) -> String {
    if t.cancelling {
        return tr("Stopping…").to_string();
    }
    let mut count = count_text(t);
    if t.batch {
        if let Some(ms) = t.eta_ms {
            count += &format!(" · {}", eta_text(ms));
        }
        if t.failed > 0 {
            count += &format!(" · {}", tr_format!("{n} failed", n = t.failed));
        }
    }
    match (count.is_empty(), t.detail.is_empty()) {
        (false, false) => format!("{count} · {}", t.detail),
        (false, true) => count,
        (true, _) => t.detail.clone(),
    }
}

/// Draw the stack (called every frame, after the panels and dialogs).
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    announce_finished(app, ctx);
    let all = app.session.activity.list();
    if all.is_empty() {
        app.activity_expanded = false;
        app.activity_open.clear();
        return;
    }
    // look again soon: a young task becomes due, bars move
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let due: Vec<&TaskInfo> = all.iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).collect();
    if due.is_empty() {
        return;
    }
    app.activity_open.retain(|id| all.iter().any(|t| t.id == *id));
    let tasks = ordered(&due, &app.activity_open);
    let t = Tokens::get(ctx);
    let shown = if app.activity_expanded { tasks.len() } else { tasks.len().min(MAX_ROWS) };
    let hidden = tasks.len().saturating_sub(shown);
    let max_height = (ctx.content_rect().height() - t.top_bar_h - 60.0).max(80.0);
    let mut cancel = None;
    let mut toggle = false;
    let mut fold = None;
    let open = app.activity_open.clone();
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.field_border)).corner_radius(6).inner_margin(Margin::same(MARGIN));
    egui::Area::new(egui::Id::unique("activity-stack")).order(egui::Order::Middle).fixed_pos(pos2(8.0, t.top_bar_h + 8.0)).show(ctx, |ui| {
        // An area lays out in the size it had last frame, and the scroll area would take that as all the room there
        // is: the stack would never grow when rows are added. Give it the whole height below the top bar instead
        // (the scroll area keeps to `max_height`).
        ui.set_max_height((ctx.content_rect().height() - t.top_bar_h - 16.0).max(80.0));
        frame.show(ui, |ui| {
            let inner = WIDTH - 2.0 * (f32::from(MARGIN) + 1.0);
            ui.set_width(inner);
            ui.spacing_mut().item_spacing.y = 3.0;
            egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                for (i, (task, depth, kids)) in tasks.iter().take(shown).enumerate() {
                    if i > 0 {
                        ui.add_space(6.0);
                    }
                    let indent = if *depth > 0 { INDENT } else { 0.0 };
                    let (x, f) = ui
                        .horizontal(|ui| {
                            ui.add_space(indent);
                            ui.vertical(|ui| row(ui, &t, task, inner - indent, *kids, open.contains(&task.id))).inner
                        })
                        .inner;
                    if x {
                        cancel = Some(task.id);
                    }
                    if f {
                        fold = Some(task.id);
                    }
                }
            });
            if hidden > 0 || (app.activity_expanded && tasks.len() > MAX_ROWS) {
                ui.add_space(4.0);
                let text = if hidden > 0 { tr_format!("+{n} more", n = hidden) } else { tr("Show less").to_string() };
                let r = ui.add(egui::Label::new(RichText::new(text).font(t.font(11.0)).color(t.accent)).sense(Sense::click()));
                register(ui, "activity:more", r.rect);
                toggle = r.clicked();
            }
        });
    });
    if let Some(id) = cancel {
        self::cancel(app, id);
    }
    if toggle {
        app.activity_expanded = !app.activity_expanded;
    }
    if let Some(id) = fold
        && !app.activity_open.remove(&id)
    {
        app.activity_open.insert(id);
    }
}

/// The rows in the order shown, each with its depth and how many per-photo rows it has: tasks oldest first, a
/// batch's per-photo rows right under it while it is open (`open`), else folded away. A row whose batch isn't shown
/// stands on its own.
fn ordered<'a>(tasks: &[&'a TaskInfo], open: &std::collections::HashSet<u64>) -> Vec<(&'a TaskInfo, u8, usize)> {
    let ids: std::collections::HashSet<u64> = tasks.iter().map(|t| t.id).collect();
    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks.iter().filter(|t| t.parent.is_none_or(|p| !ids.contains(&p))) {
        let kids: Vec<&TaskInfo> = tasks.iter().filter(|k| k.parent == Some(t.id)).copied().collect();
        out.push((*t, 0, kids.len()));
        if open.contains(&t.id) {
            out.extend(kids.into_iter().map(|k| (k, 1, 0)));
        }
    }
    out
}

/// Say once what each batch of several photos that ended did ("AI Denoise: 198 done, 2 failed: why"); a failure
/// stays longer. A single photo's says nothing here: its own messages and the photo show how it went.
fn announce_finished(app: &mut LightcraftApp, ctx: &egui::Context) {
    let finished = app.session.activity.finished();
    let Some(last) = finished.last().map(|s| s.seq) else { return };
    if last <= app.activity_seen_seq {
        return;
    }
    let seen = app.activity_seen_seq;
    app.activity_seen_seq = last;
    let new: Vec<&BatchSummary> = finished.iter().filter(|s| s.seq > seen && !s.reported && s.done >= 2).collect();
    // several at once (rare): the one with failures, else the last
    if let Some(s) = new.iter().find(|s| s.failed > 0).or(new.last()) {
        let text = summary_text(s);
        if s.failed > 0 {
            app.toast_for(ctx, text, 15.0);
        } else {
            app.toast(ctx, text);
        }
    }
}

/// ✕ on task `id`'s row. The task may have just finished or stopped being cancellable: nothing to tell the user then,
/// so the command goes to the engine directly (`LightcraftApp::run` would put its error in the status bar).
pub(crate) fn cancel(app: &mut LightcraftApp, id: u64) {
    let _ = app.session.execute("activity.cancel", &json!({"id": id}));
}

/// One task: name and ✕, the bar, the detail line, and for a batch with per-photo rows (`kids`) the line that
/// opens or folds them. Whether ✕ was clicked, and whether the fold was.
fn row(ui: &mut egui::Ui, t: &Tokens, task: &TaskInfo, width: f32, kids: usize, open: bool) -> (bool, bool) {
    let mut clicked = false;
    let mut fold = false;
    let r = ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            let cross = task.cancellable && !task.cancelling;
            let label_w = if cross { width - CROSS - 6.0 } else { width };
            ui.allocate_ui_with_layout(vec2(label_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                // the whole width, so the cross lands at the right edge however short the name
                ui.set_width(label_w);
                ui.add(egui::Label::new(RichText::new(tr(&task.label)).font(t.font(12.5)).color(t.text)).truncate());
            });
            if cross {
                let (rect, resp) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr("Cancel"));
                paint(ui.painter(), rect.shrink(2.0), Icon::Close, if resp.hovered() { t.text } else { t.icon });
                register(ui, format!("activity:cancel:{}", task.id), rect);
                clicked = resp.on_hover_text(tr("Cancel")).clicked();
            }
        });
        bar(ui, task, width);
        let line = detail_line(task);
        if !line.is_empty() {
            ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        }
        if kids > 0 {
            let text = if open { tr("Hide each photo").to_string() } else { tr_format!("Show each photo ({n})", n = kids) };
            let r = ui.add(egui::Label::new(RichText::new(text).font(t.font(11.0)).color(t.accent)).sense(Sense::click()));
            register(ui, format!("activity:children:{}", task.id), r.rect);
            fold = r.clicked();
        }
    });
    register(ui, format!("activity:row:{}", task.id), r.response.rect);
    (clicked, fold)
}

/// The progress bar, named after its task for screen readers; while the amount of work isn't
/// known, a segment sweeps across.
pub(crate) fn bar(ui: &mut egui::Ui, task: &TaskInfo, width: f32) -> egui::Response {
    let fraction = (task.total > 0).then(|| (task.done as f64 / task.total as f64) as f32);
    let response = crate::progress::Progress::new(fraction).width(width).dimmed(task.cancelling).show(ui);
    crate::widgets::screen_reader_name(ui.ctx(), response.id, tr(&task.label));
    response
}
