//! Authored interaction coverage for the production text-overlay subtree. No renderer, app
//! logic loop, OS picker, image file, or filesystem library is started by these fixtures.
use std::collections::BTreeMap;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::MemStore;
use lightcraft_engine::{Session, library::LibraryStores, slide_overlay::TextOverlay};
use serde_json::{Value, json};

use crate::headless::HeadlessView;
use crate::{LightcraftApp, Services};

#[derive(Clone, Copy)]
enum Probe {
    Normal,
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

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    native: bool,
    tick: u32,
    probe: Probe,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    rects: BTreeMap<String, Rect>,
}

impl Fixture {
    fn open(session: &mut Session, files: MemStore) {
        session
            .open_library_in(
                LibraryStores {
                    dir: "memory-only-slideshow-overlays".into(),
                    catalog: Box::new(MemStore::new()),
                    files: Box::new(files),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
    }

    fn seed(session: &mut Session, count: usize) {
        // Fixture construction only. All assertions below drive the actual production UI and
        // observe the real command journal and MemStore writes, never substitute a setter.
        session.slide_overlays.texts = (0..count)
            .map(|i| TextOverlay { text: format!("overlay {i}"), size: 0.03 + i as f64 * 0.001, opacity: 0.9, ..Default::default() })
            .collect();
        session.slide_overlays.identity_plate.text = "Untouched identity".into();
        session.slide_overlays.watermark.text = "Untouched watermark".into();
        session.save_prefs().unwrap();
    }

    fn new(native: bool, count: usize) -> Self {
        let files = MemStore::new();
        let mut session = Session::new();
        Self::open(&mut session, files.clone());
        Self::seed(&mut session, count);
        let app = LightcraftApp::new(session, Services { png: None, local_places: Some(vec![]), ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, files, native, tick: 0, probe: Probe::Normal, hits: BTreeMap::new(), rects: BTreeMap::new() };
        let before_passive_layout = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before_passive_layout, "passive initialization must not edit model, preferences or journal; native={native}");
        f
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        assert!(self.app.session.library.as_ref().is_some_and(|lib| !lib.on_disk));
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        // This is layout only: no pixel image matching this size is ever allocated.
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 16000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let native = self.native;
        let probe = self.probe;
        let out = self.ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let ctx = ui.ctx().clone();
                let mut draw = |ui: &mut egui::Ui| {
                    let mut subtree = || {
                        super::text_overlays(app, ui, &crate::theme::Tokens::get(&ctx));
                    };
                    if native {
                        super::super::native_controls::scope(&ctx, subtree);
                    } else {
                        subtree();
                    }
                };
                match probe {
                    Probe::Normal => draw(ui),
                    Probe::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Probe::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Probe::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                }
            });
        });
        drain(out);
        let (rects, hits) = crate::widgets::take_registries(&self.ctx);
        self.rects = rects.into_iter().collect();
        self.hits = hits.into_iter().collect();
    }

    fn rect(&self, name: &str) -> Rect {
        *self.rects.get(name).unwrap_or_else(|| panic!("missing {name}: {:?}", self.rects.keys()))
    }
    fn owner(&self, name: &str) -> egui::Id {
        self.hits.get(name).unwrap_or_else(|| panic!("missing real hit {name}")).id
    }
    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    fn click(&mut self, name: &str) {
        let pos = self.rect(name).center();
        self.pointer(pos, true);
        self.pointer(pos, false);
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
    fn replace(&mut self, name: &str, text: &str) {
        self.click(name);
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        if text.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.frame(vec![Event::Text(text.into())]);
        }
    }
    fn enter_number(&mut self, name: &str, value: &str) {
        self.replace(name, value);
        self.key(Key::Enter);
    }
    fn tab_to(&mut self, name: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            let owner = self.owner(name);
            let native_focus = self.ctx.memory(|m| m.focused()) == Some(owner);
            let sdk_focus = craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == owner);
            if native_focus || sdk_focus {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural {} traversal failed to reach {name}", if self.native { "native" } else { "SDK" });
    }
    fn journal(&self) -> Vec<(String, Value)> {
        self.app.session.journal.clone()
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap()
    }
    fn saved_matches(&self) {
        // JSON's f32 serializer writes its shortest round-trip decimal (e.g. watermark 0.2),
        // whereas to_value first widens that f32 to f64 (0.20000000298023224). Compare the
        // exact typed persisted model, not two different JSON Number representations.
        let saved: lightcraft_engine::slide_overlay::OverlaySettings = serde_json::from_value(self.saved()["slideOverlays"].clone()).unwrap();
        assert_eq!(saved, self.app.session.slide_overlays);
    }
    fn untouched(&self) -> Value {
        let s = &self.app.session;
        json!({"layout":s.slide_layout,"titles":s.slide_titles,"plate":s.slide_overlays.identity_plate,
            "stars":s.slide_overlays.rating_stars,"watermark":s.slide_overlays.watermark,
            "showWatermark":s.slide_overlays.show_watermark,"revision":s.catalog.revision,
            "undo":s.undo.len(),"redo":s.redo.len(),"selected":s.selection.ids,"active":s.selection.active})
    }
    fn snapshot(&self) -> Value {
        json!({"overlays":self.app.session.slide_overlays,"saved":self.saved(),"journal":self.app.session.journal,"untouched":self.untouched()})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 16000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap(); // MemStore, never a filesystem library.
        }
    }
}

#[test]
fn overlays_remove_middle_then_edit_preserves_survivor_identity_and_neighbors() {
    for native in [false, true] {
        let mut f = Fixture::new(native, 3);
        let before = f.app.session.slide_overlays.clone();
        let untouched = f.untouched();
        let last_owner = f.owner("field:slideshowText2");
        f.click("button:slideshowText1Remove");
        assert_eq!(f.app.session.slide_overlays.texts, vec![before.texts[0].clone(), before.texts[2].clone()]);
        assert_eq!(f.owner("field:slideshowText1"), last_owner, "the remaining editor follows its overlay, not its old index");
        f.replace("field:slideshowText1", "last {title} edited");
        assert_eq!(f.journal(), vec![("slideshow.overlays".into(), json!({"removeText":1}))], "typing is not a command");
        f.key(Key::Enter);
        assert_eq!(f.app.session.slide_overlays.texts[0], before.texts[0]);
        assert_eq!(f.app.session.slide_overlays.texts[1].text, "last {title} edited");
        assert_eq!(
            f.journal(),
            vec![
                ("slideshow.overlays".into(), json!({"removeText":1})),
                ("slideshow.overlays".into(), json!({"texts":[{}, {"text":"last {title} edited"}]})),
            ]
        );
        assert_eq!(f.untouched(), untouched);
        f.saved_matches();
    }
}

#[test]
fn overlays_text_cancel_enter_and_blur_commit_only_the_intended_row() {
    for native in [false, true] {
        let mut f = Fixture::new(native, 2);
        let before = f.snapshot();
        f.replace("field:slideshowText0", "discard this");
        f.key(Key::Escape);
        assert_eq!(f.snapshot(), before);
        f.replace("field:slideshowText0", "{title} literal {unknown}");
        f.click("field:slideshowText1"); // A real focus change commits the first field.
        assert_eq!(f.journal(), vec![("slideshow.overlays".into(), json!({"texts":[{"text":"{title} literal {unknown}"},{}]}))]);
        assert_eq!(f.app.session.slide_overlays.texts[1].text, "overlay 1");
        f.replace("field:slideshowText1", "");
        f.key(Key::Enter);
        assert_eq!(f.app.session.slide_overlays.texts[1].text, "");
        assert_eq!(f.journal().len(), 2);
        let long_text = "🦀".repeat(lightcraft_engine::slide_overlay::TEXT_MAX_CHARS + 17);
        f.replace("field:slideshowText1", &long_text);
        f.key(Key::Enter);
        assert_eq!(
            f.app.session.slide_overlays.texts[1].text.chars().count(),
            lightcraft_engine::slide_overlay::TEXT_MAX_CHARS,
            "the real command validates/truncates, not an alternate test setter"
        );
        assert_eq!(f.journal().len(), 3);
        f.saved_matches();
    }
}

#[test]
fn overlays_placement_and_number_field_commit_exact_units_and_payloads() {
    let mut f = Fixture::new(false, 3);
    let neighbors = [f.app.session.slide_overlays.texts[0].clone(), f.app.session.slide_overlays.texts[2].clone()];
    let untouched = f.untouched();
    f.click("button:slideshowText1Anchor-0");
    f.click("button:slideshowText1RelativeTo-1");
    f.enter_number("sliderValue:slideshowText1Opacity", "45");
    f.enter_number("sliderValue:slideshowText1OffsetX", "12.5");
    assert_eq!(f.app.session.slide_overlays.texts[1].place.anchor, lightcraft_engine::export::Anchor::TopLeft);
    assert_eq!(f.app.session.slide_overlays.texts[1].place.relative_to, lightcraft_engine::slide_overlay::RelativeTo::Photo);
    assert!((f.app.session.slide_overlays.texts[1].opacity - 0.45).abs() < 1e-9);
    assert!((f.app.session.slide_overlays.texts[1].place.offset_x - 0.125).abs() < 1e-9);
    assert_eq!(
        f.journal(),
        vec![
            ("slideshow.overlays".into(), json!({"texts":[{},{"anchor":"topLeft"},{}]})),
            ("slideshow.overlays".into(), json!({"texts":[{},{"relativeTo":"photo"},{}]})),
            ("slideshow.overlays".into(), json!({"texts":[{},{"opacity":0.45},{}]})),
            ("slideshow.overlays".into(), json!({"texts":[{},{"offsetX":0.125},{}]})),
        ]
    );
    assert_eq!(f.app.session.slide_overlays.texts[0], neighbors[0]);
    assert_eq!(f.app.session.slide_overlays.texts[2], neighbors[1]);
    assert_eq!(f.untouched(), untouched);
    f.enter_number("sliderValue:slideshowText1Size", "999");
    assert!(
        (f.app.session.slide_overlays.texts[1].size - lightcraft_engine::slide_overlay::TEXT_SIZE_MAX / 2.0).abs() < 1e-9,
        "typed values retain the existing UI range"
    );
    assert_eq!(f.untouched(), untouched);
    f.saved_matches();
}

#[test]
fn overlays_slider_preview_saves_once_on_release_and_sdk_escape_restores() {
    for native in [false, true] {
        let mut f = Fixture::new(native, 1);
        let before = f.snapshot();
        let track = f.rect("slider:slideshowText0Opacity");
        let start = egui::pos2(track.left() + track.width() * 0.8, track.center().y);
        let end = egui::pos2(track.left() + track.width() * 0.4, track.center().y);
        f.pointer(start, true);
        // Crossing the drag threshold begins the SDK gesture. Its following real movement
        // produces the first Update; DragStart deliberately emits only Begin (slider.rs).
        let middle = egui::pos2(track.left() + track.width() * 0.7, track.center().y);
        f.frame(vec![Event::PointerMoved(middle)]);
        f.frame(vec![Event::PointerMoved(end)]);
        assert_ne!(f.app.session.slide_overlays.texts[0].opacity, 0.9, "actual held drag previews the new value");
        assert!(f.journal().is_empty());
        assert_eq!(f.saved(), before["saved"], "preview does not save");
        let preview = f.app.session.slide_overlays.texts[0].opacity;
        f.pointer(end, false);
        f.frame(vec![]);
        assert_eq!(f.journal(), vec![("slideshow.overlays".into(), json!({"texts":[{"opacity":preview}]}))]);
        f.saved_matches();
        assert_eq!(f.untouched(), before["untouched"]);
        for (target, key, expected_max) in [
            ("slider:slideshowText0Size", "size", lightcraft_engine::slide_overlay::TEXT_SIZE_MAX / 2.0),
            ("slider:slideshowText0Opacity", "opacity", 1.0),
        ] {
            let commands_before = f.journal().len();
            let bounds = f.rect(target);
            let start = bounds.center();
            let end = egui::pos2(bounds.right() + 60.0, bounds.center().y);
            f.pointer(start, true);
            f.frame(vec![Event::PointerMoved(egui::pos2((start.x + end.x) * 0.5, start.y))]);
            f.frame(vec![Event::PointerMoved(end)]);
            f.pointer(end, false);
            f.frame(vec![]);
            let row = &f.app.session.slide_overlays.texts[0];
            assert_eq!(
                if key == "size" { row.size } else { row.opacity },
                expected_max,
                "real edits must still clamp at the original UI maximum; native={native}, key={key}"
            );
            assert_eq!(f.journal().get(commands_before..).unwrap(), &[("slideshow.overlays".into(), json!({"texts":[{key:expected_max}]}))]);
            assert_eq!(f.untouched(), before["untouched"]);
            f.saved_matches();
        }
    }
    let mut f = Fixture::new(false, 1);
    let before = f.snapshot();
    let track = f.rect("slider:slideshowText0Opacity");
    let start = track.center();
    let end = egui::pos2(track.left() + track.width() * 0.2, track.center().y);
    f.pointer(start, true);
    let middle = egui::pos2(track.left() + track.width() * 0.35, track.center().y);
    f.frame(vec![Event::PointerMoved(middle)]);
    f.frame(vec![Event::PointerMoved(end)]);
    assert_ne!(f.app.session.slide_overlays.texts[0].opacity, 0.9, "cancel must retire an actual preview, not only Begin");
    f.key(Key::Escape);
    f.pointer(end, false);
    f.frame(vec![]);
    assert_eq!(f.snapshot(), before, "cancel restores preview without a preference command or Undo record");
}

#[test]
fn overlays_disabled_and_both_measurement_passes_never_edit_or_replay() {
    for native in [false, true] {
        for target in ["button:slideshowText0Remove", "button:slideshowText0Anchor-0", "button:slideshowAddText", "slider:slideshowText0Opacity"] {
            let mut f = Fixture::new(native, 1);
            let before = f.snapshot();
            let at = f.rect(target).center();
            f.probe = Probe::Disabled;
            f.pointer(at, true);
            f.pointer(at, false);
            assert_eq!(f.snapshot(), before);
            for probe in [Probe::Measure, Probe::Sizing] {
                f.probe = probe;
                f.pointer(at, true);
                f.pointer(at, false);
                assert_eq!(f.snapshot(), before);
                assert!(!f.rects.keys().any(|name| name.starts_with("button:slideshowText")
                    || name.starts_with("slider:slideshowText")
                    || name == "button:slideshowAddText"));
                f.probe = Probe::Normal;
                f.frame(vec![]);
                assert_eq!(f.snapshot(), before, "measured input must not replay on the next real pass");
            }
        }
    }
}

#[test]
fn overlays_library_switch_and_external_replacement_retire_unfinished_drafts() {
    for native in [false, true] {
        let mut f = Fixture::new(native, 1);
        f.replace("field:slideshowText0", "old library draft");
        let old_owner = f.owner("field:slideshowText0");
        f.app.session.close_library().unwrap();
        f.files = MemStore::new();
        Fixture::open(&mut f.app.session, f.files.clone());
        Fixture::seed(&mut f.app.session, 1);
        let before = f.snapshot();
        f.frame(vec![]);
        assert_ne!(f.owner("field:slideshowText0"), old_owner);
        f.key(Key::Enter);
        assert_eq!(f.snapshot(), before);
        f.replace("field:slideshowText0", "old row draft");
        let old_owner = f.owner("field:slideshowText0");
        f.app.run("slideshow.overlays", json!({"texts":[{"text":"external replacement"}]})).unwrap();
        let before = f.snapshot();
        f.frame(vec![]);
        assert_ne!(f.owner("field:slideshowText0"), old_owner);
        f.key(Key::Enter);
        assert_eq!(f.snapshot(), before);
    }
}

#[test]
fn overlays_limit_hides_add_then_removal_allows_exactly_one_new_row() {
    for native in [false, true] {
        let mut f = Fixture::new(native, lightcraft_engine::slide_overlay::MAX_TEXTS);
        assert!(f.journal().is_empty(), "passive 16-row initialization must not dispatch; native={native}");
        let before_idle = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.snapshot(), before_idle, "idle numeric round-tripping must not edit; native={native}");
        assert!(!f.rects.contains_key("button:slideshowAddText"));
        let before = f.app.session.slide_overlays.texts.clone();
        f.click("button:slideshowText8Remove");
        assert!(f.rects.contains_key("button:slideshowAddText"));
        f.click("button:slideshowAddText");
        assert_eq!(f.app.session.slide_overlays.texts.len(), lightcraft_engine::slide_overlay::MAX_TEXTS);
        assert!(!f.rects.contains_key("button:slideshowAddText"));
        let expected: Vec<_> =
            before.into_iter().enumerate().filter_map(|(i, row)| (i != 8).then_some(row)).chain([TextOverlay::default()]).collect();
        assert_eq!(f.app.session.slide_overlays.texts, expected);
        assert_eq!(f.journal(), vec![("slideshow.overlays".into(), json!({"removeText":8})), ("slideshow.overlays".into(), json!({"addText":{}}))]);
        f.saved_matches();
    }
}

#[test]
fn overlays_natural_keyboard_add_and_remove_work_in_both_presentation_domains() {
    for native in [false, true] {
        for key in [Key::Enter, Key::Space] {
            let mut f = Fixture::new(native, 1);
            f.tab_to("button:slideshowAddText");
            f.key(key);
            assert_eq!(f.app.session.slide_overlays.texts.len(), 2);
            assert_eq!(f.journal(), vec![("slideshow.overlays".into(), json!({"addText":{}}))]);
            f.click("field:slideshowText0");
            f.tab_to("button:slideshowText0Remove");
            f.key(key);
            assert_eq!(f.app.session.slide_overlays.texts, vec![TextOverlay::default()]);
            assert_eq!(f.journal().len(), 2);
            f.saved_matches();
        }
    }
}
