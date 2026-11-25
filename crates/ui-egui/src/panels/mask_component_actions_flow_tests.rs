//! Actual Masking parent/opener/menu coverage in a framework-installed egui context.
//! No app logic, photo rendering, AI masks/models, OS UI or filesystem services are started.
use std::collections::BTreeMap;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{Mask, MaskOp};
use serde_json::{Value, json};

use crate::headless::HeadlessView;
use crate::{LightcraftApp, Services};

#[derive(Clone, Copy)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
#[derive(Clone, Copy, Debug)]
enum Opener {
    Context,
    Options,
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
    other_mask: u32,
    target: u32,
    native: bool,
    probe: Probe,
    tick: u32,
    journal_start: usize,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
}
impl Fixture {
    fn new(count: usize, native: bool) -> Self {
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let ids: Vec<_> = app.session.visible_cloned().into_iter().take(2).collect();
        let photos = [ids[0], ids[1]];
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Untouched mask"})).unwrap();
        let other_mask = app.session.active_mask.unwrap();
        app.run("mask.add", json!({"kind":"radial","name":"Target mask","rx":0.21})).unwrap();
        let target = app.session.active_mask.unwrap();
        app.run("mask.component", json!({"id":target,"component":0,"action":"rename","name":"Repeated label"})).unwrap();
        for i in 1..count {
            app.run("mask.addComponent", json!({"id":target,"op":"subtract","kind":"linear","start":[0.1+i as f64*0.1,0.2],"end":[0.8,0.7]}))
                .unwrap();
            app.run("mask.component", json!({"id":target,"component":i,"action":"rename","name":"Repeated label"})).unwrap();
        }
        // A second actual photo has its own non-AI mask, including a colliding mask ID.
        app.run("library.select", json!({"ids":[photos[1].0],"active":photos[1].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Other photo mask"})).unwrap();
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.select", json!({"id":target})).unwrap();
        app.run("panel.masking", json!({})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let journal_start = app.session.journal.len();
        let mut f = Self {
            app,
            ctx,
            photos,
            other_mask,
            target,
            native,
            probe: Probe::Real,
            tick: 0,
            journal_start,
            rects: BTreeMap::new(),
            hits: BTreeMap::new(),
        };
        let before = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before, "input-free installed parent must not edit; native={native}");
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(!self.app.session.segmenter.analyzing() && !self.app.session.segmenter.busy());
        let mods = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(mods));
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1400.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let probe = self.probe;
        let mut published = (vec![], vec![]);
        let out = self.ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(360.0);
                let ctx = ui.ctx().clone();
                let mut draw = |ui: &mut egui::Ui| {
                    if let Some(id) = app.session.active() {
                        super::show(app, ui, id);
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
            published = if root.ctx().will_discard() { (vec![], vec![]) } else { now };
        });
        drain(out);
        self.rects = published.0.into_iter().collect();
        self.hits = published.1.into_iter().collect();
    }
    fn rect(&self, key: &str) -> Rect {
        *self.rects.get(key).unwrap_or_else(|| panic!("missing {key}: {:?}", self.rects.keys()))
    }
    fn has(&self, key: &str) -> bool {
        self.rects.contains_key(key)
    }
    fn action(&self, k: usize, key: &str) -> String {
        format!("maskComponent:{}:{k}:{key}", self.target)
    }
    fn pointer(&mut self, at: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }]);
    }
    fn click_button(&mut self, key: &str, button: PointerButton) {
        let at = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, button, true);
        self.pointer(at, button, false);
        self.frame(vec![]);
    }
    fn click(&mut self, key: &str) {
        self.click_button(key, PointerButton::Primary);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.frame(vec![]);
    }
    fn open(&mut self, k: usize, opener: Opener) {
        self.open_phase(k, opener, "ordinary opener");
    }
    fn open_phase(&mut self, k: usize, opener: Opener, phase: &str) {
        let (key, button) = match opener {
            Opener::Context => (format!("component:{k}"), PointerButton::Secondary),
            Opener::Options => (format!("button:componentMenu{k}"), PointerButton::Primary),
        };
        let at = self.rect(&key).center();
        let observe = |f: &Self| f.ctx.data(|d| d.get_temp::<String>(egui::Id::unique(("mask-component-opener-diagnostic", f.target, k))));
        let mut receipts = vec![("before input", observe(self))];
        // Identical four real input passes as click_button: no extra settle/retry/focus.
        self.frame(vec![Event::PointerMoved(at)]);
        receipts.push(("pointer moved", observe(self)));
        self.pointer(at, button, true);
        receipts.push(("button down", observe(self)));
        self.pointer(at, button, false);
        receipts.push(("button up", observe(self)));
        self.frame(vec![]);
        receipts.push(("existing idle pass", observe(self)));
        assert!(
            self.has(&self.action(k, "invert")),
            "actual parent failed to open {opener:?}, native={}, phase={phase}, active={:?}, mask={:?}, expected_component={k}, actual_component_registry={}, receipts={receipts:#?}",
            self.native,
            self.app.session.active(),
            self.app.session.active_mask,
            self.has(&format!("component:{k}"))
        );
    }
    fn navigate(&mut self, key: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..16 {
            let id = self.hits.get(key).unwrap().id;
            if self.ctx.memory(|m| m.focused()) == Some(id) || craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == id) {
                return;
            }
            self.key(Key::ArrowDown);
        }
        panic!("natural arrow movement did not reach {key}");
    }
    fn masks(&self) -> Vec<Mask> {
        self.app.session.develop_of(self.photos[0]).unwrap().masks.clone()
    }
    fn photo(&self, id: PhotoId) -> Value {
        serde_json::to_value(self.app.session.catalog.photo(id).unwrap()).unwrap()
    }
    fn documents(&self) -> Value {
        json!([self.photo(self.photos[0]), self.photo(self.photos[1])])
    }
    fn commands(&self) -> &[(String, Value)] {
        self.app.session.journal.get(self.journal_start..).unwrap()
    }
    fn snapshot(&self) -> Value {
        json!({"documents":self.documents(),"active":self.app.session.active(),"selected":self.app.session.selection.ids,
            "activeMask":self.app.session.active_mask,"undo":self.app.session.undo.len(),"redo":self.app.session.redo.len(),
            "journal":self.commands(),"rename":self.app.ui.renaming_component})
    }
    fn undo_exact(&mut self, before: Value, undo: usize) {
        self.app.run("edit.undo", json!({})).unwrap();
        self.frame(vec![]);
        assert_eq!(self.documents(), before, "Undo restores both full actual photos");
        assert_eq!(self.app.session.undo.len(), undo);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1400.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn component_parent_invert_both_openers_targets_one_component_and_undo() {
    for opener in [Opener::Context, Opener::Options] {
        let mut f = Fixture::new(2, false);
        let before = f.documents();
        let undo = f.app.session.undo.len();
        let masks = f.masks();
        let other = f.photo(f.photos[1]);
        f.open(1, opener);
        f.click(&f.action(1, "invert"));
        let mut expected = masks.clone();
        expected.iter_mut().find(|m| m.id == f.target).unwrap().components[1].invert = true;
        assert_eq!(f.masks(), expected);
        assert_eq!(f.photo(f.photos[1]), other);
        assert_eq!(f.commands(), &[("mask.component".into(), json!({"id":f.target,"component":1,"action":"invert"}))]);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.active_mask, Some(f.target));
        assert!(!f.has(&f.action(1, "invert")), "closed menu cannot retain a live action target");
        f.undo_exact(before, undo);
    }
}

#[test]
fn component_parent_duplicate_keeps_exact_order_and_delete_only_selected_component() {
    for action in ["duplicate", "delete"] {
        let mut f = Fixture::new(3, false);
        let before = f.documents();
        let undo = f.app.session.undo.len();
        let mut expected = f.masks();
        let target = expected.iter_mut().find(|m| m.id == f.target).unwrap();
        if action == "duplicate" {
            let copy = target.components[1].clone();
            target.components.insert(2, copy);
        } else {
            target.components.remove(1);
        }
        f.open(1, Opener::Options);
        f.click(&f.action(1, action));
        assert_eq!(f.masks(), expected, "repeated names cannot retarget command indices");
        assert_eq!(f.commands(), &[("mask.component".into(), json!({"id":f.target,"component":1,"action":action}))]);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.undo_exact(before, undo);
    }
}

#[test]
fn component_parent_last_component_delete_removes_only_its_mask_and_undo() {
    let mut f = Fixture::new(1, false);
    let before = f.documents();
    let undo = f.app.session.undo.len();
    let expected: Vec<_> = f.masks().into_iter().filter(|m| m.id != f.target).collect();
    f.open(0, Opener::Context);
    f.click(&f.action(0, "delete"));
    assert_eq!(f.masks(), expected);
    assert_eq!(f.app.session.active_mask, Some(f.other_mask));
    assert_eq!(f.commands(), &[("mask.component".into(), json!({"id":f.target,"component":0,"action":"delete"}))]);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.undo_exact(before, undo);
}

#[test]
fn component_parent_mode_is_absent_for_first_and_edits_each_exact_later_mode() {
    let mut first = Fixture::new(2, false);
    let before = first.snapshot();
    first.open(0, Opener::Options);
    assert!(!first.has(&first.action(0, "mode")));
    first.key(Key::Escape);
    assert_eq!(first.snapshot(), before);
    for (op, expected_op) in [("add", MaskOp::Add), ("subtract", MaskOp::Subtract), ("intersect", MaskOp::Intersect)] {
        let mut f = Fixture::new(2, false);
        let before = f.documents();
        let undo = f.app.session.undo.len();
        let mut expected = f.masks();
        expected.iter_mut().find(|m| m.id == f.target).unwrap().components[1].op = expected_op;
        f.open(1, Opener::Context);
        f.click(&f.action(1, "mode"));
        f.click(&f.action(1, op));
        assert_eq!(f.masks(), expected);
        assert_eq!(f.commands(), &[("mask.component".into(), json!({"id":f.target,"component":1,"action":"op","op":op}))]);
        // Preserve mask.component's existing command semantics: even reapplying the current
        // mode records its own command/history/Undo step. The UI must not silently rewrite it.
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.undo_exact(before, undo);
    }
}

#[test]
fn component_parent_cancel_immediate_reopen_and_natural_keyboard_activation() {
    for opener in [Opener::Context, Opener::Options] {
        for outside in [false, true] {
            let mut f = Fixture::new(2, false);
            let before = f.snapshot();
            f.open(1, opener);
            if outside {
                let at = egui::pos2(850.0, 1200.0);
                f.pointer(at, PointerButton::Primary, true);
                f.pointer(at, PointerButton::Primary, false);
                f.frame(vec![]);
            } else {
                f.key(Key::Escape);
            }
            assert_eq!(f.snapshot(), before);
            assert!(!f.has(&f.action(1, "invert")));
            f.open(1, opener);
            f.navigate(&f.action(1, "duplicate"));
            f.key(Key::Enter);
            assert_eq!(f.commands(), &[("mask.component".into(), json!({"id":f.target,"component":1,"action":"duplicate"}))]);
            assert_eq!(f.masks().iter().find(|m| m.id == f.target).unwrap().components.len(), 3);
        }
    }
}

#[test]
fn component_parent_stale_popup_release_cannot_retarget_switched_or_deleted_rows() {
    for opener in [Opener::Context, Opener::Options] {
        for change in ["photo", "mask", "component", "delete-mask"] {
            let mut f = Fixture::new(3, false);
            f.open_phase(1, opener, &format!("{change}: original open"));
            let at = f.rect(&f.action(1, "invert")).center();
            let before_press = f.snapshot();
            f.pointer(at, PointerButton::Primary, true);
            assert_eq!(f.snapshot(), before_press, "holding the old menu action is not a command");
            let (command, params) = match change {
                "photo" => ("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})),
                "mask" => ("mask.select", json!({"id":f.other_mask})),
                "component" => ("mask.component", json!({"id":f.target,"component":0,"action":"delete"})),
                _ => ("mask.delete", json!({"id":f.target})),
            };
            f.app.run(command, params.clone()).unwrap();
            assert_eq!(f.commands(), &[(command.to_owned(), params)], "only the explicit external change precedes stale input");
            let before = f.snapshot();
            let unchanged = |f: &Fixture, phase: &str| {
                // Report a precise bounded command/Undo failure before the complete document snapshot.
                assert_eq!(json!(f.commands()), before["journal"], "unexpected command after {change}/{opener:?}/{phase}");
                assert_eq!(json!(f.app.session.undo.len()), before["undo"], "unexpected Undo after {change}/{opener:?}/{phase}");
                assert_eq!(f.snapshot(), before, "changed identity/document after {change}/{opener:?}/{phase}");
            };
            f.frame(vec![]);
            unchanged(&f, "retirement frame");
            f.pointer(at, PointerButton::Primary, false);
            unchanged(&f, "old pointer release");
            eprintln!("DBGMC {change} {opener:?} before Enter: open={} obs={:?}", f.has(&f.action(1, "invert")), f.ctx.data(|d| d.get_temp::<String>(egui::Id::unique(("mask-component-opener-diagnostic", f.target, 1usize)))).map(|s| s[..60.min(s.len())].to_owned()));
            f.key(Key::Enter);
            eprintln!("DBGMC {change} {opener:?} after Enter: open={}", f.has(&f.action(1, "invert")));
            unchanged(&f, "following Enter");
            // Keep the original combined release-plus-Enter regression as well.
            assert_eq!(f.snapshot(), before, "an old captured menu release cannot act after {change}");

            // Invalidation must not leave the row slot permanently inert. Select a real live
            // target through commands and deliberately reopen through the SAME real opener.
            f.key(Key::Escape);
            eprintln!("DBGMC {change} {opener:?} after Escape: open={}", f.has(&f.action(1, "invert")));
            if f.app.session.active() != Some(f.photos[0]) {
                f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
            }
            let fresh_mask = if change == "delete-mask" { f.other_mask } else { f.target };
            f.app.run("mask.select", json!({"id":fresh_mask})).unwrap();
            f.target = fresh_mask; // Only the fixture's locator metadata, never the model under test.
            f.frame(vec![]);
            eprintln!("DBGMC {change} {opener:?} after reselect: open={} obs={:?}", f.has(&f.action(1, "invert")), f.ctx.data(|d| d.get_temp::<String>(egui::Id::unique(("mask-component-opener-diagnostic", f.target, 1usize)))).map(|s| s[s.len().saturating_sub(120)..].to_owned()));
            let mut expected = f.masks();
            let row = expected.iter_mut().find(|m| m.id == fresh_mask).unwrap();
            let k = row.components.len().saturating_sub(1).min(1);
            row.components[k].invert = !row.components[k].invert;
            let fresh_before = f.documents();
            let fresh_undo = f.app.session.undo.len();
            let fresh_journal = f.commands().len();
            f.open_phase(k, opener, &format!("{change}: fresh selected target reopen"));
            f.click(&f.action(k, "invert"));
            assert_eq!(f.masks(), expected, "fresh selected target must still act after {change}/{opener:?}");
            assert_eq!(&f.commands()[fresh_journal..], &[("mask.component".into(), json!({"id":fresh_mask,"component":k,"action":"invert"}))]);
            assert_eq!(f.app.session.undo.len(), fresh_undo + 1);
            f.undo_exact(fresh_before, fresh_undo);
        }
    }
}
#[test]
fn component_parent_disabled_measurement_and_sizing_refuse_actions_without_replay() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new(2, false);
        f.open(1, Opener::Options);
        let at = f.rect(&f.action(1, "delete")).center();
        let before = f.snapshot();
        f.probe = probe;
        f.pointer(at, PointerButton::Primary, true);
        f.pointer(at, PointerButton::Primary, false);
        assert_eq!(f.snapshot(), before);
        if matches!(probe, Probe::Measure | Probe::Sizing) {
            assert!(!f.rects.keys().any(|k| k.starts_with("maskComponent:")));
        }
        f.probe = Probe::Real;
        f.frame(vec![]);
        assert_eq!(f.snapshot(), before, "speculative input must not replay");
    }
}


/// Narrow first step: run only this selector before repeating the full eight journeys.
#[test]
fn component_parent_both_openers_binding_discriminator_and_shifted_owner_safety() {
    for opener in [Opener::Context, Opener::Options] {
        let mut f = Fixture::new(3, false);
        let initial = f.snapshot();
        f.open(1, opener);
        assert_eq!(f.snapshot(), initial, "opening is not an action: {opener:?}");
        let at = f.rect(&f.action(1, "invert")).center();
        f.pointer(at, PointerButton::Primary, true);
        f.app.run("mask.component", json!({"id":f.target,"component":0,"action":"delete"})).unwrap();
        let before = f.snapshot();
        f.frame(vec![]);
        assert_eq!(json!(f.commands()), before["journal"], "retirement: {opener:?}");
        assert_eq!(f.snapshot(), before);
        f.pointer(at, PointerButton::Primary, false);
        assert_eq!(json!(f.commands()), before["journal"], "release: {opener:?}");
        assert_eq!(f.snapshot(), before);
        f.key(Key::Enter);
        assert_eq!(json!(f.commands()), before["journal"], "following Enter: {opener:?}");
        assert_eq!(f.snapshot(), before, "no shifted component, history or Undo mutation");
        f.key(Key::Escape);
        f.app.run("mask.select", json!({"id":f.target})).unwrap();
        f.frame(vec![]);
        let before_fresh = f.documents();
        let undo = f.app.session.undo.len();
        let journal = f.commands().len();
        let mut expected = f.masks();
        let target = expected.iter_mut().find(|m| m.id == f.target).unwrap();
        target.components[1].invert = !target.components[1].invert;
        f.open(1, opener);
        f.click(&f.action(1, "invert"));
        assert_eq!(&f.commands()[journal..], &[("mask.component".into(), json!({"id":f.target,"component":1,"action":"invert"}))]);
        assert_eq!(f.masks(), expected, "fresh opener still owns the selected shifted survivor: {opener:?}");
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.undo_exact(before_fresh, undo);
    }
}
