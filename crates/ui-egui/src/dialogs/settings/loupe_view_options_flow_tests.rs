//! Exercises the production view_options body, not a substitute picker or direct setter.
//! No app logic loop, disk library, renderer, OS services, photos loaded from disk, or settings writes.
use super::{LightcraftApp, Tokens, Value, json, view_options};
use crate::headless::HeadlessView;
use crate::state::{InfoOverlay as I, LoupeInfo, LoupeInfoField as F};
use egui::{Event, Key, Modifiers, PointerButton, Rect};

struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    native: bool,
    parent: bool,
    enabled: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    painted: Vec<String>,
}
impl Fixture {
    fn new(native: bool) -> Self {
        Self::with_parent(native, false)
    }
    fn with_parent(native: bool, parent: bool) -> Self {
        let app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { local_places: Some(vec![]), png: None, ..Default::default() },
        );
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, native, parent, enabled: true, tick: 0, rects: vec![], hits: vec![], painted: vec![] };
        f.app.run("view.viewOptions", json!({})).unwrap();
        for _ in 0..3 {
            f.frame(vec![], 0);
        }
        f
    }
    fn memory_only(&self) {
        assert!(self.app.session.library.is_none());
        assert!(self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.services.pick_display_profile.is_none());
    }
    fn frame(&mut self, events: Vec<Event>, probe: u8) {
        self.memory_only();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1400.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let native = self.native;
        let parent = self.parent;
        let enabled = self.enabled;
        let mut final_pass = None;
        let output = Output(self.ctx.run_ui(raw, |ui| {
            if parent {
                assert!(!native && enabled && probe == 0, "actual ViewOptions parent is unscoped");
                crate::dialogs::show(app, &ctx, false);
            } else {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut draw = |ui: &mut egui::Ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            let mut body = || {
                                view_options(app, ui, &Tokens::get(&ctx));
                            };
                            if native {
                                // Retain the actual Settings boundary; this tests fallback, not SDK adoption.
                                (body)();
                            } else {
                                body();
                            }
                        });
                    };
                    match probe {
                        1 => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                        2 => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                        _ => draw(ui),
                    }
                });
            }
            // run_ui can discard and rerun a pass (notably a native ComboBox's first popup
            // layout). The app drains its registry in draw_window, once per UI pass too.
            // Never combine registrations belonging to two separate presentations.
            final_pass = Some(crate::widgets::take_registries(&ctx));
        }));
        fn text(shape: &egui::Shape, words: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => words.push(t.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, words);
                    }
                }
                _ => {}
            }
        }
        self.painted.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.painted);
        }
        (self.rects, self.hits) = final_pass.expect("run_ui presents at least one pass");
        drop(output);
        self.memory_only();
    }
    fn rect(&self, name: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(id, _)| id == name).collect();
        assert_eq!(found.len(), 1, "one real locator for {name} in the presented pass (native={})", self.native);
        found[0].1
    }
    fn has(&self, name: &str) -> bool {
        self.rects.iter().any(|(id, _)| id == name)
    }
    fn menu_open(&self, face: &str) -> bool {
        if self.native {
            egui::Popup::is_any_open(&self.ctx)
        } else {
            let owner = self.hits.iter().find(|(id, _)| id == face).expect("production face").1.id;
            self.ctx.data(|data| data.get_temp::<(u64, bool)>(owner.with("menu-open"))).expect("production menu binding").1
        }
    }
    fn assert_menu_closed(&self, face: &str) {
        assert!(!self.menu_open(face), "Escape closes actual menu state (native={})", self.native);
        if !self.native {
            // Presence is not liveness: the SDK keeps a closed menu body for its exit fade.
            // any_open excludes exiting layers, unlike locator publication or overlay::is_open.
            assert!(
                !crate::dialogs::popup_open(&self.ctx),
                "no active SDK popup remains above the dialog after cancellation"
            );
        }
        assert_eq!(self.app.ui.dialog, Some(crate::state::Dialog::ViewOptions), "child cancellation must not close parent");
    }
    fn click(&mut self, name: &str) {
        let at = self.rect(name).center();
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
            );
        }
        self.frame(vec![], 0);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], 0);
        }
        self.frame(vec![], 0);
    }
    fn tab_to(&mut self, name: &str) {
        assert!(!self.native, "native fallback uses egui focus, not SDK focus metadata");
        let target = self.hits.iter().find(|(id, _)| id == name).unwrap().1.id;
        self.frame(vec![Event::PointerGone], 0);
        for _ in 0..32 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == target) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {name}; no forced focus fallback");
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        let photos: Vec<_> = s.catalog.photos().map(|photo| serde_json::to_value(photo).unwrap()).collect();
        json!({"photos": photos, "active": s.selection.active.map(|id| id.0),
            "selection": s.selection.ids.iter().map(|id| id.0).collect::<Vec<_>>(),
            "revision": s.catalog.revision, "undo": s.undo.len(), "redo": s.redo.len(), "commits": s.commits(), "journal": s.journal})
    }
    fn seed_profiles(&mut self) {
        self.app
            .run(
                "view.viewOptions",
                json!({"info1": ["camera", "lens", "none"],
            "info2": ["exposure", "dimensions", "fileName"], "briefly": "off"}),
            )
            .unwrap();
        self.frame(vec![], 0);
        assert_eq!(self.app.ui.settings.loupe_info.info1, [F::Camera, F::Lens, F::None]);
        assert_eq!(self.app.ui.settings.loupe_info.info2, [F::Exposure, F::Dimensions, F::FileName]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.memory_only();
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1400.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Output(self.ctx.run_ui(raw, |_| {})));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn loupe_profiles_pointer_edit_cancel_reopen_and_reset_only_one_profile() {
    for native in [false, true] {
        for profile in [1, 2] {
            let mut f = Fixture::new(native);
            f.seed_profiles();
            let document = f.document();
            let before = f.app.ui.settings.loupe_info;
            let face = format!("combo:viewOptionsInfo{profile}Line2");
            let lens = format!("button:viewOptionsInfo{profile}Line2-cameraAndLens");
            f.click(&face);
            assert!(f.has(&lens));
            f.click(&lens);
            let changed = f.app.ui.settings.loupe_info;
            if profile == 1 {
                assert_eq!(changed.info1, [F::Camera, F::CameraAndLens, F::None]);
                assert_eq!(changed.info2, before.info2);
            } else {
                assert_eq!(changed.info2, [F::Exposure, F::CameraAndLens, F::FileName]);
                assert_eq!(changed.info1, before.info1);
            }
            f.click(&face);
            f.key(Key::ArrowDown);
            f.key(Key::Escape);
            f.assert_menu_closed(&face);
            assert_eq!(f.app.ui.settings.loupe_info, changed, "cancel is not a commit");
            f.click(&face);
            assert!(f.menu_open(&face), "the real production menu state reopens");
            assert!(f.has(&lens), "the same production field reopens");
            f.key(Key::Escape);
            f.assert_menu_closed(&face);
            f.click(&format!("button:viewOptionsDefaults{profile}"));
            let reset = f.app.ui.settings.loupe_info;
            if profile == 1 {
                assert_eq!(reset.info1, LoupeInfo::INFO1);
                assert_eq!(reset.info2, changed.info2);
            } else {
                assert_eq!(reset.info2, LoupeInfo::INFO2);
                assert_eq!(reset.info1, changed.info1);
            }
            assert_eq!(reset.briefly, before.briefly);
            // The host serializes these real settings; no parallel UI-only picker model.
            let saved = serde_json::to_value(&f.app.ui.settings).unwrap();
            assert_eq!(saved["loupeInfo"], serde_json::to_value(reset).unwrap());
            for _ in 0..3 {
                f.frame(vec![], 0);
            }
            assert_eq!(f.app.ui.settings.loupe_info, reset);
            assert_eq!(f.document(), document, "no photo/develop/selection/Undo edits");
        }
    }
}

#[test]
fn loupe_all_six_fields_keep_semantic_locators_and_edit_their_own_line() {
    for profile in [1, 2] {
        for line in 1..=3 {
            let mut f = Fixture::new(false);
            f.seed_profiles();
            let before = f.app.ui.settings.loupe_info;
            let document = f.document();
            f.click(&format!("combo:viewOptionsInfo{profile}Line{line}"));
            for field in F::ALL {
                let key = serde_json::to_value(field).unwrap();
                let name = format!("button:viewOptionsInfo{profile}Line{line}-{}", key.as_str().unwrap());
                f.rect(&name);
                assert!(
                    f.painted.iter().any(|text| text == crate::i18n::tr(field.label())),
                    "translated literal caption for {field:?}: {:?}",
                    f.painted
                );
            }
            f.click(&format!("button:viewOptionsInfo{profile}Line{line}-captureDate"));
            let mut expected = before;
            let lines = if profile == 1 { &mut expected.info1 } else { &mut expected.info2 };
            lines[line - 1] = F::CaptureDate;
            assert_eq!(f.app.ui.settings.loupe_info, expected);
            assert_eq!(f.document(), document);
        }
    }
}

#[test]
fn loupe_natural_tab_enter_menu_pick_and_keyboard_defaults_use_real_controls() {
    for activation in [Key::Enter, Key::Space] {
        let mut f = Fixture::new(false);
        f.seed_profiles();
        let before = f.app.ui.settings.loupe_info;
        let document = f.document();
        f.tab_to("combo:viewOptionsInfo2Line2");
        f.key(activation);
        assert!(f.has("button:viewOptionsInfo2Line2-dimensions"));
        f.key(Key::ArrowDown);
        assert_eq!(f.app.ui.settings.loupe_info, before, "navigation cannot commit");
        f.key(Key::Enter);
        assert_eq!(f.app.ui.settings.loupe_info.info2, [F::Exposure, F::Exposure, F::FileName]);
        assert_eq!(f.app.ui.settings.loupe_info.info1, before.info1);
        f.tab_to("button:viewOptionsDefaults2");
        f.key(activation);
        assert_eq!(f.app.ui.settings.loupe_info.info2, LoupeInfo::INFO2);
        assert_eq!(f.app.ui.settings.loupe_info.info1, before.info1);
        assert_eq!(f.document(), document);
    }
}

#[test]
fn loupe_disabled_defaults_and_disabled_fields_do_not_change_settings() {
    for native in [false, true] {
        let mut f = Fixture::new(native);
        let document = f.document();
        let defaults = f.app.ui.settings.loupe_info;
        for profile in [1, 2] {
            f.click(&format!("button:viewOptionsDefaults{profile}"));
        }
        assert_eq!(f.app.ui.settings.loupe_info, defaults);
        f.seed_profiles();
        let settings = serde_json::to_value(&f.app.ui.settings).unwrap();
        f.enabled = false;
        f.frame(vec![], 0);
        for profile in [1, 2] {
            f.click(&format!("button:viewOptionsDefaults{profile}"));
            f.click(&format!("combo:viewOptionsInfo{profile}Line1"));
            assert!(!f.has(&format!("button:viewOptionsInfo{profile}Line1-none")));
        }
        assert_eq!(serde_json::to_value(&f.app.ui.settings).unwrap(), settings);
        assert_eq!(f.document(), document);
    }
}

#[test]
fn loupe_briefly_remains_mutually_exclusive_and_overlay_choices_stay_disabled_when_off() {
    for native in [false, true] {
        let mut f = Fixture::new(native);
        f.seed_profiles();
        let before = f.app.ui.settings.loupe_info;
        let document = f.document();
        for (profile, expected) in [(1, I::Basic), (2, I::Exposure), (2, I::Off)] {
            f.click(&format!("check:viewOptions.briefly{profile}"));
            assert_eq!(f.app.ui.settings.loupe_info.briefly, expected);
        }
        if f.app.ui.info_overlay != I::Off {
            f.click("check:viewOptions.showInfo");
        }
        assert_eq!(f.app.ui.info_overlay, I::Off);
        f.click("button:viewOptionsInfo-1");
        assert_eq!(f.app.ui.info_overlay, I::Off);
        assert_eq!(f.app.ui.settings.loupe_info.info1, before.info1);
        assert_eq!(f.app.ui.settings.loupe_info.info2, before.info2);
        assert_eq!(f.document(), document);
    }
}

#[test]
fn loupe_command_refuses_invalid_profile_atomically_and_reopen_keeps_committed_settings() {
    let mut f = Fixture::new(false);
    f.seed_profiles();
    let before = f.app.ui.settings.loupe_info;
    let document = f.document();
    assert!(f.app.run("view.viewOptions", json!({"info1": ["none", "none", "none"], "info2": ["rating", "lens", "none"]})).is_err());
    assert_eq!(f.app.ui.settings.loupe_info, before);
    f.click("combo:viewOptionsInfo1Line1");
    f.click("button:viewOptionsInfo1Line1-fileName");
    let committed = f.app.ui.settings.loupe_info;
    f.app.run("view.back", json!({})).unwrap();
    assert!(f.app.ui.dialog.is_none());
    f.app.run("view.viewOptions", json!({})).unwrap();
    f.frame(vec![], 0);
    assert_eq!(f.app.ui.dialog, Some(crate::state::Dialog::ViewOptions));
    assert_eq!(f.app.ui.settings.loupe_info, committed);
    assert_eq!(f.document(), document);
}

#[test]
fn loupe_sdk_measurement_does_not_publish_ported_hits_or_commit_input() {
    for probe in [1, 2] {
        let mut f = Fixture::new(false);
        f.seed_profiles();
        let before = f.app.ui.settings.loupe_info;
        let document = f.document();
        let at = f.rect("combo:viewOptionsInfo1Line1").center();
        for pressed in [true, false] {
            f.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                probe,
            );
            assert!(!f.has("combo:viewOptionsInfo1Line1"));
            assert!(!f.has("button:viewOptionsDefaults1"));
        }
        f.frame(vec![], 0);
        assert!(!f.has("button:viewOptionsInfo1Line1-none"));
        assert_eq!(f.app.ui.settings.loupe_info, before);
        assert_eq!(f.document(), document);
    }
}

#[test]
fn loupe_actual_parent_escape_cancels_only_menu_and_reopens_without_settings_edits() {
    let mut f = Fixture::with_parent(false, true);
    f.seed_profiles();
    let before = f.app.ui.settings.loupe_info;
    let document = f.document();
    let face = "combo:viewOptionsInfo1Line2";
    f.rect("dialog:window");
    f.click(face);
    assert!(f.menu_open(face));
    assert!(f.has("button:viewOptionsInfo1Line2-lens"));
    f.key(Key::ArrowDown);
    assert_eq!(f.app.ui.settings.loupe_info, before, "navigation is not activation");
    f.key(Key::Escape);
    f.assert_menu_closed(face);
    f.rect("dialog:window");
    assert_eq!(f.app.ui.settings.loupe_info, before, "first Escape cancels the field only");
    f.click(face);
    assert!(f.menu_open(face), "same field reopens in the actual parent");
    assert!(f.has("button:viewOptionsInfo1Line2-cameraAndLens"));
    f.key(Key::Escape);
    f.assert_menu_closed(face);
    assert_eq!(f.app.ui.settings.loupe_info, before);
    assert_eq!(f.document(), document);
}
