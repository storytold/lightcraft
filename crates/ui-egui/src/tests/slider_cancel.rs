//! A slider drag keeps the engine interaction it began: an abandoned drag cancels that one, and one another
//! client (an automation command) began in the middle of the drag is that client's to finish.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::InteractionOwner;
use crate::widgets::SliderOut;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The demo library with its first photo selected: the drags below only need a photo to edit.
fn panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

/// One step of an Exposure slider drag, as the framework reports it to the panel.
fn drag(h: &mut Headless, out: SliderOut) {
    let spec = lightcraft_develop::controls::find("light.exposure").expect("the Exposure control");
    crate::panels::edit::apply_slider_out(&mut h.app, spec, out, |app, v| app.run("develop.set", json!({"control": "light.exposure", "value": v})));
}

fn value(h: &Headless, control: &str) -> f64 {
    let photo = h.app.session.active().expect("an active photo");
    let spec = lightcraft_develop::controls::find(control).expect("a known control");
    lightcraft_develop::controls::get(&h.app.session.develop_of(photo).unwrap_or_default(), control).unwrap_or(spec.default)
}

fn settings(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let photo = h.app.session.active().expect("an active photo");
    (*h.app.session.develop_of(photo).unwrap_or_default()).clone()
}

/// Begin a drag on Exposure and move it, as a pointer drag does.
fn drag_to_15(h: &mut Headless) {
    drag(h, SliderOut { drag_started: true, ..Default::default() });
    drag(h, SliderOut { value: Some(1.5), ..Default::default() });
    assert!(h.app.session.interaction.as_ref().is_some_and(|i| i.label == "Exposure"), "the drag owns an interaction");
    assert!(h.app.ui.dragging_interaction.is_some(), "and the slider knows it began it");
}

/// A drag that ends the way a drag can — abandoned, or released — cancels or commits the interaction it began:
/// the settings are the ones from before it, or its own value, in one undo step.
#[test]
fn a_drag_ends_the_interaction_it_began() {
    for abandoned in [true, false] {
        let mut h = panel();
        let before = settings(&h);
        let undo = h.app.session.undo.len();
        drag_to_15(&mut h);
        let out =
            if abandoned { SliderOut { cancelled: true, ..Default::default() } } else { SliderOut { drag_stopped: true, ..Default::default() } };
        drag(&mut h, out);
        assert!(h.app.session.interaction.is_none(), "abandoned={abandoned}: the drag's interaction is closed");
        assert!(
            h.app.ui.dragging_control.is_none() && h.app.ui.dragging_interaction.is_none(),
            "abandoned={abandoned}: and the slider lets go of it"
        );
        if abandoned {
            assert_eq!(settings(&h), before, "an abandoned drag restores the settings from before it");
            assert_eq!(h.app.session.undo.len(), undo, "and records no undo step");
        } else {
            assert_eq!(value(&h, "light.exposure"), 1.5, "a released drag lands where it stopped");
            assert_eq!(h.app.session.undo.len() - undo, 1, "as one undo step");
        }
    }
}

/// An automation client can begin its own interaction in the middle of a drag (`develop.beginInteraction` ends the
/// drag's). The drag has no claim on that one: its cancel leaves the client's settings and live interaction as they
/// are, and its release does not commit the client's edit for it.
#[test]
fn a_drag_leaves_the_interaction_another_client_began() {
    for ending in ["cancel", "release"] {
        let mut h = panel();
        drag_to_15(&mut h);
        // the automation command: its own interaction, and one live edit of the Contrast
        h.app.run("develop.beginInteraction", json!({"label": "Contrast"})).expect("the client's interaction");
        h.app.run("develop.set", json!({"control": "light.contrast", "value": 25.0})).expect("the client's edit");
        assert_eq!(value(&h, "light.exposure"), 1.5, "the client's begin committed the drag");
        let undo = h.app.session.undo.len();
        // the slider is ended after all: the control's id still names it, the interaction is not its own
        let out = if ending == "cancel" {
            SliderOut { cancelled: true, ..Default::default() }
        } else {
            SliderOut { drag_stopped: true, ..Default::default() }
        };
        drag(&mut h, out);
        assert_eq!(value(&h, "light.contrast"), 25.0, "{ending}: the client's settings stand");
        assert!(
            h.app.session.interaction.as_ref().is_some_and(|i| i.label == "Contrast"),
            "{ending}: the client's interaction is still open: {:?}",
            h.app.session.interaction
        );
        assert_eq!(h.app.session.undo.len(), undo, "{ending}: and nothing was committed");
        assert!(h.app.ui.dragging_control.is_none() && h.app.ui.dragging_interaction.is_none(), "{ending}: the slider lets go of it");
        // the client's own commit still works, and still is the one undo step
        h.app.run("develop.endInteraction", json!({})).expect("the client's commit");
        assert!(h.app.session.interaction.is_none());
        assert_eq!(h.app.session.undo.len() - undo, 1);
        assert_eq!(value(&h, "light.contrast"), 25.0);
    }
}

/// The owner identifies one engine interaction, not a control: the interaction a drag began is its own until another
/// begins, and no two interactions share an owner.
#[test]
fn an_owner_names_one_interaction() {
    let mut h = panel();
    drag_to_15(&mut h);
    let owner = h.app.ui.dragging_interaction.clone().expect("the drag's owner");
    let live = h.app.session.interaction.clone().expect("the drag's interaction");
    assert!(owner.owns(&live) && owner.is_live(&h.app.session));
    // another client's interaction is not the owner's, though it edits the same photo
    h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).expect("another interaction");
    let other = h.app.session.interaction.clone().expect("the other interaction");
    assert!(!owner.owns(&other), "a later interaction with the same label and photo is another one");
    assert!(!owner.is_live(&h.app.session));
    assert!(InteractionOwner::of(&other).owns(&other));
}
