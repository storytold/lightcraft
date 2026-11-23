//! Local production-control coverage, not whole-app/native-window or shipped-SDK acceptance.
//! Two in-memory Demo photo records; no preview, playback, disk, chooser, or app logic tick.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton};
use lightcraft_catalog::{MemStore, Op, Photo, PhotoId, Source};
use lightcraft_engine::{Selection, Session, library::LibraryStores};

const BACKGROUND: &str = "check:settings.slideshowBackgroundImage";
const USE_SELECTED: &str = "button:slideshowUseSelected";
const RESET: &str = "button:slideshowResetLayout";

struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}

#[derive(Clone, Copy)]
enum Pass {
    Normal,
    Disabled,
    Measure,
    Sizing,
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    ids: [PhotoId; 2],
    native: bool,
    pass: Pass,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    labels: Vec<String>,
}
impl Fixture {
    fn new(native: bool) -> Self {
        let files = MemStore::new();
        let mut session = Session::new();
        session
            .open_library_in(
                LibraryStores {
                    dir: "memory-slideshow-background".into(),
                    catalog: Box::new(MemStore::new()),
                    files: Box::new(files.clone()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        let ids = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (id, name) in ids.into_iter().zip(["Background A.jpg", "Selected B.jpg"]) {
            session
                .catalog
                .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 8, 6, "2026-11-21T00:00:00")) })
                .unwrap();
        }
        session.selection = Selection::single(ids[0]);
        // Seed a real, unrelated photo Undo entry. The background preferences must not consume it.
        let mut develop = (*session.develop_of(ids[0]).unwrap()).clone();
        develop.light.exposure = 0.5;
        session.set_develop(ids[0], develop, "Fixture exposure").unwrap();
        assert!(!session.undo.is_empty());
        // Setup only: subsequent behavior assertions drive actual UI/commands, never setters.
        session.slide_layout = session
            .slide_layout
            .merged(&json!({
                "linkMargins": false, "marginLeft": 0.12, "marginRight": 0.23,
                "backdropColor": "#203040", "backgroundOpacity": 0.37, "washAngle": 61.0,
            }))
            .unwrap();
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, files, ids, native, pass: Pass::Normal, tick: 0, rects: vec![], hits: vec![], labels: vec![] };
        for _ in 0..3 {
            f.frame(vec![]);
        }
        f
    }
    fn memory_only(&self) {
        assert!(self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.services.pick_folder.is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 2);
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.memory_only();
        self.tick += 1;
        let _ = crate::widgets::take_registries(&self.ctx);
        let raw = HeadlessView::raw_input(vec2(1000.0, 800.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let native = self.native;
        let pass = self.pass;
        let output = Drain(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let mut draw = |ui: &mut egui::Ui| {
                    let s = app.session.slide_layout.clone();
                    let t = Tokens::get(ui.ctx());
                    background_photo_controls(app, ui, &t, &s);
                    reset_layout_button(app, ui);
                };
                let mut present = || match pass {
                    Pass::Normal => draw(ui),
                    Pass::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                };
                if native {
                    (present)();
                } else {
                    present();
                }
            });
        }));
        fn text(shape: &egui::Shape, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => labels.push(t.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, labels);
                    }
                }
                _ => {}
            }
        }
        self.labels.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.labels);
        }
        (self.rects, self.hits) = crate::widgets::take_registries(&self.ctx);
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}")).1
    }
    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == id)
    }
    fn pointer(&mut self, at: egui::Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click(&mut self, id: &str) {
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.frame(vec![]);
    }
    fn tab_to(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..32 {
            let rect = self.rect(id);
            let native_focus = self.ctx.memory(|m| m.focused()).and_then(|id| self.ctx.read_response(id));
            let sdk_focus = craft::ui::runtime::focus::focused_rec(&self.ctx);
            let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
            if (self.native && native_focus.is_some_and(|r| r.rect == rect)) || (!self.native && sdk_focus.is_some_and(|r| r.stop.id == hit.id)) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {id}; native={}", self.native);
    }
    fn activate(&mut self, id: &str, keyboard: bool, key: Key) {
        if keyboard {
            self.tab_to(id);
            self.key(key);
        } else {
            self.click(id);
        }
    }
    fn document(&self) -> Value {
        json!({"photos": self.app.session.catalog.photos().collect::<Vec<_>>(),
            "revision": self.app.session.catalog.revision, "selection": self.app.session.selection,
            "undo": format!("{:?}", self.app.session.undo), "redo": format!("{:?}", self.app.session.redo),
            "playback": self.app.session.slideshow, "overlays": self.app.session.slide_overlays,
            "titles": self.app.session.slide_titles})
    }
    fn state(&self) -> Value {
        json!({"document": self.document(), "layout": self.app.session.slide_layout.to_json(),
            "journal": self.app.session.journal, "commits": self.app.session.commits(),
            "toast": self.app.ui.toast.as_ref().map(|t| &t.0)})
    }
    fn layout_without_background(&self) -> Value {
        let mut value = self.app.session.slide_layout.to_json();
        value.as_object_mut().unwrap().remove("backgroundImage");
        value
    }
    fn select(&mut self, photo: Option<PhotoId>) {
        // The fixture supplies a new library selection; the controls must read its current id.
        self.app.session.selection = photo.map_or_else(Selection::default, Selection::single);
        self.frame(vec![]);
    }
    fn assert_command(&self, from: usize, payload: Value) {
        assert_eq!(self.app.session.journal.get(from..).unwrap(), &[("slideshow.layout".into(), payload)]);
        let prefs: Value = serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap();
        assert_eq!(prefs["slideLayout"], serde_json::to_value(&self.app.session.slide_layout).unwrap());
        assert!(self.app.ui.toast.is_none());
    }
    fn idle_is_inert(&mut self) {
        let before = self.state();
        for _ in 0..3 {
            self.frame(vec![]);
        }
        assert_eq!(self.state(), before);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.memory_only();
            let raw = HeadlessView::raw_input(vec2(1000.0, 800.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Drain(self.ctx.run_ui(raw, |_| {})));
            let _ = crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap(); // MemStore only.
        }
    }
}

fn journey(native: bool, keyboard: bool) {
    let mut f = Fixture::new(native);
    let unrelated = f.layout_without_background();
    let document = f.document();
    let j = f.app.session.journal.len();
    f.activate(BACKGROUND, keyboard, Key::Space);
    assert_eq!(f.app.session.slide_layout.background_image, Some(f.ids[0].0));
    f.assert_command(j, json!({"backgroundImage": f.ids[0].0}));
    assert_eq!(f.document(), document);
    assert_eq!(f.layout_without_background(), unrelated);
    assert!(!f.has(USE_SELECTED), "same selected/background identity needs no replacement");
    assert!(f.labels.iter().any(|s| s == "Background A.jpg"));
    f.idle_is_inert();

    f.select(Some(f.ids[1]));
    assert_eq!(f.app.session.slide_layout.background_image, Some(f.ids[0].0), "selection alone never rebinds the backdrop");
    let document = f.document();
    let j = f.app.session.journal.len();
    assert!(f.has(USE_SELECTED));
    f.activate(USE_SELECTED, keyboard, Key::Enter);
    assert_eq!(f.app.session.slide_layout.background_image, Some(f.ids[1].0));
    f.assert_command(j, json!({"backgroundImage": f.ids[1].0}));
    assert_eq!(f.document(), document);
    assert_eq!(f.layout_without_background(), unrelated);
    assert!(!f.has(USE_SELECTED));
    assert!(f.labels.iter().any(|s| s == "Selected B.jpg"));
    f.idle_is_inert();

    let j = f.app.session.journal.len();
    f.activate(BACKGROUND, keyboard, Key::Space);
    assert!(f.app.session.slide_layout.background_image.is_none());
    f.assert_command(j, json!({"backgroundImage": null}));
    assert_eq!(f.document(), document);
    assert_eq!(f.layout_without_background(), unrelated);
    assert!(!f.has(USE_SELECTED) && !f.has("slider:slideshowBackgroundOpacity"));
    f.idle_is_inert();

    let j = f.app.session.journal.len();
    f.activate(RESET, keyboard, Key::Enter);
    assert_eq!(f.app.session.slide_layout, SlideSettings::default());
    f.assert_command(j, SlideSettings::default().to_json());
    assert_eq!(f.document(), document, "only explicit Reset replaces the other layout fields");
    f.idle_is_inert();
}

#[test]
fn slideshow_background_pointer_enable_replace_disable_reset_preserves_photo_undo_and_other_layout() {
    for native in [false, true] {
        journey(native, false);
    }
}
#[test]
fn slideshow_background_natural_tab_space_and_enter_complete_the_same_workflow() {
    for native in [false, true] {
        journey(native, true);
    }
}
#[test]
fn slideshow_background_no_active_photo_is_disabled_but_existing_backdrop_can_be_cleared() {
    for native in [false, true] {
        let mut f = Fixture::new(native);
        f.select(None);
        let before = f.state();
        f.click(BACKGROUND);
        assert_eq!(f.state(), before);
        assert!(!f.has(USE_SELECTED));
        f.tab_to(RESET); // Disabled background must not trap natural keyboard navigation.
        let before = f.state();
        f.key(Key::Space); // Real explicit reset remains usable without any active photo.
        assert_eq!(f.app.session.slide_layout, SlideSettings::default());
        assert_eq!(f.document(), before["document"]);

        f.select(Some(f.ids[0]));
        f.click(BACKGROUND);
        f.select(None);
        assert!(!f.has(USE_SELECTED));
        let document = f.document();
        let j = f.app.session.journal.len();
        f.click(BACKGROUND);
        assert!(f.app.session.slide_layout.background_image.is_none());
        f.assert_command(j, json!({"backgroundImage": null}));
        assert_eq!(f.document(), document);
    }
}
#[test]
fn slideshow_background_reset_space_and_missing_photo_label_preserve_selection_and_history() {
    for native in [false, true] {
        let mut f = Fixture::new(native);
        // Real engine command intentionally permits a stale id; UI must display its fallback.
        f.app.run("slideshow.layout", json!({"backgroundImage": 999999})).unwrap();
        f.frame(vec![]);
        assert!(f.labels.iter().any(|s| s == crate::i18n::tr("(not in the library)")));
        let document = f.document();
        let j = f.app.session.journal.len();
        f.activate(RESET, true, Key::Space);
        f.assert_command(j, SlideSettings::default().to_json());
        assert_eq!(f.app.session.slide_layout, SlideSettings::default());
        assert_eq!(f.document(), document);
    }
}
#[test]
fn slideshow_background_disabled_measurement_and_sizing_release_cannot_commit_or_replay() {
    for native in [false, true] {
        for pass in [Pass::Disabled, Pass::Measure, Pass::Sizing] {
            for id in [BACKGROUND, USE_SELECTED, RESET] {
                let mut f = Fixture::new(native);
                f.click(BACKGROUND);
                f.select(Some(f.ids[1]));
                let before = f.state();
                let at = f.rect(id).center();
                f.frame(vec![Event::PointerMoved(at)]);
                f.pointer(at, true); // A live press followed by a noninteractive release.
                assert_eq!(f.state(), before, "press does not activate");
                f.pass = pass;
                f.pointer(at, false);
                f.key(Key::Enter);
                f.key(Key::Space);
                assert_eq!(f.state(), before);
                f.pass = Pass::Normal;
                f.frame(vec![]);
                assert_eq!(f.state(), before, "invisible input cannot replay on return");
            }
        }
    }
}
#[test]
fn slideshow_background_real_command_wrong_type_refusal_does_not_change_the_layout() {
    let mut f = Fixture::new(false);
    let before = f.state();
    assert!(f.app.run("slideshow.layout", json!({"backgroundImage": "not a photo id"})).is_err());
    assert_eq!(f.state(), before);
}

#[test]
fn slideshow_background_pointer_release_outside_cancels_without_changing_preferences() {
    for native in [false, true] {
        for id in [BACKGROUND, USE_SELECTED, RESET] {
            let mut f = Fixture::new(native);
            f.click(BACKGROUND);
            f.select(Some(f.ids[1]));
            let before = f.state();
            let at = f.rect(id).center();
            f.frame(vec![Event::PointerMoved(at)]);
            f.pointer(at, true);
            f.pointer(egui::pos2(990.0, 790.0), false);
            f.frame(vec![]);
            assert_eq!(f.state(), before);
            f.idle_is_inert();
        }
    }
}
