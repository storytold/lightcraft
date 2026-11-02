//! Auto Sync and Sync Settings never carry an AI mask's computed shape to the other photos.
//!
//! Scenarios, in the words of a photographer editing a group of photos:
//!
//! * Given a Describe mask ("sky") on every selected photo, each with the sky its own photo has,
//!   when I change the mask's settings on the active photo with Auto Sync on, every selected
//!   photo gets the new settings and keeps its own sky.
//! * When I add a component to the mask (an Object picked by clicks, subtracted), the other
//!   photos get the component too, with its clicks and Subtract, but not the active photo's
//!   segmentation: their own is still to be computed.
//! * Sync Settings with Masking does the same.
//!
//! No model is needed: the segmentations are built in the test.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape, SegMask};
use lightcraft_geom::Point;
use serde_json::json;

use super::demo;
use crate::Session;

/// A segmentation that tells photos apart (`v` fills the grid).
fn seg(v: f32) -> SegMask {
    SegMask::from_logits(4, &[v; 16])
}

fn sky(seg: SegMask) -> MaskComponent {
    MaskComponent {
        name: None,
        op: MaskOp::Add,
        invert: false,
        shape: MaskShape::Prompt { text: "sky".into(), seg: Some(seg), detail: vec![], edge: 0.0 },
    }
}

/// Every photo gets mask 1, a Describe "sky" whose segmentation is its own.
fn two_photos_with_their_own_sky() -> (Session, PhotoId, PhotoId) {
    let mut s = demo();
    let ids: Vec<PhotoId> = s.catalog.photos().take(2).map(|p| p.id).collect();
    for (n, id) in ids.iter().enumerate() {
        let mut d = (*s.develop_of(*id).unwrap()).clone();
        d.masks = vec![Mask { id: 1, name: "Sky".into(), components: vec![sky(seg(n as f32 + 1.0))], ..Mask::default() }];
        s.set_develop(*id, d, "Mask").unwrap();
    }
    s.execute("library.select", &json!({"ids": [ids[0].0, ids[1].0]})).unwrap();
    s.selection.active = Some(ids[0]);
    (s, ids[0], ids[1])
}

/// The active photo's mask 1 darkened, with an Object picked by clicks subtracted from it.
fn edit_on_active(s: &Session, id: PhotoId) -> lightcraft_develop::DevelopSettings {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks[0].adjust.exposure = -0.8;
    d.masks[0].components.push(MaskComponent {
        name: None,
        op: MaskOp::Subtract,
        invert: false,
        shape: MaskShape::Object { hint: vec![Point::new(0.3, 0.4)], exclude: vec![], seg: Some(seg(9.0)), detail: vec![seg(9.0)], edge: 20.0 },
    });
    d
}

fn assert_kept_own_shape(s: &Session, other: PhotoId) {
    let m = &s.develop_of(other).unwrap().masks[0];
    assert_eq!(m.adjust.exposure, -0.8, "the mask's settings sync");
    assert_eq!(m.components.len(), 2, "the added component syncs");
    assert_eq!(m.components[0].shape, sky(seg(2.0)).shape, "the photo keeps its own sky");
    let c = &m.components[1];
    assert_eq!(c.op, MaskOp::Subtract, "add / subtract syncs");
    match &c.shape {
        MaskShape::Object { hint, seg, detail, edge, .. } => {
            assert_eq!(hint, &vec![Point::new(0.3, 0.4)], "the clicks sync");
            assert_eq!(*edge, 20.0, "the edge setting syncs");
            assert!(seg.is_none() && detail.is_empty(), "the active photo's segmentation never syncs: {seg:?} {}", detail.len());
        }
        other => panic!("not an Object: {other:?}"),
    }
}

#[test]
fn auto_sync_carries_mask_settings_but_never_the_ai_shape() {
    let (mut s, active, other) = two_photos_with_their_own_sky();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let d = edit_on_active(&s, active);
    s.set_develop(active, d.clone(), "Mask").unwrap();
    assert_eq!(*s.develop_of(active).unwrap(), d, "the active photo keeps its edit");
    assert_kept_own_shape(&s, other);
}

#[test]
fn sync_settings_with_masking_never_carries_the_ai_shape() {
    let (mut s, active, other) = two_photos_with_their_own_sky();
    let d = edit_on_active(&s, active);
    s.set_develop(active, d, "Mask").unwrap();
    s.execute("develop.sync", &json!({"groups": ["masks"]})).unwrap();
    assert_kept_own_shape(&s, other);
}

#[test]
fn paste_with_masking_never_carries_the_ai_shape() {
    let (mut s, active, other) = two_photos_with_their_own_sky();
    let d = edit_on_active(&s, active);
    s.set_develop(active, d, "Mask").unwrap();
    s.execute("develop.copy", &json!({"groups": ["masks"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [other.0]})).unwrap();
    assert_kept_own_shape(&s, other);
}
