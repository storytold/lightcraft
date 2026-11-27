//! File ▸ Missing Photos…: a folder that moved as a whole (another drive letter, another computer) is found by
//! replacing the beginning of its photos' paths, checked, and relinked in one undo step (issue #2356).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, MissingRow, MissingStatus};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn write_png(path: &Path, seed: u8) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8 ^ seed, (y * 11) as u8, seed, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(path, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
}

/// A library of three photos in `<root>/Disk D/Photos`, whose disk then came back as `Disk E`; one photo
/// didn't come back.
fn moved_disk(tag: &str) -> (PathBuf, Headless) {
    let root = std::env::temp_dir().join(format!("lc-ui-missing-photos-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let old = root.join("Disk D").join("Photos");
    std::fs::create_dir_all(old.join("2020")).unwrap();
    for (n, seed) in [("2020/a.png", 1), ("2020/b.png", 2), ("c.png", 3)] {
        write_png(&old.join(n), seed);
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(root.join("Library"), true).unwrap();
    session.execute("library.import", &json!({"paths": [old.to_string_lossy()]})).unwrap();
    std::fs::rename(root.join("Disk D"), root.join("Disk E")).unwrap();
    std::fs::remove_file(root.join("Disk E/Photos/c.png")).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.step();
    (root, h)
}

fn rows(h: &Headless) -> Option<Vec<MissingRow>> {
    match &h.app.ui.dialog {
        Some(Dialog::MissingPhotos { rows, .. }) => rows.clone(),
        _ => None,
    }
}

fn status_of(rows: &[MissingRow], name: &str) -> Option<MissingStatus> {
    rows.iter().find(|r| r.path.ends_with(name)).and_then(|r| r.status)
}

fn ok(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
}

/// The window lists the missing photos and suggests the folder they share; typing where it is now and pressing
/// Return checks every photo; Relink relinks the ones found, in one undo step.
#[test]
fn a_moved_disk_is_relinked_by_replacing_the_beginning_of_the_paths() {
    let (root, mut h) = moved_disk("relink");
    let (old, new) = (root.join("Disk D").join("Photos"), root.join("Disk E").join("Photos"));
    crate::menus::run_ui_command(&mut h.app, "file.missingPhotos", &json!({})).unwrap().unwrap();
    assert!(h.step_until(T, |h| rows(h).is_some()), "the missing photos were never listed");
    assert_eq!(rows(&h).map(|r| r.len()), Some(3));
    let Some(Dialog::MissingPhotos { from, .. }) = &h.app.ui.dialog else { panic!("no window") };
    assert_eq!(Path::new(from), old, "Replace suggests the folder the missing photos share");

    // by keyboard: into the "with" field, type, Return
    ok(&mut h, "ui.clickWidget", json!({"id": "field:missingPhotosTo"}));
    h.step();
    ok(&mut h, "ui.text", json!({"text": new.to_string_lossy()}));
    ok(&mut h, "ui.key", json!({"key": "Enter"}));
    let checked = |h: &Headless| rows(h).is_some_and(|r| r.iter().all(|r| !matches!(r.status, None | Some(MissingStatus::Checking))));
    assert!(h.step_until(T, checked), "the check never ended: {:?}", rows(&h));
    let r = rows(&h).unwrap_or_default();
    assert_eq!(
        (status_of(&r, "a.png"), status_of(&r, "b.png"), status_of(&r, "c.png")),
        (Some(MissingStatus::Found), Some(MissingStatus::Found), Some(MissingStatus::Absent)),
        "{r:?}"
    );
    assert!(r.iter().all(|r| r.chosen == (r.status == Some(MissingStatus::Found))), "the found ones are ticked: {r:?}");

    ok(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Relink closes the window");
    let missing = |h: &mut Headless| h.app.session.execute("library.missing", &json!({})).unwrap().as_array().map(Vec::len);
    assert_eq!(missing(&mut h), Some(1), "only the photo that didn't come back is still missing");
    h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(missing(&mut h), Some(3), "one undo step");
    let _ = std::fs::remove_dir_all(&root);
}

/// A row's new path typed by hand is checked again (an edit drops the old answer and the tick), and the window
/// only relinks what is ticked.
#[test]
fn a_typed_path_is_checked_before_it_can_be_relinked() {
    let (root, mut h) = moved_disk("typed");
    crate::menus::run_ui_command(&mut h.app, "file.missingPhotos", &json!({})).unwrap().unwrap();
    assert!(h.step_until(T, |h| rows(h).is_some()));
    let a = rows(&h).unwrap_or_default().into_iter().find(|r| r.path.ends_with("a.png")).unwrap();
    let id = a.id;
    // a wrong path first, then the right one
    for (path, want) in [(root.join("nowhere/a.png"), MissingStatus::Absent), (root.join("Disk E/Photos/2020/a.png"), MissingStatus::Found)] {
        if let Some(Dialog::MissingPhotos { rows: Some(rows), .. }) = h.app.ui.dialog.as_mut()
            && let Some(r) = rows.iter_mut().find(|r| r.id == id)
        {
            (r.new_path, r.status, r.chosen) = (path.to_string_lossy().into_owned(), None, false);
        }
        h.step();
        ok(&mut h, "ui.clickWidget", json!({"id": "button:missingPhotosCheck"}));
        let done = |h: &Headless| rows(h).is_some_and(|r| r.iter().any(|r| r.id == id && !matches!(r.status, None | Some(MissingStatus::Checking))));
        assert!(h.step_until(T, done));
        assert_eq!(rows(&h).unwrap_or_default().iter().find(|r| r.id == id).and_then(|r| r.status), Some(want));
    }
    assert_eq!(h.app.ui.dialog.as_ref().map(crate::missing_photos::chosen), Some(1), "only the photo found is ticked");
    ok(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    h.step();
    let missing = h.app.session.execute("library.missing", &json!({})).unwrap();
    assert_eq!(missing.as_array().map(Vec::len), Some(2), "{missing}");
    let _ = std::fs::remove_dir_all(&root);
}

/// With nothing missing the window says so and only closes.
#[test]
fn with_nothing_missing_the_window_says_so() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 800.0], 1.0);
    h.step();
    crate::menus::run_ui_command(&mut h.app, "file.missingPhotos", &json!({})).unwrap().unwrap();
    assert!(h.step_until(T, |h| rows(h).is_some()));
    assert_eq!(rows(&h).map(|r| r.len()), Some(0));
    ok(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    h.step();
    assert!(h.app.ui.dialog.is_none(), "its only button closes it");
}
