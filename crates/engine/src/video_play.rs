//! Video playback in the loupe (LR-VID-PLAY, KEY-VIDEO-PLAY): one clip at a time, decoded on its own
//! thread into a small bounded queue of display-ready frames, shown against a clock.
//!
//! - **Frames** come from a [`FrameSource`]: the clip's decoder plus the Y′CbCr to sRGB conversion,
//!   run on the decode thread, so the UI thread only uploads a finished RGBA buffer
//!   ([`crate::video_source`] opens a clip with the codecs crate's decoders).
//! - **The queue** holds at most a few frames, capped by the memory their buffers hold
//!   ([`QUEUE_BYTES`], the buffers kept for reuse included, and at least one frame; a kept buffer
//!   far bigger than the frames decoded now is freed): memory stays bounded at any resolution,
//!   whatever the clip's header claims, and a seek throws the queue away.
//! - **The clock** is a [`Clock`]; playback runs on the wall clock. A clip's audio is not played
//!   yet (no audio output in LightCraft), so there is no audio master clock; when there is one it
//!   becomes another [`Clock`] and video keeps following it.
//! - **Late frames are dropped:** each tick shows the newest frame that is due and counts the ones
//!   it skipped ([`VideoPlayer::state`]).
//! - **Draft decoding** (the decoders skip the in-loop filters of pictures nothing predicts from) is
//!   allowed only while playing. Pausing re-decodes the frame on screen exactly, so a paused frame,
//!   a step and a scrub always show conforming pictures.
//! - **Seeking** asks the source for the frame shown at a time (the sync sample before it, then
//!   decoding forward); scrubbing coalesces: only the newest request is decoded.
//!
//! Nothing here blocks the caller: commands and [`VideoPlayer::tick`] only take a short lock, and
//! dropping a player never waits for its decode thread (which may be opening a clip on a sleeping
//! drive): the thread sees the stop flag and ends on its own.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use web_time::Instant;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

/// Bytes of decoded frames the queue and the reuse pool may hold together (a 4K RGBA frame is
/// about 33 MB, so about four queued at 4K; one frame is always allowed, however large).
pub const QUEUE_BYTES: usize = 160 << 20;
/// Frames the queue holds at most, however small.
const QUEUE_MAX: usize = 8;
/// Buffers kept for reuse at most (a frame leaving the screen gives its buffer to the next one).
const POOL_MAX: usize = 2;
/// Playing, a frame more than this many seconds behind the clock (the UI stalled, or nothing
/// ticked the player for a while) is caught up by seeking, not by decoding everything in between.
const CATCH_UP: f64 = 1.0;
/// A backward step aims this far before the frame on screen, so it lands on the frame before it
/// whatever the frame rate (above the 0.1 ms timestamp rounding [`seek_to`] allows).
const STEP_BACK: f64 = 1e-3;
/// Frames a seek decodes forward from its sync sample before giving up on reaching the target
/// (a hostile clip with no sync samples must not decode for minutes on one scrub).
pub const MAX_SEEK_FRAMES: usize = 1200;

/// What a clip is, as far as playback needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// Running time in seconds.
    pub duration: f64,
    /// Coded (stored) size of the pictures, before rotation.
    pub width: u32,
    pub height: u32,
    /// Nominal time between frames in seconds (frame step, the tolerance of "due").
    pub frame_duration: f64,
    /// How the stored pictures turn upright (the track's display matrix), applied when drawing.
    pub orientation: lightcraft_geom::Orientation,
    /// Codec name for people ("H.264", "HEVC").
    pub codec: String,
    /// The clip's audio track, described for people ("AAC, 48 kHz, stereo"); `None` when it has none.
    /// Not played: LightCraft has no audio output yet.
    pub audio: Option<String>,
}

impl SourceInfo {
    /// What the player assumes until the decode thread has opened the clip: no end yet.
    fn pending() -> SourceInfo {
        SourceInfo {
            duration: f64::INFINITY,
            width: 0,
            height: 0,
            frame_duration: 1.0 / 30.0,
            orientation: lightcraft_geom::Orientation::Normal,
            codec: String::new(),
            audio: None,
        }
    }
}

/// Opens a clip on the decode thread (reading a clip's headers can take a while on a slow or
/// sleeping drive, so neither the UI thread nor dropping the player waits for it).
pub type Open = Box<dyn FnOnce() -> Result<Box<dyn FrameSource>, String> + Send>;

/// One display-ready frame: RGBA, 8 bits, sRGB, `width × height` (before rotation).
#[derive(Clone, Debug)]
pub struct VideoFrame {
    /// Presentation time in seconds from the start of the clip.
    pub pts: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Decoded in draft mode (approximate); never left on screen while paused.
    pub draft: bool,
}

impl VideoFrame {
    /// The memory the frame holds: its buffer's capacity (a recycled buffer keeps the size of
    /// the biggest frame it held), not just the pixels in use.
    fn bytes(&self) -> usize {
        self.rgba.capacity()
    }
}

/// What [`FrameSource::next_frame`] got.
#[derive(Debug)]
pub enum Next {
    Frame(VideoFrame),
    /// The end of the clip.
    End,
    /// `stop` said to give up (a newer seek arrived) before a frame was ready.
    Stopped,
}

/// A clip's decoder, driven by the decode thread. Errors are sentences for people.
pub trait FrameSource: Send {
    fn info(&self) -> &SourceInfo;
    /// Position at the frame shown at `seconds`: the next [`FrameSource::next_frame`] calls
    /// return frames from there on, in presentation order (frames before it may come first; the
    /// player skips them).
    fn seek(&mut self, seconds: f64) -> Result<(), String>;
    /// The next frame in presentation order. `buffer` is a recycled RGBA buffer to fill (any size,
    /// possibly empty); `stop` is polled while decoding and ends the call with [`Next::Stopped`].
    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, buffer: Vec<u8>) -> Result<Next, String>;
    /// Allow approximate (draft) decoding of pictures nothing predicts from.
    fn set_draft(&mut self, on: bool);
    /// Playback is falling behind (late frames are being dropped): a source may make frames
    /// cheaper (smaller) until it can keep up again. Never called while paused.
    fn set_behind(&mut self, _behind: bool) {}
    /// The largest width or height the frames may have (the GPU's texture limit; 0: no limit). A
    /// source scales bigger pictures down or fails with a reason.
    fn set_max_side(&mut self, _pixels: usize) {}
}

/// The time playback follows, in seconds from any fixed origin.
pub trait Clock: Send + Sync {
    fn now(&self) -> f64;
}

/// The wall clock (the master clock while there is no audio output).
pub struct WallClock(Instant);

impl Default for WallClock {
    fn default() -> Self {
        WallClock(Instant::now())
    }
}

impl Clock for WallClock {
    fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// A request to the decode thread: show the frame at `target`.
#[derive(Clone, Copy, Debug)]
struct Seek {
    generation: u64,
    target: f64,
    draft: bool,
    /// Decoding the frame on screen again exactly (after playback stopped on a draft frame): an
    /// error the decoder stopped on stays reported.
    refresh: bool,
}

/// State shared by the player and its decode thread.
#[derive(Default)]
struct Shared {
    /// Frames ready to show, oldest first, each with the generation (seek) it belongs to.
    queue: VecDeque<(u64, Arc<VideoFrame>)>,
    queued_bytes: usize,
    /// The newest seek not yet taken by the decode thread (older ones are dropped: scrubbing).
    request: Option<Seek>,
    /// The generation the decode thread works on, and whether it reached the end of the clip.
    generation: u64,
    end: bool,
    /// The newest generation whose seek the decode thread has answered (with frames or without).
    served: u64,
    /// Whether draft decoding is allowed now (playing).
    draft: bool,
    /// Playback is dropping late frames ([`FrameSource::set_behind`]).
    behind: bool,
    error: Option<String>,
    /// The size of the newest frame decoded (the clip header's size until then): what one more
    /// frame costs in [`Shared::wants_frame`].
    frame_bytes: usize,
    /// [`FrameSource::set_max_side`].
    max_side: usize,
    /// The decode thread has ended (stopped, the clip could not be opened, or it failed).
    finished: bool,
    /// The clip's description, once the decode thread has opened it.
    info: Option<SourceInfo>,
    decoded: u64,
    /// RGBA buffers of frames that left the screen, for the decoder to fill again (no 33 MB
    /// allocation per 4K frame), and their bytes.
    pool: Vec<Vec<u8>>,
    pool_bytes: usize,
}

impl Shared {
    fn recycle(&mut self, f: Arc<VideoFrame>) {
        if let Ok(f) = Arc::try_unwrap(f) {
            self.keep(f.rgba);
        }
    }

    /// Keep `buffer` for reuse when the pool and the queue have room for it and it is not far
    /// bigger than the frames decoded now (else free it).
    fn keep(&mut self, buffer: Vec<u8>) {
        let held = self.queued_bytes.saturating_add(self.pool_bytes).saturating_add(buffer.capacity());
        if self.pool.len() < POOL_MAX && held <= QUEUE_BYTES && !self.oversized(buffer.capacity()) {
            self.pool_bytes = self.pool_bytes.saturating_add(buffer.capacity());
            self.pool.push(buffer);
        }
    }

    /// A buffer to decode into: one kept for reuse, or a new (empty) one. Kept buffers far bigger
    /// than the frames decoded now (4K buffers once frames come at half size) are freed instead:
    /// filling one would hold its whole size for a small frame.
    fn buffer(&mut self) -> Vec<u8> {
        while let Some(b) = self.pool.pop() {
            self.pool_bytes = self.pool_bytes.saturating_sub(b.capacity());
            if !self.oversized(b.capacity()) {
                return b;
            }
        }
        Vec::new()
    }

    /// A buffer of `capacity` bytes is more than twice the size of the frames decoded now.
    fn oversized(&self, capacity: usize) -> bool {
        self.frame_bytes > 0 && capacity > self.frame_bytes.saturating_mul(2)
    }

    /// Whether the decoder may add a frame to the queue: always when it is empty, else while the
    /// queued frames, the kept buffers and one more frame stay within [`QUEUE_BYTES`].
    fn wants_frame(&self) -> bool {
        if self.queue.is_empty() {
            return true;
        }
        // a kept buffer becomes the frame: the bytes held stay the same
        let more = if self.pool.is_empty() { self.frame_bytes } else { 0 };
        self.queue.len() < QUEUE_MAX && self.queued_bytes.saturating_add(self.pool_bytes).saturating_add(more) <= QUEUE_BYTES
    }

    fn push(&mut self, generation: u64, f: VideoFrame) {
        self.decoded += 1;
        // (the pixels of one frame: what a new buffer for the next one takes)
        self.frame_bytes = f.rgba.len().max(1);
        self.queued_bytes = self.queued_bytes.saturating_add(f.bytes());
        self.queue.push_back((generation, Arc::new(f)));
    }

    fn pop(&mut self) -> Option<(u64, Arc<VideoFrame>)> {
        let f = self.queue.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(f.1.bytes());
        Some(f)
    }

    fn clear_queue(&mut self) {
        while let Some((_, f)) = self.pop() {
            self.recycle(f);
        }
    }
}

struct Channel {
    state: Mutex<Shared>,
    wake: Condvar,
    stop: AtomicBool,
}

impl Channel {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Counters for `video.state` and the measurements in the pull request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames that reached the screen.
    pub shown: u64,
    /// Frames that were due but skipped because a newer one was due too (late).
    pub dropped: u64,
}

/// The playing clip: a decode thread, the frame on screen and the clock.
pub struct VideoPlayer {
    photo: PhotoId,
    info: SourceInfo,
    /// `info` is the clip's (the decode thread has opened it), not [`SourceInfo::pending`].
    opened: bool,
    channel: Arc<Channel>,
    thread: Option<std::thread::JoinHandle<()>>,
    clock: Arc<dyn Clock>,
    playing: bool,
    /// Media time at `anchor_clock` (the position while paused).
    anchor_media: f64,
    anchor_clock: f64,
    generation: u64,
    /// Waiting for the first frame of the current generation (the clock holds still meanwhile).
    awaiting: bool,
    /// The time the latest request asked for: where a step starts while its frame is on its way
    /// (the anchor may differ, as at the end, where an exact frame is decoded again).
    target: f64,
    /// The decode thread's error as the latest tick saw it: what [`VideoPlayer::state`] and
    /// [`VideoPlayer::error`] report, so they never show an error with playback the tick has not
    /// stopped for yet (the thread may record it any moment).
    error: Option<String>,
    ended: bool,
    current: Option<Arc<VideoFrame>>,
    /// Bumped whenever `current` changes, so the UI uploads a texture only for a new frame.
    serial: u64,
    stats: Stats,
    /// Clock times of the latest late frames dropped (the last [`LATE_WINDOW`] seconds).
    late: VecDeque<f64>,
    /// What the decode thread was last told about falling behind.
    behind: bool,
}

/// Late frames are counted over this long (s)…
const LATE_WINDOW: f64 = 1.0;
/// …and this many in it mean playback is falling behind.
const LATE_BEHIND: usize = 2;

impl VideoPlayer {
    /// Start the decode thread for an opened `source` and show its first frame (paused at 0).
    pub fn start(photo: PhotoId, source: Box<dyn FrameSource>, clock: Arc<dyn Clock>) -> Result<VideoPlayer, String> {
        VideoPlayer::open(photo, Box::new(move || Ok(source)), clock)
    }

    /// Start the decode thread, which opens the clip with `open` and shows its first frame (paused
    /// at 0). Only failing to start a thread is an error here; a clip that can't be opened ends
    /// up in [`VideoPlayer::error`].
    pub fn open(photo: PhotoId, open: Open, clock: Arc<dyn Clock>) -> Result<VideoPlayer, String> {
        VideoPlayer::open_limited(photo, open, clock, 0)
    }

    /// [`VideoPlayer::open`] with frames no wider or taller than `max_side` pixels (the GPU's
    /// texture limit; 0: no limit), from the first frame on ([`FrameSource::set_max_side`]).
    pub fn open_limited(photo: PhotoId, open: Open, clock: Arc<dyn Clock>, max_side: usize) -> Result<VideoPlayer, String> {
        let channel = Arc::new(Channel {
            state: Mutex::new(Shared { frame_bytes: 1, max_side, ..Shared::default() }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = channel.clone();
        let thread = std::thread::Builder::new()
            .name("lc-video-decode".into())
            .spawn(move || {
                // a panic in a decoder ends playback with a reason, never silently
                let r = crate::guard::catch("The video decoder", || run(open, &worker));
                let mut s = worker.lock();
                if let Err(e) = r {
                    s.error = Some(e);
                }
                s.finished = true;
                drop(s);
                worker.wake.notify_all();
            })
            .map_err(|e| format!("could not start the video decoder: {e}"))?;
        let now = clock.now();
        let mut player = VideoPlayer {
            photo,
            info: SourceInfo::pending(),
            opened: false,
            channel,
            thread: Some(thread),
            clock,
            playing: false,
            anchor_media: 0.0,
            anchor_clock: now,
            generation: 0,
            awaiting: true,
            target: 0.0,
            error: None,
            ended: false,
            current: None,
            serial: 0,
            stats: Stats::default(),
            late: VecDeque::new(),
            behind: false,
        };
        player.request(0.0);
        Ok(player)
    }

    pub fn photo(&self) -> PhotoId {
        self.photo
    }

    /// The clip's description; `None` while it is still being opened.
    pub fn info(&self) -> Option<&SourceInfo> {
        self.opened.then_some(&self.info)
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// The frame on screen and its serial (changes with every new frame).
    pub fn frame(&self) -> Option<(&Arc<VideoFrame>, u64)> {
        self.current.as_ref().map(|f| (f, self.serial))
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The decoder's error, if it stopped on one.
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    /// The decode thread has ended (the clip could not be opened, or the decoder failed): nothing
    /// more will come; opening the clip again is the way to retry.
    pub fn finished(&self) -> bool {
        self.channel.lock().finished
    }

    /// The playback position in seconds.
    pub fn position(&self) -> f64 {
        let t = if self.playing && !self.awaiting { self.anchor_media + (self.clock.now() - self.anchor_clock) } else { self.anchor_media };
        t.clamp(0.0, self.info.duration.max(0.0))
    }

    fn reanchor(&mut self, media: f64) {
        self.anchor_media = media;
        self.anchor_clock = self.clock.now();
    }

    /// Ask the decode thread for the frame at `target`, dropping everything queued.
    fn request(&mut self, target: f64) {
        self.request_frame(target, false);
    }

    fn request_frame(&mut self, target: f64, refresh: bool) {
        self.generation += 1;
        self.awaiting = true;
        self.target = target;
        let mut s = self.channel.lock();
        s.request = Some(Seek { generation: self.generation, target, draft: self.playing, refresh });
        // an error belongs to the position it happened at; a new one starts clean (unless the
        // decode thread is gone: then the reason stays)
        if !refresh && !s.finished {
            s.error = None;
            self.error = None;
        }
        s.draft = self.playing;
        s.clear_queue();
        s.end = false;
        drop(s);
        self.channel.wake.notify_all();
    }

    pub fn play(&mut self) {
        if self.playing {
            return;
        }
        let at_end = self.ended || self.position() >= self.info.duration - self.info.frame_duration.max(1e-3) * 0.5;
        self.playing = true;
        self.ended = false;
        if at_end {
            self.reanchor(0.0);
            self.request(0.0);
        } else {
            let p = self.anchor_media;
            self.reanchor(p);
            let mut s = self.channel.lock();
            s.draft = true;
            drop(s);
            self.channel.wake.notify_all();
        }
    }

    pub fn pause(&mut self) {
        if !self.playing {
            return;
        }
        let p = self.position();
        self.playing = false;
        self.reanchor(p);
        // the frame on screen (and the queue behind it) may be draft: decode it again exactly;
        // while a seek's frame is on its way, the seek's target is where playback is
        let at = if self.awaiting { p } else { self.current.as_ref().map_or(p, |f| f.pts) };
        self.anchor_media = at;
        self.request(at);
    }

    /// Playback stopped (the end, an error) on a draft or reduced frame: decode it again exactly,
    /// as [`VideoPlayer::pause`] does, keeping the position.
    fn refresh_exact(&mut self) {
        if let Some(pts) = self.current.as_ref().filter(|f| f.draft).map(|f| f.pts) {
            let anchor = self.anchor_media;
            self.request_frame(pts, true);
            self.anchor_media = anchor;
        }
    }

    pub fn toggle(&mut self) {
        if self.playing { self.pause() } else { self.play() }
    }

    /// Jump to `seconds` (clamped to the clip); playing continues from there.
    pub fn seek(&mut self, seconds: f64) {
        let t = if seconds.is_finite() { seconds.clamp(0.0, self.info.duration.max(0.0)) } else { 0.0 };
        self.ended = false;
        self.reanchor(t);
        self.request(t);
    }

    /// Show the next (`frames` > 0) or previous frame, pausing first.
    pub fn step(&mut self, frames: i32) {
        if self.playing {
            self.pause();
        }
        let dur = self.info.frame_duration.max(1e-3);
        // from the frame on screen, or from a seek's target while its frame is on its way
        let from = if self.awaiting { self.target } else { self.current.as_ref().map_or(self.anchor_media, |f| f.pts) };
        if frames > 0 && !self.awaiting {
            // the decoder runs ahead of a paused frame: the next frame is usually queued already
            let next = {
                let mut s = self.channel.lock();
                let cur = self.generation;
                while s.queue.front().is_some_and(|(g, f)| *g != cur || f.pts <= from) {
                    if let Some((_, f)) = s.pop() {
                        s.recycle(f);
                    }
                }
                if frames == 1 { s.pop().map(|(_, f)| f) } else { None }
            };
            self.channel.wake.notify_all();
            if let Some(f) = next {
                self.anchor_media = f.pts;
                self.show(f);
                return;
            }
        }
        // back: just before the frame on screen, so a variable frame rate never skips a frame
        let target = if frames < 0 { from + f64::from(frames + 1) * dur - STEP_BACK } else { from + f64::from(frames) * dur };
        let target = target.clamp(0.0, self.info.duration.max(0.0));
        self.ended = false;
        self.reanchor(target);
        self.request(target);
    }

    fn show(&mut self, f: Arc<VideoFrame>) {
        if let Some(old) = self.current.replace(f) {
            self.channel.lock().recycle(old);
        }
        self.serial = self.serial.wrapping_add(1);
        self.stats.shown += 1;
    }

    /// Count `skipped` late frames and tell the decode thread when playback starts or stops
    /// falling behind ([`LATE_BEHIND`] or more of them within [`LATE_WINDOW`]).
    fn note_late(&mut self, skipped: u64) {
        let now = self.clock.now();
        for _ in 0..skipped.min(64) {
            self.late.push_back(now);
        }
        while self.late.front().is_some_and(|t| now - t > LATE_WINDOW) || self.late.len() > 64 {
            self.late.pop_front();
        }
        let behind = self.playing && self.late.len() >= LATE_BEHIND;
        if behind != self.behind {
            self.behind = behind;
            self.channel.lock().behind = behind;
            self.channel.wake.notify_all();
        }
    }

    /// Advance to the frame due now. Call it every UI frame (and before reading the state). Returns
    /// how long until the next frame is due, so the UI can schedule its next repaint; `None` while
    /// nothing will change by itself (paused, ended, failed).
    pub fn tick(&mut self) -> Option<f64> {
        let tolerance = self.info.frame_duration.max(1e-3) * 0.5;
        let cur = self.generation;
        let mut due: Option<Arc<VideoFrame>> = None;
        let mut skipped = 0u64;
        let (end, failed, finished, served, next_pts) = {
            let mut s = self.channel.lock();
            if !self.opened
                && let Some(info) = s.info.clone()
            {
                self.info = info;
                self.opened = true;
            }
            // drop what an older seek left behind
            while s.queue.front().is_some_and(|(g, _)| *g != cur) {
                if let Some((_, f)) = s.pop() {
                    s.recycle(f);
                }
            }
            if self.awaiting {
                // the first frame of a seek is the frame at its target: show it whatever its time
                due = s.pop().map(|(_, f)| f);
            } else if self.playing {
                let pos = self.position();
                while s.queue.front().is_some_and(|(_, f)| f.pts <= pos + tolerance) {
                    if let Some((_, f)) = s.pop()
                        && let Some(late) = due.replace(f)
                    {
                        skipped += 1;
                        s.recycle(late);
                    }
                }
            }
            let end = s.generation == cur && s.end && s.queue.is_empty();
            // (the frames decoded before the error still play)
            let failed = s.error.is_some() && s.queue.is_empty();
            if self.error != s.error {
                self.error.clone_from(&s.error);
            }
            (end, failed, s.finished, s.served == cur, s.queue.front().map(|(_, f)| f.pts))
        };
        self.channel.wake.notify_all();
        self.stats.dropped += skipped;
        self.note_late(skipped);
        if let Some(f) = due {
            if self.awaiting {
                self.awaiting = false;
                // playing resumes from the frame the seek landed on
                let at = if self.playing { f.pts.max(0.0) } else { self.anchor_media };
                self.reanchor(at);
            }
            self.show(f);
        } else if self.awaiting && (served || finished) {
            // the seek was answered without a frame (past the last decodable one, an error), or
            // the decode thread is gone: nothing more will come
            self.awaiting = false;
        }
        if failed || finished {
            if std::mem::replace(&mut self.playing, false) {
                let p = self.current.as_ref().map_or(self.anchor_media, |f| f.pts);
                self.reanchor(p);
                if !finished {
                    self.refresh_exact();
                }
            }
            return if self.awaiting { Some(0.01) } else { None };
        }
        if !self.playing {
            return if self.awaiting { Some(0.01) } else { None };
        }
        let pos = self.position();
        if end && pos + tolerance >= self.last_due() {
            self.playing = false;
            self.ended = true;
            let p = self.info.duration.max(self.current.as_ref().map_or(0.0, |f| f.pts));
            self.reanchor(p.min(self.info.duration.max(0.0)));
            self.refresh_exact();
            return if self.awaiting { Some(0.01) } else { None };
        }
        if self.awaiting {
            return Some(0.01);
        }
        // far behind the clock with nothing queued (a long stall): seek there rather than decode
        // every frame in between only to drop it
        if next_pts.is_none() && !end && self.current.as_ref().is_some_and(|f| pos - f.pts > CATCH_UP) {
            self.reanchor(pos);
            self.request(pos);
            return Some(0.01);
        }
        Some(next_pts.map_or(self.info.frame_duration.max(1e-3), |t| (t - pos).max(0.0)).min(0.1))
    }

    /// Frames no wider or taller than `pixels` from now on (0: no limit); a frame on screen that is
    /// bigger is decoded again.
    pub fn set_max_side(&mut self, pixels: usize) {
        {
            let mut s = self.channel.lock();
            if s.max_side == pixels {
                return;
            }
            s.max_side = pixels;
        }
        self.channel.wake.notify_all();
        let too_big = self.current.as_ref().is_some_and(|f| pixels > 0 && (f.width as usize > pixels || f.height as usize > pixels));
        if too_big && !self.awaiting {
            let at = if self.playing { self.position() } else { self.anchor_media };
            let target = self.current.as_ref().map_or(at, |f| if self.playing { at } else { f.pts });
            let anchor = self.anchor_media;
            self.request(target);
            if !self.playing {
                self.anchor_media = anchor;
            }
        }
    }

    /// The time the last frame stops being shown (the end of the clip, or past the last frame).
    fn last_due(&self) -> f64 {
        let last = self.current.as_ref().map_or(0.0, |f| f.pts + self.info.frame_duration);
        last.min(self.info.duration.max(0.0))
    }

    /// Everything agents and the UI read about playback.
    pub fn state(&self) -> Value {
        let s = self.channel.lock();
        json!({
            "photo": self.photo.0,
            "playing": self.playing,
            "ended": self.ended,
            "position": self.position(),
            "opening": !self.opened && self.error.is_none(),
            "duration": self.opened.then_some(self.info.duration),
            "frameRate": if self.info.frame_duration > 0.0 { 1.0 / self.info.frame_duration } else { 0.0 },
            "width": self.info.width,
            "height": self.info.height,
            "orientation": self.info.orientation.to_exif(),
            "codec": self.info.codec,
            "frame": self.current.as_ref().map(|f| json!({"pts": f.pts, "draft": f.draft, "width": f.width, "height": f.height})),
            "behind": self.behind,
            "shown": self.stats.shown,
            "dropped": self.stats.dropped,
            "decoded": s.decoded,
            "queued": s.queue.len(),
            "queuedBytes": s.queued_bytes,
            "audio": self.info.audio.as_ref().map(|a| json!({"track": a, "played": false})),
            "error": self.error,
        })
    }
}

/// Dropping never waits for the decode thread: it may be inside the clip's open on a drive that
/// is waking up. Such a thread ends by itself once the open returns (it sees the stop flag and
/// decodes nothing), so each is bounded by its open; the number of them is not capped: leaving
/// several clips while each is still opening leaves one thread per clip until their opens return.
impl Drop for VideoPlayer {
    fn drop(&mut self) {
        {
            // the flag is set under the lock the decode thread checks it under before waiting, so
            // it can't miss the wake-up below
            let mut s = self.channel.lock();
            self.channel.stop.store(true, Ordering::Relaxed);
            s.request = None;
            // free the frames now; the thread may take a while to notice
            s.clear_queue();
            s.pool.clear();
            s.pool_bytes = 0;
        }
        self.channel.wake.notify_all();
        // never wait for the decode thread: it may be opening the clip on a sleeping drive or
        // decoding a frame; it checks the flag after each and ends by itself
        if let Some(t) = self.thread.take()
            && t.is_finished()
        {
            // a decoder that panicked has already ended; its result tells nothing more
            let _ = t.join();
        }
    }
}

/// Opens the clip at a path for playback: [`crate::video_source::open_file`] in the app, a fake in tests.
pub type Opener = fn(&str) -> Result<Box<dyn FrameSource>, String>;

/// The session's playback: at most one clip plays, the active photo when it is a video.
pub struct Playback {
    pub player: Option<VideoPlayer>,
    pub opener: Opener,
    pub clock: Arc<dyn Clock>,
    /// Whether this build decodes video ([`crate::video_source::available`]; tests that play
    /// fakes say yes without the decoder).
    pub decodes: fn() -> bool,
    /// The largest frame side the frontend can show (its GPU texture limit; 0: no limit).
    pub max_side: usize,
}

impl Playback {
    /// Whether clips can be played in this build (without, the loupe says why and offers no Play).
    pub fn available(&self) -> bool {
        (self.decodes)()
    }
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            player: None,
            opener: crate::video_source::open_file,
            clock: Arc::new(WallClock::default()),
            decodes: crate::video_source::available,
            max_side: 0,
        }
    }
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playback").field("photo", &self.player.as_ref().map(VideoPlayer::photo)).finish()
    }
}

/// The decode thread's work: open the clip, then serve seeks and keep the queue filled.
fn run(open: Open, ch: &Channel) {
    match open() {
        Ok(source) => {
            let info = source.info().clone();
            {
                let mut s = ch.lock();
                s.frame_bytes = (info.width as usize).saturating_mul(info.height as usize).saturating_mul(4).max(1);
                s.info = Some(info);
            }
            // (dropped while opening: nothing to decode)
            if !ch.stop.load(Ordering::Relaxed) {
                decode_loop(source, ch);
            }
        }
        Err(e) => ch.lock().error = Some(e),
    }
}

/// The decode thread: serves the newest seek, then keeps the queue full until the clip ends.
fn decode_loop(mut source: Box<dyn FrameSource>, ch: &Channel) {
    let mut generation = 0u64;
    let mut draft = false;
    let mut behind = false;
    let mut max_side = 0usize;
    loop {
        let seek = {
            let mut s = ch.lock();
            loop {
                if ch.stop.load(Ordering::Relaxed) {
                    return;
                }
                // (also while the queue is full: the next frame decoded follows them)
                if s.draft != draft {
                    draft = s.draft;
                    source.set_draft(draft);
                }
                if s.behind != behind {
                    behind = s.behind;
                    source.set_behind(behind);
                }
                if s.max_side != max_side {
                    max_side = s.max_side;
                    source.set_max_side(max_side);
                }
                if s.request.is_some() || (s.error.is_none() && !s.end && s.wants_frame()) {
                    break;
                }
                s = ch.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
            let req = s.request.take();
            (req, s.buffer())
        };
        let (seek, buffer) = seek;
        let answers_seek = seek.is_some();
        let result = match seek {
            Some(req) => {
                generation = req.generation;
                if req.draft != draft {
                    draft = req.draft;
                    source.set_draft(draft);
                }
                {
                    let mut s = ch.lock();
                    s.generation = generation;
                    if !req.refresh {
                        s.error = None;
                    }
                }
                seek_to(source.as_mut(), ch, req, buffer)
            }
            None => match source.next_frame(&mut || superseded(ch), buffer) {
                Ok(Next::Frame(f)) => Ok(Some(vec![f])),
                Ok(Next::End) => Ok(None),
                Ok(Next::Stopped) => continue,
                Err(e) => Err(e),
            },
        };
        let mut s = ch.lock();
        // a newer seek arrived while this was decoding: what it found (frames, the end, an
        // error) belongs to an older position and must not mark the new one
        if s.generation != generation || s.request.is_some_and(|r| r.generation != generation) {
            continue;
        }
        // (under the same lock as its frames: the player never sees it answered and empty)
        if answers_seek {
            s.served = generation;
        }
        match result {
            Ok(Some(frames)) => {
                for f in frames {
                    s.push(generation, f);
                }
            }
            Ok(None) => s.end = true,
            Err(e) => s.error = Some(e),
        }
    }
}

/// Whether the decode thread should drop what it is doing: stopping, or a newer seek waiting.
fn superseded(ch: &Channel) -> bool {
    ch.stop.load(Ordering::Relaxed) || ch.lock().request.is_some()
}

/// Seek and decode forward to the frame shown at `req.target` (the last one at or before it), and
/// the frame after it when there is one. Past the last frame it returns the last frame. `Ok(None)`
/// when nothing could be decoded or a newer seek arrived (the caller drops this generation).
fn seek_to(source: &mut dyn FrameSource, ch: &Channel, req: Seek, buffer: Vec<u8>) -> Result<Option<Vec<VideoFrame>>, String> {
    source.seek(req.target)?;
    // only timestamp rounding (timescale ticks to seconds), far below any frame interval
    let tolerance = 1e-4;
    let mut best: Option<VideoFrame> = None;
    let mut buffer = Some(buffer);
    for _ in 0..MAX_SEEK_FRAMES {
        let buf = buffer.take().unwrap_or_else(|| ch.lock().buffer());
        let next = match source.next_frame(&mut || superseded(ch), buf) {
            // the target frame was found: show it; the error comes back when decoding on
            Err(_) if best.is_some() => return Ok(best.map(|f| vec![f])),
            r => r?,
        };
        match next {
            Next::Stopped => return Ok(None),
            Next::End => return Ok(best.map(|f| vec![f])),
            Next::Frame(f) if f.pts <= req.target + tolerance => {
                if let Some(old) = best.replace(f) {
                    buffer = Some(old.rgba);
                }
            }
            Next::Frame(f) => {
                // the first frame after the target: the target frame, then this one queued behind it
                return Ok(Some(match best {
                    Some(b) => vec![b, f],
                    None => vec![f],
                }));
            }
        }
    }
    Ok(best.map(|f| vec![f]))
}

#[cfg(test)]
#[path = "video_play_tests.rs"]
mod tests;
