//! Constrain Crop changes the stored crop, as in Lightroom (issue #1927): with it on, the crop the
//! Crop tool shows (the stored one) never takes in the blank corners a Geometry warp leaves, so
//! ticking it, or moving a Geometry slider while it is ticked, visibly tightens the crop.

use lightcraft_geom::{CropGeometry, Rect};
use lightcraft_pipeline::geometry::Frame;
use serde_json::json;

use crate::Session;

fn crop(s: &Session) -> CropGeometry {
    s.develop_of(s.active().unwrap()).unwrap().crop.geometry
}

/// Does the stored crop already fit the warped photo (the render's constraint would leave it alone)?
fn fits(s: &Session) -> bool {
    let p = s.catalog.photo(s.active().unwrap()).unwrap();
    let mut d = (*p.develop).clone();
    d.geometry.constrain_crop = true;
    let f = Frame::with_lens(p.width as usize, p.height as usize, &d, true, p.embedded_lens.as_ref());
    let (a, b) = (f.crop.rect, p.develop.crop.geometry.rect);
    [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1].iter().all(|d| d.abs() < 1e-9)
}

fn smaller(a: Rect, b: Rect) -> bool {
    a.width() < b.width() - 1e-3 && a.height() < b.height() - 1e-3
}

fn constrain(s: &mut Session, on: bool) {
    s.execute("develop.merge", &json!({"settings": {"geometry": {"constrain_crop": on}}, "label": "Constrain Crop"})).unwrap();
}

fn vertical(s: &mut Session, v: f64) {
    s.execute("develop.set", &json!({"control": "geometry.vertical", "value": v})).unwrap();
}

#[test]
fn a_geometry_slider_tightens_the_crop_while_constrained() {
    let mut s = Session::with_demo();
    let full = crop(&s).rect;
    constrain(&mut s, true);
    assert_eq!(crop(&s).rect, full, "no warp yet: nothing to cut off");
    vertical(&mut s, -60.0);
    assert!(smaller(crop(&s).rect, full), "the stored crop shrank: {:?}", crop(&s).rect);
    assert!(fits(&s), "and holds no blank corner");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(crop(&s).rect, full, "undo puts the crop back with the slider");
}

#[test]
fn ticking_constrain_after_the_warp_tightens_the_crop() {
    let mut s = Session::with_demo();
    let full = crop(&s).rect;
    vertical(&mut s, -60.0);
    assert_eq!(crop(&s).rect, full, "unconstrained: the crop keeps the blank corners");
    assert!(!fits(&s));
    constrain(&mut s, true);
    assert!(smaller(crop(&s).rect, full) && fits(&s), "{:?}", crop(&s).rect);
    // unticking keeps the tighter crop, as in Lightroom
    let tight = crop(&s).rect;
    constrain(&mut s, false);
    assert_eq!(crop(&s).rect, tight);
}

#[test]
fn a_crop_drawn_by_hand_is_kept_as_drawn() {
    let mut s = Session::with_demo();
    constrain(&mut s, true);
    vertical(&mut s, -60.0);
    s.execute("crop.set", &json!({"rect": [0.0, 0.0, 1.0, 1.0]})).unwrap();
    assert_eq!(crop(&s).rect, Rect::new(0.0, 0.0, 1.0, 1.0), "the crop edit itself is not refitted");
}
