//! Regression contract for the union of two catalog lineages that independently used v3/v4.
//! All files are procedural MemStore records. Requires the combined writer at v8, local
//! SetUnsupportedEdits/Photo diagnostics and schema2 function curves, plus upstream v3–6 ops.

use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::*;
use serde_json::{Value, json};

const SNAP_SEQ: u64 = 10;

fn sparse(marker: u32, middle: f64) -> Value {
    json!({"version":marker,"curve":{"master":[{"x":0.0,"y":0.0},{"x":0.4,"y":middle},{"x":1.0,"y":1.0}]}})
}

fn function(lift: f64) -> Value {
    json!({"version":2,"curve":{"master":{"kind":"function","version":1,
        "base":{"pieces":[{"end":1.0,"hermite":[lift,0.9,0.7,0.7]}]},
        "adjustments":[{"x":0.0,"y":0.0},{"x":0.4,"y":0.03},{"x":1.0,"y":0.0}]}}})
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
        if version == 4 {
            snapshot["photos"]["1"]["develop"] = function(0.1);
        }
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
    // Marker0 was accepted historically. A read/upgrade must keep the explicit supported
    // legacy marker; the schema2 function path is asserted independently on the local-v4 row.
    let settings = if local && version == 4 { function(0.15) } else { sparse(0, 0.3) };
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
    let mut settings = function(0.2);
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
    assert_projection(&value["photos"]["1"]["develop"], &function(0.2));
    assert!(value["photos"]["1"]["develop"].get("process").is_none(), "explicit processV1 still uses legacy omission");
    assert_eq!(value["photos"]["1"]["unsupported_edits"], json!(["CameraProfile", "Look"]));
    assert_eq!(value["albums"]["1"]["order"], 11);
    assert_projection(&value["photos"]["1"]["embedded_lens"], &lens(0.06));
    assert_projection(&value["keyword_list"]["travel|sea"]["info"], &keyword("coast"));
    assert_eq!(value["folder_records"]["/pictures"]["label"], "green");
}

#[test]
fn both_catalog_lineages_upgrade_before_a_combined_op_is_appended() {
    assert_eq!(VERSION, 8, "diagnostics use combined7; function curves need the next writer version");
    for (local, version) in [(true, 3), (true, 4), (false, 3), (false, 4), (false, 5), (false, 6), (false, 7)] {
        let (snapshot, ops, expected) = fixture(local, version);
        let store = files(version, &snapshot, &ops);
        let (mut journal, mut catalog, report) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!((report.replayed, report.upgraded_from), (ops.len(), Some(version)), "local={local},version={version}");
        let header = saved(&store);
        assert_eq!(header["version"], 8, "upgrade is durable before the caller can append");
        assert_eq!(header["seq"], SNAP_SEQ + ops.len() as u64);
        assert!(store.get(LOG).unwrap().is_empty());
        assert_projection(&header["catalog"], &expected);
        assert!(header["catalog"]["photos"]["1"]["develop"].get("process").is_none(), "a missing legacy process stays omitted");
        let op: Op = serde_json::from_value(union_op()).unwrap();
        catalog.apply(op.clone()).unwrap();
        journal.append(&[op]).unwrap();
        assert_eq!(saved(&store)["version"], 8);
        let line = String::from_utf8(store.get(LOG).unwrap()).unwrap();
        let written: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(written["seq"], SNAP_SEQ + ops.len() as u64 + 1);
        assert_eq!(written["op"]["ops"][0]["settings"]["version"], 2);
        assert!(written["op"]["ops"][0]["settings"].get("process").is_none());
        assert_union(&catalog);
        drop(journal);
        let (_, reopened, report) = Journal::open(Box::new(store)).unwrap();
        assert_eq!((report.replayed, report.upgraded_from), (1, None));
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
        assert_eq!((report.replayed, report.upgraded_from), (0, Some(version)));
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
fn future_catalog9_keeps_all_snapshot_and_log_bytes_untouched() {
    assert_eq!(VERSION, 8);
    let (snapshot, ops, _) = fixture(true, 4);
    assert_refused_unchanged(&files(9, &snapshot, &ops), true);
    // The catalog payload need not be understood before a future outer version is refused.
    assert_refused_unchanged(&files(9, &json!({"photos":"future storage"}), &[union_op()]), true);
}

#[test]
fn future_native_curve_and_schema_markers_are_refused_in_snapshots_and_valid_logs() {
    for invalid in [
        {
            let mut v = function(0.1);
            v["version"] = json!(3);
            v
        },
        {
            let mut v = function(0.1);
            v["curve"]["master"]["version"] = json!(2);
            v
        },
    ] {
        let mut snapshot = base();
        snapshot["photos"]["1"]["develop"] = invalid.clone();
        // A supported outer version with an unreadable inner payload is a snapshot
        // parse error today; both error classes must leave every stored byte intact.
        assert_refused_unchanged(&files(8, &snapshot, &[]), false);
        let op = json!({"op":"setDevelop","id":1,"settings":invalid,"label":"Future","edited":null});
        assert_refused_unchanged(&files(8, &base(), &[op]), true);
    }
}
