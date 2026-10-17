//! Video import facts and poster thumbnails (M14.2 import side, M14.3) on synthetic H.264 clips
//! made in-test (filmcraft-h264enc + Mp4Writer, as `crates/video/tests/clips.rs` does).

use std::path::{Path, PathBuf};

use lcc_catalog::{MediaKind, Op};
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-video-posters-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

use lcc_video::test_util::write_clip;

fn mean(img: &lcc_raster::Rgba8) -> f64 {
    let n = img.data.len().max(1) as f64;
    img.data.iter().map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])).sum::<f64>() / (3.0 * n)
}

fn import_one(s: &mut Session, path: &Path) -> lcc_catalog::PhotoId {
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    s.catalog.photos().next().unwrap().id
}

#[test]
fn import_fills_video_meta_dims_date_and_gps_in_one_undo_step() {
    let dir = temp_dir("import");
    let clip = dir.join("clip.mp4");
    write_clip(&clip, 64, 48, 30, |_| 120, &[("©day", "2026-05-01T10:20:30+02:00"), ("©xyz", "+48.8584+002.2945/")]);
    let mut s = Session::new().with_fs();
    let id = import_one(&mut s, &clip);
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.kind, MediaKind::Video);
    assert_eq!((p.width, p.height), (64, 48));
    let v = p.video.as_ref().expect("video meta");
    assert_eq!((v.width, v.height, v.codec.as_str(), v.has_audio), (64, 48, "h264", false));
    assert!((v.duration - 1.0).abs() < 0.05, "{}", v.duration);
    assert_eq!(v.fps(), Some(30.0));
    assert!(p.duration.is_some_and(|d| (d - 1.0).abs() < 0.05));
    assert_eq!(p.captured.as_deref(), Some("2026-05-01T10:20:30+02:00"));
    let (la, lo) = p.meta.gps.expect("gps");
    assert!((la - 48.8584).abs() < 1e-3 && (lo - 2.2945).abs() < 1e-3);
    // the whole-file hash, streamed: same as hashing the bytes at once (duplicates and Copy verification)
    let bytes = std::fs::read(&clip).unwrap();
    assert_eq!(p.content_hash.as_deref(), Some(lcc_preview::hash_bytes(&bytes).to_string().as_str()));
    assert_eq!(p.sha1.as_deref(), Some(lcc_hash::sha1_bytes(&bytes).to_hex().as_str()));
    // a byte-identical copy is a duplicate by content
    std::fs::copy(&clip, dir.join("copy.mp4")).unwrap();
    let r = s.execute("library.import", &json!({"paths": [dir.join("copy.mp4").to_string_lossy()]})).unwrap();
    assert_eq!(r["duplicates"][0]["reason"], "content", "{r}");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn video_dates_normalise_to_the_catalog_form() {
    assert_eq!(crate::files::video_date("2026-05-01T10:20:30Z").as_deref(), Some("2026-05-01T10:20:30+00:00"));
    assert_eq!(crate::files::video_date("2026-05-01T10:20:30+0200").as_deref(), Some("2026-05-01T10:20:30+02:00"));
    assert_eq!(crate::files::video_date("2026-05-01").as_deref(), Some("2026-05-01T00:00:00"));
    assert_eq!(crate::files::video_date("garbage"), None);
    assert_eq!(crate::files::video_date(""), None);
}

#[test]
fn thumbnail_is_the_graded_poster_and_a_new_poster_invalidates_it() {
    let dir = temp_dir("thumb");
    let clip = dir.join("ramp.mp4");
    // dark first half, bright second half: the default poster (10 %) is dark
    write_clip(&clip, 64, 48, 30, |k| if k < 15 { 60 } else { 200 }, &[]);
    let mut s = Session::new().with_fs();
    let id = import_one(&mut s, &clip);
    let thumb = |s: &mut Session| {
        let job = s.thumb_job(id, 256).unwrap();
        assert!(matches!(job.source, crate::media::SourceRef::Video { .. } | crate::media::SourceRef::Loaded(_)));
        let r = job.run();
        s.accept(&r);
        r.rendered.unwrap().image
    };
    let base = thumb(&mut s);
    assert_eq!(base.width * 3, base.height * 4, "the clip's aspect");
    let base_mean = mean(&base);

    // graded: +1 EV brightens the poster
    let mut d = (*s.catalog.photo(id).unwrap().develop).clone();
    d.light.exposure = 1.0;
    s.commit("Exposure", Op::SetDevelop { id, settings: std::sync::Arc::new(d), label: "Exposure".into(), edited: None }).unwrap();
    let brighter = mean(&thumb(&mut s));
    assert!(brighter > base_mean + 10.0, "{base_mean} → {brighter}");
    s.execute("edit.undo", &json!({})).unwrap();

    // a poster in the bright half: a new key, a new (brighter) thumbnail
    let k0 = s.thumb_job(id, 256).unwrap().cache.unwrap().1;
    s.commit("Set Poster Frame", Op::SetVideoPoster { id, time: Some(0.8) }).unwrap();
    let k1 = s.thumb_job(id, 256).unwrap().cache.unwrap().1;
    assert_ne!(k0, k1);
    let later = mean(&thumb(&mut s));
    assert!(later > base_mean + 20.0, "{base_mean} → {later}");
    // the loupe shows the same graded poster frame
    let loupe = s.loupe_job(id, 400, 300, true).unwrap().run().rendered.unwrap().image;
    assert!((mean(&loupe) - later).abs() < 8.0, "{} vs {later}", mean(&loupe));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_clip_imports_and_shows_a_placeholder() {
    let dir = temp_dir("corrupt");
    let clip = dir.join("broken.mov");
    std::fs::write(&clip, b"\0\0\0\x18ftypqt  garbage that is not a movie").unwrap();
    let mut s = Session::new().with_fs();
    let id = import_one(&mut s, &clip);
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.kind, MediaKind::Video);
    assert!(p.video.is_none());
    let r = s.thumb_job(id, 256).unwrap().run();
    let img = r.rendered.expect("a placeholder, not an error").image;
    assert!(img.width >= 16 && img.height >= 9);
    // a truncated real clip too
    let good = dir.join("good.mp4");
    write_clip(&good, 64, 48, 10, |_| 100, &[]);
    let bytes = std::fs::read(&good).unwrap();
    std::fs::write(dir.join("cut.mp4"), &bytes[..bytes.len() / 2]).unwrap();
    assert!(crate::media::video_poster(&dir.join("cut.mp4").to_string_lossy(), None, 128).is_ok());
    // a missing file is an error (offline), not a placeholder
    assert!(crate::media::video_poster(&dir.join("gone.mp4").to_string_lossy(), None, 128).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
