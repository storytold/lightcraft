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

/// Albums nest in folders like the other sidebar trees: a folder row has a disclosure triangle
/// (`albumToggle:<id>`), plain albums have none, and folding a folder hides what is inside it.
#[test]
fn album_folders_have_a_disclosure_triangle() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let europe = make(&mut h, json!({"name": "Europe", "folder": true, "parent": trips}));
    let best = make(&mut h, json!({"name": "Best", "parent": europe}));
    let loose = make(&mut h, json!({"name": "Loose"}));
    h.step();
    h.step();
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    let toggle = |h: &mut Headless, id: u64| {
        let r = h.request("ui.clickWidget", json!({"id": format!("albumToggle:{id}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    // folders start open and show their albums; only folders get a triangle
    assert!(has(&h, &format!("source:album:{best}")), "open by default");
    assert!(has(&h, &format!("albumToggle:{trips}")) && has(&h, &format!("albumToggle:{europe}")));
    assert!(!has(&h, &format!("albumToggle:{best}")) && !has(&h, &format!("albumToggle:{loose}")), "albums have no triangle");
    // folding the outer folder hides everything inside it, the inner triangle included
    toggle(&mut h, trips);
    assert!(!has(&h, &format!("source:folder:{europe}")) && !has(&h, &format!("source:album:{best}")));
    assert!(has(&h, &format!("source:album:{loose}")), "siblings stay");
    // opening it again leaves the inner folder as it was
    toggle(&mut h, trips);
    assert!(has(&h, &format!("source:album:{best}")));
    toggle(&mut h, europe);
    assert!(!has(&h, &format!("source:album:{best}")) && has(&h, &format!("source:folder:{europe}")));
    // an empty folder has nothing to open: no triangle until it holds something
    let empty = make(&mut h, json!({"name": "Empty", "folder": true}));
    h.step();
    h.step();
    assert!(has(&h, &format!("source:folder:{empty}")) && !has(&h, &format!("albumToggle:{empty}")));
    make(&mut h, json!({"name": "First", "parent": empty}));
    h.step();
    h.step();
    assert!(has(&h, &format!("albumToggle:{empty}")), "it gets one with its first child");
}

/// A dialog that cannot do its job (here: the chosen parent is a plain album, as when the folder
/// was deleted or turned out not to be one) stays open with the error shown, the typed name kept.
#[test]
fn creating_in_a_non_folder_keeps_the_dialog_open() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let plain = h.app.session.execute("album.create", &json!({"name": "Plain"})).unwrap()["id"].as_u64().unwrap();
    let before = h.app.session.catalog.albums().count();
    let r = h.request("engine.execute", json!({"command": "dialog.newAlbum", "params": {"name": "Kept", "parent": plain}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.session.catalog.albums().count(), before, "nothing was made");
    assert!(
        matches!(&h.app.ui.dialog, Some(crate::state::Dialog::NewAlbum { name, .. }) if name == "Kept"),
        "the dialog stays, with its text: {:?}",
        h.app.ui.dialog
    );
}

/// Creating from a folder's menu: every `dialog.new*` command takes the folder to create in, the
/// dialog remembers it, and the new row shows up even when that folder (and the one holding it)
/// was folded shut.
#[test]
fn albums_are_created_inside_the_chosen_folder() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let europe = make(&mut h, json!({"name": "Europe", "folder": true, "parent": trips}));
    make(&mut h, json!({"name": "Seed", "parent": europe}));
    h.step();
    h.step();
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    let create = |h: &mut Headless, command: &str, name: &str, parent: Option<u64>| -> u64 {
        let before: Vec<u64> = h.app.session.catalog.albums().map(|a| a.id.0).collect();
        let mut params = json!({"name": name});
        if let Some(p) = parent {
            params["parent"] = json!(p);
        }
        let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let new: Vec<u64> = h.app.session.catalog.albums().map(|a| a.id.0).filter(|i| !before.contains(i)).collect();
        assert_eq!(new.len(), 1, "{command} made exactly one album: {new:?}");
        new[0]
    };
    let parent_of = |h: &Headless, id: u64| h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap().parent.map(|p| p.0);
    // fold both folders: the new rows must still be visible afterwards
    for f in [europe, trips] {
        let r = h.request("ui.clickWidget", json!({"id": format!("albumToggle:{f}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    }
    assert!(!has(&h, &format!("source:folder:{europe}")), "Trips is folded");
    let album = create(&mut h, "dialog.newAlbum", "Day 1", Some(europe));
    assert_eq!(parent_of(&h, album), Some(europe));
    assert!(has(&h, &format!("source:album:{album}")), "the new album is shown, its folders opened to it");
    let folder = create(&mut h, "dialog.newFolder", "Italy", Some(europe));
    assert_eq!(parent_of(&h, folder), Some(europe));
    assert!(h.app.session.catalog.album(lightcraft_catalog::AlbumId(folder)).unwrap().folder);
    let smart = create(&mut h, "dialog.smartAlbum", "Rated", Some(folder));
    assert_eq!(parent_of(&h, smart), Some(folder));
    assert!(h.app.session.catalog.album(lightcraft_catalog::AlbumId(smart)).unwrap().is_smart());
    let from_view = create(&mut h, "dialog.newSmartAlbum", "From view", Some(trips));
    assert_eq!(parent_of(&h, from_view), Some(trips));
    // without a folder they are still made at the top level
    let top = create(&mut h, "dialog.newAlbum", "Loose", None);
    assert_eq!(parent_of(&h, top), None);
}

/// Saved or scripted dialogs from before the folder parameter still load.
#[test]
fn album_dialogs_without_a_parent_still_deserialize() {
    use crate::state::Dialog;
    let d: Dialog = serde_json::from_value(json!({"kind": "newAlbum", "name": "x", "folder": false})).unwrap();
    assert!(matches!(d, Dialog::NewAlbum { parent: None, .. }));
    let d: Dialog = serde_json::from_value(json!({"kind": "newSmartAlbum", "name": "x"})).unwrap();
    assert!(matches!(d, Dialog::NewSmartAlbum { parent: None, .. }));
    let mut v = serde_json::to_value(Dialog::SmartRules { id: None, name: "x".into(), rules: Default::default(), parent: Some(3) }).unwrap();
    assert_eq!(v["parent"], 3);
    v.as_object_mut().unwrap().remove("parent");
    let d: Dialog = serde_json::from_value(v).unwrap();
    assert!(matches!(d, Dialog::SmartRules { parent: None, .. }));
}

/// An album tree to drag in: `Archive` (empty folder), `Trips` ⊃ `Sub` (folder) + `Best`, `Loose`.
struct AlbumTree {
    h: Headless,
    archive: u64,
    trips: u64,
    sub: u64,
    best: u64,
    loose: u64,
}

fn album_tree() -> AlbumTree {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let archive = make(&mut h, json!({"name": "Archive", "folder": true}));
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let sub = make(&mut h, json!({"name": "Sub", "folder": true, "parent": trips}));
    let best = make(&mut h, json!({"name": "Best", "parent": trips}));
    let loose = make(&mut h, json!({"name": "Loose"}));
    h.step();
    h.step();
    AlbumTree { h, archive, trips, sub, best, loose }
}

impl AlbumTree {
    fn row(&self, id: u64) -> egui::Rect {
        let h = &self.h;
        let name = ["source:folder:", "source:album:"].iter().map(|p| format!("{p}{id}")).find(|n| h.app.widgets.iter().any(|(w, _)| w == n));
        widget(h, &name.unwrap_or_else(|| panic!("no row for album {id}")))
    }
    /// The middle of the "Albums" header, where an album is dropped to take it to the top level.
    fn albums_header(&self) -> egui::Pos2 {
        widget(&self.h, "sidebarSection:albums").center()
    }
    fn parent(&self, id: u64) -> Option<u64> {
        self.h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap().parent.map(|p| p.0)
    }
    /// Press at `from`, move through each `(point, frames held there)`, release at the last one.
    fn drag(&mut self, from: egui::Pos2, path: &[(egui::Pos2, usize)]) {
        self.drag_with(from, path, vec![]);
    }
    /// [`Self::drag`] with `before_release` events sent just before the button comes up.
    fn drag_with(&mut self, from: egui::Pos2, path: &[(egui::Pos2, usize)], before_release: Vec<egui::Event>) {
        let mut ev = |e| self.h.app.synthetic.push(e);
        ev(egui::Event::PointerMoved(from));
        ev(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        let mut at = from;
        for (to, hold) in path {
            for i in 1..=8 {
                at = from + (*to - from) * (i as f32 / 8.0);
                ev(egui::Event::PointerMoved(at));
            }
            for _ in 0..*hold {
                ev(egui::Event::PointerMoved(*to));
            }
            at = *to;
        }
        for e in before_release {
            ev(e);
        }
        ev(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        for _ in 0..400 {
            if self.h.app.synthetic.is_empty() {
                break;
            }
            self.h.step();
        }
        self.h.step();
        self.h.step();
    }
    /// Drag `id` to `frac` of the way down the row of `onto` (0 = its top edge, 1 = its bottom edge).
    fn drag_to(&mut self, id: u64, onto: u64, frac: f32) {
        let (from, r) = (self.row(id).center(), self.row(onto));
        self.drag(from, &[(egui::pos2(r.left() + 100.0, r.top() + frac * r.height()), 3)]);
    }
    fn id_of(&self, name: &str) -> u64 {
        self.h.app.session.catalog.albums().find(|a| a.name == name).unwrap_or_else(|| panic!("no album {name}")).id.0
    }
    /// The names inside `parent`, folders or albums only.
    fn kids(&self, parent: Option<u64>, folders: bool) -> Vec<String> {
        let cat = &self.h.app.session.catalog;
        cat.album_children(parent.map(lightcraft_catalog::AlbumId)).iter().filter(|a| a.folder == folders).map(|a| a.name.clone()).collect()
    }
    fn drag_row(&mut self, id: u64, onto: u64) {
        let (from, to) = (self.row(id).center(), self.row(onto).center());
        self.drag(from, &[(to, 3)]);
    }
}

/// Given albums and folders in the sidebar, when an album or folder is dragged onto a folder,
/// then it moves into it (as Move to does); onto a plain album, itself or something inside it, it
/// stays where it was.
#[test]
fn dragging_an_album_onto_a_folder_moves_it() {
    let mut t = album_tree();
    t.drag_row(t.loose, t.archive);
    assert_eq!(t.parent(t.loose), Some(t.archive), "an album into a folder");
    assert!(t.h.app.ui.dragging_album.is_none(), "the drag ended");
    // it is shown inside its new folder, which opened for it
    assert!(t.h.app.widgets.iter().any(|(w, _)| *w == format!("source:album:{}", t.loose)));
    t.drag_row(t.sub, t.archive);
    assert_eq!(t.parent(t.sub), Some(t.archive), "a folder into a folder");
    // refused: onto itself, into its own subfolder; onto a plain album it only goes beside it
    t.drag_row(t.best, t.loose);
    assert_eq!(t.parent(t.best), Some(t.archive), "a plain album is no destination: Best lands beside Loose, not inside");
    assert_eq!(t.parent(t.loose), Some(t.archive));
    let (from, aside) = (t.row(t.archive).center(), t.row(t.archive).center() + egui::vec2(40.0, 0.0));
    t.drag(from, &[(aside, 3)]);
    assert_eq!(t.parent(t.archive), None, "not onto itself");
    t.drag_row(t.archive, t.sub);
    assert_eq!(t.parent(t.archive), None, "not into something inside it");
    assert!(t.h.app.ui.dragging_album.is_none());
    // a plain click still only opens the album
    let c = t.row(t.best).center();
    t.drag(c, &[(c, 0)]);
    assert_eq!(t.h.app.session.source, lightcraft_engine::LibrarySource::Album(lightcraft_catalog::AlbumId(t.best)));
}

/// A drag ends only with the main button, and Esc abandons it: a release of another button over a
/// folder does not drop there, and Esc then release drops nothing.
#[test]
fn an_album_drag_ignores_other_buttons_and_esc_cancels_it() {
    let mut t = album_tree();
    let (from, to) = (t.row(t.loose).center(), t.row(t.archive).center());
    let m = egui::Modifiers::default();
    let secondary = |pressed| egui::Event::PointerButton { pos: to, button: egui::PointerButton::Secondary, pressed, modifiers: m };
    // a right click over the folder mid-drag; the drag then ends elsewhere (on the Loose row itself)
    t.drag_with(from, &[(to, 3), (from, 2)], vec![secondary(true), secondary(false)]);
    assert_eq!(t.parent(t.loose), None, "the secondary release is not a drop");
    assert!(t.h.app.ui.dragging_album.is_none());
    let esc = |pressed| egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: m };
    let (from, to) = (t.row(t.loose).center(), t.row(t.archive).center());
    t.drag_with(from, &[(to, 3)], vec![esc(true), esc(false)]);
    assert_eq!(t.parent(t.loose), None, "Esc abandons the drag, the release drops nothing");
    assert!(t.h.app.ui.dragging_album.is_none());
    // and a drag that is left alone still drops
    t.drag_row(t.loose, t.archive);
    assert_eq!(t.parent(t.loose), Some(t.archive));
}

/// Given the top-level albums Garden, Loose, Night Sky, Portfolio listed by name, when an album is
/// dropped on the top or bottom half of another, then it is placed before or after it and the
/// list is ordered by hand from then on.
#[test]
fn dropping_on_the_edge_of_an_album_places_it_there() {
    let mut t = album_tree();
    let id = |t: &AlbumTree, name: &str| t.h.app.session.catalog.albums().find(|a| a.name == name).unwrap().id.0;
    let (garden, night, portfolio) = (id(&t, "Garden"), id(&t, "Night Sky"), id(&t, "Portfolio"));
    assert_eq!(t.kids(None, false), ["Garden", "Loose", "Night Sky", "Portfolio"]);
    t.drag_to(portfolio, garden, 0.2);
    assert_eq!(t.kids(None, false), ["Portfolio", "Garden", "Loose", "Night Sky"], "before Garden");
    t.drag_to(garden, night, 0.8);
    assert_eq!(t.kids(None, false), ["Portfolio", "Loose", "Night Sky", "Garden"], "after Night Sky");
    assert!(t.h.app.session.catalog.album_children_are_ordered(None));
    // one undo step per drop
    t.h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(t.kids(None, false), ["Portfolio", "Garden", "Loose", "Night Sky"]);
    assert!(t.h.app.ui.dragging_album.is_none());
}

/// Dropping beside an album in another folder moves the album there, at that place.
#[test]
fn dropping_beside_an_album_in_another_folder_moves_it_there() {
    let mut t = album_tree();
    t.drag_to(t.loose, t.best, 0.2);
    assert_eq!(t.parent(t.loose), Some(t.trips));
    assert_eq!(t.kids(Some(t.trips), false), ["Loose", "Best"]);
}

/// Where nothing would change, nothing is done: no edit, so no hand order appears.
#[test]
fn dropping_where_it_already_is_changes_nothing() {
    let mut t = album_tree();
    let garden = t.id_of("Garden");
    assert_eq!(t.kids(None, false)[..2], ["Garden", "Loose"]);
    t.drag_to(t.loose, garden, 0.8);
    t.drag_to(garden, t.loose, 0.2);
    assert!(!t.h.app.session.catalog.album_children_are_ordered(None), "Loose already follows Garden");
    // a real placement orders the albums; the same drop again then changes nothing
    t.drag_to(t.loose, garden, 0.2);
    assert!(t.h.app.session.catalog.album_children_are_ordered(None));
    let snapshot = t.h.app.session.catalog.to_snapshot();
    t.drag_to(t.loose, garden, 0.2);
    assert_eq!(t.h.app.session.catalog.to_snapshot(), snapshot);
}

/// The bottom of an open folder is inside it; from the bottom edge of a closed one a folder goes
/// after it (last of the folders); a folder over an album does nothing.
#[test]
fn folders_go_after_a_closed_folder_and_inside_an_open_one() {
    let mut t = album_tree();
    t.drag_to(t.archive, t.trips, 0.9);
    assert_eq!(t.parent(t.archive), Some(t.trips), "the bottom of an open folder is inside it");
    let travel = t.id_of("Travel 2026");
    assert_eq!(t.kids(None, true), ["Travel 2026", "Trips"]);
    let r = t.h.request("ui.clickWidget", json!({"id": format!("albumToggle:{}", t.trips)}), T);
    assert_eq!(r["ok"], true, "{r}");
    t.h.step();
    t.h.step();
    t.drag_to(travel, t.trips, 0.9);
    assert_eq!(t.kids(None, true), ["Trips", "Travel 2026"], "after Trips, the last folder");
    assert_eq!(t.parent(travel), None);
    // folders are not placed among albums
    t.drag_to(t.trips, t.loose, 0.5);
    assert_eq!(t.kids(None, true), ["Trips", "Travel 2026"]);
}

/// Resting on a closed folder's edge (where the drop goes beside it) does not open it: the rows
/// would shift under the pointer. Only a drop that goes inside opens it.
#[test]
fn hovering_a_folders_edge_does_not_open_it() {
    let mut t = album_tree();
    let r = t.h.request("ui.clickWidget", json!({"id": format!("albumToggle:{}", t.trips)}), T);
    assert_eq!(r["ok"], true, "{r}");
    t.h.step();
    t.h.step();
    let (from, r) = (t.row(t.archive).center(), t.row(t.trips));
    t.drag(from, &[(egui::pos2(r.left() + 100.0, r.top() + 0.9 * r.height()), 90)]);
    assert_eq!(t.parent(t.archive), None, "dropped after Trips, not inside it");
    assert_eq!(t.kids(None, true), ["Travel 2026", "Trips", "Archive"]);
}

/// Folders are placed among folders by their edges; their middle still means "inside"; an album
/// over a folder is always "inside"; where nothing would change nothing is done or ordered.
#[test]
fn folders_are_placed_by_their_edges_and_entered_by_their_middle() {
    let mut t = album_tree();
    assert_eq!(t.kids(None, true), ["Archive", "Travel 2026", "Trips"]);
    t.drag_to(t.trips, t.archive, 0.1);
    assert_eq!(t.kids(None, true), ["Trips", "Archive", "Travel 2026"], "Trips before Archive");
    assert_eq!(t.parent(t.trips), None);
    t.drag_to(t.sub, t.archive, 0.5);
    assert_eq!(t.parent(t.sub), Some(t.archive), "the middle of a folder is inside it");
    // an album anywhere over a folder row goes inside, even at its edge
    t.drag_to(t.loose, t.archive, 0.05);
    assert_eq!(t.parent(t.loose), Some(t.archive));
    // already in place: no change, so no hand order is made
    let mut t = album_tree();
    let travel = t.h.app.session.catalog.albums().find(|a| a.name == "Travel 2026").unwrap().id.0;
    t.drag_to(travel, t.trips, 0.1);
    assert!(!t.h.app.session.catalog.album_children_are_ordered(None), "Travel 2026 already sits right before Trips");
}

/// Holding a dragged album near the top or bottom edge of the sidebar scrolls it that way; in the
/// middle, or beside the sidebar, nothing scrolls. (Each drag ends with Esc: nothing is dropped.)
#[test]
fn a_dragged_album_scrolls_the_sidebar_at_its_edges() {
    let mut t = album_tree();
    for i in 0..60 {
        t.h.app.session.execute("album.create", &json!({"name": format!("Z{i:02}")})).unwrap();
    }
    t.h.step();
    t.h.step();
    let last = t.id_of("Z59");
    let panel = widget(&t.h, "panel:left_panel");
    let (cx, top, bottom) = (panel.center().x, panel.top(), panel.bottom());
    let start = t.row(last).top();
    assert!(start > bottom, "the last album starts below the visible sidebar: {start} vs {bottom}");
    // a row in the middle of the visible sidebar: where a drag starts
    let pick = |t: &AlbumTree| {
        let rows = t.h.app.widgets.iter().filter(|(w, r)| w.starts_with("source:album:") && r.top() > top + 120.0 && r.bottom() < bottom - 120.0);
        rows.map(|(_, r)| r.center()).next().expect("a visible album row")
    };
    let esc = |pressed| egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: Default::default() };
    let hold = |t: &mut AlbumTree, to: egui::Pos2| {
        let from = pick(t);
        t.drag_with(from, &[(to, 90)], vec![esc(true), esc(false)]);
    };
    hold(&mut t, egui::pos2(cx, panel.center().y));
    assert_eq!(t.row(last).top(), start, "no scrolling in the middle");
    hold(&mut t, egui::pos2(panel.right() + 200.0, bottom - 6.0));
    assert_eq!(t.row(last).top(), start, "no scrolling beside the sidebar");
    hold(&mut t, egui::pos2(cx, bottom - 6.0));
    let scrolled = t.row(last).top();
    assert!(scrolled < start - 100.0, "the bottom edge scrolls down: {start} -> {scrolled}");
    hold(&mut t, egui::pos2(cx, top + 70.0));
    let back = t.row(last).top();
    assert!(back > scrolled + 100.0, "the top edge scrolls back up: {scrolled} -> {back}");
    assert!(t.h.app.ui.dragging_album.is_none());
}

/// Photos dragged from the grid scroll the sidebar the same way, to reach an album further down.
#[test]
fn dragged_photos_scroll_the_sidebar_at_its_edges() {
    let mut t = album_tree();
    for i in 0..60 {
        t.h.app.session.execute("album.create", &json!({"name": format!("Z{i:02}")})).unwrap();
    }
    t.h.step();
    t.h.step();
    let (last, panel) = (t.id_of("Z59"), widget(&t.h, "panel:left_panel"));
    let start = t.row(last).top();
    let first = t.h.app.session.visible_cloned()[0].0;
    let thumb = widget(&t.h, &format!("thumb:{first}")).center();
    t.drag(thumb, &[(egui::pos2(panel.center().x, panel.bottom() - 6.0), 90)]);
    assert!(t.row(last).top() < start - 100.0, "scrolled down: {start} -> {}", t.row(last).top());
    assert!(t.h.app.ui.dragging_photos.is_none(), "the drag ended");
}

/// How fast the sidebar scrolls for a pointer at `y` (+ down) is zero in the middle, ramps up
/// within the edge band and stays at the top speed past the edge.
#[test]
fn auto_scroll_speed_ramps_up_at_the_edges() {
    use crate::panels::left::auto_scroll_speed;
    let (top, bottom) = (100.0, 700.0);
    assert_eq!(auto_scroll_speed(400.0, top, bottom), 0.0);
    assert_eq!(auto_scroll_speed(top + 60.0, top, bottom), 0.0, "outside the band");
    let (slow, fast, past) =
        (auto_scroll_speed(bottom - 30.0, top, bottom), auto_scroll_speed(bottom - 5.0, top, bottom), auto_scroll_speed(bottom + 80.0, top, bottom));
    assert!(0.0 < slow && slow < fast && fast <= past, "{slow} {fast} {past}");
    assert_eq!(past, auto_scroll_speed(bottom + 400.0, top, bottom), "capped");
    let up = auto_scroll_speed(top + 5.0, top, bottom);
    assert!(up < 0.0 && (up + fast).abs() < 1e-3, "the same ramp upwards: {up} vs {fast}");
}

/// Dragging out of a folder: the "Albums" header is the way back to the top level (it stays in
/// view however long the tree is), only offered for an album that sits in a folder, and the move
/// is one undo step. The drop does not fold the section.
#[test]
fn dragging_an_album_to_the_albums_header_takes_it_to_the_top_level() {
    let mut t = album_tree();
    let has = |t: &AlbumTree, id: &str| t.h.app.widgets.iter().any(|(w, _)| w == id);
    assert!(!has(&t, "albumDrop:top"), "no target unless an album is being dragged");
    let (from, header) = (t.row(t.best).center(), t.albums_header());
    t.drag(from, &[(header, 3)]);
    assert_eq!(t.parent(t.best), None, "moved out of Trips");
    assert!(!t.h.app.ui.sidebar_section_collapsed("albums"), "dropping on the header does not fold it");
    // a top-level album has nowhere to go: dropping it on the header does nothing
    let (from, header) = (t.row(t.best).center(), t.albums_header());
    t.drag(from, &[(header, 3)]);
    assert_eq!(t.parent(t.best), None);
    assert!(!t.h.app.ui.sidebar_section_collapsed("albums"));
    t.h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(t.parent(t.best), Some(t.trips), "one undo puts it back");
}

/// Holding a dragged album over a closed folder opens it, so the drop can go into a subfolder.
#[test]
fn a_dragged_album_opens_the_folder_it_hovers_over() {
    let mut t = album_tree();
    let r = t.h.request("ui.clickWidget", json!({"id": format!("albumToggle:{}", t.trips)}), T);
    assert_eq!(r["ok"], true, "{r}");
    t.h.step();
    t.h.step();
    assert!(!t.h.app.widgets.iter().any(|(w, _)| *w == format!("source:folder:{}", t.sub)), "Trips is folded");
    // where Sub's row will be once Trips is open: one row below it
    let trips = t.row(t.trips);
    let inside = trips.center() + egui::vec2(0.0, trips.height());
    let from = t.row(t.loose).center();
    t.drag(from, &[(trips.center(), 90), (inside, 3)]);
    assert_eq!(t.parent(t.loose), Some(t.sub), "dropped on Sub, which was only there after the hover");
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
    // the folder tree writes paths with forward slashes, and its widget ids carry them
    let b = base.to_string_lossy().replace('\\', "/");
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

/// Right-click a folder ▸ Set Color Label ▸ a colour: the folder's row shows the label's dot (a
/// row without a label shows none), and None takes it off again.
#[test]
fn a_folder_takes_a_colour_label_from_its_menu() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(!has(&h, "labelMark:libfolder:/pics/trip"), "no label, no dot");
    right_click(&mut h, "source:libfolder:/pics/trip");
    click(&mut h, "folderLabelMenu");
    click(&mut h, "folderLabel:red");
    assert_eq!(h.app.session.catalog.folder_color_label("/pics/trip"), Some(lightcraft_catalog::ColorLabel::Red));
    assert!(!popup_open(&h), "choosing closes the menu");
    h.step();
    assert!(has(&h, "labelMark:libfolder:/pics/trip"), "the row shows its label");
    assert!(!has(&h, "labelMark:libfolder:/pics/home"), "its neighbour does not");
    right_click(&mut h, "source:libfolder:/pics/trip");
    click(&mut h, "folderLabelMenu");
    click(&mut h, "folderLabel:none");
    assert_eq!(h.app.session.catalog.folder_color_label("/pics/trip"), None);
    h.step();
    assert!(!has(&h, "labelMark:libfolder:/pics/trip"));
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

const LONG_NAME: &str = "Lake Como Wedding-Day and a name much longer than any sidebar row can show";

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

/// Issue #501: use the actual context-menu button on the populated demo folder.
#[test]
fn deleting_a_populated_album_folder_from_its_menu_is_undoable() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let folder = h.app.session.catalog.albums().find(|a| a.folder && a.name == "Travel 2026").unwrap().id;
    assert!(!h.app.session.catalog.album_children(Some(folder)).is_empty());
    let before = h.app.session.catalog.to_snapshot();
    let photo_count = h.app.session.catalog.photos().count();
    right_click(&mut h, &format!("source:folder:{}", folder.0));
    click(&mut h, &format!("albumDelete:{}", folder.0));
    assert!(h.app.session.catalog.album(folder).is_none());
    assert_eq!(h.app.session.catalog.photos().count(), photo_count);
    assert!(!popup_open(&h));
    h.app.run("edit.undo", json!({})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.session.catalog.to_snapshot(), before);
    assert!(has(&h, &format!("source:folder:{}", folder.0)));
}

/// A menu command's toast shows, however long the app has been running: commands made their
/// toasts with a fresh context, whose clock is at 0, so by the app's clock they were already over
/// (Auto Advance On, Imported presets, …).
#[test]
fn a_menu_commands_toast_shows_after_a_while() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    for _ in 0..600 {
        h.step();
    }
    let r = h.request("engine.execute", serde_json::json!({"command": "view.autoAdvance"}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Auto Advance")), "{:?}", h.app.ui.toast);
}

/// Auto-Tag from Tracklog's toast shows too: it was stamped by a clock of its own, which starts at
/// zero, so a while into a session it had always already expired.
#[test]
fn the_tracklog_toast_shows_after_a_while() {
    let dir = std::env::temp_dir().join(format!("lc-tracklog-toast-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gpx = dir.join("walk.gpx");
    std::fs::write(
        &gpx,
        r#"<?xml version="1.0"?><gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg><trkpt lat="46.0" lon="7.0"><time>2026-05-01T10:00:00Z</time></trkpt></trkseg></trk></gpx>"#,
    )
    .unwrap();
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    for _ in 0..600 {
        h.step();
    }
    let p = serde_json::json!({"command": "photo.tagFromTracklog", "params": {"path": gpx.to_string_lossy(), "offset": 0}});
    let r = h.request("engine.execute", p, std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Tagged")), "{:?}", h.app.ui.toast);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #678: Settings ▸ Faces cannot switch recognition on until a recognition model is chosen:
/// the box is off and clicking it changes nothing. Installing a model switches recognition on by
/// itself, and from then on the box turns it off and on.
#[test]
fn recognise_faces_stays_off_until_a_model_is_chosen() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    let dir = std::env::temp_dir().join(format!("lc-ui-faces-toggle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    h.app.session.face_models_dir = Some(dir.join("models"));
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "faces"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    // (the first request switches the widget list on, the next frame fills it)
    h.request("ui.widgets", json!({}), T);
    h.step();
    let enabled = |h: &mut Headless| h.app.session.execute("faces.models.list", &json!({})).unwrap()["enabled"] == true;
    let runtime = h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] == true;
    assert!(!enabled(&mut h));
    let click = |h: &mut Headless| {
        let r = h.request("ui.clickWidget", json!({"id": "check:settings.facesEnabled"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    click(&mut h);
    assert!(!enabled(&mut h), "nothing to run: the box stays off");
    assert!(h.app.session.execute("faces.enable", &json!({"enabled": true})).is_err(), "the engine refuses too");
    if !runtime {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // a model: installing it chooses it and switches recognition on; the box then works both ways
    let f = dir.join("m.onnx");
    std::fs::write(&f, lightcraft_faces::synthetic::tiny_embedder_model(64)).unwrap();
    h.app.session.execute("faces.models.install", &json!({"path": f.to_string_lossy(), "acknowledged": true})).unwrap();
    h.app.caches.faces_epoch += 1;
    h.step();
    h.step();
    assert!(enabled(&mut h));
    click(&mut h);
    assert!(!enabled(&mut h), "the box switches recognition off");
    click(&mut h);
    assert!(enabled(&mut h), "and on again, with a model chosen");
    let _ = std::fs::remove_dir_all(&dir);
}
