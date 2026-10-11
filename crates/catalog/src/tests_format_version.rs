//! Issue #102: catalog format versions. Older formats load and are upgraded; a newer format is
//! refused with a clear error and left untouched (never read in part, truncated or set aside).

use crate::journal::{LOG, SNAPSHOT, VERSION, encode_record};
use crate::*;

fn photo(c: &mut Catalog) -> Op {
    let id = c.alloc_photo_id();
    Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "p.jpg", "JPEG", 4, 3, "2026-01-01")) }
}

/// A catalog with two photos and the log line for a third op on top of it.
fn legacy_parts() -> (Catalog, String, Catalog) {
    let mut c = Catalog::new();
    for _ in 0..2 {
        let op = photo(&mut c);
        c.apply(op).unwrap();
    }
    let base = c.clone();
    let id = c.photos().next().unwrap().id;
    let op = Op::SetRating { id, rating: 4 };
    c.apply(op.clone()).unwrap();
    (base, format!("{}\n", encode_record(3, &op)), c)
}

fn snapshot_version(m: &MemStore) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    v["version"].as_u64().unwrap()
}

#[test]
fn v1_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":1,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (mut j, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(1)));
    // rewritten in this format before anything is appended
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert!(m.get(LOG).unwrap().is_empty());
    assert_eq!(j.seq(), 3);
    j.append(&[Op::SetLabelName { label: ColorLabel::ALL[0], name: Some("x".into()) }]).unwrap();
    drop(j);
    let (_, _, r) = Journal::open(Box::new(m)).unwrap();
    assert_eq!((r.replayed, r.upgraded_from), (1, None));
}

/// A library saved before albums could be ordered by hand opens as it was and is rewritten in the current format.
#[test]
fn v2_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":2,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(2)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A library saved before keywords could be listed on their own opens as it was and is rewritten
/// in the current format.
#[test]
fn v3_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":3,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(3)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A v6 library (before preview-only photos recorded which version decided it, #662) opens with
/// an empty stamp on its preview-only photos, so they are rechecked once, and is rewritten in the
/// current format.
#[test]
fn v6_preview_only_photo_loads_unstamped_and_is_upgraded() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/p/a.cr3".into() }, "a.cr3", "CR3", 160, 120, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.preview_only = Some("Cr3 files are not decoded yet".into());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let snap = c.to_snapshot();
    assert!(!snap.contains("preview_checked"), "an empty stamp isn't written");
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":6,\"seq\":1,\"catalog\":{snap}}}\n").into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    let p = loaded.photo(id).unwrap();
    assert_eq!(p.preview_only.as_deref(), Some("Cr3 files are not decoded yet"));
    assert_eq!(p.preview_checked, "");
    assert_eq!(r.upgraded_from, Some(6));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

#[test]
fn v2_library_upgrades_before_recording_unsupported_edits() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":2,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (mut journal, mut loaded, report) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(loaded.to_snapshot(), full.to_snapshot());
    assert_eq!(report.upgraded_from, Some(2));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    let id = loaded.photos().next().unwrap().id;
    let op = Op::SetUnsupportedEdits { id, fields: vec!["FilterList".into()] };
    loaded.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    drop(journal);
    let (_, reopened, report) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(reopened.to_snapshot(), loaded.to_snapshot());
    assert_eq!((report.replayed, report.upgraded_from), (1, None));
}

fn function_settings() -> serde_json::Value {
    serde_json::json!({"curve":{"master":{"kind":"function","version":1,"base":{"pieces":[{"end":1.0,"hermite":[0.1,0.9,0.8,0.8]}]},"adjustments":[{"x":0.0,"y":0.0},{"x":0.4,"y":0.03},{"x":1.0,"y":0.0}]}}})
}

#[test]
fn v3_library_upgrades_before_recording_function_curves_and_reopens_exactly() {
    use std::sync::Arc;
    let mut base = Catalog::new();
    let op = photo(&mut base);
    base.apply(op).unwrap();
    let id = base.photos().next().unwrap().id;
    let legacy =
        lightcraft_develop::DevelopSettings::from_json(&serde_json::json!({"version":1,"curve":{"red":[{"x":0.0,"y":0.1},{"x":1.0,"y":0.9}]}}))
            .unwrap();
    base.apply(Op::SetDevelop { id, settings: Arc::new(legacy), label: "Sparse".into(), edited: None }).unwrap();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":3,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, Vec::new());
    let (mut journal, mut loaded, report) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(report.upgraded_from, Some(3));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert_eq!(loaded.to_snapshot(), base.to_snapshot(), "legacy sparse edits survive the upgrade");
    let mut value = function_settings();
    value["profile"] = serde_json::json!({"id":"lc.imported","imported":{"look_table":"E96CB8AF6484F8402BB0E6ECFC8AD0FC"}});
    let settings = Arc::new(lightcraft_develop::DevelopSettings::from_json(&value).unwrap());
    assert_eq!(settings.required_schema_version(), lightcraft_develop::LOOK_TABLE_SCHEMA_VERSION);
    let op = Op::SetDevelop { id, settings: settings.clone(), label: "Continuous".into(), edited: None };
    loaded.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    drop(journal);
    let (_, reopened, report) = Journal::open(Box::new(m)).unwrap();
    assert_eq!((report.replayed, report.upgraded_from), (1, None));
    assert_eq!(reopened.photo(id).unwrap().develop, settings);
    assert_eq!(reopened.to_snapshot(), loaded.to_snapshot());
}

#[test]
fn future_function_versions_in_snapshot_and_crc_valid_log_are_refused_untouched() {
    use std::sync::Arc;
    let mut catalog = Catalog::new();
    let op = photo(&mut catalog);
    catalog.apply(op).unwrap();
    let id = catalog.photos().next().unwrap().id;
    let settings = Arc::new(lightcraft_develop::DevelopSettings::from_json(&function_settings()).unwrap());
    let op = Op::SetDevelop { id, settings, label: "Continuous".into(), edited: None };
    let mut future_op = serde_json::to_value(&op).unwrap();
    future_op["settings"]["curve"]["master"]["version"] = serde_json::json!(2);
    let body = future_op.to_string();
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":1,\"catalog\":{}}}\n", catalog.to_snapshot()).into_bytes(),
    );
    m.set(LOG, format!("{{\"seq\":2,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).into_bytes());
    assert_refused_untouched(&m);

    catalog.apply(op).unwrap();
    // Restrict this replacement to the function's exact serialized discriminator/version pair.
    let snapshot = catalog.to_snapshot().replace("\"kind\":\"function\",\"version\":1", "\"kind\":\"function\",\"version\":2");
    assert_ne!(snapshot, catalog.to_snapshot());
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{snapshot}}}\n").into_bytes());
    m.set(LOG, Vec::new());
    let before = m.files.lock().unwrap().clone();
    assert!(Journal::open(Box::new(m.clone())).is_err());
    assert_eq!(*m.files.lock().unwrap(), before);
}

#[test]
fn versionless_log_only_library_is_upgraded() {
    let mut c = Catalog::new();
    let op = photo(&mut c);
    c.apply(op.clone()).unwrap();
    let m = MemStore::new();
    m.set(LOG, format!("{}\n", encode_record(1, &op)).into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(loaded.to_snapshot(), c.to_snapshot());
    assert_eq!(r.upgraded_from, Some(0));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A new library gets a snapshot in this format at once, so an older build never sees a bare log
/// it would read in part.
#[test]
fn new_library_is_written_in_this_format() {
    let m = MemStore::new();
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.created);
    assert_eq!(r.upgraded_from, None);
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

fn assert_refused_untouched(m: &MemStore) {
    let before = m.files.lock().unwrap().clone();
    let e = Journal::open(Box::new(m.clone())).err().expect("refused");
    assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
    let msg = e.to_string();
    assert!(msg.contains("newer version of LightCraft") && msg.contains("left unchanged"), "{msg}");
    assert_eq!(*m.files.lock().unwrap(), before, "nothing modified, nothing renamed");
}

#[test]
fn newer_snapshot_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},\"seq\":2,\"catalog\":{}}}\n", VERSION + 1, base.to_snapshot()).into_bytes(),
    );
    m.set(LOG, log.into_bytes());
    assert_refused_untouched(&m);
    // even when its catalog doesn't parse as this version's
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},\"seq\":2,\"catalog\":{{\"photos\":7}}}}\n", VERSION + 1).into_bytes(),
    );
    assert_refused_untouched(&m);
    let e = Journal::open(Box::new(m)).err().unwrap().to_string();
    assert!(e.contains(&format!("v{}", VERSION + 1)), "{e}");
}

#[test]
fn future_develop_schema_in_snapshots_and_valid_log_records_is_refused_untouched() {
    use lightcraft_develop::DevelopSettings;
    use std::sync::Arc;
    let future = Arc::new(DevelopSettings { version: lightcraft_develop::SCHEMA_VERSION + 1, ..Default::default() });
    for field in ["develop", "import", "version", "history"] {
        let mut c = Catalog::new();
        let mut p = Photo::new(c.alloc_photo_id(), Source::File { path: "/own.png".into() }, "own.png", "PNG", 2, 2, "today");
        match field {
            "develop" => p.develop = future.clone(),
            "import" => p.import_look = Some(future.clone()),
            "version" => p.versions.push(Version { name: "Saved".into(), created: "today".into(), settings: future.clone(), auto: false }),
            _ => p.history.push(HistoryStep { label: "Saved".into(), settings: future.clone() }),
        }
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let m = MemStore::new();
        m.set(
            SNAPSHOT,
            format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":1,\"catalog\":{}}}\n", c.to_snapshot()).into_bytes(),
        );
        m.set(LOG, Vec::new());
        let before = m.files.lock().unwrap().clone();
        assert!(Journal::open(Box::new(m.clone())).is_err(), "future settings in {field} were accepted");
        assert_eq!(*m.files.lock().unwrap(), before, "{field} modified the stored catalog");
    }
    let (base, log, _) = legacy_parts();
    let id = base.photos().next().unwrap().id;
    let future_op = Op::SetDevelop { id, settings: future, label: "Future edit".into(), edited: None };
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes(),
    );
    m.set(LOG, format!("{log}{}\n", encode_record(4, &future_op)).into_bytes());
    assert_refused_untouched(&m);
}

/// A log record with a valid CRC whose op this version doesn't know: refused, not taken for a
/// torn tail (truncated) or damage (renamed) — wherever it is in the log.
#[test]
fn unknown_op_in_the_log_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    let future = |seq: u64| {
        let body = r#"{"op":"fromTheFuture","id":1,"what":"something new"}"#;
        format!("{{\"seq\":{seq},\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes()))
    };
    let snap = format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot());
    let id = base.photos().next().unwrap().id;
    let after = format!("{}\n", encode_record(5, &Op::SetRating { id, rating: 1 }));
    for log in [format!("{log}{}", future(4)), format!("{log}{}{after}", future(4)), format!("{}{log}", future(3))] {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap.clone().into_bytes());
        m.set(LOG, log.into_bytes());
        assert_refused_untouched(&m);
    }
    // a known op with a field of an unknown shape is "newer" too
    let body = r#"{"op":"setRating","id":1,"rating":"five stars"}"#;
    let m = MemStore::new();
    m.set(SNAPSHOT, snap.into_bytes());
    m.set(LOG, format!("{log}{{\"seq\":4,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).into_bytes());
    assert_refused_untouched(&m);
}

/// A CRC mismatch is still damage / a torn tail (not "newer").
#[test]
fn bad_crc_is_still_a_torn_tail() {
    let (base, log, _) = legacy_parts();
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes(),
    );
    let body = r#"{"op":"fromTheFuture"}"#;
    m.set(LOG, format!("{log}{{\"seq\":4,\"crc\":1,\"op\":{body}}}\n").into_bytes());
    let (_, _, r) = Journal::open(Box::new(m)).unwrap();
    assert!(r.torn_bytes > 0 && r.damaged.is_none(), "{r:?}");
}

/// Guard for the version rule (see `journal` → *Format versions*): adding an [`Op`] variant breaks
/// this match. When it does, bump `journal::VERSION`, add a row to the format table, and add the
/// variant here under the new version. A version that only adds serialized fields raises
/// `NEWEST_FIELDS` instead.
#[test]
fn op_variants_are_versioned() {
    fn since(op: &Op) -> u32 {
        match op {
            Op::SetDevelop { settings, .. } if settings.profile.imported.as_ref().is_some_and(|look| look.look_table.is_some()) => 12,
            Op::SetDevelop { settings, .. } if settings.profile.imported.as_ref().is_some_and(|look| look.convert_to_grayscale) => 11,
            Op::SetDevelop { settings, .. } if settings.profile.imported.is_some() => 10,
            Op::SetDevelop { settings, .. }
                if [&settings.curve.master, &settings.curve.red, &settings.curve.green, &settings.curve.blue].iter().any(|c| c.is_function()) =>
            {
                9
            }
            Op::AddPhoto { .. }
            | Op::RemovePhoto { .. }
            | Op::SetRating { .. }
            | Op::SetFlag { .. }
            | Op::SetLabel { .. }
            | Op::SetMeta { .. }
            | Op::SetDeleted { .. }
            | Op::SetLocal { .. }
            | Op::SetVersions { .. }
            | Op::SetHistory { .. }
            | Op::PushHistory { .. }
            | Op::AddAlbum { .. }
            | Op::RemoveAlbum { .. }
            | Op::RenameAlbum { .. }
            | Op::MoveAlbum { .. }
            | Op::SetAlbumPhotos { .. }
            | Op::SetAlbumCover { .. }
            | Op::SetAlbumRules { .. }
            | Op::AddStack { .. }
            | Op::RemoveStack { .. }
            | Op::SetStack { .. }
            | Op::SetCaptured { .. }
            | Op::SetAnalysis { .. }
            | Op::SetFile { .. }
            | Op::Relink { .. }
            | Op::SetContent { .. }
            | Op::SetLabelName { .. }
            | Op::Batch { .. } => 1,
            Op::SetBrowsed { .. } => 2,
            Op::SetAlbumOrder { .. } => 3,
            Op::SetEmbeddedLens { .. } => 4,
            Op::SetKeyword { .. } => 5,
            Op::SetFolderRecord { .. } => 6,
            Op::SetAsShotWb { .. } => 7,
            Op::SetHistoryCursor { .. } => 8,
            Op::SetUnsupportedEdits { .. } => 9,
            Op::SetDevelop { .. } => 1,
        }
    }
    assert_eq!(since(&Op::SetUnsupportedEdits { id: PhotoId(1), fields: Vec::new() }), 8);
    let function = since(&Op::SetDevelop {
        id: PhotoId(1),
        settings: Arc::new(lightcraft_develop::DevelopSettings::from_json(&function_settings()).unwrap()),
        label: "function".into(),
        edited: None,
    });
    assert_eq!(function, 9);
    for (convert_to_grayscale, table, version) in [(false, None, 10), (true, None, 11), (true, Some("E96CB8AF6484F8402BB0E6ECFC8AD0FC"), VERSION)] {
        let required = since(&Op::SetDevelop {
            id: PhotoId(1),
            settings: Arc::new(
                lightcraft_develop::DevelopSettings::from_json(&serde_json::json!({
                    "profile": {"id": "lc.imported", "imported": {
                        "name": "Photo look", "contrast": 8.0, "convert_to_grayscale": convert_to_grayscale, "look_table": table
                    }}
                }))
                .unwrap(),
            ),
            label: "imported profile".into(),
            edited: None,
        });
        assert_eq!(required, version, "the newest op payload's version must be the current format version");
    }
}

#[path = "tests_format_diagnostics.rs"]
mod diagnostics_lineage;
