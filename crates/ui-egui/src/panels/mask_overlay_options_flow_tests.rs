//! Actual installed Masking panel; demo records only, no render/logic loop or OS services.
//! Native Settings scope is private: this suite does not manufacture it or claim native-parent coverage.
use super::LightcraftApp;
use crate::{Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::MaskView;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MODE: &str = "dropdown:maskOverlayMode";
const TRACK: &str = "slider:ui.maskOverlayOpacity";
const FIELD: &str = "sliderField:ui.maskOverlayOpacity";
const PINS: &str = "check:maskPins";

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
fn painted(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        egui::Shape::Text(text) => out.push((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()))),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                painted(shape, out);
            }
        }
        _ => {}
    }
}
fn swatch(c: [u8; 3]) -> String {
    format!("maskOverlayColor:{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}
fn owned_alias(key: &str) -> bool {
    key == MODE || key == PINS || key.starts_with("maskOverlayColor:") || key.ends_with(":ui.maskOverlayOpacity")
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 2],
    probe: Probe,
    tick: u32,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    texts: Vec<(String, Rect)>,
}
impl Fixture {
    fn new() -> Self {
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let ids = app.session.visible_cloned();
        let photos = [ids[0], ids[1]];
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("mask.add", json!({"kind":"linear","name":"Unchanged linear mask"})).unwrap();
        app.run("mask.add", json!({"kind":"radial","name":"Unchanged radial mask","rx":0.21})).unwrap();
        app.run("panel.masking", json!({})).unwrap();
        // Only setup uses commands. Every tested control change comes from the real panel.
        app.run("view.maskOverlayMode", json!({"mode":"color"})).unwrap();
        app.run("view.maskOverlayColor", json!({"color":[11,22,33],"opacity":37})).unwrap();
        app.run("view.maskPins", json!({"show":true})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, photos, probe: Probe::Real, tick: 0, rects: BTreeMap::new(), hits: BTreeMap::new(), texts: vec![] };
        let before = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before, "passive installed parent preserves photo/develop/history and view preferences");
        assert!(f.rects.contains_key(MODE) && f.rects.contains_key(PINS));
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
                    assert!(!crate::panels::settings::native_controls_active(ui.ctx()));
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
        self.texts.clear();
        for shape in &output.shapes {
            painted(&shape.shape, &mut self.texts);
        }
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
        for _ in 0..64 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == owner) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {key}, owner={owner:?}");
    }
    fn open(&mut self) {
        self.click(MODE);
    }
    fn choice(&mut self, view: MaskView) {
        let layer = self.hits.get(MODE).unwrap().layer_id;
        let caption = super::menu_label(view.label());
        let rows: Vec<_> =
            self.texts.iter().filter(|(s, r)| s == caption && self.ctx.layer_id_at(r.center()).is_some_and(|at| at != layer)).collect();
        assert_eq!(rows.len(), 1, "one actual popup row {caption:?}: {:?}", self.texts);
        self.click_at(rows[0].1.center());
    }
    fn choose(&mut self, view: MaskView) {
        self.open();
        self.choice(view);
    }
    fn prefs(&self) -> Value {
        json!({"on":self.app.ui.mask_overlay,"mode":self.app.ui.mask_overlay_mode,
            "color":self.app.ui.mask_overlay_color,"opacity":self.app.ui.mask_overlay_opacity,"pins":self.app.ui.mask_pins})
    }
    fn domain(&self) -> Value {
        let photos: Vec<_> = self.photos.iter().map(|id| self.app.session.catalog.photo(*id).unwrap()).collect();
        let develop: Vec<_> = self.photos.iter().map(|id| self.app.session.develop_of(*id)).collect();
        json!({"photos":photos,"develop":develop,"revision":self.app.session.catalog.revision,
            "selected":self.app.session.selection.ids,"active":self.app.session.active(),"activeMask":self.app.session.active_mask,
            "undo":self.app.session.undo.len(),"redo":self.app.session.redo.len(),"journal":self.app.session.journal})
    }
    fn neighbors(&self) -> Value {
        let mut ui = serde_json::to_value(&self.app.ui).unwrap();
        for key in ["maskOverlay", "maskOverlayMode", "maskOverlayColor", "maskOverlayOpacity", "maskPins"] {
            ui.as_object_mut().unwrap().remove(key);
        }
        ui
    }
    fn snapshot(&self) -> Value {
        json!({"domain":self.domain(),"prefs":self.prefs(),"neighbors":self.neighbors()})
    }
    fn unchanged_except_prefs(&self, domain: &Value, neighbors: &Value) {
        assert_eq!(&self.domain(), domain, "overlay input never edits either full photo, develop draft, selection, journal or Undo/Redo");
        assert_eq!(&self.neighbors(), neighbors, "unrelated view preferences, including shared tool overlay, are preserved");
    }
    fn replace_text(&mut self, value: &str) {
        self.click(FIELD);
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![Event::Text(value.to_owned())]);
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
fn overlay_parent_pointer_modes_keep_exact_view_values_and_photo_documents() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    for view in MaskView::ALL {
        let before = f.prefs();
        f.open();
        assert_eq!(f.prefs(), before, "opening a mode list changes nothing");
        f.choice(view);
        let mut expected = before;
        expected["mode"] = json!(view.name());
        expected["on"] = json!(true);
        assert_eq!(f.prefs(), expected, "exact named view mode from the existing command");
        let colored = matches!(view, MaskView::Color | MaskView::ColorOnBw);
        for c in super::OVERLAY_COLORS {
            assert_eq!(f.rects.contains_key(&swatch(c)), colored);
        }
        assert!(f.rects.contains_key(TRACK) && f.rects.contains_key(FIELD) && f.rects.contains_key(PINS));
        f.unchanged_except_prefs(&domain, &neighbors);
    }
}
#[test]
fn overlay_parent_keyboard_mode_preview_cancel_and_commit_use_real_select() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    f.tab_to(MODE);
    let before = f.prefs();
    f.key(Key::Enter);
    f.key(Key::End);
    assert_eq!(f.prefs(), before, "real list navigation proposes but does not apply");
    f.key(Key::Escape);
    assert_eq!(f.prefs(), before, "Escape cancels the real list without editing preferences");
    f.key(Key::Enter);
    f.key(Key::End);
    f.key(Key::Enter);
    let mut expected = before;
    expected["mode"] = json!("whiteOnBlack");
    expected["on"] = json!(true);
    assert_eq!(f.prefs(), expected);
    f.tab_to(MODE);
    f.key(Key::Home);
    expected["mode"] = json!("color");
    assert_eq!(f.prefs(), expected, "closed real Select key path still runs the same mode command");
    f.unchanged_except_prefs(&domain, &neighbors);
}
#[test]
fn overlay_parent_palette_pointer_and_keyboard_preserve_exact_srgb_bytes() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    for c in super::OVERLAY_COLORS {
        let mut expected = f.prefs();
        expected["color"] = json!(c);
        f.click(&swatch(c));
        assert_eq!(f.prefs(), expected);
        f.unchanged_except_prefs(&domain, &neighbors);
    }
    let red = super::OVERLAY_COLORS[0];
    f.tab_to(&swatch(red));
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_color, red);
    let green = super::OVERLAY_COLORS[1];
    f.tab_to(&swatch(green));
    f.key(Key::Space);
    assert_eq!(f.app.ui.mask_overlay_color, green);
    assert_eq!(f.app.ui.mask_overlay_opacity, 37.0);
    f.unchanged_except_prefs(&domain, &neighbors);
}
#[test]
fn overlay_parent_opacity_live_pointer_and_keyboard_change_only_view_state() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    let track = f.rect(TRACK);
    let at = egui::pos2(egui::lerp(track.x_range(), 0.37), track.center().y);
    f.frame(vec![Event::PointerMoved(at)]);
    f.pointer(at, true);
    assert_eq!(f.app.ui.mask_overlay_opacity, 37.0, "pressing without a drag is not an edit");
    // Cross the real drag threshold, then continue moving while the button is held.
    // The SDK's DragStart begins the gesture; subsequent Drag proposes live values.
    let started = egui::pos2(egui::lerp(track.x_range(), 0.62), track.center().y);
    f.frame(vec![Event::PointerMoved(started)]);
    let held = egui::pos2(egui::lerp(track.x_range(), 0.8), track.center().y);
    f.frame(vec![Event::PointerMoved(held)]);
    assert!(f.app.ui.mask_overlay_opacity > 37.0 && f.app.ui.mask_overlay_opacity <= 100.0, "opacity updates while the real pointer is held");
    f.unchanged_except_prefs(&domain, &neighbors);
    let live = f.app.ui.mask_overlay_opacity;
    let moved = egui::pos2(egui::lerp(track.x_range(), 0.62), track.center().y);
    f.frame(vec![Event::PointerMoved(moved)]);
    assert_ne!(f.app.ui.mask_overlay_opacity, live, "real drag updates the per-view value live");
    f.pointer(moved, false);
    f.frame(vec![]);
    let after = f.app.ui.mask_overlay_opacity;
    f.tab_to(TRACK);
    f.key(Key::ArrowRight);
    assert_eq!(f.app.ui.mask_overlay_opacity, (after + 1.0).min(100.0));
    f.key(Key::Home);
    assert_eq!(f.app.ui.mask_overlay_opacity, 0.0);
    f.key(Key::End);
    assert_eq!(f.app.ui.mask_overlay_opacity, 100.0);
    f.unchanged_except_prefs(&domain, &neighbors);
}
#[test]
fn overlay_parent_exact_opacity_entry_and_escape_preserve_existing_value_path() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    f.replace_text("74");
    assert_eq!(f.app.ui.mask_overlay_opacity, 37.0, "typing alone is not a committed opacity value");
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_opacity, 74.0);
    f.replace_text("21");
    assert_eq!(f.app.ui.mask_overlay_opacity, 74.0, "the replacement draft does not preview into view preferences");
    f.key(Key::Escape);
    assert_eq!(f.app.ui.mask_overlay_opacity, 74.0, "Escape restores the exact-entry gesture without a photo command");
    f.replace_text("100");
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_opacity, 100.0);
    f.replace_text("0");
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_opacity, 0.0);
    f.replace_text("63%");
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_opacity, 63.0, "typed percent remains in the existing 0..100 units");
    f.replace_text("63.4");
    f.key(Key::Enter);
    assert_eq!(f.app.ui.mask_overlay_opacity, 63.0, "the existing whole-percent step is retained");
    f.unchanged_except_prefs(&domain, &neighbors);
}
#[test]
fn overlay_parent_pins_gate_only_the_existing_shared_tool_overlay_row() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    assert!(f.rects.contains_key("dropdown:toolOverlay"));
    let mut expected = f.prefs();
    expected["pins"] = json!(false);
    f.click(PINS);
    assert_eq!(f.prefs(), expected);
    assert!(!f.rects.contains_key("dropdown:toolOverlay"));
    f.tab_to(PINS);
    f.key(Key::Space);
    expected["pins"] = json!(true);
    assert_eq!(f.prefs(), expected);
    assert!(f.rects.contains_key("dropdown:toolOverlay"));
    f.unchanged_except_prefs(&domain, &neighbors);
}
#[test]
fn overlay_parent_noncolored_and_hidden_overlay_refuse_opacity_but_keep_view_preferences() {
    let mut f = Fixture::new();
    let domain = f.domain();
    let neighbors = f.neighbors();
    for view in [MaskView::ImageOnBlack, MaskView::ImageOnWhite, MaskView::WhiteOnBlack] {
        f.choose(view);
        let before = f.prefs();
        assert!(!f.rects.keys().any(|s| s.starts_with("maskOverlayColor:")));
        let track = f.rect(TRACK);
        f.click_at(egui::pos2(track.right() - 1.0, track.center().y));
        assert_eq!(f.prefs(), before, "noncolored mode leaves disabled opacity unchanged");
    }
    f.choose(MaskView::ColorOnBw);
    assert!(super::OVERLAY_COLORS.iter().all(|c| f.rects.contains_key(&swatch(*c))));
    f.app.run("view.maskOverlay", json!({"show":false})).unwrap();
    f.frame(vec![]);
    let before = f.prefs();
    let track = f.rect(TRACK);
    f.click_at(egui::pos2(track.right() - 1.0, track.center().y));
    assert_eq!(f.prefs(), before, "overlay-off also disables opacity");
    // Picking the current mode still enables overlay through the original mode command.
    f.choose(MaskView::ColorOnBw);
    let mut expected = before;
    expected["on"] = json!(true);
    assert_eq!(f.prefs(), expected);
    f.unchanged_except_prefs(&domain, &neighbors);
    // Preferences belong to the view, not the selected photo. The explicit library command
    // may itself make a version; take its domain baseline AFTER that legitimate command.
    let prefs = f.prefs();
    f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
    let after_select = f.domain();
    f.frame(vec![]);
    assert_eq!(f.prefs(), prefs);
    assert_eq!(f.domain(), after_select);
    f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
    let returned = f.domain();
    f.frame(vec![]);
    assert_eq!(f.prefs(), prefs);
    assert_eq!(f.domain(), returned);
}
#[test]
fn overlay_parent_disabled_measurement_and_sizing_cannot_edit_or_replay() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new();
        let targets = [
            f.rect(MODE).center(),
            f.rect(&swatch(super::OVERLAY_COLORS[1])).center(),
            f.rect(TRACK).right_center() - egui::vec2(2.0, 0.0),
            f.rect(PINS).center(),
        ];
        let before = f.snapshot();
        f.probe = probe;
        for at in targets {
            f.pointer(at, true);
            f.pointer(at, false);
            assert_eq!(f.snapshot(), before, "inert actual-parent input: {probe:?}");
            if matches!(probe, Probe::Measure | Probe::Sizing) {
                assert!(!f.rects.keys().any(|key| owned_alias(key)), "speculative owned controls publish no action aliases");
            }
        }
        f.probe = Probe::Real;
        f.frame(vec![]);
        assert_eq!(f.snapshot(), before, "speculative input must not replay after normal rendering resumes");
        // A deliberate fresh action, not a state setter, still works after the inert passes.
        f.click(PINS);
        let mut expected = before["prefs"].clone();
        expected["pins"] = json!(false);
        assert_eq!(f.prefs(), expected);
        assert_eq!(f.domain(), before["domain"]);
        assert_eq!(f.neighbors(), before["neighbors"]);
    }
}
