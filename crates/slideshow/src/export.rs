//! Export a slideshow as a JPEG sequence: one composed slide per photo (plus the intro and ending
//! screens when they are on), named `<name>-001.jpg` … in show order; or as one PDF, `<name>.pdf`,
//! with a page per slide (the same JPEG frames embedded through `lcc-pdf`, page size = the slide
//! at 96 px per inch). Rendering goes through the engine's export renderer, encoding through
//! `lcc_engine::export` (the one encoder of the app).
//!
//! Or as a movie, `<name>.mp4` (H.264 + AAC) or `<name>.mov` (ProRes 422 + PCM): one pass of the
//! show at the chosen frame rate, with the timing, transitions, pan and zoom of playback and the
//! music (mixed with its volume and balance) as the sound track, encoded by `lcc-video-export`
//! (the encoder of clip and timelapse export; native builds only).
//!
//! Sources: own design (frames sampled from [`Plan::frame_at`], the playback timeline).

use std::path::{Path, PathBuf};

use lcc_catalog::PhotoId;
use lcc_engine::export::{ExportFormat, ExportOptions, encode_image, write_file};
use lcc_engine::{RenderJob, Session};
use lcc_pipeline::{OutputDepth, OutputSpace};
use lcc_raster::Rgba8;

use crate::compose::{MAX_EDGE, compose_slide, compose_title, geometry};
use crate::settings::Settings;
use crate::timeline::{Plan, Segment};
use crate::tokens::SlideInfo;

/// Most photos one export takes.
pub const MAX_PHOTOS: usize = 5000;

/// Decode a backdrop image file (any format the app reads), fitted within 4096 px.
pub fn load_background(path: &str) -> Result<Rgba8, String> {
    if path.trim().is_empty() {
        return Err("no backdrop image".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > 256 * 1024 * 1024 {
        return Err(format!("{path}: larger than 256 MiB"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    lcc_codecs::decode_thumbnail(&bytes, 4096).map(|t| t.image).map_err(|e| format!("{path}: not a usable image: {e}"))
}

/// The render size a photo needs in a `w × h` slide: its drawn rectangle (larger when filling).
pub fn render_box(w: usize, h: usize, pw: u32, ph: u32, s: &Settings) -> (usize, usize) {
    let g = geometry(w, h, pw as usize, ph as usize, s);
    let zoom = if s.playback.pan_zoom { 1.0 + 0.3 * s.playback.pan_zoom_amount } else { 1.0 };
    // the photo's orientation may swap its sides: ask for a square box of the long edge
    let side = (g.photo.w().max(g.photo.h()) * zoom).ceil().clamp(16.0, MAX_EDGE as f32) as usize;
    (side, side)
}

/// One slide to render.
struct Item {
    info: SlideInfo,
    job: RenderJob,
}

/// An export ready to run off the UI thread.
pub struct Prepared {
    settings: Settings,
    width: usize,
    height: usize,
    quality: u8,
    dir: PathBuf,
    name: String,
    plate: String,
    segments: Vec<Segment>,
    items: Vec<Item>,
    background: Option<Rgba8>,
    /// One PDF instead of a JPEG sequence.
    pdf: bool,
    /// A movie instead of a JPEG sequence.
    video: Option<VideoSpec>,
    /// Seconds each segment holds and fades (movies).
    timing: (f64, f64),
}

/// Movie container and codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFormat {
    /// H.264 + AAC in MP4.
    H264,
    /// ProRes 422 + 16-bit PCM in MOV.
    ProRes,
}

impl VideoFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "h264" | "mp4" => Ok(Self::H264),
            "prores" | "mov" => Ok(Self::ProRes),
            other => Err(format!("unknown video format “{other}” (h264 or prores)")),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::H264 => "mp4",
            Self::ProRes => "mov",
        }
    }
}

/// A movie export's settings.
#[derive(Clone, Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct VideoSpec {
    format: VideoFormat,
    fps: u32,
    /// The mixed music (interleaved stereo), when the show has music.
    music: Option<crate::music::Audio>,
    /// Why the music was left out, if it was.
    warning: Option<String>,
}

fn safe_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let n = n.trim().to_string();
    if n.is_empty() { "Slideshow".into() } else { n.chars().take(80).collect() }
}

/// Prepare a JPEG-sequence export of `ids` at `width × height` into `dir`.
pub fn prepare(
    session: &mut Session,
    ids: &[PhotoId],
    settings: &Settings,
    width: usize,
    height: usize,
    quality: u8,
    dir: &Path,
    name: &str,
    plate: &str,
) -> Result<Prepared, String> {
    if ids.is_empty() {
        return Err("no photos in the slideshow".into());
    }
    if ids.len() > MAX_PHOTOS {
        return Err(format!("a slideshow export takes at most {MAX_PHOTOS} photos"));
    }
    if width < 16 || height < 16 || width > MAX_EDGE || height > MAX_EDGE {
        return Err(format!("slide size must be 16…{MAX_EDGE} px a side"));
    }
    let settings = settings.clone().sanitized();
    // the sequence has no timing, but the order (random or not) and the title screens apply
    let mut ordered = settings.clone();
    ordered.music.fit_to_music = false;
    let plan = Plan::new(ids.len(), &ordered, 0x5eed, None);
    let mut items = Vec::with_capacity(ids.len());
    for (seq, id) in ids.iter().enumerate() {
        let p = session.catalog.photo(*id).ok_or("no such photo")?;
        let info = SlideInfo::from_photo(p, seq + 1, ids.len());
        let (bw, bh) = render_box(width, height, p.width, p.height, &settings);
        let job = session.export_job(*id, bw, bh, OutputSpace::Srgb, OutputDepth::U8)?;
        items.push(Item { info, job });
    }
    let background = if settings.backdrop.image.trim().is_empty() { None } else { Some(load_background(&settings.backdrop.image)?) };
    Ok(Prepared {
        settings,
        width,
        height,
        quality: quality.clamp(1, 100),
        dir: dir.to_path_buf(),
        name: safe_name(name),
        plate: plate.to_string(),
        segments: plan.segments,
        items,
        background,
        pdf: false,
        video: None,
        timing: (plan.hold, plan.fade),
    })
}

/// Most seconds a slideshow movie runs.
pub const MAX_VIDEO_SECONDS: f64 = 3.0 * 60.0 * 60.0;

/// Decode and join the show's music tracks (all at the first one's rate), mixed with volume and
/// balance. `Err` names why there is no music track.
pub fn mixed_music(s: &Settings) -> Result<Option<crate::music::Audio>, String> {
    let m = &s.music;
    if !m.enabled || m.tracks.is_empty() {
        return Ok(None);
    }
    let mut all = crate::music::Audio::default();
    for t in &m.tracks {
        let a = crate::music::decode(Path::new(t))?;
        if all.rate != 0 && a.rate != all.rate {
            return Err(format!("{t}: a different sample rate than the first track"));
        }
        all.rate = a.rate;
        all.samples.extend(a.samples);
    }
    crate::music::mix(&mut all.samples, m.volume, m.balance);
    Ok(Some(all))
}

/// [`prepare`] for a movie, `<dir>/<name>.mp4|.mov`, of one pass of the show at `fps` (the size
/// is rounded down to even). The music is decoded here; music that can't be read leaves the movie
/// silent, and [`Prepared::warning`] says why.
#[allow(clippy::too_many_arguments)]
pub fn prepare_video(
    session: &mut Session,
    ids: &[PhotoId],
    settings: &Settings,
    width: usize,
    height: usize,
    quality: u8,
    fps: u32,
    format: VideoFormat,
    dir: &Path,
    name: &str,
    plate: &str,
) -> Result<Prepared, String> {
    if !(1..=60).contains(&fps) {
        return Err("frame rate must be 1…60".into());
    }
    let (width, height) = (width & !1, height & !1);
    let mut p = prepare(session, ids, settings, width, height, quality, dir, name, plate)?;
    let (music, warning) = match mixed_music(&p.settings) {
        Ok(m) => (m, None),
        Err(e) => (None, Some(e)),
    };
    // one pass, timed like playback (fit to music included)
    let mut once = p.settings.clone();
    once.playback.repeat = false;
    let secs = music.as_ref().map(crate::music::Audio::seconds);
    let plan = Plan::new(ids.len(), &once, 0x5eed, secs);
    if plan.duration() > MAX_VIDEO_SECONDS {
        return Err(format!("a slideshow movie runs at most {} hours", MAX_VIDEO_SECONDS / 3600.0));
    }
    p.segments = plan.segments;
    p.timing = (plan.hold, plan.fade);
    p.video = Some(VideoSpec { format, fps, music, warning });
    Ok(p)
}

/// A composed slide as encoded R'G'B' 0..=1 (the movie writer's input).
#[cfg(not(target_arch = "wasm32"))]
fn to_rgb32f(img: &Rgba8) -> lcc_raster::Rgb32f {
    lcc_raster::Rgb32f::from_fn(img.width, img.height, |x, y| {
        let p = img.data.get(y * img.width + x).copied().unwrap_or([0; 4]);
        [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]
    })
}

/// [`prepare`] for a PDF: one page per frame, written as `<dir>/<name>.pdf`.
#[allow(clippy::too_many_arguments)]
pub fn prepare_pdf(
    session: &mut Session,
    ids: &[PhotoId],
    settings: &Settings,
    width: usize,
    height: usize,
    quality: u8,
    dir: &Path,
    name: &str,
    plate: &str,
) -> Result<Prepared, String> {
    let mut p = prepare(session, ids, settings, width, height, quality, dir, name, plate)?;
    p.pdf = true;
    Ok(p)
}

/// Points per pixel of a PDF slide (96 px per inch).
const PT_PER_PX: f64 = 0.75;

impl Prepared {
    /// Frames to write (slides plus title screens).
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Why a movie has no sound although the show has music.
    pub fn warning(&self) -> Option<&str> {
        self.video.as_ref().and_then(|v| v.warning.as_deref())
    }

    /// The movie's timeline: one pass, not repeating.
    fn plan(&self) -> Plan {
        Plan { segments: self.segments.clone(), hold: self.timing.0, fade: self.timing.1, repeat: false }
    }

    /// Frames a movie export writes (0 for stills).
    pub fn video_frames(&self) -> u64 {
        self.video.as_ref().map_or(0, |v| (self.plan().duration() * f64::from(v.fps)).ceil().max(1.0) as u64)
    }

    /// Write the movie: every frame sampled from the show's timeline, the music muxed in.
    #[cfg(not(target_arch = "wasm32"))]
    fn run_video(mut self, spec: VideoSpec, progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Vec<String>, String> {
        use lcc_video_export::{ClipFormat, SequenceWriter, encode_pcm_track};
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let format = match spec.format {
            VideoFormat::H264 => ClipFormat::H264Mp4,
            VideoFormat::ProRes => ClipFormat::ProResMov,
        };
        let plan = self.plan();
        let total = (plan.duration() * f64::from(spec.fps)).ceil().max(1.0) as u64;
        let secs = total as f64 / f64::from(spec.fps);
        let never = std::sync::atomic::AtomicBool::new(false);
        let audio = match &spec.music {
            Some(m) if m.rate > 0 && !m.samples.is_empty() => {
                // the music, cut or padded with silence to the movie's length
                let n = ((secs * f64::from(m.rate)).round() as usize).saturating_mul(2);
                let mut pcm = m.samples.clone();
                pcm.resize(n, 0.0);
                Some(encode_pcm_track(&pcm, 2, m.rate, format, &never).map_err(|e| e.to_string())?)
            }
            _ => None,
        };
        let out = self.dir.join(format!("{}.{}", self.name, spec.format.extension()));
        let mut writer = SequenceWriter::create_with_audio(&out, format, self.quality, self.width as u32, self.height as u32, spec.fps, 1, audio)
            .map_err(|e| e.to_string())?;
        let items = std::mem::take(&mut self.items);
        let infos: Vec<SlideInfo> = items.iter().map(|x| x.info.clone()).collect();
        let mut jobs: Vec<Option<RenderJob>> = items.into_iter().map(|x| Some(x.job)).collect();
        // rendered photos of the segments on screen, and composed stills while nothing moves
        let mut rendered: Vec<(usize, Rgba8)> = Vec::new();
        let mut still: Vec<(usize, Rgba8)> = Vec::new();
        let moving = self.settings.playback.pan_zoom;
        let mut last_seg = usize::MAX;
        for f in 0..total {
            let Some(fr) = plan.frame_at(f as f64 / f64::from(spec.fps)) else { break };
            if fr.index != last_seg {
                last_seg = fr.index;
                let label = match fr.current {
                    Segment::Intro => "Intro".to_string(),
                    Segment::Ending => "Ending".to_string(),
                    Segment::Slide(i) => infos.get(i).map(|x| x.filename.clone()).unwrap_or_default(),
                };
                if !progress(fr.index, &label) {
                    return Err("Slideshow export cancelled".into());
                }
                let keep: Vec<usize> = [fr.index, fr.index + 1]
                    .into_iter()
                    .filter_map(|k| match self.segments.get(k) {
                        Some(Segment::Slide(i)) => Some(*i),
                        _ => None,
                    })
                    .collect();
                rendered.retain(|(i, _)| keep.contains(i));
                still.retain(|(k, _)| *k >= fr.index);
            }
            let mut frame_of = |k: usize, at: f32| -> Result<Rgba8, String> {
                if !moving && let Some((_, img)) = still.iter().find(|(j, _)| *j == k) {
                    return Ok(img.clone());
                }
                let img = match self.segments.get(k).copied().ok_or("segment out of range")? {
                    Segment::Intro => compose_title(self.width, self.height, &self.settings.titles.intro, &self.plate)?,
                    Segment::Ending => compose_title(self.width, self.height, &self.settings.titles.ending, &self.plate)?,
                    Segment::Slide(i) => {
                        if !rendered.iter().any(|(j, _)| *j == i) {
                            let job = jobs.get_mut(i).and_then(Option::take).ok_or("slide out of range")?;
                            rendered.push((i, job.run().rendered?.image));
                        }
                        let photo = rendered.iter().find(|(j, _)| *j == i).map(|(_, p)| p);
                        let info = infos.get(i).cloned().unwrap_or_default();
                        let s = &self.settings;
                        let view = if s.playback.pan_zoom { crate::timeline::pan_zoom(k, at, s.playback.pan_zoom_amount) } else { (1.0, 0.5, 0.5) };
                        compose_slide(self.width, self.height, photo, &info, s, view, self.background.as_ref(), &self.plate)?
                    }
                };
                if !moving {
                    still.push((k, img.clone()));
                }
                Ok(img)
            };
            let a = frame_of(fr.index, fr.progress)?;
            let img = match fr.next {
                Some((_, mix)) if fr.index + 1 < self.segments.len() => {
                    let b = frame_of(fr.index + 1, 0.0)?;
                    crate::compose::transition(&a, &b, mix, &self.settings)
                }
                _ => a,
            };
            writer.push_encoded(&to_rgb32f(&img)).map_err(|e| e.to_string())?;
        }
        let rep = writer.finish().map_err(|e| e.to_string())?;
        progress(self.segments.len(), "");
        Ok(vec![rep.path.display().to_string()])
    }

    #[cfg(target_arch = "wasm32")]
    fn run_video(self, _spec: VideoSpec, _progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Vec<String>, String> {
        Err("slideshow video export is not available in the web build".into())
    }

    /// Render, compose and write every frame; `progress(done, label)` returning false cancels
    /// (frames already written stay). Returns the written paths.
    pub fn run(mut self, progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Vec<String>, String> {
        if let Some(spec) = self.video.take() {
            return self.run_video(spec, progress);
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let o = ExportOptions { format: ExportFormat::Jpeg, quality: self.quality, ..Default::default() };
        let mut items: Vec<Option<Item>> = self.items.into_iter().map(Some).collect();
        let mut written = Vec::with_capacity(self.segments.len());
        let mut doc = self.pdf.then(|| {
            let mut d = lcc_pdf::Document::new();
            d.metadata.title = Some(self.name.clone());
            d.metadata.creator = Some(lcc_brand::DISPLAY_NAME.to_string());
            d
        });
        let page_size = (
            (self.width as f64 * PT_PER_PX).clamp(lcc_pdf::MIN_SIDE, lcc_pdf::MAX_SIDE),
            (self.height as f64 * PT_PER_PX).clamp(lcc_pdf::MIN_SIDE, lcc_pdf::MAX_SIDE),
        );
        for (k, seg) in self.segments.iter().enumerate() {
            let label = match seg {
                Segment::Intro => "Intro".to_string(),
                Segment::Ending => "Ending".to_string(),
                Segment::Slide(i) => items.get(*i).and_then(|x| x.as_ref()).map(|x| x.info.filename.clone()).unwrap_or_default(),
            };
            if !progress(k, &label) {
                return Err("Slideshow export cancelled".into());
            }
            let img = match seg {
                Segment::Intro => compose_title(self.width, self.height, &self.settings.titles.intro, &self.plate)?,
                Segment::Ending => compose_title(self.width, self.height, &self.settings.titles.ending, &self.plate)?,
                Segment::Slide(i) => {
                    let item = items.get_mut(*i).and_then(Option::take).ok_or("slide out of range")?;
                    let photo = item.job.run().rendered?.image;
                    compose_slide(
                        self.width,
                        self.height,
                        Some(&photo),
                        &item.info,
                        &self.settings,
                        (1.0, 0.5, 0.5),
                        self.background.as_ref(),
                        &self.plate,
                    )?
                }
            };
            let bytes = encode_image(&img, &o)?;
            if let Some(d) = doc.as_mut() {
                let image = d.add_image(lcc_pdf::Image::jpeg(bytes, lcc_pdf::ColorSpace::Rgb)).map_err(|e| format!("PDF: {e}"))?;
                let mut page = lcc_pdf::Page::new(page_size.0, page_size.1);
                page.image(image, lcc_pdf::Rect::new(0.0, 0.0, page_size.0, page_size.1));
                d.push_page(page).map_err(|e| format!("PDF: {e}"))?;
                continue;
            }
            let path = self.dir.join(format!("{}-{:03}.jpg", self.name, k + 1));
            let path_s = path.display().to_string();
            write_file(&path_s, &bytes)?;
            written.push(path_s);
        }
        if let Some(d) = doc {
            let bytes = d.to_bytes().map_err(|e| format!("PDF: {e}"))?;
            let path = self.dir.join(format!("{}.pdf", self.name)).display().to_string();
            write_file(&path, &bytes)?;
            written.push(path);
        }
        progress(self.segments.len(), "");
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "______etc_passwd");
        assert_eq!(safe_name("  "), "Slideshow");
        assert_eq!(safe_name("Trip 2026"), "Trip 2026");
    }

    #[test]
    fn exports_a_demo_library_as_jpegs() {
        let mut session = Session::with_demo();
        let ids: Vec<PhotoId> = session.catalog.photos().take(2).map(|p| p.id).collect();
        assert_eq!(ids.len(), 2);
        let mut s = Settings::default();
        s.titles.intro.enabled = true;
        s.titles.intro.text = "Hello".into();
        s.overlays.rating = true;
        let dir = std::env::temp_dir().join(format!("lcc-slideshow-export-{}", std::process::id()));
        let p = prepare(&mut session, &ids, &s, 320, 180, 85, &dir, "Show", "Plate").unwrap();
        assert_eq!(p.len(), 3);
        let files = p.run(&mut |_, _| true).unwrap();
        assert_eq!(files.len(), 3);
        for f in &files {
            let b = std::fs::read(f).unwrap();
            assert_eq!(&b[..2], &[0xFF, 0xD8], "a JPEG");
        }
        assert!(files[0].ends_with("Show-001.jpg"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exports_a_pdf_with_a_page_per_frame() {
        let mut session = Session::with_demo();
        let ids: Vec<PhotoId> = session.catalog.photos().take(2).map(|p| p.id).collect();
        let mut s = Settings::default();
        s.titles.ending.enabled = true;
        s.titles.ending.text = "The end".into();
        let dir = std::env::temp_dir().join(format!("lcc-slideshow-pdf-{}", std::process::id()));
        let p = prepare_pdf(&mut session, &ids, &s, 320, 180, 85, &dir, "Show", "Plate").unwrap();
        assert_eq!(p.len(), 3);
        let files = std::thread::spawn(move || p.run(&mut |_, _| true)).join().unwrap().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("Show.pdf"));
        let b = std::fs::read(&files[0]).unwrap();
        assert!(b.starts_with(b"%PDF-"));
        let text = String::from_utf8_lossy(&b);
        assert!(text.contains("/Count 3"), "three pages");
        assert!(text.contains("/MediaBox [0 0 240 135]"), "320×180 px at 96 ppi");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exports_a_movie_with_transitions_and_music() {
        let mut session = Session::with_demo();
        let ids: Vec<PhotoId> = session.catalog.photos().take(2).map(|p| p.id).collect();
        let dir = std::env::temp_dir().join(format!("lcc-slideshow-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("m.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..44_100 * 2 {
            w.write_sample(((i as f32 * 0.03).sin() * 8000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut s = Settings::default();
        s.playback.slide_secs = 1.0;
        s.playback.fade_secs = 0.5;
        s.playback.pan_zoom = true;
        s.playback.repeat = true; // a movie is still one pass
        s.music.enabled = true;
        s.music.tracks = vec![wav.display().to_string()];
        for (format, ext, music) in [(VideoFormat::H264, "mp4", true), (VideoFormat::ProRes, "mov", false)] {
            let mut s = s.clone();
            s.music.enabled = music;
            // odd sizes round down to even
            let p = prepare_video(&mut session, &ids, &s, 161, 91, 70, 10, format, &dir, "Show", "Plate").unwrap();
            assert!(p.warning().is_none());
            // 2 × 1 s + one 0.5 s fade = 2.5 s at 10 fps
            assert_eq!(p.video_frames(), 25);
            let files = std::thread::spawn(move || p.run(&mut |_, _| true)).join().unwrap().unwrap();
            assert!(files[0].ends_with(&format!("Show.{ext}")), "{files:?}");
            let r = lcc_video::FrameReader::open(std::path::Path::new(&files[0])).unwrap();
            assert_eq!(r.frame_times().len(), 25);
            assert_eq!(lcc_video::AudioReader::open(std::path::Path::new(&files[0])).is_ok(), music, "{ext}");
        }
        // unreadable music: a silent movie and a warning
        let mut bad = s.clone();
        bad.music.tracks = vec![dir.join("missing.flac").display().to_string()];
        let p = prepare_video(&mut session, &ids, &bad, 160, 90, 70, 10, VideoFormat::H264, &dir, "Silent", "").unwrap();
        assert!(p.warning().is_some());
        assert!(prepare_video(&mut session, &ids, &s, 160, 90, 70, 0, VideoFormat::H264, &dir, "x", "").is_err());
        assert!(VideoFormat::parse("avi").is_err());
        assert_eq!(VideoFormat::parse("ProRes").unwrap(), VideoFormat::ProRes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_requests_are_errors() {
        let mut session = Session::with_demo();
        let s = Settings::default();
        let dir = std::env::temp_dir();
        assert!(prepare(&mut session, &[], &s, 320, 180, 85, &dir, "x", "").is_err());
        let id = session.catalog.photos().next().map(|p| p.id).unwrap();
        assert!(prepare(&mut session, &[id], &s, 1, 180, 85, &dir, "x", "").is_err());
        assert!(prepare(&mut session, &[PhotoId(u64::MAX)], &s, 320, 180, 85, &dir, "x", "").is_err());
        let mut bad = s.clone();
        bad.backdrop.image = "/nonexistent/backdrop.png".into();
        assert!(prepare(&mut session, &[id], &bad, 320, 180, 85, &dir, "x", "").is_err());
    }
}
