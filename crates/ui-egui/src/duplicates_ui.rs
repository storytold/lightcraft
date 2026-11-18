//! The Duplicate Finder's review window (`library.findDuplicates`, Library ▸ Find Duplicates…):
//! groups of photos that are copies of one picture, each with the best copy to keep first and the
//! others marked for removal. The same-bytes groups are there at once (the catalog's content
//! hashes); Find Similar Copies reads every photo's picture in the background and adds the groups
//! that differ — resized, re-compressed, renamed — plus raw + JPEG pairs of the same shot, which
//! are there to stack, not delete. What is marked goes to Recently Deleted (`photo.delete`), is
//! rejected, or is stacked, all through the existing commands. Fork-owned.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, vec2};
use lcc_catalog::PhotoId;
use lcc_engine::Session;
use lcc_engine::activity::TaskHandle;
use lcc_engine::duplicates::{GroupKind, ScanReport, candidates, report_to_json, scan};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::menus::UiCommand;
use crate::render::{Renderer, Slot};
use crate::theme::Tokens;
use crate::widgets::register;

pub const OPEN: &str = "library.findDuplicates";

/// Width of a member's card: its thumbnail and the facts under it.
const CARD_W: f32 = 184.0;

/// Show one group's photos large and side by side: the group becomes the selection and the view
/// is Survey (`{group?}`: 0-based index into the window's groups, default the first). The window
/// steps aside with its marks kept; Find Duplicates… brings it back.
pub const COMPARE: &str = "library.compareDuplicateGroup";

pub const COMMANDS: &[UiCommand] = &[(OPEN, "Find Duplicates…", None, "Library"), (COMPARE, "Compare Group in Survey", None, "")];

/// The review window's state, kept while it can be reopened.
pub struct Finder {
    pub open: bool,
    pub groups: Vec<Group>,
    /// Whether the groups include the similar scan's (Find Similar Copies ran for them).
    pub similar_ran: bool,
    /// (done, total) while the similar scan runs.
    pub progress: Option<Arc<Mutex<(usize, usize)>>>,
    /// The running scan's cancel, for the window's Stop button (the activity row's ✕ shares it).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Pictures the similar scan could not read.
    pub failed: usize,
    /// The window stepped aside to show a group in Survey; opening Find Duplicates… again brings
    /// it back with its marks instead of scanning anew.
    pub parked: bool,
}

/// One group: the best copy to keep first, each member with whether it is marked for removal.
pub struct Group {
    pub kind: GroupKind,
    pub members: Vec<Member>,
}

pub struct Member {
    pub id: PhotoId,
    /// Same file bytes as the group's best copy.
    pub identical: bool,
    pub marked: bool,
}

impl Finder {
    /// The window's state from a finished scan: every member marked except each group's best copy
    /// (a raw + JPEG pair is there to stack, so nothing in it starts marked).
    fn from_report(report: &ScanReport, similar_ran: bool) -> Finder {
        let groups = report
            .groups
            .iter()
            .map(|g| Group {
                kind: g.kind,
                members: g
                    .members
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, identical))| Member { id, identical, marked: g.kind != GroupKind::SameShot && i > 0 })
                    .collect(),
            })
            .collect();
        Finder { open: true, groups, similar_ran, progress: None, cancel: None, failed: report.failed, parked: false }
    }

    /// The ids marked for removal.
    fn marked(&self) -> Vec<PhotoId> {
        self.groups.iter().flat_map(|g| g.members.iter().filter(|m| m.marked).map(|m| m.id)).collect()
    }

    /// Drop `gone` from the groups; a group left with fewer than two photos is no longer a group.
    /// The survivors keep their marks.
    fn drop_ids(&mut self, gone: &[PhotoId]) {
        for g in &mut self.groups {
            g.members.retain(|m| !gone.contains(&m.id));
        }
        self.groups.retain(|g| g.members.len() > 1);
    }
}

/// Open the window with the same-bytes groups, which the catalog answers at once. Returns the
/// scan's JSON (what the `library.findDuplicates` command returns) for the control channel. A
/// window that stepped aside for Survey comes back as it was (marks and similar groups kept),
/// minus the photos that are gone or in Recently Deleted by now.
fn open(app: &mut LccApp) -> Result<Value, String> {
    let report = scan(&candidates(&app.session.catalog), None, None, false, &AtomicBool::new(false), &|_, _| {});
    let out = report_to_json(&app.session.catalog, &report);
    if let Some(f) = app.duplicates.as_mut().filter(|f| f.parked) {
        let cat = &app.session.catalog;
        let gone: Vec<PhotoId> =
            f.groups.iter().flat_map(|g| g.members.iter().map(|m| m.id)).filter(|id| cat.photo(*id).is_none_or(|p| p.deleted)).collect();
        f.drop_ids(&gone);
        f.parked = false;
        f.open = true;
        return Ok(out);
    }
    app.duplicates = Some(Finder::from_report(&report, false));
    Ok(out)
}

/// `library.compareDuplicateGroup`: the group's photos become the selection (the group's best copy
/// active) and Survey shows them large, side by side. Reuses the library's Survey rather than a
/// viewer of its own; the window is parked until Find Duplicates… is chosen again.
fn compare_group(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let group = match p.get("group") {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or("`group` must be a whole number (0 is the first group)")?,
    };
    let finder = app.duplicates.as_ref().filter(|f| f.open).ok_or("open Find Duplicates… first")?;
    let g = finder.groups.get(group).ok_or_else(|| format!("no duplicate group {group}"))?;
    let ids: Vec<u64> = g.members.iter().map(|m| m.id.0).collect();
    let first = *ids.first().ok_or("the group is empty")?;
    app.run("library.select", json!({"ids": ids, "active": first}))?;
    app.run("view.survey", Value::Null)?;
    if let Some(f) = app.duplicates.as_mut() {
        f.open = false;
        f.parked = true;
    }
    app.renderer.forget_duplicates();
    Ok(json!({"group": group, "photos": ids}))
}

/// Find Similar Copies: read every photo's picture on a worker and swap the groups in when it is
/// done. The scan never touches the session; the snapshot, the raw-embedded-JPEG loader and the
/// preview cache travel with it.
fn start_similar(app: &mut LccApp, ctx: &egui::Context) {
    if app.duplicates.as_ref().is_some_and(|f| f.progress.is_some()) {
        return;
    }
    let snapshot = candidates(&app.session.catalog);
    let loader = app.session.media.preview_loader.clone();
    let cache = app.session.media.rendered.clone();
    let progress = Arc::new(Mutex::new((0usize, 0usize)));
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(f) = app.duplicates.as_mut() {
        f.progress = Some(Arc::clone(&progress));
        f.cancel = Some(Arc::clone(&cancel));
    }
    let work_cancel = Arc::clone(&cancel);
    let work = move |handle: TaskHandle| {
        scan(&snapshot, loader.as_ref(), Some(cache.as_ref()), true, &work_cancel, &|done, total| {
            handle.progress(done as u64, total as u64);
            if let Ok(mut p) = progress.lock() {
                *p = (done, total);
            }
        })
    };
    let label = tr("Find Similar Copies").to_string();
    let done = move |app: &mut LccApp, ctx: &egui::Context, report: ScanReport| {
        let cancelled = report.cancelled;
        if let Some(f) = app.duplicates.as_mut() {
            *f = Finder::from_report(&report, true);
        }
        if cancelled {
            app.toast(ctx, tr("Search stopped; the groups are the ones already found"));
        } else {
            app.toast(ctx, summary_text(&report));
        }
    };
    if let Err(e) = crate::tasks::spawn_tracked(app, &label, "findDuplicates", cancel, work, done) {
        if let Some(f) = app.duplicates.as_mut() {
            f.progress = None;
            f.cancel = None;
        }
        app.toast_error(ctx, e);
    }
}

fn summary_text(report: &ScanReport) -> String {
    let count = |k: GroupKind| report.groups.iter().filter(|g| g.kind == k).count();
    let parts = [
        (count(GroupKind::Exact), "copies of the same file"),
        (count(GroupKind::Similar), "sets of copies that differ"),
        (count(GroupKind::SameShot), "raw + JPEG pairs"),
    ];
    let shown: Vec<String> = parts.iter().filter(|(n, _)| *n > 0).map(|(n, what)| tr_format!("{what}: {n}", what = tr(what), n = n)).collect();
    if shown.is_empty() { tr("No duplicates found.").to_string() } else { shown.join(" · ") }
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    match id {
        OPEN => Some(app.session.catalog.photos().next().is_some()),
        COMPARE => Some(app.duplicates.as_ref().is_some_and(|f| f.open && !f.groups.is_empty())),
        _ => None,
    }
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        OPEN => Some(open(app)),
        COMPARE => Some(compare_group(app, p)),
        _ => None,
    }
}

/// What a footer button asked for, applied after the window drew.
enum Action {
    Remove(Vec<PhotoId>),
    Reject(Vec<PhotoId>),
    Stack(Vec<Vec<PhotoId>>),
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let LccApp { duplicates, session, renderer, .. } = app;
    let Some(finder) = duplicates.as_mut().filter(|f| f.open) else { return };
    let t = Tokens::get(ctx);
    let id = egui::Id::new("find-duplicates");
    let mut action: Option<Action> = None;
    let mut close = false;
    let mut closed = false;
    let mut similar = false;
    let mut forget = false;
    let mut compare: Option<usize> = None;
    let (done_count, total) = finder.progress.as_ref().and_then(|p| p.lock().ok().map(|p| *p)).unwrap_or((0, 0));
    let scanning = finder.progress.is_some();
    let marked = finder.marked();
    let stackable: Vec<Vec<PhotoId>> = finder.groups.iter().map(|g| g.members.iter().map(|m| m.id).collect()).collect();
    let shown = egui::Window::new(tr("Find Duplicates"))
        .id(id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .default_width(820.0)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_height(crate::dialog_frame::max_window_height(ctx))
        .show(ctx, |ui| {
            closed = crate::dialog_frame::card_header(ui, tr("Find Duplicates"), Some("find-duplicates:close"));
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, id);
            ui.add(egui::Label::new(RichText::new(summary_line(finder)).color(t.text_label)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                #[cfg(not(target_arch = "wasm32"))]
                if !finder.similar_ran && !scanning {
                    let r = ui.add(egui::Button::new(tr("Find Similar Copies…")).min_size(vec2(0.0, 24.0)));
                    register(ui.ctx(), "button:duplicates.similar", r.rect);
                    if r.clicked() {
                        similar = true;
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(tr("Reads every photo's picture; a large library takes as long as reading it")).color(t.text_dim).small(),
                        )
                        .wrap(),
                    );
                }
                if scanning {
                    ui.add(egui::Label::new(
                        RichText::new(tr_format!("Reading pictures {done} of {total}", done = done_count, total = total)).color(t.text_label),
                    ));
                    if let Some(cancel) = finder.cancel.clone() {
                        let r = ui.add(egui::Button::new(tr("Stop")).min_size(vec2(0.0, 24.0)));
                        register(ui.ctx(), "button:duplicates.stop", r.rect);
                        if r.clicked() {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
            if finder.failed > 0 {
                ui.add(
                    egui::Label::new(
                        RichText::new(tr_format!(
                            "Could not read {failed} picture{}; those photos are not in the groups",
                            if finder.failed == 1 { "" } else { "s" },
                            failed = finder.failed
                        ))
                        .small()
                        .color(t.text_dim),
                    )
                    .wrap(),
                );
            }
            ui.add_space(6.0);
            layout.body(ui, |ui| {
                let mut member_index = 0u32;
                for (gi, g) in finder.groups.iter_mut().enumerate() {
                    let header = format!("{} · {}", g.members.len(), tr(kind_label(g.kind)));
                    egui::CollapsingHeader::new(RichText::new(header).strong()).id_salt(("duplicates-group", gi)).default_open(gi == 0).show(
                        ui,
                        |ui| {
                            let r = ui
                                .add(egui::Button::new(tr("Compare Group in Survey")).min_size(vec2(0.0, 24.0)))
                                .on_hover_text(tr("Show this group large and side by side in Survey"));
                            register(ui.ctx(), format!("button:duplicates.compare:{gi}"), r.rect);
                            if r.clicked() {
                                compare = Some(gi);
                            }
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                                for m in g.members.iter_mut() {
                                    member_cell(session, renderer, ui, &t, member_index, m, g.kind);
                                    member_index += 1;
                                }
                            });
                        },
                    );
                }
            });
            layout.footer(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let n = marked.len();
                    let r = ui.add_enabled(
                        n > 0,
                        egui::Button::new(tr_format!("Remove {n} Photo{}", if n == 1 { "" } else { "s" }, n = n)).min_size(vec2(0.0, 26.0)),
                    );
                    register(ui.ctx(), "button:duplicates.remove", r.rect);
                    if r.clicked() {
                        action = Some(Action::Remove(marked.clone()));
                    }
                    let r = ui.add_enabled(n > 0, egui::Button::new(tr_format!("Reject {n}", n = n)).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), "button:duplicates.reject", r.rect);
                    if r.clicked() {
                        action = Some(Action::Reject(marked.clone()));
                    }
                    let r = ui.add(egui::Button::new(tr("Stack Groups")).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), "button:duplicates.stack", r.rect);
                    if r.clicked() {
                        action = Some(Action::Stack(stackable.clone()));
                    }
                    let r = ui.add(egui::Button::new(tr("Close")).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), "button:duplicates.close", r.rect);
                    if r.clicked() {
                        close = true;
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, id, shown.as_ref().map(|w| w.response.rect));
    if closed || close {
        finder.open = false;
        forget = true;
    }
    let _ = finder;
    if forget {
        renderer.forget_duplicates();
    }
    if similar {
        start_similar(app, ctx);
    }
    if let Some(group) = compare {
        match compare_group(app, &json!({"group": group})) {
            Ok(_) => app.toast(ctx, tr("Find Duplicates… brings the window back with its marks")),
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if let Some(action) = action {
        apply(app, ctx, action);
    }
}

fn summary_line(finder: &Finder) -> String {
    if finder.groups.is_empty() && !finder.similar_ran {
        return tr("No photos share their file with another photo in this library.").to_string();
    }
    if finder.groups.is_empty() {
        return tr("No duplicates found.").to_string();
    }
    let n: usize = finder.groups.iter().map(|g| g.members.len().saturating_sub(1)).sum();
    tr_format!(
        "{groups} group{}: {n} photo{} are copies of another photo. The best copy of each group is first and unmarked; click a photo to change what is marked.",
        if finder.groups.len() == 1 { "" } else { "s" },
        if n == 1 { "" } else { "s" },
        groups = finder.groups.len(),
        n = n
    )
}

fn kind_label(kind: GroupKind) -> &'static str {
    match kind {
        GroupKind::Exact => "copies of the same file",
        GroupKind::Similar => "copies that differ",
        GroupKind::SameShot => "raw + JPEG of the same shot",
    }
}

/// What the window says about one photo, whole: nothing is cut to fit (the labels wrap).
struct Facts {
    name: String,
    /// The folder the file is in (empty for a photo that is not a file, or a bare name).
    folder: String,
    /// The capture time as the catalog stores it, `YYYY-MM-DD HH:MM:SS` (empty when unknown).
    captured: String,
    width: u32,
    height: u32,
    size: u64,
    format: String,
}

impl Facts {
    fn of(p: &lcc_catalog::Photo) -> Facts {
        let path = match &p.source {
            lcc_catalog::Source::File { path } => path.as_str(),
            _ => "",
        };
        // `/` and `\` are one byte, so the split is at a char boundary
        let folder = path.rfind(['/', '\\']).and_then(|i| path.get(..i)).unwrap_or("");
        let folder = if folder.is_empty() && path.starts_with(['/', '\\']) { path.get(..1).unwrap_or("") } else { folder };
        let captured = p.captured.as_deref().map(|c| c.chars().take(19).map(|ch| if ch == 'T' { ' ' } else { ch }).collect()).unwrap_or_default();
        Facts {
            name: p.file_name.clone(),
            folder: folder.to_string(),
            captured,
            width: p.width,
            height: p.height,
            size: p.file_size,
            format: p.format.clone(),
        }
    }
}

/// A wrapping label that may break inside a long word (a path has no spaces to break at).
fn wrapped(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: Color32, width: f32) {
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::simple(font, color));
    job.wrap.max_width = width;
    job.wrap.break_anywhere = true;
    ui.add(egui::Label::new(job));
}

/// One photo of a group: its thumbnail (click to change the mark), a badge for the copy to keep
/// (or for being the same file as it), and under it the whole story: file name, folder, capture
/// date, size in pixels and bytes, so the copies can be told apart and the right one kept. The
/// texts wrap rather than cut; the full path is also the tooltip.
fn member_cell(session: &mut Session, renderer: &mut Renderer, ui: &mut egui::Ui, t: &Tokens, index: u32, m: &mut Member, kind: GroupKind) {
    let facts = session.catalog.photo(m.id).map(|p| Facts::of(p)).unwrap_or_else(|| Facts {
        name: String::new(),
        folder: String::new(),
        captured: String::new(),
        width: 0,
        height: 0,
        size: 0,
        format: String::new(),
    });
    let side = CARD_W;
    ui.vertical(|ui| {
        ui.set_width(side);
        let (rect, mut resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
        register(ui.ctx(), format!("duplicates:{index}"), rect);
        crate::widgets::announce(&resp, facts.name.clone());
        {
            let p = ui.painter();
            p.rect_filled(rect, 3.0, t.skeleton);
            let slot = Slot::Duplicates(index);
            if !renderer.textures.contains_key(&slot)
                && let Some(job) = session.thumb_job(m.id, 384)
                && let Some(quick) = session.quick_thumb_job(&job)
            {
                renderer.request_quick(slot, quick, 4);
            }
            if let Some(tex) = renderer.textures.get(&slot) {
                let [tw, th] = tex.size;
                let s = ((rect.width() - 8.0) / tw.max(1) as f32).min((rect.height() - 8.0) / th.max(1) as f32);
                let fit = Rect::from_center_size(rect.center(), vec2(tw as f32 * s, th as f32 * s));
                p.image(
                    tex.id(),
                    fit,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    if m.marked { Color32::from_gray(110) } else { Color32::WHITE },
                );
            }
            // the mark's checkbox, top left
            let cb = Rect::from_min_size(rect.left_top() + vec2(6.0, 6.0), vec2(16.0, 16.0));
            if m.marked || kind != GroupKind::SameShot {
                p.rect(
                    cb,
                    3.0,
                    if m.marked { t.accent } else { Color32::from_black_alpha(150) },
                    Stroke::new(1.0, Color32::from_gray(200)),
                    StrokeKind::Inside,
                );
                if m.marked {
                    p.line_segment([cb.left_center() + vec2(3.5, 0.5), cb.center_bottom() + vec2(-1.0, -4.0)], Stroke::new(2.0, Color32::WHITE));
                    p.line_segment([cb.center_bottom() + vec2(-1.0, -4.0), cb.right_top() + vec2(-3.5, 4.0)], Stroke::new(2.0, Color32::WHITE));
                }
            }
            // badge: the copy to keep, or the same file as it
            let badge = if !m.marked && kind != GroupKind::SameShot {
                Some((tr("Keep").to_string(), t.accent))
            } else if m.identical {
                Some((tr("Same file").to_string(), Color32::from_rgba_unmultiplied(90, 90, 90, 220)))
            } else {
                None
            };
            if let Some((text, color)) = badge {
                let g = p.layout_no_wrap(text, t.semibold(10.0), Color32::WHITE);
                let br = Rect::from_min_size(rect.right_top() + vec2(-g.size().x - 10.0, 4.0), g.size() + vec2(8.0, 4.0));
                p.rect_filled(br, 3.0, color);
                p.galley(br.min + vec2(4.0, 2.0), g, Color32::WHITE);
            }
        }
        // the facts under the photo, whole
        let name_color = if m.marked { t.text_dim } else { t.text_label };
        wrapped(ui, &facts.name, t.semibold(11.5), name_color, side);
        if !facts.folder.is_empty() {
            wrapped(ui, &facts.folder, t.font(10.0), t.text_dim, side);
        }
        let when = if facts.captured.is_empty() { tr("No capture date").to_string() } else { facts.captured.clone() };
        wrapped(ui, &when, t.font(10.5), t.text_dim, side);
        let mb = (facts.size as f64 / 1e6 * 10.0).round() / 10.0;
        wrapped(
            ui,
            &tr_format!("{w} × {h} · {mb} MB · {format}", w = facts.width, h = facts.height, mb = mb, format = facts.format),
            t.font(10.5),
            t.text_dim,
            side,
        );
        if !facts.folder.is_empty() {
            resp = resp.on_hover_text(format!("{}\n{}", facts.folder, facts.name));
        }
        if resp.clicked() {
            m.marked = !m.marked;
        }
    });
}

/// Apply the footer's choice: what is marked leaves for Recently Deleted (one undo step), is
/// rejected, or each group becomes a stack. The groups then drop the photos that went.
fn apply(app: &mut LccApp, ctx: &egui::Context, action: Action) {
    let ids = |v: &[PhotoId]| json!({"ids": v.iter().map(|i| i.0).collect::<Vec<_>>()});
    match action {
        Action::Remove(list) => {
            let n = list.len();
            if app.run("photo.delete", ids(&list)).is_ok() {
                if let Some(f) = app.duplicates.as_mut() {
                    f.drop_ids(&list);
                }
                app.toast(ctx, tr_format!("Removed {n} photo{} to Recently Deleted", if n == 1 { "" } else { "s" }, n = n));
            }
        }
        Action::Reject(list) => {
            let n = list.len();
            if app.run("photo.reject", ids(&list)).is_ok() {
                if let Some(f) = app.duplicates.as_mut() {
                    for m in f.groups.iter_mut().flat_map(|g| g.members.iter_mut()) {
                        if list.contains(&m.id) {
                            m.marked = false;
                        }
                    }
                }
                app.toast(ctx, tr_format!("Rejected {n} photo{}", if n == 1 { "" } else { "s" }, n = n));
            }
        }
        Action::Stack(groups) => {
            let mut stacked = 0;
            for g in &groups {
                if g.len() > 1 && app.run("stack.group", ids(g)).is_ok() {
                    stacked += 1;
                }
            }
            app.toast(ctx, tr_format!("Stacked {n} group{}", if stacked == 1 { "" } else { "s" }, n = stacked));
        }
    }
}
