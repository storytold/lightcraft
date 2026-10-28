//! Issue #2154: a snapshot keeps each distinct segmentation grid once (`Catalog.grids`) and the
//! masks of the develop settings, versions, history steps and import look refer to it by index,
//! so an unchanged mask no longer repeats with every history step. In-memory catalogs and log
//! records always hold the grids inline; older catalogs (inline everywhere) load and are upgraded.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, SegMask};

use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::*;

fn snapshot_version(m: &MemStore) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    v["version"].as_u64().unwrap()
}

fn open(m: &MemStore) -> (Journal, Catalog, LoadReport) {
    Journal::open(Box::new(m.clone())).unwrap()
}

/// Develop settings with one AI mask over an object segmentation.
fn settings_with_seg(seg: SegMask, exposure: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = exposure;
    s.masks.push(Mask {
        id: 1,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object {
                hint: vec![lightcraft_geom::Point::new(0.5, 0.5)],
                exclude: vec![],
                seg: Some(seg),
                detail: vec![],
                edge: 0.0,
            },
        }],
        ..Default::default()
    });
    s
}

/// One mask (`id`) whose only component is an Object segmentation `seg`.
fn object_mask(id: u32, seg: SegMask) -> Mask {
    Mask {
        id,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object {
                hint: vec![lightcraft_geom::Point::new(0.5, 0.5)],
                exclude: vec![],
                seg: Some(seg),
                detail: vec![],
                edge: 0.0,
            },
        }],
        ..Default::default()
    }
}

/// A catalog of one photo whose history is `steps` slider edits over the same mask, plus the
/// ops that built it (each step is one `SetDevelop` + one `PushHistory`, as the engine commits
/// an edit) and the shared grid's bytes.
fn masked_catalog(steps: usize) -> (Catalog, Vec<Op>, PhotoId, String) {
    let seg = SegMask::from_logits(64, &vec![0.5f32; 64 * 64]);
    let blob = seg.data.clone();
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let add = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: "/p/1.jpg".into() }, "1.jpg", "JPEG", 60, 40, "2026-01-01")) };
    c.apply(add.clone()).unwrap();
    let mut ops = vec![add];
    for i in 1..=steps {
        let settings = Arc::new(settings_with_seg(seg.clone(), f64::from(i as u32) * 0.1));
        let op = Op::Batch {
            ops: vec![
                Op::SetDevelop { id, settings: settings.clone(), label: "Exposure".into(), edited: Some(format!("t{i}")) },
                Op::PushHistory { id, step: HistoryStep { label: "Exposure".into(), settings } },
            ],
        };
        c.apply(op.clone()).unwrap();
        ops.push(op);
    }
    (c, ops, id, blob)
}

/// N history steps over one mask store the grid exactly once and load back identical.
#[test]
fn snapshot_deduplicates_shared_grid() {
    let (live, ops, id, blob) = masked_catalog(20);
    let m = MemStore::new();
    let (mut j, _, report) = open(&m);
    assert!(report.created);
    j.append(&ops).unwrap();
    j.snapshot(&live).unwrap();
    drop(j);

    let snap_bytes = m.get(SNAPSHOT).unwrap();
    let body = std::str::from_utf8(&snap_bytes).unwrap();
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert_eq!(body.match_indices(&blob).count(), 1, "the grid is stored once for every step that shares it");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["catalog"]["grids"].as_array().unwrap().len(), 1);

    let (_, loaded, report) = open(&m);
    assert_eq!((report.replayed, report.upgraded_from, report.repaired.len()), (0, None, 0));
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
    let p = loaded.photo(id).unwrap();
    assert_eq!(p.history.len(), 20);
    for h in &p.history {
        let MaskShape::Object { seg: Some(seg), .. } = &h.settings.masks[0].components[0].shape else { panic!("an Object mask") };
        assert_eq!((seg.data.as_str(), seg.grid), (blob.as_str(), None), "every step's grid is back inline");
    }
}

/// A catalog written before the shared table (grids inline everywhere, format v9) loads as it
/// was and its upgrade rewrite deduplicates.
#[test]
fn v9_catalog_with_inline_grids_loads_and_is_upgraded() {
    let (live, _, _, blob) = masked_catalog(3);
    let body = live.to_snapshot();
    assert!(body.contains(&blob), "a live catalog stores its grids inline");
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":9,\"seq\":2,\"catalog\":{body}}}\n").into_bytes());
    let (_, loaded, report) = open(&m);
    assert_eq!((report.replayed, report.upgraded_from), (0, Some(9)));
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
    // rewritten in this format before anything is appended, with the grid once
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    let rewritten_bytes = m.get(SNAPSHOT).unwrap();
    let rewritten = std::str::from_utf8(&rewritten_bytes).unwrap();
    assert_eq!(rewritten.match_indices(&blob).count(), 1);
    assert!(rewritten.contains("\"grids\":["), "the shared table is in the file");
}

/// The op log is not deduplicated: every record keeps the grids inline (a log never carries the
/// shared table), and replaying it rebuilds the same state.
#[test]
fn log_records_keep_their_grids_inline() {
    let (live, ops, _, blob) = masked_catalog(2);
    let m = MemStore::new();
    let (mut j, _, _) = open(&m);
    j.append(&ops).unwrap();
    drop(j); // no snapshot: the log holds the whole state
    let log_bytes = m.get(LOG).unwrap();
    let log = std::str::from_utf8(&log_bytes).unwrap();
    assert_eq!(log.match_indices(&blob).count(), 4, "two edits, SetDevelop + PushHistory each, inline");
    let (_, loaded, report) = open(&m);
    assert_eq!(report.replayed, 3, "the photo and the two edits");
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
}

/// A reference that points past the shared table is cleared (the mask renders nothing, like any
/// damaged grid); a good reference resolves back to the full grid.
#[test]
fn shared_grid_references_resolve_or_are_repaired() {
    let (live, _, id, blob) = masked_catalog(1);
    let inline = live.to_snapshot();

    // a damaged snapshot: the develop mask refers past the one-entry table
    let body = inline.replacen(&format!("\"data\":\"{blob}\""), "\"grid\":7", 1);
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!(
            "{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{{\"grids\":[\"dHdv\"],{} }} }}\n",
            &body[1..body.len() - 1]
        )
        .into_bytes(),
    );
    let (_, damaged, report) = open(&m);
    assert_eq!(report.repaired.len(), 1, "{:?}", report.repaired);
    assert!(report.repaired[0].contains("pointed past the shared table"), "{:?}", report.repaired);
    let p = damaged.photo(id).unwrap();
    let MaskShape::Object { seg: Some(seg), .. } = &p.develop.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!((seg.data.as_str(), seg.grid), ("", None), "the dangling reference is cleared, never a crash");
    // the history step's inline grid is untouched
    let MaskShape::Object { seg: Some(seg), .. } = &p.history[0].settings.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!(seg.data, blob);

    // a good reference resolves (the normal way a snapshot in this format reads, so it is not
    // reported as a repair): the mask reads exactly as it was written
    let body = inline.replacen(&format!("\"data\":\"{blob}\""), "\"grid\":0", 1);
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!(
            "{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{{\"grids\":[\"{blob}\"],{} }} }}\n",
            &body[1..body.len() - 1]
        )
        .into_bytes(),
    );
    let (_, resolved, report) = open(&m);
    assert_eq!(report.repaired, Vec::<String>::new(), "{:?}", report.repaired);
    assert_eq!(resolved.to_snapshot(), live.to_snapshot());
}

/// The background snapshot path shares the grids too (the interner is per thread).
#[test]
fn background_snapshot_deduplicates_shared_grid() {
    let (live, ops, _, blob) = masked_catalog(5);
    let mut m = MemStore::new();
    m.background = true;
    let (mut j, _, _) = open(&m);
    j.append(&ops).unwrap();
    j.snapshot_in_background(&live).unwrap();
    j.wait().unwrap();
    drop(j);
    let snap_bytes = m.get(SNAPSHOT).unwrap();
    let body = std::str::from_utf8(&snap_bytes).unwrap();
    assert_eq!(body.match_indices(&blob).count(), 1);
    let (_, loaded, _) = open(&m);
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
}

/// A version (not only history steps) shares the grid: one photo with a snapshot version and a
/// history step over the same mask stores the grid once.
#[test]
fn versions_share_the_grid_too() {
    let seg = SegMask::from_logits(32, &vec![1.0f32; 32 * 32]);
    let blob = seg.data.clone();
    let id = PhotoId(1);
    let add = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, "d.jpg", "JPEG", 60, 40, "2026-01-01")) };
    let settings = Arc::new(settings_with_seg(seg, 0.5));
    let versions =
        Op::SetVersions { id, versions: vec![Version { name: "v".into(), created: "2026-01-01".into(), settings: settings.clone(), auto: false }] };
    let history = Op::PushHistory { id, step: HistoryStep { label: "e".into(), settings } };
    let mut live = Catalog::new();
    for op in [&add, &versions, &history] {
        live.apply(op.clone()).unwrap();
    }
    let m = MemStore::new();
    let (mut j, _, _) = open(&m);
    j.append(&[add, versions, history]).unwrap();
    j.snapshot(&live).unwrap();
    drop(j);
    let snap_bytes = m.get(SNAPSHOT).unwrap();
    let body = std::str::from_utf8(&snap_bytes).unwrap();
    assert_eq!(body.match_indices(&blob).count(), 1, "the version and the step share the grid");
    let (_, loaded, _) = open(&m);
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
}

/// The import look (`Photo.import_look`, the settings a default preset gave the photo on import)
/// can hold AI masks, and their grids can be grids the develop settings or a history step hold.
/// Every settings holder shares the snapshot's table, so the look's masks are referenced and
/// resolved on load like the others; a grid only the look holds is in the table too.
#[test]
fn import_look_masks_share_the_table_and_resolve_on_load() {
    let shared = SegMask::from_logits(64, &vec![0.5f32; 64 * 64]);
    let look_only = SegMask::from_logits(64, &vec![-0.5f32; 64 * 64]);
    let mut look = DevelopSettings::default();
    look.masks.push(object_mask(1, shared.clone()));
    look.masks.push(object_mask(2, look_only.clone()));
    let mut edit = DevelopSettings::default();
    edit.light.exposure = 0.7;
    edit.masks.push(object_mask(1, shared.clone()));
    let edit = Arc::new(edit);

    let id = PhotoId(1);
    let mut photo = Photo::new(id, Source::File { path: "/p/2.jpg".into() }, "2.jpg", "JPEG", 60, 40, "2026-01-01");
    photo.develop = edit.clone();
    photo.import_look = Some(Arc::new(look));
    photo.history.push(HistoryStep { label: "Exposure".into(), settings: edit });
    let add = Op::AddPhoto { photo: Box::new(photo) };
    let mut live = Catalog::new();
    live.apply(add.clone()).unwrap();

    let m = MemStore::new();
    let (mut j, _, _) = open(&m);
    j.append(&[add]).unwrap();
    j.snapshot(&live).unwrap();
    drop(j);
    let snap_bytes = m.get(SNAPSHOT).unwrap();
    let body = std::str::from_utf8(&snap_bytes).unwrap();
    assert_eq!(body.match_indices(&shared.data).count(), 1, "the grid of the look and of the edit is stored once");
    assert_eq!(body.match_indices(&look_only.data).count(), 1);

    let (_, loaded, report) = open(&m);
    assert_eq!(report.repaired, Vec::<String>::new(), "{:?}", report.repaired);
    let p = loaded.photo(id).unwrap();
    let look_masks = &p.import_look.as_ref().unwrap().masks;
    assert_eq!(look_masks.len(), 2);
    for (mask, want) in look_masks.iter().zip([&shared, &look_only]) {
        let MaskShape::Object { seg: Some(seg), .. } = &mask.components[0].shape else { panic!("an Object mask") };
        assert_eq!((seg.data.as_str(), seg.grid), (want.data.as_str(), None), "the look's grid is back inline");
    }
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["catalog"]["grids"].as_array().unwrap().len(), 2, "one table entry per distinct grid, the look's included");
}

/// A `grid` that arrives from a document (a command's mask JSON, a preset, a merged partial) is
/// not a reference and is never written as one: a snapshot must not resolve it to another mask's
/// grid. Here photo two's mask carries `grid: 0` with empty data, while photo one's grid is the
/// table's entry 0; before the fix the inline form wrote `"grid":0` and the load filled photo two
/// with photo one's grid.
#[test]
fn stray_grid_from_a_document_is_not_written_as_a_reference() {
    let shared = SegMask::from_logits(64, &vec![0.5f32; 64 * 64]);
    let stray: SegMask = serde_json::from_str(r#"{"side":64,"data":"","grid":0}"#).unwrap();
    assert_eq!(stray.grid, Some(0), "a document can set grid");
    let mut stray_settings = DevelopSettings::default();
    stray_settings.masks.push(object_mask(1, stray));

    let (one, two) = (PhotoId(1), PhotoId(2));
    let photo = |id: PhotoId, name: &str| Photo::new(id, Source::File { path: format!("/p/{name}") }, name, "JPEG", 60, 40, "2026-01-01");
    let ops = [
        Op::AddPhoto { photo: Box::new(photo(one, "1.jpg")) },
        Op::SetDevelop { id: one, settings: Arc::new(settings_with_seg(shared.clone(), 0.2)), label: "Exposure".into(), edited: Some("t1".into()) },
        Op::AddPhoto { photo: Box::new(photo(two, "2.jpg")) },
        Op::SetDevelop { id: two, settings: Arc::new(stray_settings), label: "Edit".into(), edited: Some("t2".into()) },
    ];
    let mut live = Catalog::new();
    for op in &ops {
        live.apply(op.clone()).unwrap();
    }

    let m = MemStore::new();
    let (mut j, _, _) = open(&m);
    j.append(&ops).unwrap();
    j.snapshot(&live).unwrap();
    drop(j);
    let (_, loaded, report) = open(&m);
    assert_eq!(report.repaired, Vec::<String>::new(), "{:?}", report.repaired);
    let MaskShape::Object { seg: Some(seg), .. } = &loaded.photo(two).unwrap().develop.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!((seg.data.as_str(), seg.grid), ("", None), "the stray reference is not resolved to photo one's grid");
    assert_eq!(loaded.to_snapshot(), live.to_snapshot());
}

/// A snapshot whose shared grid table is empty (`grids: []`) but whose masks still refer into
/// it: the table being empty must not skip the repair. A reference past an empty table is
/// dangling, so it is cleared (the mask renders nothing, like any other damaged grid), the
/// repair is reported, and a mask that carries its own grid inline is left untouched.
#[test]
fn empty_shared_grid_table_clears_dangling_references() {
    let (live, _, id, blob) = masked_catalog(1);
    let inline = live.to_snapshot();

    // a damaged snapshot: an empty table, but the develop mask still refers to entry 0
    let body = inline.replacen(&format!("\"data\":\"{blob}\""), "\"grid\":0", 1);
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!(
            "{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{{\"grids\":[],{} }} }}\n",
            &body[1..body.len() - 1]
        )
        .into_bytes(),
    );
    let (_, damaged, report) = open(&m);
    assert_eq!(report.repaired.len(), 1, "{:?}", report.repaired);
    assert!(report.repaired[0].contains("pointed past the shared table"), "{:?}", report.repaired);
    let p = damaged.photo(id).unwrap();
    let MaskShape::Object { seg: Some(seg), .. } = &p.develop.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!((seg.data.as_str(), seg.grid), ("", None), "the dangling reference is cleared even with an empty table");

    // the history step carries its grid inline: nothing there to resolve, left as it was
    let MaskShape::Object { seg: Some(seg), .. } = &p.history[0].settings.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!(seg.data, blob, "the history step's inline grid is untouched");
}

/// A snapshot that omits the shared grid table entirely (no `grids` key) but whose masks still
/// refer into it: an absent table deserializes to an empty Vec, so a reference past it is
/// dangling and is cleared and reported, just like the empty-table case.
#[test]
fn omitted_shared_grid_table_clears_dangling_references() {
    let (live, _, id, blob) = masked_catalog(1);
    let inline = live.to_snapshot();

    // a damaged snapshot: no table at all, but the develop mask still refers to entry 0
    let body = inline.replacen(&format!("\"data\":\"{blob}\""), "\"grid\":0", 1);
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{{{} }} }}\n", &body[1..body.len() - 1])
            .into_bytes(),
    );
    let (_, damaged, report) = open(&m);
    assert_eq!(report.repaired.len(), 1, "{:?}", report.repaired);
    assert!(report.repaired[0].contains("pointed past the shared table"), "{:?}", report.repaired);
    let p = damaged.photo(id).unwrap();
    let MaskShape::Object { seg: Some(seg), .. } = &p.develop.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!((seg.data.as_str(), seg.grid), ("", None), "the dangling reference is cleared even with no table");

    // the history step carries its grid inline: nothing there to resolve, left as it was
    let MaskShape::Object { seg: Some(seg), .. } = &p.history[0].settings.masks[0].components[0].shape else { panic!("an Object mask") };
    assert_eq!(seg.data, blob, "the history step's inline grid is untouched");
}
