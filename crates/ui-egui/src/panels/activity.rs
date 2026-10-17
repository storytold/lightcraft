//! The activity stack (issue #345): every long-running task in one place, under the bell in the top bar. While
//! something runs the bell wears a ring (how far along the work is, or a sweep while that isn't known), and the bar
//! names the task beside it when there is room. A click opens the list: a row each with its name, a progress bar,
//! what it is working on, and ✕ for the ones that can be cancelled. Nothing floats over the panels, so the list
//! covers them only while it is open. A task that said how it ended stays in the list until it is dismissed: what it
//! did, what went wrong and for which files, and where its results are. One that did not end well comes first there,
//! and puts a mark on the bell until it is dismissed. The tasks come from the engine's registry
//! ([`lightcraft_engine::activity`]); ✕ runs `activity.cancel` like any other frontend would.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Align, Layout, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_engine::activity::{Ending, Finished, TaskInfo, Unit};
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
/// A task that just ended is named beside the bell for this long (while nothing runs).
const ANNOUNCE_MS: u64 = 6_000;
/// The failed files an opened row lists; "Copy all" has every one that was kept.
const FAILURES_SHOWN: usize = 5;
const MARK: f32 = 14.0;

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

/// The list's popup. Its open state is egui's, like a menu's: Esc and a click elsewhere close it, and opening a menu
/// closes it too.
pub fn popup_id() -> egui::Id {
    egui::Id::new("activity-list")
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
    let problems = app.session.activity.problems();
    // with nothing running, a task that just ended says beside the bell how it went
    let ended = app.session.activity.last_finished().filter(|f| tasks.is_empty() && f.age_ms < ANNOUNCE_MS).as_ref().map(summary_of);
    if ended.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    let (text, (name, value)) = match ended {
        Some(said) => (said.clone(), (said, String::new())),
        None => (readout_text(&tasks), readout_parts(&tasks)),
    };
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
    let resp = ui.interact(chip, egui::Id::new(("top", "bell")), Sense::click());
    let label = match tasks.len() {
        0 if text.is_empty() => tr("Tasks").to_string(),
        0 | 1 => format!("{}: {text}", tr("Tasks")),
        _ => text,
    };
    let label = if problems > 0 { format!("{label} · {}", tr("Needs attention")) } else { label };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, was_open, label.as_str()));
    register(&ctx, "icon:bell", icon);
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
        register(&ctx, "activity:readout", Rect::from_min_max(chip.min, pos2(icon.left() - READOUT_GAP, icon.bottom())));
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
        register(&ctx, "activity:ring", icon);
        if tasks.len() > 1 && !has_readout && problems == 0 {
            // no room to say how many: a count at the bell's shoulder, clear of the ring
            let c = icon.center() + vec2(15.5, -12.5);
            p.circle_filled(c, 6.5, progress_ink(t));
            p.text(c, egui::Align2::CENTER_CENTER, tasks.len().min(99).to_string(), t.semibold(9.5), t.canvas);
            register(&ctx, "activity:count", Rect::from_center_size(c, vec2(13.0, 13.0)));
        }
    }
    if problems > 0 {
        // a task did not end well and its row has not been dismissed: a mark at the bell's shoulder
        let c = icon.center() + vec2(15.5, -12.5);
        p.circle_filled(c, 6.5, t.caution);
        p.text(c, egui::Align2::CENTER_CENTER, "!", t.semibold(9.5), t.canvas);
        register(&ctx, "activity:attention", Rect::from_center_size(c, vec2(13.0, 13.0)));
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
    let mut act = None;
    let (activity, opened, reveal) = (app.session.activity.clone(), app.activity_failures, app.services.reveal.is_some());
    egui::Popup::from_response(&resp)
        .id(popup_id())
        .open_memory(set)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(6.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            // (the ended tasks with all their failed files are only read while the list is on screen)
            let finished = activity.finished();
            act = list(ui, t, &Rows { tasks: &all, finished: &finished, opened, reveal });
        });
    app.activity_open = is_open(&ctx);
    match act {
        Some(Act::Cancel(id)) => self::cancel(app, id),
        Some(Act::Dismiss(id)) => {
            // refusal expected: the row may have been dismissed by an agent between the frame that drew ✕ and the click
            let _ = app.session.execute("activity.dismiss", &json!({"id": id}));
            if app.activity_failures == Some(id) {
                app.activity_failures = None;
            }
        }
        Some(Act::Clear) => {
            // refusal expected: none (`all` never fails); nothing to say if it did
            let _ = app.session.execute("activity.dismiss", &json!({"all": true}));
            app.activity_failures = None;
        }
        Some(Act::Failures(id)) => app.activity_failures = (app.activity_failures != Some(id)).then_some(id),
        Some(Act::Copy(text)) => ctx.copy_text(text),
        Some(Act::Reveal(path)) => {
            if let Some(Err(e)) = app.services.reveal.as_mut().map(|reveal| reveal(&path)) {
                app.toast(&ctx, e);
            }
        }
        None => {}
    }
}

/// What the list shows.
struct Rows<'a> {
    /// Running, newest first.
    tasks: &'a [TaskInfo],
    /// Ended, newest first.
    finished: &'a [Finished],
    /// The ended task whose failed files are listed.
    opened: Option<u64>,
    /// The host can show a folder in the file manager.
    reveal: bool,
}

/// What was asked for in the list.
enum Act {
    Cancel(u64),
    Dismiss(u64),
    Clear,
    /// List the failed files of an ended task, or put them away again.
    Failures(u64),
    Copy(String),
    Reveal(String),
}

/// What an ended task says of itself; one that said nothing is worded from its name and how it ended.
pub fn summary_of(f: &Finished) -> String {
    if !f.summary.is_empty() {
        return f.summary.clone();
    }
    let task = tr(&f.label);
    match f.ending {
        Ending::Done => tr_format!("{task}: done", task = task),
        Ending::Problems => tr_format!("{task}: done, with problems", task = task),
        Ending::Failed => tr_format!("{task}: failed", task = task),
        Ending::Stopped => tr_format!("{task}: stopped", task = task),
        Ending::Died => tr_format!("{task}: stopped unexpectedly", task = task),
    }
}

/// What else an ended task's row says: what the task said, or how many it could not do when it only counted them
/// (a preview build).
pub fn detail_of(f: &Finished) -> String {
    if f.detail.is_empty() && f.failures.is_empty() && f.failed > 0 { tr_format!("{n} failed", n = f.failed) } else { f.detail.clone() }
}

/// "took 42 s", "took 12 min", "took 1 h 52 min".
pub fn took(ran_ms: u64) -> String {
    let s = ran_ms / 1000;
    if s < 60 {
        tr_format!("took {n} s", n = s)
    } else if s < 3600 {
        tr_format!("took {n} min", n = s / 60)
    } else {
        tr_format!("took {h} h {m} min", h = s / 3600, m = (s % 3600) / 60)
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
    accent_at(t, 3.0)
}

/// The colour of a link in the list: the accent at the contrast text needs (4.5:1).
fn link_ink(t: &Tokens) -> egui::Color32 {
    accent_at(t, 4.5)
}

/// The accent, moved towards the text colour until it is `ratio`:1 against the panel.
fn accent_at(t: &Tokens, ratio: f32) -> egui::Color32 {
    let mut ink = t.accent;
    for step in 1..=10 {
        if contrast(ink, t.chrome) >= ratio {
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

/// The list under the bell: its name, then the tasks. Those that did not end well come first, then the running ones,
/// then those that ended well, each group under its name when there is more than one. What was asked for in it.
fn list(ui: &mut egui::Ui, t: &Tokens, rows: &Rows) -> Option<Act> {
    let screen = ui.ctx().content_rect();
    let width = WIDTH.min(screen.width() - 40.0).max(160.0);
    let mut act = None;
    // A popup lays out in the size it had last frame, and the scroll area would take that as all the room there is:
    // the list would never grow when rows are added. Give it the whole height below the top bar instead (the scroll
    // area keeps to `max_height`).
    ui.set_max_height((screen.height() - t.top_bar_h - 16.0).max(80.0));
    let (problems, done): (Vec<&Finished>, Vec<&Finished>) = rows.finished.iter().partition(|f| f.ending.needs_attention());
    let groups = [!problems.is_empty(), !rows.tasks.is_empty(), !done.is_empty()].iter().filter(|shown| **shown).count();
    let r = ui
        .vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(tr("Tasks")).font(t.semibold(12.5)).color(t.text)));
                if !rows.finished.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // (every ended task's row, those that did not go well too: named for what it does)
                        if link(ui, t, tr("Clear all"), "activity:clear") {
                            act = Some(Act::Clear);
                        }
                    });
                }
            });
            ui.add_space(5.0);
            if groups == 0 {
                let r = ui
                    .vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(tr("No tasks are running")).font(t.font(12.5)).color(t.text_label)));
                        let hint = tr("Imports, exports and other long jobs show here while they run.");
                        ui.add(egui::Label::new(RichText::new(hint).font(t.font(11.0)).color(t.text_dim)).wrap());
                    })
                    .response;
                register(ui.ctx(), "activity:empty", r.rect);
                return;
            }
            // as many rows as the window has room for; the rest scroll
            let max_height = (screen.height() - t.top_bar_h - 96.0).max(80.0);
            egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                let mut first = true;
                let mut group = |ui: &mut egui::Ui, name: &str| {
                    if !std::mem::take(&mut first) {
                        ui.add_space(10.0);
                    }
                    if groups > 1 {
                        ui.add(egui::Label::new(RichText::new(tr(name)).font(t.semibold(11.0)).color(t.text_dim)));
                        ui.add_space(2.0);
                    }
                };
                if !problems.is_empty() {
                    group(ui, "Needs attention");
                }
                for (i, f) in problems.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    act = finished_row(ui, t, f, width, rows).or(act.take());
                }
                if !rows.tasks.is_empty() {
                    group(ui, "Running");
                }
                for (i, task) in rows.tasks.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    if row(ui, t, task, width) {
                        act = Some(Act::Cancel(task.id));
                    }
                }
                if !done.is_empty() {
                    group(ui, "Finished");
                }
                for (i, f) in done.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    act = finished_row(ui, t, f, width, rows).or(act.take());
                }
            });
        })
        .response;
    register(ui.ctx(), "activity:list", r.rect);
    act
}

/// A link in the list (widget `id`). True when clicked.
fn link(ui: &mut egui::Ui, t: &Tokens, text: &str, id: impl Into<String>) -> bool {
    let r = ui.add(egui::Link::new(RichText::new(text).font(t.font(11.0)).color(link_ink(t))));
    if ui.is_rect_visible(r.rect) {
        register(ui.ctx(), id, r.rect);
    }
    r.clicked()
}

/// A task that ended: a mark for how, what it says of itself and ✕ to let go of it; what else needs reading and how
/// long it took; then what can be done with it (its failed files, its folder). What was asked for.
fn finished_row(ui: &mut egui::Ui, t: &Tokens, f: &Finished, width: f32, rows: &Rows) -> Option<Act> {
    let mut act = None;
    let said = summary_of(f);
    let r = ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (mark, _) = ui.allocate_exact_size(vec2(MARK, 18.0), Sense::hover());
            ending_mark(ui, t, Rect::from_center_size(mark.center(), vec2(MARK, MARK)), f.ending);
            let label_w = width - MARK - CROSS - 12.0;
            ui.allocate_ui_with_layout(vec2(label_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(label_w);
                ui.add(egui::Label::new(RichText::new(said.as_str()).font(t.font(12.5)).color(t.text)).truncate());
            });
            let (rect, resp) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{}: {said}", tr("Dismiss"))));
            paint(ui.painter(), rect.shrink(2.0), Icon::Close, if resp.hovered() || resp.has_focus() { t.text } else { t.icon });
            if ui.is_rect_visible(rect) {
                register(ui.ctx(), format!("activity:dismiss:{}", f.id), rect);
            }
            if resp.on_hover_text(tr("Dismiss")).clicked() {
                act = Some(Act::Dismiss(f.id));
            }
        });
        let took = took(f.ran_ms);
        let detail = detail_of(f);
        let line = if detail.is_empty() { took } else { format!("{detail} · {took}") };
        ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        // (the failed files can be listed when the job named them; an import only counts them, in the line above)
        let listed = !f.failures.is_empty();
        let path = f.path.as_deref().filter(|path| rows.reveal && !path.is_empty());
        if listed || path.is_some() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if listed && link(ui, t, &tr_format!("Show failed ({n})", n = f.failed), format!("activity:failures:{}", f.id)) {
                    act = Some(Act::Failures(f.id));
                }
                if let Some(path) = path
                    && link(ui, t, tr(crate::menus::reveal_label()), format!("activity:reveal:{}", f.id))
                {
                    act = Some(Act::Reveal(path.to_string()));
                }
            });
        }
        if rows.opened == Some(f.id) && !f.failures.is_empty() {
            // the first few, each with its reason; the whole list goes to the clipboard
            for failure in f.failures.iter().take(FAILURES_SHOWN) {
                let line = format!("{} — {}", failure.name, failure.reason);
                ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_label)).truncate());
            }
            // what is not shown here, and what was not kept at all, is said: nothing goes missing silently
            if f.failed > FAILURES_SHOWN {
                let more = tr_format!("and {n} more", n = f.failed - FAILURES_SHOWN);
                ui.add(egui::Label::new(RichText::new(more).font(t.font(11.0)).color(t.text_dim)));
            }
            if link(ui, t, tr("Copy all"), format!("activity:copy:{}", f.id)) {
                act = Some(Act::Copy(failures_text(f)));
            }
        }
    });
    if ui.is_rect_visible(r.response.rect) {
        register(ui.ctx(), format!("activity:done:{}", f.id), r.response.rect);
    }
    act
}

/// The failed files of an ended task for the clipboard: a line each, the name and the reason with a tab between,
/// and a last line saying how many more there were when not all were kept.
pub fn failures_text(f: &Finished) -> String {
    let one_line = |text: &str| text.replace(['\n', '\r', '\t'], " ");
    let mut lines: Vec<String> = f.failures.iter().map(|x| format!("{}\t{}", one_line(&x.name), one_line(&x.reason))).collect();
    if f.failed > f.failures.len() {
        lines.push(tr_format!("and {n} more", n = f.failed - f.failures.len()));
    }
    lines.join("\n")
}

/// How a task ended, as a mark that differs in shape as well as colour: a tick, a warning triangle, a square for
/// one that was stopped.
fn ending_mark(ui: &egui::Ui, t: &Tokens, rect: Rect, ending: Ending) {
    let p = ui.painter();
    match ending {
        Ending::Done => paint(p, rect, Icon::Check, t.text_label),
        Ending::Stopped => {
            p.rect_stroke(rect.shrink(3.5), 1.0, Stroke::new(1.5, t.text_dim), StrokeKind::Inside);
        }
        Ending::Problems | Ending::Failed | Ending::Died => {
            let (c, h) = (rect.center(), rect.height() / 2.0 - 1.0);
            let triangle = vec![c + vec2(0.0, -h), c + vec2(h, h * 0.85), c + vec2(-h, h * 0.85)];
            p.add(egui::Shape::closed_line(triangle, Stroke::new(1.3, t.caution)));
            p.line_segment([c + vec2(0.0, -h * 0.35), c + vec2(0.0, h * 0.25)], Stroke::new(1.3, t.caution));
            p.circle_filled(c + vec2(0.0, h * 0.55), 0.8, t.caution);
        }
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
                crate::widgets::announce(&resp, format!("{}: {}", tr("Cancel"), tr(&task.label)));
                paint(ui.painter(), rect.shrink(2.0), Icon::Close, if resp.hovered() || resp.has_focus() { t.text } else { t.icon });
                // (a row scrolled out of view has nothing to click: its ✕ is the list's to bring back first)
                if ui.is_rect_visible(rect) {
                    register(ui.ctx(), format!("activity:cancel:{}", task.id), rect);
                }
                clicked = resp.on_hover_text(tr("Cancel")).clicked();
            }
        });
        bar(ui, t, task, width);
        let line = detail_line(task);
        if !line.is_empty() {
            ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        }
    });
    if ui.is_rect_visible(r.response.rect) {
        register(ui.ctx(), format!("activity:row:{}", task.id), r.response.rect);
    }
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
