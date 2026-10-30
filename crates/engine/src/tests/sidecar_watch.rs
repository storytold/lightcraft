//! Re-reading sidecars other apps change while the app runs (issue #3175): `library.sidecarScan`.

use std::path::Path;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::xmp::{rating_sidecar, temp_dir, write_png};
use crate::Session;

/// A library of one photo `a.png` in a new folder, the preference as given, and the first scan done (it
/// only records what is there).
fn watched(tag: &str, on: bool) -> (Session, std::path::PathBuf, PhotoId) {
    let src = temp_dir(tag);
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"rereadChanged": on})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let r = scan(&mut s, 0.0);
    assert_eq!(r["updated"], json!([]), "{r}");
    (s, src, id)
}

fn scan(s: &mut Session, at: f64) -> Value {
    s.execute("library.sidecarScan", &json!({"at": at})).unwrap()
}

/// Give `path` a modification time of `secs` after the epoch (file systems stamp writes at their own
/// granularity; the scan compares what the file says).
fn stamp(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
}

fn rating(s: &Session, id: PhotoId) -> u8 {
    s.catalog.photo(id).unwrap().rating
}

/// Another app's change is read once, after it has held still for the settle time, as one undo step, and the
/// sidecar is not written back even with auto-write on.
#[test]
fn a_changed_sidecar_is_reread_once() {
    let (mut s, src, id) = watched("sw-changed", true);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let sidecar = src.join("a.xmp");
    let foreign = rating_sidecar(4);
    std::fs::write(&sidecar, &foreign).unwrap();
    stamp(&sidecar, 1_000);
    let undo = s.undo.len();

    let r = scan(&mut s, 10.0);
    assert_eq!((r["updated"].clone(), r["waiting"].clone()), (json!([]), json!(1)), "noticed, not yet read: {r}");
    assert_eq!(scan(&mut s, 12.5)["updated"], json!([]), "held for 2.5 s only");
    assert_eq!(rating(&s, id), 0);
    let r = scan(&mut s, 13.0);
    assert_eq!(r["updated"], json!([id.0]), "{r}");
    assert_eq!(rating(&s, id), 4);
    assert_eq!(s.undo.len(), undo + 1, "one step");
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), foreign, "not written back");
    for at in [14.0, 30.0] {
        let r = scan(&mut s, at);
        assert_eq!((r["updated"].clone(), r["waiting"].clone()), (json!([]), json!(0)), "{r}");
    }
    assert_eq!(s.undo.len(), undo + 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// A sidecar copied in two steps (a partial file, then the complete one) is read once, when complete and
/// still; the partial one is never read.
#[test]
fn a_sidecar_written_in_two_steps_is_read_once_when_stable() {
    let (mut s, src, id) = watched("sw-two-steps", true);
    let sidecar = src.join("a.xmp");
    let full = rating_sidecar(5);
    std::fs::write(&sidecar, &full[..full.len() / 2]).unwrap();
    stamp(&sidecar, 1_000);
    assert_eq!(scan(&mut s, 10.0)["updated"], json!([]));
    // the second step lands 2 s later: the settle time starts over
    std::fs::write(&sidecar, &full).unwrap();
    stamp(&sidecar, 1_002);
    assert_eq!(scan(&mut s, 12.0)["updated"], json!([]));
    assert_eq!(scan(&mut s, 14.0)["updated"], json!([]), "2 s since the complete file");
    assert_eq!(rating(&s, id), 0, "the partial file was not read");
    let undo = s.undo.len();
    assert_eq!(scan(&mut s, 15.5)["updated"], json!([id.0]));
    assert_eq!(rating(&s, id), 5);
    assert_eq!(scan(&mut s, 30.0)["updated"], json!([]));
    assert_eq!(s.undo.len(), undo + 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// A temp file renamed over the sidecar looks like any other change of its (size, mtime).
#[test]
fn a_sidecar_renamed_into_place_is_read() {
    let (mut s, src, id) = watched("sw-rename", true);
    std::fs::write(src.join("a.xmp.tmp"), rating_sidecar(2)).unwrap();
    assert_eq!(scan(&mut s, 5.0)["updated"], json!([]), "a temp file is not a sidecar");
    std::fs::rename(src.join("a.xmp.tmp"), src.join("a.xmp")).unwrap();
    scan(&mut s, 10.0);
    assert_eq!(scan(&mut s, 13.0)["updated"], json!([id.0]));
    assert_eq!(rating(&s, id), 2);
    let _ = std::fs::remove_dir_all(&src);
}

/// LightCraft's own automatic writes are not read back.
#[test]
fn its_own_sidecar_writes_are_ignored() {
    let (mut s, src, id) = watched("sw-own", true);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();
    assert!(src.join("a.xmp").is_file());
    let undo = s.undo.len();
    for at in [10.0, 20.0, 30.0] {
        let r = scan(&mut s, at);
        assert_eq!((r["updated"].clone(), r["unchanged"].clone(), r["waiting"].clone()), (json!([]), json!(0), json!(0)), "{r}");
    }
    assert_eq!(s.undo.len(), undo);
    // a later write by the other app is still noticed
    std::fs::write(src.join("a.xmp"), rating_sidecar(1)).unwrap();
    stamp(&src.join("a.xmp"), 5_000);
    scan(&mut s, 40.0);
    assert_eq!(scan(&mut s, 44.0)["updated"], json!([id.0]));
    assert_eq!(rating(&s, id), 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// A sidecar that was only touched (its content gives the settings the photo has) is looked at but changes
/// nothing: no undo step.
#[test]
fn a_touched_sidecar_changes_nothing() {
    let (mut s, src, id) = watched("sw-touch", true);
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    scan(&mut s, 1.0);
    let undo = s.undo.len();
    stamp(&src.join("a.xmp"), 9_000);
    scan(&mut s, 10.0);
    let r = scan(&mut s, 14.0);
    assert_eq!((r["updated"].clone(), r["unchanged"].clone()), (json!([]), json!(1)), "{r}");
    assert_eq!(s.undo.len(), undo, "nothing committed");
    let _ = std::fs::remove_dir_all(&src);
}

/// With the preference off the scan does nothing and nothing is followed.
#[test]
fn nothing_is_watched_with_the_setting_off() {
    let (mut s, src, id) = watched("sw-off", false);
    assert!(s.sidecar_watch_folders().is_empty());
    std::fs::write(src.join("a.xmp"), rating_sidecar(4)).unwrap();
    let undo = s.undo.len();
    for at in [10.0, 20.0] {
        assert_eq!(scan(&mut s, at), json!({"updated": [], "unchanged": 0, "failed": [], "skipped": [], "waiting": 0}));
    }
    assert_eq!((rating(&s, id), s.undo.len()), (0, undo));
    // turning it on starts from what is there now: the old sidecar is not an "other app's change"
    s.execute("library.xmpPreferences", &json!({"rereadChanged": true})).unwrap();
    assert_eq!(s.sidecar_watch_folders(), vec![src.to_string_lossy().to_string()]);
    scan(&mut s, 30.0);
    assert_eq!(scan(&mut s, 40.0)["updated"], json!([]));
    assert_eq!(rating(&s, id), 0);
    let _ = std::fs::remove_dir_all(&src);
}

/// A photo whose own automatic write is still waiting is left alone (and said once); it is read when the
/// wait is over.
#[test]
fn a_photo_with_a_pending_write_is_skipped() {
    let (mut s, src, id) = watched("sw-pending", true);
    s.unsaved_sidecars.insert(id);
    std::fs::write(src.join("a.xmp"), rating_sidecar(4)).unwrap();
    stamp(&src.join("a.xmp"), 1_000);
    scan(&mut s, 10.0);
    let r = scan(&mut s, 14.0);
    assert_eq!((r["updated"].clone(), r["skipped"].clone()), (json!([]), json!([id.0])), "{r}");
    assert_eq!(rating(&s, id), 0, "neither side overwritten");
    let r = scan(&mut s, 18.0);
    assert_eq!((r["updated"].clone(), r["skipped"].clone()), (json!([]), json!([])), "said once: {r}");
    s.unsaved_sidecars.clear();
    assert_eq!(scan(&mut s, 22.0)["updated"], json!([id.0]));
    assert_eq!(rating(&s, id), 4);
    let _ = std::fs::remove_dir_all(&src);
}

/// The scan takes a listing from the caller (a worker thread) like `library.autoImportScan`.
#[test]
fn the_scan_takes_the_listing_from_the_caller() {
    let (mut s, src, id) = watched("sw-listing", true);
    let sidecar = src.join("a.xmp");
    std::fs::write(&sidecar, rating_sidecar(3)).unwrap();
    let listing = json!([[sidecar.to_string_lossy(), 100, 7_000_000]]);
    s.execute("library.sidecarScan", &json!({"listing": listing, "at": 10.0})).unwrap();
    let r = s.execute("library.sidecarScan", &json!({"listing": listing, "at": 14.0})).unwrap();
    assert_eq!(r["updated"], json!([id.0]), "{r}");
    assert_eq!(rating(&s, id), 3);
    let _ = std::fs::remove_dir_all(&src);
}

/// The preference is part of the XMP preferences (saved with the library); off by default.
#[test]
fn the_preference_is_an_xmp_preference() {
    let mut s = Session::new();
    assert_eq!(s.execute("library.xmpPreferences", &json!({})).unwrap()["rereadChanged"], json!(false));
    let r = s.execute("library.xmpPreferences", &json!({"rereadChanged": true})).unwrap();
    assert_eq!((r["rereadChanged"].clone(), r["autoWrite"].clone()), (json!(true), json!(false)));
    assert!(s.xmp.reread_changed);
}
