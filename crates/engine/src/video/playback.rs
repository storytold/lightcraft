//! Frames for the loupe while a clip plays or sits paused (M14.4).
//!
//! - [`Playback`]: a decode-ahead worker for one playing clip. It decodes sequentially
//!   ([`VideoBackend::frames_from`]) at display size, grades every frame through a
//!   [`GradeLut`] baked from `video_settings(develop)` (rebaked only when the settings change),
//!   and hands frames over a bounded queue ([`QUEUE`] frames), so it runs at most that far ahead.
//!   Frames already behind the playhead the host reports ([`Playback::set_target`]) are skipped
//!   before grading, which holds real time on a slow decoder. The worker never blocks its host:
//!   the host polls with [`Playback::poll`].
//!   With [`Playback::start_linear`] the worker skips that grade and hands over the decoded
//!   linear frame with the LUT ([`FramePixels::Linear`]) for the host to grade on the GPU
//!   (`lcc_gpu::video_lut`), which takes the per-frame LUT cost off the CPU.
//! - [`spawn_still`]: one full-quality graded frame (the real pipeline, `grade_frame`) for a
//!   paused or scrubbed clip, on a thread of its own.
//!
//! Both are long-lived interactive work, not activity rows; decode errors are logged and reported
//! once to the host. Sources: own design.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};

use lcc_develop::DevelopSettings;
use lcc_pipeline::Quality;
use lcc_pipeline::grade::{GradeLut, grade_frame, video_settings};
use lcc_raster::{Rgb32f, Rgba8};

use super::VideoBackend;

/// Most frames the worker decodes ahead of the screen.
pub const QUEUE: usize = 8;
/// Longest side of a frame decoded for playback.
pub const PLAY_MAX_SIDE: u32 = 960;

/// A frame's pixels.
#[derive(Clone, Debug)]
pub enum FramePixels {
    /// Graded, display-encoded sRGB.
    Graded(Arc<Rgba8>),
    /// Decoded but not graded ([`Playback::start_linear`]): linear RGB, to be graded with `lut`
    /// (whose identity is `key`: the same key, the same table).
    Linear { image: Arc<Rgb32f>, lut: Arc<GradeLut>, key: u64 },
}

impl FramePixels {
    /// The frame graded on the CPU (a linear frame is graded through its LUT here).
    pub fn graded(&self) -> Arc<Rgba8> {
        match self {
            FramePixels::Graded(g) => g.clone(),
            FramePixels::Linear { image, lut, .. } => Arc::new(lut.apply(image, Quality::Full)),
        }
    }
}

/// A frame ready to show.
#[derive(Clone, Debug)]
pub struct ShownFrame {
    /// Presentation time, seconds.
    pub time: f64,
    pub pixels: FramePixels,
    /// Rendered by the full pipeline (paused) rather than the playback LUT.
    pub full: bool,
}

/// What the worker sends.
enum Msg {
    Frame(ShownFrame),
    End,
    Error(String),
}

/// What [`Playback::poll`] reports.
#[derive(Debug, Default)]
pub struct Poll {
    /// The newest frame due at the playhead (earlier due frames were dropped as late).
    pub frame: Option<ShownFrame>,
    /// The worker reached the clip's end.
    pub ended: bool,
    /// Decoding failed; the worker has stopped.
    pub error: Option<String>,
}

struct Shared {
    settings: Mutex<(u64, DevelopSettings)>,
    target: Mutex<f64>,
    stop: AtomicBool,
}

/// The decode-ahead worker of one playing clip. Dropping it stops the worker.
pub struct Playback {
    rx: Receiver<Msg>,
    shared: Arc<Shared>,
    /// A frame received but not yet due.
    ahead: Option<ShownFrame>,
    /// Where it started decoding (seconds).
    pub start: f64,
    pub path: PathBuf,
    ended: bool,
    /// Frames come ungraded ([`Playback::start_linear`]).
    pub linear: bool,
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playback").field("path", &self.path).field("start", &self.start).finish()
    }
}

fn settings_key(d: &DevelopSettings) -> u64 {
    video_settings(d).hash64()
}

impl Playback {
    /// Start decoding `path` from `start` at most `max_side` px, graded with `develop`.
    pub fn start(backend: Arc<dyn VideoBackend>, path: PathBuf, start: f64, max_side: u32, develop: &DevelopSettings) -> Result<Playback, String> {
        Playback::start_mode(backend, path, start, max_side, develop, false)
    }

    /// [`Playback::start`], but frames come ungraded ([`FramePixels::Linear`]) for a GPU grade.
    pub fn start_linear(
        backend: Arc<dyn VideoBackend>,
        path: PathBuf,
        start: f64,
        max_side: u32,
        develop: &DevelopSettings,
    ) -> Result<Playback, String> {
        Playback::start_mode(backend, path, start, max_side, develop, true)
    }

    fn start_mode(
        backend: Arc<dyn VideoBackend>,
        path: PathBuf,
        start: f64,
        max_side: u32,
        develop: &DevelopSettings,
        linear: bool,
    ) -> Result<Playback, String> {
        let shared = Arc::new(Shared {
            settings: Mutex::new((settings_key(develop), develop.clone())),
            target: Mutex::new(start),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = sync_channel(QUEUE);
        let (sh, p) = (shared.clone(), path.clone());
        let side = max_side.clamp(16, PLAY_MAX_SIDE);
        std::thread::Builder::new()
            .name("video-playback".into())
            .spawn(move || worker(backend, p, start, side, linear, sh, tx))
            .map_err(|e| format!("can't start video playback: {e}"))?;
        Ok(Playback { rx, shared, ahead: None, start, path, ended: false, linear })
    }

    /// New develop settings: the worker rebakes its LUT when the video grade changed.
    pub fn set_settings(&self, develop: &DevelopSettings) {
        let key = settings_key(develop);
        let mut g = self.shared.settings.lock().unwrap_or_else(PoisonError::into_inner);
        if g.0 != key {
            *g = (key, develop.clone());
        }
    }

    /// The playhead now: frames before it (by more than `slack`) aren't graded.
    pub fn set_target(&self, t: f64) {
        *self.shared.target.lock().unwrap_or_else(PoisonError::into_inner) = t;
    }

    /// Take what has arrived: the newest frame whose time is at or before `now + slack`.
    pub fn poll(&mut self, now: f64, slack: f64) -> Poll {
        let mut out = Poll::default();
        loop {
            if let Some(f) = self.ahead.take() {
                if f.time <= now + slack {
                    out.frame = Some(f);
                } else {
                    self.ahead = Some(f);
                    break;
                }
            }
            match self.rx.try_recv() {
                Ok(Msg::Frame(f)) => self.ahead = Some(f),
                Ok(Msg::End) => {
                    self.ended = true;
                }
                Ok(Msg::Error(e)) => {
                    out.error = Some(e);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.ended {
                        out.error = Some("video playback stopped unexpectedly".into());
                        self.ended = true;
                    }
                    break;
                }
            }
        }
        out.ended = self.ended && self.ahead.is_none();
        out
    }

    /// The time of the next frame waiting (not yet due), if any.
    pub fn next_time(&self) -> Option<f64> {
        self.ahead.as_ref().map(|f| f.time)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn worker(backend: Arc<dyn VideoBackend>, path: PathBuf, start: f64, side: u32, linear: bool, sh: Arc<Shared>, tx: SyncSender<Msg>) {
    let fail = |tx: &SyncSender<Msg>, e: String| {
        log::warn!("video playback of {}: {e}", path.display());
        let _ = tx.send(Msg::Error(e));
    };
    let frames = match backend.frames_from(&path, start, Some(side)) {
        Ok(f) => f,
        Err(e) => return fail(&tx, e),
    };
    let mut baked: Option<(u64, Arc<GradeLut>)> = None;
    let mut last: Option<f64> = None;
    for item in frames {
        if sh.stop.load(Ordering::Relaxed) {
            return;
        }
        let (time, img) = match item {
            Ok(f) => f,
            Err(e) => return fail(&tx, e),
        };
        // late: a frame further behind the playhead than one frame interval isn't worth grading
        let target = *sh.target.lock().unwrap_or_else(PoisonError::into_inner);
        let interval = last.map(|l| (time - l).abs()).filter(|d| *d > 0.0).unwrap_or(1.0 / 30.0);
        last = Some(time);
        if time + interval < target {
            continue;
        }
        let (key, develop) = sh.settings.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if baked.as_ref().is_none_or(|(k, _)| *k != key) {
            match GradeLut::bake(&video_settings(&develop)) {
                Ok(l) => baked = Some((key, Arc::new(l))),
                Err(e) => return fail(&tx, format!("can't grade the clip: {e}")),
            }
        }
        let Some((key, lut)) = baked.as_ref() else { return };
        let pixels = if linear {
            FramePixels::Linear { image: Arc::new(img), lut: lut.clone(), key: *key }
        } else {
            FramePixels::Graded(Arc::new(lut.apply(&img, Quality::Full)))
        };
        if tx.send(Msg::Frame(ShownFrame { time, pixels, full: false })).is_err() {
            return;
        }
    }
    let _ = tx.send(Msg::End);
}

/// A full-quality graded frame at `t` (longest side ≤ `max_side`), rendered on its own thread.
/// The receiver gets exactly one answer.
pub fn spawn_still(
    backend: Arc<dyn VideoBackend>,
    path: PathBuf,
    t: f64,
    max_side: u32,
    develop: &DevelopSettings,
) -> Receiver<Result<ShownFrame, String>> {
    let (tx, rx) = sync_channel(1);
    let develop = develop.clone();
    let tx2 = tx.clone();
    let spawned = std::thread::Builder::new().name("video-still".into()).spawn(move || {
        let r = backend.frame_at(&path, t, Some(max_side.max(16))).map(|img| ShownFrame {
            time: t,
            pixels: FramePixels::Graded(Arc::new(grade_frame(&img, &video_settings(&develop), Quality::Full))),
            full: true,
        });
        if let Err(e) = &r {
            log::warn!("video frame of {} at {t:.3} s: {e}", path.display());
        }
        let _ = tx.send(r);
    });
    if let Err(e) = spawned {
        let _ = tx2.send(Err(format!("can't render the video frame: {e}")));
    }
    rx
}

/// Most seconds of sound decoded ahead of the sound card.
const AUDIO_AHEAD_SECS: usize = 1;

/// A playing clip's sound (only with the `audio-out` feature; see
/// [`VideoBackend::can_play_audio`]): a feeder thread decodes from `t` with
/// `lcc_video::AudioReader`, resamples to the device rate, downmixes to stereo, and the
/// `lcc-audio-out` stream drains it. Follow its [`lcc_audio_out::AudioClock`] with
/// `VideoState::follow_audio_clock` so the picture keeps to the sound. Dropping it stops both.
pub struct AudioPlayback {
    _out: lcc_audio_out::Output,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for AudioPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioPlayback")
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl AudioPlayback {
    /// Start the clip's sound at `t` seconds. Returns the playback and its clock.
    pub fn start(path: PathBuf, t: f64) -> Result<(AudioPlayback, lcc_audio_out::AudioClock), String> {
        let rate = lcc_audio_out::default_rate().ok_or("no audio output device")?;
        let mut reader = lcc_video::AudioReader::open(&path).map_err(|e| e.to_string())?;
        let info = reader.info().clone();
        let channels = info.channels.max(1);
        let queue: Arc<Mutex<std::collections::VecDeque<f32>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let cap = (rate as usize).saturating_mul(2).saturating_mul(AUDIO_AHEAD_SECS);
        let (q, st) = (queue.clone(), stop.clone());
        reader.seek(t);
        std::thread::Builder::new()
            .name("video-audio".into())
            .spawn(move || {
                while !st.load(Ordering::Relaxed) {
                    if q.lock().unwrap_or_else(PoisonError::into_inner).len() > cap {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    let block = match reader.next_block() {
                        Ok(Some(b)) => b,
                        Ok(None) => return,
                        Err(e) => {
                            log::warn!("video sound of {}: {e}", path.display());
                            return;
                        }
                    };
                    // the first block may start before `t`: drop the part before it
                    let skip = if block.time < t { (((t - block.time) * f64::from(info.sample_rate)) as usize).saturating_mul(channels) } else { 0 };
                    let src = block.samples.get(skip.min(block.samples.len())..).unwrap_or(&[]);
                    let pcm = lcc_video::resample(src, channels, info.sample_rate, rate);
                    let mut g = q.lock().unwrap_or_else(PoisonError::into_inner);
                    for f in pcm.chunks(channels) {
                        let l = f.first().copied().unwrap_or(0.0);
                        let r = f.get(1).copied().unwrap_or(l);
                        g.push_back(l);
                        g.push_back(r);
                    }
                }
            })
            .map_err(|e| format!("can't start the clip's sound: {e}"))?;
        let fill: lcc_audio_out::Fill = Box::new(move |out: &mut [f32]| {
            let mut g = queue.lock().unwrap_or_else(PoisonError::into_inner);
            for s in out.iter_mut() {
                *s = g.pop_front().unwrap_or(0.0);
            }
        });
        let out = lcc_audio_out::Output::start(rate, 2, fill)?;
        let clock = out.clock();
        Ok((AudioPlayback { _out: out, stop }, clock))
    }
}
