//! Random ids for photos, albums and stacks (issue #294).
//!
//! Counter ids (`next + 1`) collide as soon as two machines add to copies of one catalog: both
//! hand out the same next number. Random ids don't need coordination. They are drawn from
//! `[1, 2^53)` so they survive JSON clients that read numbers as doubles (JavaScript), and are
//! re-drawn when the catalog already uses one. Ids are never rewritten: libraries with counter
//! ids keep them.

/// Ids are below this (2^53: integers a double represents exactly).
pub const MAX_ID: u64 = 1 << 53;

/// A splitmix64 sequence. [`Default`] seeds it from the OS's randomness and the clock; tests seed it
/// with [`IdGen::seeded`]. Not part of the catalog's data: never serialized, and ignored by
/// equality, so two catalogs with the same content compare equal whatever ids they'd draw next.
#[derive(Clone, Debug)]
pub struct IdGen {
    state: u64,
}

impl IdGen {
    pub fn seeded(seed: u64) -> IdGen {
        IdGen { state: seed }
    }

    /// The next id in `[1, MAX_ID)` that `taken` doesn't reject. `taken` must leave some id free:
    /// this loops until it finds one.
    pub fn draw(&mut self, taken: impl Fn(u64) -> bool) -> u64 {
        loop {
            let id = self.next() >> 11; // top 53 bits: [0, 2^53)
            if id != 0 && !taken(id) {
                return id;
            }
        }
    }

    /// splitmix64 (Steele, Lea & Flood, "Fast splittable pseudorandom number generators", 2014).
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl Default for IdGen {
    fn default() -> IdGen {
        use std::hash::{BuildHasher, Hasher};
        // `RandomState` keys come from the OS's randomness (once per thread, then varied per
        // instance), the clock covers platforms where they don't.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        h.write_u128(now);
        IdGen { state: h.finish() }
    }
}

impl PartialEq for IdGen {
    fn eq(&self, _: &IdGen) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_in_range_and_nonzero() {
        let mut g = IdGen::seeded(7);
        for _ in 0..10_000 {
            let id = g.draw(|_| false);
            assert!((1..MAX_ID).contains(&id), "{id}");
        }
    }

    #[test]
    fn same_seed_same_sequence() {
        let (mut a, mut b) = (IdGen::seeded(42), IdGen::seeded(42));
        for _ in 0..100 {
            assert_eq!(a.draw(|_| false), b.draw(|_| false));
        }
    }

    #[test]
    fn taken_ids_are_redrawn() {
        let first = IdGen::seeded(1).draw(|_| false);
        let mut g = IdGen::seeded(1);
        let id = g.draw(|v| v == first);
        assert_ne!(id, first);
        assert!((1..MAX_ID).contains(&id));
    }

    #[test]
    fn default_generators_differ() {
        // Two catalogs opened at the same moment must not draw the same sequence.
        let (mut a, mut b) = (IdGen::default(), IdGen::default());
        let xs: Vec<u64> = (0..4).map(|_| a.draw(|_| false)).collect();
        let ys: Vec<u64> = (0..4).map(|_| b.draw(|_| false)).collect();
        assert_ne!(xs, ys);
    }

    #[test]
    fn equality_ignores_state() {
        assert_eq!(IdGen::seeded(1), IdGen::seeded(2));
    }
}
