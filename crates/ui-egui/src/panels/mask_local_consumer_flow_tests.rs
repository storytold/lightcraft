//! Actual Masking parent consumer checks. The modifier comparison also runs against an unchanged native parent.
use super::LightcraftApp;
use crate::{Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn drain(mut out: egui::FullOutput) {
    out.textures_delta.clear();
    out.platform_output.commands.clear();
    out.viewport_output.clear();
    out.shapes.clear();
}
#[derive(Clone, Copy)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 2],
    target: u32,
    neighbor: u32,
    tick: u32,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    probe: Probe,
}
impl Fixture {
    fn new() -> Self {
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let visible = app.session.visible_cloned();
        let photos = [visible[0], visible[1]];
        app.run("library.select", json!({"ids":[photos[1].0],"active":photos[1].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Other photo mask"})).unwrap();
        app.run("mask.adjust", json!({"values":{"exposure":1.25,"contrast":-9}})).unwrap();
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Neighbor mask"})).unwrap();
        let neighbor = app.session.active_mask.unwrap();
        app.run("mask.adjust", json!({"values":{"exposure":-0.7,"contrast":17}})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Native local target"})).unwrap();
        let target = app.session.active_mask.unwrap();
        app.run("mask.adjust", json!({"values":{"exposure":0.01,"contrast":3,"clarity":4}})).unwrap();
        app.run("panel.masking", json!({})).unwrap();
        app.ui.tool.clear();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, photos, target, neighbor, tick: 0, rects: BTreeMap::new(), hits: BTreeMap::new(), probe: Probe::Real };
        let before = f.domain();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.domain(), before, "passive installed Masking parent is domain-quiet");
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(!self.app.session.segmenter.analyzing() && !self.app.session.segmenter.busy());
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. }
                | Event::PointerButton { modifiers, .. }
                | Event::ModifiersChanged(modifiers)
                | Event::MouseWheel { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 2800.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let mut rects = (vec![], vec![]);
        let probe = self.probe;
        let ctx = self.ctx.clone();
        let out = self.ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(360.0);
                let mut draw = |ui: &mut egui::Ui| {
                    if app.ui.right == crate::state::RightPanel::Masking
                        && let Some(photo) = app.session.active()
                    {
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
            rects = if root.ctx().will_discard() { (vec![], vec![]) } else { now };
        });
        self.rects = rects.0.into_iter().collect();
        self.hits = rects.1.into_iter().collect();
        drain(out);
    }
    fn rect(&self, prefix: &str, name: &str) -> Rect {
        *self.rects.get(&format!("{prefix}:{name}")).unwrap()
    }
    fn hover(&mut self, name: &str) {
        self.frame(vec![Event::PointerMoved(self.rect("slider", name).center())]);
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn settings(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.app.session.active().unwrap()).unwrap()).clone()
    }
    fn value(&self, name: &str) -> f64 {
        let settings = self.settings();
        let mask = settings.masks.iter().find(|mask| mask.id == self.target).unwrap();
        if name == "refine" { mask.refine } else { super::local_get(&mask.adjust, name) }
    }
    fn tab(&mut self, prefix: &str, name: &str) {
        let id = self.hits[&format!("{prefix}:{name}")].id;
        self.frame(vec![Event::PointerGone]);
        for _ in 0..128 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|record| record.stop.id == id) {
                return;
            }
            self.key(Key::Tab, Modifiers::NONE);
        }
        panic!("natural Tab never reached actual {prefix}:{name}");
    }
    fn text(&mut self, name: &str, value: &str) {
        let at = self.rect("sliderValue", name).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
        self.key(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![Event::Text(value.to_owned())]);
        assert!(self.rects.contains_key(&format!("sliderField:{name}")), "the real SDK numeric editor is open");
        assert_eq!(
            craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| record.stop.id),
            Some(self.hits[&format!("sliderField:{name}")].id)
        );
    }
    fn photos(&self) -> Value {
        json!(self.photos.iter().map(|id| self.app.session.catalog.photo(*id).unwrap()).collect::<Vec<_>>())
    }
    fn undo(&self) -> Value {
        json!(self.app.session.undo.iter().map(|entry| json!({"label":entry.label,"op":entry.op})).collect::<Vec<_>>())
    }
    fn domain(&self) -> Value {
        json!({"photos":self.photos(),"revision":self.app.session.catalog.revision,"active":self.app.session.active(),"selected":self.app.session.selection.ids,
            "mask":self.app.session.active_mask,"journal":self.app.session.journal,"undo":self.undo(),
            "redo":self.app.session.redo.iter().map(|entry|json!({"label":entry.label,"op":entry.op})).collect::<Vec<_>>(),"interaction":self.app.session.interaction.is_some()})
    }
    fn trace(&self, phase: &str) {
        let sdk_focus = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| format!("{:?}", record.stop.id));
        let focus = self.ctx.memory(|memory| memory.focused());
        let focus_rect = focus
            .and_then(|id| self.ctx.read_response(id))
            .map(|response| [response.rect.min.x, response.rect.min.y, response.rect.max.x, response.rect.max.y]);
        println!(
            "MASK_LOCAL_CONSUMER_TRACE {}",
            json!({"phase":phase,"exposure":self.value("exposure"),"contrast":self.value("contrast"),"amount":self.value("amount"),"refine":self.value("refine"),
            "sdkFocus":sdk_focus,"focus":focus.map(|id|format!("{id:?}")),"focusRect":focus_rect,"pointer":self.ctx.input(|input|json!({"down":input.pointer.primary_down(),"position":input.pointer.interact_pos().map(|at|[at.x,at.y])})),
            "undo":self.app.session.undo.len(),"redo":self.app.session.redo.len(),"interaction":self.app.session.interaction.is_some(),"overlay":self.app.ui.mask_overlay})
        );
    }
    fn isolated(&self, before: &DevelopSettings, other: &Value, changes: &[(&str, f64)]) {
        let mut expected = serde_json::to_value(before).unwrap();
        let index = before.masks.iter().position(|mask| mask.id == self.target).unwrap();
        for (name, value) in changes {
            if *name == "refine" {
                expected["masks"][index]["refine"] = json!(value);
            } else {
                expected["masks"][index]["adjust"][*name] = json!(value);
            }
        }
        assert_eq!(
            serde_json::to_value(self.settings()).unwrap(),
            expected,
            "only the specified target mask field changes, never another mask or global develop field"
        );
        assert_eq!(json!(self.app.session.catalog.photo(self.photos[1]).unwrap()), *other, "untouched complete neighbor photo/history");
        assert_eq!(self.app.session.active(), Some(self.photos[0]));
        assert_eq!(self.app.session.active_mask, Some(self.target));
    }
    fn roundtrip(&mut self, before_photos: &Value, before_undo: &Value, after_photos: &Value) {
        let committed_undo = self.undo();
        self.app.run("edit.undo", json!({})).unwrap();
        self.frame(vec![]);
        assert_eq!(self.photos(), *before_photos, "Undo restores complete photo values, timestamps and history");
        assert_eq!(self.undo(), *before_undo, "Undo restores every prior label and operation");
        assert_eq!(self.app.session.redo.len(), 1);
        self.app.run("edit.redo", json!({})).unwrap();
        self.frame(vec![]);
        assert_eq!(self.photos(), *after_photos, "Redo restores the complete committed photo/history records");
        assert_eq!(self.undo(), committed_undo, "Redo restores exact Undo labels and operations");
        assert!(self.app.session.redo.is_empty() && self.app.session.interaction.is_none());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 2800.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

fn before(f: &Fixture) -> (DevelopSettings, Value, Value, Value) {
    (f.settings(), f.photos(), f.undo(), json!(f.app.session.catalog.photo(f.photos[1]).unwrap()))
}
#[test]
fn local_parent_all_nineteen_rows_keep_specs_target_units_shift_five_and_exact_undo_redo() {
    let mut specs = super::LOCAL.iter().filter(|spec| spec.id != "lens_blur").copied().collect::<Vec<_>>();
    specs.push(lightcraft_develop::ControlSpec {
        id: "amount",
        label: "Amount",
        section: lightcraft_develop::Section::Light,
        min: 0.0,
        max: 200.0,
        default: 100.0,
        step: 1.0,
        decimals: 0,
        track: lightcraft_develop::Track::Plain,
    });
    specs.push(lightcraft_develop::ControlSpec {
        id: "refine",
        label: "Refine Edges",
        section: lightcraft_develop::Section::Light,
        min: 0.0,
        max: 100.0,
        default: 0.0,
        step: 1.0,
        decimals: 0,
        track: lightcraft_develop::Track::Plain,
    });
    assert_eq!(specs.len(), 19);
    for spec in specs {
        let mut f = Fixture::new();
        f.app.run("view.maskOverlay", json!({"show":true})).unwrap();
        f.frame(vec![]);
        let (settings, photos, undo, other) = before(&f);
        let old = f.value(spec.id);
        f.hover(spec.id);
        f.key(Key::ArrowUp, Modifiers::SHIFT);
        let expected = crate::widgets::nudged(&spec, old, 5.0);
        f.isolated(&settings, &other, &[(spec.id, expected)]);
        assert_eq!(f.app.ui.mask_overlay, spec.id == "refine");
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
#[test]
fn local_parent_exposure_hover_regular_and_shift_are_001_to_005_and_021() {
    for (mods, expected) in [(Modifiers::NONE, 0.05), (Modifiers::SHIFT, 0.21)] {
        let mut f = Fixture::new();
        let (settings, photos, undo, other) = before(&f);
        f.hover("exposure");
        f.key(Key::ArrowUp, mods);
        f.trace("SDK whole row key");
        f.isolated(&settings, &other, &[("exposure", expected)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
#[test]
fn local_parent_real_slider_focus_and_other_hover_keep_first_eligible_draw_order() {
    for (focused, hovered) in [("exposure", "contrast"), ("contrast", "exposure")] {
        let mut f = Fixture::new();
        f.tab("slider", focused);
        f.hover(hovered);
        let (settings, photos, undo, other) = before(&f);
        f.trace("SDK actual focus versus hover before key");
        f.key(Key::ArrowUp, Modifiers::NONE);
        f.isolated(&settings, &other, &[("exposure", 0.05)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
#[test]
fn local_parent_typed_comma_sign_empty_escape_clamp_and_noop_are_commit_only() {
    let mut f = Fixture::new();
    let (settings, photos, undo, other) = before(&f);
    let domain = f.domain();
    f.text("exposure", "+0,37");
    assert_eq!(f.domain(), domain);
    f.key(Key::Enter, Modifiers::NONE);
    f.isolated(&settings, &other, &[("exposure", 0.37)]);
    assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
    let after = f.photos();
    f.roundtrip(&photos, &undo, &after);
    let domain = f.domain();
    f.text("exposure", "-1,23");
    assert_eq!(f.domain(), domain);
    f.key(Key::Escape, Modifiers::NONE);
    assert_eq!(f.domain(), domain);
    f.text("exposure", "");
    f.key(Key::Backspace, Modifiers::NONE);
    assert_eq!(f.domain(), domain);
    f.key(Key::Enter, Modifiers::NONE);
    assert_eq!(f.domain(), domain, "empty is not zero and no-op has no journal");
    f.text("exposure", "+0,37");
    f.key(Key::Enter, Modifiers::NONE);
    assert_eq!(f.domain(), domain, "same value never creates Undo or journal");
    let (settings, photos, undo, other) = before(&f);
    f.text("amount", "999");
    f.key(Key::Enter, Modifiers::NONE);
    f.isolated(&settings, &other, &[("amount", 200.0)]);
    let after = f.photos();
    f.roundtrip(&photos, &undo, &after);
}
#[test]
fn local_parent_editor_keys_never_leak_to_another_hovered_scalar_unlike_native() {
    for (editor, hovered) in [("exposure", "contrast"), ("contrast", "exposure")] {
        let mut f = Fixture::new();
        let (settings, photos, undo, other) = before(&f);
        f.text(editor, if editor == "exposure" { "0.37" } else { "31" });
        f.hover(hovered);
        f.trace("SDK editor conflict before Up");
        f.key(Key::ArrowUp, Modifiers::NONE);
        f.trace("SDK editor conflict after Up");
        // The canonical numeric editor may step its own proposal; it must never nudge the other row.
        // This intentionally does NOT assert the disproved native editor-consumes-Up prediction.
        let own = f.value(editor);
        f.isolated(&settings, &other, &[(editor, own)]);
        assert_eq!(f.value(hovered), super::local_get(&settings.masks.iter().find(|mask| mask.id == f.target).unwrap().adjust, hovered));
        assert!(f.app.session.undo.len() <= undo.as_array().unwrap().len() + 1, "only the editor may create one step");
        if f.app.session.undo.len() > undo.as_array().unwrap().len() {
            let after = f.photos();
            f.roundtrip(&photos, &undo, &after);
        }
        f.key(Key::Escape, Modifiers::NONE);
    }
}
#[test]
fn local_parent_capture_blocks_other_hover_keys_in_both_draw_orders_unlike_native() {
    for (drag, hovered) in [("exposure", "contrast"), ("contrast", "exposure")] {
        let mut f = Fixture::new();
        let (settings, photos, undo, other) = before(&f);
        let spec = super::LOCAL.iter().find(|spec| spec.id == drag).unwrap();
        let rect = f.rect("slider", drag);
        let other_rect = f.rect("slider", hovered);
        let fraction = ((f.value(drag) - spec.min) / (spec.max - spec.min)) as f32;
        let start = egui::pos2(egui::lerp(rect.x_range(), fraction), rect.center().y);
        f.frame(vec![Event::PointerMoved(start)]);
        f.pointer(start, true);
        f.frame(vec![Event::PointerMoved(egui::pos2(start.x + rect.width() * 0.03, start.y))]);
        let end = egui::pos2(start.x + rect.width() * 0.06, other_rect.center().y);
        f.frame(vec![
            Event::PointerMoved(end),
            Event::Key { key: Key::ArrowUp, physical_key: Some(Key::ArrowUp), pressed: true, repeat: false, modifiers: Modifiers::NONE },
        ]);
        f.trace("SDK captured row plus other hover Up");
        let value = f.value(drag);
        assert!(value > super::local_get(&settings.masks.iter().find(|mask| mask.id == f.target).unwrap().adjust, drag));
        f.isolated(&settings, &other, &[(drag, value)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len());
        f.frame(vec![Event::Key { key: Key::ArrowUp, physical_key: Some(Key::ArrowUp), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        f.pointer(end, false);
        f.frame(vec![]);
        f.isolated(&settings, &other, &[(drag, value)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
#[test]
fn local_parent_stale_mask_photo_revision_and_hide_drafts_never_replay_on_replacement_fields() {
    for change in ["mask", "photo", "revision", "hide"] {
        let mut f = Fixture::new();
        f.text("exposure", "0.91");
        let old = f.hits["sliderField:exposure"].id;
        match change {
            "mask" => {
                f.app.run("mask.select", json!({"id":f.neighbor})).unwrap();
                f.target = f.neighbor;
            }
            "photo" => {
                f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
                f.app.run("mask.select", json!({"id":1})).unwrap();
                f.target = 1;
            }
            "revision" => {
                f.app.run("mask.adjust", json!({"values":{"contrast":27}})).unwrap();
            }
            _ => {
                f.app.run("panel.edit", json!({})).unwrap();
                f.frame(vec![]);
                f.app.run("panel.masking", json!({})).unwrap();
            }
        }
        let domain = f.domain();
        f.frame(vec![]);
        assert_eq!(f.domain(), domain);
        assert_ne!(f.hits["sliderField:exposure"].id, old, "owner/revision/hidden epoch retires old SDK field identity");
        // Deliberately reacquire the real replacement field; no assumption about fallback Return ownership.
        f.tab("sliderField", "exposure");
        f.key(Key::Enter, Modifiers::NONE);
        assert_eq!(f.domain(), domain, "no old typed proposal reaches the new owner");
    }
}
#[test]
fn local_parent_disabled_measure_sizing_and_reopen_do_not_alias_mutate_or_replay() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new();
        f.hover("exposure");
        let domain = f.domain();
        f.probe = probe;
        f.key(Key::ArrowUp, Modifiers::SHIFT);
        assert_eq!(f.domain(), domain);
        if !matches!(probe, Probe::Disabled) {
            assert!(!f.rects.keys().any(|name| name.contains("maskLocal")));
        }
        f.probe = Probe::Real;
        f.frame(vec![]);
        assert_eq!(f.domain(), domain, "speculative keys are not delayed real edits");
        let (settings, photos, undo, other) = before(&f);
        f.hover("exposure");
        f.key(Key::ArrowUp, Modifiers::NONE);
        f.isolated(&settings, &other, &[("exposure", 0.05)]);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
#[test]
fn local_parent_pointer_modifier_comparison_records_fresh_thumb_travel_and_exact_undo_redo() {
    let mut landed = vec![];
    let mut ported = false;
    for (name, mods) in [("plain", Modifiers::NONE), ("shift", Modifiers::SHIFT), ("alt", Modifiers::ALT)] {
        let mut f = Fixture::new();
        let (settings, photos, undo, other) = before(&f);
        assert_eq!(f.value("exposure"), 0.01);
        let rect = f.rect("slider", "exposure");
        ported = f.rects.contains_key("sliderRow:maskLocal:exposure");
        let start = egui::pos2(egui::lerp(rect.x_range(), 0.50125), rect.center().y);
        let normalized = 0.02;
        let end = egui::pos2(start.x + rect.width() * normalized, start.y);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(start)]);
        f.frame(vec![
            Event::PointerMoved(start),
            Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: mods },
        ]);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(egui::pos2((start.x + end.x) * 0.5, start.y))]);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(end)]);
        let held = f.value("exposure");
        assert!(held > 0.01 && held < 3.0, "the comparison must not clip at bounds");
        assert!(f.app.session.interaction.is_some());
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len());
        f.frame(vec![Event::PointerMoved(end), Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: mods }]);
        f.frame(vec![]);
        let value = f.value("exposure");
        f.isolated(&settings, &other, &[("exposure", value)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        println!(
            "MASK_LOCAL_MODIFIER {}",
            json!({"implementation":if ported{"sdk"}else{"native"},"modifier":name,"initial":0.01,"trackWidth":rect.width(),"travel":end.x-start.x,"normalizedTravel":(end.x-start.x)/rect.width(),"held":held,"landed":value,"undoSteps":1})
        );
        landed.push(value - 0.01);
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
    // The SDK slider is bound with Shift as the fine modifier and none coarse (see `mask_local_scalar`), which is
    // what the native slider did: Shift travels slowly, Alt changes nothing. Both use fresh identical values.
    let _ = ported;
    assert!(landed[1] < landed[0] * 0.5 && (landed[2] - landed[0]).abs() < 0.011);
}

#[test]
fn local_parent_caption_uses_the_real_composite_row_for_hover_key_ownership() {
    let mut f = Fixture::new();
    let (settings, photos, undo, other) = before(&f);
    let row = f.rects["sliderRow:maskLocal:exposure"];
    let at = egui::pos2(row.left() + 3.0, row.top() + 3.0);
    assert!(!f.rect("slider", "exposure").contains(at));
    f.frame(vec![Event::PointerMoved(at)]);
    f.key(Key::ArrowUp, Modifiers::NONE);
    f.isolated(&settings, &other, &[("exposure", 0.05)]);
    let after = f.photos();
    f.roundtrip(&photos, &undo, &after);
}
fn wheel(f: &mut Fixture, delta: egui::Vec2, mods: Modifiers) {
    f.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers: mods,
        source: egui::MouseWheelSource::Wheel,
    }]);
    f.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::Vec2::ZERO,
        phase: egui::TouchPhase::End,
        modifiers: mods,
        source: egui::MouseWheelSource::Wheel,
    }]);
    for _ in 0..20 {
        f.frame(vec![]);
    }
}
#[test]
fn local_parent_wheel_direction_hover_and_arming_keep_one_intended_row() {
    for direction in ["vertical-unarmed", "horizontal", "shift-horizontal", "vertical-armed"] {
        let mut f = Fixture::new();
        f.hover("exposure");
        if direction == "vertical-armed" {
            let rect = f.rect("slider", "exposure");
            let at = egui::pos2(egui::lerp(rect.x_range(), 0.50125), rect.center().y);
            let quiet = f.domain();
            f.pointer(at, true);
            f.pointer(at, false);
            f.frame(vec![]);
            assert_eq!(f.domain(), quiet);
        }
        let (settings, photos, undo, other) = before(&f);
        let domain = f.domain();
        let (delta, mods) = match direction {
            "horizontal" => (egui::vec2(80.0, 0.0), Modifiers::NONE),
            "shift-horizontal" => (egui::vec2(0.0, 80.0), Modifiers::SHIFT),
            _ => (egui::vec2(0.0, 80.0), Modifiers::NONE),
        };
        wheel(&mut f, delta, mods);
        f.trace(direction);
        if direction == "vertical-unarmed" {
            assert_eq!(f.domain(), domain, "ordinary vertical panel scroll is not a scalar edit");
        } else {
            let value = f.value("exposure");
            assert!(value > 0.01 && value < 4.0);
            f.isolated(&settings, &other, &[("exposure", value)]);
            assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
            let after = f.photos();
            f.roundtrip(&photos, &undo, &after);
        }
    }
}
#[test]
fn local_parent_held_escape_restores_complete_photos_and_release_cannot_replay() {
    let mut f = Fixture::new();
    let (settings, photos, undo, other) = before(&f);
    let rect = f.rect("slider", "exposure");
    let start = egui::pos2(egui::lerp(rect.x_range(), 0.50125), rect.center().y);
    f.frame(vec![Event::PointerMoved(start)]);
    f.pointer(start, true);
    let end = egui::pos2(start.x + rect.width() * 0.05, start.y);
    f.frame(vec![Event::PointerMoved(egui::pos2((start.x + end.x) * 0.5, start.y))]);
    f.frame(vec![Event::PointerMoved(end)]);
    assert!(f.value("exposure") > 0.01 && f.app.session.interaction.is_some());
    f.key(Key::Escape, Modifiers::NONE);
    f.trace("held Escape");
    f.isolated(&settings, &other, &[]);
    assert_eq!(f.photos(), photos);
    assert_eq!(f.undo(), undo);
    assert!(f.app.session.interaction.is_none());
    let cancelled = f.domain();
    f.pointer(end, false);
    f.frame(vec![]);
    assert_eq!(f.domain(), cancelled, "physical release after Cancel is not a new edit");
}

#[test]
fn local_parent_pointer_modifier_parity_matches_native_held_landed_and_exact_undo_redo() {
    for (name, mods) in [("plain", Modifiers::NONE), ("shift", Modifiers::SHIFT), ("alt", Modifiers::ALT)] {
        let mut f = Fixture::new();
        let (settings, photos, undo, other) = before(&f);
        assert_eq!(f.value("exposure"), 0.01);
        let rect = f.rect("slider", "exposure");
        let ported = f.rects.contains_key("sliderRow:maskLocal:exposure");
        let start = egui::pos2(egui::lerp(rect.x_range(), 0.50125), rect.center().y);
        let normalized = 0.02;
        let end = egui::pos2(start.x + rect.width() * normalized, start.y);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(start)]);
        f.frame(vec![
            Event::PointerMoved(start),
            Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: mods },
        ]);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(egui::pos2((start.x + end.x) * 0.5, start.y))]);
        f.frame(vec![Event::ModifiersChanged(mods), Event::PointerMoved(end)]);
        let held = f.value("exposure");
        assert!(held > 0.01 && held < 3.0, "the comparison must not clip at bounds");
        assert!(f.app.session.interaction.is_some());
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len());
        f.frame(vec![Event::PointerMoved(end), Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: mods }]);
        f.frame(vec![]);
        let value = f.value("exposure");
        f.isolated(&settings, &other, &[("exposure", value)]);
        assert_eq!(f.app.session.undo.len(), undo.as_array().unwrap().len() + 1);
        println!(
            "MASK_LOCAL_MODIFIER {}",
            json!({"implementation":if ported{"sdk"}else{"native"},"modifier":name,"initial":0.01,"trackWidth":rect.width(),"travel":end.x-start.x,"normalizedTravel":(end.x-start.x)/rect.width(),"held":held,"landed":value,"undoSteps":1})
        );
        let native = if name == "shift" { 0.03 } else { 0.17 };
        assert_eq!(held, native, "native held value for {name}");
        assert_eq!(value, native, "native landed value for {name}");
        let after = f.photos();
        f.roundtrip(&photos, &undo, &after);
    }
}
