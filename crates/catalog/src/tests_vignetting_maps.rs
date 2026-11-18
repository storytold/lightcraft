use std::sync::Arc;

use lcc_develop::{DevelopSettings, EmbeddedVignettingMap, VignettingMapId, VignettingMapOrigin};

use crate::db::DB_FILE;
use crate::store::FsStore;
use crate::tests_v4::Scratch;
use crate::transfer::{ConflictRule, ExportOptions, plan_import, subset};
use crate::*;

const SHA_A: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
const SHA_B: &str = "84983e441c3bd26ebaae4aa1f95129e5e54670f1";

fn map(ev: f64, sha: &str) -> EmbeddedVignettingMap {
    let source = VignettingMapOrigin { sha1: Some(sha.into()), file_name: Some("REF.NEF".into()), ..Default::default() };
    EmbeddedVignettingMap::from_ev([40, 30], [4, 3], None, source, &(0..12).map(|i| ev * f64::from(i) / 11.0).collect::<Vec<_>>()).unwrap()
}

fn record(m: &EmbeddedVignettingMap, photo: Option<PhotoId>) -> Box<VignettingMapRecord> {
    Box::new(VignettingMapRecord { map: m.clone(), photo, added: "2026-11-11T00:00:00".into(), withdrawn: false })
}

/// A photo with file SHA-1 `sha`, whose settings use map `uses` (current settings, a History step and a Version).
fn add_photo(c: &mut Catalog, i: u64, sha: &str, uses: Option<&VignettingMapId>) -> PhotoId {
    let mut p = crate::tests_v4::photo(c, i);
    p.sha1 = Some(sha.into());
    if let Some(id) = uses {
        let mut d = DevelopSettings::default();
        d.optics.vignetting_calibration = Some(id.clone());
        let s = Arc::new(d);
        p.develop = s.clone();
        p.history = vec![HistoryStep { label: "Flat Field".into(), settings: s.clone(), batch: None }];
        p.versions = vec![Version { name: "v".into(), created: "2026-01-02T00:00:00".into(), settings: s, auto: false }];
    }
    let id = p.id;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

#[test]
fn a_map_is_stored_once_and_undo_never_removes_it() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    let inv = c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    assert_eq!(inv, Op::Batch { ops: vec![] }, "the inverse leaves the table alone");
    c.apply(inv).unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.photo), Some(Some(reference)), "undo kept it");
    // the same id again keeps the first record
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, None) }).unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.photo), Some(Some(reference)));
    assert_eq!(c.vignetting_maps().count(), 1);
    // a link to a photo that isn't here is no link
    let other = map(2.0, SHA_B);
    c.apply(Op::AddVignettingMap { id: VignettingMapId::of(&other), record: record(&other, Some(PhotoId(999))) }).unwrap();
    assert_eq!(c.vignetting_map(&VignettingMapId::of(&other)).map(|r| r.photo), Some(None));
}

#[test]
fn a_map_under_another_id_or_a_broken_map_is_refused() {
    let mut c = Catalog::new();
    let m = map(1.0, SHA_A);
    let wrong = VignettingMapId::of(&map(1.5, SHA_A));
    assert!(matches!(c.apply(Op::AddVignettingMap { id: wrong, record: record(&m, None) }), Err(CatalogError::Invalid(_))));
    let mut broken = m.clone();
    broken.grid = [1, 12];
    assert!(c.apply(Op::AddVignettingMap { id: VignettingMapId::of(&broken), record: record(&broken, None) }).is_err());
    assert_eq!(c.vignetting_maps().count(), 0);
}

#[test]
fn removing_the_reference_photo_keeps_the_map_for_the_photos_that_use_it() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    let user = add_photo(&mut c, 2, SHA_B, Some(&id));
    assert_eq!(reference_photo(&c, &id), Some(reference));
    c.apply(Op::RemovePhoto { id: reference }).unwrap();
    assert!(c.vignetting_map(&id).is_some(), "the map stays");
    assert_eq!(reference_photo(&c, &id), None, "no longer in this catalog");
    assert_eq!(c.photo(user).unwrap().develop.optics.vignetting_calibration.as_ref(), Some(&id));
    // History and Versions reference it too
    assert_eq!(c.photo(user).unwrap().vignetting_map_ids().count(), 3);
}

/// The approved cross-catalog rule: a stored id counts only when that photo here has the map's reference SHA-1;
/// otherwise the photo with that SHA-1; otherwise none.
#[test]
fn the_reference_photo_is_trusted_by_its_file_not_its_id() {
    let mut c = Catalog::new();
    let unrelated = add_photo(&mut c, 1, SHA_B, None);
    let real = add_photo(&mut c, 2, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    // the stored id names a photo whose file is another: the SHA-1 finds the real one
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(unrelated)) }).unwrap();
    assert_eq!(reference_photo(&c, &id), Some(real));
    // no photo with that file: none, even though the stored id exists
    let mut c2 = Catalog::new();
    let only = add_photo(&mut c2, 1, SHA_B, None);
    c2.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(only)) }).unwrap();
    assert_eq!(reference_photo(&c2, &id), None);
    // a map without a SHA-1 links to nothing
    let bare = EmbeddedVignettingMap { source: VignettingMapOrigin::default(), ..m };
    c2.apply(Op::AddVignettingMap { id: VignettingMapId::of(&bare), record: record(&bare, Some(only)) }).unwrap();
    assert_eq!(reference_photo(&c2, &VignettingMapId::of(&bare)), None);
}

#[test]
fn the_table_survives_snapshots_checkpoints_and_the_log() {
    let s = Scratch::new("ff");
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    let ops = vec![
        Op::AddPhoto { photo: Box::new((**c.photo(reference).unwrap()).clone()) },
        Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) },
    ];
    // (the photo is already in `c`; the log gets both ops)
    c.apply(ops[1].clone()).unwrap();
    j.append(&ops).unwrap();
    drop(j);
    // replayed from the log
    let (mut j, c2, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    assert_eq!(c2.vignetting_map(&id).map(|r| r.map.clone()), Some(m.clone()));
    // a checkpoint into the v4 store, then a load from it
    j.snapshot(&c2).unwrap();
    drop(j);
    let (_j, c3, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    assert_eq!(c3.vignetting_map(&id).map(|r| (r.map.clone(), r.photo)), Some((m.clone(), Some(reference))));
    let (_, _, bad) = crate::db::CatalogDb::open(&s.path().join(DB_FILE)).and_then(|mut d| d.load()).unwrap();
    assert_eq!(bad, 0);
    // the JSON snapshot form
    let back = Catalog::from_snapshot(&c3.to_snapshot()).unwrap();
    assert_eq!(back.vignetting_map(&id).map(|r| r.map.clone()), Some(m));
}

#[test]
fn a_damaged_row_is_left_out_and_the_rest_loads() {
    let mut c = Catalog::new();
    let good = map(1.0, SHA_A);
    let other = map(2.0, SHA_B);
    for m in [&good, &other] {
        c.apply(Op::AddVignettingMap { id: VignettingMapId::of(m), record: record(m, None) }).unwrap();
    }
    let mut v: serde_json::Value = serde_json::from_str(&c.to_snapshot()).unwrap();
    let key = VignettingMapId::of(&other).to_string();
    v["vignetting_maps"][&key]["map"]["ev"] = serde_json::json!("AAAA");
    v["vignetting_maps"]["not-an-id"] = serde_json::json!({"map": 1});
    let back = Catalog::from_snapshot(&v.to_string()).unwrap();
    assert_eq!(back.vignetting_maps().count(), 1);
    assert!(back.vignetting_map(&VignettingMapId::of(&good)).is_some());
    // a row filed under another map's id is no row either
    let mut v: serde_json::Value = serde_json::from_str(&c.to_snapshot()).unwrap();
    let swapped = v["vignetting_maps"][&key].clone();
    v["vignetting_maps"][VignettingMapId::of(&good).as_str()] = swapped;
    assert_eq!(Catalog::from_snapshot(&v.to_string()).unwrap().vignetting_maps().count(), 1);
    // and in the v4 store
    let s = Scratch::new("ffbad");
    let (mut j, _, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    j.snapshot(&c).unwrap();
    drop(j);
    {
        let db = redb::Database::open(s.path().join(DB_FILE)).unwrap();
        let w = db.begin_write().unwrap();
        w.open_table(redb::TableDefinition::<&str, &[u8]>::new("vignetting_maps")).unwrap().insert(key.as_str(), b"{broken".as_slice()).unwrap();
        w.commit().unwrap();
    }
    let (c2, _, bad) = crate::db::CatalogDb::open(&s.path().join(DB_FILE)).and_then(|mut d| d.load()).unwrap();
    assert_eq!((c2.vignetting_maps().count(), bad), (1, 1));
}

#[test]
fn export_as_catalog_takes_the_maps_along_and_drops_a_reference_left_behind() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    let user = add_photo(&mut c, 2, SHA_B, Some(&id));
    let with = subset(&c, &ExportOptions { photos: vec![reference, user], ..Default::default() });
    assert_eq!(with.vignetting_map(&id).map(|r| r.photo), Some(Some(reference)));
    let without = subset(&c, &ExportOptions { photos: vec![user], ..Default::default() });
    assert_eq!(without.vignetting_map(&id).map(|r| r.photo), Some(None), "the reference stayed behind");
    let none = subset(&c, &ExportOptions { photos: vec![reference], ..Default::default() });
    assert_eq!(none.vignetting_maps().count(), 0, "no exported photo uses it");
}

#[test]
fn import_from_another_catalog_remaps_the_reference_or_drops_it() {
    // the other catalog: the reference photo and a photo using its map
    let mut other = Catalog::new();
    let _pad = add_photo(&mut other, 7, "cccccccccccccccccccccccccccccccccccccccc", None);
    let reference = add_photo(&mut other, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    other.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    add_photo(&mut other, 2, SHA_B, Some(&id));
    // this library: a photo that happens to have the other's reference id, with another file
    let mut here = Catalog::new();
    let _first = add_photo(&mut here, 8, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", None);
    let squatter = add_photo(&mut here, 9, "dddddddddddddddddddddddddddddddddddddddd", None);
    assert_eq!(squatter, reference, "the same id, another photo");
    let plan = plan_import(&here, &other);
    let op = plan.ops(&mut here, &other, ConflictRule::Keep);
    here.apply(op).unwrap();
    let r = here.vignetting_map(&id).expect("the map came along");
    assert_ne!(r.photo, Some(squatter), "never the other catalog's id");
    let new_ref = reference_photo(&here, &id).expect("the reference photo came along");
    assert_eq!(here.photo(new_ref).unwrap().sha1.as_deref(), Some(SHA_A));
    assert_eq!(r.photo, Some(new_ref), "remapped to this library's id for it");
}

/// Withdrawing is one undoable op: the record changes, the map stays; undo puts it back in use. On a map the
/// catalog doesn't hold it does nothing.
#[test]
fn a_map_is_withdrawn_and_put_back_in_use_by_undo() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    let user = add_photo(&mut c, 2, SHA_B, Some(&id));
    let inv = c.apply(Op::SetVignettingMapState { id: id.clone(), withdrawn: true }).unwrap();
    assert_eq!(inv, Op::SetVignettingMapState { id: id.clone(), withdrawn: false });
    assert_eq!(c.vignetting_map(&id).map(|r| r.withdrawn), Some(true));
    assert_eq!(c.photo(user).unwrap().develop.optics.vignetting_calibration.as_ref(), Some(&id), "the photo keeps its map");
    c.apply(inv).unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.withdrawn), Some(false));
    let unknown = VignettingMapId::of(&map(3.0, SHA_B));
    assert_eq!(c.apply(Op::SetVignettingMapState { id: unknown, withdrawn: true }).unwrap(), Op::Batch { ops: vec![] });
    // the field is left out of the JSON while false (older readers of format 18 and the tests' fixtures)
    let json = serde_json::to_string(&**c.vignetting_map(&id).unwrap()).unwrap();
    assert!(!json.contains("withdrawn"), "{json}");
}

/// Create adds a map withdrawn and puts it in use in the same batch: undoing the batch withdraws it.
#[test]
fn undoing_a_create_batch_withdraws_the_map() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    let mut r = record(&m, Some(reference));
    r.withdrawn = true;
    let inv = c
        .apply(Op::Batch {
            ops: vec![Op::AddVignettingMap { id: id.clone(), record: r }, Op::SetVignettingMapState { id: id.clone(), withdrawn: false }],
        })
        .unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.withdrawn), Some(false));
    let redo = c.apply(inv).unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.withdrawn), Some(true), "undo withdrew it; the map stays");
    c.apply(redo).unwrap();
    assert_eq!(c.vignetting_map(&id).map(|r| r.withdrawn), Some(false), "redo put it back in use");
}

/// The reference index follows the table, the photos' files and deletion, with no refresh, and a photo with an active
/// and a withdrawn map is a reference.
#[test]
fn the_reference_state_follows_the_table() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let other = add_photo(&mut c, 2, SHA_B, None);
    assert_eq!(c.vignetting_reference_state(reference), None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    let since = c.revision;
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, None) }).unwrap();
    assert_eq!(c.vignetting_reference_state(reference), Some(ReferenceState::Reference), "found by its SHA-1");
    assert_eq!(c.vignetting_reference_state(other), None);
    assert!(c.touched_since(since).is_some_and(|t| t.contains(&reference)), "smart collections hear of it");
    let since = c.revision;
    c.apply(Op::SetVignettingMapState { id: id.clone(), withdrawn: true }).unwrap();
    assert_eq!(c.vignetting_reference_state(reference), Some(ReferenceState::Withdrawn));
    assert!(c.touched_since(since).is_some_and(|t| t.contains(&reference)));
    // a second map from the same photo, in use: a reference again
    let m2 = map(2.0, SHA_A);
    c.apply(Op::AddVignettingMap { id: VignettingMapId::of(&m2), record: record(&m2, Some(reference)) }).unwrap();
    assert_eq!(c.vignetting_reference_state(reference), Some(ReferenceState::Reference));
    // deleted: no longer a reference here
    c.apply(Op::SetDeleted { id: reference, deleted: true }).unwrap();
    assert_eq!(c.vignetting_reference_state(reference), None);
    assert!(c.vignetting_reference_states().is_empty());
}

/// A withdraw on a map already in the v4 store reaches the next checkpoint (the store compares records, not ids).
#[test]
fn a_withdrawn_map_survives_a_checkpoint() {
    let s = Scratch::new("ffwd");
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    let add = Op::AddVignettingMap { id: id.clone(), record: record(&m, None) };
    c.apply(add.clone()).unwrap();
    j.append(&[add]).unwrap();
    j.snapshot(&c).unwrap();
    let wd = Op::SetVignettingMapState { id: id.clone(), withdrawn: true };
    c.apply(wd.clone()).unwrap();
    j.append(&[wd]).unwrap();
    j.snapshot(&c).unwrap();
    drop(j);
    let (c2, _, _) = crate::db::CatalogDb::open(&s.path().join(DB_FILE)).and_then(|mut d| d.load()).unwrap();
    assert_eq!(c2.vignetting_map(&id).map(|r| r.withdrawn), Some(true), "the store has it");
    let (_j, c3, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    assert_eq!(c3.vignetting_map(&id).map(|r| r.withdrawn), Some(true));
}

/// The library filter and the smart-collection rule read the reference state, follow a withdraw at once, and a rule
/// with a value the field doesn't have is a problem.
#[test]
fn the_filter_and_the_rule_find_references() {
    let mut c = Catalog::new();
    let reference = add_photo(&mut c, 1, SHA_A, None);
    let other = add_photo(&mut c, 2, SHA_B, None);
    let m = map(1.0, SHA_A);
    let id = VignettingMapId::of(&m);
    c.apply(Op::AddVignettingMap { id: id.clone(), record: record(&m, Some(reference)) }).unwrap();
    let find = |c: &Catalog, want: &str| {
        let f = crate::query::Filter { vignetting_reference: Some(want.into()), ..Default::default() };
        let mut ids = c.query(&f, &crate::query::Sort::default());
        ids.sort();
        ids
    };
    let rule = |c: &Catalog, op: &str, want: &str| {
        let set = RuleSet {
            mode: Match::All,
            rules: vec![Rule::Field { field: "vignettingReference".into(), op: op.into(), value: serde_json::json!(want) }],
        };
        assert!(set.check(c).is_empty(), "{:?}", set.check(c));
        let mut ids: Vec<PhotoId> = c.photos().filter(|p| set.matches(p, c)).map(|p| p.id).collect();
        ids.sort();
        ids
    };
    assert_eq!(find(&c, "reference"), vec![reference]);
    assert_eq!(find(&c, "none"), vec![other]);
    assert_eq!(rule(&c, "is", "reference"), vec![reference]);
    assert_eq!(rule(&c, "isNot", "reference"), vec![other]);
    c.apply(Op::SetVignettingMapState { id, withdrawn: true }).unwrap();
    assert_eq!(find(&c, "reference"), Vec::<PhotoId>::new());
    // withdrawn: a normal photo to the filter and the rule
    assert_eq!(find(&c, "none"), vec![reference, other]);
    assert_eq!(rule(&c, "is", "none"), vec![reference, other]);
    assert_eq!(rule(&c, "is", "reference"), Vec::<PhotoId>::new());
    assert_eq!(find(&c, "withdrawn"), Vec::<PhotoId>::new(), "no such value");
    assert!(!c.is_vignetting_reference(reference));
    assert_eq!(find(&c, "nonsense"), Vec::<PhotoId>::new());
    let bad = RuleSet {
        mode: Match::All,
        rules: vec![Rule::Field { field: "vignettingReference".into(), op: "is".into(), value: serde_json::json!("withdrawn") }],
    };
    assert!(!bad.check(&c).is_empty());
    let f = crate::query::Filter { vignetting_reference: Some("reference".into()), ..Default::default() };
    assert!(f.describe().contains("vignetting reference") && !f.describe().contains("not"), "{}", f.describe());
}
