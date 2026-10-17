//! Catalog format 10: video clips (`Photo.video`, `Op::SetVideoMeta` / `SetVideoTrim` / `SetVideoPoster`).

use crate::journal::{Journal, SNAPSHOT};
use crate::query::{Filter, Sort};
use crate::rules::RuleSet;
use crate::{Catalog, MediaKind, MemStore, Op, Photo, PhotoId, Source, VideoMeta};

fn meta() -> VideoMeta {
    VideoMeta {
        duration: 10.0,
        fps_num: 30000,
        fps_den: 1001,
        width: 1920,
        height: 1080,
        codec: "h264".into(),
        has_audio: true,
        trim_in: 0.0,
        trim_out: None,
        poster_time: None,
    }
}

fn clip(c: &mut Catalog) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/v/clip.mp4".into() }, "clip.mp4", "MP4", 1920, 1080, "2026-10-12T00:00:00");
    p.kind = MediaKind::Video;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn roundtrip(op: Op) -> Op {
    serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap()
}

#[test]
fn video_meta_trim_poster_apply_and_undo() {
    let mut c = Catalog::default();
    let id = clip(&mut c);
    assert!(c.apply(Op::SetVideoTrim { id, trim_in: 1.0, trim_out: None }).is_err(), "no video meta yet");
    let undo_meta = c.apply(roundtrip(Op::SetVideoMeta { id, meta: Some(meta()) })).unwrap();
    let undo_trim = c.apply(roundtrip(Op::SetVideoTrim { id, trim_in: 1.5, trim_out: Some(8.0) })).unwrap();
    let undo_poster = c.apply(roundtrip(Op::SetVideoPoster { id, time: Some(4.0) })).unwrap();
    let v = c.photo(id).unwrap().video.clone().unwrap();
    assert_eq!((v.trim_in, v.trim_out, v.poster_time), (1.5, Some(8.0), Some(4.0)));
    assert_eq!(v.trim_end(), 8.0);
    assert_eq!(v.poster(), 4.0);
    // undo, newest first; each inverse redoes
    let redo_poster = c.apply(roundtrip(undo_poster)).unwrap();
    assert_eq!(c.photo(id).unwrap().video.as_ref().unwrap().poster(), 1.0, "default: 10 % in");
    let redo_trim = c.apply(roundtrip(undo_trim)).unwrap();
    assert_eq!(c.photo(id).unwrap().video.as_ref().unwrap().trim_end(), 10.0);
    c.apply(undo_meta).unwrap();
    assert!(c.photo(id).unwrap().video.is_none());
    c.apply(Op::SetVideoMeta { id, meta: Some(meta()) }).unwrap();
    c.apply(redo_trim).unwrap();
    c.apply(redo_poster).unwrap();
    assert_eq!(c.photo(id).unwrap().video.as_ref().unwrap().poster_time, Some(4.0));
}

#[test]
fn bad_video_values_are_errors_and_change_nothing() {
    let mut c = Catalog::default();
    let id = clip(&mut c);
    c.apply(Op::SetVideoMeta { id, meta: Some(meta()) }).unwrap();
    for (i, o) in [(-1.0, None), (5.0, Some(5.0)), (6.0, Some(2.0)), (0.0, Some(11.0)), (f64::NAN, None), (0.0, Some(f64::INFINITY)), (10.0, None)] {
        assert!(c.apply(Op::SetVideoTrim { id, trim_in: i, trim_out: o }).is_err(), "{i} {o:?}");
    }
    for t in [-0.1, 10.5, f64::NAN, f64::NEG_INFINITY] {
        assert!(c.apply(Op::SetVideoPoster { id, time: Some(t) }).is_err(), "{t}");
    }
    let mut bad = meta();
    bad.duration = f64::NAN;
    assert!(c.apply(Op::SetVideoMeta { id, meta: Some(bad) }).is_err());
    let mut bad = meta();
    bad.poster_time = Some(20.0);
    assert!(c.apply(Op::SetVideoMeta { id, meta: Some(bad) }).is_err());
    assert!(c.apply(Op::SetVideoPoster { id: PhotoId(999), time: None }).is_err());
    assert_eq!(c.photo(id).unwrap().video, Some(meta()));
    // a zero-fps clip has no rate, not a division by zero
    let mut z = meta();
    z.fps_den = 0;
    assert_eq!(z.fps(), None);
    assert!((meta().fps().unwrap() - 29.97).abs() < 0.01);
}

/// A format 9 snapshot (no `video` anywhere) loads with `video: None` and is upgraded to 10; a clip's
/// meta survives a reload.
#[test]
fn format_nine_upgrades_and_video_persists() {
    let mut base = Catalog::default();
    let id = clip(&mut base);
    let m = MemStore::default();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":9,\"seq\":1,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    let (mut j, mut c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(r.upgraded_from, Some(9));
    assert!(c.photo(id).unwrap().video.is_none());
    let op = Op::SetVideoMeta { id, meta: Some(meta()) };
    c.apply(op.clone()).unwrap();
    j.append(&[op]).unwrap();
    drop(j);
    let (_, c2, _) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(c2.photo(id).unwrap().video, Some(meta()));
}

#[test]
fn video_filter_and_rule() {
    let mut c = Catalog::default();
    let v = clip(&mut c);
    let still = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(still, Source::Demo { scene: 0 }, "a.jpg", "JPEG", 4, 3, "2026-10-12")) }).unwrap();
    for (text, want) in [("video:yes", vec![v]), ("kind:video", vec![v]), ("video:no", vec![still])] {
        let f = Filter { text: text.into(), ..Default::default() };
        assert_eq!(c.query(&f, &Sort::default()), want, "{text}");
    }
    let rules: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "kind", "op": "is", "value": "video"}]})).unwrap();
    let p = c.photo(v).unwrap();
    assert!(rules.matches(p, &c));
    assert!(!rules.matches(c.photo(still).unwrap(), &c));
}
