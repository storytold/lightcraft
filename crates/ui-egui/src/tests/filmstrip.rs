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
    // Badge routing needs three real photos, not full 24 MP demo renders.
    let library = crate::tests_fixtures::SyntheticLibrary::with_photos("filmstrip-stack-badge", 3);
    let mut app = library.app();
    let vis = app.session.visible_cloned();
    let (loose, top, member) = (vis[0], vis[1], vis[2]);
    let stack = Stack { id: StackId(1), photos: vec![top, member], collapsed: true };
    app.session.catalog.apply(Op::AddStack { stack }).unwrap();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // the loose photo is the one in the loupe: the stack isn't selected
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [loose.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the filmstrip settles before clicking its stack badge");
    assert!(!h.app.session.visible_cloned().contains(&member), "collapsed: only the top shows");
    let badge = format!("filmstack:{}", top.0);
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the expanded stack settles");
    assert!(!h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a click on the badge expands the stack");
    assert!(h.app.session.visible_cloned().contains(&member));
    assert_eq!(h.app.session.selection.active, Some(loose), "the loupe keeps its photo");
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the collapsed stack settles");
    assert!(h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a second click collapses it");
    assert!(!h.app.session.visible_cloned().contains(&member));
}

/// The filmstrip rings its selected photos by the same rule as the grid, in the theme's colours:
/// the active photo's ring is heavier than the other selected photo's, and a light theme gets no
/// fixed white or grey ring.
#[test]
fn the_filmstrip_marks_the_active_photo_heavier_than_the_other_selected_one() {
    use egui::epaint::Shape;
    let mut session = lightcraft_engine::Session::with_demo();
    let ids = session.visible_cloned();
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.settings.appearance_mode = crate::state::AppearanceMode::Light;
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [a.0, b.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let t = crate::theme::Tokens::get(&h.view.ctx);
    assert!(crate::theme::current(&h.view.ctx).is_some_and(|k| !k.is_dark()));
    let active = h.app.session.selection.active.expect("active");
    let other = if active == a { b } else { a };
    let shapes = h.view.painted_shapes();
    let strokes = |id: lightcraft_catalog::PhotoId| -> Vec<(f32, egui::Color32)> {
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r).expect("cell drawn");
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(rs) if rs.stroke.width > 0.0 && rs.stroke_kind == egui::StrokeKind::Outside && cell.contains_rect(rs.rect) => {
                    Some((rs.stroke.width, rs.stroke.color))
                }
                _ => None,
            })
            .collect()
    };
    let (on_active, on_other, on_unselected) = (strokes(active), strokes(other), strokes(c));
    assert_eq!(on_active, vec![(3.0, t.ring_active), (1.0, t.ring_inner)]);
    assert_eq!(on_other, vec![(1.5, t.ring_selected)]);
    assert!(on_unselected.is_empty(), "{on_unselected:?}");
}
