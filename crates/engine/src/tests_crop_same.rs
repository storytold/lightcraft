//! Crop to Same Aspect Ratio, Crop As Shot and Process Version (issue #989).

use lcc_catalog::PhotoId;
use lcc_develop::ProcessVersion;
use serde_json::json;

use crate::{EngineError, Session};

fn three() -> (Session, Vec<PhotoId>) {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    s.selection = crate::Selection { ids: ids.clone(), active: ids.first().copied() };
    (s, ids)
}

fn crop_ratio(s: &Session, id: PhotoId) -> f64 {
    let p = s.catalog.photo(id).unwrap();
    let (w, h) = (p.width as f64, p.height as f64);
    let (w, h) = if p.develop.orientation.swaps_axes() { (h, w) } else { (w, h) };
    let r = p.develop.crop.geometry.rect_px(w, h);
    r.width() / r.height()
}

#[test]
fn same_aspect_as_shot_and_set_process_work_on_the_selection_in_one_undo_step() {
    let (mut s, ids) = three();
    let active = s.active().unwrap();
    s.execute("crop.aspect", &json!({"aspect": [1, 1]})).unwrap();
    let before: Vec<_> = ids.iter().map(|i| s.develop_of(*i).unwrap()).collect();
    let r = s.execute("crop.sameAspect", &json!({})).unwrap();
    assert_eq!(r["changed"], 2);
    for id in &ids {
        let q = crop_ratio(&s, *id);
        assert!(q.is_finite() && (q - 1.0).abs() < 0.01, "{q}");
    }
    s.execute("edit.undo", &json!({})).unwrap();
    for (id, b) in ids.iter().zip(&before) {
        if *id != active {
            assert_eq!(s.develop_of(*id).unwrap().crop, b.crop);
        }
    }

    // Crop As Shot restores the full decoded frame
    s.execute("crop.aspect", &json!({"aspect": [16, 9]})).unwrap();
    s.execute("crop.asShot", &json!({})).unwrap();
    let c = s.develop_of(active).unwrap().crop;
    assert_eq!(c.geometry, Default::default());
    assert!(c.aspect.is_none());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.develop_of(active).unwrap().crop.aspect.is_some());

    // Process Version moves the selection and refuses unknown versions
    let r = s.execute("develop.setProcess", &json!({"version": 3})).unwrap();
    assert_eq!(r["changed"], 3);
    for id in &ids {
        assert_eq!(s.develop_of(*id).unwrap().process, ProcessVersion(3));
    }
    s.execute("edit.undo", &json!({})).unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(*id).unwrap().process, ProcessVersion::LATEST);
    }
    for bad in [
        json!({}),
        json!({"version": 0}),
        json!({"version": 99}),
        json!({"version": -1}),
        json!({"version": 2.5}),
        json!({"version": "x"}),
        json!({"version": null}),
    ] {
        assert!(matches!(s.execute("develop.setProcess", &bad), Err(EngineError::BadParams { .. })), "{bad}");
    }

    // Same Aspect needs a real crop: one of no size hands on nothing (and stores no NaN)
    let mut d = (*s.develop_of(active).unwrap()).clone();
    d.crop.geometry.rect = lcc_geom::Rect::new(0.5, 0.5, 0.5, 0.5);
    s.set_develop(active, d, "t").unwrap();
    assert!(s.execute("crop.sameAspect", &json!({})).is_err());
    for id in ids.iter().skip(1) {
        assert!(crop_ratio(&s, *id).is_finite());
    }
    // …and another photo
    s.selection = crate::Selection { ids: vec![active], active: Some(active) };
    assert!(matches!(s.execute("crop.sameAspect", &json!({})), Err(EngineError::Disabled(..))));
}
