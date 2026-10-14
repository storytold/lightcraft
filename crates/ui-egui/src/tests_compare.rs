//! Headless tests of Compare with the filmstrip.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    Headless::new(app, [1400.0, 900.0], 1.0)
}

/// In Compare, the photo the Edit panel and the rating keys act on is always one of the two on
/// screen: a filmstrip click must not leave the pair showing while a third photo becomes active.
#[test]
fn filmstrip_click_in_compare_keeps_the_active_photo_on_screen() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let rating = |h: &Headless, id: u64| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).map(|p| p.rating);
    for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
        let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.app.ui.zoom = zoom;
        h.settle(SETTLE);
        let target = vis[4];
        let before = rating(&h, target);
        let r = h.request("ui.clickWidget", json!({"id": format!("film:{target}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let (sel, cand) = h.app.ui.compare.expect("still comparing");
        let active = h.app.session.selection.active.map(|p| p.0).expect("an active photo");
        // a rating key must not change a photo that isn't shown
        h.request("ui.key", json!({"key": "3"}), T);
        if target != sel && target != cand {
            assert_eq!(rating(&h, target), before, "zoom {zoom:?}: the hidden photo {target} was rated");
        }
        assert!(active == sel || active == cand, "zoom {zoom:?}: active photo {active} is not on screen (compare shows {sel} | {cand})");
    }
}

/// A filmstrip click in Compare puts the photo into the active side: the candidate after
/// entering Compare, the select after clicking the select's pane; a photo already on the other
/// side swaps the two. `compare.set` does the same for agents, with an explicit `side`.
#[test]
fn filmstrip_click_replaces_the_active_side_in_compare() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    let (sel, cand) = (r["result"]["select"].as_u64().unwrap(), r["result"]["candidate"].as_u64().unwrap());
    h.settle(SETTLE);
    let click_film = |h: &mut Headless, id: u64| {
        let r = h.request("ui.clickWidget", json!({"id": format!("film:{id}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    let active = |h: &Headless| h.app.session.selection.active.map(|p| p.0);
    let others: Vec<u64> = vis.iter().copied().filter(|v| *v != sel && *v != cand).take(3).collect();
    // the candidate is active after entering Compare: it is replaced
    click_film(&mut h, others[0]);
    assert_eq!(h.app.ui.compare, Some((sel, others[0])));
    assert_eq!(active(&h), Some(others[0]));
    // the select's photo, clicked in the filmstrip: the two swap, it stays active
    click_film(&mut h, sel);
    assert_eq!(h.app.ui.compare, Some((others[0], sel)));
    assert_eq!(active(&h), Some(sel));
    // a side chosen explicitly
    let r = h.request("engine.execute", json!({"command": "compare.set", "params": {"id": others[1], "side": "select"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.compare, Some((others[1], sel)));
    assert_eq!(active(&h), Some(others[1]));
    // the select is active now: the next filmstrip click replaces it
    click_film(&mut h, others[2]);
    assert_eq!(h.app.ui.compare, Some((others[2], sel)));
    let r = h.request("engine.execute", json!({"command": "compare.set", "params": {"id": others[0], "side": "left"}}), T);
    assert_eq!(r["ok"], false, "unknown side is an error: {r}");
}

/// Compare is where sharpness is judged: while a pane's render of an edit is on its way, that pane
/// says "Rendering…" once it has lasted a moment, and stops when the render lands.
#[test]
fn a_compare_pane_says_rendering_while_its_edit_renders() {
    use std::{cell::Cell, rc::Rc};
    let pill = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w.starts_with("compare:rendering:"));
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!pill(&h), "finished renders");
    let open = Rc::new(Cell::new(false));
    h.app.renderer.set_offload(Box::new(crate::headless::RenderGate::new(open.clone())));
    h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), T);
    for _ in 0..12 {
        h.step();
    }
    assert!(!pill(&h), "not at once");
    for _ in 0..30 {
        h.step();
    }
    assert!(pill(&h), "the edited pane's render is on its way");
    open.set(true);
    assert!(h.step_until(SETTLE, |h| !pill(h)), "gone once the render lands");
}

/// A pane whose render fails behind its thumbnail says so, as the loupe does, instead of showing
/// the thumbnail as if it were the photo.
#[test]
fn a_compare_pane_says_when_its_render_failed() {
    use crate::render::Slot;
    use std::{cell::Cell, rc::Rc};
    let failed = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w.starts_with("compare:renderFailed:"));
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    h.request("engine.execute", json!({"command": "view.compare"}), T);
    h.settle(SETTLE);
    assert!(!failed(&h));
    let open = Rc::new(Cell::new(true));
    h.app.renderer.set_offload(Box::new(crate::headless::RenderGate::failing(open, |s| matches!(s, Slot::Compare(_)))));
    // another photo in the second pane: its render fails, its thumbnail stays up
    let r = h.request("engine.execute", json!({"command": "compare.set", "params": {"id": vis[2], "side": "candidate"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, failed), "the failure is said");
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("compare:rendering:")));
}
