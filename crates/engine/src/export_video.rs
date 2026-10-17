//! Video clips in an export batch (M14.8).
//!
//! A clip in a batch exported to a rendered format (JPEG, PNG…) is exported as a movie instead:
//! its trim (`Photo.video.trim_in` / `trim_out`), its develop settings reduced to the video
//! controls ([`lcc_pipeline::grade::video_settings`]), graded at full quality on every frame by
//! the photo pipeline, and encoded by `lcc-video-export` as H.264 MP4 or ProRes MOV
//! ([`VideoFormat`]). Photos in the same batch export as before. Exporting a clip as
//! [`crate::export::ExportFormat::Original`] copies its file.
//!
//! The movie is written to a temporary file by [`crate::export::PreparedExport::run`] (so it can
//! run beside the batch's other items) and moved to its place in batch order; it never sits in
//! memory. Cancel and per-frame progress come from [`VideoHooks`].
//!
//! Sources: own design (reuses `lcc-video-export` and the pipeline's video grade).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

/// The movie format clips in an export are written as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoFormat {
    /// H.264 High 8-bit 4:2:0 + AAC in MP4.
    #[default]
    #[serde(rename = "h264")]
    H264,
    /// ProRes 422 10-bit (HQ at quality ≥ 80, Standard ≥ 50, else LT) + AAC in MOV.
    #[serde(rename = "prores")]
    ProRes,
}

impl VideoFormat {
    pub const NAMES: &'static [&'static str] = &["h264", "prores"];

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "h264" | "mp4" => Some(Self::H264),
            "prores" | "mov" => Some(Self::ProRes),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::H264 => "mp4",
            Self::ProRes => "mov",
        }
    }
}

/// Smallest and largest `videoMaxSide` (0 = the clip's size).
pub const VIDEO_MAX_SIDE_RANGE: (u32, u32) = (16, 8192);

/// Per-clip progress: `(file name, fraction 0..=1)`.
pub type VideoProgress = Arc<dyn Fn(&str, f64) + Send + Sync>;

/// How a batch's clips are stopped and report progress: `cancel` is the flag the batch's activity
/// row sets (`Cancel::Flag`), checked once per frame.
#[derive(Clone)]
pub struct VideoHooks {
    pub cancel: Arc<AtomicBool>,
    pub progress: Option<VideoProgress>,
}

impl Default for VideoHooks {
    fn default() -> Self {
        Self { cancel: Arc::new(AtomicBool::new(false)), progress: None }
    }
}

/// One clip's export, set up from the session.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct VideoWork {
    pub src: PathBuf,
    pub trim: (f64, Option<f64>),
    pub settings: lcc_develop::DevelopSettings,
    pub format: VideoFormat,
    pub quality: u8,
    pub max_side: Option<u32>,
    pub file_name: String,
    pub hooks: VideoHooks,
}

/// A finished movie in a temporary file, removed when dropped unless it was moved into place.
pub struct TempFile(pub PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl TempFile {
    /// Move the file to `dest` (its folder made if needed): a rename, else a copy (another volume).
    pub fn move_to(&self, dest: &str) -> Result<(), String> {
        let d = Path::new(dest);
        if let Some(dir) = d.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if std::fs::rename(&self.0, d).is_ok() {
            return Ok(());
        }
        // copy beside the destination, then rename: a failure never leaves a truncated movie
        let part = PathBuf::from(format!("{dest}.partial"));
        let r = std::fs::copy(&self.0, &part)
            .map_err(|e| format!("{dest}: {e}"))
            .and_then(|_| std::fs::rename(&part, d).map_err(|e| format!("{dest}: {e}")));
        if r.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        r
    }

    pub fn len(&self) -> u64 {
        std::fs::metadata(&self.0).map_or(0, |m| m.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a clip export produced.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct VideoOut {
    pub file: TempFile,
    pub width: usize,
    pub height: usize,
}

#[cfg(not(target_arch = "wasm32"))]
fn temp_path(ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("video-export-{}-{n}.{ext}", std::process::id()))
}

/// Grade one linear frame in place with the clip's (video-reduced) settings at full quality.
/// The frame stays linear Rec.709 (the export crate clamps and sRGB-encodes it afterwards).
pub fn grade_linear(frame: &mut lcc_raster::Rgb32f, settings: &lcc_develop::DevelopSettings) -> Result<(), String> {
    let out = lcc_pipeline::grade::grade_frame_linear(frame, settings, lcc_pipeline::Quality::Full).ok_or("the grade produced no float frame")?;
    if (out.width, out.height) != (frame.width, frame.height) {
        return Err(format!("the grade changed the frame size ({}×{} → {}×{})", frame.width, frame.height, out.width, out.height));
    }
    *frame = out;
    Ok(())
}

/// Grade one linear frame on the GPU (8-bit sRGB output decoded back to linear). `None` when the
/// GPU did not render it (unavailable, switched off or failed): grade on the CPU then. Used for
/// 8-bit H.264 only, where the encoder quantises to 8 bits anyway (within one code value of the
/// CPU float grade); 10-bit ProRes always takes the float CPU path.
pub fn grade_gpu(frame: &mut lcc_raster::Rgb32f, video_settings: &lcc_develop::DevelopSettings) -> Option<()> {
    use lcc_pipeline::grade;
    let req = grade::grade_request(frame.width, frame.height, lcc_pipeline::Quality::Full);
    let src = Arc::new(std::mem::replace(frame, lcc_raster::Rgb32f::new(0, 0)));
    let r = lcc_gpu::try_render(&src, &grade::video_source_info(), video_settings, &req, None);
    *frame = Arc::try_unwrap(src).unwrap_or_else(|a| (*a).clone());
    let img = r.ok()?.image;
    if (img.width, img.height) != (frame.width, frame.height) || img.data.len() != frame.data.len() {
        return None;
    }
    let lut = srgb8_lut();
    for (d, p) in frame.data.iter_mut().zip(&img.data) {
        *d = [lut[usize::from(p[0])], lut[usize::from(p[1])], lut[usize::from(p[2])]];
    }
    Some(())
}

fn srgb8_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| lcc_color::transfer::srgb_to_linear(i as f32 / 255.0)))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run(w: VideoWork) -> Result<VideoOut, String> {
    use lcc_video_export::{ClipExportOptions, ClipFormat, Reframe, export_clip_framed};
    let format = match w.format {
        VideoFormat::H264 => ClipFormat::H264Mp4,
        VideoFormat::ProRes => ClipFormat::ProResMov,
    };
    let tmp = temp_path(format.extension());
    let opts = ClipExportOptions { format, quality: w.quality, max_side: w.max_side, out: tmp.clone() };
    // a default grade is the identity: skip the pipeline
    // the clip's settings are reduced once, not per frame
    let video_settings = lcc_pipeline::grade::video_settings(&w.settings);
    let neutral = video_settings == lcc_pipeline::grade::video_settings(&lcc_develop::DevelopSettings::default());
    let gpu = w.format == VideoFormat::H264 && lcc_gpu::enabled() && lcc_gpu::available();
    let failed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let grade = |f: &mut lcc_raster::Rgb32f| {
        if neutral {
            return;
        }
        if gpu && grade_gpu(f, &video_settings).is_some() {
            return;
        }
        if let Err(e) = grade_linear(f, &video_settings) {
            let mut g = failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            g.get_or_insert(e);
        }
    };
    let name = w.file_name.clone();
    let progress = |x: f64| {
        if let Some(p) = &w.hooks.progress {
            p(&name, x);
        }
    };
    // the crop (straighten, flips) is drawn from the decoded frame before the grade
    let framing = lcc_pipeline::grade::video_framing(&w.settings);
    let sample = |f: &lcc_raster::Rgb32f, ow: usize, oh: usize| lcc_pipeline::grade::frame_video(f, &framing, ow, oh);
    let reframe = if lcc_pipeline::grade::video_is_framed(&w.settings) {
        let info = lcc_video::probe(&w.src).map_err(|e| format!("{}: {e}", w.file_name))?;
        let size = lcc_pipeline::native_output_size(info.width as usize, info.height as usize, &framing);
        Some(Reframe { size, sample: &sample })
    } else {
        None
    };
    let r = export_clip_framed(&w.src, w.trim, reframe.as_ref(), &grade, &opts, &w.hooks.cancel, &progress);
    let file = TempFile(tmp);
    let rep = r.map_err(|e| format!("{}: {e}", w.file_name))?;
    if let Some(e) = failed.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        return Err(format!("{}: {e}", w.file_name));
    }
    Ok(VideoOut { file, width: rep.width as usize, height: rep.height as usize })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn run(w: VideoWork) -> Result<VideoOut, String> {
    Err(format!("{}: video export is not available in the web build", w.file_name))
}
