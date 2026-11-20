//! Reading focus points for photos already in the library (`photo.readFocusPoints`, `focus.pump`): header-only reads,
//! batches committed without undo steps, "looked, found nothing" remembered across sessions, and the activity row.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::focus_scan::{read_focus, record_contains, record_len};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-focus-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sony_arw(mode: u8, samples: usize) -> Vec<u8> {
    lightcraft_meta::focus::synthetic_arw(mode, samples)
}

/// A photo of `camera` at `path` (an import made before focus points were read: none stored).
fn photo(id: u64, path: &Path, camera: &str) -> Photo {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let mut p = Photo::new(PhotoId(id), Source::File { path: path.to_string_lossy().into_owned() }, &name, "ARW", 8640, 5760, "2026-10-13T00:00:00");
    p.meta.camera = camera.into();
    p
}

fn add(s: &mut Session, photos: Vec<Photo>) {
    s.execute_fn("test.add", |s| {
        for p in photos {
            s.apply_system(Op::AddPhoto { photo: Box::new(p) })?;
        }
        Ok(Value::Null)
    })
    .unwrap();
}

fn focus(s: &Session, id: u64) -> Option<lightcraft_meta::FocusPoint> {
    s.catalog.photo(PhotoId(id)).unwrap().meta.focus.clone()
}

fn pump_until_idle(s: &mut Session) -> Value {
    let started = Instant::now();
    loop {
        let v = s.execute("focus.pump", &json!({"pace": "normal"})).unwrap();
        if v["active"] == false {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the job never ended: {v}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_header_read_reads_a_small_part_of_the_file() {
    let dir = temp_dir("header");
    let path = dir.join("a.ARW");
    let file = sony_arw(3, 4_000_000);
    std::fs::write(&path, &file).unwrap();
    let r = read_focus(&path.to_string_lossy()).unwrap();
    assert!(r.focus.is_some());
    assert_eq!(r.focus, lightcraft_meta::extract(&file).focus, "the same point as a whole-file read");
    assert!(r.bytes_read <= 32 * 1024 && r.bytes_read * 100 < file.len() as u64, "read {} of {}", r.bytes_read, file.len());
    assert!(read_focus(&dir.join("gone.ARW").to_string_lossy()).is_err());
}

/// Fills the missing points; leaves other cameras, other formats and known points alone without opening them; records
/// "looked, found nothing" (manual focus, garbage) but not a file it could not read; makes no undo step.
#[test]
fn fills_missing_points_and_remembers_photos_without_one() {
    let dir = temp_dir("fill");
    let af = dir.join("af.ARW");
    std::fs::write(&af, sony_arw(3, 100_000)).unwrap();
    let manual = dir.join("manual.ARW");
    std::fs::write(&manual, sony_arw(0, 100_000)).unwrap();
    let garbage = dir.join("garbage.ARW");
    std::fs::write(&garbage, vec![0x5a; 70_000]).unwrap();
    let heif = dir.join("x.HIF");
    std::fs::write(&heif, sony_arw(3, 1000)).unwrap();
    let known = lightcraft_meta::FocusPoint {
        rect: lightcraft_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.2, y1: 0.2 },
        mode: Some("AF-S".into()),
        area: None,
        tracking: None,
    };
    let mut with_point = photo(4, &af, "SONY ILCE-1");
    with_point.meta.focus = Some(known.clone());
    let mut s = Session::new();
    add(
        &mut s,
        vec![
            photo(1, &af, "SONY ILCE-1"),
            photo(2, &manual, "SONY ILCE-1"),
            // another body's photo is not opened, even when its file would have a point
            photo(3, &af, "Canon EOS R5"),
            with_point,
            photo(5, &dir.join("offline.ARW"), "SONY ILCE-7SM3"),
            photo(6, &garbage, "SONY ILCE-1"),
            photo(7, &heif, "SONY ILCE-1"),
        ],
    );
    let undo = s.undo.len();
    let r = s.execute("photo.readFocusPoints", &json!({"wait": true})).unwrap();
    assert_eq!(r["queued"], 4, "{r}");
    let last = &r["last"];
    assert_eq!(
        (last["done"].as_u64(), last["found"].as_u64(), last["none"].as_u64(), last["unavailable"].as_u64()),
        (Some(4), Some(1), Some(2), Some(1)),
        "{r}"
    );
    assert!(last["bytesRead"].as_u64().unwrap() <= 3 * 32 * 1024, "{r}");
    assert_eq!(focus(&s, 1), lightcraft_meta::extract(&std::fs::read(&af).unwrap()).focus);
    assert!(focus(&s, 1).is_some());
    assert_eq!(focus(&s, 1).unwrap().words(), vec!["AF-C", "Wide", "Animal Eye Tracking"]);
    assert_eq!(focus(&s, 2), None);
    assert_eq!(focus(&s, 3), None);
    assert_eq!(focus(&s, 4), Some(known), "a stored point is never replaced");
    assert_eq!(focus(&s, 5), None);
    assert_eq!(focus(&s, 7), None);
    assert!(record_contains(&s, PhotoId(2)) && record_contains(&s, PhotoId(6)));
    assert!(!record_contains(&s, PhotoId(5)), "an unreadable file is tried again later");
    assert_eq!(record_len(&s), 2);
    assert_eq!(s.undo.len(), undo, "no undo step");
    // asked again: only the offline photo is left
    assert_eq!(s.execute("photo.readFocusPoints", &json!({"wait": true})).unwrap()["queued"], 1);
    // named photos are looked at again, even those recorded as having none
    assert_eq!(s.execute("photo.readFocusPoints", &json!({"ids": [2, 3, 4], "wait": true})).unwrap()["queued"], 1);
}

/// What was read survives a restart: the next session reads nothing again, and a photo whose file was offline is read
/// once it is back. The automatic start (once per library opening, from the pump) picks it up.
#[test]
fn resumes_after_a_restart_without_reading_again() {
    let dir = temp_dir("resume");
    let lib = dir.join("library");
    let af = dir.join("af.ARW");
    std::fs::write(&af, sony_arw(3, 50_000)).unwrap();
    let manual = dir.join("manual.ARW");
    std::fs::write(&manual, sony_arw(0, 50_000)).unwrap();
    let later = dir.join("later.ARW");
    {
        let mut s = Session::new();
        s.open_library(&lib, false).unwrap();
        add(&mut s, vec![photo(1, &af, "SONY ILCE-1"), photo(2, &manual, "SONY ILCE-1"), photo(3, &later, "SONY ILCE-1")]);
        // the pump starts it by itself
        let first = s.execute("focus.pump", &json!({"pace": "normal"})).unwrap();
        assert_eq!(first["active"], true, "{first}");
        let v = pump_until_idle(&mut s);
        assert_eq!(v["last"]["found"], 1, "{v}");
        assert!(lib.join(crate::focus_scan::RECORD_FILE).exists());
    }
    let mut s = Session::new();
    s.open_library(&lib, false).unwrap();
    assert!(focus(&s, 1).is_some(), "the point was saved with the library");
    // the file that was offline is back: only it is read
    std::fs::write(&later, sony_arw(3, 50_000)).unwrap();
    let v = s.execute("focus.pump", &json!({"pace": "normal"})).unwrap();
    assert!(record_contains(&s, PhotoId(2)), "the record was read back");
    assert_eq!(v["running"]["total"], 1, "{v}");
    let v = pump_until_idle(&mut s);
    assert_eq!((v["last"]["total"].as_u64(), v["last"]["found"].as_u64()), (Some(1), Some(1)), "{v}");
    assert!(focus(&s, 3).is_some());
    assert!(record_contains(&s, PhotoId(2)));
    // and it doesn't start again in this opening
    assert_eq!(s.execute("focus.pump", &json!({})).unwrap()["active"], false);
}

/// The job has a row in the activity stack (n of N, cancellable); paused, it reads nothing new; cancelled (the row's ✕),
/// it commits what it read and the row goes away.
#[test]
fn the_activity_row_progresses_pauses_and_cancels() {
    let dir = temp_dir("row");
    let mut photos = Vec::new();
    for i in 0..40u64 {
        let p = dir.join(format!("{i:03}.ARW"));
        std::fs::write(&p, sony_arw(3, 20_000)).unwrap();
        photos.push(photo(i + 1, &p, "SONY ILCE-7SM3"));
    }
    let mut s = Session::new();
    add(&mut s, photos);
    // the pump starts the job by itself, at the pace it is given: paused, while the user drags
    let v = s.execute("focus.pump", &json!({"pace": "pause"})).unwrap();
    assert_eq!(v["active"], true);
    let rows = s.activity.list();
    let row = rows.iter().find(|t| t.kind == "focus").expect("an activity row");
    assert_eq!((row.label.as_str(), row.total, row.cancellable), ("Reading focus points", 40, true));
    // already running: asking again starts nothing new
    let r = s.execute("photo.readFocusPoints", &json!({})).unwrap();
    assert_eq!((r["queued"].as_u64(), r["running"].as_bool()), (Some(0), Some(true)));
    std::thread::sleep(Duration::from_millis(300));
    let still = s.execute("focus.pump", &json!({"pace": "pause"})).unwrap()["running"]["done"].as_u64().unwrap();
    assert_eq!(still, 0, "paused: nothing read");
    // a few more at a light pace, then cancel
    let started = Instant::now();
    let mut done = still;
    while done < still + 3 && started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
        done = s.execute("focus.pump", &json!({"pace": "light"})).unwrap()["running"]["done"].as_u64().unwrap_or(40);
    }
    let row = s.activity.list().into_iter().find(|t| t.kind == "focus").expect("still running");
    assert!(row.done >= 3, "progress shows: {}", row.done);
    s.execute("focus.pump", &json!({"pace": "pause"})).unwrap();
    s.activity.cancel(row.id).unwrap();
    let v = s.execute("focus.pump", &json!({"pace": "pause"})).unwrap();
    assert_eq!(v["active"], false, "{v}");
    assert_eq!(v["last"]["cancelled"], true, "{v}");
    assert!(s.activity.list().iter().all(|t| t.kind != "focus"), "the row is gone");
    let found = s.catalog.photos().filter(|p| p.meta.focus.is_some()).count() as u64;
    assert_eq!(found, v["last"]["found"].as_u64().unwrap(), "what was read before the cancel is kept");
    assert!(found < 40);
    // the rest are read next time
    let r = s.execute("photo.readFocusPoints", &json!({"wait": true})).unwrap();
    assert_eq!(r["queued"].as_u64().unwrap(), 40 - found);
    assert_eq!(s.catalog.photos().filter(|p| p.meta.focus.is_some()).count(), 40);
}

/// Photos deleted, moved or given a point while the job runs are left alone; a library closed mid-run takes the job
/// with it (nothing lands in the next library), and nothing panics.
#[test]
fn changes_under_a_running_job_are_respected() {
    let dir = temp_dir("changes");
    let mut photos = Vec::new();
    for i in 0..6u64 {
        let p = dir.join(format!("{i}.ARW"));
        std::fs::write(&p, sony_arw(3, 10_000)).unwrap();
        photos.push(photo(i + 1, &p, "SONY ILCE-1"));
    }
    let mut s = Session::new();
    s.open_library(dir.join("lib-a"), false).unwrap();
    add(&mut s, photos);
    // started by the pump, paused
    assert_eq!(s.execute("focus.pump", &json!({"pace": "pause"})).unwrap()["running"]["total"], 6);
    // meanwhile: photo 1 removed, photo 2 relinked elsewhere, photo 3 given a point by the user's Reload, photo 4's file
    // replaced by garbage, photo 5's deleted
    let mine =
        lightcraft_meta::FocusPoint { rect: lightcraft_geom::Rect { x0: 0.5, y0: 0.5, x1: 0.6, y1: 0.6 }, mode: None, area: None, tracking: None };
    s.execute_fn("test.change", |s| {
        s.apply_system(Op::RemovePhoto { id: PhotoId(1) })?;
        let moved = dir.join("moved.ARW");
        std::fs::copy(dir.join("1.ARW"), &moved).map_err(|e| crate::EngineError::Other(e.to_string()))?;
        s.apply_system(Op::Relink {
            id: PhotoId(2),
            file_name: "moved.ARW".into(),
            source: Source::File { path: moved.to_string_lossy().into() },
            format: None,
        })?;
        let mut meta = s.catalog.photo(PhotoId(3)).unwrap().meta.clone();
        meta.focus = Some(mine.clone());
        s.apply_system(Op::SetMeta { id: PhotoId(3), meta: Box::new(meta) })?;
        Ok(Value::Null)
    })
    .unwrap();
    std::fs::write(dir.join("3.ARW"), b"not a raw any more").unwrap();
    std::fs::remove_file(dir.join("4.ARW")).unwrap();
    let v = pump_until_idle(&mut s);
    assert!(s.catalog.photo(PhotoId(1)).is_none());
    assert_eq!(focus(&s, 2), None, "read at the old path: left for another run");
    assert_eq!(focus(&s, 3), Some(mine));
    assert_eq!(focus(&s, 5), None);
    assert!(focus(&s, 6).is_some());
    assert!(v["last"]["done"].as_u64().unwrap() <= 6, "{v}");
    // a library closed mid-run: the job goes with it
    s.execute("focus.pump", &json!({"pace": "pause"})).unwrap();
    assert_eq!(s.execute("photo.readFocusPoints", &json!({"ids": [2, 4, 5]})).unwrap()["queued"], 3);
    s.open_library(dir.join("lib-b"), false).unwrap();
    let v = s.execute("focus.pump", &json!({"pace": "normal"})).unwrap();
    assert_eq!(v["active"], false, "{v}");
    assert!(s.activity.list().iter().all(|t| t.kind != "focus"));
    assert_eq!(s.catalog.photos().count(), 0);
}

/// Opt-in measurement on real files: `LIGHTCRAFT_FOCUS_FILES=<a text file listing raw paths, one per line>` reads each
/// whole and by header, asserts they give the same focus point, and prints the bytes the header read took. Skipped
/// without it; no user file is part of the repository.
#[test]
fn real_files_header_read_matches_whole_file() {
    let Ok(list) = std::env::var("LIGHTCRAFT_FOCUS_FILES") else { return };
    let paths: Vec<String> = std::fs::read_to_string(list).unwrap().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    let (mut n, mut with_point, mut bytes, mut max) = (0u64, 0u64, 0u64, 0u64);
    for p in &paths {
        let whole = lightcraft_meta::extract(&std::fs::read(p).unwrap()).focus;
        let r = read_focus(p).unwrap();
        assert_eq!(r.focus, whole, "{p}");
        n += 1;
        with_point += u64::from(whole.is_some());
        bytes += r.bytes_read;
        max = max.max(r.bytes_read);
    }
    eprintln!(
        "focus header reads: {n} files ({with_point} with a point) agree with whole-file reads; {} bytes each on average, {max} at most",
        bytes / n.max(1)
    );
}
