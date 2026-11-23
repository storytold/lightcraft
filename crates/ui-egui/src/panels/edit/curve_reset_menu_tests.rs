//! Curve-reset SDK seam: actual graph popup, in-memory session, no chooser, IO or renderer.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Vec2};
use lightcraft_develop::ToneCurve;

struct CurveResetFrames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    nodes: HashMap<String, egui::accesskit::Node>,
}

impl CurveResetFrames {
    fn new(channel: &str) -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.curve_channel = channel.into();
        for channel in ["master", "red", "green", "blue"] {
            app.run("develop.curve", json!({"channel": channel, "points": [[0.0, 0.05], [0.4, 0.6], [1.0, 0.95]]})).unwrap();
        }
        for (control, value) in [
            ("curve.shadows", 25), ("curve.darks", -20), ("curve.lights", 30), ("curve.highlights", -35),
            ("curve.splitShadows", 20), ("curve.splitMid", 45), ("curve.splitHighlights", 80), ("curve.refineSaturation", 42),
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
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = crate::headless::HeadlessView::raw_input(Vec2::new(900.0, 1400.0), 1.0, self.time, events);
        let settings = self.settings();
        let photo = self.app.session.active().unwrap();
        let mut output = self.ctx.run_ui(raw, |ui| curve_editor(&mut self.app, ui, photo, &settings));
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
        for _ in 0..3 { self.frame(vec![]); }
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

    fn open(&mut self) {
        self.click(self.rect("curve").center(), PointerButton::Secondary);
        self.settle();
        assert!(egui::Popup::is_any_open(&self.ctx));
        for id in ["curveMenu:resetChannel", "curveMenu:resetAll"] {
            assert_eq!(self.rects.iter().filter(|(name, _)| name == id).count(), 1);
            assert_eq!(self.hits.iter().filter(|(name, _)| name == id).count(), 1);
            let hit = self.hit(id);
            let response = self.ctx.read_response(hit.id).expect("real menu response");
            assert!(response.sense.senses_click(), "not an automation hover probe");
            assert_eq!(hit.layer_id, response.layer_id);
            assert_eq!(hit.interact_rect, response.interact_rect);
            assert_eq!(self.rect(id), response.rect);
        }
    }

    fn key(&mut self, key: Key) {
        let events = [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE });
        self.frame(events.to_vec());
        self.settle();
    }

    fn assert_one_reset(&self, journal: usize, undo: usize, channel: &str) {
        let commands = &self.app.session.journal[journal..];
        assert_eq!(commands.len(), 1, "one dispatch, including no-op duplicates: {commands:?}");
        assert_eq!(commands[0], ("curve.reset".to_owned(), json!({"channel": channel})));
        assert_eq!(self.app.session.undo.len(), undo + 1);
        assert!(!egui::Popup::is_any_open(&self.ctx), "native egui menu dismissed");
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn expected_current(mut before: ToneCurve, channel: &str) -> ToneCurve {
    let d = ToneCurve::default();
    match channel {
        "parametric" => {
            (before.shadows, before.darks, before.lights, before.highlights) = (d.shadows, d.darks, d.lights, d.highlights);
            (before.split_shadows, before.split_mid, before.split_highlights) = (d.split_shadows, d.split_mid, d.split_highlights);
        }
        "master" => before.master.clear(),
        "red" => before.red.clear(),
        "green" => before.green.clear(),
        "blue" => before.blue.clear(),
        _ => panic!("test channel"),
    }
    before
}

#[test]
fn sdk_curve_reset_current_keeps_channel_payload_and_restores_full_settings_on_undo() {
    for (channel, label) in [
        ("parametric", "Reset Parametric Curve"), ("master", "Reset RGB Channel"),
        ("red", "Reset Red Channel"), ("green", "Reset Green Channel"), ("blue", "Reset Blue Channel"),
    ] {
        let mut f = CurveResetFrames::new(channel);
        let before = f.settings();
        let journal = f.app.session.journal.len();
        let undo = f.app.session.undo.len();
        f.open();
        for label in [label, "Reset All Curves"] {
            assert!(f.nodes.values().any(|n| n.role() == egui::accesskit::Role::MenuItem && n.label() == Some(label)), "SDK menu role/name: {label}");
        }
        assert_eq!(f.settings(), before, "secondary opening alone never edits");
        f.click(f.rect("curveMenu:resetChannel").center(), PointerButton::Primary);
        f.settle();
        f.assert_one_reset(journal, undo, channel);
        let mut expected = before.clone();
        expected.curve = expected_current(before.curve.clone(), channel);
        assert_eq!(f.settings(), expected, "only {channel} resets; refine saturation and other edits survive");
        assert_eq!(f.app.ui.curve_channel, channel);
        f.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(f.settings(), before);
        f.app.run("edit.redo", json!({})).unwrap();
        assert_eq!(f.settings(), expected);
    }
}

#[test]
fn sdk_curve_reset_all_pointer_enter_and_space_dispatch_once_and_dismiss_native_popup() {
    for channel in ["parametric", "red"] {
        for key in [None, Some(Key::Enter), Some(Key::Space)] {
            let mut f = CurveResetFrames::new(channel);
            let before = f.settings();
            let journal = f.app.session.journal.len();
            let undo = f.app.session.undo.len();
            f.open();
            if let Some(key) = key {
                let hit = f.hit("curveMenu:resetAll");
                f.ctx.memory_mut(|m| m.request_focus(hit.id));
                f.frame(vec![]);
                f.key(key);
            } else {
                f.click(f.rect("curveMenu:resetAll").center(), PointerButton::Primary);
                f.settle();
            }
            f.assert_one_reset(journal, undo, "all");
            let mut expected = before.clone();
            expected.curve = ToneCurve::default();
            assert_eq!(f.settings(), expected, "all includes parametric splits and refine saturation");
            f.app.run("edit.undo", json!({})).unwrap();
            assert_eq!(f.settings(), before);
            f.app.run("edit.redo", json!({})).unwrap();
            assert_eq!(f.settings(), expected);
        }
    }
}

#[test]
fn sdk_curve_reset_escape_outside_secondary_and_cancelled_press_do_not_dispatch() {
    for cancel in ["escape", "outside", "secondary", "drag-out"] {
        let mut f = CurveResetFrames::new("green");
        let before = f.settings();
        let journal = f.app.session.journal.len();
        let undo = f.app.session.undo.len();
        f.open();
        let target = f.rect("curveMenu:resetChannel").center();
        match cancel {
            "escape" => f.key(Key::Escape),
            "outside" => f.click(pos2(4.0, 4.0), PointerButton::Primary),
            "secondary" => f.click(target, PointerButton::Secondary),
            _ => {
                f.frame(vec![Event::PointerMoved(target)]);
                f.frame(vec![pointer(target, PointerButton::Primary, true)]);
                f.frame(vec![Event::PointerMoved(pos2(4.0, 4.0))]);
                f.frame(vec![pointer(pos2(4.0, 4.0), PointerButton::Primary, false)]);
            }
        }
        f.settle();
        assert_eq!(f.settings(), before, "{cancel}");
        assert_eq!(f.app.session.journal.len(), journal, "{cancel}");
        assert_eq!(f.app.session.undo.len(), undo, "{cancel}");
        if cancel == "escape" || cancel == "outside" {
            assert!(!egui::Popup::is_any_open(&f.ctx), "{cancel} dismisses");
        }
    }
}

#[test]
fn sdk_curve_reset_each_sizing_mode_suppresses_automation_and_commands() {
    for (egui_sizing, sdk_measure) in [(true, false), (false, true), (true, true)] {
        let mut f = CurveResetFrames::new("parametric");
        let before = f.settings();
        let journal = f.app.session.journal.len();
        let undo = f.app.session.undo.len();
        let ctx = f.ctx.clone();
        let _ = take_registries(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut draw = || {
                let mut builder = egui::UiBuilder::new().max_rect(ui.max_rect());
                if egui_sizing { builder = builder.sizing_pass(); }
                let mut child = ui.new_child(builder);
                assert_eq!(child.is_sizing_pass(), egui_sizing);
                assert_eq!(craft::ui::runtime::measure::active(child.ctx()), sdk_measure);
                curve_reset_menu(&mut f.app, &mut child, "parametric");
            };
            if sdk_measure { craft::ui::runtime::measure::run(&ctx, draw); } else { draw(); }
        });
        output.textures_delta.clear();
        let (rects, hits) = take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty(), "no invisible targets: {egui_sizing}/{sdk_measure}");
        assert_eq!(f.settings(), before);
        assert_eq!(f.app.session.journal.len(), journal);
        assert_eq!(f.app.session.undo.len(), undo);
    }
}
