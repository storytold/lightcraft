//! The playback state machine: one clip at a time, playing or paused, its position driven by a
//! monotonic clock while playing. Positions are seconds in the clip, kept inside the trim
//! `[in, out]`; playing to `out` stops there (or wraps to `in` when looping). Seeks snap to frame
//! starts; frame steps move whole frames.

use lcc_catalog::PhotoId;
use serde::Serialize;

use super::VideoMeta;

/// Most frames one step may move (a hostile `delta` can't overflow the frame maths).
pub const MAX_STEP: i64 = 1_000_000;

/// The play range of a clip: `(in, out)` with `0 ≤ in < out ≤ duration`.
pub fn bounds(m: &VideoMeta) -> (f64, f64) {
    let d = if m.duration.is_finite() { m.duration.max(0.0) } else { 0.0 };
    let out = m.trim_out.filter(|t| t.is_finite()).unwrap_or(d).clamp(0.0, d);
    let tin = if m.trim_in.is_finite() { m.trim_in.clamp(0.0, out) } else { 0.0 };
    (tin, out)
}

/// Frames per second (0 for a clip without a rate).
pub fn fps(m: &VideoMeta) -> f64 {
    if m.fps_num == 0 || m.fps_den == 0 { 0.0 } else { f64::from(m.fps_num) / f64::from(m.fps_den) }
}

/// The start of the frame shown at `t`.
pub fn snap(t: f64, fps: f64) -> f64 {
    if fps <= 0.0 || !t.is_finite() {
        return t;
    }
    // a hair of tolerance so a time stored as n/fps stays on frame n
    ((t * fps) + 1e-6).floor() / fps
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Player {
    /// The clip the state belongs to; another clip starts paused at its trim in.
    pub photo: Option<PhotoId>,
    pub playing: bool,
    /// Wrap to `in` at `out` instead of stopping.
    pub looping: bool,
    /// Position at `anchor_clock` (the current position while paused).
    anchor_pos: f64,
    anchor_clock: f64,
}

impl Player {
    /// Follow `photo`: a different clip resets to paused at `bounds.0`.
    pub fn attach(&mut self, photo: PhotoId, bounds: (f64, f64)) {
        if self.photo != Some(photo) {
            *self = Player { photo: Some(photo), anchor_pos: bounds.0, ..Player::default() };
        }
    }

    /// Where playback is at `now` (clamped into `bounds`, wrapped when looping).
    pub fn position(&self, now: f64, (tin, out): (f64, f64)) -> f64 {
        let mut p = self.anchor_pos;
        if self.playing {
            let dt = now - self.anchor_clock;
            p += if dt.is_finite() { dt.max(0.0) } else { 0.0 };
            if p >= out && self.looping && out > tin {
                p = tin + (p - tin).rem_euclid(out - tin);
            }
        }
        if p.is_finite() { p.clamp(tin, out) } else { tin }
    }

    /// Advance to `now`: playing to the end without looping stops there. Returns the position.
    pub fn tick(&mut self, now: f64, b: (f64, f64)) -> f64 {
        let p = self.position(now, b);
        if self.playing && !self.looping && p >= b.1 {
            self.playing = false;
        }
        if !self.playing {
            self.anchor_pos = p;
        }
        p
    }

    /// Start playing (from `in` when at the end).
    pub fn play(&mut self, now: f64, b: (f64, f64)) {
        let mut p = self.tick(now, b);
        if p >= b.1 {
            p = b.0;
        }
        self.anchor_pos = p;
        self.anchor_clock = now;
        self.playing = b.1 > b.0;
    }

    pub fn pause(&mut self, now: f64, b: (f64, f64)) {
        self.anchor_pos = self.position(now, b);
        self.playing = false;
    }

    /// Jump to `t`, snapped to its frame and kept inside the trim; playing goes on from there.
    pub fn seek(&mut self, t: f64, now: f64, b: (f64, f64), fps: f64) -> f64 {
        let t = if t.is_finite() { t } else { b.0 };
        let p = snap(t.clamp(b.0, b.1), fps).clamp(b.0, b.1);
        self.anchor_pos = p;
        self.anchor_clock = now;
        p
    }

    /// Pause and move `delta` whole frames (clamped to the trim). Returns the new position.
    pub fn step(&mut self, delta: i64, now: f64, b: (f64, f64), fps: f64) -> f64 {
        let p = self.position(now, b);
        self.playing = false;
        if fps <= 0.0 {
            self.anchor_pos = p;
            return p;
        }
        let frame = (p * fps + 1e-6).floor() + delta.clamp(-MAX_STEP, MAX_STEP) as f64;
        let mut t = (frame / fps).clamp(b.0, b.1);
        // stepping back from the trim's first partial frame lands on `in`, not before it
        if t < b.0 {
            t = b.0;
        }
        self.anchor_pos = t;
        self.anchor_clock = now;
        t
    }

    /// Keep the position inside new bounds (after a trim change).
    pub fn clamp_to(&mut self, now: f64, b: (f64, f64)) {
        let p = self.position(now, b);
        self.anchor_pos = p;
        self.anchor_clock = now;
    }
}
