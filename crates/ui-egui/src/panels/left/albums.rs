use std::collections::HashMap;

use super::rows::{AlbumDrag, MAX_DEPTH, MenuAt, SideEvent, SideRow, SideSpec, entry, host_menu, rule, set_tree_open, side_list, submenu, tree_open};
use super::{Album, AlbumId, Align2, LibrarySource, LightcraftApp, Rect, Tokens, json, pos2, register, vec2};

/// How far each level of the albums' tree steps in, for the line that shows where a dragged album
/// would go.
const LEVEL: f32 = 16.0;

/// Albums: the tree of albums and the folders they are sorted into. `reveal`: an album just made,
/// whose folders open down to it.
pub(super) fn albums_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, reveal: Option<u64>) {
    let kids: AlbumKids = app.session.catalog.album_children_by_parent().into_iter().map(|(k, v)| (k, v.into_iter().cloned().collect())).collect();
    let cat = &app.session.catalog;
    let mut open_to = Vec::new();
    let mut cur = reveal.map(AlbumId).and_then(|id| cat.album(id)).and_then(|a| a.parent);
    while let Some(p) = cur.filter(|p| !open_to.contains(p) && open_to.len() < 64) {
        open_to.push(p);
        cur = cat.album(p).and_then(|a| a.parent);
    }
    let mut rows = Vec::new();
    let mut meta = HashMap::new();
    flatten(app, ui, &kids, None, 0, &open_to, &mut rows, &mut meta);
    let selected = meta.iter().find(|(_, m)| !m.album.folder && app.session.source == LibrarySource::Album(m.album.id)).map(|(key, _)| key.clone());
    let spec = SideSpec::new("albums", crate::i18n::tr("Albums"), app.ui.show_counts).selected(selected.as_deref()).album_drag();
    let out = side_list(ui, spec, rows);
    let mut request = None;
    for event in &out.events {
        match event {
            SideEvent::Open { key, open } => {
                if let Some(open_id) = meta.get(key).and_then(|m| m.open_id) {
                    set_tree_open(ui, open_id, *open);
                }
            }
            SideEvent::Activate(key) => {
                let Some(m) = meta.get(key) else { continue };
                if m.album.folder {
                    // a folder is no source, so its row folds it too; the triangle is the same click, aimed
                    if let Some(open_id) = m.open_id {
                        set_tree_open(ui, open_id, !m.open);
                    }
                } else {
                    let _ = app.run("library.source", json!({"kind": "album", "id": m.album.id.0}));
                }
            }
            SideEvent::Menu { key, at } => {
                if meta.contains_key(key) {
                    request = Some((key.clone(), MenuAt::Point(*at)));
                }
            }
        }
    }
    for row in &out.shown {
        let Some(m) = meta.get(&row.key) else { continue };
        if m.problems {
            crate::widgets::register(ui, format!("albumProblem:{}", m.album.id.0), row.rect);
        }
    }
    for row in &out.shown {
        let Some(m) = meta.get(&row.key) else { continue };
        let mut open = m.open;
        if album_drag_over(app, ui, row.rect, &m.album, m.open_id.map(|_| &mut open), m.depth)
            && let Some(open_id) = m.open_id
        {
            set_tree_open(ui, open_id, true);
        }
        if !m.album.folder && !m.album.is_smart() {
            drop_target(app, ui, row.rect, &m.album);
        }
    }
    host_menu(ui, "albums", crate::i18n::tr("Album"), out.response.as_ref(), request, |ui, key| {
        if let Some(m) = meta.get(key) {
            folder_menu(app, ui, &m.album);
        }
    });
}

/// The albums inside each folder (`None`: the top level), in the order they are listed.
type AlbumKids = HashMap<Option<AlbumId>, Vec<Album>>;

/// What an Albums row stands for, by the row's key.
struct Meta {
    album: Album,
    depth: usize,
    /// A folder with something inside it: its open state (the triangle's), and where that is kept.
    open_id: Option<egui::Id>,
    open: bool,
    /// A smart album whose rules no longer check.
    problems: bool,
}

/// The rows of the albums inside `parent` and, for the open folders, of what is inside them.
/// `open_to`: folders forced open this frame (the way down to an album that was just made).
#[allow(clippy::too_many_arguments)]
fn flatten(
    app: &mut LightcraftApp,
    ui: &egui::Ui,
    all: &AlbumKids,
    parent: Option<AlbumId>,
    depth: usize,
    open_to: &[AlbumId],
    rows: &mut Vec<SideRow>,
    meta: &mut HashMap<String, Meta>,
) {
    // 64 folders deep at most: a catalog that loops must not overflow the stack
    if depth > MAX_DEPTH {
        return;
    }
    let Some(kids) = all.get(&parent) else { return };
    for a in kids {
        if a.folder {
            let open_id = egui::Id::unique(("folder-open", a.id.0));
            let mut open = tree_open(ui, open_id, true);
            if open_to.contains(&a.id) && !open {
                open = true;
                set_tree_open(ui, open_id, true);
            }
            let has_children = all.get(&Some(a.id)).is_some_and(|v| !v.is_empty());
            let id = format!("folder:{}", a.id.0);
            let mut row = SideRow::new(id.clone(), "folder", a.name.clone()).depth(depth);
            if has_children {
                row = row.group(open, format!("albumToggle:{}", a.id.0));
            }
            rows.push(row);
            meta.insert(id, Meta { album: a.clone(), depth, open_id: has_children.then_some(open_id), open, problems: false });
            if open && has_children {
                flatten(app, ui, all, Some(a.id), depth + 1, open_to, rows, meta);
            }
        } else {
            let icon = if a.is_smart() { "sparkles" } else { "bookmark" };
            // cached: a smart album's count scans the catalog (in the background: no number until it is known)
            let now = (app.session.clock)();
            let n = app.caches.album_counts(&app.session.catalog, app.session.library_generation(), &now, ui.ctx()).get(&a.id).copied();
            // the album B adds to is marked "+"
            let target =
                app.session.target_album.filter(|t| app.session.catalog.album(*t).is_some()).or_else(|| app.session.catalog.quick_collection());
            let label = if target == Some(a.id) { format!("{} +", a.name) } else { a.name.clone() };
            // a smart album whose rules no longer check (an album they test was deleted) says so
            let problems = app.caches.smart_album_problems(&app.session.catalog).get(&a.id).cloned().unwrap_or_default();
            let label = if problems.is_empty() { label } else { format!("⚠ {label}") };
            let id = format!("album:{}", a.id.0);
            let mut row = SideRow::new(id.clone(), icon, label).count(n).depth(depth);
            if let Some(rules) = &a.smart {
                // one tooltip: the rules, then what needs fixing in them
                let mut tip = crate::i18n::tr_format!("Smart album: {}", crate::i18n::filter_label(rules, &app.session.catalog));
                if !problems.is_empty() {
                    tip.push_str("\n\n");
                    tip.push_str(crate::i18n::tr("Some rules need fixing (Edit Smart Album…):"));
                    for p in &problems {
                        tip.push('\n');
                        tip.push_str(&crate::i18n::problem_line(p));
                    }
                }
                row = row.tip(tip);
            }
            rows.push(row);
            meta.insert(id, Meta { album: a.clone(), depth, open_id: None, open: false, problems: !problems.is_empty() });
        }
    }
}

/// Whether `id` is `ancestor` or lies inside it.
pub(super) fn is_within(app: &LightcraftApp, id: AlbumId, ancestor: AlbumId) -> bool {
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

/// The context menu of an album or folder row.
fn folder_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, a: &Album) {
    if a.folder {
        // the same commands as File ▸ New…, aimed at this folder
        submenu(ui, crate::i18n::tr("New"), None, true, |ui, _| {
            for (label, command) in [
                ("Create Album…", "dialog.newAlbum"),
                ("Create Smart Album…", "dialog.smartAlbum"),
                ("Create Smart Album from Filter…", "dialog.newSmartAlbum"),
                ("Create Folder…", "dialog.newFolder"),
            ] {
                if entry("new", crate::i18n::tr(label)).show(ui) {
                    let _ = app.run(command, json!({"parent": a.id.0}));
                }
            }
        });
        if app.session.catalog.album_children_are_ordered(Some(a.id))
            && entry("album", crate::i18n::tr("Sort Contents A–Z")).tip(crate::i18n::tr("Go back to listing them by name")).show(ui)
        {
            let _ = app.run("album.sort", json!({"parent": a.id.0}));
        }
        rule(ui);
    }
    if !a.folder && !a.is_smart() {
        add_selected_item(app, ui, a);
        import_menu(app, ui, a);
        let is_target = app.session.target_album == Some(a.id) || (app.session.target_album.is_none() && a.quick);
        if !is_target && entry("album", crate::i18n::tr("Set as Target Album (B adds to it)")).show(ui) {
            let _ = app.run("album.setTarget", json!({"id": if a.quick { serde_json::Value::Null } else { json!(a.id.0) }}));
        }
        if is_target && !a.quick && entry("album", crate::i18n::tr("Stop Using as Target Album")).show(ui) {
            let _ = app.run("album.setTarget", json!({"id": null}));
        }
    }
    if a.quick && entry("album", crate::i18n::tr("Clear Quick Collection")).show(ui) {
        let _ = app.run("album.clearQuick", json!({}));
    }
    if a.is_smart() && entry("album", crate::i18n::tr("Edit Smart Album…")).show(ui) {
        // the same editor as `dialog.smartAlbum` (older smart albums keep their filter fields;
        // it works on the rule set, upgraded)
        let _ = app.run("dialog.smartAlbum", json!({"id": a.id.0}));
    }
    if a.is_smart() && entry("album", crate::i18n::tr("Update Rules from Current Filter")).show(ui) {
        // refused when the view would make the album test itself: say why
        if let Err(e) = app.run("album.setRules", json!({"id": a.id.0, "fromView": true})) {
            app.toast(ui.ctx(), e);
        }
    }
    if !a.folder {
        export_items(app, ui, a);
        rule(ui);
    }
    move_menu(app, ui, a);
    if entry("album", crate::i18n::tr("Rename…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::RenameAlbum { id: a.id.0, name: a.name.clone() });
    }
    let delete = entry("album", crate::i18n::tr("Delete"))
        .widget(format!("albumDelete:{}", a.id.0))
        .tip(crate::i18n::tr("Photos stay in All Photos. Deleting a folder also deletes its albums. You can undo this."))
        .show(ui);
    if delete && let Err(e) = app.run("album.delete", json!({"id": a.id.0})) {
        app.toast(ui.ctx(), e);
    }
}

/// Add Selected Photos (Local photos can't be: the tooltip says why).
fn add_selected_item(app: &mut LightcraftApp, ui: &mut egui::Ui, a: &Album) {
    let local_only = crate::panels::grid::local_only(app, &app.session.selected());
    let hint = crate::panels::grid::albums_hint();
    let mut item = entry("album", crate::i18n::tr("Add Selected Photos")).enabled(!local_only);
    if local_only {
        item = item.tip(&hint);
    }
    if item.show(ui) {
        match app.run("album.addPhotos", json!({"id": a.id.0})) {
            Ok(r) => crate::panels::grid::toast_skipped(app, ui.ctx(), &r),
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
}

/// File ▸ Import's ways in; the import review opens with this album chosen.
fn import_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, a: &Album) {
    submenu(ui, crate::i18n::tr("Import"), Some("button:albumImport"), true, |ui, _| {
        let run = |app: &mut LightcraftApp, ui: &egui::Ui, command: &str, params: serde_json::Value| {
            if let Err(e) = app.run(command, params) {
                app.toast(ui.ctx(), e);
            }
        };
        for (label, command, widget) in
            [("Import Photos…", "file.addPhotos", "button:albumImportPhotos"), ("Import from Folder…", "file.addFolder", "button:albumImportFolder")]
        {
            if entry("import", crate::i18n::tr(label)).widget(widget).show(ui) {
                run(app, ui, command, json!({"album": a.id.0}));
            }
        }
        // like File ▸ Import from Device: one entry per camera or card found
        submenu(ui, crate::i18n::tr("Import from Device"), Some("button:albumImportDevice"), true, |ui, _| {
            let devices = lightcraft_engine::devices::devices();
            if devices.is_empty() {
                entry("device", crate::i18n::tr("No Camera or Card Found")).enabled(false).show(ui);
            }
            for (i, d) in devices.into_iter().enumerate() {
                let label = format!("{}…", d.name);
                if entry(&label, &label).widget(format!("button:albumImportDevice:{i}")).show(ui) {
                    run(app, ui, "file.addFromDevice", json!({"path": d.path, "album": a.id.0}));
                }
            }
        });
    });
}

/// Show the album and select its photos, so an export takes them.
fn show_all(app: &mut LightcraftApp, a: &Album) {
    let _ = app.run("library.source", json!({"kind": "album", "id": a.id.0}));
    let _ = app.run("library.selectAll", json!({}));
}

/// Export: show the album, select its photos, then the dialog / a preset.
fn export_items(app: &mut LightcraftApp, ui: &mut egui::Ui, a: &Album) {
    let has_photos = app.session.catalog.album_count(a.id) > 0;
    if entry("album", crate::i18n::tr("Export Album…")).enabled(has_photos).show(ui) {
        show_all(app, a);
        let _ = app.run("dialog.export", json!({}));
    }
    submenu(ui, crate::i18n::tr("Export Album with Preset"), None, has_photos, |ui, _| {
        for (p, _) in app.session.all_export_presets() {
            if entry("preset", &p.name).show(ui) {
                show_all(app, a);
                if let Err(e) = app.run("app.export", json!({"preset": p.name, "background": true})) {
                    app.toast(ui.ctx(), e);
                }
            }
        }
    });
}

/// Move into another folder (not into itself or one of its own subfolders).
fn move_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, a: &Album) {
    let mut folders: Vec<(u64, String)> =
        app.session.catalog.albums().filter(|f| f.folder && !is_within(app, f.id, a.id)).map(|f| (f.id.0, f.name.clone())).collect();
    folders.sort_by_key(|(_, n)| n.to_lowercase());
    submenu(ui, crate::i18n::tr("Move to"), None, true, |ui, _| {
        if entry("move", crate::i18n::tr("Top Level")).enabled(a.parent.is_some()).show(ui) {
            let _ = app.run("album.move", json!({"id": a.id.0, "parent": null}));
        }
        for (fid, name) in &folders {
            if entry(&format!("move-{fid}"), name).enabled(a.parent.map(|p| p.0) != Some(*fid)).show(ui) {
                let _ = app.run("album.move", json!({"id": a.id.0, "parent": fid}));
            }
        }
    });
}

/// Whether the pointer is over `rect` and over what the panel shows of it (a row scrolled out of
/// sight is not a drop target).
fn pointer_over(ui: &egui::Ui, rect: Rect) -> bool {
    crate::docking::drop_target(ui, rect, ui.input(|i| i.pointer.latest_pos()))
}

/// [`pointer_over`] for a row: a row scrolled beneath the pinned header is covered by it, so the
/// pointer there is over the header, not over the row.
pub(super) fn pointer_over_row(ui: &egui::Ui, rect: Rect) -> bool {
    pointer_over(ui, rect) && !pointer_on_pinned_header(ui)
}

/// Where the pinned header was drawn last frame, if one was (rows are laid out before it).
fn pinned_rect(ui: &egui::Ui) -> Option<Rect> {
    ui.data(|d| d.get_temp::<Option<Rect>>(egui::Id::unique("left-pinned-rect"))).flatten()
}

fn pointer_on_pinned_header(ui: &egui::Ui) -> bool {
    let pos = ui.input(|i| i.pointer.latest_pos());
    pinned_rect(ui).zip(pos).is_some_and(|(r, p)| r.contains(p))
}

/// How close to the top or bottom edge of the sidebar (points) a dragged album starts scrolling it.
const SCROLL_EDGE: f32 = 36.0;

/// The fastest the sidebar scrolls for a drag (points per second).
const SCROLL_MAX_SPEED: f32 = 700.0;

/// How fast the sidebar scrolls (points per second, positive = down) for a dragged item with the
/// pointer at `y`, the visible part being `top..bottom`: none in the middle, faster the nearer to
/// the edge, the top speed at and past it.
pub fn auto_scroll_speed(y: f32, top: f32, bottom: f32) -> f32 {
    let depth = |dist: f32| ((SCROLL_EDGE - dist) / SCROLL_EDGE).clamp(0.0, 1.0);
    SCROLL_MAX_SPEED * (depth(bottom - y) - depth(y - top))
}

/// While an album, or photos from the grid, are dragged over the sidebar: scrolls it when the
/// pointer is near its top or bottom edge, so rows beyond the visible part can be reached. Call
/// inside the scroll area.
pub(super) fn drag_auto_scroll(app: &LightcraftApp, ui: &egui::Ui) {
    if app.ui.dragging_album.is_none() && app.ui.dragging_photos.is_none() && app.ui.dragging_section.is_none() {
        return;
    }
    let view = ui.clip_rect();
    let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|p| p.x >= view.left() && p.x <= view.right()) else { return };
    // a section picked up by its pinned header starts inside the top edge: it scrolls once it has
    // left the header. Any other drag scrolls there as at any edge, to reach what is above
    let from_pinned = egui::Id::unique("left-section-drag-from-pinned");
    if app.ui.dragging_section.is_some() && ui.data(|d| d.get_temp::<bool>(from_pinned)) == Some(true) {
        if pointer_on_pinned_header(ui) {
            return;
        }
        ui.data_mut(|d| d.insert_temp(from_pinned, false));
    }
    let speed = auto_scroll_speed(p.y, view.top(), view.bottom());
    if speed != 0.0 {
        // a positive delta moves the content down: the view goes up
        ui.scroll_with_delta_animation(vec2(0.0, -speed * ui.input(|i| i.stable_dt).min(0.1)), egui::style::ScrollAnimation::none());
        ui.ctx().request_repaint();
    }
}

/// How long a dragged album must rest on a closed folder before it opens.
const HOVER_OPEN_SECS: f64 = 0.6;

/// Whether the album `dragged` may be dropped into the folder `target` (`None`: the top level):
/// a folder other than where it already is, and not itself or something inside it.
fn can_drop_album(app: &LightcraftApp, dragged: AlbumId, target: Option<AlbumId>) -> bool {
    let Some(d) = app.session.catalog.album(dragged) else { return false };
    match target {
        None => d.parent.is_some(),
        Some(t) => app.session.catalog.album(t).is_some_and(|f| f.folder) && d.parent != Some(t) && !is_within(app, t, dragged),
    }
}

/// Ends a drag that ran `result`: the folders down to the album open so it shows in its new place.
fn finish_drop(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, result: Result<serde_json::Value, String>) {
    forget_hover(app, ui.ctx());
    match result {
        Ok(_) => {
            app.ui.reveal_album = Some(dragged.0);
            ui.ctx().request_repaint();
        }
        Err(e) => app.toast(ui.ctx(), e),
    }
    app.ui.dragging_album = None;
}

/// Forgets how long the pointer rested on each folder (a new drag starts counting again).
fn forget_hover(app: &LightcraftApp, ctx: &egui::Context) {
    let ids: Vec<u64> = app.session.catalog.albums().filter(|a| a.folder).map(|a| a.id.0).collect();
    ctx.data_mut(|d| {
        for id in ids {
            d.remove_temp::<f64>(egui::Id::unique(("album-hover", id)));
        }
    });
}

/// Moves the dragged album into a folder (`album.move`, one undo step; `None`: the top level).
fn drop_album(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, target: Option<AlbumId>) {
    let r = app.run("album.move", json!({"id": dragged.0, "parent": target.map(|t| t.0)}));
    finish_drop(app, ui, dragged, r);
}

/// Places the dragged album in `parent` before the sibling `before` (`None`: last of its kind), by
/// hand (`album.reorder`, one undo step).
fn place_album(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, parent: Option<AlbumId>, before: Option<AlbumId>) {
    let r = app.run("album.reorder", json!({"id": dragged.0, "parent": parent.map(|p| p.0), "before": before.map(|b| b.0)}));
    finish_drop(app, ui, dragged, r);
}

/// What dropping the dragged album on a row would do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AlbumDrop {
    /// Into the folder.
    Into(AlbumId),
    /// Beside the row: in `parent`, before `before` (`None`: last of its kind); the line is drawn
    /// above the row (`above`) or below it.
    Place { parent: Option<AlbumId>, before: Option<AlbumId>, above: bool },
}

/// What dropping `dragged` at `frac` (0 = top edge, 1 = bottom edge) of the row of `target` does,
/// if anything. An album over a folder goes inside it; a folder over a folder goes inside by its
/// middle and beside it by its edges; an album over an album goes before it by its top half and
/// after it by its bottom half; a folder over an album does nothing. Placing where it already
/// is, or inside itself, is nothing too. `expanded`: `target` is a folder shown open.
fn album_drop_at(app: &LightcraftApp, dragged: AlbumId, target: &Album, frac: f32, expanded: bool) -> Option<AlbumDrop> {
    let cat = &app.session.catalog;
    let d = cat.album(dragged)?;
    let into = || can_drop_album(app, dragged, Some(target.id)).then_some(AlbumDrop::Into(target.id));
    if d.folder != target.folder {
        return if target.folder { into() } else { None };
    }
    if target.id == dragged {
        return None;
    }
    let after = frac >= if target.folder { 0.7 } else { 0.5 };
    // the middle of a folder, and the bottom of an open one (its first child is next), mean inside
    if target.folder && frac >= 0.3 && (!after || expanded) {
        return into();
    }
    let parent = target.parent;
    if parent.is_some_and(|p| is_within(app, p, dragged)) {
        return None;
    }
    let sibs = cat.album_children(parent);
    let at = sibs.iter().position(|a| a.id == target.id)?;
    let was = sibs.iter().position(|a| a.id == dragged);
    if !after {
        (was.map(|w| w + 1) != Some(at)).then_some(AlbumDrop::Place { parent, before: Some(target.id), above: true })
    } else {
        let next = sibs.iter().skip(at + 1).find(|a| a.id != dragged).filter(|a| a.folder == target.folder).map(|a| a.id);
        (was != Some(at + 1)).then_some(AlbumDrop::Place { parent, before: next, above: false })
    }
}

/// A row while an album is dragged: a folder outlined under the pointer when the drop would go
/// inside it, a line above or below the row when it would go beside it, and the move or placement
/// done on release. A closed folder with something inside (`open`) opens once the pointer rests on
/// it; returns whether it did.
fn album_drag_over(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect, row: &Album, open: Option<&mut bool>, depth: usize) -> bool {
    let hover_id = egui::Id::unique(("album-hover", row.id.0));
    let Some(dragged) = app.ui.dragging_album.map(AlbumId) else { return false };
    let pos = ui.input(|i| i.pointer.latest_pos()).filter(|_| pointer_over_row(ui, rect));
    let Some(pos) = pos else {
        if ui.data(|d| d.get_temp::<f64>(hover_id)).is_some() {
            ui.data_mut(|d| d.remove_temp::<f64>(hover_id));
        }
        return false;
    };
    let expanded = open.as_deref().copied().unwrap_or(false);
    let frac = ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    let drop = album_drop_at(app, dragged, row, frac, expanded);
    let mut opened = false;
    // only a drop that goes inside opens the folder: on its edges the rows would shift under the pointer
    let inside = matches!(drop, Some(AlbumDrop::Into(_)));
    if !inside && ui.data(|d| d.get_temp::<f64>(hover_id)).is_some() {
        ui.data_mut(|d| d.remove_temp::<f64>(hover_id));
    }
    if inside
        && let Some(open) = open
        && !*open
    {
        let now = ui.input(|i| i.time);
        let since = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(hover_id, || now));
        if now - since >= HOVER_OPEN_SECS {
            *open = true;
            opened = true;
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let t = Tokens::get(ui.ctx());
    let released = ui.input(|i| i.pointer.primary_released());
    match drop {
        Some(AlbumDrop::Into(folder)) => {
            ui.painter().rect_stroke(rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
            if released {
                drop_album(app, ui, dragged, Some(folder));
            }
        }
        Some(AlbumDrop::Place { parent, before, above }) => {
            let y = if above { rect.top() + 0.5 } else { rect.bottom() - 0.5 };
            let (x0, x1) = (rect.left() + 18.0 + depth as f32 * LEVEL, ui.clip_rect().right() - 8.0);
            ui.painter().circle_stroke(pos2(x0, y), 3.0, egui::Stroke::new(1.5, t.accent));
            ui.painter().line_segment([pos2(x0 + 3.0, y), pos2(x1.max(x0 + 3.0), y)], egui::Stroke::new(2.0, t.accent));
            if released {
                place_album(app, ui, dragged, parent, before);
            }
        }
        None => {}
    }
    opened
}

/// While an album that sits in a folder is dragged: the "Albums" header takes it back to the top
/// level (outlined, with a hint). The header stays in view however long the tree is, and nothing
/// moves when it appears.
pub(super) fn top_level_drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, header: Rect) {
    let Some(dragged) = app.ui.dragging_album.map(AlbumId) else { return };
    if !can_drop_album(app, dragged, None) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    register(ui, "albumDrop:top", header);
    let over = pointer_over(ui, header);
    let stroke = if over { egui::Stroke::new(1.5, t.accent) } else { egui::Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)) };
    ui.painter().rect_stroke(header.shrink2(vec2(8.0, 3.0)), 4.0, stroke, egui::StrokeKind::Inside);
    // right-aligned, left of the + button
    let edge = header.right().min(ui.clip_rect().right());
    ui.painter().text(pos2(edge - 56.0, header.center().y), Align2::RIGHT_CENTER, crate::i18n::tr("Top Level"), t.font(12.5), t.text_dim);
    if over && ui.input(|i| i.pointer.primary_released()) {
        drop_album(app, ui, dragged, None);
    }
}

/// While an album is dragged in the sidebar: its name at the pointer; the drag ends when the
/// button is up (the drop targets act on the release frame, before this runs).
pub fn album_drag_feedback(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(id) = app.ui.dragging_album else { return };
    let (released, down, pos, esc) =
        ctx.input(|i| (i.pointer.primary_released(), i.pointer.primary_down(), i.pointer.latest_pos(), i.key_pressed(egui::Key::Escape)));
    if released || !down || esc {
        app.ui.dragging_album = None;
        forget_hover(app, ctx);
        return;
    }
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    // (the framework's drag ghost names the album when the drag is its session's)
    if craft::ui::runtime::drag::active::<AlbumDrag>(ctx).is_some() {
        return;
    }
    let (Some(pos), Some(name)) = (pos, app.session.catalog.album(AlbumId(id)).map(|a| a.name.clone())) else { return };
    let t = Tokens::get(ctx);
    egui::Area::new(egui::Id::unique("drag-album")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(
        ctx,
        |ui| {
            egui::Frame::NONE.fill(t.accent).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
                ui.label(egui::RichText::new(name).color(egui::Color32::WHITE).font(t.semibold(12.0)));
            });
        },
    );
}

/// An album row while photos are dragged from the grid: highlighted under the pointer; a
/// release there adds them.
fn drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect, a: &Album) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    let over = pointer_over_row(ui, rect);
    if !over {
        return;
    }
    let released = ui.input(|i| i.pointer.primary_released());
    // albums hold library photos (issue #684): Local ones can't be dropped in; the row doesn't
    // light up, and a release says why
    let photos: Vec<lightcraft_catalog::PhotoId> = ids.iter().map(|id| lightcraft_catalog::PhotoId(*id)).collect();
    if crate::panels::grid::local_only(app, &photos) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        if released {
            app.toast(ui.ctx(), crate::panels::grid::albums_hint());
            app.ui.dragging_photos = None;
        }
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_stroke(rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    if released {
        drop_photos_on_album(app, ui, a, ids);
        app.ui.dragging_photos = None;
    }
}

/// Adds the dragged photos to the album and toasts how many went in (and how many Local ones were skipped).
fn drop_photos_on_album(app: &mut LightcraftApp, ui: &egui::Ui, a: &Album, ids: Vec<u64>) {
    match app.run("album.addPhotos", json!({"id": a.id.0, "ids": ids})) {
        Ok(r) => {
            let name = crate::destination::album(&app.session.catalog, a);
            app.toast(ui.ctx(), added_message(&name, &r));
        }
        Err(e) => app.toast(ui.ctx(), e),
    }
}

/// "Added N photos to “album”", plus a note when Local photos were skipped.
fn added_message(name: &str, r: &serde_json::Value) -> String {
    let n = r.get("added").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let skipped = r.get("skipped").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let mut msg = crate::i18n::tr_format!("Added {n} photo{} to “{}”", if n == 1 { "" } else { "s" }, name, n = n);
    if skipped > 0 {
        msg.push_str(" · ");
        msg.push_str(&skipped_note(skipped));
    }
    msg
}

fn skipped_note(skipped: u64) -> String {
    crate::i18n::tr_format!(
        "{n} Local photo{} skipped: albums hold library photos (Add to My Photos first)",
        if skipped == 1 { "" } else { "s" },
        n = skipped
    )
}
