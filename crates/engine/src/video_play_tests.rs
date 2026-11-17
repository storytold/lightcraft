use super::*;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// A clock the test moves by hand (milliseconds).
#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn advance(&self, seconds: f64) {
        self.0.fetch_add((seconds * 1000.0).round() as u64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64 / 1000.0
    }
}

/// `frames` frames at 25 fps, a sync sample every `gop` frames; each frame's first byte is its index.
struct Synthetic {
    info: SourceInfo,
    frames: usize,
    gop: usize,
    next: usize,
    draft: bool,
    /// Fail when decoding this frame index.
    fail_at: Option<usize>,
    /// Shared count of frames decoded (to check what a seek costs).
    decoded: Arc<AtomicU64>,
    /// What the player last said about falling behind.
    behind: Arc<std::sync::atomic::AtomicBool>,
}

fn synthetic(frames: usize, gop: usize) -> (Synthetic, Arc<AtomicU64>) {
    let decoded = Arc::new(AtomicU64::new(0));
    let s = Synthetic {
        info: SourceInfo {
            duration: frames as f64 * 0.04,
            width: 4,
            height: 2,
            frame_duration: 0.04,
            orientation: lightcraft_geom::Orientation::Normal,
            codec: "Test".into(),
            audio: None,
        },
        frames,
        gop,
        next: 0,
        draft: false,
        fail_at: None,
        decoded: decoded.clone(),
        behind: Arc::default(),
    };
    (s, decoded)
}

impl FrameSource for Synthetic {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        let i = ((seconds / 0.04) + 1e-6).floor().max(0.0) as usize;
        self.next = (i.min(self.frames.saturating_sub(1)) / self.gop) * self.gop;
        Ok(())
    }
    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, mut buffer: Vec<u8>) -> Result<Next, String> {
        if stop() {
            return Ok(Next::Stopped);
        }
        if self.fail_at == Some(self.next) {
            return Err("damaged frame".into());
        }
        if self.next >= self.frames {
            return Ok(Next::End);
        }
        let i = self.next;
        self.next += 1;
        self.decoded.fetch_add(1, Ordering::SeqCst);
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        buffer.clear();
        buffer.resize(w * h * 4, 0);
        buffer[0] = i as u8;
        Ok(Next::Frame(VideoFrame { pts: i as f64 * 0.04, width: w as u32, height: h as u32, rgba: buffer, draft: self.draft && i % 2 == 1 }))
    }
    fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }
    fn set_behind(&mut self, behind: bool) {
        self.behind.store(behind, Ordering::SeqCst);
    }
}

fn index(p: &VideoPlayer) -> Option<u8> {
    p.frame().map(|(f, _)| f.rgba[0])
}

/// Tick until `done` holds (the decode thread runs on its own), at most 5 s.
fn tick_until(p: &mut VideoPlayer, done: impl Fn(&VideoPlayer) -> bool) {
    for _ in 0..5000 {
        p.tick();
        if done(p) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out: state {}", p.state());
}

/// Wait until the decoder has filled the queue (so a tick sees every frame that is due).
fn settle(p: &mut VideoPlayer) {
    for _ in 0..5000 {
        {
            let s = p.channel.lock();
            if s.request.is_none() && (s.end || s.queue.len() >= s.capacity) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("decoder did not settle");
}

fn player(frames: usize, gop: usize) -> (VideoPlayer, Arc<ManualClock>, Arc<AtomicU64>) {
    let (src, decoded) = synthetic(frames, gop);
    let clock = Arc::new(ManualClock::default());
    let p = VideoPlayer::start(PhotoId(7), Box::new(src), clock.clone()).unwrap();
    (p, clock, decoded)
}

#[test]
fn opens_paused_on_the_first_frame() {
    let (mut p, _clock, _) = player(50, 10);
    tick_until(&mut p, |p| p.frame().is_some());
    assert_eq!(index(&p), Some(0));
    assert!(!p.playing());
    assert_eq!(p.position(), 0.0);
    // nothing moves while paused
    assert_eq!(p.tick(), None);
}

#[test]
fn plays_on_the_clock_and_drops_late_frames() {
    let (mut p, clock, _) = player(50, 10);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    settle(&mut p);
    clock.advance(0.04);
    p.tick();
    assert_eq!(index(&p), Some(1));
    assert_eq!(p.stats().dropped, 0);
    // the UI stalled for three frames: the newest due frame is shown, the others are counted
    settle(&mut p);
    clock.advance(0.12);
    p.tick();
    assert_eq!(index(&p), Some(4));
    assert_eq!(p.stats().dropped, 2);
}

#[test]
fn stops_at_the_end_and_play_restarts() {
    let (mut p, clock, _) = player(6, 3);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    for _ in 0..40 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    assert!(!p.playing());
    assert_eq!(p.state()["ended"], true);
    assert_eq!(index(&p), Some(5));
    // Play at the end starts over
    p.play();
    assert!(p.playing());
    tick_until(&mut p, |p| index(p) == Some(0));
}

#[test]
fn seek_shows_the_frame_at_the_target_paused() {
    let (mut p, _clock, decoded) = player(100, 25);
    tick_until(&mut p, |p| p.frame().is_some());
    settle(&mut p);
    let before = decoded.load(Ordering::SeqCst);
    p.seek(1.23); // frame 30 (1.20 s) is shown until 1.24 s
    tick_until(&mut p, |p| index(p) == Some(30));
    assert!(!p.playing());
    assert!((p.position() - 1.23).abs() < 1e-9);
    // it decoded forward from the sync sample at frame 25, not from the start
    let cost = decoded.load(Ordering::SeqCst) - before;
    assert!(cost < 25, "decoded {cost} frames for one seek");
}

#[test]
fn seek_clamps_hostile_times() {
    let (mut p, _clock, _) = player(10, 5);
    tick_until(&mut p, |p| p.frame().is_some());
    p.seek(f64::NAN);
    tick_until(&mut p, |p| index(p) == Some(0));
    p.seek(1e300);
    tick_until(&mut p, |p| index(p) == Some(9));
    assert!(p.position() <= p.info().unwrap().duration);
    p.seek(-5.0);
    tick_until(&mut p, |p| index(p) == Some(0));
}

#[test]
fn step_forward_and_back() {
    let (mut p, _clock, _) = player(30, 10);
    tick_until(&mut p, |p| p.frame().is_some());
    p.step(1);
    tick_until(&mut p, |p| index(p) == Some(1));
    p.step(1);
    tick_until(&mut p, |p| index(p) == Some(2));
    p.step(-1);
    tick_until(&mut p, |p| index(p) == Some(1));
    // a step back across a sync sample decodes from the earlier one
    p.seek(0.40);
    tick_until(&mut p, |p| index(p) == Some(10));
    p.step(-1);
    tick_until(&mut p, |p| index(p) == Some(9));
    assert!(!p.playing());
}

#[test]
fn pausing_replaces_a_draft_frame_with_an_exact_one() {
    let (mut p, clock, _) = player(50, 10);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    // frames queued while paused were decoded exactly; a seek while playing decodes draft
    p.seek(0.0);
    tick_until(&mut p, |p| !p.awaiting && index(p) == Some(0));
    settle(&mut p);
    clock.advance(0.04);
    p.tick();
    // odd frames come out draft while playing
    assert_eq!(index(&p), Some(1));
    assert!(p.frame().unwrap().0.draft);
    p.pause();
    tick_until(&mut p, |p| p.frame().is_some_and(|(f, _)| !f.draft));
    assert_eq!(index(&p), Some(1));
}

#[test]
fn scrubbing_coalesces_to_the_newest_seek() {
    let (mut p, _clock, _) = player(200, 50);
    tick_until(&mut p, |p| p.frame().is_some());
    for i in 0..40 {
        p.seek(f64::from(i) * 0.1);
    }
    tick_until(&mut p, |p| index(p) == Some(97));
    assert!((p.position() - 3.9).abs() < 1e-9);
}

#[test]
fn a_decoder_error_stops_playback_and_is_reported() {
    let (mut src, _) = synthetic(50, 10);
    src.fail_at = Some(3);
    let clock = Arc::new(ManualClock::default());
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), clock.clone()).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(2));
        clock.advance(0.04);
        p.tick();
    }
    assert!(!p.playing());
    assert_eq!(p.error().as_deref(), Some("damaged frame"));
    assert_eq!(p.state()["error"], "damaged frame");
}

#[test]
fn the_queue_is_bounded_by_bytes() {
    let (mut src, _) = synthetic(500, 500);
    // a 4K frame: the queue keeps at most QUEUE_BYTES of them, and never fewer than two
    src.info.width = 3840;
    src.info.height = 2160;
    let clock = Arc::new(ManualClock::default());
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), clock).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    settle(&mut p);
    let cap = p.channel.lock().capacity;
    assert_eq!(cap, (QUEUE_BYTES / (3840 * 2160 * 4)).clamp(QUEUE_MIN, QUEUE_MAX));
    assert!(p.channel.lock().queue.len() <= cap);
}

#[test]
fn dropping_the_player_stops_the_decode_thread() {
    let (mut p, _clock, decoded) = player(100_000, 100_000);
    tick_until(&mut p, |p| p.frame().is_some());
    drop(p);
    let after = decoded.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(decoded.load(Ordering::SeqCst), after);
}

#[test]
fn a_clip_that_cannot_be_opened_reports_why_without_blocking() {
    let clock = Arc::new(ManualClock::default());
    let open: Open = Box::new(|| {
        // a slow drive: the caller is not held up
        std::thread::sleep(Duration::from_millis(50));
        Err("The clip could not be opened: no such file".to_string())
    });
    let started = Instant::now();
    let mut p = VideoPlayer::open(PhotoId(3), open, clock).unwrap();
    assert!(started.elapsed() < Duration::from_millis(40));
    assert_eq!(p.state()["opening"], true);
    assert!(p.info().is_none());
    tick_until(&mut p, |p| p.error().is_some());
    assert_eq!(p.state()["opening"], false);
    p.play();
    p.tick();
    assert!(!p.playing());
    assert!(p.frame().is_none());
}

#[test]
fn dropping_late_frames_tells_the_source_it_is_behind_until_playback_pauses() {
    let (src, _) = synthetic(500, 10);
    let behind = src.behind.clone();
    let clock = Arc::new(ManualClock::default());
    let mut p = VideoPlayer::start(PhotoId(7), Box::new(src), clock.clone()).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    // on time: not behind
    for _ in 0..5 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    assert_eq!(p.state()["behind"], false);
    // two stalls of three frames each within a second: behind
    for _ in 0..2 {
        settle(&mut p);
        clock.advance(0.12);
        p.tick();
    }
    assert_eq!(p.state()["behind"], true);
    tick_until(&mut p, |_| behind.load(Ordering::SeqCst));
    // a second on time: no longer behind
    for _ in 0..30 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    assert_eq!(p.state()["behind"], false);
    tick_until(&mut p, |_| !behind.load(Ordering::SeqCst));
    // paused is never behind
    settle(&mut p);
    clock.advance(0.2);
    p.tick();
    p.pause();
    p.tick();
    assert_eq!(p.state()["behind"], false);
}
