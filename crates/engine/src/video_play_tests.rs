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
    /// The frames' real size when it differs from the header's (`info`).
    real_size: Option<(usize, usize)>,
    /// From this frame on, frames come at half the size (as when playback falls behind).
    small_from: Option<usize>,
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
            decoder_bytes: 0,
        },
        frames,
        gop,
        next: 0,
        draft: false,
        fail_at: None,
        decoded: decoded.clone(),
        behind: Arc::default(),
        real_size: None,
        small_from: None,
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
        let (w, h) = self.real_size.unwrap_or((self.info.width as usize, self.info.height as usize));
        let (w, h) = if self.small_from.is_some_and(|n| i >= n) { (w / 2, h / 2) } else { (w, h) };
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
            if s.request.is_none() && (s.end || s.error.is_some() || !s.wants_frame()) {
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

/// Queued frames plus kept buffers, in bytes, and the frames queued, once the decoder has settled.
fn held(p: &mut VideoPlayer) -> (usize, usize) {
    tick_until(p, |p| p.frame().is_some());
    settle(p);
    let s = p.channel.lock();
    (s.queued_bytes + s.pool_bytes, s.queue.len())
}

#[test]
fn the_queue_is_bounded_by_the_bytes_of_the_frames_decoded() {
    // the header says 4 × 2, the pictures are 4K: the frames' real size counts
    let (mut src, _) = synthetic(500, 500);
    src.real_size = Some((3840, 2160));
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), Arc::new(ManualClock::default())).unwrap();
    let (bytes, queued) = held(&mut p);
    assert!(bytes <= QUEUE_BYTES, "{bytes} bytes held");
    // 160 MiB is five 4K frames (the one on screen not counted)
    assert_eq!(queued, QUEUE_BYTES / (3840 * 2160 * 4), "4K frames queued");
    // step through some frames: buffers are reused, never more than the budget held
    for _ in 0..6 {
        p.step(1);
        tick_until(&mut p, |p| !p.awaiting);
        let (bytes, _) = held(&mut p);
        assert!(bytes <= QUEUE_BYTES, "{bytes} bytes held after steps");
    }
    // an 8K frame (133 MB) alone: one frame queued, however large
    let (mut src, _) = synthetic(500, 500);
    src.real_size = Some((7680, 4320));
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), Arc::new(ManualClock::default())).unwrap();
    let (_, queued) = held(&mut p);
    assert_eq!(queued, 1);
    // tiny frames: at most QUEUE_MAX
    let (mut p, _clock, _) = player(500, 500);
    let (_, queued) = held(&mut p);
    assert_eq!(queued, QUEUE_MAX);
}

#[test]
fn dropping_the_player_stops_the_decode_thread() {
    let (mut p, _clock, decoded) = player(100_000, 100_000);
    tick_until(&mut p, |p| p.frame().is_some());
    let ch = p.channel.clone();
    drop(p);
    for _ in 0..5000 {
        if ch.lock().finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(ch.lock().finished, "the decode thread did not end");
    let after = decoded.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(decoded.load(Ordering::SeqCst), after);
    assert!(ch.lock().queue.is_empty(), "the frames are freed when the player goes");
}

#[test]
fn dropping_the_player_never_waits_for_a_clip_being_opened() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let open: Open = Box::new(move || {
        // a sleeping drive: opening takes until the test lets it go
        let (m, c) = &*g;
        let mut go = m.lock().unwrap();
        while !*go {
            go = c.wait(go).unwrap();
        }
        let (src, _) = synthetic(10, 5);
        Ok(Box::new(src) as Box<dyn FrameSource>)
    });
    let p = VideoPlayer::open(PhotoId(3), open, Arc::new(ManualClock::default())).unwrap();
    let ch = p.channel.clone();
    // (dropped on a thread of its own: without the fix the drop waits for the open, and the test
    // fails instead of hanging)
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started = Instant::now();
        drop(p);
        let _ = tx.send(started.elapsed());
    });
    let waited = rx.recv_timeout(Duration::from_secs(2));
    // let the open finish either way, so nothing is left waiting
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let waited = waited.expect("dropping the player waited for the clip being opened");
    assert!(waited < Duration::from_millis(200), "dropping waited {waited:?} for the open");
    // once the drive answered, the thread saw it was dropped and ends without decoding
    for _ in 0..5000 {
        if ch.lock().finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let s = ch.lock();
    assert!(s.finished && s.decoded == 0 && s.queue.is_empty());
}

#[test]
fn dropping_the_player_never_hangs_on_a_lost_wakeup() {
    struct Slow(Synthetic);
    impl FrameSource for Slow {
        fn info(&self) -> &SourceInfo {
            self.0.info()
        }
        fn seek(&mut self, s: f64) -> Result<(), String> {
            self.0.seek(s)
        }
        fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, b: Vec<u8>) -> Result<Next, String> {
            self.0.next_frame(stop, b)
        }
        fn set_draft(&mut self, on: bool) {
            // widens the window between the decode thread's stop check and its wait
            std::thread::sleep(Duration::from_millis(50));
            self.0.set_draft(on);
        }
    }
    let (src, _) = synthetic(500, 10);
    let mut p = VideoPlayer::start(PhotoId(7), Box::new(Slow(src)), Arc::new(ManualClock::default())).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    settle(&mut p);
    // queue full and paused: the decode thread waits; a draft change wakes it into set_draft
    p.channel.lock().draft = true;
    p.channel.wake.notify_all();
    std::thread::sleep(Duration::from_millis(10));
    let ch = p.channel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(p);
        let _ = tx.send(());
    });
    assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok(), "dropping the player hung");
    // and the thread itself ended (it was not left waiting forever)
    for _ in 0..3000 {
        if ch.lock().finished {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the decode thread missed the stop");
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

#[test]
fn pause_right_after_a_seek_keeps_the_seek_target() {
    let (mut p, clock, _) = player(200, 50);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    settle(&mut p);
    clock.advance(0.04);
    p.tick();
    // seek while playing, pause before the seek's frame arrived
    p.seek(4.0);
    p.pause();
    tick_until(&mut p, |p| !p.awaiting);
    assert_eq!(index(&p), Some(100), "{}", p.state());
    assert!((p.position() - 4.0).abs() < 1e-9);
}

#[test]
fn a_step_right_after_a_seek_steps_from_the_seek_target() {
    let (mut p, _clock, _) = player(200, 50);
    tick_until(&mut p, |p| p.frame().is_some());
    settle(&mut p);
    p.seek(4.0);
    p.step(1);
    tick_until(&mut p, |p| !p.awaiting);
    assert_eq!(index(&p), Some(101), "{}", p.state());
    p.seek(2.0);
    p.step(-1);
    tick_until(&mut p, |p| !p.awaiting);
    assert_eq!(index(&p), Some(49), "{}", p.state());
}

/// Frames at the given times, every one a sync sample (a variable frame rate).
struct Timed {
    info: SourceInfo,
    pts: Vec<f64>,
    next: usize,
}

impl FrameSource for Timed {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.next = self.pts.iter().rposition(|t| *t <= seconds + 1e-9).unwrap_or(0);
        Ok(())
    }
    fn next_frame(&mut self, _: &mut dyn FnMut() -> bool, mut buffer: Vec<u8>) -> Result<Next, String> {
        let Some(&pts) = self.pts.get(self.next) else { return Ok(Next::End) };
        buffer.clear();
        buffer.resize(4, 0);
        buffer[0] = self.next as u8;
        self.next += 1;
        Ok(Next::Frame(VideoFrame { pts, width: 1, height: 1, rgba: buffer, draft: false }))
    }
    fn set_draft(&mut self, _: bool) {}
}

#[test]
fn a_step_back_with_a_variable_frame_rate_lands_on_the_previous_frame() {
    // the nominal frame lasts 50 ms, but three frames come 20 ms apart
    let pts = vec![0.0, 0.05, 0.10, 0.12, 0.14, 0.19];
    let info = SourceInfo { duration: 0.24, width: 1, height: 1, frame_duration: 0.05, ..SourceInfo::pending() };
    let src = Timed { info, pts, next: 0 };
    let mut p = VideoPlayer::start(PhotoId(2), Box::new(src), Arc::new(ManualClock::default())).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    p.seek(0.14);
    tick_until(&mut p, |p| index(p) == Some(4));
    for want in [3u8, 2, 1, 0, 0] {
        p.step(-1);
        tick_until(&mut p, |p| !p.awaiting);
        assert_eq!(index(&p), Some(want), "{}", p.state());
    }
}

#[test]
fn a_panicking_decoder_stops_playback_with_a_reason() {
    struct Boom;
    impl FrameSource for Boom {
        fn info(&self) -> &SourceInfo {
            Box::leak(Box::new(SourceInfo::pending()))
        }
        fn seek(&mut self, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn next_frame(&mut self, _: &mut dyn FnMut() -> bool, _: Vec<u8>) -> Result<Next, String> {
            let v: Vec<u8> = Vec::new();
            let i = v.len() + 3;
            Ok(Next::Frame(VideoFrame { pts: f64::from(v[i]), width: 1, height: 1, rgba: vec![0; 4], draft: false }))
        }
        fn set_draft(&mut self, _: bool) {}
    }
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(Boom), Arc::new(ManualClock::default())).unwrap();
    tick_until(&mut p, |p| p.error().is_some());
    assert!(p.error().is_some_and(|e| e.contains("failed unexpectedly")), "{}", p.state());
    assert!(p.finished());
    // nothing will come: no repaints asked for, and Play does nothing
    assert_eq!(p.tick(), None);
    p.play();
    assert_eq!(p.tick(), None);
    assert!(!p.playing());
}

#[test]
fn playback_that_ends_on_a_draft_frame_shows_it_exactly() {
    // six frames: the last (5) is odd, so it comes out draft while playing
    let (mut p, clock, _) = player(6, 3);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    // (frames queued while paused were decoded exactly; a seek while playing decodes draft)
    p.seek(0.0);
    tick_until(&mut p, |p| !p.awaiting);
    assert!(p.playing());
    for _ in 0..40 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    assert_eq!(p.state()["ended"], true);
    tick_until(&mut p, |p| !p.awaiting && p.frame().is_some_and(|(f, _)| !f.draft));
    assert_eq!(index(&p), Some(5));
    assert_eq!(p.state()["ended"], true);
    assert!((p.position() - 0.24).abs() < 1e-9, "{}", p.state());
}

#[test]
fn a_step_back_while_the_end_is_decoded_exactly_shows_the_frame_before() {
    let (mut p, clock, _) = player(6, 3);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    p.seek(0.0);
    tick_until(&mut p, |p| !p.awaiting);
    for _ in 0..40 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
        if p.state()["ended"] == true {
            break;
        }
    }
    // the last frame (5) was draft: its exact decode is on its way, the position stays at the end
    assert_eq!(p.state()["ended"], true);
    assert!(p.awaiting, "{}", p.state());
    p.step(-1);
    tick_until(&mut p, |p| !p.awaiting);
    assert_eq!(index(&p), Some(4), "{}", p.state());
}

#[test]
fn recycled_buffers_never_hold_more_than_the_budget_when_frames_shrink() {
    // 4K frames, then (from frame 20 on) frames at half size: buffers kept from the 4K frames
    // must not hold their 33 MB for 8 MB frames
    let (mut src, _) = synthetic(500, 500);
    src.real_size = Some((3840, 2160));
    src.small_from = Some(20);
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), Arc::new(ManualClock::default())).unwrap();
    let held_now = |p: &mut VideoPlayer| {
        tick_until(p, |p| p.frame().is_some());
        settle(p);
        let s = p.channel.lock();
        let real: usize = s.queue.iter().map(|(_, f)| f.rgba.capacity()).sum::<usize>() + s.pool.iter().map(Vec::capacity).sum::<usize>();
        // what the player counts is what the buffers hold
        assert_eq!(real, s.queued_bytes + s.pool_bytes);
        real
    };
    for i in 0..40 {
        p.step(1);
        tick_until(&mut p, |p| !p.awaiting);
        let real = held_now(&mut p);
        assert!(real <= QUEUE_BYTES, "{real} bytes held after step {i}");
    }
    // past the switch no 4K buffer is held (queued or kept), so the half-size frames fill the
    // queue: 4K buffers reused for them would hold 33 MB each and leave room for only five
    let s = p.channel.lock();
    let small = 2 * 1920 * 1080 * 4;
    assert!(s.queue.iter().all(|(_, f)| f.rgba.capacity() <= small), "a queued frame holds a 4K buffer");
    assert!(s.pool.iter().all(|b| b.capacity() <= small), "a 4K buffer is still kept");
    assert_eq!(s.queue.len(), QUEUE_MAX);
}

#[test]
fn an_error_while_playing_leaves_the_exact_frame_and_the_reason() {
    let (mut src, _) = synthetic(50, 10);
    src.fail_at = Some(4);
    let clock = Arc::new(ManualClock::default());
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), clock.clone()).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    p.seek(0.0);
    tick_until(&mut p, |p| !p.awaiting);
    for _ in 0..3 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    // frame 3 is on screen, drafted; decoding frame 4 fails
    assert_eq!(index(&p), Some(3));
    assert!(p.frame().unwrap().0.draft);
    for _ in 0..20 {
        settle(&mut p);
        clock.advance(0.04);
        p.tick();
    }
    tick_until(&mut p, |p| !p.awaiting);
    assert!(!p.playing());
    assert_eq!(index(&p), Some(3));
    assert!(!p.frame().unwrap().0.draft, "the frame left on screen is exact");
    assert_eq!(p.error().as_deref(), Some("damaged frame"));
    assert_eq!(p.tick(), None);
}

#[test]
fn a_long_stall_seeks_instead_of_decoding_every_frame_in_between() {
    let (mut p, clock, decoded) = player(500, 10);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    settle(&mut p);
    let before = decoded.load(Ordering::SeqCst);
    // nothing ticked the player for three seconds (75 frames)
    clock.advance(3.0);
    p.tick();
    tick_until(&mut p, |p| !p.awaiting);
    let at = index(&p).unwrap();
    assert!((74..=76).contains(&at), "frame {at}");
    let cost = decoded.load(Ordering::SeqCst) - before;
    assert!(cost < 40, "decoded {cost} frames to catch up");
    assert!(p.playing());
}

/// Frames the test hands over one by one; `stop` is ignored (a decoder busy with one picture).
struct Gate {
    info: SourceInfo,
    frames: std::sync::mpsc::Receiver<Result<Next, String>>,
}

impl FrameSource for Gate {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn seek(&mut self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn next_frame(&mut self, _: &mut dyn FnMut() -> bool, _: Vec<u8>) -> Result<Next, String> {
        self.frames.recv().unwrap_or_else(|_| Err("closed".into()))
    }
    fn set_draft(&mut self, _: bool) {}
}

fn frame_at(pts: f64) -> Result<Next, String> {
    Ok(Next::Frame(VideoFrame { pts, width: 1, height: 1, rgba: vec![(pts * 25.0).round() as u8, 0, 0, 0], draft: false }))
}

#[test]
fn what_an_older_seek_finds_never_marks_the_newer_one() {
    let (tx, rx) = std::sync::mpsc::channel();
    let info = SourceInfo { duration: 10.0, width: 1, height: 1, frame_duration: 0.04, ..SourceInfo::pending() };
    let mut p = VideoPlayer::start(PhotoId(4), Box::new(Gate { info, frames: rx }), Arc::new(ManualClock::default())).unwrap();
    // the first seek's decode is under way when a newer seek comes
    std::thread::sleep(Duration::from_millis(20));
    p.seek(1.0);
    // the older seek ends (the clip's end, as far as it got) only now
    tx.send(Ok(Next::End)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert!(!p.channel.lock().end, "the older seek's end marked the newer one");
    tx.send(frame_at(1.0)).unwrap();
    tx.send(frame_at(1.04)).unwrap();
    tick_until(&mut p, |p| !p.awaiting);
    assert_eq!(index(&p), Some(25));
    assert!(!p.channel.lock().end);
    assert_eq!(p.state()["ended"], false);
    drop(tx);
}

#[test]
fn a_frame_bigger_than_the_gpu_allows_is_decoded_again_within_it() {
    struct Sized(Synthetic, Arc<AtomicU64>);
    impl FrameSource for Sized {
        fn info(&self) -> &SourceInfo {
            self.0.info()
        }
        fn seek(&mut self, s: f64) -> Result<(), String> {
            self.0.seek(s)
        }
        fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, b: Vec<u8>) -> Result<Next, String> {
            let max = self.1.load(Ordering::SeqCst) as usize;
            self.0.real_size = Some(if max > 0 && max < 4 { (2, 1) } else { (4, 2) });
            self.0.next_frame(stop, b)
        }
        fn set_draft(&mut self, on: bool) {
            self.0.set_draft(on);
        }
        fn set_max_side(&mut self, pixels: usize) {
            self.1.store(pixels as u64, Ordering::SeqCst);
        }
    }
    let (src, _) = synthetic(50, 10);
    let max = Arc::new(AtomicU64::new(0));
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(Sized(src, max.clone())), Arc::new(ManualClock::default())).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    assert_eq!(p.frame().unwrap().0.width, 4);
    // the frontend learns its texture limit: the frame on screen is too wide and comes again
    p.set_max_side(3);
    tick_until(&mut p, |p| p.frame().is_some_and(|(f, _)| f.width == 2));
    assert_eq!(index(&p), Some(0));
    assert_eq!(max.load(Ordering::SeqCst), 3);
    // a player opened with the limit asks for it before the first frame
    let (src, _) = synthetic(50, 10);
    let max = Arc::new(AtomicU64::new(0));
    let open: Open = Box::new(move || Ok(Box::new(Sized(src, max)) as Box<dyn FrameSource>));
    let mut p = VideoPlayer::open_limited(PhotoId(1), open, Arc::new(ManualClock::default()), 3).unwrap();
    tick_until(&mut p, |p| p.frame().is_some());
    assert_eq!(p.frame().unwrap().0.width, 2);
}

#[test]
fn the_state_never_shows_an_error_with_playback_still_running() {
    let (mut p, _clock, _) = player(500, 500);
    tick_until(&mut p, |p| p.frame().is_some());
    p.play();
    p.tick();
    // (the decoder idle on a full queue, so nothing arrives behind the error)
    settle(&mut p);
    // the decode thread records an error between two ticks (here: by hand, at a known moment)
    {
        let mut s = p.channel.lock();
        s.clear_queue();
        s.error = Some("damaged frame".into());
    }
    // until a tick has stopped playback for it, the state reports neither
    let v = p.state();
    assert_eq!((v["playing"].clone(), v["error"].clone()), (serde_json::json!(true), serde_json::Value::Null), "{v}");
    assert_eq!(p.error(), None);
    // the tick stops playback and the state reports the reason with it
    p.tick();
    let v = p.state();
    assert_eq!((v["playing"].clone(), v["error"].clone()), (serde_json::json!(false), serde_json::json!("damaged frame")), "{v}");
}
