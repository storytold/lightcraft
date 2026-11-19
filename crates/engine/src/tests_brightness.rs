//! Brightness (`light.brightness`) through the engine: the develop control, undo, the history group, Copy / Paste
//! (also from an unedited photo), the single-setting copy and Reset.

use std::sync::Arc;

use lcc_catalog::PhotoId;
use lcc_develop::DevelopSettings;
use serde_json::json;

use crate::Session;

fn dev(s: &Session, id: PhotoId) -> Arc<DevelopSettings> {
    s.develop_of(id).unwrap()
}

fn set(s: &mut Session, id: PhotoId, value: f64) {
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    s.execute("develop.set", &json!({"control": "light.brightness", "value": value})).unwrap();
}

fn two() -> (Session, PhotoId, PhotoId) {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
    (s, ids[0], ids[1])
}

#[test]
fn the_slider_sets_clamps_undoes_and_is_a_basic_history_step() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    assert_eq!(dev(&s, id).light.brightness, 0.0, "a photo that never had the slider starts at 0");
    s.execute("develop.set", &json!({"control": "light.brightness", "value": 35})).unwrap();
    assert_eq!(dev(&s, id).light.brightness, 35.0);
    s.execute("develop.set", &json!({"values": {"light.brightness": 500}})).unwrap();
    assert_eq!(dev(&s, id).light.brightness, 100.0, "kept in range");
    // one step of the Basic section's group
    let g = s.execute("history.groups", &json!({})).unwrap();
    let last = g["groups"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["section"], json!("basic"), "{last}");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev(&s, id).light.brightness, 35.0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev(&s, id).light.brightness, 0.0);
}

#[test]
fn pasting_from_an_unedited_photo_resets_brightness_and_pasting_from_an_edited_one_sets_it() {
    let (mut s, a, b) = two();
    set(&mut s, b, 25.0);
    // `a` is unedited: copying its Light settings and pasting them on `b` brings `b` back to 0
    s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["light"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(dev(&s, b).light.brightness, 0.0);
    // an edited photo carries it
    set(&mut s, a, 40.0);
    s.execute("develop.copy", &json!({"groups": ["light"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(dev(&s, b).light.brightness, 40.0);
    // Copy with everything ticked (the default) carries it too
    set(&mut s, a, -60.0);
    s.execute("develop.copy", &json!({"groups": null})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(dev(&s, b).light.brightness, -60.0);
}

#[test]
fn single_setting_copies_name_brightness_or_leave_it_alone() {
    let (mut s, a, b) = two();
    set(&mut s, b, 25.0);
    set(&mut s, a, 70.0);
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    // Exposure alone: Brightness stays where it was on the target
    s.execute("develop.copy", &json!({"settings": ["light.exposure"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!((dev(&s, b).light.exposure, dev(&s, b).light.brightness), (1.0, 25.0));
    // Brightness alone, also from a photo at 0
    s.execute("develop.copy", &json!({"settings": ["light.brightness"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(dev(&s, b).light.brightness, 70.0);
    set(&mut s, a, 0.0);
    s.execute("develop.copy", &json!({"settings": ["light.brightness"]})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(dev(&s, b).light.brightness, 0.0);
}

#[test]
fn reset_takes_brightness_back_to_zero() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.brightness", "value": 55})).unwrap();
    s.execute("develop.reset", &json!({})).unwrap();
    assert_eq!(dev(&s, id).light.brightness, 0.0);
}
