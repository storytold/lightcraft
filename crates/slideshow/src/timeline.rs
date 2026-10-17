//! When each slide shows: the order (random or not), the intro and ending screens, slide and fade
//! durations (or durations fitted to the music), repeat, and the pan-and-zoom motion of each slide.
//!
//! The show is a list of segments (intro, the slides, ending). Each holds for `hold` seconds and
//! then fades for `fade` seconds into the next one, so segment `k` starts at `k * (hold + fade)`.

use crate::settings::Settings;

/// One part of the show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Intro,
    /// Index into the photo list the show was planned for.
    Slide(usize),
    Ending,
}

/// What to draw at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Position of `current` in [`Plan::segments`].
    pub index: usize,
    pub current: Segment,
    /// 0..1 through the current segment (drives pan and zoom).
    pub progress: f32,
    /// The segment fading in and how far (0..1).
    pub next: Option<(Segment, f32)>,
    /// The show is over (not repeating, past the end): `current` is the last segment.
    pub done: bool,
}

/// A planned show.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub segments: Vec<Segment>,
    /// Seconds each segment holds before its fade.
    pub hold: f64,
    pub fade: f64,
    pub repeat: bool,
}

/// A small deterministic generator (xorshift64*), for random order and pan directions.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// The photo order: `0..n`, shuffled (Fisher–Yates) when `random`, with `seed`.
pub fn order(n: usize, random: bool, seed: u64) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    if random && n > 1 {
        let mut s = seed | 1;
        for i in (1..n).rev() {
            let j = (next_rand(&mut s) % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
    v
}

impl Plan {
    /// Plan a show of `n` photos. `music_secs`: the total length of the music, used when the
    /// settings fit the slides to it.
    pub fn new(n: usize, s: &Settings, seed: u64, music_secs: Option<f64>) -> Plan {
        let mut segments = Vec::with_capacity(n.saturating_add(2));
        if s.titles.intro.enabled {
            segments.push(Segment::Intro);
        }
        segments.extend(order(n, s.playback.random, seed).into_iter().map(Segment::Slide));
        if s.titles.ending.enabled {
            segments.push(Segment::Ending);
        }
        let fade = f64::from(s.playback.fade_secs).max(0.0);
        let mut hold = f64::from(s.playback.slide_secs).max(0.1);
        if s.music.enabled
            && s.music.fit_to_music
            && let Some(total) = music_secs.filter(|t| t.is_finite() && *t > 0.0)
            && !segments.is_empty()
        {
            // segments * hold + (segments - 1) * fade = total
            let k = segments.len() as f64;
            hold = ((total - (k - 1.0) * fade) / k).max(0.1);
        }
        Plan { segments, hold, fade, repeat: s.playback.repeat }
    }

    fn period(&self) -> f64 {
        self.hold + self.fade
    }

    /// The length of one pass, in seconds.
    pub fn duration(&self) -> f64 {
        if self.segments.is_empty() {
            return 0.0;
        }
        self.segments.len() as f64 * self.period() - if self.repeat { 0.0 } else { self.fade }
    }

    /// When segment `k` starts.
    pub fn start_of(&self, k: usize) -> f64 {
        k as f64 * self.period()
    }

    /// What shows `t` seconds into the show; `None` for an empty show.
    pub fn frame_at(&self, t: f64) -> Option<Frame> {
        let n = self.segments.len();
        let last = *self.segments.last()?;
        let period = self.period();
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let t = if self.repeat {
            t % (n as f64 * period)
        } else if t >= self.duration() {
            return Some(Frame { index: n - 1, current: last, progress: 1.0, next: None, done: true });
        } else {
            t
        };
        let k = ((t / period).floor() as usize).min(n - 1);
        let local = t - self.start_of(k);
        let current = *self.segments.get(k)?;
        let progress = (local / period).clamp(0.0, 1.0) as f32;
        let next = if local > self.hold && self.fade > 0.0 {
            let following = if k + 1 < n {
                self.segments.get(k + 1).copied()
            } else if self.repeat {
                self.segments.first().copied()
            } else {
                None
            };
            following.map(|s| (s, ((local - self.hold) / self.fade).clamp(0.0, 1.0) as f32))
        } else {
            None
        };
        Some(Frame { index: k, current, progress, next, done: false })
    }
}

/// Pan and zoom for a slide: the visible window of the photo as (zoom ≥ 1, centre x, centre y), the
/// centre in 0..1 photo coordinates. `amount` 0..1, `progress` 0..1 through the slide; `key` picks
/// the direction (alternating zoom in / out, a pan direction from a hash).
pub fn pan_zoom(key: usize, progress: f32, amount: f32) -> (f32, f32, f32) {
    let amount = if amount.is_finite() { amount.clamp(0.0, 1.0) } else { 0.0 };
    let p = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
    let max = 1.0 + 0.3 * amount;
    let zoom = if key.is_multiple_of(2) { 1.0 + (max - 1.0) * p } else { max - (max - 1.0) * p };
    let mut s = (key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let angle = (next_rand(&mut s) % 360) as f32 * std::f32::consts::PI / 180.0;
    let room = (1.0 - 1.0 / zoom) / 2.0;
    let travel = (2.0 * p - 1.0) * room;
    (zoom, 0.5 + angle.cos() * travel, 0.5 + angle.sin() * travel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(intro: bool, ending: bool, repeat: bool) -> Settings {
        let mut s = Settings::default();
        s.titles.intro.enabled = intro;
        s.titles.ending.enabled = ending;
        s.playback.repeat = repeat;
        s.playback.slide_secs = 4.0;
        s.playback.fade_secs = 1.0;
        s
    }

    #[test]
    fn walks_segments_with_fades() {
        let p = Plan::new(3, &settings(true, true, false), 1, None);
        assert_eq!(p.segments, vec![Segment::Intro, Segment::Slide(0), Segment::Slide(1), Segment::Slide(2), Segment::Ending]);
        assert_eq!(p.duration(), 24.0);
        let f = p.frame_at(2.0).unwrap();
        assert_eq!((f.current, f.next), (Segment::Intro, None));
        let f = p.frame_at(4.5).unwrap();
        assert_eq!(f.current, Segment::Intro);
        assert_eq!(f.next, Some((Segment::Slide(0), 0.5)));
        assert_eq!(p.frame_at(5.0).unwrap().current, Segment::Slide(0));
        let end = p.frame_at(100.0).unwrap();
        assert!(end.done);
        assert_eq!(end.current, Segment::Ending);
    }

    #[test]
    fn repeat_wraps_and_fades_into_the_first() {
        let p = Plan::new(2, &settings(false, false, true), 1, None);
        let f = p.frame_at(9.5).unwrap();
        assert_eq!(f.current, Segment::Slide(1));
        assert_eq!(f.next, Some((Segment::Slide(0), 0.5)));
        assert_eq!(p.frame_at(10.0 + 1.0).unwrap().current, Segment::Slide(0));
        assert!(!p.frame_at(1e9).unwrap().done);
    }

    #[test]
    fn hostile_times_and_empty_shows() {
        let p = Plan::new(0, &settings(false, false, true), 1, None);
        assert!(p.frame_at(3.0).is_none());
        let p = Plan::new(1, &settings(false, false, false), 1, None);
        assert!(p.frame_at(f64::NAN).is_some());
        assert!(p.frame_at(-5.0).is_some());
        assert!(p.frame_at(f64::INFINITY).is_some());
    }

    #[test]
    fn fits_slides_to_music() {
        let mut s = settings(false, false, false);
        s.music.enabled = true;
        s.music.fit_to_music = true;
        let p = Plan::new(4, &s, 1, Some(63.0));
        assert!((p.duration() - 63.0).abs() < 1e-9);
        assert!((p.hold - 15.0).abs() < 1e-9);
    }

    #[test]
    fn random_order_is_a_permutation() {
        let o = order(50, true, 7);
        let mut sorted = o.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        assert_ne!(o, sorted);
        assert_eq!(order(50, true, 7), o, "deterministic for a seed");
    }

    #[test]
    fn pan_zoom_stays_inside_the_photo() {
        for key in 0..20 {
            for p in [0.0, 0.3, 1.0] {
                let (z, cx, cy) = pan_zoom(key, p, 1.0);
                assert!((1.0..=1.3001).contains(&z));
                let half = 0.5 / z;
                assert!(cx - half >= -1e-5 && cx + half <= 1.0 + 1e-5);
                assert!(cy - half >= -1e-5 && cy + half <= 1.0 + 1e-5);
            }
        }
        assert_eq!(pan_zoom(0, 0.5, 0.0), (1.0, 0.5, 0.5));
    }
}
