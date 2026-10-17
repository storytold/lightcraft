//! Headless tests of MIDI controller input (`docs/midi.md`): faders set, knobs nudge, a turn is one
//! undo step, and the arrow buttons step through the photos.

use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use lightcraft_engine::midi::{Action, Map, Message};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn edit_view() -> (Headless, Sender<Action>) {
    let services = Services { png: None, ..Default::default() };
    let (tx, rx) = channel();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services).with_midi(rx, Box::new(|| Ok("reloaded".into())));
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (h, tx)
}

/// What the built-in map makes of a message, as the port thread would send it.
fn send(tx: &Sender<Action>, msg: Message) {
    tx.send(smc().action(msg).unwrap()).unwrap();
}

fn smc() -> Map {
    Map::from_toml(include_str!("../../../docs/midi/smc-mixer.toml")).unwrap()
}

fn value(h: &Headless, control: &str) -> f64 {
    let id = h.app.session.active().unwrap();
    lightcraft_develop::controls::get(&h.app.session.develop_of(id).unwrap(), control).unwrap()
}

#[test]
fn a_fader_sets_and_a_knob_nudges_and_each_turn_is_one_undo_step() {
    let (mut h, tx) = edit_view();
    let (contrast, tint) = (value(&h, "light.contrast"), value(&h, "wb.tint"));

    // fader 2 to the top of its travel, in several steps: one undo step
    for v in [0u16, 4096, 8192, 16256] {
        send(&tx, Message::PitchBend { channel: 1, value: v });
        h.step();
    }
    assert_eq!(value(&h, "light.contrast"), 100.0);

    // knob 2 clockwise three ticks, then counter-clockwise one: tint moves by +2
    for v in [1u8, 1, 1, 65] {
        send(&tx, Message::ControlChange { channel: 0, cc: 17, value: v });
        h.step();
    }
    assert_eq!(value(&h, "wb.tint"), tint + 2.0);

    // after the idle gap the turns are closed: two undos put both controls back
    std::thread::sleep(Duration::from_millis(600));
    h.step();
    h.step();
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(value(&h, "wb.tint"), tint);
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(value(&h, "light.contrast"), contrast);
}

#[test]
fn the_arrows_step_through_the_photos() {
    let (mut h, tx) = edit_view();
    let first = h.app.session.active().unwrap();
    send(&tx, Message::NoteOn { channel: 0, note: 99 }); // Right
    h.step();
    let second = h.app.session.active().unwrap();
    assert_ne!(first, second);
    send(&tx, Message::NoteOn { channel: 0, note: 98 }); // Left
    h.step();
    assert_eq!(h.app.session.active(), Some(first));
}

#[test]
fn midi_reload_runs_the_hosts_reloader_and_reports_its_answer() {
    let (mut h, _tx) = edit_view();
    let r = h.app.run("midi.reload", json!({}));
    assert_eq!(r, Ok(json!("reloaded")));
    // without MIDI input (a host that has none) the command says so instead of doing nothing
    let mut bare = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    assert!(bare.run("midi.reload", json!({})).unwrap_err().contains("not available"));
}
