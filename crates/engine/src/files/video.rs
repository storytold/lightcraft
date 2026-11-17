//! Video poster frames from files: what a catalogued clip's grid tile, filmstrip cell and loupe
//! show ([`lightcraft_codecs::poster`]), decoded from the file in place (never read whole: a clip
//! can be gigabytes).
//!
//! Decoding needs `lightcraft-codecs`' feature `video-frames` (the apps' own feature
//! `video-frames` turns it on); without it [`crate::Session::with_fs`] installs no video loader, so
//! a clip gets no render jobs and keeps its card. While any poster decodes, the activity stack has
//! one row for all of them ("Reading video frames", done of the burst's total); its ✕ stops the
//! decodes in flight (and those waiting for memory), which fail with [`VIDEO_STOPPED`]; a decode
//! that starts after it gets a fresh row.

use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_codecs::poster::{PosterOptions, poster_frame};
#[cfg(not(target_arch = "wasm32"))]
use lightcraft_codecs::video_stream::FileSource;
use lightcraft_codecs::video_stream::{StreamOptions, VideoError, VideoStream};
use lightcraft_pipeline::SourceInfo;

use crate::activity::{Activity, Cancel, TaskGuard};
use crate::media::{FileLoader, RawDecode, SourceLevel};

/// The activity row's kind ([`crate::activity`]).
pub const VIDEO_FRAMES_KIND: &str = "videoFrames";
/// The activity row's label (translated by the frontend).
pub const VIDEO_FRAMES_LABEL: &str = "Reading video frames";
/// What a poster decode the row's ✕ stopped fails with: not a lasting failure (the renderer asks
/// again, a preview build doesn't count it as failed).
pub const VIDEO_STOPPED: &str = "Stopped";

/// Pictures a decoder may hold at once (H.264 / HEVC reference and reorder pictures).
const HELD_PICTURES: u64 = 16;

/// What one poster decode of a `width` × `height` clip counts for at
/// [`crate::memory::work_gate`]: the decoder's pictures (4:2:0, 1.5 bytes a pixel each; at 4K 16
/// of 12 MB) and the converted picture in linear float (12 bytes a pixel, at most the poster's
/// conversion budget): about 300 MB at 4K, 1 GB at 8K.
fn poster_weight(width: u32, height: u32) -> usize {
    let px = u64::from(width).saturating_mul(u64::from(height));
    let converted = px.min(PosterOptions::default().max_convert_pixels as u64);
    let bytes = px.saturating_mul(HELD_PICTURES).saturating_mul(3) / 2 + converted.saturating_mul(12);
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// Decoder threads for a poster the loupe waits for; a background thumbnail decodes on one
/// thread, since the grid already renders several at once.
const INTERACTIVE_THREADS: usize = 4;

/// The shared row and its burst's counts.
#[derive(Default)]
struct Row {
    guard: Option<TaskGuard>,
    cancel: Arc<AtomicBool>,
    /// Which row this is: a ✕ ends it, and the decodes still finishing count for it, not the next.
    generation: u64,
    running: u64,
    done: u64,
}

/// The posters in flight, with their row in the activity stack.
#[derive(Clone)]
pub struct VideoFrames {
    activity: Activity,
    row: Arc<Mutex<Row>>,
}

/// One poster decode's place in the row; dropping it counts the decode done.
struct Ticket {
    row: Arc<Mutex<Row>>,
    generation: u64,
}

impl VideoFrames {
    pub fn new(activity: Activity) -> VideoFrames {
        VideoFrames { activity, row: Arc::default() }
    }

    /// Count a decode in, starting the row when it is the first, or when the row's ✕ was pressed
    /// (the stopped decodes finish under the old flag; this one must not inherit it); the flag
    /// stops it.
    fn enter(&self) -> (Ticket, Arc<AtomicBool>) {
        let mut row = self.row.lock().unwrap_or_else(PoisonError::into_inner);
        if row.guard.is_none() || row.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            // a fresh flag per row: a ✕ on the last one doesn't stop the next decode
            row.cancel = Arc::new(AtomicBool::new(false));
            row.generation = row.generation.wrapping_add(1);
            (row.running, row.done) = (0, 0);
            row.guard = Some(self.activity.start(VIDEO_FRAMES_KIND, VIDEO_FRAMES_LABEL, Cancel::Flag(row.cancel.clone())));
        }
        row.running = row.running.saturating_add(1);
        progress(&row);
        (Ticket { row: self.row.clone(), generation: row.generation }, row.cancel.clone())
    }

    /// The [`FileLoader`] for catalogued videos: the poster frame, fitted to `max_edge`, upright,
    /// in linear Rec.2020.
    pub fn loader(&self) -> FileLoader {
        let frames = self.clone();
        Arc::new(move |path: &str, max_edge: usize, _with: RawDecode<'_>| frames.load(path, max_edge))
    }

    fn load(&self, path: &str, max_edge: usize) -> Result<(lightcraft_raster::Rgb32f, SourceInfo), String> {
        if !lightcraft_codecs::video_stream::video_decoding_available() {
            return Err(lightcraft_codecs::video_stream::NO_VIDEO_DECODING.into());
        }
        // (in the row from the start: a decode waiting for memory counts in its total, and its ✕
        // stops it before it decodes)
        let (_ticket, cancel) = self.enter();
        let background = crate::memory::is_background();
        let opts = poster_options(max_edge, background);
        // the clip's picture size weighs the decode (its headers only)
        let (w, h) = {
            let s = VideoStream::open(open_clip(path)?, &opts.stream).map_err(|e| reason(&e))?;
            (s.info().width, s.info().height)
        };
        let gate = crate::memory::work_gate();
        let weight = poster_weight(w, h);
        let _permit = if background { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(VIDEO_STOPPED.into());
        }
        let r = poster_frame(open_clip(path)?, &opts, &cancel);
        crate::memory::release();
        let p = r.map_err(|e| reason(&e))?;
        Ok((p.image, SourceInfo::default()))
    }
}

/// The clip at `path`, read in place. The reason it can't be opened is worded for the card and
/// Info (translated there, no path: the card and its accessibility label show it); the log keeps
/// the system's own message.
#[cfg(not(target_arch = "wasm32"))]
fn open_clip(path: &str) -> Result<FileSource, String> {
    FileSource::open(path).map_err(|e| {
        log::warn!("video poster: {path}: {e}");
        open_failure(e.kind()).to_string()
    })
}

/// Why a clip's file can't be opened, as the card says it.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn open_failure(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "the clip's file is missing",
        std::io::ErrorKind::PermissionDenied => "the clip's file can't be read (no permission)",
        _ => "the clip's file can't be read",
    }
}

/// The web build has no files to read a clip from (and no video decoding: `load` answers that
/// before it gets here).
#[cfg(target_arch = "wasm32")]
fn open_clip(_path: &str) -> Result<Box<dyn lightcraft_codecs::video_stream::ClipSource>, String> {
    Err(lightcraft_codecs::video_stream::NO_VIDEO_DECODING.into())
}

/// Poster options for a source of `max_edge` (`usize::MAX`: the full picture).
fn poster_options(max_edge: usize, background: bool) -> PosterOptions {
    let threads = if background { 1 } else { INTERACTIVE_THREADS };
    PosterOptions {
        max_edge: if max_edge >= SourceLevel::Full.max_edge() { 0 } else { max_edge.max(1) },
        stream: StreamOptions { threads, ..PosterOptions::default().stream },
        ..PosterOptions::default()
    }
}

/// The reason a poster isn't there, as the Info panel and the loupe show it.
fn reason(e: &VideoError) -> String {
    match e {
        VideoError::Cancelled => VIDEO_STOPPED.into(),
        e => e.to_string(),
    }
}

fn progress(row: &Row) {
    if let Some(g) = &row.guard {
        g.progress(row.done, row.done.saturating_add(row.running));
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut row = self.row.lock().unwrap_or_else(PoisonError::into_inner);
        if row.generation != self.generation {
            // a decode the ✕ stopped, finishing after a fresh row started
            return;
        }
        row.running = row.running.saturating_sub(1);
        row.done = row.done.saturating_add(1);
        if row.running == 0 {
            row.guard = None;
        } else {
            progress(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_stopped(flag: &AtomicBool) -> bool {
        flag.load(Ordering::Relaxed)
    }

    #[test]
    fn one_row_for_every_poster_in_flight() {
        let activity = Activity::default();
        let frames = VideoFrames::new(activity.clone());
        assert!(activity.list().is_empty());
        let (a, flag_a) = frames.enter();
        let (b, flag_b) = frames.enter();
        let rows = activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].total, rows[0].cancellable), (VIDEO_FRAMES_KIND, VIDEO_FRAMES_LABEL, 2, true));
        drop(a);
        let rows = activity.list();
        assert_eq!((rows[0].done, rows[0].total), (1, 2));
        // ✕ stops every decode of the burst
        activity.cancel(rows[0].id).unwrap();
        assert!(is_stopped(&flag_a) && is_stopped(&flag_b));
        drop(b);
        assert!(activity.list().is_empty(), "the row goes with the last decode");
        // the next burst starts afresh, not stopped
        let (_c, flag_c) = frames.enter();
        assert!(!is_stopped(&flag_c));
        assert_eq!(activity.list()[0].total, 1);
    }

    /// A decode that starts after the row's ✕ (while a stopped one still finishes) gets a fresh
    /// row and flag: it must not fail as stopped, and the stopped decode's end doesn't count in
    /// the new row.
    #[test]
    fn a_decode_after_the_cancel_gets_a_fresh_row() {
        let activity = Activity::default();
        let frames = VideoFrames::new(activity.clone());
        let (a, flag_a) = frames.enter();
        activity.cancel(activity.list()[0].id).unwrap();
        assert!(is_stopped(&flag_a));
        let (b, flag_b) = frames.enter();
        assert!(!is_stopped(&flag_b), "the entrant after the cancel is not stopped");
        let rows = activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].done, rows[0].total, rows[0].cancellable), (0, 1, true));
        drop(a);
        let rows = activity.list();
        assert_eq!((rows.len(), rows[0].done, rows[0].total), (1, 0, 1), "the stopped decode is not the new row's");
        drop(b);
        assert!(activity.list().is_empty());
    }

    /// A decode weighs what its clip's pictures take: about 300 MB at 4K, four times the pixels
    /// at 8K, the float copy capped by the poster's conversion budget.
    #[test]
    fn a_poster_weighs_its_clips_picture_size() {
        let mb = |w, h| poster_weight(w, h) >> 20;
        assert!((280..=320).contains(&mb(3840, 2160)), "{}", mb(3840, 2160));
        assert!((900..=1100).contains(&mb(7680, 4320)), "{}", mb(7680, 4320));
        assert!(mb(640, 360) < 20);
        // (no overflow: a hostile size weighs about everything)
        assert!(poster_weight(u32::MAX, u32::MAX) >= usize::MAX / 2);
    }

    #[test]
    fn options_follow_the_level_and_the_caller() {
        let o = poster_options(SourceLevel::Thumb.max_edge(), true);
        assert_eq!((o.max_edge, o.stream.threads), (512, 1));
        let o = poster_options(SourceLevel::Full.max_edge(), false);
        assert_eq!((o.max_edge, o.stream.threads), (0, INTERACTIVE_THREADS));
        assert_eq!(reason(&VideoError::Cancelled), "Stopped");
        assert_eq!(reason(&VideoError::Unsupported("Apple ProRes video can't be decoded yet".into())), "Apple ProRes video can't be decoded yet");
    }

    /// A clip that can't be opened says so without its path (the card, Info and the card's
    /// accessibility label show the reason; the catalogs translate it).
    #[test]
    fn a_missing_file_is_an_error_without_its_path() {
        let frames = VideoFrames::new(Activity::default());
        let r = frames.load("/nonexistent/lightcraft-video-frames/clip.mp4", 512);
        let e = r.err().unwrap_or_default();
        assert!(!e.contains("/nonexistent"), "{e}");
        if lightcraft_codecs::video_stream::video_decoding_available() {
            assert_eq!(e, "the clip's file is missing");
        }
        assert!(frames.activity.list().is_empty());
        assert_eq!(open_failure(std::io::ErrorKind::PermissionDenied), "the clip's file can't be read (no permission)");
        assert_eq!(open_failure(std::io::ErrorKind::Other), "the clip's file can't be read");
    }
}
