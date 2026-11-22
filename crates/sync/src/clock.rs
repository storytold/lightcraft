//! A hybrid logical clock: wall time that never runs backwards and never repeats, so two changes
//! can always be put in order, on any number of devices, without a server.

use serde::{Deserialize, Serialize};

/// How far apart two devices' wall clocks may be for them to sync (milliseconds). Past that the
/// later write would not be the later one, so the exchange is refused with a message.
pub const MAX_SKEW_MS: u64 = 5 * 60 * 1000;

/// When a change was made: wall time (ms since 1970), a counter for changes within one
/// millisecond, and a tag of the device (the last tie-break).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stamp {
    pub t: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub c: u32,
    #[serde(default)]
    pub d: u32,
}

fn is_zero(c: &u32) -> bool {
    *c == 0
}

impl Stamp {
    /// Before every real change.
    pub const ZERO: Stamp = Stamp { t: 0, c: 0, d: 0 };

    /// Whether this stamp is more than [`MAX_SKEW_MS`] later than `now_ms`: no device whose
    /// clock is within the allowed skew could have made it, so it is refused.
    pub fn ahead_of(&self, now_ms: u64) -> bool {
        self.t > now_ms.saturating_add(MAX_SKEW_MS)
    }
}

/// A short tag of a device id, for [`Stamp::d`].
pub fn device_tag(device_id: &str) -> u32 {
    let h = crate::fnv64(device_id.as_bytes());
    (h ^ (h >> 32)) as u32
}

/// This device's clock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    last: Stamp,
}

impl Clock {
    /// The stamp for a change made now: later than every stamp made or seen before.
    pub fn tick(&mut self, now_ms: u64, device_id: &str) -> Stamp {
        let t = now_ms.max(self.last.t);
        let c = if t == self.last.t { self.last.c.saturating_add(1) } else { 0 };
        self.last = Stamp { t, c, d: device_tag(device_id) };
        self.last
    }

    /// A stamp another device made was seen: this clock's next stamp is later than it.
    pub fn observe(&mut self, s: Stamp) {
        if s > self.last {
            self.last = s;
        }
    }

    /// [`Clock::observe`], unless `s` is more than [`MAX_SKEW_MS`] past `now_ms`: a stamp from
    /// the far future would make every later change here lose to it. Says whether it was taken.
    pub fn observe_at(&mut self, s: Stamp, now_ms: u64) -> bool {
        if s.ahead_of(now_ms) {
            return false;
        }
        self.observe(s);
        true
    }

    pub fn last(&self) -> Stamp {
        self.last
    }
}

/// A value with the time it was last written; of two writes the later one is kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lww<T> {
    pub v: T,
    pub s: Stamp,
}

impl<T: Serialize + PartialEq + Clone> Lww<T> {
    pub fn new(v: T, s: Stamp) -> Lww<T> {
        Lww { v, s }
    }

    /// Whether `other` replaces this value: it is later, or as late and different with the
    /// greater text (so two devices that tie settle on the same one).
    pub fn loses_to(&self, other: &Lww<T>) -> bool {
        match other.s.cmp(&self.s) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => other.v != self.v && text(&other.v) > text(&self.v),
        }
    }

    /// Takes `other` if it wins; says whether it did.
    pub fn merge(&mut self, other: &Lww<T>) -> bool {
        let take = self.loses_to(other);
        if take {
            *self = other.clone();
        }
        take
    }
}

fn text<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_never_repeat_or_run_backwards() {
        let mut c = Clock::default();
        let a = c.tick(1000, "a");
        let b = c.tick(1000, "a");
        let back = c.tick(500, "a");
        assert!(a < b && b < back, "{a:?} {b:?} {back:?}");
        c.observe(Stamp { t: 9000, c: 3, d: 1 });
        assert!(c.tick(2000, "a") > Stamp { t: 9000, c: 3, d: 1 });
    }

    #[test]
    fn far_future_stamps_are_not_observed() {
        let now = 1_000_000_000;
        let mut c = Clock::default();
        let day = Stamp { t: now + 24 * 3600 * 1000, c: 0, d: 7 };
        assert!(!c.observe_at(day, now));
        assert!(c.tick(now, "a") < day);
        assert!(c.observe_at(Stamp { t: now + MAX_SKEW_MS, c: 0, d: 7 }, now));
        // the end of time neither overflows nor panics
        let end = Stamp { t: u64::MAX, c: u32::MAX, d: u32::MAX };
        assert!(!c.observe_at(end, now));
        assert!(!end.ahead_of(u64::MAX));
        c.observe(end);
        let _ = c.tick(u64::MAX, "a");
    }

    #[test]
    fn the_later_write_wins_whatever_the_order_of_merging() {
        let old = Lww::new("old".to_string(), Stamp { t: 1, c: 0, d: 1 });
        let new = Lww::new("new".to_string(), Stamp { t: 2, c: 0, d: 2 });
        let (mut x, mut y) = (old.clone(), new.clone());
        assert!(x.merge(&new));
        assert!(!y.merge(&old));
        assert_eq!(x, y);
        // a tie settles the same way on both sides
        let (p, q) = (Lww::new(1, Stamp::ZERO), Lww::new(2, Stamp::ZERO));
        let (mut a, mut b) = (p.clone(), q.clone());
        a.merge(&q);
        b.merge(&p);
        assert_eq!(a, b);
    }
}
