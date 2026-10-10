//! How new photo, album and stack ids are chosen.
//!
//! By default ids are counters (`next + 1`, from 1), exactly as in older builds: the demo library,
//! `docs/showcase` and the documented `--demo` workflows refer to ids 1, 2, ... and a catalog opened
//! by an older build keeps working. Counter ids collide as soon as two machines add to copies of one
//! catalog (issue #294), so a catalog meant to be shared can opt in to random ids
//! ([`Catalog::use_random_ids`](crate::Catalog::use_random_ids)). Those need no coordination: they
//! are drawn from `[1, 2^53)` so they survive JSON clients that read numbers as doubles
//! (JavaScript), and re-drawn when the catalog already uses one. Ids are never rewritten: a library
//! keeps the ids it has, whatever mode it allocates in.

/// Ids are below this (2^53: integers a double represents exactly).
pub const MAX_ID: u64 = 1 << 53;

/// Where new ids come from: the catalog's counters ([`IdGen::Sequential`], the default) or a
/// splitmix64 sequence ([`IdGen::Random`]), which [`IdGen::random`] seeds from the OS's randomness
/// and the clock and tests seed with [`IdGen::seeded`]. Not part of the catalog's data: never
/// serialized, and ignored by equality, so two catalogs with the same content compare equal whatever
/// ids they'd allocate next.
#[derive(Clone, Debug, Default)]
pub enum IdGen {
    #[default]
    Sequential,
    Random {
        state: u64,
    },
}

impl IdGen {
    pub fn seeded(seed: u64) -> IdGen {
        IdGen::Random { state: seed }
    }

    /// Random ids from an unpredictable seed.
    pub fn random() -> IdGen {
        use std::hash::{BuildHasher, Hasher};
        // `RandomState` keys come from the OS's randomness (once per thread, then varied per
        // instance), the clock covers platforms where they don't.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        h.write_u128(now);
        IdGen::Random { state: h.finish() }
    }

    pub fn is_random(&self) -> bool {
        matches!(self, IdGen::Random { .. })
    }

    /// The next id in `[1, MAX_ID)` that `taken` doesn't reject, or `None` when sequential (the
    /// caller then uses its counter). `taken` must leave some id free: this loops until it finds one.
    ///
    /// A clone draws the same ids as the original: re-seed it first if it will allocate independently.
    pub fn draw(&mut self, taken: impl Fn(u64) -> bool) -> Option<u64> {
        let IdGen::Random { state } = self else { return None };
        loop {
            let id = Self::next(state) >> 11; // top 53 bits: [0, 2^53)
            if id != 0 && !taken(id) {
                return Some(id);
            }
        }
    }

    /// splitmix64 (Steele, Lea & Flood, "Fast splittable pseudorandom number generators", 2014).
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
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
            let id = g.draw(|_| false).unwrap();
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
        let first = IdGen::seeded(1).draw(|_| false).unwrap();
        let mut g = IdGen::seeded(1);
        let id = g.draw(|v| v == first).unwrap();
        assert_ne!(id, first);
        assert!((1..MAX_ID).contains(&id));
    }

    #[test]
    fn random_generators_differ() {
        // Two catalogs switched to random ids at the same moment must not draw the same sequence.
        let (mut a, mut b) = (IdGen::random(), IdGen::random());
        let xs: Vec<u64> = (0..4).map(|_| a.draw(|_| false).unwrap()).collect();
        let ys: Vec<u64> = (0..4).map(|_| b.draw(|_| false).unwrap()).collect();
        assert_ne!(xs, ys);
    }

    #[test]
    fn sequential_is_the_default_and_draws_nothing() {
        let mut g = IdGen::default();
        assert!(!g.is_random());
        assert_eq!(g.draw(|_| false), None);
        assert!(IdGen::random().is_random());
    }

    #[test]
    fn equality_ignores_state() {
        assert_eq!(IdGen::seeded(1), IdGen::seeded(2));
        assert_eq!(IdGen::seeded(1), IdGen::Sequential);
    }
}
