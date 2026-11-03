//! Issue #3440: the Export dialog's After Export choice (Lightroom's Post-Processing): show the
//! files in the file manager or open them in an application once the export has written them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_engine::export::AfterAction;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(120);

/// What the fake desktop was asked to do: `reveal <path>`, `open <app> <path>`, `openAll <app> <paths…>`.
type Calls = Arc<Mutex<Vec<String>>>;

/// A desktop whose file manager and application launcher only record what they are asked, and
/// whose writer writes for real (the action only hands over files that exist).
fn desktop(calls: &Calls) -> Services {
    let (reveal, open, open_all) = (calls.clone(), calls.clone(), calls.clone());
    Services {
        png: None,
        write_shared: Some(Arc::new(|path: &str, bytes: &[u8]| lightcraft_engine::export::write_file(path, bytes))),
        write: Some(Box::new(|path: &str, bytes: &[u8]| lightcraft_engine::export::write_file(path, bytes))),
        reveal: Some(Box::new(move |path: &str| {
            reveal.lock().unwrap().push(format!("reveal {path}"));
            Ok(())
        })),
        open_with: Some(Box::new(move |path: &str, app: &str| {
            open.lock().unwrap().push(format!("open {app} {path}"));
            Ok(())
        })),
        open_all_with: Some(Box::new(move |paths: &[String], app: &str| {
            open_all.lock().unwrap().push(format!("openAll {app} {}", paths.join(" ")));
            Ok(())
        })),
        check_editor: Some(Box::new(|_: &str| Ok(()))),
        ..Default::default()
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-after-export-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app(services: Services) -> Headless {
    sized(services, [1200.0, 800.0])
}

fn sized(services: Services, size: [f32; 2]) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), size, 1.0);
    h.settle(T);
    h
}

/// Export the first `n` photos in view through the Export dialog (64 px JPEGs into `dir`) with
/// `after` chosen, and wait for the background export to finish.
fn export(h: &mut Headless, n: usize, dir: &std::path::Path, after: AfterAction, conflict: lightcraft_engine::export::Conflict) {
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(n).map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), T);
    h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    h.step();
    let Some(crate::state::Dialog::Export { opts, full_size, resize, dir: d, .. }) = &mut h.app.ui.dialog else { panic!("no Export dialog") };
    *full_size = false;
    *resize = lightcraft_engine::export::Resize::long_edge(64);
    *d = dir.to_string_lossy().into_owned();
    opts.naming = "{seq}".into();
    opts.conflict = conflict;
    opts.after_export = after;
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.export.is_some(), "the dialog exports in the background");
    assert!(h.step_until(T, |h| h.app.export.is_none()), "the export finished");
}

fn written(h: &Headless) -> Vec<String> {
    let r = h.app.last_export_result.as_ref().expect("an export result");
    r["files"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).map(abs).collect()
}

fn abs(p: &str) -> String {
    std::path::absolute(p).unwrap().to_string_lossy().into_owned()
}

fn outcome(h: &Headless) -> Value {
    h.app.last_export_result.as_ref().map(|r| r["afterExport"].clone()).unwrap_or_default()
}

#[test]
fn show_in_finder_reveals_the_first_file_once_the_export_is_done() {
    let calls = Calls::default();
    let mut h = app(desktop(&calls));
    let dir = scratch("reveal");
    export(&mut h, 2, &dir, AfterAction::ShowInFileManager, Default::default());
    let files = written(&h);
    assert_eq!(files.len(), 2, "{:?}", h.app.last_export_result);
    assert_eq!(*calls.lock().unwrap(), [format!("reveal {}", files[0])], "one reveal, of the first file");
    assert_eq!(outcome(&h), json!({"action": "showInFileManager", "revealed": files[0]}));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_in_an_application_gets_every_file_written_in_one_start() {
    let calls = Calls::default();
    let mut h = app(desktop(&calls));
    let dir = scratch("open");
    export(&mut h, 3, &dir, AfterAction::OpenIn("Preview".into()), Default::default());
    let files = written(&h);
    assert_eq!(files.len(), 3, "{:?}", h.app.last_export_result);
    assert_eq!(*calls.lock().unwrap(), [format!("openAll Preview {}", files.join(" "))]);
    assert_eq!(outcome(&h), json!({"action": "openIn", "app": "Preview", "opened": files}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// PhotoCraft is found as Photo ▸ Edit In finds it: where Settings ▸ External Editing says, else by name.
#[test]
fn open_in_photocraft_uses_the_edit_in_lookup() {
    let calls = Calls::default();
    let mut services = desktop(&calls);
    services.open_all_with = None; // one start per file where the host can't hand over several
    let mut h = app(services);
    h.app.ui.settings.photocraft_path = "/Portable/PhotoCraft.app".into();
    let dir = scratch("photocraft");
    export(&mut h, 2, &dir, AfterAction::OpenInPhotoCraft, Default::default());
    let files = written(&h);
    let want: Vec<String> = files.iter().map(|f| format!("open /Portable/PhotoCraft.app {f}")).collect();
    assert_eq!(*calls.lock().unwrap(), want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_happens_when_the_export_wrote_nothing() {
    let calls = Calls::default();
    let mut h = app(desktop(&calls));
    let dir = scratch("skipped");
    export(&mut h, 2, &dir, AfterAction::Nothing, Default::default());
    assert!(calls.lock().unwrap().is_empty(), "Do nothing does nothing");
    assert_eq!(outcome(&h), Value::Null);
    // the same two again, with If file exists ▸ Skip: no file written, nothing revealed
    export(&mut h, 2, &dir, AfterAction::ShowInFileManager, lightcraft_engine::export::Conflict::Skip);
    assert!(written(&h).is_empty(), "{:?}", h.app.last_export_result);
    assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock().unwrap());
    assert_eq!(outcome(&h), json!({"action": "showInFileManager", "skipped": "the export wrote no files"}));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_happens_when_the_export_is_cancelled() {
    let calls = Calls::default();
    let mut services = desktop(&calls);
    services.write_shared = Some(Arc::new(|path: &str, bytes: &[u8]| {
        std::thread::sleep(Duration::from_millis(300));
        lightcraft_engine::export::write_file(path, bytes)
    }));
    let mut h = app(services);
    let dir = scratch("cancel");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(5).map(|p| p.0).collect();
    let r = h.request(
        "engine.execute",
        json!({"command": "app.export", "params": {"ids": ids, "dir": dir, "longEdge": 64, "background": true, "afterExport": {"openIn": "Preview"}}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.app.export.as_ref().unwrap().cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.export.is_none()));
    assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock().unwrap());
    assert_eq!(outcome(&h)["cancelled"], true, "{}", outcome(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// No file manager (the web, a host without one): the export still writes its files, and the
/// result and the toast say the action was skipped.
#[test]
fn without_a_file_manager_the_export_says_it_skipped_the_action() {
    let calls = Calls::default();
    let mut services = desktop(&calls);
    services.reveal = None;
    let mut h = app(services);
    let dir = scratch("unavailable");
    export(&mut h, 1, &dir, AfterAction::ShowInFileManager, Default::default());
    assert_eq!(written(&h).len(), 1);
    assert_eq!(outcome(&h), json!({"action": "showInFileManager", "skipped": "not available here"}));
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("not available here"), "{toast}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `app.export` without `background` (a control-channel script) does it too, before answering.
#[test]
fn a_foreground_export_does_it_before_answering() {
    let calls = Calls::default();
    let mut h = app(desktop(&calls));
    let dir = scratch("foreground");
    let id = h.app.session.active().unwrap().0;
    let r = h.request(
        "engine.execute",
        json!({"command": "app.export", "params": {"ids": [id], "dir": dir, "longEdge": 64, "afterExport": "showInFileManager"}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let path = abs(r["result"]["files"][0]["path"].as_str().unwrap());
    assert_eq!(*calls.lock().unwrap(), [format!("reveal {path}")]);
    assert_eq!(r["result"]["afterExport"], json!({"action": "showInFileManager", "revealed": path}));
    let _ = std::fs::remove_dir_all(&dir);
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

fn chosen(h: &Headless) -> AfterAction {
    match &h.app.ui.dialog {
        Some(crate::state::Dialog::Export { opts, .. }) => opts.after_export.clone(),
        _ => panic!("no Export dialog"),
    }
}

/// The dialog's After export dropdown, and Choose… for another application (the host's file
/// dialog answers `dialog.exportChooseApp` with the application picked).
#[test]
fn the_dialog_offers_each_choice() {
    let calls = Calls::default();
    let mut services = desktop(&calls);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let a = asked.clone();
    services.picker = Some(Box::new(move |req: crate::pick::PickRequest| {
        a.lock().unwrap().push(req);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(vec!["/Applications/Preview.app".to_string()]).unwrap();
        Ok(rx)
    }));
    // tall enough that the dialog shows its last rows without scrolling
    let mut h = sized(services, [1400.0, 1800.0]);
    h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    for (item, want) in [
        ("showInFileManager", AfterAction::ShowInFileManager),
        ("openInPhotoCraft", AfterAction::OpenInPhotoCraft),
        ("nothing", AfterAction::Nothing),
    ] {
        click(&mut h, "combo:exportAfter");
        click(&mut h, &format!("item:exportAfter-{item}"));
        assert_eq!(chosen(&h), want, "{item}");
    }
    // another application starts from the one Settings ▸ External Editing names
    h.app.ui.settings.external_editor = "Affinity Photo".into();
    click(&mut h, "combo:exportAfter");
    click(&mut h, "item:exportAfter-openIn");
    assert_eq!(chosen(&h), AfterAction::OpenIn("Affinity Photo".into()));
    click(&mut h, "button:exportChooseApp");
    assert!(h.step_until(T, |h| chosen(h) == AfterAction::OpenIn("/Applications/Preview.app".into())), "{:?}", chosen(&h));
    let req = asked.lock().unwrap().clone();
    assert_eq!((req.len(), req[0].kind), (1, crate::pick::PickKind::File));
    if cfg!(target_os = "macos") {
        assert_eq!(req[0].filter.as_ref().unwrap().1, &["app"]);
    }
    // the control channel can name it outright; a hostile one is refused
    let r = h.request("engine.execute", json!({"command": "dialog.exportChooseApp", "params": {"app": "Affinity Photo 2"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(chosen(&h), AfterAction::OpenIn("Affinity Photo 2".into()));
    let r = h.request("engine.execute", json!({"command": "dialog.exportChooseApp", "params": {"app": "a\nb"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(chosen(&h), AfterAction::OpenIn("Affinity Photo 2".into()));
    // the dialog's params (Export, Save preset) carry it
    let Some(crate::state::Dialog::Export { opts, .. }) = &h.app.ui.dialog else { panic!("no Export dialog") };
    assert_eq!(opts.to_json()["afterExport"], json!({"openIn": "Affinity Photo 2"}));
}
