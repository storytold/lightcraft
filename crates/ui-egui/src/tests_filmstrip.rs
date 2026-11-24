//! Headless tests of the filmstrip: file-name labels (#266) — a name with multi-byte (CJK)
//! characters is drawn without panicking — and its stack badges, which expand / collapse a stack.

use std::time::Duration;

use lcc_catalog::{Op, Stack, StackId};
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A file name with multi-byte (CJK) characters is drawn without panicking (#266), and a stack's badge expands and
/// collapses the stack while the loupe keeps its photo.
#[test]
fn filmstrip_shows_a_cjk_file_name_and_its_stack_badge_expands_and_collapses_the_stack() {
    let mut session = lcc_engine::Session::with_demo();
    let vis = session.visible_cloned();
    let (loose, top, member) = (vis[0], vis[1], vis[2]);
    let source = session.catalog.photo(loose).expect("demo photo").source.clone();
    // the name from #266: cutting it at byte 13 lands inside '限'
    session.catalog.apply(Op::SetFile { id: loose, file_name: "202407層三限定訂閱圖(4).jpg".into(), source }).unwrap();
    let stack = Stack { id: StackId(1), photos: vec![top, member], collapsed: true };
    session.catalog.apply(Op::AddStack { stack }).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // the loose photo is the one in the loupe: the stack isn't selected
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [loose.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.settings.film_names);
    let film = format!("film:{}", loose.0);
    assert!(h.app.widgets.iter().any(|(w, _)| *w == film), "{film} drawn in the filmstrip with its CJK name");
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

/// The filmstrip rings its selected photos by the same rule as the grid, in the theme's colours:
/// the active photo's ring is heavier than the other selected photo's, and a light theme gets no
/// fixed white or grey ring.
#[test]
fn the_filmstrip_marks_the_active_photo_heavier_than_the_other_selected_one() {
    use egui::epaint::Shape;
    let mut session = lcc_engine::Session::with_demo();
    let ids = session.visible_cloned();
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
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
    let strokes = |id: lcc_catalog::PhotoId| -> Vec<(f32, egui::Color32)> {
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

/// Resizing the strip keeps the active photo where it was on screen (#1082).
#[test]
fn resizing_the_filmstrip_keeps_the_active_photo_in_place() {
    use crate::panels::detail::film_rescroll;
    // photo 40 drawn with its centre 300 points into the strip
    let (old_w, new_w, at) = (100.0, 150.0, 40usize);
    let old_off = (at as f32 + 0.5) * old_w - 300.0;
    let new_off = film_rescroll(old_off, old_w, new_w, Some(at));
    assert!(((at as f32 + 0.5) * new_w - new_off - 300.0).abs() < 0.01);
    // never before the start, and hostile numbers do not escape
    assert_eq!(film_rescroll(0.0, 100.0, 50.0, Some(0)), 0.0);
    assert_eq!(film_rescroll(f32::NAN, 100.0, 50.0, None), 0.0);
    assert_eq!(film_rescroll(10.0, 0.0, 50.0, None), 0.0);
}

/// The filmstrip's header filters, as in Lightroom Classic (#1296): flags, then the rating, then the colour
/// labels, left to right; a click narrows the strip to the matching photos and never flags or rates one.
#[test]
fn the_filmstrip_header_filters_by_flag_rating_and_label_in_lightroom_order() {
    let mut session = lcc_engine::Session::with_demo();
    let ids = session.visible_cloned();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 900.0], 1.0);
    let (picked, other) = (ids[0], ids[1]);
    for (cmd, params) in [
        ("library.select", json!({"ids": [picked.0]})),
        ("photo.flag", json!({"flag": "pick"})),
        ("photo.rate", json!({"rating": 3})),
        ("library.select", json!({"ids": [other.0]})),
    ] {
        let r = h.request("engine.execute", json!({"command": cmd, "params": params}), T);
        assert_eq!(r["ok"], true, "{cmd}: {r}");
    }
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.center().x);
    let order = [
        "filmfilter:pick",
        "filmfilter:none",
        "filmfilter:reject",
        "filmfilter:ratingOp",
        "filmfilter:star1",
        "filmfilter:star5",
        "filmfilter:label-red",
    ];
    let xs: Vec<f32> = order.iter().map(|id| at(&h, id).unwrap_or_else(|| panic!("{id} drawn"))).collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "flags, then rating, then labels: {xs:?}");
    let ratings_before: Vec<u8> = ids.iter().map(|id| h.app.session.catalog.photo(*id).unwrap().rating).collect();

    // the pick flag filter: only picks stay in the strip
    let r = h.request("ui.clickWidget", json!({"id": "filmfilter:pick"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.filter.flag, Some(lcc_catalog::Flag::Pick));
    let vis = h.app.session.visible_cloned();
    assert!(vis.contains(&picked) && !vis.contains(&other), "{vis:?}");
    assert!(vis.iter().all(|id| h.app.session.catalog.photo(*id).unwrap().flag == lcc_catalog::Flag::Pick), "only picks: {vis:?}");
    assert_eq!(h.app.session.catalog.photo(other).unwrap().flag, lcc_catalog::Flag::None, "a filter click flags nothing");
    // a second click lifts it
    let r = h.request("ui.clickWidget", json!({"id": "filmfilter:pick"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.filter.flag, None);

    // three stars or more
    let r = h.request("ui.clickWidget", json!({"id": "filmfilter:star3"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.filter.rating, 3);
    let vis = h.app.session.visible_cloned();
    assert!(vis.contains(&picked) && vis.iter().all(|id| h.app.session.catalog.photo(*id).unwrap().rating >= 3), "{vis:?}");
    let ratings_after: Vec<u8> = ids.iter().map(|id| h.app.session.catalog.photo(*id).unwrap().rating).collect();
    assert_eq!(ratings_before, ratings_after, "a star click rates nothing");

    // a colour label
    let r = h.request("ui.clickWidget", json!({"id": "filmfilter:label-red"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.filter.labels_shown().unwrap_or_default(), vec![lcc_catalog::ColorLabel::Red]);
}
