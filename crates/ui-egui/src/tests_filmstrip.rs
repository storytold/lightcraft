//! Headless tests of the filmstrip: file-name labels (#266) — a name with multi-byte (CJK)
//! characters is drawn without panicking — and its stack badges, which expand / collapse a stack.

use std::time::Duration;

use lightcraft_catalog::{Op, Stack, StackId};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

#[test]
fn filmstrip_shows_a_cjk_file_name() {
    let mut session = lightcraft_engine::Session::with_demo();
    let first = session.visible_cloned()[0];
    let source = session.catalog.photo(first).expect("demo photo").source.clone();
    // the name from #266: cutting it at byte 13 lands inside '限'
    session.catalog.apply(Op::SetFile { id: first, file_name: "202407層三限定訂閱圖(4).jpg".into(), source }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.settings.film_names);
    let film = format!("film:{}", first.0);
    assert!(h.app.widgets.iter().any(|(w, _)| *w == film), "{film} drawn in the filmstrip");
}

#[test]
fn filmstrip_stack_badge_expands_and_collapses_the_stack() {
    let mut session = lightcraft_engine::Session::with_demo();
    let vis = session.visible_cloned();
    let (loose, top, member) = (vis[0], vis[1], vis[2]);
    let stack = Stack { id: StackId(1), photos: vec![top, member], collapsed: true };
    session.catalog.apply(Op::AddStack { stack }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // the loose photo is the one in the loupe: the stack isn't selected
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [loose.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.session.visible_cloned().contains(&member), "collapsed: only the top shows");
    let badge = format!("filmstack:{}", top.0);
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a click on the badge expands the stack");
    assert!(h.app.session.visible_cloned().contains(&member));
    assert_eq!(h.app.session.selection.active, Some(loose), "the loupe keeps its photo");
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a second click collapses it");
    assert!(!h.app.session.visible_cloned().contains(&member));
}

/// The strokes a filmstrip cell paints around its photo: the rectangles stroked in `color`
/// that lie inside the cell.
fn rings_in(h: &Headless, id: lightcraft_catalog::PhotoId, color: egui::Color32) -> Vec<egui::Rect> {
    let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r).expect("cell drawn");
    h.view
        .painted_shapes()
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.stroke.color == color && r.stroke.width > 0.0 && cell.expand(2.0).contains_rect(r.rect) => Some(r.rect),
            _ => None,
        })
        .collect()
}

/// Until its thumbnail arrives, the active photo's cell and a selected one's still carry
/// the white and gray rings of the loaded cells, around the same placeholder the grid draws.
#[test]
fn filmstrip_rings_selected_cells_whose_thumbnail_is_not_loaded() {
    let mut session = lightcraft_engine::Session::with_demo();
    let vis = session.visible_cloned();
    let (active, other) = (vis[0], vis[1]);
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // renders are held: the thumbnails stay unloaded however fast the machine is
    h.app.renderer.set_offload(Box::new(crate::headless::RenderGate::new(std::rc::Rc::new(std::cell::Cell::new(false)))));
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [active.0, other.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.session.selection.active, Some(active));
    assert!(h.app.renderer.thumb(active).is_none() && h.app.renderer.thumb(other).is_none(), "thumbnails are still loading");
    let white = rings_in(&h, active, egui::Color32::WHITE);
    let gray = rings_in(&h, other, egui::Color32::from_gray(170));
    assert_eq!(white.len(), 1, "the active cell has a white ring: {white:?}");
    assert_eq!(gray.len(), 1, "the other selected cell has a gray ring: {gray:?}");
    let cell = |id: lightcraft_catalog::PhotoId| h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r).unwrap();
    let placeholder = egui::Rect::from_min_max(cell(active).min + egui::vec2(10.0, 22.0), cell(active).max - egui::vec2(10.0, 8.0));
    assert!(white[0].expand(1.0).contains_rect(placeholder), "the ring surrounds the placeholder: {:?} vs {placeholder:?}", white[0]);
    assert!(rings_in(&h, vis[2], egui::Color32::WHITE).is_empty(), "an unselected cell has no ring");
    assert!(rings_in(&h, vis[2], egui::Color32::from_gray(170)).is_empty(), "an unselected cell has no gray ring");
}
