//! The whole Titles section plus the enable/content/editor leaves on their own.
//! Every control is a framework control, so keyboard traversal is one focus chain; this is not
//! desktop playback or preview/render acceptance.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{MemStore, Op, Photo, Source};
use lightcraft_engine::{Selection, Session, library::LibraryStores};

#[derive(Clone, Copy, Debug)]
enum Presentation {
    NativeSections,
    SdkWorkflow,
}
#[derive(Clone, Copy)]
enum Pass {
    Normal,
    Disabled,
    Measure,
    Sizing,
}
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
fn names(title: Title) -> (&'static str, &'static str, &'static str) {
    match title {
        Title::Intro => ("slideshowIntro", "settingsSlideshowIntroContent", "Intro screen"),
        Title::Ending => ("slideshowEnding", "settingsSlideshowEndingContent", "Ending screen"),
    }
}
fn enable_id(title: Title) -> String {
    format!("check:settings.{}", names(title).0)
}
fn content_id(title: Title, index: usize) -> String {
    format!("button:{}-{index}", names(title).1)
}
fn field_id(title: Title) -> String {
    format!("field:{}Text", names(title).0)
}
fn other(title: Title) -> Title {
    match title {
        Title::Intro => Title::Ending,
        Title::Ending => Title::Intro,
    }
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    presentation: Presentation,
    pass: Pass,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    labels: Vec<String>,
}
impl Fixture {
    fn new(presentation: Presentation) -> Self {
        let files = MemStore::new();
        let mut s = Session::new();
        s.open_library_in(
            LibraryStores { dir: "memory-title-screens".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false },
            false,
        )
        .unwrap();
        for name in ["A.jpg", "B.jpg"] {
            let id = s.catalog.alloc_photo_id();
            s.catalog
                .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 8, 6, "2026-11-21T00:00:00")) })
                .unwrap();
            s.selection = Selection::single(id);
        }
        let id = s.active().unwrap();
        let mut develop = (*s.develop_of(id).unwrap()).clone();
        develop.light.exposure = 0.5;
        s.set_develop(id, develop, "Unrelated exposure").unwrap();
        assert!(!s.undo.is_empty());
        // Fixture setup only. All subsequent commits/cancels drive real UI or real engine commands.
        s.slide_titles = s
            .slide_titles
            .merged(&json!({
                "intro": {"text": "Original intro", "color": "#203040", "scale": 0.15, "duration": 3.0},
                "ending": {"text": "Original ending", "color": "#405060", "scale": 0.21, "duration": 4.0}
            }))
            .unwrap();
        s.slide_overlays.identity_plate.text = "Studio identity".into();
        assert!(s.slide_overlays.identity_plate.image.is_empty());
        let app = LightcraftApp::new(s, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, files, presentation, pass: Pass::Normal, tick: 0, rects: vec![], hits: vec![], labels: vec![] };
        for _ in 0..3 {
            f.frame(vec![]);
        }
        f
    }
    fn memory_only(&self) {
        assert!(self.app.session.library_folder.is_none() && self.app.session.smart_previews_dir.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.session.slide_overlays.identity_plate.image.is_empty());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 2);
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.memory_only();
        self.tick += 1;
        let _ = crate::widgets::take_registries(&self.ctx);
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1300.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let presentation = self.presentation;
        let pass = self.pass;
        let mut final_pass = None;
        let output = Drain(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let mut draw = |ui: &mut egui::Ui| {
                    let t = Tokens::get(ui.ctx());
                    match presentation {
                        Presentation::NativeSections => {
                            sections(app, ui, &t);
                        }
                        Presentation::SdkWorkflow => {
                            let settings = app.session.slide_titles.clone();
                            // These are the actual functions called by production `screen`, not
                            // alternate controls. Native colour/scale/duration are outside this leaf.
                            for title in [Title::Intro, Title::Ending] {
                                let s = settings.screen(title);
                                let (name, content, label) = names(title);
                                if screen_enable(app, ui, title, s, name, label) {
                                    screen_content(app, ui, &t, title, s, name, content);
                                }
                            }
                        }
                    }
                };
                match pass {
                    Pass::Normal => draw(ui),
                    Pass::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                }
            });
            // Like the real app's draw_window, drain once per UI pass. run_ui may discard
            // and rerun a pass; accumulating those registries invents duplicate locators.
            // Still reject actual duplicate owners in EVERY pass, including discarded ones.
            let registry = crate::widgets::take_registries(ui.ctx());
            for title in [Title::Intro, Title::Ending] {
                let id = field_id(title);
                let rectangles = registry.0.iter().filter(|(name, _)| name == &id).count();
                let owners = registry.1.iter().filter(|(name, _)| name == &id).count();
                assert!(rectangles <= 1, "duplicate {id} within one UI pass in {presentation:?}");
                assert_eq!(owners, rectangles, "each {id} locator has exactly one hit owner");
            }
            final_pass = Some(registry);
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
        (self.rects, self.hits) = final_pass.expect("a completed UI pass");
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.presentation)).1
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
        self.tick += 30; // Distinct intentional clicks, never a double-click selection gesture.
        let at = self.rect(id).center();
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
    fn tab_to(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..64 {
            let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
            // The whole section is framework controls now (the colour wells and sliders too), so
            // both presentations walk one focus chain.
            let sdk_focus = craft::ui::runtime::focus::focused_rec(&self.ctx);
            if sdk_focus.is_some_and(|r| r.stop.id == hit.id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab failed to reach {id} in {:?}", self.presentation);
    }
    fn activate(&mut self, id: &str, keyboard: bool, key: Key) {
        if keyboard {
            self.tab_to(id);
            self.key(key);
        } else {
            self.click(id);
        }
    }
    fn replace(&mut self, title: Title, value: &str, keyboard: bool) {
        let field = field_id(title);
        if keyboard {
            self.tab_to(&field);
        } else {
            self.click(&field);
        }
        self.key_mod(Key::A, Modifiers::COMMAND);
        self.frame(vec![Event::Text(value.to_owned())]);
        self.frame(vec![]);
        assert!(self.labels.iter().any(|label| label == value), "typed draft is visibly rendered");
    }
    fn document(&self) -> Value {
        json!({"photos": self.app.session.catalog.photos().collect::<Vec<_>>(),
            "selection": self.app.session.selection, "revision": self.app.session.catalog.revision,
            "undo": format!("{:?}", self.app.session.undo), "redo": format!("{:?}", self.app.session.redo),
            "layout": self.app.session.slide_layout, "playback": self.app.session.slideshow,
            "overlays": self.app.session.slide_overlays})
    }
    fn state(&self) -> Value {
        json!({"document": self.document(), "titles": self.app.session.slide_titles.to_json(),
            "journal": self.app.session.journal, "commits": self.app.session.commits(),
            "previewChoice": preview_shows(&self.ctx).map(|t| t.id()), "toast": self.app.ui.toast.as_ref().map(|t| &t.0)})
    }
    fn screen(&self, title: Title) -> Value {
        self.app.session.slide_titles.screen(title).to_json()
    }
    fn assert_command(&self, from: usize, title: Title, payload: Value) {
        assert_eq!(self.app.session.journal.get(from..).unwrap(), &[("slideshow.titles".into(), json!({title.id():payload}))]);
        self.saved_matches_model();
        assert!(self.app.ui.toast.is_none());
    }
    fn saved_matches_model(&self) {
        let prefs: Value = serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap();
        assert_eq!(prefs["slideTitles"], serde_json::to_value(&self.app.session.slide_titles).unwrap());
    }
    fn ready_text(&mut self, title: Title) {
        self.click(&enable_id(title));
        self.click(&content_id(title, 1));
        assert!(self.has(&field_id(title)));
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
            let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1300.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Drain(self.ctx.run_ui(raw, |_| {})));
            let _ = crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap();
        }
    }
}

fn content_journey(presentation: Presentation, keyboard: bool) {
    let mut f = Fixture::new(presentation);
    let document = f.document();
    for title in [Title::Intro, Title::Ending] {
        let untouched = f.screen(other(title));
        let mut expected = f.screen(title);
        let j = f.app.session.journal.len();
        f.activate(&enable_id(title), keyboard, Key::Space);
        expected["enabled"] = json!(true);
        assert_eq!(f.screen(title), expected);
        f.assert_command(j, title, json!({"enabled":true}));
        assert_eq!(f.screen(other(title)), untouched);
        for (index, content) in [(1, "text"), (0, "identityPlate"), (2, "none"), (1, "text")] {
            let j = f.app.session.journal.len();
            f.activate(&content_id(title, index), keyboard, Key::Enter);
            expected["content"] = json!(content);
            f.assert_command(j, title, json!({"content":content}));
            assert_eq!(f.screen(title), expected);
            assert_eq!(f.screen(other(title)), untouched);
            assert_eq!(f.has(&field_id(title)), content == "text");
            assert_eq!(f.document(), document);
            let before = f.state();
            f.activate(&content_id(title, index), keyboard, Key::Space);
            assert_eq!(f.state(), before, "choosing the current content is a command no-op");
            f.idle_is_inert();
        }
    }
}
#[test]
fn title_screens_pointer_enable_text_identity_colour_only_keep_the_other_screen_and_photo_history() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        content_journey(p, false);
    }
}
#[test]
fn title_screens_natural_tab_space_enter_follow_native_sections_and_sdk_leaf_workflows() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        content_journey(p, true);
    }
}
#[test]
fn title_screen_text_escape_discards_and_enter_commits_once_without_cross_screen_changes() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        for title in [Title::Intro, Title::Ending] {
            let mut f = Fixture::new(p);
            f.ready_text(title);
            let before = f.state();
            let old = f.screen(title)["text"].as_str().unwrap().to_owned();
            f.replace(title, "Cancelled title 🦀", false);
            assert_eq!(f.state(), before, "typing is draft-only");
            f.key(Key::Escape);
            assert_eq!(f.state(), before, "Escape sends no command");
            assert!(f.labels.iter().any(|label| label == &old));
            let document = f.document();
            let untouched = f.screen(other(title));
            let j = f.app.session.journal.len();
            f.replace(title, "Committed title é", true);
            f.key(Key::Enter);
            f.assert_command(j, title, json!({"text":"Committed title é"}));
            assert_eq!(f.screen(title)["text"], "Committed title é");
            assert_eq!(f.screen(other(title)), untouched);
            assert_eq!(f.document(), document);
            f.idle_is_inert();
        }
    }
}
#[test]
fn title_screen_focus_move_commits_to_its_own_screen_and_never_carries_the_draft_to_the_other() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        let mut f = Fixture::new(p);
        f.ready_text(Title::Intro);
        f.ready_text(Title::Ending);
        let document = f.document();
        let ending = f.screen(Title::Ending);
        let j = f.app.session.journal.len();
        f.replace(Title::Intro, "Intro after leaving", false);
        f.click(&field_id(Title::Ending)); // Actual focus loss commits the Intro editor.
        f.assert_command(j, Title::Intro, json!({"text":"Intro after leaving"}));
        assert_eq!(f.screen(Title::Ending), ending);
        assert!(f.labels.iter().any(|s| s == "Original ending"));
        let j = f.app.session.journal.len();
        f.replace(Title::Ending, "Ending only", false);
        f.key(Key::Tab); // Native/SDK Away commit, not direct state mutation.
        f.assert_command(j, Title::Ending, json!({"text":"Ending only"}));
        assert_eq!(f.screen(Title::Intro)["text"], "Intro after leaving");
        f.click(&field_id(Title::Intro));
        assert!(f.labels.iter().any(|s| s == "Intro after leaving"));
        assert_eq!(f.document(), document);
        f.idle_is_inert();
    }
}
#[test]
fn title_screen_hidden_editor_restarts_from_its_authoritative_text_after_real_command_disable() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        for title in [Title::Intro, Title::Ending] {
            let mut f = Fixture::new(p);
            f.ready_text(title);
            let document = f.document();
            let other_before = f.screen(other(title));
            let model_text = f.screen(title)["text"].as_str().unwrap().to_owned();
            f.replace(title, "Must not come back", false);
            let j = f.app.session.journal.len();
            // A real command can hide a focused field without first giving it a pointer blur.
            f.app.run("slideshow.titles", json!({title.id():{"enabled":false}})).unwrap();
            f.frame(vec![]);
            assert!(!f.has(&field_id(title)));
            f.assert_command(j, title, json!({"enabled":false}));
            let j = f.app.session.journal.len();
            f.click(&enable_id(title));
            f.assert_command(j, title, json!({"enabled":true}));
            assert!(f.labels.iter().any(|s| s == &model_text));
            assert!(!f.labels.iter().any(|s| s == "Must not come back"));
            let j = f.app.session.journal.len();
            f.replace(title, "Fresh after re-enable", false);
            f.key(Key::Enter);
            f.assert_command(j, title, json!({"text":"Fresh after re-enable"}));
            assert_eq!(f.screen(other(title)), other_before);
            assert_eq!(f.document(), document);
        }
    }
}
#[test]
fn title_screen_unicode_text_reaches_engine_validation_without_an_editor_limit_or_undo_entry() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        let mut f = Fixture::new(p);
        f.ready_text(Title::Intro);
        let document = f.document();
        let ending = f.screen(Title::Ending);
        let j = f.app.session.journal.len();
        let typed = "é🦀".repeat(550); // More than the SDK default single-line limit (1024).
        f.click(&field_id(Title::Intro));
        f.key_mod(Key::A, Modifiers::COMMAND);
        f.frame(vec![Event::Paste(typed.clone())]);
        assert_eq!(f.app.session.journal.len(), j);
        f.key(Key::Enter);
        f.assert_command(j, Title::Intro, json!({"text":typed}));
        let expected: String = typed.chars().take(lightcraft_engine::slide_overlay::TEXT_MAX_CHARS).collect();
        assert_eq!(f.screen(Title::Intro)["text"], expected);
        assert_eq!(f.document(), document);
        assert_eq!(f.screen(Title::Ending), ending);
        f.idle_is_inert();
    }
}
#[test]
fn title_screen_disabled_measurement_and_sizing_input_cannot_commit_or_replay() {
    for p in [Presentation::NativeSections, Presentation::SdkWorkflow] {
        for pass in [Pass::Disabled, Pass::Measure, Pass::Sizing] {
            for field in [false, true] {
                let mut f = Fixture::new(p);
                f.ready_text(Title::Intro);
                let target = if field { field_id(Title::Intro) } else { content_id(Title::Intro, 2) };
                let at = f.rect(&target).center();
                if field {
                    f.replace(Title::Intro, "Invisible commit forbidden", false);
                }
                let before = f.state();
                f.frame(vec![Event::PointerMoved(at)]);
                f.pointer(at, true);
                f.pass = pass;
                f.pointer(at, false);
                f.key(Key::Enter);
                f.key(Key::Space);
                assert_eq!(f.state(), before);
                f.pass = Pass::Normal;
                f.frame(vec![]);
                assert_eq!(f.state(), before);
                f.idle_is_inert();
            }
        }
    }
}
#[test]
fn title_screen_real_command_rejects_invalid_content_without_partial_screen_or_history_changes() {
    let mut f = Fixture::new(Presentation::NativeSections);
    let before = f.state();
    assert!(f.app.run("slideshow.titles", json!({"intro":{"enabled":true,"content":"unrecognized"},"ending":{"text":"Not applied"}})).is_err());
    assert_eq!(f.state(), before);
}
