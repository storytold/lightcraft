use super::folders::folder_tree;
use super::rows::Row;
use super::{Icon, LibrarySource, LightcraftApp, SidebarSection, json};
use crate::menu_item::MenuItem;

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
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let mut builtin = app.services.local_places.clone().unwrap_or_default();
    if app.services.local_places.is_none() && !home.is_empty() {
        for (name, sub) in [("Pictures", "Pictures"), ("Desktop", "Desktop"), ("Downloads", "Downloads"), ("Home", "")] {
            // joined with the platform's separator, like the paths browsing gives back
            let p = if sub.is_empty() { home.clone() } else { std::path::Path::new(&home).join(sub).to_string_lossy().to_string() };
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

/// Local's rows: each place as a folder tree, Browse Folder…, and the way back for hidden places.
pub(super) fn local_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, now: &LocalNow) {
    let (local, current) = (&now.places, now.current.as_deref());
    for (i, (name, path)) in local.places.iter().enumerate() {
        let transient = local.browse_root.as_deref() == Some(path.as_str());
        let reveal = local.owner == Some(i);
        folder_tree(app, ui, path, name, path, 0.0, current, reveal, transient);
    }
    if app.services.pick_folder.is_some() && Row::new("local:browse", Icon::Plus, crate::i18n::tr("Browse Folder…")).show(app, ui).clicked() {
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
    // the way back for hidden places: one comes back from the row itself; several are listed, so
    // each comes back on its own, or all at once (issue #2114: the row used to bring every one back)
    let hidden = now.hidden.len();
    if hidden > 0 {
        let label = crate::i18n::tr_format!("Show {hidden} hidden location{}", if hidden == 1 { "" } else { "s" }, hidden = hidden);
        let resp = Row::new("local:restoreHidden", Icon::Folder, &label)
            .show(app, ui)
            .on_hover_text(crate::i18n::tr("Put the locations you removed from Local back (no files change)"));
        if let [(_, only)] = now.hidden.as_slice() {
            if resp.clicked() {
                let _ = app.run("local.restoreHidden", json!({"path": only}));
            }
        } else {
            egui::Popup::menu(&resp).show(|ui| hidden_menu(app, ui, &now.hidden));
        }
    }
}

/// The list behind "Show N hidden locations": each place by name (its path on hover), then
/// Restore All.
fn hidden_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, hidden: &[(String, String)]) {
    for (name, path) in hidden {
        let r = MenuItem::new(name).widget(format!("local:restoreHidden:{path}")).show(ui).on_hover_text(path);
        if r.clicked() {
            let _ = app.run("local.restoreHidden", json!({"path": path}));
            ui.close();
        }
    }
    ui.separator();
    let r = MenuItem::new(crate::i18n::tr("Restore All")).widget("local:restoreHidden:all").show(ui);
    if r.clicked() {
        let _ = app.run("local.restoreHidden", json!({}));
        ui.close();
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

/// How many [`fs_cached`] answers for `ctx` are being worked out right now. Rows appear (and
/// the sidebar below them moves) when they land, so the headless driver counts them as pending
/// work and waits for them before acting on widget positions.
pub(crate) fn fs_cached_running(ctx: &egui::Context) -> usize {
    fs_running_counter(ctx).load(std::sync::atomic::Ordering::Acquire)
}

fn fs_running_counter(ctx: &egui::Context) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
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

    /// Every row of the "Show N hidden locations" list is a `MenuItem`: the first one, reached with
    /// Tab, has the menu row's focus ring over its whole row, and it is reachable by its widget name.
    #[test]
    fn the_hidden_locations_list_is_made_of_menu_item_rows() {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        let hidden =
            vec![("Pictures".to_string(), "/lightcraft-test/pictures".to_string()), ("Work".to_string(), "/lightcraft-test/work".to_string())];
        let id = "local:restoreHidden:/lightcraft-test/pictures";
        let mut view = crate::headless::HeadlessView::new();
        for frame in 0..4 {
            let tab = egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
            let events = if frame == 1 { vec![tab] } else { vec![] };
            let raw = crate::headless::HeadlessView::raw_input(egui::vec2(300.0, 300.0), 1.0, frame as f64 / 60.0, events);
            view.run(raw, |ui| super::hidden_menu(&mut app, ui, &hidden));
            let widgets = crate::widgets::take_registry(&view.ctx);
            let rect = widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not registered: {widgets:?}")).1;
            if frame == 3 {
                let accent = crate::theme::Tokens::get(&view.ctx).accent;
                let ringed = view
                    .painted_shapes()
                    .into_iter()
                    .any(|s| matches!(s, egui::epaint::Shape::Rect(r) if r.rect == rect && r.stroke.width == 1.5 && r.stroke.color == accent));
                assert!(ringed, "{id}: the focused row has no menu row focus ring");
            }
        }
    }

    #[test]
    fn local_folder_rows_outside_the_viewport_skip_interactive_widgets() {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        let mut view = crate::headless::HeadlessView::new();
        for frame in 0..3 {
            let raw = crate::headless::HeadlessView::raw_input(egui::vec2(300.0, 160.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for index in 0..500 {
                        let path = format!("/lightcraft-test/folder-{index:04}");
                        super::folder_tree(&mut app, ui, &path, &format!("Folder {index}"), &path, 0.0, None, false, false);
                    }
                });
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
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        let mut view = HeadlessView::new();
        let path = "/lightcraft-test/Pictures";
        let mut widgets = vec![];
        for frame in 0..3 {
            let raw = HeadlessView::raw_input(egui::vec2(300.0, 200.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                super::Row::new("all", super::Icon::Photos, "All Photos").show(&mut app, ui);
                super::folder_tree(&mut app, ui, path, "Pictures", path, 0.0, None, false, false);
            });
            widgets = crate::widgets::take_registry(&view.ctx);
        }
        let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not registered")).1;
        let (all, local) = (rect("label:all"), rect(&format!("label:local:{path}")));
        assert_eq!(local.left(), all.left(), "a top-level Local folder's name starts where All Photos' does");
        let triangle = rect(&format!("folderToggle:{path}"));
        // the icon is 16 points wide and ends 8 points before the name
        assert!(triangle.right() <= local.left() - 24.0, "triangle {triangle:?} overlaps the icon before {local:?}");
    }
}
