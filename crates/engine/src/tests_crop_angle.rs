//! Every way of setting the crop angle keeps the crop inside the photo (issue #742): the generic
//! control path (`develop.set` on `crop.angle`, as MCP, the control channel and `lightcraft-cli
//! render --set` use it, plus `develop.adjust`, `develop.quickAdjust`, `develop.resetControl`) gives
//! the same crop as `crop.straighten` (the rotate handle, the Straighten slider, the Angle field)
//! and `crop.set {angle}`.

use lightcraft_geom::{CropGeometry, Rect};
use serde_json::{Value, json};

use crate::Session;

fn geometry(s: &Session) -> CropGeometry {
    s.develop_of(s.active().unwrap()).unwrap().crop.geometry
}

fn dims(s: &Session) -> (f64, f64) {
    let p = s.catalog.photo(s.active().unwrap()).unwrap();
    let (w, h) = (p.width as f64, p.height as f64);
    if p.develop.orientation.swaps_axes() { (h, w) } else { (w, h) }
}

/// Run `cmd` from the current state, return the crop it leaves, and undo it.
fn try_cmd(s: &mut Session, cmd: &str, p: Value) -> CropGeometry {
    s.execute(cmd, &p).unwrap();
    let g = geometry(s);
    s.execute("edit.undo", &json!({})).unwrap();
    g
}

fn close(a: CropGeometry, b: CropGeometry) -> bool {
    let r = |g: CropGeometry| [g.rect.x0, g.rect.y0, g.rect.x1, g.rect.y1, g.angle];
    r(a).iter().zip(r(b)).all(|(x, y)| (x - y).abs() < 1e-9)
}

#[test]
fn every_way_of_setting_the_angle_gives_the_straighten_crop() {
    let mut s = Session::with_demo();
    let (w, h) = dims(&s);
    for aspect in [json!("original"), json!("free"), json!("1x1"), json!("16x9"), json!("4x5")] {
        s.execute("crop.aspect", &json!({"aspect": aspect})).unwrap();
        let start = geometry(&s);
        for angle in [-45.0, -30.0, -7.5, 0.5, 2.0, 15.0, 33.3] {
            let want = try_cmd(&mut s, "crop.straighten", json!({"angle": angle}));
            assert!(want.is_within_image(w, h), "{aspect} {angle}°: straighten fits");
            assert_eq!(want.angle, angle);
            for (cmd, p) in [
                ("develop.set", json!({"control": "crop.angle", "value": angle})),
                ("develop.set", json!({"values": {"crop.angle": angle, "light.exposure": 0.3}})),
                ("crop.set", json!({"angle": angle})),
                ("develop.adjust", json!({"control": "crop.angle", "delta": angle - start.angle})),
            ] {
                let got = try_cmd(&mut s, cmd, p.clone());
                assert!(close(got, want), "{aspect} {angle}°: {cmd} {p} gave {got:?}, crop.straighten {want:?}");
            }
            assert!(close(geometry(&s), start), "undo restored the crop");
        }
    }
}

#[test]
fn a_crop_that_still_fits_is_kept() {
    let mut s = Session::with_demo();
    s.execute("crop.set", &json!({"rect": [0.3, 0.3, 0.7, 0.7]})).unwrap();
    for angle in [-4.0, 1.0, 3.0, 6.0] {
        s.execute("develop.set", &json!({"control": "crop.angle", "value": angle})).unwrap();
        let g = geometry(&s);
        assert_eq!(g.rect, Rect::new(0.3, 0.3, 0.7, 0.7), "no needless shrink at {angle}°");
        assert_eq!(g.angle, angle);
    }
}

#[test]
fn quick_develop_reset_and_batch_paths_fit_too() {
    let mut s = Session::with_demo();
    let (w, h) = dims(&s);
    let id = s.active().unwrap();
    let want = try_cmd(&mut s, "crop.straighten", json!({"angle": 12.0}));

    let got = try_cmd(&mut s, "develop.quickAdjust", json!({"control": "crop.angle", "delta": 12.0, "ids": [id.0]}));
    assert!(close(got, want), "quickAdjust {got:?} vs {want:?}");
    let got = try_cmd(&mut s, "develop.set", json!({"control": "crop.angle", "value": 12.0, "ids": [id.0]}));
    assert!(close(got, want), "develop.set ids {got:?} vs {want:?}");

    // back to 0°: the crop that fitted at 12° fits at 0° too and is kept
    s.execute("develop.set", &json!({"control": "crop.angle", "value": 12.0})).unwrap();
    s.execute("develop.resetControl", &json!({"control": "crop.angle"})).unwrap();
    let g = geometry(&s);
    assert_eq!((g.rect, g.angle), (want.rect, 0.0));
    assert!(g.is_within_image(w, h));
}

#[test]
fn a_straighten_slider_drag_fits_live_and_is_one_undo_step() {
    let mut s = Session::with_demo();
    let (w, h) = dims(&s);
    let start = geometry(&s);
    let undo = s.undo.len();
    s.execute("develop.beginInteraction", &json!({"label": "Straighten"})).unwrap();
    for angle in [1.0, 4.0, 9.0, 15.0] {
        s.execute("develop.set", &json!({"control": "crop.angle", "value": angle})).unwrap();
        assert!(geometry(&s).is_within_image(w, h), "fitted at every step ({angle}°)");
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert_eq!(s.undo.len(), undo + 1, "the drag is one undo step");
    assert_eq!(geometry(&s).angle, 15.0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(close(geometry(&s), start));
}
