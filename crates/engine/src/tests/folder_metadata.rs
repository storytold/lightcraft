//! Save Metadata to Files / Read Metadata from Files for a folder (`folder.saveMetadata`,
//! `folder.readMetadata`): scope, cancel, one undo step, failures that don't stop the rest,
//! hostile input. Procedural files in temp directories only.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::cmd::folder_metadata::{Direction, FolderMetadataJob};
use crate::tests_xmp::temp_dir;

fn write_png(path: &Path, seed: u8) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    let (w, h) = (16usize, 12usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 12) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library holding `a/one.png`, `a/two.png`, `a/sub/three.png` and `b/four.png`, auto-write off.
struct Fixture {
    s: Session,
    src: PathBuf,
    lib: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let src = temp_dir(&format!("fm-{tag}-src"));
        let lib = temp_dir(&format!("fm-{tag}-lib"));
        for (i, rel) in ["a/one.png", "a/two.png", "a/sub/three.png", "b/four.png"].iter().enumerate() {
            write_png(&src.join(rel), i as u8 + 1);
        }
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        s.execute("library.xmpPreferences", &json!({"autoWrite": false})).unwrap();
        let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 4, "{r}");
        Fixture { s, src, lib }
    }

    fn id(&self, name: &str) -> PhotoId {
        self.s.catalog.photos().find(|p| p.file_name == name).unwrap().id
    }

    fn rating(&self, name: &str) -> u8 {
        self.s.catalog.photo(self.id(name)).unwrap().rating
    }

    fn rate_all(&mut self, rating: u8) {
        let ids: Vec<u64> = self.s.catalog.photos().map(|p| p.id.0).collect();
        self.s.execute("photo.rate", &json!({"ids": ids, "rating": rating})).unwrap();
    }

    fn folder(&self, rel: &str) -> String {
        self.src.join(rel).to_string_lossy().into_owned()
    }

    fn sidecar(&self, rel: &str) -> PathBuf {
        self.src.join(rel)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.src);
        let _ = std::fs::remove_dir_all(&self.lib);
    }
}

fn count(v: &Value, key: &str) -> usize {
    v[key].as_array().map_or(usize::MAX, Vec::len)
}

#[test]
fn saving_a_folder_writes_its_photos_and_the_folders_inside_only() {
    let mut f = Fixture::new("save-scope");
    f.rate_all(3);
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!((r["total"].as_u64(), r["done"].as_u64(), r["cancelled"].as_bool()), (Some(3), Some(3), Some(false)), "{r}");
    assert_eq!((count(&r, "written"), count(&r, "failed")), (3, 0), "{r}");
    for rel in ["a/one.xmp", "a/two.xmp", "a/sub/three.xmp"] {
        assert!(f.sidecar(rel).is_file(), "{rel}");
    }
    assert!(!f.sidecar("b/four.xmp").exists(), "a photo outside the folder is untouched");
    // a folder beside it writes only its own
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b")})).unwrap();
    assert_eq!(count(&r, "written"), 1, "{r}");
    assert!(f.sidecar("b/four.xmp").is_file());
    // and a subfolder alone leaves the folder around it as it is
    std::fs::remove_file(f.sidecar("a/one.xmp")).unwrap();
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a/sub")})).unwrap();
    assert_eq!((r["total"].as_u64(), count(&r, "written")), (Some(1), 1), "{r}");
    assert!(!f.sidecar("a/one.xmp").exists(), "a/one.png is not in a/sub");
    // the row ends with the command
    assert!(f.s.activity.list().is_empty());
}

#[test]
fn saving_leaves_local_browse_records_deleted_photos_and_virtual_copies_alone() {
    let mut f = Fixture::new("save-kinds");
    // a Local-only record inside the folder: a file seen while browsing, never imported
    write_png(&f.src.join("a/browsed.png"), 9);
    let mut local =
        Photo::new(PhotoId(900_001), Source::File { path: f.folder("a/browsed.png") }, "browsed.png", "PNG", 16, 12, "2026-10-25T00:00:00");
    local.local = true;
    f.s.execute_fn("test.add", |s| s.apply_system(Op::AddPhoto { photo: Box::new(local) }).map(|()| Value::Null)).unwrap();
    // a deleted photo and a virtual copy
    let (two, one) = (f.id("two.png"), f.id("one.png"));
    f.s.execute("photo.delete", &json!({"ids": [two.0]})).unwrap();
    f.s.execute("photo.virtualCopy", &json!({"ids": [one.0]})).unwrap();
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!(r["total"], 2, "one.png and three.png: {r}");
    assert_eq!((count(&r, "written"), count(&r, "failed")), (2, 0), "{r}");
    assert!(f.sidecar("a/one.xmp").is_file() && f.sidecar("a/sub/three.xmp").is_file());
    assert!(!f.sidecar("a/browsed.xmp").exists(), "a Local-only record is not a library photo");
    assert!(!f.sidecar("a/two.xmp").exists(), "a deleted photo is not in the folder");
}

#[test]
fn a_missing_file_is_reported_and_the_rest_are_saved() {
    let mut f = Fixture::new("save-missing");
    let two = f.id("two.png");
    std::fs::remove_file(f.src.join("a/two.png")).unwrap();
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!((count(&r, "written"), count(&r, "failed")), (2, 1), "{r}");
    assert_eq!(r["failed"][0]["id"], two.0, "{r}");
    assert!(r["failed"][0]["error"].as_str().unwrap().contains("two.png"), "{r}");
    assert_eq!(r["done"], 3, "{r}");
    assert!(f.sidecar("a/one.xmp").is_file() && f.sidecar("a/sub/three.xmp").is_file());
}

#[test]
fn a_save_merges_into_an_existing_sidecar_as_the_photo_command_does() {
    let mut f = Fixture::new("save-merge");
    std::fs::write(
        f.sidecar("a/one.xmp"),
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:foo=\"http://example.com/foo/\" foo:keep=\"yes\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!(count(&r, "merged"), 1, "{r}");
    let text = std::fs::read_to_string(f.sidecar("a/one.xmp")).unwrap();
    assert!(text.contains("foo:keep"), "another app's data is kept: {text}");
}

#[test]
fn cancelling_a_save_stops_between_photos_and_reports_what_was_done() {
    let mut f = Fixture::new("save-cancel");
    let mut job = FolderMetadataJob::plan(&f.s, Direction::Save, &f.folder("a")).unwrap();
    assert_eq!((job.total(), job.done()), (3, 0));
    let rows = f.s.activity.list();
    assert_eq!((rows.len(), rows[0].kind, rows[0].total, rows[0].cancellable), (1, "saveMetadata", 3, true), "{rows:?}");
    assert!(job.step(&mut f.s), "more photos are left");
    assert_eq!(f.s.activity.list()[0].done, 1);
    f.s.execute("activity.cancel", &json!({"id": job.task_id()})).unwrap();
    assert!(!job.step(&mut f.s), "no photo is started once cancelled");
    let r = job.finish(&mut f.s).unwrap();
    assert_eq!((r["total"].as_u64(), r["done"].as_u64(), r["cancelled"].as_bool()), (Some(3), Some(1), Some(true)), "{r}");
    assert_eq!(count(&r, "written"), 1, "the file in hand was finished: {r}");
    let written = ["a/one.xmp", "a/two.xmp", "a/sub/three.xmp"].iter().filter(|rel| f.sidecar(rel).exists()).count();
    assert_eq!(written, 1, "the sidecars written before the cancel stay, no others are written");
    assert!(f.s.activity.list().is_empty(), "the row ends with the job");
}

/// Save every sidecar with rating 3, then give every photo rating 1 in the catalog only (auto-write off).
fn saved_at_three(f: &mut Fixture) {
    f.rate_all(3);
    let ids: Vec<u64> = f.s.catalog.photos().map(|p| p.id.0).collect();
    let r = f.s.execute("photo.saveMetadataToFile", &json!({"ids": ids})).unwrap();
    assert_eq!(count(&r, "written"), 4, "{r}");
    f.rate_all(1);
}

#[test]
fn reading_a_folder_is_one_undo_step_and_only_that_folder() {
    let mut f = Fixture::new("read-scope");
    saved_at_three(&mut f);
    let before = f.s.commits();
    let r = f.s.execute("folder.readMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!((r["total"].as_u64(), r["cancelled"].as_bool()), (Some(3), Some(false)), "{r}");
    assert_eq!((count(&r, "read"), count(&r, "failed")), (3, 0), "{r}");
    assert_eq!(f.s.commits(), before + 1, "one commit for the whole folder");
    for n in ["one.png", "two.png", "three.png"] {
        assert_eq!(f.rating(n), 3, "{n}");
    }
    assert_eq!(f.rating("four.png"), 1, "a photo outside the folder is not read");
    f.s.execute("edit.undo", &json!({})).unwrap();
    for n in ["one.png", "two.png", "three.png", "four.png"] {
        assert_eq!(f.rating(n), 1, "one undo restores {n}");
    }
    assert!(f.s.activity.list().is_empty());
}

#[test]
fn reading_does_not_write_the_sidecars_it_reads_even_with_auto_write_on() {
    let mut f = Fixture::new("read-noautowrite");
    saved_at_three(&mut f);
    let bytes = |f: &Fixture| std::fs::read(f.sidecar("a/one.xmp")).unwrap();
    let before = bytes(&f);
    f.s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    f.s.execute("folder.readMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!(f.rating("one.png"), 3);
    assert_eq!(bytes(&f), before, "the sidecar is as it was");
    assert!(f.s.unsaved_sidecars() == 0);
}

#[test]
fn a_photo_without_a_sidecar_is_reported_and_the_rest_are_read() {
    let mut f = Fixture::new("read-missing");
    saved_at_three(&mut f);
    std::fs::remove_file(f.sidecar("a/two.xmp")).unwrap();
    let two = f.id("two.png");
    let r = f.s.execute("folder.readMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!((count(&r, "read"), count(&r, "failed")), (2, 1), "{r}");
    assert_eq!(r["failed"][0]["id"], two.0, "{r}");
    assert_eq!((f.rating("one.png"), f.rating("two.png"), f.rating("three.png")), (3, 1, 3));
}

#[test]
fn an_unreadable_sidecar_is_reported_and_the_rest_are_read() {
    let mut f = Fixture::new("read-bad");
    saved_at_three(&mut f);
    std::fs::write(f.sidecar("a/one.xmp"), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
    let r = f.s.execute("folder.readMetadata", &json!({"path": f.folder("a")})).unwrap();
    assert_eq!((count(&r, "read"), count(&r, "failed")), (2, 1), "{r}");
    assert_eq!((f.rating("one.png"), f.rating("two.png")), (1, 3));
}

#[test]
fn cancelling_a_read_commits_what_was_read_as_one_undo_step() {
    let mut f = Fixture::new("read-cancel");
    saved_at_three(&mut f);
    let before = f.s.commits();
    let mut job = FolderMetadataJob::plan(&f.s, Direction::Read, &f.folder("a")).unwrap();
    assert_eq!(f.s.activity.list()[0].kind, "readMetadata");
    assert!(job.step(&mut f.s));
    assert!(job.step(&mut f.s));
    f.s.execute("activity.cancel", &json!({"all": true})).unwrap();
    assert!(!job.step(&mut f.s));
    let r = f.s.execute_fn("test.finish", |s| job.finish(s)).unwrap();
    assert_eq!((r["total"].as_u64(), r["done"].as_u64(), r["cancelled"].as_bool()), (Some(3), Some(2), Some(true)), "{r}");
    assert_eq!(count(&r, "read"), 2, "{r}");
    assert_eq!(f.s.commits(), before + 1);
    let read: usize = ["one.png", "two.png", "three.png"].iter().filter(|n| f.rating(n) == 3).count();
    assert_eq!(read, 2, "what was read is kept, the rest is not read");
    f.s.execute("edit.undo", &json!({})).unwrap();
    assert!(["one.png", "two.png", "three.png"].iter().all(|n| f.rating(n) == 1), "one undo step takes it back");
}

#[test]
fn a_photo_relinked_while_the_job_runs_is_not_written() {
    let mut f = Fixture::new("relinked");
    let mut job = FolderMetadataJob::plan(&f.s, Direction::Save, &f.folder("a")).unwrap();
    let ids: Vec<PhotoId> = ["one.png", "two.png", "three.png"].iter().map(|n| f.id(n)).collect();
    f.s.execute("photo.delete", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    f.s.execute("edit.undo", &json!({})).unwrap();
    f.s.execute_fn("test.relink", |s| {
        for id in &ids {
            let path = s.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_default();
            s.apply_system(Op::Relink { id: *id, file_name: path.clone(), source: Source::File { path: format!("/nowhere/{path}") }, format: None })?;
        }
        Ok(Value::Null)
    })
    .unwrap();
    while job.step(&mut f.s) {}
    let r = job.finish(&mut f.s).unwrap();
    assert_eq!((count(&r, "written"), count(&r, "failed")), (0, 3), "{r}");
    assert!(!f.sidecar("a/one.xmp").exists());
}

#[test]
fn hostile_input_is_an_error_never_a_panic_or_a_write() {
    let mut f = Fixture::new("hostile");
    let nowhere = f.src.join("nowhere").to_string_lossy().into_owned();
    let empty = f.src.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let empty = empty.to_string_lossy().into_owned();
    let cases = [
        json!({}),
        json!({"path": null}),
        json!({"path": 5}),
        json!({"path": ["a"]}),
        json!({"path": ""}),
        json!({"path": "   "}),
        json!({"path": "relative/dir"}),
        json!({"path": "../../.."}),
        json!({"path": "\u{0}"}),
        json!({"path": nowhere}),
        json!({"path": empty}),
        json!({"path": f.folder("a/one.png")}),
        json!({"path": format!("{}/a/../nowhere", f.src.display()), "ids": "x"}),
    ];
    for id in ["folder.saveMetadata", "folder.readMetadata"] {
        for case in &cases {
            let r = f.s.execute(id, case);
            assert!(r.is_err() || r.as_ref().is_ok_and(|v| v["total"].as_u64().is_some()), "{id} {case}: {r:?}");
        }
    }
    assert!(!f.sidecar("a/one.xmp").exists() && !f.sidecar("b/four.xmp").exists(), "nothing was written");
    assert!(f.s.activity.list().is_empty(), "no row is left behind");
    assert!(f.s.execute("folder.saveMetadata", &json!({"path": empty})).is_err());
    // and a good call still works after them
    assert_eq!(f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b")})).unwrap()["total"], 1);
}

#[cfg(unix)] // Windows drops a trailing space from a folder name
#[test]
fn a_folder_named_with_a_space_at_its_edge_is_not_its_neighbor() {
    let src = temp_dir("fm-space-src");
    let lib = temp_dir("fm-space-lib");
    write_png(&src.join("shoot /a.png"), 1);
    write_png(&src.join("shoot/b.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": false})).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let r = s.execute("folder.saveMetadata", &json!({"path": format!("{}/shoot ", src.display())})).unwrap();
    assert_eq!(r["total"], 1, "{r}");
    assert!(src.join("shoot /a.xmp").is_file(), "the folder with the space is the one written");
    assert!(!src.join("shoot/b.xmp").exists(), "its neighbor is not");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Pump until the job ends (at most `max` times); returns the pump's last reply.
fn pump_until_done(f: &mut Fixture, max: usize) -> Value {
    for _ in 0..max {
        let r = f.s.execute("folder.metadataPump", &json!({"budgetMs": 1})).unwrap();
        if r["running"] == false {
            return r;
        }
    }
    panic!("the job did not end in {max} pumps");
}

#[test]
fn a_background_save_is_done_by_the_pump_and_shows_its_row() {
    let mut f = Fixture::new("bg-save");
    assert_eq!(f.s.execute("folder.metadataPump", &json!({})).unwrap(), json!({"running": false}), "no job: nothing to do");
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    assert_eq!(r, json!({"started": true, "total": 3}));
    assert!(!f.sidecar("a/one.xmp").exists(), "starting writes nothing");
    let rows = f.s.activity.list();
    assert_eq!((rows.len(), rows[0].kind, rows[0].done, rows[0].total), (1, "saveMetadata", 0, 3), "{rows:?}");
    // a second one is refused while the first is waiting, plain or not
    assert!(f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b")})).is_err());
    assert!(f.s.execute("folder.readMetadata", &json!({"path": f.folder("b"), "background": true})).is_err());
    let r = pump_until_done(&mut f, 100);
    assert_eq!((r["result"]["total"].as_u64(), count(&r["result"], "written"), r["result"]["cancelled"].as_bool()), (Some(3), 3, Some(false)), "{r}");
    assert!(f.sidecar("a/sub/three.xmp").is_file() && !f.sidecar("b/four.xmp").exists());
    assert!(f.s.activity.list().is_empty(), "the row ends with the job");
    assert_eq!(f.s.execute("folder.metadataPump", &json!({})).unwrap(), json!({"running": false}));
    // and the next one starts
    f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b"), "background": true})).unwrap();
}

#[test]
fn stopping_a_background_read_between_pumps_commits_what_was_read_once() {
    let mut f = Fixture::new("bg-read-cancel");
    saved_at_three(&mut f);
    let before = f.s.commits();
    f.s.execute("folder.readMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    // a pump with a zero budget still does one photo
    let r = f.s.execute("folder.metadataPump", &json!({"budgetMs": 0})).unwrap();
    assert_eq!((r["running"].as_bool(), r["done"].as_u64(), r["total"].as_u64()), (Some(true), Some(1), Some(3)), "{r}");
    let row = f.s.activity.list();
    assert_eq!((row[0].kind, row[0].done), ("readMetadata", 1));
    f.s.execute("activity.cancel", &json!({"id": row[0].id})).unwrap();
    let r = f.s.execute("folder.metadataPump", &json!({})).unwrap();
    assert_eq!((r["running"].as_bool(), r["result"]["cancelled"].as_bool(), r["result"]["done"].as_u64()), (Some(false), Some(true), Some(1)), "{r}");
    assert_eq!(count(&r["result"], "read"), 1, "{r}");
    assert_eq!(f.s.commits(), before + 1, "one undo step");
    assert_eq!(["one.png", "two.png", "three.png"].iter().filter(|n| f.rating(n) == 3).count(), 1);
    assert!(f.s.activity.list().is_empty());
    f.s.execute("edit.undo", &json!({})).unwrap();
    assert!(["one.png", "two.png", "three.png"].iter().all(|n| f.rating(n) == 1));
}

#[test]
fn a_photo_edited_while_a_background_read_waits_keeps_its_edit() {
    let mut f = Fixture::new("bg-read-edit");
    saved_at_three(&mut f);
    f.s.execute("folder.readMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    f.s.execute("folder.metadataPump", &json!({"budgetMs": 0})).unwrap();
    let r = f.s.execute("folder.metadataPump", &json!({"budgetMs": 0})).unwrap();
    assert_eq!((r["running"].as_bool(), r["done"].as_u64()), (Some(true), Some(2)), "two photos are read, nothing is committed yet: {r}");
    // every photo is edited before the job ends: the two already read must keep the edit
    let ids: Vec<u64> = ["one.png", "two.png", "three.png"].iter().map(|n| f.id(n).0).collect();
    f.s.execute("photo.rate", &json!({"ids": ids, "rating": 5})).unwrap();
    let r = pump_until_done(&mut f, 10);
    let done = &r["result"];
    assert_eq!((done["total"].as_u64(), count(done, "read"), count(done, "failed")), (Some(3), 1, 2), "{r}");
    assert!(done["failed"][0]["error"].as_str().unwrap().contains("changed"), "{r}");
    let ratings: Vec<u8> = ["one.png", "two.png", "three.png"].iter().map(|n| f.rating(n)).collect();
    assert_eq!(ratings.iter().filter(|r| **r == 5).count(), 2, "the edit stays on the two photos read before it: {ratings:?}");
    assert_eq!(ratings.iter().filter(|r| **r == 3).count(), 1, "the photo read after the edit takes its file: {ratings:?}");
}

#[test]
fn a_photo_deleted_while_a_background_save_waits_is_not_written() {
    let mut f = Fixture::new("bg-save-deleted");
    f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    let ids: Vec<(String, PhotoId)> = ["one.png", "two.png", "three.png"].iter().map(|n| (n.to_string(), f.id(n))).collect();
    for (_, id) in &ids {
        f.s.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
    }
    let r = pump_until_done(&mut f, 10);
    assert_eq!((count(&r["result"], "written"), count(&r["result"], "failed")), (0, 3), "{r}");
    assert!(!f.sidecar("a/one.xmp").exists());
}

#[test]
fn opening_another_library_drops_a_waiting_job() {
    let mut f = Fixture::new("bg-switch");
    f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    let other = temp_dir("fm-switch-other");
    f.s.open_library(&other, false).unwrap();
    assert!(f.s.activity.list().is_empty(), "its row ended");
    assert_eq!(f.s.execute("folder.metadataPump", &json!({})).unwrap(), json!({"running": false}));
    assert!(!f.sidecar("a/one.xmp").exists());
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn a_read_names_each_photos_sidecar_as_the_photo_command_does() {
    // a raw-less stem shared by a PNG and a JPG: the first by file name owns `x.xmp`, the other `x.jpg.xmp`
    let src = temp_dir("fm-stem-src");
    let lib = temp_dir("fm-stem-lib");
    write_png(&src.join("d/x.png"), 1);
    write_png(&src.join("d/x.tif"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": false})).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    s.execute("photo.rate", &json!({"ids": ids, "rating": 2})).unwrap();
    s.execute("folder.saveMetadata", &json!({"path": src.join("d").to_string_lossy()})).unwrap();
    let by_photo = s.execute("photo.readMetadataFromFile", &json!({"ids": ids})).unwrap();
    let by_folder = s.execute("folder.readMetadata", &json!({"path": src.join("d").to_string_lossy()})).unwrap();
    let mut a: Vec<String> = by_photo["read"].as_array().unwrap().iter().map(|r| format!("{}={}", r["id"], r["from"])).collect();
    let mut b: Vec<String> = by_folder["read"].as_array().unwrap().iter().map(|r| format!("{}={}", r["id"], r["from"])).collect();
    a.sort();
    b.sort();
    assert_eq!(a.len(), 2, "{by_photo}");
    assert_eq!(a, b, "the same sidecar for each photo");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_stopped_background_job_nobody_pumps_does_not_block_the_next_one() {
    let mut f = Fixture::new("bg-stopped");
    saved_at_three(&mut f);
    f.s.execute("folder.readMetadata", &json!({"path": f.folder("a"), "background": true})).unwrap();
    f.s.execute("folder.metadataPump", &json!({"budgetMs": 0})).unwrap();
    let before = f.s.commits();
    f.s.execute("activity.cancel", &json!({"all": true})).unwrap();
    // no pump follows: the next folder job ends the stopped one, which commits the one photo it read
    let r = f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b")})).unwrap();
    assert_eq!((r["total"].as_u64(), count(&r, "written")), (Some(1), 1), "{r}");
    assert_eq!(f.s.commits(), before + 1, "the stopped read kept what it had read");
    assert_eq!(["one.png", "two.png", "three.png"].iter().filter(|n| f.rating(n) == 3).count(), 1);
    assert!(f.s.activity.list().is_empty());
    // a job that was not stopped still blocks
    f.s.execute("folder.saveMetadata", &json!({"path": f.folder("b"), "background": true})).unwrap();
    assert!(f.s.execute("folder.saveMetadata", &json!({"path": f.folder("a")})).is_err());
}

#[test]
fn a_file_imported_while_a_background_save_waits_is_named_as_the_photo_command_would() {
    let src = temp_dir("fm-stem-late-src");
    let lib = temp_dir("fm-stem-late-lib");
    write_png(&src.join("d/x.tif"), 1);
    write_png(&src.join("other/x.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": false})).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("d").to_string_lossy()]})).unwrap();
    s.execute("folder.saveMetadata", &json!({"path": src.join("d").to_string_lossy(), "background": true})).unwrap();
    // x.png joins d/ before the first pump: it sorts before x.tif, so it now owns the stem sidecar x.xmp
    std::fs::copy(src.join("other/x.png"), src.join("d/x.png")).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("d/x.png").to_string_lossy()]})).unwrap();
    let r = s.execute("folder.metadataPump", &json!({})).unwrap();
    assert_eq!(r["result"]["total"], 1, "{r}");
    let id = s.catalog.photos().find(|p| p.file_name == "x.tif").unwrap().id;
    let by_photo = s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["result"]["written"], by_photo["written"], "the same sidecar path as the photo command: {r} vs {by_photo}");
    assert!(!src.join("d/x.xmp").exists(), "the tif no longer takes the stem sidecar the png owns");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}
