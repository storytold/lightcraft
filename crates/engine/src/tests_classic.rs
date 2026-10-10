//! Library's Classic panels (P1.4), engine side: Quick Develop on many photos, folder operations
//! on disk, collection export/import and keyword attributes.

use serde_json::json;

use crate::Session;

fn demo_with(n: usize) -> (Session, Vec<u64>) {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(n).map(|p| p.0).collect();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    (s, ids)
}

/// Quick Develop's choices reach every selected photo in one undo step; the selection stays.
#[test]
fn quick_set_applies_to_every_selected_photo_in_one_step() {
    let (mut s, ids) = demo_with(3);
    let active = s.selection.active;
    let undo = s.undo.len();
    let r = s.execute("develop.quickSet", &json!({"treatment": "bw"})).unwrap();
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.selection.active, active, "the selection is put back");
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().treatment, dac_develop::Treatment::Bw);
    }
    s.undo_step().unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().treatment, dac_develop::Treatment::Color);
    }
    // crop ratio and white balance
    s.execute("develop.quickSet", &json!({"aspect": "1x1"})).unwrap();
    for id in &ids {
        let (w, h) = s.develop_of(dac_catalog::PhotoId(*id)).unwrap().crop.aspect.unwrap();
        assert_eq!(w, h);
    }
    s.execute("develop.quickSet", &json!({"wb": "tungsten"})).unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().wb.mode, dac_develop::WbMode::Tungsten);
    }
    // bad input is an error, not a panic
    assert!(s.execute("develop.quickSet", &json!({"treatment": "sepia"})).is_err());
    assert!(s.execute("develop.quickSet", &json!({"wb": "custom"})).is_err());
    assert!(s.execute("develop.quickSet", &json!({})).is_err());
    assert!(s.execute("develop.quickSet", &json!({"aspect": "axb"})).unwrap()["errors"].as_array().is_some_and(|e| !e.is_empty()));
}
