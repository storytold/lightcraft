//! What an import reads from the photos themselves, in the app: chosen for the library in
//! Settings ▸ Import ▸ Read from photos, and for one import in the Import Photos review, which
//! opens with the library's choice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-importread-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A photo with a sidecar that rates it four stars and gives it a keyword.
fn a_rated_photo_with_a_keyword(dir: &Path, name: &str, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join(format!("{name}.png")), bytes).unwrap();
    std::fs::write(
        dir.join(format!("{name}.xmp")),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="4">
 <dc:subject><rdf:Bag><rdf:li>fjord</rdf:li></rdf:Bag></dc:subject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
}

fn app() -> Headless {
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 1000.0], 1.0);
    h.step();
    h
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

fn read(h: &mut Headless) -> Value {
    h.app.run("library.preferences", json!({})).unwrap()["import"]["read"].clone()
}

/// The photo named `name` in the library: its rating and keywords.
fn imported(h: &Headless, name: &str) -> (u8, Vec<String>) {
    let p = h.app.session.catalog.photos().find(|p| p.file_name.starts_with(name)).unwrap_or_else(|| panic!("{name} was imported"));
    (p.rating, p.meta.keywords.clone())
}

/// Open the import review for a folder, and wait for it.
fn review(h: &mut Headless, dir: &Path) {
    h.app.run("file.addPhotos", json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    h.step();
}

fn import_what_is_reviewed(h: &mut Headless, photos: usize) {
    click(h, "button:dialogOk");
    assert!(h.step_until(T, |h| h.app.session.catalog.len() == photos && h.app.import.is_none()), "{} photos", h.app.session.catalog.len());
}

/// Given Settings ▸ Import, then everything is read from photos until chosen otherwise; when
/// Keywords is unticked there, the library's imports leave keywords in the files, and ticking
/// it again reads them again. The other kinds stay as they were.
#[test]
fn what_imports_read_from_photos_is_chosen_in_settings() {
    let mut h = app();
    assert_eq!(read(&mut h), json!({"keywords": true, "ratings": true, "descriptions": true, "location": true, "develop": true}));
    assert_eq!(h.request("ui.key", json!({"key": ",", "cmd": true}), T)["ok"], true);
    click(&mut h, "button:settingsTab-import");
    let r = h.request("ui.widgets", json!({"filter": "check:settings.read."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(5), "one for each kind: {r}");
    click(&mut h, "check:settings.read.keywords");
    assert_eq!(read(&mut h), json!({"keywords": false, "ratings": true, "descriptions": true, "location": true, "develop": true}));
    click(&mut h, "check:settings.read.location");
    click(&mut h, "check:settings.read.keywords");
    assert_eq!(read(&mut h), json!({"keywords": true, "ratings": true, "descriptions": true, "location": false, "develop": true}));
}

/// Given a library that does not read keywords from photos, when a folder is imported through
/// the review as it opens, then its photos come in rated and without their keywords. When, in
/// the review of a second folder, Keywords is ticked and Ratings and labels unticked for once,
/// then those photos have their keywords and no rating, and the library's own choice is as it was.
#[test]
fn the_import_review_opens_with_the_librarys_choice_and_can_differ_for_once() {
    let (first, second) = (temp("first"), temp("second"));
    a_rated_photo_with_a_keyword(&first, "one", 1);
    a_rated_photo_with_a_keyword(&second, "two", 2);
    let mut h = app();
    h.app.run("library.preferences", json!({"import": {"read": {"keywords": false}}})).unwrap();

    review(&mut h, &first);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review") };
    assert!(!opts.read.keywords && opts.read.ratings, "as the library has it: {:?}", opts.read);
    let r = h.request("ui.widgets", json!({"filter": "check:importRead."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(5), "{r}");
    import_what_is_reviewed(&mut h, 1);
    assert_eq!(imported(&h, "one"), (4, Vec::new()));

    review(&mut h, &second);
    click(&mut h, "check:importRead.keywords");
    click(&mut h, "check:importRead.ratings");
    import_what_is_reviewed(&mut h, 2);
    assert_eq!(imported(&h, "two"), (0, vec!["fjord".to_string()]));
    assert_eq!(read(&mut h)["keywords"], json!(false), "for that import only");
    assert_eq!(imported(&h, "one"), (4, Vec::new()), "and what was imported before is as it was");
    for d in [first, second] {
        let _ = std::fs::remove_dir_all(d);
    }
}

// ---- the dialogs remember their Read row; Synchronize Folder has one too ----

/// Given a library that reads everything, when one import is reviewed with Ratings and labels
/// unticked, then the next review opens with it unticked, while the library's choice (and so a
/// dropped file) is as it was. When the library's choice is changed in Settings, the review
/// follows the library again.
#[test]
fn the_import_review_opens_as_it_was_last_used() {
    let (first, second, third) = (temp("last-first"), temp("last-second"), temp("last-third"));
    a_rated_photo_with_a_keyword(&first, "one", 1);
    a_rated_photo_with_a_keyword(&second, "two", 2);
    a_rated_photo_with_a_keyword(&third, "three", 3);
    let mut h = app();

    review(&mut h, &first);
    click(&mut h, "check:importRead.ratings");
    import_what_is_reviewed(&mut h, 1);
    assert_eq!(imported(&h, "one"), (0, vec!["fjord".to_string()]));

    review(&mut h, &second);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review") };
    assert!(!opts.read.ratings && opts.read.keywords, "as it was last used: {:?}", opts.read);
    assert_eq!(read(&mut h)["ratings"], json!(true), "the library's choice is its own");
    h.app.ui.dialog = None;
    // a file that is dropped in has no review: it is read as the library says
    crate::import::start_paths(&mut h.app, vec![second.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(T, |h| h.app.session.catalog.len() == 2 && h.app.import.is_none()));
    assert_eq!(imported(&h, "two"), (4, vec!["fjord".to_string()]));

    // Settings ▸ Import ▸ Read from photos changes: the review follows it again
    h.app.run("library.preferences", json!({"import": {"read": {"keywords": false}}})).unwrap();
    review(&mut h, &third);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review") };
    assert!(opts.read.ratings && !opts.read.keywords, "as the library has it now: {:?}", opts.read);
    for d in [first, second, third] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Synchronize Folder on `dir`, scanned.
fn synchronize_dialog(h: &mut Headless, dir: &Path) {
    crate::sync::open(&mut h.app, &dir.to_string_lossy(), "folder", false).unwrap();
    assert!(h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. }))), "the scan finished");
    h.step();
}

/// Given a folder of the library that got a new photo, then Synchronize Folder has the Read row
/// of the import review, opening as the library has it. When Keywords is unticked and the
/// folder synchronized, the new photo comes in rated and without its keywords; the dialog opens
/// that way the next time, and the import review is as it was.
#[test]
fn synchronize_folder_has_a_read_row_that_it_remembers() {
    let (dir, other) = (temp("sync-row"), temp("sync-row-other"));
    a_rated_photo_with_a_keyword(&dir, "one", 1);
    a_rated_photo_with_a_keyword(&other, "three", 3);
    let mut h = app();
    h.app.session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    a_rated_photo_with_a_keyword(&dir, "two", 2);

    synchronize_dialog(&mut h, &dir);
    let r = h.request("ui.widgets", json!({"filter": "check:syncRead."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(5), "one for each kind: {r}");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { read, .. }) if read.is_all()), "as the library has it");
    click(&mut h, "check:syncRead.keywords");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { read, .. }) if !read.keywords && read.ratings), "{:?}", h.app.ui.dialog);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    assert!(h.step_until(T, |h| h.app.sync_run.is_none() && h.app.session.catalog.len() == 2), "the new photo comes in");
    assert_eq!(imported(&h, "two"), (4, Vec::new()), "rated, its keywords left in the file");
    assert_eq!(imported(&h, "one"), (4, vec!["fjord".to_string()]), "the photo that was there is as it was");

    // the next time: as it was last used
    a_rated_photo_with_a_keyword(&dir, "four", 4);
    synchronize_dialog(&mut h, &dir);
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { read, .. }) if !read.keywords && read.ratings), "{:?}", h.app.ui.dialog);
    h.app.ui.dialog = None;
    h.step();
    // the other dialog and the library's choice are their own
    assert_eq!(read(&mut h)["keywords"], json!(true));
    review(&mut h, &other);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review") };
    assert!(opts.read.is_all(), "{:?}", opts.read);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// A folder with nothing new has nothing to read: no Read row.
#[test]
fn a_folder_with_no_new_photos_shows_no_read_row() {
    let dir = temp("sync-norow");
    a_rated_photo_with_a_keyword(&dir, "one", 1);
    let mut h = app();
    h.app.session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    synchronize_dialog(&mut h, &dir);
    let r = h.request("ui.widgets", json!({"filter": "check:syncRead."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(0), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a Synchronize Folder dialog with Keywords unticked whose scan went stale, when it is
/// confirmed, then it is refused and scanned again with the tick as it was, and nothing is
/// remembered for a synchronize that did not happen. When it is then synchronized, the choice
/// is remembered; ticking Keywords again the next time forgets it, and the dialog follows the
/// library once more.
#[test]
fn only_a_synchronize_that_was_taken_is_remembered_and_ticking_back_forgets() {
    let dir = temp("sync-stale");
    a_rated_photo_with_a_keyword(&dir, "one", 1);
    let mut h = app();
    h.app.session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    a_rated_photo_with_a_keyword(&dir, "two", 2);
    let unticked = |h: &Headless| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { read, counts: Some(_), .. }) if !read.keywords);

    synchronize_dialog(&mut h, &dir);
    click(&mut h, "check:syncRead.keywords");
    // the folder holds another photo of the library since the scan (it came in some other way)
    a_rated_photo_with_a_keyword(&dir, "extra", 9);
    h.app.session.execute("library.import", &json!({"paths": [dir.join("extra.png").to_string_lossy()]})).unwrap();
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err(), "what was shown is no longer so");
    assert!(h.app.session.import_defaults.read_last.is_empty(), "nothing was synchronized: nothing is remembered");
    assert!(h.step_until(T, unticked), "scanned again, the tick as it was: {:?}", h.app.ui.dialog);

    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    assert!(h.step_until(T, |h| h.app.sync_run.is_none() && h.app.session.catalog.len() == 3));
    assert!(h.app.session.import_defaults.read_last.synchronize.is_some_and(|r| !r.keywords), "now it is");

    // the next time, Keywords is ticked again: as the library has it, so nothing to remember
    a_rated_photo_with_a_keyword(&dir, "three", 3);
    synchronize_dialog(&mut h, &dir);
    assert!(unticked(&h));
    click(&mut h, "check:syncRead.keywords");
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    assert!(h.step_until(T, |h| h.app.sync_run.is_none() && h.app.session.catalog.len() == 4));
    assert!(h.app.session.import_defaults.read_last.is_empty(), "the dialog follows the library again");
    assert_eq!(imported(&h, "three"), (4, vec!["fjord".to_string()]));

    // with Import new photos unticked the row was not used: not remembered either
    a_rated_photo_with_a_keyword(&dir, "four", 4);
    synchronize_dialog(&mut h, &dir);
    let mut dlg = h.app.ui.dialog.take().expect("the dialog is open");
    if let Dialog::SynchronizeFolder { import_new, read, .. } = &mut dlg {
        (*import_new, read.ratings) = (false, false);
    }
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    assert!(h.step_until(T, |h| h.app.sync_run.is_none()));
    assert!(h.app.session.import_defaults.read_last.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
