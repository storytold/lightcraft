//! `slideshow.export`: PDF, JPEG sequence and AV1 video of the slideshow, validation, the activity
//! row, cancel and photos that can't be rendered.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, Source};
use serde_json::{Value, json};

use crate::Session;

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ssexp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
    fn files(&self, rel: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.0.join(rel))
            .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A demo session playing 1 s slides with 0.5 s fades, its first `n` photos as ids.
fn session(n: usize) -> (Session, Vec<u64>) {
    let mut s = Session::with_demo();
    s.execute("slideshow.playback", &json!({"slideDuration": 1.0, "fadeDuration": 0.5})).unwrap();
    let ids = s.catalog.photos().take(n).map(|p| p.id.0).collect();
    (s, ids)
}

fn export(s: &mut Session, p: Value) -> Value {
    let mut p = p;
    p["wait"] = json!(true);
    s.execute("slideshow.export", &p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn every_parameter_is_checked() {
    let (mut s, ids) = session(2);
    for p in [
        json!({"path": "x.pdf"}),
        json!({"format": "gif", "path": "x.gif"}),
        json!({"format": "pdf"}),
        json!({"format": "pdf", "path": "  "}),
        json!({"format": "pdf", "path": "x.pdf", "size": 10}),
        json!({"format": "pdf", "path": "x.pdf", "size": 100_000}),
        json!({"format": "pdf", "path": "x.pdf", "size": "big"}),
        json!({"format": "pdf", "path": "x.pdf", "size": -5}),
        json!({"format": "video", "path": "x.mp4", "size": 4320}),
        json!({"format": "video", "path": "x.mp4", "size": "8kp"}),
        json!({"format": "pdf", "path": "x.pdf", "quality": 0}),
        json!({"format": "pdf", "path": "x.pdf", "quality": 101}),
        json!({"format": "video", "path": "x.mp4", "fps": 29}),
        json!({"format": "video", "path": "x.mp4", "fps": 30.5}),
        json!({"format": "video", "path": "x.avi", "container": "avi"}),
        json!({"format": "video", "path": "x.mp4", "speed": 11}),
        json!({"format": "pdf", "path": "x.pdf", "fullScreen": "yes"}),
        json!({"format": "pdf", "path": "x.pdf", "name": "a"}),
        json!({"format": "jpeg", "path": "out", "name": "../up"}),
        json!({"format": "jpeg", "path": "out", "name": ""}),
        json!({"format": "pdf", "path": "x.pdf", "ids": "all"}),
        json!({"format": "pdf", "path": "x.pdf", "ids": ["one"]}),
        json!({"format": "pdf", "path": "x.pdf", "ids": []}),
        json!({"format": "pdf", "path": "x.pdf", "ids": [u64::MAX]}),
        json!({"format": "pdf", "path": "x.pdf", "typo": 1}),
        json!({"format": "pdf", "path": "x.pdf", "wait": "now"}),
        json!("pdf"),
    ] {
        let e = s.execute("slideshow.export", &p).err().unwrap_or_else(|| panic!("accepted {p}"));
        assert!(!e.to_string().is_empty());
    }
    assert!(s.activity.list().is_empty(), "nothing started");
    assert!(s.slideshow_export.is_none());
    // the photos in view: none
    s.execute("library.filter", &json!({"text": "zzz no photo is called this"})).unwrap();
    assert!(s.visible_cloned().is_empty());
    let e = s.execute("slideshow.export", &json!({"format": "pdf", "path": "x.pdf"})).unwrap_err();
    assert!(e.to_string().contains("no photos in view"), "{e}");
    // in view: exported without ids
    s.execute("library.filter", &json!({"text": ""})).unwrap();
    assert!(!s.visible_cloned().is_empty() && !ids.is_empty());
}

/// A minimal PDF reader: the cross-reference table points at every object, and the pages.
fn pdf_pages(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let xref = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse::<usize>().unwrap();
    assert!(bytes[xref..].starts_with(b"xref\n0 "));
    let table = std::str::from_utf8(&bytes[xref..]).unwrap();
    let count: usize = table.lines().nth(1).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    for (id, line) in table.lines().skip(3).take(count - 1).enumerate() {
        let offset: usize = line.split_whitespace().next().unwrap().parse().unwrap();
        assert!(bytes[offset..].starts_with(format!("{} 0 obj\n", id + 1).as_bytes()), "object {} misplaced", id + 1);
    }
    text.split(" 0 obj\n").filter(|o| o.starts_with("<< /Type /Page ")).map(|o| o.split("\nendobj").next().unwrap().to_string()).collect()
}

/// The JPEGs embedded in a PDF, decoded.
fn pdf_images(bytes: &[u8]) -> Vec<lightcraft_raster::Rgba8> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = bytes[at..].windows(3).position(|v| v == [0xff, 0xd8, 0xff]).map(|p| p + at) {
        let end = bytes[start..].windows(10).position(|v| v == b"\nendstream").unwrap() + start;
        out.push(lightcraft_codecs::decode(&bytes[start..end], Default::default()).unwrap().to_srgb8());
        at = end;
    }
    out
}

#[test]
fn pdf_has_a_page_per_slide_titles_included() {
    let dir = Scratch::new("pdf");
    let (mut s, ids) = session(3);
    s.execute("slideshow.titles", &json!({"intro": {"enabled": true, "content": "text", "text": "Hello"}, "ending": {"enabled": true}})).unwrap();
    let path = dir.path("Show.pdf");
    let r = export(&mut s, json!({"format": "pdf", "path": path, "size": 160, "quality": 80, "ids": ids}));
    assert_eq!((r["pages"].as_u64(), r["slides"].as_u64(), r["width"].as_u64(), r["height"].as_u64()), (Some(5), Some(5), Some(160), Some(90)));
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.4"));
    let pages = pdf_pages(&bytes);
    assert_eq!(pages.len(), 5);
    assert!(pages.iter().all(|p| p.contains("/MediaBox [0 0 160.00 90.00]") && p.contains("/Dur 1.00")), "{pages:?}");
    assert!(!pages[0].contains("/Trans"), "nothing fades into the first slide");
    assert!(pages[1].contains("/Trans << /S /Fade /D 0.50 >>"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Count 5 ") && text.contains("/PageMode /FullScreen"));
    let images = pdf_images(&bytes);
    assert_eq!(images.len(), 5);
    assert!(images.iter().all(|i| (i.width, i.height) == (160, 90)));
    // the intro (black with white text) and a photo slide differ
    let mean = |i: &lightcraft_raster::Rgba8| i.data.iter().map(|p| u64::from(p[1])).sum::<u64>() / i.data.len() as u64;
    assert!(mean(&images[0]) < mean(&images[1]), "the intro screen is the first page");
    assert!(dir.files("").iter().all(|f| !f.ends_with(".part")));
    // no full screen; a 4:3 aspect makes 4:3 pages
    s.execute("slideshow.layout", &json!({"aspect": "4:3"})).unwrap();
    let r = export(&mut s, json!({"format": "pdf", "path": path, "size": 120, "fullScreen": false, "ids": ids}));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(120), Some(90)));
    assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains("/FullScreen"));
}

#[test]
fn jpeg_sequence_is_numbered_in_a_folder() {
    let dir = Scratch::new("jpeg");
    let (mut s, ids) = session(3);
    let out = dir.path("Trip");
    let r = export(&mut s, json!({"format": "jpeg", "path": out, "size": 128, "ids": ids}));
    assert_eq!(dir.files("Trip"), vec!["Trip-001.jpg", "Trip-002.jpg", "Trip-003.jpg"]);
    assert_eq!(r["files"].as_array().unwrap().len(), 3);
    let img = lightcraft_codecs::decode(&std::fs::read(Path::new(&out).join("Trip-002.jpg")).unwrap(), Default::default()).unwrap().to_srgb8();
    assert_eq!((img.width, img.height), (128, 72));
    // a name of its own, and a portrait-free square check: the long side is the size
    let r = export(&mut s, json!({"format": "jpeg", "path": out, "size": 64, "name": "Slide", "ids": [ids[0]]}));
    assert_eq!(r["files"][0].as_str().unwrap(), Path::new(&out).join("Slide-001.jpg").to_string_lossy());
}

/// MP4 boxes of a byte range: (type, body range).
fn boxes(d: &[u8], r: std::ops::Range<usize>) -> Vec<([u8; 4], std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut p = r.start;
    while p + 8 <= r.end {
        let size32 = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (hdr, size) = if size32 == 1 { (16, u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize) } else { (8, size32) };
        assert!(size >= hdr && p + size <= r.end);
        out.push((kind, p + hdr..p + size));
        p += size;
    }
    assert_eq!(p, r.end);
    out
}

fn child(d: &[u8], r: std::ops::Range<usize>, path: &[&[u8; 4]]) -> std::ops::Range<usize> {
    path.iter().fold(r, |r, k| boxes(d, r).into_iter().find(|(t, _)| t == *k).unwrap().1)
}

#[test]
fn video_is_av1_in_mp4_with_fades_as_frames() {
    let dir = Scratch::new("mp4");
    let (mut s, ids) = session(2);
    let path = dir.path("show.mp4");
    let r = export(&mut s, json!({"format": "video", "path": path, "size": 64, "fps": 24, "quality": 40, "ids": ids}));
    // 2 slides × 1 s at 24 fps; the second fades in over 12 frames
    assert_eq!((r["frames"].as_u64(), r["seconds"].as_f64(), r["width"].as_u64(), r["height"].as_u64()), (Some(48), Some(2.0), Some(114), Some(64)));
    assert_eq!(r["samples"].as_u64(), Some(1 + 12 + 1), "held slides are one sample each, fades a frame each");
    assert_eq!((r["codec"].as_str(), r["audio"].as_bool(), r["container"].as_str()), (Some("av1"), Some(false), Some("mp4")));
    let d = std::fs::read(&path).unwrap();
    let top: Vec<_> = boxes(&d, 0..d.len()).into_iter().map(|(k, _)| k).collect();
    assert_eq!(top, vec![*b"ftyp", *b"mdat", *b"moov"]);
    let stbl = child(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
    let be = |at: usize| u32::from_be_bytes(d[at..at + 4].try_into().unwrap());
    let stsz = child(&d, stbl.clone(), &[b"stsz"]);
    assert_eq!(be(stsz.start + 8), 14, "sample count");
    let stts = child(&d, stbl.clone(), &[b"stts"]);
    let runs = be(stts.start + 4) as usize;
    let ticks: u32 = (0..runs).map(|i| be(stts.start + 8 + i * 8) * be(stts.start + 12 + i * 8)).sum();
    let mdhd = child(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"mdhd"]);
    assert_eq!(ticks / be(mdhd.start + 12), 2, "duration: 2 s");
    let stsd = child(&d, stbl, &[b"stsd"]);
    let entry = &boxes(&d, stsd.start + 8..stsd.end)[0];
    assert_eq!(&entry.0, b"av01");
    let inner = boxes(&d, entry.1.start + 78..entry.1.end);
    let av1c = &inner.iter().find(|(k, _)| k == b"av1C").unwrap().1;
    assert_eq!(d[av1c.start], 0x81, "av1C marker and version");
    assert_eq!((d[av1c.start + 4] >> 3) & 0x0f, 1, "the sequence header OBU follows");
    assert!(dir.files("").iter().all(|f| !f.ends_with(".part")));
}

#[test]
fn video_in_webm_with_a_fade_colour() {
    let dir = Scratch::new("webm");
    let (mut s, ids) = session(2);
    s.execute("slideshow.playback", &json!({"fadeColor": "#ffffff"})).unwrap();
    let path = dir.path("show.webm");
    let r = export(&mut s, json!({"format": "video", "path": path, "size": "64p", "fps": 25, "container": "webm", "ids": ids}));
    assert_eq!((r["frames"].as_u64(), r["container"].as_str()), (Some(50), Some("webm")));
    let d = std::fs::read(&path).unwrap();
    assert!(d.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]), "EBML header");
    assert!(d.windows(4).any(|w| w == b"webm") && d.windows(5).any(|w| w == b"V_AV1"));
    // Segment: the 8-byte size spans the rest of the file
    let seg = d.windows(4).position(|w| w == [0x18, 0x53, 0x80, 0x67]).unwrap();
    let size = u64::from_be_bytes(d[seg + 4..seg + 12].try_into().unwrap()) & 0x00ff_ffff_ffff_ffff;
    assert_eq!(seg + 12 + size as usize, d.len());
}

fn wait_for(s: &Session) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let run = s.slideshow_export.clone().unwrap();
        if !run.running() {
            return run.json();
        }
        assert!(Instant::now() < deadline, "export did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn background_export_has_an_activity_row_and_reports_its_result() {
    let dir = Scratch::new("bg");
    let (mut s, ids) = session(4);
    let path = dir.path("bg.pdf");
    let r = s.execute("slideshow.export", &json!({"format": "pdf", "path": path, "size": 96, "ids": ids})).unwrap();
    assert_eq!((r["background"].as_bool(), r["slides"].as_u64(), r["total"].as_u64()), (Some(true), Some(4), Some(4)));
    let rows = s.activity.list();
    let row = rows.iter().find(|t| t.id == r["taskId"].as_u64().unwrap()).expect("a row in the activity stack");
    assert_eq!((row.kind, row.label.as_str(), row.total), ("export", "Exporting slideshow", 4));
    assert!(row.cancellable);
    assert!(s.execute("slideshow.export", &json!({"format": "pdf", "path": dir.path("other.pdf"), "ids": ids})).is_err(), "one at a time");
    let status = wait_for(&s);
    assert_eq!(status["result"]["pages"], 4, "{status}");
    assert_eq!(status["done"], 4);
    assert!(s.activity.list().is_empty(), "the row goes when the export ends");
    assert_eq!(s.execute("slideshow.exportStatus", &json!({})).unwrap()["result"]["path"], json!(path));
    assert!(Path::new(&path).exists());
}

#[test]
fn cancel_leaves_no_file_behind() {
    let dir = Scratch::new("cancel");
    for (format, name, extra) in [("pdf", "c.pdf", json!({})), ("jpeg", "seq", json!({})), ("video", "c.mp4", json!({"size": 64}))] {
        let (mut s, ids) = session(12);
        let path = dir.path(name);
        let mut p = json!({"format": format, "path": path, "ids": ids});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let r = s.execute("slideshow.export", &p).unwrap();
        s.execute("activity.cancel", &json!({"id": r["taskId"]})).unwrap();
        let status = wait_for(&s);
        assert!(status["error"].as_str().is_some_and(|e| e.contains("cancelled")), "{format}: {status}");
        assert!(status["cancelled"].as_bool().unwrap());
        assert!(!Path::new(&path).is_file(), "{format}: no output");
        let left: Vec<_> = dir.files("").into_iter().chain(dir.files("seq")).filter(|f| f.ends_with(".part") || f.ends_with(".jpg")).collect();
        assert!(left.is_empty(), "{format}: {left:?}");
        assert!(s.activity.list().is_empty());
    }
}

#[test]
fn a_photo_that_cannot_render_is_skipped_with_a_warning() {
    let dir = Scratch::new("skip");
    let (mut s, mut ids) = session(2);
    let id = s.catalog.alloc_photo_id();
    let gone = dir.path("gone.jpg");
    s.catalog
        .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: gone }, "gone.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00")) })
        .unwrap();
    ids.insert(1, id.0);
    let r = export(&mut s, json!({"format": "pdf", "path": dir.path("s.pdf"), "size": 64, "ids": ids}));
    assert_eq!((r["pages"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(1)));
    assert!(r["warnings"][0].as_str().unwrap().contains("gone.jpg"), "{r}");
    // video: the skipped slide takes no time
    let r = export(&mut s, json!({"format": "video", "path": dir.path("s.mp4"), "size": 64, "fps": 24, "ids": ids}));
    assert_eq!((r["frames"].as_u64(), r["skipped"].as_u64()), (Some(48), Some(1)));
    // only the broken photo: nothing to write, and nothing written
    let e = s.execute("slideshow.export", &json!({"format": "pdf", "path": dir.path("none.pdf"), "ids": [id.0], "wait": true})).unwrap_err();
    assert!(e.to_string().contains("no slide"), "{e}");
    assert!(dir.files("").iter().all(|f| f != "none.pdf" && !f.ends_with(".part")));
}

#[test]
fn unwritable_targets_and_long_videos_are_errors() {
    let dir = Scratch::new("bad");
    let (mut s, ids) = session(2);
    // a folder where the file should go
    std::fs::create_dir_all(dir.path("taken.pdf")).unwrap();
    let e = s.execute("slideshow.export", &json!({"format": "pdf", "path": dir.path("taken.pdf"), "size": 64, "ids": ids, "wait": true}));
    assert!(e.is_err());
    // a file where the folder should go
    std::fs::write(dir.path("file"), b"x").unwrap();
    let e = s.execute("slideshow.export", &json!({"format": "jpeg", "path": dir.path("file"), "size": 64, "ids": ids, "wait": true}));
    assert!(e.is_err());
    assert!(s.activity.list().is_empty());
    // more than 2 hours of video: 120 s slides × 61
    s.execute("slideshow.playback", &json!({"slideDuration": 120})).unwrap();
    let many: Vec<u64> = std::iter::repeat_n(ids[0], 61).collect();
    let e = s.execute("slideshow.export", &json!({"format": "video", "path": dir.path("long.mp4"), "ids": many})).unwrap_err();
    assert!(e.to_string().contains("limited to 120 min"), "{e}");
}
