//! RAW+JPEG detection, journaled links, and rejections.

use std::time::{Duration, Instant};

use crate::journal::Journal;
use crate::*;

fn file(c: &mut Catalog, path: &str, kind: MediaKind, captured: Option<&str>, camera: &str) -> PhotoId {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let id = c.alloc_photo_id();
    let mut p =
        Photo::new(id, Source::File { path: path.to_string() }, name, if kind == MediaKind::Raw { "RAF" } else { "JPEG" }, 64, 48, "2026-01-01");
    p.kind = kind;
    p.captured = captured.map(str::to_string);
    p.meta.camera = camera.to_string();
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn pair_of(scan: &pairs::PairScan, raw: PhotoId) -> Option<PhotoId> {
    scan.pairs.iter().find(|p| p.raw == raw).map(|p| p.jpeg)
}

#[test]
fn same_stem_pairs_and_edge_cases_do_not() {
    let mut c = Catalog::new();
    let raw = file(&mut c, "/pics/DSCF0001.RAF", MediaKind::Raw, Some("2026-04-01T10:00:00"), "Fujifilm X-S10");
    let jpeg = file(&mut c, "/pics/dscf0001.jpg", MediaKind::Image, Some("2026-04-01T10:00:02"), "FUJIFILM X-S10");
    let export = file(&mut c, "/pics/exported/DSCF0001.JPG", MediaKind::Image, Some("2026-04-01T10:00:00"), "Fujifilm X-S10");
    let dng = file(&mut c, "/pics/DSCF0002.DNG", MediaKind::Raw, None, "");
    let _raf = file(&mut c, "/pics/DSCF0002.RAF", MediaKind::Raw, None, "");
    let late = file(&mut c, "/pics/DSCF0003.RAF", MediaKind::Raw, Some("2026-04-01T10:00:00"), "");
    let _late_jpg = file(&mut c, "/pics/DSCF0003.JPG", MediaKind::Image, Some("2026-04-01T10:00:05"), "");
    let cam = file(&mut c, "/other/DSCF0004.RAF", MediaKind::Raw, Some("2026-04-01T10:00:00"), "Fujifilm X-S10");
    let _cam_jpg = file(&mut c, "/other/DSCF0004.JPG", MediaKind::Image, Some("2026-04-01T10:00:00"), "Sony ILCE-7M4");
    let heic_raw = file(&mut c, "/pics/DSCF0005.RAF", MediaKind::Raw, None, "");
    let _heic = file(&mut c, "/pics/DSCF0005.HEIC", MediaKind::Image, None, "");
    let dup = file(&mut c, "/pics/DSCF0006.RAF", MediaKind::Raw, None, "");
    let _j1 = file(&mut c, "/pics/DSCF0006.JPG", MediaKind::Image, None, "");
    let _j2 = file(&mut c, "/pics/DSCF0006.JPEG", MediaKind::Image, None, "");
    let alone = file(&mut c, "/pics/DSCF0007.RAF", MediaKind::Raw, None, "");

    let scan = c.scan_pairs();
    assert_eq!(pair_of(&scan, raw), Some(jpeg));
    assert!(scan.exports.iter().any(|e| e.raw == raw && e.jpeg == export));
    assert!(scan.ambiguous.iter().any(|a| a.stem == "dscf0002" && a.reason == "two raws" && a.ids.contains(&dng)));
    assert!(scan.ambiguous.iter().any(|a| a.stem == "dscf0003" && a.reason == "capture time mismatch" && a.ids.contains(&late)));
    assert!(scan.ambiguous.iter().any(|a| a.stem == "dscf0004" && a.reason == "camera mismatch" && a.ids.contains(&cam)));
    assert!(scan.ambiguous.iter().any(|a| a.stem == "dscf0005" && a.reason == "HEIC companion" && a.ids.contains(&heic_raw)));
    assert!(scan.ambiguous.iter().any(|a| a.stem == "dscf0006" && a.reason == "more than one JPEG" && a.ids.contains(&dup)));
    assert!(pair_of(&scan, alone).is_none());
    assert!(scan.pairs.iter().all(|p| p.raw != alone && p.jpeg != export));
}

#[test]
fn join_split_rejection_reopen_and_permanent_delete() {
    let mut c = Catalog::new();
    let raw = file(&mut c, "/pics/DSCF0001.RAF", MediaKind::Raw, Some("2026-04-01T10:00:00"), "Fujifilm X-S10");
    let jpeg = file(&mut c, "/pics/DSCF0001.JPG", MediaKind::Image, Some("2026-04-01T10:00:01"), "Fujifilm X-S10");
    let scan = c.scan_pairs();
    let found = scan.pairs.iter().find(|p| p.raw == raw).unwrap();
    let id = c.alloc_pair_id();
    c.apply(Op::AddPair { pair: RawJpegPair { id, raw: found.raw, jpeg: found.jpeg } }).unwrap();
    assert_eq!(c.pair_partner(raw), Some(jpeg));

    let stack_top = file(&mut c, "/pics/other.RAF", MediaKind::Raw, None, "");
    let grouped = c.group_ops(stack_top, &[raw], true).unwrap();
    c.apply(grouped).unwrap();
    assert!(c.stack_of(raw).is_some());
    assert!(c.pair_of(raw).is_some(), "a user stack does not remove the pair");

    let inv = c.apply(Op::RemovePair { id }).unwrap();
    c.apply(Op::RejectPair { directory: "/pics".into(), stem: "DSCF0001".into(), rejected: true }).unwrap();
    assert!(c.scan_pairs().pairs.is_empty());
    assert!(c.scan_pairs().ambiguous.iter().any(|a| a.reason == "rejected"));
    c.apply(inv).unwrap();
    assert_eq!(c.pair_partner(raw), Some(jpeg));
    c.apply(Op::RejectPair { directory: "/pics".into(), stem: "dscf0001".into(), rejected: false }).unwrap();

    let store = MemStore::new();
    store.set(
        journal::SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},\"seq\":0,\"catalog\":{}}}\n", journal::VERSION, c.to_snapshot()).into_bytes(),
    );
    store.set(journal::LOG, Vec::new());
    let (_, loaded, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(loaded.pair_partner(raw), Some(jpeg));

    let mut gone = loaded;
    gone.apply(gone.delete_photos_permanently_ops(&[raw])).unwrap();
    assert!(gone.pair_of(jpeg).is_none());
    assert!(gone.photo(jpeg).is_some());
    assert!(gone.photo(raw).is_none());
    assert!(gone.stack_of(stack_top).is_none());
    assert!(gone.stack_of(jpeg).is_none());
}

#[test]
fn renaming_apart_drops_the_link_and_undo_restores_it() {
    let mut c = Catalog::new();
    let raw = file(&mut c, "/pics/DSCF0001.RAF", MediaKind::Raw, None, "");
    let jpeg = file(&mut c, "/pics/DSCF0001.JPG", MediaKind::Image, None, "");
    let id = c.alloc_pair_id();
    c.apply(Op::AddPair { pair: RawJpegPair { id, raw, jpeg } }).unwrap();
    let inv =
        c.apply(Op::SetFile { id: jpeg, file_name: "DSCF0099.JPG".into(), source: Source::File { path: "/pics/DSCF0099.JPG".into() } }).unwrap();
    assert!(c.pair_of(raw).is_none());
    c.apply(inv).unwrap();
    assert_eq!(c.pair_partner(raw), Some(jpeg));
    assert_eq!(c.photo(jpeg).unwrap().file_name, "DSCF0001.JPG");
}

#[test]
fn pair_index_stays_near_one_catalog_walk() {
    let mut c = Catalog::new();
    for i in 0..10_000 {
        let raw = file(&mut c, &format!("/pics/IMG_{i:05}.RAF"), MediaKind::Raw, None, "");
        let jpeg = file(&mut c, &format!("/pics/IMG_{i:05}.JPG"), MediaKind::Image, None, "");
        let id = c.alloc_pair_id();
        c.apply(Op::AddPair { pair: RawJpegPair { id, raw, jpeg } }).unwrap();
    }
    let walk = Instant::now();
    let n = c.photos().count();
    let walk_ns = walk.elapsed();
    let scan_at = Instant::now();
    let found = c.scan_pairs();
    let scan_ns = scan_at.elapsed();
    assert_eq!(n, 20_000);
    assert_eq!(found.pairs.len(), 10_000);
    let budget = walk_ns.saturating_mul(40).max(Duration::from_millis(1500));
    assert!(scan_ns <= budget, "scan {scan_ns:?} exceeded {budget:?} (walk {walk_ns:?})");
}

#[test]
fn undo_permanent_delete_restores_a_deleted_pairs_member() {
    let mut c = Catalog::new();
    let raw = file(&mut c, "/pics/DSCF0001.RAF", MediaKind::Raw, None, "");
    let jpeg = file(&mut c, "/pics/DSCF0001.JPG", MediaKind::Image, None, "");
    let id = c.alloc_pair_id();
    c.apply(Op::AddPair { pair: RawJpegPair { id, raw, jpeg } }).unwrap();
    c.apply(Op::SetDeleted { id: raw, deleted: true }).unwrap();
    let before = c.to_snapshot();
    let inverse = c.apply(c.delete_photos_permanently_ops(&[raw])).unwrap();
    c.apply(inverse).unwrap();
    assert_eq!(c.to_snapshot(), before);
    assert!(c.photo(raw).unwrap().deleted);
    assert_eq!(c.pair_partner(raw), Some(jpeg));
    assert!(c.scan_pairs().pairs.is_empty(), "deleted photos are not fresh candidates");
}
