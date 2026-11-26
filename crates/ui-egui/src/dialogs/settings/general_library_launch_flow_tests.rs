//! Real General Library/launch section; physical inputs and real command/picker boundaries.
//! Every filesystem target is exclusively created by this fixture. No native chooser is opened.
use super::*;
use crate::headless::HeadlessView;
use crate::state::{Dialog, LaunchLibrary};
use egui::{Event, Key, Modifiers, PointerButton, Rect, vec2};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc::{self, Sender},
};

const OPEN: &str = "button:settingsOpenLibrary";
const NEW: &str = "button:settingsNewLibrary";
const LAST: &str = "button:settingsAtLaunch-0";
const ASK: &str = "button:settingsAtLaunch-1";
const SENTINEL: &[u8] = b"owned General fixture; not a library";
static NEXT: AtomicU64 = AtomicU64::new(0);

fn observation_key() -> egui::Id {
    egui::Id::unique("general-library-launch-observations")
}
pub(super) fn observe(ui: &egui::Ui, alias: &str, response: &egui::Response, native: bool, clicked: bool) {
    // Focus/response queries re-enter egui; finish them before borrowing its data mutably.
    let value = json!({"alias":alias,"id":format!("{:?}",response.id),"native":native,"clicked":clicked,
        "enabled":response.enabled(),"focused":response.has_focus(),
        "box":[response.rect.left(),response.rect.top(),response.rect.right(),response.rect.bottom()]});
    ui.data_mut(|data| {
        let mut values = data.get_temp::<Vec<Value>>(observation_key()).unwrap_or_default();
        values.push(value);
        data.insert_temp(observation_key(), values);
    });
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lightcraft-general-library-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        // Exclusive creation: never remove/reuse a pre-existing directory or follow a planted link.
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("not-a-folder"), SENTINEL).unwrap();
        std::fs::create_dir(path.join(lightcraft_engine::library::DEFAULT_NAME)).unwrap();
        std::fs::write(path.join(lightcraft_engine::library::DEFAULT_NAME).join("keep.txt"), SENTINEL).unwrap();
        Self(path)
    }
    fn guard(&self) {
        assert_eq!(std::fs::read(self.0.join("not-a-folder")).unwrap(), SENTINEL);
        assert_eq!(std::fs::read(self.0.join(lightcraft_engine::library::DEFAULT_NAME).join("keep.txt")).unwrap(), SENTINEL);
        assert_eq!(std::fs::read_dir(&self.0).unwrap().count(), 2, "no library or other file was created");
        assert_eq!(std::fs::read_dir(self.0.join(lightcraft_engine::library::DEFAULT_NAME)).unwrap().count(), 1);
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone, Copy, Debug)]
enum Pass {
    Real,
    Disabled,
    Measure,
    Sizing,
}
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.shapes.clear();
        self.0.textures_delta.clear();
        self.0.viewport_output.clear();
        self.0.platform_output.commands.clear();
    }
}
struct Fixture {
    app: LightcraftApp,
    scratch: Scratch,
    ctx: egui::Context,
    native: bool,
    window: bool,
    pass: Pass,
    tick: u32,
    document: Value,
    settings: Value,
    requests: Arc<Mutex<Vec<crate::pick::PickRequest>>>,
    answers: Arc<Mutex<Vec<Sender<Vec<String>>>>>,
    synchronous: Arc<AtomicUsize>,
    rects: Vec<(String, Rect)>,
    labels: Vec<String>,
    observations: Vec<Value>,
    activations: Vec<String>,
}
impl Fixture {
    fn new(native: bool) -> Self {
        let scratch = Scratch::new();
        let mut session = Session::new();
        let id = session.catalog.alloc_photo_id();
        session
            .catalog
            .apply(Op::AddPhoto {
                photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, "General fixture.jpg", "JPEG", 8, 6, "2026-11-22T00:00:00")),
            })
            .unwrap();
        session.selection = Selection::single(id);
        let mut develop = (*session.develop_of(id).unwrap()).clone();
        develop.light.exposure = 0.5;
        session.set_develop(id, develop, "General fixture exposure").unwrap();
        assert!(!session.undo.is_empty());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(Vec::new()));
        let synchronous = Arc::new(AtomicUsize::new(0));
        let request_log = Arc::clone(&requests);
        let senders = Arc::clone(&answers);
        let sync_calls = Arc::clone(&synchronous);
        let services = crate::Services {
            local_places: Some(vec![]),
            png: None,
            picker: Some(Box::new(move |request| {
                request_log.lock().unwrap().push(request);
                let (tx, rx) = mpsc::channel();
                senders.lock().unwrap().push(tx);
                Ok(rx)
            })),
            pick_folder: Some(Box::new(move || {
                sync_calls.fetch_add(1, Ordering::SeqCst);
                None
            })),
            ..Default::default()
        };
        let mut app = LightcraftApp::new(session, services);
        app.ui.settings = Default::default();
        app.ui.settings.at_launch = LaunchLibrary::Last;
        app.ui.settings.library_path = scratch.0.join("isolated-remembered").to_string_lossy().into_owned();
        app.ui.settings.recent_libraries = vec![app.ui.settings.library_path.clone()];
        app.ui.settings.recent_opened.clear();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let document = Self::document_of(&app);
        let settings = Self::other_settings(&app);
        let mut fixture = Self {
            app,
            scratch,
            ctx,
            native,
            window: false,
            pass: Pass::Real,
            tick: 0,
            document,
            settings,
            requests,
            answers,
            synchronous,
            rects: vec![],
            labels: vec![],
            observations: vec![],
            activations: vec![],
        };
        for _ in 0..3 {
            fixture.frame(vec![], Modifiers::NONE, "opening");
        }
        fixture
    }
    fn document_of(app: &LightcraftApp) -> Value {
        json!({"photos":app.session.catalog.photos().collect::<Vec<_>>(),"revision":app.session.catalog.revision,
            "selection":app.session.selection,"undo":format!("{:?}",app.session.undo),"redo":format!("{:?}",app.session.redo),
            "journal":app.session.journal,"commits":app.session.commits(),"playback":app.session.slideshow,
            "reference":app.ui.reference,"compare":app.ui.compare,"autoAdvance":app.ui.auto_advance})
    }
    fn other_settings(app: &LightcraftApp) -> Value {
        let mut settings = serde_json::to_value(&app.ui.settings).unwrap();
        let fields = settings.as_object_mut().unwrap();
        assert!(fields.remove("atLaunch").is_some(), "actual serialized launch field");
        settings
    }
    fn guard(&self) {
        assert_eq!(Self::document_of(&self.app), self.document, "launch/cancel/refusal must preserve photos, selection, preferences and Undo");
        assert_eq!(Self::other_settings(&self.app), self.settings, "unrelated host settings and isolated recent-library state stay exact");
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.import.is_none() && self.app.scan.is_none() && self.app.export.is_none());
        assert_eq!(self.synchronous.load(Ordering::SeqCst), 0, "never open even the synchronous fallback chooser");
        self.scratch.guard();
    }
    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers, stage: &str) {
        self.tick += 1;
        let mut input = vec![Event::ModifiersChanged(modifiers)];
        input.extend(events);
        println!(
            "GENERAL_LIBRARY_INPUT {}",
            json!({"stage":stage,"tick":self.tick,"native":self.native,"pass":format!("{:?}",self.pass),"events":format!("{:?}",input)})
        );
        crate::pick::poll(&mut self.app, &self.ctx);
        let _ = crate::widgets::take_registries(&self.ctx);
        self.ctx.data_mut(|data| data.remove::<Vec<Value>>(observation_key()));
        let raw = HeadlessView::raw_input(vec2(1200.0, 1000.0), 1.0, f64::from(self.tick) / 60.0, input);
        let window = self.window;
        let pass = self.pass;
        let app = &mut self.app;
        let output = Drain(self.ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            if app.ui.dialog.is_some() {
                crate::dialogs::show(app, &ctx, false);
            } else {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut draw = |ui: &mut egui::Ui| {
                        if window {
                            super::super::window(app, ui, &mut "general".to_string(), 900.0);
                        } else {
                            library_launch_section(app, ui, &Tokens::get(ui.ctx()));
                        }
                    };
                    let mut run = |ui: &mut egui::Ui| match pass {
                        Pass::Real => draw(ui),
                        Pass::Disabled => {
                            ui.add_enabled_ui(false, |ui| draw(ui));
                        }
                        Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                        Pass::Sizing => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                    };
                    run(ui);
                });
            }
        }));
        fn text(shape: &egui::Shape, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(shape) => labels.push(shape.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, labels)
                    }
                }
                _ => {}
            }
        }
        self.labels.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.labels);
        }
        let (rects, _) = crate::widgets::take_registries(&self.ctx);
        self.rects = rects;
        self.observations = self.ctx.data(|data| data.get_temp::<Vec<Value>>(observation_key()).unwrap_or_default());
        for observation in &self.observations {
            if observation["clicked"] == true {
                self.activations.push(observation["alias"].as_str().unwrap().to_string());
            }
        }
        self.guard();
        println!(
            "GENERAL_LIBRARY_STATE {}",
            json!({"stage":stage,"tick":self.tick,"observations":self.observations,
            "targets":self.rects.iter().map(|(id,r)|json!({"alias":id,"box":[r.left(),r.top(),r.right(),r.bottom()]})).collect::<Vec<_>>(),
            "labels":self.labels,"atLaunch":format!("{:?}",self.app.ui.settings.at_launch),"dialog":format!("{:?}",self.app.ui.dialog),
            "pickerRequests":format!("{:?}",self.requests.lock().unwrap()),"pending":self.app.pending_picks.iter().map(|p|p.command.clone()).collect::<Vec<_>>(),
            "focused":self.ctx.memory(|memory|memory.focused()).map(|id|format!("{:?}",id)),"activations":self.activations})
        );
    }
    fn idle(&mut self) {
        self.frame(vec![], Modifiers::NONE, "idle");
    }
    fn rect(&self, id: &str) -> Rect {
        let hits = self.rects.iter().filter(|(alias, _)| alias == id).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "one current target {id}");
        hits[0].1
    }
    fn pointer(&mut self, at: egui::Pos2, pressed: bool) {
        assert!(self.ctx.content_rect().contains(at));
        self.frame(
            vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
            Modifiers::NONE,
            if pressed { "pointer-down" } else { "pointer-up" },
        );
    }
    fn click(&mut self, id: &str) {
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)], Modifiers::NONE, "hover");
        let at = self.rect(id).center();
        self.pointer(at, true);
        assert!(self.rect(id).contains(at), "press remains in the actual current target");
        self.pointer(at, false);
        self.idle();
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Modifiers::NONE };
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }], modifiers, "key");
        }
        self.idle();
    }
    fn focus_alias(&self) -> Option<String> {
        let id = self.ctx.memory(|memory| memory.focused())?;
        let response = self.ctx.read_response(id)?;
        [OPEN, NEW, LAST, ASK].into_iter().find(|alias| self.rect(alias).contains(response.rect.center())).map(str::to_owned)
    }
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn assert_request(&self, command: &str) {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].kind, crate::pick::PickKind::Folder);
        assert_eq!(self.app.pending_picks.len(), 1);
        assert_eq!(self.app.pending_picks[0].command, command);
    }
    fn answer(&mut self, paths: Vec<String>) {
        for path in &paths {
            assert!(std::path::Path::new(path).starts_with(&self.scratch.0), "never answer with a foreign path");
        }
        assert_eq!(self.answers.lock().unwrap().len(), 1);
        self.answers.lock().unwrap().remove(0).send(paths).unwrap();
        self.idle();
        assert!(self.app.pending_picks.is_empty());
    }
    fn prompt(&self) {
        match &self.app.ui.dialog {
            Some(Dialog::TextPrompt { command, key, params, value, .. }) => {
                assert_eq!(command, "app.newLibrary");
                assert_eq!(key, "name");
                assert_eq!(params, &json!({}));
                assert_eq!(value, lightcraft_engine::library::DEFAULT_NAME);
            }
            other => panic!("real new-library prompt required: {other:?}"),
        }
    }
}
fn discovery(native: bool) {
    let mut f = Fixture::new(native);
    for alias in [OPEN, NEW, LAST, ASK, "text:settingsLocation"] {
        assert!(f.rect(alias).is_positive());
    }
    assert!(f.labels.iter().any(|s| s.contains("In memory")));
    let ids = f.observations.iter().map(|o| o["id"].clone()).collect::<Vec<_>>();
    f.idle();
    assert_eq!(ids, f.observations.iter().map(|o| o["id"].clone()).collect::<Vec<_>>());
    assert_eq!(f.request_count(), 0);
}
fn unavailable(native: bool) {
    let mut f = Fixture::new(native);
    f.app.services.pick_folder = None;
    f.app.services.picker = None;
    f.idle();
    assert!(!f.rects.iter().any(|(id, _)| id == LAST || id == ASK));
    f.click(OPEN);
    f.click(NEW);
    assert_eq!(f.request_count(), 0);
    assert!(f.app.ui.dialog.is_none());
    assert!(f.activations.is_empty());
}
fn open_cancel(native: bool) {
    let mut f = Fixture::new(native);
    let at = f.rect(OPEN).center();
    f.frame(vec![Event::PointerMoved(at)], Modifiers::NONE, "hover");
    f.pointer(at, true);
    assert_eq!(f.request_count(), 0);
    f.pointer(at, false);
    f.idle();
    f.assert_request("app.openLibrary");
    assert_eq!(f.activations, vec![OPEN]);
    f.answer(vec![]);
    assert!(f.app.ui.dialog.is_none());
    assert!(f.app.ui.toast.is_none());
}
fn open_refusal(native: bool) {
    let mut f = Fixture::new(native);
    f.click(OPEN);
    f.assert_request("app.openLibrary");
    let path = f.scratch.0.join("not-a-folder");
    f.answer(vec![path.to_string_lossy().into_owned()]);
    assert!(f.app.ui.toast.is_some(), "real command refusal is surfaced by picker poll");
}
fn new_escape(native: bool) {
    let mut f = Fixture::new(native);
    f.click(NEW);
    f.prompt();
    assert_eq!(f.request_count(), 0);
    f.key(Key::Escape, false);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.request_count(), 0);
}
// The cold-Enter schedules stay frozen in the earlier comparison, not normative here.
// A real field click acquires focus through normal input. Only after observing that actual
// field's public id, response and current registered rectangle do we send the confirming key.
fn confirm_new_with_observed_focus(f: &mut Fixture) {
    f.click("field:prompt");
    f.prompt();
    let field_id = crate::text_field::id("field:prompt");
    let focused = f.ctx.memory(|memory| memory.focused());
    let response = f.ctx.read_response(field_id).expect("actual live prompt field response");
    let registered = f.rect("field:prompt");
    println!(
        "GENERAL_LIBRARY_FOCUSED_ENTER {}",
        json!({"tick":f.tick,"expectedField":format!("{:?}",field_id),"focused":focused.map(|id|format!("{:?}",id)),
            "actualResponse":format!("{:?}",response.id),"hasFocus":response.has_focus(),"enabled":response.enabled(),
            "responseBox":[response.rect.left(),response.rect.top(),response.rect.right(),response.rect.bottom()],
            "registeredBox":[registered.left(),registered.top(),registered.right(),registered.bottom()]})
    );
    // The framework field keeps egui's focus on its text part, the part that types: the field or its text part.
    assert!(
        focused == Some(field_id) || focused == Some(crate::text_field::text_id(field_id)),
        "the actual prompt field must own focus before Enter: {focused:?}"
    );
    assert!(crate::text_field::has_focus_id(&f.ctx, field_id) && response.enabled(), "a genuinely focused enabled field, not its hover probe");
    assert_eq!(response.rect, registered, "focus belongs to the actual current prompt geometry");
    f.key(Key::Enter, false);
}

fn new_focused_enter_cancel(native: bool) {
    let mut f = Fixture::new(native);
    f.click(NEW);
    f.prompt();
    confirm_new_with_observed_focus(&mut f);
    f.assert_request("app.newLibrary");
    f.answer(vec![]);
    assert!(f.app.ui.dialog.is_none());
    assert!(f.app.ui.toast.is_none());
}
fn new_focused_enter_refusal(native: bool) {
    let mut f = Fixture::new(native);
    f.click(NEW);
    f.prompt();
    confirm_new_with_observed_focus(&mut f);
    f.assert_request("app.newLibrary");
    let path = f.scratch.0.to_string_lossy().into_owned();
    f.answer(vec![path]);
    assert!(f.app.ui.toast.is_some(), "nonempty owned target is refused without opening any library");
}
fn waiting_refusal(native: bool) {
    let mut f = Fixture::new(native);
    f.app.waiting_import.files = vec![f.scratch.0.join("waiting.jpg").to_string_lossy().into_owned()];
    assert!(super::super::can_switch_library(&f.app).is_err());
    for id in [OPEN, NEW] {
        f.click(id);
    }
    assert_eq!(f.activations, vec![OPEN, NEW]);
    assert_eq!(f.request_count(), 0);
    assert!(f.app.ui.dialog.is_none());
}
fn launch_choices(native: bool) {
    let mut f = Fixture::new(native);
    f.click(ASK);
    assert_eq!(f.app.ui.settings.at_launch, LaunchLibrary::Ask);
    let saved = serde_json::to_value(&f.app.ui.settings).unwrap();
    let restored: crate::state::AppSettings = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.at_launch, LaunchLibrary::Ask);
    f.click(LAST);
    assert_eq!(f.app.ui.settings.at_launch, LaunchLibrary::Last);
    assert_eq!(f.request_count(), 0);
}
fn tab_cycle(native: bool) {
    let mut f = Fixture::new(native);
    for alias in [OPEN, NEW, LAST, ASK] {
        f.key(Key::Tab, false);
        assert_eq!(f.focus_alias().as_deref(), Some(alias), "natural forward traversal");
    }
    for alias in [LAST, NEW, OPEN] {
        f.key(Key::Tab, true);
        assert_eq!(f.focus_alias().as_deref(), Some(alias), "natural backward traversal");
    }
    assert_eq!(f.request_count(), 0);
}
fn keyboard_open(native: bool) {
    let mut f = Fixture::new(native);
    f.key(Key::Tab, false);
    assert_eq!(f.focus_alias().as_deref(), Some(OPEN));
    f.key(Key::Enter, false);
    f.assert_request("app.openLibrary");
    f.answer(vec![]);
}
fn keyboard_new(native: bool) {
    let mut f = Fixture::new(native);
    f.key(Key::Tab, false);
    f.key(Key::Tab, false);
    assert_eq!(f.focus_alias().as_deref(), Some(NEW));
    f.key(Key::Space, false);
    f.prompt();
    f.key(Key::Escape, false);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.request_count(), 0);
}
fn inert_release(native: bool, pass: Pass) {
    let mut f = Fixture::new(native);
    let at = f.rect(OPEN).center();
    f.frame(vec![Event::PointerMoved(at)], Modifiers::NONE, "hover");
    f.pointer(at, true);
    f.pass = pass;
    f.pointer(at, false);
    assert_eq!(f.request_count(), 0, "disabled/speculative release must not run a chooser command");
    f.pass = Pass::Real;
    f.idle();
    assert_eq!(f.request_count(), 0, "restoring real draw cannot replay speculative input");
}
fn disabled(native: bool) {
    inert_release(native, Pass::Disabled);
}
fn measurement(native: bool) {
    inert_release(native, Pass::Measure);
}
fn sizing(native: bool) {
    inert_release(native, Pass::Sizing);
}
#[test]
fn general_library_sdk_discovery() {
    discovery(false);
}

#[test]
fn general_library_sdk_unavailable() {
    unavailable(false);
}

#[test]
fn general_library_sdk_open_cancel() {
    open_cancel(false);
}

#[test]
fn general_library_sdk_open_refusal() {
    open_refusal(false);
}

#[test]
fn general_library_sdk_new_escape() {
    new_escape(false);
}

#[test]
fn general_library_sdk_new_focused_enter_cancel() {
    new_focused_enter_cancel(false);
}

#[test]
fn general_library_sdk_new_focused_enter_refusal() {
    new_focused_enter_refusal(false);
}

#[test]
fn general_library_sdk_waiting_refusal() {
    waiting_refusal(false);
}

#[test]
fn general_library_sdk_launch_choices() {
    launch_choices(false);
}

#[test]
fn general_library_sdk_tab_cycle() {
    tab_cycle(false);
}

#[test]
fn general_library_sdk_keyboard_open() {
    keyboard_open(false);
}

#[test]
fn general_library_sdk_keyboard_new() {
    keyboard_new(false);
}

#[test]
fn general_library_sdk_disabled() {
    disabled(false);
}

#[test]
fn general_library_sdk_measurement() {
    measurement(false);
}

#[test]
fn general_library_sdk_sizing() {
    sizing(false);
}

// Separate from the preserved immediate-Enter schedules: hover observes the modal's current
// visible footer geometry before the real primary press/release on its actual Create button.
fn new_visible_create_cancel(native: bool) {
    let mut f = Fixture::new(native);
    f.click(NEW);
    f.prompt();
    f.click("button:dialogOk");
    f.assert_request("app.newLibrary");
    assert_eq!(f.activations, vec![NEW]);
    f.answer(vec![]);
    assert!(f.app.ui.dialog.is_none());
    assert!(f.app.ui.toast.is_none());
}
fn new_visible_create_refusal(native: bool) {
    let mut f = Fixture::new(native);
    f.click(NEW);
    f.prompt();
    f.click("button:dialogOk");
    f.assert_request("app.newLibrary");
    assert_eq!(f.activations, vec![NEW]);
    let path = f.scratch.0.to_string_lossy().into_owned();
    f.answer(vec![path]);
    assert!(f.app.ui.toast.is_some(), "actual named command refuses its owned nonempty target");
    assert!(f.app.ui.dialog.is_none());
}

#[test]
fn general_library_sdk_new_visible_create_cancel() {
    new_visible_create_cancel(false);
}

#[test]
fn general_library_sdk_new_visible_create_refusal() {
    new_visible_create_refusal(false);
}
