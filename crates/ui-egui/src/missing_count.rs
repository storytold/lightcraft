//! The sidebar's Missing Photos count: the library photos whose file the background checks
//! ([`lightcraft_engine::availability::Availability`]) found gone.
//!
//! The photos are kept as a set of ids and followed edit by edit: a photo that came in, moved or
//! was deleted is looked up in the files found gone, nothing else is touched. A pass over every
//! photo is made only when a file is found gone or back, or when the edits can't be followed
//! (a library just opened, thousands of photos at once), on a worker thread, at most every
//! [`RECOUNT_EVERY`] seconds, while the count shown stays. A frame takes a copy of the catalog for
//! it (the photos are shared: a few milliseconds at 324,000), never a pass over its photos, and an
//! import's batch costs a lookup per photo it brings.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::{Aspects, Catalog, PhotoId};
use lightcraft_engine::availability::Availability;

/// The fewest seconds between two passes over every photo.
const RECOUNT_EVERY: f64 = 0.25;

/// The most photos changed at once that are followed one by one; past it, a pass over every photo
/// (and a check of every file) covers them.
pub const MAX_FOLLOWED: usize = 8_192;

/// What a photo's file being found gone depends on: whether it is in the library, and its file.
const READS: Aspects = Aspects::PRESENCE.with(Aspects::FILE);

/// A pass over every photo, on a worker thread.
struct Recount {
    library: u64,
    revision: u64,
    gone: u64,
    done: Arc<Mutex<Option<HashSet<PhotoId>>>>,
}

#[derive(Default)]
pub struct MissingCount {
    /// The library and catalog revision `ids` are right for (before the edits since are followed).
    at: Option<(u64, u64)>,
    /// The generation of the files found gone `ids` are right for.
    gone: u64,
    ids: HashSet<PhotoId>,
    recount: Option<Recount>,
    /// When the last pass ended or was tried (egui time).
    last: Option<f64>,
    /// A pass is due but waits for [`RECOUNT_EVERY`].
    waiting: bool,
    /// Passes over every photo started.
    passes: usize,
}

impl MissingCount {
    /// Passes over every photo started so far (tests).
    pub fn passes(&self) -> usize {
        self.passes
    }

    /// A pass over every photo is under way or due soon (tests wait for it).
    pub fn busy(&self) -> bool {
        self.recount.is_some() || self.waiting
    }

    /// A pass over every photo is due soon: the caller asks for a frame then.
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// The count as of `cat`; and whether changes to the library couldn't be followed photo by
    /// photo (the files of the photos that changed then need a check of every file).
    pub fn update(&mut self, cat: &Catalog, library: u64, files: &Availability, now: f64) -> (usize, bool) {
        self.waiting = false;
        if self.at.is_some_and(|(l, _)| l != library) {
            *self = MissingCount { passes: self.passes, ..Default::default() };
        }
        let mut lost = self.adopt(library, cat, now);
        if let Some((_, rev)) = self.at.filter(|(_, rev)| *rev != cat.revision) {
            match cat.photos_changed_since(rev, READS) {
                Some(changed) if changed.len() <= MAX_FOLLOWED => {
                    for id in changed {
                        let file = cat.photo(id).and_then(|p| p.library_file());
                        // (the file of a photo that came in or moved is asked about at once)
                        let gone = file.is_some_and(|f| {
                            let _ = files.exists(f);
                            files.is_gone(f)
                        });
                        if gone {
                            self.ids.insert(id);
                        } else {
                            self.ids.remove(&id);
                        }
                    }
                    self.at = Some((library, cat.revision));
                }
                _ => {
                    lost = true;
                    self.at = None;
                }
            }
        }
        let gone = files.gone_generation();
        let due = self.at.is_none() || self.gone != gone;
        if due && self.recount.is_none() {
            if files.gone_len() == 0 {
                // nothing is gone: no photo is missing (and no pass to find it out)
                self.ids.clear();
                (self.at, self.gone) = (Some((library, cat.revision)), gone);
            } else if self.last.is_some_and(|last| now - last < RECOUNT_EVERY && now >= last) {
                self.waiting = true;
            } else {
                self.spawn(cat, library, files, gone, now);
            }
        }
        (self.ids.len(), lost)
    }

    /// The result of a pass that finished, if it is of this library: followed up to `cat`. `true` when
    /// the changes since the pass began can't be followed photo by photo (those photos' files need a check).
    fn adopt(&mut self, library: u64, cat: &Catalog, now: f64) -> bool {
        let Some(r) = &self.recount else { return false };
        let Some(ids) = r.done.lock().unwrap_or_else(PoisonError::into_inner).take() else { return false };
        let (of, revision, gone) = (r.library, r.revision, r.gone);
        self.recount = None;
        self.last = Some(now);
        // (a pass made at the catalog's own revision needs nothing followed: a library just loaded has no history to follow from)
        if of == library && (revision == cat.revision || cat.photos_changed_since(revision, READS).is_some()) {
            (self.ids, self.at, self.gone) = (ids, Some((library, revision)), gone);
            false
        } else {
            // (made for a catalog that can't be followed from: another pass)
            self.at = None;
            of == library
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(&mut self, cat: &Catalog, library: u64, files: &Availability, gone: u64, now: f64) {
        let done = Arc::new(Mutex::new(None));
        let (copy, files, out) = (cat.clone(), files.clone(), done.clone());
        let started = std::thread::Builder::new().name("lc-missing-count".into()).spawn(move || {
            let ids: HashSet<PhotoId> = copy.photos().filter(|p| p.library_file().is_some_and(|f| files.is_gone(f))).map(|p| p.id).collect();
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(ids);
            // (stored before the frame is asked for, so the frame it wakes finds it)
            files.wake();
        });
        match started {
            Ok(_) => {
                self.passes += 1;
                self.recount = Some(Recount { library, revision: cat.revision, gone, done });
            }
            Err(e) => {
                // (backs off: a copy of the catalog is not made every frame for a thread that can't be had)
                self.last = Some(now);
                log::warn!("missing photos: no thread for the count ({e})");
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(&mut self, _cat: &Catalog, _library: u64, _files: &Availability, _gone: u64, _now: f64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo, Source};
    use std::time::{Duration, Instant};

    fn catalog(n: usize) -> Catalog {
        let mut c = Catalog::new();
        let ops: Vec<Op> = (0..n)
            .map(|i| {
                let id = c.alloc_photo_id();
                let name = format!("{}_{i}.jpg", if i < 2 { "gone" } else { "IMG" });
                Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: format!("/mc/{name}") }, &name, "JPEG", 6, 4, "2026-10-21")) }
            })
            .collect();
        c.apply(Op::Batch { ops }).unwrap();
        c
    }

    fn files(gone: &[&str]) -> Availability {
        let mut a = Availability::default();
        a.set_probe(Arc::new(|p: &str| !p.contains("gone")));
        a.run_in_background(Arc::new(|| {}));
        assert!(a.sweep(gone.iter().map(|g| format!("/mc/{g}")).collect()));
        let t0 = Instant::now();
        while (a.sweeping() > 0 || a.pending() > 0 || a.gone_len() < gone.iter().filter(|g| g.starts_with("gone")).count())
            && t0.elapsed() < Duration::from_secs(20)
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        a
    }

    /// Updates at `at` until the count is `n` (the pass runs on a worker thread).
    fn until(m: &mut MissingCount, cat: &Catalog, a: &Availability, at: f64, n: usize) {
        let t0 = Instant::now();
        while m.update(cat, 1, a, at).0 != n && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(m.update(cat, 1, a, at).0, n);
    }

    #[test]
    fn a_pass_over_every_photo_is_made_only_when_a_file_is_found_gone_or_back() {
        let cat = catalog(1_000);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        until(&mut m, &cat, &a, 0.0, 1);
        // nothing changes for ten seconds of frames: the count is kept, nothing is passed over
        for t in 1..=10 {
            assert_eq!(m.update(&cat, 1, &a, f64::from(t)).0, 1);
        }
        assert_eq!(m.passes(), 1);
        // another file is found gone: one more pass
        assert!(a.sweep(vec!["/mc/gone_1.jpg".into()]));
        let t0 = Instant::now();
        while a.gone_len() < 2 && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
        until(&mut m, &cat, &a, 11.0, 2);
        assert_eq!(m.passes(), 2);
    }

    #[test]
    fn passes_wait_a_quarter_of_a_second_between_them() {
        let cat = catalog(100);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        until(&mut m, &cat, &a, 0.0, 1);
        assert!(a.sweep(vec!["/mc/gone_1.jpg".into()]));
        let t0 = Instant::now();
        while a.gone_len() < 2 && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(m.update(&cat, 1, &a, 0.1).0, 1, "too soon after the last pass");
        assert!(m.waiting());
        assert_eq!(m.passes(), 1);
    }

    /// A pass that finishes after the catalog moved on in a way that can't be followed photo by photo is not
    /// adopted, and says so: the photos that changed meanwhile need a check of their files.
    #[test]
    fn a_pass_that_finishes_after_unfollowable_changes_reports_them_lost() {
        let cat = catalog(100);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        m.update(&cat, 1, &a, 0.0);
        let t0 = Instant::now();
        while !m.recount.as_ref().is_some_and(|r| r.done.lock().unwrap().is_some()) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
        // (the catalog was set from outside: its history doesn't reach back to the pass's revision)
        let (_, lost) = m.update(&Catalog::new(), 1, &a, 1.0);
        assert!(lost, "the pass's photos can't be followed up to the catalog");
    }

    /// A library that was just opened (its catalog has no history yet) with a file gone takes one pass over its
    /// photos, not one every quarter of a second.
    #[test]
    fn a_just_loaded_catalog_takes_one_pass() {
        let mut cat = catalog(100);
        // (set from outside, as a library being opened is: no history to follow from)
        cat.revision = 7 << 32;
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        until(&mut m, &cat, &a, 0.0, 1);
        for t in 1..=8 {
            assert_eq!(m.update(&cat, 1, &a, f64::from(t)).0, 1);
            assert!(!m.busy() && !m.waiting(), "at {t} s");
        }
        assert_eq!(m.passes(), 1);
    }
}
