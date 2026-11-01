//! Masks stay on the photo's content when its geometry changes: Rotate
//! Left / Right turns them with the photo, an old mask (on the corrected frame) goes onto the
//! photo at its next edit without moving, and the AI models see the photo the masks live on
//! (no lens corrections, Upright, Transform, crop or flips), so changing those leaves the AI
//! selections as they are.

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use serde_json::json;

use crate::Session;

fn active_dev(s: &Session) -> DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

fn radial_at(p: Point) -> Mask {
    Mask {
        id: 1,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Radial { center: p, rx: 0.1, ry: 0.05, angle: 20.0, feather: 50.0, invert: false },
        }],
        ..Default::default()
    }
}

fn centre(d: &DevelopSettings) -> (Point, f64) {
    match &d.masks[0].components[0].shape {
        MaskShape::Radial { center, angle, .. } => (*center, *angle),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rotating_the_photo_turns_its_masks_with_it() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let mut d = active_dev(&s);
    let p = Point::new(0.2, 0.3);
    d.masks = vec![radial_at(p)];
    s.set_develop(id, d, "Mask").unwrap();
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let (c, a) = centre(&active_dev(&s));
    // a quarter turn clockwise: (x, y) → (1 − y, x), and the ellipse turns along
    assert!((c.x - 0.7).abs() < 1e-9 && (c.y - 0.2).abs() < 1e-9, "{c:?}");
    assert_eq!(a, 110.0);
    s.execute("photo.rotateLeft", &json!({})).unwrap();
    let (c, a) = centre(&active_dev(&s));
    assert!((c.x - p.x).abs() < 1e-9 && (c.y - p.y).abs() < 1e-9 && a == 20.0, "{c:?} {a}");
}

#[test]
fn an_old_mask_goes_onto_the_photo_at_its_next_edit_without_moving() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let mut d = active_dev(&s);
    d.geometry.vertical = 30.0;
    d.geometry.rotate = 5.0;
    let p = Point::new(0.3, 0.4);
    let mut m = radial_at(p);
    m.on_photo = false;
    d.masks = vec![m];
    // as a catalog from before keeps it
    s.catalog.apply(lightcraft_catalog::Op::SetDevelop { id, settings: std::sync::Arc::new(d.clone()), label: "Old".into(), edited: None }).unwrap();
    assert!(!active_dev(&s).masks[0].on_photo);
    d.light.exposure = 0.5;
    s.set_develop(id, d.clone(), "Exposure").unwrap();
    let now = active_dev(&s);
    assert!(now.masks[0].on_photo, "attached at the edit");
    let photo = s.catalog.photo(id).unwrap();
    let frame = lightcraft_pipeline::geometry::Frame::with_lens(photo.width as usize, photo.height as usize, &d, false, None);
    let (c, _) = centre(&now);
    let back = frame.for_mask(true).mask_to_norm(c);
    assert!((back.x - p.x).abs() < 1e-6 && (back.y - p.y).abs() < 1e-6, "renders where it did: {back:?}");
    assert!((c.x - p.x).hypot(c.y - p.y) > 0.005, "its place on the photo differs from the corrected frame's: {c:?}");
    // a later Transform change leaves it on the photo where it is
    let mut d = now.clone();
    d.geometry.vertical = -10.0;
    s.set_develop(id, d, "Transform").unwrap();
    assert_eq!(centre(&active_dev(&s)).0, c);
}

#[test]
fn the_ai_models_see_the_photo_the_masks_live_on() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let key = s.segment_key(id).unwrap().0;
    let mut d = active_dev(&s);
    d.geometry.vertical = 20.0;
    d.optics.distortion = 30.0;
    d.crop.geometry.angle = 5.0;
    d.crop.flip_h = true;
    s.set_develop(id, d.clone(), "Geometry").unwrap();
    assert_eq!(s.segment_key(id).unwrap().0, key, "geometry doesn't change what the models see: the AI masks stay valid");
    d.light.exposure = 1.0;
    s.set_develop(id, d, "Exposure").unwrap();
    assert_ne!(s.segment_key(id).unwrap().0, key, "the look still does");
}

/// A mask saved before `Mask::on_photo` reads as an old mask (on the corrected frame), a new mask
/// is on the photo, and an old one is written as it was read.
#[test]
fn a_mask_saved_before_reads_as_an_old_mask() {
    let old: Mask = serde_json::from_value(json!({"id": 3, "name": "Mask 3", "components": []})).unwrap();
    assert!(!old.on_photo);
    assert!(Mask::default().on_photo);
    let written = serde_json::to_value(&old).unwrap();
    assert!(written.get("on_photo").is_none(), "{written}");
    assert_eq!(serde_json::to_value(Mask::default()).unwrap()["on_photo"], json!(true));
}

/// Old masks an edit brings in (paste, sync, a preset, a history step, a sidecar) go onto the
/// photo from the settings they came with, where they render now, not from the photo's settings
/// before the edit (review of #3550).
#[test]
fn old_masks_an_edit_brings_in_go_onto_the_photo_from_the_settings_they_came_with() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    // the photo now: no Transform
    let before = active_dev(&s);
    // what comes in: a Transform and an old mask drawn under it
    let p = Point::new(0.3, 0.4);
    let mut incoming = before.clone();
    incoming.geometry.vertical = 30.0;
    incoming.geometry.rotate = 5.0;
    let mut m = radial_at(p);
    m.on_photo = false;
    incoming.masks = vec![m];
    s.set_develop(id, incoming.clone(), "Paste Settings").unwrap();
    let now = active_dev(&s);
    assert!(now.masks[0].on_photo);
    let photo = s.catalog.photo(id).unwrap();
    let frame = lightcraft_pipeline::geometry::Frame::with_lens(photo.width as usize, photo.height as usize, &incoming, false, None);
    let back = frame.for_mask(true).mask_to_norm(centre(&now).0);
    assert!((back.x - p.x).abs() < 1e-6 && (back.y - p.y).abs() < 1e-6, "renders where it did under its own settings: {back:?}");
}

/// A click on an old Object mask goes onto the photo with the mask before the model reads it: the
/// stored clicks and the new one are then in the same coordinates (review of #3550).
#[test]
fn a_click_on_an_old_object_mask_goes_onto_the_photo_with_it() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let mut d = active_dev(&s);
    d.geometry.vertical = 30.0;
    d.geometry.rotate = 5.0;
    let p = Point::new(0.3, 0.4);
    d.masks = vec![Mask {
        id: 1,
        on_photo: false,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object { hint: vec![p], exclude: Vec::new(), seg: None, detail: Vec::new(), edge: 0.0 },
        }],
        ..Default::default()
    }];
    s.catalog.apply(lightcraft_catalog::Op::SetDevelop { id, settings: std::sync::Arc::new(d.clone()), label: "Old".into(), edited: None }).unwrap();
    s.active_mask = Some(1);
    // (no model: the command fails at the model, after the mask went onto the photo)
    s.segmenter.dir = Some(std::env::temp_dir().join("lightcraft-no-model-mask-geometry"));
    let _ = s.execute("mask.objectPoint", &json!({"x": 0.6, "y": 0.5}));
    let now = active_dev(&s);
    assert!(now.masks[0].on_photo);
    let photo = s.catalog.photo(id).unwrap();
    let frame = lightcraft_pipeline::geometry::Frame::with_lens(photo.width as usize, photo.height as usize, &d, false, None);
    let MaskShape::Object { hint, .. } = &now.masks[0].components[0].shape else { panic!() };
    let want = frame.for_mask(true).norm_to_mask(p);
    assert!((hint[0].x - want.x).abs() < 1e-9 && (hint[0].y - want.y).abs() < 1e-9, "{hint:?} vs {want:?}");
}
