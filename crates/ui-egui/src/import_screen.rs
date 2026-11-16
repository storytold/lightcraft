//! Import (fork-owned), the one import review of every layout: a full-window sheet.
//!
//! - The header: ‹ Back (Back, Esc and Android's Back cancel), the title, and where the photos come from.
//! - Left, a rail of sources: Files & folders (the files scanned), Immich (each server connected).
//!   Icons only on a narrow window.
//! - The middle: All / New / Duplicates, Check All / Uncheck All, a toggle per file type, then the
//!   photos found.
//! - Right (under the photos on a narrow window): the options in cards. How (Copy / Move / Add in
//!   place; the copy-only rows show only for a copy), what to apply (preset, metadata, keywords,
//!   album), and one big "Import N photos".
//!
//! Every [`ImportDialog`] field keeps its control and its widget id from the dialog this replaces.
//!
//! Own design.

use egui::{Align2, Color32, Id, Rect, Sense, Stroke, Vec2, pos2, vec2};
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::import::{DEFAULT_FOLDER_TEMPLATE, ImportDialog};
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

const HEADER_H: f32 = 56.0;
const RAIL_WIDE: f32 = 200.0;
const RAIL_NARROW: f32 = 60.0;
const OPTIONS_W: f32 = 340.0;
/// The photos' narrowest beside the options column (else the options go under them).
const GRID_MIN: f32 = 280.0;
/// From this width the rail shows the sources' names.
const ROOMY: f32 = 640.0;
const CELL: f32 = 116.0;

/// Which photos the grid shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    New,
    Duplicates,
}

fn filter_id() -> Id {
    Id::new("import-screen-filter")
}

/// A candidate already in the library, a copy of another in this import, or in Recently Deleted.
fn is_dup(d: &ImportDialog, i: usize) -> bool {
    // (a browsed photo isn't offered while copying: it counts as already there)
    d.candidates.get(i).is_some_and(|c| c.duplicate.is_some()) || d.is_trashed(i) || (d.copy && d.is_browsed(i))
}

/// How many photos the button imports.
fn count(app: &LccApp, d: &ImportDialog) -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    if d.immich {
        return crate::panels::connections::selected_count(app);
    }
    let _ = app;
    d.selected_paths().len()
}

/// Registers the rect registered this frame as `from` under `to` too (a segment under the id
/// its button had in the dialog).
fn alias(ctx: &egui::Context, from: &str, to: &str) {
    let r = ctx
        .data(|d| d.get_temp::<crate::widgets::Registry>(Id::new("lc-registry")))
        .and_then(|r| r.0.iter().rev().find(|(w, _)| w == from).map(|(_, r)| *r));
    if let Some(r) = r {
        register(ctx, to, r);
    }
}

/// Draws Import over the whole window (every layout) while the review is open.
pub(crate) fn show(app: &mut LccApp, ctx: &egui::Context) {
    let Some(Dialog::Import { opts }) = app.ui.dialog.clone() else { return };
    let mut d = *opts;
    let screen = ctx.content_rect();
    let mut cancel = false;
    let mut go = false;
    if !egui::Popup::is_any_open(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        cancel = true;
    }
    let t = Tokens::get(ctx);
    // the dialog's own modal layer (`panels::dialogs::prepare` keeps popups opened on it, e.g. the
    // album picker, and blocks the panels under it)
    let shown = egui::Area::new(Id::new("app-import-dialog")).movable(false).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (bg, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
        ui.painter().rect_filled(bg, 0.0, t.chrome);
        // header
        let header = Rect::from_min_size(bg.min, vec2(bg.width(), HEADER_H));
        // macOS draws the traffic lights over the header's top-left corner: clear them like
        // Settings does (issue #1083, as #793); full screen hides them and gives the room back.
        let traffic = if app.integrated_titlebar && !app.window_is_fullscreen { 68.0 } else { 0.0 };
        let b = Rect::from_min_size(pos2(header.left() + 10.0 + traffic, header.center().y - 18.0), Vec2::splat(36.0));
        let resp = ui.interact(b, ui.id().with("import-back"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("Back")));
        register(ui.ctx(), "import:back", b);
        ui.painter().circle_filled(b.center(), 18.0, if resp.hovered() { t.pressed } else { t.button });
        paint(ui.painter(), Rect::from_center_size(b.center(), Vec2::splat(18.0)), Icon::ChevronLeft, t.text);
        if resp.clicked() {
            cancel = true;
        }
        ui.painter().text(pos2(b.right() + 12.0, header.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Import"), t.semibold(17.0), t.text);
        // where the photos come from: "Source" and the summary (both translated), right-aligned
        let from =
            if d.immich { crate::i18n::tr("Immich").to_string() } else { crate::import::source_summary_with_subfolders(&d.sources, d.subfolders) };
        if header.width() > 520.0 && !from.is_empty() {
            let g = ui.painter().layout(from, t.font(12.0), t.text_label, header.width() * 0.4);
            let x = header.right() - 16.0 - g.size().x;
            ui.painter().galley(pos2(x, header.center().y - g.size().y / 2.0), g, t.text_label);
            ui.painter().text(pos2(x - 8.0, header.center().y), Align2::RIGHT_CENTER, crate::i18n::tr("Source"), t.font(12.0), t.text_dim);
        }
        // (the dialog's source line's id)
        if !d.sources.is_empty() {
            register(ui.ctx(), "label:importSource", Rect::from_min_max(pos2(header.center().x, header.top()), header.max));
        }
        ui.painter().hline(bg.x_range(), header.bottom(), Stroke::new(1.0, t.divider));
        let body = Rect::from_min_max(pos2(bg.left(), header.bottom() + 1.0), bg.max);
        let roomy = body.width() >= ROOMY;
        let rail = Rect::from_min_size(body.min, vec2(if roomy { RAIL_WIDE } else { RAIL_NARROW }, body.height()));
        sources(app, ui, rail, &mut d, roomy);
        ui.painter().vline(rail.right(), body.y_range(), Stroke::new(1.0, t.divider));
        let rest = Rect::from_min_max(pos2(rail.right() + 1.0, body.top()), body.max);
        let empty = !d.immich && d.candidates.is_empty();
        if rest.width() >= OPTIONS_W + GRID_MIN && !empty {
            let opts = Rect::from_min_max(pos2(rest.right() - OPTIONS_W, rest.top()), rest.max);
            ui.painter().vline(opts.left(), rest.y_range(), Stroke::new(1.0, t.divider));
            let mid = Rect::from_min_max(rest.min, pos2(opts.left() - 1.0, rest.bottom()));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(mid.shrink(12.0)));
            photos(app, &mut c, &mut d, None);
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(opts.shrink2(vec2(12.0, 12.0))));
            go |= options_column(app, &mut c, &mut d, true);
        } else {
            // one column: the photos (a few rows), then the options, scrolling together
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rest.shrink(12.0)));
            egui::ScrollArea::vertical().id_salt("import-screen-narrow").auto_shrink([false, false]).show(&mut c, |ui| {
                photos(app, ui, &mut d, Some((rest.height() * 0.45).max(CELL + 40.0)));
                if !empty {
                    ui.add_space(8.0);
                    go |= options_column(app, ui, &mut d, false);
                }
            });
        }
    });
    ctx.memory_mut(|m| m.set_modal_layer(shown.response.layer_id));
    if cancel {
        app.ui.dialog = None;
        return;
    }
    // the rows may have closed the review (Choose Files…) or opened another dialog
    let still = matches!(app.ui.dialog, Some(Dialog::Import { .. }));
    if still {
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(d.clone()) });
    }
    if go && still {
        match crate::import::start(app, &d) {
            Ok(_) => app.ui.dialog = None,
            // (an unusable folder template, nothing selected: the review stays open)
            Err(e) => app.toast(ctx, e),
        }
    }
}

/// A rail entry: key (for its widget id), name, icon, the path picking it scans (none: Files &
/// folders and Immich, which switch the source instead).
struct Source {
    key: String,
    name: String,
    icon: Icon,
    path: Option<String>,
}

/// Tests: cards to list in place of the mounted ones, as (name, path).
#[cfg(test)]
pub(crate) const TEST_DEVICES: &str = "import-test-devices";

/// The cards and cameras mounted now (their names and paths come from the system: untrusted, only
/// shown and scanned). From the background scan; none until it has run once.
pub(crate) fn devices(ctx: &egui::Context) -> Vec<(String, String)> {
    #[cfg(test)]
    if let Some(v) = ctx.data(|d| d.get_temp::<Vec<(String, String)>>(egui::Id::new(TEST_DEVICES))) {
        return v;
    }
    let _ = ctx;
    #[cfg(not(target_arch = "wasm32"))]
    {
        lcc_engine::devices::devices_scanned().unwrap_or_default().into_iter().map(|d| (d.name, d.path)).collect()
    }
    #[cfg(target_arch = "wasm32")]
    Vec::new()
}

/// The source rail: Files & folders, Immich, each card or camera mounted, the watched folder.
/// Picking a card or the watched folder scans it into the review (absent when there is none).
fn sources(app: &mut LccApp, ui: &mut egui::Ui, rail: Rect, d: &mut ImportDialog, roomy: bool) {
    let t = Tokens::get(ui.ctx());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rail.shrink2(vec2(8.0, 10.0))));
    child.spacing_mut().item_spacing.y = 2.0;
    let mut items = vec![Source { key: "files".into(), name: crate::i18n::tr("Files & folders").to_string(), icon: Icon::Folder, path: None }];
    if cfg!(not(target_arch = "wasm32")) {
        items.push(Source { key: "immich".into(), name: crate::i18n::tr("Immich").to_string(), icon: Icon::Cloud, path: None });
    }
    for (i, (name, path)) in devices(ui.ctx()).into_iter().enumerate() {
        let name =
            if name.trim().is_empty() { crate::i18n::tr("Card").to_string() } else { format!("{} · {}", crate::i18n::tr("Card"), name.trim()) };
        items.push(Source { key: format!("card:{i}"), name, icon: Icon::Photos, path: Some(path) });
    }
    if let Some(folder) = app.session.import_defaults.auto_folder.clone().filter(|f| !f.trim().is_empty()) {
        items.push(Source { key: "watched".into(), name: crate::i18n::tr("Watched folder").to_string(), icon: Icon::Clock, path: Some(folder) });
    }
    // the entry the review shows: a card or the watched folder when its path is the one scanned
    let scanned = |p: &str| d.sources.len() == 1 && d.sources.first().is_some_and(|s| s == p);
    let lit = items.iter().find(|s| s.path.as_deref().is_some_and(scanned)).map(|s| s.key.clone());
    let mut pick: Option<Source> = None;
    for src in items {
        let on = match (&lit, src.key.as_str()) {
            (_, "immich") => d.immich,
            (Some(k), _) => !d.immich && *k == src.key,
            (None, "files") => !d.immich,
            _ => false,
        };
        let (r, resp) = child.allocate_exact_size(vec2(child.available_width(), 44.0), Sense::click());
        let name = src.name.clone();
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name.clone()));
        register(child.ctx(), format!("import:source:{}", src.key), r);
        if src.path.is_none() {
            // (the dialog's Files / Immich buttons' ids)
            register(child.ctx(), format!("button:importSource:{}", src.key), r);
        }
        let resp = if roomy { resp } else { resp.on_hover_text(name.clone()) };
        let p = child.painter();
        if on {
            p.rect_filled(r, 12.0, Color32::from_white_alpha(30));
            p.rect_stroke(r, 12.0, Stroke::new(1.0, Color32::from_white_alpha(24)), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect_filled(r, 12.0, Color32::from_white_alpha(14));
        }
        let c = if on { t.accent } else { t.text_label };
        let at = if roomy { pos2(r.left() + 22.0, r.center().y) } else { r.center() };
        paint(p, Rect::from_center_size(at, Vec2::splat(20.0)), src.icon, c);
        if roomy {
            let mut job = egui::text::LayoutJob::simple_singleline(name, t.font(14.0), if on { t.text } else { t.text_label });
            job.wrap = egui::text::TextWrapping {
                max_width: (r.width() - 52.0).max(10.0),
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let g = p.layout_job(job);
            p.galley(pos2(r.left() + 44.0, r.center().y - g.size().y / 2.0), g, t.text_label);
        }
        if resp.clicked() && !on {
            pick = Some(src);
        }
    }
    let Some(src) = pick else { return };
    match (src.key.as_str(), src.path) {
        ("immich", _) => {
            d.immich = true;
            #[cfg(not(target_arch = "wasm32"))]
            crate::panels::connections::open_source(app, None);
        }
        (_, Some(path)) => {
            // scan it; the review shows what it finds when the scan ends
            d.immich = false;
            if let Err(e) = crate::import::open(app, vec![path]) {
                app.toast(ui.ctx(), e);
            }
        }
        _ => d.immich = false,
    }
}

/// The toolbar and the photos found (or Immich's browser, or the way to choose files).
fn photos(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, max_h: Option<f32>) {
    let t = Tokens::get(ui.ctx());
    #[cfg(not(target_arch = "wasm32"))]
    if d.immich {
        crate::panels::connections::import_source(app, ui);
        return;
    }
    if d.candidates.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            let msg = if d.has_folder_source() {
                crate::i18n::tr("No supported photos found in this folder.")
            } else {
                crate::i18n::tr("No files chosen yet.")
            };
            ui.label(egui::RichText::new(msg).color(t.text_dim));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let fb = crate::widgets::text_button(ui, "importChooseFolder", crate::i18n::tr("Choose Folder…"), false);
                register(ui.ctx(), "import:chooseFolder", fb.rect);
                register(ui.ctx(), "button:importChooseFolder", fb.rect);
                if fb.clicked() {
                    let _ = app.run("file.addFolder", json!({}));
                }
                let pb = crate::widgets::text_button(ui, "importChooseFiles", crate::i18n::tr("Choose Files…"), false);
                register(ui.ctx(), "import:chooseFiles", pb.rect);
                register(ui.ctx(), "button:importChooseFiles", pb.rect);
                if pb.clicked() {
                    let _ = app.run("file.addPhotos", json!({}));
                }
            });
            ui.add_space(8.0);
            let mut sub = d.subfolders;
            let c = ui.checkbox(&mut sub, crate::i18n::tr("Include subfolders"));
            register(ui.ctx(), "check:includeSubfolders", c.rect);
            register(ui.ctx(), "check:importSubfolders", c.rect);
            if c.changed() {
                d.subfolders = sub;
                if d.has_folder_source() {
                    let _ = crate::import::rescan_with_subfolders(app, d, sub);
                }
            }
        });
        return;
    }
    let n = d.candidates.len();
    let dups = (0..n).filter(|i| is_dup(d, *i)).count();
    let mut filter: Filter = ui.ctx().data(|m| m.get_temp(filter_id())).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        let labels = [
            crate::i18n::tr_format!("All {n}", n = n),
            crate::i18n::tr_format!("New {n}", n = n - dups),
            crate::i18n::tr_format!("Duplicates {n}", n = dups),
        ];
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let chosen = match filter {
            Filter::All => 0,
            Filter::New => 1,
            Filter::Duplicates => 2,
        };
        if let Some(i) = crate::widgets::segments(ui, "importFilter", &refs, Some(chosen)) {
            filter = [Filter::All, Filter::New, Filter::Duplicates].get(i).copied().unwrap_or_default();
            ui.ctx().data_mut(|m| m.insert_temp(filter_id(), filter));
        }
        for (i, key) in ["all", "new", "dups"].iter().enumerate() {
            alias(ui.ctx(), &format!("button:importFilter-{i}"), &format!("import:filter:{key}"));
        }
        ui.add_space(8.0);
        let sel = d.selected_paths().len();
        ui.label(egui::RichText::new(crate::i18n::tr_format!("{sel} selected", sel = sel)).color(t.text_dim));
        if crate::widgets::text_button(ui, "importAll", crate::i18n::tr("Check All"), false).clicked() {
            for i in 0..n {
                let on = d.checked_by_default(i);
                if let Some(c) = d.checked.get_mut(i) {
                    *c = on;
                }
            }
        }
        if crate::widgets::text_button(ui, "importNone", crate::i18n::tr("Uncheck All"), false).clicked() {
            d.checked.iter_mut().for_each(|c| *c = false);
        }
        if d.has_folder_source() {
            ui.add_space(8.0);
            let mut sub = d.subfolders;
            let c = ui.checkbox(&mut sub, crate::i18n::tr("Include subfolders"));
            register(ui.ctx(), "check:includeSubfolders", c.rect);
            register(ui.ctx(), "check:importSubfolders", c.rect);
            if c.changed() {
                d.subfolders = sub;
                let _ = crate::import::rescan_with_subfolders(app, d, sub);
            }
        }
        ui.add_space(8.0);
        let fb = crate::widgets::text_button(ui, "importChooseFolder", crate::i18n::tr("Choose Folder…"), false);
        register(ui.ctx(), "import:chooseFolder", fb.rect);
        register(ui.ctx(), "button:importChooseFolder", fb.rect);
        if fb.clicked() {
            let _ = app.run("file.addFolder", json!({}));
        }
        let pb = crate::widgets::text_button(ui, "importChooseFiles", crate::i18n::tr("Choose Files…"), false);
        register(ui.ctx(), "import:chooseFiles", pb.rect);
        register(ui.ctx(), "button:importChooseFiles", pb.rect);
        if pb.clicked() {
            let _ = app.run("file.addPhotos", json!({}));
        }
    });
    // what the scan left out, when it left something out
    if let Some(note) = crate::import::scan_note(d) {
        let r = ui.label(egui::RichText::new(note).color(t.caution).small());
        crate::widgets::register(ui.ctx(), "label:importScanNote", r.rect);
    }
    let types = d.file_types();
    if types.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("File types:")).color(t.text_dim));
            for (ty, count) in &types {
                let on = d.type_checked(ty);
                if crate::widgets::text_button(ui, &format!("importType:{ty}"), &format!("{ty} · {count}"), on).clicked() {
                    d.check_type(ty, !on);
                }
            }
        });
    }
    ui.add_space(4.0);
    let shown: Vec<usize> = (0..n)
        .filter(|i| match filter {
            Filter::All => true,
            Filter::New => !is_dup(d, *i),
            Filter::Duplicates => is_dup(d, *i),
        })
        .collect();
    let avail = ui.available_width();
    let cols = ((avail + 6.0) / (CELL + 6.0)).floor().max(1.0) as usize;
    let rows = shown.len().div_ceil(cols);
    let row_h = CELL + 26.0;
    let height = max_h.unwrap_or(ui.available_height()).max(row_h);
    egui::ScrollArea::vertical().id_salt("import-screen-grid").max_height(height).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * row_h), Sense::hover());
        register(ui.ctx(), "import:grid", area.intersect(ui.clip_rect()));
        for (k, i) in shown.iter().enumerate() {
            let (c, r) = (k % cols, k / cols);
            let local = Rect::from_min_size(pos2(c as f32 * (CELL + 6.0), r as f32 * row_h), vec2(CELL, CELL + 20.0));
            if !local.intersects(viewport.expand(CELL)) {
                continue;
            }
            crate::import::candidate_cell(app, ui, d, *i, local.translate(area.min.to_vec2()));
        }
    });
}

/// The options in cards and the Import button; true when it was clicked.
fn options_column(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, beside: bool) -> bool {
    let n = count(app, d);
    // (the column's width, before any row could widen the ui)
    let column_w = ui.available_width();
    let mut go = false;
    let button_h = 48.0;
    let body = |app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog| {
        let was = crate::dialog_frame::set_finger_sized(ui.ctx(), true);
        // (inset: the group cards reach a little past the rows on each side)
        // labels above their controls: the column is narrow (inset: the group cards reach a
        // little past the rows on each side)
        let w = ui.available_width();
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| {
            ui.set_max_width(w - 20.0);
            crate::dialog_frame::stacked_form(ui, 92.0, |ui| options(app, ui, d));
        });
        crate::dialog_frame::set_finger_sized(ui.ctx(), was);
    };
    if beside {
        let room = ui.available_height() - button_h - 12.0;
        egui::ScrollArea::vertical().id_salt("import-screen-options").max_height(room.max(80.0)).auto_shrink([false, false]).show(ui, |ui| {
            body(app, ui, d);
        });
    } else if !d.immich {
        body(app, ui, d);
    }
    ui.add_space(8.0);
    let verb = if d.copy && d.move_files && !d.immich { "Move" } else { "Import" };
    let label = crate::i18n::tr_format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }, verb = crate::i18n::tr(verb), n = n);
    let (r, _) = ui.allocate_exact_size(vec2(column_w, button_h), Sense::hover());
    if crate::button::Button::new(&label).primary().large().enabled(n > 0).widget("import:go").show_at(ui, r).clicked() {
        go = true;
    }
    // (the dialog's OK button's id, for scripts and tests)
    register(ui.ctx(), "button:dialogOk", r);
    go
}

/// The camera roll's review: keep adding the photos taken later (the same switch as Settings ▸
/// Folders ▸ Camera roll ▸ Watch for new photos), and where they go.
fn camera_roll_watch(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let mut on = app.ui.camera_roll_watch;
    let c = ui.checkbox(&mut on, crate::i18n::tr("Keep adding new photos from the camera roll"));
    register(ui.ctx(), "check:importWatchCameraRoll", c.rect);
    if c.changed() {
        crate::camera_roll::set_watch(app, on);
    }
    // (device sync isn't in the web build)
    #[cfg(not(target_arch = "wasm32"))]
    let syncing = crate::panels::peer_sync::catalog_syncs(app);
    #[cfg(target_arch = "wasm32")]
    let syncing = false;
    let line = match (on, syncing) {
        (true, true) => {
            "Photos you take later are added on their own. While this catalog syncs, every camera roll photo is in the collection “Camera Roll - this device's name”; Collection below adds only these photos to another one."
        }
        (true, false) => "Photos you take later are added on their own, to All Photographs. Collection below adds only these photos to a collection.",
        (false, _) => "Only these photos are added. Turn this on, or Settings ▸ Folders ▸ Watch for new photos, to add new ones as you take them.",
    };
    let r = ui.label(egui::RichText::new(crate::i18n::tr(line)).color(t.text_dim).small());
    register(ui.ctx(), "label:importWatchCameraRollHelp", r.rect);
}

/// The option rows (in a dialog form, so their groups are cards).
fn options(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    options_for(app, ui, d, false);
}

/// [`options`]; `tab`: the Import tab's settings, which have no scanned files (no duplicates or
/// Recently Deleted rows) and no recipe (a browsed photo keeps its own adjustments).
pub(crate) fn options_for(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, tab: bool) {
    let t = Tokens::get(ui.ctx());
    use crate::dialog_frame::{form_row, group_begin, help};
    group_begin(ui, Some("How"));
    // the camera roll is added where it is (the camera keeps writing there; a copy would double it)
    let can_copy = !d.camera_roll && (app.session.library.as_ref().is_some_and(|l| l.on_disk) || app.services.pick_folder.is_some());
    form_row(ui, "Transfer", |ui| {
        let labels: Vec<&str> = if can_copy { vec!["Copy", "Move", "Add in place"] } else { vec!["Add in place"] };
        let chosen = match (d.copy, d.move_files, can_copy) {
            (_, _, false) => 0,
            (true, false, _) => 0,
            (true, true, _) => 1,
            (false, _, _) => 2,
        };
        let pick = crate::widgets::segments(ui, "importHow", &labels, Some(chosen));
        // the dialog's buttons' ids
        for (i, id) in if can_copy { ["importCopy", "importMove", "importAdd"].as_slice() } else { ["importAdd"].as_slice() }.iter().enumerate() {
            alias(ui.ctx(), &format!("button:importHow-{i}"), &format!("button:{id}"));
        }
        match (pick, can_copy) {
            (Some(0), true) => {
                d.copy = true;
                d.move_files = false;
            }
            (Some(1), true) => {
                d.copy = true;
                d.move_files = true;
            }
            (Some(_), _) => {
                d.copy = false;
                d.move_files = false;
            }
            _ => {}
        }
    });
    let r = ui.label(
        egui::RichText::new(crate::i18n::tr(match (d.copy, d.move_files) {
            (true, false) => "The files are copied; the originals are left as they are.",
            (true, true) => "Each original is removed from the source only after its copy is verified.",
            _ => "The catalog uses the files where they are; nothing is copied or moved.",
        }))
        .color(if d.copy && d.move_files { t.caution } else { t.text_dim })
        .small(),
    );
    register(ui.ctx(), "label:importModeHelp", r.rect);
    if d.camera_roll && !tab {
        camera_roll_watch(app, ui, &t);
    }
    if d.copy {
        form_row(ui, "Into", |ui| {
            ui.horizontal_wrapped(|ui| {
                let shown = if d.destination.trim().is_empty() { crate::i18n::tr("Catalog Originals").to_string() } else { d.destination.clone() };
                ui.label(egui::RichText::new(shown).color(t.text_label));
                if app.services.pick_folder.is_some()
                    && crate::widgets::text_button(ui, "importDest", crate::i18n::tr("Choose…"), false).clicked()
                    && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
                {
                    d.destination = f;
                }
                if !d.destination.is_empty() && crate::widgets::text_button(ui, "importDest", crate::i18n::tr("Library"), false).clicked() {
                    d.destination.clear();
                }
            });
        });
        form_row(ui, "Folders", |ui| {
            let opts = [("date", "By day"), ("month", "By month"), ("flat", "One folder"), ("custom", "Custom…")];
            let key = if d.organize.is_empty() { "date".to_string() } else { d.organize.clone() };
            let cur = opts.iter().find(|o| o.0 == key).map_or(opts[0].1, |o| o.1);
            let combo = crate::widgets::combo_box("import-organize").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (k, label) in opts {
                    let r = ui.selectable_label(key == k, crate::i18n::tr(label));
                    register(ui.ctx(), format!("button:importOrganize-{k}"), r.rect);
                    if r.clicked() {
                        d.organize = k.to_string();
                        if k == "custom" && d.folder_template.trim().is_empty() {
                            d.folder_template = DEFAULT_FOLDER_TEMPLATE.into();
                        }
                    }
                }
            });
            register(ui.ctx(), "combo:importOrganize", combo.response.rect);
        });
        if d.organize == "custom" {
            let folders_id = Id::new("import-folder-template");
            let open = form_row(ui, "Template", |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 40.0).max(80.0);
                    let r =
                        ui.add(egui::TextEdit::singleline(&mut d.folder_template).id(folders_id).hint_text(DEFAULT_FOLDER_TEMPLATE).desired_width(w));
                    register(ui.ctx(), "field:importFolderTemplate", r.rect);
                    crate::import::tag_toggle(ui, "importFolders")
                })
                .inner
            });
            if open {
                crate::import::tag_help(ui, "importFolders", &mut d.folder_template, folders_id);
            }
            match lcc_engine::rename::folder_template_error(&d.folder_template) {
                Some(e) => {
                    ui.label(egui::RichText::new(e).color(t.caution));
                }
                None => crate::import::unknown_tags_warning(ui, &d.folder_template),
            }
        }
        let rename_id = Id::new("import-rename");
        let open = form_row(ui, "Rename", |ui| {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 40.0).max(80.0);
                let r = ui.add(egui::TextEdit::singleline(&mut d.rename).id(rename_id).hint_text(crate::i18n::tr("keep names")).desired_width(w));
                register(ui.ctx(), "field:importRename", r.rect);
                crate::import::tag_toggle(ui, "importRename")
            })
            .inner
        });
        if open {
            crate::import::tag_help(ui, "importRename", &mut d.rename, rename_id);
        }
        crate::import::unknown_tags_warning(ui, &d.rename);
        if let Some(example) = crate::import::example_destination(app, d) {
            let r = ui.label(egui::RichText::new(crate::i18n::tr_format!("Example: {example}", example = example)).color(t.text_dim).small());
            register(ui.ctx(), "label:importExample", r.rect);
        }
        if !d.move_files {
            let r = crate::dialog_frame::checkbox(ui, &mut d.dng, crate::i18n::tr("Raws as DNG"));
            register(ui.ctx(), "check:importDng", r.rect);
        }
    }
    if tab {
        apply_rows(app, ui, d, tab);
        return;
    }
    let mut skip = !d.import_duplicates;
    let r = crate::dialog_frame::checkbox(ui, &mut skip, crate::i18n::tr("Don't Import Suspected Duplicates"));
    register(ui.ctx(), "check:importSkipDuplicates", r.rect);
    if r.changed() {
        d.set_skip_duplicates(skip);
    }
    if d.trashed.iter().any(|t| *t) {
        form_row(ui, "Deleted", |ui| {
            let opts: [(&str, &str); 3] = [("restore", "Restore"), ("fresh", "As new"), ("", "Leave")];
            let chosen = opts.iter().position(|(v, _)| *v == d.on_deleted);
            let labels: Vec<&str> = opts.iter().map(|(_, l)| *l).collect();
            if let Some(i) = crate::widgets::segments(ui, "importOnDeleted", &labels, chosen)
                && let Some((v, _)) = opts.get(i)
            {
                d.set_on_deleted(v);
            }
        });
        help(ui, "Some are in Recently Deleted: Restore keeps their edits; As new deletes the old record first.");
    }
    apply_rows(app, ui, d, tab);
}

/// The Apply card: the preset stack, camera profiles, metadata, keywords, recipe (not in the Import
/// tab), album.
fn apply_rows(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, tab: bool) {
    use crate::dialog_frame::{form_row, group_begin};
    group_begin(ui, Some("Apply"));
    crate::import::preset_stack_section(app, ui, d, true);
    crate::import::camera_profile_section(app, ui, d, true);
    // (always there: New Preset… makes one when there are none)
    form_row(ui, "Metadata", |ui| crate::import::metadata_preset_picker(app, ui, d));
    form_row(ui, "Keywords", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.keywords).hint_text(crate::i18n::tr("comma, separated")).desired_width(f32::INFINITY));
        register(ui.ctx(), "field:importKeywords", r.rect);
    });
    if d.options_skip_browsed() {
        let t = Tokens::get(ui.ctx());
        ui.label(
            egui::RichText::new(crate::i18n::tr("Keywords and presets apply to new files only, not to photos already in Local"))
                .color(t.text_dim)
                .small(),
        );
    }
    if !tab {
        crate::import::recipe_section(app, ui, d);
    }
    let mut albums = crate::album_picker::entries_from(&app.session.catalog, |_| None);
    albums.retain(|a| a.folder || !a.smart);
    d.album = d.album.filter(|id| albums.iter().any(|a| a.id == *id && !a.folder));
    form_row(ui, "Collection", |ui| {
        ui.vertical(|ui| {
            let mut chosen = d.album;
            let mut extra = None;
            let empty = crate::i18n::tr(if d.new_album.is_empty() { "None" } else { "New collection" });
            let response = crate::album_picker::AlbumPicker::new("import-album", &mut chosen, &albums)
                .width(ui.available_width())
                .empty_text(empty)
                .show_with_choices(ui, |ui| {
                    let none = ui.selectable_label(d.album.is_none() && d.new_album.is_empty(), crate::i18n::tr("None"));
                    register(ui.ctx(), "importAlbum:none", none.rect);
                    if none.clicked() {
                        extra = Some(false);
                    }
                    let new = ui.selectable_label(d.album.is_none() && !d.new_album.is_empty(), crate::i18n::tr("New collection…"));
                    register(ui.ctx(), "importAlbum:new", new.rect);
                    if new.clicked() {
                        extra = Some(true);
                    }
                    ui.separator();
                    extra.is_some()
                });
            if response.changed() {
                d.album = chosen;
                d.new_album.clear();
            }
            if let Some(new) = extra {
                d.album = None;
                if !new {
                    d.new_album.clear();
                } else if d.new_album.is_empty() {
                    d.new_album = crate::i18n::tr("Imported Photos").into();
                }
            }
            if d.album.is_none() && !d.new_album.is_empty() {
                let r = ui.add(egui::TextEdit::singleline(&mut d.new_album).desired_width(f32::INFINITY));
                register(ui.ctx(), "field:importAlbumName", r.rect);
                if extra == Some(true) {
                    // just asked for: in view, where its name is typed
                    r.scroll_to_me(None);
                }
            }
        });
    });
}
