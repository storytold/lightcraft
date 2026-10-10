//! Regression contract for the union of two catalog lineages that independently used v3/v4.
//! Procedural MemStore records cover sparse Local v3/v4 diagnostics and upstream v3-v6
//! operations. Function-bearing Local v4 records are unsupported until the later curve replay.

use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::*;
use serde_json::{Value, json};

const SNAP_SEQ: u64 = 10;

fn sparse(marker: u32, middle: f64) -> Value {
    json!({"version":marker,"curve":{"master":[{"x":0.0,"y":0.0},{"x":0.4,"y":middle},{"x":1.0,"y":1.0}]}})
}

fn lens(amount: f64) -> Value {
    json!({"warp":null,"vignette":{"k":[amount,0.0,0.0,0.0,0.0],"center":{"x":0.5,"y":0.5},"radius":1.0}})
}

fn keyword(synonym: &str) -> Value {
    json!({"synonyms":[synonym],"includeOnExport":true,"exportContaining":false,"exportSynonyms":true,"person":false})
}

fn base() -> Value {
    // Spell the legacy wire directly; do not generate fixtures with today's serializer, which
    // could silently add the very new fields/markers this test is meant to detect losing.
    json!({"photos":{"1":{"id":1,"source":{"type":"file","path":"/pictures/procedural.jpg"},
        "file_name":"procedural.jpg","format":"JPEG","width":4,"height":3,
        "imported":"2026-01-01","develop":sparse(1,0.25)}},
        "albums":{"1":{"id":1,"name":"Procedural","photos":[1]}},"next_photo":2,"next_album":2})
}

fn record(seq: u64, op: &Value) -> String {
    let body = op.to_string();
    format!("{{\"seq\":{seq},\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes()))
}

fn files(version: u32, snapshot: &Value, ops: &[Value]) -> MemStore {
    let store = MemStore::new();
    store.set(SNAPSHOT, json!({"format":"lightcraft-catalog","version":version,"seq":SNAP_SEQ,"catalog":snapshot}).to_string().into_bytes());
    let log: String = ops.iter().enumerate().map(|(i, op)| record(SNAP_SEQ + i as u64 + 1, op)).collect();
    store.set(LOG, log.into_bytes());
    store
}

fn saved(store: &MemStore) -> Value {
    serde_json::from_slice(&store.get(SNAPSHOT).unwrap()).unwrap()
}

/// Every explicitly authored field must survive; today's default fields may be added.
fn assert_projection(actual: &Value, expected: &Value) {
    if let Value::Object(fields) = expected {
        for (key, value) in fields {
            assert_projection(actual.get(key).unwrap_or_else(|| panic!("lost field {key}")), value);
        }
    } else {
        assert_eq!(actual, expected);
    }
}

fn fixture(local: bool, version: u32) -> (Value, Vec<Value>, Value) {
    let mut snapshot = base();
    if local {
        snapshot["photos"]["1"]["unsupported_edits"] = json!(["Look"]);
    } else {
        snapshot["albums"]["1"]["order"] = json!(2);
        if version >= 4 {
            snapshot["photos"]["1"]["embedded_lens"] = lens(0.02);
        }
        if version >= 5 {
            snapshot["keyword_list"] = json!({"travel|sea":{"path":"Travel|Sea","info":keyword("water")}});
        }
        if version >= 6 {
            snapshot["folder_records"] = json!({"/pictures":{"label":"red"}});
        }
        if version == 7 {
            // The intermediate rebased writer has upstream v3–6 plus local diagnostics, but
            // function curves do not exist until the final v8 checkpoint.
            snapshot["photos"]["1"]["unsupported_edits"] = json!(["Look"]);
        }
    }
    let mut expected = snapshot.clone();
    let mut ops = Vec::new();
    // Supported sparse schema markers must survive without normalizing historical wire data.
    let settings = sparse(0, 0.3);
    ops.push(json!({"op":"setDevelop","id":1,"settings":settings,"label":"Legacy edit","edited":"2026-01-02"}));
    expected["photos"]["1"]["develop"] = settings;
    expected["photos"]["1"]["edited"] = json!("2026-01-02");
    if local {
        ops.push(json!({"op":"setUnsupportedEdits","id":1,"fields":["FilterList","Look"]}));
        expected["photos"]["1"]["unsupported_edits"] = json!(["FilterList", "Look"]);
    } else {
        ops.push(json!({"op":"setAlbumOrder","id":1,"order":7}));
        expected["albums"]["1"]["order"] = json!(7);
        if version >= 4 {
            ops.push(json!({"op":"setEmbeddedLens","id":1,"lens":lens(0.04)}));
            expected["photos"]["1"]["embedded_lens"] = lens(0.04);
        }
        if version >= 5 {
            ops.push(json!({"op":"setKeyword","path":"Travel|Sea","info":keyword("ocean")}));
            expected["keyword_list"]["travel|sea"]["info"] = keyword("ocean");
        }
        if version >= 6 {
            ops.push(json!({"op":"setFolderRecord","folder":"/pictures","record":{"label":"blue"}}));
            expected["folder_records"]["/pictures"]["label"] = json!("blue");
        }
        if version == 7 {
            ops.push(json!({"op":"setUnsupportedEdits","id":1,"fields":["FilterList","Look"]}));
            expected["photos"]["1"]["unsupported_edits"] = json!(["FilterList", "Look"]);
        }
    }
    (snapshot, ops, expected)
}

fn union_op() -> Value {
    let mut settings = sparse(1, 0.2);
    settings["hdr"] = json!({"enabled":true,"max_ev":4.0,"sdr_brightness":17.0});
    settings["profile"] = json!({"camera_local_tone":true});
    settings["process"] = json!(1);
    json!({"op":"batch","ops":[
        {"op":"setDevelop","id":1,"settings":settings,"label":"Combined edit","edited":"2026-01-03"},
        {"op":"setUnsupportedEdits","id":1,"fields":["CameraProfile","Look"]},
        {"op":"setAlbumOrder","id":1,"order":11},
        {"op":"setEmbeddedLens","id":1,"lens":lens(0.06)},
        {"op":"setKeyword","path":"Travel|Sea","info":keyword("coast")},
        {"op":"setFolderRecord","folder":"/pictures","record":{"label":"green"}}
    ]})
}

fn assert_union(catalog: &Catalog) {
    let value: Value = serde_json::from_str(&catalog.to_snapshot()).unwrap();
    assert_projection(&value["photos"]["1"]["develop"], &sparse(1, 0.2));
    assert_eq!(value["photos"]["1"]["develop"]["hdr"]["enabled"], true);
    assert_eq!(value["photos"]["1"]["develop"]["hdr"]["max_ev"], 4.0);
    assert_eq!(value["photos"]["1"]["develop"]["hdr"]["sdr_brightness"], 17.0);
    assert_eq!(value["photos"]["1"]["develop"]["profile"]["camera_local_tone"], true);
    assert!(value["photos"]["1"]["develop"].get("process").is_none(), "explicit processV1 still uses legacy omission");
    assert_eq!(value["photos"]["1"]["unsupported_edits"], json!(["CameraProfile", "Look"]));
    assert_eq!(value["albums"]["1"]["order"], 11);
    assert_projection(&value["photos"]["1"]["embedded_lens"], &lens(0.06));
    assert_projection(&value["keyword_list"]["travel|sea"]["info"], &keyword("coast"));
    assert_eq!(value["folder_records"]["/pictures"]["label"], "green");
}

#[test]
fn both_catalog_lineages_upgrade_before_a_combined_op_is_appended() {
    for (local, version) in [(true, 3), (true, 4), (false, 3), (false, 4), (false, 5), (false, 6), (false, 7)] {
        let (snapshot, ops, expected) = fixture(local, version);
        let store = files(version, &snapshot, &ops);
        let (mut journal, mut catalog, report) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!((report.replayed, report.upgraded_from), (ops.len(), (version < VERSION).then_some(version)), "local={local},version={version}");
        let header = saved(&store);
        assert_eq!(header["version"], VERSION, "upgrade is durable before the caller can append");
        if version < VERSION {
            assert_eq!(header["seq"], SNAP_SEQ + ops.len() as u64);
            assert!(store.get(LOG).unwrap().is_empty());
            assert_projection(&header["catalog"], &expected);
        } else {
            assert_projection(&serde_json::from_str(&catalog.to_snapshot()).unwrap(), &expected);
        }
        assert!(header["catalog"]["photos"]["1"]["develop"].get("process").is_none(), "a missing legacy process stays omitted");
        let op: Op = serde_json::from_value(union_op()).unwrap();
        catalog.apply(op.clone()).unwrap();
        journal.append(&[op]).unwrap();
        assert_eq!(saved(&store)["version"], VERSION);
        let line = String::from_utf8(store.get(LOG).unwrap()).unwrap();
        let written: Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
        assert_eq!(written["seq"], SNAP_SEQ + ops.len() as u64 + 1);
        assert_eq!(written["op"]["ops"][0]["settings"]["version"], 1);
        assert!(written["op"]["ops"][0]["settings"].get("process").is_none());
        assert_union(&catalog);
        drop(journal);
        let (_, reopened, report) = Journal::open(Box::new(store)).unwrap();
        assert_eq!((report.replayed, report.upgraded_from), (if version < VERSION { 1 } else { ops.len() + 1 }, None));
        assert_eq!(reopened.to_snapshot(), catalog.to_snapshot());
        assert_union(&reopened);
    }
}

#[test]
fn both_lineages_keep_authored_snapshot_fields_even_without_log_records() {
    for (local, version) in [(true, 3), (true, 4), (false, 3), (false, 4), (false, 5), (false, 6), (false, 7)] {
        let (snapshot, _, _) = fixture(local, version);
        let store = files(version, &snapshot, &[]);
        let (_, catalog, report) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!((report.replayed, report.upgraded_from), (0, (version < VERSION).then_some(version)));
        assert_projection(&serde_json::from_str(&catalog.to_snapshot()).unwrap(), &snapshot);
        assert_projection(&saved(&store)["catalog"], &snapshot);
    }
}

fn assert_refused_unchanged(store: &MemStore, must_report_newer: bool) {
    store.set("unrelated-marker", b"preserve me too".to_vec());
    let before = store.files.lock().unwrap().clone();
    let error = Journal::open(Box::new(store.clone())).err().expect("must refuse unsupported wire");
    if must_report_newer {
        assert!(matches!(error, CatalogError::Newer(_)), "{error}");
    }
    assert_eq!(*store.files.lock().unwrap(), before, "no truncate, snapshot, rename or deletion");
}

#[test]
fn future_catalog_keeps_all_snapshot_and_log_bytes_untouched() {
    let (snapshot, ops, _) = fixture(true, 4);
    assert_refused_unchanged(&files(VERSION + 1, &snapshot, &ops), true);
    // The catalog payload need not be understood before a future outer version is refused.
    assert_refused_unchanged(&files(VERSION + 1, &json!({"photos":"future storage"}), &[union_op()]), true);
}

#[test]
fn diagnostics_operation_preserves_local_baseline_and_targets_photo() {
    let mut catalog = Catalog::new();
    let mut photo = Photo::new(PhotoId(1), Source::File { path: "/pictures/local.jpg".into() }, "local.jpg", "JPEG", 4, 3, "2026-01-01");
    photo.local = true;
    photo.set_local_baseline();
    let baseline = photo.local_baseline;
    let wire = serde_json::to_string(&photo).unwrap();
    assert!(!wire.contains("unsupported_edits"), "empty diagnostics must preserve old fingerprints");
    let legacy: Photo = serde_json::from_str(&wire).unwrap();
    assert!(legacy.untouched_local());
    catalog.apply(Op::AddPhoto { photo: Box::new(legacy) }).unwrap();
    let original = catalog.to_snapshot();
    let op = Op::SetUnsupportedEdits { id: PhotoId(1), fields: vec!["FilterList".into()] };
    let mut ids = Vec::new();
    op.photo_ids(&mut |id| ids.push(id));
    assert_eq!(ids, vec![PhotoId(1)], "diagnostics participate in undo/in-use protection");
    let inverse = catalog.apply(op).unwrap();
    let changed = catalog.photo(PhotoId(1)).unwrap();
    assert_eq!(changed.local_baseline, baseline);
    assert!(!changed.untouched_local(), "a diagnostic change is part of the saved state");
    catalog.apply(inverse).unwrap();
    assert_eq!(catalog.to_snapshot(), original);
    assert!(catalog.photo(PhotoId(1)).unwrap().untouched_local());
}

#[test]
fn unsupported_curve_payload_in_legacy_envelope_is_refused_without_rewrite() {
    let mut settings = sparse(1, 0.25);
    settings["curve"]["master"] = json!({"kind":"futureCurve", "version":99, "payload":[1,2,3]});
    for version in [3, 4, VERSION] {
        let mut snapshot = base();
        snapshot["photos"]["1"]["develop"] = settings.clone();
        assert_refused_unchanged(&files(version, &snapshot, &[]), false);
        let op = json!({"op":"setDevelop","id":1,"settings":settings,"label":"Future curve","edited":null});
        assert_refused_unchanged(&files(version, &base(), &[op]), true);
    }
}
