//! Slideshow export (Lightroom Classic's Export PDF / Export JPEG / Export Video): the photos in
//! view as their composed slides (layout, overlays: [`crate::slide`]), between the intro and
//! ending screens that are on ([`crate::slide_title`]), written as
//!
//! - **PDF**: one page per slide, the page the slide's size in points (1 px = 1 pt), its picture a
//!   JPEG; each page carries the slide duration and a fade transition for viewers' presentation
//!   mode, and "full screen" asks the viewer to open in it (`/PageMode /FullScreen`);
//! - **JPEG sequence**: `<name>-001.jpg`, `<name>-002.jpg`… in a folder;
//! - **video**: AV1 in MP4 or WebM ([`lightcraft_codecs::video`], our own muxers), the timeline
//!   sampled at 24/25/30/60 fps: each slide held for its duration, fades rendered as blended
//!   frames (crossfade, or through the fade colour). No audio track.
//!
//! The slides play once in view order (random order and repeat are playback choices, not
//! export ones). The slide aspect comes from Layout's aspect preview; "screen" exports 16:9.
//!
//! [`prepare`] reads the session (cheap: render jobs, no pixels) and [`Prepared::run`] does the
//! work on any thread, with progress and a cancel flag. Files are written as `<file>.part` and
//! renamed when everything is written, so a cancelled or failed export leaves nothing behind. A
//! slide whose photo can't be rendered (file gone, unreadable) is skipped with a warning.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::PhotoId;
use lightcraft_codecs::video::{Av1Encoder, Container, VideoConfig, VideoWriter};
use lightcraft_pipeline::{OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::media::RenderJob;
use crate::originals::OriginalGuard;
use crate::slide_title::TitleCompose;
use crate::slideshow::{SLIDE_MAX, SLIDE_MIN, Title};
use crate::{Session, export::ExportFormat, export::ExportOptions};

/// The longest video exported (seconds): 2 hours.
pub const MAX_SECONDS: f64 = 2.0 * 3600.0;
/// The most slides an export takes.
pub const MAX_SLIDES: usize = 10_000;
/// Long side of PDF pages and JPEGs, in pixels.
pub const SIZE_MIN: usize = 64;
pub const SIZE_MAX: usize = lightcraft_slideshow::MAX_SIDE;
/// Video frame height, in pixels.
pub const VIDEO_MIN: usize = 64;
pub const VIDEO_MAX: usize = 2160;
/// Frame rates offered.
pub const FPS: [u32; 4] = [24, 25, 30, 60];

/// What an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Jpeg,
    Video,
}

impl Format {
    pub fn id(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Jpeg => "jpeg",
            Format::Video => "video",
        }
    }
}

/// The export's settings (the dialog's state, and `slideshow.export`'s params without `path`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportSettings {
    pub format: Format,
    /// PDF / JPEG: the long side in pixels; video: the frame height (720 = 720p).
    pub size: usize,
    /// JPEG quality (PDF pages, JPEG files) or video quality, 1–100.
    pub quality: u8,
    /// Video frames per second.
    pub fps: u32,
    /// Video container: "mp4" or "webm".
    pub container: String,
    /// PDF: ask the viewer to open in full screen.
    pub full_screen: bool,
    /// Video: rav1e speed preset, 1 (slow, smaller) – 10 (fast).
    pub speed: u8,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings { format: Format::Pdf, size: 1920, quality: 90, fps: 30, container: "mp4".into(), full_screen: true, speed: 10 }
    }
}

impl ExportSettings {
    /// The defaults for `format` (sizes: PDF/JPEG 1920 px, video 720p; video quality 75).
    pub fn for_format(format: Format) -> Self {
        match format {
            Format::Video => ExportSettings { format, size: 720, quality: 75, ..Default::default() },
            _ => ExportSettings { format, ..Default::default() },
        }
    }

    /// The file extension the export's path takes (none: a JPEG sequence goes in a folder).
    pub fn extension(&self) -> Option<&'static str> {
        match self.format {
            Format::Pdf => Some("pdf"),
            Format::Jpeg => None,
            Format::Video => Some(if self.container == "webm" { "webm" } else { "mp4" }),
        }
    }
}

/// `slideshow.export`'s params, checked: settings, path, file name prefix and photos.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub settings: ExportSettings,
    pub path: String,
    /// JPEG sequence: the files' name before `-001` (default: the folder's name).
    pub name: Option<String>,
    pub ids: Option<Vec<PhotoId>>,
}

const KEYS: &str = "format, path, size, quality, fps, container, fullScreen, speed, name, ids, wait";

fn uint(k: &str, v: &Value, lo: u64, hi: u64) -> Result<u64, String> {
    match v.as_u64() {
        Some(n) if (lo..=hi).contains(&n) => Ok(n),
        _ => Err(format!("`{k}` must be a whole number from {lo} to {hi}, not {v}")),
    }
}

impl Request {
    pub fn parse(p: &Value) -> Result<Request, String> {
        let o: &Map<String, Value> = p.as_object().ok_or("params must be an object")?;
        if let Some(k) = o.keys().find(|k| !KEYS.split(", ").any(|x| x == k.as_str())) {
            return Err(format!("unknown key `{k}` (known: {KEYS})"));
        }
        let format = match o.get("format").and_then(Value::as_str).map(str::trim) {
            Some("pdf") => Format::Pdf,
            Some("jpeg" | "jpg") => Format::Jpeg,
            Some("video") => Format::Video,
            _ => return Err(format!("`format` must be \"pdf\", \"jpeg\" or \"video\", not {}", o.get("format").unwrap_or(&Value::Null))),
        };
        let mut s = ExportSettings::for_format(format);
        let path = o.get("path").and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).ok_or("missing `path`")?.to_string();
        if let Some(v) = o.get("size").filter(|v| !v.is_null()) {
            // "720p" / "1080p" for video
            let v = match v.as_str().map(|x| x.trim().trim_end_matches(['p', 'P'])) {
                Some(x) => x.parse::<u64>().map(Value::from).map_err(|_| format!("`size` must be a number of pixels, not {v}"))?,
                None => v.clone(),
            };
            s.size = match format {
                Format::Video => uint("size", &v, VIDEO_MIN as u64, VIDEO_MAX as u64)? as usize,
                _ => uint("size", &v, SIZE_MIN as u64, SIZE_MAX as u64)? as usize,
            };
        }
        if let Some(v) = o.get("quality").filter(|v| !v.is_null()) {
            s.quality = uint("quality", v, 1, 100)? as u8;
        }
        if let Some(v) = o.get("fps").filter(|v| !v.is_null()) {
            let f =
                v.as_u64().filter(|f| FPS.iter().any(|x| u64::from(*x) == *f)).ok_or_else(|| format!("`fps` must be 24, 25, 30 or 60, not {v}"))?;
            s.fps = f as u32;
        }
        if let Some(v) = o.get("container").filter(|v| !v.is_null()) {
            let c = v.as_str().and_then(Container::parse).ok_or_else(|| format!("`container` must be \"mp4\" or \"webm\", not {v}"))?;
            s.container = c.id().into();
        }
        if let Some(v) = o.get("fullScreen").filter(|v| !v.is_null()) {
            s.full_screen = v.as_bool().ok_or_else(|| format!("`fullScreen` must be true or false, not {v}"))?;
        }
        if let Some(v) = o.get("speed").filter(|v| !v.is_null()) {
            s.speed = uint("speed", v, 1, 10)? as u8;
        }
        if let Some(v) = o.get("wait").filter(|v| !v.is_null() && !v.is_boolean()) {
            return Err(format!("`wait` must be true or false, not {v}"));
        }
        let name = match o.get("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(n)) => {
                let n = n.trim();
                if n.is_empty()
                    || n.chars().count() > 120
                    || n.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
                {
                    return Err(format!("`name` must be 1–120 characters without path separators or reserved characters, not {n:?}"));
                }
                Some(n.to_string())
            }
            Some(v) => return Err(format!("`name` must be text, not {v}")),
        };
        if name.is_some() && format != Format::Jpeg {
            return Err("`name` is for the JPEG sequence only".into());
        }
        let ids = match o.get("ids") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => {
                let ids: Option<Vec<PhotoId>> = a.iter().map(|v| v.as_u64().map(PhotoId)).collect();
                Some(ids.ok_or("`ids` must be an array of photo ids")?)
            }
            Some(v) => return Err(format!("`ids` must be an array of photo ids, not {v}")),
        };
        Ok(Request { settings: s, path, name, ids })
    }
}

/// What a slide shows.
enum Source {
    Photo { name: String, job: Box<RenderJob> },
    Title { title: Title, compose: Arc<TitleCompose> },
}

struct Slide {
    source: Source,
    /// Seconds it stays up, its fade in included.
    duration: f64,
    /// Seconds the transition into it takes (none for the first).
    fade: f64,
}

impl Slide {
    fn label(&self) -> String {
        match &self.source {
            Source::Photo { name, .. } => name.clone(),
            Source::Title { title, .. } => title.id().to_string(),
        }
    }

    /// The slide's pixels, `w × h`.
    fn render(self, w: usize, h: usize) -> Result<Rgba8, String> {
        let img = match self.source {
            Source::Photo { job, .. } => job.run().rendered?.image,
            Source::Title { compose, .. } => compose.compose(),
        };
        Ok(fit(img, w, h))
    }
}

/// `img` at exactly `w × h`: as it is when it already is (the composer's output), else centred
/// on black (a slide bigger than the composer draws, or a surprise from a renderer).
fn fit(img: Rgba8, w: usize, h: usize) -> Rgba8 {
    if img.width == w && img.height == h && img.data.len() == w * h {
        return img;
    }
    let mut out = Rgba8::filled(w, h, [0, 0, 0, 255]);
    let (cw, ch) = (img.width.min(w), img.height.min(h));
    let (dx, dy) = ((w - cw) / 2, (h - ch) / 2);
    let (sx, sy) = ((img.width - cw) / 2, (img.height - ch) / 2);
    for y in 0..ch {
        let (Some(src), Some(dst)) =
            (img.data.get((sy + y) * img.width + sx..(sy + y) * img.width + sx + cw), out.data.get_mut((dy + y) * w + dx..(dy + y) * w + dx + cw))
        else {
            continue;
        };
        dst.copy_from_slice(src);
    }
    out
}

/// An export ready to run: everything it needs from the session.
pub struct Prepared {
    pub request: Request,
    slides: Vec<Slide>,
    pub width: usize,
    pub height: usize,
    fade_color: Option<[u8; 3]>,
    guard: OriginalGuard,
    /// Problems found while preparing (photos not in the library).
    pub warnings: Vec<String>,
}

/// The size of a slide whose long side is `long` at `ratio` (width / height).
fn sized(ratio: f64, long: usize) -> (usize, usize) {
    let short = |l: usize, r: f64| ((l as f64 / r).round() as usize).max(1);
    if ratio >= 1.0 { (long, short(long, ratio)) } else { (short(long, 1.0 / ratio), long) }
}

/// Read the session: the slides, their render jobs and durations, the output size.
pub fn prepare(s: &mut Session, p: &Value) -> Result<Prepared, String> {
    let request = Request::parse(p)?;
    let settings = &request.settings;
    let ratio = s.slide_layout.aspect.ratio(16.0 / 9.0);
    let (width, height) = match settings.format {
        Format::Video => {
            let h = settings.size & !1;
            ((((h as f64 * ratio).round() as usize) & !1).max(16), h)
        }
        _ => sized(ratio, settings.size),
    };
    let ids = match &request.ids {
        Some(ids) => ids.clone(),
        None => s.visible_cloned(),
    };
    if ids.is_empty() {
        return Err("no photos in view to export".into());
    }
    if ids.len() > MAX_SLIDES {
        return Err(format!("a slideshow export takes at most {MAX_SLIDES} photos; this view has {}", ids.len()));
    }
    let play = s.slideshow.clone().sanitized();
    let (intro, ending) = s.slide_titles.durations(play.slide_duration);
    let clamp = |d: f64| if d.is_finite() { d.clamp(SLIDE_MIN, SLIDE_MAX) } else { play.slide_duration };
    let layout = s.slide_layout.clone().sanitized();
    let mut warnings = Vec::new();
    let mut slides = Vec::with_capacity(ids.len() + 2);
    let title = |s: &Session, t: Title, d: f64| Slide {
        source: Source::Title { title: t, compose: s.title_compose(t, width, height) },
        duration: clamp(d),
        fade: 0.0,
    };
    if let Some(d) = intro {
        slides.push(title(s, Title::Intro, d));
    }
    for id in ids {
        let Some(photo) = s.catalog.photo(id).cloned() else {
            warnings.push(format!("photo {} is not in the library; skipped", id.0));
            continue;
        };
        let aspect = s.slide_photo_aspect(id).unwrap_or(1.5);
        let (pw, ph) = lightcraft_slideshow::photo_size(&layout, aspect, width, height);
        let job = match s.export_job(id, pw.max(8), ph.max(8), OutputSpace::Srgb, OutputDepth::U8) {
            Ok(job) => job,
            Err(e) => {
                warnings.push(format!("{}: {e}; skipped", photo.file_name));
                continue;
            }
        };
        let compose = s.slide_compose(&layout, width, height, Some(id));
        slides.push(Slide {
            source: Source::Photo { name: photo.file_name.clone(), job: Box::new(job.with_slide(compose)) },
            duration: play.slide_duration,
            fade: 0.0,
        });
    }
    if !slides.iter().any(|x| matches!(x.source, Source::Photo { .. })) {
        return Err("none of the photos can be exported".into());
    }
    if let Some(d) = ending {
        slides.push(title(s, Title::Ending, d));
    }
    // the transition into each slide after the first: the fade, never longer than the slide
    for x in slides.iter_mut().skip(1) {
        x.fade = play.fade_duration.min(x.duration).max(0.0);
    }
    if settings.format == Format::Video {
        let total: f64 = slides.iter().map(|x| x.duration).sum();
        if total > MAX_SECONDS {
            return Err(format!(
                "the slideshow lasts {} min; video export is limited to {} min (shorten the slides or export fewer photos)",
                (total / 60.0).ceil(),
                MAX_SECONDS / 60.0
            ));
        }
        if !lightcraft_codecs::video::AV1_AVAILABLE {
            return Err("video export (AV1) is not available in this build".into());
        }
    }
    s.check_write_target(&request.path)?;
    Ok(Prepared { request, slides, width, height, fade_color: play.fade_color, guard: s.original_guard(), warnings })
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub json: Value,
}

/// Progress: (done, total, the slide being worked on). `total` is slides (PDF, JPEG) or frames
/// at the nominal rate (video).
pub type Progress<'a> = &'a dyn Fn(u64, u64, &str);

const CANCELLED: &str = "Slideshow export cancelled";

/// A file written as `<path>.part` and renamed over `path` when complete; dropped before that,
/// the part file is removed.
struct PartFile {
    part: PathBuf,
    target: PathBuf,
    done: bool,
}

impl PartFile {
    fn new(target: &Path) -> Result<(PartFile, std::fs::File), String> {
        if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut name = target.file_name().ok_or_else(|| format!("{}: not a file name", target.display()))?.to_os_string();
        name.push(".part");
        let part = target.with_file_name(name);
        let f = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        Ok((PartFile { part, target: target.to_path_buf(), done: false }, f))
    }

    fn commit(mut self, guard: &OriginalGuard) -> Result<(), String> {
        guard.check(&self.target)?;
        std::fs::rename(&self.part, &self.target).map_err(|e| format!("{}: {e}", self.target.display()))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// Blend `a` → `b` at `k` (0 = a, 1 = b), or through `colour` (out to it, then in from it).
fn blend(a: &Rgba8, b: &Rgba8, k: f32, colour: Option<[u8; 3]>) -> Vec<[u8; 4]> {
    let mix = |x: u8, y: u8, t: f32| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round().clamp(0.0, 255.0) as u8;
    match colour {
        Some(c) => {
            let (img, veil) = if k < 0.5 { (a, k * 2.0) } else { (b, 2.0 - k * 2.0) };
            img.data.iter().map(|p| [mix(p[0], c[0], veil), mix(p[1], c[1], veil), mix(p[2], c[2], veil), 255]).collect()
        }
        None => a.data.iter().zip(&b.data).map(|(p, q)| [mix(p[0], q[0], k), mix(p[1], q[1], k), mix(p[2], q[2], k), 255]).collect(),
    }
}

impl Prepared {
    /// Slides to export (titles included).
    pub fn len(&self) -> usize {
        self.slides.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slides.is_empty()
    }

    /// Seconds the slideshow lasts.
    pub fn seconds(&self) -> f64 {
        self.slides.iter().map(|x| x.duration).sum()
    }

    /// Units of progress: slides, or frames at the nominal rate for video.
    pub fn total(&self) -> u64 {
        match self.request.settings.format {
            Format::Video => (self.seconds() * f64::from(self.request.settings.fps)).round() as u64,
            _ => self.slides.len() as u64,
        }
    }

    /// Write the export. `cancel` is checked between slides and frames.
    pub fn run(self, progress: Progress, cancel: &AtomicBool) -> Result<Outcome, String> {
        match self.request.settings.format {
            Format::Pdf => self.run_pdf(progress, cancel),
            Format::Jpeg => self.run_jpeg(progress, cancel),
            Format::Video => self.run_video(progress, cancel),
        }
    }

    fn jpeg(&self, img: &Rgba8) -> Result<Vec<u8>, String> {
        crate::export::encode_image(img, &ExportOptions { format: ExportFormat::Jpeg, quality: self.request.settings.quality, ..Default::default() })
    }

    fn summary(&self, format: Format, slides: usize, skipped: usize, warnings: Vec<String>) -> Value {
        json!({
            "format": format.id(),
            "path": self.request.path,
            "width": self.width,
            "height": self.height,
            "slides": slides,
            "skipped": skipped,
            "warnings": warnings,
        })
    }

    fn run_pdf(mut self, progress: Progress, cancel: &AtomicBool) -> Result<Outcome, String> {
        let target = PathBuf::from(&self.request.path);
        let (part, file) = PartFile::new(&target)?;
        let mut pdf = crate::pdf::Pdf::new(BufWriter::new(file))?;
        let (w, h) = (self.width, self.height);
        let total = self.slides.len() as u64;
        let mut warnings = std::mem::take(&mut self.warnings);
        let mut skipped = 0;
        let slides = std::mem::take(&mut self.slides);
        for (i, slide) in slides.into_iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            let label = slide.label();
            progress(i as u64, total, &label);
            let (duration, fade) = (slide.duration, slide.fade);
            let img = match slide.render(w, h) {
                Ok(img) => img,
                Err(e) => {
                    warnings.push(format!("{label}: {e}; skipped"));
                    skipped += 1;
                    continue;
                }
            };
            let jpg = self.jpeg(&img)?;
            // presentation mode: how long the page stays, and the fade into it
            let mut extra = format!(" /Dur {duration:.2}");
            if fade > 0.0 {
                extra += &format!(" /Trans << /S /Fade /D {fade:.2} >>");
            }
            pdf.image_page(&jpg, w, h, (w as f64, h as f64), &extra)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let pages = pdf.pages();
        if pages == 0 {
            return Err("no slide could be rendered".into());
        }
        let bytes = pdf.len();
        let catalog = if self.request.settings.full_screen { " /PageMode /FullScreen" } else { "" };
        pdf.finish(catalog)?.into_inner().map_err(|e| format!("PDF: {}", e.error()))?.sync_all().map_err(|e| format!("PDF: {e}"))?;
        part.commit(&self.guard)?;
        progress(total, total, "");
        let mut out = self.summary(Format::Pdf, pages, skipped, warnings);
        out["pages"] = json!(pages);
        out["bytes"] = json!(bytes);
        out["fullScreen"] = json!(self.request.settings.full_screen);
        Ok(Outcome { json: out })
    }

    fn run_jpeg(mut self, progress: Progress, cancel: &AtomicBool) -> Result<Outcome, String> {
        let dir = PathBuf::from(&self.request.path);
        let prefix = self
            .request
            .name
            .clone()
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().trim().to_string()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "Slideshow".into());
        let digits = self.slides.len().to_string().len().max(3);
        let (w, h) = (self.width, self.height);
        let total = self.slides.len() as u64;
        let mut warnings = std::mem::take(&mut self.warnings);
        let mut skipped = 0;
        let mut parts = Vec::new();
        let mut bytes = 0usize;
        let slides = std::mem::take(&mut self.slides);
        for (i, slide) in slides.into_iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            let label = slide.label();
            progress(i as u64, total, &label);
            let img = match slide.render(w, h) {
                Ok(img) => img,
                Err(e) => {
                    warnings.push(format!("{label}: {e}; skipped"));
                    skipped += 1;
                    continue;
                }
            };
            let jpg = self.jpeg(&img)?;
            let target = dir.join(format!("{prefix}-{:0digits$}.jpg", parts.len() + 1));
            self.guard.check(&target)?;
            let (part, mut f) = PartFile::new(&target)?;
            f.write_all(&jpg).map_err(|e| format!("{}: {e}", part.part.display()))?;
            f.sync_all().map_err(|e| format!("{}: {e}", part.part.display()))?;
            bytes = bytes.saturating_add(jpg.len());
            parts.push(part);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        if parts.is_empty() {
            return Err("no slide could be rendered".into());
        }
        let files: Vec<String> = parts.iter().map(|p| p.target.to_string_lossy().to_string()).collect();
        // all written: now they take their names (a failure here removes the rest)
        for part in parts {
            part.commit(&self.guard)?;
        }
        progress(total, total, "");
        let mut out = self.summary(Format::Jpeg, files.len(), skipped, warnings);
        out["files"] = json!(files);
        out["bytes"] = json!(bytes);
        Ok(Outcome { json: out })
    }

    fn run_video(mut self, progress: Progress, cancel: &AtomicBool) -> Result<Outcome, String> {
        let st = self.request.settings.clone();
        let container = Container::parse(&st.container).unwrap_or(Container::Mp4);
        let (w, h) = (self.width, self.height);
        let fps = st.fps;
        let mut enc = Av1Encoder::new(&VideoConfig { width: w as u32, height: h as u32, fps, quality: st.quality, speed: st.speed })
            .map_err(|e| e.to_string())?;
        let target = PathBuf::from(&self.request.path);
        let (part, file) = PartFile::new(&target)?;
        let mut mux = VideoWriter::new(BufWriter::new(file), container, w as u32, h as u32, fps).map_err(|e| e.to_string())?;
        let total = self.total();
        let mut warnings = std::mem::take(&mut self.warnings);
        let mut skipped = 0;
        let mut slides_done = 0;
        let mut samples = 0u64;
        let mut keys = 0u64;
        // frames emitted so far, and the slideshow time (seconds) of the slides kept
        let mut frames = 0u64;
        let mut clock = 0.0f64;
        let mut prev: Option<Rgba8> = None;
        let mut emit = |mux: &mut VideoWriter<BufWriter<std::fs::File>>,
                        enc: &mut Av1Encoder,
                        pixels: &[[u8; 4]],
                        periods: u64,
                        key: bool|
         -> Result<(), String> {
            let out = enc.push(pixels, periods, key).map_err(|e| e.to_string())?;
            for s in out {
                if let Some(c) = enc.config_record() {
                    mux.set_config(c);
                }
                keys += u64::from(s.key);
                samples += 1;
                mux.add(&s).map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let fade_color = self.fade_color;
        let slides = std::mem::take(&mut self.slides);
        for slide in slides {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            let label = slide.label();
            progress(frames, total, &label);
            let (duration, fade) = (slide.duration, slide.fade);
            let img = match slide.render(w, h) {
                Ok(img) => img,
                Err(e) => {
                    warnings.push(format!("{label}: {e}; skipped"));
                    skipped += 1;
                    continue;
                }
            };
            let start = (clock * f64::from(fps)).round() as u64;
            clock += duration;
            let end = ((clock * f64::from(fps)).round() as u64).max(start + 1);
            let n = end - start;
            let fade_frames = match &prev {
                Some(_) => ((fade * f64::from(fps)).round() as u64).min(n),
                None => 0,
            };
            if let Some(a) = &prev {
                for j in 0..fade_frames {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(CANCELLED.into());
                    }
                    let k = (j + 1) as f32 / (fade_frames + 1) as f32;
                    emit(&mut mux, &mut enc, &blend(a, &img, k, fade_color), 1, false)?;
                    frames += 1;
                    progress(frames, total, &label);
                }
            }
            if n > fade_frames {
                // a held slide is a key frame: clean of the slide before it, and a place to seek to
                emit(&mut mux, &mut enc, &img.data, n - fade_frames, true)?;
                frames += n - fade_frames;
            }
            slides_done += 1;
            prev = Some(img);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        if slides_done == 0 {
            return Err("no slide could be rendered".into());
        }
        progress(frames, total, "AV1");
        for s in enc.finish().map_err(|e| e.to_string())? {
            if let Some(c) = enc.config_record() {
                mux.set_config(c);
            }
            keys += u64::from(s.key);
            samples += 1;
            mux.add(&s).map_err(|e| e.to_string())?;
        }
        let periods = mux.periods();
        let file = mux.finish().map_err(|e| e.to_string())?;
        let file = file.into_inner().map_err(|e| format!("video: {}", e.error()))?;
        file.sync_all().map_err(|e| format!("video: {e}"))?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        drop(file);
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        part.commit(&self.guard)?;
        progress(total, total, "");
        let mut out = self.summary(Format::Video, slides_done, skipped, warnings);
        out["container"] = json!(container.id());
        out["codec"] = json!("av1");
        out["fps"] = json!(fps);
        out["frames"] = json!(periods);
        out["samples"] = json!(samples);
        out["keyFrames"] = json!(keys);
        out["seconds"] = json!(periods as f64 / f64::from(fps));
        out["bytes"] = json!(bytes);
        out["audio"] = json!(false);
        Ok(Outcome { json: out })
    }
}

/// A slideshow export running in the background (`slideshow.export`; `slideshow.exportStatus`).
#[derive(Debug, Default)]
pub struct ExportRun {
    /// Its row in the activity stack (`activity.cancel {id}` stops it).
    pub task_id: u64,
    pub format: String,
    pub path: String,
    pub total: std::sync::atomic::AtomicU64,
    pub done: std::sync::atomic::AtomicU64,
    pub cancel: Arc<AtomicBool>,
    pub finished: AtomicBool,
    /// The front end has told the user how it ended.
    pub reported: AtomicBool,
    pub result: std::sync::Mutex<Option<Result<Value, String>>>,
}

impl ExportRun {
    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    pub fn result(&self) -> Option<Result<Value, String>> {
        self.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn json(&self) -> Value {
        let mut out = json!({
            "taskId": self.task_id,
            "format": self.format,
            "path": self.path,
            "running": self.running(),
            "done": self.done.load(Ordering::Relaxed),
            "total": self.total.load(Ordering::Relaxed),
            "cancelled": self.cancel.load(Ordering::Relaxed),
        });
        match self.result() {
            Some(Ok(v)) => out["result"] = v,
            Some(Err(e)) => out["error"] = json!(e),
            None => {}
        }
        out
    }
}

/// Run `prepared` to the end, recording progress and the outcome in `run` (and the row).
pub fn run_to_end(prepared: Prepared, run: &ExportRun, task: &crate::activity::TaskGuard) {
    let handle = task.handle();
    let progress = |done: u64, total: u64, detail: &str| {
        run.done.store(done, Ordering::Relaxed);
        run.total.store(total, Ordering::Relaxed);
        handle.progress(done, total);
        handle.detail(detail);
    };
    // a panic in a renderer or the encoder is an error, never a crash (the part file goes with it)
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepared.run(&progress, &run.cancel)))
        .unwrap_or_else(|_| Err("slideshow export failed unexpectedly".into()))
        .map(|o| o.json);
    *run.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
    run.finished.store(true, Ordering::Relaxed);
}
