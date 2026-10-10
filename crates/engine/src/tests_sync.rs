//! Synchronize Folder: bring a library folder up to date with what is on disk (see
//! [`crate::sync`]).
//!
//! Scenarios, in the words of someone whose folder changed outside LightCraft:
//!
//! * Given a folder of the library, when files were added to it (or to a folder inside it), the
//!   scan lists them as new; files the library already has, by path or by content, are not new.
//! * When a photo's file was deleted or moved away, the scan lists the photo as missing.
//! * When another app saved a photo's XMP sidecar after the photo came into the library, and the
//!   sidecar says something the library doesn't, the scan lists a metadata update; a sidecar older
//!   than that, or one that agrees with the library, is not an update.
//! * Scanning changes nothing.
//! * Synchronizing imports the new files; it removes missing photos (to Recently Deleted) and
//!   reads metadata updates only when asked. Whatever it does is one undo step.
//! * Only a folder of the library can be synchronized, never the whole startup disk.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::Session;

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A small procedural PNG (distinct per seed).
fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn sidecar(rating: u8) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/>
</rdf:RDF></x:xmpmeta>"#
    )
}

/// Write `a.xmp` beside `a.png`, dated `age` before now.
fn write_sidecar(png: &str, rating: u8, age: Duration) {
    let xmp = Path::new(png).with_extension("xmp");
    std::fs::write(&xmp, sidecar(rating)).unwrap();
    let f = std::fs::File::options().write(true).open(&xmp).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

/// A library holding `trip/a.png` and `trip/day1/b.png`, the session clock at a fixed past time.
fn library(dir: &Scratch) -> Session {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/day1/b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    assert_eq!(s.catalog.len(), 2);
    s
}

fn paths(v: &Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = v[key].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap().to_string()).collect();
    out.sort();
    out
}

/// What the library holds: each photo's file, whether it is in the library, and its rating.
fn holdings(s: &Session) -> Vec<(String, bool, u8)> {
    let mut v: Vec<(String, bool, u8)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.in_library(), p.rating)).collect();
    v.sort();
    v
}

fn scan(s: &mut Session, path: &str) -> Value {
    s.execute("folder.scanChanges", &json!({"path": path})).unwrap()
}

#[test]
fn an_unchanged_folder_has_no_changes() {
    let dir = Scratch::new("none");
    let mut s = library(&dir);
    let r = scan(&mut s, &dir.path("trip"));
    assert!(paths(&r, "new").is_empty() && paths(&r, "missing").is_empty() && paths(&r, "metadata").is_empty(), "{r}");
}

#[test]
fn files_added_to_the_folder_or_a_folder_inside_it_are_new() {
    let dir = Scratch::new("new");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    write_png(&dir.path("trip/day2/d.png"), 4);
    // the same bytes as a photo the library has: not new
    std::fs::copy(dir.path("trip/a.png"), dir.path("trip/a-copy.png")).unwrap();
    let before = s.catalog.to_snapshot();
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(paths(&r, "new"), vec![dir.path("trip/c.png"), dir.path("trip/day2/d.png")], "{r}");
    assert_eq!(r["duplicates"], 1, "{r}");
    assert_eq!(s.catalog.to_snapshot(), before, "scanning changes nothing");
}

#[test]
fn a_photo_whose_file_is_gone_is_missing() {
    let dir = Scratch::new("missing");
    let mut s = library(&dir);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(paths(&r, "missing"), vec![dir.path("trip/day1/b.png")], "{r}");
    assert!(r["missing"][0]["id"].is_u64());
}

#[test]
fn a_sidecar_saved_by_another_app_is_a_metadata_update() {
    let dir = Scratch::new("meta");
    let mut s = library(&dir);
    // saved now: after the photo came in (the session clock says 2026-09-30)
    write_sidecar(&dir.path("trip/a.png"), 4, Duration::ZERO);
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(paths(&r, "metadata"), vec![dir.path("trip/a.png")], "{r}");
}

#[test]
fn an_old_sidecar_or_one_that_agrees_is_no_update() {
    let dir = Scratch::new("meta-old");
    let mut s = library(&dir);
    // older than the import: whatever it says, the library has had its say since
    write_sidecar(&dir.path("trip/a.png"), 4, Duration::from_secs(400 * 24 * 3600));
    // new, but says what the library already knows
    write_sidecar(&dir.path("trip/day1/b.png"), 0, Duration::ZERO);
    let r = scan(&mut s, &dir.path("trip"));
    assert!(paths(&r, "metadata").is_empty(), "{r}");
}

#[test]
fn synchronizing_imports_new_files_and_leaves_the_rest_unless_asked() {
    let dir = Scratch::new("sync");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    write_sidecar(&dir.path("trip/a.png"), 5, Duration::ZERO);
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!((r["imported"].as_u64(), r["removed"].as_u64(), r["read"].as_u64()), (Some(1), Some(0), Some(0)), "{r}");
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 3, "the missing photo stays");
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap();
    assert_eq!(a.rating, 0, "metadata is read only when asked");
}

#[test]
fn synchronizing_everything_is_one_undo_step() {
    let dir = Scratch::new("sync-all");
    let mut s = library(&dir);
    let before = holdings(&s);
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    write_sidecar(&dir.path("trip/a.png"), 5, Duration::ZERO);
    let r =
        s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "importNew": true, "removeMissing": true, "readMetadata": true})).unwrap();
    assert_eq!((r["imported"].as_u64(), r["removed"].as_u64(), r["read"].as_u64()), (Some(1), Some(1), Some(1)), "{r}");
    let library: Vec<String> = s.catalog.photos().filter(|p| p.in_library()).map(|p| p.file_name.clone()).collect();
    assert_eq!(library.len(), 2, "b went to Recently Deleted: {library:?}");
    assert_eq!(s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().rating, 5, "the sidecar was read");
    let undo = s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(undo["undone"], "Synchronize Folder");
    assert_eq!(holdings(&s), before, "one undo step puts everything back");
}

#[test]
fn a_folder_with_no_changes_synchronizes_without_an_undo_step() {
    let dir = Scratch::new("sync-none");
    let mut s = library(&dir);
    let steps = s.undo.len();
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "removeMissing": true, "readMetadata": true})).unwrap();
    assert_eq!((r["imported"].as_u64(), r["removed"].as_u64(), r["read"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
    assert_eq!(s.undo.len(), steps);
}

#[test]
fn only_a_folder_of_the_library_is_synchronized() {
    let dir = Scratch::new("refused");
    let mut s = library(&dir);
    std::fs::create_dir_all(dir.0.join("elsewhere")).unwrap();
    for (cmd, p, why) in [
        ("folder.scanChanges", json!({}), "missing path"),
        ("folder.scanChanges", json!({"path": "  "}), "blank path"),
        ("folder.scanChanges", json!({"path": dir.path("elsewhere")}), "no photo was imported from it"),
        ("folder.scanChanges", json!({"path": "/"}), "the startup disk"),
        ("folder.synchronize", json!({"path": dir.path("elsewhere")}), "no photo was imported from it"),
        ("folder.synchronize", json!({"path": "/"}), "the startup disk"),
    ] {
        assert!(s.execute(cmd, &p).is_err(), "{cmd}: {why}");
    }
}

#[test]
fn synchronizing_acts_on_the_changes_just_scanned() {
    let dir = Scratch::new("cached");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(paths(&r, "new").len(), 1);
    // arrived after the scan the person was shown: not part of what they agreed to
    write_png(&dir.path("trip/d.png"), 4);
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
    // the scan is used once; the next synchronize looks again
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["imported"], 1, "d.png now: {r}");
}

#[test]
fn a_scan_is_stale_once_the_library_changed() {
    let dir = Scratch::new("stale");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    scan(&mut s, &dir.path("trip"));
    // c.png comes in some other way between the scan and the synchronize
    s.execute("library.import", &json!({"paths": [dir.path("trip/c.png")]})).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["imported"], 0, "the library changed: it looked again: {r}");
}

#[test]
fn a_cancelled_scan_stops_checking_files() {
    let dir = Scratch::new("cancel");
    let mut s = library(&dir);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    let input = crate::sync::SyncInput::new(&mut s, &dir.path("trip"), false).unwrap();
    let progress = crate::sync::SyncProgress::default();
    progress.files.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let c = crate::sync::scan_with(input, &progress);
    assert!(c.missing.is_empty() && c.metadata.is_empty(), "{c:?}");
}

#[test]
fn many_photos_are_checked_in_parallel_with_the_same_answer() {
    let dir = Scratch::new("many");
    for i in 0..40u8 {
        write_png(&dir.path(&format!("big/{i:02}.png")), i);
    }
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("big")]})).unwrap();
    for i in (0..40u8).step_by(3) {
        std::fs::remove_file(dir.path(&format!("big/{i:02}.png"))).unwrap();
    }
    for i in (1..40u8).step_by(5) {
        write_sidecar(&dir.path(&format!("big/{i:02}.png")), 2, Duration::ZERO);
    }
    let r = scan(&mut s, &dir.path("big"));
    let expect_missing: Vec<String> = (0..40u8).step_by(3).map(|i| dir.path(&format!("big/{i:02}.png"))).collect();
    let expect_meta: Vec<String> = (1..40u8).step_by(5).filter(|i| i % 3 != 0).map(|i| dir.path(&format!("big/{i:02}.png"))).collect();
    assert_eq!(paths(&r, "missing"), expect_missing);
    assert_eq!(paths(&r, "metadata"), expect_meta);
}

#[test]
fn a_folder_that_is_not_there_is_not_scanned_as_if_everything_went_missing() {
    let dir = Scratch::new("offline");
    let mut s = library(&dir);
    // an unplugged disk, or a folder moved away whole
    std::fs::rename(dir.0.join("trip"), dir.0.join("trip-elsewhere")).unwrap();
    for cmd in ["folder.scanChanges", "folder.synchronize"] {
        let e = s.execute(cmd, &json!({"path": dir.path("trip"), "removeMissing": true})).unwrap_err().to_string();
        assert!(e.contains("not there"), "{cmd}: {e}");
    }
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 2, "nothing was removed");
}

#[test]
fn a_whole_disk_or_a_folder_holding_disks_is_synchronized_only_on_request() {
    let dir = Scratch::new("disks");
    let mut s = library(&dir);
    // a library photo on another disk makes the scratch folder's root look like it holds disks
    let id = s.catalog.alloc_photo_id();
    let p = lightcraft_catalog::Photo::new(
        id,
        lightcraft_catalog::Source::File { path: "/Volumes/nas/x.jpg".into() },
        "x.jpg",
        "JPEG",
        6,
        4,
        "2026-01-01",
    );
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    if std::path::Path::new("/Volumes").is_dir() {
        let e = s.execute("folder.scanChanges", &json!({"path": "/Volumes"})).unwrap_err().to_string();
        assert!(e.contains("disk: true"), "{e}");
    }
    // a folder inside one disk needs nothing more
    assert!(s.execute("folder.scanChanges", &json!({"path": dir.path("trip")})).is_ok());
}

#[test]
fn a_file_only_browsed_in_local_is_new_and_joins_the_library() {
    let dir = Scratch::new("local");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    // looking at the folder in Local catalogues c.png as a browse record, not a library photo
    s.execute("library.browse", &json!({"path": dir.path("trip")})).unwrap();
    assert!(s.catalog.photos().any(|p| p.file_name == "c.png" && p.local));
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(paths(&r, "new"), vec![dir.path("trip/c.png")], "{r}");
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
    assert!(s.catalog.photos().any(|p| p.file_name == "c.png" && p.in_library()));
}

#[test]
fn a_file_renamed_or_moved_inside_the_folder_is_relinked_not_lost() {
    let dir = Scratch::new("moved");
    let mut s = library(&dir);
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    std::fs::rename(dir.path("trip/a.png"), dir.path("trip/day1/a-renamed.png")).unwrap();
    let r = scan(&mut s, &dir.path("trip"));
    assert!(paths(&r, "missing").is_empty() && paths(&r, "new").is_empty(), "{r}");
    assert_eq!(r["moved"][0]["id"], a.0, "{r}");
    assert_eq!(r["moved"][0]["to"], dir.path("trip/day1/a-renamed.png"), "{r}");
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "removeMissing": true})).unwrap();
    assert_eq!((r["relinked"].as_u64(), r["removed"].as_u64(), r["imported"].as_u64()), (Some(1), Some(0), Some(0)), "{r}");
    let p = s.catalog.photo(a).unwrap();
    assert!(p.in_library() && p.rating == 4, "the same photo, edits and all");
    assert_eq!(p.source, lightcraft_catalog::Source::File { path: dir.path("trip/day1/a-renamed.png") });
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().source, lightcraft_catalog::Source::File { path: dir.path("trip/a.png") });
}

#[test]
fn synchronizing_is_one_undo_step_even_with_a_full_undo_history() {
    let dir = Scratch::new("full-undo");
    let mut s = library(&dir);
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    // a long session: the history is at its limit and drops its oldest steps
    for i in 0..1005u32 {
        s.execute("photo.rate", &json!({"ids": [a.0], "rating": i % 5})).unwrap();
    }
    let before = holdings(&s);
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "removeMissing": true})).unwrap();
    assert_eq!((r["imported"].as_u64(), r["removed"].as_u64()), (Some(1), Some(1)), "{r}");
    let undo = s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(undo["undone"], "Synchronize Folder");
    assert_eq!(holdings(&s), before, "one step undoes the import and the removal");
}

#[test]
fn a_scan_stays_current_while_only_other_folders_change() {
    let dir = Scratch::new("current");
    let mut s = library(&dir);
    write_png(&dir.path("other/x.png"), 9);
    s.execute("library.import", &json!({"paths": [dir.path("other")]})).unwrap();
    write_png(&dir.path("trip/c.png"), 3);
    scan(&mut s, &dir.path("trip"));
    // something elsewhere in the library changes while the dialog is open (a background import…)
    let x = s.catalog.photos().find(|p| p.file_name == "x.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [x.0], "rating": 3})).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "scanned": true})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
}

#[test]
fn a_change_to_the_folders_photos_makes_the_scan_stale() {
    let dir = Scratch::new("stale-folder");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    scan(&mut s, &dir.path("trip"));
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 2})).unwrap();
    // the dialog asks for exactly what it showed: refused, never redone on the spot
    let e = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "scanned": true})).unwrap_err().to_string();
    assert!(e.contains("scan again"), "{e}");
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 2, "nothing was done");
    // an agent that just asks to synchronize gets a fresh scan
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
}

#[test]
fn the_progress_counts_the_photos_checked_too() {
    use std::sync::atomic::Ordering::Relaxed;
    let dir = Scratch::new("progress");
    let mut s = library(&dir);
    let input = crate::sync::SyncInput::new(&mut s, &dir.path("trip"), false).unwrap();
    let progress = crate::sync::SyncProgress::default();
    crate::sync::scan_with(input, &progress);
    assert_eq!((progress.checked.load(Relaxed), progress.to_check.load(Relaxed)), (2, 2));
    let (done, total) = progress.counts();
    assert!(done == total && total >= 2, "{done}/{total}");
}

#[test]
fn a_sidecar_the_library_wrote_itself_is_no_update() {
    let dir = Scratch::new("autowrite");
    let mut s = library(&dir);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 3})).unwrap();
    s.execute("photo.label", &json!({"ids": [a.0], "label": "green"})).unwrap();
    assert!(Path::new(&dir.path("trip/a.xmp")).exists(), "auto-write wrote the sidecar");
    let r = scan(&mut s, &dir.path("trip"));
    assert!(paths(&r, "metadata").is_empty(), "{r}");
}

#[test]
fn a_renamed_photo_with_no_content_hash_is_relinked_when_its_file_is_unmistakable() {
    // as photos taken over from a Lightroom catalog are: no hash on record
    let dir = Scratch::new("nohash");
    let mut s = library(&dir);
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    let mut p = (**s.catalog.photo(a).unwrap()).clone();
    p.content_hash = None;
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: a }).unwrap();
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    std::fs::rename(dir.path("trip/a.png"), dir.path("trip/a-renamed.png")).unwrap();
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(r["moved"][0]["to"], dir.path("trip/a-renamed.png"), "{r}");
    assert!(paths(&r, "missing").is_empty() && paths(&r, "new").is_empty(), "{r}");
}

#[test]
fn renamed_duplicates_are_each_relinked() {
    let dir = Scratch::new("dups");
    let mut s = library(&dir);
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    s.execute("photo.duplicate", &json!({"ids": [a.0]})).unwrap();
    assert!(Path::new(&dir.path("trip/a-copy.png")).exists());
    std::fs::rename(dir.path("trip/a.png"), dir.path("trip/one.png")).unwrap();
    std::fs::rename(dir.path("trip/a-copy.png"), dir.path("trip/two.png")).unwrap();
    let r = scan(&mut s, &dir.path("trip"));
    assert_eq!(r["moved"].as_array().map(Vec::len), Some(2), "{r}");
    assert!(paths(&r, "missing").is_empty(), "{r}");
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(r["relinked"], 2, "{r}");
}

#[test]
fn face_regions_found_meanwhile_leave_the_scan_current() {
    let dir = Scratch::new("faces");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    scan(&mut s, &dir.path("trip"));
    // background face search adds a region to a photo of the folder
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    let mut meta = s.catalog.photo(a).unwrap().meta.clone();
    meta.regions.push(lightcraft_meta::Region { rect: Default::default(), kind: lightcraft_meta::RegionKind::Face, name: None, description: None });
    s.catalog.apply(lightcraft_catalog::Op::SetMeta { id: a, meta: Box::new(meta) }).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.path("trip"), "scanned": true})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
}

/// Synchronizing in halves, as the app does it: readied on the UI thread without touching the
/// disk, the work (every file read) on a worker, each step committed back on the UI thread.
fn sync_in_halves(s: &mut Session, path: &str, choice: crate::sync::SyncChoice) -> crate::sync::SyncReport {
    let changes = crate::sync::scan(s, path, false).unwrap();
    let (work, mut commit) = crate::sync::SyncJob::start(s, changes, choice).unwrap();
    let progress = crate::sync::SyncProgress::default();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || work.run(&progress, |step| tx.send(step).is_ok())).join().unwrap();
    for step in rx {
        commit.apply(s, step);
    }
    commit.finish(s)
}

#[test]
fn the_work_of_synchronizing_runs_apart_from_its_commits() {
    let dir = Scratch::new("halves");
    let mut s = library(&dir);
    let before = holdings(&s);
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    write_sidecar(&dir.path("trip/a.png"), 5, Duration::ZERO);
    let choice = crate::sync::SyncChoice { import_new: true, relink_moved: true, remove_missing: true, read_metadata: true };
    let r = sync_in_halves(&mut s, &dir.path("trip"), choice);
    assert_eq!((r.imported, r.removed, r.read), (1, 1, 1), "{r:?}");
    assert_eq!(s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().rating, 5);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(holdings(&s), before, "still one undo step");
}

#[test]
fn the_work_checks_again_that_a_new_file_is_still_there() {
    let dir = Scratch::new("ready");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    let changes = crate::sync::scan(&mut s, &dir.path("trip"), false).unwrap();
    // the file goes away between readying (UI thread) and the work (worker): the work finds out
    let (work, mut commit) = crate::sync::SyncJob::start(&mut s, changes, crate::sync::SyncChoice::default()).unwrap();
    std::fs::remove_file(dir.path("trip/c.png")).unwrap();
    let mut steps = Vec::new();
    work.run(&crate::sync::SyncProgress::default(), |step| {
        steps.push(step);
        true
    });
    for step in steps {
        commit.apply(&mut s, step);
    }
    let r = commit.finish(&mut s);
    assert_eq!(r.imported, 0, "{r:?}");
    assert_eq!(r.failed.len(), 1, "{r:?}");
}

#[test]
fn a_cancelled_synchronize_does_nothing_more() {
    let dir = Scratch::new("cancel-run");
    let mut s = library(&dir);
    let steps_before = s.undo.len();
    write_png(&dir.path("trip/c.png"), 3);
    let changes = crate::sync::scan(&mut s, &dir.path("trip"), false).unwrap();
    let (work, commit) = crate::sync::SyncJob::start(&mut s, changes, crate::sync::SyncChoice::default()).unwrap();
    let progress = crate::sync::SyncProgress::default();
    progress.files.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut sent = 0;
    work.run(&progress, |_| {
        sent += 1;
        true
    });
    assert_eq!(sent, 0);
    let r = commit.finish(&mut s);
    assert_eq!((r.imported, s.undo.len()), (0, steps_before));
}

/// Ready a synchronize of the trip folder and do its work, keeping the steps for the test to
/// commit when it likes.
fn readied(s: &mut Session, dir: &Scratch, choice: crate::sync::SyncChoice) -> (Vec<crate::sync::SyncStep>, crate::sync::SyncCommit) {
    let changes = crate::sync::scan(s, &dir.path("trip"), false).unwrap();
    let (work, commit) = crate::sync::SyncJob::start(s, changes, choice).unwrap();
    let mut steps = Vec::new();
    work.run(&crate::sync::SyncProgress::default(), |step| {
        steps.push(step);
        true
    });
    (steps, commit)
}

#[test]
fn steps_for_a_library_that_was_closed_meanwhile_are_not_applied() {
    let dir = Scratch::new("switched");
    let mut s = library(&dir);
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    let choice = crate::sync::SyncChoice { remove_missing: true, ..Default::default() };
    let (steps, mut commit) = readied(&mut s, &dir, choice);
    // File › Open Library while the work runs: another catalog, whose photo ids mean other photos
    s.library_identity = std::sync::Arc::new(());
    let before = holdings(&s);
    let undo = s.undo.len();
    for step in steps {
        commit.apply(&mut s, step);
    }
    let r = commit.finish(&mut s);
    assert_eq!(holdings(&s), before, "nothing landed in the other library");
    assert_eq!(s.undo.len(), undo);
    assert_eq!((r.imported, r.removed), (0, 0), "{r:?}");
    assert!(!r.failed.is_empty(), "and the report says why: {r:?}");
}

#[test]
fn an_undo_pressed_while_synchronizing_is_not_folded_into_it() {
    let dir = Scratch::new("undo-mid");
    let mut s = library(&dir);
    let a = s.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/day1/b.png")).unwrap();
    let choice = crate::sync::SyncChoice { remove_missing: true, ..Default::default() };
    let (mut steps, mut commit) = readied(&mut s, &dir, choice);
    assert!(steps.len() >= 2, "an import step and a remove step");
    let first = steps.remove(0);
    commit.apply(&mut s, first);
    // the person presses Undo between two steps of the run
    s.execute("edit.undo", &json!({})).unwrap();
    for step in steps {
        commit.apply(&mut s, step);
    }
    commit.finish(&mut s);
    // undoing the run's last step never takes the rating with it
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().rating, 4, "the earlier, unrelated step stays");
}

#[test]
fn sidecar_namings_for_many_photos_agree_with_one_at_a_time() {
    let dir = Scratch::new("namings");
    let mut s = library(&dir);
    // a raw beside its JPEG: the raw owns IMG_1.xmp, the JPEG's is IMG_1.JPG.xmp
    for name in ["IMG_1.CR3", "IMG_1.JPG", "IMG_2.JPG"] {
        let id = s.catalog.alloc_photo_id();
        let p = lightcraft_catalog::Photo::new(
            id,
            lightcraft_catalog::Source::File { path: dir.path(&format!("trip/{name}")) },
            name,
            "X",
            6,
            4,
            "2026-01-01",
        );
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let ids: Vec<lightcraft_catalog::PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    let one_by_one: Vec<_> = ids.iter().map(|id| s.sidecar_naming(*id)).collect();
    assert_eq!(s.sidecar_namings(&ids), one_by_one);
    assert!(one_by_one.contains(&crate::sidecar::SidecarNaming::Full), "the case that matters is there");
}
