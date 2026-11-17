//! Catalogued videos render their poster frame ([`crate::files::VideoFrames`]): thumbnail and
//! loupe jobs exist once a video loader is installed, every other job keeps refusing a clip, and
//! the real loader decodes a clip made here (with the codecs' `video-frames` feature) or says
//! why it can't.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};

use crate::Session;
use crate::media::{FileLoader, SourceRef};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-video-frames-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn add_clip(s: &mut Session, id: u64, path: &Path) {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let mut p = Photo::new(PhotoId(id), Source::File { path: path.to_string_lossy().into() }, &name, "MP4", 64, 48, "2026-11-16T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(3.0);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

/// A loader that counts its calls and answers with a flat picture, or with `fail`.
fn fake_loader(calls: Arc<AtomicUsize>, fail: Option<&'static str>) -> FileLoader {
    Arc::new(move |_path: &str, max_edge: usize, _with| {
        calls.fetch_add(1, Ordering::Relaxed);
        if let Some(why) = fail {
            return Err(why.into());
        }
        let (w, h) = (64.min(max_edge), 48.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.2, 0.3, 0.4]; w * h] }, Default::default()))
    })
}

#[test]
fn without_a_video_loader_a_clip_gets_no_jobs() {
    let mut s = Session::new();
    add_clip(&mut s, 1, Path::new("/lc-video-frames-test/clip.mp4"));
    assert!(s.thumb_job(PhotoId(1), 256).is_none());
    assert!(s.render_job(PhotoId(1), 1024, 1024, false, true).is_none());
}

#[test]
fn a_video_loader_gives_clips_thumbnail_and_loupe_jobs_and_nothing_else() {
    let mut s = Session::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let loader = fake_loader(calls.clone(), None);
    s.media.video_loader = Some(loader.clone());
    // the still-image loaders must never see a clip
    let still_calls = Arc::new(AtomicUsize::new(0));
    s.media.file_loader = Some(fake_loader(still_calls.clone(), Some("an image loader read a clip")));
    add_clip(&mut s, 1, Path::new("/lc-video-frames-test/clip.mp4"));
    let thumb = s.thumb_job(PhotoId(1), 256).expect("a thumbnail job");
    match &thumb.source {
        SourceRef::File { loader: Some(l), reduced, denoise, camera_profile, outliers, .. } => {
            assert!(Arc::ptr_eq(l, &loader), "the video loader");
            assert!(reduced.is_none() && denoise.is_none() && camera_profile.is_none() && !outliers);
        }
        _ => panic!("a file source"),
    }
    assert!(thumb.cache.is_some(), "cached like a photo's thumbnail");
    let r = thumb.run();
    let img = r.rendered.expect("rendered").image;
    assert_eq!(img.width * 3, img.height * 4, "the poster's 4:3 frame");
    let loupe = s.render_job(PhotoId(1), 1024, 1024, false, true).expect("a loupe job");
    assert!(loupe.run().rendered.is_ok());
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(still_calls.load(Ordering::Relaxed), 0);
    // a preset hovered in the loupe renders the clip's poster too, never with an image loader
    let mut settings = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
    settings.profile.id = "Adobe Standard".into();
    let hover = s.preview_job(PhotoId(1), 512, 512, true, &settings).expect("a hover job");
    assert!(matches!(&hover.source, SourceRef::File { loader: Some(l), .. } if Arc::ptr_eq(l, &loader)));
    assert!(hover.run().rendered.is_ok());
    assert_eq!(still_calls.load(Ordering::Relaxed), 0);
    // variants, face tiles, samples and exports still refuse a clip
    assert!(s.variant_job(PhotoId(1), &settings, 128).is_none());
    assert!(s.face_job(PhotoId(1), lightcraft_geom::Rect::new(0.2, 0.2, 0.5, 0.5), 64).is_none());
    let export = s.export_job(PhotoId(1), 1024, 1024, Default::default(), Default::default());
    assert_eq!(export.err().as_deref(), Some("a video has no frame to export"));
}

#[test]
fn a_video_poster_has_its_own_cache_key() {
    let mut s = Session::new();
    s.media.video_loader = Some(fake_loader(Arc::default(), None));
    add_clip(&mut s, 1, Path::new("/lc-video-frames-test/clip.mp4"));
    let video = s.thumb_job(PhotoId(1), 256).unwrap().cache.unwrap().1;
    // the same file catalogued as a still: another key (a poster version bump re-renders videos only)
    let mut p = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    p.kind = MediaKind::Image;
    p.duration = None;
    s.media.file_loader = Some(fake_loader(Arc::default(), None));
    let video_again = s.thumb_job(PhotoId(1), 256).unwrap().cache.unwrap().1;
    assert_eq!(video, video_again, "a key follows its inputs");
    p.id = PhotoId(2);
    p.format = "JPEG".into();
    p.file_name = "clip.jpg".into();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    assert!(!s.catalog.photo(PhotoId(2)).unwrap().is_video());
    let still = s.thumb_job(PhotoId(2), 256).unwrap().cache.unwrap().1;
    assert_ne!(video, still);
}

#[test]
fn a_failed_poster_says_why() {
    let mut s = Session::new();
    s.media.video_loader = Some(fake_loader(Arc::default(), Some("Apple ProRes video can't be decoded yet")));
    add_clip(&mut s, 1, Path::new("/lc-video-frames-test/clip.mov"));
    let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
    assert_eq!(r.rendered.err().as_deref(), Some("Apple ProRes video can't be decoded yet"));
}

/// An MP4 of `frames` flat grey H.264 pictures at 30 fps (`luma` rising per picture).
fn h264_mp4(frames: usize) -> Vec<u8> {
    use craft_media::isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
    let (w, h) = (64usize, 48usize);
    let mut cfg = EncoderConfig::new(w as u32, h as u32, 30, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.rate = RateControl::Qp(20);
    cfg.bframes = 0;
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    for i in 0..frames {
        let y = vec![(20 + 2 * i).min(230) as u8; w * h];
        let c = vec![128u8; w * h / 4];
        packets.extend(enc.encode(&YuvFrame { y: &y, u: &c, v: &c, y_stride: w, uv_stride: w / 2 }, i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let entry = SampleEntry::avc(AvcConfig::parse(&enc.avcc()).unwrap(), w as u16, h as u16);
    let mut m = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = m.add_track(TrackConfig::new(entry, 30_000)).unwrap();
    for p in &packets {
        m.write_sample(t, WriteSample { data: &p.data, duration: 1000, composition_offset: 0, is_sync: p.keyframe }).unwrap();
    }
    m.finish_faststart().unwrap().into_inner()
}

#[test]
fn the_file_loader_decodes_a_video_poster_with_one_activity_row() {
    let dir = temp_dir("loader");
    let path = dir.join("clip.mp4");
    std::fs::write(&path, h264_mp4(90)).unwrap();
    let mut s = Session::new().with_fs();
    add_clip(&mut s, 1, &path);
    if !lightcraft_codecs::video_stream::video_decoding_available() {
        lightcraft_testkit::skip!(Feature, "lightcraft-codecs is built without video-frames: no poster decoded");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let job = s.thumb_job(PhotoId(1), 256).expect("with_fs installs the video loader");
    let r = job.run();
    assert!(s.activity.list().is_empty(), "the row goes when the poster is done");
    let img = r.rendered.expect("a poster").image;
    assert_eq!(img.width * 3, img.height * 4, "the clip's 4:3 frame");
    // the picture one second in (frame 30, luma 80): a mid grey, not the first frame's near black
    let g = img.data[img.height / 2 * img.width + img.width / 2];
    assert!(g[1] > 60 && g[1] < 110, "{g:?}");
    // a damaged clip fails with a reason, and the row is gone too
    std::fs::write(&path, b"\0\0\0\x18ftypisom\0\0\0\0isomavc1").unwrap();
    let r = s.render_job(PhotoId(1), 512, 512, false, true).unwrap().run();
    assert!(r.rendered.is_err());
    assert!(s.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build without video decoding installs no video loader: a clip gets no render jobs, so a
/// preview build skips it instead of counting it failed (500 clips imported must not read as
/// "500 couldn't be rendered"), and nothing renders to fail with a warning.
#[test]
fn a_build_without_video_decoding_gives_clips_no_jobs() {
    let dir = temp_dir("nodecode");
    let path = dir.join("clip.mp4");
    std::fs::write(&path, h264_mp4(10)).unwrap();
    let mut s = Session::new().with_fs();
    add_clip(&mut s, 1, &path);
    let available = lightcraft_codecs::video_stream::video_decoding_available();
    assert_eq!(s.media.video_loader.is_some(), available);
    assert_eq!(s.thumb_job(PhotoId(1), 256).is_some(), available);
    assert_eq!(s.loupe_job(PhotoId(1), 512, 512, true).is_some(), available);
    let r = s.execute("library.buildPreviews", &serde_json::json!({"ids": [1], "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["failed"].as_u64(), Some(0), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A poster the activity row's ✕ stopped is not a failed preview: the photo is just not built.
#[test]
fn a_stopped_video_poster_is_not_a_failed_preview() {
    let mut s = Session::new();
    s.media.video_loader = Some(fake_loader(Arc::default(), Some(crate::files::VIDEO_STOPPED)));
    add_clip(&mut s, 1, Path::new("/lc-video-frames-test/clip.mp4"));
    let r = s.execute("library.buildPreviews", &serde_json::json!({"ids": [1], "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["failed"].as_u64(), Some(0), "{r}");
    // a real failure still counts
    s.media.video_loader = Some(fake_loader(Arc::default(), Some("Apple ProRes video can't be decoded yet")));
    let r = s.execute("library.buildPreviews", &serde_json::json!({"ids": [1], "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["failed"].as_u64(), Some(1), "{r}");
}

/// The real loader puts a decode in the activity row from the start, while it still waits for
/// memory, and the row's ✕ stops it there: the load fails as stopped, and the next decode gets a
/// fresh row that is not stopped.
#[test]
fn the_video_loader_shows_its_row_and_its_cancel_stops_the_decode() {
    if !lightcraft_codecs::video_stream::video_decoding_available() {
        lightcraft_testkit::skip!(Feature, "lightcraft-codecs is built without video-frames: no poster decoded");
        return;
    }
    let dir = temp_dir("row");
    let path = dir.join("clip.mp4");
    std::fs::write(&path, h264_mp4(40)).unwrap();
    let activity = crate::activity::Activity::default();
    let loader = crate::files::VideoFrames::new(activity.clone()).loader();
    // all the working memory held: a background decode waits for it
    let gate = crate::memory::work_gate();
    let held = gate.acquire_urgent(usize::MAX);
    let (l, p) = (loader.clone(), path.to_string_lossy().to_string());
    let decode = std::thread::spawn(move || crate::memory::in_background(|| l(&p, 256, Default::default()).map(|_| ())));
    let start = std::time::Instant::now();
    while activity.list().is_empty() && start.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let rows = activity.list();
    assert_eq!(rows.len(), 1, "the waiting decode is in the row");
    assert_eq!((rows[0].kind, rows[0].total), (crate::files::VIDEO_FRAMES_KIND, 1));
    activity.cancel(rows[0].id).unwrap();
    drop(held);
    assert_eq!(decode.join().unwrap().err().as_deref(), Some(crate::files::VIDEO_STOPPED));
    assert!(activity.list().is_empty());
    // the next decode is not stopped
    assert!(loader(&path.to_string_lossy(), 256, Default::default()).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}
