//! Levels commands: setting, reset, history, the pickers and the histogram (demo scene only).

use serde_json::json;

use crate::Session;

fn dev(s: &Session) -> lcc_develop::DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

#[test]
fn set_levels_one_channel_in_one_history_step() {
    let mut s = Session::with_demo();
    let r = s.execute("develop.levels", &json!({"black": 20, "white": 230, "midtone": 0.4, "outBlack": 5})).unwrap();
    assert_eq!(r["channel"], "master");
    let l = dev(&s).levels;
    assert_eq!((l.master.black, l.master.white, l.master.midtone, l.master.out_black), (20.0, 230.0, 0.4, 5.0));
    assert!(l.red.black == 0.0 && l.green.white == 255.0);
    // one undo undoes all five values
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dev(&s).levels.is_default());
}

#[test]
fn channels_black_and_white_keep_their_distance_and_may_invert() {
    let mut s = Session::with_demo();
    s.execute("develop.levels", &json!({"channel": "red", "black": 100, "white": 100})).unwrap();
    let c = dev(&s).levels.red;
    assert!((c.white - c.black).abs() >= 2.0, "{c:?}");
    s.execute("develop.levels", &json!({"channel": "blue", "black": 215, "white": 40})).unwrap();
    let c = dev(&s).levels.blue;
    assert_eq!((c.black, c.white), (215.0, 40.0));
}

#[test]
fn midtone_is_limited_and_the_marker_sets_it() {
    let mut s = Session::with_demo();
    s.execute("develop.levels", &json!({"midtone": 5})).unwrap();
    assert_eq!(dev(&s).levels.master.midtone, 1.0);
    let r = s.execute("develop.levels", &json!({"marker": 128})).unwrap();
    assert_eq!(dev(&s).levels.master.midtone, 0.0, "the centre sets exactly 0: {r}");
    s.execute("develop.levels", &json!({"marker": 64})).unwrap();
    assert!(dev(&s).levels.master.midtone > 0.0);
    s.execute("develop.levels", &json!({"marker": 190})).unwrap();
    assert!(dev(&s).levels.master.midtone < 0.0);
}

#[test]
fn bad_input_is_refused() {
    let mut s = Session::with_demo();
    assert!(s.execute("develop.levels", &json!({})).is_err());
    assert!(s.execute("develop.levels", &json!({"channel": "alpha", "black": 3})).is_err());
    assert!(s.execute("develop.levels", &json!({"black": "x"})).is_err());
    assert!(s.execute("levels.reset", &json!({"channel": "alpha"})).is_err());
    assert!(s.execute("develop.levelsPick", &json!({"target": "mid", "x": 0.5, "y": 0.5})).is_err());
    assert!(s.execute("develop.levelsPick", &json!({"x": 0.5, "y": 0.5})).is_err());
}

#[test]
fn reset_one_channel_or_all() {
    let mut s = Session::with_demo();
    s.execute("develop.levels", &json!({"channel": "red", "black": 30})).unwrap();
    s.execute("develop.levels", &json!({"channel": "green", "white": 200})).unwrap();
    s.execute("levels.reset", &json!({"channel": "red"})).unwrap();
    let l = dev(&s).levels;
    assert!(l.red.black == 0.0 && l.green.white == 200.0);
    s.execute("levels.reset", &json!({})).unwrap();
    assert!(dev(&s).levels.is_default());
}

#[test]
fn controls_and_commands_agree() {
    let mut s = Session::with_demo();
    s.execute("develop.set", &json!({"control": "levels.green.midtone", "value": -0.6})).unwrap();
    s.execute("develop.set", &json!({"control": "levels.master.black", "value": 254})).unwrap();
    let l = dev(&s).levels;
    assert_eq!(l.green.midtone, -0.6);
    assert!((l.master.white - l.master.black).abs() >= 2.0, "gap kept through develop.set: {l:?}");
}

#[test]
fn picker_sets_black_and_white_on_the_master() {
    let mut s = Session::with_demo();
    let dark = s.execute("develop.levelsPick", &json!({"target": "black", "x": 0.05, "y": 0.95})).unwrap();
    let bright = s.execute("develop.levelsPick", &json!({"target": "white", "x": 0.5, "y": 0.05})).unwrap();
    let (b, w) = (dark["value"].as_f64().unwrap(), bright["value"].as_f64().unwrap());
    assert!((0.0..=255.0).contains(&b) && (0.0..=255.0).contains(&w));
    let l = dev(&s).levels;
    assert_eq!(l.master.black, b);
    assert!((l.master.white - l.master.black).abs() >= 2.0);
    assert!(l.red == lcc_develop::ChannelLevels::default());
}

#[test]
fn picker_reads_the_photo_before_the_levels() {
    let mut s = Session::with_demo();
    let first = s.execute("develop.levelsPick", &json!({"target": "white", "x": 0.5, "y": 0.2})).unwrap()["value"].as_f64().unwrap();
    // with Levels already on, the same point gives the same reading
    s.execute("develop.levels", &json!({"black": 60, "white": 140, "midtone": 0.7})).unwrap();
    let second = s.execute("develop.levelsPick", &json!({"target": "white", "x": 0.5, "y": 0.2})).unwrap()["value"].as_f64().unwrap();
    assert_eq!(first, second);
}

#[test]
fn histogram_counts_the_photo_without_levels() {
    let mut s = Session::with_demo();
    let a = s.execute("levels.histogram", &json!({})).unwrap();
    assert_eq!(a["luma"].as_array().unwrap().len(), 256);
    assert!(a["total"].as_u64().unwrap() > 0);
    s.execute("develop.levels", &json!({"black": 100, "white": 150})).unwrap();
    let b = s.execute("levels.histogram", &json!({})).unwrap();
    assert_eq!(a, b, "the histogram is of the input to the Levels");
}
