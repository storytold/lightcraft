//! Export a slideshow as a JPEG sequence: one composed slide per photo (plus the intro and ending
//! screens when they are on), named `<name>-001.jpg` … in show order. Rendering goes through the
//! engine's export renderer, encoding through `dac_engine::export` (the one encoder of the app).
//!
//! PDF and video export come later (PLAN phase 3.4).

use std::path::{Path, PathBuf};

use dac_catalog::PhotoId;
use dac_engine::export::{ExportFormat, ExportOptions, encode_image, write_file};
use dac_engine::{RenderJob, Session};
use dac_pipeline::{OutputDepth, OutputSpace};
use dac_raster::Rgba8;

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
    dac_codecs::decode_thumbnail(&bytes, 4096).map(|t| t.image).map_err(|e| format!("{path}: not a usable image: {e}"))
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
    })
}

impl Prepared {
    /// Frames to write (slides plus title screens).
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Render, compose and write every frame; `progress(done, label)` returning false cancels
    /// (frames already written stay). Returns the written paths.
    pub fn run(self, progress: &mut dyn FnMut(usize, &str) -> bool) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let o = ExportOptions { format: ExportFormat::Jpeg, quality: self.quality, ..Default::default() };
        let mut items: Vec<Option<Item>> = self.items.into_iter().map(Some).collect();
        let mut written = Vec::with_capacity(self.segments.len());
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
            let path = self.dir.join(format!("{}-{:03}.jpg", self.name, k + 1));
            let path_s = path.display().to_string();
            write_file(&path_s, &bytes)?;
            written.push(path_s);
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
        let dir = std::env::temp_dir().join(format!("dac-slideshow-export-{}", std::process::id()));
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
