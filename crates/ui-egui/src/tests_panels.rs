//! Headless tests of the side panels: dragging their inner edge resizes them within limits, and
//! the chosen widths survive switching panels and a save/load of the UI state (issue #20).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{LEFT_WIDTH, MIN_PHOTO_WIDTH, RIGHT_WIDTH};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Drag horizontally from `x` by `dx` (within the 1400 pt window) at mid height.
fn drag(h: &mut Headless, x: f32, dx: f32) {
    let to = (x + dx).clamp(1.0, 1399.0);
    let r = h.request("ui.drag", json!({"x": x, "y": 500.0, "toX": to, "toY": 500.0, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

#[test]
fn side_panels_resize_by_their_inner_edge_and_remember_it() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail", "right": "edit", "leftPanel": true}));
    let right = widget(&h, "panel:right_panel");
    let left = widget(&h, "panel:left_panel");
    assert_eq!((left.width(), right.width()), (LEFT_WIDTH.default, RIGHT_WIDTH.default));
    // the right panel's left edge: 120 pt wider
    drag(&mut h, right.left() + 2.0, -120.0);
    assert!((h.app.ui.right_width - (RIGHT_WIDTH.default + 120.0)).abs() <= 2.0, "{}", h.app.ui.right_width);
    assert!((widget(&h, "panel:right_panel").width() - h.app.ui.right_width).abs() <= 1.0);
    // the left sidebar's right edge: 70 pt wider
    drag(&mut h, left.right() - 2.0, 70.0);
    assert!((h.app.ui.left_width - (LEFT_WIDTH.default + 70.0)).abs() <= 2.0, "{}", h.app.ui.left_width);
    // kept across panel switches and view changes
    let (lw, rw) = (h.app.ui.left_width, h.app.ui.right_width);
    for set in [json!({"right": "info"}), json!({"view": "photoGrid"}), json!({"right": "masking", "view": "detail"})] {
        h.request("ui.set", set, T);
        h.step();
        assert_eq!((widget(&h, "panel:left_panel").width(), widget(&h, "panel:right_panel").width()), (lw, rw));
    }
    // and across a save/load of the UI state (ui.json)
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!((back.left_width, back.right_width), (lw, rw));
    // limits: the photo area stays usable however far the edges are dragged…
    let right = widget(&h, "panel:right_panel");
    drag(&mut h, right.left() + 2.0, -1400.0);
    let left = widget(&h, "panel:left_panel");
    drag(&mut h, left.right() - 2.0, 1400.0);
    h.step();
    let (left, right) = (widget(&h, "panel:left_panel"), widget(&h, "panel:right_panel"));
    assert!(right.width() <= RIGHT_WIDTH.max && left.width() <= LEFT_WIDTH.max, "{left:?} {right:?}");
    assert!(right.left() - left.right() >= MIN_PHOTO_WIDTH - 1.0, "photo area {} wide", right.left() - left.right());
    // …and neither panel gets narrower than its minimum
    drag(&mut h, right.left() + 2.0, 1400.0);
    let left = widget(&h, "panel:left_panel");
    drag(&mut h, left.right() - 2.0, -1400.0);
    assert_eq!((h.app.ui.left_width, h.app.ui.right_width), (LEFT_WIDTH.min, RIGHT_WIDTH.min));
    // out-of-range saved widths are clamped on load
    let mut u = crate::UiState { left_width: 5.0, right_width: f32::NAN, ..Default::default() }.sanitized();
    assert_eq!((u.left_width, u.right_width), (LEFT_WIDTH.min, RIGHT_WIDTH.default));
    u.right_width = 9000.0;
    assert_eq!(u.sanitized().right_width, RIGHT_WIDTH.max);
}

/// Every widget drawn over the right panel lies within it (issue #47: selecting a mask added a
/// row wider than the panel, which pushed the panel's contents left, clipping them).
fn assert_inside_right_panel(h: &Headless, what: &str) {
    let panel = widget(h, "panel:right_panel");
    let mut n = 0;
    for (id, r) in &h.app.widgets {
        // widgets of the photo area (pins, filmstrip cells clipped at its edge) and of the tool
        // strip are outside on purpose
        if ["panel:", "film:", "maskPin"].iter().any(|p| id.starts_with(p))
            || r.bottom() <= panel.top()
            || r.right() <= panel.left() + 1.5
            || r.left() >= panel.right() - 1.5
        {
            continue;
        }
        n += 1;
        assert!(r.left() >= panel.left() - 0.5 && r.right() <= panel.right() + 0.5, "{what}: {id} {r:?} sticks out of the panel {panel:?}");
    }
    assert!(n > 10, "{what}: only {n} widgets in the panel");
}

#[test]
fn masking_contents_fit_the_right_panel_at_any_width() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail", "right": "masking"}));
    for (kind, op) in [("subject", None), ("linear", None), ("luminanceRange", Some("subtract")), ("radial", Some("intersect"))] {
        let (command, params) = match op {
            None => ("mask.add", json!({"kind": kind})),
            Some(op) => ("mask.addComponent", json!({"kind": kind, "op": op})),
        };
        let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request(
        "engine.execute",
        json!({"command": "mask.component", "params": {"component": 0, "action": "rename", "name": "A rather long component name for this mask"}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let long = "The person standing beside the very long fence across the background";
    let r = h.request("engine.execute", json!({"command": "mask.rename", "params": {"id": 1, "name": long}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for width in [RIGHT_WIDTH.min, RIGHT_WIDTH.default, 330.0, RIGHT_WIDTH.max] {
        // a long Describe prompt being typed: the field and its Select button stay in the panel
        h.app.ui.describe = Some(("new".into(), long.repeat(2)));
        h.step();
        h.step();
        assert_inside_right_panel(&h, &format!("width {width}, describing"));
        let (field, go) = (widget(&h, "maskDescribe"), widget(&h, "button:maskDescribeGo"));
        assert!(field.right() <= go.left() && go.right() <= widget(&h, "panel:right_panel").right(), "width {width}: {field:?} {go:?}");
        h.app.ui.describe = None;
        // the painted text, not just the widgets: tile labels inside their tiles, a long mask
        // name cut short before the eye
        h.step();
        for (id, r) in h.app.widgets.iter().filter(|(id, _)| id.starts_with("maskNewLabel:")) {
            let tile = widget(&h, &id.replace("maskNewLabel:", "maskNew:"));
            assert!(tile.contains_rect(*r), "width {width}: {id} {r:?} outside its tile {tile:?}");
        }
        let (name, row) = (widget(&h, "maskName:1"), widget(&h, "mask:1"));
        assert!(name.right() <= row.right() - 27.0, "width {width}: the name {name:?} runs under the eye of {row:?}");
        for (selected, tool) in [(false, ""), (true, ""), (true, "brush")] {
            let mask = if selected { json!(2) } else { serde_json::Value::Null };
            h.request("engine.execute", json!({"command": "mask.select", "params": {"id": mask}}), T);
            h.request("ui.set", json!({"rightWidth": width, "tool": tool}), T);
            h.step();
            h.step();
            let what = format!("width {width}, mask selected {selected}, tool {tool:?}");
            assert_eq!(widget(&h, "panel:right_panel").width(), width, "{what}");
            if selected {
                assert!(h.app.widgets.iter().any(|(id, _)| id == "button:maskInvert"), "{what}: no mask actions");
            }
            assert_inside_right_panel(&h, &what);
        }
    }
}

#[test]
fn a_narrow_window_shrinks_the_panels_without_forgetting_their_width() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail", "right": "edit", "leftPanel": true, "rightWidth": 480.0, "leftWidth": 400.0}));
    assert_eq!(widget(&h, "panel:right_panel").width(), 480.0);
    h.request("ui.resize", json!({"width": 1000.0, "height": 800.0}), T);
    h.settle(SETTLE);
    let (left, right) = (widget(&h, "panel:left_panel"), widget(&h, "panel:right_panel"));
    assert!(right.left() - left.right() >= MIN_PHOTO_WIDTH - 1.0, "{left:?} {right:?}");
    // the chosen widths come back when the window does
    assert_eq!((h.app.ui.left_width, h.app.ui.right_width), (400.0, 480.0));
    h.request("ui.resize", json!({"width": 1400.0, "height": 900.0}), T);
    h.settle(SETTLE);
    assert_eq!(widget(&h, "panel:right_panel").width(), 480.0);
}

/// The left sidebar's Albums, Local, By Date and Keywords headers fold their sections; the choice
/// is part of the saved UI state.
#[test]
fn sidebar_sections_collapse_and_remember_it() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    let click = |h: &mut Headless, id: &str| {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    // By Date lists years while open; its header folds them away and back
    assert!(has(&h, "sidebarSection:byDate"));
    let year_rows = |h: &Headless| h.app.widgets.iter().filter(|(w, _)| w.starts_with("source:date:")).count();
    assert!(year_rows(&h) > 0, "the demo library has dated photos");
    click(&mut h, "sidebarSection:byDate");
    assert_eq!(year_rows(&h), 0, "folded");
    assert!(h.app.ui.sidebar_section_collapsed("byDate") && !h.app.ui.sidebar_section_collapsed("albums"));
    assert!(has(&h, "sidebarSection:byDate"), "the header stays so it can be reopened");
    // the choice survives a save/load of the UI state
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert!(back.sidebar_section_collapsed("byDate"));
    click(&mut h, "sidebarSection:byDate");
    assert!(year_rows(&h) > 0, "unfolded again");
    // Albums folds too, and the plus button inside its header still works on its own
    click(&mut h, "sidebarSection:albums");
    assert!(h.app.ui.sidebar_section_collapsed("albums"));
    assert!(has(&h, "icon:albumNew"), "the Create Album button stays in the header");
    click(&mut h, "sidebarSection:albums");
    assert!(!h.app.ui.sidebar_section_collapsed("albums"));
    // Keywords and Local fold their rows too
    let rows = |h: &Headless, prefix: &str| h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).count();
    assert!(rows(&h, "source:keyword:") > 0, "the demo library has keywords");
    click(&mut h, "sidebarSection:keywords");
    assert_eq!(rows(&h, "source:keyword:"), 0, "keywords folded");
    click(&mut h, "sidebarSection:keywords");
    assert!(rows(&h, "source:keyword:") > 0);
    if has(&h, "sidebarSection:local") {
        click(&mut h, "sidebarSection:local");
        assert_eq!(rows(&h, "source:local:"), 0, "local folded");
        assert!(!has(&h, "source:local:browse"), "Browse Folder… folds with it");
        click(&mut h, "sidebarSection:local");
        assert!(!h.app.ui.sidebar_section_collapsed("local"));
    }
    // a click on the plus is the button's, not the header's
    click(&mut h, "icon:albumNew");
    assert!(!h.app.ui.sidebar_section_collapsed("albums"), "the plus does not fold Albums");
}

/// A headless app over a library of file-backed photos that exist only in the catalog.
fn folders_app(paths: &[&str]) -> Headless {
    folders_app_sized(paths, [1400.0, 900.0])
}

fn folders_app_sized(paths: &[&str], size: [f32; 2]) -> Headless {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new();
    for path in paths {
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: (*path).into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Photos imported from two folders: the sidebar's Folders section lists them, choosing one
/// fills the grid with its photos, and All Photos shows everything again.
#[test]
fn folders_section_lists_where_photos_were_imported_from_and_fills_the_grid() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"]);
    // the disk's row starts open; the folders inside a folder stay folded until it is opened
    assert!(has(&h, "source:libfolder:/pics"), "the Folders section lists the library's folders");
    assert!(!has(&h, "source:libfolder:/pics/trip"));
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(has(&h, "source:libfolder:/pics/trip") && has(&h, "source:libfolder:/pics/home"));
    assert_eq!(h.app.session.visible().len(), 3);
    click(&mut h, "source:libfolder:/pics/trip");
    assert_eq!(h.app.session.visible().len(), 2, "only the photos imported from that folder");
    assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::LibraryFolder);
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/trip"));
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default(), "a source, not a filter");
    click(&mut h, "source:all");
    assert_eq!(h.app.session.visible().len(), 3, "All Photos shows everything again");
    // the section folds like the others
    click(&mut h, "sidebarSection:folders");
    assert!(!has(&h, "source:libfolder:/pics"), "folded");
    click(&mut h, "sidebarSection:folders");
    assert!(has(&h, "source:libfolder:/pics"));
}

/// Each disk is a row of its own; the startup disk's row only opens and closes, another disk's
/// row also chooses everything on that disk.
#[test]
fn disks_are_rows_of_their_own() {
    let mut h = folders_app(&["/Users/me/a.jpg", "/Volumes/nas/p/b.jpg", "/Volumes/nas/c.jpg"]);
    assert!(has(&h, "source:libfolder:/") && has(&h, "source:libfolder:/Volumes/nas"), "both disks are listed");
    assert!(has(&h, "source:libfolder:/Volumes/nas/p"), "an open disk shows its folders");
    click(&mut h, "source:libfolder:/");
    assert_eq!(h.app.session.library_folder, None, "the startup disk's path would cover every disk");
    assert!(!has(&h, "source:libfolder:/Users/me"), "a click on it folds it instead");
    click(&mut h, "source:libfolder:/");
    assert!(has(&h, "source:libfolder:/Users/me"));
    click(&mut h, "source:libfolder:/Volumes/nas");
    assert_eq!(h.app.session.visible().len(), 2, "everything on the nas");
}

/// A folder loads into the grid the way an album, a Local folder or Picks does: whatever was
/// shown before is replaced, and the folder's own row is highlighted.
#[test]
fn choosing_a_library_folder_replaces_whatever_was_shown() {
    use lightcraft_engine::LibrarySource;
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    for before in [LibrarySource::Picks, LibrarySource::Folder, LibrarySource::RecentlyDeleted, LibrarySource::Missing] {
        h.app.session.source = before;
        click(&mut h, "source:libfolder:/pics/trip");
        assert_eq!(h.app.session.source, LibrarySource::LibraryFolder, "from {before:?}");
        assert_eq!(h.app.session.visible().len(), 1);
    }
    click(&mut h, "source:libfolder:/pics/trip");
    assert_eq!(h.app.session.source, LibrarySource::LibraryFolder, "choosing it again keeps it, as an album does");
    click(&mut h, "source:libfolder:/pics/home");
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/home"));
}

#[test]
fn the_grid_is_titled_after_the_folder() {
    let mut h = folders_app(&["/Volumes/tokyo/photos/travel/a.jpg"]);
    h.app.session.source = lightcraft_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some("/Volumes/tokyo/photos/travel".into());
    assert_eq!(crate::i18n::source_title(&h.app.session), "photos/travel");
    h.app.session.source = lightcraft_engine::LibrarySource::All;
    assert_eq!(crate::i18n::source_title(&h.app.session), "All Photos");
}

/// A folder that holds nothing itself and leads to one folder starts open: the tree opens down to
/// where the library branches.
#[test]
fn a_chain_of_single_folders_starts_open() {
    let h = folders_app(&["/Users/me/Pictures/2024/a.jpg", "/Users/me/Pictures/2025/b.jpg"]);
    assert!(has(&h, "source:libfolder:/Users/me/Pictures"), "Users and me opened by themselves");
    assert!(!has(&h, "source:libfolder:/Users/me/Pictures/2024"), "the branching folder waits to be opened");
}

/// Removing a folder from the library asks first, then moves its photos to Recently Deleted.
#[test]
fn removing_a_folder_from_the_library_asks_first() {
    use crate::state::Dialog;
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"]);
    h.app.ui.dialog = Some(Dialog::RemoveFolder { path: "/pics/trip".into(), name: "trip".into(), count: 2, disk: false });
    h.step();
    assert_eq!(h.app.session.visible().len(), 3, "nothing happens until it is confirmed");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.visible().len(), 1, "the photos left the library");
    assert_eq!(h.app.ui.dialog, None);
}

fn popup_open(h: &Headless) -> bool {
    egui::Popup::is_any_open(&h.view.ctx)
}

fn right_click(h: &mut Headless, id: &str) {
    let c = widget(h, id).center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// A folder row that holds other disks as well would show their photos too: it only opens.
#[test]
fn a_folder_that_holds_other_disks_only_opens() {
    let mut h = folders_app(&["/Volumes/1.jpg", "/Volumes/tokyo/x.jpg"]);
    assert!(has(&h, "source:libfolder:/Volumes"));
    click(&mut h, "source:libfolder:/Volumes");
    assert_eq!(h.app.session.library_folder, None, "choosing it would show the tokyo disk too");
    click(&mut h, "source:libfolder:/Volumes/tokyo");
    assert_eq!(h.app.session.visible().len(), 1, "a disk's own row does choose");
}

/// Whatever sets the chosen folder (a click, an agent, a rename, undo), its row is on screen.
#[test]
fn the_chosen_folder_is_always_in_view() {
    let mut h = folders_app(&["/pics/trip/day1/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"]);
    assert!(!has(&h, "source:libfolder:/pics/trip/day1"), "folded to begin with");
    h.app.session.source = lightcraft_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some("/pics/trip/day1".into());
    h.step();
    h.step();
    assert!(has(&h, "source:libfolder:/pics/trip/day1"), "the rows above it opened");
    // folding a parent by hand sticks until the choice changes
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(!has(&h, "source:libfolder:/pics/trip/day1"));
}

#[test]
fn a_renamed_folder_keeps_its_place_in_the_tree() {
    /// Removes the folder however the test ends.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = std::env::temp_dir().join(format!("lc-ui-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.clone());
    std::fs::create_dir_all(base.join("pics/trip")).unwrap();
    std::fs::create_dir_all(base.join("pics/home")).unwrap();
    let b = base.to_string_lossy().to_string();
    let mut h = folders_app(&[&format!("{b}/pics/trip/a.jpg"), &format!("{b}/pics/home/b.jpg")]);
    h.app.session.source = lightcraft_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some(format!("{b}/pics/trip"));
    h.step();
    h.step();
    assert!(has(&h, &format!("source:libfolder:{b}/pics/trip")));
    let r = h.request("engine.execute", json!({"command": "folder.rename", "params": {"path": format!("{b}/pics"), "name": "pics2"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(&h, &format!("source:libfolder:{b}/pics2/trip")), "the chosen folder is still on screen after the rename");
}

/// The menu opens from anywhere on a folder row, the disclosure triangle included; a disk row
/// has its own, the startup disk none.
#[test]
fn right_click_opens_a_folders_menu_from_the_triangle_too() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg", "/Volumes/nas/c.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    right_click(&mut h, "source:libfolder:/pics/trip");
    assert!(popup_open(&h), "on the row");
    h.request("ui.key", json!({"key": "escape"}), T);
    h.step();
    h.step();
    assert!(!popup_open(&h));
    right_click(&mut h, "libraryFolderToggle:/pics");
    assert!(popup_open(&h), "on the triangle");
    h.request("ui.key", json!({"key": "escape"}), T);
    h.step();
    h.step();
    right_click(&mut h, "source:libfolder:/Volumes/nas");
    assert!(popup_open(&h), "a disk row offers removing the disk");
    h.request("ui.key", json!({"key": "escape"}), T);
    h.step();
    h.step();
    right_click(&mut h, "source:libfolder:/");
    assert!(!popup_open(&h), "the startup disk offers nothing");
}

#[test]
fn removing_a_disk_from_the_library_asks_first() {
    use crate::state::Dialog;
    let mut h = folders_app(&["/Volumes/nas/a/1.jpg", "/Volumes/nas/2.jpg", "/Users/me/3.jpg"]);
    h.app.ui.dialog = Some(Dialog::RemoveFolder { path: "/Volumes/nas".into(), name: "nas".into(), count: 2, disk: true });
    h.step();
    assert_eq!(h.app.session.visible().len(), 3);
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.visible().len(), 1, "everything on that disk left the library");
}

/// However deep the folders go, each level is indented one step more than its parent, and the
/// sidebar scrolls sideways to reach them instead of squeezing the nesting.
#[test]
fn a_deep_chain_keeps_its_nesting_and_the_sidebar_scrolls_sideways() {
    let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m";
    let mut h = folders_app(&[&format!("{deep}/x/1.jpg"), &format!("{deep}/y/2.jpg")]);
    let mut xs: Vec<f32> = Vec::new();
    let mut path = String::new();
    for name in deep.split('/').filter(|n| !n.is_empty()) {
        path.push('/');
        path.push_str(name);
        xs.push(widget(&h, &format!("libraryFolderToggle:{path}")).center().x);
    }
    assert!(xs.len() == 13 && xs.windows(2).all(|w| (w[1] - w[0] - 16.0).abs() < 0.5), "every level steps right by one indent: {xs:?}");
    let panel = widget(&h, "panel:left_panel");
    assert!(crate::panels::left::content_width(&h.view.ctx) > panel.width(), "the content is wider than the panel: the bar appears");
    assert!(widget(&h, "source:all").width() > panel.width(), "the rows are as wide as the widest one, so there is something to scroll to");
    // counts stay where you can read them, at the visible edge, however wide the rows are
    for id in ["count:all", "count:picks", "count:recentlyDeleted", "count:libfolder:/a"] {
        assert!(widget(&h, id).right() <= panel.right(), "{id} stays inside the panel");
    }
    // what a row needs is known before it is drawn off screen, so the plus stays reachable
    assert!(widget(&h, "icon:albumNew").right() <= panel.right(), "Create Album stays inside the visible panel");
    // a click in the middle of a deep row chooses it; only the little triangle folds it
    click(&mut h, "source:libfolder:/a/b/c/d/e/f/g/h/i/j/k/l");
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/a/b/c/d/e/f/g/h/i/j/k/l"), "the row's middle is the row, not its triangle");
}

/// A sidebar that fits needs no sideways scrolling.
#[test]
fn a_sidebar_that_fits_does_not_scroll_sideways() {
    let h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    let panel = widget(&h, "panel:left_panel");
    let w = crate::panels::left::content_width(&h.view.ctx);
    assert!(w > 0.0 && w <= panel.width(), "rows were measured and fit: no horizontal bar ({w} vs {})", panel.width());
    assert!((widget(&h, "source:all").width() - (panel.width() - 0.0)).abs() < 24.0, "rows are as wide as the panel");
}

/// A long folder name is trimmed before it can run under the photo count.
#[test]
fn a_long_folder_name_never_runs_under_the_photo_count() {
    let long = "/very/long/2024-06-12 Tripping Through The Extremely Long Named Mountains Of Somewhere";
    let mut h = folders_app_sized(&[&format!("{long}/a.jpg")], [900.0, 700.0]);
    h.app.session.source = lightcraft_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some(long.into());
    h.step();
    h.step();
    let (title, count) = (widget(&h, "grid:title"), widget(&h, "grid:count"));
    assert!(title.right() + 8.0 <= count.left(), "title {title:?} vs count {count:?}");
}

fn tooltip_shown(h: &Headless) -> bool {
    h.view.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().any(|l| l.order == egui::Order::Tooltip))
}

const LONG_NAME: &str = "Aliah Ira Polanco-Grylls and a name much longer than any sidebar row can show";

/// A name never runs past the photo count: it is cut with an ellipsis, in full on hover.
#[test]
fn a_long_name_stops_at_the_count_and_shows_in_full_on_hover() {
    let mut h = folders_app(&[&format!("/pics/{LONG_NAME}/a.jpg"), "/pics/short/b.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    let (long, short) = (format!("/pics/{LONG_NAME}"), "/pics/short".to_string());
    let (label, count) = (widget(&h, &format!("label:libfolder:{long}")), widget(&h, &format!("count:libfolder:{long}")));
    assert!(label.right() + 4.0 <= count.left(), "label {label:?} stays left of the count {count:?}");
    let short_label = widget(&h, &format!("label:libfolder:{short}"));
    assert!(short_label.width() < label.width(), "a name that fits is not cut");
    // hovering the cut one shows the whole name; a name that fits needs no tooltip of its own
    let c = widget(&h, &format!("source:libfolder:{long}")).center();
    let r = h.request("ui.move", json!({"x": c.x, "y": c.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(5), tooltip_shown), "the full name appears on hover");
}

/// More room shows more of the name.
#[test]
fn widening_the_sidebar_shows_more_of_a_long_name() {
    let mut h = folders_app(&[&format!("/pics/{LONG_NAME}/a.jpg"), "/pics/short/b.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    let id = format!("label:libfolder:/pics/{LONG_NAME}");
    let narrow = widget(&h, &id).width();
    let r = h.request("ui.set", json!({"leftWidth": 480.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.step();
    assert!(widget(&h, &id).width() > narrow + 100.0, "a wider panel shows more of it");
}

/// A long name alone does not make the sidebar scroll sideways: depth does.
#[test]
fn a_long_name_alone_does_not_make_the_sidebar_scroll() {
    let h = folders_app(&[&format!("/{LONG_NAME}/a.jpg")]);
    let panel = widget(&h, "panel:left_panel");
    assert!(crate::panels::left::content_width(&h.view.ctx) <= panel.width(), "a name takes at most what a row can show");
}

/// The selection bar ends at the panel's edge, not past it, when the content is wider.
#[test]
fn the_selection_bar_stays_inside_the_panel() {
    let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m";
    let mut h = folders_app(&[&format!("{deep}/x/1.jpg"), &format!("{deep}/y/2.jpg")]);
    click(&mut h, "source:libfolder:/a/b/c/d/e/f/g/h/i/j/k/l");
    let panel = widget(&h, "panel:left_panel");
    let bar = widget(&h, "highlight:libfolder:/a/b/c/d/e/f/g/h/i/j/k/l");
    assert!(bar.right() <= panel.right(), "the bar {bar:?} ends inside the panel {panel:?}");
}

/// Select All in the loupe: every filmstrip cell of a selected photo is drawn selected, not just
/// the active one (issue #298).
#[test]
fn filmstrip_marks_every_selected_photo() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail"}));
    let r = h.request("ui.key", json!({"key": "a", "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    let img = h.snapshot(SETTLE);
    assert_eq!(h.view.ctx.pixels_per_point(), 1.0, "the test samples pixels at point coordinates");
    let active = h.app.session.selection.active.expect("active photo");
    let t = crate::theme::Tokens::get(&h.view.ctx);
    let ids = h.app.session.visible_cloned();
    assert!(ids.len() > 3 && h.app.session.selection.ids.len() == ids.len());
    let mut checked = 0;
    for id in ids.iter().filter(|id| **id != active) {
        let Some(r) = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r) else { continue };
        // cells scrolled under the right panel (default width plus the tool rail) are not visible
        if r.max.x > 1400.0 - RIGHT_WIDTH.default - 48.0 {
            continue;
        }
        // a pixel in the cell's left margin at mid height, clear of the name text and the thumbnail
        let (x, y) = ((r.min.x + 4.0) as usize, r.center().y as usize);
        assert_eq!(img[(x, y)], t.cell_selected, "film cell of selected photo {} is drawn selected", id.0);
        checked += 1;
    }
    assert!(checked >= 3, "checked {checked} cells");
}

/// By Date and Keywords count the whole library, so choosing a row shows its photos from All
/// Photos even when another source (here Recently Deleted, which is empty) was open (issue #341).
#[test]
fn date_and_keyword_rows_show_their_photos_from_any_source() {
    // tall enough that the By Date rows are on screen below the other sections
    let mut h = demo([1400.0, 2000.0], json!({"view": "photoGrid", "leftPanel": true}));
    let first = h.app.session.visible_cloned()[0];
    let year = h.app.session.catalog.photo(first).and_then(|p| p.captured.clone()).expect("demo photo date")[..4].to_string();
    // (Keywords rows go through the same helper, `browse_all_photos`)
    let (row, key) = (format!("source:date:{year}"), "date");
    let r = h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "recentlyDeleted"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.visible().is_empty(), "nothing deleted in the demo");
    let r = h.request("ui.clickWidget", json!({"id": row}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All, "{key}");
    assert!(!h.app.session.visible().is_empty(), "{key}: its photos are shown");
    // choosing the row again clears it, and stays in All Photos
    let r = h.request("ui.clickWidget", json!({"id": row}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default(), "{key}");
}
