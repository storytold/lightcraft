//! The photo counts `albums.list` answers with.
//!
//! Counting a smart album asks every photo whether its rules hold: seconds in a large library, and a window that does
//! it on its UI thread stops answering for that long. So a window offers the counts it keeps
//! ([`Session::offer_album_counts`]; they are counted on a worker thread) and `albums.list` counts nothing itself.
//!
//! With no window (the CLI, the MCP server, a script) there is no UI thread to stop, and the caller wants the numbers:
//! they are counted here, once, and kept. An edit to a photo or a few is followed photo by photo, as the sidebar's
//! are ([`AlbumTally::update`]); anything more counts every photo again.

use std::collections::HashMap;
use std::sync::Arc;

use lightcraft_catalog::tally::AlbumTally;
use lightcraft_catalog::{AlbumId, Catalog};

/// Every album's count, as `albums.list` takes them.
pub(crate) type Counts = Arc<HashMap<AlbumId, usize>>;

/// How far the counts a window keeps are from the catalog and the session clock as they are now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountsAge {
    /// Of the catalog as it is now, and of this minute.
    Current,
    /// Of the catalog as it is now, but of an earlier minute: only a smart album whose rules ask about "the last N
    /// days" (its own, or those of an album it tests) can have changed since.
    ClockBehind,
    /// Of an earlier catalog, or none yet: every smart album's count is behind.
    Behind,
}

/// Counts, and how current they are.
pub(crate) struct Answer {
    pub counts: Counts,
    pub age: CountsAge,
}

/// The counts a session keeps for `albums.list`.
#[derive(Default)]
pub(crate) struct AlbumCounts {
    /// What a window offered for the next `albums.list`.
    offered: Option<Answer>,
    /// Counted here, when no window offers any.
    kept: Option<Kept>,
    /// How often every photo was counted here (tests check that a repeated list, and an edit to a photo or a few,
    /// don't).
    pub scans: usize,
}

/// The counts made here.
struct Kept {
    /// The library they are of ([`Session::library_generation`]): album ids start again in every library.
    library: u64,
    /// The minute they are of while a smart album asks about "the last N days" (else empty).
    minute: String,
    tally: AlbumTally,
    shown: Counts,
}

impl AlbumCounts {
    /// `albums.list` takes `counts` next, `age` being how far they are from the catalog and the clock as they are now.
    pub fn offer(&mut self, counts: Counts, age: CountsAge) {
        self.offered = Some(Answer { counts, age });
    }

    /// Nothing is offered any more.
    pub fn withdraw(&mut self) {
        self.offered = None;
    }

    /// The counts for `albums.list`: the ones offered, else counted here (`now` is the session clock).
    pub fn answer(&mut self, cat: &Catalog, library: u64, now: &str) -> Answer {
        if let Some(offered) = self.offered.take() {
            return offered;
        }
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { crate::clock_minute(now) } else { "" };
        if relative {
            // "in the last N days" counts back from the session clock, as in the grid
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let followed = self.kept.take().filter(|k| k.library == library && k.minute == minute).and_then(|mut k| {
            if k.tally.revision() == cat.revision {
                return Some(k);
            }
            k.tally.update(cat).then(|| {
                k.shown = Arc::new(k.tally.counts().clone());
                k
            })
        });
        let kept = followed.unwrap_or_else(|| {
            self.scans += 1;
            let tally = AlbumTally::of(cat);
            let shown = Arc::new(tally.counts().clone());
            Kept { library, minute: minute.to_string(), tally, shown }
        });
        let counts = kept.shown.clone();
        self.kept = Some(kept);
        Answer { counts, age: CountsAge::Current }
    }
}
