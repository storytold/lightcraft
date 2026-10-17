//! Library's and Develop's Classic left columns (fork-owned). Library draws Navigator, Catalog,
//! Folders, Collections and Publish Services; Develop draws Classic's Develop column — Navigator,
//! Presets, Versions (Classic's Snapshots), History and Collections — with upstream's My Photos
//! sidebar offered there as a panel of its own (hidden by default, so a pull from upstream stays
//! mechanical), and it pins the Navigator above the scroll area, where Lightroom Classic keeps it.
//! Kept out of the shared `left.rs` (U.5): `left::show` hands the module's side to
//! [`show`], and the sidebar layout calls [`catalog_section`]. A child module of `left`, so it
//! uses that file's private helpers without widening them.

use egui::{Rect, pos2, vec2};
use lcc_catalog::AlbumId;
use lcc_engine::LibrarySource;
use serde_json::json;

use crate::LccApp;
use crate::icons::Icon;
use crate::module::PanelId;
use crate::widgets::icon_button;

/// A module's Classic left column: the panels of `side` in the user's order, each under its
/// collapsible header, in a scroll area as wide as its widest row.
pub(super) fn show(app: &mut LccApp, ui: &mut egui::Ui, side: &[PanelId]) {
    let counts = app.caches.counts(&app.session.catalog);
    let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
    // The Navigator stays at the top of the column and the panels below it scroll under it, as in
    // Lightroom Classic (issue #76): the loupe's level and the way back to it are always in reach,
    // wherever the column was left scrolled. It is not a panel of the scroll: `columns` skips it.
    if !app.ui.hidden_panels.contains(&PanelId::Navigator) {
        pinned_navigator(app, ui);
    }
    // Lightroom Classic's two buttons under the column (issue #118): Import… / Export… in
    // Library, Copy… / Paste in Develop. Outside the scroll area, so they never scroll away.
    let full = ui.available_rect_before_wrap();
    let bar = Rect::from_min_max(pos2(full.left(), full.bottom() - BOTTOM_BAR_H), full.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| bottom_bar(app, ui, bar));
    let body = Rect::from_min_max(full.min, pos2(full.right(), bar.top()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| scrolled_columns(app, ui, side, total, picks, deleted));
}

/// Height of the Import… / Export… (Copy… / Paste) bar at the foot of the left column.
const BOTTOM_BAR_H: f32 = 46.0;

/// The buttons of the bar at the foot of the left column, by module: (widget id, label, command).
pub(crate) fn bottom_bar_buttons(module: crate::module::ModuleId) -> [(&'static str, &'static str, &'static str); 2] {
    match module {
        crate::module::ModuleId::Develop => [("leftBar-copy", "Copy…", "dialog.copySettings"), ("leftBar-paste", "Paste", "develop.paste")],
        _ => [("leftBar-import", "Import…", "file.addPhotos"), ("leftBar-export", "Export…", "dialog.export")],
    }
}

/// Lightroom Classic's bar under the left column: two buttons sharing its width.
fn bottom_bar(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().hline(r.x_range(), r.top() + 0.5, egui::Stroke::new(1.0, t.divider));
    let inner = r.shrink2(vec2(10.0, 11.0));
    let gap = 8.0;
    let w = ((inner.width() - gap) / 2.0).max(0.0);
    for (i, (id, label, cmd)) in bottom_bar_buttons(app.ui.module).into_iter().enumerate() {
        let x = inner.left() + i as f32 * (w + gap);
        let br = Rect::from_min_size(pos2(x, inner.top()), vec2(w, inner.height()));
        let enabled = match cmd {
            "develop.paste" => app.session.active().is_some() && app.session.clipboard.is_some(),
            c => crate::menus::ui_enabled(app, c),
        };
        let label = crate::i18n::tr(label);
        let resp = ui.interact(br, egui::Id::new(("left-bottom-bar", id)), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        crate::widgets::register(ui.ctx(), format!("button:{id}"), br);
        let fill = if !enabled {
            t.button
        } else if resp.is_pointer_button_down_on() {
            t.pressed
        } else if resp.hovered() {
            t.hover
        } else {
            t.button
        };
        let p = ui.painter();
        p.rect(br, egui::CornerRadius::same(3), fill, egui::Stroke::new(1.0, t.button_border), egui::StrokeKind::Inside);
        let color = if enabled { t.text } else { t.text_dim };
        p.with_clip_rect(br).text(br.center(), egui::Align2::CENTER_CENTER, label, t.font(12.5), color);
        if resp.clicked() {
            let _ = app.run(cmd, json!({}));
        }
    }
}

/// The column's panels under the pinned Navigator, in their scroll area.
fn scrolled_columns(app: &mut LccApp, ui: &mut egui::Ui, side: &[PanelId], total: usize, picks: usize, deleted: usize) {
    egui::ScrollArea::both().id_salt("left-scroll").auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        super::drag_auto_scroll(app, ui);
        // rows are as wide as the widest one needs (last frame), at least the panel
        let wide = viewport.width().max(super::content_width(ui.ctx()));
        ui.set_min_width(wide);
        ui.set_max_width(wide);
        ui.data_mut(|d| {
            d.insert_temp(egui::Id::new("left-content-width-next"), 0.0f32);
            // where the visible part of the content ends (screen x), for what stays at the edge
            d.insert_temp(egui::Id::new("left-visible-right"), ui.cursor().left() + viewport.max.x);
            // where it starts (screen y): what a section pinned at the top is measured against
            d.insert_temp(egui::Id::new("left-view-top"), ui.cursor().top() + viewport.min.y);
        });
        columns(app, ui, viewport, side, total, picks, deleted);
        crate::widgets::follow_focus(ui);
        // what the rows asked for becomes next frame's width
        let next = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width-next"))).unwrap_or(0.0);
        if (next - super::content_width(ui.ctx())).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-content-width"), next));
            ui.ctx().request_repaint();
        }
    });
}

/// The Navigator, pinned at the top of the column above the scroll area; the panels below scroll
/// under it, so the loupe's level (and the way back to the photo) never leaves the panel. Drawn at
/// the panel's width — it does not travel sideways with the rows below — and its own header folds
/// it, so a folded Navigator is just its header row.
fn pinned_navigator(app: &mut LccApp, ui: &mut egui::Ui) {
    let (w, avail) = (ui.available_width(), ui.available_height());
    ui.data_mut(|d| d.insert_temp(egui::Id::new("left-visible-right"), ui.cursor().left() + w));
    // the navigator's own height is what comes back, so the scroll area starts under it
    ui.allocate_ui_with_layout(vec2(w, avail), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        crate::panels::navigator::show(app, ui);
    });
}

/// The panels of one side, in the user's order: Library's Classic column (Catalog, Folders,
/// Collections, Publish Services) or Develop's (Presets, Versions, History, Collections, and My
/// Photos when it is shown). The Navigator is not among them: [`pinned_navigator`] draws it above
/// the scroll area, where it stays.
fn columns(app: &mut LccApp, ui: &mut egui::Ui, viewport: Rect, side: &[PanelId], total: usize, picks: usize, deleted: usize) {
    for p in crate::panels::classic::ordered(app, side) {
        match p {
            PanelId::Navigator => {}
            PanelId::Catalog => {
                if crate::panels::classic::header(app, ui, p, side).1 {
                    catalog_section(app, ui, total, picks, deleted, true);
                    ui.add_space(6.0);
                    dates_section(app, ui);
                }
            }
            PanelId::Folders => {
                let (r, open) = crate::panels::classic::header(app, ui, p, side);
                crate::panels::folders::header_buttons(app, ui, r, viewport);
                if open {
                    crate::panels::folders::volumes(app, ui);
                    super::local_section(app, ui);
                    folders_body(app, ui);
                }
            }
            PanelId::Collections => {
                let (r, open) = crate::panels::classic::header(app, ui, p, side);
                // left of the header's disclosure triangle
                let mut vp = viewport;
                vp.max.x -= 26.0;
                collections_plus(app, ui, Rect::from_min_max(r.min, pos2(r.right() - 26.0, r.bottom())), vp);
                super::top_level_drop_target(app, ui, r);
                if open {
                    albums_body(app, ui);
                    crate::panels::collections::footer(app, ui);
                }
            }
            // Develop's own panels: the presets, the photo's versions and its history.
            // The presets are given a share of the column, as Lightroom's Presets panel has: left
            // to itself it takes all of it, and the panels below would be pushed out of sight. Its
            // own Classic header folds it (issue #75): folded it takes only that header, and the
            // panels below get the room back. It is drawn once: while the presets column (Window ▸ Presets)
            // is open, that column is the presets.
            PanelId::Presets if !app.ui.presets => {
                let avail = ui.available_height();
                let h = if crate::panels::classic::is_open_with_key(app, crate::panels::presets::IN_COLUMN_KEY) {
                    if avail.is_finite() { (avail * 0.5).clamp(200.0, 420.0) } else { 320.0 }
                } else {
                    crate::panels::presets::HEADER_H
                };
                ui.add_space(6.0);
                ui.allocate_ui(vec2(ui.available_width(), h), |ui| crate::panels::presets::show(app, ui, side, true));
                ui.add_space(6.0);
            }
            PanelId::Presets => {}
            PanelId::Versions => {
                if crate::panels::classic::header(app, ui, p, side).1 {
                    versions_or_history(app, ui, crate::panels::right::versions_body);
                    ui.add_space(6.0);
                }
            }
            PanelId::History => {
                if crate::panels::classic::header(app, ui, p, side).1 {
                    versions_or_history(app, ui, crate::panels::right::history_body);
                    ui.add_space(6.0);
                }
            }
            // upstream's My Photos sidebar (hidden until it is asked for): the whole column as
            // upstream draws it — its title row (the open library's name, the sections' menu) and
            // its own pinned headers — as one panel of this column. Self-headered, like the
            // Navigator: it is folded and moved by its own list rather than by a Classic header,
            // and another panel's menu (or `panel.show`) hides it.
            PanelId::Sources => {
                super::my_photos_title(app, ui);
                super::my_photos(app, ui, ui.clip_rect().top());
            }
            #[cfg(not(target_arch = "wasm32"))]
            PanelId::Publish => crate::panels::publish::show(app, ui, side),
            _ => {}
        }
    }
    ui.add_space(10.0);
}

/// The Versions or History rows of the photo being edited; with no photo, what to do about it.
fn versions_or_history(app: &mut LccApp, ui: &mut egui::Ui, rows: fn(&mut LccApp, &mut egui::Ui, lcc_catalog::PhotoId)) {
    match app.session.active() {
        Some(id) => rows(app, ui, id),
        None => {
            let t = crate::theme::Tokens::get(ui.ctx());
            ui.add_space(4.0);
            ui.label(egui::RichText::new(crate::i18n::tr("No photo selected")).color(t.text_dim));
            ui.add_space(4.0);
        }
    }
}

/// All Photos, Quick Collection, Previous Import, Recently Added, Picks, Missing (and, in the
/// Classic Catalog panel, Recently Deleted).
pub(super) fn catalog_section(app: &mut LccApp, ui: &mut egui::Ui, total: usize, picks: usize, deleted: usize, with_deleted: bool) {
    let src = app.session.source;
    let quick = app.session.catalog.quick_collection().map(|q| app.session.catalog.album_count(q)).unwrap_or(0);
    for (id, icon, label, count, s) in [
        ("all", Icon::Photos, "All Photos", Some(total), LibrarySource::All),
        ("quickCollection", Icon::Album, "Quick Collection", Some(quick), LibrarySource::QuickCollection),
        ("previousImport", Icon::Clock, "Previous Import", None, LibrarySource::PreviousImport),
        ("recentlyAdded", Icon::Clock, "Recently Added", None, LibrarySource::RecentlyAdded),
        ("picks", Icon::FlagPick, "Picks", Some(picks), LibrarySource::Picks),
    ] {
        if super::row(app, ui, id, icon, label, count, src == s, 0.0).clicked() {
            let _ = app.run("library.source", json!({"kind": id}));
        }
    }
    // photos whose files can't be found (checked every few seconds, not every frame)
    // the last export's photos (once something was exported, like Lightroom's set)
    if (!app.session.previous_export.is_empty() || src == LibrarySource::PreviousExport)
        && super::row(app, ui, "previousExport", Icon::Clock, "Previous Export", None, src == LibrarySource::PreviousExport, 0.0).clicked()
    {
        let _ = app.run("library.source", json!({"kind": "previousExport"}));
    }
    let missing = super::missing_count(app, ui);
    if (missing > 0 || src == LibrarySource::Missing)
        && super::row(app, ui, "missing", Icon::Folder, "Missing Photos", Some(missing), src == LibrarySource::Missing, 0.0).clicked()
    {
        let _ = app.run("library.source", json!({"kind": "missing"}));
    }
    if with_deleted {
        recently_deleted_row(app, ui, deleted);
    }
}

fn recently_deleted_row(app: &mut LccApp, ui: &mut egui::Ui, deleted: usize) {
    let src = app.session.source;
    let r = super::row(app, ui, "recentlyDeleted", Icon::Trash, "Recently Deleted", Some(deleted), src == LibrarySource::RecentlyDeleted, 0.0);
    if r.clicked() {
        let _ = app.run("library.source", json!({"kind": "recentlyDeleted"}));
    }
    r.context_menu(|ui| {
        let b = ui.add_enabled(deleted > 0, egui::Button::new(crate::i18n::tr("Empty Recently Deleted")));
        crate::widgets::register(ui.ctx(), "menu:emptyRecentlyDeleted", b.rect);
        if b.clicked() {
            let _ = crate::menubar::run_item(app, "library.emptyRecentlyDeleted", json!({}));
        }
    });
}

/// By Date: year → month → day rows.
fn dates_section(app: &mut LccApp, ui: &mut egui::Ui) {
    // By date
    let (_, dates_open) = super::sidebar_section_header(app, ui, "byDate", "By Date");
    let groups = if dates_open { app.caches.date_groups(&app.session.catalog) } else { Default::default() };
    for g in groups.iter() {
        // year → month → day; a click filters by that prefix, the triangle opens a level
        if super::date_row(app, ui, &g.year, &crate::i18n::date_group_label(&g.year, true), g.count, 0.0) {
            for (m, n) in &g.months {
                let label = crate::i18n::date_group_label(m, true);
                if super::date_row(app, ui, m, &label, *n, 16.0) {
                    for (d, n) in g.days.iter().filter(|(d, _)| d.starts_with(m.as_str())) {
                        let label = crate::i18n::date_group_label(d, true);
                        super::date_row(app, ui, d, &label, *n, 32.0);
                    }
                }
            }
        }
    }
}

/// The + menu at the right end of an albums / collections header.
fn collections_plus(app: &mut LccApp, ui: &mut egui::Ui, ar: Rect, viewport: Rect) {
    // the + stays at the visible edge when the sidebar is scrolled sideways
    let plus_right = (ar.left() + viewport.max.x).min(ar.right());
    let mut hdr = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(plus_right - 50.0, ar.top()), pos2(plus_right, ar.bottom())))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let plus = icon_button(&mut hdr, "albumNew", Icon::Plus, vec2(26.0, 26.0), false, true, "Create Collection");
    egui::Popup::menu(&plus).show(|ui| {
        if ui.button(crate::i18n::tr("Create Collection…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: true });
        }
        if ui.button(crate::i18n::tr("Create Smart Collection…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::SmartRules {
                id: None,
                name: String::new(),
                rules: lcc_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
                parent: None,
            });
        }
        if ui.button(crate::i18n::tr("Create Smart Collection from Filter…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: String::new(), parent: None });
        }
        if ui.button(crate::i18n::tr("Create Collection Set…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: true, parent: None, include_selected: false });
        }
        crate::panels::collections::import_item(app, ui, None);
        // only once the albums were put in an order by hand
        if app.session.catalog.album_children_are_ordered(None) {
            ui.separator();
            if ui.button(crate::i18n::tr("Sort Collections A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked() {
                let _ = app.run("album.sort", json!({}));
            }
        }
    });
}

/// The album tree (opening the folders down to an album just made).
fn albums_body(app: &mut LccApp, ui: &mut egui::Ui) {
    // the album just made: the folders down to it open, once (also when the section is shut)
    let reveal = app.ui.reveal_album.take();
    {
        let kids: super::AlbumKids =
            app.session.catalog.album_children_by_parent().into_iter().map(|(k, v)| (k, v.into_iter().cloned().collect())).collect();
        let cat = &app.session.catalog;
        let mut open_to = Vec::new();
        let mut cur = reveal.map(AlbumId).and_then(|id| cat.album(id)).and_then(|a| a.parent);
        while let Some(p) = cur.filter(|p| !open_to.contains(p) && open_to.len() < 64) {
            open_to.push(p);
            cur = cat.album(p).and_then(|a| a.parent);
        }
        super::albums_tree(app, ui, &kids, None, 0.0, &open_to);
    }
}

/// The Classic Folders panel's tree (no section header of its own).
fn folders_body(app: &mut LccApp, ui: &mut egui::Ui) {
    #[cfg(not(target_arch = "wasm32"))]
    disk::set_ctx(ui.ctx());
    let tree = app.caches.folder_tree(&app.session.catalog);
    super::reveal_chosen(app, ui, &tree);
    super::folder_rows(app, ui, &tree, 0.0);
}

/// Add the folders on disk that hold no library photo yet (empty ones, a folder just made with
/// New Folder…) under each folder that holds photos, as Classic's Folders panel lists them. Only
/// below folders with photos of their own (a parent like `Users` would list half the disk), a few
/// levels down, and capped, so a huge tree can't stall a frame.
///
/// The folders are listed off the UI thread ([`disk`]): this uses what the last listing of each
/// folder found and asks for the ones not listed yet (or listed a while ago), and the tree is made
/// again when they come in ([`disk_generation`]). Listing a folder tree on a network share can take
/// many seconds when the share's directory cache has expired, and the tree is made again after
/// every change to the catalog.
pub fn with_disk_folders(tree: Vec<lcc_catalog::FolderNode>) -> Vec<lcc_catalog::FolderNode> {
    #[cfg(not(target_arch = "wasm32"))]
    return merge_disk_folders(tree, &mut disk::subfolders);
    #[cfg(target_arch = "wasm32")]
    tree
}

/// The catalog changed: list the disk folders again (the tree keeps the old listings meanwhile).
pub fn invalidate_disk_folders() {
    #[cfg(not(target_arch = "wasm32"))]
    disk::invalidate();
}

/// Changes whenever a disk folder listing comes in: part of the folder tree's cache key.
pub fn disk_generation() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    return disk::generation();
    #[cfg(target_arch = "wasm32")]
    0
}

/// [`with_disk_folders`] with `list` giving the names of the folders inside a folder (`None`: not
/// known yet).
#[cfg(not(target_arch = "wasm32"))]
fn merge_disk_folders(mut tree: Vec<lcc_catalog::FolderNode>, list: &mut dyn FnMut(&str) -> Option<Vec<String>>) -> Vec<lcc_catalog::FolderNode> {
    let mut budget = MAX_DISK_FOLDERS;
    for n in &mut tree {
        add_disk_folders(n, 0, &mut budget, list);
    }
    tree
}

/// The most folders [`with_disk_folders`] reads or adds.
#[cfg(not(target_arch = "wasm32"))]
const MAX_DISK_FOLDERS: usize = 2_000;

#[cfg(not(target_arch = "wasm32"))]
fn add_disk_folders(n: &mut lcc_catalog::FolderNode, empty_depth: usize, budget: &mut usize, list: &mut dyn FnMut(&str) -> Option<Vec<String>>) {
    for c in &mut n.children {
        add_disk_folders(c, 0, budget, list);
    }
    // a folder with photos of its own, or one this pass added (up to 3 levels)
    let scan = !n.volume && n.selectable && (n.own > 0 || (n.count == 0 && empty_depth > 0)) && empty_depth <= 3;
    if !scan || *budget == 0 {
        return;
    }
    *budget -= 1;
    let Some(names) = list(&n.path) else { return };
    let mut added = Vec::new();
    for name in names {
        if *budget == 0 {
            break;
        }
        let path = format!("{}/{}", n.path.trim_end_matches(['/', '\\']), name);
        if n.children.iter().any(|c| super::same_folder(&c.path, &path)) {
            continue;
        }
        *budget -= 1;
        let mut child = lcc_catalog::FolderNode { name, path, count: 0, own: 0, volume: false, selectable: true, label: None, children: Vec::new() };
        add_disk_folders(&mut child, empty_depth + 1, budget, list);
        added.push(child);
    }
    added.sort_by_cached_key(|c| c.name.to_lowercase());
    n.children.extend(added);
    n.children.sort_by_cached_key(|c| c.name.to_lowercase());
}

/// The disk folder listings behind [`with_disk_folders`], made on a thread of their own so a slow
/// or offline network share never holds up a frame.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod disk {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError, mpsc};
    use std::time::Instant;

    /// Most folders whose listing is kept.
    const MAX_KEPT: usize = 8_192;

    #[derive(Default)]
    struct Lister {
        listed: Mutex<HashMap<String, (Instant, Vec<String>)>>,
        pending: Mutex<HashSet<String>>,
        jobs: Mutex<Option<mpsc::Sender<String>>>,
        generation: AtomicU64,
        ctx: Mutex<Option<egui::Context>>,
        /// Listings started before this are stale ([`invalidate`]).
        stale_before: Mutex<Option<Instant>>,
    }

    fn lister() -> &'static Lister {
        static L: OnceLock<Lister> = OnceLock::new();
        L.get_or_init(Lister::default)
    }

    /// The context to repaint when a listing comes in.
    pub fn set_ctx(ctx: &egui::Context) {
        let mut c = lister().ctx.lock().unwrap_or_else(PoisonError::into_inner);
        if c.is_none() {
            *c = Some(ctx.clone());
        }
    }

    pub fn generation() -> u64 {
        lister().generation.load(Ordering::Relaxed)
    }

    /// Every listing made so far is out of date (the catalog changed: a folder may have been
    /// created, moved or removed). The folders are listed again as the tree asks for them; until
    /// then the tree keeps the old listings.
    pub fn invalidate() {
        *lister().stale_before.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }

    /// The names of the folders inside `path` as last listed (`None` until it has been), asking
    /// for a listing when there is none or it is stale ([`invalidate`]).
    pub fn subfolders(path: &str) -> Option<Vec<String>> {
        let l = lister();
        let cached = l.listed.lock().unwrap_or_else(PoisonError::into_inner).get(path).cloned();
        let stale_before = *l.stale_before.lock().unwrap_or_else(PoisonError::into_inner);
        if needs_listing(cached.as_ref().map(|(at, _)| *at), stale_before) {
            queue(path);
        }
        cached.map(|(_, names)| names)
    }

    /// Whether a folder last listed at `listed` (`None`: never) is listed again. Only a catalog
    /// change makes a listing stale, never its age, so a pass over a slow share ends: a tree made
    /// again because listings came in doesn't ask for the same folders once more.
    pub(super) fn needs_listing(listed: Option<Instant>, stale_before: Option<Instant>) -> bool {
        match (listed, stale_before) {
            (None, _) => true,
            (Some(at), Some(s)) => at < s,
            (Some(_), None) => false,
        }
    }

    fn queue(path: &str) {
        let l = lister();
        if !l.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_string()) {
            return;
        }
        let mut jobs = l.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.is_none() {
            *jobs = start();
        }
        if jobs.as_ref().is_none_or(|tx| tx.send(path.to_string()).is_err()) {
            *jobs = None;
            l.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
        }
    }

    /// The thread that lists folders, one at a time.
    fn start() -> Option<mpsc::Sender<String>> {
        let (tx, rx) = mpsc::channel::<String>();
        let spawned = std::thread::Builder::new().name("lcc-disk-folders".into()).spawn(move || {
            for path in rx {
                // the listing counts from when it started: a change made while it ran makes it stale
                let started = Instant::now();
                let names = read(&path);
                let l = lister();
                {
                    let mut listed = l.listed.lock().unwrap_or_else(PoisonError::into_inner);
                    if listed.len() >= MAX_KEPT {
                        listed.clear();
                    }
                    listed.insert(path.clone(), (started, names));
                }
                l.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&path);
                l.generation.fetch_add(1, Ordering::Relaxed);
                if let Some(ctx) = l.ctx.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                    ctx.request_repaint();
                }
            }
        });
        match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                log::warn!("could not start the disk folder listing: {e}");
                None
            }
        }
    }

    /// The visible folders inside `path`, sorted; none when it can't be read (gone, offline).
    pub fn read(path: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(path) else { return Vec::new() };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    /// Folders holding no library photo (an empty one, one just created) are listed under the
    /// folder with photos that contains them; hidden ones are not.
    #[test]
    fn folders_without_photos_are_listed_under_a_folder_with_photos() {
        let d = std::env::temp_dir().join(format!("libfolders-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("new/inner")).unwrap();
        std::fs::create_dir_all(d.join(".hidden")).unwrap();
        std::fs::create_dir_all(d.join("shoot")).unwrap();
        let p = d.to_string_lossy().replace('\\', "/");
        let node = |name: &str, path: String, own: usize, children| lcc_catalog::FolderNode {
            name: name.into(),
            path,
            count: own,
            own,
            volume: false,
            selectable: true,
            label: None,
            children,
        };
        let tree = vec![node("root", p.clone(), 3, vec![node("shoot", format!("{p}/shoot"), 2, vec![])])];
        let out = super::merge_disk_folders(tree, &mut |p| Some(super::disk::read(p)));
        let names: Vec<&str> = out[0].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["new", "shoot"]);
        assert_eq!(out[0].children[0].count, 0);
        assert_eq!(out[0].children[0].children[0].name, "inner", "a level inside a new folder too");
        assert_eq!(out[0].children[1].count, 2, "the folder with photos is kept as it was");
        // a missing folder is no error
        let gone = super::merge_disk_folders(vec![node("gone", format!("{p}/nope"), 1, vec![])], &mut |p| Some(super::disk::read(p)));
        assert!(gone[0].children.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The disk folders are listed off the UI thread: the first tree comes back at once without
    /// them (a folder on a network share can take seconds to list), and they are there once the
    /// listing has come in, which changes the generation the tree is cached under.
    #[test]
    fn disk_folders_are_listed_off_the_ui_thread() {
        let d = std::env::temp_dir().join(format!("libfolders-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("2026/10-14 - Lake")).unwrap();
        let p = d.to_string_lossy().replace('\\', "/");
        let tree = || {
            vec![lcc_catalog::FolderNode {
                name: "root".into(),
                path: p.clone(),
                count: 1,
                own: 1,
                volume: false,
                selectable: true,
                label: None,
                children: vec![],
            }]
        };
        let before = super::disk_generation();
        let first = super::with_disk_folders(tree());
        assert!(first[0].children.is_empty(), "nothing waits for the disk");
        let t = std::time::Instant::now();
        let mut out = first;
        while t.elapsed() < std::time::Duration::from_secs(10) {
            if super::disk_generation() != before {
                out = super::with_disk_folders(tree());
                if out[0].children.first().is_some_and(|c| !c.children.is_empty()) {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(out[0].children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["2026"]);
        assert_eq!(out[0].children[0].children[0].name, "10-14 - Lake", "and the folders inside new ones, as they're listed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Only a catalog change makes a listing stale, not its age: a tree made again because a
    /// listing came in asks for no folder already listed since, so a pass over a slow network share
    /// ends instead of starting over.
    #[test]
    fn a_listing_is_made_again_only_after_a_catalog_change() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(60);
        assert!(super::disk::needs_listing(None, None), "never listed");
        assert!(!super::disk::needs_listing(Some(t0), None), "listed long ago, nothing changed since");
        assert!(super::disk::needs_listing(Some(t0), Some(t1)), "listed before a catalog change");
        assert!(!super::disk::needs_listing(Some(t1), Some(t0)), "listed after it");
    }

    /// A folder created after the first listing (New Folder…, which changes the catalog) shows up
    /// once the folders are listed again, without waiting for the listing to age.
    #[test]
    fn a_new_folder_shows_up_after_a_catalog_change() {
        let d = std::env::temp_dir().join(format!("libfolders-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("old")).unwrap();
        let p = d.to_string_lossy().replace('\\', "/");
        let tree = || {
            vec![lcc_catalog::FolderNode {
                name: "root".into(),
                path: p.clone(),
                count: 1,
                own: 1,
                volume: false,
                selectable: true,
                label: None,
                children: vec![],
            }]
        };
        let names = |out: &[lcc_catalog::FolderNode]| out[0].children.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let wait_for = |want: &[&str]| {
            let t = std::time::Instant::now();
            let mut got = Vec::new();
            while t.elapsed() < std::time::Duration::from_secs(10) {
                got = names(&super::with_disk_folders(tree()));
                if got == want {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            got
        };
        assert_eq!(wait_for(&["old"]), ["old"]);
        std::fs::create_dir_all(d.join("new")).unwrap();
        super::invalidate_disk_folders();
        assert_eq!(wait_for(&["new", "old"]), ["new", "old"], "the new folder is listed after the catalog change");
        let _ = std::fs::remove_dir_all(&d);
    }
}
