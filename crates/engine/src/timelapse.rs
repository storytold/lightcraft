//! Timelapse video from stills (upstream issue #497): the selected photos, in the shown order
//! (the current sort, Custom Order in an album), each rendered with its own develop settings,
//! fitted (letterboxed) or cropped to fill one frame size, held for `hold` frames, optionally
//! cross-faded over `crossfade` extra frames, and encoded by `lcc-video-export` as H.264 MP4 or
//! ProRes MOV without audio. The movie is written to `<path>.partial` and renamed when done; a
//! cancel or an error leaves no file.
//!
//! Frames: `photos × hold + (photos − 1) × crossfade`.
//!
//! Sources: own design (reuses the export render jobs and `lcc_video_export::SequenceWriter`).

use std::sync::atomic::{AtomicBool, Ordering};

use lcc_catalog::{MediaKind, PhotoId};
use lcc_pipeline::{OutputDepth, OutputSpace};
use lcc_raster::{Rgb32f, Rgba8};
use serde_json::{Value, json};

use crate::Session;
use crate::export_video::VideoFormat;
use crate::media::RenderJob;

/// Most photos in one timelapse.
pub const MAX_PHOTOS: usize = 10_000;
/// Most frames one photo is held, or one cross-fade lasts.
pub const MAX_HOLD: u32 = 1000;
/// Most frames in one movie (2 h at 60 fps).
pub const MAX_FRAMES: u64 = 2 * 3600 * 60;
/// Frame sizes: each side 16..=8192, rounded down to even.
pub const SIDE_RANGE: (u32, u32) = (16, 8192);

/// The options of `export.timelapse` (everything but `path` and `ids`).
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    /// Crop to fill the frame (else fit inside it, letterboxed in black).
    pub fill: bool,
    /// Frames each photo is shown (≥ 1).
    pub hold: u32,
    /// Extra frames blending one photo into the next (0 = cut).
    pub crossfade: u32,
    pub format: VideoFormat,
    pub quality: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { fps: 24, width: 1920, height: 1080, fill: false, hold: 1, crossfade: 0, format: VideoFormat::H264, quality: 80 }
    }
}

const KEYS: &[&str] = &["path", "ids", "id", "fps", "size", "fit", "hold", "crossfade", "format", "quality"];

fn uint(p: &Value, k: &str, lo: u32, hi: u32, d: u32) -> Result<u32, String> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(v) => v
            .as_u64()
            .filter(|n| (u64::from(lo)..=u64::from(hi)).contains(n))
            .map(|n| n as u32)
            .ok_or(format!("{k} must be a whole number {lo}–{hi}")),
    }
}

/// `"1920x1080"` or `[1920, 1080]` → even sides within [`SIDE_RANGE`].
pub fn parse_size(v: &Value) -> Result<(u32, u32), String> {
    let err = || format!("size must be \"WIDTHxHEIGHT\" or [width, height], each {}–{}", SIDE_RANGE.0, SIDE_RANGE.1);
    let (w, h) = match v {
        Value::String(s) => {
            let (a, b) = s.trim().split_once(['x', 'X', '×']).ok_or_else(err)?;
            (a.trim().parse::<u64>().map_err(|_| err())?, b.trim().parse::<u64>().map_err(|_| err())?)
        }
        Value::Array(a) if a.len() == 2 => (a.first().and_then(Value::as_u64).ok_or_else(err)?, a.get(1).and_then(Value::as_u64).ok_or_else(err)?),
        _ => return Err(err()),
    };
    let r = u64::from(SIDE_RANGE.0)..=u64::from(SIDE_RANGE.1);
    if !r.contains(&w) || !r.contains(&h) {
        return Err(err());
    }
    Ok((w as u32 & !1, h as u32 & !1))
}

impl Options {
    pub fn parse(p: &Value) -> Result<Self, String> {
        let o = p.as_object().ok_or("params must be an object")?;
        if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown option `{k}` (expected {})", KEYS.join(", ")));
        }
        let d = Self::default();
        let (width, height) = match p.get("size") {
            None | Some(Value::Null) => (d.width, d.height),
            Some(v) => parse_size(v)?,
        };
        let fill = match p.get("fit") {
            None | Some(Value::Null) => false,
            Some(v) => match v.as_str() {
                Some("fit" | "letterbox") => false,
                Some("fill" | "crop") => true,
                _ => return Err("fit must be \"fit\" or \"fill\"".into()),
            },
        };
        let path_ext =
            p.get("path").and_then(Value::as_str).and_then(|s| std::path::Path::new(s).extension()).map(|e| e.to_string_lossy().to_ascii_lowercase());
        let format = match p.get("format") {
            None | Some(Value::Null) => {
                if path_ext.as_deref() == Some("mov") {
                    VideoFormat::ProRes
                } else {
                    VideoFormat::H264
                }
            }
            Some(v) => v.as_str().and_then(VideoFormat::parse).ok_or("format must be \"h264\" or \"prores\"")?,
        };
        Ok(Self {
            fps: uint(p, "fps", 1, 120, d.fps)?,
            width,
            height,
            fill,
            hold: uint(p, "hold", 1, MAX_HOLD, d.hold)?,
            crossfade: uint(p, "crossfade", 0, MAX_HOLD, d.crossfade)?,
            format,
            quality: uint(p, "quality", 1, 100, u32::from(d.quality))? as u8,
        })
    }

    /// Frames for `photos` photos.
    pub fn frames(&self, photos: usize) -> u64 {
        let n = photos as u64;
        n.saturating_mul(u64::from(self.hold)).saturating_add(n.saturating_sub(1).saturating_mul(u64::from(self.crossfade)))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "fps": self.fps, "size": format!("{}x{}", self.width, self.height), "fit": if self.fill { "fill" } else { "fit" },
            "hold": self.hold, "crossfade": self.crossfade, "format": VideoFormat::NAMES.get(usize::from(self.format == VideoFormat::ProRes)).copied().unwrap_or("h264"),
            "quality": self.quality,
        })
    }
}

/// A timelapse set up from the session, to run anywhere (a worker thread).
pub struct Prepared {
    pub path: String,
    pub options: Options,
    items: Vec<(String, RenderJob)>,
    /// Video clips left out.
    pub skipped: usize,
}

/// What a timelapse wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub path: String,
    pub frames: u64,
    pub photos: usize,
    pub skipped: usize,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub bytes: u64,
}

impl Report {
    pub fn json(&self) -> Value {
        json!({"path": self.path, "frames": self.frames, "photos": self.photos, "skipped": self.skipped, "width": self.width, "height": self.height, "duration": self.duration, "bytes": self.bytes, "timelapse": true})
    }
}

/// The photos a timelapse uses: `ids` as given, else the selection in the shown order.
pub fn ordered_targets(s: &mut Session, p: &Value) -> Result<Vec<PhotoId>, String> {
    if let Some(ids) = p.get("ids") {
        let a = ids.as_array().ok_or("ids must be an array of photo ids")?;
        if a.iter().any(|v| v.as_u64().is_none()) {
            return Err("ids must contain only photo ids".into());
        }
    }
    if p.get("id").is_some_and(|v| v.as_u64().is_none()) {
        return Err("id must be a photo id".into());
    }
    let mut ids = s.targets(p).map_err(|e| e.to_string())?;
    if p.get("ids").is_none() && p.get("id").is_none() {
        // stable: photos not shown keep their selection order, after the shown ones
        let mut keyed: Vec<(usize, PhotoId)> = ids.iter().map(|id| (s.visible_position(*id).unwrap_or(usize::MAX), *id)).collect();
        keyed.sort_by_key(|k| k.0);
        ids = keyed.into_iter().map(|k| k.1).collect();
    }
    Ok(ids)
}

pub fn prepare(s: &mut Session, p: &Value) -> Result<Prepared, String> {
    let options = Options::parse(p)?;
    let path = p.get("path").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("missing path")?.to_string();
    s.check_write_target(&path)?;
    let ids = ordered_targets(s, p)?;
    let mut items = Vec::with_capacity(ids.len().min(MAX_PHOTOS));
    let mut skipped = 0;
    for id in ids {
        let ph = s.catalog.photo(id).ok_or_else(|| format!("no photo {}", id.0))?;
        if ph.kind == MediaKind::Video {
            skipped += 1;
            continue;
        }
        if items.len() >= MAX_PHOTOS {
            return Err(format!("a timelapse takes at most {MAX_PHOTOS} photos"));
        }
        let name = ph.file_name.clone();
        let (bw, bh) = render_box(&options, ph.width, ph.height);
        let job = s.export_job(id, bw, bh, OutputSpace::Srgb, OutputDepth::U8)?;
        items.push((name, job));
    }
    if items.is_empty() {
        return Err("select at least one photo (video clips are left out)".into());
    }
    if options.frames(items.len()) > MAX_FRAMES {
        return Err(format!("the timelapse would have {} frames (at most {MAX_FRAMES})", options.frames(items.len())));
    }
    Ok(Prepared { path, options, items, skipped })
}

/// The box a photo renders into: the frame for fit; for fill, large enough that the photo
/// (aspect from its catalog size, before crop) covers the frame.
fn render_box(o: &Options, pw: u32, ph: u32) -> (usize, usize) {
    let (w, h) = (o.width as f64, o.height as f64);
    if !o.fill || pw == 0 || ph == 0 {
        return (o.width as usize, o.height as usize);
    }
    let a = (f64::from(pw) / f64::from(ph)).clamp(0.05, 20.0);
    let cap = f64::from(SIDE_RANGE.1);
    let (bw, bh) = if a >= w / h { (h * a, h) } else { (w, w / a) };
    ((bw.ceil().min(cap) as usize).max(1), (bh.ceil().min(cap) as usize).max(1))
}

/// Place a render in a `w × h` frame: scaled to fit (black bars) or to fill (centre crop).
/// Values are the render's encoded sRGB, 0..=1.
pub fn place(img: &Rgba8, w: usize, h: usize, fill: bool) -> Rgb32f {
    let src = Rgb32f::from_fn(img.width, img.height, |x, y| {
        let p = img.data.get(y * img.width + x).copied().unwrap_or([0; 4]);
        [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]
    });
    if img.width == 0 || img.height == 0 {
        return Rgb32f::new(w, h);
    }
    let (sx, sy) = (w as f32 / img.width as f32, h as f32 / img.height as f32);
    let s = if fill { sx.max(sy) } else { sx.min(sy) };
    let (dw, dh) = (img.width as f32 * s, img.height as f32 * s);
    let (ox, oy) = ((w as f32 - dw) / 2.0, (h as f32 - dh) / 2.0);
    Rgb32f::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 + 0.5 - ox, y as f32 + 0.5 - oy);
        if fx < 0.0 || fy < 0.0 || fx > dw || fy > dh {
            return [0.0; 3];
        }
        src.sample_bilinear(fx / s, fy / s)
    })
}

fn blend(a: &Rgb32f, b: &Rgb32f, t: f32) -> Rgb32f {
    let mut o = a.clone();
    for (d, q) in o.data.iter_mut().zip(&b.data) {
        for c in 0..3 {
            d[c] += (q[c] - d[c]) * t;
        }
    }
    o
}

impl Prepared {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn frames(&self) -> u64 {
        self.options.frames(self.items.len())
    }

    /// Render, blend and encode. `progress(photos done, file name)` runs before each photo;
    /// setting `cancel` stops within a frame and removes the partial movie.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(self, cancel: &AtomicBool, progress: &mut dyn FnMut(usize, &str)) -> Result<Report, String> {
        use lcc_video_export::{ClipFormat, SequenceWriter};
        let o = &self.options;
        let format = match o.format {
            VideoFormat::H264 => ClipFormat::H264Mp4,
            VideoFormat::ProRes => ClipFormat::ProResMov,
        };
        let out = std::path::PathBuf::from(&self.path);
        if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let (w, h) = (o.width as usize, o.height as usize);
        let mut writer = SequenceWriter::create(&out, format, o.quality, o.width, o.height, o.fps, 1).map_err(|e| e.to_string())?;
        let stopped = || cancel.load(Ordering::Relaxed);
        let cancelled = || "Timelapse cancelled".to_string();
        let photos = self.items.len();
        let mut prev: Option<Rgb32f> = None;
        for (i, (name, job)) in self.items.into_iter().enumerate() {
            progress(i, &name);
            if stopped() {
                return Err(cancelled());
            }
            let image = job.run().rendered.map_err(|e| format!("{name}: {e}"))?.image;
            let frame = place(&image, w, h, o.fill);
            if let Some(p) = &prev {
                for k in 0..o.crossfade {
                    if stopped() {
                        return Err(cancelled());
                    }
                    let t = (k + 1) as f32 / (o.crossfade + 1) as f32;
                    writer.push_encoded(&blend(p, &frame, t)).map_err(|e| e.to_string())?;
                }
            }
            for _ in 0..o.hold {
                if stopped() {
                    return Err(cancelled());
                }
                writer.push_encoded(&frame).map_err(|e| e.to_string())?;
            }
            prev = Some(frame);
        }
        progress(photos, "");
        if stopped() {
            return Err(cancelled());
        }
        let rep = writer.finish().map_err(|e| e.to_string())?;
        let bytes = std::fs::metadata(&out).map_or(0, |m| m.len());
        Ok(Report {
            path: self.path,
            frames: rep.frames,
            photos,
            skipped: self.skipped,
            width: rep.width,
            height: rep.height,
            duration: rep.duration,
            bytes,
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn run(self, _cancel: &AtomicBool, _progress: &mut dyn FnMut(usize, &str)) -> Result<Report, String> {
        Err("timelapse export is not available in the web build".into())
    }
}

/// `export.timelapse` on the calling thread, with an activity row whose ✕ stops it.
pub fn run_command(s: &mut Session, p: &Value) -> Result<Value, String> {
    let prepared = prepare(s, p)?;
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let row = s.activity.start("export", "Exporting timelapse", crate::activity::Cancel::Flag(cancel.clone()));
    let total = prepared.len() as u64;
    row.progress(0, total);
    let rep = prepared.run(&cancel, &mut |done, name| {
        row.progress(done as u64, total);
        row.detail(name);
    })?;
    Ok(rep.json())
}
