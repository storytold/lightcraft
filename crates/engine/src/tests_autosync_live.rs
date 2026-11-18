//! LR-AUTOSYNC-LIVE: with Auto Sync on, a slider drag edits every selected photo live (each set
//! to the active photo's value, as the commit does), as one undo step; a cancel puts them all back.

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::Session;

fn two(s: &mut Session) -> (PhotoId, PhotoId) {
    let ids: Vec<PhotoId> = s.visible_cloned().iter().take(2).copied().collect();
    let (a, b) = (ids[0], ids[1]);
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    (a, b)
}

fn exp(s: &Session, id: PhotoId) -> f64 {
    s.develop_of(id).unwrap().light.exposure
}

#[test]
fn auto_sync_drag_edits_every_selected_photo_live() {
    let mut s = Session::with_demo();
    let (a, b) = two(&mut s);
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.25})).unwrap();
    assert!((exp(&s, b) - 0.25).abs() < 1e-9, "b follows during the drag: {}", exp(&s, b));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert!((exp(&s, b) - 0.5).abs() < 1e-9);
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert!((exp(&s, a) - 0.5).abs() < 1e-9 && (exp(&s, b) - 0.5).abs() < 1e-9);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(exp(&s, a).abs() < 1e-9 && exp(&s, b).abs() < 1e-9, "one undo step for both");

    // a cancelled drag puts every photo back
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    s.execute("develop.cancelInteraction", &json!({})).unwrap();
    assert!(exp(&s, a).abs() < 1e-9 && exp(&s, b).abs() < 1e-9, "cancel");
}
