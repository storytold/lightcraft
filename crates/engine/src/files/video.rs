//! Video poster frames from files: what a catalogued clip's grid tile, filmstrip cell and loupe
//! show ([`lightcraft_codecs::poster`]), decoded from the file in place (never read whole: a clip
//! can be gigabytes).
//!
//! Decoding needs `lightcraft-codecs`' feature `video-frames` (the desktop app and the CLI turn it
//! on); without it the loader answers why, the render fails with that reason and the clip keeps
//! its placeholder. While any poster decodes, the activity stack has one row for all of them
//! ("Reading video frames", done of the burst's total); its ✕ stops the decodes in flight.

use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_codecs::poster::{PosterOptions, poster_frame};
use lightcraft_codecs::video_stream::{FileSource, StreamOptions, VideoError};
use lightcraft_pipeline::SourceInfo;

use crate::activity::{Activity, Cancel, TaskGuard};
use crate::media::{FileLoader, RawDecode, SourceLevel};

/// The activity row's kind ([`crate::activity`]).
pub const VIDEO_FRAMES_KIND: &str = "videoFrames";
/// The activity row's label (translated by the frontend).
pub const VIDEO_FRAMES_LABEL: &str = "Reading video frames";

/// What one poster decode counts for at [`crate::memory::work_gate`]: the decoder's pictures (a
/// 4K stream keeps up to 16 of 12 MB) and the converted picture (100 MB at 4K in linear float).
const POSTER_WEIGHT: usize = 320 << 20;

/// Decoder threads for a poster the loupe waits for; a background thumbnail decodes on one
/// thread, since the grid already renders several at once.
const INTERACTIVE_THREADS: usize = 4;

/// The shared row and its burst's counts.
#[derive(Default)]
struct Row {
    guard: Option<TaskGuard>,
    cancel: Arc<AtomicBool>,
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
}

impl VideoFrames {
    pub fn new(activity: Activity) -> VideoFrames {
        VideoFrames { activity, row: Arc::default() }
    }

    /// Count a decode in, starting the row when it is the first; the flag stops it.
    fn enter(&self) -> (Ticket, Arc<AtomicBool>) {
        let mut row = self.row.lock().unwrap_or_else(PoisonError::into_inner);
        if row.guard.is_none() {
            // a fresh flag per row: a ✕ on the last one doesn't stop the next burst
            row.cancel = Arc::new(AtomicBool::new(false));
            row.done = 0;
            row.guard = Some(self.activity.start(VIDEO_FRAMES_KIND, VIDEO_FRAMES_LABEL, Cancel::Flag(row.cancel.clone())));
        }
        row.running = row.running.saturating_add(1);
        progress(&row);
        (Ticket { row: self.row.clone() }, row.cancel.clone())
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
        let background = crate::memory::is_background();
        let gate = crate::memory::work_gate();
        let _permit = if background { gate.acquire(POSTER_WEIGHT) } else { gate.acquire_urgent(POSTER_WEIGHT) };
        let (_ticket, cancel) = self.enter();
        let src = FileSource::open(path).map_err(|e| format!("{path}: {e}"))?;
        let opts = poster_options(max_edge, background);
        let r = poster_frame(src, &opts, &cancel);
        crate::memory::release();
        let p = r.map_err(|e| reason(&e))?;
        Ok((p.image, SourceInfo::default()))
    }
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
        VideoError::Cancelled => "Stopped".into(),
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

    #[test]
    fn options_follow_the_level_and_the_caller() {
        let o = poster_options(SourceLevel::Thumb.max_edge(), true);
        assert_eq!((o.max_edge, o.stream.threads), (512, 1));
        let o = poster_options(SourceLevel::Full.max_edge(), false);
        assert_eq!((o.max_edge, o.stream.threads), (0, INTERACTIVE_THREADS));
        assert_eq!(reason(&VideoError::Cancelled), "Stopped");
        assert_eq!(reason(&VideoError::Unsupported("Apple ProRes video can't be decoded yet".into())), "Apple ProRes video can't be decoded yet");
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let frames = VideoFrames::new(Activity::default());
        let r = frames.load("/nonexistent/lightcraft-video-frames/clip.mp4", 512);
        assert!(r.is_err());
        assert!(frames.activity.list().is_empty());
    }
}
