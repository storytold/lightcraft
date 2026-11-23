//! Real production controls with injected host callbacks; no fetch or model load.
use super::*;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use crate::widgets::{WidgetHit, take_registries};
use craft::ui::runtime::focus::{self, FocusTarget};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Ui, vec2};
use lightcraft_engine::segment::{DownloadStatus, LICENSE_URL};
use std::cell::RefCell;
use std::rc::Rc;

struct Frames {
    ctx: egui::Context,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    texts: Vec<String>,
    url_commands: usize,
}

fn text(shape: &egui::Shape, texts: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(t) => texts.push(t.galley.job.text.clone()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text(s, texts)),
        _ => {}
    }
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, rects: vec![], hits: vec![], texts: vec![], url_commands: 0 }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(1100.0, 800.0), 1.0, self.time, events);
        let mut output = self.ctx.run_ui(raw, |ui| draw(ui));
        self.texts.clear();
        for shape in &output.shapes {
            text(&shape.shape, &mut self.texts);
        }
        self.url_commands += output.platform_output.commands.iter().filter(|c| matches!(c, egui::OutputCommand::OpenUrl(_))).count();
        output.textures_delta.clear();
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self, mut draw: impl FnMut(&mut Ui)) {
        for _ in 0..3 {
            self.frame(vec![], &mut draw);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn hit(&self, id: &str) -> WidgetHit {
        self.hits.iter().find(|(name, _)| name == id).unwrap().1
    }

    fn click(&mut self, id: &str, button: PointerButton, hold: f64, mut draw: impl FnMut(&mut Ui)) {
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)], &mut draw);
        self.frame(vec![pointer(at, button, true)], &mut draw);
        self.time += hold;
        self.frame(vec![pointer(at, button, false)], &mut draw);
    }

    fn focus(&self, id: &str) {
        assert!(focus::request(&self.ctx, FocusTarget::Id(self.hit(id).id)));
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn key(key: Key) -> Vec<Event> {
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }).to_vec()
}

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default())
}

fn running(total: u64) -> DownloadStatus {
    DownloadStatus { running: true, done: 500_000_000, total, file: "model.safetensors".into(), ..Default::default() }
}

#[test]
fn sam_port155_license_primary_release_dispatches_host_once_without_accepting_or_downloading() {
    let mut f = Frames::new();
    let mut app = app();
    let opened = Rc::new(RefCell::new(vec![]));
    let log = opened.clone();
    app.services.open_url = Some(Box::new(move |url| {
        log.borrow_mut().push(url.to_owned());
        Ok(())
    }));
    app.ui.dialog = Some(Dialog::SamModel { then: Some(("object".into(), "new".into())), error: None });
    let dialog = app.ui.dialog.clone();
    let status = app.session.segmenter.download_status();
    f.settle(|ui| sam_model_body(&mut app, ui, None));
    f.click("link:samLicense", PointerButton::Secondary, 0.0, |ui| sam_model_body(&mut app, ui, None));
    assert!(opened.borrow().is_empty());
    let at = f.rect("link:samLicense").center();
    f.frame(vec![Event::PointerMoved(at)], |ui| sam_model_body(&mut app, ui, None));
    f.frame(vec![pointer(at, PointerButton::Primary, true)], |ui| sam_model_body(&mut app, ui, None));
    assert!(opened.borrow().is_empty(), "press does not accept the licence or open a browser");
    f.time += 1.0;
    f.frame(vec![pointer(at, PointerButton::Primary, false)], |ui| sam_model_body(&mut app, ui, None));
    assert_eq!(*opened.borrow(), [LICENSE_URL]);
    assert_eq!(f.url_commands, 0, "host owns the sole URL dispatch");
    assert_eq!(app.ui.dialog, dialog);
    assert_eq!(app.session.segmenter.download_status(), status);
}

#[test]
fn sam_port155_license_keyboard_enter_and_space_use_the_same_host_action() {
    let mut f = Frames::new();
    let mut app = app();
    let opened = Rc::new(RefCell::new(vec![]));
    let log = opened.clone();
    app.services.open_url = Some(Box::new(move |url| {
        log.borrow_mut().push(url.to_owned());
        Ok(())
    }));
    f.settle(|ui| sam_model_body(&mut app, ui, None));
    for k in [Key::Enter, Key::Space] {
        f.focus("link:samLicense");
        f.frame(vec![], |ui| sam_model_body(&mut app, ui, None));
        f.frame(key(k), |ui| sam_model_body(&mut app, ui, None));
    }
    assert_eq!(*opened.borrow(), [LICENSE_URL, LICENSE_URL]);
    assert_eq!(f.url_commands, 0);
    assert!(!app.session.segmenter.download_status().running);
}

#[test]
fn sam_port155_manual_help_failure_keeps_pending_action_and_reports_the_host_error() {
    let mut f = Frames::new();
    let mut app = app();
    assert!(app.session.segmenter.mirrors().is_empty(), "fixture requires no configured SAM mirror");
    app.ui.dialog = Some(Dialog::SamModel { then: Some(("describe".into(), "new".into())), error: Some("original download error".into()) });
    let dialog = app.ui.dialog.clone();
    let urls = Rc::new(RefCell::new(vec![]));
    let log = urls.clone();
    app.services.open_url = Some(Box::new(move |url| {
        log.borrow_mut().push(url.to_owned());
        Err("test browser unavailable".into())
    }));
    f.settle(|ui| sam_model_body(&mut app, ui, Some("original download error")));
    assert!(f.texts.iter().any(|s| s.contains("original download error")), "download error remains visible");
    assert!(!f.rects.iter().any(|(id, _)| id == "button:samFolder"), "no configured folder/opener");
    f.click("link:samHelp", PointerButton::Primary, 0.0, |ui| sam_model_body(&mut app, ui, Some("original download error")));
    assert_eq!(*urls.borrow(), [SAM_HELP]);
    assert_eq!(app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("test browser unavailable"));
    assert_eq!(app.ui.dialog, dialog);
    assert!(!app.session.segmenter.download_status().running);
}

#[test]
fn sam_port155_running_snapshot_cancel_is_primary_release_and_keyboard_only() {
    let mut f = Frames::new();
    let d = running(2_000_000_000);
    let mut cancellations = 0;
    f.settle(|ui| {
        cancellations += usize::from(sam_download(ui, &d));
    });
    assert!(f.texts.iter().any(|s| s == "0.50 / 2.00 GB · model.safetensors"));
    assert!(f.rect("progress:samDownload").bottom() <= f.rect("button:samCancel").top());
    f.click("button:samCancel", PointerButton::Secondary, 0.0, |ui| {
        cancellations += usize::from(sam_download(ui, &d));
    });
    assert_eq!(cancellations, 0);
    let at = f.rect("button:samCancel").center();
    f.frame(vec![Event::PointerMoved(at)], |ui| {
        cancellations += usize::from(sam_download(ui, &d));
    });
    f.frame(vec![pointer(at, PointerButton::Primary, true)], |ui| {
        cancellations += usize::from(sam_download(ui, &d));
    });
    assert_eq!(cancellations, 0);
    f.time += 1.0;
    f.frame(vec![pointer(at, PointerButton::Primary, false)], |ui| {
        cancellations += usize::from(sam_download(ui, &d));
    });
    assert_eq!(cancellations, 1);
    for k in [Key::Space, Key::Enter] {
        f.focus("button:samCancel");
        f.frame(vec![], |ui| {
            cancellations += usize::from(sam_download(ui, &d));
        });
        f.frame(key(k), |ui| {
            cancellations += usize::from(sam_download(ui, &d));
        });
    }
    assert_eq!(cancellations, 3);
    // A zero total keeps the existing indeterminate progress path, and never cancels on draw.
    f.settle(|ui| {
        cancellations += usize::from(sam_download(ui, &running(0)));
    });
    assert_eq!(cancellations, 3);
    assert!(f.rect("progress:samDownload").is_positive());
}

#[test]
fn sam_port155_sdk_controls_publish_real_hit_owners_and_roles() {
    let mut f = Frames::new();
    let mut app = app();
    let mut roles = vec![];
    f.settle(|ui| {
        let (_, records) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("sam-port155-inspect"), |ui| {
            sam_model_body(&mut app, ui, None);
            let _ = sam_download(ui, &running(1_000_000_000));
        });
        roles = records.iter().flat_map(|r| r.elements.iter()).filter_map(|e| e.facts.as_ref().map(|facts| facts.role.clone())).collect();
    });
    assert!(roles.iter().any(|r| r.as_str() == "Link"), "{roles:?}");
    assert!(roles.iter().any(|r| r.as_str() == "Button"), "{roles:?}");
    for id in ["link:samLicense", "button:samCancel"] {
        let hit = f.hit(id);
        assert_ne!(hit.id, egui::Id::unique(("lc-automation-hit", id, 0usize)), "not a hover proxy");
        assert!(hit.interact_rect.contains(f.rect(id).center()));
        assert_eq!(f.rects.iter().filter(|(name, _)| name == id).count(), 1);
    }
}

#[test]
fn sam_port155_native_and_sdk_measurement_publish_no_actions_or_automation() {
    let mut f = Frames::new();
    let mut app = app();
    app.services.open_url = Some(Box::new(|_| panic!("measurement must not open URLs")));
    let before = app.session.segmenter.download_status();
    f.frame(vec![], |ui| {
        let ctx = ui.ctx().clone();
        let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        sam_model_body(&mut app, &mut sizing, Some("retained error"));
        assert!(!sam_download(&mut sizing, &running(0)));
        let (rects, hits) = take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty(), "native sizing leaked registration");
        craft::ui::runtime::measure::run(&ctx, || {
            sam_model_body(&mut app, ui, Some("retained error"));
            assert!(!sam_download(ui, &running(1_000_000_000)));
        });
        let (rects, hits) = take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty(), "SDK measurement leaked registration");
    });
    assert_eq!(app.session.segmenter.download_status(), before);
    assert_eq!(f.url_commands, 0);
}

#[test]
fn sam_port155_manual_folder_button_requires_a_host_and_opens_the_configured_folder() {
    struct Folder(std::path::PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let temp = Folder(std::env::temp_dir().join(format!("lc-sam-port155-{}-{stamp}", std::process::id())));
    let dir = temp.0.join("sam3");
    let mut f = Frames::new();
    let mut app = app();
    assert!(app.session.segmenter.mirrors().is_empty(), "fixture requires no configured SAM mirror");
    app.session.segmenter.dir = Some(dir.clone());
    f.settle(|ui| sam_model_body(&mut app, ui, None));
    assert!(!f.rects.iter().any(|(id, _)| id == "button:samFolder"));
    assert!(!dir.exists(), "drawing without a host must not create directories");
    let calls = Rc::new(RefCell::new(vec![]));
    let log = calls.clone();
    app.services.open_with = Some(Box::new(move |path, editor| {
        log.borrow_mut().push((path.to_owned(), editor.to_owned()));
        Ok(())
    }));
    // Force the normal synchronous service path on Linux as well, never a native file manager.
    #[cfg(target_os = "linux")]
    {
        app.services.open_model_folder = None;
    }
    app.ui.dialog = Some(Dialog::SamModel { then: Some(("object".into(), "new".into())), error: None });
    let dialog = app.ui.dialog.clone();
    f.settle(|ui| sam_model_body(&mut app, ui, None));
    assert!(!dir.exists(), "the folder is created only on activation");
    f.click("button:samFolder", PointerButton::Primary, 0.0, |ui| sam_model_body(&mut app, ui, None));
    assert_eq!(*calls.borrow(), [(dir.to_str().unwrap().to_owned(), String::new())]);
    assert!(dir.is_dir());
    assert_eq!(app.ui.dialog, dialog);
    assert!(!app.session.segmenter.download_status().running);
}
