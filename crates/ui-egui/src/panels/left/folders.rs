use std::collections::HashMap;

use super::albums::pointer_over_row;
use super::local::same_folder;
use super::rows::{
    LIBFOLDER_REVEALED, MAX_DEPTH, MenuAt, SideEvent, SideRow, SideSpec, entry, host_menu, rule, set_tree_open, side_list, submenu, tree_open,
};
use super::{FolderNode, LibrarySource, LightcraftApp, Rect, Tokens, json, vec2};

/// What a Folders row stands for, by the row's key.
struct Meta<'a> {
    node: &'a FolderNode,
    open_id: egui::Id,
    open: bool,
}

/// The rows of `nodes` and, for the open ones, of the folders inside them.
fn flatten<'a>(
    app: &LightcraftApp,
    ui: &egui::Ui,
    nodes: &'a [FolderNode],
    depth: usize,
    rows: &mut Vec<SideRow>,
    meta: &mut HashMap<String, Meta<'a>>,
) {
    for n in nodes {
        let key = lightcraft_catalog::query::folder_key(&n.path);
        let open_id = egui::Id::unique(("libfolder-open", key));
        // a disk starts open, and so does a folder that holds nothing itself and leads to one
        // folder (`Users` → `me`): the first row where the library branches is what you look for
        let leads_on = n.own == 0 && n.children.len() == 1;
        let open = tree_open(ui, open_id, n.volume || leads_on);
        // a row whose path would cover other disks' photos too only opens and closes
        let name = if n.selectable { n.name.clone() } else { crate::i18n::tr("This Computer").to_string() };
        let id = format!("libfolder:{}", n.path);
        // the label is said with the name, as it is shown beside it
        let spoken = match n.label {
            Some(l) => format!("{name}, {}", crate::i18n::color_label(&app.session.catalog, l)),
            None => name.clone(),
        };
        let tip = if n.children.is_empty() { n.path.clone() } else { format!("{} ({})", n.path, crate::i18n::tr("includes the folders inside it")) };
        let mut row = SideRow::new(id.clone(), "folder", name)
            .spoken(spoken)
            .count(Some(n.count))
            .depth(depth)
            .mark(n.label.map(crate::theme::label_color))
            .tip(tip);
        if !n.children.is_empty() {
            row = row.group(open, format!("libraryFolderToggle:{}", n.path));
        }
        rows.push(row);
        meta.insert(id, Meta { node: n, open_id, open });
        if open && !n.children.is_empty() && depth < MAX_DEPTH {
            flatten(app, ui, &n.children, depth + 1, rows, meta);
        }
    }
}

pub(super) fn folder_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[FolderNode]) {
    let mut rows = Vec::new();
    let mut meta = HashMap::new();
    flatten(app, ui, nodes, 0, &mut rows, &mut meta);
    let selected = meta
        .iter()
        .find(|(_, m)| {
            m.node.selectable
                && app.session.source == LibrarySource::LibraryFolder
                && app.session.library_folder.as_deref().is_some_and(|f| same_folder(f, &m.node.path))
        })
        .map(|(key, _)| key.clone());
    let spec = SideSpec::new("folders", crate::i18n::tr("Folders"), app.ui.show_counts).selected(selected.as_deref());
    let out = side_list(ui, spec, rows);
    let mut request = None;
    for event in &out.events {
        match event {
            SideEvent::Open { key, open } => {
                if let Some(m) = meta.get(key) {
                    set_tree_open(ui, m.open_id, *open);
                }
            }
            SideEvent::Activate(key) => {
                let Some(m) = meta.get(key) else { continue };
                if m.node.selectable {
                    // a source like an album or a Local folder: it replaces what the grid showed
                    let _ = app.run("library.source", json!({"kind": "libraryFolder", "path": m.node.path}));
                } else {
                    set_tree_open(ui, m.open_id, !m.open);
                }
            }
            SideEvent::Menu { key, at } => {
                // (the startup disk has no menu)
                if meta.get(key).is_some_and(|m| !(m.node.volume && m.node.path == "/")) {
                    request = Some((key.clone(), MenuAt::Point(*at)));
                }
            }
        }
    }
    if app.ui.dragging_photos.is_some() {
        for row in &out.shown {
            if let Some(m) = meta.get(&row.key) {
                folder_drop_target(app, ui, row.rect, m.node, m.open_id, &mut m.open.clone());
            }
        }
    }
    host_menu(ui, "folders", crate::i18n::tr("Folder"), out.response.as_ref(), request, |ui, key| {
        if let Some(m) = meta.get(key) {
            row_menu(app, ui, m.node);
        }
    });
}

// "Folders" (drawn in `show`): where on disk the library's photos were imported from, with photo counts (see
// `lightcraft_catalog::folders`). A click makes that folder the source, like an album or a Local
// folder: its photos and those of the folders inside it fill the grid. The triangle opens a
// level. Only folders holding imported photos are listed; every folder on disk is under Local.

/// Whenever the shown folder changes (a click, an agent, a rename or its undo), open the rows
/// above it so it is on screen; folding one by hand afterwards sticks until the choice changes.
pub(super) fn reveal_chosen(app: &LightcraftApp, ui: &egui::Ui, tree: &[FolderNode]) {
    let shown = app.session.library_folder.clone().filter(|_| app.session.source == LibrarySource::LibraryFolder);
    let chosen = shown.filter(|c| !lightcraft_catalog::query::folder_key(c).is_empty());
    let seen = egui::Id::unique(LIBFOLDER_REVEALED);
    let now = chosen.as_deref().map(lightcraft_catalog::query::folder_key);
    if ui.data(|d| d.get_temp::<Option<String>>(seen)) == Some(now.clone()) {
        return;
    }
    ui.data_mut(|d| d.insert_temp(seen, now));
    let Some(chosen) = chosen else { return };
    fn open_above(ui: &egui::Ui, nodes: &[FolderNode], chosen: &str) {
        for n in nodes {
            if lightcraft_catalog::query::folder_within(chosen, &n.path) && !same_folder(chosen, &n.path) {
                let key = lightcraft_catalog::query::folder_key(&n.path);
                ui.data_mut(|d| d.insert_temp(egui::Id::unique(("libfolder-open", key)), true));
                open_above(ui, &n.children, chosen);
            }
        }
    }
    open_above(ui, tree, &chosen);
}

/// The Folders row that stands for folder `dir`, however it is spelled. `None` when no row is that
/// folder.
pub(crate) fn folder_row_of<'a>(tree: &'a [FolderNode], dir: &str) -> Option<&'a FolderNode> {
    // only the rows that hold `dir` are opened (the startup disk's holds the other disks' paths
    // too, so more than one may), and the tree is at most `MAX_DEPTH` deep
    tree.iter()
        .filter(|n| lightcraft_catalog::query::folder_within(dir, &n.path))
        .find_map(|n| if same_folder(&n.path, dir) { Some(n) } else { folder_row_of(&n.children, dir) })
}

/// How long photos dragged over a closed Folders row wait before it opens, so a folder inside it
/// can be reached.
const SPRING_OPEN_SECONDS: f64 = 0.7;

/// A Folders row while photos are dragged from the grid: a release moves their files there on
/// disk (`photo.moveToFolder`, one undo step). The row lights up and the dragged label says where
/// they will go, as the same gesture onto an album only adds them to it. Held over a closed row,
/// it opens. Photos that can't go there leave it dark (a row covering several disks; Local photos,
/// not in the library) and a release says why; photos already there say so.
fn folder_drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect, n: &FolderNode, open_id: egui::Id, open: &mut bool) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    if !pointer_over_row(ui, rect) {
        return;
    }
    spring_open(ui, n, open_id, open);
    // (the label, the confirmation and the toast say which folder when another row has the name)
    let name = crate::destination::folder(&app.caches.folder_tree(&app.session.catalog), &n.path);
    let photos: Vec<_> = ids.iter().filter_map(|id| app.session.catalog.photo(lightcraft_catalog::PhotoId(*id))).collect();
    let refused = if !n.selectable {
        Some(crate::i18n::tr("This row covers more than one disk: drop on a folder inside it"))
    } else if photos.iter().any(|p| p.local) {
        Some(crate::i18n::tr("Local photos aren't in the library yet: Add to My Photos first, then move them"))
    } else {
        None
    };
    let here = |p: &lightcraft_catalog::Photo| match &p.source {
        lightcraft_catalog::Source::File { path } => std::path::Path::new(path).parent().is_some_and(|d| same_folder(&d.to_string_lossy(), &n.path)),
        _ => false,
    };
    let all_here = !photos.is_empty() && photos.iter().all(|p| here(p));
    let count = photos.len();
    let released = ui.input(|i| i.pointer.primary_released());
    let hint = match (&refused, all_here) {
        (Some(_), _) => crate::i18n::tr("Can't be moved here").to_string(),
        (None, true) => crate::i18n::tr_format!("Already in “{}”", name),
        (None, false) => crate::i18n::tr_format!("Move {n} photo{} to “{}”", if count == 1 { "" } else { "s" }, name, n = count),
    };
    crate::panels::grid::drag_hint(ui.ctx(), hint);
    if let Some(why) = refused {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        if released {
            app.toast(ui.ctx(), why);
            app.ui.dragging_photos = None;
        }
        return;
    }
    if all_here {
        if released {
            app.ui.dragging_photos = None;
        }
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_stroke(rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    if released {
        app.ui.dragging_photos = None;
        // a drop moves files on disk: asked first (as Lightroom Classic does), unless the person
        // said not to; the menu's Move Selected Photos Here is asked for by name and goes at once
        if app.ui.settings.ask_before_move {
            let library = app.session.library_generation();
            app.ui.dialog = Some(crate::state::Dialog::MovePhotos { ids, folder: n.path.clone(), name, dont_ask: false, library });
        } else {
            move_photos_here(app, ui.ctx(), Some(&ids), &n.path, &name);
        }
    }
}

/// Photos dragged over the closed row `n` for [`SPRING_OPEN_SECONDS`] open it (a row with folders
/// inside it). Restarts on each row the pointer comes to, and when it comes back to one after
/// leaving it (or after another drag).
fn spring_open(ui: &egui::Ui, n: &FolderNode, open_id: egui::Id, open: &mut bool) {
    if *open || n.children.is_empty() {
        return;
    }
    let key = egui::Id::unique("folder-spring-open");
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    // held without a break: over this row last frame too
    let since = match ui.data(|d| d.get_temp::<(String, f64, u64)>(key)) {
        Some((path, since, seen)) if path == n.path && seen.saturating_add(1) >= frame => since,
        _ => now,
    };
    ui.data_mut(|d| d.insert_temp(key, (n.path.clone(), since, frame)));
    if now - since >= SPRING_OPEN_SECONDS {
        *open = true;
        set_tree_open(ui, open_id, true);
        ui.data_mut(|d| d.remove::<(String, f64, u64)>(key));
    } else {
        // keep the clock running while the pointer rests
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Move photos (`ids`, else the selection) into the folder `path` on disk (`photo.moveToFolder`).
/// Photos that leave the view leave the selection, so nothing done next (Delete, an edit) reaches
/// photos no longer on screen. How many moved, or why none did.
pub(crate) fn move_photos(app: &mut LightcraftApp, ids: Option<&[u64]>, path: &str) -> Result<u64, String> {
    let mut params = json!({"folder": path});
    if let Some(ids) = ids {
        params["ids"] = json!(ids);
    }
    let r = app.run("photo.moveToFolder", params)?;
    let visible: std::collections::HashSet<lightcraft_catalog::PhotoId> = app.session.visible_cloned().into_iter().collect();
    let sel = &mut app.session.selection;
    sel.ids.retain(|id| visible.contains(id));
    if sel.active.is_some_and(|a| !visible.contains(&a)) {
        sel.active = sel.ids.first().copied();
    }
    Ok(r["moved"].as_u64().unwrap_or(0))
}

/// What a move of `n` photos into the folder `name` says: how many moved and that Undo moves them
/// back, or that they were there already.
pub(crate) fn moved_text(n: u64, name: &str) -> String {
    if n == 0 {
        crate::i18n::tr_format!("Already in “{}”", name)
    } else {
        crate::i18n::tr_format!("Moved {n} photo{} to “{}”. Edit → Undo moves them back.", if n == 1 { "" } else { "s" }, name, n = n)
    }
}

/// What `folder.create` of the folder at `path` inside `parent` says when it succeeds: the folder
/// is on disk now, but it has no photos, so the Folders list may not show it yet.
pub(crate) fn created_text(path: &str, parent: &str) -> String {
    let name = std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |f| f.to_string_lossy().to_string());
    crate::i18n::tr_format!("Created folder “{name}” in “{parent}”", name = name, parent = lightcraft_catalog::folders::folder_label(parent))
}

/// [`move_photos`], saying how it went in a toast.
fn move_photos_here(app: &mut LightcraftApp, ctx: &egui::Context, ids: Option<&[u64]>, path: &str, name: &str) {
    match move_photos(app, ids, path) {
        Ok(n) => app.toast(ctx, moved_text(n, name)),
        Err(e) => app.toast(ctx, e),
    }
}

/// The context menu of a Folders row: a folder's own, a disk's (remove it).
fn row_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    if !n.volume {
        folder_menu_for_library(app, ui, n);
        return;
    }
    folder_label_menu(app, ui, n);
    locate_missing_item(app, ui, n);
    synchronize_item(app, ui, n, &n.name);
    rule(ui);
    if entry("disk", crate::i18n::tr("Remove Disk from Library…"))
        .tip(crate::i18n::tr("Moves every photo imported from this disk to Recently Deleted; no file is touched"))
        .show(ui)
    {
        app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: n.path.clone(), name: n.name.clone(), count: n.count, disk: true });
    }
}

/// Locate Missing Files… of a Folders row: choose where this folder moved, then let the existing
/// background finder relink only this folder's photos (including the folders inside it).
fn locate_missing_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    if entry("folder", crate::i18n::tr("Locate Missing Files…")).widget("folderLocateMissing").show(ui)
        && let Err(e) = app.run("file.findMissing", json!({"within": n.path}))
    {
        app.toast(ui.ctx(), e);
    }
}

/// The node Synchronize Folder… of a Local folder `path` works on, with the name its dialog shows:
/// the Folders row for that very folder, else (a folder above rows) the folder itself, as a folder
/// holding other folders. `None` when no library photo lies in `path` or inside it. Reads the
/// cached Folders tree only; the disk is not touched.
pub(super) fn library_node_for(tree: &[FolderNode], path: &str) -> Option<(FolderNode, String)> {
    use lightcraft_catalog::query::folder_within;
    fn exact<'a>(tree: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
        tree.iter().find_map(|n| if same_folder(&n.path, path) { Some(n) } else { exact(&n.children, path) })
    }
    fn any_inside(tree: &[FolderNode], path: &str) -> bool {
        tree.iter().any(|n| (n.count > 0 && folder_within(&n.path, path)) || any_inside(&n.children, path))
    }
    if let Some(n) = exact(tree, path).filter(|n| n.count > 0) {
        return Some((n.clone(), lightcraft_catalog::folders::folder_label(&n.path)));
    }
    if !any_inside(tree, path) {
        return None;
    }
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let node = FolderNode {
        name: name.clone(),
        path: path.to_string(),
        count: 0,
        own: 0,
        volume: false,
        selectable: false,
        label: None,
        children: Vec::new(),
    };
    Some((node, name))
}

/// Move Folder To… can't start while Open Library… or New Library… waits for its folder: the
/// dialogs answer in the order they were opened ([`crate::pick::poll`]), so the library could
/// switch first and the move then run in the other library, moving the folder on disk with
/// nothing to relink (issue #3140).
fn can_move_folder(app: &LightcraftApp) -> Result<(), String> {
    for (command, label) in [("app.openLibrary", "Open Library…"), ("app.newLibrary", "New Library…")] {
        if app.pending_picks.iter().any(|p| p.command == command) {
            return Err(format!("wait for {label} to finish before moving a folder"));
        }
    }
    Ok(())
}

/// Move Folder To… of a folder row (widget `folderMoveTo`): asks for the destination through the
/// shared folder dialog, off the UI thread where the host has one (issue #3140: a dialog on the UI
/// thread froze the window); `folder.move` then runs when it closes ([`crate::pick::poll`]).
/// Disabled while a library dialog is up ([`can_move_folder`]).
pub(super) fn move_folder_item(app: &mut LightcraftApp, ui: &mut egui::Ui, path: &str) {
    if app.services.pick_folder.is_none() && app.services.picker.is_none() {
        return;
    }
    if !entry("folder", crate::i18n::tr("Move Folder To…")).enabled(can_move_folder(app).is_ok()).widget("folderMoveTo").show(ui) {
        return;
    }
    if let Err(e) = can_move_folder(app) {
        app.toast(ui.ctx(), e);
        return;
    }
    let req = crate::pick::PickRequest::folder(crate::i18n::tr("Move Folder To…"));
    let params = json!({"path": path});
    let picked = crate::pick::ask(app, "folder.move", &params, "into", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x]));
    if let crate::pick::Picked::Now(v) = picked
        && let Some(into) = v.into_iter().next()
    {
        let said = app.run("folder.move", json!({"path": path, "into": into})).map(|r| crate::pick::answered_message("folder.move", &r));
        match said {
            Ok(Some(text)) | Err(text) => app.toast(ui.ctx(), text),
            Ok(None) => {}
        }
    }
}

/// Synchronize Folder… of a Folders row (widget `folderSynchronize`): opens its dialog, which
/// scans the folder in the background.
pub(super) fn synchronize_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode, name: &str) {
    let run = entry("folder", crate::i18n::tr("Synchronize Folder…"))
        .tip(crate::i18n::tr("Find photos added to or missing from this folder on disk, and XMP sidecars changed by other apps"))
        .widget("folderSynchronize")
        .show(ui);
    // a disk, or a folder holding disks, was chosen as such
    if run && let Err(e) = crate::sync::open(app, &n.path, name, n.volume || !n.selectable) {
        app.toast(ui.ctx(), e);
    }
}

/// Set Color Label ▸ of a Folders row (`folder.label`; the submenu is widget `folderLabelMenu`,
/// its colours `folderLabel:<colour>`).
fn folder_label_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    submenu(ui, crate::i18n::tr("Set Color Label"), Some("folderLabelMenu"), true, |ui, menu| {
        if let Some(l) = crate::panels::grid::label_items(app, ui, n.label, "folderLabel") {
            if let Err(e) = app.run("folder.label", json!({"path": n.path, "label": crate::panels::grid::label_param(l)})) {
                app.toast(ui.ctx(), e);
            }
            menu.request_close();
        }
    });
}

/// The context menu of a folder row: the folder's disk actions (the same as Local's, photos
/// follow) and taking its photos out of the library.
fn folder_menu_for_library(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    let path = n.path.as_str();
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    // what dialogs call it: the last two names, so same-named folders are told apart
    let label = lightcraft_catalog::folders::folder_label(path);
    if entry("folder", crate::i18n::tr("Rename Folder…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
            title: crate::i18n::tr_format!("Rename “{name}”", name = label),
            hint: "Folder name (renamed on disk; its photos follow)".into(),
            value: name.clone(),
            command: "folder.rename".into(),
            params: json!({"path": path}),
            key: "name".into(),
        });
    }
    move_folder_item(app, ui, path);
    if entry("folder", crate::i18n::tr("New Folder on Disk…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
            title: crate::i18n::tr_format!("New Folder in “{name}”", name = label),
            hint: "Folder name (created on disk)".into(),
            value: String::new(),
            command: "folder.create".into(),
            params: json!({"parent": path}),
            key: "name".into(),
        });
    }
    if !app.session.selection.ids.is_empty()
        && entry("folder", crate::i18n::tr("Move Selected Photos Here"))
            .tip(crate::i18n::tr("Moves the files and their XMP sidecars into this folder; never overwrites"))
            .show(ui)
    {
        let name = crate::destination::folder(&app.caches.folder_tree(&app.session.catalog), path);
        move_photos_here(app, ui.ctx(), None, path, &name);
    }
    reveal_item(app, ui, path);
    folder_label_menu(app, ui, n);
    locate_missing_item(app, ui, n);
    synchronize_item(app, ui, n, &label);
    rule(ui);
    if entry("folder", crate::i18n::tr("Remove from Library…"))
        .tip(crate::i18n::tr("Moves the photos imported from this folder to Recently Deleted; no file is touched"))
        .show(ui)
    {
        app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: path.to_string(), name: label.clone(), count: n.count, disk: false });
    }
}

/// Show in Finder / Explorer, where the host can.
pub(super) fn reveal_item(app: &mut LightcraftApp, ui: &mut egui::Ui, path: &str) {
    if app.services.reveal.is_some()
        && entry("folder", crate::i18n::tr(crate::menus::reveal_label())).show(ui)
        && let Some(f) = app.services.reveal.as_mut()
    {
        let _ = f(path);
    }
}
