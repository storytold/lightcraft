//! Find Duplicate Photos (`library.findDuplicates`, `duplicates.pump`): the same picture at another size or
//! compression is grouped and the best copy kept; another picture is left alone; the view and selection show the
//! result; the run has a row in the activity stack; bad parameters are refused before anything is decoded.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-dupes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A smooth procedural picture of `seed`, drawn in normalized coordinates so every size shows the same picture.
fn picture(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity(w * h * 3);
    let s = seed as f64;
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
            let r = 0.5 + 0.5 * (std::f64::consts::TAU * (u * (1.0 + s) + s * 0.3)).sin();
            let g = 0.5 + 0.5 * (std::f64::consts::TAU * (v * (2.0 + s * 0.5))).cos();
            let b = 0.5 + 0.5 * (std::f64::consts::TAU * ((u + v) * 1.5 + s)).sin();
            // a bright block whose place depends on the seed
            let block = (u - 0.2 - 0.15 * s).abs() < 0.12 && (v - 0.6 + 0.1 * s).abs() < 0.15;
            let (r, g, b) = if block { (0.95, 0.9, 0.2) } else { (r, g, b) };
            px.extend_from_slice(&[(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]);
        }
    }
    px
}

fn write_jpeg(path: &Path, w: usize, h: usize, seed: u32, quality: u8) {
    let px = picture(w, h, seed);
    let bytes =
        encode_jpeg(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&px)), quality, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library of: picture 0 at 300 × 200 (quality 90), the same at 150 × 100, the same re-saved at quality 35, and
/// picture 1 at 300 × 200. Returns the session and the ids by file name.
fn library(tag: &str) -> (Session, std::collections::HashMap<String, u64>) {
    let dir = temp_dir(tag);
    write_jpeg(&dir.join("a.jpg"), 300, 200, 0, 90);
    write_jpeg(&dir.join("a_small.jpg"), 150, 100, 0, 90);
    write_jpeg(&dir.join("a_resaved.jpg"), 300, 200, 0, 35);
    write_jpeg(&dir.join("b.jpg"), 300, 200, 1, 90);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(4), "{r}");
    let ids = s.catalog.photos().map(|p| (p.file_name.clone(), p.id.0)).collect();
    (s, ids)
}

fn pump_until_idle(s: &mut Session) -> Value {
    let started = Instant::now();
    loop {
        let v = s.execute("duplicates.pump", &json!({"pace": "normal"})).unwrap();
        if v["active"] == false {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "the job never ended: {v}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ids_of(v: &Value) -> Vec<u64> {
    v.as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect()
}

#[test]
fn the_same_picture_at_another_size_or_compression_is_grouped_and_the_best_copy_kept() {
    let (mut s, ids) = library("group");
    let undo = s.undo.len();
    let r = s.execute("library.findDuplicates", &json!({"wait": true})).unwrap();
    assert_eq!(r["queued"], 4, "{r}");
    let last = &r["last"];
    assert_eq!(last["photos"], 4);
    assert_eq!(last["decoded"], 4);
    assert_eq!(last["unreadable"], 0);
    assert_eq!(last["cancelled"], false);
    let groups = last["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "one group of the three copies of picture 0, not picture 1: {last}");
    let g = &groups[0];
    assert_eq!(g["exact"], false);
    assert_eq!(g["keep"], ids["a.jpg"], "the largest, least compressed copy is kept: {g}");
    let mut delete = ids_of(&g["delete"]);
    delete.sort_unstable();
    let mut expected = vec![ids["a_small.jpg"], ids["a_resaved.jpg"]];
    expected.sort_unstable();
    assert_eq!(delete, expected, "{g}");
    assert_eq!(last["duplicates"], 2);
    let why: Vec<&str> = g["photos"].as_array().unwrap().iter().filter_map(|p| p["why"].as_str()).collect();
    assert!(why.iter().any(|w| w.contains("300×200 beats 150×100")), "{why:?}");
    assert!(why.iter().any(|w| w.contains("less compression")), "{why:?}");
    // the view shows the group alone and the copies to remove are selected, ready for Delete
    let mut vis: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    vis.sort_unstable();
    let mut all_a = vec![ids["a.jpg"], ids["a_small.jpg"], ids["a_resaved.jpg"]];
    all_a.sort_unstable();
    assert_eq!(vis, all_a, "the view is filtered to the group");
    let mut sel: Vec<u64> = s.selection.ids.iter().map(|p| p.0).collect();
    sel.sort_unstable();
    assert_eq!(sel, expected, "the lower-quality copies are selected");
    assert!(s.selection.active.is_some_and(|a| expected.contains(&a.0)));
    assert_eq!(s.undo.len(), undo, "no undo step: nothing in the catalog changed");
    // Delete then removes exactly them
    s.execute("photo.delete", &json!({})).unwrap();
    let left: Vec<u64> = s.catalog.photos().filter(|p| p.in_library()).map(|p| p.id.0).collect();
    assert!(left.contains(&ids["a.jpg"]) && left.contains(&ids["b.jpg"]) && left.len() == 2, "{left:?}");
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.visible_cloned().len(), 2);
}

#[test]
fn a_second_run_decodes_nothing_and_a_run_without_duplicates_shows_an_empty_view() {
    let (mut s, ids) = library("again");
    s.execute("library.findDuplicates", &json!({"wait": true})).unwrap();
    let r = s.execute("library.findDuplicates", &json!({"wait": true})).unwrap();
    assert_eq!(r["queued"], 0, "{r}");
    assert_eq!(r["last"]["decoded"], 0);
    assert!(s.activity.list().iter().all(|t| t.kind != "duplicates"), "the grouping-only run's row is gone too");
    assert_eq!(r["last"]["groups"].as_array().map(Vec::len), Some(1));
    // only the two different pictures: no duplicates, an explicitly empty view, Clear Filters brings everything back
    let r = s.execute("library.findDuplicates", &json!({"ids": [ids["a.jpg"], ids["b.jpg"]], "wait": true})).unwrap();
    assert_eq!(r["last"]["groups"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(s.visible_cloned().len(), 0, "no duplicates must show no photos, not the whole library");
    assert!(s.selection.ids.is_empty() && s.selection.active.is_none(), "nothing the empty view doesn't show stays selected for Delete");
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.visible_cloned().len(), 4);
    // with filter and selection switched off nothing but the answer changes
    s.execute("library.select", &json!({"ids": [ids["b.jpg"]]})).unwrap();
    let r = s.execute("library.findDuplicates", &json!({"wait": true, "filter": false, "selectExtras": false})).unwrap();
    assert_eq!(r["last"]["groups"].as_array().map(Vec::len), Some(1));
    assert_eq!(s.visible_cloned().len(), 4);
    assert_eq!(s.selection.ids, vec![PhotoId(ids["b.jpg"])]);
}

#[test]
fn the_run_has_a_row_in_the_activity_stack_and_the_frame_pump_finishes_it() {
    let (mut s, _) = library("activity");
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    let rows = s.activity.list();
    let row = rows.iter().find(|t| t.kind == "duplicates").unwrap_or_else(|| panic!("no row: {rows:?}"));
    assert_eq!(row.label, "Finding duplicate photos");
    assert!(row.cancellable, "✕ stops it");
    assert_eq!(row.total, 4);
    // a second call while it runs starts nothing
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    assert_eq!((r["queued"].as_u64(), r["running"].as_bool()), (Some(0), Some(true)));
    let v = pump_until_idle(&mut s);
    assert!(s.activity.list().iter().all(|t| t.kind != "duplicates"), "the row goes when the job ends");
    assert_eq!(v["last"]["groups"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["hashed"], 4);
    assert_eq!(s.visible_cloned().len(), 3, "the pump that ends the run shows the group");
}

/// A script that waits while a run is already going gets that run's result, not a "running" answer.
#[test]
fn waiting_on_a_run_already_going_returns_its_result() {
    let (mut s, ids) = library("wait");
    s.execute("duplicates.pump", &json!({"pace": "pause"})).unwrap();
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    let r = s.execute("library.findDuplicates", &json!({"wait": true})).unwrap();
    assert_eq!(r["running"], false, "{r}");
    assert_eq!(r["last"]["groups"][0]["keep"], ids["a.jpg"], "{r}");
    assert!(s.activity.list().iter().all(|t| t.kind != "duplicates"));
}

/// The difference hash is held to the same distance as the DCT hash: a pair whose difference hashes are further
/// apart than `distance` is not a duplicate, whatever the DCT hash says.
#[test]
fn both_hashes_must_be_within_the_distance() {
    let a = crate::dupes::Sig { dct: 0, diff: 0, aspect: 1.5 };
    let b = crate::dupes::Sig { dct: 0b11, diff: 0b111_1111, aspect: 1.5 };
    assert!(!crate::dupes::same_picture_for_tests(&a, &b, 6), "7 difference-hash bits at distance 6");
    let c = crate::dupes::Sig { dct: 0b11, diff: 0b11_1111, aspect: 1.5 };
    assert!(crate::dupes::same_picture_for_tests(&a, &c, 6));
    let d = crate::dupes::Sig { dct: 0, diff: 0, aspect: 1.6 };
    assert!(!crate::dupes::same_picture_for_tests(&a, &d, 6), "another frame");
}

#[test]
fn cancelling_the_run_shows_nothing_and_keeps_the_view() {
    let (mut s, ids) = library("cancel");
    s.execute("library.select", &json!({"ids": [ids["b.jpg"]]})).unwrap();
    let selection = s.selection.clone();
    // paused, the worker hashes nothing, so the cancel arrives before the run can end by itself
    s.execute("duplicates.pump", &json!({"pace": "pause"})).unwrap();
    s.execute("library.findDuplicates", &json!({})).unwrap();
    let id = s.activity.list().iter().find(|t| t.kind == "duplicates").map(|t| t.id).unwrap();
    s.execute("activity.cancel", &json!({"id": id})).unwrap();
    let v = pump_until_idle(&mut s);
    assert!(s.activity.list().iter().all(|t| t.kind != "duplicates"));
    assert_eq!(v["last"]["cancelled"], true, "{v}");
    assert_eq!(s.visible_cloned().len(), 4, "a stopped run leaves the view alone: {v}");
    assert_eq!(s.selection, selection, "and the selection");
}

#[test]
fn bad_parameters_are_refused_before_anything_is_decoded() {
    let (mut s, _) = library("params");
    for p in [
        json!({"distance": 99}),
        json!({"distance": -1}),
        json!({"distance": "6"}),
        json!({"filter": "yes"}),
        json!({"selectExtras": 1}),
        json!({"ids": "3"}),
    ] {
        let e = s.execute("library.findDuplicates", &p).unwrap_err().to_string();
        assert!(e.contains("distance") || e.contains("filter") || e.contains("selectExtras") || e.contains("ids"), "{p}: {e}");
    }
    assert!(s.activity.list().is_empty(), "nothing started");
    let v = s.execute("duplicates.pump", &json!({})).unwrap();
    assert_eq!(v["hashed"], 0);
}

#[test]
fn an_unreadable_file_is_counted_and_the_rest_still_group() {
    let (mut s, ids) = library("unreadable");
    // a photo whose file is gone
    let gone = temp_dir("gone").join("gone.jpg");
    write_jpeg(&gone, 300, 200, 2, 90);
    let r = s.execute("library.import", &json!({"paths": [gone.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    std::fs::remove_file(&gone).unwrap();
    let r = s.execute("library.findDuplicates", &json!({"wait": true})).unwrap();
    assert_eq!(r["last"]["unreadable"], 1, "{r}");
    assert_eq!(r["last"]["groups"][0]["keep"], ids["a.jpg"]);
}
