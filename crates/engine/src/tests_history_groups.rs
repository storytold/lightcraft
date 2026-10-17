//! Feature: two-level History. Consecutive steps on one section form a group; an edit made at a
//! past step drops only the later steps of its own group, and every later group is replayed on top.

use serde_json::{Value, json};

use crate::Session;

fn dev(s: &Session) -> std::sync::Arc<lcc_develop::DevelopSettings> {
    s.develop_of(s.active().unwrap()).unwrap()
}

fn set(s: &mut Session, control: &str, value: f64) {
    s.execute("develop.set", &json!({"control": control, "value": value})).unwrap();
}

fn groups(s: &mut Session) -> Value {
    s.execute("history.groups", &json!({})).unwrap()
}

/// The section and step count of each group after the first `skip` groups (the demo photo's own).
fn shape(s: &mut Session, skip: usize) -> Vec<(Option<String>, usize)> {
    let g = groups(s);
    g["groups"].as_array().unwrap()[skip..]
        .iter()
        .map(|g| (g["section"].as_str().map(str::to_string), g["steps"].as_array().unwrap().len()))
        .collect()
}

fn len(s: &Session) -> usize {
    s.catalog.photo(s.active().unwrap()).unwrap().history.len()
}

fn sec(x: &str) -> Option<String> {
    Some(x.to_string())
}

/// Demo session plus the number of groups (and steps) it starts with.
fn start() -> (Session, usize, usize) {
    let mut s = Session::with_demo();
    let n = groups(&mut s)["groups"].as_array().unwrap().len();
    let l = len(&s);
    (s, n, l)
}

#[test]
fn consecutive_steps_on_one_section_form_a_group() {
    let (mut s, g0, _) = start();
    set(&mut s, "light.exposure", 1.0);
    set(&mut s, "light.contrast", 10.0);
    set(&mut s, "color.vibrance", 20.0);
    set(&mut s, "light.exposure", 2.0);
    assert_eq!(shape(&mut s, g0), vec![(sec("basic"), 2), (sec("color"), 1), (sec("basic"), 1)]);
}

#[test]
fn editing_a_past_step_drops_only_later_steps_of_its_group_and_replays_later_groups() {
    let (mut s, g0, l0) = start();
    set(&mut s, "light.exposure", 1.0); // l0
    set(&mut s, "light.contrast", 10.0); // l0 + 1, dropped by the edit
    set(&mut s, "color.vibrance", 20.0); // later group, replayed
    s.execute("history.restore", &json!({"index": l0})).unwrap();
    assert_eq!(dev(&s).light.contrast, 0.0, "the controls show the state at the step");
    set(&mut s, "light.highlights", -30.0);
    // the photo stays at the edited point; latest has the replayed later group on top
    assert_eq!((dev(&s).light.highlights, dev(&s).color.vibrance), (-30.0, 0.0));
    assert_eq!(shape(&mut s, g0), vec![(sec("basic"), 2), (sec("color"), 1)]);
    s.execute("history.latest", &json!({})).unwrap();
    let d = dev(&s);
    assert_eq!((d.light.exposure, d.light.contrast, d.light.highlights, d.color.vibrance), (1.0, 0.0, -30.0, 20.0));
}

#[test]
fn a_later_group_setting_the_same_control_still_wins() {
    let (mut s, _, l0) = start();
    set(&mut s, "light.exposure", 1.0);
    set(&mut s, "color.vibrance", 20.0);
    set(&mut s, "light.exposure", 3.0);
    s.execute("history.editAt", &json!({"index": l0, "control": "light.exposure", "value": -2.0})).unwrap();
    assert_eq!(dev(&s).light.exposure, -2.0, "settings at the edited step");
    s.execute("history.latest", &json!({})).unwrap();
    assert_eq!((dev(&s).light.exposure, dev(&s).color.vibrance), (3.0, 20.0));
}

#[test]
fn an_edit_on_another_section_inserts_a_group_and_keeps_the_rest() {
    let (mut s, g0, l0) = start();
    set(&mut s, "light.exposure", 1.0);
    set(&mut s, "light.contrast", 10.0);
    s.execute("history.editAt", &json!({"index": l0, "control": "detail.sharpenAmount", "value": 60.0})).unwrap();
    assert_eq!(shape(&mut s, g0), vec![(sec("basic"), 1), (sec("detail"), 1), (sec("basic"), 1)]);
    s.execute("history.latest", &json!({})).unwrap();
    let d = dev(&s);
    assert_eq!((d.light.exposure, d.light.contrast, d.detail.sharpen_amount), (1.0, 10.0, 60.0));
}

#[test]
fn a_multi_section_preset_group_replays() {
    let (mut s, g0, l0) = start();
    set(&mut s, "light.exposure", 1.0);
    let mut d = (*dev(&s)).clone();
    d.light.whites = 25.0;
    d.color.saturation = -40.0;
    s.set_develop(s.active().unwrap(), d, "Preset: Test").unwrap();
    assert_eq!(shape(&mut s, g0), vec![(sec("basic"), 1), (None, 1)]);
    s.execute("history.editAt", &json!({"index": l0, "control": "light.exposure", "value": 0.5})).unwrap();
    s.execute("history.latest", &json!({})).unwrap();
    let d = dev(&s);
    assert_eq!((d.light.exposure, d.light.whites, d.color.saturation), (0.5, 25.0, -40.0));
}

#[test]
fn a_rewrite_is_one_undo_step() {
    let (mut s, _, l0) = start();
    set(&mut s, "light.exposure", 1.0);
    set(&mut s, "color.vibrance", 20.0);
    let before = s.catalog.photo(s.active().unwrap()).unwrap().clone();
    s.execute("history.editAt", &json!({"index": l0, "control": "light.exposure", "value": 2.0})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    let after = s.catalog.photo(s.active().unwrap()).unwrap();
    assert_eq!((&after.history, after.history_cursor, &after.develop), (&before.history, before.history_cursor, &before.develop));
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(dev(&s).light.exposure, 2.0);
}

#[test]
fn edits_at_the_latest_step_append_as_before() {
    let (mut s, _, l0) = start();
    set(&mut s, "light.exposure", 1.0);
    set(&mut s, "color.vibrance", 20.0);
    assert_eq!(len(&s), l0 + 2);
    assert_eq!(s.catalog.photo(s.active().unwrap()).unwrap().history_cursor, None);
}

#[test]
fn old_flat_history_loads_grouped() {
    let (mut s, _, _) = start();
    for v in [1.0, 2.0] {
        set(&mut s, "light.exposure", v);
    }
    set(&mut s, "color.vibrance", 5.0);
    // a photo as written before groups existed: plain label + settings steps, no group data
    let id = s.active().unwrap();
    let v = serde_json::to_value(s.catalog.photo(id).unwrap()).unwrap();
    let old: lcc_catalog::Photo = serde_json::from_value(v).unwrap();
    let g = crate::history_groups::groups(&old.history);
    let tail: Vec<_> = g.iter().rev().take(2).map(|g| (g.section.clone(), g.end - g.start + 1)).collect();
    assert_eq!(tail, vec![(sec("color"), 1), (sec("basic"), 2)]);
}
