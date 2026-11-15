//! The activity stack (issue #345): every long-running task in one place while something runs — a row each with its
//! name, a progress bar, what it is working on, and ✕ for the ones that can be cancelled. The tasks come from the
//! engine's registry ([`lcc_engine::activity`]); ✕ runs `activity.cancel` like any other frontend would.
//!
//! Where it shows depends on the layout:
//! - **Classic** (as Lightroom Classic's activity centre): in the module bar, in the identity plate's place while
//!   tasks run ([`plate`]). One task shows its name, its bar and ✕; several show "3 operations in progress…" over a thin bar
//!   each. A click there opens the list of every task under it ([`show`], `view.activityList`); Esc or a click
//!   outside closes it. With the module bar hidden, or Show Status and Activity off, nothing shows (the tasks run on).
//! - **Modern**: a floating card under the Library / Develop switch, three rows and "+N more".
//!
//! A batch (AI Denoise or an export over many photos) is one row: "37 of 200 · about
//! 6 min left · 2 failed". Its per-photo rows fold under it ("Show each photo"), and when it ends its summary shows
//! once as a message ("AI Denoise: 198 done, 2 failed: why").
//!
//! A continuous job with `controls` (the face scan, search indexing) has a pause button (resume while paused: it stays
//! on the stack, saying "Paused") and a gear that opens its Settings page; they run the job's own commands.
//!
//! A task whose stop throws work away (`stopWarning`: a plug-in's wait for a program) has no ✕ anywhere. The list and
//! the Modern card give it "Stop Waiting…", which asks first ([`Dialog::StopWaiting`]); its confirm button runs
//! `activity.cancel`.

use egui::{Align, Layout, Margin, Rect, RichText, Sense, Stroke, pos2, vec2};
use lcc_engine::activity::{BatchSummary, Controls, TaskInfo, Unit, fill};
use serde_json::json;

use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::icons::{Icon, paint};
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

/// A task shows once it has run this long, so quick ones don't flash.
pub const SHOW_AFTER_MS: u64 = 500;
/// Rows shown before "+N more".
pub const MAX_ROWS: usize = 3;
/// The stack's width (points), and the Classic list's.
pub const WIDTH: f32 = 280.0;
/// The Classic list's widget id and area.
pub const LIST: &str = "activity:list";
/// The Classic header's widget id: the identity plate's activity area, which opens the list.
pub const PLATE: &str = "activity:plate";
/// A thin bar in the header, one per task while several run, and the gap between two.
const THIN: f32 = 4.0;
const THIN_GAP: f32 = 2.0;

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

/// The task's name, translated: a label with names ("Waiting for {program} to finish") is translated first and the
/// names put in after.
pub fn label_text(t: &TaskInfo) -> String {
    match &t.templates {
        Some(x) => fill(tr(&x.label), &x.names),
        None => tr(&t.label).to_string(),
    }
}

/// What the task is on: a name as given, or a text with names translated around them.
fn detail_text(t: &TaskInfo) -> String {
    match &t.templates {
        Some(x) if !x.detail.is_empty() => fill(tr(&x.detail), &x.names),
        // a detail is a name, kept as it is, or one of the engine's few messages, such as what a batch waits for
        _ => tr(&t.detail).to_string(),
    }
}

/// What stopping the task costs, translated, for a task whose stop throws work away.
fn stop_warning_text(t: &TaskInfo) -> Option<String> {
    match t.templates.as_ref().and_then(|x| x.stop_warning.as_ref().map(|w| (w, &x.names))) {
        Some((w, names)) => Some(fill(tr(w), names)),
        None => t.stop_warning.as_deref().map(|w| tr(w).to_string()),
    }
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
    let detail = detail_text(t);
    match (count.is_empty(), detail.is_empty()) {
        (false, false) => format!("{count} · {detail}"),
        (false, true) => count,
        (true, _) => detail,
    }
}

/// Draw the stack (called every frame, after the panels and dialogs): the Modern card, or Classic's list under the
/// module bar's activity area when it is open.
pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    announce_finished(app, ctx);
    // where the module bar drew the tasks this frame (Classic), kept for the list and `view.activityList`
    let plate = app.activity_plate_drawn.take();
    app.activity_plate = plate;
    let all = app.session.activity.list();
    // a Stop Waiting question about a task that has ended has nothing left to stop
    if let Some(Dialog::StopWaiting { id, .. }) = &app.ui.dialog
        && !all.iter().any(|t| t.id == *id)
    {
        app.ui.dialog = None;
    }
    app.activity_bottom = None;
    if all.is_empty() {
        app.activity_expanded = false;
        app.activity_open.clear();
        app.activity_list = false;
        return;
    }
    // look again soon: a young task becomes due, bars move
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let due: Vec<&TaskInfo> = all.iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).collect();
    if due.is_empty() {
        app.activity_list = false;
        return;
    }
    app.activity_open.retain(|id| all.iter().any(|t| t.id == *id));
    let tasks = ordered(&due, &app.activity_open);
    if !crate::modern::on(app) {
        list(app, ctx, &tasks, plate);
        return;
    }
    // the Modern card has no list (one opened in Classic closes when the layout changes)
    app.activity_list = false;
    let t = Tokens::get(ctx);
    let shown = if app.activity_expanded { tasks.len() } else { tasks.len().min(MAX_ROWS) };
    let hidden = tasks.len().saturating_sub(shown);
    // centred under its Library / Develop switch, inside the screen's insets (clear of the buttons in the corners
    // and of the cards)
    let origin = pos2(ctx.content_rect().center().x - WIDTH / 2.0, crate::modern::overlay_origin(app).y);
    let top = origin.y - 8.0;
    let max_height = (ctx.content_rect().height() - top - 60.0).max(80.0);
    let mut picked = Picked::default();
    let mut toggle = false;
    let open = app.activity_open.clone();
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.field_border)).corner_radius(6).inner_margin(Margin::same(MARGIN));
    let area = egui::Area::new(egui::Id::new("activity-stack")).order(egui::Order::Middle).fixed_pos(origin).show(ctx, |ui| {
        // An area lays out in the size it had last frame, and the scroll area would take that as all the room there
        // is: the stack would never grow when rows are added. Give it the whole height below the top bar instead
        // (the scroll area keeps to `max_height`).
        ui.set_max_height((ctx.content_rect().height() - top - 16.0).max(80.0));
        frame.show(ui, |ui| {
            picked = rows(ui, &t, tasks.get(..shown).unwrap_or(&tasks), &open, max_height);
            if hidden > 0 || (app.activity_expanded && tasks.len() > MAX_ROWS) {
                ui.add_space(4.0);
                let text = if hidden > 0 { tr_format!("+{n} more", n = hidden) } else { tr("Show less").to_string() };
                let r = ui.add(egui::Label::new(RichText::new(text).font(t.font(11.0)).color(t.accent)).sense(Sense::click()));
                register(ui.ctx(), "activity:more", r.rect);
                toggle = r.clicked();
            }
        });
    });
    // what draws under the stack (the eye strip's messages) keeps clear of it
    app.activity_bottom = Some(area.response.rect.bottom());
    apply(app, picked);
    if toggle {
        app.activity_expanded = !app.activity_expanded;
    }
}

/// What a click in the rows asked for: ✕ on a task, Stop Waiting… on one, a batch's photos opened or folded, or a
/// job's pause, resume or gear.
#[derive(Default)]
struct Picked {
    cancel: Option<u64>,
    stop: Option<(u64, String, String)>,
    fold: Option<u64>,
    control: Option<(Controls, Control)>,
}

/// A click on a job's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    /// Pause, or resume when paused.
    Toggle,
    /// The gear: its Settings page.
    Settings,
}

/// Run what a click on a job's controls asked for: its pause or resume command, or Settings at its page.
fn control(app: &mut LccApp, c: &Controls, what: Control) {
    let r = match what {
        Control::Toggle => app.run(if c.paused { c.resume } else { c.pause }, json!({})),
        Control::Settings => match c.settings_section {
            Some(section) => app.run("app.settings", json!({"section": section})),
            None => app.run("app.settings", json!({"tab": c.settings_tab})),
        },
    };
    if let Err(e) = r {
        app.ui.status = e;
    }
}

/// The pause (or resume) button and the gear of a task with `controls`, at the right of its name; which was clicked.
fn control_icons(ui: &mut egui::Ui, t: &Tokens, task: &TaskInfo) -> Option<Control> {
    let c = task.controls.as_ref()?;
    let mut clicked = None;
    let (icon, name, id) = if c.paused { (Icon::Play, "Resume", "resume") } else { (Icon::Pause, "Pause", "pause") };
    let (rect, r) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
    crate::widgets::announce(&r, tr(name));
    paint(ui.painter(), rect.shrink(2.0), icon, if r.hovered() { t.text } else { t.icon });
    register(ui.ctx(), format!("activity:{id}:{}", task.id), rect);
    if r.on_hover_text(tr(name)).clicked() {
        clicked = Some(Control::Toggle);
    }
    let (rect, r) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
    crate::widgets::announce(&r, tr("Settings"));
    paint(ui.painter(), rect.shrink(2.0), Icon::Gear, if r.hovered() { t.text } else { t.icon });
    register(ui.ctx(), format!("activity:settings:{}", task.id), rect);
    if r.on_hover_text(tr("Settings")).clicked() {
        clicked = Some(Control::Settings);
    }
    clicked
}

/// The room the icons at the right of a task's name take: ✕ (`cross`) and the pause button and gear.
fn icons_width(task: &TaskInfo, cross: bool) -> f32 {
    let n = usize::from(cross) + if task.controls.is_some() { 2 } else { 0 };
    n as f32 * (CROSS + 6.0)
}

fn apply(app: &mut LccApp, picked: Picked) {
    if let Some(id) = picked.cancel {
        cancel(app, id);
    }
    if let Some((c, what)) = picked.control {
        control(app, &c, what);
    }
    // ask first; another dialog open (it takes the clicks anyway) is left alone
    if let Some((id, task, warning)) = picked.stop
        && app.ui.dialog.is_none()
    {
        app.ui.dialog = Some(Dialog::StopWaiting { id, task, warning });
    }
    if let Some(id) = picked.fold
        && !app.activity_open.remove(&id)
    {
        app.activity_open.insert(id);
    }
}

/// The rows (from [`ordered`]) in a frame of [`WIDTH`], scrolling past `max_height`.
fn rows(ui: &mut egui::Ui, t: &Tokens, tasks: &[(&TaskInfo, u8, usize)], open: &std::collections::HashSet<u64>, max_height: f32) -> Picked {
    let mut picked = Picked::default();
    let inner = WIDTH - 2.0 * (f32::from(MARGIN) + 1.0);
    ui.set_width(inner);
    ui.spacing_mut().item_spacing.y = 3.0;
    egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
        for (i, (task, depth, kids)) in tasks.iter().enumerate() {
            if i > 0 {
                ui.add_space(6.0);
            }
            let indent = if *depth > 0 { INDENT } else { 0.0 };
            let clicked = ui
                .horizontal(|ui| {
                    ui.add_space(indent);
                    ui.vertical(|ui| row(ui, t, task, inner - indent, *kids, open.contains(&task.id))).inner
                })
                .inner;
            if clicked.cross {
                picked.cancel = Some(task.id);
            }
            if clicked.stop
                && let Some(warning) = stop_warning_text(task)
            {
                picked.stop = Some((task.id, label_text(task), warning));
            }
            if clicked.fold {
                picked.fold = Some(task.id);
            }
            if let (Some(what), Some(c)) = (clicked.control, &task.controls) {
                picked.control = Some((c.clone(), what));
            }
        }
    });
    picked
}

/// Classic: the list of every task, under the module bar's activity area (`plate`, `None` when it isn't drawn: the
/// list closes then). Esc or a click outside it and the area closes it.
fn list(app: &mut LccApp, ctx: &egui::Context, tasks: &[(&TaskInfo, u8, usize)], plate: Option<Rect>) {
    let Some(plate) = plate else {
        app.activity_list = false;
        return;
    };
    if !app.activity_list {
        return;
    }
    let id = egui::Id::new(LIST);
    let last = ctx.memory(|m| m.area_rect(id));
    let outside =
        ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !plate.contains(p) && last.is_none_or(|r| !r.contains(p))));
    if outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.activity_list = false;
        return;
    }
    let t = Tokens::get(ctx);
    let origin = pos2(plate.left(), plate.bottom() + 8.0);
    let max_height = (ctx.content_rect().bottom() - origin.y - 60.0).max(80.0);
    let open = app.activity_open.clone();
    let mut picked = Picked::default();
    // Compact's sharper corners, as its other cards (Classic's otherwise)
    let frame =
        egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.field_border)).corner_radius(t.radius(6.0)).inner_margin(Margin::same(MARGIN));
    let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(origin).show(ctx, |ui| {
        // as the Modern card: lay out in all the room below, not last frame's size, so the list grows with its rows
        ui.set_max_height((ctx.content_rect().bottom() - origin.y - 16.0).max(80.0));
        frame.show(ui, |ui| picked = rows(ui, &t, tasks, &open, max_height));
    });
    register(ctx, LIST, area.response.rect);
    app.activity_bottom = Some(area.response.rect.bottom());
    apply(app, picked);
}

/// The tasks the identity plate shows, oldest first: those due on the stack, a batch's per-photo rows left out (one
/// bar per task the user started). None in the Modern layout, which has its card.
fn plate_tasks(app: &LccApp) -> Vec<TaskInfo> {
    if crate::modern::on(app) {
        return Vec::new();
    }
    due_tasks(app).into_iter().map(|(t, ..)| t).collect()
}

/// The tasks due on the stack, oldest first, as [`ordered`] pairs them (a batch's per-photo rows
/// folded away): the ones any layout's activity surface owes the user.
fn due_tasks(app: &LccApp) -> Vec<(TaskInfo, u8, usize)> {
    let all = app.session.activity.list();
    let due: Vec<&TaskInfo> = all.iter().filter(|t| t.age_ms >= SHOW_AFTER_MS).collect();
    ordered(&due, &std::collections::HashSet::new()).into_iter().map(|(t, a, b)| (t.clone(), a, b)).collect()
}

/// Full-screen preview: the tasks running, top right in a quiet card — the module bar (Classic)
/// and the Modern card are both off the screen there, but the work goes on and so does its say
/// (Library ▸ Previews builds while a photo is judged full screen). One task: its name, count and
/// bar; several: "N operations in progress…" over a thin bar each, as the identity plate draws
/// them. No clicks: it takes no part of the photo away.
pub fn fullscreen(app: &mut LccApp, ctx: &egui::Context) {
    let tasks = due_tasks(app);
    let [first, rest @ ..] = tasks.as_slice() else { return };
    // bars move: look again soon (as the stack does)
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let t = Tokens::get(ctx);
    let rect = ctx.content_rect();
    let width = WIDTH;
    let origin = pos2(rect.right() - width - MARGIN as f32, rect.top() + f32::from(MARGIN));
    let frame =
        egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.field_border)).corner_radius(t.radius(6.0)).inner_margin(Margin::same(MARGIN));
    let mut bars: Vec<egui::Rect> = Vec::new();
    let area = egui::Area::new(egui::Id::new("activity-fullscreen")).order(egui::Order::Middle).fixed_pos(origin).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(width - 2.0 * (f32::from(MARGIN) + 1.0));
            ui.spacing_mut().item_spacing.y = 3.0;
            let caption = if rest.is_empty() { label_text(&first.0) } else { tr_format!("{n} operations in progress…", n = tasks.len()) };
            ui.add(egui::Label::new(RichText::new(caption).font(t.font(11.5)).color(t.text)).truncate().selectable(false));
            let shown = tasks.len().min(MAX_ROWS);
            for task in tasks.iter().take(shown) {
                let fraction = crate::progress::fraction_of(task.0.done, task.0.total);
                let r = crate::progress::Progress::new(fraction).width(ui.available_width()).height(THIN).dimmed(task.0.cancelling).show(ui);
                bars.push(r.rect);
            }
            if tasks.len() > shown {
                ui.add(
                    egui::Label::new(RichText::new(tr_format!("+{n} more", n = tasks.len() - shown)).font(t.font(11.0)).color(t.text_dim))
                        .truncate()
                        .selectable(false),
                );
            }
        })
    });
    register(ctx, "activity:fullscreen", area.response.rect);
    for (task, r) in tasks.iter().take(bars.len()).zip(&bars) {
        register(ctx, format!("activity:fullscreen:bar:{}", task.0.id), *r);
    }
}

/// Classic: tasks are running that take the identity plate's place in the module bar ([`plate`]).
pub fn in_plate(app: &LccApp) -> bool {
    !plate_tasks(app).is_empty()
}

/// Classic: the tasks in the module bar, in `area` (where the identity plate is), in its place. One task: its name,
/// ✕ when it can be cancelled, and its bar. Several: "N operations in progress…" over a thin bar each, oldest first,
/// as many as fit. A click on the area opens or closes the list (`view.activityList`).
pub fn plate(app: &mut LccApp, ui: &mut egui::Ui, area: Rect) {
    let top = plate_tasks(app);
    if top.is_empty() {
        return;
    }
    app.activity_plate_drawn = Some(area);
    let t = Tokens::get(ui.ctx());
    // the area first, so ✕ (registered after it) sits on top and takes its own clicks
    let resp = ui.interact(area, egui::Id::new(PLATE), Sense::click());
    crate::access::button(&resp, "Activity");
    register(ui.ctx(), PLATE, area);
    let resp = resp.on_hover_text(tr("Show running tasks"));
    let list_open = app.activity_list;
    let mut cancel = None;
    let mut controlled = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(area).layout(Layout::top_down(Align::Min)), |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        let width = area.width();
        if let [task] = top.as_slice() {
            ui.add_space(((area.height() - 18.0 - 3.0 - 6.0) / 2.0).max(0.0));
            ui.horizontal(|ui| {
                // the list, while open, has the task's ✕ (and its widget id)
                // a task whose stop throws work away is stopped from the list, after a question
                let cross = task.cancellable && !task.cancelling && !list_open && task.stop_warning.is_none();
                // the list, while open, has the pause button and the gear too
                let icons = if list_open { 0.0 } else { icons_width(task, cross) };
                let label_w = width - icons;
                ui.allocate_ui_with_layout(vec2(label_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_width(label_w);
                    ui.add(egui::Label::new(RichText::new(label_text(task)).font(t.font(12.0)).color(t.text)).truncate().selectable(false));
                });
                if !list_open
                    && let Some(what) = control_icons(ui, &t, task)
                    && let Some(c) = &task.controls
                {
                    controlled = Some((c.clone(), what));
                }
                if cross {
                    let (rect, r) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
                    crate::widgets::announce(&r, crate::i18n::tr("Cancel"));
                    paint(ui.painter(), rect.shrink(2.0), Icon::Close, if r.hovered() { t.text } else { t.icon });
                    register(ui.ctx(), format!("activity:cancel:{}", task.id), rect);
                    if r.on_hover_text(tr("Cancel")).clicked() {
                        cancel = Some(task.id);
                    }
                }
            });
            bar(ui, task, width);
        } else {
            let caption = tr_format!("{n} operations in progress…", n = top.len());
            // as many thin bars as fit under the caption; the caption counts them all
            let fit = (((area.height() - 14.0 - 3.0 + THIN_GAP) / (THIN + THIN_GAP)).floor().max(1.0)) as usize;
            let n = top.len().min(fit);
            let used = 14.0 + 3.0 + n as f32 * (THIN + THIN_GAP) - THIN_GAP;
            ui.add_space(((area.height() - used) / 2.0).max(0.0));
            ui.add(egui::Label::new(RichText::new(caption).font(t.font(11.5)).color(t.text)).truncate().selectable(false));
            ui.spacing_mut().item_spacing.y = THIN_GAP;
            for task in top.iter().take(n) {
                let fraction = crate::progress::fraction_of(task.done, task.total);
                let r = crate::progress::Progress::new(fraction).width(width).height(THIN).dimmed(task.cancelling).show(ui);
                crate::widgets::screen_reader_name(ui.ctx(), r.id, &label_text(task));
                register(ui.ctx(), format!("activity:bar:{}", task.id), r.rect);
            }
        }
    });
    if let Some(id) = cancel {
        self::cancel(app, id);
    } else if let Some((c, what)) = controlled {
        control(app, &c, what);
    } else if resp.clicked()
        && let Err(e) = app.run("view.activityList", serde_json::json!({}))
    {
        app.ui.status = e;
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
fn announce_finished(app: &mut LccApp, ctx: &egui::Context) {
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
/// so the command goes to the engine directly (`LccApp::run` would put its error in the status bar).
pub(crate) fn cancel(app: &mut LccApp, id: u64) {
    let _ = app.session.execute("activity.cancel", &json!({"id": id}));
}

/// What was clicked in a row.
struct RowClicks {
    cross: bool,
    stop: bool,
    fold: bool,
    control: Option<Control>,
}

/// One task: name and ✕, the bar, the detail line, Stop Waiting… for a task whose stop throws work away (instead of
/// ✕), and for a batch with per-photo rows (`kids`) the line that opens or folds them.
fn row(ui: &mut egui::Ui, t: &Tokens, task: &TaskInfo, width: f32, kids: usize, open: bool) -> RowClicks {
    let mut clicked = false;
    let mut stop = false;
    let mut fold = false;
    let mut control = None;
    let stoppable = task.cancellable && !task.cancelling;
    let r = ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            let cross = stoppable && task.stop_warning.is_none();
            let label_w = width - icons_width(task, cross);
            ui.allocate_ui_with_layout(vec2(label_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                // the whole width, so the cross lands at the right edge however short the name
                ui.set_width(label_w);
                ui.add(egui::Label::new(RichText::new(label_text(task)).font(t.font(12.5)).color(t.text)).truncate());
            });
            control = control_icons(ui, t, task);
            if cross {
                let (rect, resp) = ui.allocate_exact_size(vec2(CROSS, CROSS), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr("Cancel"));
                paint(ui.painter(), rect.shrink(2.0), Icon::Close, if resp.hovered() { t.text } else { t.icon });
                register(ui.ctx(), format!("activity:cancel:{}", task.id), rect);
                clicked = resp.on_hover_text(tr("Cancel")).clicked();
            }
        });
        bar(ui, task, width);
        let line = detail_line(task);
        if !line.is_empty() {
            ui.add(egui::Label::new(RichText::new(line).font(t.font(11.0)).color(t.text_dim)).truncate());
        }
        if stoppable && task.stop_warning.is_some() {
            let r = ui.add(egui::Label::new(RichText::new(tr("Stop Waiting…")).font(t.font(11.0)).color(t.accent)).sense(Sense::click()));
            crate::widgets::announce(&r, tr("Stop Waiting…"));
            register(ui.ctx(), format!("activity:stop:{}", task.id), r.rect);
            stop = r.clicked();
        }
        if kids > 0 {
            let text = if open { tr("Hide each photo").to_string() } else { tr_format!("Show each photo ({n})", n = kids) };
            let r = ui.add(egui::Label::new(RichText::new(text).font(t.font(11.0)).color(t.accent)).sense(Sense::click()));
            register(ui.ctx(), format!("activity:children:{}", task.id), r.rect);
            fold = r.clicked();
        }
    });
    register(ui.ctx(), format!("activity:row:{}", task.id), r.response.rect);
    RowClicks { cross: clicked, stop, fold, control }
}

/// The progress bar, named after its task for screen readers; while the amount of work isn't
/// known, a segment sweeps across.
pub(crate) fn bar(ui: &mut egui::Ui, task: &TaskInfo, width: f32) -> egui::Response {
    let fraction = (task.total > 0).then(|| (task.done as f64 / task.total as f64) as f32);
    let response = crate::progress::Progress::new(fraction).width(width).dimmed(task.cancelling).show(ui);
    crate::widgets::screen_reader_name(ui.ctx(), response.id, &label_text(task));
    response
}
