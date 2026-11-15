//! Headless tests of the side panels: dragging their inner edge resizes them within limits, and
//! the chosen widths survive switching panels and a save/load of the UI state (issue #20).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{LEFT_WIDTH, MIN_PHOTO_WIDTH, RIGHT_WIDTH};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The compositor preference changes native presentation, never idle frame scheduling.
#[cfg(target_os = "windows")]
#[test]
fn compositor_workaround_does_not_request_continuous_repaints() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let mut app = LccApp::new(lcc_engine::Session::new(), Services::default());
    app.ui.settings.compose_window = true;
    app.headless_host = false;
    let ctx = egui::Context::default();
    let mut input = egui::RawInput { focused: true, ..Default::default() };
    input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(false);
    for _ in 0..3 {
        ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
    }
    let requested = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&requested);
    ctx.set_request_repaint_callback(move |_| {
        seen.store(true, Ordering::SeqCst);
    });
    app.end_frame(&ctx, 0.0);
    assert!(!requested.load(Ordering::SeqCst));
    ctx.request_repaint();
    assert!(requested.load(Ordering::SeqCst), "the observer must detect an actual repaint request");
}

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    // these tests measure the sources list: the Navigator above it is folded
    h.app.ui.toggle_sidebar_section("panel:navigator");
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// The fork's Library draws its Classic panels on the left (Navigator, Catalog, Folders,
/// Collections); Develop draws its own Classic stack (Navigator, Presets, Versions, History,
/// Collections) and keeps upstream's reorderable sections (Albums, Local, By Date, Folders,
/// Keywords under My Photos) as one more panel of that column, hidden until it is asked for. These
/// tests are about those sections, so they ask for that panel and leave the rest of the column
/// out: the loupe with the edit panel, whatever view `ui` asks for.
fn sections_demo(size: [f32; 2], mut ui: serde_json::Value) -> Headless {
    ui["view"] = json!("detail");
    ui["right"] = json!("edit");
    let mut h = demo(size, ui);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    my_photos_only(&mut h);
    h
}

/// Leave only Develop's "My Photos" panel (upstream's sidebar) in the left column.
pub(crate) fn my_photos_only(h: &mut Headless) {
    use crate::module::PanelId;
    h.app.ui.hidden_panels = vec![PanelId::Navigator, PanelId::Presets, PanelId::Versions, PanelId::History, PanelId::Collections];
    h.settle(SETTLE);
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
    // a narrow window shrinks the panels without forgetting their width
    {
        let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "leftPanel": true, "rightWidth": 480.0, "leftWidth": 400.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
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

/// The modules that lay out their own sides (Web, Map, Print) resize them by the inner edge too,
/// into the same right width the Library and Develop panels use (so it carries across modules).
#[test]
fn module_side_columns_resize_by_their_inner_edge_and_share_the_width() {
    let mut h = demo([1400.0, 900.0], json!({}));
    for (module, panel) in [("web", "panel:web:settings"), ("map", "panel:map:side"), ("print", "panel:print:right")] {
        let r = h.request("engine.execute", json!({"command": "module.switch", "params": {"module": module}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let before = h.app.ui.right_width;
        let col = widget(&h, panel);
        assert!((col.width() - before).abs() <= 1.0, "{module}: {col:?} vs {before}");
        let grow = if before + 60.0 <= RIGHT_WIDTH.max { 60.0 } else { -60.0 };
        drag(&mut h, col.left() + 1.0, -grow);
        assert!((h.app.ui.right_width - (before + grow)).abs() <= 2.0, "{module}: {} after a drag from {before}", h.app.ui.right_width);
        assert!((widget(&h, panel).width() - h.app.ui.right_width).abs() <= 1.0, "{module}");
    }
}

/// The left sidebar's Albums, Local, By Date and Keywords headers fold their sections; the choice
/// is part of the saved UI state.
#[test]
fn sidebar_sections_collapse_and_remember_it() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true, "right": "none"}));
    // the Keyword List alone in Library's right column, at its top
    h.app.ui.hidden_panels = vec![crate::module::PanelId::QuickDevelop, crate::module::PanelId::Keywording];
    h.step();
    h.step();
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
    assert!(h.app.ui.sidebar_section_collapsed("byDate") && !h.app.ui.sidebar_section_collapsed("panel:collections"));
    assert!(has(&h, "sidebarSection:byDate"), "the header stays so it can be reopened");
    // the choice survives a save/load of the UI state
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert!(back.sidebar_section_collapsed("byDate"));
    click(&mut h, "sidebarSection:byDate");
    assert!(year_rows(&h) > 0, "unfolded again");
    // Albums folds too, and the plus button inside its header still works on its own
    click(&mut h, "classicPanel:collections");
    assert!(h.app.ui.sidebar_section_collapsed("panel:collections"));
    assert!(has(&h, "icon:albumNew"), "the Create Album button stays in the header");
    click(&mut h, "classicPanel:collections");
    assert!(!h.app.ui.sidebar_section_collapsed("panel:collections"));
    // Keywords and Local fold their rows too
    // (the Keyword List header stays a drop target for the top level while folded: not a row)
    let rows = |h: &Headless, prefix: &str| h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix) && w != "keywordList:topLevel").count();
    assert!(rows(&h, "keywordList:") > 0, "the demo library has keywords");
    click(&mut h, "classicPanel:keywordList");
    assert_eq!(rows(&h, "keywordList:"), 0, "keywords folded");
    click(&mut h, "classicPanel:keywordList");
    assert!(rows(&h, "keywordList:") > 0);
    if has(&h, "sidebarSection:local") {
        click(&mut h, "sidebarSection:local");
        assert_eq!(rows(&h, "source:local:"), 0, "local folded");
        assert!(!has(&h, "source:local:browse"), "Browse Folder… folds with it");
        click(&mut h, "sidebarSection:local");
        assert!(!h.app.ui.sidebar_section_collapsed("local"));
    }
    // a click on the plus is the button's, not the header's
    click(&mut h, "icon:albumNew");
    assert!(!h.app.ui.sidebar_section_collapsed("panel:collections"), "the plus does not fold Collections");
}

/// Given a settings file from before sections could be arranged (it lists the folded ones as
/// `collapsedSidebar`), when it is loaded, then those sections are still folded, and what is
/// saved from then on is the arrangement alone.
#[test]
fn folded_sections_of_an_older_settings_file_stay_folded() {
    let old = json!({"leftPanel": true, "collapsedSidebar": ["byDate", "keywords", "no such section"]});
    let ui = serde_json::from_value::<crate::UiState>(old).unwrap().sanitized();
    assert!(ui.sidebar_section_collapsed("byDate") && ui.sidebar_section_collapsed("keywords") && !ui.sidebar_section_collapsed("albums"));
    let saved = serde_json::to_value(&ui).unwrap();
    assert!(saved.get("collapsedSidebar").is_none(), "{saved}");
    assert_eq!(saved["sidebar"][2], json!({"id": "byDate", "hidden": false, "collapsed": true}));
}

/// Given a settings file whose sidebar arrangement is damaged, when it is loaded, then the rest of
/// the settings load and the sidebar draws every section.
#[test]
fn a_damaged_sidebar_arrangement_does_not_lose_the_settings() {
    for sidebar in [json!("oops"), json!([{"id": "folders"}, {"id": "folders"}, 7, {"id": "ghost"}]), json!({"albums": 1})] {
        let saved = json!({"leftPanel": true, "leftWidth": 300.0, "sidebar": sidebar});
        let ui = serde_json::from_value::<crate::UiState>(saved.clone()).unwrap().sanitized();
        assert_eq!(ui.left_width, 300.0, "{saved}");
        assert_eq!(ui.sidebar.sections().len(), crate::sidebar::SidebarSection::ALL.len(), "{saved}");
    }
}

/// Put the sidebar back to its usual arrangement: every section shown and open, in the usual order.
fn usual_sidebar(h: &mut Headless) {
    // (refused when there is nothing to reset: the sidebar is the usual one either way)
    h.request("engine.execute", json!({"command": "view.sidebarReset", "params": {}}), T);
    h.step();
    h.step();
    assert!(h.app.ui.sidebar.is_default(), "the usual arrangement");
}

/// The sections drawn in the sidebar, top to bottom.
fn sections_drawn(h: &Headless) -> Vec<String> {
    let mut drawn: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(w, r)| w.strip_prefix("sidebarSection:").map(|id| (r.top(), id.to_string()))).collect();
    drawn.sort_by(|a, b| a.0.total_cmp(&b.0));
    drawn.into_iter().map(|(_, id)| id).collect()
}

/// One sidebar through its arrangement: an order and hidden sections are drawn as arranged; a
/// section is hidden and shown from Window ▸ Sidebar Sections, from its header and from the My
/// Photos title; Reset brings every section back; hiding a section leaves what the grid shows,
/// and a new album brings a hidden Albums section back.
#[test]
fn sidebar_sections_are_arranged_hidden_shown_and_reset() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    // Reset Sidebar Sections is only offered when there is something to reset, and shows and opens every section again
    {
        let reset_enabled = |h: &Headless| {
            let bar = crate::menubar::menu_bar(&h.app);
            let json = serde_json::to_string(&bar).unwrap();
            // the item as the menu model describes it
            let at = json.find("view.sidebarReset").expect("the reset item");
            json[at..].split("enabled\":").nth(1).is_some_and(|rest| rest.starts_with("true"))
        };
        assert!(!reset_enabled(&h), "nothing to reset yet");
        exec(&mut h, "view.sidebarSection", json!({"section": "albums", "show": false}));
        exec(&mut h, "view.sidebarSection", json!({"section": "keywords", "collapsed": true}));
        assert!(reset_enabled(&h));
        assert!(!has(&h, "sidebarSection:albums"));
        exec(&mut h, "view.sidebarReset", json!({}));
        assert!(h.app.ui.sidebar.is_default());
        assert!(has(&h, "sidebarSection:albums") && !h.app.ui.sidebar_section_collapsed("keywords"));
    }
    // the sections are drawn in the order arranged, a hidden one not at all
    {
        // (Local is not on every platform)
        let listed = |drawn: &[String]| drawn.iter().filter(|s| s.as_str() != "local").cloned().collect::<Vec<_>>();
        let usual = sections_drawn(&h);
        assert_eq!(listed(&usual), ["albums", "byDate", "folders", "keywords"], "{usual:?}");
        // the demo library was imported from no folder: Folders says so in place of rows
        assert!(has(&h, "sidebarEmpty:folders") && !has(&h, "sidebarEmpty:keywords"));
        let r = h.request("ui.set", json!({"sidebar": [{"id": "keywords"}, {"id": "byDate", "hidden": true}, {"id": "albums"}]}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let drawn = sections_drawn(&h);
        assert_eq!(listed(&drawn), ["keywords", "albums", "folders"], "{drawn:?}");
        assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("source:date:")), "a hidden section draws no rows");
    }
    usual_sidebar(&mut h);
    // unchecked in Window ▸ Sidebar Sections a section leaves the sidebar, and comes back as it was; kept with the UI state
    {
        let checked = |h: &Headless, id: &str| sidebar_menu_items(h).into_iter().find(|(s, _)| s == id).unwrap_or_else(|| panic!("no {id} item")).1;
        assert_eq!(checked(&h, "byDate"), Some(true));
        assert!(has(&h, "sidebarSection:byDate"));
        // folded first: it must come back folded
        click(&mut h, "sidebarSection:byDate");
        let r = exec(&mut h, "view.sidebarSection", json!({"section": "byDate"}));
        assert_eq!(r, json!({"section": "byDate", "show": false, "collapsed": true}));
        assert!(!has(&h, "sidebarSection:byDate"), "the header is gone");
        assert_eq!(checked(&h, "byDate"), Some(false));
        assert_eq!(checked(&h, "albums"), Some(true), "the others stay");
        // saved and read back
        let back = serde_json::from_value::<crate::UiState>(serde_json::to_value(&h.app.ui).unwrap()).unwrap().sanitized();
        assert!(back.sidebar.is_hidden(crate::sidebar::SidebarSection::ByDate));
        let r = exec(&mut h, "view.sidebarSection", json!({"section": "byDate"}));
        assert_eq!(r["show"], true);
        assert!(has(&h, "sidebarSection:byDate") && h.app.ui.sidebar_section_collapsed("byDate"), "back, still folded");
        // saying what is wanted, instead of flipping it: showing a shown section changes nothing
        let r = exec(&mut h, "view.sidebarSection", json!({"section": "byDate", "show": true, "collapsed": false}));
        assert_eq!(r, json!({"section": "byDate", "show": true, "collapsed": false}));
        assert!(h.app.widgets.iter().any(|(w, _)| w.starts_with("source:date:")), "open again: its rows are drawn");
    }
    usual_sidebar(&mut h);
    // hidden from its header's menu, brought back from the menu of the My Photos title, which is always there
    {
        right_click(&mut h, "sidebarSection:keywords");
        assert!(popup_open(&h), "the header has a menu");
        click(&mut h, "sidebarMenu:hide");
        assert!(!has(&h, "sidebarSection:keywords") && !popup_open(&h));
        assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("source:keyword:")), "its rows went with it");
        right_click(&mut h, "sidebarTitle");
        assert!(popup_open(&h), "the title has the menu too");
        assert!(!has(&h, "sidebarMenu:hide"), "the title is no section: nothing to hide");
        click(&mut h, "sidebarMenu:section:keywords");
        assert!(has(&h, "sidebarSection:keywords"));
        // …and from there every section can go back to the usual arrangement
        exec(&mut h, "view.sidebarSection", json!({"section": "byDate", "show": false}));
        right_click(&mut h, "sidebarTitle");
        click(&mut h, "sidebarMenu:reset");
        assert!(h.app.ui.sidebar.is_default());
    }
    usual_sidebar(&mut h);
    // hiding Albums while an album is shown leaves the grid showing the album
    {
        let album = h.app.session.execute("album.create", &json!({"name": "Kept"})).unwrap()["id"].as_u64().unwrap();
        exec(&mut h, "library.source", json!({"kind": "album", "id": album}));
        let shown = h.app.session.source;
        exec(&mut h, "view.sidebarSection", json!({"section": "albums", "show": false}));
        assert!(!has(&h, &format!("source:album:{album}")));
        assert_eq!(h.app.session.source, shown);
    }
    // an album made while Albums is hidden (and folded) brings Albums back, open, so it can be seen
    {
        exec(&mut h, "view.sidebarSection", json!({"section": "albums", "show": false, "collapsed": true}));
        exec(&mut h, "dialog.newAlbum", json!({"name": "Fresh"}));
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let id = h.app.session.catalog.albums().find(|a| a.name == "Fresh").expect("the album was made").id.0;
        assert!(has(&h, "sidebarSection:albums") && has(&h, &format!("source:album:{id}")));
    }
}

/// Run a command that must succeed; its result.
fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

/// The Window ▸ Sidebar Sections items: (section id or command, checked).
fn sidebar_menu_items(h: &Headless) -> Vec<(String, Option<bool>)> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(&h.app);
    let view = &bar.iter().find(|(t, _)| t == "Window").expect("Window menu").1;
    let children = view
        .iter()
        .find_map(|n| match n {
            MenuNode::Submenu { label, children } if label == "Sidebar Sections" => Some(children.clone()),
            _ => None,
        })
        .expect("Window ▸ Sidebar Sections");
    children
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { id, params, checked, .. } => Some((params["section"].as_str().unwrap_or(id).to_string(), *checked)),
            _ => None,
        })
        .collect()
}

/// A command that names no section, an unknown one, or a flag that is no true / false is refused
/// with what it takes, and changes nothing.
#[test]
fn sidebar_commands_refuse_what_they_cannot_read() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    for (params, says) in [
        (json!({}), "`section` is needed (albums|local|byDate|folders|keywords)"),
        (json!({"section": 3}), "`section` is needed"),
        (json!({"section": "map"}), "unknown section `map`"),
        (json!({"section": "albums", "show": "no"}), "`show` must be true or false"),
        (json!({"section": "albums", "collapsed": 1}), "`collapsed` must be true or false"),
    ] {
        let r = h.request("engine.execute", json!({"command": "view.sidebarSection", "params": params}), T);
        assert_eq!(r["ok"], false, "{params}: {r}");
        assert!(r.to_string().contains(says), "{params}: {r}");
    }
    assert!(h.app.ui.sidebar.is_default());
    // an arrangement set through the control channel is checked: a wrong one is refused and leaves the sidebar as it was; the older `collapsedSidebar` list still folds and unfolds
    {
        exec(&mut h, "view.sidebarSection", json!({"section": "keywords", "show": false}));
        let arranged = h.app.ui.sidebar.clone();
        for bad in [json!(null), json!({"albums": 1}), json!([{"id": "keywrods", "hidden": true}]), json!(["albums", "albums"])] {
            let r = h.request("ui.set", json!({"sidebar": bad}), T);
            assert_eq!(r["ok"], false, "{bad}: {r}");
            assert_eq!(h.app.ui.sidebar, arranged, "{bad}");
        }
        let r = h.request("ui.set", json!({"collapsedSidebar": ["byDate"]}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.sidebar_section_collapsed("byDate") && !h.app.ui.sidebar_section_collapsed("albums"));
        assert!(h.app.ui.sidebar.is_hidden(crate::sidebar::SidebarSection::Keywords), "the rest of the arrangement stays");
        let r = h.request("ui.set", json!({"collapsedSidebar": []}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(!h.app.ui.sidebar_section_collapsed("byDate"));
        // a name it doesn't know, or both ways of saying it at once, is refused and changes nothing
        exec(&mut h, "view.sidebarSection", json!({"section": "albums", "collapsed": true}));
        let arranged = h.app.ui.sidebar.clone();
        for bad in [json!({"collapsedSidebar": ["albms"]}), json!({"collapsedSidebar": [], "sidebar": [{"id": "albums", "collapsed": true}]})] {
            let r = h.request("ui.set", bad.clone(), T);
            assert_eq!(r["ok"], false, "{bad}: {r}");
            assert_eq!(h.app.ui.sidebar, arranged, "{bad}");
        }
    }
}

/// Press on `from`, move to `to` over a few frames, and release there (after `before_release`).
fn drag_between(h: &mut Headless, from: egui::Pos2, to: egui::Pos2, before_release: Vec<egui::Event>) {
    let m = egui::Modifiers::NONE;
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    h.app.synthetic.push(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    h.step();
    for e in before_release {
        h.app.synthetic.push(e);
        h.step();
    }
    h.app.synthetic.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    h.step();
    h.step();
    h.step();
}

fn header(h: &Headless, id: &str) -> egui::Rect {
    widget(h, &format!("sidebarSection:{id}"))
}

/// Given the sidebar, when a section's header is dragged above another section and released,
/// then the section is listed there, the others keeping their order; while it is dragged a line
/// shows where it would go; and the new order is part of the saved UI state.
#[test]
fn dragging_a_header_moves_its_section() {
    // folders too: photos imported from a folder
    let mut h = folders_sections_app(&["/pics/trip/a.jpg"], [1400.0, 1400.0]);
    let usual = sections_drawn(&h);
    let with_local = usual.contains(&"local".to_string());
    let listed = |ids: &[&str]| ids.iter().filter(|s| with_local || **s != "local").map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(usual, listed(&["albums", "local", "byDate", "folders", "keywords"]));
    assert!(has(&h, "sidebarEmpty:keywords"), "no keywords in this library: the section says so and stays");
    // Folders, dragged to just above Albums
    let (from, to) = (header(&h, "folders").center(), header(&h, "albums").center() - egui::vec2(0.0, 12.0));
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    h.step();
    assert_eq!(h.app.ui.dragging_section, Some(crate::sidebar::SidebarSection::Folders));
    let line = widget(&h, "sidebarSectionDrop");
    assert!(
        line.center().y < header(&h, "albums").top() && line.center().y > header(&h, "albums").top() - 10.0,
        "the line is in the gap above Albums: {line:?}"
    );
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
    h.step();
    assert_eq!(h.app.ui.dragging_section, None);
    assert!(!has(&h, "sidebarSectionDrop"));
    assert_eq!(sections_drawn(&h), listed(&["folders", "albums", "local", "byDate", "keywords"]));
    // dragged below everything: last
    // (above the column's Import / Export bar when it has one, #118: the bar is not part of the list)
    let foot =
        h.app.widgets.iter().find(|(w, _)| w.starts_with("button:leftBar-")).map_or(widget(&h, "panel:left_panel").bottom(), |(_, r)| r.top() - 11.0);
    let end = egui::pos2(header(&h, "keywords").center().x, foot - 20.0);
    let from = header(&h, "albums").center();
    drag_between(&mut h, from, end, Vec::new());
    assert_eq!(sections_drawn(&h), listed(&["folders", "local", "byDate", "keywords", "albums"]));
    // kept with the UI state
    let back = serde_json::from_value::<crate::UiState>(serde_json::to_value(&h.app.ui).unwrap()).unwrap().sanitized();
    assert_eq!(back.sidebar, h.app.ui.sidebar);
    // the header still folds its section with a click
    click(&mut h, "sidebarSection:folders");
    assert!(h.app.ui.sidebar_section_collapsed("folders"));
}

/// A drag that ends with Esc, or is released away from the sidebar, moves nothing.
#[test]
fn a_section_drag_that_is_given_up_moves_nothing() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    let usual = sections_drawn(&h);
    let (from, to) = (header(&h, "keywords").center(), header(&h, "albums").center() - egui::vec2(0.0, 12.0));
    let esc = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    drag_between(&mut h, from, to, vec![esc]);
    assert_eq!((sections_drawn(&h), h.app.ui.dragging_section), (usual.clone(), None), "Esc");
    // released over the photos
    drag_between(&mut h, from, egui::pos2(800.0, to.y), Vec::new());
    assert_eq!((sections_drawn(&h), h.app.ui.dragging_section), (usual.clone(), None), "released outside");
    assert!(h.app.ui.sidebar.is_default());
}

/// An album dragged to the Albums header is not a section drag, and the other way round.
#[test]
fn dragging_a_section_is_not_dragging_an_album() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    let (from, to) = (header(&h, "albums").center(), header(&h, "keywords").center() + egui::vec2(0.0, 4.0));
    let before = h.app.session.catalog.revision;
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * 0.5));
    h.step();
    h.step();
    assert!(h.app.ui.dragging_section.is_some() && h.app.ui.dragging_album.is_none());
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
    assert_eq!(h.app.session.catalog.revision, before, "no album moved");
    assert!(!h.app.ui.sidebar.is_default(), "the section did");
    // …and an album dragged onto another section's header moves no section
    let album = h.app.session.catalog.albums().find(|a| !a.folder).expect("the demo library has albums").id.0;
    let arranged = h.app.ui.sidebar.clone();
    let (from, to) = (widget(&h, &format!("source:album:{album}")).center(), header(&h, "keywords").center());
    drag_between(&mut h, from, to, Vec::new());
    assert_eq!((h.app.ui.sidebar.clone(), h.app.ui.dragging_section), (arranged, None));
}

/// Given a section's header menu, Move Up and Move Down move it one shown section at a time
/// (stepping over hidden ones), and the command says when there is nowhere to go.
#[test]
fn a_section_moves_up_and_down_from_its_menu() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    // (Local is not on every platform: taken out, so the same sections are there everywhere)
    exec(&mut h, "view.sidebarSection", json!({"section": "local", "show": false}));
    exec(&mut h, "view.sidebarSection", json!({"section": "folders", "show": false}));
    assert_eq!(sections_drawn(&h), ["albums", "byDate", "keywords"]);
    right_click(&mut h, "sidebarSection:keywords");
    click(&mut h, "sidebarMenu:moveUp");
    assert_eq!(sections_drawn(&h), ["albums", "keywords", "byDate"]);
    right_click(&mut h, "sidebarSection:albums");
    click(&mut h, "sidebarMenu:moveDown");
    assert_eq!(sections_drawn(&h), ["keywords", "albums", "byDate"], "one step, over the hidden sections");
    // the ends: nowhere to go
    let r = exec(&mut h, "view.sidebarMoveSection", json!({"section": "keywords", "direction": "up"}));
    assert_eq!(r["moved"], false, "{r}");
    let r = exec(&mut h, "view.sidebarMoveSection", json!({"section": "byDate", "direction": "down"}));
    assert_eq!(r["moved"], false, "{r}");
    // above a named section, or last
    let r = exec(&mut h, "view.sidebarMoveSection", json!({"section": "byDate", "before": "keywords"}));
    assert_eq!(r["moved"], true, "{r}");
    assert_eq!(sections_drawn(&h), ["byDate", "keywords", "albums"]);
    exec(&mut h, "view.sidebarMoveSection", json!({"section": "byDate", "before": null}));
    assert_eq!(sections_drawn(&h), ["keywords", "albums", "byDate"]);
    assert_eq!(h.app.ui.sidebar.sections().last().map(|s| s.id.id()), Some("byDate"));
    for (params, says) in [
        (json!({"section": "albums"}), "give either `direction`"),
        (json!({"section": "albums", "direction": "up", "before": null}), "give either `direction`"),
        (json!({"section": "albums", "direction": "left"}), "`direction` must be"),
        (json!({"section": "albums", "before": "map"}), "`before` must be a section"),
        (json!({"direction": "up"}), "`section` is needed"),
    ] {
        let r = h.request("engine.execute", json!({"command": "view.sidebarMoveSection", "params": params}), T);
        assert_eq!(r["ok"], false, "{params}: {r}");
        assert!(r.to_string().contains(says), "{params}: {r}");
    }
}

/// Where a dragged section goes: above the first section whose middle is below the pointer.
#[test]
fn a_dragged_section_goes_above_the_section_it_is_over_the_top_half_of() {
    use crate::panels::left::{PlacedSection, section_drop_before};
    use crate::sidebar::SidebarSection::{Albums, ByDate, Keywords};
    let placed = [
        PlacedSection { section: Albums, top: 100.0, bottom: 300.0, open: true, rows: 1 },
        PlacedSection { section: ByDate, top: 310.0, bottom: 344.0, open: true, rows: 1 },
        PlacedSection { section: Keywords, top: 354.0, bottom: 500.0, open: true, rows: 1 },
    ];
    for (y, before) in [
        (0.0, Some(Albums)),
        (199.0, Some(Albums)),
        (201.0, Some(ByDate)),
        (326.0, Some(ByDate)),
        (328.0, Some(Keywords)),
        (430.0, None),
        (9e9, None),
    ] {
        assert_eq!(section_drop_before(&placed, y), before, "{y}");
    }
    assert_eq!(section_drop_before(&[], 10.0), None);
    assert_eq!(section_drop_before(&placed, f32::NAN), None);
}

/// Given a section longer than the sidebar, when its header scrolls out of sight, then a copy of
/// the header stays at the top of the sidebar for as long as the section's rows are there; a
/// click on it folds the section and brings the section's own header into view.
#[test]
fn the_header_of_a_long_section_stays_in_sight() {
    let mut h = sections_demo([1400.0, 700.0], json!({"view": "photoGrid", "leftPanel": true}));
    for i in 0..40 {
        h.app.session.execute("album.create", &json!({"name": format!("Album {i:02}")})).unwrap();
    }
    h.step();
    h.step();
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("sidebarSectionPinned:")), "nothing is pinned while every header is in sight");
    let own = header(&h, "albums");
    let c = egui::pos2(own.center().x, 400.0);
    let scroll = |h: &mut Headless, dy: f32| {
        // the wheel scrolls what the pointer is over
        h.request("ui.move", json!({"x": c.x, "y": c.y}), T);
        let r = h.request("ui.scroll", json!({"dy": dy}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..60 {
            h.step();
        }
    };
    scroll(&mut h, -500.0);
    assert!(header(&h, "albums").top() < own.top() - 300.0, "the sidebar scrolled");
    let pinned = widget(&h, "sidebarSectionPinned:albums");
    let first_row = h
        .app
        .widgets
        .iter()
        .filter(|(w, r)| w.starts_with("source:album:") && r.bottom() > pinned.bottom())
        .map(|(_, r)| r.top())
        .fold(f32::MAX, f32::min);
    assert!(
        pinned.top() > header(&h, "albums").bottom() && first_row < pinned.bottom() + 29.0,
        "at the top of the rows in view: {pinned:?} {first_row}"
    );
    assert!(h.app.widgets.iter().any(|(w, r)| w == "icon:albumNew" && pinned.contains_rect(*r)), "with its buttons");
    // folded from the pinned header: Albums' own header is what the sidebar shows now
    click(&mut h, "sidebarSectionPinned:albums");
    h.settle(SETTLE);
    assert!(h.app.ui.sidebar_section_collapsed("albums"));
    assert!(!has(&h, "sidebarSectionPinned:albums"), "a folded section has no rows to pin its header over");
    let own = header(&h, "albums");
    assert!((own.top() - pinned.top()).abs() < 1.0, "its own header is where the pinned one was, at the top of the list: {own:?} {pinned:?}");
}

/// Scroll the sidebar (the pointer over it) by `dy`.
pub(crate) fn scroll_sidebar(h: &mut Headless, dy: f32) {
    h.request("ui.move", json!({"x": 120.0, "y": 400.0}), T);
    let r = h.request("ui.scroll", json!({"dy": dy}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..60 {
        h.step();
    }
}

/// Given a pinned Albums header with album folders scrolled beneath it, when an album is dragged
/// onto the pinned header and released, then it goes to the top level — what the header shows —
/// and not into the folder hidden under the header.
#[test]
fn a_drop_on_the_pinned_header_is_not_a_drop_on_the_row_beneath_it() {
    let mut h = sections_demo([1400.0, 700.0], json!({"view": "photoGrid", "leftPanel": true}));
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    for i in 0..30 {
        make(&mut h, json!({"name": format!("Folder {i:02}"), "folder": true}));
    }
    let last = make(&mut h, json!({"name": "Zz last", "folder": true}));
    let best = make(&mut h, json!({"name": "Best", "parent": last}));
    h.step();
    h.step();
    let parent = |h: &Headless| h.app.session.catalog.album(lcc_catalog::AlbumId(best)).unwrap().parent.map(|p| p.0);
    let row = |h: &Headless| widget(h, &format!("source:album:{best}"));
    // far enough that Best is in view and Albums' own header is not
    for _ in 0..8 {
        if row(&h).bottom() < 600.0 {
            break;
        }
        scroll_sidebar(&mut h, -200.0);
    }
    let pinned = widget(&h, "sidebarSectionPinned:albums");
    assert!(row(&h).bottom() < 600.0 && row(&h).top() > pinned.bottom(), "Best is in view below the pinned header: {:?}", row(&h));
    let beneath = h.app.widgets.iter().filter(|(w, r)| w.starts_with("source:folder:") && r.contains(pinned.center())).count();
    assert_eq!(beneath, 1, "a folder row lies under the middle of the pinned header");
    let from = row(&h).center();
    drag_between(&mut h, from, pinned.center(), Vec::new());
    assert_eq!(parent(&h), None, "to the top level, as the header said");
}

/// Given a long section scrolled so that its header is pinned, when another section is dragged up
/// to the top edge, then the sidebar scrolls up to reach the sections above, although the pointer
/// is over the pinned header there. A section picked up by the pinned header itself does not
/// start scrolling until the pointer has left it.
#[test]
fn a_section_dragged_to_the_top_edge_scrolls_past_the_pinned_header() {
    let mut h = sections_demo([1400.0, 700.0], json!({"view": "photoGrid", "leftPanel": true}));
    for i in 0..40 {
        h.app.session.execute("album.create", &json!({"name": format!("Album {i:02}")})).unwrap();
    }
    h.step();
    let press = |h: &mut Headless, at: egui::Pos2| {
        h.app.synthetic.push(egui::Event::PointerMoved(at));
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    };
    let hold = |h: &mut Headless, at: egui::Pos2, frames: usize| {
        for _ in 0..frames {
            h.app.synthetic.push(egui::Event::PointerMoved(at));
            h.step();
        }
    };
    let give_up = |h: &mut Headless| {
        h.app.synthetic.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(800.0, 400.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.step();
    };
    // Local's header is in view below the albums, Albums' is pinned
    for _ in 0..12 {
        if has(&h, "sidebarSectionPinned:albums") && header(&h, "byDate").top() < 600.0 {
            break;
        }
        scroll_sidebar(&mut h, -200.0);
    }
    let pinned = widget(&h, "sidebarSectionPinned:albums");
    let own = |h: &Headless| header(h, "albums").top();
    // picked up by the pinned header: held inside it, nothing scrolls
    let start = own(&h);
    press(&mut h, pinned.center());
    hold(&mut h, pinned.center() + egui::vec2(12.0, 3.0), 12);
    assert_eq!(h.app.ui.dragging_section, Some(crate::sidebar::SidebarSection::Albums));
    assert_eq!(own(&h), start, "still on the header it was picked up by");
    give_up(&mut h);
    // By Date, picked up by its own header and taken to the top edge: the sidebar scrolls up
    let start = own(&h);
    let from = header(&h, "byDate").center();
    press(&mut h, from);
    hold(&mut h, from - egui::vec2(0.0, 60.0), 3);
    hold(&mut h, pinned.center(), 30);
    assert!(own(&h) > start + 20.0, "scrolled towards the sections above: {start} -> {}", own(&h));
    give_up(&mut h);
    assert!(h.app.ui.sidebar.is_default());
}

/// The pinned header belongs to the section whose rows are at the top of the sidebar: scrolling on
/// into the next section pushes it out and pins that section's header instead.
#[test]
fn the_next_section_takes_over_the_pinned_header() {
    let mut h = sections_demo([1400.0, 500.0], json!({"view": "photoGrid", "leftPanel": true, "sidebar": ["albums", "keywords"]}));
    exec(&mut h, "view.sidebarSection", json!({"section": "local", "show": false}));
    exec(&mut h, "view.sidebarSection", json!({"section": "byDate", "show": false}));
    exec(&mut h, "view.sidebarSection", json!({"section": "folders", "show": false}));
    for i in 0..12 {
        h.app.session.execute("album.create", &json!({"name": format!("Album {i:02}")})).unwrap();
    }
    h.step();
    let pinned = |h: &Headless| -> Vec<String> {
        h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("sidebarSectionPinned:").map(str::to_string)).collect()
    };
    let mut seen = Vec::new();
    for _ in 0..30 {
        scroll_sidebar(&mut h, -60.0);
        let now = pinned(&h);
        assert!(now.len() <= 1, "one header at a time: {now:?}");
        if let Some(id) = now.first().filter(|id| seen.last() != Some(*id)) {
            seen.push(id.clone());
        }
    }
    assert_eq!(seen, ["albums", "keywords"]);
}

/// Given the Keywords header, when its + is clicked, then Create Keyword Tag opens (for a keyword
/// at the usual place, whatever is picked in the Keyword List) and the section does not fold;
/// and the keyword made there is listed in the section.
#[test]
fn the_keywords_header_creates_a_keyword() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    // something picked in the Keyword List: not where a keyword made from the sidebar goes
    h.app.ui.keyword_list_selected = Some("aurora".into());
    let (button, head) = (widget(&h, "icon:keywordNew"), header(&h, "keywords"));
    assert!(head.contains_rect(button), "the + is in the header: {button:?} in {head:?}");
    click(&mut h, "icon:keywordNew");
    assert!(!h.app.ui.sidebar_section_collapsed("keywords"), "the + does not fold Keywords");
    let Some(crate::state::Dialog::KeywordTag { editing, name, parent, .. }) = h.app.ui.dialog.as_mut() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((editing.clone(), parent.clone()), (None, None));
    *name = "zebra crossing".into();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(&h, "source:keyword:zebra crossing"), "the new keyword is listed");
    // the sidebar scrolls to it: it is listed last, below what the panel shows
    h.settle(SETTLE);
    let (row, panel) = (widget(&h, "source:keyword:zebra crossing"), widget(&h, "panel:left_panel"));
    assert!(panel.contains_rect(row), "the new keyword is in sight: {row:?} in {panel:?}");
}

/// A folded section's row count sits to the left of its header button; a section without a button
/// has its count at the edge.
#[test]
fn a_folded_section_shows_its_count_beside_its_header_button() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    for (section, button) in [("keywords", "icon:keywordNew"), ("albums", "icon:albumNew")] {
        click(&mut h, &format!("sidebarSection:{section}"));
        assert!(h.app.ui.sidebar_section_collapsed(section));
        let (count, plus) = (widget(&h, &format!("sidebarSectionCount:{section}")), widget(&h, button));
        assert!(count.right() <= plus.left(), "{section}: the count {count:?} is left of the button {plus:?}");
    }
    // By Date has no button: its count goes where the others have theirs
    click(&mut h, "sidebarSection:byDate");
    let (plain, beside_button) = (widget(&h, "sidebarSectionCount:byDate"), widget(&h, "sidebarSectionCount:keywords"));
    assert!(plain.right() >= widget(&h, "icon:keywordNew").left() && plain.right() > beside_button.right() + 20.0, "{plain:?} {beside_button:?}");
    // a folded section says how many rows it holds (unless photo counts are off); an open one
    // doesn't need to
    usual_sidebar(&mut h);
    assert!(!has(&h, "sidebarSectionCount:byDate"));
    click(&mut h, "sidebarSection:byDate");
    assert!(has(&h, "sidebarSectionCount:byDate"));
    exec(&mut h, "view.photoCounts", json!({"show": false}));
    assert!(!has(&h, "sidebarSectionCount:byDate"));
    // an empty section has nothing to count
    exec(&mut h, "view.photoCounts", json!({"show": true}));
    click(&mut h, "sidebarSection:folders");
    assert!(h.app.ui.sidebar_section_collapsed("folders") && !has(&h, "sidebarSectionCount:folders"));
}

/// Given Keywords is folded and new keywords go inside a parent, when a keyword is made with the
/// header's +, then Keywords opens, and so does the parent, so the new keyword is there to see.
/// A keyword made elsewhere (the Keyword List) leaves the sidebar as it is.
#[test]
fn a_keyword_made_from_the_header_is_shown() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    exec(&mut h, "keyword.create", json!({"name": "places", "parent": null}));
    exec(&mut h, "keyword.setDefaultParent", json!({"keyword": "places"}));
    click(&mut h, "sidebarSection:keywords");
    assert!(h.app.ui.sidebar_section_collapsed("keywords"));
    let confirm = |h: &mut Headless, name: &str| {
        let Some(crate::state::Dialog::KeywordTag { name: n, .. }) = h.app.ui.dialog.as_mut() else { panic!("{:?}", h.app.ui.dialog) };
        *n = name.into();
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    // from the Keyword List: nothing is revealed
    h.app.ui.dialog = Some(crate::panels::keyword_list::create_dialog(&h.app));
    confirm(&mut h, "harbour");
    assert!(h.app.ui.sidebar_section_collapsed("keywords") && !has(&h, "source:keyword:places|harbour"));
    // from the header's +
    click(&mut h, "icon:keywordNew");
    confirm(&mut h, "lighthouse");
    assert!(!h.app.ui.sidebar_section_collapsed("keywords"), "Keywords opened");
    assert!(has(&h, "source:keyword:places|lighthouse"), "and its parent, down to the new keyword");
}

/// A press on a header's button that turns into a drag is not a drag of the section.
#[test]
fn dragging_from_a_header_button_does_not_drag_the_section() {
    let mut h = sections_demo([1400.0, 1400.0], json!({"view": "photoGrid", "leftPanel": true}));
    for button in ["icon:keywordNew", "icon:albumNew"] {
        let from = widget(&h, button).center();
        h.app.synthetic.push(egui::Event::PointerMoved(from));
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        for i in 1..=6 {
            h.app.synthetic.push(egui::Event::PointerMoved(from - egui::vec2(8.0 * i as f32, 30.0 * i as f32)));
            h.step();
            assert_eq!(h.app.ui.dragging_section, None, "{button}");
        }
        let to = from - egui::vec2(48.0, 180.0);
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.step();
    }
    assert!(h.app.ui.sidebar.is_default());
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
    // Alt-click on a triangle folds or opens the folder and everything inside it; a plain click still
    // flips only that row
    toggle(&mut h, europe);
    assert!(has(&h, &format!("source:album:{best}")));
    let click = |h: &mut Headless, id: u64, alt: bool| {
        let r = h.request("ui.clickWidget", json!({"id": format!("albumToggle:{id}"), "alt": alt}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..6 {
            h.step();
        }
    };
    click(&mut h, trips, true);
    assert!(!has(&h, &format!("source:folder:{europe}")), "the whole branch is folded");
    click(&mut h, trips, false);
    assert!(has(&h, &format!("source:folder:{europe}")) && !has(&h, &format!("source:album:{best}")), "the inner folder was folded too");
    click(&mut h, trips, true);
    click(&mut h, trips, true);
    assert!(has(&h, &format!("source:album:{best}")), "Alt-click on a folded folder opens everything inside it");
}

/// Survey's bottom bar has a filmstrip size slider that sets the filmstrip height within 100-240.
#[test]
fn survey_filmstrip_size_slider_sets_the_filmstrip_height() {
    let mut h = demo([1400.0, 900.0], json!({"view": "survey"}));
    h.step();
    h.step();
    let r = h.app.widgets.iter().find(|(w, _)| w == "slider:filmSize").map(|(_, r)| *r).expect("the slider shows in Survey");
    let y = r.center().y;
    let r2 = h.request("ui.click", json!({"x": r.right() - 0.5, "y": y}), T);
    assert_eq!(r2["ok"], true, "{r2}");
    h.step();
    assert!((h.app.ui.film_height - 240.0).abs() < 8.0, "{}", h.app.ui.film_height);
    let r3 = h.request("ui.click", json!({"x": r.left() + 0.5, "y": y}), T);
    assert_eq!(r3["ok"], true, "{r3}");
    h.step();
    assert!(h.app.ui.film_height <= 108.0 && h.app.ui.film_height >= 96.0, "{}", h.app.ui.film_height);
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
    let parent_of = |h: &Headless, id: u64| h.app.session.catalog.album(lcc_catalog::AlbumId(id)).unwrap().parent.map(|p| p.0);
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
    assert!(h.app.session.catalog.album(lcc_catalog::AlbumId(folder)).unwrap().folder);
    let smart = create(&mut h, "dialog.smartAlbum", "Rated", Some(folder));
    assert_eq!(parent_of(&h, smart), Some(folder));
    assert!(h.app.session.catalog.album(lcc_catalog::AlbumId(smart)).unwrap().is_smart());
    let from_view = create(&mut h, "dialog.newSmartAlbum", "From view", Some(trips));
    assert_eq!(parent_of(&h, from_view), Some(trips));
    // without a folder they are still made at the top level
    let top = create(&mut h, "dialog.newAlbum", "Loose", None);
    assert_eq!(parent_of(&h, top), None);
    // a dialog that cannot do its job (the chosen parent is a plain album, as when the folder was
    // deleted or turned out not to be one) stays open with the error shown, the typed name kept
    let before = h.app.session.catalog.albums().count();
    let r = h.request("engine.execute", json!({"command": "dialog.newAlbum", "params": {"name": "Kept", "parent": top}}), T);
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
    // the Collections panel's tree is what these tests drag in: the panels above it are folded
    h.app.ui.toggle_sidebar_section("panel:catalog");
    h.app.ui.toggle_sidebar_section("panel:folders");
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
        widget(&self.h, "classicPanel:collections").center()
    }
    fn parent(&self, id: u64) -> Option<u64> {
        self.h.app.session.catalog.album(lcc_catalog::AlbumId(id)).unwrap().parent.map(|p| p.0)
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
        cat.album_children(parent.map(lcc_catalog::AlbumId)).iter().filter(|a| a.folder == folders).map(|a| a.name.clone()).collect()
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
    assert_eq!(t.h.app.session.source, lcc_engine::LibrarySource::Album(lcc_catalog::AlbumId(t.best)));
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
fn dragged_albums_and_photos_scroll_the_sidebar_at_its_edges() {
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
    hold(&mut t, egui::pos2(cx, top + 26.0));
    let back = t.row(last).top();
    assert!(back > scrolled + 100.0, "the top edge scrolls back up: {scrolled} -> {back}");
    assert!(t.h.app.ui.dragging_album.is_none());
    // photos dragged from the grid scroll the sidebar the same way, to reach an album further down
    let start = t.row(last).top();
    let first = t.h.app.session.visible_cloned()[0].0;
    let thumb = widget(&t.h, &format!("thumb:{first}")).center();
    t.drag(thumb, &[(egui::pos2(cx, bottom - 6.0), 90)]);
    assert!(t.row(last).top() < start - 100.0, "photos scroll it down: {start} -> {}", t.row(last).top());
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
    // dropping beside an album in another folder moves the album there, at that place
    t.drag_to(t.loose, t.best, 0.2);
    assert_eq!(t.parent(t.loose), Some(t.trips));
    assert_eq!(t.kids(Some(t.trips), false), ["Loose", "Best"]);
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
    // (tall enough for the Catalog rows above the folders)
    folders_app_sized(paths, [1400.0, 980.0])
}

fn folders_app_sized(paths: &[&str], size: [f32; 2]) -> Headless {
    folders_app_with(paths, size, json!({"view": "photoGrid", "leftPanel": true, "moduleBar": false}))
}

/// [`folders_app_sized`] in Develop, whose left column draws upstream's sections as its
/// "My Photos" panel (see [`sections_demo`]).
fn folders_sections_app(paths: &[&str], size: [f32; 2]) -> Headless {
    let mut h = folders_app_with(paths, size, json!({"view": "detail", "right": "edit", "leftPanel": true, "moduleBar": false}));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    my_photos_only(&mut h);
    h
}

fn folders_app_with(paths: &[&str], size: [f32; 2], ui: serde_json::Value) -> Headless {
    folders_app_full(paths, size, ui, Services { png: None, ..Default::default() })
}

fn folders_app_with_services(paths: &[&str], size: [f32; 2], services: Services) -> Headless {
    folders_app_full(paths, size, json!({"view": "photoGrid", "leftPanel": true, "moduleBar": false}), services)
}

fn folders_app_full(paths: &[&str], size: [f32; 2], ui: serde_json::Value, services: Services) -> Headless {
    folders_app_prepared(paths, size, ui, services, |_| {})
}

/// [`folders_app_full`], with `prepare` run on the app before its first frame.
fn folders_app_prepared(paths: &[&str], size: [f32; 2], ui: serde_json::Value, services: Services, prepare: impl FnOnce(&mut LccApp)) -> Headless {
    use lcc_catalog::{Op, Photo, Source};
    let mut session = lcc_engine::Session::new();
    for path in paths {
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: (*path).into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut app = LccApp::new(session, services);
    prepare(&mut app);
    let mut h = Headless::new(app, size, 1.0);
    h.app.ui.toggle_sidebar_section("panel:navigator");
    // (the module bar off: these layouts were sized before it existed)
    let r = h.request("ui.set", ui, T);
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
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::LibraryFolder);
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/trip"));
    assert_eq!(h.app.session.filter, lcc_catalog::Filter::default(), "a source, not a filter");
    click(&mut h, "source:all");
    assert_eq!(h.app.session.visible().len(), 3, "All Photos shows everything again");
    // the section folds like the others
    click(&mut h, "classicPanel:folders");
    assert!(!has(&h, "source:libfolder:/pics"), "folded");
    click(&mut h, "classicPanel:folders");
    assert!(has(&h, "source:libfolder:/pics"));
}

/// Each disk is a bar heading its folders, as in Lightroom Classic: a click folds and unfolds them and chooses nothing.
/// Photos right at a disk's top are under a folder row named after the disk.
#[test]
fn disks_are_bars_over_their_folders() {
    let mut h = folders_app(&["/Users/me/a.jpg", "/Volumes/nas/p/b.jpg", "/Volumes/nas/c.jpg"]);
    assert!(has(&h, "volume:/") && has(&h, "volume:/Volumes/nas"), "both disks have a bar");
    // (`Users`, holding only `me`, gives its row to it)
    assert!(has(&h, "source:libfolder:/Users/me"), "the startup disk's folders are under its bar");
    assert!(has(&h, "source:libfolder:/Volumes/nas"), "the disk's own photos have a row");
    assert!(has(&h, "source:libfolder:/Volumes/nas/p"), "and its folders below it");
    click(&mut h, "volume:/");
    assert_eq!(h.app.session.library_folder, None, "a bar chooses nothing");
    assert!(!has(&h, "source:libfolder:/Users/me"), "a click on it folds it");
    click(&mut h, "volume:/");
    assert!(has(&h, "source:libfolder:/Users/me"));
}

/// A folder loads into the grid the way an album, a Local folder or Picks does: whatever was
/// shown before is replaced, and the folder's own row is highlighted.
#[test]
fn choosing_a_library_folder_replaces_whatever_was_shown() {
    use lcc_engine::LibrarySource;
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    // (a browsed folder in the Library tab's own view: Import as a tab off)
    h.app.ui.settings.import_as_tab = false;
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
    use lcc_catalog::{Op, Photo, Source};
    let mut session = lcc_engine::Session::new();
    let id = session.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: "/Volumes/tokyo/photos/travel/a.jpg".into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    session.source = lcc_engine::LibrarySource::LibraryFolder;
    session.library_folder = Some("/Volumes/tokyo/photos/travel".into());
    assert_eq!(crate::i18n::source_title(&session), "photos/travel");
    session.source = lcc_engine::LibrarySource::All;
    assert_eq!(crate::i18n::source_title(&session), "All Photographs");
}

/// Removing a folder, or a whole disk, from the library asks first, then moves its photos to
/// Recently Deleted.
#[test]
fn removing_a_folder_or_a_disk_from_the_library_asks_first() {
    use crate::state::Dialog;
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg", "/Volumes/nas/a/1.jpg", "/Volumes/nas/2.jpg"]);
    let mut left = 5;
    for (path, name, disk) in [("/pics/trip", "trip", false), ("/Volumes/nas", "nas", true)] {
        h.app.ui.dialog = Some(Dialog::RemoveFolder { path: path.into(), name: name.into(), count: 2, disk });
        h.step();
        assert_eq!(h.app.session.visible().len(), left, "{path}: nothing happens until it is confirmed");
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        left -= 2;
        assert_eq!(h.app.session.visible().len(), left, "{path}: its photos left the library");
        assert_eq!(h.app.ui.dialog, None);
    }
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
    h.app.session.source = lcc_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some("/pics/trip/day1".into());
    h.step();
    h.step();
    assert!(has(&h, "source:libfolder:/pics/trip/day1"), "the rows above it opened");
    // folding a parent by hand sticks until the choice changes
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(!has(&h, "source:libfolder:/pics/trip/day1"));
}

/// Make the photo imported from `path` the active one.
fn activate(h: &mut Headless, path: &str) {
    let id = h.app.session.catalog.photos().find(|p| matches!(&p.source, lcc_catalog::Source::File { path: f } if f == path)).unwrap().id;
    h.app.run("library.select", json!({"ids": [id.0], "active": id.0})).unwrap();
    h.step();
}

/// Photo ▸ Show in Folders (Lightroom's Go to Folder in Library) shows the active photo's folder
/// as a click on its Folders row does: the left column, the Classic Folders panel and the folders
/// above the row open, also when the folder was already shown and a row above it was folded by hand.
#[test]
fn show_in_folders_chooses_the_photos_folder_and_opens_the_rows_above_it() {
    use crate::module::PanelId;
    let mut h = folders_app(&["/pics/trip/day1/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"]);
    activate(&mut h, "/pics/trip/day1/a.jpg");
    assert!(crate::menus::ui_enabled(&h.app, "app.showInFolders"));
    // (the fork's Library column is Classic's: its Folders panel, folded here)
    crate::panels::classic::set_open(&mut h.app, PanelId::Folders, false);
    h.app.run("view.leftPanel", json!({})).unwrap();
    h.step();
    assert!(!h.app.ui.left_panel && !has(&h, "source:libfolder:/pics"), "the column is hidden");
    let r = h.request("ui.menu.invoke", json!({"id": "app.showInFolders"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::LibraryFolder);
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/trip/day1"));
    assert_eq!(h.app.session.visible().len(), 1, "the folder's photos fill the grid");
    assert_eq!(h.app.session.active(), h.app.session.visible().first().copied(), "the photo stays the active one");
    assert!(h.app.ui.left_panel && crate::panels::classic::is_open(&h.app, PanelId::Folders), "the column and Folders opened");
    assert!(has(&h, "source:libfolder:/pics/trip/day1") && has(&h, "highlight:libfolder:/pics/trip/day1"), "its row is shown chosen");
    // folded by hand, then asked again: the rows above open again
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(!has(&h, "source:libfolder:/pics/trip/day1"));
    h.app.run("app.showInFolders", json!({})).unwrap();
    h.step();
    h.step();
    assert!(has(&h, "source:libfolder:/pics/trip/day1"), "opened again");
}

/// A folder far down the list is scrolled into view.
#[test]
fn show_in_folders_scrolls_the_folder_into_view() {
    let paths: Vec<String> = (0..60).map(|i| format!("/pics/f{i:02}/a.jpg")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut h = folders_app_sized(&refs, [1400.0, 500.0]);
    activate(&mut h, "/pics/f59/a.jpg");
    h.app.run("app.showInFolders", json!({})).unwrap();
    h.settle(SETTLE);
    let row = widget(&h, "source:libfolder:/pics/f59");
    // (the fork's Classic column: the panel's visible part is what the row must be inside)
    let panel = widget(&h, "panel:left_panel");
    assert!(row.top() >= panel.top() && row.bottom() <= panel.bottom().min(500.0), "in view: {row:?}");
}

/// Without an active photo in a library folder there is nothing to show: no photo, a demo scene.
#[test]
fn show_in_folders_needs_a_photo_in_a_library_folder() {
    let mut h = folders_app(&["/pics/trip/a.jpg"]);
    h.app.run("library.select", json!({"ids": []})).unwrap();
    assert_eq!(h.app.session.active(), None);
    assert!(!crate::menus::ui_enabled(&h.app, "app.showInFolders"));
    let entry = crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == "app.showInFolders").unwrap();
    // (the fork lists it under Photo ▸ File)
    assert!(!entry.enabled && entry.menu.first().map(String::as_str) == Some("Photo"), "listed in Photo, disabled: {:?}", entry.menu);
    assert!(h.app.run("app.showInFolders", json!({})).is_err());
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::All, "nothing changed");
    let mut demo = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let scene = demo.session.visible().first().copied().unwrap();
    demo.run("library.select", json!({"ids": [scene.0]})).unwrap();
    assert!(!crate::menus::ui_enabled(&demo, "app.showInFolders"), "a demo scene has no folder");
    // a folder deeper than the tree goes has no row: the entry is disabled rather than failing
    {
        let deep = format!("/pics{}/a.jpg", "/d".repeat(lcc_catalog::folders::MAX_DEPTH + 4));
        let mut h = folders_app(&[deep.as_str()]);
        activate(&mut h, &deep);
        assert!(!h.app.caches.folder_tree(&h.app.session.catalog).is_empty());
        assert!(!crate::menus::ui_enabled(&h.app, "app.showInFolders"));
        assert!(h.app.run("app.showInFolders", json!({})).is_err());
    }
}

/// The grid's menu reveals the photo right-clicked, not the active one a multi-selection keeps.
#[test]
fn show_in_folders_reveals_the_photo_asked_for_and_follows_to_its_folder() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/c.jpg"]);
    let id_of = |h: &Headless, path: &str| {
        h.app.session.catalog.photos().find(|p| matches!(&p.source, lcc_catalog::Source::File { path: f } if f == path)).unwrap().id
    };
    let (a, c) = (id_of(&h, "/pics/trip/a.jpg"), id_of(&h, "/pics/home/c.jpg"));
    h.app.run("library.select", json!({"ids": [a.0, c.0], "active": a.0})).unwrap();
    h.step();
    let r = h.app.run("app.showInFolders", json!({"id": c.0})).unwrap();
    assert_eq!(r["path"], "/pics/home");
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/home"));
    assert_eq!(h.app.session.active(), Some(c), "the view of that folder shows the photo asked for");
    assert!(crate::menus::show_in_folders_enabled(&h.app, Some(c)));
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
    let _cleanup = Cleanup(base.to_path_buf());
    std::fs::create_dir_all(base.join("pics/trip")).unwrap();
    std::fs::create_dir_all(base.join("pics/home")).unwrap();
    // the folder tree writes paths with forward slashes, and its widget ids carry them
    let b = base.to_string_lossy().replace('\\', "/");
    let mut h = folders_app(&[&format!("{b}/pics/trip/a.jpg"), &format!("{b}/pics/home/b.jpg")]);
    h.app.session.source = lcc_engine::LibrarySource::LibraryFolder;
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
    right_click(&mut h, "volume:/");
    assert!(!popup_open(&h), "the startup disk offers nothing");
}

/// Issue #1194: a folder's menu chooses the folder's new location, then the bulk finder relinks
/// only the missing photos from that folder and the folders inside it.
#[test]
fn a_folder_menu_locates_its_missing_tree_only() {
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let base = std::env::temp_dir().join(format!("lc-ui-locate-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.to_path_buf());
    let new_trip = base.join("new/trip");
    std::fs::create_dir_all(new_trip.join("day2")).unwrap();
    for path in [new_trip.join("a.jpg"), new_trip.join("day2/b.jpg"), new_trip.join("c.jpg")] {
        std::fs::write(path, b"photo").unwrap();
    }

    // (the missing tree is not on disk: a plain root, so the Folders section lists it the same on every platform)
    let old = "/lc-locate-old".to_string();
    let old_trip = format!("{old}/trip");
    let paths = [format!("{old_trip}/a.jpg"), format!("{old_trip}/day2/b.jpg"), format!("{old}/other/c.jpg")];
    let picked = new_trip.to_string_lossy().to_string();
    let services = Services { pick_folder: Some(Box::new(move || Some(picked.clone()))), png: None, ..Default::default() };
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut h = folders_app_with_services(&refs, [1400.0, 900.0], services);

    click(&mut h, &format!("libraryFolderToggle:{old}"));
    right_click(&mut h, &format!("source:libfolder:{old_trip}"));
    assert!(has(&h, "folderLocateMissing"), "the folder menu offers the action");
    click(&mut h, "folderLocateMissing");
    assert!(h.step_until(T, |h| h.app.ui.last_find_missing.is_some()), "the background search finished");
    let result = h.app.ui.last_find_missing.as_ref().unwrap();
    assert_eq!(result["found"].as_array().map(Vec::len), Some(2), "{result}");
    assert_eq!(result["missing"], 0, "{result}");

    let sources: Vec<String> = h
        .app
        .session
        .catalog
        .photos()
        .filter_map(|photo| match &photo.source {
            lcc_catalog::Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert!(sources.contains(&new_trip.join("a.jpg").to_string_lossy().to_string()), "{sources:?}");
    assert!(sources.contains(&new_trip.join("day2").join("b.jpg").to_string_lossy().to_string()), "{sources:?}");
    assert!(sources.contains(&format!("{old}/other/c.jpg")), "another missing folder stays untouched: {sources:?}");
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
    assert_eq!(h.app.session.catalog.folder_color_label("/pics/trip"), Some(lcc_catalog::ColorLabel::Red));
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

fn reveal_and_pick_services(reveal: Result<(), String>) -> Services {
    Services { pick_folder: Some(Box::new(|| None)), reveal: Some(Box::new(move |_| reveal.clone())), png: None, ..Default::default() }
}

/// Show in Finder on a folder says why when the file manager can't show it, instead of nothing.
#[test]
fn a_folder_that_cant_be_shown_in_the_file_manager_says_why() {
    let services = reveal_and_pick_services(Err("no file manager".into()));
    let mut h = folders_app_with_services(&["/pics/trip/a.jpg", "/pics/home/b.jpg"], [1400.0, 900.0], services);
    click(&mut h, "libraryFolderToggle:/pics");
    right_click(&mut h, "source:libfolder:/pics/trip");
    h.app.ui.toasts.clear();
    click(&mut h, "folderReveal");
    assert!(h.app.ui.toast().is_some_and(|t| t.0.contains("no file manager")), "{:?}", h.app.ui.toast());
}

/// A Local place's menu renames the folder by its own name on disk. A folder inside a place lists
/// Remove from Local grayed out, since only a place can be removed, while its Add to Local makes
/// it a place.
#[test]
fn a_local_folders_menu_renames_by_its_own_name_and_grays_out_remove_inside_a_place() {
    let dir = lcc_testkit::TempDir::new("lc-ui-local-menu");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("place/inner")).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.to_path_buf());
    let place = dir.join("place").to_string_lossy().to_string();
    let inner = dir.join("place").join("inner").to_string_lossy().to_string();
    let mut h = folders_app_with_services(&["/pics/trip/a.jpg"], [1400.0, 900.0], reveal_and_pick_services(Ok(())));
    h.app.run("local.addRoot", json!({"path": place})).unwrap();
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == format!("source:local:{place}"))));
    right_click(&mut h, &format!("source:local:{place}"));
    // Rename starts from the folder's own name on disk, not what dialogs call it ("parent/name")
    click(&mut h, "folderRename");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { value, .. }) if value == "place"), "{:?}", h.app.ui.dialog);
    h.app.ui.dialog = None;
    h.step();
    h.step();
    click(&mut h, &format!("folderToggle:{place}"));
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == format!("source:local:{inner}"))));
    right_click(&mut h, &format!("source:local:{inner}"));
    click(&mut h, "folderRemoveFromLocal");
    assert!(h.app.ui.hidden_locations.is_empty(), "a folder inside a place can't be removed from Local: {:?}", h.app.ui.hidden_locations);
    // the same row's Add to Local does act: the folder becomes a place of its own
    right_click(&mut h, &format!("source:local:{inner}"));
    click(&mut h, "folderKeepInLocal");
    assert!(h.app.ui.local_roots.contains(&inner), "{:?}", h.app.ui.local_roots);
}

/// Copy Path is in every folder menu and puts the path, as the OS spells it, on the clipboard
/// and says so.
#[test]
fn copy_path_puts_a_folders_path_on_the_clipboard_from_each_kind_of_row() {
    let native = |p: &str| p.replace('/', std::path::MAIN_SEPARATOR_STR);
    let dir = lcc_testkit::TempDir::new("lc-ui-copy-path");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("place")).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.to_path_buf());
    let place = dir.join("place").to_string_lossy().to_string();
    let mut h = folders_app_with_services(
        &["/pics/trip/a.jpg", "/pics/home/b.jpg", "/Volumes/nas/c.jpg"],
        [1400.0, 1400.0],
        Services { png: None, ..Default::default() },
    );
    click(&mut h, "libraryFolderToggle:/pics");
    let copy_from = |h: &mut Headless, row: &str, path: &str| {
        assert!(h.step_until(T, |h| has(h, row)), "{row} is listed");
        h.view.clipboard.clear();
        h.app.ui.toasts.clear();
        right_click(h, row);
        assert!(
            has(h, "folderCopyPath"),
            "{row}: {:?}",
            h.app.widgets.iter().map(|(w, _)| w.clone()).filter(|w| w.starts_with("folder")).collect::<Vec<_>>()
        );
        click(h, "folderCopyPath");
        assert_eq!(h.view.clipboard, native(path), "{row}");
        assert_eq!(h.app.ui.toast().map(|t| t.0.as_str()), Some(crate::i18n::Locale::En.tr("Path copied")), "{row}");
        assert!(!popup_open(h), "choosing closes the menu");
    };
    copy_from(&mut h, "source:libfolder:/pics/trip", "/pics/trip");
    copy_from(&mut h, "source:libfolder:/Volumes/nas", "/Volumes/nas");
    h.app.run("local.addRoot", json!({"path": place})).unwrap();
    copy_from(&mut h, &format!("source:local:{place}"), &place);
}

/// A path is copied with the OS's separators, and a bare drive as its root.
#[test]
fn a_copied_path_has_the_separators_of_the_os_and_a_drives_root() {
    use crate::panels::left::native_path;
    assert_eq!(native_path("D:", '\\'), "D:\\");
    assert_eq!(native_path("D:/photos/trip", '\\'), "D:\\photos\\trip");
    assert_eq!(native_path("//server/share/x", '\\'), "\\\\server\\share\\x");
    assert_eq!(native_path("/Volumes/nas", '/'), "/Volumes/nas");
}

/// A disk's menu offers Show in Finder with the disk's path and New Folder on Disk… inside the
/// disk, which the startup disk (no menu at all) doesn't.
#[test]
fn a_disks_menu_shows_it_in_the_file_manager_and_makes_a_folder_in_it() {
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = shown.clone();
    let services = Services {
        reveal: Some(Box::new(move |p| {
            seen.lock().unwrap().push(p.to_string());
            Ok(())
        })),
        png: None,
        ..Default::default()
    };
    let mut h = folders_app_with_services(&["/Volumes/nas/a/1.jpg", "/Volumes/nas/2.jpg", "/Users/me/3.jpg"], [1400.0, 900.0], services);
    right_click(&mut h, "source:libfolder:/Volumes/nas");
    click(&mut h, "folderReveal");
    assert_eq!(*shown.lock().unwrap(), ["/Volumes/nas"]);
    right_click(&mut h, "source:libfolder:/Volumes/nas");
    click(&mut h, "folderNewFolder");
    match &h.app.ui.dialog {
        Some(crate::state::Dialog::TextPrompt { title, command, params, key, .. }) => {
            assert_eq!((command.as_str(), key.as_str()), ("folder.create", "name"));
            assert!(title.contains("“nas”"), "the disk is called by its own name: {title}");
            assert_eq!(params["parent"], "/Volumes/nas");
        }
        other => panic!("New Folder on Disk… asks for a name: {other:?}"),
    }
}

/// New Folder on Disk… in a Local folder makes the folder inside it, and its row is listed.
#[test]
fn a_local_folders_new_folder_is_made_on_disk_and_listed() {
    let dir = lcc_testkit::TempDir::new("lc-ui-local-new");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("place/inner")).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.to_path_buf());
    let place = dir.join("place").to_string_lossy().to_string();
    let inner = dir.join("place").join("inner").to_string_lossy().to_string();
    let made = dir.join("place").join("picks").to_string_lossy().to_string();
    let mut h = folders_app_with_services(&["/pics/trip/a.jpg"], [1400.0, 900.0], Services { png: None, ..Default::default() });
    h.app.run("local.addRoot", json!({"path": place})).unwrap();
    assert!(h.step_until(T, |h| has(h, &format!("source:local:{place}"))));
    click(&mut h, &format!("folderToggle:{place}"));
    assert!(h.step_until(T, |h| has(h, &format!("source:local:{inner}"))), "the folder inside is listed");
    right_click(&mut h, &format!("source:local:{place}"));
    click(&mut h, "folderNewFolder");
    let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    *value = "picks".into();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(std::path::Path::new(&made).is_dir(), "the folder is on disk");
    assert!(h.step_until(T, |h| has(h, &format!("source:local:{made}"))), "its row is listed");
}

/// A library with a photo in every folder down `deep` and two folders below it, in a window
/// `size` big, with every level but the last opened. (Every level holds a photo, so it has a row
/// of its own and starts closed, whether or not the sidebar passes over folders that only lead on.)
fn deep_folders_app(deep: &str, size: [f32; 2]) -> Headless {
    let mut paths = vec![format!("{deep}/x/1.jpg"), format!("{deep}/y/2.jpg")];
    let mut levels = Vec::new();
    let mut level = String::new();
    for name in deep.split('/').filter(|n| !n.is_empty()) {
        level.push('/');
        level.push_str(name);
        paths.push(format!("{level}/0.jpg"));
        levels.push(level.clone());
    }
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut h = folders_app_sized(&paths, size);
    // each level is opened once its row is drawn (a slow machine may need more than a frame)
    for level in &levels[..levels.len() - 1] {
        let toggle = format!("libraryFolderToggle:{level}");
        assert!(h.step_until(T, |h| has(h, &toggle)), "{toggle} is drawn");
        click(&mut h, &toggle);
    }
    let last = format!("libraryFolderToggle:{}", levels[levels.len() - 1]);
    assert!(h.step_until(T, |h| has(h, &last)), "{last} is drawn");
    h.settle(SETTLE);
    h
}

/// The x of each folder's chevron down the chain `path` (`/a/b/c` → `/a`, `/a/b`, `/a/b/c`).
fn chevron_xs(h: &Headless, path: &str) -> Vec<f32> {
    let mut xs: Vec<f32> = Vec::new();
    let mut at = String::new();
    for name in path.split('/').filter(|n| !n.is_empty()) {
        at.push('/');
        at.push_str(name);
        xs.push(widget(h, &format!("libraryFolderToggle:{at}")).center().x);
    }
    xs
}

/// However deep the folders go, each level is indented one step more than its parent. The step
/// narrows to make room, never below the narrowest, and past that the sidebar scrolls sideways
/// instead of squeezing the nesting further.
#[test]
fn a_deep_chain_keeps_its_nesting_and_the_sidebar_scrolls_sideways() {
    let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z";
    // (tall enough for the whole chain: Recently Deleted is above it now)
    let mut h = deep_folders_app(deep, [1400.0, 1700.0]);
    let xs = chevron_xs(&h, deep);
    let step = crate::panels::left::MIN_INDENT_STEP;
    assert_eq!(crate::panels::left::indent_step(&h.view.ctx), step, "26 levels do not fit the default panel even at the narrowest step");
    assert!(xs.len() == 26 && xs.windows(2).all(|w| (w[1] - w[0] - step).abs() < 0.5), "every level steps right by one indent: {xs:?}");
    let panel = widget(&h, "panel:left_panel");
    assert!(crate::panels::left::content_width(&h.view.ctx) > panel.width(), "the content is wider than the panel: the bar appears");
    assert!(widget(&h, "source:all").width() > panel.width(), "the rows are as wide as the widest one, so there is something to scroll to");
    // counts stay where you can read them, at the visible edge, however wide the rows are
    for id in ["count:all", "count:picks", "count:recentlyDeleted", "count:libfolder:/a"] {
        assert!(widget(&h, id).right() <= panel.right(), "{id} stays inside the panel");
    }
    // what a row needs is known before it is drawn off screen, so the plus stays reachable
    assert!(widget(&h, "icon:albumNew").right() <= panel.right(), "Create Album stays inside the visible panel");
    // a click in the middle of a deep row chooses it; only the little chevron folds it
    let chosen = "/a/b/c/d/e/f/g/h/i/j/k/l";
    click(&mut h, &format!("source:libfolder:{chosen}"));
    assert_eq!(h.app.session.library_folder.as_deref(), Some(chosen), "the row's middle is the row, not its chevron");
}

/// A library below a deep path (issue #2298): in the default panel the levels step right by
/// less than a full step, so the deepest folder's name has room and nothing scrolls sideways;
/// a wide panel goes back to full steps, and a shallow tree never narrows.
#[test]
fn a_deep_library_path_narrows_the_indent_instead_of_cutting_the_names() {
    let deep = "/Users/someone/Library/CloudStorage/Nextcloud-cloud.example.com/RAW/Club/2026-09-24 Team/RAW";
    let mut h = deep_folders_app(deep, [1400.0, 1200.0]);
    let full = crate::panels::left::INDENT_STEP;
    let narrow = crate::panels::left::indent_step(&h.view.ctx);
    assert!(narrow < full && narrow >= crate::panels::left::MIN_INDENT_STEP, "the step narrows in the default panel: {narrow}");
    let xs = chevron_xs(&h, deep);
    assert!(xs.windows(2).all(|w| (w[1] - w[0] - narrow).abs() < 0.5), "every level still steps right by the same amount: {xs:?}");
    let panel = widget(&h, "panel:left_panel");
    assert!(crate::panels::left::content_width(&h.view.ctx) <= panel.width(), "nothing scrolls sideways");
    let id = format!("libfolder:{deep}");
    let (label, count) = (widget(&h, &format!("label:{id}")), widget(&h, &format!("count:{id}")));
    assert!(label.width() > 20.0 && label.right() + 4.0 <= count.left(), "the deepest name shows, left of its count: {label:?} {count:?}");
    // a wide panel has room for full steps again
    let r = h.request("ui.set", json!({"leftWidth": 480.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(crate::panels::left::indent_step(&h.view.ctx), full, "a wide panel indents by the full step");
    let xs = chevron_xs(&h, deep);
    assert!(xs.windows(2).all(|w| (w[1] - w[0] - full).abs() < 0.5), "{xs:?}");
    // a shallow tree keeps the full step in the default panel
    let h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    assert_eq!(crate::panels::left::indent_step(&h.view.ctx), full, "a shallow tree is drawn as before");
}

/// A sidebar that fits needs no sideways scrolling.
#[test]
fn a_sidebar_that_fits_does_not_scroll_sideways() {
    let h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    let panel = widget(&h, "panel:left_panel");
    let w = crate::panels::left::content_width(&h.view.ctx);
    assert!(w > 0.0 && w <= panel.width(), "rows were measured and fit: no horizontal bar ({w} vs {})", panel.width());
    assert!((widget(&h, "source:all").width() - (panel.width() - 0.0)).abs() < 24.0, "rows are as wide as the panel");
    // a long name alone does not make the sidebar scroll sideways: depth does
    let h = folders_app(&[&format!("/{LONG_NAME}/a.jpg")]);
    let panel = widget(&h, "panel:left_panel");
    assert!(crate::panels::left::content_width(&h.view.ctx) <= panel.width(), "a name takes at most what a row can show");
}

/// A long folder name is trimmed before it can run under the photo count.
#[test]
fn a_long_folder_name_never_runs_under_the_photo_count() {
    let long = "/very/long/2024-06-12 Tripping Through The Extremely Long Named Mountains Of Somewhere";
    let mut h = folders_app_sized(&[&format!("{long}/a.jpg")], [900.0, 700.0]);
    h.app.session.source = lcc_engine::LibrarySource::LibraryFolder;
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
    // (the tooltip waits out egui's delay in the app's own time, a frame at a time: counted in frames, not in
    // seconds of a machine that may be busy)
    let shown = (0..120).any(|_| {
        h.step();
        tooltip_shown(&h)
    });
    assert!(shown, "the full name appears on hover");
    // more room shows more of the name
    h.request("ui.move", json!({"x": 800.0, "y": 450.0}), T);
    let r = h.request("ui.set", json!({"leftWidth": 480.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.step();
    assert!(widget(&h, &format!("label:libfolder:{long}")).width() > label.width() + 100.0, "a wider panel shows more of it");
}

/// The selection bar ends at the panel's edge, not past it, when the content is wider.
#[test]
fn the_selection_bar_stays_inside_the_panel() {
    let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m";
    // (tall enough for the whole chain: Recently Deleted is above it now)
    // a photo at every level: a level holding none would give its row to the one folder inside it
    let mut photos: Vec<String> = vec![format!("{deep}/x/1.jpg"), format!("{deep}/y/2.jpg")];
    let mut level = String::new();
    for name in deep.split('/').filter(|n| !n.is_empty()) {
        level.push('/');
        level.push_str(name);
        photos.push(format!("{level}/p.jpg"));
    }
    let photos: Vec<&str> = photos.iter().map(String::as_str).collect();
    let mut h = folders_app_sized(&photos, [1400.0, 1000.0]);
    // choosing the deepest folder opens every level above it
    h.app.session.source = lcc_engine::LibrarySource::LibraryFolder;
    h.app.session.library_folder = Some(format!("{deep}/x"));
    h.settle(SETTLE);
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
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::All, "{key}");
    assert!(!h.app.session.visible().is_empty(), "{key}: its photos are shown");
    // choosing the row again clears it, and stays in All Photos
    let r = h.request("ui.clickWidget", json!({"id": row}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.filter, lcc_catalog::Filter::default(), "{key}");
    // By Date opens a year into its months with the triangle, and closes it again
    let rows = |h: &Headless| h.app.widgets.iter().filter(|(w, _)| w.starts_with("source:date:")).count();
    let years = rows(&h);
    let toggle = h.app.widgets.iter().find(|(w, _)| w.starts_with("dateToggle:")).map(|(w, _)| w.clone()).expect("a year has a triangle");
    click(&mut h, &toggle);
    assert!(rows(&h) > years, "months are listed");
    click(&mut h, &toggle);
    assert_eq!(rows(&h), years, "closed again");
}

/// Library's Navigator panel: its 1:1 preset opens the loupe at 1:1, a click on its picture pans
/// there, Fit goes back; folding it hides the picture.
#[test]
fn navigator_panel_presets_and_click_to_pan() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    h.app.ui.toggle_sidebar_section("panel:navigator");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "navigator:1:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
    assert_eq!(h.app.ui.zoom, crate::state::Zoom::Percent(100.0));
    h.step();
    h.app.ui.pan = (0.1, 0.9);
    let r = h.request("ui.clickWidget", json!({"id": "navigator:image"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let (u, v) = h.app.ui.pan;
    assert!((u - 0.5).abs() < 0.05 && (v - 0.5).abs() < 0.05, "a click in the middle centres the loupe: {u},{v}");
    let r = h.request("ui.clickWidget", json!({"id": "navigator:fit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.zoom, crate::state::Zoom::Fit);
    let r = h.request("ui.clickWidget", json!({"id": "sidebarSection:navigator"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "navigator:image"));
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
fn command_toasts_show_after_a_while() {
    let dir = lcc_testkit::TempDir::new("lc-tracklog-toast");
    std::fs::create_dir_all(&dir).unwrap();
    let gpx = dir.join("walk.gpx");
    std::fs::write(
        &gpx,
        r#"<?xml version="1.0"?><gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg><trkpt lat="46.0" lon="7.0"><time>2026-05-01T10:00:00Z</time></trkpt></trkseg></trk></gpx>"#,
    )
    .unwrap();
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    for _ in 0..600 {
        h.step();
    }
    let r = h.request("engine.execute", serde_json::json!({"command": "view.autoAdvance"}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.toast().is_some_and(|t| t.0.starts_with("Auto Advance")), "{:?}", h.app.ui.toasts);
    // Auto-Tag from Tracklog's toast shows too: it was stamped by a clock of its own, which starts
    // at zero, so a while into a session it had always already expired
    h.app.ui.toasts.clear();
    let p = serde_json::json!({"command": "photo.tagFromTracklog", "params": {"path": gpx.to_string_lossy(), "offset": 0}});
    let r = h.request("engine.execute", p, std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.toast().is_some_and(|t| t.0.starts_with("Tagged")), "{:?}", h.app.ui.toasts);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #678: Settings ▸ Faces cannot switch recognition on until a recognition model is chosen:
/// the box is off and clicking it changes nothing. Installing a model chooses it but leaves recognition
/// off (it is off by default: the scan can take long); from then on the box turns it on and off, and the
/// Face scan section has its status and Pause / Clear… / Restart….
#[test]
fn recognise_faces_stays_off_until_a_model_is_chosen() {
    let dir = lcc_testkit::TempDir::new("lc-ui-faces-toggle");
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
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
    // a model: installing it chooses it and leaves recognition off; the box then works both ways
    let f = dir.join("m.onnx");
    std::fs::write(&f, lcc_faces::synthetic::tiny_embedder_model(64)).unwrap();
    h.app.session.execute("faces.models.install", &json!({"path": f.to_string_lossy(), "acknowledged": true})).unwrap();
    h.app.caches.faces_epoch += 1;
    h.step();
    h.step();
    assert!(!enabled(&mut h), "installing a model leaves recognition off");
    click(&mut h);
    assert!(enabled(&mut h), "the box switches recognition on, with a model chosen");
    click(&mut h);
    assert!(!enabled(&mut h), "and off again");
    // the Face scan section: its status and buttons; Clear… asks before it deletes anything
    click(&mut h);
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    for id in ["faces:status", "button:facesPause", "button:facesClear", "button:facesRestart"] {
        assert!(has(&h, id), "{id} is shown");
    }
    let press = |h: &mut Headless, id: &str| {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        h.step();
    };
    press(&mut h, "button:facesPause");
    assert_eq!(h.app.session.execute("faces.status", &json!({})).unwrap()["paused"], true);
    assert!(has(&h, "button:facesResume"), "paused: the button offers resume");
    press(&mut h, "button:facesClear");
    assert!(has(&h, "button:facesClearConfirm"), "Clear… asks first");
    press(&mut h, "button:facesConfirmCancel");
    assert!(!has(&h, "button:facesClearConfirm"));
    press(&mut h, "button:facesResume");
    assert_eq!(h.app.session.execute("faces.status", &json!({})).unwrap()["paused"], false);
}

/// Issue #684: albums hold library photos. For a browsed (Local) photo the grid's right-click
/// Add to Album is off, Photo ▸ Add to Album says to Add to My Photos first with its entries off,
/// and a drop onto an album row is refused with that hint; once the photo is in My Photos all
/// three work.
#[test]
fn local_photos_cannot_be_added_to_albums_until_they_join_my_photos() {
    let dir = lcc_testkit::TempDir::new("lc-ui-albums-local");
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    // (dragging browsed photos onto the library's albums: Import as a tab off)
    h.app.ui.settings.import_as_tab = false;
    let album = h.app.session.execute("album.create", &json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let album_id = lcc_catalog::AlbumId(album);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(dir.join("a.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let vis = h.app.session.visible_cloned();
    assert_eq!(vis.len(), 1, "{vis:?}");
    let local = vis[0];
    assert!(h.app.session.catalog.photo(local).unwrap().local);
    // Local folded: on Windows the temp folder lies under the home folder, so browsing it opens Local
    // at every folder on the way, which pushes the album row below the window
    if !h.app.ui.sidebar_section_collapsed("local") {
        h.app.ui.toggle_sidebar_section("local");
    }
    h.settle(SETTLE);
    let thumb = widget(&h, &format!("thumb:{}", local.0));

    // right-click: Add to Album is there but off, so hovering opens no album entries
    let r = h.request("ui.click", json!({"x": thumb.center().x, "y": thumb.center().y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.session.selection.contains(local));
    let r = h.request("ui.hoverWidget", json!({"id": "button:contextAddToAlbum"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:contextAlbum:")), "the submenu must not open for a Local photo");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();

    // Photo ▸ Add to Album: the hint item first (off), the album and New Album… off
    let add_to_album = |app: &LccApp| -> Vec<crate::menubar::MenuNode> {
        let bar = crate::menubar::menu_bar(app);
        let photo = &bar.iter().find(|(t, _)| t == "Photo").expect("Photo menu").1;
        photo
            .iter()
            .find_map(|n| match n {
                crate::menubar::MenuNode::Submenu { label, children } if label == "Add to Collection" => Some(children.clone()),
                _ => None,
            })
            .expect("Add to Collection submenu")
    };
    let enabled_of = |nodes: &[crate::menubar::MenuNode], id: &str| -> Option<bool> {
        nodes.iter().find_map(|n| match n {
            crate::menubar::MenuNode::Item { id: i, enabled, .. } if i == id => Some(*enabled),
            _ => None,
        })
    };
    let nodes = add_to_album(&h.app);
    assert!(matches!(nodes.first(), Some(crate::menubar::MenuNode::Item { id, enabled: false, .. }) if id == "photo.addToLibrary"), "{nodes:?}");
    assert_eq!(enabled_of(&nodes, "album.addPhotos"), Some(false), "{nodes:?}");
    assert_eq!(enabled_of(&nodes, "dialog.newAlbum"), Some(false), "{nodes:?}");

    // a drop onto the album row is refused and says why
    let row = widget(&h, &format!("source:album:{album}"));
    let r = h.request("ui.drag", json!({"x": thumb.center().x, "y": thumb.center().y, "toX": row.center().x, "toY": row.center().y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.catalog.album(album_id).unwrap().photos.is_empty(), "nothing invisible was added");
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Add to My Photos first"), "{toast:?}");

    // in My Photos: the menu entries come on, the submenu opens, and the album takes the photo
    h.app.run("photo.addToLibrary", json!({"ids": [local.0]})).unwrap();
    // the photo joined the library, so the quick filter's not-imported view would leave it out:
    // Show All keeps it in the folder view for the rest of the test
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy(), "show": "all"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let nodes = add_to_album(&h.app);
    assert!(!matches!(nodes.first(), Some(crate::menubar::MenuNode::Item { id, .. }) if id == "photo.addToLibrary"), "{nodes:?}");
    assert_eq!(enabled_of(&nodes, "album.addPhotos"), Some(true), "{nodes:?}");
    assert_eq!(enabled_of(&nodes, "dialog.newAlbum"), Some(true), "{nodes:?}");
    let thumb = widget(&h, &format!("thumb:{}", local.0));
    let r = h.request("ui.click", json!({"x": thumb.center().x, "y": thumb.center().y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.hoverWidget", json!({"id": "button:contextAddToAlbum"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": format!("button:contextAlbum:{album}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.session.catalog.album(album_id).unwrap().photos, vec![local]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #676: right-click ▸ Add to Album offers New Album… like the Photo menu does, also when
/// the library has no album yet (the submenu used to come up empty); the new album holds the photo.
#[test]
fn right_click_add_to_album_offers_a_new_album_when_there_is_none() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let plain: Vec<u64> = h.app.session.catalog.albums().filter(|a| !a.folder && !a.is_smart()).map(|a| a.id.0).collect();
    for id in plain {
        h.app.session.execute("album.delete", &json!({"id": id})).unwrap();
    }
    assert!(h.app.session.catalog.albums().all(|a| a.folder || a.is_smart()), "no plain album is left");
    let photo = lcc_catalog::PhotoId(h.app.session.visible_cloned()[1].0);
    h.step();
    let thumb = widget(&h, &format!("thumb:{}", photo.0));
    let r = h.request("ui.click", json!({"x": thumb.center().x, "y": thumb.center().y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.session.selection.contains(photo), "right-click selects the photo");
    let r = h.request("ui.hoverWidget", json!({"id": "button:contextAddToAlbum"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:contextNewAlbum"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    match &mut h.app.ui.dialog {
        Some(crate::state::Dialog::NewAlbum { name, folder: false, parent: None, .. }) => *name = "Trip".into(),
        other => panic!("New Album dialog expected, got {other:?}"),
    }
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let trip = h.app.session.catalog.albums().find(|a| a.name == "Trip").expect("the album was created");
    assert!(trip.photos.contains(&photo), "the right-clicked photo is in it: {:?}", trip.photos);
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
}

/// An album's context menu ▸ Import ▸ Import from Folder… (and Import Photos…, Import from Device)
/// opens the import review with that album chosen; a folder row doesn't offer it, and an `album`
/// the review can't offer (a folder, or an album deleted while the scan ran) leaves its album empty.
#[test]
fn an_album_menu_imports_into_that_album() {
    let dir = lcc_testkit::TempDir::new("lc-ui-album-import");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..2u8 {
        let (w, h) = (40usize, 30usize);
        let data: Vec<[u8; 4]> = (0..w * h).map(|k| [(k % w * 6) as u8, i * 90, (k / w * 8) as u8, 255]).collect();
        let img = lcc_raster::Rgba8 { width: w, height: h, data };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("card{i}.png")), png).unwrap();
    }
    let picked = dir.to_string_lossy().to_string();
    let services = Services { png: None, pick_folder: Some(Box::new(move || Some(picked.clone()))), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    // the demo's top-level album and album folder, both shown in the sidebar
    let id_of = |name: &str| h.app.session.catalog.albums().find(|a| a.name == name).map(|a| a.id.0).expect(name);
    let (album, folder) = (id_of("Garden"), id_of("Travel 2026"));
    let right_click = |h: &mut Headless, id: &str| {
        // the Classic column can put the Collections panel below the window: scroll the row into view
        let mut row = widget(h, id);
        if row.bottom() > 850.0 {
            scroll_sidebar(h, 500.0 - row.bottom());
            row = widget(h, id);
        }
        assert!(row.bottom() <= 850.0 && row.top() >= 0.0, "{id} on screen: {row:?}");
        let r = h.request("ui.click", json!({"x": row.center().x, "y": row.center().y, "button": "right"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    let review_album = |h: &mut Headless| {
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review: {:?}", h.app.ui.dialog) };
        let chosen = opts.album;
        h.app.ui.dialog = None;
        chosen
    };

    // a folder row has no Import
    right_click(&mut h, &format!("source:folder:{folder}"));
    assert!(!has(&h, "button:albumImport"), "folders hold no photos");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();

    // an album's menu: Import ▸ Import from Folder… → the (stubbed) folder dialog → the review, album chosen
    right_click(&mut h, &format!("source:album:{album}"));
    let r = h.request("ui.hoverWidget", json!({"id": "button:albumImport"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    for way in ["button:albumImportPhotos", "button:albumImportFolder", "button:albumImportDevice"] {
        assert!(has(&h, way), "{way} is offered");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:albumImportFolder"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(review_album(&mut h), Some(album));

    // Import Photos… passes the album the same way
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()], "album": album}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    assert_eq!(review_album(&mut h), Some(album));

    // an album the review can't offer (here a folder) is not chosen
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()], "album": folder}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    assert_eq!(review_album(&mut h), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Delete asks before photos go to Recently Deleted, by default and from every way in: the key, the
/// Photo menu and a photo's right-click menu (#1239). With the setting off, it deletes at once.
#[test]
fn deleting_a_photo_asks_first_from_the_key_the_menu_and_the_right_click_menu() {
    use crate::state::Dialog;
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    assert!(h.app.ui.settings.confirm_delete, "on by default");
    let photo = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [photo.0]}));
    let deleted = |h: &Headless| h.app.session.catalog.photo(photo).unwrap().deleted;

    // ⌫ and Photo ▸ Delete Photo ask; Cancel keeps the photo
    h.request("ui.key", json!({"key": "delete"}), T);
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, disk_files: 0 }), "the key asks");
    h.request("ui.dialog.cancel", json!({}), T);
    assert!(!deleted(&h));
    // (what a click on the menu bar's item runs; `ui.menu.invoke` is the agents' engine.execute)
    crate::menubar::run_item(&mut h.app, "photo.delete", json!({})).unwrap();
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, disk_files: 0 }), "the menu asks");
    h.request("ui.dialog.cancel", json!({}), T);
    assert!(!deleted(&h));

    // the right-click menu's Delete Photo asks too; OK deletes
    let thumb = format!("thumb:{}", photo.0);
    right_click(&mut h, &thumb);
    click(&mut h, "button:contextDeletePhoto");
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, disk_files: 0 }), "the right-click menu asks");
    assert!(!deleted(&h), "nothing deleted before the answer");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(deleted(&h));

    // turned off, the right-click menu deletes at once
    exec(&mut h, "edit.undo", json!({}));
    assert!(!deleted(&h));
    h.app.ui.settings.confirm_delete = false;
    right_click(&mut h, &thumb);
    click(&mut h, "button:contextDeletePhoto");
    assert_eq!(h.app.ui.dialog, None);
    assert!(deleted(&h));
}

/// New Album's "Include selected photos" (as in Lightroom): on by default; unticked, the new album
/// starts empty; with nothing selected the option isn't shown, and a folder never has it.
#[test]
fn new_album_can_leave_the_selected_photos_out() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    let photo = lcc_catalog::PhotoId(h.app.session.visible_cloned()[1].0);
    h.app.session.execute("library.select", &json!({"ids": [photo.0]})).unwrap();
    let create = |h: &mut Headless, command: &str, name: &str| {
        h.app.run(command, json!({"name": name})).unwrap();
        h.step();
        h.step();
    };
    let confirm = |h: &mut Headless| {
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    };
    let photos = |h: &Headless, name: &str| h.app.session.catalog.albums().find(|a| a.name == name).expect("the album was created").photos.clone();

    // ticked by default: the selection goes in, as before
    create(&mut h, "dialog.newAlbum", "With");
    assert!(has(&h, "check:albumIncludeSelected"), "the option is offered with a selection");
    confirm(&mut h);
    assert_eq!(photos(&h, "With"), vec![photo]);

    // unticked: an empty album
    create(&mut h, "dialog.newAlbum", "Without");
    let r = h.request("ui.clickWidget", json!({"id": "check:albumIncludeSelected"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    confirm(&mut h);
    assert!(photos(&h, "Without").is_empty(), "nothing was added");

    // a folder never offers it
    create(&mut h, "dialog.newFolder", "Folder");
    assert!(!has(&h, "check:albumIncludeSelected"), "folders hold no photos");
    confirm(&mut h);

    // nothing selected: no option, an empty album
    h.app.session.selection = lcc_engine::Selection { ids: vec![], active: None };
    create(&mut h, "dialog.newAlbum", "Nothing");
    assert!(!has(&h, "check:albumIncludeSelected"), "no selection, no option");
    confirm(&mut h);
    assert!(photos(&h, "Nothing").is_empty());
}

/// Recently Deleted can be emptied from its view's button and from the sidebar row's right-click
/// menu; both ask first, Cancel keeps the photos, and OK removes only the trashed photos.
#[test]
fn emptying_recently_deleted_asks_first_and_removes_only_trashed_photos() {
    use crate::state::Dialog;
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid", "leftPanel": true}));
    let all = h.app.session.visible_cloned();
    let (gone, kept) = (vec![all[0].0, all[1].0], all.len() - 2);
    exec(&mut h, "photo.delete", json!({"ids": gone}));
    exec(&mut h, "library.source", json!({"kind": "recentlyDeleted"}));
    h.settle(SETTLE);
    click(&mut h, "button:emptyRecentlyDeleted");
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmPurge { count: 2, all: true }), "the view's button asks");
    h.request("ui.dialog.cancel", json!({}), T);
    assert_eq!(h.app.session.catalog.len(), all.len(), "Cancel keeps them");
    right_click(&mut h, "source:recentlyDeleted");
    click(&mut h, "menu:emptyRecentlyDeleted");
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmPurge { count: 2, all: true }), "the sidebar menu asks");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.catalog.len(), kept, "only the trashed photos are gone");
    assert!(h.app.session.catalog.photos().all(|p| !p.deleted));
}

/// Delete from Disk in confirmation dialog moves the file to trash and permanently deletes from catalog.
#[test]
fn delete_from_disk_dialog_trashes_file_and_removes_from_catalog() {
    use crate::state::Dialog;
    let tmp = lcc_testkit::TempDir::new("lcc-test-ui-delete");
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));

    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let file = tmp.join("disk_photo.jpg");
    std::fs::write(&file, b"content").unwrap();

    let id = h.app.session.catalog.alloc_photo_id();
    let op = lcc_catalog::Op::AddPhoto {
        photo: Box::new(lcc_catalog::Photo::new(
            id,
            lcc_catalog::Source::File { path: file.to_string_lossy().to_string() },
            "disk_photo.jpg",
            "JPEG",
            100,
            100,
            "2026-01-01",
        )),
    };
    h.app.session.commit("Add photo", op).unwrap();

    let trashed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let t_clone = trashed.clone();
    h.app.session.trash = Some(std::sync::Arc::new(move |p: &std::path::Path| {
        t_clone.lock().unwrap().push(p.to_path_buf());
        Ok(())
    }));

    exec(&mut h, "library.select", json!({"ids": [id.0]}));
    h.request("ui.key", json!({"key": "delete"}), T);

    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, disk_files: 1 }));
    h.step();
    assert!(has(&h, "button:dialogDeleteFromDisk"));

    click(&mut h, "button:dialogDeleteFromDisk");
    assert_eq!(h.app.ui.dialog, None);
    assert!(trashed.lock().unwrap().contains(&file));
    assert!(h.app.session.catalog.photo(id).is_none());

    exec(&mut h, "edit.undo", json!({}));
    assert!(h.app.session.catalog.photo(id).is_some());

    // with "Confirm before deleting" off, Photo ▸ Delete from Disk still asks first
    h.app.ui.settings.confirm_delete = false;
    exec(&mut h, "library.select", json!({"ids": [id.0]}));
    crate::menubar::run_item(&mut h.app, "photo.deleteFromDisk", json!({})).unwrap();
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, disk_files: 1 }), "the menu asks");
    h.request("ui.dialog.cancel", json!({}), T);
    assert!(h.app.session.catalog.photo(id).is_some(), "cancel keeps the photo");
    // and an agent calling the command directly has to confirm
    assert!(h.app.session.execute("photo.deleteFromDisk", &json!({})).is_err());
    assert!(h.app.session.catalog.photo(id).is_some());

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Histogram corner controls stay usable even before a histogram is available, and each
/// click invokes the same clipping-warning command as the J shortcut exactly once.
#[test]
fn histogram_corners_toggle_clipping_warnings() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    for texture in h.app.renderer.textures.values_mut() {
        texture.histogram = None;
    }
    h.step();
    h.app.ui.show_clipping = false;
    let histogram = h.app.widgets.iter().find(|(id, _)| id == "histogram").unwrap().1;
    for id in ["histogram:shadows", "histogram:highlights"] {
        let rect = h.app.widgets.iter().find(|(widget, _)| widget == id).unwrap().1;
        assert!(histogram.shrink(8.0).contains_rect(rect), "{id} keeps clear space to the histogram edges: {rect:?} in {histogram:?}");
    }
    for (id, enabled) in [("histogram:shadows", true), ("histogram:highlights", false), ("histogram", true)] {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.ui.show_clipping, enabled, "{id} must toggle exactly once");
    }
    let r = h.request("ui.key", json!({"key": "J"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.ui.show_clipping);
}

#[test]
fn the_histogram_clipping_buttons_stay_clear_of_the_hovered_scroll_bar() {
    // every section open: the Edit panel scrolls, so its scroll bar runs past the histogram
    let h = demo(
        [1400.0, 700.0],
        json!({"view": "detail", "right": "edit", "histogram": true, "openSections": ["light", "color", "effects", "detail", "optics"]}),
    );
    let hist = widget(&h, "histogram");
    let (low, high) = (widget(&h, "histogram:shadows"), widget(&h, "histogram:highlights"));
    // a hovered floating scroll bar grows to its full width at the panel's right edge, over the content
    let scroll = h.view.ctx.global_style().spacing.scroll;
    let bar_left = hist.right() - scroll.bar_outer_margin - scroll.bar_width;
    assert!(high.right() <= bar_left, "the highlight clipping button {high:?} is under the scroll bar (from x = {bar_left})");
    // and the shadow one keeps the same inset from its edge
    assert!(((low.left() - hist.left()) - (hist.right() - high.right())).abs() < 0.5, "{low:?} {high:?} in {hist:?}");
}

/// An imported folder is listed under its own name, not under every folder from the disk down to
/// it (the full path is on hover); below the first listed folder every folder nests in its own
/// parent, the ones holding only subfolders included (#1022).
#[test]
fn an_imported_folder_is_listed_by_its_own_name() {
    let h = folders_app(&["/Users/me/Desktop/shoot/c.jpg"]);
    assert!(has(&h, "source:libfolder:/Users/me/Desktop/shoot"), "the imported folder is a row straight under its disk");
    assert!(!has(&h, "source:libfolder:/Users") && !has(&h, "source:libfolder:/Users/me"), "no rows for the folders on the way");
    let h =
        folders_app(&["/Users/me/Pictures/2026/January/31st/a.jpg", "/Users/me/Pictures/2026/August/31st/b.jpg", "/Users/me/Pictures/2025/x/c.jpg"]);
    let tree = crate::panels::left::sidebar_folders(h.app.session.catalog.folder_tree(), &Default::default());
    let pictures = &tree[0].children[0];
    assert_eq!((pictures.name.as_str(), pictures.path.as_str()), ("Pictures", "/Users/me/Pictures"), "where the two branch");
    let y2026 = pictures.children.iter().find(|n| n.path == "/Users/me/Pictures/2026").expect("2026 is a row");
    let months: Vec<&str> = y2026.children.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(months, ["August", "January"], "the months holding only subfolders keep their rows");
    let jan = &y2026.children[1];
    assert_eq!(
        jan.children.iter().map(|n| (n.name.as_str(), n.path.as_str())).collect::<Vec<_>>(),
        [("31st", "/Users/me/Pictures/2026/January/31st")]
    );
    let y2025 = pictures.children.iter().find(|n| n.path == "/Users/me/Pictures/2025").expect("2025 is a row");
    assert_eq!(y2025.children.len(), 1, "a single subfolder below a listed folder is not lifted");
    // a folder that holds nothing itself and leads to one folder starts open: the tree opens down to
    // where the library branches
    let h = folders_app(&["/Users/me/Pictures/2024/a.jpg", "/Users/me/Pictures/2025/b.jpg"]);
    assert!(has(&h, "source:libfolder:/Users/me/Pictures"), "Users and me opened by themselves");
    assert!(!has(&h, "source:libfolder:/Users/me/Pictures/2024"), "the branching folder waits to be opened");
}

/// Show Parent Folder gives a folder hidden on the way down from the disk a row of its own in the
/// Folders list and chooses it (#1022).
#[test]
fn show_parent_folder_lists_the_parent_in_folders() {
    let mut h = folders_app(&["/Users/me/Desktop/shoot/c.jpg"]);
    assert!(!has(&h, "source:libfolder:/Users/me/Desktop"));
    right_click(&mut h, "source:libfolder:/Users/me/Desktop/shoot");
    click(&mut h, "folderShowParent");
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/Users/me/Desktop"));
    h.step();
    assert!(has(&h, "source:libfolder:/Users/me/Desktop"), "the parent is a row of the Folders list");
    assert!(has(&h, "source:libfolder:/Users/me/Desktop/shoot"), "with the folder inside it");
}

#[test]
fn export_folder_shows_the_folder_selects_its_photos_and_opens_export() {
    let mut h = folders_app_with_services(
        &["/pics/trip/a.jpg", "/pics/trip/day2/b.jpg", "/pics/home/c.jpg", "/Volumes/nas/d.jpg"],
        [1400.0, 900.0],
        reveal_and_pick_services(Ok(())),
    );
    click(&mut h, "libraryFolderToggle:/pics");
    right_click(&mut h, "source:libfolder:/pics/trip");
    click(&mut h, "folderExport");
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::LibraryFolder);
    assert_eq!(h.app.session.library_folder.as_deref(), Some("/pics/trip"));
    let photos_in = |h: &Headless, dir: &str| -> std::collections::BTreeSet<u64> {
        h.app
            .session
            .catalog
            .photos()
            .filter(|p| matches!(&p.source, lcc_catalog::Source::File { path } if path.starts_with(dir)))
            .map(|p| p.id.0)
            .collect()
    };
    let selected: std::collections::BTreeSet<u64> = h.app.session.selection.ids.iter().map(|i| i.0).collect();
    assert_eq!(selected.len(), 2, "the folder's photo and the one in the folder inside it");
    assert_eq!(selected, photos_in(&h, "/pics/trip"));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Export, "the Export module is open");
    assert!(!popup_open(&h), "choosing closes the menu");
}

#[test]
fn export_folder_is_grayed_out_for_a_row_that_cant_be_shown_or_holds_nothing() {
    let mut h = folders_app_with_services(&["/Volumes/1.jpg", "/Volumes/tokyo/x.jpg"], [1400.0, 900.0], reveal_and_pick_services(Ok(())));
    right_click(&mut h, "source:libfolder:/Volumes");
    assert!(has(&h, "folderExport") && has(&h, "folderExportPresetMenu"), "the rows are listed");
    h.app.ui.toasts.clear();
    click(&mut h, "folderExport");
    assert_eq!(h.app.session.library_folder, None, "a grayed-out row shows nothing");
    assert!(h.app.ui.dialog.is_none());
    // an enabled row would try, and the engine's refusal would show as a toast
    assert!(h.app.ui.toast().is_none(), "a grayed-out row does nothing: {:?}", h.app.ui.toast());
    let node = |selectable, count| lcc_catalog::FolderNode {
        name: "x".into(),
        path: "/x".into(),
        count,
        own: count,
        volume: false,
        selectable,
        label: None,
        children: vec![],
    };
    assert!(crate::panels::left::folder_export_blocker(&node(false, 3)).is_some());
    assert!(crate::panels::left::folder_export_blocker(&node(true, 0)).is_some());
    assert!(crate::panels::left::folder_export_blocker(&node(true, 3)).is_none());
}

/// The Missing Photos row appears when the background count of missing files lands, and moves
/// every row below it down. Settling waits for that count, so a click aimed at a folder row from
/// the settled layout doesn't land on the row above it.
#[test]
fn settling_waits_for_the_missing_photos_count() {
    // longer than settling takes without the count (a few hundred ms here)
    let delay = Duration::from_secs(1);
    let paths = ["/lc-missing-count/other/a.jpg", "/lc-missing-count/trip/b.jpg"];
    let services = Services { png: None, ..Default::default() };
    let ui = json!({"view": "photoGrid", "leftPanel": true, "moduleBar": false});
    let mut h = folders_app_prepared(&paths, [1400.0, 900.0], ui, services, |app| app.missing_count_test_delay = Some(delay));
    assert!(has(&h, "source:missing"), "the Missing Photos row is on screen once the sidebar settled");
    click(&mut h, "libraryFolderToggle:/lc-missing-count");
    let settled = widget(&h, "source:libfolder:/lc-missing-count/trip");
    let t0 = std::time::Instant::now();
    while t0.elapsed() < delay / 4 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(widget(&h, "source:libfolder:/lc-missing-count/trip"), settled, "nothing moves the folder rows after settling");
    // the timed refresh of a slow count is not waited for: the fast simulated clock would start
    // the next one before the last finished, and settling would never end
    assert!(h.settle(Duration::from_secs(30)), "the sidebar settles while a slow count refreshes");
}

/// "I" toggles the Info panel; pressing it again brings back exactly what the right column showed
/// before, closed included (ported from OneSlips' KEY-INFO).
#[test]
fn info_toggled_off_brings_back_exactly_what_was_there() {
    use crate::state::RightPanel;
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let exec = |h: &mut Headless, id: &str| {
        let r = h.request("ui.menu.invoke", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        h.step();
    };
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Info);
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Edit, "Edit → I → I = Edit");
    exec(&mut h, "panel.masking");
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Info);
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Masking, "Masks → I → I = Masks");
    h.app.ui.right = RightPanel::None;
    h.step();
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Info);
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::None, "closed → I → I = closed");
}

/// Assisted Culling: the eyes are asked about; with "blurry eyes" ticked the dialog asks how soft is
/// too soft, and whether only the main subjects count; the choices reach the dialog's state.
#[test]
fn assisted_culling_asks_about_the_eyes() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    exec(&mut h, "dialog.cull", json!({}));
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "radio:cullSoftToo"), "only once blurry eyes are asked for");
    click(&mut h, "check:cullBlurryEyes");
    click(&mut h, "radio:cullSoftToo");
    click(&mut h, "check:cullMainOnly");
    let Some(crate::state::Dialog::Cull { blurry_eyes, soft_eyes, everyone, closed_eyes, .. }) = h.app.ui.dialog.clone() else {
        panic!("the dialog stays")
    };
    assert!(blurry_eyes && soft_eyes && everyone && !closed_eyes);
}

/// View ▸ Space Zooms To: the choice is kept with the settings, turns Zoom to Focus Point on, and
/// a word it doesn't know is refused. Without a camera focus point Space zooms to the eyes; "eyes
/// first" prefers them to the point.
#[test]
fn space_zooms_to_is_a_choice_in_the_view_menu() {
    use crate::state::SpaceZoom;
    let mut h = demo([1400.0, 900.0], json!({"view": "detail"}));
    exec(&mut h, "view.zoomToFocus", json!({"on": false}));
    exec(&mut h, "view.spaceZoomsTo", json!({"to": "eye"}));
    assert_eq!(h.app.ui.settings.space_zoom, SpaceZoom::Eye);
    assert!(h.app.ui.settings.zoom_to_focus);
    let r = h.request("engine.execute", json!({"command": "view.spaceZoomsTo", "params": {"to": "nose"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(h.app.ui.settings.space_zoom, SpaceZoom::Eye);
    let (point, eye) = (Some((0.2, 0.3)), Some((0.6, 0.4)));
    use crate::panels::detail::space_target;
    assert_eq!(space_target(SpaceZoom::FocusPoint, point, eye), point);
    assert_eq!(space_target(SpaceZoom::FocusPoint, None, eye), eye, "no focus point: the eyes");
    assert_eq!(space_target(SpaceZoom::Eye, point, eye), eye);
    assert_eq!(space_target(SpaceZoom::Eye, point, None), point, "no face: the focus point");
    assert_eq!(space_target(SpaceZoom::Eye, None, None), None);
    // a settings file with a word it doesn't know reads as the default
    let s: crate::state::AppSettings = serde_json::from_value(json!({"spaceZoom": "nose"})).expect("lenient");
    assert_eq!(s.space_zoom, SpaceZoom::FocusPoint);
}

/// Issue #774: folders and collections remember their open/closed state across sessions.
#[test]
fn remembering_folder_and_collection_conditions_across_sessions() {
    let mut h = folders_app(&["/pics/sub/a.jpg", "/pics/sub/b.jpg", "/pics/other/c.jpg"]);
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let europe = make(&mut h, json!({"name": "Europe", "folder": true, "parent": trips}));
    let _best = make(&mut h, json!({"name": "Best", "parent": europe}));
    h.settle(SETTLE);

    // Initial state: /pics subfolders folded, album folders start open
    assert!(!has(&h, "source:libfolder:/pics/sub"));
    assert!(has(&h, &format!("albumToggle:{trips}")));
    assert!(has(&h, &format!("albumToggle:{europe}")));

    // Unfold /pics folder in Folders
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(has(&h, "source:libfolder:/pics/sub"));

    // Collapse Europe album folder in Collections
    click(&mut h, &format!("albumToggle:{europe}"));

    // Serialize ui state to JSON as the app writer does on quit
    let saved_ui = serde_json::to_value(&h.app.ui).unwrap();

    // Verify tree_open recorded the conditions
    assert_eq!(h.app.ui.tree_open.get("libfolder:/pics"), Some(&true));
    assert_eq!(h.app.ui.tree_open.get(&format!("album:{europe}")), Some(&false));

    // Deserialized UiState preserves tree_open
    let restored = serde_json::from_value::<crate::UiState>(saved_ui.clone()).unwrap().sanitized();
    assert_eq!(restored.tree_open.get("libfolder:/pics"), Some(&true));
    assert_eq!(restored.tree_open.get(&format!("album:{europe}")), Some(&false));

    // Re-create app session with restored ui settings
    let mut h2 = folders_app_prepared(
        &["/pics/sub/a.jpg", "/pics/sub/b.jpg", "/pics/other/c.jpg"],
        [1400.0, 980.0],
        serde_json::json!({"view": "photoGrid", "leftPanel": true, "moduleBar": false}),
        Services { png: None, ..Default::default() },
        |app| {
            app.ui = restored.clone();
        },
    );
    let trips2 = make(&mut h2, json!({"name": "Trips", "folder": true}));
    let europe2 = make(&mut h2, json!({"name": "Europe", "folder": true, "parent": trips2}));
    let _best2 = make(&mut h2, json!({"name": "Best", "parent": europe2}));
    h2.settle(SETTLE);

    // In restored session, /pics folder stays open, and Europe album folder stays collapsed!
    assert!(has(&h2, "source:libfolder:/pics/sub"), "remembered /pics was expanded");
    assert_eq!(h2.app.ui.tree_open.get(&format!("album:{europe2}")), Some(&false));
}

/// `ui.prompt` asks for one line of text and then runs the named command with it; bad calls
/// are errors, not dialogs.
#[test]
fn ui_prompt_asks_for_a_name_then_runs_the_command_with_it() {
    let mut h = folders_app(&["/pics/trip/a.jpg"]);
    for bad in [json!({}), json!({"title": "T", "command": "preset.createGroup"}), json!({"title": "T", "command": "ui.prompt", "key": "name"})] {
        assert!(h.app.run("ui.prompt", bad).is_err());
        assert!(h.app.ui.dialog.is_none());
    }
    assert!(h.app.run("ui.prompt", json!({"title": "T", "command": "preset.createGroup", "key": "name", "params": 3})).is_err());
    h.app.run("ui.prompt", json!({"title": "New Preset Folder", "hint": "Folder name", "command": "preset.createGroup", "key": "name"})).unwrap();
    h.step();
    let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    *value = "  Wedding ".into();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.preset_folders.iter().any(|f| f == "Wedding"), "{:?}", h.app.session.preset_folders);
}
