//! The moving picture in the loupe (M14.4): while a clip plays, frames from the engine's
//! decode-ahead worker ([`lcc_engine::video::playback::Playback`]) shown by the player clock;
//! paused or scrubbed, a full-quality graded frame at the playhead
//! ([`lcc_engine::video::playback::spawn_still`]). The texture is drawn over the loupe's poster
//! render; until the first frame arrives the poster shows.
//!
//! Nothing here waits: the worker and still renders run on their own threads and are polled once
//! per UI frame. A decode error stops playback (`video.pause`) and shows one toast.

use std::sync::mpsc::{Receiver, TryRecvError};

use lcc_catalog::PhotoId;
use lcc_engine::video::playback::{AudioPlayback, FramePixels, Playback, ShownFrame, spawn_still};
use serde_json::json;

use crate::LccApp;
use crate::video_ui::VideoUiState;

/// How far the shown frame may drift from the clock before the worker restarts there (a seek or
/// a loop while playing).
const RESYNC_SECS: f64 = 0.5;

/// The loupe's video frame state (one clip at a time).
#[derive(Default)]
pub struct VideoView {
    playback: Option<(PhotoId, Playback)>,
    /// The clip's sound while playing (`audio-out` builds); the player clock follows it.
    audio: Option<AudioPlayback>,
    /// What is on screen: the clip, the frame's time, its look key, full quality?, the texture.
    shown: Option<Shown>,
    /// A still being rendered and the key it is for.
    still: Option<(StillKey, Receiver<Result<ShownFrame, String>>)>,
    /// The last still key whose render failed (not retried until it changes).
    failed: Option<StillKey>,
    /// The GPU grade failed once: playing frames are graded on the CPU from then on.
    gpu_failed: bool,
}

/// A frame's texture: egui's own (CPU-graded) or the window GPU's (graded there).
enum Tex {
    Egui(egui::TextureHandle),
    Gpu(egui::TextureId, [usize; 2]),
}

impl Tex {
    fn id(&self) -> egui::TextureId {
        match self {
            Tex::Egui(t) => t.id(),
            Tex::Gpu(id, _) => *id,
        }
    }
    fn size(&self) -> [usize; 2] {
        match self {
            Tex::Egui(t) => t.size(),
            Tex::Gpu(_, s) => *s,
        }
    }
}

struct Shown {
    photo: PhotoId,
    time: f64,
    full: bool,
    key: Option<StillKey>,
    tex: Tex,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StillKey {
    photo: PhotoId,
    /// The frame's time in microseconds.
    t_us: i64,
    look: u64,
    side: u32,
}

/// What the loupe shows of a clip right now (for `ui.inspect` and tests).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoFrameInfo {
    pub time: f64,
    pub full: bool,
    pub playing: bool,
    /// Graded on the GPU.
    pub gpu: bool,
}

impl VideoView {
    /// The frame on screen for `id`, if any.
    pub fn info(&self, id: PhotoId) -> Option<VideoFrameInfo> {
        let s = self.shown.as_ref().filter(|s| s.photo == id)?;
        Some(VideoFrameInfo {
            time: s.time,
            full: s.full,
            playing: self.playback.as_ref().is_some_and(|(p, _)| *p == id),
            gpu: matches!(s.tex, Tex::Gpu(..)),
        })
    }
    /// The texture on screen for `id` (a new one per frame shown).
    pub fn texture_id(&self, id: PhotoId) -> Option<egui::TextureId> {
        self.shown.as_ref().filter(|s| s.photo == id).map(|s| s.tex.id())
    }
}

/// Stop the worker and the sound (another photo, another view); the clock goes back to the system's.
pub fn stop(app: &mut LccApp) {
    app.video_view.playback = None;
    app.video_view.still = None;
    if app.video_view.audio.take().is_some() {
        app.session.video.follow_system_clock();
    }
}

fn upload(ctx: &egui::Context, f: &ShownFrame) -> Tex {
    Tex::Egui(ctx.load_texture("video-frame", crate::render::color_image(&f.pixels.graded()), egui::TextureOptions::LINEAR))
}

/// A playing frame's texture: graded on the window's GPU when it came ungraded and the GPU
/// grade works, else on the CPU (and the GPU isn't tried again).
fn show(app: &mut LccApp, ctx: &egui::Context, f: &ShownFrame) -> Tex {
    if let FramePixels::Linear { image, lut, key } = &f.pixels
        && let Some(g) = app.services.video_gpu.as_mut()
        && !app.video_view.gpu_failed
    {
        match g.grade(image, *key, lut) {
            Ok((id, size)) => return Tex::Gpu(id, size),
            Err(e) => {
                log::warn!("video playback: GPU grade failed, grading on the CPU: {e}");
                app.video_view.gpu_failed = true;
            }
        }
    }
    upload(ctx, f)
}

/// Bring the loupe's video frame up to date for clip `id` with control-bar state `state`;
/// `side` is the drawn long edge in pixels. Returns the texture to draw, if one is ready.
pub fn update(app: &mut LccApp, ctx: &egui::Context, id: PhotoId, state: &VideoUiState, side: u32) -> Option<(egui::TextureId, [usize; 2])> {
    let develop = app.session.catalog.photo(id).map(|p| (*p.develop).clone()).unwrap_or_default();
    let look = lcc_pipeline::grade::video_settings(&develop).hash64();
    let backend = app.session.video.backend.clone();
    let path = lcc_engine::video::adapter::clip_path(&app.session, id).ok();
    let (Some(backend), Some(path)) = (backend, path) else {
        stop(app);
        return None;
    };
    let fps = if state.fps > 0.0 { state.fps } else { 30.0 };
    let slack = 0.5 / fps;
    if state.playing {
        app.video_view.still = None;
        // a seek (or a wrap when looping) while playing: restart the worker at the playhead
        let shown_t = app.video_view.shown.as_ref().filter(|s| s.photo == id && !s.full).map(|s| s.time);
        let next_t = app.video_view.playback.as_ref().and_then(|(_, p)| p.next_time());
        let started = app.video_view.playback.as_ref().filter(|(p, _)| *p == id).map(|(_, p)| p.start);
        let off = |t: f64| state.current < t - RESYNC_SECS || state.current > t + RESYNC_SECS;
        let drifted = match (next_t, shown_t) {
            (Some(n), _) => state.current < n - RESYNC_SECS - 1.0 / fps || state.current > n + RESYNC_SECS,
            // nothing queued yet: a worker just restarted near the playhead is waited for (it
            // was restarted every frame until its first frame arrived after a long jump)
            (None, Some(s)) => off(s) && started.is_none_or(off),
            (None, None) => false,
        };
        let gpu = app.services.video_gpu.is_some() && app.ui.settings.gpu && !app.video_view.gpu_failed;
        let same = app.video_view.playback.as_ref().is_some_and(|(p, pb)| *p == id && pb.path == path && pb.linear == gpu);
        if !same || drifted {
            app.video_view.playback = None;
            // the sound restarts with the picture; the player clock follows it
            if app.video_view.audio.take().is_some() {
                app.session.video.follow_system_clock();
            }
            if backend.can_play_audio(&path) {
                match AudioPlayback::start(path.clone(), state.current) {
                    Ok((a, clock)) => {
                        app.session.video.follow_audio_clock(clock);
                        app.video_view.audio = Some(a);
                    }
                    Err(e) => log::warn!("video sound: {e}"),
                }
            }
            let max = side.min(lcc_engine::video::playback::PLAY_MAX_SIDE);
            let started = if gpu {
                Playback::start_linear(backend, path, state.current, max, &develop)
            } else {
                Playback::start(backend, path, state.current, max, &develop)
            };
            match started {
                Ok(p) => app.video_view.playback = Some((id, p)),
                Err(e) => return fail(app, ctx, e),
            }
        }
        let (_, pb) = app.video_view.playback.as_mut()?;
        pb.set_settings(&develop);
        pb.set_target(state.current);
        let polled = pb.poll(state.current, slack);
        if let Some(e) = polled.error {
            return fail(app, ctx, e);
        }
        if let Some(f) = polled.frame {
            let tex = show(app, ctx, &f);
            app.video_view.shown = Some(Shown { photo: id, time: f.time, full: false, key: None, tex });
        }
        // the engine clock decides the end (stop at the trim end, or loop); keep frames coming
        ctx.request_repaint();
    } else {
        app.video_view.playback = None;
        if app.video_view.audio.take().is_some() {
            app.session.video.follow_system_clock();
        }
        let snapped = lcc_engine::video::player::snap(state.current, fps);
        let key = StillKey { photo: id, t_us: (snapped * 1e6).round() as i64, look, side: side.clamp(64, 3840) };
        let have = app.video_view.shown.as_ref().is_some_and(|s| s.key == Some(key));
        let pending = app.video_view.still.as_ref().is_some_and(|(k, _)| *k == key);
        if !have && !pending && app.video_view.failed != Some(key) {
            let rx = spawn_still(backend, path, snapped, key.side, &develop);
            app.video_view.still = Some((key, rx));
        }
        if let Some((k, rx)) = app.video_view.still.as_ref() {
            let k = *k;
            match rx.try_recv() {
                Ok(Ok(f)) => {
                    let tex = upload(ctx, &f);
                    app.video_view.shown = Some(Shown { photo: id, time: f.time, full: true, key: Some(k), tex });
                    app.video_view.still = None;
                }
                Ok(Err(e)) => {
                    app.video_view.still = None;
                    app.video_view.failed = Some(k);
                    app.toast(ctx, e);
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(15)),
                Err(TryRecvError::Disconnected) => app.video_view.still = None,
            }
        }
    }
    app.video_view.shown.as_ref().filter(|s| s.photo == id).map(|s| (s.tex.id(), s.tex.size()))
}

fn fail(app: &mut LccApp, ctx: &egui::Context, e: String) -> Option<(egui::TextureId, [usize; 2])> {
    log::warn!("video playback: {e}");
    app.video_view.playback = None;
    if app.video_view.audio.take().is_some() {
        app.session.video.follow_system_clock();
    }
    let _ = app.session.execute("video.pause", &json!({}));
    app.toast(ctx, format!("{}: {e}", crate::i18n::tr("Playback stopped")));
    app.video_view.shown.as_ref().map(|s| (s.tex.id(), s.tex.size()))
}

#[cfg(test)]
#[path = "tests_video_view.rs"]
mod tests;
