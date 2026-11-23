//! Work326 actual Settings parent, not an isolated SDK-control stand-in.
//! MemStore demo only; no app logic, watcher, OS picker, GPU page or filesystem probe.
use std::cell::Cell;
use std::rc::Rc;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Rect};
use lightcraft_catalog::MemStore;
use lightcraft_engine::{Session, library::LibraryStores};
use serde_json::{Value, json};

use crate::headless::HeadlessView;
use crate::{LightcraftApp, Services};

const CHOOSE: &str = "button:settingsAutoFolder";
const COPY: &str = "check:settingsAutoCopy";
const ALBUM: &str = "field:settingsAutoAlbum";

#[derive(Clone, Copy)]
enum Pass { Real, Measure, Sizing }

fn services(calls: Rc<Cell<usize>>, available: bool) -> Services {
    Services {
        local_places: Some(vec![]),
        pick_folder: available.then(|| Box::new(move || {
            calls.set(calls.get() + 1);
            None // Never return a path to engine Path::is_dir.
        }) as crate::services::PickFolder),
        png: None,
        ..Default::default()
    }
}

struct Parent {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    calls: Rc<Cell<usize>>,
    available: bool,
    enabled: bool,
    pass: Pass,
    time: f64,
    tab: String,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    trace: Vec<String>,
}

impl Parent {
    fn new(available: bool) -> Self {
        let files = MemStore::new();
        let mut session = Session::new();
        session.clock = Box::new(|| "2026-11-21T04:00:00".to_owned());
        session.open_library_in(LibraryStores {
            dir: "fake-settings-mixed-parent".into(),
            catalog: Box::new(MemStore::new()),
            files: Box::new(files.clone()),
            on_disk: false,
        }, true).unwrap(); // Procedural demo in memory makes document/history assertions non-vacuous.
        let id = session.catalog.photos().next().expect("seeded demo photo").id;
        session.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
        for value in [1.0, 2.0] {
            session.execute("develop.set", &json!({"control": "light.exposure", "value": value})).unwrap();
        }
        assert!(!session.catalog.photo(id).unwrap().history.is_empty());
        // Fixture setup, not a selected folder; copy/album commands perform no directory validation.
        session.import_defaults.auto_folder = Some("fake-watched-folder".into());
        session.execute("library.autoImport", &json!({"copy": false, "album": "Kept album"})).unwrap();
        let calls = Rc::new(Cell::new(0));
        let app = LightcraftApp::new(session, services(calls.clone(), available));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut parent = Self {
            app, ctx, files, calls, available, enabled: true, pass: Pass::Real, time: 0.0,
            tab: "import".to_owned(), rects: vec![], hits: vec![], trace: vec![],
        };
        parent.settle();
        parent
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.app.services = services(self.calls.clone(), self.available);
        let requested = format!("{events:?}");
        self.time += 0.04;
        let mut raw = HeadlessView::raw_input(egui::vec2(1200.0, 1400.0), 1.0, self.time, events);
        raw.events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
        let _ = crate::widgets::take_registries(&self.ctx);
        let ctx = self.ctx.clone();
        let app = &mut self.app;
        let tab = &mut self.tab;
        let enabled = self.enabled;
        let pass = self.pass;
        let mut completed = (vec![], vec![]);
        let mut passes = Vec::new();
        let mut ingress = Vec::new();
        let mut output = ctx.run_ui(raw, |ui| {
            if ingress.len() < 8 {
                let album_id = crate::text_field::id(ALBUM);
                let before_album = ctx.read_response(album_id).map(|response| (
                    response.id, response.is_pointer_button_down_on(), response.secondary_clicked(),
                ));
                let before_popup = ctx.read_response(album_id.with("popup")).map(|response| (
                    response.id, response.layer_id, response.sense,
                    response.is_pointer_button_down_on(), response.secondary_clicked(),
                ));
                let before_receivers = ctx.interaction_snapshot(|snapshot| (
                    snapshot.clicked, snapshot.dragged,
                    snapshot.hovered.iter().copied().take(12).collect::<Vec<_>>(),
                    snapshot.contains_pointer.iter().copied().take(12).collect::<Vec<_>>(),
                ));
                ingress.push((ctx.cumulative_pass_nr(), before_album, before_popup, before_receivers));
            }
            // Draw the ACTUAL installed parent and let it establish native_controls::scope itself.
            assert!(!super::active(&ctx));
            ui.add_enabled_ui(enabled, |ui| match pass {
                Pass::Real => super::super::window(app, ui, tab, 1100.0),
                Pass::Measure => craft::ui::runtime::measure::run(&ctx, || super::super::window(app, ui, tab, 1100.0)),
                Pass::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    super::super::window(app, &mut child, tab, 1100.0);
                }
            });
            assert!(!super::active(&ctx), "the real Settings parent restores its native presentation scope");
            let registries = crate::widgets::take_registries(&ctx);
            let discarded = ctx.will_discard();
            if passes.len() < 8 {
                passes.push((ctx.cumulative_pass_nr(), discarded));
            }
            completed = if discarded { (vec![], vec![]) } else { registries };
        });
        output.textures_delta.clear();
        (self.rects, self.hits) = completed;
        if self.trace.len() == 64 { self.trace.remove(0); }
        let native = ctx.memory(|m| m.focused());
        let sdk = craft::ui::runtime::focus::focused_rec(&ctx).map(|rec| (rec.stop.id, rec.stop.root, rec.stop.group));
        // Actual native field identity, never the automation hover-probe id. Read-only diagnostics:
        // show who receives the secondary press after Escape and which old layer is still hit-testable.
        let album = ctx.read_response(crate::text_field::id(ALBUM)).map(|response| {
            let at = response.interact_rect.center();
            let popup_id = egui::Popup::default_response_id(&response);
            let popup_layer = egui::LayerId::new(egui::Order::Foreground, popup_id);
            let popup_area = egui::AreaState::load(&ctx, popup_id).map(|area| (area.rect(), area.interactable));
            let popup_visible = ctx.memory(|memory| memory.areas().visible_last_frame(&popup_layer));
            let popup_response = ctx.read_response(popup_id).map(|popup| (
                popup.id, popup.layer_id, popup.sense, popup.rect,
                popup.is_pointer_button_down_on(), popup.clicked(), popup.secondary_clicked(),
            ));
            (response.id, response.layer_id, response.rect, response.interact_rect,
                response.has_focus(), response.contains_pointer(), response.hovered(), response.secondary_clicked(),
                response.is_pointer_button_down_on(), ctx.layer_id_at(at), (popup_area, popup_response), popup_visible)
        });
        let receivers = ctx.interaction_snapshot(|snapshot| (
            snapshot.clicked, snapshot.dragged,
            snapshot.hovered.iter().copied().take(12).collect::<Vec<_>>(),
            snapshot.contains_pointer.iter().copied().take(12).collect::<Vec<_>>(),
        ));
        self.trace.push(format!("input={requested} passes={passes:?} native={native:?} sdk={sdk:?} native_popup={} album={album:?} receivers={receivers:?} ingress={ingress:?}", egui::Popup::is_any_open(&ctx)));
    }

    fn settle(&mut self) { for _ in 0..3 { self.frame(vec![]); } }

    fn rect(&self, name: &str) -> Rect {
        let matches: Vec<_> = self.rects.iter().filter(|(id, _)| id == name).collect();
        assert_eq!(matches.len(), 1, "one current geometry for {name}; {:?}", self.trace);
        matches[0].1
    }

    fn hit(&self, name: &str) -> crate::widgets::WidgetHit {
        let matches: Vec<_> = self.hits.iter().filter(|(id, _)| id == name).collect();
        assert_eq!(matches.len(), 1, "one current response/probe for {name}; {:?}", self.trace);
        matches[0].1
    }

    fn key(&mut self, key: Key, shift: bool) {
        let mods = Modifiers { shift, ..Modifiers::NONE };
        for pressed in [true, false] {
            self.frame(vec![Event::ModifiersChanged(mods), Event::Key {
                key, physical_key: Some(key), pressed, repeat: false, modifiers: mods,
            }]);
        }
        self.settle();
    }

    fn sdk_focused(&self, name: &str) -> bool {
        let id = self.hit(name).id;
        craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == id)
            && self.ctx.memory(|m| m.focused()) == Some(id)
    }

    fn native_album_focused(&self) -> bool {
        let Some(id) = self.ctx.memory(|m| m.focused()) else { return false };
        // register() gives automation a hover probe; that probe is NOT a native focus substitute.
        id != self.hit(ALBUM).id
            && craft::ui::runtime::focus::focused_rec(&self.ctx).is_none()
            && egui::text_edit::TextEditState::load(&self.ctx, id).is_some()
            && self.ctx.read_response(id).is_some_and(|response| response.rect == self.rect(ALBUM))
    }

    fn tab_to_sdk(&mut self, name: &str, shift: bool) {
        self.reveal(name);
        for _ in 0..64 {
            if self.sdk_focused(name) { return; }
            self.key(Key::Tab, shift);
        }
        panic!("natural Tab shift={shift} never reached actual SDK {name}: {:?}", self.trace);
    }

    fn tab_to_native_album(&mut self, shift: bool) {
        self.reveal(ALBUM);
        for _ in 0..64 {
            if self.native_album_focused() { return; }
            self.key(Key::Tab, shift);
        }
        panic!("natural Tab shift={shift} never reached actual native Album: {:?}", self.trace);
    }

    fn reveal(&mut self, name: &str) {
        for _ in 0..8 {
            if self.hit(name).interact_rect.is_positive() { return; }
            let at = self.rect("settings:pageRoom").center();
            self.frame(vec![Event::PointerMoved(at), Event::MouseWheel {
                unit: MouseWheelUnit::Point, delta: egui::vec2(0.0, -250.0), modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown,
            }]);
            self.settle();
        }
        panic!("real wheel could not reveal {name}: {:?}", self.trace);
    }

    fn click(&mut self, name: &str, button: PointerButton) {
        self.reveal(name);
        let at = self.hit(name).interact_rect.center();
        self.frame(vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    fn prefs(&self) -> Value {
        serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap()
    }

    fn document(&self) -> Value {
        let mut before: Vec<_> = self.app.session.before.iter().map(|(id, settings)| (id.0, settings.clone())).collect();
        before.sort_by_key(|(id, _)| *id);
        json!({"catalog": self.app.session.catalog, "selection": self.app.session.selection, "before": before,
            "undo": self.app.session.undo.len(), "redo": self.app.session.redo.len()})
    }

    fn state(&self) -> Value {
        json!({"document": self.document(), "settings": self.app.ui.settings, "import": self.app.session.import_defaults,
            "prefs": self.prefs(), "journal": self.app.session.journal, "tab": self.tab,
            "toast": self.app.ui.toast.as_ref().map(|toast| &toast.0)})
    }

    fn model(&self) -> Value {
        let mut value = self.state();
        // Existing native Album commits an unchanged value on blur. Pure traversal checks the
        // model/persisted settings/document, not a new no-op-journal contract for that leaf.
        value.as_object_mut().unwrap().remove("journal");
        value
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1400.0), 1.0, self.time + 0.04, vec![]);
            let mut output = self.ctx.run_ui(raw, |_| {});
            output.textures_delta.clear();
            let _ = crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap(); // MemStore only.
        }
    }
}

#[test]
fn settings_parent_natural_tab_and_shift_tab_cross_native_album_and_sdk_copy() {
    let mut f = Parent::new(true);
    let baseline = f.model();
    f.tab_to_sdk(COPY, false);
    f.tab_to_native_album(false); // SDK → native, not SDK-only cycling.
    assert!(f.native_album_focused());
    f.tab_to_sdk(COPY, true); // Native → SDK with real Shift+Tab.
    assert!(f.sdk_focused(COPY));
    f.tab_to_native_album(true); // Reverse traversal must also reach native controls.
    assert!(f.native_album_focused());
    f.tab_to_sdk(COPY, false);
    assert!(f.sdk_focused(COPY));
    assert_eq!(f.model(), baseline, "traversal alone changes no model, persisted setting, photo history or selection");
    assert_eq!(f.calls.get(), 0);
    eprintln!("Settings actual-parent mixed traversal: {:?}", f.trace);
}

#[test]
fn settings_parent_natural_sdk_space_commits_copy_once_preserving_document_and_other_settings() {
    let mut f = Parent::new(true);
    f.tab_to_sdk(COPY, false);
    let document = f.document();
    let settings = serde_json::to_value(&f.app.ui.settings).unwrap();
    let mut expected = f.app.session.import_defaults.clone();
    expected.auto_copy = true;
    let mut prefs = f.prefs();
    prefs["import"] = serde_json::to_value(&expected).unwrap();
    let journal = f.app.session.journal.len();
    f.key(Key::Space, false);
    assert_eq!(serde_json::to_value(&f.app.session.import_defaults).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.autoImport".into(), json!({"copy": true}))]);
    assert_eq!(f.document(), document);
    assert_eq!(serde_json::to_value(&f.app.ui.settings).unwrap(), settings);
    assert_eq!(f.prefs(), prefs);
    assert_eq!(f.calls.get(), 0);
}

#[test]
fn settings_parent_native_popup_escape_immediate_reopen_then_sdk_picker_cancel_keep_state() {
    let mut f = Parent::new(true);
    f.reveal(ALBUM);
    let baseline = f.state();
    f.click(ALBUM, PointerButton::Secondary);
    assert!(egui::Popup::is_any_open(&f.ctx), "the actual native Album menu opened");
    assert_eq!(f.state(), baseline, "opening a native menu is not a settings commit");
    let at = f.hit(ALBUM).interact_rect.center();
    f.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
    assert!(!egui::Popup::is_any_open(&f.ctx));
    // No idle/settle frame between real Escape and the next real secondary press/release.
    f.frame(vec![
        Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: false, repeat: false, modifiers: Modifiers::NONE },
        Event::PointerMoved(at),
        Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE },
    ]);
    f.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE }]);
    assert!(egui::Popup::is_any_open(&f.ctx), "same real field menu reopens immediately: {:?}", f.trace);
    assert_eq!(f.state(), baseline);
    f.key(Key::Escape, false); // Close menu, returning the field's own focus.
    assert!(!egui::Popup::is_any_open(&f.ctx));
    f.key(Key::Escape, false); // End the native edit without committing it.
    assert_eq!(f.state(), baseline, "cancel preserves all settings, journal and photo history");
    f.tab_to_sdk(CHOOSE, false);
    let cancelled = f.state();
    f.key(Key::Enter, false);
    assert_eq!(f.calls.get(), 1, "the naturally focused real SDK Choose action ran its cancelling picker once");
    assert_eq!(f.state(), cancelled, "picker cancellation changes no setting, journal, document or selection");
}

#[test]
fn settings_parent_disabled_and_both_measurement_releases_do_not_commit_then_recover() {
    for pass in [Pass::Measure, Pass::Sizing] {
        let mut f = Parent::new(true);
        f.reveal(ALBUM);
        f.tab_to_sdk(COPY, false);
        let at = f.hit(COPY).interact_rect.center();
        let baseline = f.state();
        f.enabled = false;
        for pressed in [true, false] {
            f.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        f.key(Key::Space, false);
        f.key(Key::Enter, false);
        f.click(ALBUM, PointerButton::Secondary);
        assert!(!egui::Popup::is_any_open(&f.ctx), "the disabled native field cannot open its menu");
        assert_eq!(f.state(), baseline);
        assert_eq!(f.calls.get(), 0);
        f.enabled = true;
        f.settle();
        f.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        f.pass = pass;
        f.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        assert_eq!(f.state(), baseline, "a measured release is not an app commit");
        assert!(!f.hits.iter().any(|(id, _)| matches!(id.as_str(), CHOOSE | COPY)));
        f.pass = Pass::Real;
        f.settle();
        assert_eq!(f.state(), baseline, "return to normal rendering does not replay a speculative release");
        f.tab_to_sdk(COPY, false);
        let journal = f.app.session.journal.len();
        let document = f.document();
        f.key(Key::Space, false);
        assert!(f.app.session.import_defaults.auto_copy);
        assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.autoImport".into(), json!({"copy": true}))]);
        assert_eq!(f.document(), document);
        assert_eq!(f.calls.get(), 0);
    }
}
