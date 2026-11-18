//! Straightening never unlocks the crop's aspect ratio (test notes 2026-10-31 note 9). On an
//! untouched photo the crop tool shows the crop locked to the photo's shape while the crop itself
//! carries no lock yet; an edit of the angle (the Straighten slider, the Straighten tool's line,
//! Auto, rotating with the pointer, `develop.set`), a flip or Constrain Crop first stores that
//! lock, so the crop stays locked and keeps its ratio. Nothing but the lock button (and A) unlocks it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn crop_tool_on(session: lcc_engine::Session) -> Headless {
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "detail"}));
    ask(&mut h, "engine.execute", json!({"command": "panel.crop"}));
    h.settle(SETTLE);
    let id = h.app.session.active().expect("a photo");
    assert!(untouched(&h, id), "the photo starts untouched");
    assert!(h.app.crop_tool_aspect().is_some(), "and the tool shows it locked");
    h
}

fn crop_tool() -> Headless {
    crop_tool_on(lcc_engine::Session::with_demo())
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
    r
}

/// The photo's own width / height, as shown (after its orientation).
fn photo_ratio(h: &Headless) -> f64 {
    let p = h.app.session.catalog.photo(h.app.session.active().expect("a photo")).expect("the photo");
    let (w, ht) = (p.width as f64, p.height as f64);
    if p.develop.orientation.swaps_axes() { ht / w } else { w / ht }
}

/// (stored lock as width / height, the crop rect's width / height, the angle)
fn crop_state(h: &Headless) -> (Option<f64>, f64, f64) {
    let p = h.app.session.catalog.photo(h.app.session.active().expect("a photo")).expect("the photo");
    let (w, ht) = (p.width as f64, p.height as f64);
    let (w, ht) = if p.develop.orientation.swaps_axes() { (ht, w) } else { (w, ht) };
    let c = &p.develop.crop;
    let r = c.geometry.rect_px(w, ht);
    (c.aspect.map(|(a, b)| f64::from(a) / f64::from(b)), r.width() / r.height(), c.geometry.angle)
}

/// After `what`, the crop is still locked to `want` (width / height) and the rect keeps that ratio.
fn assert_locked(h: &Headless, want: f64, what: &str) {
    let (lock, rect, angle) = crop_state(h);
    assert!(angle.abs() > 0.05, "{what}: the photo was straightened (angle {angle})");
    let lock = lock.unwrap_or_else(|| panic!("{what}: the lock is gone (the crop reads as unlocked)"));
    assert!((lock - want).abs() < 0.01, "{what}: locked to {lock}, want {want}");
    assert!((rect - want).abs() < 0.01, "{what}: the rect is {rect}, want {want}");
    let shown = h.app.crop_tool_aspect().map(|(a, b)| f64::from(a) / f64::from(b));
    assert!(shown.is_some_and(|s| (s - want).abs() < 0.01), "{what}: the tool shows {shown:?}");
}

/// Undo until the photo is untouched again (an edit is one undo step, or two with a stored lock).
fn back_to_untouched(h: &mut Headless, what: &str) {
    let id = h.app.session.active().expect("a photo");
    for _ in 0..3 {
        if untouched(h, id) {
            break;
        }
        h.app.run("edit.undo", json!({})).expect("undo");
    }
    h.settle(SETTLE);
    assert!(untouched(h, id), "{what}: Undo takes the angle and the stored lock back together");
    assert!(h.app.crop_tool_aspect().is_some(), "{what}: undone, the tool shows the photo locked again");
}

/// However the angle is set on an untouched photo (a command, the Straighten slider, rotating with the pointer,
/// the Straighten tool's line), the crop stays locked to the photo's shape; one session, undone between the ways.
#[test]
fn straightening_keeps_the_lock_however_the_angle_is_set() {
    let mut h = crop_tool();
    let want = photo_ratio(&h);
    for (what, command, params) in [
        ("crop.straighten", "crop.straighten", json!({"angle": 3.0})),
        ("develop.set crop.angle", "develop.set", json!({"control": "crop.angle", "value": -4.0})),
        ("develop.adjust crop.angle", "develop.adjust", json!({"control": "crop.angle", "delta": 2.0})),
        ("crop.set angle", "crop.set", json!({"angle": 5.0})),
    ] {
        let undo = h.app.session.undo.len();
        ask(&mut h, "engine.execute", json!({"command": command, "params": params}));
        assert_locked(&h, want, what);
        assert_eq!(h.app.session.undo.len(), undo + 1, "{what}: one undo step");
        // and one Undo puts the photo back as it was: untouched, shown locked
        h.app.run("edit.undo", json!({})).expect("undo");
        let id = h.app.session.active().expect("a photo");
        assert!(untouched(&h, id), "{what}: Undo takes the angle and the stored lock back together");
    }
    h.settle(SETTLE);

    let undo = h.app.session.undo.len();
    ask(&mut h, "ui.dragWidget", json!({"id": "slider:crop.angle", "dx": 40.0, "dy": 0.0, "steps": 6}));
    assert_locked(&h, want, "the Straighten slider");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step for the drag, lock included");
    back_to_untouched(&mut h, "the Straighten slider");

    ask(
        &mut h,
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}, {"kind": "up", "x": -0.05, "y": 0.3}]}),
    );
    assert_locked(&h, want, "rotating with the pointer");
    back_to_untouched(&mut h, "rotating with the pointer");

    h.app.ui.tool = "straighten".into();
    h.step();
    let img = h.app.image_rect.expect("the photo on screen");
    let (a, b) = (egui::pos2(img.left() + img.width() * 0.2, img.center().y), egui::pos2(img.left() + img.width() * 0.8, img.center().y + 30.0));
    ask(&mut h, "ui.drag", json!({"x": a.x, "y": a.y, "toX": b.x, "toY": b.y, "steps": 10}));
    assert_locked(&h, want, "the Straighten tool's line");
}

/// A photo with a horizon tilted by 6° and bands below it parallel to it (several agreeing lines,
/// as Auto needs): bright sky over a striped ground.
fn tilted_horizon() -> (lcc_testkit::TempDir, lcc_engine::Session) {
    let dir = lcc_testkit::TempDir::new("lc-ui-crop-lock-auto");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, ht) = (900usize, 600usize);
    let tilt = 6f64.to_radians().tan();
    let mut data = Vec::with_capacity(w * ht);
    for y in 0..ht {
        for x in 0..w {
            let horizon = ht as f64 / 2.0 + (x as f64 - w as f64 / 2.0) * tilt;
            let band = (((y as f64 - horizon) / 30.0) as i64) % 2 == 0;
            data.push(if (y as f64) < horizon {
                [200, 215, 235, 255]
            } else if band {
                [20, 35, 60, 255]
            } else {
                [40, 70, 95, 255]
            });
        }
    }
    let img = lcc_raster::Rgba8 { width: w, height: ht, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("horizon.png");
    std::fs::write(&path, bytes).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    (dir, session)
}

#[test]
fn c_auto_straighten_keeps_the_lock() {
    let (dir, session) = tilted_horizon();
    let mut h = crop_tool_on(session);
    let want = photo_ratio(&h);
    let r = ask(&mut h, "engine.execute", json!({"command": "crop.autoStraighten"}));
    assert_eq!(r["result"]["changed"], true, "Auto found the horizon: {r}");
    assert_locked(&h, want, "Auto");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_chosen_4x5_survives_straightening() {
    let mut h = crop_tool();
    ask(&mut h, "engine.execute", json!({"command": "crop.aspect", "params": {"aspect": "4x5"}}));
    let (lock, _, _) = crop_state(&h);
    let want = lock.expect("4x5 locked");
    assert!((want - 0.8).abs() < 1e-6 || (want - 1.25).abs() < 1e-6, "4x5 in either orientation: {want}");
    ask(&mut h, "ui.dragWidget", json!({"id": "slider:crop.angle", "dx": 40.0, "dy": 0.0, "steps": 6}));
    assert_locked(&h, want, "4x5 then the slider");
    ask(&mut h, "engine.execute", json!({"command": "crop.straighten", "params": {"angle": -6.0}}));
    assert_locked(&h, want, "4x5 then crop.straighten");
}

#[test]
fn flips_and_constrain_crop_keep_the_lock() {
    let mut h = crop_tool();
    for (what, command, params) in [
        ("flip horizontal", "photo.flipHorizontal", json!({})),
        ("flip vertical", "photo.flipVertical", json!({})),
        ("Constrain Crop", "develop.merge", json!({"settings": {"geometry": {"constrain_crop": true}}, "label": "Constrain Crop"})),
    ] {
        let want = photo_ratio(&h);
        let undo = h.app.session.undo.len();
        ask(&mut h, "engine.execute", json!({"command": command, "params": params}));
        let id = h.app.session.active().expect("a photo");
        let shown = h.app.crop_tool_aspect().map(|(a, b)| f64::from(a) / f64::from(b));
        assert!(shown.is_some_and(|s| (s - want).abs() < 0.01), "{what}: still shown locked to {want}: {shown:?}");
        if !untouched(&h, id) {
            assert!(crop_state(&h).0.is_some(), "{what}: the crop carries the lock it shows");
        }
        // each from the untouched photo
        while h.app.session.undo.len() > undo {
            h.app.run("edit.undo", json!({})).expect("undo");
        }
        h.settle(SETTLE);
        assert!(untouched(&h, id), "{what}: undone");
    }
}

#[test]
fn only_the_lock_unlocks_it() {
    let mut h = crop_tool();
    ask(&mut h, "engine.execute", json!({"command": "crop.straighten", "params": {"angle": 3.0}}));
    assert!(h.app.crop_tool_aspect().is_some());
    ask(&mut h, "engine.execute", json!({"command": "crop.lockAspect"}));
    assert_eq!(h.app.crop_tool_aspect(), None, "the lock toggle unlocks it");
    // and a free crop stays free when straightened again
    ask(&mut h, "engine.execute", json!({"command": "crop.straighten", "params": {"angle": -2.0}}));
    assert_eq!(h.app.crop_tool_aspect(), None, "a free crop isn't locked by straightening");
}

/// The remembered choice "free" (the lock toggled off on an earlier photo) is respected: an
/// untouched photo straightened stays free.
#[test]
fn a_remembered_free_crop_stays_free() {
    let mut h = crop_tool();
    h.app.ui.crop_default_aspect = Some(json!("free"));
    ask(&mut h, "engine.execute", json!({"command": "crop.straighten", "params": {"angle": 3.0}}));
    assert_eq!(crop_state(&h).0, None);
    assert_eq!(h.app.crop_tool_aspect(), None);
}

fn untouched(h: &Headless, id: lcc_catalog::PhotoId) -> bool {
    h.app.session.develop_of(id).is_some_and(|d| d.crop == lcc_develop::Crop::default())
}

#[test]
fn x_swaps_keep_the_lock_shown_on_an_untouched_photo() {
    let mut h = crop_tool();
    let want = photo_ratio(&h);
    for i in 0..4 {
        ask(&mut h, "engine.execute", json!({"command": "crop.rotateAspect"}));
        let (lock, rect, _) = crop_state(&h);
        let shape = if i % 2 == 0 { 1.0 / want } else { want };
        let lock = lock.expect("still locked after the swap");
        assert!((lock - shape).abs() < 0.02 && (rect - shape).abs() < 0.02, "swap {i}: lock {lock}, rect {rect}, want {shape}");
    }
}
