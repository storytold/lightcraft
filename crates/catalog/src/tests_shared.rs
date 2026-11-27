//! Photos whose develop settings are equal share one settings value in memory
//! ([`crate::shared`]): a library of photos nobody edited holds the defaults once, not once per
//! photo.

use std::sync::Arc;

use lightcraft_develop::DevelopSettings;

use crate::journal::encode_record;
use crate::*;

fn photo(id: u64) -> Photo {
    Photo::new(
        PhotoId(id),
        Source::File { path: format!("/pictures/2019/trip/IMG_{id:06}.jpg") },
        &format!("IMG_{id:06}.jpg"),
        "JPEG",
        4032,
        3024,
        "2026-10-10T12:00:00",
    )
}

fn raw(id: u64) -> Photo {
    let mut p = photo(id);
    p.kind = MediaKind::Raw;
    p.format = "CR3".into();
    p.as_shot_wb = Some((5200.0, 3.0));
    // as an import gives it: settings of its own that equal every other CR3's
    p.develop = Arc::new(p.import_defaults());
    p
}

fn edited(d: impl FnOnce(&mut DevelopSettings)) -> Arc<DevelopSettings> {
    let mut s = DevelopSettings::default();
    d(&mut s);
    Arc::new(s)
}

fn catalog_of(photos: impl IntoIterator<Item = Photo>) -> Catalog {
    let mut c = Catalog::new();
    for p in photos {
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    c
}

fn settings_of(c: &Catalog, id: u64) -> &Arc<DevelopSettings> {
    &c.photo(PhotoId(id)).unwrap().develop
}

/// How many different settings values the catalog's photos hold between them.
fn distinct(c: &Catalog) -> usize {
    let mut seen: Vec<*const DevelopSettings> = c.photos().map(|p| Arc::as_ptr(&p.develop)).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// The catalog through a saved library and back: from its snapshot, from a library directory's
/// snapshot file, and from the log alone.
fn reloaded(c: &Catalog) -> Vec<(&'static str, Catalog)> {
    let mut out = vec![("snapshot", Catalog::from_snapshot(&c.to_snapshot()).unwrap())];
    let m = MemStore::new();
    let (mut j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.snapshot(c).unwrap();
    drop(j);
    out.push(("library", Journal::open(Box::new(m)).unwrap().1));
    let m = MemStore::new();
    let log: String =
        c.photos().enumerate().map(|(k, p)| encode_record(k as u64 + 1, &Op::AddPhoto { photo: Box::new(p.as_ref().clone()) }) + "\n").collect();
    m.set(journal::LOG, log.into_bytes());
    out.push(("the log", Journal::open(Box::new(m)).unwrap().1));
    out
}

/// Given photos nobody edited, when they are added to a catalog or a saved library is opened,
/// then they all hold the same settings value.
#[test]
fn unedited_photos_share_the_default_settings() {
    let c = catalog_of((1..=20).map(photo));
    assert_eq!(distinct(&c), 1);
    assert_eq!(**settings_of(&c, 1), DevelopSettings::default());
    for (how, back) in reloaded(&c) {
        assert_eq!(back.len(), 20, "{how}");
        assert_eq!(distinct(&back), 1, "{how}");
        assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
    }
}

/// Given raws that were given their import defaults one by one (equal settings, a value each),
/// then they share one value too, also after the library is opened again.
#[test]
fn photos_with_equal_settings_share_them() {
    let c = catalog_of((1..=20).map(raw));
    assert_ne!(**settings_of(&c, 1), DevelopSettings::default());
    assert_eq!(distinct(&c), 1);
    for (how, back) in reloaded(&c) {
        assert_eq!(distinct(&back), 1, "{how}");
        assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
    }
    // settings set by a change are shared like those a photo arrives with
    let mut c = catalog_of((1..=4).map(photo));
    for id in 1..=4 {
        c.apply(Op::SetDevelop { id: PhotoId(id), settings: edited(|s| s.light.exposure = 0.5), label: "Exposure".into(), edited: None }).unwrap();
    }
    assert_eq!(distinct(&c), 1);
}

/// Given photos that share their settings, when one of them is edited, then only that one
/// changes: sharing is never a way for one photo's edit to reach another.
#[test]
fn editing_one_photo_leaves_the_ones_it_shared_with() {
    let mut c = catalog_of((1..=3).map(photo));
    let undo =
        c.apply(Op::SetDevelop { id: PhotoId(2), settings: edited(|s| s.light.exposure = 1.0), label: "Exposure".into(), edited: None }).unwrap();
    assert_eq!(settings_of(&c, 2).light.exposure, 1.0);
    assert_eq!((&**settings_of(&c, 1), &**settings_of(&c, 3)), (&DevelopSettings::default(), &DevelopSettings::default()));
    assert!(Arc::ptr_eq(settings_of(&c, 1), settings_of(&c, 3)));
    // and undoing it gives the photo the shared value back
    c.apply(undo).unwrap();
    assert_eq!(distinct(&c), 1);
    // a caller that edits a copy of a photo's settings (`Arc::make_mut` copies a shared value) and
    // gives it back as a change leaves the photos that held the old value alone
    let mut own = c.photo(PhotoId(1)).unwrap().as_ref().clone();
    Arc::make_mut(&mut own.develop).light.contrast = 30.0;
    c.apply(Op::SetDevelop { id: PhotoId(1), settings: own.develop.clone(), label: "Contrast".into(), edited: None }).unwrap();
    assert_eq!(settings_of(&c, 1).light.contrast, 30.0);
    assert_eq!(**settings_of(&c, 2), DevelopSettings::default());
    assert_eq!(**settings_of(&c, 3), DevelopSettings::default());
    assert_eq!(*shared::default_settings(), DevelopSettings::default());
    c.apply(Op::SetDevelop { id: PhotoId(1), settings: shared::default_settings(), label: "Reset".into(), edited: None }).unwrap();
    // the same after a reload
    for (how, mut back) in reloaded(&c) {
        back.apply(Op::SetDevelop { id: PhotoId(2), settings: edited(|s| s.light.exposure = 1.0), label: "Exposure".into(), edited: None }).unwrap();
        assert_eq!(**settings_of(&back, 1), DevelopSettings::default(), "{how}");
        assert_eq!(**settings_of(&back, 3), DevelopSettings::default(), "{how}");
    }
}

/// A photo's history steps, versions and import look share with its settings where they are
/// equal: after the library is opened again as before it was saved.
#[test]
fn a_photos_history_shares_with_its_settings() {
    let now = edited(|s| s.light.exposure = 0.3);
    let before = edited(|s| s.light.contrast = 10.0);
    let mut p = photo(1);
    p.develop = now.clone();
    p.history = vec![
        HistoryStep { label: "Contrast".into(), settings: before.clone() },
        HistoryStep { label: "Reset".into(), settings: Arc::new(DevelopSettings::default()) },
        HistoryStep { label: "Exposure".into(), settings: now.clone() },
    ];
    p.versions = vec![Version { name: "A".into(), created: "2026-10-10".into(), settings: before.clone(), auto: false }];
    p.import_look = Some(before);
    let c = catalog_of([p, photo(2)]);
    for (how, back) in reloaded(&c).into_iter().chain([("as added", c.clone())]) {
        let p = back.photo(PhotoId(1)).unwrap();
        assert!(Arc::ptr_eq(&p.develop, &p.history[2].settings), "{how}");
        assert!(Arc::ptr_eq(&p.history[0].settings, &p.versions[0].settings), "{how}");
        assert!(Arc::ptr_eq(&p.history[0].settings, p.import_look.as_ref().unwrap()), "{how}");
        assert!(Arc::ptr_eq(&p.history[1].settings, settings_of(&back, 2)), "{how}: the defaults");
        assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
    }
}

/// A long history (more steps than a thread remembers values) still ends with the photo's
/// settings as one value.
#[test]
fn a_long_history_ends_with_the_photos_settings() {
    let mut p = photo(1);
    let mut d = DevelopSettings::default();
    for step in 0..40 {
        d.light.exposure = f64::from(step) * 0.01;
        p.history.push(HistoryStep { label: "Exposure".into(), settings: Arc::new(d.clone()) });
    }
    p.develop = Arc::new(d);
    let c = catalog_of([p, photo(2)]);
    for (how, back) in reloaded(&c).into_iter().chain([("as added", c.clone())]) {
        let p = back.photo(PhotoId(1)).unwrap();
        assert!(Arc::ptr_eq(&p.develop, &p.history[39].settings), "{how}");
        assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
        // the next photo's settings are its own
        assert_eq!(**settings_of(&back, 2), DevelopSettings::default(), "{how}");
    }
}

/// Settings that are equal but for the sign of a zero are not the same settings (their JSON,
/// and the fingerprint and preview-cache key made from it, differ): they are never shared.
#[test]
fn settings_that_differ_in_the_sign_of_a_zero_are_not_shared() {
    let negative = |id: u64| {
        let mut p = photo(id);
        p.develop = edited(|s| s.wb.tint = -0.0);
        p
    };
    let c = catalog_of([photo(1), negative(2), photo(3), negative(4)]);
    assert_eq!(*settings_of(&c, 2), *settings_of(&c, 1), "equal as numbers");
    for (how, back) in reloaded(&c).into_iter().chain([("as added", c.clone())]) {
        for id in [2, 4] {
            let p = back.photo(PhotoId(id)).unwrap();
            assert!(!Arc::ptr_eq(&p.develop, settings_of(&back, 1)), "{how}");
            assert!(p.develop.wb.tint.is_sign_negative(), "{how}");
            assert_eq!(p.develop.hash64(), c.photo(PhotoId(id)).unwrap().develop.hash64(), "{how}");
        }
        assert!(settings_of(&back, 1).wb.tint.is_sign_positive() && Arc::ptr_eq(settings_of(&back, 1), settings_of(&back, 3)), "{how}");
    }
}

/// Settings that aren't the defaults and their twin with a negative zero, met one after the
/// other in either order, as two photos' settings and as a photo's settings and history step:
/// each keeps its own sign, and neither is ever handed the other's value.
#[test]
fn a_negative_zero_twin_is_never_what_others_share() {
    let twin = |tint: f64| {
        edited(|s| {
            s.light.exposure = 0.5;
            s.wb.tint = tint;
        })
    };
    for (first, second) in [(-0.0, 0.0), (0.0, -0.0)] {
        let (mut a, mut b, mut both) = (photo(1), photo(2), photo(3));
        a.develop = twin(first);
        b.develop = twin(second);
        both.develop = twin(first);
        both.history = vec![HistoryStep { label: "x".into(), settings: twin(second) }, HistoryStep { label: "y".into(), settings: twin(first) }];
        let c = catalog_of([a, b, both]);
        for (how, back) in reloaded(&c).into_iter().chain([("as added", c.clone())]) {
            let sign = |s: &Arc<DevelopSettings>| s.wb.tint.is_sign_negative();
            let (a, b, both) = (settings_of(&back, 1), settings_of(&back, 2), back.photo(PhotoId(3)).unwrap());
            assert_eq!((sign(a), sign(b)), (first.is_sign_negative(), second.is_sign_negative()), "{how} {first} {second}");
            assert!(!Arc::ptr_eq(a, b), "{how}");
            assert_eq!((sign(&both.develop), sign(&both.history[0].settings), sign(&both.history[1].settings)), (sign(a), sign(b), sign(a)), "{how}");
            assert!(!Arc::ptr_eq(&both.history[0].settings, &both.history[1].settings), "{how}");
            assert_eq!((a.hash64(), b.hash64()), (twin(first).hash64(), twin(second).hash64()), "{how}");
            assert_ne!(a.hash64(), b.hash64());
            assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
        }
    }
}

/// A change's settings that equal the photo's own as numbers but differ in the sign of a zero are not handed
/// the photo's value, in either order.
#[test]
fn changes_never_take_the_photos_settings_across_a_negative_zero() {
    let twin = |tint: f64| {
        edited(|s| {
            s.light.exposure = 0.5;
            s.wb.tint = tint;
        })
    };
    for (own, given) in [(0.0, -0.0), (-0.0, 0.0)] {
        let mut c = catalog_of([photo(1)]);
        c.apply(Op::SetDevelop { id: PhotoId(1), settings: twin(own), label: "Tint".into(), edited: None }).unwrap();
        c.apply(Op::SetHistory { id: PhotoId(1), history: vec![HistoryStep { label: "Tint".into(), settings: twin(given) }] }).unwrap();
        let p = c.photo(PhotoId(1)).unwrap();
        assert!(!Arc::ptr_eq(&p.develop, &p.history[0].settings), "own {own:?}, given {given:?}");
        assert_eq!(p.history[0].settings.wb.tint.is_sign_negative(), given.is_sign_negative());
        assert_eq!(p.develop.wb.tint.is_sign_negative(), own.is_sign_negative());
    }
}

/// Settings given by any change are shared: a history step pushed or set, and versions.
#[test]
fn settings_given_by_changes_are_shared() {
    let look = || edited(|s| s.light.exposure = 0.7);
    let mut c = catalog_of((1..=2).map(photo));
    c.apply(Op::SetDevelop { id: PhotoId(1), settings: look(), label: "Exposure".into(), edited: None }).unwrap();
    c.apply(Op::PushHistory { id: PhotoId(1), step: HistoryStep { label: "Exposure".into(), settings: look() } }).unwrap();
    let p = c.photo(PhotoId(1)).unwrap();
    assert!(Arc::ptr_eq(&p.develop, &p.history[0].settings), "PushHistory");
    c.apply(Op::SetVersions { id: PhotoId(2), versions: vec![Version { name: "A".into(), created: "2026".into(), settings: look(), auto: false }] })
        .unwrap();
    assert!(Arc::ptr_eq(&c.photo(PhotoId(2)).unwrap().versions[0].settings, settings_of(&c, 1)), "SetVersions");
    c.apply(Op::SetHistory { id: PhotoId(2), history: vec![HistoryStep { label: "A".into(), settings: look() }] }).unwrap();
    assert!(Arc::ptr_eq(&c.photo(PhotoId(2)).unwrap().history[0].settings, settings_of(&c, 1)), "SetHistory");
    // a new photo's settings are the shared defaults from the start
    assert!(Arc::ptr_eq(&photo(7).develop, &photo(8).develop));
}

/// Settings given by a change are shared with the photo's own settings even when the thread's list of recent
/// values has since moved on to others: the target photo's settings are what they are compared with first.
#[test]
fn settings_given_by_changes_are_shared_with_the_photos_own_even_when_not_recent() {
    let look = || edited(|s| s.light.exposure = 0.7);
    let mut c = catalog_of((1..=2).map(photo));
    c.apply(Op::SetDevelop { id: PhotoId(1), settings: look(), label: "Exposure".into(), edited: None }).unwrap();
    // more different values than the thread remembers, held by another photo
    let others = |c: &mut Catalog| {
        let versions = (1..=6u32)
            .map(|i| Version {
                name: format!("V{i}"),
                created: "2026".into(),
                settings: edited(|s| s.light.exposure = f64::from(i) / 10.0 + 1.0),
                auto: false,
            })
            .collect();
        c.apply(Op::SetVersions { id: PhotoId(2), versions }).unwrap();
    };
    others(&mut c);
    c.apply(Op::SetHistory { id: PhotoId(1), history: vec![HistoryStep { label: "A".into(), settings: look() }] }).unwrap();
    let p = c.photo(PhotoId(1)).unwrap();
    assert!(Arc::ptr_eq(&p.develop, &p.history[0].settings), "SetHistory");
    others(&mut c);
    c.apply(Op::PushHistory { id: PhotoId(1), step: HistoryStep { label: "B".into(), settings: look() } }).unwrap();
    let p = c.photo(PhotoId(1)).unwrap();
    assert!(Arc::ptr_eq(&p.develop, &p.history[1].settings), "PushHistory");
    others(&mut c);
    c.apply(Op::SetVersions { id: PhotoId(1), versions: vec![Version { name: "C".into(), created: "2026".into(), settings: look(), auto: false }] })
        .unwrap();
    let p = c.photo(PhotoId(1)).unwrap();
    assert!(Arc::ptr_eq(&p.develop, &p.versions[0].settings), "SetVersions");
}

/// Settings that can't be read are still an error naming what is wrong, not shared defaults.
#[test]
fn bad_settings_are_still_refused() {
    for bad in [r#"{"light":{"exposure":"a lot"}}"#, r#"["x"]"#, r#""x""#, r#"7"#, r#"null"#] {
        let record = format!(
            r#"{{"id":1,"source":{{"type":"file","path":"/a.jpg"}},"file_name":"a.jpg","format":"JPEG","width":1,"height":1,"imported":"2026","develop":{bad}}}"#
        );
        assert!(serde_json::from_str::<Photo>(&record).is_err(), "{bad}");
    }
}

/// What a thread remembers to compare with keeps no settings alive, and is a handful of values:
/// settings no photo holds any more are freed, however many were seen.
#[test]
fn remembered_settings_are_not_kept_alive() {
    let mut watched = Vec::new();
    for k in 0..50 {
        let s = shared::settings(edited(|s| s.light.exposure = 0.001 * f64::from(k + 1)));
        watched.push(Arc::downgrade(&s));
    }
    assert!(watched.iter().all(|w| w.upgrade().is_none()), "{} kept alive", watched.iter().filter(|w| w.upgrade().is_some()).count());
    // and of values that are all still held (a library of photos each edited differently), it
    // remembers a handful, not all of them
    let held: Vec<_> = (1..=200).map(|k| shared::settings(edited(|s| s.light.contrast = f64::from(k)))).collect();
    assert!(shared::remembered() <= 4, "{} remembered", shared::remembered());
    drop(held);
    // while they are held, an equal value is still found
    let held = shared::settings(edited(|s| s.light.exposure = 0.9));
    assert!(Arc::ptr_eq(&held, &shared::settings(edited(|s| s.light.exposure = 0.9))));
}

/// New photos start from the one held default.
#[test]
fn a_new_photos_settings_are_the_held_default() {
    assert!(Arc::ptr_eq(&photo(7).develop, &shared::default_settings()));
}

/// A negative zero anywhere in the settings, in whatever field it is, keeps them from being
/// shared with the equal settings that hold a positive one.
#[test]
fn a_negative_zero_in_any_field_is_found() {
    fn zeros(v: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match v {
            serde_json::Value::Number(n) if n.is_f64() && n.as_f64() == Some(0.0) => out.push(path.clone()),
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    path.push(k.clone());
                    zeros(v, path, out);
                    path.pop();
                }
            }
            serde_json::Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    path.push(i.to_string());
                    zeros(v, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let default = serde_json::to_value(DevelopSettings::default()).unwrap();
    let mut found = Vec::new();
    zeros(&default, &mut Vec::new(), &mut found);
    assert!(found.len() > 20, "{} zeros in the defaults: the walk finds none", found.len());
    let base = shared::settings(Arc::new(DevelopSettings::default()));
    let mut checked = 0;
    for path in found {
        let mut v = default.clone();
        let pointer = path.iter().map(|k| format!("/{k}")).collect::<String>();
        *v.pointer_mut(&pointer).unwrap() = serde_json::Value::Number(serde_json::Number::from_f64(-0.0).unwrap());
        let twin: DevelopSettings = serde_json::from_value(v).unwrap();
        if twin.hash64() == DevelopSettings::default().hash64() {
            continue; // (a zero the settings don't write back: nothing to tell apart)
        }
        assert_eq!(twin, DevelopSettings::default(), "{path:?}");
        checked += 1;
        let twin = Arc::new(twin);
        let shared = shared::settings(twin.clone());
        assert!(Arc::ptr_eq(&shared, &twin) && !Arc::ptr_eq(&shared, &base), "{path:?} was shared with the defaults");
    }
    assert!(checked > 20, "only {checked} negative zeros could be told apart");
}
