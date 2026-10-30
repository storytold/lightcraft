//! The sidebar's Missing Photos count: the library photos whose file the background checks
//! ([`lightcraft_engine::availability::Availability`]) found gone.
//!
//! The photos are kept as a set of ids and followed change by change, never by a pass over the
//! library per change:
//!
//! * a photo that came in, moved or was deleted is looked up in the files found gone;
//! * a file found gone or back ([`Availability::gone_changes_since`]) is mapped to its photos by
//!   a lookup of the path (`paths`: a hash of each photo's file → the photo), which the frame
//!   that sees it does for each such file;
//! * the lookup is built, and the count made from scratch, by walking the photos a step at a time
//!   ([`SCAN_STEP`] photos a frame) when a library is opened, when changes to it couldn't be
//!   followed (thousands of photos at once) or when too many files changed to be told one by one.
//!
//! So what a frame costs does not depend on the size of the library: no copy of the catalog is
//! made (the one that was, on every pass, took a third of the UI thread's time on 427,000 photos
//! while a share was being checked), and no worker thread is needed.

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};

use lightcraft_catalog::{Aspects, Catalog, PhotoId};
use lightcraft_engine::availability::Availability;

/// The most photos changed at once that are followed one by one; past it, a walk over every photo
/// (and a check of every file) covers them.
pub const MAX_FOLLOWED: usize = 8_192;

/// The photos a frame looks at when walking the library: about half a millisecond.
const SCAN_STEP: usize = 1_024;

/// What a photo's file being found gone depends on: whether it is in the library, and its file.
const READS: Aspects = Aspects::PRESENCE.with(Aspects::FILE);

/// Where the walk over the photos, that finds each one's file for `paths`, is.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum Walk {
    /// No file is gone (or the library is new here, or changes couldn't be followed): `paths` is
    /// empty and kept up to date by nothing. The walk starts when a file is gone.
    #[default]
    Idle,
    /// Done up to the photo (`None`: not started).
    Under(Option<PhotoId>),
    /// Every photo is in `paths`; changes keep it so.
    Done,
}

#[derive(Default)]
pub struct MissingCount {
    /// The library and catalog revision `ids` follow.
    at: Option<(u64, u64)>,
    /// The generation of the files found gone `ids` follow.
    gone: u64,
    ids: HashSet<PhotoId>,
    /// (hash of the photo's file, photo), for the photos looked at so far. A hash may be shared by
    /// two files and an entry may be of a photo that has moved since: a lookup checks the file.
    paths: BTreeSet<(u64, PhotoId)>,
    walk: Walk,
    /// `ids` was made for a catalog that is not this one's (changes couldn't be followed): what
    /// the walk doesn't find in the library is dropped when it ends.
    stale: bool,
    /// Walks over every photo started.
    walks: usize,
}

fn key(path: &str) -> u64 {
    // (fixed keys: the same hash every time)
    let mut h = std::hash::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

impl MissingCount {
    /// Walks over every photo started so far (tests).
    pub fn walks(&self) -> usize {
        self.walks
    }

    /// A walk over the photos is under way: the caller asks for another frame to go on with it.
    pub fn walking(&self) -> bool {
        matches!(self.walk, Walk::Under(_))
    }

    /// The count as of `cat`; and whether changes to the library couldn't be followed photo by
    /// photo (the files of the photos that changed then need a check of every file).
    pub fn update(&mut self, cat: &Catalog, library: u64, files: &Availability) -> (usize, bool) {
        if self.at.is_some_and(|(l, _)| l != library) {
            *self = MissingCount { walks: self.walks, ..Default::default() };
        }
        let mut lost = false;
        match self.at {
            Some((_, rev)) if rev != cat.revision => match cat.photos_changed_since(rev, READS) {
                Some(changed) if changed.len() <= MAX_FOLLOWED => {
                    for id in changed {
                        self.follow_photo(cat, files, id);
                    }
                }
                _ => {
                    lost = true;
                    self.forget();
                }
            },
            _ => {}
        }
        self.at = Some((library, cat.revision));
        self.follow_files(cat, files);
        self.step(cat, files);
        (self.ids.len(), lost)
    }

    /// Back to not knowing which photos are where (the count shown stays until the walk makes a new one).
    fn forget(&mut self) {
        self.paths.clear();
        self.walk = Walk::Idle;
        self.stale = true;
    }

    /// Photo `id` came in, moved, went or came back.
    fn follow_photo(&mut self, cat: &Catalog, files: &Availability, id: PhotoId) {
        let Some(file) = cat.photo(id).and_then(|p| p.library_file()) else {
            self.ids.remove(&id);
            return;
        };
        // (the file of a photo that came in or moved is asked about at once)
        let _ = files.exists(file);
        if self.walk != Walk::Idle {
            self.paths.insert((key(file), id));
        }
        self.set(id, files.is_gone(file));
    }

    fn set(&mut self, id: PhotoId, gone: bool) {
        if gone {
            self.ids.insert(id);
        } else {
            self.ids.remove(&id);
        }
    }

    /// The files found gone or back since the last frame.
    fn follow_files(&mut self, cat: &Catalog, files: &Availability) {
        if files.gone_len() == 0 {
            // nothing is gone: no photo is missing (and no walk to find it out)
            self.ids.clear();
            self.paths.clear();
            self.walk = Walk::Idle;
            self.stale = false;
            self.gone = files.gone_generation();
            return;
        }
        match files.gone_changes_since(self.gone) {
            Some((generation, mut changed)) => {
                self.gone = generation;
                if self.walk != Walk::Idle {
                    changed.sort_unstable();
                    changed.dedup();
                    for path in &changed {
                        self.follow_file(cat, files, path);
                    }
                }
            }
            None => {
                // (too much changed to say what: every photo is looked at again)
                self.gone = files.gone_generation();
                self.forget();
            }
        }
        if self.walk == Walk::Idle {
            self.walk = Walk::Under(None);
            self.walks += 1;
        }
    }

    /// The photos with the file at `path` are missing or not, as the file is.
    fn follow_file(&mut self, cat: &Catalog, files: &Availability, path: &str) {
        let h = key(path);
        let found: Vec<PhotoId> = self.paths.range((h, PhotoId(0))..=(h, PhotoId(u64::MAX))).map(|(_, id)| *id).collect();
        let gone = files.is_gone(path);
        for id in found {
            if cat.photo(id).and_then(|p| p.library_file()) == Some(path) {
                self.set(id, gone);
            } else {
                match cat.photo(id).and_then(|p| p.library_file()) {
                    // (another file with the same hash: its entry stays for that file's changes)
                    Some(f) if key(f) == h => {}
                    // (the photo moved to a file of another hash)
                    Some(_) => {
                        self.paths.remove(&(h, id));
                    }
                    None => {
                        self.paths.remove(&(h, id));
                        self.ids.remove(&id);
                    }
                }
            }
        }
    }

    /// A step of the walk over the photos.
    fn step(&mut self, cat: &Catalog, files: &Availability) {
        let Walk::Under(after) = self.walk else { return };
        let (mut last, mut n) = (after, 0);
        for p in cat.photos_after(after).take(SCAN_STEP) {
            n += 1;
            last = Some(p.id);
            match p.library_file() {
                Some(file) => {
                    self.paths.insert((key(file), p.id));
                    self.set(p.id, files.is_gone(file));
                }
                None => {
                    self.ids.remove(&p.id);
                }
            }
        }
        if n < SCAN_STEP {
            if self.stale {
                // (made for another catalog: what is not in this one's library is not missing)
                self.ids.retain(|id| cat.photo(*id).is_some_and(|p| p.library_file().is_some()));
                self.stale = false;
            }
            self.walk = Walk::Done;
        } else {
            self.walk = Walk::Under(last);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo, Source};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn catalog(n: usize) -> Catalog {
        catalog_with(n, 2)
    }

    /// `n` photos, the first `gone` of them named so that the test probe says their files are not there.
    fn catalog_with(n: usize, gone: usize) -> Catalog {
        let mut c = Catalog::new();
        let ops: Vec<Op> = (0..n)
            .map(|i| {
                let id = c.alloc_photo_id();
                let name = format!("{}_{i}.jpg", if i < gone { "gone" } else { "IMG" });
                Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: format!("/mc/{name}") }, &name, "JPEG", 6, 4, "2026-10-21")) }
            })
            .collect();
        c.apply(Op::Batch { ops }).unwrap();
        c
    }

    /// Checks of `gone` (and nothing else) are done, the files named `gone…` are not there.
    fn files(gone: &[&str]) -> Availability {
        let mut a = Availability::default();
        a.set_probe(Arc::new(|p: &str| !p.contains("gone")));
        a.run_in_background(Arc::new(|| {}));
        found(&a, gone)
    }

    /// `paths` checked (more than one sweep at a time is refused: waits for the last one first).
    fn found(a: &Availability, paths: &[&str]) -> Availability {
        let t0 = Instant::now();
        while a.sweeping() > 0 && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let before = a.gone_len();
        assert!(a.sweep(paths.iter().map(|g| format!("/mc/{g}")).collect()));
        let want = before + paths.iter().filter(|g| g.starts_with("gone")).count();
        while (a.sweeping() > 0 || a.pending() > 0 || a.gone_len() < want) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(1));
        }
        a.clone()
    }

    /// Updates until no walk is under way.
    fn settle(m: &mut MissingCount, cat: &Catalog, a: &Availability) -> usize {
        let mut n = m.update(cat, 1, a).0;
        let mut frames = 0;
        while m.walking() && frames < 10_000 {
            n = m.update(cat, 1, a).0;
            frames += 1;
        }
        n
    }

    #[test]
    fn a_frame_looks_at_a_step_of_the_photos_not_at_all_of_them() {
        let cat = catalog(SCAN_STEP * 5 / 2);
        let a = files(&["gone_0.jpg", "gone_1.jpg"]);
        let mut m = MissingCount::default();
        // the first frame: the photos of one step, which hold both of the files gone
        assert_eq!(m.update(&cat, 1, &a).0, 2);
        assert!(m.walking(), "the rest of the library is for the frames after it");
        assert_eq!(m.paths.len(), SCAN_STEP);
        m.update(&cat, 1, &a);
        assert!(m.walking());
        assert_eq!(m.paths.len(), SCAN_STEP * 2);
        m.update(&cat, 1, &a);
        assert!(!m.walking());
        assert_eq!(m.paths.len(), SCAN_STEP * 5 / 2);
        assert_eq!(m.walks(), 1);
    }

    #[test]
    fn a_file_found_gone_or_back_is_followed_without_a_walk_over_every_photo() {
        let cat = catalog_with(SCAN_STEP * 3, 6);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(settle(&mut m, &cat, &a), 1);
        // nothing changes for ten seconds of frames: the count is kept, no photo is looked at
        for _ in 0..10 {
            assert_eq!(m.update(&cat, 1, &a).0, 1);
            assert!(!m.walking());
        }
        // more files are found gone, one after another: each is in the count by the next frame
        for k in 1..6 {
            found(&a, &[&format!("gone_{k}.jpg")]);
            let n = m.update(&cat, 1, &a).0;
            assert_eq!(n, k + 1, "file {k}");
            assert!(!m.walking(), "no walk to follow a file");
        }
        assert_eq!(m.walks(), 1, "one walk when the library was opened, none for six files found gone");
    }

    /// The checks being replaced (a probe set) changes any file: the count is made again by a walk, and the file
    /// that came back drops out of it.
    #[test]
    fn a_file_that_comes_back_drops_out_of_the_count_when_the_checks_are_replaced() {
        let cat = catalog(SCAN_STEP + 10);
        let mut a = files(&["gone_0.jpg", "gone_1.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(settle(&mut m, &cat, &a), 2);
        // gone_0 is back (its probe answers yes now), the other is still not there
        // (a moment for the checks' thread, idle by now, to stop: a new probe is read by a new thread)
        std::thread::sleep(Duration::from_millis(50));
        a.set_probe(Arc::new(|p: &str| !p.ends_with("gone_1.jpg")));
        a.run_in_background(Arc::new(|| {}));
        assert!(a.sweep(vec!["/mc/gone_1.jpg".into(), "/mc/gone_0.jpg".into()]));
        let t0 = Instant::now();
        while (a.sweeping() > 0 || a.pending() > 0 || a.gone_len() < 1) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(settle(&mut m, &cat, &a), 1);
        assert_eq!(m.walks(), 2, "every photo is looked at again");
        assert!(a.is_gone("/mc/gone_1.jpg") && !a.is_gone("/mc/gone_0.jpg"));
    }

    #[test]
    fn when_nothing_is_gone_no_photo_is_looked_at() {
        let cat = catalog(SCAN_STEP * 2);
        let a = files(&[]);
        let mut m = MissingCount::default();
        assert_eq!(m.update(&cat, 1, &a), (0, false));
        assert!(!m.walking());
        assert_eq!((m.walks(), m.paths.len()), (0, 0));
    }

    #[test]
    fn photos_that_come_in_or_move_are_followed_before_and_during_the_walk() {
        let mut cat = catalog(SCAN_STEP * 2);
        let a = files(&["gone_0.jpg", "gone_new.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(m.update(&cat, 1, &a).0, 1);
        assert!(m.walking());
        // a photo with a missing file comes in, and one is moved to one, while the walk is half done
        let id = cat.alloc_photo_id();
        let new = Photo::new(id, Source::File { path: "/mc/gone_new.jpg".into() }, "gone_new.jpg", "JPEG", 6, 4, "2026-10-21");
        cat.apply(Op::AddPhoto { photo: Box::new(new) }).unwrap();
        assert_eq!(settle(&mut m, &cat, &a), 2);
        assert_eq!(m.walks(), 1);
        // later ones the same: found by the lookup of their files
        let id = cat.alloc_photo_id();
        let again = Photo::new(id, Source::File { path: "/mc/gone_new.jpg".into() }, "gone_new.jpg", "JPEG", 6, 4, "2026-10-21");
        cat.apply(Op::AddPhoto { photo: Box::new(again) }).unwrap();
        assert_eq!(m.update(&cat, 1, &a).0, 3, "two photos of the same file");
        assert!(!m.walking());
        // the first of them goes
        cat.apply(Op::RemovePhoto { id: PhotoId(1) }).unwrap();
        assert_eq!(m.update(&cat, 1, &a).0, 2);
    }

    #[test]
    fn more_changes_than_can_be_followed_walk_the_library_again_and_report_it() {
        let mut cat = catalog(SCAN_STEP);
        let a = files(&["gone_0.jpg", "gone_1.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(settle(&mut m, &cat, &a), 2);
        let ops: Vec<Op> = (0..MAX_FOLLOWED + 1)
            .map(|i| {
                let id = cat.alloc_photo_id();
                Op::AddPhoto {
                    photo: Box::new(Photo::new(id, Source::File { path: format!("/mc/NEW_{i}.jpg") }, "n.jpg", "JPEG", 6, 4, "2026-10-21")),
                }
            })
            .collect();
        cat.apply(Op::Batch { ops }).unwrap();
        // (one of the photos that was missing is not in the library now)
        cat.apply(Op::RemovePhoto { id: PhotoId(1) }).unwrap();
        let (_, lost) = m.update(&cat, 1, &a);
        assert!(lost);
        assert_eq!(settle(&mut m, &cat, &a), 1, "the walk finds what is missing now");
        assert_eq!(m.walks(), 2);
    }

    #[test]
    fn another_library_starts_over() {
        let cat = catalog(100);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(settle(&mut m, &cat, &a), 1);
        let other = catalog_with(50, 0);
        assert_eq!(m.update(&other, 2, &a).0, 0);
        assert_eq!(m.walks(), 2);
    }

    /// Cost of each frame's update on a large library while files are found gone one after another (a sweep of a
    /// share). Run with `--release --ignored --nocapture`.
    #[test]
    #[ignore]
    fn frame_cost_on_a_large_library_while_files_are_found_gone() {
        let cat = catalog_with(400_000, 42);
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        let (mut walk, mut follow) = (Vec::new(), Vec::new());
        let frame = |m: &mut MissingCount, into: &mut Vec<f64>| {
            let t = Instant::now();
            let n = m.update(&cat, 1, &a).0;
            into.push(t.elapsed().as_secs_f64() * 1e3);
            n
        };
        while {
            frame(&mut m, &mut walk);
            m.walking()
        } {}
        for k in 1..42 {
            found(&a, &[&format!("gone_{k}.jpg")]);
            frame(&mut m, &mut follow);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(frame(&mut m, &mut follow), 42);
        for (what, costs) in [("walking the library once", &mut walk), ("following 41 files found gone", &mut follow)] {
            costs.sort_by(f64::total_cmp);
            eprintln!(
                "{what}: update ms over {} frames: median {:.3} p95 {:.3} max {:.3}",
                costs.len(),
                costs[costs.len() / 2],
                costs[costs.len() * 95 / 100],
                costs[costs.len() - 1]
            );
        }
        eprintln!("walks {}", m.walks());
    }

    /// A library that was just opened (its catalog has no history yet) with a file gone takes one walk over its
    /// photos, not one on every frame.
    #[test]
    fn a_just_loaded_catalog_takes_one_walk() {
        let mut cat = catalog(100);
        // (set from outside, as a library being opened is: no history to follow from)
        cat.revision = 7 << 32;
        let a = files(&["gone_0.jpg"]);
        let mut m = MissingCount::default();
        assert_eq!(settle(&mut m, &cat, &a), 1);
        for _ in 0..8 {
            let (n, lost) = m.update(&cat, 1, &a);
            assert_eq!(n, 1);
            assert!(!lost && !m.walking());
        }
        assert_eq!(m.walks(), 1);
    }
}
