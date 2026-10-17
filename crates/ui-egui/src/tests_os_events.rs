//! Files and quit requests the operating system hands us: on macOS a Finder double-click, Open
//! With or a drop on the Dock icon arrive as Apple events, not as arguments (see
//! `Services::os_events`). Before the host delivered them, macOS answered that LightCraft cannot
//! open files in the “JPEG image” format.

use crate::panels::library_problem::LibraryProblem;
use crate::{LightcraftApp, OsEvent, Services};

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

fn queue(app: &mut LightcraftApp, mut events: Vec<OsEvent>) {
    app.services.os_events = Some(Box::new(move || std::mem::take(&mut events)));
}

#[test]
fn photos_the_os_opens_start_an_import() {
    let mut app = app();
    assert!(app.import.is_none());
    queue(&mut app, vec![OsEvent::Open(vec!["/photos/IMG_0001.CR2".into(), "/photos/IMG_0002.jpg".into()])]);
    let ctx = egui::Context::default();
    app.drain_os_events(&ctx);
    assert_eq!(app.import.as_ref().map(|i| i.total), Some(2), "both photos are queued for the import worker");
    // Drained: a second poll queues nothing more.
    app.drain_os_events(&ctx);
    assert_eq!(app.import.as_ref().map(|i| i.total), Some(2));
}

#[test]
fn files_wait_while_the_library_problem_dialog_is_up() {
    let mut app = app();
    app.library_problem = Some(LibraryProblem::new("/library", "locked"));
    queue(&mut app, vec![OsEvent::Open(vec!["/photos/a.jpg".into()])]);
    app.drain_os_events(&egui::Context::default());
    assert!(app.import.is_none(), "nothing imports before the user has chosen a library");
    assert_eq!(app.library_problem.as_ref().map(|p| p.pending_import.clone()), Some(vec!["/photos/a.jpg".to_string()]));
}

#[test]
fn files_import_into_the_temporary_session_once_the_problem_is_dismissed() {
    let mut app = app();
    let mut problem = LibraryProblem::new("/library", "locked");
    problem.dismissed = true;
    app.library_problem = Some(problem);
    queue(&mut app, vec![OsEvent::Open(vec!["/photos/a.jpg".into()])]);
    app.drain_os_events(&egui::Context::default());
    assert_eq!(app.import.as_ref().map(|i| i.total), Some(1));
    assert!(app.library_problem.as_ref().is_some_and(|p| p.pending_import.is_empty()));
}

#[test]
fn quit_requests_take_the_app_quit_path_and_empty_requests_do_nothing() {
    let mut app = app();
    queue(&mut app, vec![OsEvent::Open(Vec::new()), OsEvent::Quit]);
    app.drain_os_events(&egui::Context::default());
    assert!(app.import.is_none());
    assert!(app.ui.quit);
}
