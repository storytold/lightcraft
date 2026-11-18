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
    // Photo ▸ Show in Folders: the Folders panel opens, and so do the folders above the one shown
    // (`reveal_chosen` unfolds them again once its memory of the last reveal is cleared)
    // …and the column scrolls to its row (cleared once the panels are drawn)
    if side.contains(&PanelId::Folders)
        && let Some(path) = app.ui.reveal_folder.take()
    {
        crate::panels::classic::set_open(app, PanelId::Folders, true);
        ui.data_mut(|d| {
            d.remove::<Option<String>>(egui::Id::new(super::LIBFOLDER_REVEALED));
            d.insert_temp(egui::Id::new(super::SCROLL_TO_ROW), format!("libfolder:{path}").to_lowercase());
        });
    }
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
        _ => [("leftBar-import", "Import…", "dialog.import"), ("leftBar-export", "Export…", "file.export")],
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
            d.insert_temp(egui::Id::new("left-deepest-indent-next"), 0.0f32);
            // where the visible part of the content ends (screen x), for what stays at the edge
            d.insert_temp(egui::Id::new("left-visible-right"), ui.cursor().left() + viewport.max.x);
            // where it starts (screen y): what a section pinned at the top is measured against
            d.insert_temp(egui::Id::new("left-view-top"), ui.cursor().top() + viewport.min.y);
        });
        columns(app, ui, viewport, side, total, picks, deleted);
        ui.data_mut(|d| d.remove::<String>(egui::Id::new(super::SCROLL_TO_ROW)));
        crate::widgets::follow_focus(ui);
        // what the rows asked for becomes next frame's width
        let next = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width-next"))).unwrap_or(0.0);
        if (next - super::content_width(ui.ctx())).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-content-width"), next));
            ui.ctx().request_repaint();
        }
        // …and the deepest row drawn (or passed over out of sight) sets next frame's indent step,
        // so a deep library path narrows the nesting before it cuts the names
        let deepest = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-deepest-indent-next"))).unwrap_or(0.0);
        let step = narrowed_step(viewport.width(), deepest, next, super::indent_step(ui.ctx()));
        if step != super::indent_step(ui.ctx()) {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-indent-step"), step));
            ui.ctx().request_repaint();
        }
    });
}

/// The indent step for next frame: [`super::step_for`]'s, narrowed by a band at a time while the
/// widest row (`content`, drawn at step `current`) would still not fit the panel `panel_w` wide —
/// the Classic column's rows carry longer names than `step_for` leaves room for. A wider step is
/// taken only when the content would fit at it (as if every level of the deepest row widened), so
/// the step settles instead of flipping between two bands.
fn narrowed_step(panel_w: f32, deepest: f32, content: f32, current: f32) -> f32 {
    let levels = deepest / super::INDENT_STEP;
    let mut step = super::step_for(panel_w, deepest);
    if levels < 1.0 || !(panel_w.is_finite() && content.is_finite()) {
        return step;
    }
    while step > super::MIN_INDENT_STEP && content + levels * (step - current) > panel_w + 0.5 {
        step -= 2.0;
    }
    step.max(super::MIN_INDENT_STEP)
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
                    folders_body(app, ui);
                }
            }
            // the fork's own: folders on disk shown in place, not in the catalog (Lightroom Classic has no such panel,
            // so it comes last, under its own header)
            PanelId::Local => {
                if crate::panels::classic::header(app, ui, p, side).1 {
                    super::local_body(app, ui);
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
            // panels below get the room back. This is the one place the presets are drawn.
            PanelId::Presets => {
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

/// All Photographs (Lightroom Classic's name), Quick Collection, Previous Import, Recently Added,
/// Picks, Missing (and, in the Classic Catalog panel, Recently Deleted).
pub(super) fn catalog_section(app: &mut LccApp, ui: &mut egui::Ui, total: usize, picks: usize, deleted: usize, with_deleted: bool) {
    let src = app.session.source;
    let quick = app.session.catalog.quick_collection().map(|q| app.session.catalog.album_count(q)).unwrap_or(0);
    for (id, icon, label, count, s) in [
        ("all", Icon::Photos, "All Photographs", Some(total), LibrarySource::All),
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

/// The Classic Folders panel, as Lightroom Classic lays it out: a bar per disk the library's photos are on, with
/// that disk's folders below it (a click on the bar folds them; it doesn't choose the disk, as in Lightroom). Folders
/// not on a disk of their own are listed as they are. Only the folders the library's photos were
/// imported from are listed, with their subfolders — no empty folders off the disk.
fn folders_body(app: &mut LccApp, ui: &mut egui::Ui) {
    let tree = app.caches.folder_tree(&app.session.catalog);
    super::reveal_chosen(app, ui, &tree);
    for n in tree.iter().take(64) {
        if !n.volume {
            super::folder_rows(app, ui, std::slice::from_ref(n), 0.0);
            continue;
        }
        let folder_k = lcc_catalog::query::folder_key(&n.path);
        let key = format!("libfolder:{folder_k}");
        let open_id = egui::Id::new(("libfolder-open", folder_k));
        let mut open = super::tree_open(app, ui, &key, open_id, true);
        let bar = crate::panels::folders::volume_bar(ui, n, open);
        super::row_menu(app, &bar, n);
        if bar.clicked() {
            open = !open;
            super::set_tree_open(app, ui, &key, open_id, open);
        }
        if !open {
            continue;
        }
        if n.own > 0 && n.selectable {
            // photos right at the disk's top: the disk is a folder of its own too, holding them and the rest
            super::folder_rows(app, ui, std::slice::from_ref(n), 0.0);
        } else {
            super::folder_rows(app, ui, &n.children, 0.0);
        }
    }
}
