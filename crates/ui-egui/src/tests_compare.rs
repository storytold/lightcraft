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

/// A ⌘/Ctrl-click or Shift-click on the filmstrip in Compare changes the selection as it does in
/// the grid (#187): the photo is toggled, or the range from the active photo is added. The two
/// photos shown stay selected and the active one stays active, so a rating key still acts on a
/// photo that is on screen (#744).
#[test]
fn modified_filmstrip_clicks_in_compare_change_the_selection_not_the_pair() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    let (sel, cand) = (r["result"]["select"].as_u64().unwrap(), r["result"]["candidate"].as_u64().unwrap());
    h.settle(SETTLE);
    let click_film = |h: &mut Headless, id: u64, cmd: bool, shift: bool| {
        let r = h.request("ui.clickWidget", json!({"id": format!("film:{id}"), "cmd": cmd, "shift": shift}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    let selected = |h: &Headless| -> Vec<u64> {
        let s = &h.app.session.selection;
        vis.iter().copied().filter(|v| s.contains(lightcraft_catalog::PhotoId(*v))).collect()
    };
    let active = |h: &Headless| h.app.session.selection.active.map(|p| p.0);
    let rating = |h: &Headless, id: u64| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).map(|p| p.rating);
    let others: Vec<u64> = vis.iter().copied().filter(|v| *v != sel && *v != cand).collect();
    assert!(others.len() >= 4, "the demo library has {} photos", vis.len());
    let before = active(&h);
    // ⌘-click: a third photo joins the selection; the pair and the active photo stay
    click_film(&mut h, others[0], true, false);
    assert_eq!(selected(&h), {
        let mut v = vec![sel, cand, others[0]];
        v.sort_by_key(|id| vis.iter().position(|v| v == id));
        v
    });
    assert_eq!(h.app.ui.compare, Some((sel, cand)), "the pair is untouched");
    assert_eq!(active(&h), before, "the active photo is untouched");
    // ⌘-click again: it leaves
    click_film(&mut h, others[0], true, false);
    assert_eq!(selected(&h), {
        let mut v = vec![sel, cand];
        v.sort_by_key(|id| vis.iter().position(|v| v == id));
        v
    });
    assert_eq!(h.app.ui.compare, Some((sel, cand)));
    assert_eq!(active(&h), before);
    // Shift-click: everything from the active photo to the clicked one is selected
    let far = others[3];
    click_film(&mut h, far, false, true);
    let (a, b) = (vis.iter().position(|v| Some(*v) == before).unwrap(), vis.iter().position(|v| *v == far).unwrap());
    let now = selected(&h);
    for id in &vis[a.min(b)..=a.max(b)] {
        assert!(now.contains(id), "{id} is in the range {before:?}..{far}: {now:?}");
    }
    assert!(now.contains(&sel) && now.contains(&cand), "the pair stays selected: {now:?}");
    assert_eq!(h.app.ui.compare, Some((sel, cand)));
    assert_eq!(active(&h), before, "Shift-click does not move the active photo in Compare");
    // ⌘-click on a photo that is on screen: it cannot leave the selection while it is compared
    click_film(&mut h, sel, true, false);
    assert!(selected(&h).contains(&sel), "the select stays selected");
    assert_eq!(h.app.ui.compare, Some((sel, cand)));
    assert_eq!(active(&h), before);
    // ⌘-click on the active photo: `toggle` would hand "active" to another photo; it stays put
    click_film(&mut h, before.unwrap(), true, false);
    assert!(selected(&h).contains(&before.unwrap()), "the active photo stays selected");
    assert_eq!(h.app.ui.compare, Some((sel, cand)));
    assert_eq!(active(&h), before);
    // a rating key acts on the active photo, which is on screen; a merely selected photo is left alone
    let was = rating(&h, far);
    h.request("ui.key", json!({"key": "3"}), T);
    h.settle(SETTLE);
    assert_eq!(rating(&h, before.unwrap()), Some(3));
    assert_eq!(rating(&h, far), was, "the hidden photo was rated");
    // a plain click still replaces the active side
    click_film(&mut h, others[1], false, false);
    assert_ne!(h.app.ui.compare, Some((sel, cand)));
    assert!(h.app.ui.compare.is_some_and(|(s, c)| s == others[1] || c == others[1]));
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

#[test]
fn reference_escape_leaves_overlays_then_the_view_without_changing_edits() {
    use crate::state::{LightsOut, ViewMode};
    let mut h = demo();
    let reference = h.app.session.visible_cloned()[0];
    h.app.run("library.select", json!({"ids": [reference.0]})).unwrap();
    h.app.run("view.reference", json!({})).unwrap();
    let active = h.app.session.active().unwrap();
    h.app.run("develop.set", json!({"control": "light.exposure", "value": 0.7})).unwrap();
    h.app.ui.lights_out = LightsOut::Dim;
    h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!((h.app.ui.view, h.app.ui.lights_out), (ViewMode::Reference, LightsOut::On));
    h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert_eq!(h.app.ui.reference, Some(reference.0), "leaving the view keeps the pinned photo");
    assert_eq!(h.app.session.active(), Some(active));
    assert_eq!(h.app.session.develop_of(active).unwrap().light.exposure, 0.7);
    h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
}

#[test]
fn invalid_or_deleted_reference_is_refused_without_replacing_the_pin() {
    let mut h = demo();
    let ids = h.app.session.visible_cloned();
    h.app.run("library.select", json!({"ids": [ids[0].0]})).unwrap();
    h.app.run("photo.setReference", json!({})).unwrap();
    h.app.run("photo.delete", json!({"ids": [ids[1].0]})).unwrap();
    for id in [u64::MAX, ids[1].0] {
        let r = h.app.run("photo.setReference", json!({"id": id}));
        assert!(r.is_err(), "unavailable photo {id} must not be pinned");
        assert_eq!(h.app.ui.reference, Some(ids[0].0));
    }
    let deleted = h.app.run("photo.setReference", json!({"id": ids[1].0})).unwrap_err();
    assert!(deleted.contains("Recently Deleted"), "the caller learns to restore it: {deleted}");
    // a malformed id is refused, not taken for "the active photo"
    h.app.run("library.select", json!({"ids": [ids[2].0]})).unwrap();
    for id in [json!(ids[2].0.to_string()), json!(-1), json!(1.5), json!([ids[2].0])] {
        assert!(h.app.run("photo.setReference", json!({"id": id})).is_err(), "{id}");
        assert_eq!(h.app.ui.reference, Some(ids[0].0));
    }
    h.app.run("photo.setReference", json!({"id": null})).unwrap();
    assert_eq!(h.app.ui.reference, Some(ids[2].0), "a null id means the active photo");
    h.app.run("photo.setReference", json!({"id": ids[0].0})).unwrap();
    h.app.run("library.select", json!({"ids": []})).unwrap();
    assert!(h.app.run("photo.setReference", json!({})).is_err());
    assert_eq!(h.app.ui.reference, Some(ids[0].0));
}

#[test]
fn deleting_the_pinned_reference_stops_drawing_it_and_reentry_uses_a_live_photo() {
    let mut h = demo();
    let reference = h.app.session.visible_cloned()[0];
    h.app.run("library.select", json!({"ids": [reference.0]})).unwrap();
    h.app.run("view.reference", json!({})).unwrap();
    h.settle(SETTLE);
    let texture = h.app.renderer.textures.get(&crate::render::Slot::Compare(0)).unwrap().tex.id();
    let drawn = |h: &Headless| h.view.painted_shapes().iter().any(|s| matches!(s, egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == texture));
    assert!(drawn(&h), "the live reference is shown before deletion");
    let active = h.app.session.active().unwrap();
    h.app.run("photo.delete", json!({"ids": [reference.0]})).unwrap();
    h.settle(SETTLE);
    assert!(!drawn(&h), "Recently Deleted photos must not remain on the reference canvas");
    assert!(!h.app.renderer.textures.contains_key(&crate::render::Slot::Compare(0)), "the deleted reference's pane is released");
    assert!(!h.app.renderer.textures.contains_key(&crate::render::Slot::Compare(1)), "and the active photo's pane beside it");
    assert!(h.app.compare_windows[0].is_none());
    let r = h.app.run("view.reference", json!({})).unwrap();
    assert_eq!(r["reference"], active.0);
    assert_eq!(h.app.ui.reference, Some(active.0));
    h.app.run("photo.restore", json!({"ids": [reference.0]})).unwrap();
    h.app.run("photo.setReference", json!({"id": reference.0})).unwrap();
    h.settle(SETTLE);
    assert!(h.app.renderer.textures.get(&crate::render::Slot::Compare(0)).is_some_and(|t| t.photo == reference));
}

/// Clicking a colour label (or stars) in the Info panel while culling labels the active photo
/// only, as the label keys and the bottom bar's stars and flags do, and the label shows under that
/// photo in Compare (#2398: "Can't assign color to photo when culling").
#[test]
fn info_panel_label_in_compare_labels_the_active_photo_and_shows_it() {
    use lightcraft_catalog::{ColorLabel, PhotoId};
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    for id in &vis[..2] {
        h.request("engine.execute", json!({"command": "photo.label", "params": {"label": "none", "ids": [id]}}), T);
        h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 0, "ids": [id]}}), T);
    }
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let (sel, cand) = (r["result"]["select"].as_u64().unwrap(), r["result"]["candidate"].as_u64().unwrap());
    h.app.ui.right = crate::state::RightPanel::Info;
    h.settle(SETTLE);
    let photo = |h: &Headless, id: u64| h.app.session.catalog.photo(PhotoId(id)).cloned().expect("photo");
    let widgets = |h: &mut Headless| h.request("ui.widgets", json!({}), T)["result"].to_string();
    assert!(!widgets(&mut h).contains(&format!("cull:{cand}:label")), "no label yet");
    let r = h.request("ui.clickWidget", json!({"id": "label:red"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(photo(&h, cand).label, Some(ColorLabel::Red), "the active photo (the candidate) is labelled");
    assert_eq!(photo(&h, sel).label, None, "the select is not labelled with it");
    assert!(widgets(&mut h).contains(&format!("cull:{cand}:label")), "the label shows under the candidate");
    let r = h.request("ui.clickWidget", json!({"id": "star:info:3"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(photo(&h, cand).rating, 3);
    assert_eq!(photo(&h, sel).rating, 0, "the select is not rated with it");
}
