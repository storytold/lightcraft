//! Installed Masking parent workflows. Demo records only; no render/model/logic loop or OS services.
//! The Settings native scope stays private; these tests do not manufacture native-parent coverage.
use super::{LightcraftApp, MaskShape};
use crate::{Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
fn drain(mut out: egui::FullOutput) {
    out.textures_delta.clear();
    out.platform_output.commands.clear();
    out.viewport_output.clear();
    out.shapes.clear();
}
fn lum() -> MaskShape {
    MaskShape::LuminanceRange { lo: 0.2, hi: 0.8, lo_feather: 0.1, hi_feather: 0.15 }
}
fn color() -> MaskShape {
    MaskShape::ColorRange { samples: vec![[0.6, 0.1, -0.2], [0.3, -0.1, 0.1]], refine: 37.0 }
}
fn object() -> MaskShape {
    MaskShape::Object { hint: vec![], exclude: vec![], seg: Some(lightcraft_develop::SegMask::from_logits(1, &[0.7])), detail: vec![], edge: 12.0 }
}
fn prompt() -> MaskShape {
    MaskShape::Prompt {
        text: "Stored fixture, never submitted to a model".into(),
        seg: Some(lightcraft_develop::SegMask::from_logits(1, &[0.8])),
        detail: vec![],
        edge: -17.0,
    }
}
fn control(prefix: &str, comp: usize, name: &str) -> String {
    format!("{prefix}:maskRange:{comp}:{name}")
}
fn owned(key: &str) -> bool {
    key.contains(":maskRange:") || key.starts_with("check:lumMap")
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 2],
    target: u32,
    other_mask: u32,
    initial_photos: Value,
    probe: Probe,
    tick: u32,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
}
impl Fixture {
    fn new(shapes: &[MaskShape]) -> Self {
        assert!(!shapes.is_empty());
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let ids = app.session.visible_cloned();
        let photos = [ids[0], ids[1]];
        app.run("library.select", json!({"ids":[photos[1].0],"active":photos[1].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Other photo, never edited"})).unwrap();
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Other mask, never edited"})).unwrap();
        let other_mask = app.session.active_mask.unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Range target"})).unwrap();
        let target = app.session.active_mask.unwrap();
        for (k, shape) in shapes.iter().enumerate() {
            if k > 0 {
                app.run("mask.addComponent", json!({"id":target,"op":"subtract","kind":"linear"})).unwrap();
            }
            // mask.update just replaces stored shape data. No AI resolve, sample or input path is invoked.
            app.run("mask.update", json!({"id":target,"component":k,"shape":shape})).unwrap();
        }
        app.run("panel.masking", json!({})).unwrap();
        app.run("view.maskOverlayMode", json!({"mode":"color"})).unwrap();
        app.ui.tool.clear();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let initial_photos = json!(photos.iter().map(|id| app.session.catalog.photo(*id).unwrap()).collect::<Vec<_>>());
        let mut f =
            Self { app, ctx, photos, target, other_mask, initial_photos, probe: Probe::Real, tick: 0, rects: BTreeMap::new(), hits: BTreeMap::new() };
        let before = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before, "passive actual parent changes no photo, journal or preferences");
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
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 2000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let probe = self.probe;
        let ctx = self.ctx.clone();
        let mut registries = (vec![], vec![]);
        let out = self.ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(360.0);
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
            registries = if root.ctx().will_discard() { (vec![], vec![]) } else { now };
        });
        self.rects = registries.0.into_iter().collect();
        self.hits = registries.1.into_iter().collect();
        drain(out);
    }
    fn rect(&self, key: &str) -> Rect {
        *self.rects.get(key).unwrap_or_else(|| panic!("missing actual target {key}: {:?}", self.rects.keys()))
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click(&mut self, key: &str) {
        let at = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
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
    fn tab(&mut self, key: &str) {
        let id = self.hits.get(key).unwrap().id;
        self.frame(vec![Event::PointerGone]);
        for _ in 0..96 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab never reached {key}");
    }
    fn text(&mut self, comp: usize, name: &str, value: &str) {
        self.click(&control("sliderField", comp, name));
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![Event::Text(value.into())]);
    }
    fn held_drag(&mut self, comp: usize, name: &str, from: f32, to: f32) -> Pos2 {
        let r = self.rect(&control("slider", comp, name));
        let at = egui::pos2(egui::lerp(r.x_range(), from), r.center().y);
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.frame(vec![Event::PointerMoved(egui::pos2(egui::lerp(r.x_range(), (from + to) * 0.5), r.center().y))]);
        let at = egui::pos2(egui::lerp(r.x_range(), to), r.center().y);
        self.frame(vec![Event::PointerMoved(at)]);
        at
    }
    fn release(&mut self, at: Pos2) {
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn settings(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.photos[0]).unwrap()).clone()
    }
    fn shape(&self, k: usize) -> MaskShape {
        self.settings().masks.iter().find(|m| m.id == self.target).unwrap().components[k].shape.clone()
    }
    fn photos(&self) -> Value {
        json!(self.photos.iter().map(|id| self.app.session.catalog.photo(*id).unwrap()).collect::<Vec<_>>())
    }
    fn domain(&self) -> Value {
        json!({"photos":self.photos(),"revision":self.app.session.catalog.revision,"active":self.app.session.active(),"selected":self.app.session.selection.ids,"mask":self.app.session.active_mask,"journal":self.app.session.journal,"undo":self.app.session.undo.len(),"redo":self.app.session.redo.len()})
    }
    fn snapshot(&self) -> Value {
        json!({"domain":self.domain(),"ui":self.app.ui})
    }
    fn isolated(&self, before: &DevelopSettings, other: &Value, k: usize) {
        let mut actual = self.settings();
        let original = before.masks.iter().find(|m| m.id == self.target).unwrap().components[k].shape.clone();
        actual.masks.iter_mut().find(|m| m.id == self.target).unwrap().components[k].shape = original;
        assert_eq!(&actual, before, "only the exact component shape may change in the full DevelopSettings");
        assert_eq!(
            &serde_json::to_value(self.app.session.catalog.photo(self.photos[1]).unwrap()).unwrap(),
            other,
            "the other full photo never changes"
        );
        assert_eq!(self.app.session.active(), Some(self.photos[0]));
        assert_eq!(self.app.session.active_mask, Some(self.target));
    }
    fn other(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photo(self.photos[1]).unwrap()).unwrap()
    }
    fn undo(&mut self, before: &DevelopSettings, other: &Value, undo: usize) {
        self.app.run("edit.undo", json!({})).unwrap();
        self.frame(vec![]);
        assert_eq!(&self.settings(), before, "Undo restores complete target develop state");
        assert_eq!(self.app.session.undo.len(), undo);
        assert_eq!(&self.other(), other);
        assert_eq!(self.photos(), self.initial_photos, "Undo restores both complete photo records, including develop/history metadata");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 2000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn range_parent_smoothness_live_drag_is_one_undo_and_changes_only_both_feathers() {
    let mut f = Fixture::new(&[lum(), color()]);
    let before = f.settings();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let at = f.held_drag(0, "smoothness", 0.25, 0.78);
    let MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } = f.shape(0) else { panic!() };
    assert_eq!((lo, hi), (0.2, 0.8));
    assert_eq!(lo_feather, hi_feather);
    assert!(lo_feather > 0.25 && lo_feather <= 0.5, "real held drag updates the shape live");
    assert_eq!(f.app.session.undo.len(), undo, "held edit has no intermediate Undo steps");
    f.isolated(&before, &other, 0);
    f.release(at);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.isolated(&before, &other, 0);
    f.undo(&before, &other, undo);
}
#[test]
fn range_parent_exact_smoothness_percent_commit_escape_and_bounds_keep_native_units() {
    let mut f = Fixture::new(&[lum()]);
    let before = f.settings();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let untouched = f.domain();
    f.text(0, "smoothness", "63%");
    assert_eq!(f.domain(), untouched, "draft text never edits the photo or journal");
    f.key(Key::Enter);
    assert_eq!(f.shape(0), MaskShape::LuminanceRange { lo: 0.2, hi: 0.8, lo_feather: 0.315, hi_feather: 0.315 });
    f.isolated(&before, &other, 0);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.undo(&before, &other, undo);
    let untouched = f.domain();
    f.text(0, "smoothness", "21");
    f.key(Key::Escape);
    assert_eq!(f.domain(), untouched, "Escape cancels a draft without commands");
    f.text(0, "smoothness", "");
    f.key(Key::Backspace);
    assert_eq!(f.domain(), untouched, "clearing a draft does not preview zero");
    f.key(Key::Enter);
    assert_eq!(f.domain(), untouched, "an absent committed number leaves the existing value and history unchanged");
    f.text(0, "smoothness", "999");
    f.key(Key::Enter);
    let MaskShape::LuminanceRange { lo_feather, hi_feather, .. } = f.shape(0) else { panic!() };
    assert_eq!((lo_feather, hi_feather), (0.5, 0.5));
}
#[test]
fn range_parent_luminance_map_pointer_keyboard_roundtrip_is_view_only() {
    let mut f = Fixture::new(&[lum()]);
    f.app.run("view.maskOverlayMode", json!({"mode":"imageOnWhite"})).unwrap();
    f.app.run("view.maskOverlay", json!({"show":false})).unwrap();
    f.frame(vec![]);
    let domain = f.domain();
    let before = serde_json::to_value(&f.app.ui).unwrap();
    f.click("check:lumMap0");
    assert!(f.app.ui.mask_overlay);
    assert_eq!(f.app.ui.mask_overlay_mode, "colorOnBw");
    assert_eq!(f.app.ui.luminance_map_restore, Some((false, "imageOnWhite".into())));
    assert_eq!(f.domain(), domain);
    f.tab("check:lumMap0");
    f.key(Key::Space);
    assert!(!f.app.ui.mask_overlay);
    assert_eq!(f.app.ui.mask_overlay_mode, "imageOnWhite");
    assert!(f.app.ui.luminance_map_restore.is_none());
    assert_eq!(serde_json::to_value(&f.app.ui).unwrap(), before);
    assert_eq!(f.domain(), domain);
}
#[test]
fn range_parent_refine_pointer_keyboard_and_existing_pick_preserve_samples_and_neighbors() {
    let mut f = Fixture::new(&[color(), lum()]);
    let before = f.settings();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let at = f.held_drag(0, "refine", 0.37, 0.8);
    let MaskShape::ColorRange { samples, refine } = f.shape(0) else { panic!() };
    assert!(refine > 37.0);
    let MaskShape::ColorRange { samples: original, .. } = color() else { panic!() };
    assert_eq!(samples, original);
    f.release(at);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.isolated(&before, &other, 0);
    f.undo(&before, &other, undo);
    f.tab(&control("slider", 0, "refine"));
    f.key(Key::ArrowRight);
    let MaskShape::ColorRange { refine, .. } = f.shape(0) else { panic!() };
    assert_eq!(refine, 38.0);
    f.isolated(&before, &other, 0);
    let domain = f.domain();
    f.tab("button:colorPick0");
    f.key(Key::Space);
    assert_eq!(f.app.ui.tool, "colorRange");
    f.key(Key::Space);
    assert!(f.app.ui.tool.is_empty());
    assert_eq!(f.domain(), domain, "already-SDK Pick arms/disarms without sampling or editing a document");
}
#[test]
fn range_parent_object_and_prompt_edge_exact_and_keyboard_target_separate_components() {
    let mut f = Fixture::new(&[object(), prompt()]);
    let before = f.settings();
    let other = f.other();
    let undo = f.app.session.undo.len();
    f.text(1, "edge", "-63%");
    f.key(Key::Enter);
    let MaskShape::Prompt { edge, .. } = f.shape(1) else { panic!() };
    assert_eq!(edge, -63.0);
    f.isolated(&before, &other, 1);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.undo(&before, &other, undo);
    f.tab(&control("slider", 0, "edge"));
    f.key(Key::Home);
    let MaskShape::Object { edge, .. } = f.shape(0) else { panic!() };
    assert_eq!(edge, -100.0);
    f.isolated(&before, &other, 0);
    f.key(Key::End);
    let MaskShape::Object { edge, .. } = f.shape(0) else { panic!() };
    assert_eq!(edge, 100.0);
    f.isolated(&before, &other, 0);
}
#[test]
fn range_parent_numeric_scrub_live_escape_and_release_preserve_gesture_history() {
    let mut f = Fixture::new(&[color()]);
    let before = f.settings();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let r = f.rect(&control("sliderValue", 0, "refine"));
    let start = r.center();
    f.frame(vec![Event::PointerMoved(start)]);
    f.pointer(start, true);
    f.frame(vec![Event::PointerMoved(start + egui::vec2(15.0, 0.0))]);
    f.frame(vec![Event::PointerMoved(start + egui::vec2(24.0, 0.0))]);
    let MaskShape::ColorRange { refine, .. } = f.shape(0) else { panic!() };
    assert_ne!(refine, 37.0, "numeric scrub previews while held");
    assert_eq!(f.app.session.undo.len(), undo);
    f.isolated(&before, &other, 0);
    f.key(Key::Escape);
    f.release(start + egui::vec2(24.0, 0.0));
    assert_eq!(f.settings(), before);
    assert_eq!(f.app.session.undo.len(), undo);
    let at = f.held_drag(0, "refine", 0.37, 0.7);
    f.release(at);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.undo(&before, &other, undo);
}
#[test]
fn range_parent_stale_drafts_cannot_retarget_mask_photo_or_shifted_component() {
    let mut f = Fixture::new(&[color(), color()]);
    for switch in ["mask", "photo", "delete"] {
        f.text(1, "refine", "81");
        match switch {
            "mask" => {
                f.app.run("mask.select", json!({"id":f.other_mask})).unwrap();
            }
            "photo" => {
                f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
            }
            _ => {
                f.app.run("mask.component", json!({"id":f.target,"component":0,"action":"delete"})).unwrap();
            }
        }
        let baseline = f.domain();
        f.frame(vec![]);
        f.key(Key::Enter);
        assert_eq!(f.domain(), baseline, "old numeric draft cannot apply after {switch}");
        if switch == "photo" {
            f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
        }
        f.app.run("mask.select", json!({"id":f.target})).unwrap();
        f.frame(vec![]);
    }
    let before = f.settings();
    let other = f.other();
    f.text(0, "refine", "66");
    f.key(Key::Enter);
    let MaskShape::ColorRange { refine, .. } = f.shape(0) else { panic!() };
    assert_eq!(refine, 66.0);
    f.isolated(&before, &other, 0);
}
#[test]
fn range_parent_disabled_measurement_and_sizing_cannot_edit_or_replay_ordinary_controls() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new(&[lum(), color(), object()]);
        let keys = [control("slider", 0, "smoothness"), control("sliderField", 1, "refine"), control("slider", 2, "edge"), "check:lumMap0".into()];
        let targets: Vec<_> = keys.iter().map(|key| f.rect(key).center()).collect();
        let before = f.snapshot();
        f.probe = probe;
        for at in targets {
            f.frame(vec![Event::PointerMoved(at)]);
            f.pointer(at, true);
            f.pointer(at, false);
            assert_eq!(f.snapshot(), before, "inert actual parent: {probe:?}");
            if matches!(probe, Probe::Measure | Probe::Sizing) {
                assert!(!f.rects.keys().any(|key| owned(key)), "ordinary speculative locators are not published");
            }
        }
        f.probe = Probe::Real;
        f.frame(vec![]);
        assert_eq!(f.snapshot(), before, "no speculative input replays on return");
        f.click("check:lumMap0");
        assert_eq!(f.app.ui.mask_overlay_mode, "colorOnBw", "a deliberate fresh action still works");
    }
}
