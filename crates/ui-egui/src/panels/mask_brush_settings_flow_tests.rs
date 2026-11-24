//! Actual installed Masking Brush parent. Demo records only; no stroke, render/logic loop or model.
//! Settings native scope stays private; this suite does not manufacture or claim native-parent coverage.
use super::LightcraftApp;
use crate::{Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const AUTO: &str = "check:brushAutoMask";
const PRESSURE: &str = "check:brushPressure";
fn control(prefix: &str, name: &str) -> String {
    format!("{prefix}:ui.brush{name}")
}
fn owned_alias(key: &str) -> bool {
    key == AUTO || key == PRESSURE || ["Size", "Feather", "Flow"].iter().any(|name| key.ends_with(&format!(":ui.brush{name}")))
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
fn drain(mut output: egui::FullOutput) {
    output.textures_delta.clear();
    output.platform_output.commands.clear();
    output.viewport_output.clear();
    output.shapes.clear();
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 2],
    masks: [u32; 3],
    probe: Probe,
    tick: u32,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
}
impl Fixture {
    fn new() -> Self {
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let ids = app.session.visible_cloned();
        let photos = [ids[0], ids[1]];
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.add", json!({"kind":"brush","name":"Target brush"})).unwrap();
        let target = app.session.active_mask.unwrap();
        app.run("mask.add", json!({"kind":"brush","name":"Neighbor brush"})).unwrap();
        let other = app.session.active_mask.unwrap();
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[1].0})).unwrap();
        app.run("mask.add", json!({"kind":"brush","name":"Other photo brush"})).unwrap();
        let secondary = app.session.active_mask.unwrap();
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.select", json!({"id":target})).unwrap();
        app.run("tool.brush", json!({})).unwrap();
        assert_eq!(app.ui.tool, "brush");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f =
            Self { app, ctx, photos, masks: [target, other, secondary], probe: Probe::Real, tick: 0, rects: BTreeMap::new(), hits: BTreeMap::new() };
        let before = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before, "passive installed Brush parent changes no photo, history or preference");
        assert!(f.rects.contains_key(AUTO) && f.rects.contains_key(PRESSURE));
        assert!(f.rects.contains_key(&control("slider", "Size")));
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(!self.app.session.segmenter.analyzing() && !self.app.session.segmenter.busy());
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1600.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let probe = self.probe;
        let ctx = self.ctx.clone();
        let mut registries = (vec![], vec![]);
        let output = self.ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(360.0);
                let mut draw = |ui: &mut egui::Ui| {
                    if let Some(photo) = app.session.active() {
                        super::show(app, ui, photo);
                    }
                };
                match probe {
                    Probe::Real => draw(ui),
                    Probe::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Probe::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Probe::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                }
            });
            let now = crate::widgets::take_registries(root.ctx());
            registries = if root.ctx().will_discard() { (vec![], vec![]) } else { now };
        });
        self.rects = registries.0.into_iter().collect();
        self.hits = registries.1.into_iter().collect();
        drain(output);
    }
    fn rect(&self, key: &str) -> Rect {
        *self.rects.get(key).unwrap_or_else(|| panic!("missing actual panel locator {key}: {:?}", self.rects.keys()))
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_at(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn click(&mut self, key: &str) {
        self.click_at(self.rect(key).center());
    }
    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn tab_to(&mut self, key: &str) {
        let owner = self.hits.get(key).unwrap().id;
        self.frame(vec![Event::PointerGone]);
        for _ in 0..96 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == owner) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {key}, owner={owner:?}");
    }

    fn trace(&self, name: &str, phase: &str) {
        let field = self.hits.get(&control("sliderField", name)).map(|hit| format!("{:?}", hit.id));
        let focus = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| format!("{:?}", record.stop.id));
        println!(
            "BRUSH_INPUT_TRACE {}",
            json!({"phase":phase,"name":name,"photo":self.app.session.active(),
            "mask":self.app.session.active_mask,"revision":self.app.session.catalog.revision,"field":field,"focus":focus,
            "pointer":self.ctx.input(|input|json!({"down":input.pointer.primary_down(),"position":input.pointer.interact_pos().map(|at|[at.x,at.y])})),
            "shown":self.displayed(name),"erase":self.app.ui.brush_erase})
        );
    }
    fn text(&mut self, name: &str, text: &str) {
        self.click(&control("sliderField", name));
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![Event::Text(text.to_owned())]);
    }
    fn drag(&mut self, name: &str, from: f32, to: f32) -> Pos2 {
        let rect = self.rect(&control("slider", name));
        let start = egui::pos2(egui::lerp(rect.x_range(), from), rect.center().y);
        self.frame(vec![Event::PointerMoved(start)]);
        self.pointer(start, true);
        self.frame(vec![Event::PointerMoved(egui::pos2(egui::lerp(rect.x_range(), (from + to) * 0.5), rect.center().y))]);
        let end = egui::pos2(egui::lerp(rect.x_range(), to), rect.center().y);
        self.frame(vec![Event::PointerMoved(end)]);
        end
    }
    fn release(&mut self, at: Pos2) {
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn domain(&self) -> Value {
        let photos: Vec<_> = self.photos.iter().map(|id| self.app.session.catalog.photo(*id).unwrap()).collect();
        let undo: Vec<_> = self.app.session.undo.iter().map(|entry| json!({"label":entry.label,"op":entry.op})).collect();
        let redo: Vec<_> = self.app.session.redo.iter().map(|entry| json!({"label":entry.label,"op":entry.op})).collect();
        json!({"photos":photos,"revision":self.app.session.catalog.revision,"active":self.app.session.active(),"selected":self.app.session.selection.ids,"mask":self.app.session.active_mask,"undo":undo,"redo":redo,"journal":self.app.session.journal,"interaction":self.app.session.interaction.is_some()})
    }
    fn ui(&self) -> Value {
        serde_json::to_value(&self.app.ui).unwrap()
    }
    fn snapshot(&self) -> Value {
        json!({"domain":self.domain(),"ui":self.ui()})
    }
    fn except(mut ui: Value, allowed: &[&str]) -> Value {
        for name in allowed {
            ui.as_object_mut().unwrap().remove(*name);
        }
        ui
    }
    fn only(&self, before: &Value, allowed: &[&str], domain: &Value) {
        assert_eq!(
            &self.domain(),
            domain,
            "brush preferences never edit either complete photo, history, journal, selection or exact Undo/Redo operations"
        );
        assert_eq!(Self::except(self.ui(), allowed), Self::except(before.clone(), allowed), "only the requested brush preferences change");
    }
    fn displayed(&self, name: &str) -> f32 {
        match name {
            "Size" => self.app.ui.brush_size * 400.0,
            "Feather" => self.app.ui.brush_feather,
            _ => self.app.ui.brush_flow,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1600.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn brush_parent_auto_mask_pointer_and_keyboard_change_only_view_preference() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    assert!(!f.app.ui.brush_auto_mask);
    f.click(AUTO);
    assert!(f.app.ui.brush_auto_mask);
    f.only(&before, &["brushAutoMask"], &domain);
    f.tab_to(AUTO);
    f.key(Key::Space);
    assert!(!f.app.ui.brush_auto_mask);
    f.only(&before, &["brushAutoMask"], &domain);
}
#[test]
fn brush_parent_pressure_pointer_and_keyboard_preserve_every_photo_and_neighbor() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    assert!(f.app.ui.brush_pressure);
    f.click(PRESSURE);
    assert!(!f.app.ui.brush_pressure);
    f.only(&before, &["brushPressure"], &domain);
    f.tab_to(PRESSURE);
    f.key(Key::Space);
    assert!(f.app.ui.brush_pressure);
    f.only(&before, &["brushPressure"], &domain);
}
#[test]
fn brush_parent_size_live_pointer_and_keyboard_retain_long_edge_fraction_units() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    let at = f.drag("Size", 15.0 / 99.0, 0.72);
    assert!(f.app.ui.brush_size > 0.13 && f.app.ui.brush_size < 0.23, "held Size is a long-edge fraction, not 1..100: {}", f.app.ui.brush_size);
    f.only(&before, &["brushSize"], &domain);
    f.release(at);
    f.tab_to(&control("slider", "Size"));
    f.key(Key::Home);
    assert!((f.app.ui.brush_size - 1.0 / 400.0).abs() < 1e-6);
    f.key(Key::End);
    assert!((f.app.ui.brush_size - 0.25).abs() < 1e-6);
    f.only(&before, &["brushSize"], &domain);
}
#[test]
fn brush_parent_feather_and_flow_pointer_keyboard_and_reset_target_separate_preferences() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    let at = f.drag("Feather", 0.5, 0.82);
    assert!(f.app.ui.brush_feather > 70.0);
    f.release(at);
    f.only(&before, &["brushFeather"], &domain);
    let at = f.drag("Flow", 59.0 / 99.0, 0.28);
    assert!(f.app.ui.brush_flow < 40.0);
    f.release(at);
    for (name, min) in [("Feather", 0.0), ("Flow", 1.0)] {
        f.tab_to(&control("slider", name));
        f.key(Key::Home);
        assert_eq!(f.displayed(name), min);
        f.key(Key::End);
        assert_eq!(f.displayed(name), 100.0);
        let label = control("sliderLabel", name);
        f.click(&label);
        f.click(&label);
        assert_eq!(f.displayed(name), min);
    }
    f.only(&before, &["brushFeather", "brushFlow"], &domain);
}
#[test]
fn brush_parent_exact_entry_escape_clear_and_bounds_preserve_original_size_units() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    for (name, min) in [("Size", 1.0), ("Feather", 0.0), ("Flow", 1.0)] {
        let draft = f.snapshot();
        f.text(name, "63%");
        assert_eq!(f.snapshot(), draft, "typed proposals are commit-only");
        f.key(Key::Enter);
        assert!((f.displayed(name) - 63.0).abs() < 1e-4);
        let committed = f.snapshot();
        f.trace(name, "committed text before blur");
        // Return committed text but kept the numeric editor active; a drag there selects text.
        // Naturally blur it before testing the SDK's distinct, non-editing scrub gesture.
        f.tab_to(&control("slider", name));
        f.trace(name, "track owns focus before field scrub");
        assert_eq!(f.snapshot(), committed, "leaving text edit changes no preference or domain state");
        let start = f.rect(&control("sliderValue", name)).center();
        f.frame(vec![Event::PointerMoved(start)]);
        f.pointer(start, true);
        f.trace(name, "field press");
        f.frame(vec![Event::PointerMoved(start + egui::vec2(15.0, 0.0))]);
        f.trace(name, "first held move");
        f.frame(vec![Event::PointerMoved(start + egui::vec2(24.0, 0.0))]);
        f.trace(name, "second held move");
        assert!((f.displayed(name) - 63.0).abs() > 0.5, "numeric scrub previews while held");
        assert_eq!(f.domain(), domain);
        f.key(Key::Escape);
        f.trace(name, "Escape while held");
        f.release(start + egui::vec2(24.0, 0.0));
        f.trace(name, "release after cancellation");
        assert_eq!(f.snapshot(), committed, "scrub cancellation restores the complete view preference without history");
        f.text(name, "21");
        f.key(Key::Escape);
        assert_eq!(f.snapshot(), committed);
        f.text(name, "");
        f.key(Key::Backspace);
        f.key(Key::Enter);
        assert_eq!(f.snapshot(), committed, "absent input does not coerce a preference to zero");
        f.text(name, "-5");
        f.key(Key::Enter);
        assert!((f.displayed(name) - min).abs() < 1e-4);
        f.text(name, "999");
        f.key(Key::Enter);
        assert_eq!(f.displayed(name), 100.0);
        f.text(name, "72,6%");
        f.key(Key::Enter);
        assert!((f.displayed(name) - 73.0).abs() < 1e-4);
    }
    f.only(&before, &["brushSize", "brushFeather", "brushFlow"], &domain);
}
#[test]
fn brush_parent_existing_add_erase_sdk_leaves_keep_pointer_and_keyboard_view_only() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let before = f.ui();
    f.click("button:brushErase");
    assert!(f.app.ui.brush_erase);
    f.only(&before, &["brushErase"], &domain);
    f.tab_to("button:brushAdd");
    f.key(Key::Space);
    assert!(!f.app.ui.brush_erase);
    f.only(&before, &["brushErase"], &domain);
}
#[test]
fn brush_parent_stale_drafts_and_held_toggles_do_not_replay_after_target_or_tool_change() {
    let mut f = Fixture::new();
    let initial = f.displayed("Size");
    f.text("Size", "91");
    f.trace("Size", "old mask draft");
    f.app.run("mask.select", json!({"id":f.masks[1]})).unwrap();
    f.frame(vec![]);
    let switched = f.snapshot();
    f.trace("Size", "new mask before navigation");
    // The retired editor no longer owns Return; reach its replacement naturally.
    // Immediate Return has a separate exact-path control; this checks replacement-field input.
    f.tab_to(&control("sliderField", "Size"));
    f.trace("Size", "new mask field owns Return");
    f.key(Key::Enter);
    f.trace("Size", "Return on new mask field");
    assert_eq!(f.snapshot(), switched);
    assert_eq!(f.displayed("Size"), initial);
    f.text("Size", "82");
    f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[1].0})).unwrap();
    f.app.run("mask.select", json!({"id":f.masks[2]})).unwrap();
    f.app.run("tool.brush", json!({})).unwrap();
    f.frame(vec![]);
    let switched = f.snapshot();
    f.trace("Size", "new photo before navigation");
    f.tab_to(&control("sliderField", "Size"));
    f.trace("Size", "new photo field owns Return");
    f.key(Key::Enter);
    f.trace("Size", "Return on new photo field");
    assert_eq!(f.snapshot(), switched);
    assert_eq!(f.displayed("Size"), initial);
    let at = f.rect(AUTO).center();
    f.pointer(at, true);
    f.app.run("mask.select", json!({"id":f.masks[2]})).unwrap(); // revision alone stays stable; explicit tool exit retires the parent.
    f.app.run("tool.none", json!({})).unwrap();
    f.frame(vec![]);
    assert!(!f.rects.keys().any(|key| owned_alias(key)));
    f.pointer(at, false);
    f.app.run("tool.brush", json!({})).unwrap();
    f.frame(vec![]);
    assert!(!f.app.ui.brush_auto_mask, "an unmounted press does not activate on a later visit");
    f.text("Flow", "12");
    f.app.run("tool.none", json!({})).unwrap();
    f.frame(vec![]);
    f.app.run("tool.brush", json!({})).unwrap();
    f.frame(vec![]);
    let reopened = f.snapshot();
    f.trace("Flow", "reopened tool before navigation");
    f.tab_to(&control("sliderField", "Flow"));
    f.trace("Flow", "reopened field owns Return");
    f.key(Key::Enter);
    f.trace("Flow", "Return on reopened field");
    assert_eq!(f.snapshot(), reopened, "hidden drafts cannot revive on re-entry");
}
#[test]
fn brush_parent_disabled_measurement_and_sizing_cannot_edit_or_replay_preferences() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new();
        let before = f.snapshot();
        let mut targets = vec![f.rect(AUTO).center(), f.rect(PRESSURE).center()];
        for name in ["Size", "Feather", "Flow"] {
            targets.push(f.rect(&control("slider", name)).center());
            targets.push(f.rect(&control("sliderField", name)).center());
        }
        f.probe = probe;
        for at in targets {
            f.pointer(at, true);
            f.frame(vec![Event::Text("99".into())]);
            f.key(Key::Space);
            f.pointer(at, false);
            assert_eq!(f.snapshot(), before, "{probe:?} preserves the complete app domain and all preferences");
        }
        if matches!(probe, Probe::Measure | Probe::Sizing) {
            assert!(!f.rects.keys().any(|key| owned_alias(key)));
        }
        f.probe = Probe::Real;
        f.frame(vec![]);
        f.frame(vec![]);
        assert_eq!(f.snapshot(), before, "quiet input cannot replay on return");
    }
}

#[test]
fn brush_parent_immediate_return_after_mask_switch_observes_fallback_owner_and_no_numeric_replay() {
    let mut f = Fixture::new();
    let size = f.displayed("Size");
    f.text("Size", "91");
    f.trace("Size", "immediate path old draft owner");
    f.app.run("mask.select", json!({"id":f.masks[1]})).unwrap();
    f.frame(vec![]);
    let domain = f.domain();
    let before = f.ui();
    assert_eq!(f.displayed("Size"), size);
    let erase_owner = f.hits.get("button:brushErase").unwrap().id;
    let focused_owner = craft::ui::runtime::focus::focused_rec(&f.ctx).map(|record| record.stop.id);
    f.trace("Size", "immediate Return focused owner before key");
    println!(
        "BRUSH_IMMEDIATE_RETURN_OWNER {}",
        json!({"expectedEraseOwner":format!("{erase_owner:?}"),"observedFocus":focused_owner.map(|id|format!("{id:?}"))})
    );
    // No Tab, focus setter, click or replacement-field acquisition occurs on this exact path. The framework no
    // longer hands the keyboard to a neighbouring control when the focused field goes away: nothing is focused
    // (or, on an older base, the Erase button). Either way, the retired field is not the owner.
    assert!(focused_owner.is_none() || focused_owner == Some(erase_owner), "observe the owner before Return: {focused_owner:?}");
    assert!(!f.app.ui.brush_erase);
    f.key(Key::Enter);
    f.trace("Size", "immediate Return result");
    assert_eq!(f.displayed("Size"), size, "the retired numeric draft never replays");
    if focused_owner == Some(erase_owner) {
        assert!(f.app.ui.brush_erase, "Return activates the observed Erase owner");
        f.only(&before, &["brushErase"], &domain);
    } else {
        assert!(!f.app.ui.brush_erase, "with no owner, Return acts on nothing");
        assert_eq!(f.domain(), domain, "no document, Undo or selection change");
        assert_eq!(f.ui(), before, "no preference change");
    }
}
