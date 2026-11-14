//! A drag that ends where the hand rested, not where the lifting finger slid to.
//!
//! On a trackpad, a value is set finely by rocking the finger while the click is held. Lifting the
//! finger lets it roll back, and its contact point slides for a few frames before the click ends:
//! that motion arrives as ordinary drag motion, and since the whole drag is one undo step, Undo
//! cannot take the jerk back on its own (#4228). [`ReleaseSettle`] keeps the last frames of a drag
//! and, at the release, names the value from the rest before such a jerk.
//!
//! It knows nothing of the direction of the jerk. In the recordings the finger always sprang back
//! upwards, against a drag down, but another hand, finger or trackpad can roll the other way.

use std::collections::VecDeque;

/// How far (screen points) the pointer may wander while it rests: a resting finger trembles by
/// 0.1–0.3 pt, and the start of a jerk creeps at 0.2–0.5 pt a frame, so a tighter bound ends the
/// rest inside the jerk.
pub(crate) const REST_DISTANCE: f32 = 2.0;
/// How long (seconds) the pointer must stay within [`REST_DISTANCE`] to count as resting. The rests
/// before a release that were measured lasted 0.1–1.6 s.
pub(crate) const REST_SECONDS: f64 = 0.10;
/// The longest (seconds) a jerk runs from the end of the rest to the release; the longest measured
/// took 134 ms.
pub(crate) const JERK_SECONDS: f64 = 0.15;
/// The farthest (screen points) a jerk reaches. The measured jerks went up to 45 pt, while quick
/// drags after a pause started at 183 pt: motion beyond this was meant.
pub(crate) const JERK_DISTANCE: f32 = 60.0;
/// Frames kept at most, whatever their timing. The half second of history looked at is ~60 frames
/// at 120 fps, so this bounds a drag's memory on any display without cutting into it.
const MAX_SAMPLES: usize = 512;

/// One frame of a drag: when it ran, where the pointer was, and the value it produced.
#[derive(Clone, Copy, Debug)]
struct Sample<V> {
    t: f64,
    pos: [f32; 2],
    value: V,
}

/// The recent history of one drag. Start a new one (`default()`) for every drag.
#[derive(Clone, Debug)]
pub(crate) struct ReleaseSettle<V: Copy> {
    samples: VecDeque<Sample<V>>,
    /// Where the drag's first frame had the pointer.
    origin: Option<[f32; 2]>,
    /// When the pointer first got farther than [`REST_DISTANCE`] from `origin`: a rest only counts
    /// after the drag has moved, so pressing, holding still and then pulling away is never undone.
    moved_at: Option<f64>,
}

impl<V: Copy> Default for ReleaseSettle<V> {
    fn default() -> Self {
        Self { samples: VecDeque::new(), origin: None, moved_at: None }
    }
}

impl<V: Copy> ReleaseSettle<V> {
    /// One frame of the drag: the time (seconds), the pointer (screen points) and the value the
    /// frame set. A frame with a time or position that is not finite, or older than the last one,
    /// is left out.
    pub(crate) fn push(&mut self, t: f64, pos: [f32; 2], value: V) {
        if !t.is_finite() || !pos.iter().all(|c| c.is_finite()) || self.samples.back().is_some_and(|last| t < last.t) {
            return;
        }
        let origin = *self.origin.get_or_insert(pos);
        if self.moved_at.is_none() && distance(origin, pos) > REST_DISTANCE {
            self.moved_at = Some(t);
        }
        self.samples.push_back(Sample { t, pos, value });
        // a rest and the jerk after it, with room for the rest to have started earlier
        let horizon = t - (REST_SECONDS + JERK_SECONDS) * 2.0;
        while self.samples.len() > MAX_SAMPLES || self.samples.front().is_some_and(|s| s.t < horizon) {
            self.samples.pop_front();
        }
    }

    /// The value to commit when the drag is released at time `t` instead of the last one pushed,
    /// or `None` to keep the last one: `Some` only when the pointer had rested after moving and
    /// then jerked briefly and not far (no frame since the rest farther than [`JERK_DISTANCE`]) before
    /// the release.
    pub(crate) fn on_release(&self, t: f64) -> Option<V> {
        let last = self.samples.back()?;
        let moved_at = self.moved_at?;
        if !t.is_finite() {
            return None;
        }
        // the latest frame that ends a rest, searched back from the frame before the last
        let (i, end, rest_start) = (0..self.samples.len().saturating_sub(1)).rev().find_map(|i| {
            let end = self.samples.get(i)?;
            let first = (0..=i).rev().map_while(|j| self.samples.get(j)).take_while(|s| distance(s.pos, end.pos) <= REST_DISTANCE).last()?;
            (end.t - first.t >= REST_SECONDS).then_some((i, end, first.t))
        })?;
        // the farthest the pointer got from the rest, not only where it ended: a pull out and back
        // is the hand's
        let reach = self.samples.iter().skip(i + 1).map(|s| distance(end.pos, s.pos)).fold(0.0_f32, f32::max);
        // a rest may begin on the frame that moved the pointer there
        (moved_at <= rest_start && t - end.t <= JERK_SECONDS && distance(end.pos, last.pos) > 0.0 && reach <= JERK_DISTANCE).then_some(end.value)
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame interval of the drags below: 120 fps, as the recordings ran.
    const DT: f64 = 1.0 / 120.0;

    /// A drag that follows `path` one frame each, from `t0`, with the pointer's y as its value.
    /// Returns the drag and the time of the frame after the last.
    fn drag(path: &[[f32; 2]], t0: f64) -> (ReleaseSettle<f32>, f64) {
        let mut s = ReleaseSettle::default();
        let mut t = t0;
        for &p in path {
            s.push(t, p, p[1]);
            t += DT;
        }
        (s, t)
    }

    /// `frames` frames from `from` to `to` in equal steps (excluding `from`).
    fn line(from: [f32; 2], to: [f32; 2], frames: usize) -> Vec<[f32; 2]> {
        (1..=frames)
            .map(|k| {
                let f = k as f32 / frames as f32;
                [from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f]
            })
            .collect()
    }

    /// `frames` frames at `at`, trembling by a few tenths of a point.
    fn rest(at: [f32; 2], frames: usize) -> Vec<[f32; 2]> {
        (0..frames).map(|k| [at[0] + 0.2 * (k % 2) as f32, at[1] - 0.1 * (k % 3) as f32]).collect()
    }

    fn frames_for(seconds: f64) -> usize {
        (seconds / DT).ceil() as usize + 1
    }

    /// Press at the origin and drag 30 pt down to `rested`, as a rocking finger does.
    fn rocked_down() -> (Vec<[f32; 2]>, [f32; 2]) {
        let rested = [0.0, 30.0];
        let mut path = vec![[0.0, 0.0]];
        path.extend(line([0.0, 0.0], rested, 20));
        (path, rested)
    }

    #[test]
    fn a_short_jerk_after_a_rest_gives_back_the_value_from_the_rest() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        let resting_value = path.last().map(|p| p[1]);
        // the finger springs back up 17 pt over 5 frames before the click ends
        path.extend(line(*path.last().unwrap(), [rested[0] - 10.0, rested[1] - 17.0], 5));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), resting_value);
    }

    #[test]
    fn a_drag_that_moves_up_to_the_release_is_kept() {
        let mut path = vec![[0.0, 0.0]];
        path.extend(line([0.0, 0.0], [0.0, 120.0], 40));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn motion_after_the_rest_that_runs_longer_than_a_jerk_is_kept() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        // 20 pt, slowly enough to take twice the jerk window
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] - 20.0], frames_for(JERK_SECONDS * 2.0)));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn a_pause_shorter_than_a_rest_is_not_one() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS / 2.0)));
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] - 15.0], 4));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn a_rest_up_to_the_release_changes_nothing() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        path.push(*path.last().unwrap());
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn pressing_holding_still_and_pulling_away_is_kept() {
        // nothing moved before the rest: the rest is the press itself
        let mut path = rest([0.0, 0.0], frames_for(REST_SECONDS * 2.0));
        path.extend(line(*path.last().unwrap(), [0.0, 40.0], 5));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn a_quick_pull_after_a_rest_that_goes_farther_than_a_jerk_is_kept() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] - (JERK_DISTANCE + 120.0)], 8));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn a_pull_out_and_back_that_reaches_farther_than_a_jerk_is_kept() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        // out 110 pt and back to 40 pt from the rest, all within the jerk window
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] + 110.0], 4));
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] + 40.0], 4));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), None);
    }

    #[test]
    fn a_rest_that_begins_on_the_frame_that_moved_there_counts() {
        // one frame takes the pointer from the press 30 pt down, and it rests there
        let mut path = vec![[0.0, 0.0]];
        path.extend(rest([0.0, 30.0], frames_for(REST_SECONDS * 2.0)));
        let resting_value = path.last().map(|p| p[1]);
        path.extend(line(*path.last().unwrap(), [0.0, 12.0], 4));
        let (s, up) = drag(&path, 1.0);
        assert_eq!(s.on_release(up), resting_value);
    }

    #[test]
    fn a_release_long_after_the_rest_is_kept() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] - 10.0], 3));
        let (s, last) = drag(&path, 1.0);
        // no frame ran between the jerk and a release that came well after it
        assert_eq!(s.on_release(last + JERK_SECONDS * 2.0), None);
    }

    #[test]
    fn frames_without_finite_numbers_or_running_back_in_time_are_left_out() {
        let (mut path, rested) = rocked_down();
        path.extend(rest(rested, frames_for(REST_SECONDS * 2.0)));
        let resting_value = path.last().map(|p| p[1]);
        path.extend(line(*path.last().unwrap(), [rested[0], rested[1] - 12.0], 4));
        let (mut s, up) = drag(&path, 1.0);
        s.push(f64::NAN, [0.0, 0.0], 0.0);
        s.push(up, [f32::INFINITY, 0.0], 0.0);
        s.push(up, [0.0, f32::NAN], 0.0);
        s.push(0.5, [500.0, 500.0], 500.0);
        assert_eq!(s.on_release(up), resting_value);
        assert_eq!(s.on_release(f64::NAN), None);
        assert_eq!(ReleaseSettle::<f32>::default().on_release(up), None);
    }

    #[test]
    fn a_long_drag_keeps_only_a_bounded_history() {
        let mut s = ReleaseSettle::default();
        for k in 0..100_000 {
            s.push(k as f64 * DT, [(k % 7) as f32, (k % 11) as f32], k);
        }
        assert!(s.samples.len() <= MAX_SAMPLES);
        assert!(s.samples.len() < 100, "only the last fraction of a second is kept: {}", s.samples.len());
        // frames all at one instant are bounded by count
        let mut s = ReleaseSettle::default();
        for k in 0..100_000 {
            s.push(1.0, [0.0, 0.0], k);
        }
        assert_eq!(s.samples.len(), MAX_SAMPLES);
    }
}
