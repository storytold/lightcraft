//! A host segmenter (Apple Vision on iPad) answers Subject and Object masks with no model files.
//! Its own test binary: the registration is process-wide.
#![cfg(feature = "sam")]

use lightcraft_engine::Session;
use lightcraft_engine::segment::platform::{PlatformMask, set_platform_segmenter};
use serde_json::json;

/// The left half of the photo is the foreground, unless a click excludes it.
fn left_half(rgb: &[u8], w: usize, h: usize, _key: u64, clicks: &[[f32; 3]]) -> Result<Option<PlatformMask>, String> {
    assert_eq!(rgb.len(), w * h * 3);
    if clicks.iter().any(|c| c[2] < 0.5) {
        return Ok(None);
    }
    let side = 32;
    let logits = (0..side * side).map(|i| if i % side < side / 2 { 8.0 } else { -8.0 }).collect();
    Ok(Some(PlatformMask { side, logits }))
}

#[test]
fn subject_and_object_masks_come_from_the_host_without_a_model() {
    assert!(set_platform_segmenter(left_half));
    let mut s = Session::with_demo();
    assert!(s.segmenter.installed(), "a host segmenter counts as the model being there");
    assert!(s.segmenter.require_model().is_ok());
    let id = s.catalog.photos().next().map(|p| p.id.0).unwrap_or(1);
    s.execute("library.select", &json!({"ids": [id]})).unwrap();

    // Subject: the host's foreground replaces the built-in estimate
    s.execute("mask.add", &json!({"kind": "subject"})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    let shape = &d.masks[0].components[0].shape;
    assert!(matches!(shape, lightcraft_develop::MaskShape::Object { seg: Some(_), .. }), "{shape:?}");

    // Object: a tap selects through the host too
    s.execute("mask.add", &json!({"kind": "object"})).unwrap();
    s.execute("mask.objectPoint", &json!({"x": 0.2, "y": 0.5})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    let last = d.masks.last().unwrap();
    assert!(matches!(last.components[0].shape, lightcraft_develop::MaskShape::Object { seg: Some(_), .. }));
}
