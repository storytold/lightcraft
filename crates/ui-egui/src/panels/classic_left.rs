//! Library's Classic left column (Navigator, Catalog, Folders, Collections) and the Catalog
//! section's fork rows, kept out of the shared `left.rs` (U.5): `left::show` hands Library to
//! [`show`], and the sidebar layout calls [`catalog_section`]. A child module of `left`, so it
//! uses that file's private helpers without widening them.

use dac_catalog::AlbumId;
use dac_engine::LibrarySource;
use egui::{Rect, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::icons::Icon;
use crate::module::PanelId;
use crate::widgets::icon_button;

/// Library's left column: the panels in a scroll area as wide as its widest row.
pub(super) fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let counts = app.caches.counts(&app.session.catalog);
    let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
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
        });
        columns(app, ui, viewport, total, picks, deleted);
        // what the rows asked for becomes next frame's width
        let next = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width-next"))).unwrap_or(0.0);
        if (next - super::content_width(ui.ctx())).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-content-width"), next));
            ui.ctx().request_repaint();
        }
    });
}

/// Library's Classic left column: Navigator, Catalog, Folders, Collections, each under a
/// collapsible header (see [`super::classic`]), in the user's order.
fn columns(app: &mut DacApp, ui: &mut egui::Ui, viewport: Rect, total: usize, picks: usize, deleted: usize) {
    let side = crate::module::LIBRARY_LEFT;
    for p in crate::panels::classic::ordered(app, side) {
        match p {
            PanelId::Navigator => {
                crate::panels::navigator::show(app, ui);
                ui.add_space(4.0);
            }
            PanelId::Catalog => {
                if crate::panels::classic::header(app, ui, p, side).1 {
                    catalog_section(app, ui, total, picks, deleted, true);
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
            #[cfg(not(target_arch = "wasm32"))]
            PanelId::Publish => crate::panels::publish::show(app, ui, side),
            _ => {}
        }
    }
    ui.add_space(10.0);
}

/// All Photos, Quick Collection, Previous Import, Recently Added, Picks, Missing (and, in the
/// Classic Catalog panel, Recently Deleted).
pub(super) fn catalog_section(app: &mut DacApp, ui: &mut egui::Ui, total: usize, picks: usize, deleted: usize, with_deleted: bool) {
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
    ui.add_space(6.0);
}

fn recently_deleted_row(app: &mut DacApp, ui: &mut egui::Ui, deleted: usize) {
    let src = app.session.source;
    if super::row(app, ui, "recentlyDeleted", Icon::Trash, "Recently Deleted", Some(deleted), src == LibrarySource::RecentlyDeleted, 0.0).clicked() {
        let _ = app.run("library.source", json!({"kind": "recentlyDeleted"}));
    }
}

/// By Date: year → month → day rows.
fn dates_section(app: &mut DacApp, ui: &mut egui::Ui) {
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
fn collections_plus(app: &mut DacApp, ui: &mut egui::Ui, ar: Rect, viewport: Rect) {
    // the + stays at the visible edge when the sidebar is scrolled sideways
    let plus_right = (ar.left() + viewport.max.x).min(ar.right());
    let mut hdr = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(plus_right - 50.0, ar.top()), pos2(plus_right, ar.bottom())))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let plus = icon_button(&mut hdr, "albumNew", Icon::Plus, vec2(26.0, 26.0), false, true, "Create Album");
    egui::Popup::menu(&plus).show(|ui| {
        if ui.button(crate::i18n::tr("Create Album…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: false, parent: None });
        }
        if ui.button(crate::i18n::tr("Create Smart Album…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::SmartRules {
                id: None,
                name: String::new(),
                rules: dac_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
                parent: None,
            });
        }
        if ui.button(crate::i18n::tr("Create Smart Album from Filter…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: String::new(), parent: None });
        }
        if ui.button(crate::i18n::tr("Create Folder…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: true, parent: None });
        }
        crate::panels::collections::import_item(app, ui, None);
        // only once the albums were put in an order by hand
        if app.session.catalog.album_children_are_ordered(None) {
            ui.separator();
            if ui.button(crate::i18n::tr("Sort Albums A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked() {
                let _ = app.run("album.sort", json!({}));
            }
        }
    });
}

/// The album tree (opening the folders down to an album just made).
fn albums_body(app: &mut DacApp, ui: &mut egui::Ui) {
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
fn folders_body(app: &mut DacApp, ui: &mut egui::Ui) {
    let tree = app.caches.folder_tree(&app.session.catalog);
    super::reveal_chosen(app, ui, &tree);
    super::folder_rows(app, ui, &tree, 0.0);
}

/// Add the folders on disk that hold no library photo yet (empty ones, a folder just made with
/// New Folder…) under each folder that holds photos, as Classic's Folders panel lists them. Only
/// below folders with photos of their own (a parent like `Users` would list half the disk), a few
/// levels down, and capped, so a huge tree can't stall a frame.
pub fn with_disk_folders(mut tree: Vec<dac_catalog::FolderNode>) -> Vec<dac_catalog::FolderNode> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut budget = MAX_DISK_FOLDERS;
        for n in &mut tree {
            add_disk_folders(n, 0, &mut budget);
        }
    }
    tree
}

/// The most folders [`with_disk_folders`] reads or adds.
#[cfg(not(target_arch = "wasm32"))]
const MAX_DISK_FOLDERS: usize = 2_000;

#[cfg(not(target_arch = "wasm32"))]
fn add_disk_folders(n: &mut dac_catalog::FolderNode, empty_depth: usize, budget: &mut usize) {
    for c in &mut n.children {
        add_disk_folders(c, 0, budget);
    }
    // a folder with photos of its own, or one this pass added (up to 3 levels)
    let scan = !n.volume && n.selectable && (n.own > 0 || (n.count == 0 && empty_depth > 0)) && empty_depth <= 3;
    if !scan || *budget == 0 {
        return;
    }
    *budget -= 1;
    let Ok(rd) = std::fs::read_dir(&n.path) else { return };
    let mut added = Vec::new();
    for e in rd.flatten() {
        if *budget == 0 {
            break;
        }
        let Ok(t) = e.file_type() else { continue };
        if !t.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{}/{}", n.path.trim_end_matches(['/', '\\']), name);
        if n.children.iter().any(|c| super::same_folder(&c.path, &path)) {
            continue;
        }
        *budget -= 1;
        let mut child = dac_catalog::FolderNode { name, path, count: 0, own: 0, volume: false, selectable: true, label: None, children: Vec::new() };
        add_disk_folders(&mut child, empty_depth + 1, budget);
        added.push(child);
    }
    added.sort_by_cached_key(|c| c.name.to_lowercase());
    n.children.extend(added);
    n.children.sort_by_cached_key(|c| c.name.to_lowercase());
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
        let node = |name: &str, path: String, own: usize, children| dac_catalog::FolderNode {
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
        let out = super::with_disk_folders(tree);
        let names: Vec<&str> = out[0].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["new", "shoot"]);
        assert_eq!(out[0].children[0].count, 0);
        assert_eq!(out[0].children[0].children[0].name, "inner", "a level inside a new folder too");
        assert_eq!(out[0].children[1].count, 2, "the folder with photos is kept as it was");
        // a missing folder is no error
        let gone = super::with_disk_folders(vec![node("gone", format!("{p}/nope"), 1, vec![])]);
        assert!(gone[0].children.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
