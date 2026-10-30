use super::albums::pointer_over_row;
use super::local::{same_folder, subfolders};
use super::rows::{INDENT_STEP, Inside, LIBFOLDER_REVEALED, Row, out_of_sight, tree_toggle};
use super::{Album, Branch, FolderNode, Icon, LibrarySource, LightcraftApp, Said, Tokens, TreeState, json, register, vec2};

/// A folder on disk with a disclosure triangle: click browses it, the triangle lists its
/// subfolders. In the tree that holds the folder being browsed (`reveal`), the folders on the
/// way down to it open whenever the browsed folder changes, so it shows highlighted in place.
/// `transient`: a top-level row listed for this session only (it offers Keep in Local).
/// `place`: the top-level folder this tree hangs from. A folder can be listed in more than one
/// tree (Pictures on its own and inside Home): each row opens and closes by itself, and their
/// triangles have ids of their own, else the copy drawn last takes the other's clicks (#1093).
#[allow(clippy::too_many_arguments)]
pub(super) fn folder_tree(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    place: &str,
    name: &str,
    path: &str,
    indent: f32,
    current: Option<&str>,
    reveal: bool,
    transient: bool,
    above: Option<Said>,
) {
    let branch = Branch::local(place, path);
    let sel = current.is_some_and(|c| same_folder(c, path));
    let on_the_way = !sel && current.is_some_and(|c| lightcraft_catalog::query::folder_within(c, path));
    let mut open = app.ui.sidebar_trees.is_open_under(&branch, above);
    if reveal && on_the_way {
        // opened once per browsed folder: collapsing it again afterwards sticks
        let revealed_id = egui::Id::unique(("folder-revealed", place.to_string(), path.to_string()));
        let target = current.map(str::to_string);
        let revealed = ui.data(|d| d.get_temp::<Option<String>>(revealed_id));
        if revealed != Some(target.clone()) {
            if target.is_some() && target == as_opened(ui.ctx()).local {
                // what the library opened with: the tree is as it was left, a folded row stays
                // folded (and this is no reveal to remember: coming back to the folder is one)
                app.ui.sidebar_trees.show_unless_left(&branch, above);
            } else {
                app.ui.sidebar_trees.show(&branch, above);
                ui.data_mut(|d| d.insert_temp(revealed_id, target));
            }
            open = app.ui.sidebar_trees.is_open_under(&branch, above);
        }
    }
    if !out_of_sight(app, ui, &format!("local:{path}"), name, None, indent, false) {
        // at the tree's own indent, as in Albums and Folders: the triangle sits in the gutter and the
        // icon lines up with the rows above
        let resp = Row::new(&format!("local:{path}"), Icon::Folder, name).selected(sel).indent(indent).show(app, ui).on_hover_text(path);
        let tr = tree_toggle(ui, &mut app.ui.sidebar_trees, &resp, indent, &branch, &mut open, format!("folderToggle:{path}"), Inside::Folds);
        if !tr.clicked()
            && resp.clicked()
            && let Err(e) = app.run("library.browse", json!({"path": path}))
        {
            app.toast(ui.ctx(), e);
        }
        crate::widgets::context_menu(&resp, |ui| {
            // the library actions of the Folders tree, when this folder (or one inside it) holds library photos
            let library = app.caches.folder_tree(&app.session.catalog);
            let held = library_node_for(&library, path);
            if (transient || indent > 0.0)
                && ui
                    .button(crate::i18n::tr(if transient { "Keep in Local" } else { "Add to Local" }))
                    .on_hover_text(crate::i18n::tr("List this folder in Local from now on"))
                    .clicked()
            {
                let _ = app.run("local.addRoot", json!({"path": path}));
                ui.close();
            }
            if indent == 0.0
                && ui
                    .button(crate::i18n::tr("Remove from Local"))
                    .on_hover_text(crate::i18n::tr("Hides this shortcut only; the folder and its photos stay as they are"))
                    .clicked()
            {
                let _ = app.run("local.hide", json!({"path": path}));
                ui.close();
            }
            if ui.button(crate::i18n::tr("Rename Folder…")).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                    title: crate::i18n::tr_format!("Rename “{name}”", name = name),
                    hint: "Folder name (renamed on disk; its photos follow)".into(),
                    value: name.to_string(),
                    command: "folder.rename".into(),
                    params: json!({"path": path}),
                    key: "name".into(),
                });
            }
            if app.services.pick_folder.is_some() && ui.button(crate::i18n::tr("Move Folder To…")).clicked() {
                let into = app.services.pick_folder.as_mut().and_then(|f| f());
                if let Some(into) = into {
                    match app.run("folder.move", json!({"path": path, "into": into})) {
                        Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("Moved; {} photo(s) relinked", r["relinked"])),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
            if app.services.reveal.is_some()
                && ui.button(crate::i18n::tr(crate::menus::reveal_label())).clicked()
                && let Some(f) = app.services.reveal.as_mut()
            {
                let _ = f(path);
            }
            if let Some(held) = &held {
                synchronize_item(app, ui, &held.0, &held.1);
            }
        });
    }
    if open && indent < INDENT_STEP * 8.0 {
        let inside = app.ui.sidebar_trees.inside(&branch, above);
        for (n, p) in subfolders(ui, path).iter() {
            folder_tree(app, ui, place, n, p, indent + INDENT_STEP, current, reveal, false, inside);
        }
    }
}

pub(super) fn folder_menu(app: &mut LightcraftApp, resp: &egui::Response, a: &Album) {
    crate::widgets::context_menu(resp, |ui| {
        if a.folder {
            // the same commands as File ▸ New…, aimed at this folder
            crate::widgets::menu_button(ui, crate::i18n::tr("New"), |ui| {
                for (label, command) in [
                    ("Create Album…", "dialog.newAlbum"),
                    ("Create Smart Album…", "dialog.smartAlbum"),
                    ("Create Smart Album from Filter…", "dialog.newSmartAlbum"),
                    ("Create Folder…", "dialog.newFolder"),
                ] {
                    if ui.button(crate::i18n::tr(label)).clicked() {
                        let _ = app.run(command, json!({"parent": a.id.0}));
                        ui.close();
                    }
                }
            });
            if app.session.catalog.album_children_are_ordered(Some(a.id))
                && ui.button(crate::i18n::tr("Sort Contents A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked()
            {
                let _ = app.run("album.sort", json!({"parent": a.id.0}));
                ui.close();
            }
            ui.separator();
        }
        if !a.folder && !a.is_smart() {
            let local_only = crate::panels::grid::local_only(app, &app.session.selected());
            let b = ui
                .add_enabled(!local_only, egui::Button::new(crate::i18n::tr("Add Selected Photos")))
                .on_disabled_hover_text(crate::panels::grid::albums_hint());
            if b.clicked() {
                match app.run("album.addPhotos", json!({"id": a.id.0})) {
                    Ok(r) => crate::panels::grid::toast_skipped(app, ui.ctx(), &r),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            // File ▸ Import's ways in; the import review opens with this album chosen
            let menu = crate::widgets::menu_button(ui, crate::i18n::tr("Import"), |ui| {
                let mut entry = |ui: &mut egui::Ui, label: String, widget: String, command: &str, params: serde_json::Value| {
                    let r = ui.button(label);
                    crate::widgets::register(ui, widget, r.rect);
                    if r.clicked() {
                        if let Err(e) = app.run(command, params) {
                            app.toast(ui.ctx(), e);
                        }
                        ui.close();
                    }
                };
                for (label, command, widget) in [
                    ("Import Photos…", "file.addPhotos", "button:albumImportPhotos"),
                    ("Import from Folder…", "file.addFolder", "button:albumImportFolder"),
                ] {
                    entry(ui, crate::i18n::tr(label).into(), widget.into(), command, json!({"album": a.id.0}));
                }
                // like File ▸ Import from Device: one entry per camera or card found
                let device = crate::widgets::menu_button(ui, crate::i18n::tr("Import from Device"), |ui| {
                    let devices = lightcraft_engine::devices::devices();
                    if devices.is_empty() {
                        ui.add_enabled(false, egui::Button::new(crate::i18n::tr("No Camera or Card Found")));
                    }
                    for (i, d) in devices.into_iter().enumerate() {
                        let params = json!({"path": d.path, "album": a.id.0});
                        entry(ui, format!("{}…", d.name), format!("button:albumImportDevice:{i}"), "file.addFromDevice", params);
                    }
                });
                crate::widgets::register(ui, "button:albumImportDevice", device.response.rect);
            });
            crate::widgets::register(ui, "button:albumImport", menu.response.rect);
        }
        if !a.folder && !a.is_smart() {
            let is_target = app.session.target_album == Some(a.id) || (app.session.target_album.is_none() && a.quick);
            if !is_target && ui.button(crate::i18n::tr("Set as Target Album (B adds to it)")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": if a.quick { serde_json::Value::Null } else { json!(a.id.0) }}));
            }
            if is_target && !a.quick && ui.button(crate::i18n::tr("Stop Using as Target Album")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": null}));
            }
        }
        if a.quick && ui.button(crate::i18n::tr("Clear Quick Collection")).clicked() {
            let _ = app.run("album.clearQuick", json!({}));
        }
        if a.is_smart() && ui.button(crate::i18n::tr("Edit Smart Album…")).clicked() {
            // the same editor as `dialog.smartAlbum` (older smart albums keep their filter fields;
            // it works on the rule set, upgraded)
            let _ = app.run("dialog.smartAlbum", json!({"id": a.id.0}));
        }
        if a.is_smart() && ui.button(crate::i18n::tr("Update Rules from Current Filter")).clicked() {
            // refused when the view would make the album test itself: say why
            if let Err(e) = app.run("album.setRules", json!({"id": a.id.0, "fromView": true})) {
                app.toast(ui.ctx(), e);
            }
        }
        if !a.folder {
            // export: show the album, select its photos, then the dialog / a preset
            let show_all = |app: &mut LightcraftApp| {
                let _ = app.run("library.source", json!({"kind": "album", "id": a.id.0}));
                let _ = app.run("library.selectAll", json!({}));
            };
            let has_photos = app.session.catalog.album_count(a.id) > 0;
            if ui.add_enabled(has_photos, egui::Button::new(crate::i18n::tr("Export Album…"))).clicked() {
                show_all(app);
                let _ = app.run("dialog.export", json!({}));
            }
            ui.add_enabled_ui(has_photos, |ui| {
                crate::widgets::menu_button(ui, crate::i18n::tr("Export Album with Preset"), |ui| {
                    for (p, _) in app.session.all_export_presets() {
                        if ui.button(&p.name).clicked() {
                            show_all(app);
                            if let Err(e) = app.run("app.export", json!({"preset": p.name, "background": true})) {
                                app.toast(ui.ctx(), e);
                            }
                        }
                    }
                });
            });
            ui.separator();
        }
        // move into another folder (not into itself or one of its own subfolders)
        let mut folders: Vec<(u64, String)> =
            app.session.catalog.albums().filter(|f| f.folder && !is_within(app, f.id, a.id)).map(|f| (f.id.0, f.name.clone())).collect();
        folders.sort_by_key(|(_, n)| n.to_lowercase());
        crate::widgets::menu_button(ui, crate::i18n::tr("Move to"), |ui| {
            if ui.add_enabled(a.parent.is_some(), egui::Button::new(crate::i18n::tr("Top Level"))).clicked() {
                let _ = app.run("album.move", json!({"id": a.id.0, "parent": null}));
            }
            for (fid, name) in &folders {
                if ui.add_enabled(a.parent.map(|p| p.0) != Some(*fid), egui::Button::new(name)).clicked() {
                    let _ = app.run("album.move", json!({"id": a.id.0, "parent": fid}));
                }
            }
        });
        if ui.button(crate::i18n::tr("Rename…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::RenameAlbum { id: a.id.0, name: a.name.clone() });
        }
        let delete = ui.button(crate::i18n::tr("Delete"));
        register(ui, format!("albumDelete:{}", a.id.0), delete.rect);
        let delete =
            delete.on_hover_text(crate::i18n::tr("Photos stay in All Photos. Deleting a folder also deletes its albums. You can undo this."));
        if delete.clicked() {
            if let Err(e) = app.run("album.delete", json!({"id": a.id.0})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
}

/// Whether `id` is `ancestor` or lies inside it.
pub(super) fn is_within(app: &LightcraftApp, id: lightcraft_catalog::AlbumId, ancestor: lightcraft_catalog::AlbumId) -> bool {
    let mut cur = Some(id);
    let mut guard = 0;
    while let Some(c) = cur {
        if c == ancestor {
            return true;
        }
        cur = app.session.catalog.album(c).and_then(|x| x.parent);
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    false
}

// "Folders" (drawn in `show`): where on disk the library's photos were imported from, with photo counts (see
// `lightcraft_catalog::folders`). A click makes that folder the source, like an album or a Local
// folder: its photos and those of the folders inside it fill the grid. The triangle opens a
// level. Only folders holding imported photos are listed; every folder on disk is under Local.

/// What the library had the grid show when it was opened (as the app starts, or by Open
/// Library): a Local folder, a folder of the library, each for as long as the grid still shows
/// it. That is no choice of now: the trees are as they were left, so showing where the grid is
/// opens only the rows nobody left folded ([`TreeState::show_unless_left`]). Once what is shown
/// changes, the rows above it open whatever was said of them, as they always have.
#[derive(Clone, Default, PartialEq)]
struct AsOpened {
    /// Which opening of a library this is of ([`lightcraft_engine::Session::library_generation`]).
    library: u64,
    local: Option<String>,
    folder: Option<String>,
}

const AS_OPENED: &str = "sidebar-shown-as-opened";

/// Follows what the grid shows, every frame and whether the sidebar is drawn or not, so that a
/// folder chosen while it is hidden is known for a choice when it is shown again.
pub(crate) fn follow_shown(app: &LightcraftApp, ctx: &egui::Context) {
    let library = app.session.library_generation();
    let local = app.session.browse.as_ref().filter(|_| app.session.source == LibrarySource::Folder).map(|b| b.path.clone());
    let folder = app.session.library_folder.clone().filter(|_| app.session.source == LibrarySource::LibraryFolder);
    let id = egui::Id::unique(AS_OPENED);
    let was = ctx.data(|d| d.get_temp::<AsOpened>(id));
    let now = match &was {
        // (what is no longer shown was left: coming back to it is a choice)
        Some(was) if was.library == library => AsOpened {
            library,
            local: was.local.clone().filter(|w| Some(w) == local.as_ref()),
            folder: was.folder.clone().filter(|w| Some(w) == folder.as_ref()),
        },
        _ => AsOpened { library, local, folder },
    };
    if was.as_ref() != Some(&now) {
        ctx.data_mut(|d| d.insert_temp(id, now));
    }
}

fn as_opened(ctx: &egui::Context) -> AsOpened {
    ctx.data(|d| d.get_temp::<AsOpened>(egui::Id::unique(AS_OPENED))).unwrap_or_default()
}

/// Whenever the shown folder changes (a click, an agent, a rename or its undo), open the rows
/// above it so it is on screen; folding one by hand afterwards sticks until the choice changes.
/// The folder the library opened with is no change: a row above it that was left folded stays
/// folded, unless `forced` (Show in Folders asked for it: a choice of now).
pub(super) fn reveal_chosen(app: &mut LightcraftApp, ui: &egui::Ui, tree: &[FolderNode], forced: bool) {
    let shown = app.session.library_folder.clone().filter(|_| app.session.source == LibrarySource::LibraryFolder);
    let as_left = !forced && shown.is_some() && shown == as_opened(ui.ctx()).folder;
    let chosen = shown.filter(|c| !lightcraft_catalog::query::folder_key(c).is_empty());
    let seen = egui::Id::unique(LIBFOLDER_REVEALED);
    let now = chosen.as_deref().map(lightcraft_catalog::query::folder_key);
    if ui.data(|d| d.get_temp::<Option<String>>(seen)) == Some(now.clone()) {
        return;
    }
    ui.data_mut(|d| d.insert_temp(seen, now));
    let Some(chosen) = chosen else { return };
    fn open_above(trees: &mut TreeState, nodes: &[FolderNode], chosen: &str, as_left: bool, above: Option<Said>) {
        for n in nodes {
            if lightcraft_catalog::query::folder_within(chosen, &n.path) && !same_folder(chosen, &n.path) {
                let branch = folder_branch(n, &lightcraft_catalog::query::folder_key(&n.path));
                if as_left {
                    trees.show_unless_left(&branch, above);
                } else {
                    trees.show(&branch, above);
                }
                let inside = trees.inside(&branch, above);
                open_above(trees, &n.children, chosen, as_left, inside);
            }
        }
    }
    open_above(&mut app.ui.sidebar_trees, tree, &chosen, as_left, None);
}

/// A Folders row in the remembered trees (`key`: its [`lightcraft_catalog::query::folder_key`]).
/// A disk starts open, and so does a folder that holds nothing itself and leads to one folder
/// (`Users` → `me`): the first row where the library branches is what you look for.
fn folder_branch(n: &FolderNode, key: &str) -> Branch {
    Branch::folder(key, n.volume || (n.own == 0 && n.children.len() == 1))
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

/// The rows of the Folders tree. `said`: what holds for these rows from the rows above them
/// ([`TreeState::inside`]).
pub(super) fn folder_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[FolderNode], indent: f32, said: Option<Said>) {
    for n in nodes {
        let key = lightcraft_catalog::query::folder_key(&n.path);
        let branch = folder_branch(n, &key);
        let mut open = app.ui.sidebar_trees.is_open_under(&branch, said);
        // a row whose path would cover other disks' photos too only opens and closes
        let name = if n.selectable { n.name.clone() } else { crate::i18n::tr("This Computer").to_string() };
        let id = format!("libfolder:{}", n.path);
        if !out_of_sight(app, ui, &id, &name, Some(n.count), indent, n.label.is_some()) {
            folder_row(app, ui, n, &id, &name, indent, &branch, &mut open, said);
        }
        if open && !n.children.is_empty() {
            let inside = app.ui.sidebar_trees.inside(&branch, said);
            folder_rows(app, ui, &n.children, indent + INDENT_STEP, inside);
        }
    }
}

/// One drawn row of [`folder_rows`], shown as `name`; `branch` and `open` are its triangle's.
#[allow(clippy::too_many_arguments)]
fn folder_row(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    n: &FolderNode,
    id: &str,
    name: &str,
    indent: f32,
    branch: &Branch,
    open: &mut bool,
    said: Option<Said>,
) {
    let sel = n.selectable
        && app.session.source == LibrarySource::LibraryFolder
        && app.session.library_folder.as_deref().is_some_and(|f| same_folder(f, &n.path));
    // the label is said with the name, as it is shown beside it
    let spoken = match n.label {
        Some(l) => format!("{name}, {}", crate::i18n::color_label(&app.session.catalog, l)),
        None => name.to_string(),
    };
    let mark = n.label.map(crate::theme::label_color);
    let resp = Row::new(id, Icon::Folder, name).spoken(&spoken).count(Some(n.count)).selected(sel).indent(indent).mark(mark).show(app, ui);
    folder_drop_target(app, ui, &resp, n, branch, open, said);
    let mut toggled = false;
    if !n.children.is_empty() {
        let tr =
            tree_toggle(ui, &mut app.ui.sidebar_trees, &resp, indent, branch, open, format!("libraryFolderToggle:{}", n.path), Inside::OpensAndFolds);
        toggled = tr.clicked();
        // the triangle sits on the row and takes its clicks: the menu opens from it too
        row_menu(app, &tr, n);
    }
    let tip = if n.children.is_empty() { n.path.clone() } else { format!("{} ({})", n.path, crate::i18n::tr("includes the folders inside it")) };
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && !toggled {
        if n.selectable {
            // a source like an album or a Local folder: it replaces what the grid showed
            let _ = app.run("library.source", json!({"kind": "libraryFolder", "path": n.path}));
        } else {
            *open = !*open;
            app.ui.sidebar_trees.set(branch, *open);
        }
    }
    row_menu(app, &resp, n);
}

/// How long photos dragged over a closed Folders row wait before it opens, so a folder inside it
/// can be reached.
const SPRING_OPEN_SECONDS: f64 = 0.7;

/// A Folders row while photos are dragged from the grid: a release moves their files there on
/// disk (`photo.moveToFolder`, one undo step). The row lights up and the dragged label says where
/// they will go, as the same gesture onto an album only adds them to it. Held over a closed row,
/// it opens. Photos that can't go there leave it dark (a row covering several disks; Local photos,
/// not in the library) and a release says why; photos already there say so.
fn folder_drop_target(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    n: &FolderNode,
    branch: &Branch,
    open: &mut bool,
    said: Option<Said>,
) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    if !pointer_over_row(ui, resp.rect) {
        return;
    }
    if spring_open(ui, n, open) {
        // open for the session, not a choice kept (as the folders above the browsed one); a row folded by hand or with the rows above it opens for good
        app.ui.sidebar_trees.show(branch, said);
    }
    let name = std::path::Path::new(&n.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| n.name.clone());
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
    let released = ui.input(|i| i.pointer.any_released());
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
    ui.painter().rect_stroke(resp.rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
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
fn spring_open(ui: &egui::Ui, n: &FolderNode, open: &mut bool) -> bool {
    if *open || n.children.is_empty() {
        return false;
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
        ui.data_mut(|d| d.remove::<(String, f64, u64)>(key));
        return true;
    }
    // keep the clock running while the pointer rests
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    false
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

/// [`move_photos`], saying how it went in a toast.
fn move_photos_here(app: &mut LightcraftApp, ctx: &egui::Context, ids: Option<&[u64]>, path: &str, name: &str) {
    match move_photos(app, ids, path) {
        Ok(n) => app.toast(ctx, moved_text(n, name)),
        Err(e) => app.toast(ctx, e),
    }
}

/// The context menu of a Folders row: a folder's own, a disk's (remove it), none for the
/// startup disk.
fn row_menu(app: &mut LightcraftApp, resp: &egui::Response, n: &FolderNode) {
    if !n.volume {
        folder_menu_for_library(app, resp, n);
    } else if n.path != "/" {
        crate::widgets::context_menu(resp, |ui| {
            folder_label_menu(app, ui, n);
            locate_missing_item(app, ui, n);
            synchronize_item(app, ui, n, &n.name);
            ui.separator();
            if ui
                .button(crate::i18n::tr("Remove Disk from Library…"))
                .on_hover_text(crate::i18n::tr("Moves every photo imported from this disk to Recently Deleted; no file is touched"))
                .clicked()
            {
                app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: n.path.clone(), name: n.name.clone(), count: n.count, disk: true });
                ui.close();
            }
        });
    }
}

/// Locate Missing Files… of a Folders row: choose where this folder moved, then let the existing
/// background finder relink only this folder's photos (including the folders inside it).
fn locate_missing_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    let r = ui.button(crate::i18n::tr("Locate Missing Files…"));
    register(ui, "folderLocateMissing", r.rect);
    if r.clicked() {
        if let Err(e) = app.run("file.findMissing", json!({"within": n.path})) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// The node Synchronize Folder… of a Local folder `path` works on, with the name its dialog shows:
/// the Folders row for that very folder, else (a folder above rows) the folder itself, as a folder
/// holding other folders. `None` when no library photo lies in `path` or inside it. Reads the
/// cached Folders tree only; the disk is not touched.
fn library_node_for(tree: &[FolderNode], path: &str) -> Option<(FolderNode, String)> {
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

/// Synchronize Folder… of a Folders row (widget `folderSynchronize`): opens its dialog, which
/// scans the folder in the background.
fn synchronize_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode, name: &str) {
    let r = ui
        .button(crate::i18n::tr("Synchronize Folder…"))
        .on_hover_text(crate::i18n::tr("Find photos added to or missing from this folder on disk, and XMP sidecars changed by other apps"));
    crate::widgets::register(ui, "folderSynchronize", r.rect);
    if r.clicked() {
        // a disk, or a folder holding disks, was chosen as such
        if let Err(e) = crate::sync::open(app, &n.path, name, n.volume || !n.selectable) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// Set Color Label ▸ of a Folders row (`folder.label`; the submenu is widget `folderLabelMenu`,
/// its colours `folderLabel:<colour>`).
fn folder_label_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
    let menu = crate::widgets::menu_button(ui, crate::i18n::tr("Set Color Label"), |ui| {
        if let Some(l) = crate::panels::grid::label_items(app, ui, n.label, "folderLabel") {
            if let Err(e) = app.run("folder.label", json!({"path": n.path, "label": crate::panels::grid::label_param(l)})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
    register(ui, "folderLabelMenu", menu.response.rect);
}

/// The context menu of a folder row: the folder's disk actions (the same as Local's, photos
/// follow) and taking its photos out of the library.
fn folder_menu_for_library(app: &mut LightcraftApp, resp: &egui::Response, n: &FolderNode) {
    let path = n.path.as_str();
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    // what dialogs call it: the last two names, so same-named folders are told apart
    let label = lightcraft_catalog::folders::folder_label(path);
    crate::widgets::context_menu(resp, |ui| {
        if ui.button(crate::i18n::tr("Rename Folder…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: crate::i18n::tr_format!("Rename “{name}”", name = label),
                hint: "Folder name (renamed on disk; its photos follow)".into(),
                value: name.clone(),
                command: "folder.rename".into(),
                params: json!({"path": path}),
                key: "name".into(),
            });
            ui.close();
        }
        if app.services.pick_folder.is_some() && ui.button(crate::i18n::tr("Move Folder To…")).clicked() {
            let into = app.services.pick_folder.as_mut().and_then(|f| f());
            if let Some(into) = into {
                match app.run("folder.move", json!({"path": path, "into": into})) {
                    Ok(r) => app.toast(ui.ctx(), format!("Moved; {} photo(s) relinked", r["relinked"])),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            ui.close();
        }
        if ui.button(crate::i18n::tr("New Folder on Disk…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: crate::i18n::tr_format!("New Folder in “{name}”", name = label),
                hint: "Folder name (created on disk)".into(),
                value: String::new(),
                command: "folder.create".into(),
                params: json!({"parent": path}),
                key: "name".into(),
            });
            ui.close();
        }
        let selected = app.session.selection.ids.len();
        if selected > 0
            && ui
                .button(crate::i18n::tr("Move Selected Photos Here"))
                .on_hover_text(crate::i18n::tr("Moves the files and their XMP sidecars into this folder; never overwrites"))
                .clicked()
        {
            move_photos_here(app, ui.ctx(), None, path, &name);
            ui.close();
        }
        if app.services.reveal.is_some()
            && ui.button(crate::i18n::tr(crate::menus::reveal_label())).clicked()
            && let Some(f) = app.services.reveal.as_mut()
        {
            let _ = f(path);
            ui.close();
        }
        folder_label_menu(app, ui, n);
        locate_missing_item(app, ui, n);
        synchronize_item(app, ui, n, &label);
        ui.separator();
        if ui
            .button(crate::i18n::tr("Remove from Library…"))
            .on_hover_text(crate::i18n::tr("Moves the photos imported from this folder to Recently Deleted; no file is touched"))
            .clicked()
        {
            app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: path.to_string(), name: label.clone(), count: n.count, disk: false });
            ui.close();
        }
    });
}
