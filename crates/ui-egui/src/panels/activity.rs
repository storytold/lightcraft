//! The activity stack (issue #345): every long-running task in one place, under the bell in the top bar. While
//! something runs the bell wears a ring (how far along the work is, or a sweep while that isn't known), and the bar
//! names the task beside it when there is room. A click opens the list: a row each with its name, a progress bar,
//! what it is working on, and ✕ for the ones that can be cancelled. Nothing floats over the panels, so the list
//! covers them only while it is open. The tasks come from the engine's registry
//! ([`lightcraft_engine::activity`]); ✕ runs `activity.cancel` like any other frontend would.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Align, Layout, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_engine::activity::{TaskInfo, Unit};
use serde_json::json;

use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

/// A task shows once it has run this long, so quick ones don't flash.
pub const SHOW_AFTER_MS: u64 = 500;
/// The list's width (points), where the window has that much.
pub const WIDTH: f32 = 340.0;

const CROSS: f32 = 16.0;
/// The ring round the bell: its radius and line width.
const RING: f32 = 12.0;
const RING_WIDTH: f32 = 2.0;
/// The line beside the bell never takes more room than this, and leaves the task's name out when it would get less
/// than `READOUT_NAME_MIN` of it.
const READOUT_MAX: f32 = 220.0;
const READOUT_NAME_MIN: f32 = 56.0;
const READOUT_GAP: f32 = 8.0;

/// "3 of 25", "12 of 340 MB", "40 %"; empty while the amount of work isn't known.
pub fn count_text(t: &TaskInfo) -> String {
    if t.total == 0 {
        return String::new();
    }
    match t.unit {
        Unit::Count => tr_format!("{done} of {total}", done = t.done, total = t.total),
        Unit::Bytes => tr_format!("{done} of {total} MB", done = megabytes(t.done), total = megabytes(t.total)),
        Unit::Percent => tr_format!("{pct} %", pct = percent(t)),
    }
}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb < 10.0 { format!("{mb:.1}") } else { format!("{:.0}", mb.floor()) }
}

/// How far along a task with a known amount of work is, 0 to 100.
fn percent(t: &TaskInfo) -> u64 {
    t.done.min(t.total).saturating_mul(100).checked_div(t.total).unwrap_or(0)
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

/// The line beside the bell, in its two parts: the one task's name, and what changes about it (how far along it
/// is, or that it is stopping); with several tasks only how many run ("3 tasks"). Both empty when nothing runs.
fn readout_parts(tasks: &[TaskInfo]) -> (String, String) {
    match tasks {
        [] => (String::new(), String::new()),
        [one] => {
            let value = if one.cancelling {
                tr("Stopping…").to_string()
            } else if one.total > 0 {
                tr_format!("{pct} %", pct = percent(one))
            } else {
                String::new()
            };
            (tr(&one.label).to_string(), value)
        }
        many => (String::new(), tr_format!("{n} tasks", n = many.len())),
    }
}

/// The line beside the bell as one text: "Exporting · 40 %", "Finding faces", "3 tasks". Empty when nothing runs.
pub fn readout_text(tasks: &[TaskInfo]) -> String {
    match readout_parts(tasks) {
        (name, value) if !name.is_empty() && !value.is_empty() => format!("{name} · {value}"),
        (name, value) => name + &value,
    }
}

/// How far along the tasks are together, 0 to 1: the mean of those whose amount of work is known. `None` while none
/// is, and the ring sweeps.
pub fn overall(tasks: &[TaskInfo]) -> Option<f32> {
    let known: Vec<f32> = tasks.iter().filter(|t| t.total > 0).map(|t| (t.done.min(t.total) as f64 / t.total as f64) as f32).collect();
    if known.is_empty() {
        return None;
    }
    Some((known.iter().sum::<f32>() / known.len() as f32).clamp(0.0, 1.0))
}

/// The list's popup. Its open state is egui's, like a menu's: Esc and a click elsewhere close it, and so does opening
/// an in-window menu. A native menu (the macOS menu bar) does not: AppKit tracks it outside the window, egui sees no
/// event, and `muda` has no "will open" hook, so the list stays until one of the above or a dialog closes it.
pub fn popup_id() -> egui::Id {
    egui::Id::unique("activity-list")
}

/// Whether the list is open.
pub fn is_open(ctx: &egui::Context) -> bool {
    egui::Popup::is_id_open(ctx, popup_id())
}

/// The bell in the top bar and, when open, the list under it (called by the top bar every frame). `icon` is the
/// bell's square; the line naming what runs goes left of it, no further than `room_left`.
pub fn bell(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, icon: Rect, room_left: f32) {
    let ctx = ui.ctx().clone();
    // newest first: the one just started is the one looked for
    let mut all = app.session.activity.list();
    all.reverse();
    if !all.is_empty() {
        // look again soon: a young task becomes due, the ring and the bars move
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    // the bell shows the tasks old enough not to be a flash; the open list shows them all
    let tasks: Vec<TaskInfo> = all.iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).cloned().collect();
    let text = readout_text(&tasks);
    let (name, value) = readout_parts(&tasks);
    // The line beside the bell, in the room the bar has for it. What changes (the percentage, the count) is kept
    // whole; the task's name is cut short to fit, and left out when there is hardly room for any of it.
    let room = (icon.left() - READOUT_GAP - room_left).min(READOUT_MAX);
    let font = t.font(12.0);
    let value = (!value.is_empty()).then(|| ui.painter().layout_no_wrap(value, font.clone(), t.text_label));
    let value = value.filter(|g| g.size().x <= room);
    let dot = ui.painter().layout_no_wrap(" · ".into(), font.clone(), t.text_label);
    let name_room = room - value.as_ref().map_or(0.0, |g| g.size().x + dot.size().x);
    let name = (!name.is_empty() && name_room >= READOUT_NAME_MIN).then(|| {
        let mut job = egui::text::LayoutJob::simple_singleline(name, font, t.text_label);
        job.wrap = egui::text::TextWrapping { max_width: name_room, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        ui.painter().layout_job(job)
    });
    // name · value, the dot only between the two
    let dot = (name.is_some() && value.is_some()).then_some(dot);
    let line: Vec<_> = [name, dot, value].into_iter().flatten().collect();
    let line_w: f32 = line.iter().map(|g| g.size().x).sum();
    let has_readout = line_w > 0.0;
    let chip = if has_readout { Rect::from_min_max(pos2(icon.left() - READOUT_GAP - line_w, icon.top()), icon.max) } else { icon };
    let was_open = is_open(&ctx);
    let resp = ui.interact(chip, egui::Id::unique(("top", "bell")), Sense::click());
    let label = match tasks.len() {
        0 => tr("Tasks").to_string(),
        1 => format!("{}: {text}", tr("Tasks")),
        _ => text,
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, was_open, label.as_str()));
    register(ui, "icon:bell", icon);
    let p = ui.painter();
    if was_open {
        p.rect_filled(icon.expand(2.0), 4.0, t.tool_active);
    }
    if resp.has_focus() {
        p.rect_stroke(chip.expand(2.0), 4.0, Stroke::new(1.5, progress_ink(t)), StrokeKind::Outside);
    }
    let lit = resp.hovered() || was_open;
    let ink = if lit { t.text } else { t.text_label };
    if has_readout {
        register(ui, "activity:readout", Rect::from_min_max(chip.min, pos2(icon.left() - READOUT_GAP, icon.bottom())));
        let mut x = chip.left();
        for galley in line {
            let w = galley.size().x;
            p.galley_with_override_text_color(pos2(x, icon.center().y - galley.size().y / 2.0), galley, ink);
            x += w;
        }
    }
    if tasks.is_empty() {
        paint(p, icon.shrink(5.0), Icon::Bell, if lit { t.text } else { t.icon });
    } else {
        // a smaller bell inside the ring
        paint(p, icon.shrink(7.5), Icon::Bell, if lit { t.text } else { t.icon });
        ring(ui, t, icon, &tasks);
        register(ui, "activity:ring", icon);
        if tasks.len() > 1 && !has_readout {
            // no room to say how many: a count at the bell's shoulder, clear of the ring
            let c = icon.center() + vec2(15.5, -12.5);
            p.circle_filled(c, 6.5, progress_ink(t));
            p.text(c, egui::Align2::CENTER_CENTER, tasks.len().min(99).to_string(), t.semibold(9.5), t.canvas);
            register(ui, "activity:count", Rect::from_center_size(c, vec2(13.0, 13.0)));
        }
    }
    // a dialog, a notice or the question before quitting takes the window: the list gives way to it
    let asked = app.activity_set.take();
    let set = if blocked(app).is_some() {
        Some(egui::SetOpenCommand::Bool(false))
    } else if let Some(open) = asked {
        // `view.tasks` (the Window menu, an agent)
        Some(egui::SetOpenCommand::Bool(open))
    } else {
        resp.clicked().then_some(egui::SetOpenCommand::Toggle)
    };
    let resp = if was_open { resp } else { resp.on_hover_text(label.as_str()) };
    let mut cancel = None;
    egui::Popup::from_response(&resp)
        .id(popup_id())
        .open_memory(set)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(6.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| cancel = list(ui, t, &all));
    app.activity_open = is_open(&ctx);
    if let Some(id) = cancel {
        self::cancel(app, id);
    }
}

/// Why the list can't be shown now, if it can't: something else has the window, or the top bar isn't drawn.
pub(crate) fn blocked(app: &LightcraftApp) -> Option<&'static str> {
    if app.ui.fullscreen {
        Some("the top bar isn't shown in Full Screen Preview")
    } else if app.ui.dialog.is_some() {
        Some("a dialog is open")
    } else if app.quit_prompt.is_some() || !app.notices.is_empty() {
        Some("a question is waiting for its answer")
    } else if crate::panels::library_problem::blocks_dialog_input(app) {
        Some("the library window is waiting for its answer")
    } else {
        None
    }
}

/// The ring round the bell: an arc from the top for how far along the tasks are, or a quarter going round while
/// that isn't known.
fn ring(ui: &egui::Ui, t: &Tokens, icon: Rect, tasks: &[TaskInfo]) {
    let p = ui.painter();
    let c = icon.center();
    p.circle_stroke(c, RING, Stroke::new(RING_WIDTH, t.track));
    let (from, sweep) = match overall(tasks) {
        // (a sliver at the start, so a task at 0 % shows it has begun)
        Some(frac) => (-FRAC_PI_2, frac.max(0.02) * TAU),
        None => (-FRAC_PI_2 + (ui.input(|i| i.time) * 0.7).fract() as f32 * TAU, FRAC_PI_2),
    };
    let steps = ((sweep / TAU) * 48.0).ceil().max(2.0) as usize;
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let a = from + sweep * i as f32 / steps as f32;
            c + RING * vec2(a.cos(), a.sin())
        })
        .collect();
    let stopping = tasks.iter().all(|task| task.cancelling);
    p.add(egui::Shape::line(points, Stroke::new(RING_WIDTH, if stopping { t.text_dim } else { progress_ink(t) })));
}

/// The colour progress is drawn in: the accent, lightened or darkened towards the text colour in the themes where it
/// would be too close to the panel behind it to see (3:1, WCAG 1.4.11).
fn progress_ink(t: &Tokens) -> egui::Color32 {
    let mut ink = t.accent;
    for step in 1..=10 {
        if contrast(ink, t.chrome) >= 3.0 {
            break;
        }
        ink = t.accent.lerp_to_gamma(t.text, step as f32 / 10.0);
    }
    ink
}

/// The WCAG contrast ratio of two opaque colours.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    fn luminance(c: egui::Color32) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The list under the bell: its name, then a row a task (or that nothing runs). The id of the task whose ✕ was
/// clicked.
fn list(ui: &mut egui::Ui, t: &Tokens, tasks: &[TaskInfo]) -> Option<u64> {
    let screen = ui.ctx().content_rect();
    let width = WIDTH.min(screen.width() - 40.0).max(160.0);
    let mut cancel = None;
    // A popup lays out in the size it had last frame, and the scroll area would take that as all the room there is:
    // the list would never grow when rows are added. Give it the whole height below the top bar instead (the scroll
    // area keeps to `max_height`).
    ui.set_max_height((screen.height() - t.top_bar_h - 16.0).max(80.0));
    let r = ui
        .vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.add(egui::Label::new(RichText::new(tr("Tasks")).font(t.semibold(12.5)).color(t.text)));
            ui.add_space(5.0);
            if tasks.is_empty() {
                let r = ui
                    .vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(tr("No tasks are running")).font(t.font(12.5)).color(t.text_label)));
                        let hint = tr("Imports, exports and other long jobs show here while they run.");
                        ui.add(egui::Label::new(RichText::new(hint).font(t.font(11.0)).color(t.text_dim)).wrap());
                    })
                    .response;
                register(ui, "activity:empty", r.rect);
                return;
            }
            // as many rows as the window has room for; the rest scroll
            let max_height = (screen.height() - t.top_bar_h - 96.0).max(80.0);
            egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                for (i, task) in tasks.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    if row(ui, t, task, width) {
                        cancel = Some(task.id);
                    }
                }
            });
        })
        .response;
    register(ui, "activity:list", r.rect);
    cancel
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
                crate::widgets::announce(&resp, format!("{}: {}", tr("Cancel"), tr(&task.label)));
                paint(ui.painter(), rect.shrink(2.0), Icon::Close, if resp.hovered() || resp.has_focus() { t.text } else { t.icon });
                // (a row scrolled out of view has nothing to click: its ✕ is the list's to bring back first)
                if ui.is_rect_visible(rect) {
                    register(ui, format!("activity:cancel:{}", task.id), rect);
                }
                clicked = resp.on_hover_text(tr("Cancel")).clicked();
            }
        });
        bar(ui, task, width);
        let line = detail_line(task);
        if !line.is_empty() {
            ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        }
    });
    if ui.is_rect_visible(r.response.rect) {
        register(ui, format!("activity:row:{}", task.id), r.response.rect);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_can_be_told_from_the_panel_in_every_theme() {
        for kind in crate::theme::ThemeKind::ALL {
            let t = Tokens::for_kind(kind);
            let ratio = contrast(progress_ink(&t), t.chrome);
            assert!(ratio >= 3.0, "{}: the ring's arc is {ratio:.2}:1 on the bar", kind.id());
        }
    }
}
