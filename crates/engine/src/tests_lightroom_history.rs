//! Lightroom history steps and snapshots read from a catalog file into the import archive: stored as text
//! (Lightroom 6) or as compressed BLOBs (history steps since Lightroom Classic 13, which also converts an upgraded
//! catalog's steps).

use crate::lightroom_fixture::{self as fixture, Field, Table};
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
    let archived = fixture::read_archive(&stored);
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
