use std::collections::HashMap;

use super::folders::{library_node_for, move_folder_item, reveal_item, synchronize_item};
use super::rows::{MAX_DEPTH, MenuAt, SideEvent, SideRow, SideSpec, entry, host_menu, rule, set_tree_open, side_list, tree_open};
use super::{LibrarySource, LightcraftApp, SidebarSection, json};

/// Local's folders as they stand this frame, and the folder being browsed among them.
pub(super) struct LocalNow {
    pub(super) places: LocalPlaces,
    current: Option<String>,
    /// The places removed from Local, as (name, path), in the order they were hidden.
    hidden: Vec<(String, String)>,
}

/// Folders on this computer to browse without adding (Lightroom's Local): Pictures, Desktop,
/// Downloads, the home folder, the folder being browsed, and the folders kept with Browse
/// Folder…. Worked out every frame the sidebar shows, drawn or not: the row listed for this
/// session only (`local_browse_root`) follows the browsing. `None` where there is no Local (web).
pub(super) fn local_now(app: &mut LightcraftApp, ui: &egui::Ui) -> Option<LocalNow> {
    if !SidebarSection::Local.available() {
        return None;
    }
    let home = home_folder();
    let mut builtin = app.services.local_places.clone().unwrap_or_default();
    if app.services.local_places.is_none() {
        for (name, p) in default_places(&home) {
            // (checked off the UI thread: a home folder can be on a network share)
            if fs_cached(ui, "is-dir", &p, 5.0, |p| std::path::Path::new(p).is_dir()) == Some(true) {
                builtin.push((crate::i18n::tr(name).to_string(), p));
            }
        }
    }
    let browsing = app.session.browse.clone().filter(|_| app.session.source == LibrarySource::Folder);
    let current = browsing.as_ref().map(|b| b.path.clone());
    // a hidden built-in place keeps its name (Home, not the user's folder name)
    let name_of = |path: &str| builtin.iter().find(|(_, p)| same_folder(p, path)).map_or_else(|| folder_label(path), |(n, _)| n.clone());
    let hidden = app.ui.hidden_locations.iter().map(|h| (name_of(h), h.clone())).collect();
    let unlisted = current.as_deref().and_then(|path| fs_cached(ui, "unlisted-ancestors", path, 2.0, unlisted_ancestors));
    let mut places = local_places_with_unlisted(
        builtin,
        &app.ui.local_roots,
        current.as_deref(),
        app.ui.local_browse_root.as_deref(),
        &app.ui.hidden_locations,
        unlisted.as_deref().unwrap_or(&[]),
    );
    if current.is_some() && unlisted.is_none() {
        places.owner = None;
    }
    if current.is_some() && unlisted.is_some() {
        app.ui.local_browse_root = places.browse_root.clone();
    }
    Some(LocalNow { places, current, hidden })
}

/// The user's home folder, the engine's own reading of it (so the menu and the rename/move guard
/// agree); empty when the host has none.
fn home_folder() -> String {
    lightcraft_engine::library::home_folder().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Local's built-in places under `home` as (English name, path), in order. The names are keys for
/// `tr`, never folder names: the folders on disk keep their own (Pictures, not "Bilder").
fn default_places(home: &str) -> Vec<(&'static str, String)> {
    if home.is_empty() {
        return Vec::new();
    }
    [("Pictures", "Pictures"), ("Desktop", "Desktop"), ("Downloads", "Downloads"), ("Home", "")]
        .into_iter()
        // joined with the platform's separator, like the paths browsing gives back
        .map(|(name, sub)| (name, if sub.is_empty() { home.to_string() } else { std::path::Path::new(home).join(sub).to_string_lossy().to_string() }))
        .collect()
}

/// Whether `path` is one of Local's built-in places (the home folder, Pictures, Desktop,
/// Downloads, or a place the host listed itself). They are the computer's own folders, shown under
/// a translated label, so Local offers no Rename or Move for them.
pub(super) fn is_builtin_place(app: &LightcraftApp, path: &str) -> bool {
    builtin_place_in(app.services.local_places.as_deref(), &home_folder(), path)
}

/// [`is_builtin_place`] for the places the host listed (`injected`, which replace the defaults) or,
/// without them, the ones under `home`.
fn builtin_place_in(injected: Option<&[(String, String)]>, home: &str, path: &str) -> bool {
    match injected {
        Some(places) => places.iter().any(|(_, p)| same_folder(p, path)),
        None => default_places(home).iter().any(|(_, p)| same_folder(p, path)),
    }
}

/// What a Local row stands for, by the row's key.
struct Meta {
    path: String,
    name: String,
    depth: usize,
    transient: bool,
    /// The place the row's tree hangs from (the same folder can be listed in more than one tree).
    place: String,
}

/// The key of the Browse Folder… row, and of the one that brings hidden places back.
const BROWSE: &str = "local:browse";
const RESTORE: &str = "local:restoreHidden";

fn open_key(place: &str, path: &str) -> egui::Id {
    egui::Id::unique(("folder-open", place.to_string(), path.to_string()))
}

/// A folder on disk with a disclosure triangle: click browses it, the triangle lists its
/// subfolders. In the tree that holds the folder being browsed (`reveal`), the folders on the
/// way down to it open whenever the browsed folder changes, so it shows highlighted in place.
/// `transient`: a top-level row listed for this session only (it offers Keep in Local).
/// `place`: the top-level folder this tree hangs from. A folder can be listed in more than one
/// tree (Pictures on its own and inside Home): each row opens and closes by itself, and has a key
/// of its own (#1093).
#[allow(clippy::too_many_arguments)]
fn flatten(
    ui: &egui::Ui,
    place: &str,
    name: &str,
    path: &str,
    depth: usize,
    current: Option<&str>,
    reveal: bool,
    transient: bool,
    rows: &mut Vec<SideRow>,
    meta: &mut HashMap<String, Meta>,
) {
    let open_id = open_key(place, path);
    let sel = current.is_some_and(|c| same_folder(c, path));
    let on_the_way = !sel && current.is_some_and(|c| lightcraft_catalog::query::folder_within(c, path));
    let mut open = tree_open(ui, open_id, false);
    if reveal && on_the_way {
        // opened once per browsed folder: collapsing it again afterwards sticks
        let revealed_id = egui::Id::unique(("folder-revealed", place.to_string(), path.to_string()));
        let target = current.map(str::to_string);
        if ui.data(|d| d.get_temp::<Option<String>>(revealed_id)) != Some(target.clone()) {
            open = true;
            set_tree_open(ui, open_id, true);
            ui.data_mut(|d| d.insert_temp(revealed_id, target));
        }
    }
    let key = format!("local:{place}\u{1f}{path}");
    let id = format!("local:{path}");
    rows.push(SideRow::new(id, "folder", name).key(key.clone()).depth(depth).group(open, format!("folderToggle:{path}")).tip(path));
    meta.insert(key, Meta { path: path.to_string(), name: name.to_string(), depth, transient, place: place.to_string() });
    if open && depth < MAX_DEPTH.min(8) {
        for (n, p) in subfolders(ui, path).iter() {
            flatten(ui, place, n, p, depth + 1, current, reveal, false, rows, meta);
        }
    }
}

/// Local's rows: each place as a folder tree, Browse Folder…, and the way back for hidden places.
pub(super) fn local_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, now: &LocalNow) {
    let (local, current) = (&now.places, now.current.as_deref());
    let mut rows = Vec::new();
    let mut meta = HashMap::new();
    for (i, (name, path)) in local.places.iter().enumerate() {
        let transient = local.browse_root.as_deref() == Some(path.as_str());
        let reveal = local.owner == Some(i);
        flatten(ui, path, name, path, 0, current, reveal, transient, &mut rows, &mut meta);
    }
    let browse = app.services.pick_folder.is_some();
    if browse {
        rows.push(SideRow::new("local:browse", "plus", crate::i18n::tr("Browse Folder…")).key(BROWSE));
    }
    // the way back for hidden places: one comes back from the row itself; several are listed, so
    // each comes back on its own, or all at once (issue #2114: the row used to bring every one back)
    let hidden = now.hidden.len();
    if hidden > 0 {
        let label = crate::i18n::tr_format!("Show {hidden} hidden location{}", if hidden == 1 { "" } else { "s" }, hidden = hidden);
        rows.push(
            SideRow::new("local:restoreHidden", "folder", label)
                .key(RESTORE)
                .tip(crate::i18n::tr("Put the locations you removed from Local back (no files change)")),
        );
    }
    let selected = meta.iter().find(|(_, m)| current.is_some_and(|c| same_folder(c, &m.path))).map(|(key, _)| key.clone());
    let spec = SideSpec::new("local", crate::i18n::tr("Local"), app.ui.show_counts).selected(selected.as_deref());
    let out = side_list(ui, spec, rows);
    let mut request = None;
    for event in &out.events {
        match event {
            SideEvent::Open { key, open } => {
                if let Some(m) = meta.get(key) {
                    set_tree_open(ui, open_key(&m.place, &m.path), *open);
                }
            }
            SideEvent::Activate(key) if key == BROWSE => {
                let picked = app.services.pick_folder.as_mut().and_then(|f| f());
                if let Some(path) = picked {
                    match app.run("library.browse", json!({"path": path})) {
                        // the picked folder stays in Local (and comes back if it was hidden)
                        Ok(r) => {
                            let dir = r["path"].as_str().unwrap_or(&path).to_string();
                            let _ = app.run("local.addRoot", json!({"path": dir}));
                        }
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
            SideEvent::Activate(key) if key == RESTORE => {
                if let [(_, only)] = now.hidden.as_slice() {
                    let _ = app.run("local.restoreHidden", json!({"path": only}));
                } else if let Some(rect) = out.rect_of(RESTORE) {
                    request = Some((key.clone(), MenuAt::Rect(rect)));
                }
            }
            SideEvent::Activate(key) => {
                if let Some(m) = meta.get(key)
                    && let Err(e) = app.run("library.browse", json!({"path": m.path}))
                {
                    app.toast(ui.ctx(), e);
                }
            }
            SideEvent::Menu { key, at } => {
                if meta.contains_key(key) {
                    request = Some((key.clone(), MenuAt::Point(*at)));
                }
            }
        }
    }
    host_menu(ui, "local", crate::i18n::tr("Local"), out.response.as_ref(), request, |ui, key| {
        if key == RESTORE {
            hidden_menu(app, ui, &now.hidden);
        } else if let Some(m) = meta.get(key) {
            folder_menu(app, ui, m);
        }
    });
}

/// The context menu of a Local folder row.
fn folder_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &Meta) {
    let (path, name, top) = (m.path.as_str(), m.name.as_str(), m.depth == 0);
    // the library actions of the Folders tree, when this folder (or one inside it) holds library photos
    let library = app.caches.folder_tree(&app.session.catalog);
    let held = library_node_for(&library, path);
    if (m.transient || !top)
        && entry("local", crate::i18n::tr(if m.transient { "Keep in Local" } else { "Add to Local" }))
            .tip(crate::i18n::tr("List this folder in Local from now on"))
            .show(ui)
    {
        let _ = app.run("local.addRoot", json!({"path": path}));
    }
    if top
        && entry("local", crate::i18n::tr("Remove from Local"))
            .tip(crate::i18n::tr("Hides this shortcut only; the folder and its photos stay as they are"))
            .show(ui)
    {
        let _ = app.run("local.hide", json!({"path": path}));
    }
    // a built-in place is one of the computer's own folders under a translated label (Home is
    // the user's whole home): Local offers no Rename or Move for it (the other items stay)
    if !(top && is_builtin_place(app, path)) {
        if entry("local", crate::i18n::tr("Rename Folder…")).show(ui) {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: crate::i18n::tr_format!("Rename “{name}”", name = name),
                hint: "Folder name (renamed on disk; its photos follow)".into(),
                value: name.to_string(),
                command: "folder.rename".into(),
                params: json!({"path": path}),
                key: "name".into(),
            });
        }
        move_folder_item(app, ui, path);
    }
    reveal_item(app, ui, path);
    if let Some(held) = &held {
        synchronize_item(app, ui, &held.0, &held.1);
    }
}

/// The list behind "Show N hidden locations": each place by name (its path on hover), then
/// Restore All.
fn hidden_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, hidden: &[(String, String)]) {
    for (name, path) in hidden {
        if entry(path, name).tip(path).widget(format!("local:restoreHidden:{path}")).show(ui) {
            let _ = app.run("local.restoreHidden", json!({"path": path}));
        }
    }
    rule(ui);
    if entry("all", crate::i18n::tr("Restore All")).widget("local:restoreHidden:all").show(ui) {
        let _ = app.run("local.restoreHidden", json!({}));
    }
}

/// Whether two paths name the same folder, however they are spelled (separators, trailing
/// slash, `.`/`..`, drive-letter case; see `lightcraft_catalog::query::folder_key`).
pub(crate) fn same_folder(a: &str, b: &str) -> bool {
    a == b || lightcraft_catalog::query::folder_key(a) == lightcraft_catalog::query::folder_key(b)
}

/// Local's top-level folders and how the folder being browsed sits among them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LocalPlaces {
    /// (label, path) of each top-level folder, in order.
    pub places: Vec<(String, String)>,
    /// The top-level folder the browsed folder lies in (the innermost one): its tree opens on
    /// the way down to it. None when that way passes through a hidden folder.
    pub owner: Option<usize>,
    /// A folder listed only for this session because the browsed folder is in no saved
    /// location (browsed from a breadcrumb, the CLI…); it stays while browsing below it.
    pub browse_root: Option<String>,
}

fn folder_label(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Local's top-level folders, in order: the built-in places, the folders kept with Browse
/// Folder… / Keep in Local (`saved`), then — only when the browsed folder lies in none of those —
/// a session root for it: the previous one (`browse_root`) while browsing stays below it, else
/// the browsed folder itself. A folder inside a listed one is shown inside that one's tree (the
/// root stays; its siblings stay reachable), never as a root of its own. Each folder is listed
/// once however its path is spelled (the first spelling wins), and hidden ones are left out —
/// they stay reachable through breadcrumbs and Browse Folder….
#[cfg(test)]
fn local_places(
    builtin: Vec<(String, String)>,
    saved: &[String],
    browsing: Option<&str>,
    browse_root: Option<&str>,
    hidden: &[String],
) -> LocalPlaces {
    local_places_with_unlisted(builtin, saved, browsing, browse_root, hidden, &[])
}

fn local_places_with_unlisted(
    builtin: Vec<(String, String)>,
    saved: &[String],
    browsing: Option<&str>,
    browse_root: Option<&str>,
    hidden: &[String],
    unlisted: &[String],
) -> LocalPlaces {
    use lightcraft_catalog::query::{folder_key, folder_within};
    let is_hidden = |p: &str| hidden.iter().any(|h| same_folder(h, p));
    let listed_below = |current: &str, root: &str| {
        folder_within(current, root) && !unlisted.iter().any(|h| !same_folder(h, root) && folder_within(current, h) && folder_within(h, root))
    };
    let mut places = builtin;
    for path in saved {
        if !places.iter().any(|(_, p)| same_folder(p, path)) {
            places.push((folder_label(path), path.clone()));
        }
    }
    places.retain(|(_, p)| !is_hidden(p));
    let mut out = LocalPlaces::default();
    if let Some(c) = browsing
        && !places.iter().any(|(_, p)| listed_below(c, p))
    {
        let root = browse_root.filter(|r| listed_below(c, r)).unwrap_or(c);
        if !is_hidden(root) {
            places.push((folder_label(root), root.to_string()));
            out.browse_root = Some(root.to_string());
        }
    }
    if let Some(c) = browsing {
        // A tree never opens on the way down through a hidden folder: hiding a kept folder
        // beneath Home would otherwise reveal it again inside Home's (possibly huge) tree.
        let through_hidden = |p: &str| hidden.iter().any(|h| folder_within(c, h) && folder_within(h, p));
        out.owner = places
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| listed_below(c, p) && !through_hidden(p))
            .max_by_key(|(_, (_, p))| folder_key(p).len())
            .map(|(i, _)| i);
    }
    out.places = places;
    out
}

fn unlisted_ancestors(path: &str) -> Vec<String> {
    std::path::Path::new(path).ancestors().filter(|p| lightcraft_engine::walk::is_hidden_path(p)).map(|p| p.to_string_lossy().to_string()).collect()
}

/// The subfolders of `path` (not hidden ones), sorted; listed on a worker thread at most every
/// 2 s per folder (empty until the first listing).
pub(super) fn subfolders(ui: &egui::Ui, path: &str) -> std::sync::Arc<Vec<(String, String)>> {
    fs_cached(ui, "subfolders", path, 2.0, list_subfolders).unwrap_or_default()
}

fn list_subfolders(path: &str) -> std::sync::Arc<Vec<(String, String)>> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !lightcraft_engine::walk::is_hidden_entry(e))
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    (name, e.path().to_string_lossy().to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(n, _)| n.to_lowercase());
    v.into()
}

/// How many file-system answers for `ctx` are being worked out right now: [`fs_cached`]'s and the
/// Missing Photos count's. Rows appear (and the sidebar below them moves) when they land, so the
/// headless driver counts them as pending work and waits for them before acting on widget
/// positions.
pub(crate) fn fs_cached_running(ctx: &egui::Context) -> usize {
    fs_running_counter(ctx).load(std::sync::atomic::Ordering::Acquire)
}

/// The counter behind [`fs_cached_running`]: a worker adds one before it starts and takes it off
/// after its answer is stored.
pub(super) fn fs_running_counter(ctx: &egui::Context) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<std::sync::Arc<std::sync::atomic::AtomicUsize>>(egui::Id::unique("fs-cached-running")).clone())
}

/// A file-system answer for `path` (`f(path)`), kept per `kind` and path and refreshed on a worker
/// thread at most every `every` seconds: a folder on a sleeping NAS, a dropped share or a
/// spinning-up drive never blocks a frame. `None` until the first answer arrives.
pub(crate) fn fs_cached<T: Clone + Send + 'static>(ui: &egui::Ui, kind: &'static str, path: &str, every: f64, f: fn(&str) -> T) -> Option<T> {
    struct Entry<T> {
        value: Option<T>,
        at: Option<f64>,
        running: bool,
    }
    type Cell<T> = std::sync::Arc<std::sync::Mutex<Entry<T>>>;
    let id = egui::Id::unique(("fs-cached", kind, path.to_string()));
    let now = ui.input(|i| i.time);
    let cell: Cell<T> = match ui.data(|d| d.get_temp::<Cell<T>>(id)) {
        Some(c) => c,
        None => {
            let c: Cell<T> = std::sync::Arc::new(std::sync::Mutex::new(Entry { value: None, at: None, running: false }));
            ui.data_mut(|d| d.insert_temp(id, c.clone()));
            c
        }
    };
    let lock = |c: &Cell<T>| c.lock().unwrap_or_else(std::sync::PoisonError::into_inner).value.clone();
    let start = {
        let mut e = cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = !e.running && e.at.is_none_or(|at| now - at >= every || now < at);
        if due {
            e.running = true;
            e.at = Some(now);
        }
        due
    };
    if start {
        use std::sync::atomic::Ordering;
        let running = fs_running_counter(ui.ctx());
        running.fetch_add(1, Ordering::AcqRel);
        let (out, path, repaint, done) = (cell.clone(), path.to_string(), ui.ctx().clone(), running.clone());
        let work = move || {
            let v = f(&path);
            let mut e = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            e.value = Some(v);
            e.running = false;
            drop(e);
            // after the answer is stored: a frame that sees the count drop also sees the answer
            done.fetch_sub(1, Ordering::AcqRel);
            repaint.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        if std::thread::Builder::new().name("lc-fs-list".into()).spawn(work).is_err() {
            cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).running = false;
            running.fetch_sub(1, Ordering::AcqRel);
        }
        #[cfg(target_arch = "wasm32")]
        work();
    }
    lock(&cell)
}

#[cfg(test)]
mod tests {
    use super::local_places;
    use crate::widgets::elide_head;

    #[test]
    fn the_standard_places_under_a_home_are_built_in_and_nothing_else_is() {
        let home = "/home/example";
        for path in [home, "/home/example/", "/home/example/Pictures", "/home/example/Pictures/", "/home/example/Desktop", "/home/example/Downloads"]
        {
            assert!(super::builtin_place_in(None, home, path), "{path}");
        }
        for path in ["/home/example/Pictures/Trip", "/home/example/Documents", "/home/other", "/home", "Pictures"] {
            assert!(!super::builtin_place_in(None, home, path), "{path}");
        }
        // no home: no standard places, and no relative ones
        assert!(!super::builtin_place_in(None, "", "Pictures") && !super::builtin_place_in(None, "", ""));
        // places the host listed replace the standard ones
        let injected = vec![("Sandbox".to_string(), "/lightcraft-test/home".to_string())];
        assert!(super::builtin_place_in(Some(&injected), home, "/lightcraft-test/home"));
        assert!(!super::builtin_place_in(Some(&injected), home, "/home/example/Pictures"));
    }

    #[test]
    fn injected_places_replace_the_machine_home() {
        let places = vec![("Sandbox".to_string(), "/lightcraft-test/home".to_string())];
        let services = crate::Services { local_places: Some(places.clone()), ..Default::default() };
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), services);
        let mut view = crate::headless::HeadlessView::new();
        let raw = crate::headless::HeadlessView::raw_input(egui::vec2(300.0, 200.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            assert_eq!(super::local_now(&mut app, ui).unwrap().places.places, places);
        });
    }

    #[test]
    fn local_folder_rows_outside_the_viewport_skip_interactive_widgets() {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        let mut view = crate::headless::HeadlessView::new();
        let places = (0..500).map(|index| (format!("Folder {index}"), format!("/lightcraft-test/folder-{index:04}"))).collect();
        let now = super::LocalNow { places: super::LocalPlaces { places, owner: None, browse_root: None }, current: None, hidden: Vec::new() };
        for frame in 0..3 {
            let raw = crate::headless::HeadlessView::raw_input(egui::vec2(300.0, 160.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| super::local_rows(&mut app, ui, &now));
            });
            let widgets = crate::widgets::take_registry(&view.ctx);
            if frame == 2 {
                let rows = widgets.iter().filter(|(id, _)| id.starts_with("source:local:")).count();
                let toggles = widgets.iter().filter(|(id, _)| id.starts_with("folderToggle:")).count();
                assert_eq!(rows, 500, "offscreen rows remain available for scrolling");
                assert!((1..=10).contains(&toggles), "only visible rows build interactive widgets: {toggles}");
            }
        }
    }

    #[test]
    fn browsing_below_an_unlisted_folder_has_a_session_root() {
        let home = "/home/example";
        let hidden = format!("{home}/.hidden");
        let current = format!("{hidden}/Photos");
        let builtin = || vec![("Home".into(), home.into())];
        let unlisted = vec![hidden];
        let first = super::local_places_with_unlisted(builtin(), &[], Some(&current), None, &[], &unlisted);
        assert_eq!(first.browse_root.as_deref(), Some(current.as_str()));
        assert_eq!(first.owner, Some(1));
        let child = format!("{current}/Trip");
        let next = super::local_places_with_unlisted(builtin(), &[], Some(&child), first.browse_root.as_deref(), &[], &unlisted);
        assert_eq!(next.browse_root, first.browse_root);
        assert_eq!(next.owner, Some(1));
        let kept = super::local_places_with_unlisted(builtin(), std::slice::from_ref(&current), Some(&child), None, &[], &unlisted);
        assert_eq!(kept.browse_root, None);
        assert_eq!(kept.owner, Some(1));
        let deeper = format!("{current}/.private");
        let inside = format!("{deeper}/Trip");
        let unlisted = vec![unlisted[0].clone(), deeper];
        let moved = super::local_places_with_unlisted(builtin(), &[], Some(&inside), first.browse_root.as_deref(), &[], &unlisted);
        assert_eq!(moved.browse_root.as_deref(), Some(inside.as_str()));
        assert_eq!(moved.owner, Some(1));
    }

    #[test]
    fn subfolders_leave_out_hidden_folders() {
        let dir = std::env::temp_dir().join(format!("lc-ui-hidden-subfolders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["Shown", ".dot", "Hidden"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        #[cfg(windows)]
        assert!(std::process::Command::new("attrib").arg("+h").arg(dir.join("Hidden")).status().unwrap().success());
        #[cfg(target_os = "macos")]
        assert!(std::process::Command::new("chflags").arg("hidden").arg(dir.join("Hidden")).status().unwrap().success());
        let expected = if cfg!(any(windows, target_os = "macos")) { vec!["Shown"] } else { vec!["Hidden", "Shown"] };
        let listed = super::list_subfolders(&dir.to_string_lossy());
        assert_eq!(listed.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), expected);
        let unlisted = super::unlisted_ancestors(&dir.join(".dot/Photos").to_string_lossy());
        assert!(unlisted.contains(&dir.join(".dot").to_string_lossy().to_string()));
        if cfg!(any(windows, target_os = "macos")) {
            let unlisted = super::unlisted_ancestors(&dir.join("Hidden/Photos").to_string_lossy());
            assert!(unlisted.contains(&dir.join("Hidden").to_string_lossy().to_string()));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Width = characters, so a limit of 12 is "12 characters".
    fn fit(text: &str, max: usize) -> String {
        elide_head(text, max as f32, |s| s.chars().count() as f32)
    }

    #[test]
    fn a_label_that_fits_is_left_alone() {
        assert_eq!(fit("/Users/me/Pictures", 18), "/Users/me/Pictures");
    }

    #[test]
    fn a_long_path_loses_its_leading_folders_not_its_end() {
        assert_eq!(fit("/Users/me/Pictures/Lightroom", 20), "…/Pictures/Lightroom");
        assert_eq!(fit("/Users/me/Pictures/Lightroom", 12), "…/Lightroom");
    }

    #[test]
    fn a_single_name_too_long_is_cut_at_the_end() {
        assert_eq!(fit("2024-summer-holiday", 8), "2024-su…");
        assert_eq!(fit("2024-summer-holiday", 1), "…");
        assert_eq!(fit("2024-summer-holiday", 0), "…");
    }

    #[test]
    fn multibyte_names_are_cut_on_character_boundaries() {
        assert_eq!(fit("/写真/夏休み旅行の記録", 6), "夏休み旅行…");
    }

    fn names(v: &[(String, String)]) -> Vec<&str> {
        v.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// A built-in location browsed (or picked) under another spelling of its path is the same
    /// row, not a second root — for every built-in, not just named ones.
    #[test]
    fn equivalent_paths_are_one_local_location() {
        for (home, sep) in [("D:\\Example", '\\'), ("D:/Example", '/'), ("/home/example", '/')] {
            let builtin: Vec<(String, String)> =
                ["Pictures", "Desktop", "Downloads"].iter().map(|s| (s.to_string(), format!("{home}{sep}{s}"))).collect();
            for (_, p) in builtin.clone() {
                // the browsed spelling: other separators, a trailing one, a different drive-letter case
                let flipped = p.replace(['/', '\\'], if sep == '/' { "\\" } else { "/" });
                for browsing in [flipped.clone(), format!("{p}{sep}"), p.replacen("D:", "d:", 1)] {
                    let l = local_places(builtin.clone(), std::slice::from_ref(&browsing), Some(&browsing), None, &[]);
                    assert_eq!(names(&l.places), ["Pictures", "Desktop", "Downloads"], "{p} browsed as {browsing}");
                    assert_eq!(l.places[l.owner.unwrap()].1, p, "the built-in row is the one highlighted");
                }
                // hiding under one spelling hides the other
                let l = local_places(builtin.clone(), &[], None, None, std::slice::from_ref(&flipped));
                assert_eq!(l.places.len(), 2, "hidden {flipped}");
            }
        }
        // a different folder is still added
        let l = local_places(vec![("Pictures".into(), "/home/example/Pictures".into())], &[], Some("/home/example/Pictures2"), None, &[]);
        assert_eq!(names(&l.places), ["Pictures", "Pictures2"]);
    }

    /// A folder inside a kept root is shown inside that root's tree: the root stays (its other
    /// subfolders stay reachable) and no second root appears; other kept roots stay too.
    #[test]
    fn browsing_below_a_kept_root_keeps_the_root() {
        let photos = "/data/Photos".to_string();
        let other = "/data/Scans".to_string();
        let saved = [photos.clone(), other.clone()];
        let builtin = || vec![("Home".to_string(), "/home/example".to_string())];
        for browsing in ["/data/Photos/2026/20260101", "/data/Photos/2026", "/data/Photos", "D:\\x"] {
            let l = local_places(builtin(), &saved, Some(browsing), None, &[]);
            let roots: Vec<&str> = l.places.iter().map(|(_, p)| p.as_str()).collect();
            if browsing.starts_with("/data") {
                assert_eq!(roots, ["/home/example", "/data/Photos", "/data/Scans"], "{browsing}");
                assert_eq!(l.owner, Some(1), "the Photos tree opens down to {browsing}");
                assert_eq!(l.browse_root, None);
            } else {
                assert_eq!(roots, ["/home/example", "/data/Photos", "/data/Scans", "D:\\x"], "a folder outside them gets a row");
            }
        }
        // the innermost containing root is the one that opens
        let l = local_places(builtin(), &["/home/example/Pictures".into()], Some("/home/example/Pictures/Trip"), None, &[]);
        assert_eq!(l.owner, Some(1));
    }

    /// A browsed folder outside every kept root gets a session row, which stays while browsing
    /// below it and gives way when browsing moves elsewhere.
    #[test]
    fn session_root_stays_while_browsing_below_it() {
        let l = local_places(Vec::new(), &[], Some("/t/base"), None, &[]);
        assert_eq!(l.browse_root.as_deref(), Some("/t/base"));
        let l = local_places(Vec::new(), &[], Some("/t/base/Trip/Day 1"), l.browse_root.as_deref(), &[]);
        assert_eq!((l.places.len(), l.browse_root.as_deref(), l.owner), (1, Some("/t/base"), Some(0)), "{l:?}");
        let l = local_places(Vec::new(), &[], Some("/elsewhere"), l.browse_root.as_deref(), &[]);
        assert_eq!(l.browse_root.as_deref(), Some("/elsewhere"));
        // hidden: no row at all
        let l = local_places(Vec::new(), &[], Some("/t/base"), None, &["/t/base/".into()]);
        assert!(l.places.is_empty() && l.browse_root.is_none());
    }

    /// A hidden folder inside a listed one (a kept folder beneath Home) is not revealed in that
    /// one's tree while it is browsed, nor are the folders below it; hiding Home itself still
    /// lets Pictures open down to a folder browsed inside it.
    #[test]
    fn hidden_folder_is_not_revealed_in_an_outer_tree() {
        let builtin = || vec![("Pictures".to_string(), "/home/example/Pictures".to_string()), ("Home".to_string(), "/home/example".to_string())];
        let kept = ["/home/example/AppData/Temp/lc".to_string()];
        let l = local_places(builtin(), &kept, Some("/home/example/AppData/Temp/lc"), None, &[]);
        assert_eq!(l.owner, Some(2), "shown as its own kept row");
        for browsing in ["/home/example/AppData/Temp/lc", "/home/example/AppData/Temp/lc/Day 1"] {
            let l = local_places(builtin(), &kept, Some(browsing), None, &["/home/example/AppData/Temp/lc/".into()]);
            assert_eq!(names(&l.places), ["Pictures", "Home"], "{browsing}");
            assert_eq!((l.owner, l.browse_root.as_deref()), (None, None), "Home does not open down to {browsing}");
        }
        let l = local_places(builtin(), &[], Some("/home/example/Pictures/Trip"), None, &["/home/example".into()]);
        assert_eq!((names(&l.places), l.owner), (vec!["Pictures"], Some(0)));
    }

    #[test]
    fn a_local_folder_lines_up_with_the_rows_above_and_its_triangle_stays_left_of_the_icon() {
        use crate::Services;
        use crate::headless::HeadlessView;
        use crate::panels::left::rows::{SideRow, SideSpec, side_list};
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        let mut view = HeadlessView::new();
        let path = "/lightcraft-test/Pictures";
        let now = super::LocalNow {
            places: super::LocalPlaces { places: vec![("Pictures".into(), path.into())], owner: None, browse_root: None },
            current: None,
            hidden: Vec::new(),
        };
        let mut widgets = vec![];
        for frame in 0..3 {
            let raw = HeadlessView::raw_input(egui::vec2(300.0, 200.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                side_list(ui, SideSpec::new("test-sources", "Sources", true).flat(), vec![SideRow::new("all", "image", "All Photos")]);
                super::local_rows(&mut app, ui, &now);
            });
            widgets = crate::widgets::take_registry(&view.ctx);
        }
        let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not registered")).1;
        let (all, local) = (rect("label:all"), rect(&format!("label:local:{path}")));
        assert_eq!(local.left(), all.left(), "a top-level Local folder's name starts where All Photos' does");
        let triangle = rect(&format!("folderToggle:{path}"));
        let row = rect(&format!("source:local:{path}"));
        // the triangle is the row's first part, in the gutter before the icon and the name
        assert!(triangle.left() >= row.left() && triangle.center().x < local.left() + 24.0, "triangle {triangle:?} in {row:?} before {local:?}");
    }
}
