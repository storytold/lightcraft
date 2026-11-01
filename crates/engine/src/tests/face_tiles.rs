//! Face tiles from the embedded preview, kept in the thumbnail cache (issue #1440).
//!
//! The fixture is a TIFF-family raw with two embedded JPEG previews (a small flat red one and a
//! larger picture) and a sensor strip, as #1020's planned-read tests build them. Counting wrappers
//! around the session's file loader (the path that reads the whole original and develops the raw)
//! and its preview loader say which source a face tile came from.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lightcraft_catalog::PhotoId;
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
use lightcraft_geom::Rect;
use lightcraft_raster::Rgba8;
use serde_json::json;

use crate::Session;

const W: u32 = 600;
const H: u32 = 400;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-face-tiles-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The big preview's pixel at (x, y): smooth ramps and a soft disc, so a resampled square of it
/// compares well between two resamplers.
fn big_pixel(x: u32, y: u32) -> [u8; 3] {
    let (dx, dy) = (x as f64 - 300.0, y as f64 - 200.0);
    let disc = (-(dx * dx + dy * dy) / (2.0 * 40.0 * 40.0)).exp();
    [(40.0 + 150.0 * x as f64 / W as f64) as u8, (60.0 + 120.0 * y as f64 / H as f64) as u8, (50.0 + 160.0 * disc) as u8]
}

fn jpeg(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let rgb: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| px(x, y)).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    encode_jpeg(&img, 95, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap()
}

/// A raw (DNG-tagged) whose IFD0 is a small flat red preview, IFD1 the big picture and IFD2 a CFA
/// "sensor" strip of `tail` bytes. The sensor data can't be developed, so the import describes the
/// photo by its largest preview (600 × 400) and the develop path renders from it, after reading
/// the whole file.
fn raw_with_two_previews(tail: usize) -> Vec<u8> {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let preview = |w: u32, h: u32, bytes: Vec<u8>, first: bool| {
        let mut ifd = IfdBuilder::new();
        if first {
            ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        }
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        ifd.set(t::COMPRESSION, Value::Short(vec![7]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::RGB]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![bytes] });
        ifd
    };
    let small = preview(120, 80, jpeg(120, 80, |_, _| [200, 40, 40]), true);
    let big = preview(W, H, jpeg(W, H, big_pixel), false);
    let mut raw = IfdBuilder::new();
    raw.set(t::COMPRESSION, Value::Short(vec![1]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![6000]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![4000]));
    raw.set_image(ImageData::Strips { rows_per_strip: 4000, strips: vec![vec![0x33u8; tail]] });
    TiffWriter::default().write(&[small, big, raw]).unwrap()
}

/// How often each loader ran.
#[derive(Clone, Default)]
struct Loads {
    /// The original read whole and developed (for a raw: its sensor data decoded).
    files: Arc<AtomicUsize>,
    /// The embedded preview read (planned read: header and preview only).
    previews: Arc<AtomicUsize>,
}

impl Loads {
    fn get(&self) -> (usize, usize) {
        (self.files.load(Ordering::SeqCst), self.previews.load(Ordering::SeqCst))
    }
}

/// Count the session's loads from now on.
fn count_loads(s: &mut Session) -> Loads {
    let loads = Loads::default();
    let file = s.media.file_loader.clone().unwrap();
    let n = loads.files.clone();
    s.media.file_loader = Some(Arc::new(move |p: &str, e: usize| {
        n.fetch_add(1, Ordering::SeqCst);
        file(p, e)
    }));
    let preview = s.media.preview_loader.clone().unwrap();
    let n = loads.previews.clone();
    s.media.preview_loader = Some(Arc::new(move |p: &str, e: usize| {
        n.fetch_add(1, Ordering::SeqCst);
        preview(p, e)
    }));
    loads
}

/// A session on the library in `dir` (created on first use).
fn open(dir: &Path) -> Session {
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s
}

/// A library holding the fixture: (dir, session, the photo, the file).
fn setup(tag: &str) -> (PathBuf, Session, PhotoId, PathBuf) {
    let dir = temp_dir(tag);
    let file = dir.join("faces.dng");
    std::fs::write(&file, raw_with_two_previews(400_000)).unwrap();
    let mut s = open(&dir);
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let p = s.catalog.photos().next().unwrap();
    assert_eq!((p.kind, p.width, p.height), (lightcraft_catalog::MediaKind::Raw, W, H), "described by its largest preview");
    let id = p.id;
    (dir, s, id, file)
}

/// A face of a tenth of the frame, centred: a 64 px tile needs a 377 px source, which the 600 px
/// preview has.
const FACE: Rect = Rect { x0: 0.45, y0: 0.45, x1: 0.55, y1: 0.55 };
/// A face of 2 %: a 64 px tile would need a 1883 px source; the preview is under ¾ of that.
const TINY_FACE: Rect = Rect { x0: 0.49, y0: 0.49, x1: 0.51, y1: 0.51 };

fn tile(s: &mut Session, id: PhotoId, face: Rect, edge: usize) -> Result<Rgba8, String> {
    s.face_job(id, face, edge).unwrap().run().rendered.map(|r| r.image)
}

fn mean_abs_diff(a: &Rgba8, b: &Rgba8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |c| u64::from(p[c].abs_diff(q[c])))).sum();
    sum as f64 / (a.data.len() * 3) as f64
}

/// An unedited raw's face tile is cut from its largest embedded preview: the original is never read
/// whole nor its raw developed, and the tile matches the develop path's at tile size.
#[test]
fn an_unedited_raws_face_comes_from_its_largest_preview() {
    let (dir, mut s, id, file) = setup("preview");
    // (the preview loader's planned read: the header and the preview, not the sensor strip)
    let (_, _, read) = crate::files::preview_window(&file.to_string_lossy(), usize::MAX).unwrap().unwrap();
    assert!(read.fraction() < 0.5, "{read:?}");
    let loads = count_loads(&mut s);
    let from_preview = tile(&mut s, id, FACE, 64).unwrap();
    assert_eq!((from_preview.width, from_preview.height), (64, 64));
    assert_eq!(loads.get(), (0, 1), "one preview read, no whole-file read or raw decode");
    // the big preview, not the small flat red one
    let centre = from_preview.data[32 * 64 + 32];
    assert!(centre[2] > 150 && centre[0] < 150, "the face's centre is the big preview's disc: {centre:?}");

    // the develop path (no preview loader: what every face used before), from the memory cache's reach
    s.media.preview_loader = None;
    let mut job = s.face_job(id, FACE, 64).unwrap();
    job.cache = None;
    let developed = job.run().rendered.unwrap().image;
    assert_eq!(loads.files.load(Ordering::SeqCst), 1, "the develop path reads the original");
    let d = mean_abs_diff(&from_preview, &developed);
    eprintln!("mean difference preview/develop: {d:.3}");
    assert!(d < 4.0, "the preview's tile looks like the developed one: mean difference {d:.2} of 255");
    let _ = std::fs::remove_dir_all(dir);
}

/// A preview under the soft limit (¾ of the pixels the tile needs across the face) leaves the tile to
/// the develop path; so does an edited photo, whose look the camera's JPEG doesn't have.
#[test]
fn a_preview_too_small_or_an_edited_photo_falls_back_to_the_develop_path() {
    let (dir, mut s, id, _) = setup("fallback");
    let loads = count_loads(&mut s);
    let img = tile(&mut s, id, TINY_FACE, 64).unwrap();
    assert_eq!((img.width, img.height), (64, 64));
    assert_eq!(loads.get(), (1, 1), "the preview was tried, then the original developed");

    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let edited = tile(&mut s, id, FACE, 64).unwrap();
    assert_eq!(loads.get(), (2, 1), "an edited photo's face is developed; its preview isn't read");
    let _ = std::fs::remove_dir_all(dir);
    drop(edited);
}

/// Rendered tiles are kept in the library's thumbnail cache: a new session (the next launch) shows
/// them without reading the original, from the preview or the develop path alike; and a size
/// between two steps reuses the tile of its step.
#[test]
fn face_tiles_are_kept_on_disk_for_the_next_launch() {
    let (dir, mut s, id, _) = setup("disk");
    let first = tile(&mut s, id, FACE, 64).unwrap();
    let _ = tile(&mut s, id, TINY_FACE, 64).unwrap();
    drop(s);

    let mut s = open(&dir);
    let loads = count_loads(&mut s);
    let again = tile(&mut s, id, FACE, 64).unwrap();
    let tiny = tile(&mut s, id, TINY_FACE, 64).unwrap();
    assert_eq!(loads.get(), (0, 0), "both tiles come from the disk cache: the original is not touched");
    assert_eq!((again.width, tiny.width), (64, 64));
    assert!(mean_abs_diff(&first, &again) < 3.0, "the cached tile is the rendered one (as a JPEG)");
    // 60 px asks for the 64 px step: the same tile, no new render
    assert_eq!(s.face_job(id, FACE, 60).unwrap().key, s.face_job(id, FACE, 64).unwrap().key);
    let _ = tile(&mut s, id, FACE, 60).unwrap();
    assert_eq!(loads.get(), (0, 0));
    let _ = std::fs::remove_dir_all(dir);
}

/// What can go wrong never panics: a preview loader that fails or answers nonsense, a damaged
/// preview, face boxes at the edge or degenerate, an offline original, a damaged cache entry.
#[test]
fn face_tiles_never_crash() {
    let (dir, mut s, id, file) = setup("crash");

    // a damaged cache entry: dropped, and the tile rendered again (from the preview)
    let job = s.face_job(id, FACE, 64).unwrap();
    let key = job.cache.as_ref().unwrap().1.to_string();
    assert!(job.run().rendered.is_ok());
    let thumbs = s.library.as_ref().unwrap().thumbs_dir();
    drop(s);
    let entry = thumbs.join(&key[..2]).join(format!("{key}.lcthumb"));
    assert!(entry.exists(), "the tile was written to the thumbnail cache");
    std::fs::write(&entry, b"not a jpeg").unwrap();
    let mut s = open(&dir);
    let loads = count_loads(&mut s);
    assert_eq!(tile(&mut s, id, FACE, 64).unwrap().width, 64);
    assert_eq!(loads.get(), (0, 1), "rendered again from the preview");

    // face boxes at the corners, oversized, empty, not numbers: a tile or an error, never a panic
    for face in [
        Rect { x0: 0.0, y0: 0.0, x1: 0.05, y1: 0.05 },
        Rect { x0: 0.95, y0: 0.95, x1: 1.0, y1: 1.0 },
        Rect { x0: -5.0, y0: -5.0, x1: 5.0, y1: 5.0 },
        Rect { x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5 },
        Rect { x0: f64::NAN, y0: 0.5, x1: 0.6, y1: f64::INFINITY },
        Rect { x0: f64::NEG_INFINITY, y0: f64::NAN, x1: f64::NAN, y1: f64::NAN },
    ] {
        for edge in [0, 1, 64, usize::MAX] {
            let img = tile(&mut s, id, face, edge).unwrap_or_else(|e| panic!("{face:?} at {edge}: {e}"));
            assert_eq!(img.width, img.height, "{face:?} at {edge}");
        }
    }

    // a preview loader that answers nonsense or nothing: the develop path renders the tile
    let id2 = id;
    for answer in [None, Some(Rgba8 { width: W as usize, height: H as usize, data: Vec::new() }), Some(Rgba8::new(1, 1))] {
        s.media.preview_loader = Some(Arc::new(move |_: &str, _: usize| answer.clone()));
        let mut job = s.face_job(id2, FACE, 64).unwrap();
        job.cache = None;
        assert_eq!(job.run().rendered.unwrap().image.width, 64);
    }

    // a damaged preview in the file: no tile from it, and no panic either way
    s.media.preview_loader = Some(crate::files::fs_preview_loader());
    let mut bytes = std::fs::read(&file).unwrap();
    let n = bytes.len();
    for b in bytes.iter_mut().take(n - 400_000).skip(1000) {
        *b = 0x5a;
    }
    std::fs::write(&file, &bytes).unwrap();
    let mut job = s.face_job(id, FACE, 64).unwrap();
    job.cache = None;
    let _ = job.run();

    // the original gone (an offline drive): an error, no panic
    std::fs::remove_file(&file).unwrap();
    let mut job = s.face_job(id, FACE, 64).unwrap();
    job.cache = None;
    assert!(job.run().rendered.is_err());
    let _ = std::fs::remove_dir_all(dir);
}
