//! How many photos each album shows, kept up to date from what the latest ops touched.
//!
//! A smart album's count asks every photo whether its rules hold. Asked again after every edit, that is a pass over
//! the library for each smart album each time a keyword is given. [`AlbumTally`] remembers which photos each smart
//! album holds, and after a small edit asks only the photos the edit touched ([`Catalog::touched_since`]). An op
//! that changes more than photos of its own (an album's rules or photos, a stack, the keyword list) has it counted
//! whole again, as before.

use std::collections::{HashMap, HashSet};

use crate::{AlbumId, Aspects, Catalog, PhotoId};

/// The most photos an edit may touch for the counts to follow it photo by photo.
const MAX_FOLLOWED: usize = 256;
/// Bits are kept while they cost no more than this many words a photo held, and these to start with. Past that
/// (photo ids far apart: a library whose ids don't count up from 1) a set is kept instead, so what an album
/// remembers is always in proportion to the photos it holds, never to the largest id in the library.
const WORDS_A_MEMBER: usize = 4;
const WORDS_TO_START: usize = 1024;

/// The photos a smart album holds.
#[derive(Clone, Debug)]
enum Members {
    /// One bit a photo id, and how many are set.
    Bits {
        words: Vec<u64>,
        len: usize,
    },
    Set(HashSet<PhotoId>),
}

impl Members {
    fn none() -> Members {
        Members::Bits { words: Vec::new(), len: 0 }
    }

    fn len(&self) -> usize {
        match self {
            Members::Bits { len, .. } => *len,
            Members::Set(set) => set.len(),
        }
    }

    #[cfg(test)]
    fn holds(&self, id: PhotoId) -> bool {
        match self {
            Members::Bits { words, .. } => {
                usize::try_from(id.0 / 64).ok().and_then(|at| words.get(at)).is_some_and(|word| word & (1u64 << (id.0 % 64)) != 0)
            }
            Members::Set(set) => set.contains(&id),
        }
    }

    /// Put `id` in or take it out.
    fn set(&mut self, id: PhotoId, held: bool) {
        if let Members::Bits { words, len } = self {
            let bit = 1u64 << (id.0 % 64);
            match usize::try_from(id.0 / 64) {
                Ok(at) if at < words.len() => {
                    if let Some(word) = words.get_mut(at) {
                        match (*word & bit != 0, held) {
                            (false, true) => {
                                *word |= bit;
                                *len = len.saturating_add(1);
                            }
                            (true, false) => {
                                *word &= !bit;
                                *len = len.saturating_sub(1);
                            }
                            _ => {}
                        }
                    }
                    return;
                }
                // (past the end: not held, so there is nothing to take out)
                _ if !held => return,
                // room for it, as long as the bits stay in proportion to the photos held
                Ok(at) if at < len.saturating_mul(WORDS_A_MEMBER).saturating_add(WORDS_TO_START) => {
                    words.resize(at + 1, 0);
                    if let Some(word) = words.get_mut(at) {
                        *word |= bit;
                        *len = len.saturating_add(1);
                    }
                    return;
                }
                // ids too far apart for bits: a set from here on
                _ => {
                    let ids = words
                        .iter()
                        .enumerate()
                        .flat_map(|(at, word)| (0..64u64).filter(move |b| word & (1u64 << b) != 0).map(move |b| PhotoId(at as u64 * 64 + b)));
                    *self = Members::Set(ids.collect());
                }
            }
        }
        if let Members::Set(set) = self {
            if held {
                set.insert(id);
            } else {
                set.remove(&id);
            }
        }
    }
}

/// [`Catalog::album_count`] of every album, as of one revision of the catalog.
#[derive(Clone, Debug)]
pub struct AlbumTally {
    revision: u64,
    counts: HashMap<AlbumId, usize>,
    /// The smart albums' photos.
    members: HashMap<AlbumId, Members>,
}

impl AlbumTally {
    /// Counted from every photo.
    pub fn of(cat: &Catalog) -> AlbumTally {
        let mut counts = HashMap::new();
        let mut members = HashMap::new();
        for album in cat.albums() {
            if album.is_smart() {
                let mut held = Members::none();
                for p in cat.photos().filter(|p| cat.album_contains(album.id, p)) {
                    held.set(p.id, true);
                }
                counts.insert(album.id, held.len());
                members.insert(album.id, held);
            } else {
                counts.insert(album.id, cat.album_count(album.id));
            }
        }
        AlbumTally { revision: cat.revision, counts, members }
    }

    /// The count of each album.
    pub fn counts(&self) -> &HashMap<AlbumId, usize> {
        &self.counts
    }

    /// The catalog's revision the counts are of.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bring the counts up to `cat` as it is now. `true` when that was done from the photos the ops since touched
    /// (or nothing had changed), `false` when every photo was asked again.
    ///
    /// `cat` has to be the catalog the counts were made of, further along. Another catalog is told apart only by
    /// its revisions: a library that is loaded takes a range of its own (and then everything is counted again), but
    /// two catalogs both counting from 0 look like one.
    pub fn update(&mut self, cat: &Catalog) -> bool {
        if self.revision == cat.revision {
            return true;
        }
        let followed = match (cat.touched_since(self.revision), cat.aspects_since(self.revision)) {
            (Some(touched), Some(changed)) if touched.len() <= MAX_FOLLOWED => {
                self.follow(cat, &touched, changed);
                true
            }
            _ => false,
        };
        if !followed {
            *self = AlbumTally::of(cat);
        }
        followed
    }

    /// The counts after ops that touched only `touched`: each smart album asks those photos again; an album of
    /// chosen photos counts again only when a photo came, went or was deleted (who is in it did not change: that
    /// is an op on the album, and not followed).
    fn follow(&mut self, cat: &Catalog, touched: &[PhotoId], changed: Aspects) {
        for (album, held) in &mut self.members {
            for id in touched {
                held.set(*id, cat.photo(*id).is_some_and(|p| cat.album_contains(*album, p)));
            }
            self.counts.insert(*album, held.len());
        }
        if changed.any_of(Aspects::PRESENCE) {
            for album in cat.albums().filter(|a| !a.is_smart()) {
                self.counts.insert(album.id, cat.album_count(album.id));
            }
        }
        self.revision = cat.revision;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(m: &Members, of: &[u64]) -> Vec<u64> {
        of.iter().copied().filter(|id| m.holds(PhotoId(*id))).collect()
    }

    #[test]
    fn members_are_kept_as_bits_at_the_edges_of_a_word_and_past_the_end() {
        let mut m = Members::none();
        let probe = [0, 1, 63, 64, 65, 127, 128, 5000];
        assert!(ids(&m, &probe).is_empty() && m.len() == 0);
        for id in [0, 63, 64, 5000] {
            m.set(PhotoId(id), true);
        }
        assert_eq!((ids(&m, &probe), m.len()), (vec![0, 63, 64, 5000], 4));
        // put in twice, taken out twice, taken out where nothing is, far past the end: counted once each way
        m.set(PhotoId(63), true);
        m.set(PhotoId(64), false);
        m.set(PhotoId(64), false);
        m.set(PhotoId(1), false);
        m.set(PhotoId(900_000), false);
        assert_eq!((ids(&m, &probe), m.len()), (vec![0, 63, 5000], 3));
        assert!(matches!(m, Members::Bits { .. }));
    }

    #[test]
    fn photo_ids_far_apart_are_kept_as_a_set_in_proportion_to_the_photos_held() {
        // one photo with an id in the thousands of millions: no bit for every id up to it
        let mut m = Members::none();
        for id in [3, 70, 9_000_000_000] {
            m.set(PhotoId(id), true);
        }
        assert!(matches!(m, Members::Set(_)), "a set once the ids are too far apart for bits");
        assert_eq!((ids(&m, &[3, 70, 71, 9_000_000_000, u64::MAX]), m.len()), (vec![3, 70, 9_000_000_000], 3));
        m.set(PhotoId(u64::MAX), true);
        m.set(PhotoId(3), false);
        assert_eq!((ids(&m, &[3, 70, 9_000_000_000, u64::MAX]), m.len()), (vec![70, 9_000_000_000, u64::MAX], 3));
        // an album of many photos with ids counting up stays bits, however many
        let mut dense = Members::none();
        for id in 1..=200_000u64 {
            dense.set(PhotoId(id), true);
        }
        assert!(matches!(&dense, Members::Bits { words, .. } if words.len() <= 200_000 / 64 + 1));
        assert_eq!(dense.len(), 200_000);
    }
}
