//! Face controls exercised through real pointer/key events; no GPU, model fetch, or filesystem.

use super::*;
use crate::headless::HeadlessView;
use crate::state::{Dialog, NameEdit};
use crate::widgets::take_registries;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Ui, vec2};
use std::cell::RefCell;
use std::rc::Rc;

struct Frames {
    ctx: egui::Context,
    time: f64,
    rects: Vec<(String, Rect)>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, rects: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(1100.0, 800.0), 1.0, self.time, events);
        let mut output = self.ctx.run_ui(raw, |ui| draw(ui));
        output.textures_delta.clear();
        self.rects = take_registries(&self.ctx).0;
    }

    fn settle(&mut self, mut draw: impl FnMut(&mut Ui)) {
        for _ in 0..3 {
            self.frame(vec![], &mut draw);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn click(&mut self, id: &str, button: PointerButton, hold: f64, mut draw: impl FnMut(&mut Ui)) {
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)], &mut draw);
        self.frame(vec![pointer(at, button, true)], &mut draw);
        self.time += hold;
        self.frame(vec![pointer(at, button, false)], &mut draw);
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn key(key: Key) -> Vec<Event> {
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }).to_vec()
}

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default())
}

fn draw_model(ui: &mut Ui, app: &mut LightcraftApp, model: &Value, download: Option<&Value>) {
    let tokens = Tokens::get(ui.ctx());
    model_row(app, ui, &tokens, model, download, true);
}

fn draw_terms(ui: &mut Ui, app: &mut LightcraftApp, info: &Value, accepted: &mut bool) {
    let tokens = Tokens::get(ui.ctx());
    model_dialog(app, ui, &tokens, info, accepted);
}

fn model() -> Value {
    json!({
        "id": "port110-model", "name": "Test face model", "role": "embedder", "installed": false,
        "selected": false, "sizeBytes": 1000000, "downloadHost": "example.invalid",
        "source": "https://example.invalid/model", "licence": {"name": "MIT", "commercial": "yes"}
    })
}

#[test]
fn download_release_opens_terms_once_without_starting_a_fetch() {
    let mut frames = Frames::new();
    let mut app = app();
    let model = model();
    frames.settle(|ui| draw_model(ui, &mut app, &model, None));
    frames.click("faces:download:port110-model", PointerButton::Secondary, 0.0, |ui| {
        draw_model(ui, &mut app, &model, None);
    });
    assert!(app.ui.dialog.is_none(), "a secondary click must not download");
    frames.click("faces:download:port110-model", PointerButton::Primary, 0.0, |ui| {
        draw_model(ui, &mut app, &model, None);
    });
    let Some(Dialog::FaceModel { info, accepted, path }) = app.ui.dialog.clone() else { panic!("terms did not open") };
    assert_eq!(info["download"], "port110-model");
    assert!(!accepted);
    assert!(path.is_empty());
    assert!(install(&mut app, &path, &info, false).is_err());
    assert!(app.session.face_downloads.snapshot().is_empty());
    assert!(app.caches.faces_dl_watch.is_empty());
}

#[test]
fn download_uses_sdk_primary_release_after_a_long_hold() {
    let mut frames = Frames::new();
    let mut app = app();
    let model = model();
    frames.settle(|ui| draw_model(ui, &mut app, &model, None));
    frames.click("faces:download:port110-model", PointerButton::Primary, 1.0, |ui| {
        draw_model(ui, &mut app, &model, None);
    });
    assert!(matches!(app.ui.dialog, Some(Dialog::FaceModel { accepted: false, .. })));
    assert!(app.session.face_downloads.snapshot().is_empty());
}

#[test]
fn model_page_uses_the_host_once_and_consent_can_be_revoked() {
    let mut frames = Frames::new();
    let mut app = app();
    let urls = Rc::new(RefCell::new(Vec::new()));
    let opened = urls.clone();
    app.services.open_url = Some(Box::new(move |url| {
        opened.borrow_mut().push(url.to_owned());
        Ok(())
    }));
    let info = json!({"kind": "known", "fileName": "model.onnx", "model": model()});
    let mut accepted = false;
    frames.settle(|ui| draw_terms(ui, &mut app, &info, &mut accepted));
    frames.click("faceModel:page", PointerButton::Primary, 0.0, |ui| {
        draw_terms(ui, &mut app, &info, &mut accepted);
    });
    assert_eq!(*urls.borrow(), ["https://example.invalid/model"]);
    for expected in [true, false] {
        frames.click("check:faceModel.accept", PointerButton::Primary, 0.0, |ui| {
            draw_terms(ui, &mut app, &info, &mut accepted);
        });
        assert_eq!(accepted, expected);
    }
    assert!(app.session.face_downloads.snapshot().is_empty());
}

#[test]
fn cancelling_a_running_download_removes_only_its_watch() {
    let mut frames = Frames::new();
    let mut app = app();
    let model = model();
    let running = json!({"state": "running", "bytes": 1, "total": 1000000});
    app.caches.faces_dl_watch = vec!["port110-model".into(), "other-model".into()];
    frames.settle(|ui| draw_model(ui, &mut app, &model, Some(&running)));
    frames.click("faces:cancelDownload:port110-model", PointerButton::Primary, 0.0, |ui| {
        draw_model(ui, &mut app, &model, Some(&running));
    });
    assert_eq!(app.caches.faces_dl_watch, ["other-model"]);
    assert!(app.ui.dialog.is_none());
}

#[test]
fn inline_name_typing_submits_trimmed_text_and_escape_cancels() {
    let mut frames = Frames::new();
    let mut edit = NameEdit { fresh: true, ..Default::default() };
    let mut outcome = Editor::Open;
    frames.settle(|ui| outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &[], &[], Setup::Running));
    frames.frame(vec![Event::Text("  Ada  ".into())], |ui| {
        outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &[], &[], Setup::Running);
    });
    frames.frame(key(Key::Enter), |ui| {
        outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &[], &[], Setup::Running);
    });
    assert!(matches!(&outcome, Editor::Submit(name) if name == "Ada"));
    frames.frame(key(Key::Escape), |ui| {
        outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &[], &[], Setup::Running);
    });
    assert!(matches!(outcome, Editor::Cancel));
}

#[test]
fn inline_suggestion_and_setup_link_keep_their_distinct_actions() {
    let mut frames = Frames::new();
    let mut edit = NameEdit { fresh: true, ..Default::default() };
    let candidates = vec![("Ada".into(), 0.95)];
    let mut outcome = Editor::Open;
    frames.settle(|ui| outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &candidates, &[], Setup::GetModel));
    frames.click("faceName:pick:Ada", PointerButton::Primary, 0.0, |ui| {
        outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &candidates, &[], Setup::GetModel);
    });
    assert!(matches!(&outcome, Editor::Submit(name) if name == "Ada"));
    frames.click("faces:setup", PointerButton::Primary, 0.0, |ui| {
        outcome = name_editor(ui.ctx(), egui::pos2(20.0, 20.0), &mut edit, &candidates, &[], Setup::GetModel);
    });
    assert!(matches!(outcome, Editor::Setup));
}
