//! Locating a library folder that is somewhere else now (see [`crate::relocate`]; issue #1112).
//!
//! Scenarios, in the words of someone whose folder is not where the library looks for it:
//!
//! * Given a folder of the library that was moved or renamed outside LightCraft (or whose
//!   catalog came from another computer), when the person says where it is now, its photos —
//!   those in the folders inside it and in Recently Deleted too — point at the same places below
//!   the new folder. No file is touched, and it is one undo step.
//! * A path written by Windows (`D:\Photos\…`) is located on a computer that spells paths `/…`.
//! * What the library knows about the folder (its colour label) and the folder being shown
//!   follow; undo brings both back.
//! * A folder that holds none of the photos' files is refused: it is not the folder. One that
//!   holds only some is refused unless the person says so; the others then stay missing there.
//!   Looking for the files can run apart from the session, and can be stopped.
//! * A folder whose files the library already has is refused, as are the folder itself, the
//!   library's own folder, a path that is no folder, and a folder that is not the library's.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use lightcraft_catalog::{ColorLabel, Filter, Op, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::relocate::{Relocation, look_at};

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-relocate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).components().collect::<PathBuf>().to_string_lossy().into_owned()
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

/// A library holding `trip/a.png` and `trip/day1/b.png`.
fn library(dir: &Scratch) -> Session {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/day1/b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    assert_eq!(s.catalog.len(), 2);
    s
}

/// The folder moved away whole, outside LightCraft: `trip` is now `archive/journey`.
fn moved_away(dir: &Scratch) -> String {
    std::fs::create_dir_all(dir.0.join("archive")).unwrap();
    std::fs::rename(dir.0.join("trip"), dir.0.join("archive/journey")).unwrap();
    dir.path("archive/journey")
}

/// Where the library looks for each photo's file.
fn files(s: &Session) -> Vec<String> {
    let mut v: Vec<String> = s
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// [`crate::relocate::relocate`] with the parameters named as an agent would give them
/// (`{folder, path, disk?, incomplete?}`).
fn update_location(s: &mut Session, params: &Value) -> crate::Result<Value> {
    let flag = |key: &str| params.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text = |key: &str| params[key].as_str().unwrap().to_string();
    crate::relocate::relocate(s, &text("folder"), &text("path"), flag("disk"), flag("incomplete"))
}

fn relocate(s: &mut Session, from: &str, to: &str) -> Value {
    update_location(s, &json!({"folder": from, "path": to})).unwrap()
}

fn refused(s: &mut Session, params: Value) -> String {
    let (before, steps) = (files(s), s.undo.len());
    let e = update_location(s, &params).unwrap_err().to_string();
    assert_eq!(files(s), before, "a refusal changes nothing");
    assert_eq!(s.undo.len(), steps, "and leaves no undo step");
    e
}

#[test]
fn a_folder_moved_outside_lightcraft_is_located_and_its_photos_follow() {
    let dir = Scratch::new("follow");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    assert_eq!(s.execute("library.missing", &json!({})).unwrap().as_array().unwrap().len(), 2, "both are missing first");

    let r = relocate(&mut s, &dir.path("trip"), &now);

    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64(), r["notFound"].as_u64()), (Some(2), Some(2), Some(0)), "{r}");
    assert_eq!(r["path"], json!(now));
    assert_eq!(files(&s), [dir.path("archive/journey/a.png"), dir.path("archive/journey/day1/b.png")]);
    assert!(s.execute("library.missing", &json!({})).unwrap().as_array().unwrap().is_empty(), "nothing is missing any more");
    let scan = s.execute("folder.scanChanges", &json!({"path": now})).unwrap();
    for key in ["new", "missing", "moved", "metadata"] {
        assert!(scan[key].as_array().unwrap().is_empty(), "{key}: the library is up to date with the folder: {scan}");
    }
}

#[test]
fn locating_is_one_undo_step_and_touches_no_file() {
    let dir = Scratch::new("undo");
    let mut s = library(&dir);
    let before = files(&s);
    let now = moved_away(&dir);
    let steps = s.undo.len();

    relocate(&mut s, &dir.path("trip"), &now);
    assert_eq!(s.undo.len(), steps + 1);
    let after = files(&s);

    s.undo_step().unwrap();
    assert_eq!(files(&s), before, "undo points the photos back");
    assert!(Path::new(&now).join("a.png").is_file() && !dir.0.join("trip").exists(), "and moves nothing on disk");
    s.redo_step().unwrap();
    assert_eq!(files(&s), after);
    assert!(Path::new(&now).join("day1/b.png").is_file() && !dir.0.join("trip").exists());
}

/// Point every photo of the library at the same place below `root`, as a catalog from
/// another computer would have it.
fn as_written_elsewhere(s: &mut Session, dir: &Scratch, root: &str, sep: &str) {
    let ops = s
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            Source::File { path } => {
                let rel: Vec<String> =
                    Path::new(path).strip_prefix(&dir.0).unwrap().components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                Some(Op::Relink {
                    id: p.id,
                    file_name: p.file_name.clone(),
                    source: Source::File { path: format!("{root}{}", rel.join(sep)) },
                    format: None,
                })
            }
            _ => None,
        })
        .collect();
    s.commit("elsewhere", Op::Batch { ops }).unwrap();
}

#[test]
fn a_folder_of_a_windows_catalog_is_located_on_this_computer() {
    let dir = Scratch::new("windows");
    let mut s = library(&dir);
    as_written_elsewhere(&mut s, &dir, "D:\\Photos\\", "\\");
    assert_eq!(files(&s), ["D:\\Photos\\trip\\a.png", "D:\\Photos\\trip\\day1\\b.png"]);

    let r = relocate(&mut s, "D:\\Photos\\trip", &dir.path("trip"));

    assert_eq!(r["relinked"], json!(2), "{r}");
    assert_eq!(files(&s), [dir.path("trip/a.png"), dir.path("trip/day1/b.png")], "spelled the way this computer spells paths");
}

#[test]
fn a_whole_disk_is_located_only_on_request() {
    let dir = Scratch::new("disk");
    let mut s = library(&dir);
    as_written_elsewhere(&mut s, &dir, "D:\\", "\\");

    let e = refused(&mut s, json!({"folder": "D:\\", "path": dir.path("")}));
    assert!(e.contains("disk: true"), "{e}");

    let r = update_location(&mut s, &json!({"folder": "D:\\", "path": dir.path(""), "disk": true})).unwrap();
    assert_eq!(r["relinked"], json!(2), "{r}");
    assert_eq!(files(&s), [dir.path("trip/a.png"), dir.path("trip/day1/b.png")]);
}

#[test]
fn photos_in_recently_deleted_follow_their_folder() {
    let dir = Scratch::new("trash");
    let mut s = library(&dir);
    let id = s.catalog.photos().find(|p| p.file_name == "a.png").map(|p| p.id).unwrap();
    s.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
    let now = moved_away(&dir);

    let r = relocate(&mut s, &dir.path("trip"), &now);

    // (the one in the trash is not looked for, but restoring it must find its file)
    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64()), (Some(2), Some(1)), "{r}");
    assert_eq!(files(&s), [dir.path("archive/journey/a.png"), dir.path("archive/journey/day1/b.png")]);
}

#[test]
fn the_folders_colour_labels_follow() {
    let dir = Scratch::new("labels");
    let mut s = library(&dir);
    s.execute("folder.label", &json!({"path": dir.path("trip"), "label": "red"})).unwrap();
    s.execute("folder.label", &json!({"path": dir.path("trip/day1"), "label": "blue"})).unwrap();
    let now = moved_away(&dir);

    relocate(&mut s, &dir.path("trip"), &now);

    assert_eq!(s.catalog.folder_color_label(&now), Some(ColorLabel::Red));
    assert_eq!(s.catalog.folder_color_label(&dir.path("archive/journey/day1")), Some(ColorLabel::Blue));
    assert_eq!(s.catalog.folder_color_label(&dir.path("trip")), None, "nothing is left at the old place");
    s.undo_step().unwrap();
    assert_eq!(s.catalog.folder_color_label(&dir.path("trip")), Some(ColorLabel::Red), "undo brings the labels back with the photos");
    assert_eq!(s.catalog.folder_color_label(&now), None);
}

#[test]
fn the_folder_being_shown_follows_and_comes_back_on_undo() {
    let dir = Scratch::new("shown");
    let mut s = library(&dir);
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": dir.path("trip/day1")})).unwrap();
    let now = moved_away(&dir);

    relocate(&mut s, &dir.path("trip"), &now);
    assert_eq!(s.library_folder.as_deref(), Some(dir.path("archive/journey/day1").as_str()));
    assert_eq!(s.visible().len(), 1, "its photo is still the one shown");

    s.undo_step().unwrap();
    assert_eq!(s.library_folder.as_deref(), Some(dir.path("trip/day1").as_str()));
    s.redo_step().unwrap();
    assert_eq!(s.library_folder.as_deref(), Some(dir.path("archive/journey/day1").as_str()));
}

#[test]
fn the_filter_set_aside_by_enable_filters_follows_too() {
    let dir = Scratch::new("shelved");
    let mut s = library(&dir);
    let before = Filter { library_folder: Some(dir.path("trip/day1")), ..Default::default() };
    s.shelved_filter = Some(before);
    let now = moved_away(&dir);

    relocate(&mut s, &dir.path("trip"), &now);
    let shelved = |s: &Session| s.shelved_filter.as_ref().and_then(|f| f.library_folder.clone());
    assert_eq!(shelved(&s), Some(dir.path("archive/journey/day1")));

    s.undo_step().unwrap();
    assert_eq!(shelved(&s), Some(dir.path("trip/day1")));
}

/// A folder named with a space at its edge is that folder, not the one without the space.
#[cfg(unix)]
#[test]
fn names_with_spaces_at_their_edge_are_not_trimmed() {
    let dir = Scratch::new("edge-space");
    write_png(&dir.path("trip /a.png"), 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("trip ")]})).unwrap();
    std::fs::rename(dir.0.join("trip "), dir.0.join("moved ")).unwrap();

    let r = relocate(&mut s, &dir.path("trip "), &dir.path("moved "));

    assert_eq!(r["relinked"].as_u64(), Some(1), "{r}");
    assert_eq!(files(&s), [dir.path("moved /a.png")]);
}

#[test]
fn a_folder_that_holds_none_of_the_photos_is_refused() {
    let dir = Scratch::new("none");
    let mut s = library(&dir);
    moved_away(&dir);
    // one level too high: the files are in `archive/journey`, not in `archive`
    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": dir.path("archive")}));
    assert!(e.contains("none of the 2 files"), "{e}");
    // and saying "anyway" does not make it the folder
    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": dir.path("archive"), "incomplete": true}));
    assert!(e.contains("none of the 2 files"), "{e}");
}

#[test]
fn a_folder_that_holds_only_some_is_located_only_on_request() {
    let dir = Scratch::new("some");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    std::fs::remove_file(Path::new(&now).join("a.png")).unwrap();

    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": now}));
    assert!(e.contains("1 of the 2 files") && e.contains("incomplete: true"), "{e}");

    let r = update_location(&mut s, &json!({"folder": dir.path("trip"), "path": now, "incomplete": true})).unwrap();
    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64(), r["notFound"].as_u64()), (Some(2), Some(1), Some(1)), "{r}");
    // the one that isn't there is missing at the new place, where Synchronize Folder finds it
    let missing = s.execute("library.missing", &json!({})).unwrap();
    assert_eq!(missing.as_array().unwrap().len(), 1);
    assert_eq!(missing[0]["path"], json!(dir.path("archive/journey/a.png")));
}

#[test]
fn a_folder_whose_files_the_library_already_has_is_refused() {
    let dir = Scratch::new("taken");
    let mut s = library(&dir);
    write_png(&dir.path("copy/a.png"), 3);
    write_png(&dir.path("copy/day1/b.png"), 4);
    s.execute("library.import", &json!({"paths": [dir.path("copy")]})).unwrap();
    assert_eq!(s.catalog.len(), 4);
    std::fs::remove_dir_all(dir.0.join("trip")).unwrap();

    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": dir.path("copy")}));
    assert!(e.contains("already in the library"), "{e}");
}

#[test]
fn only_a_missing_or_moved_library_folder_and_a_real_folder_are_taken() {
    let dir = Scratch::new("refusals");
    let mut s = library(&dir);
    let trip = dir.path("trip");
    std::fs::create_dir_all(dir.0.join("empty")).unwrap();

    for (params, why) in [
        (json!({"folder": trip, "path": trip}), "already looks"),
        (json!({"folder": trip, "path": format!("{trip}{}", std::path::MAIN_SEPARATOR)}), "already looks"),
        (json!({"folder": trip, "path": dir.path("nowhere")}), "is not a folder"),
        (json!({"folder": trip, "path": dir.path("trip/a.png")}), "is not a folder"),
        (json!({"folder": dir.path("empty"), "path": trip}), "no photo in the library"),
        (json!({"folder": "/", "path": trip}), "startup disk"),
    ] {
        let e = refused(&mut s, params.clone());
        assert!(e.contains(why), "{params}: {e}");
    }
}

#[test]
fn the_librarys_own_folder_is_no_place_for_photos() {
    let dir = Scratch::new("own");
    write_png(&dir.path("trip/a.png"), 1);
    let lib = dir.path("lib");
    let mut s = Session::new().with_fs();
    s.open_library(Path::new(&lib), false).unwrap();
    s.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    std::fs::create_dir_all(dir.0.join("lib/inside")).unwrap();

    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": dir.path("lib/inside")}));
    assert!(e.contains("library's own"), "{e}");
}

#[test]
fn the_plan_names_each_file_once_and_the_count_can_be_stopped() {
    let dir = Scratch::new("count");
    let mut s = library(&dir);
    // a virtual copy shares its photo's file: one file to look for
    let id = s.catalog.photos().find(|p| p.file_name == "a.png").map(|p| p.id).unwrap();
    s.execute("photo.virtualCopy", &json!({"ids": [id.0]})).unwrap();
    let now = moved_away(&dir);

    let plan = Relocation::new(&s, &dir.path("trip"), &now, false).unwrap();
    assert_eq!(plan.files().len(), 2, "{:?}", plan.files());
    let progress = crate::import::ScanProgress::default();
    assert_eq!(look_at(plan.files(), &Default::default(), &progress).found, 2);
    assert_eq!((progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed)), (2, 2));

    // ✕ on its row in the activity stack
    let stopped = crate::import::ScanProgress::default();
    stopped.cancel.store(true, Ordering::Relaxed);
    assert_eq!(look_at(plan.files(), &Default::default(), &stopped).found, 0, "nothing more is looked for");

    let r = relocate(&mut s, &dir.path("trip"), &now);
    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64()), (Some(3), Some(2)), "the copy follows its photo: {r}");
    assert!(s.catalog.photo(PhotoId(id.0)).is_some());
}

#[test]
fn hostile_parameters_are_refused_not_crashed_on() {
    let dir = Scratch::new("hostile");
    let mut s = library(&dir);
    for params in [
        json!({"folder": "", "path": ""}),
        json!({"folder": "..", "path": "."}),
        json!({"folder": dir.path("trip"), "path": "\0"}),
        json!({"folder": "\\\\?\\", "path": dir.path("trip"), "disk": true}),
    ] {
        refused(&mut s, params);
    }
}

#[test]
fn a_new_place_already_browsed_in_local_does_not_bring_its_files_in_twice() {
    let dir = Scratch::new("browsed");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    // looking for the photos, the person browsed the folder where they are now
    s.execute("library.browse", &json!({"path": now})).unwrap();
    s.execute("library.browse", &json!({"path": dir.path("archive/journey/day1")})).unwrap();
    let records = s.catalog.len();
    assert!(records > 2, "browsing catalogued the files as Local records");

    let r = relocate(&mut s, &dir.path("trip"), &now);
    assert_eq!(r["relinked"], json!(2), "{r}");

    let scan = s.execute("folder.scanChanges", &json!({"path": now})).unwrap();
    assert!(scan["new"].as_array().unwrap().is_empty(), "the library's photos are these files: {scan}");
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 2);
}

#[test]
fn undo_brings_the_local_records_back_and_work_done_while_browsing_is_not_given_up() {
    let dir = Scratch::new("browsed-undo");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    s.execute("library.browse", &json!({"path": now})).unwrap();
    let records = s.catalog.len();
    let local = s.catalog.photos().find(|p| p.local).map(|p| p.id).unwrap();
    // the photo being looked at in Local is one of those that give way
    s.execute("library.select", &json!({"ids": [local.0]})).unwrap();

    let r = relocate(&mut s, &dir.path("trip"), &now);
    assert_eq!(r["replacedLocal"], json!(records - 2), "{r}");
    assert!(s.catalog.photo(local).is_none());
    assert!(s.selection.ids.iter().all(|id| s.catalog.photo(*id).is_some()), "nothing that is gone stays selected");
    s.undo_step().unwrap();
    assert_eq!(s.catalog.len(), records, "undo brings the Local records back");
    assert_eq!(files(&s).iter().filter(|f| f.starts_with(&dir.path("trip"))).count(), 2);

    // a rating given while browsing is the person's work: the library's photo can't replace it
    s.execute("library.select", &json!({"ids": [local.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": now}));
    assert!(e.contains("while browsing it in Local"), "{e}");
}

#[test]
fn what_was_found_is_not_applied_to_a_library_opened_meanwhile() {
    let dir = Scratch::new("switched");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    let located = Relocation::new(&s, &dir.path("trip"), &now, false).unwrap().look(&crate::import::ScanProgress::default()).unwrap();
    // File › Open Library while the files were looked for: its photo ids mean other photos
    s.library_identity = std::sync::Arc::new(());
    let (before, steps) = (files(&s), s.undo.len());

    let e = located.commit(&mut s, false).unwrap_err().to_string();

    assert!(e.contains("another library"), "{e}");
    assert_eq!(files(&s), before, "nothing landed in the other library");
    assert_eq!(s.undo.len(), steps);
}

#[test]
fn a_photo_relinked_while_the_files_were_looked_for_is_left_where_it_is() {
    let dir = Scratch::new("meanwhile");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    let located = Relocation::new(&s, &dir.path("trip"), &now, false).unwrap().look(&crate::import::ScanProgress::default()).unwrap();
    // meanwhile the person located one photo by hand, somewhere else
    write_png(&dir.path("elsewhere/a.png"), 1);
    let id = s.catalog.photos().find(|p| p.file_name == "a.png").map(|p| p.id).unwrap();
    s.execute("photo.relink", &json!({"id": id.0, "path": dir.path("elsewhere/a.png")})).unwrap();

    let r = located.commit(&mut s, false).unwrap();

    assert_eq!(r["relinked"], json!(1), "{r}");
    assert_eq!(files(&s), [dir.path("archive/journey/day1/b.png"), dir.path("elsewhere/a.png")], "its own choice stands");
}

/// What `look` found for `trip` → `to`, the files not yet relinked.
fn looked(s: &Session, dir: &Scratch, to: &str) -> crate::relocate::Located {
    Relocation::new(s, &dir.path("trip"), to, false).unwrap().look(&crate::import::ScanProgress::default()).unwrap()
}

#[test]
fn a_new_place_browsed_while_the_files_were_looked_for_still_has_one_record_per_file() {
    let dir = Scratch::new("browsed-meanwhile");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    let located = looked(&s, &dir, &now);
    // meanwhile the person clicked the folder in Local
    s.execute("library.browse", &json!({"path": now})).unwrap();
    let local = s.catalog.photos().filter(|p| p.local).count();
    assert!(local > 0);

    let r = located.commit(&mut s, false).unwrap();

    assert_eq!(r["replacedLocal"], json!(local), "{r}");
    let mut seen = std::collections::HashSet::new();
    assert!(files(&s).into_iter().all(|f| seen.insert(f)), "one record per file: {:?}", files(&s));
}

#[test]
fn a_file_that_became_another_photos_while_the_files_were_looked_for_is_not_shared() {
    let dir = Scratch::new("taken-meanwhile");
    let mut s = library(&dir);
    write_png(&dir.path("other/x.png"), 9);
    s.execute("library.import", &json!({"paths": [dir.path("other")]})).unwrap();
    let now = moved_away(&dir);
    let located = looked(&s, &dir, &now);
    // meanwhile another photo was pointed at one of the files
    let other = s.catalog.photos().find(|p| p.file_name == "x.png").map(|p| p.id).unwrap();
    s.execute("photo.relink", &json!({"id": other.0, "path": dir.path("archive/journey/a.png")})).unwrap();
    let (before, steps) = (files(&s), s.undo.len());

    let e = located.commit(&mut s, false).unwrap_err().to_string();

    assert!(e.contains("already in the library"), "{e}");
    assert_eq!((files(&s), s.undo.len()), (before, steps), "nothing changed");
}

#[test]
fn a_browsed_record_in_recently_deleted_at_the_new_place_is_not_doubled() {
    let dir = Scratch::new("local-trash");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    s.execute("library.browse", &json!({"path": now})).unwrap();
    let local = s.catalog.photos().find(|p| p.local).map(|p| p.id).unwrap();
    s.catalog.apply(Op::SetDeleted { id: local, deleted: true }).unwrap();

    // restoring it later would make a second photo of the file
    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": now}));
    assert!(e.contains("Recently Deleted included"), "{e}");
}

/// A library that also holds `archive/journey/phone/p.png` (labelled red), with the files of
/// `trip` moved into `archive/journey` behind its back.
fn moved_into_a_folder_of_the_library(dir: &Scratch) -> (Session, String) {
    let mut s = library(dir);
    write_png(&dir.path("archive/journey/phone/p.png"), 7);
    s.execute("library.import", &json!({"paths": [dir.path("archive/journey/phone")]})).unwrap();
    s.execute("folder.label", &json!({"path": dir.path("archive/journey/phone"), "label": "red"})).unwrap();
    std::fs::rename(dir.0.join("trip/a.png"), dir.0.join("archive/journey/a.png")).unwrap();
    std::fs::rename(dir.0.join("trip/day1"), dir.0.join("archive/journey/day1")).unwrap();
    std::fs::remove_dir(dir.0.join("trip")).unwrap();
    (s, dir.path("archive/journey"))
}

#[test]
fn folders_of_the_library_already_at_the_new_place_keep_their_labels() {
    let dir = Scratch::new("labels-there");
    let (mut s, now) = moved_into_a_folder_of_the_library(&dir);
    s.execute("folder.label", &json!({"path": dir.path("trip/day1"), "label": "blue"})).unwrap();

    let r = relocate(&mut s, &dir.path("trip"), &now);

    assert_eq!(r["relinked"], json!(2), "{r}");
    assert_eq!(s.catalog.folder_color_label(&dir.path("archive/journey/phone")), Some(ColorLabel::Red), "it was there all along");
    assert_eq!(s.catalog.folder_color_label(&dir.path("archive/journey/day1")), Some(ColorLabel::Blue), "and the located one's came along");
    s.undo_step().unwrap();
    assert_eq!(s.catalog.folder_color_label(&dir.path("archive/journey/phone")), Some(ColorLabel::Red));
    assert_eq!(s.catalog.folder_color_label(&dir.path("trip/day1")), Some(ColorLabel::Blue));
}

#[test]
fn undo_leaves_a_view_that_did_not_move_where_it_is() {
    let dir = Scratch::new("view-stays");
    let (mut s, now) = moved_into_a_folder_of_the_library(&dir);
    relocate(&mut s, &dir.path("trip"), &now);
    // a folder of the library that was at the new place all along, chosen since
    let phone = dir.path("archive/journey/phone");
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": phone})).unwrap();
    s.undo_step().unwrap();
    assert_eq!(s.library_folder.as_deref(), Some(phone.as_str()), "its photos never moved");
    s.redo_step().unwrap();
    assert_eq!(s.library_folder.as_deref(), Some(phone.as_str()));

    // and a folder browsed in Local is a place on disk
    s.execute("library.browse", &json!({"path": dir.path("archive/journey/day1")})).unwrap();
    let browsed = s.browse.as_ref().map(|b| b.path.clone());
    assert!(browsed.is_some());
    s.undo_step().unwrap();
    assert_eq!(s.browse.as_ref().map(|b| b.path.clone()), browsed, "the folder on disk is where it was");
}

#[test]
fn a_folder_whose_files_went_into_a_folder_inside_it_is_located_there() {
    let dir = Scratch::new("inside");
    let mut s = library(&dir);
    // everything in `trip` was put into `trip/2019`
    std::fs::create_dir_all(dir.0.join("2019")).unwrap();
    std::fs::rename(dir.0.join("trip/a.png"), dir.0.join("2019/a.png")).unwrap();
    std::fs::rename(dir.0.join("trip/day1"), dir.0.join("2019/day1")).unwrap();
    std::fs::rename(dir.0.join("2019"), dir.0.join("trip/2019")).unwrap();

    let r = relocate(&mut s, &dir.path("trip"), &dir.path("trip/2019"));

    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64()), (Some(2), Some(2)), "{r}");
    assert_eq!(files(&s), [dir.path("trip/2019/a.png"), dir.path("trip/2019/day1/b.png")]);
    s.undo_step().unwrap();
    assert_eq!(files(&s), [dir.path("trip/a.png"), dir.path("trip/day1/b.png")]);
}

// ── Is the file there the photo's? (review of #1122) ─────────────────────────────────────────
//
// * A file being at the new place does not make it the photo's. A place that is, as the library
//   has it, another photo's file of the same folder (the new place is inside the folder, or the
//   folder inside it) is taken only when the file's content is the photo's; "anyway" does not
//   cover it, and neither does a library that recorded no sizes or contents.
// * A file of another size than the library recorded is not counted as found: it is said, and
//   taken only on request. A folder where no file is the library's is not the folder.

/// A library holding `a/x.png` and `a/b/x.png`: the same name one level apart.
fn same_name_one_level_apart(dir: &Scratch) -> Session {
    write_png(&dir.path("a/x.png"), 1);
    write_png(&dir.path("a/b/x.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("a")]})).unwrap();
    assert_eq!(s.catalog.len(), 2);
    s
}

/// As a catalog from another app leaves its photos: no size and no content recorded.
fn forget_sizes_and_contents(s: &mut Session) {
    let ops: Vec<Op> = s
        .catalog
        .photos()
        // (a record of having looked in Local stays as browsing left it: untouched)
        .filter(|p| !p.local)
        .map(|p| Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: 0,
            content_hash: None,
            preview_only: None,
            preview_checked: String::new(),
        })
        .collect();
    s.commit("Forget", Op::Batch { ops }).unwrap();
}

#[test]
fn a_place_inside_the_folder_never_gives_a_photo_another_photos_file() {
    let dir = Scratch::new("nested");
    let mut s = same_name_one_level_apart(&dir);
    // one level too low: `a/b/x.png` is the other photo's file, and nothing moved
    for incomplete in [false, true] {
        let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": incomplete}));
        assert!(e.contains("other photos of this folder") && e.contains(dir.path("a/b/x.png").as_str()), "incomplete {incomplete}: {e}");
    }
}

#[test]
fn nor_when_the_library_recorded_no_size_and_no_content() {
    let dir = Scratch::new("nested-blind");
    let mut s = same_name_one_level_apart(&dir);
    forget_sizes_and_contents(&mut s);
    for incomplete in [false, true] {
        let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": incomplete}));
        assert!(e.contains("other photos of this folder"), "incomplete {incomplete}: {e}");
    }
}

#[test]
fn nor_a_photo_in_recently_deleted() {
    let dir = Scratch::new("nested-deleted");
    let mut s = same_name_one_level_apart(&dir);
    write_png(&dir.path("a/k.png"), 3);
    s.execute("library.import", &json!({"paths": [dir.path("a/k.png")]})).unwrap();
    let upper = s.catalog.photos().find(|p| matches!(&p.source, Source::File { path } if *path == dir.path("a/x.png"))).map(|p| p.id).unwrap();
    s.execute("photo.delete", &json!({"ids": [upper.0]})).unwrap();
    // `k.png` is in `a/b` too, so one of the library's two files is found there
    std::fs::copy(dir.path("a/k.png"), dir.path("a/b/k.png")).unwrap();

    let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": true}));
    assert!(e.contains("other photos of this folder"), "restoring it would show the other photo's file: {e}");
}

#[test]
fn files_that_did_go_one_level_down_are_told_by_their_content() {
    let dir = Scratch::new("shifted");
    let mut s = same_name_one_level_apart(&dir);
    let upper = s.catalog.photos().find(|p| matches!(&p.source, Source::File { path } if *path == dir.path("a/x.png"))).map(|p| p.id).unwrap();
    // everything in `a` was put into `a/b`: `a/b/x.png` went to `a/b/b/x.png`, `a/x.png` to `a/b/x.png`
    std::fs::create_dir_all(dir.0.join("a/b/b")).unwrap();
    std::fs::rename(dir.0.join("a/b/x.png"), dir.0.join("a/b/b/x.png")).unwrap();
    std::fs::rename(dir.0.join("a/x.png"), dir.0.join("a/b/x.png")).unwrap();

    let r = relocate(&mut s, &dir.path("a"), &dir.path("a/b"));

    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64(), r["notFound"].as_u64()), (Some(2), Some(2), Some(0)), "{r}");
    assert!(matches!(&s.catalog.photo(upper).unwrap().source, Source::File { path } if *path == dir.path("a/b/x.png")));
    assert!(s.execute("library.missing", &json!({})).unwrap().as_array().unwrap().is_empty());

    // without contents recorded nothing tells the two apart: refused rather than guessed
    s.undo_step().unwrap();
    forget_sizes_and_contents(&mut s);
    let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b")}));
    assert!(e.contains("other photos of this folder"), "{e}");
}

/// Another picture under the same name (not the size the library recorded).
fn write_other(path: &str) {
    let (w, h) = (40usize, 30usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i * 7) as u8, (i * 3) as u8, (i / 5) as u8, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    assert_ne!(bytes.len() as u64, std::fs::metadata(path).unwrap().len(), "the test needs another size");
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_file_of_another_size_is_not_counted_as_found() {
    let dir = Scratch::new("other-size");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    write_other(&dir.path("archive/journey/a.png"));

    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": now}));
    assert!(
        e.contains("1 of the 2 files") && e.contains("1 file there is not the one the library recorded") && e.contains("incomplete: true"),
        "{e}"
    );

    // on request it is taken: the file was changed where it lies, as files are
    let r = update_location(&mut s, &json!({"folder": dir.path("trip"), "path": now, "incomplete": true})).unwrap();
    assert_eq!(
        (r["relinked"].as_u64(), r["found"].as_u64(), r["different"].as_u64(), r["notFound"].as_u64()),
        (Some(2), Some(1), Some(1), Some(0)),
        "{r}"
    );
}

#[test]
fn a_folder_where_no_file_is_the_librarys_is_not_the_folder() {
    let dir = Scratch::new("other-disk");
    let mut s = library(&dir);
    let now = moved_away(&dir);
    // the same names in the same folders, other pictures: another disk laid out alike
    write_other(&dir.path("archive/journey/a.png"));
    write_other(&dir.path("archive/journey/day1/b.png"));

    for incomplete in [false, true] {
        let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": now, "incomplete": incomplete}));
        assert!(
            e.contains("none of the 2 files") && e.contains("2 files there are not the ones the library recorded"),
            "incomplete {incomplete}: {e}"
        );
    }
}

#[test]
fn a_file_whose_size_the_library_never_recorded_is_taken_and_said() {
    let dir = Scratch::new("no-size");
    let mut s = library(&dir);
    forget_sizes_and_contents(&mut s);
    let now = moved_away(&dir);
    write_other(&dir.path("archive/journey/a.png"));

    let r = relocate(&mut s, &dir.path("trip"), &now);

    assert_eq!((r["found"].as_u64(), r["unverified"].as_u64(), r["different"].as_u64()), (Some(2), Some(2), Some(0)), "{r}");
}

// ── The same place under another spelling (second review) ────────────────────────────────────
//
// * A place inside the folder is the same place however it is written: with `..`, in other
//   letter case on a disk that doesn't tell case apart, or through a link.
// * The folder inside the chosen one is the same mistake the other way round.
// * A file only browsed in Local at the new place is not "another photo": its record gives way.
// * Looking that was stopped found nothing out.

fn at(s: &Session, path: &str) -> PhotoId {
    s.catalog.photos().find(|p| matches!(&p.source, Source::File { path: q } if q == path)).map(|p| p.id).unwrap()
}

#[test]
#[cfg(unix)]
fn a_place_written_with_dot_dot_is_refused() {
    let dir = Scratch::new("dotdot");
    let mut s = same_name_one_level_apart(&dir);
    let to = format!("{}{sep}b{sep}..{sep}b", dir.path("a"), sep = std::path::MAIN_SEPARATOR);
    let e = refused(&mut s, json!({"folder": dir.path("a"), "path": to, "incomplete": true}));
    assert!(e.contains("`..`"), "{e}");
}

#[test]
fn a_place_inside_the_folder_in_other_letter_case_is_the_same_place() {
    let dir = Scratch::new("case");
    write_png(&dir.path("a/x.png"), 1);
    write_png(&dir.path("a/B/x.png"), 2);
    if !Path::new(&dir.path("a/b/x.png")).is_file() {
        return; // this disk tells `B` from `b`: `a/b` is no folder here
    }
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("a")]})).unwrap();
    for blind in [false, true] {
        if blind {
            forget_sizes_and_contents(&mut s);
        }
        let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": true}));
        assert!(e.contains("other photos of this folder"), "blind {blind}: {e}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_to_a_place_inside_the_folder_is_the_same_place() {
    let dir = Scratch::new("link");
    let mut s = same_name_one_level_apart(&dir);
    std::os::unix::fs::symlink(dir.path("a/b"), dir.path("link")).unwrap();
    for blind in [false, true] {
        if blind {
            forget_sizes_and_contents(&mut s);
        }
        let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("link"), "incomplete": true}));
        assert!(e.contains("other photos of this folder"), "blind {blind}: {e}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_to_the_folder_itself_gives_no_photo_another_photos_file() {
    let dir = Scratch::new("link-self");
    let mut s = library(&dir);
    forget_sizes_and_contents(&mut s);
    std::os::unix::fs::symlink(dir.path("trip"), dir.path("link")).unwrap();
    // every file there is the photo's own, under another name
    let r = relocate(&mut s, &dir.path("trip"), &dir.path("link"));
    assert_eq!((r["relinked"].as_u64(), r["found"].as_u64()), (Some(2), Some(2)), "{r}");
}

#[test]
fn the_folder_inside_the_chosen_one_is_the_same_mistake() {
    let dir = Scratch::new("too-high");
    write_png(&dir.path("a/b/x.png"), 1);
    write_png(&dir.path("a/b/b/x.png"), 2);
    write_png(&dir.path("a/b/k.png"), 3);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("a/b")]})).unwrap();
    std::fs::copy(dir.path("a/b/k.png"), dir.path("a/k.png")).unwrap();
    forget_sizes_and_contents(&mut s);
    // one level too high: `a/b/x.png` would be taken for the photo of `a/b/b/x.png`
    let e = refused(&mut s, json!({"folder": dir.path("a/b"), "path": dir.path("a"), "incomplete": true}));
    assert!(e.contains("other photos of this folder"), "{e}");
}

#[test]
fn a_file_only_browsed_in_local_at_a_place_inside_the_folder_gives_way() {
    let dir = Scratch::new("nested-local");
    write_png(&dir.path("a/x.png"), 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("a")]})).unwrap();
    let photo = at(&s, &dir.path("a/x.png"));
    // the file went one level down, and the folder it is in now was looked at in Local
    std::fs::create_dir_all(dir.0.join("a/b")).unwrap();
    std::fs::rename(dir.0.join("a/x.png"), dir.0.join("a/b/x.png")).unwrap();
    s.execute("library.browse", &json!({"path": dir.path("a/b")})).unwrap();
    assert_eq!(s.catalog.len(), 2, "the library's photo and the record of having looked");

    for blind in [false, true] {
        if blind {
            s.undo_step().unwrap();
            forget_sizes_and_contents(&mut s);
        }
        let r = relocate(&mut s, &dir.path("a"), &dir.path("a/b"));
        assert_eq!((r["relinked"].as_u64(), r["found"].as_u64(), r["replacedLocal"].as_u64()), (Some(1), Some(1), Some(1)), "blind {blind}: {r}");
        assert_eq!(files(&s), [dir.path("a/b/x.png")], "one record for the one file");
        assert!(s.catalog.photo(photo).is_some_and(|p| p.in_library()));
    }
}

#[test]
fn a_claimed_file_of_another_size_than_the_photos_is_not_read_to_be_refused() {
    let dir = Scratch::new("claimed-size");
    let mut s = same_name_one_level_apart(&dir);
    write_other(&dir.path("a/b/x.png"));
    let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": true}));
    assert!(e.contains("other photos of this folder"), "{e}");
}

#[test]
fn looking_that_was_stopped_found_nothing_out() {
    let dir = Scratch::new("stopped");
    let s = library(&dir);
    let now = moved_away(&dir);
    let stopped = crate::import::ScanProgress::default();
    stopped.cancel.store(true, Ordering::Relaxed);
    let e = Relocation::new(&s, &dir.path("trip"), &now, false).unwrap().look(&stopped).unwrap_err();
    assert!(e.contains("stopped"), "{e}");
}

#[test]
fn a_files_content_read_in_pieces_is_the_content_import_recorded() {
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let pieces = crate::cmd::missing::hash_reader(&bytes[..]).unwrap();
    assert_eq!(pieces, lightcraft_preview::hash_bytes(&bytes).to_string());
}

// ── Third review: files shared below the two folders, and a browsed file that is another picture ─

#[cfg(unix)]
#[test]
fn a_link_inside_the_folder_to_the_chosen_place_is_seen() {
    let dir = Scratch::new("link-below-from");
    write_png(&dir.path("big/2023/x.png"), 2);
    write_png(&dir.path("pics/x.png"), 1);
    std::os::unix::fs::symlink(dir.path("big/2023"), dir.path("pics/2023")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("pics/x.png"), dir.path("pics/2023/x.png")]})).unwrap();
    assert_eq!(s.catalog.len(), 2);
    for blind in [false, true] {
        if blind {
            forget_sizes_and_contents(&mut s);
        }
        // `big/2023/x.png` is the file of the photo at `pics/2023/x.png`
        let e = refused(&mut s, json!({"folder": dir.path("pics"), "path": dir.path("big/2023"), "incomplete": true}));
        assert!(e.contains("other photos of this folder"), "blind {blind}: {e}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_inside_the_chosen_place_back_to_the_folder_is_seen() {
    let dir = Scratch::new("link-below-to");
    let mut s = Session::new().with_fs();
    write_png(&dir.path("a/x.png"), 1);
    write_png(&dir.path("a/sub/x.png"), 2);
    s.execute("library.import", &json!({"paths": [dir.path("a")]})).unwrap();
    std::fs::create_dir_all(dir.0.join("new")).unwrap();
    std::fs::copy(dir.path("a/x.png"), dir.path("new/x.png")).unwrap();
    std::os::unix::fs::symlink(dir.path("a"), dir.path("new/sub")).unwrap();
    for blind in [false, true] {
        if blind {
            forget_sizes_and_contents(&mut s);
        }
        // `new/sub/x.png` is `a/x.png`: the other photo's file
        let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("new"), "incomplete": true}));
        assert!(e.contains("other photos of this folder"), "blind {blind}: {e}");
    }
}

#[test]
fn a_browsed_file_inside_the_folder_that_is_another_picture_is_not_taken_for_the_photo() {
    let dir = Scratch::new("nested-local-other");
    write_png(&dir.path("a/x.png"), 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("a")]})).unwrap();
    // nothing moved: one level down lies another picture of the same name (and size), looked at in Local
    write_png(&dir.path("a/b/x.png"), 2);
    s.execute("library.browse", &json!({"path": dir.path("a/b")})).unwrap();

    let e = refused(&mut s, json!({"folder": dir.path("a"), "path": dir.path("a/b"), "incomplete": true}));
    assert!(e.contains("another picture than the library recorded"), "{e}");
}

#[test]
#[cfg(unix)]
fn a_relative_place_with_dot_dot_is_told_to_be_written_in_full() {
    let dir = Scratch::new("dotdot-relative");
    // (Windows resolves `..` itself when it makes the path absolute: unix only, like the other `..` test)
    let mut s = library(&dir);
    let e = refused(&mut s, json!({"folder": dir.path("trip"), "path": ".."}));
    assert!(e.contains("full path"), "{e}");
}
