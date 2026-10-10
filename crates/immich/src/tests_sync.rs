//! IMM-SYNC, IMM-PEOPLE and IMM-SEARCH: the pure merge, the region mapping, the client calls
//! against an in-process server, and hostile JSON.

use std::collections::{BTreeMap, BTreeSet};

use dac_catalog::Flag;
use dac_meta::{Region, RegionKind};

use super::*;
use crate::people;
use crate::sync::{self, Direction, FavoriteMap, Field, FieldRule, Policy, Side, Sides, Snapshot, SyncConfig};

fn snap(rating: u8, desc: &str, kw: &[&str]) -> Snapshot {
    Snapshot { rating, description: desc.into(), keywords: kw.iter().map(|s| s.to_string()).collect(), ..Snapshot::default() }
}

fn run(base: Option<&Snapshot>, l: &Snapshot, r: &Snapshot, cfg: &SyncConfig) -> sync::Outcome {
    run_at(base, l, r, cfg, None, None, &BTreeMap::new())
}

fn run_at(
    base: Option<&Snapshot>,
    l: &Snapshot,
    r: &Snapshot,
    cfg: &SyncConfig,
    lt: Option<&str>,
    rt: Option<&str>,
    forced: &BTreeMap<Field, Side>,
) -> sync::Outcome {
    let stuck = BTreeSet::new();
    sync::reconcile(Sides { base, local: l, remote: r, stuck: &stuck, forced, local_changed_at: lt, remote_updated_at: rt }, cfg)
}

#[test]
fn one_sided_changes_flow_to_the_other_side() {
    let cfg = SyncConfig::default();
    let base = snap(2, "old", &["a"]);
    // the catalog changed the rating, Immich the description
    let o = run(Some(&base), &snap(4, "old", &["a"]), &snap(2, "new", &["a"]), &cfg);
    assert_eq!(o.to_remote, [Field::Rating].into());
    assert_eq!(o.to_local, [Field::Description].into());
    assert!(o.conflicts.is_empty());
    assert_eq!(o.target.rating, 4);
    assert_eq!(o.target.description, "new");
    // nothing changed: nothing to do
    assert!(run(Some(&base), &base, &base, &cfg).is_noop());
}

#[test]
fn both_sides_changed_newest_wins_else_conflict() {
    let cfg = SyncConfig::default();
    let base = snap(2, "", &[]);
    let (l, r) = (snap(3, "", &[]), snap(5, "", &[]));
    let none = BTreeMap::new();
    let o = run_at(Some(&base), &l, &r, &cfg, Some("2026-01-01T10:00:00Z"), Some("2026-01-01T09:00:00.000Z"), &none);
    assert_eq!(o.to_remote, [Field::Rating].into(), "the catalog's change is newer");
    let o = run_at(Some(&base), &l, &r, &cfg, Some("2026-01-01T10:00:00Z"), Some("2026-01-01T12:30:00+02:00"), &none);
    assert_eq!(o.to_local, [Field::Rating].into(), "offsets are honoured: 12:30+02:00 is 10:30Z, later than 10:00Z");
}

#[test]
fn newest_compares_instants_across_zones() {
    assert!(sync::instant("2026-01-01T12:30:00+02:00") < sync::instant("2026-01-01T10:45:00Z"));
    assert_eq!(sync::instant("2026-01-01T00:00:00Z"), sync::instant("2026-01-01T00:00:00.000+00:00"));
    assert!(sync::instant("garbage").is_none());
    assert!(sync::instant("2026-13-01T00:00:00Z").is_none());
    assert!(sync::instant("").is_none());
}

#[test]
fn unknown_times_and_ask_policy_leave_conflicts() {
    let base = snap(2, "", &[]);
    let (l, r) = (snap(3, "", &[]), snap(5, "", &[]));
    let o = run(Some(&base), &l, &r, &SyncConfig::default());
    assert_eq!(o.conflicts, [Field::Rating].into());
    assert!(o.to_local.is_empty() && o.to_remote.is_empty());
    assert_eq!(o.target.rating, 3, "a conflicted field keeps the catalog's value");
    // the user decides: Immich
    let forced = [(Field::Rating, Side::Immich)].into();
    let o = run_at(Some(&base), &l, &r, &SyncConfig::default(), None, None, &forced);
    assert_eq!(o.to_local, [Field::Rating].into());
    assert_eq!(o.target.rating, 5);
    // policy per field
    let mut cfg = SyncConfig::default();
    cfg.rules.insert(Field::Rating, FieldRule { direction: Direction::TwoWay, policy: Policy::Catalog });
    assert_eq!(run(Some(&base), &l, &r, &cfg).to_remote, [Field::Rating].into());
}

#[test]
fn location_and_date_ask_by_default() {
    let base = Snapshot { location: Some((1.0, 2.0)), captured: Some("2020-01-01T00:00:00".into()), ..Snapshot::default() };
    let l = Snapshot { location: Some((1.5, 2.0)), captured: Some("2020-01-02T00:00:00".into()), ..Snapshot::default() };
    let r = Snapshot { location: Some((1.0, 2.5)), captured: Some("2020-01-03T00:00:00".into()), ..Snapshot::default() };
    let o = run_at(Some(&base), &l, &r, &SyncConfig::default(), Some("2030-01-01T00:00:00Z"), Some("2020-01-01T00:00:00Z"), &BTreeMap::new());
    assert_eq!(o.conflicts, [Field::Location, Field::Captured].into(), "even when one side is newer");
}

#[test]
fn keywords_merge_as_sets() {
    let cfg = SyncConfig::default();
    let base = snap(0, "", &["a", "b", "c"]);
    // catalog removed b, added x; Immich removed c, added y
    let o = run(Some(&base), &snap(0, "", &["a", "c", "x"]), &snap(0, "", &["a", "b", "y"]), &cfg);
    let want: BTreeSet<String> = ["a", "x", "y"].iter().map(|s| s.to_string()).collect();
    assert_eq!(o.target.keywords, want);
    assert!(o.conflicts.is_empty());
    assert_eq!(o.add_tags, ["x".to_string()].into());
    assert_eq!(o.remove_tags, ["b".to_string()].into());
    // first sync: a union
    let o = run(None, &snap(0, "", &["a"]), &snap(0, "", &["b"]), &cfg);
    assert_eq!(o.target.keywords.len(), 2);
}

#[test]
fn first_sync_takes_the_side_that_has_a_value() {
    let cfg = SyncConfig::default();
    let o = run(None, &snap(0, "", &[]), &snap(4, "from immich", &[]), &cfg);
    assert_eq!(o.to_local, [Field::Rating, Field::Description].into());
    let o = run(None, &snap(3, "", &[]), &snap(0, "", &[]), &cfg);
    assert_eq!(o.to_remote, [Field::Rating].into());
    let o = run(None, &snap(3, "", &[]), &snap(4, "", &[]), &cfg);
    assert_eq!(o.conflicts, [Field::Rating].into(), "both have a value and no times");
}

#[test]
fn directions_and_off() {
    let base = snap(1, "", &[]);
    let (l, r) = (snap(2, "", &[]), snap(3, "", &[]));
    let mut cfg = SyncConfig::default();
    cfg.rules.insert(Field::Rating, FieldRule { direction: Direction::ToImmich, policy: Policy::Ask });
    assert_eq!(run(Some(&base), &l, &r, &cfg).to_remote, [Field::Rating].into(), "one-way: its direction decides");
    assert!(run(Some(&base), &base, &r, &cfg).is_noop(), "Immich changes are ignored");
    cfg.rules.insert(Field::Rating, FieldRule { direction: Direction::FromImmich, policy: Policy::Ask });
    assert_eq!(run(Some(&base), &l, &r, &cfg).to_local, [Field::Rating].into());
    assert!(run(Some(&base), &l, &base, &cfg).is_noop(), "catalog changes are not sent");
    cfg.rules.insert(Field::Rating, FieldRule { direction: Direction::Off, policy: Policy::Ask });
    assert!(run(Some(&base), &l, &r, &cfg).is_noop());
}

fn photo_with(rating: u8, flag: Flag, caption: &str, kw: &[&str]) -> Photo {
    let mut p = Photo::new(PhotoId(7), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00");
    p.rating = rating;
    p.flag = flag;
    p.meta.caption = caption.into();
    p.meta.keywords = kw.iter().map(|s| s.to_string()).collect();
    p
}

#[test]
fn local_snapshot_and_ops_round_trip() {
    let cfg = SyncConfig::default();
    let private = |k: &str| k.starts_with("Private");
    let p = photo_with(3, Flag::Pick, " hi ", &["Trip|Paris", "Private|Home"]);
    let s = sync::local(&p, &cfg, &private);
    assert!(s.favorite && !s.archived);
    assert_eq!(s.description, "hi");
    assert_eq!(s.keywords, ["Trip|Paris".to_string()].into(), "export-excluded keywords are never sent");
    // apply a target: rating 5, unfavourite, a new keyword; the private keyword stays
    let mut t = s.clone();
    t.rating = 5;
    t.favorite = false;
    t.keywords.insert("Sun".into());
    let ops = sync::local_ops(&p, &t, &[Field::Rating, Field::Favorite, Field::Keywords].into(), &cfg, &private);
    let mut c = Catalog::default();
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    for op in ops {
        c.apply(op).unwrap();
    }
    let q = c.photo(PhotoId(7)).unwrap();
    assert_eq!(q.rating, 5);
    assert_eq!(q.flag, Flag::None);
    assert!(q.meta.keywords.contains(&"Private|Home".to_string()) && q.meta.keywords.contains(&"Sun".to_string()));
    assert_eq!(sync::local(q, &cfg, &private), t);
}

#[test]
fn favourite_as_five_stars() {
    let cfg = SyncConfig { favorite: FavoriteMap::FiveStars, ..SyncConfig::default() };
    let p = photo_with(5, Flag::None, "", &[]);
    assert!(sync::local(&p, &cfg, &|_| false).favorite);
    let t = Snapshot { rating: 5, favorite: false, ..Snapshot::default() };
    let ops = sync::local_ops(&p, &t, &[Field::Favorite].into(), &cfg, &|_| false);
    assert!(matches!(ops.as_slice(), [Op::SetRating { rating: 4, .. }]));
    let u = sync::remote_update(&Snapshot { favorite: true, ..Snapshot::default() }, &[Field::Favorite].into(), &cfg, None);
    assert_eq!(u.rating, Some(Some(5)));
}

#[test]
fn remote_snapshot_and_update_body() {
    let a: Asset = serde_json::from_value(serde_json::json!({
        "id": "x", "isFavorite": true, "visibility": "archive", "localDateTime": "2024-01-08T12:00:00.000Z",
        "exifInfo": {"rating": -1, "description": " d ", "latitude": 48.8566001, "longitude": 2.35, "dateTimeOriginal": "2024-01-08T12:00:00+02:00"},
        "tags": [{"id": "t", "value": "Trip/Paris"}]
    }))
    .unwrap();
    let s = sync::remote(&a);
    assert_eq!((s.rating, s.favorite, s.archived, s.description.as_str()), (0, true, true, "d"));
    assert_eq!(s.keywords, ["Trip|Paris".to_string()].into());
    assert_eq!(s.location, Some((48.8566, 2.35)));
    assert_eq!(s.captured.as_deref(), Some("2024-01-08T12:00:00"));
    assert_eq!(sync::zone_of("2024-01-08T12:00:00+02:00"), Some("+02:00"));
    assert_eq!(sync::zone_of("2024-01-08T12:00:00.123Z"), Some("Z"));
    assert_eq!(sync::zone_of("2024-01-08T12:00:00"), None);
    let all: BTreeSet<Field> = Field::ALL.into();
    let u = sync::remote_update(&s, &all, &SyncConfig::default(), Some("+02:00"));
    let v = serde_json::to_value(&u).unwrap();
    assert_eq!(v["rating"], serde_json::Value::Null, "0 stars clears the rating (Immich refuses 0)");
    assert!(v.as_object().unwrap().contains_key("rating"));
    assert_eq!(v["visibility"], "archive");
    assert_eq!(v["dateTimeOriginal"], "2024-01-08T12:00:00+02:00");
    assert_eq!(sync::keyword_to_tag("A/B | C"), "A-B/C");
    let locked = Asset { visibility: Some("locked".into()), ..Asset::default() };
    assert!(sync::untouchable(&locked));
}

#[test]
fn sync_state_round_trips_and_caps_its_log() {
    let mut st = sync::SyncState::default();
    st.items.insert("a".into(), sync::ItemState { photo: 1, base: Some(snap(1, "x", &["k"])), ..Default::default() });
    for _ in 0..(sync::LOG_CAP + 5) {
        st.push_log(sync::RunLog::default());
    }
    assert_eq!(st.log.len(), sync::LOG_CAP);
    let back: sync::SyncState = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
    assert_eq!(back, st);
    // a config with field rules survives JSON
    let mut cfg = SyncConfig::default();
    cfg.rules.insert(Field::Archived, FieldRule { direction: Direction::TwoWay, policy: Policy::Immich });
    let back: SyncConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(back, cfg);
}

// ---- people

fn face(pid: &str, name: &str, hidden: bool, b: [f64; 4]) -> Face {
    Face {
        id: "f".into(),
        bounding_box_x1: b[0],
        bounding_box_y1: b[1],
        bounding_box_x2: b[2],
        bounding_box_y2: b[3],
        image_width: 100.0,
        image_height: 50.0,
        person: (!pid.is_empty()).then(|| Person { id: pid.into(), name: name.into(), is_hidden: hidden, birth_date: None }),
        source_type: None,
    }
}

#[test]
fn faces_become_regions_marked_from_immich() {
    let mut p = photo_with(0, Flag::None, "", &[]);
    // a region of the photo's own XMP over the first face
    p.meta.regions.push(Region {
        rect: dac_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.5 },
        kind: RegionKind::Face,
        name: Some("Me".into()),
        description: None,
    });
    let faces = vec![
        face("p1", "Alice", false, [10.0, 5.0, 30.0, 25.0]),
        face("p2", "Bob", false, [60.0, 10.0, 80.0, 30.0]),
        face("p3", "Hidden", true, [40.0, 10.0, 50.0, 20.0]),
        face("", "", false, [0.0, 0.0, 5.0, 5.0]),
        face("p4", "Bad", false, [f64::NAN, 0.0, f64::INFINITY, 1.0]),
    ];
    let (regions, m) = people::regions_with_faces(&p, &faces, false);
    assert_eq!((m.added, m.skipped), (2, 1), "{m:?}");
    let bob = regions.iter().find(|r| r.name.as_deref() == Some("Bob")).unwrap();
    assert_eq!(people::person_of(bob), Some("p2"));
    assert!((bob.rect.x0 - 0.6).abs() < 1e-9 && (bob.rect.y1 - 0.6).abs() < 1e-9);
    assert!(regions.iter().any(|r| people::person_of(r) == Some("")), "an unnamed face is kept, unnamed");
    // again with the same faces: nothing changes
    p.meta.regions = regions.clone();
    let (again, m) = people::regions_with_faces(&p, &faces, false);
    assert_eq!(again, regions);
    assert_eq!((m.added, m.removed), (0, 0));
    // confirmed regions survive a re-import that no longer has them
    p.meta.regions = people::confirm(&p);
    assert!(p.meta.regions.iter().filter(|r| people::person_of(r).is_some()).all(people::is_confirmed));
    let (after, _) = people::regions_with_faces(&p, &[], false);
    assert_eq!(after.len(), p.meta.regions.len());
}

#[test]
fn names_given_in_the_app_go_back_as_renames_and_merges() {
    let mut a = photo_with(0, Flag::None, "", &[]);
    let r = |pid: &str, name: &str| Region {
        rect: dac_geom::Rect::UNIT,
        kind: RegionKind::Face,
        name: Some(name.into()),
        description: Some(format!("immich:{pid}")),
    };
    a.meta.regions = vec![r("p1", "Alice"), r("p2", "Alice"), r("p3", "Carol"), r("p4", "X"), r("p4", "Y")];
    let people = vec![
        Person { id: "p1".into(), name: "".into(), ..Default::default() },
        Person { id: "p2".into(), name: "Alice".into(), ..Default::default() },
        Person { id: "p3".into(), name: "Carol".into(), ..Default::default() },
        Person { id: "p4".into(), name: "".into(), ..Default::default() },
    ];
    let push = people::names_to_push(std::iter::once(&a), &people, true);
    assert_eq!(push.renames, vec![("p1".to_string(), "Alice".to_string())], "p4 has two names: ambiguous, left alone");
    assert_eq!(push.merges, vec![("p1".to_string(), vec!["p2".to_string()])]);
    assert!(people::names_to_push(std::iter::once(&a), &people, false).merges.is_empty());
}

// ---- client calls against an in-process server

fn sync_server(req: &Req) -> Vec<u8> {
    if req.header("x-api-key") != Some(KEY) {
        return response("401 Unauthorized", b"{}");
    }
    let path = req.target.split('?').next().unwrap_or_default();
    match (req.method.as_str(), path) {
        ("PUT", "/api/assets/a1") => response("200 OK", br#"{"id":"a1","isFavorite":true}"#),
        ("PUT", "/api/tags") => response("200 OK", br#"[{"id":"t1","value":"Trip"},{"id":"t2","value":"Trip/Paris"}]"#),
        ("PUT", "/api/tags/t2/assets") | ("DELETE", "/api/tags/t2/assets") => response("200 OK", br#"[{"id":"a1","success":true}]"#),
        ("GET", "/api/faces") => response(
            "200 OK",
            br#"[{"id":"f","boundingBoxX1":1,"boundingBoxY1":2,"boundingBoxX2":3,"boundingBoxY2":4,"imageWidth":10,"imageHeight":10,"person":{"id":"p","name":"A","isHidden":false}}]"#,
        ),
        ("GET", "/api/people") => {
            if req.target.contains("page=1") {
                response("200 OK", br#"{"people":[{"id":"p","name":"A","birthDate":"1990-01-02"}],"hasNextPage":true}"#)
            } else {
                response("200 OK", br#"{"people":[{"id":"q","name":"B","isHidden":true}],"hasNextPage":false}"#)
            }
        }
        ("PUT", "/api/people") | ("POST", "/api/people/p/merge") => response("200 OK", b"[]"),
        ("DELETE", "/api/assets") => response("204 No Content", b""),
        ("POST", "/api/search/smart") => response("200 OK", PAGE1.as_bytes()),
        _ => response("404 Not Found", b"{}"),
    }
}

#[test]
fn sync_people_and_search_calls() {
    let (url, seen) = serve(sync_server);
    let c = client(&url, KEY);
    let u = AssetUpdate { rating: Some(None), is_favorite: Some(true), ..Default::default() };
    assert_eq!(c.update_asset("a1", &u).unwrap().id, "a1");
    let tags = c.upsert_tags(&["Trip/Paris".into()]).unwrap();
    assert_eq!(tags.len(), 2);
    c.tag_assets("t2", &["a1".into()]).unwrap();
    c.untag_assets("t2", &["a1".into()]).unwrap();
    let f = c.faces("a1").unwrap();
    assert_eq!(f[0].person.as_ref().unwrap().name, "A");
    let ppl = c.people_all().unwrap();
    assert_eq!(ppl.len(), 2);
    assert!(ppl[1].is_hidden && ppl[0].birth_date.is_some());
    c.rename_people(&[("p".into(), "Z".into())]).unwrap();
    c.merge_people("p", &["q".into()]).unwrap();
    c.trash_assets(&["a1".into()]).unwrap();
    let page = c.smart_search(&SmartSearch { query: "a dog".into(), size: Some(10), ..Default::default() }).unwrap();
    assert_eq!(page.items.len(), 3);
    let seen = seen.lock().unwrap();
    let put = seen.iter().find(|r| r.target == "/api/assets/a1").unwrap();
    let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
    assert_eq!(body, serde_json::json!({"rating": null, "isFavorite": true}), "only what changes is sent");
    let trash = seen.iter().find(|r| r.method == "DELETE" && r.target == "/api/assets").unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&trash.body).unwrap()["force"], false, "to the trash, never a hard delete");
    // hostile ids never reach a path
    assert!(c.faces("../x").is_err() && c.merge_people("p", &["a/b".into()]).is_err() && c.trash_assets(&["?".into()]).is_err());
}

/// Fuzz-style: mutated server answers never panic the decoders (errors are fine).
#[test]
fn hostile_json_never_panics_the_decoders() {
    let seeds: [&[u8]; 4] = [
        PAGE1.as_bytes(),
        br#"[{"id":"f","boundingBoxX1":1e308,"boundingBoxY1":-1,"boundingBoxX2":"x","imageWidth":0,"person":null}]"#,
        br#"{"people":[{"id":"p","name":"A"}],"hasNextPage":true}"#,
        br#"{"id":"a","exifInfo":{"rating":99,"latitude":1e400,"longitude":-1e999},"tags":[{"value":"////"}],"localDateTime":"9999-99-99T99:99:99"}"#,
    ];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for seed in seeds {
        for _ in 0..400 {
            let mut b = seed.to_vec();
            for _ in 0..(next() % 8 + 1) {
                if b.is_empty() {
                    break;
                }
                let i = (next() as usize) % b.len();
                match next() % 3 {
                    0 => b[i] = (next() & 0xff) as u8,
                    1 => {
                        b.remove(i);
                    }
                    _ => b.insert(i, b"{}[]\":,-0e9"[(next() as usize) % 11]),
                }
            }
            if let Ok(a) = serde_json::from_slice::<Asset>(&b) {
                let s = sync::remote(&a);
                let _ = sync::remote_update(&s, &Field::ALL.into(), &SyncConfig::default(), a.local_date_time.as_deref().and_then(sync::zone_of));
            }
            if let Ok(fs) = serde_json::from_slice::<Vec<Face>>(&b) {
                let p = photo_with(0, Flag::None, "", &[]);
                let _ = people::regions_with_faces(&p, &fs, true);
            }
            let _ = serde_json::from_slice::<SearchResponse>(&b);
            let _ = serde_json::from_slice::<sync::SyncState>(&b);
            let _ = std::str::from_utf8(&b).map(|t| (sync::instant(t), sync::zone_of(t)));
        }
    }
}
