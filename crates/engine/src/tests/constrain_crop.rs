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

/// Review of #1990: with Auto Sync, a Geometry edit on the active photo fits each selected photo's
/// own crop; the active photo's fitted crop is never copied over another photo's (smaller) crop.
#[test]
fn auto_sync_fits_each_photos_own_crop() {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let (a, b) = (ids[0], ids[1]);
    // B: a small crop off-centre; both constrained
    s.selection = crate::Selection { ids: vec![b], active: Some(b) };
    s.execute("crop.set", &json!({"rect": [0.05, 0.1, 0.45, 0.5]})).unwrap();
    constrain(&mut s, true);
    s.selection = crate::Selection { ids: vec![a], active: Some(a) };
    constrain(&mut s, true);
    let b_before = s.develop_of(b).unwrap().crop.geometry.rect;
    // A active, both selected, Auto Sync on: a Geometry edit on A
    s.selection = crate::Selection { ids: vec![a, b], active: Some(a) };
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    vertical(&mut s, -60.0);
    let (da, db) = (s.develop_of(a).unwrap(), s.develop_of(b).unwrap());
    assert_eq!(db.geometry.vertical, -60.0, "the edit reached B");
    let b_after = db.crop.geometry.rect;
    assert_ne!(b_after, da.crop.geometry.rect, "B keeps a crop of its own");
    let inside = |r: Rect, o: Rect| r.x0 >= o.x0 - 1e-9 && r.y0 >= o.y0 - 1e-9 && r.x1 <= o.x1 + 1e-9 && r.y1 <= o.y1 + 1e-9;
    assert!(inside(b_after, b_before), "B's crop only shrinks within itself: {b_before:?} -> {b_after:?}");
    s.selection = crate::Selection { ids: vec![b], active: Some(b) };
    assert!(fits(&s), "and fits B's warp");
}

/// Review of #1990 (CodeRabbit): the same with a slider drag (an interaction), the path the
/// window uses: the drag fits A's crop live, and its end must not carry that crop over to B.
#[test]
fn auto_sync_after_a_slider_drag_fits_each_photos_own_crop() {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let (a, b) = (ids[0], ids[1]);
    s.selection = crate::Selection { ids: vec![b], active: Some(b) };
    s.execute("crop.set", &json!({"rect": [0.05, 0.1, 0.45, 0.5]})).unwrap();
    constrain(&mut s, true);
    s.selection = crate::Selection { ids: vec![a], active: Some(a) };
    constrain(&mut s, true);
    let b_before = s.develop_of(b).unwrap().crop.geometry.rect;
    s.selection = crate::Selection { ids: vec![a, b], active: Some(a) };
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("develop.beginInteraction", &json!({"label": "Vertical"})).unwrap();
    for v in [-20.0, -40.0, -60.0] {
        vertical(&mut s, v);
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    let (da, db) = (s.develop_of(a).unwrap(), s.develop_of(b).unwrap());
    assert_eq!((da.geometry.vertical, db.geometry.vertical), (-60.0, -60.0));
    let b_after = db.crop.geometry.rect;
    assert_ne!(b_after, da.crop.geometry.rect, "B keeps a crop of its own");
    let inside = |r: Rect, o: Rect| r.x0 >= o.x0 - 1e-9 && r.y0 >= o.y0 - 1e-9 && r.x1 <= o.x1 + 1e-9 && r.y1 <= o.y1 + 1e-9;
    assert!(inside(b_after, b_before), "{b_before:?} -> {b_after:?}");
    for photo in [a, b] {
        s.selection = crate::Selection { ids: vec![photo], active: Some(photo) };
        assert!(fits(&s), "each crop fits its photo's warp");
    }
}

/// Review of #1990 (CodeRabbit): only the Geometry and Optics eyes change the warp; turning
/// another section's eye off leaves a crop drawn by hand as it is.
#[test]
fn an_unrelated_section_eye_leaves_a_hand_drawn_crop() {
    let mut s = Session::with_demo();
    constrain(&mut s, true);
    vertical(&mut s, -60.0);
    s.execute("crop.set", &json!({"rect": [0.0, 0.0, 1.0, 1.0]})).unwrap();
    s.execute("develop.sectionEnabled", &json!({"section": "light", "enabled": false})).unwrap();
    assert_eq!(crop(&s).rect, Rect::new(0.0, 0.0, 1.0, 1.0), "the Light eye is no warp change");
    s.execute("develop.sectionEnabled", &json!({"section": "geometry", "enabled": false})).unwrap();
    s.execute("develop.sectionEnabled", &json!({"section": "geometry", "enabled": true})).unwrap();
    assert!(fits(&s), "the Geometry eye is one: the crop fits again");
}
