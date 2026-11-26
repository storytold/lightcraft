//! Preset workflow using the existing Curve graph frame fixture; no host services or file-backed library.

use std::collections::HashMap;

use super::*;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Vec2};
use lightcraft_develop::ToneCurve;

struct PresetFrames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    nodes: HashMap<String, egui::accesskit::Node>,
}

impl PresetFrames {
    fn new(channel: &str) -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.install_framework(&ctx);
        app.ui.curve_channel = channel.into();
        for channel in ["master", "red", "green", "blue"] {
            app.run("develop.curve", json!({"channel": channel, "points": [[0.0, 0.05], [0.4, 0.6], [1.0, 0.95]]})).unwrap();
        }
        for (control, value) in [
            ("curve.shadows", 25),
            ("curve.darks", -20),
            ("curve.lights", 30),
            ("curve.highlights", -35),
            ("curve.splitShadows", 20),
            ("curve.splitMid", 45),
            ("curve.splitHighlights", 80),
            ("curve.refineSaturation", 42),
        ] {
            app.run("develop.set", json!({"control": control, "value": value})).unwrap();
        }
        let mut f = Self { ctx, app, time: 0.0, rects: vec![], hits: vec![], nodes: HashMap::new() };
        f.settle();
        f
    }

    fn settings(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.app.session.active().unwrap()).unwrap()).clone()
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.frame_checked(events, 0, |_, _| {});
    }
    fn frame_checked(&mut self, events: Vec<Event>, mode: u8, mut after: impl FnMut(&egui::Ui, &LightcraftApp)) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = crate::headless::HeadlessView::raw_input(Vec2::new(900.0, 1400.0), 1.0, self.time, events);
        let settings = self.settings();
        let photo = self.app.session.active().unwrap();
        let mut output = self.ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                curve_editor(&mut self.app, ui, photo, &settings);
                after(ui, &self.app);
            };
            match mode {
                0 => draw(ui),
                1 => {
                    let ctx = ui.ctx().clone();
                    craft::ui::runtime::measure::run(&ctx, || draw(ui));
                }
                2 => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw(&mut child);
                }
                _ => unreachable!(),
            }
        });
        output.textures_delta.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if let Some(label) = node.label() {
                    self.nodes.insert(label.to_owned(), node);
                }
            }
        }
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn hit(&self, id: &str) -> WidgetHit {
        self.hits.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing response {id}")).1
    }

    fn click(&mut self, at: Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        self.frame(vec![pointer(at, button, false)]);
    }

    fn action(&mut self, id: &str, button: PointerButton) {
        let at = self.rect(id).center();
        self.click(at, button);
        self.settle();
    }
    fn menu_open(&self) -> bool {
        use craft::ui::runtime::overlay::{OpenFilter, any_open};
        any_open(&self.ctx, OpenFilter::Keys)
    }
    fn open(&mut self) {
        self.action("dropdown:curvePreset", PointerButton::Primary);
        assert!(self.menu_open());
    }
}
fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}
#[test]
fn builtin_preset_sets_only_point_channels_then_real_reset_and_two_undos() {
    let mut f = PresetFrames::new("master");
    let before = f.settings();
    let mut expected = before.clone();
    expected.curve.master = vec![Point::new(0.0, 0.0), Point::new(0.25, 0.2), Point::new(0.5, 0.5), Point::new(0.75, 0.8), Point::new(1.0, 1.0)];
    expected.curve.red.clear();
    expected.curve.green.clear();
    expected.curve.blue.clear();
    let start = f.app.session.journal.len();
    let undo = f.app.session.undo.len();
    let sel = f.app.session.selection.clone();
    f.open();
    let row = "curvePreset:Medium Contrast";
    let response = f.ctx.read_response(f.hit(row).id).unwrap();
    assert_eq!(response.rect, f.rect(row));
    assert!(response.sense.senses_click());
    f.action(row, PointerButton::Primary);
    assert_eq!(f.settings(), expected);
    assert!(!f.menu_open());
    f.action("button:curveReset", PointerButton::Primary);
    let mut reset = expected.clone();
    reset.curve = ToneCurve::default();
    assert_eq!(f.settings(), reset);
    assert_eq!(f.app.session.undo.len(), undo + 2);
    assert_eq!(
        f.app.session.journal[start..],
        [("curve.applyPreset".into(), json!({"name":"Medium Contrast"})), ("curve.reset".into(), json!({"channel":"all"}))]
    );
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.settings(), expected);
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.settings(), before);
    assert_eq!(f.app.session.selection, sel);
}
#[test]
fn secondary_click_deletes_only_named_user_preset_without_applying_or_editing_photo() {
    let mut f = PresetFrames::new("red");
    assert!(f.app.session.library.is_none() && f.app.session.library_folder.is_none());
    for name in ["Keep mine", "Remove mine"] {
        f.app.run("curve.savePreset", json!({"name":name})).unwrap();
    }
    let settings = f.settings();
    let undo = f.app.session.undo.len();
    let j = f.app.session.journal.len();
    f.open();
    f.action("curvePreset:Remove mine", PointerButton::Secondary);
    f.action("curvePresetDelete:Remove mine", PointerButton::Primary);
    assert_eq!(f.app.session.curve_presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Keep mine"]);
    assert_eq!(f.app.session.journal[j..], [("curve.deletePreset".into(), json!({"name":"Remove mine"}))]);
    assert_eq!(f.app.session.undo.len(), undo);
    assert_eq!(f.settings(), settings);
}

#[test]
fn secondary_delete_cancel_does_not_leak_to_reopened_or_builtin_preset() {
    let mut f = PresetFrames::new("red");
    f.app.run("curve.savePreset", json!({"name":"Keep mine"})).unwrap();
    let before = snapshot(&f.app);
    f.open();
    f.action("curvePreset:Keep mine", PointerButton::Secondary);
    assert!(f.rect("curvePresetDelete:Keep mine").is_positive());
    let context = craft::ui::runtime::overlay::top(&f.ctx).expect("Delete menu takes keys");
    key(&mut f, Key::Escape);
    assert_ne!(craft::ui::runtime::overlay::top(&f.ctx), Some(context), "Escape must dismiss the context menu");
    // The exiting layer can still paint, but must remove its registered row after the fade.
    for _ in 0..20 {
        f.frame(vec![]);
    }
    assert!(!f.hits.iter().any(|(name, _)| name.starts_with("curvePresetDelete:")));
    if f.menu_open() {
        key(&mut f, Key::Escape);
    }
    assert!(!f.menu_open());
    f.open();
    assert!(!f.hits.iter().any(|(name, _)| name.starts_with("curvePresetDelete:")));
    f.action("curvePreset:Linear", PointerButton::Secondary);
    assert!(!f.hits.iter().any(|(name, _)| name.starts_with("curvePresetDelete:")));
    assert_eq!(snapshot(&f.app), before);
}

fn snapshot(app: &LightcraftApp) -> serde_json::Value {
    json!({"photo":app.session.catalog.photo(app.session.active().unwrap()).unwrap(),"presets":app.session.curve_presets,"journal":app.session.journal,"undo":app.session.undo.len(),"redo":app.session.redo.len(),"selection":app.session.selection,"revision":app.session.catalog.revision,"commits":app.session.commits(),"dialog":app.ui.dialog})
}
fn key(f: &mut PresetFrames, k: Key) {
    for pressed in [true, false] {
        f.frame(vec![Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::NONE }]);
    }
    f.settle();
}
#[test]
fn missing_file_services_and_linear_save_are_disabled_but_shaped_save_opens_exact_prompt() {
    let mut f = PresetFrames::new("master");
    assert!(f.app.session.library.is_none() && f.app.services.pick_curve_preset_files.is_none() && f.app.services.save_curve_preset_file.is_none());
    f.open();
    for row in ["curvePresetMenu:import", "curvePresetMenu:export"] {
        let old = snapshot(&f.app);
        assert!(!f.ctx.read_response(f.hit(row).id).unwrap().enabled());
        f.action(row, PointerButton::Primary);
        assert_eq!(snapshot(&f.app), old);
        if !f.menu_open() {
            f.open();
        }
    }
    f.action("curvePresetMenu:save", PointerButton::Primary);
    assert_eq!(
        json!(f.app.ui.dialog),
        json!({"kind":"textPrompt","title":"Save Point Curve Preset","hint":"Preset name","value":"","command":"curve.savePreset","params":{},"key":"name"})
    );
    f.app.ui.dialog = None;
    f.open();
    f.action("curvePreset:Linear", PointerButton::Primary);
    f.open();
    assert!(!f.ctx.read_response(f.hit("curvePresetMenu:save").id).unwrap().enabled());
    let old = snapshot(&f.app);
    f.action("curvePresetMenu:save", PointerButton::Primary);
    assert_eq!(snapshot(&f.app), old);
}
#[test]
fn escape_dismisses_then_keyboard_reopens_and_applies_focused_preset() {
    let mut f = PresetFrames::new("master");
    let initial = snapshot(&f.app);
    f.open();
    key(&mut f, Key::Escape);
    assert!(!f.menu_open());
    assert_eq!(snapshot(&f.app), initial);
    // Escape returns the actual opener; reopen without a pointer, then navigate the menu with arrows.
    let opener = f.hit("dropdown:curvePreset").id;
    assert!(f.ctx.memory(|m| m.has_focus(opener)));
    key(&mut f, Key::Enter);
    assert!(f.menu_open());
    let owner = f.hit("curvePreset:Medium Contrast").id;
    for _ in 0..3 {
        if f.ctx.memory(|m| m.has_focus(owner)) {
            break;
        }
        key(&mut f, Key::ArrowDown);
    }
    assert!(f.ctx.memory(|m| m.has_focus(owner)));
    assert_eq!(snapshot(&f.app), initial);
    let j = f.app.session.journal.len();
    key(&mut f, Key::Enter);
    assert_eq!(f.app.session.journal[j..], [("curve.applyPreset".into(), json!({"name":"Medium Contrast"}))]);
    assert!(!f.menu_open());
}
#[test]
fn preset_menu_arrows_focus_and_apply_a_curve_preset_without_pointer_or_forced_focus() {
    let mut f = PresetFrames::new("master");
    f.open();
    f.frame(vec![Event::PointerGone]);
    let initial = snapshot(&f.app);
    let linear = f.hit("curvePreset:Linear").id;
    let medium = f.hit("curvePreset:Medium Contrast").id;
    // Traverse the actual preset rows through the menu's keyboard owner.
    key(&mut f, Key::ArrowDown);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(linear));
    key(&mut f, Key::ArrowDown);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(medium));
    key(&mut f, Key::ArrowUp);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(linear));
    key(&mut f, Key::ArrowDown);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(medium));
    assert_eq!(snapshot(&f.app), initial);
    let j = f.app.session.journal.len();
    key(&mut f, Key::Enter);
    assert_eq!(f.app.session.journal[j..], [("curve.applyPreset".to_owned(), json!({"name":"Medium Contrast"}))]);
    assert!(!f.menu_open());
}

#[test]
fn tab_to_closed_opener_then_enter_arrows_and_enter_apply_once() {
    let mut f = PresetFrames::new("master");
    let opener = f.hit("dropdown:curvePreset").id;
    let before = snapshot(&f.app);
    f.frame(vec![Event::PointerGone]);
    for _ in 0..40 {
        if f.ctx.memory(|m| m.has_focus(opener)) {
            break;
        }
        key(&mut f, Key::Tab);
    }
    assert!(f.ctx.memory(|m| m.has_focus(opener)), "Tab must reach the closed preset opener");
    assert!(!f.menu_open());
    key(&mut f, Key::Enter);
    assert!(f.menu_open());
    let row = f.hit("curvePreset:Medium Contrast").id;
    for _ in 0..3 {
        if f.ctx.memory(|m| m.has_focus(row)) {
            break;
        }
        key(&mut f, Key::ArrowDown);
    }
    assert!(f.ctx.memory(|m| m.has_focus(row)));
    assert_eq!(snapshot(&f.app), before);
    let j = f.app.session.journal.len();
    key(&mut f, Key::Enter);
    assert_eq!(f.app.session.journal[j..], [("curve.applyPreset".into(), json!({"name":"Medium Contrast"}))]);
    assert!(!f.menu_open());
}

fn tab_direction(f: &mut PresetFrames, reverse: bool) {
    let modifiers = Modifiers { shift: reverse, ..Modifiers::NONE };
    for pressed in [true, false] {
        f.frame(vec![
            Event::ModifiersChanged(modifiers),
            Event::Key { key: Key::Tab, physical_key: Some(Key::Tab), pressed, repeat: false, modifiers },
        ]);
    }
    f.frame(vec![Event::ModifiersChanged(Modifiers::NONE)]);
    f.settle();
}
fn tab_to_opener(f: &mut PresetFrames) -> egui::Id {
    let owner = f.hit("dropdown:curvePreset").id;
    for _ in 0..40 {
        if f.ctx.memory(|m| m.has_focus(owner)) {
            return owner;
        }
        key(f, Key::Tab);
    }
    panic!("Tab did not reach the closed Curve opener");
}
fn check_curve_exit(reverse: bool, escape: bool) {
    let mut f = PresetFrames::new("master");
    let before = snapshot(&f.app);
    let opener = tab_to_opener(&mut f);
    tab_direction(&mut f, reverse);
    let neighbor = f.ctx.memory(|m| m.focused()).expect("real closed-menu neighbor");
    assert_ne!(neighbor, opener);
    tab_direction(&mut f, !reverse);
    assert!(f.ctx.memory(|m| m.has_focus(opener)));
    key(&mut f, Key::Enter);
    assert!(f.menu_open());
    key(&mut f, Key::ArrowDown);
    if escape {
        key(&mut f, Key::Escape);
        assert!(!f.menu_open(), "Escape must close the menu");
        assert!(f.ctx.memory(|m| m.has_focus(opener)), "Escape should restore the actual opener");
    }
    tab_direction(&mut f, reverse);
    assert_eq!(snapshot(&f.app), before, "leaving must not apply a preset");
    let focus = f.ctx.memory(|m| m.focused());
    assert!(!f.menu_open(), "Tab(reverse={reverse}) left menu open with focus {focus:?}; expected {neighbor:?}");
    assert_eq!(focus, Some(neighbor), "focus must continue to the real closed-menu neighbor");
}
#[test]
fn tab_leaves_curve_menu_for_next_control_without_applying() {
    check_curve_exit(false, false);
}
#[test]
fn shift_tab_leaves_curve_menu_for_previous_control_without_applying() {
    check_curve_exit(true, false);
}
#[test]
fn escape_returns_to_opener_then_tab_reaches_next_control() {
    check_curve_exit(false, true);
}

fn live_preset(mode: u8) {
    let mut f = PresetFrames::new("master");
    f.open();
    let id = "curvePreset:Medium Contrast";
    let owner = f.hit(id).id;
    let at = f.ctx.read_response(owner).unwrap().interact_rect.center();
    let before = snapshot(&f.app);
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame_checked(vec![pointer(at, PointerButton::Primary, true)], 0, |ui, app| {
        assert!(ui.ctx().read_response(owner).unwrap().is_pointer_button_down_on());
        assert_eq!(snapshot(app), before);
    });
    let mut seen = 0;
    f.frame_checked(vec![pointer(at, PointerButton::Primary, false)], mode, |ui, app| {
        seen += 1;
        assert_eq!(ui.is_sizing_pass(), mode == 2);
        assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), mode == 1);
        assert_eq!(snapshot(app), before);
        let (r, h) = take_registries(ui.ctx());
        assert!(r.iter().all(|(id, _)| !id.starts_with("curvePreset")) && h.iter().all(|(id, _)| !id.starts_with("curvePreset")));
    });
    assert!(seen > 0);
    f.settle();
    assert_eq!(snapshot(&f.app), before);
    if !f.menu_open() {
        f.open();
    }
    assert_eq!(f.hit(id).id, owner);
    let j = f.app.session.journal.len();
    f.action(id, PointerButton::Primary);
    assert_eq!(f.app.session.journal[j..], [("curve.applyPreset".into(), json!({"name":"Medium Contrast"}))]);
}
#[test]
fn sdk_measured_preset_release_then_same_owner_ordinary_apply() {
    live_preset(1);
}
#[test]
fn native_sizing_preset_release_then_same_owner_ordinary_apply() {
    live_preset(2);
}
