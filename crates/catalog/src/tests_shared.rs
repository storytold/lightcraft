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

/// The catalog through a saved library and back: in segments, in one file, and from the log alone.
fn reloaded(c: &Catalog) -> Vec<(&'static str, Catalog)> {
    let mut out = Vec::new();
    for (how, single_file) in [("segments", false), ("one file", true)] {
        let m = MemStore { single_file, ..Default::default() };
        let (mut j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
        j.policy.segment_photos = 4;
        j.snapshot(c).unwrap();
        drop(j);
        out.push((how, Journal::open(Box::new(m)).unwrap().1));
    }
    let m = MemStore::new();
    let log: String =
        c.photos().enumerate().map(|(k, p)| encode_record(k as u64 + 1, &Op::AddPhoto { photo: Box::new(p.as_ref().clone()) }) + "\n").collect();
    m.set(journal::LOG, log.into_bytes());
    out.push(("the log", Journal::open(Box::new(MemStore { single_file: true, ..m })).unwrap().1));
    out
}

/// Given photos nobody edited, when they are added to a catalog or a saved library is opened,
/// then they all hold the same settings value, which is the defaults.
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
        assert!(distinct(&back) <= 3, "{how}: {} values (one per reading thread at most)", distinct(&back));
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
    // a caller that changes a photo's settings in place gets a copy of its own to change
    let mut own = c.photo(PhotoId(1)).unwrap().as_ref().clone();
    Arc::make_mut(&mut own.develop).light.contrast = 30.0;
    assert_eq!(**settings_of(&c, 2), DevelopSettings::default());
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

/// Settings saved without anything stated (`{}`: every library's unedited photos) and the
/// defaults of this build are told apart where they differ (a newer default rendering process),
/// and each is held once.
#[test]
fn settings_with_nothing_stated_are_what_they_read_as() {
    let nothing: DevelopSettings = serde_json::from_str("{}").unwrap();
    let mut photos: Vec<Photo> = (1..=6).map(photo).collect();
    for p in photos.iter_mut().take(3) {
        p.develop = Arc::new(nothing.clone());
    }
    let c = catalog_of(photos);
    for (how, back) in reloaded(&c) {
        assert_eq!(**settings_of(&back, 1), nothing, "{how}");
        assert_eq!(**settings_of(&back, 6), DevelopSettings::default(), "{how}");
        assert!(Arc::ptr_eq(settings_of(&back, 1), settings_of(&back, 3)) && Arc::ptr_eq(settings_of(&back, 4), settings_of(&back, 6)), "{how}");
        assert_eq!(back.to_snapshot(), c.to_snapshot(), "{how}");
    }
}

/// Settings that can't be read are still an error naming what is wrong, not shared defaults.
#[test]
fn bad_settings_are_still_refused() {
    for bad in [r#"{"light":{"exposure":"a lot"}}"#, r#"["x"]"#, r#""x""#, r#"7"#, r#"null"#, r#"{"crop":{"geometry":{"angle":1}}}"#] {
        let record = format!(
            r#"{{"id":1,"source":{{"type":"file","path":"/a.jpg"}},"file_name":"a.jpg","format":"JPEG","width":1,"height":1,"imported":"2026","develop":{bad}}}"#
        );
        assert!(serde_json::from_str::<Photo>(&record).is_err(), "{bad}");
    }
    // settings as a list of their fields (nothing writes that) read as before
    let listed: DevelopSettings = serde_json::from_str("[1,2]").unwrap();
    let record = r#"{"id":1,"source":{"type":"file","path":"/a.jpg"},"file_name":"a.jpg","format":"JPEG","width":1,"height":1,"imported":"2026","develop":[1,2]}"#;
    assert_eq!(*serde_json::from_str::<Photo>(record).unwrap().develop, listed);
    // and settings that state nothing but unknown fields read as before: as if nothing was stated
    let record = r#"{"id":1,"source":{"type":"file","path":"/a.jpg"},"file_name":"a.jpg","format":"JPEG","width":1,"height":1,"imported":"2026","develop":{"from_the_future":1}}"#;
    let old: DevelopSettings = serde_json::from_str(r#"{"from_the_future":1}"#).unwrap();
    assert_eq!(*serde_json::from_str::<Photo>(record).unwrap().develop, old);
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
    assert!(Arc::ptr_eq(&photo(7).develop, &shared::default_settings()));
}

/// Settings saved with nothing stated are exactly what `{}` reads as (the first rendering
/// process, whatever the default one is), in a photo and in its history.
#[test]
fn nothing_stated_is_the_first_process() {
    let record = r#"{"id":1,"source":{"type":"file","path":"/a.jpg"},"file_name":"a.jpg","format":"JPEG","width":1,"height":1,"imported":"2026","develop":{},"history":[{"label":"x","settings":{}}]}"#;
    let p: Photo = serde_json::from_str(record).unwrap();
    let nothing: DevelopSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(serde_json::to_string(p.develop.as_ref()).unwrap(), serde_json::to_string(&nothing).unwrap());
    assert!(p.develop.process.is_legacy() && p.history[0].settings.process.is_legacy());
    assert!(Arc::ptr_eq(&p.develop, &p.history[0].settings));
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
