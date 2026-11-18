//! Lightroom history steps and snapshots read from a catalog file: stored as text (Lightroom 6) or as compressed BLOBs
//! (history steps since Lightroom Classic 13, which also converts an upgraded catalog's steps). They become the
//! photo's History and Versions, and are kept in the import archive.

use crate::Session;
use crate::lightroom_fixture::{self as fixture, Field, Table};
use lcc_catalog::{Photo, PhotoId};
use serde_json::{Value, json};

const CURRENT: &str = "s = { Exposure2012 = 1.25, Contrast2012 = 17 }";
const FIRST: &str = "s = { Exposure2012 = 1.25 }";

/// Write a one-photo catalog with `extra` tables, read it, archive it, and return the archive's JSON.
fn archive(tag: &str, extra: Vec<Table>) -> Value {
    let dir = fixture::dir(tag);
    let mut tables = fixture::one_photo(&dir.join("originals"), CURRENT);
    tables.extend(extra);
    let catalog = dir.join("test.lrcat");
    fixture::write(&catalog, &tables);
    let data = crate::lightroom_catalog::read(&catalog).unwrap();
    let stored = crate::lightroom_archive::store(&dir.join("Interop"), &data).unwrap();
    let archived = fixture::read_archive(&stored.path);
    let _ = std::fs::remove_dir_all(dir);
    archived
}

fn steps(settings: impl Fn(&str) -> Field) -> Table {
    Table {
        name: "Adobe_libraryImageDevelopHistoryStep",
        columns: fixture::HISTORY_COLUMNS,
        rows: vec![fixture::history_step(3, 1, 20.0, "Contrast", settings(CURRENT)), fixture::history_step(2, 1, 10.0, "Exposure", settings(FIRST))],
    }
}

/// `(name, dateCreated, text)` of each archived row, in the order the archive holds them.
fn rows(archive: &Value, key: &str) -> Vec<(Value, Value, Value)> {
    let rows = archive["photos"][0][key].as_array().unwrap_or_else(|| panic!("the archive should list {key}: {archive}"));
    rows.iter().map(|r| (r["name"].clone(), r["dateCreated"].clone(), r["text"].clone())).collect()
}

#[test]
fn compressed_history_steps_keep_their_settings_in_the_archive() {
    let archived = archive("history-blob", vec![steps(|t| Field::Blob(fixture::compressed(t)))]);
    let mut steps = rows(&archived, "history");
    steps.sort_by(|a, b| a.1.as_f64().partial_cmp(&b.1.as_f64()).unwrap());
    assert_eq!(
        steps,
        [(json!("Exposure"), json!(10.0), json!(FIRST)), (json!("Contrast"), json!(20.0), json!(CURRENT))],
        "each compressed step should be archived with its decoded settings: {archived}"
    );
    assert_eq!(archived["warnings"], json!([]), "no step should be reported unreadable");
}

#[test]
fn text_history_steps_are_archived_as_before() {
    let archived = archive("history-text", vec![steps(fixture::text)]);
    let mut steps = rows(&archived, "history");
    steps.sort_by(|a, b| a.1.as_f64().partial_cmp(&b.1.as_f64()).unwrap());
    assert_eq!(steps, [(json!("Exposure"), json!(10.0), json!(FIRST)), (json!("Contrast"), json!(20.0), json!(CURRENT))], "{archived}");
    assert_eq!(archived["warnings"], json!([]));
}

#[test]
fn compressed_and_text_snapshots_keep_their_settings_in_the_archive() {
    let snapshots = Table {
        name: "Adobe_libraryImageDevelopSnapshot",
        columns: fixture::SNAPSHOT_COLUMNS,
        rows: vec![
            fixture::snapshot(5, 1, "Bright", Field::Blob(fixture::compressed("s = { Exposure2012 = 2 }"))),
            fixture::snapshot(6, 1, "Plain", fixture::text("s = { Contrast2012 = 30 }")),
        ],
    };
    let archived = archive("snapshot-blob", vec![snapshots]);
    let mut kept: Vec<_> = rows(&archived, "snapshots").into_iter().map(|(name, _, text)| (name, text)).collect();
    kept.sort_by_key(|(name, _)| name.to_string());
    assert_eq!(
        kept,
        [(json!("Bright"), json!("s = { Exposure2012 = 2 }")), (json!("Plain"), json!("s = { Contrast2012 = 30 }"))],
        "compressed and text snapshots should both be archived with their settings: {archived}"
    );
    assert_eq!(archived["warnings"], json!([]));
}

/// A hostile or damaged catalog: each bad BLOB costs only its own row's settings, with a warning naming the image.
#[test]
fn a_malformed_settings_blob_is_archived_without_its_settings_and_warned_about() {
    let mut mismatch = fixture::compressed(FIRST);
    mismatch[3] = mismatch[3].wrapping_add(1);
    let mut over_cap = fixture::compressed(FIRST);
    over_cap[..4].copy_from_slice(&((16u32 << 20) + 1).to_be_bytes());
    let bad = [
        ("Short", vec![0, 0]),
        ("Not zlib", vec![0, 0, 0, 5, 1, 2, 3, 4, 5]),
        ("Wrong length", mismatch),
        ("Too big", over_cap),
        ("Not UTF-8", fixture::compressed_bytes(&[0xff, 0xfe])),
    ];
    let mut history = steps(|t| Field::Blob(fixture::compressed(t)));
    for (i, (name, bytes)) in bad.iter().enumerate() {
        let id = 10 + i64::try_from(i).unwrap();
        history.rows.push(fixture::history_step(id, 1, 30.0 + id as f64, name, Field::Blob(bytes.clone())));
    }
    let snapshots = Table {
        name: "Adobe_libraryImageDevelopSnapshot",
        columns: fixture::SNAPSHOT_COLUMNS,
        rows: vec![fixture::snapshot(5, 1, "Broken", Field::Blob(vec![0, 0, 0, 9, 9]))],
    };
    let archived = archive("history-bad", vec![history, snapshots]);
    assert_eq!(archived["photos"].as_array().map(Vec::len), Some(1), "the photo is still read: {archived}");
    let steps = rows(&archived, "history");
    assert_eq!(steps.len(), 7, "every step is still archived: {archived}");
    for (name, _, text) in &steps {
        let good = name == "Exposure" || name == "Contrast";
        assert_eq!(text.is_string(), good, "{name}: only the good steps keep settings: {archived}");
    }
    assert_eq!(rows(&archived, "snapshots")[0].2, Value::Null, "the broken snapshot is archived without settings");
    let warnings: Vec<_> = archived["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(warnings.len(), bad.len() + 1, "one warning per bad step and snapshot: {warnings:?}");
    assert!(warnings.iter().all(|w| w.starts_with("image 1: ")), "each warning names the image: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("history step 10")), "a warning names the step: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("snapshot 5")), "a warning names the snapshot: {warnings:?}");
}

/// Only the settings column is decoded; any other BLOB (even one in Lightroom's compressed encoding) stays out of
/// the archive, as before.
#[test]
fn other_blob_columns_never_reach_the_archive() {
    let mut history = steps(|t| Field::Blob(fixture::compressed(t)));
    for row in &mut history.rows {
        row[3] = Field::Blob(vec![1, 2, 3, 4]); // digest
        row[10] = Field::Blob(fixture::compressed("s = { Exposure2012 = 3 }")); // valueString
    }
    let archived = archive("history-other-blobs", vec![history]);
    for step in archived["photos"][0]["history"].as_array().unwrap() {
        assert!(step.get("digest").is_none() && step.get("valueString").is_none(), "other BLOB columns leaked: {step}");
        assert!(step["text"].is_string(), "the settings are still decoded: {step}");
    }
}

/// Write a one-photo catalog with `extra` tables and import it into a new session.
fn import(tag: &str, extra: Vec<Table>) -> (Session, Value) {
    let dir = fixture::dir(tag);
    let mut tables = fixture::one_photo(&dir.join("originals"), CURRENT);
    tables.extend(extra);
    let catalog = dir.join("test.lrcat");
    fixture::write(&catalog, &tables);
    let mut s = Session::new();
    let report = crate::lightroom_catalog::import(&mut s, &catalog, false).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    (s, report)
}

fn photo(s: &Session, report: &Value) -> std::sync::Arc<Photo> {
    let id = report["mapping"]["1"].as_u64().unwrap_or_else(|| panic!("image 1 should be imported: {report}"));
    s.catalog.photo(PhotoId(id)).unwrap().clone()
}

fn warnings(report: &Value) -> Vec<String> {
    report["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).map(String::from).collect()
}

#[test]
fn compressed_history_steps_import_as_history_in_order() {
    let (s, report) = import("history-blob-import", vec![steps(|t| Field::Blob(fixture::compressed(t)))]);
    let p = photo(&s, &report);
    let labels: Vec<_> = p.history.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["Exposure", "Contrast"], "each compressed step should become a History entry, oldest first: {report}");
    assert_eq!(p.history[0].settings.light.exposure, 1.25);
    assert_eq!(p.history[0].settings.light.contrast, 0.0, "the first step is before the contrast change");
    assert_eq!(warnings(&report), Vec::<String>::new(), "no step should be reported unreadable");
}

#[test]
fn text_history_steps_still_import_as_history_in_order() {
    let (s, report) = import("history-text-import", vec![steps(fixture::text)]);
    let p = photo(&s, &report);
    let labels: Vec<_> = p.history.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["Exposure", "Contrast"], "{report}");
    assert_eq!(p.history[0].settings.light.contrast, 0.0);
    assert_eq!(warnings(&report), Vec::<String>::new());
}

#[test]
fn compressed_and_text_snapshots_import_as_versions() {
    let snapshots = Table {
        name: "Adobe_libraryImageDevelopSnapshot",
        columns: fixture::SNAPSHOT_COLUMNS,
        rows: vec![
            fixture::snapshot(5, 1, "Bright", Field::Blob(fixture::compressed("s = { Exposure2012 = 2 }"))),
            fixture::snapshot(6, 1, "Plain", fixture::text("s = { Contrast2012 = 30 }")),
        ],
    };
    let (s, report) = import("snapshot-blob-import", vec![snapshots]);
    let p = photo(&s, &report);
    let names: Vec<_> = p.versions.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Bright", "Plain"], "compressed and text snapshots should both become versions: {report}");
    assert_eq!(p.versions[0].settings.light.exposure, 2.0);
    assert_eq!(p.versions[1].settings.light.contrast, 30.0);
    assert_eq!(warnings(&report), Vec::<String>::new());
}

/// A damaged step or snapshot is left out of History or Versions, with warnings naming the image and the step; the
/// good steps and the rest of the import still come in.
#[test]
fn a_malformed_settings_blob_is_left_out_of_history_with_a_warning() {
    let mut history = steps(|t| Field::Blob(fixture::compressed(t)));
    history.rows.push(fixture::history_step(10, 1, 15.0, "Broken", Field::Blob(vec![0, 0, 0, 5, 1, 2, 3, 4, 5])));
    let snapshots = Table {
        name: "Adobe_libraryImageDevelopSnapshot",
        columns: fixture::SNAPSHOT_COLUMNS,
        rows: vec![
            fixture::snapshot(5, 1, "Broken", Field::Blob(vec![0, 0, 0, 9, 9])),
            fixture::snapshot(6, 1, "Plain", fixture::text("s = { Contrast2012 = 30 }")),
        ],
    };
    let (s, report) = import("history-bad-import", vec![history, snapshots]);
    let p = photo(&s, &report);
    let labels: Vec<_> = p.history.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["Exposure", "Contrast"], "the good steps still import: {report}");
    let names: Vec<_> = p.versions.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Plain"], "the good snapshot still imports: {report}");
    let warnings = warnings(&report);
    assert!(warnings.iter().any(|w| w.starts_with("image 1: history step 10: ")), "the reason names the step: {warnings:?}");
    assert!(warnings.iter().any(|w| w.starts_with("image 1: snapshot 5: ")), "and the snapshot: {warnings:?}");
    assert!(warnings.iter().any(|w| w == "image 1: 1 Lightroom history step(s) could not be read; left out of History"), "{warnings:?}");
    assert!(warnings.iter().all(|w| !w.contains("kept in the archive")), "nothing claims an unreadable step was kept: {warnings:?}");
}
