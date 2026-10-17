//! The tick box and the radio: what a click and Space do, that a disabled one ignores both, what
//! screen readers hear, that they read in every theme, and that migrated controls are reachable by
//! their automation ids.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, vec2};
use serde_json::json;

use crate::button::{State, contrast};
use crate::check::{Check, box_look, check, radio};
use crate::headless::Headless;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

/// A context kept across frames, so focus and pointer state carry from one to the next.
struct Rig {
    ctx: egui::Context,
}

impl Rig {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        Rig { ctx }
    }

    /// One frame with `events`; returns what `draw` made and the AccessKit nodes' (role, name, toggled).
    fn frame<T>(&self, events: Vec<egui::Event>, draw: impl FnOnce(&mut egui::Ui) -> T) -> (T, Vec<(String, String, Option<String>)>) {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
        let mut out_val = None;
        let mut draw = Some(draw);
        let mut out = self.ctx.run_ui(raw, |ui| out_val = draw.take().map(|d| d(ui)));
        out.textures_delta.clear();
        let nodes = out
            .platform_output
            .accesskit_update
            .take()
            .map(|u| {
                u.nodes
                    .iter()
                    .map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string(), n.toggled().map(|t| format!("{t:?}"))))
                    .collect()
            })
            .unwrap_or_default();
        (out_val.expect("the frame ran"), nodes)
    }
}

fn click_at(at: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]
}

fn space() -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key: egui::Key::Space,
        physical_key: Some(egui::Key::Space),
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }]
}

#[test]
fn a_click_toggles_a_check_and_the_response_says_changed() {
    let rig = Rig::new();
    let mut on = false;
    let (r, _) = rig.frame(Vec::new(), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    let (r, _) = rig.frame(click_at(r.rect.center()), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    assert!(r.changed() && on, "a click turns it on");
    let (r, _) = rig.frame(click_at(r.rect.center()), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    assert!(r.changed() && !on, "and another turns it off");
}

#[test]
fn space_toggles_the_focused_check_and_radio() {
    let rig = Rig::new();
    let (mut on, mut choice) = (false, 1);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = check(ui, "a", on, "Alpha");
        let r = radio(ui, "b-2", choice, 2, "Two");
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.ctx.memory_mut(|m| m.request_focus(c.id));
    rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert!(on, "Space turned the focused check on");
    assert_eq!(choice, 1, "and left the radio alone");
    rig.ctx.memory_mut(|m| m.request_focus(r.id));
    rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert_eq!(choice, 2, "Space picked the focused radio");
}

#[test]
fn a_radio_picks_its_value_once() {
    let rig = Rig::new();
    let mut choice = "a";
    let draw =
        |ui: &mut egui::Ui, choice: &mut &str| ui.horizontal(|ui| (radio(ui, "r-a", choice, "a", "A"), radio(ui, "r-b", choice, "b", "B"))).inner;
    let ((_, b), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut choice));
    let ((_, b), _) = rig.frame(click_at(b.rect.center()), |ui| draw(ui, &mut choice));
    assert!(b.changed());
    assert_eq!(choice, "b");
    let ((_, b), _) = rig.frame(click_at(b.rect.center()), |ui| draw(ui, &mut choice));
    assert!(!b.changed(), "clicking the chosen one again changes nothing");
    assert_eq!(choice, "b");
}

#[test]
fn a_disabled_check_and_radio_ignore_clicks_and_keys() {
    let rig = Rig::new();
    let (mut on, mut choice) = (false, 0);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = Check::new("d", on, "Off").enabled(false).tooltip("Needs a model").show(ui);
        let r = crate::check::Radio::new("e", choice, 1, "Radio").enabled(false).show(ui);
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    for target in [c.rect.center(), r.rect.center()] {
        rig.frame(click_at(target), |ui| draw(ui, &mut on, &mut choice));
    }
    rig.ctx.memory_mut(|m| m.request_focus(c.id));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert!(!on && choice == 0, "nothing changed");
    // and a screen reader is told so
    let (_, nodes) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    assert!(nodes.iter().any(|n| n.1 == "Off"), "still announced: {nodes:?}");
}

#[test]
fn a_disabled_check_and_radio_say_why_on_hover() {
    let rig = Rig::new();
    rig.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
    let (mut on, mut choice) = (false, 0);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = Check::new("d", on, "Off").enabled(false).tooltip("Needs a model").show(ui);
        let r = crate::check::Radio::new("e", choice, 1, "Radio").enabled(false).tooltip("Not now").show(ui);
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    for target in [c.rect.center(), r.rect.center()] {
        let hover = vec![egui::Event::PointerMoved(target)];
        for _ in 0..3 {
            rig.frame(hover.clone(), |ui| draw(ui, &mut on, &mut choice));
        }
        let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
        assert!(c.is_tooltip_open() || r.is_tooltip_open(), "the reason shows on hover at {target:?}");
    }
}

#[test]
fn screen_readers_hear_a_checkbox_and_a_radio_with_their_state() {
    let rig = Rig::new();
    let mut choice = 3;
    let mut on = true;
    let (_, nodes) = rig.frame(Vec::new(), |ui| {
        check(ui, "x", &mut on, "Auto Crop");
        radio(ui, "y-3", &mut choice, 3, "Spherical");
        radio(ui, "y-4", &mut choice, 4, "Cylindrical");
    });
    let find = |name: &str| nodes.iter().find(|n| n.1 == name).cloned().unwrap_or_default();
    let (check_node, picked, other) = (find("Auto Crop"), find("Spherical"), find("Cylindrical"));
    assert_eq!(check_node.0, "CheckBox", "{nodes:?}");
    assert_eq!(check_node.2.as_deref(), Some("True"));
    assert_eq!(picked.0, "RadioButton");
    assert_eq!(picked.2.as_deref(), Some("True"));
    assert_eq!(other.2.as_deref(), Some("False"));
}

#[test]
fn a_check_is_addressable_by_its_id() {
    let rig = Rig::new();
    let mut on = false;
    rig.frame(Vec::new(), |ui| {
        check(ui, "settings.gpu", &mut on, "Use the GPU");
        radio(ui, "theme-paper", &mut 0, 0, "Paper");
    });
    let ids: Vec<String> = crate::widgets::take_registry(&rig.ctx).into_iter().map(|(id, _)| id).collect();
    assert!(ids.contains(&"check:settings.gpu".to_string()) && ids.contains(&"radio:theme-paper".to_string()), "{ids:?}");
}

#[test]
fn the_box_reads_in_every_theme_and_state() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        for state in [State::Idle, State::Hovered, State::Pressed] {
            let on = box_look(&t, true, state);
            assert!(contrast(on.mark, on.fill) >= 4.5, "{kind:?} {state:?}: the tick on the accent {:.2}:1", contrast(on.mark, on.fill));
            let off = box_look(&t, false, state);
            for ground in [t.chrome, t.canvas] {
                assert!(contrast(off.outline, ground) >= 3.0, "{kind:?} {state:?}: the empty box's outline {:.2}:1", contrast(off.outline, ground));
            }
        }
        // and a disabled one is fainter than an idle one
        let (idle, off) = (box_look(&t, true, State::Idle), box_look(&t, true, State::Disabled));
        assert!(off.fill.a() < idle.fill.a() || off.fill != idle.fill);
        assert_ne!(Color32::TRANSPARENT, off.mark);
    }
}

// ------------------------------------------------------------------------ migrated controls

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn keywords_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    h
}

/// A dialog's checkbox (Edit Keyword Set's "Save as a new set") is reached by `check:…` and a click
/// turns the choice on; the Keywording view switch (a toggle row) by `button:…`.
#[test]
fn a_dialog_checkbox_and_a_toggle_are_reachable_by_id() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    h.settle(SETTLE);
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordSetCombo"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordSetMenu:edit"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:keywordSetAsNew"}));
    let Some(crate::state::Dialog::KeywordSet { as_new, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert!(as_new, "the click ticked the box");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogCancel"}));

    ask(&mut h, "ui.clickWidget", json!({"id": "button:keywordView-containing"}));
    assert_eq!(h.app.ui.keywording_view, crate::state::KeywordingView::Containing);
}
