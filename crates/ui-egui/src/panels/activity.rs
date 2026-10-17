//! Running tasks and recent notifications (issue #345), out of the way of the photos: the top bar's bell. While
//! something runs, a ring around the bell shows how far it has got (a segment sweeps while the amount of work isn't
//! known) and, where the bar has room, the task's name and count sit beside it; a dot says notifications came in.
//! A click opens the popover: a row for each task with its name, a progress bar, what it is working on and ✕ for the
//! ones that can be cancelled, and below them the recent notifications ([`crate::notifications`]) with a Details
//! link for those that have a report. The tasks come from the engine's registry ([`lightcraft_engine::activity`]);
//! ✕ runs `activity.cancel` like any other frontend would.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Align, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_engine::activity::{TaskInfo, Unit};
use serde_json::json;

use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

/// A task shows once it has run this long, so quick ones don't flash.
pub const SHOW_AFTER_MS: u64 = 500;
/// The popover's width (points).
pub const WIDTH: f32 = 320.0;
/// The widest the running task's name and count get beside the bell.
const LABEL_MAX_W: f32 = 220.0;

const MARGIN: i8 = 10;
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

/// The tasks on show: those that have run half a second.
fn visible(app: &LightcraftApp) -> Vec<TaskInfo> {
    app.session.activity.list().into_iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).collect()
}

/// How far the running tasks have got together (0..=1), or `None` while one of them doesn't know yet.
pub fn overall(tasks: &[TaskInfo]) -> Option<f32> {
    if tasks.is_empty() || tasks.iter().any(|t| t.total == 0) {
        return None;
    }
    // each task counts the same, whatever its unit
    let sum: f64 = tasks.iter().map(|t| (t.done as f64 / t.total as f64).clamp(0.0, 1.0)).sum();
    Some((sum / tasks.len() as f64) as f32)
}

/// What the bar says beside the bell: the task and its count, or how many run.
pub fn label_text(tasks: &[TaskInfo]) -> String {
    match tasks {
        [] => String::new(),
        [t] => match count_text(t) {
            c if c.is_empty() => tr(&t.label).to_string(),
            c => format!("{} · {c}", tr(&t.label)),
        },
        _ => format!("{}: {}", tr("Tasks running"), tasks.len()),
    }
}

/// An arc around `c` from `from` radians, `sweep` radians clockwise (screen coordinates).
fn arc(p: &egui::Painter, c: Pos2, r: f32, from: f32, sweep: f32, stroke: Stroke) {
    let n = ((sweep.abs() / TAU) * 48.0).ceil().max(2.0) as usize;
    let points: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = from + sweep * i as f32 / n as f32;
            c + r * vec2(a.cos(), a.sin())
        })
        .collect();
    p.add(egui::Shape::line(points, stroke));
}

/// The bell at the left of the top bar, laid out next in `ui` (a left-to-right row), with up to `label_room` points
/// right of it for the running task's name. Draws the ring, the dot and the name; a click opens or closes the
/// popover.
pub fn bell(app: &mut LightcraftApp, ui: &mut egui::Ui, label_room: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
    let tasks = visible(app);
    if !app.session.activity.list().is_empty() {
        // look again soon: a young task becomes due, bars move
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("activity-bell"), r));
    // the name and count right of the bell, when the bar has room for them
    let mut hit = r;
    if !tasks.is_empty() {
        let room = label_room.min(LABEL_MAX_W);
        if room >= 70.0 {
            let mut job = egui::text::LayoutJob::simple_singleline(label_text(&tasks), t.font(12.0), t.text_dim);
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let g = ui.painter().layout_job(job);
            let (slot, _) = ui.allocate_exact_size(vec2(g.size().x + 2.0, 28.0), Sense::hover());
            let lr = Rect::from_min_size(pos2(slot.left() + 2.0, slot.center().y - g.size().y / 2.0), g.size());
            ui.painter().galley(lr.min, g, t.text_dim);
            register(ui.ctx(), "activity:label", lr);
            hit = hit.union(lr.expand2(vec2(4.0, 6.0)));
        }
    }
    let resp = ui.interact(hit, egui::Id::new(("top", "bell")), Sense::click());
    let tip = if tasks.is_empty() {
        tr("Activity").to_string()
    } else {
        tasks.iter().map(|t| format!("{}  {}", tr(&t.label), detail_line(t))).collect::<Vec<_>>().join("\n")
    };
    let resp = resp.on_hover_text(tip.clone());
    crate::widgets::announce(&resp, tip);
    register(ui.ctx(), "icon:bell", r);
    let open = app.notifications.open;
    if open {
        ui.painter().rect_filled(r, 4.0, t.pressed);
    }
    paint(ui.painter(), r.shrink(if tasks.is_empty() { 5.0 } else { 7.0 }), Icon::Bell, if resp.hovered() || open { t.text } else { t.icon });
    if !tasks.is_empty() {
        let c = r.center();
        let radius = r.width() / 2.0 - 1.5;
        let p = ui.painter();
        p.circle_stroke(c, radius, Stroke::new(2.0, t.field_border));
        let colour = if tasks.iter().all(|t| t.cancelling) { t.text_dim } else { t.accent };
        match overall(&tasks) {
            Some(f) if f > 0.0 => arc(p, c, radius, -FRAC_PI_2, TAU * f, Stroke::new(2.0, colour)),
            Some(_) => {}
            None => {
                let phase = (ui.input(|i| i.time) * 1.2).fract() as f32;
                arc(p, c, radius, -FRAC_PI_2 + TAU * phase, TAU * 0.28, Stroke::new(2.0, colour));
            }
        }
        register(ui.ctx(), "activity:ring", r);
    }
    if app.notifications.unread > 0 && !open {
        let c = r.right_top() + vec2(-5.0, 6.0);
        ui.painter().circle_filled(c, 4.0, if app.notifications.unread_warning { t.caution } else { t.accent });
        register(ui.ctx(), "indicator:unread", Rect::from_center_size(c, vec2(8.0, 8.0)));
    }
    if resp.clicked() {
        let _ = app.run("view.activity", json!({}));
    }
}

/// Open (`Some(true)`), close or toggle (`None`) the bell's popover. Opening it marks the notifications read.
pub fn set_open(app: &mut LightcraftApp, open: Option<bool>) {
    let open = open.unwrap_or(!app.notifications.open);
    app.notifications.open = open;
    if open {
        app.notifications.mark_read();
    }
}

/// Draw the popover while it is open (called every frame, after the panels and dialogs).
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.notifications.open {
        return;
    }
    let Some(bell) = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("activity-bell"))) else { return };
    let t = Tokens::get(ctx);
    let tasks = visible(app);
    let now = ctx.input(|i| i.time);
    let mut cancel = None;
    let mut details = None;
    let mut clear = false;
    let screen = ctx.content_rect();
    let left = bell.left().clamp(screen.left() + 8.0, (screen.right() - WIDTH - 8.0).max(screen.left()));
    let max_h = (screen.bottom() - bell.bottom() - 24.0).max(120.0);
    let frame = egui::Frame::NONE
        .fill(t.chrome)
        .stroke(Stroke::new(1.0, t.field_border))
        .corner_radius(8)
        .inner_margin(Margin::same(MARGIN))
        .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: egui::Color32::from_black_alpha(90) });
    let area = egui::Area::new(egui::Id::new("activity-popover")).order(egui::Order::Foreground).fixed_pos(pos2(left, bell.bottom() + 6.0)).show(
        ctx,
        |ui| {
            ui.set_max_height(max_h);
            frame.show(ui, |ui| {
                let inner = WIDTH - 2.0 * (f32::from(MARGIN) + 1.0);
                ui.set_width(inner);
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(RichText::new(tr("Activity")).font(t.semibold(13.0)).color(t.text));
                if tasks.is_empty() {
                    ui.label(RichText::new(tr("Nothing running")).font(t.font(12.0)).color(t.text_dim));
                } else {
                    egui::ScrollArea::vertical().id_salt("activity-tasks").max_height((max_h * 0.45).max(80.0)).auto_shrink([false, true]).show(
                        ui,
                        |ui| {
                            // (the room the scroll area leaves: a scroll bar's, when it shows one)
                            let width = ui.available_width();
                            for (i, task) in tasks.iter().enumerate() {
                                if i > 0 {
                                    ui.add_space(6.0);
                                }
                                if row(ui, &t, task, width) {
                                    cancel = Some(task.id);
                                }
                            }
                        },
                    );
                }
                ui.add_space(4.0);
                ui.separator();
                ui.allocate_ui_with_layout(vec2(inner, 18.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new(tr("Recent")).font(t.semibold(13.0)).color(t.text));
                    if !app.notifications.items.is_empty() {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let r = ui.add(egui::Label::new(RichText::new(tr("Clear")).font(t.font(11.5)).color(t.accent)).sense(Sense::click()));
                            register(ui.ctx(), "activity:clear", r.rect);
                            clear = r.clicked();
                        });
                    }
                });
                if app.notifications.items.is_empty() {
                    ui.label(RichText::new(tr("Nothing yet")).font(t.font(12.0)).color(t.text_dim));
                } else {
                    egui::ScrollArea::vertical().id_salt("activity-recent").max_height((max_h * 0.5).max(100.0)).auto_shrink([false, true]).show(
                        ui,
                        |ui| {
                            for n in &app.notifications.items {
                                ui.add_space(4.0);
                                let r = ui.vertical(|ui| {
                                    ui.set_width(inner);
                                    ui.horizontal_top(|ui| {
                                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
                                        ui.painter().circle_filled(
                                            pos2(dot.center().x, dot.top() + 8.0),
                                            3.0,
                                            if n.warning { t.caution } else { t.text_dim },
                                        );
                                        let mut job = egui::text::LayoutJob::simple(n.text.clone(), t.font(12.0), t.text_label, inner - 16.0);
                                        job.wrap.max_rows = 3;
                                        ui.add(egui::Label::new(job).selectable(true));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.add_space(16.0);
                                        ui.label(RichText::new(crate::notifications::age_text(now, n.time)).font(t.font(11.0)).color(t.text_dim));
                                        if n.report.is_some() {
                                            let r = ui.add(
                                                egui::Label::new(RichText::new(tr("Show Details")).font(t.font(11.0)).color(t.accent))
                                                    .sense(Sense::click()),
                                            );
                                            register(ui.ctx(), format!("notification:details:{}", n.id), r.rect);
                                            if r.clicked() {
                                                details = Some(n.id);
                                            }
                                        }
                                    });
                                });
                                register(ui.ctx(), format!("notification:{}", n.id), r.response.rect);
                            }
                        },
                    );
                }
            });
        },
    );
    register(ctx, "activity:popover", area.response.rect);
    if let Some(id) = cancel {
        self::cancel(app, id);
    }
    if clear {
        app.notifications.clear();
    }
    if let Some(id) = details
        && app.show_report(id)
    {
        app.notifications.open = false;
    }
    // Esc, or a click anywhere else (the bell's own click toggles it), closes it
    let outside =
        ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !bell.contains(p)));
    if outside {
        app.notifications.open = false;
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
                register(ui.ctx(), format!("activity:cancel:{}", task.id), rect);
                clicked = resp.on_hover_text(tr("Cancel")).clicked();
            }
        });
        bar(ui, t, task, width);
        let line = detail_line(task);
        if !line.is_empty() {
            ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        }
    });
    register(ui.ctx(), format!("activity:row:{}", task.id), r.response.rect);
    clicked
}

/// The progress bar; while the amount of work isn't known, a segment sweeps across.
fn bar(ui: &mut egui::Ui, t: &Tokens, task: &TaskInfo, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 5.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.5, t.field);
    let fill = if task.total > 0 {
        let frac = (task.done as f64 / task.total as f64).clamp(0.0, 1.0) as f32;
        Rect::from_min_size(rect.min, vec2(rect.width() * frac, rect.height()))
    } else {
        let phase = (ui.input(|i| i.time) * 0.7).fract() as f32;
        let w = rect.width() * 0.3;
        let x = rect.left() + (rect.width() + w) * phase - w;
        Rect::from_min_max(pos2(x.max(rect.left()), rect.top()), pos2((x + w).min(rect.right()), rect.bottom()))
    };
    if fill.width() > 0.0 {
        p.rect_filled(fill, 2.5, if task.cancelling { t.text_dim } else { t.accent });
    }
}
