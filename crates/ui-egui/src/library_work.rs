//! The sidebar's tally of the whole library ([`LibraryTally`]: folders, dates, keywords, the
//! library's counts), kept for the panels.
//!
//! An edit to a few photos (an import's batch, a rating, a keyword) is followed photo by photo on
//! the frame it lands. What can't be followed (thousands of photos at once, a library just opened,
//! a keyword whose first spelling went) is counted whole: off the UI thread past
//! [`COUNT_HERE_UP_TO`] photos, on a copy of the catalog (the photos are shared, so the copy is
//! cheap), while the panels keep showing the tally they had. A frame never counts every photo of
//! a large library.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use lightcraft_catalog::Catalog;
use lightcraft_catalog::library_tally::LibraryTally;

/// Libraries up to this many photos are counted whole on the calling thread: a few milliseconds,
/// less than handing the count to a thread costs. Larger ones are counted on a worker.
pub const COUNT_HERE_UP_TO: usize = 5_000;

/// How a library is counted whole (tests hold it back).
pub type CountWith = Arc<dyn Fn(&Catalog) -> LibraryTally + Send + Sync>;

/// A count of every photo running on a worker thread.
struct Recount {
    /// The library and catalog revision it counts.
    library: u64,
    revision: u64,
    done: Receiver<Result<LibraryTally, String>>,
    /// Set by the worker once it has sent its result.
    finished: Arc<std::sync::atomic::AtomicBool>,
}

/// The tally of the open library (see the module docs).
pub struct LibraryWork {
    /// The tally and the library it is of.
    tally: Option<(u64, LibraryTally)>,
    /// The tally can't follow the catalog from where it is: a count of every photo is due.
    behind: bool,
    recount: Option<Recount>,
    /// The library and revision a count failed at: not tried again until one changes.
    failed: Option<(u64, u64)>,
    count_here_up_to: usize,
    count_with: CountWith,
    /// Asks for a frame when a background count is done.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Counts of every photo made (on this thread, on a worker).
    here: usize,
    background: usize,
}

impl Default for LibraryWork {
    fn default() -> Self {
        LibraryWork {
            tally: None,
            behind: false,
            recount: None,
            failed: None,
            count_here_up_to: COUNT_HERE_UP_TO,
            count_with: Arc::new(LibraryTally::of),
            wake: None,
            here: 0,
            background: 0,
        }
    }
}

impl LibraryWork {
    /// What asks for a frame once a count made in the background is done.
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    /// The tally of library `library` (its [`lightcraft_engine::Session::library_generation`])
    /// as far as it is known: up to `cat` when it could follow, else the one shown before while
    /// the library is counted again. `None` before the library's first count.
    pub fn current(&mut self, cat: &Catalog, library: u64) -> Option<&LibraryTally> {
        self.take_count(library);
        if self.tally.as_ref().is_some_and(|(of, _)| *of != library) {
            // another library: nothing of this one is shown, not even for a frame
            self.tally = None;
        }
        if let Some((_, t)) = self.tally.as_mut()
            && !self.behind
            && t.revision() != cat.revision
            && t.follow(cat).is_none()
        {
            self.behind = true;
        }
        let fresh = self.tally.as_ref().is_some_and(|(_, t)| t.revision() == cat.revision && t.exact());
        if !fresh {
            self.count(cat, library);
        }
        self.tally.as_ref().map(|(_, t)| t)
    }

    /// Whether the tally is of library `library` and counts `cat` as it is now: not while the
    /// photos of a change too large to follow are counted again (the tally then shows an older
    /// state), nor before the first count. A read-only look: it counts nothing.
    pub fn is_current(&self, cat: &Catalog, library: u64) -> bool {
        self.tally.as_ref().is_some_and(|(of, t)| *of == library && t.revision() == cat.revision && t.exact())
    }

    /// Whether a count of every photo is under way (its result is taken by the next
    /// [`LibraryWork::current`]).
    pub fn busy(&self) -> bool {
        self.recount.as_ref().is_some_and(|r| !r.finished.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Counts of every photo made so far: (on the calling thread, on a worker).
    pub fn whole_counts(&self) -> (usize, usize) {
        (self.here, self.background)
    }

    /// Count libraries of more than `n` photos on a worker (tests: any size).
    pub fn count_in_background_past(&mut self, n: usize) {
        self.count_here_up_to = n;
    }

    /// Count every photo with `f` (tests: a count that waits).
    pub fn count_with(&mut self, f: CountWith) {
        self.count_with = f;
    }

    /// A count of every photo that finished, if it is of this library.
    fn take_count(&mut self, library: u64) {
        let Some(r) = &self.recount else { return };
        let got = match r.done.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(got) => got,
            Err(TryRecvError::Disconnected) => Err("the count stopped before it was done".to_string()),
        };
        let (of, revision) = (r.library, r.revision);
        self.recount = None;
        match got {
            // (made for the catalog as it was: what changed since is followed from it)
            Ok(t) if of == library => self.replace(library, t),
            Ok(_) => {}
            Err(e) => {
                log::warn!("library counts: {e}");
                self.failed = Some((of, revision));
            }
        }
    }

    /// Count every photo: here for a small library, else on a worker (one at a time; what changes
    /// meanwhile is followed from its result).
    fn count(&mut self, cat: &Catalog, library: u64) {
        if self.recount.is_some() || self.failed == Some((library, cat.revision)) {
            return;
        }
        if cat.len() > self.count_here_up_to && self.spawn(cat, library) {
            return;
        }
        // (no thread to be had, or a small library: counted here)
        self.here += 1;
        let t = (self.count_with)(cat);
        self.replace(library, t);
    }

    /// `t`, counted from every photo, takes the tally's place.
    fn replace(&mut self, library: u64, t: LibraryTally) {
        self.tally = Some((library, t));
        self.behind = false;
    }

    /// A count of every photo of `cat` on a worker thread; `false` when none could be started.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(&mut self, cat: &Catalog, library: u64) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let (cat, count, wake) = (cat.clone(), self.count_with.clone(), self.wake.clone());
        let revision = cat.revision;
        let started = std::thread::Builder::new().name("lc-library-counts".into()).spawn(move || {
            // last-resort guard: a panic is this count's failure, not the app's
            let r = lightcraft_engine::guard::catch("library counts", || count(&cat));
            drop(cat);
            // (sent before the frame is asked for, so the frame it wakes finds it)
            let _ = tx.send(r);
            done.store(true, std::sync::atomic::Ordering::Release);
            if let Some(w) = wake {
                w();
            }
        });
        match started {
            Ok(_) => {
                self.background += 1;
                self.recount = Some(Recount { library, revision, done: rx, finished });
                true
            }
            Err(e) => {
                log::warn!("library counts: no thread for the count ({e}): counted on this one");
                false
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(&mut self, _cat: &Catalog, _library: u64) -> bool {
        false
    }
}
