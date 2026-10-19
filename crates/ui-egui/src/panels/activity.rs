//! The activity stack (issue #345): every long-running task in one place, floating in the top-left corner under the
//! top bar while something runs — a row each with its name, a progress bar, what it is working on, and ✕ for the ones
//! that can be cancelled. The tasks come from the engine's registry ([`lightcraft_engine::activity`]); ✕ runs
//! `activity.cancel` like any other frontend would.

use egui::{Align, Layout, Margin, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_engine::activity::{TaskInfo, Unit};
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

/// The row's second line: "Stopping…" once cancelled, else the count and what is being worked on.
fn detail_line(t: &TaskInfo) -> String {
    if t.cancelling {
        return tr("Stopping…").to_string();
    }
    let count = count_text(t);
    match (count.is_empty(), t.detail.is_empty()) {
        (false, false) => format!("{count} · {}", t.detail),
        (false, true) => count,
        (true, _) => t.detail.clone(),
    }
}

/// Draw the stack (called every frame, after the panels and dialogs).
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    let all = app.session.activity.list();
    if all.is_empty() {
        app.activity_expanded = false;
        return;
    }
    // look again soon: a young task becomes due, bars move
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let tasks: Vec<&TaskInfo> = all.iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).collect();
    if tasks.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let shown = if app.activity_expanded { tasks.len() } else { tasks.len().min(MAX_ROWS) };
    let hidden = tasks.len().saturating_sub(shown);
    let max_height = (ctx.content_rect().height() - t.top_bar_h - 60.0).max(80.0);
    let mut cancel = None;
    let mut toggle = false;
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
                for (i, task) in tasks.iter().take(shown).enumerate() {
                    if i > 0 {
                        ui.add_space(6.0);
                    }
                    if row(ui, &t, task, inner) {
                        cancel = Some(task.id);
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
}

/// ✕ on task `id`'s row. The task may have just finished or stopped being cancellable: nothing to tell the user then,
/// so the command goes to the engine directly (`LightcraftApp::run` would put its error in the status bar).
pub(crate) fn cancel(app: &mut LightcraftApp, id: u64) {
    let _ = app.session.execute("activity.cancel", &json!({"id": id}));
}

/// One task: name and ✕, the bar, the detail line. True when ✕ was clicked.
fn row(ui: &mut egui::Ui, t: &Tokens, task: &TaskInfo, width: f32) -> bool {
    let mut clicked = false;
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
    });
    register(ui, format!("activity:row:{}", task.id), r.response.rect);
    clicked
}

/// The progress bar, named after its task for screen readers; while the amount of work isn't
/// known, a segment sweeps across.
pub(crate) fn bar(ui: &mut egui::Ui, task: &TaskInfo, width: f32) -> egui::Response {
    let fraction = (task.total > 0).then(|| (task.done as f64 / task.total as f64) as f32);
    let response = crate::progress::Progress::new(fraction).width(width).dimmed(task.cancelling).show(ui);
    crate::widgets::screen_reader_name(ui.ctx(), response.id, tr(&task.label));
    response
}

/// The progress bar; while the amount of work isn't known, a segment sweeps across.
/// The sweeping segment of an indeterminate bar, as offsets from the bar's left edge. Reduced motion
/// leaves a still segment (a quarter of the bar, at its left end) that says "working" without moving.
pub(crate) fn indeterminate_span(width: f32, time: f64, reduced: bool) -> (f32, f32) {
    if reduced {
        return (0.0, width * 0.25);
    }
    let phase = (time * 0.7).fract() as f32;
    let w = width * 0.3;
    let x = (width + w) * phase - w;
    (x.max(0.0), (x + w).min(width))
}
