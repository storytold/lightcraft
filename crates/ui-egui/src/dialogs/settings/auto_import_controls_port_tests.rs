//! Source-qualified interaction coverage; never starts app logic, watchers, OS pickers, or filesystem probes.
use std::cell::Cell;
use std::rc::Rc;

use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::MemStore;
use lightcraft_engine::{Session, library::LibraryStores};
use serde_json::{Value, json};

use crate::headless::HeadlessView;
use crate::{LightcraftApp, Services};

const CHOOSE: &str = "button:settingsAutoFolder";
const OFF: &str = "button:settingsAutoOff";
const COPY: &str = "check:settingsAutoCopy";

// Every draw gets fresh fake services. The picker ONLY cancels: no selected string can reach
// engine library.autoImport's hardcoded Path::is_dir. Copy and {folder:null} need no OS access.
fn fake_services(calls: Rc<Cell<usize>>, available: bool) -> Services {
    Services {
        local_places: Some(vec![]),
        pick_folder: available.then(|| {
            Box::new(move || {
                calls.set(calls.get() + 1);
                None
            }) as crate::services::PickFolder
        }),
        png: None,
        ..Default::default()
    }
}
fn release(mut output: egui::FullOutput) {
    output.textures_delta.clear();
    drop(output);
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    calls: Rc<Cell<usize>>,
    available: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
}
impl Fixture {
    fn new(watching: bool, available: bool) -> Self {
        let files = MemStore::new();
        let mut session = Session::new();
        session
            .open_library_in(
                LibraryStores {
                    dir: "fake-auto-import-library".into(),
                    catalog: Box::new(MemStore::new()),
                    files: Box::new(files.clone()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        // Fixture setup, not a chosen folder or acceptance test. No path is ever listed/stat'ed.
        session.import_defaults.auto_folder = watching.then(|| "fake-watched-folder".into());
        session.import_defaults.auto_copy = false;
        session.import_defaults.auto_album = Some("Kept album".into());
        let calls = Rc::new(Cell::new(0));
        let app = LightcraftApp::new(session, fake_services(calls.clone(), available));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut fixture = Self { app, ctx, files, calls, available, tick: 0, rects: vec![], hits: vec![] };
        for _ in 0..3 {
            fixture.frame(vec![], 0, true);
        }
        fixture
    }
    fn frame(&mut self, events: Vec<Event>, mode: u8, enabled: bool) {
        self.app.services = fake_services(self.calls.clone(), self.available);
        let _ = crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1400.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        release(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut child;
                let ui = if mode == 2 {
                    child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                    &mut child
                } else {
                    ui
                };
                ui.add_enabled_ui(enabled, |ui| {
                    // The actual Settings presentation keeps neighboring text/Album native.
                    (|| {
                        let tokens = crate::theme::Tokens::get(&ctx);
                        let mut draw = |ui: &mut egui::Ui| super::import_tab(app, ui, &tokens);
                        if mode == 1 {
                            craft::ui::runtime::measure::run(&ctx, || draw(ui));
                        } else {
                            draw(ui);
                        }
                    })();
                });
            });
        }));
        (self.rects, self.hits) = crate::widgets::take_registries(&self.ctx);
    }
    fn rect(&self, name: &str) -> Rect {
        self.rects.iter().find(|(key, _)| key == name).unwrap_or_else(|| panic!("missing {name}")).1
    }
    fn has(&self, name: &str) -> bool {
        self.rects.iter().any(|(key, _)| key == name)
    }
    fn click(&mut self, name: &str, enabled: bool) {
        self.app.services = fake_services(self.calls.clone(), self.available);
        let at = self.rect(name).center();
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
                enabled,
            );
        }
        self.frame(vec![], 0, enabled);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], 0, true);
        }
    }
    fn tab_to(&mut self, name: &str) {
        self.app.services = fake_services(self.calls.clone(), self.available);
        let id = self.hits.iter().find(|(key, _)| key == name).unwrap().1.id;
        for _ in 0..64 {
            self.key(Key::Tab);
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == id) {
                return;
            }
        }
        panic!("natural mixed Settings traversal did not reach {name}");
    }
    fn snapshot(&self) -> Value {
        json!({"defaults": self.app.session.import_defaults, "journal": self.app.session.journal,
            "undo": self.app.session.undo.len(), "catalog": self.app.session.catalog.revision,
            "pickerCalls": self.calls.get(), "toast": self.app.ui.toast.as_ref().map(|t| &t.0)})
    }
    fn saved_matches_model(&self) {
        let saved: Value = serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap();
        assert_eq!(saved["import"], serde_json::to_value(&self.app.session.import_defaults).unwrap());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Retire shown components while the installed runtime/context is still alive. Drain every
        // FullOutput texture delta, including the teardown pass, rather than keeping GPU handles.
        self.app.services = fake_services(self.calls.clone(), self.available);
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1400.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            release(self.ctx.run_ui(raw, |_| {}));
            let _ = crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap(); // MemStore only, no real filesystem.
        }
    }
}

#[test]
fn auto_import_copy_pointer_and_space_commit_true_once_to_the_fake_library() {
    for keyboard in [false, true] {
        let mut f = Fixture::new(true, true);
        let undo = f.app.session.undo.len();
        let revision = f.app.session.catalog.revision;
        let before = f.app.session.journal.len();
        if keyboard {
            f.tab_to(COPY);
            f.key(Key::Space);
        } else {
            f.click(COPY, true);
        }
        assert!(f.app.session.import_defaults.auto_copy, "verify true model value, not just presence");
        assert_eq!(f.app.session.journal.get(before..).unwrap(), &[("library.autoImport".into(), json!({"copy": true}))]);
        assert_eq!(f.app.session.import_defaults.auto_folder.as_deref(), Some("fake-watched-folder"));
        assert_eq!(f.app.session.import_defaults.auto_album.as_deref(), Some("Kept album"));
        assert_eq!(f.app.session.undo.len(), undo);
        assert_eq!(f.app.session.catalog.revision, revision);
        assert_eq!(f.calls.get(), 0);
        f.saved_matches_model();
        let stable = f.snapshot();
        for _ in 0..3 {
            f.frame(vec![], 0, true);
        }
        assert_eq!(f.snapshot(), stable);
    }
}

#[test]
fn auto_import_cancelled_choose_is_not_turn_off_and_enter_only_calls_fake_picker() {
    for keyboard in [false, true] {
        let mut f = Fixture::new(true, true);
        let before = f.app.session.journal.clone();
        if keyboard {
            f.tab_to(CHOOSE);
            f.key(Key::Enter);
        } else {
            f.click(CHOOSE, true);
        }
        assert_eq!(f.calls.get(), 1);
        assert_eq!(f.app.session.journal, before);
        assert_eq!(f.app.session.import_defaults.auto_folder.as_deref(), Some("fake-watched-folder"));
        assert!(f.has(OFF) && f.has(COPY) && f.has("field:settingsAutoAlbum"));
        assert!(f.app.ui.toast.is_none());
    }
}

#[test]
fn auto_import_turn_off_pointer_and_enter_emit_null_then_remove_dependent_controls() {
    for keyboard in [false, true] {
        let mut f = Fixture::new(true, true);
        let before = f.app.session.journal.len();
        if keyboard {
            f.tab_to(OFF);
            f.key(Key::Enter);
        } else {
            f.click(OFF, true);
        }
        assert!(f.app.session.import_defaults.auto_folder.is_none());
        assert_eq!(f.app.session.journal.get(before..).unwrap(), &[("library.autoImport".into(), json!({"folder": null}))]);
        assert_eq!(f.calls.get(), 0);
        assert!(!f.has(OFF) && !f.has(COPY) && !f.has("field:settingsAutoAlbum"));
        assert_eq!(f.app.session.import_defaults.auto_album.as_deref(), Some("Kept album"));
        f.saved_matches_model();
        // No synthetic focus request: a subsequent real Tab can reach remaining Choose.
        f.tab_to(CHOOSE);
        let before = f.app.session.journal.clone();
        f.key(Key::Enter);
        assert_eq!(f.calls.get(), 1);
        assert_eq!(f.app.session.journal, before);
    }
}

#[test]
fn auto_import_absent_picker_disabled_controls_and_both_measurement_modes_are_inert() {
    let mut unavailable = Fixture::new(false, false);
    let before = unavailable.snapshot();
    assert!(!unavailable.has(OFF) && !unavailable.has(COPY));
    unavailable.click(CHOOSE, true);
    assert_eq!(unavailable.snapshot(), before);
    for id in [CHOOSE, OFF, COPY] {
        let mut disabled = Fixture::new(true, true);
        let before = disabled.snapshot();
        disabled.frame(vec![], 0, false);
        disabled.click(id, false);
        assert_eq!(disabled.snapshot(), before);
        for mode in [1, 2] {
            let mut f = Fixture::new(true, true);
            let before = f.snapshot();
            let at = f.rect(id).center();
            for pressed in [true, false] {
                f.frame(
                    vec![
                        Event::PointerMoved(at),
                        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
                    ],
                    mode,
                    true,
                );
                assert!(![CHOOSE, OFF, COPY].iter().any(|name| f.has(name)), "sizing cannot publish SDK hit targets");
            }
            f.frame(vec![], 0, true);
            assert_eq!(f.snapshot(), before, "sizing cannot invoke picker, command, or replay input");
        }
    }
}
