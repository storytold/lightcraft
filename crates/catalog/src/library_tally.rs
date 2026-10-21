//! What the sidebar counts of the whole library: photos per folder, per day and per keyword, the
//! All Photos / Picks / Recently Deleted counts. Kept up to date from the photos the latest ops changed.
//!
//! Counting every photo is a pass over the library: about 100 ms at 180,000 photos. An import or a
//! Synchronize Folder adds photos a dozen or so at a time, and counting again after each batch
//! kept the window from answering for as long as the import ran. A [`LibraryTally`] remembers
//! what each photo added to the counts, so after an edit to a few photos
//! ([`Catalog::photos_changed_since`]) it takes their part out and puts what they add now back
//! in ([`LibraryTally::follow`]). What it can't follow (too many photos, a library just opened)
//! the caller counts whole ([`LibraryTally::of`]), wherever it wants: off the UI thread in the app.
//!
//! The trees are built from the counts by the code [`Catalog::folder_tree`],
//! [`Catalog::date_groups`] and [`Catalog::keyword_tree`] build theirs with.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folders::FolderNode;
use crate::keywords::KeywordNode;
use crate::query::DateGroup;
use crate::{Aspects, Catalog, Flag, Photo, PhotoId};

/// The most photos an edit may change for the tally to follow it photo by photo (each one costs
/// a few microseconds; past this, counting whole on a worker thread is the better deal).
pub const MAX_FOLLOWED: usize = 2048;

/// What the tally reads of a photo ([`Part`]): whether it is in the library or Recently Deleted,
/// its flag, its file, its capture time and its keywords. An edit to anything else (a rating, a
/// slider, however many photos) leaves it as it is.
const READS: Aspects = Aspects::PRESENCE.with(Aspects::FLAG).with(Aspects::FILE).with(Aspects::CAPTURED).with(Aspects::META);

/// All Photos, Picks and Recently Deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    /// Photos in the library (not in Recently Deleted, not browse records of Local).
    pub total: usize,
    /// Of those, the picks.
    pub picks: usize,
    /// Photos in Recently Deleted.
    pub deleted: usize,
}

/// What one photo adds to the tally (nothing is kept for a photo that adds nothing: a browse
/// record of Local).
#[derive(Clone, Debug, Default)]
struct Part {
    library: bool,
    pick: bool,
    trashed: bool,
    /// Its folder ([`crate::folders::photo_folder`]), when in the library.
    dir: Option<Arc<str>>,
    /// Its capture day ([`crate::query::capture_day`]), when in the library.
    day: Option<Arc<str>>,
    /// The keyword levels it carries, when in the library: each once, by lower-case path, with
    /// the path as its first keyword through that level writes it.
    levels: Box<[(Arc<str>, Arc<str>)]>,
}

/// One way a keyword level is written, by the photos that write it so.
#[derive(Clone, Debug)]
struct Spelling {
    path: Arc<str>,
    photos: usize,
    /// The lowest photo id that writes it so; `None` once that photo went (the next lowest is
    /// not known photo by photo).
    first: Option<PhotoId>,
}

/// A keyword level: the photos that carry it, and how they write it.
#[derive(Clone, Debug, Default)]
struct Level {
    photos: usize,
    spellings: Vec<Spelling>,
}

impl Level {
    /// The path as the photo with the lowest id writes it (what [`Catalog::keyword_tree`] shows).
    fn path(&self) -> Option<&Arc<str>> {
        self.spellings.iter().min_by_key(|s| s.first.map_or(u64::MAX, |id| id.0)).map(|s| &s.path)
    }

    /// Whether which spelling comes first is not known: two or more, and one of them lost its
    /// first photo.
    fn unsure(&self) -> bool {
        self.spellings.len() > 1 && self.spellings.iter().any(|s| s.first.is_none())
    }
}

/// Bumped by every change to a tally's folders, days or keywords, so a tree built from them can
/// be kept until one changes ([`LibraryTally::folders_stamp`]).
static STAMPS: AtomicU64 = AtomicU64::new(1);

fn stamp() -> u64 {
    STAMPS.fetch_add(1, Ordering::Relaxed)
}

/// The library's photos counted by folder, day and keyword, as of one revision of the catalog.
#[derive(Clone, Debug, Default)]
pub struct LibraryTally {
    revision: u64,
    parts: HashMap<PhotoId, Part>,
    counts: LibraryCounts,
    dirs: BTreeMap<Arc<str>, usize>,
    days: BTreeMap<Arc<str>, usize>,
    levels: HashMap<Arc<str>, Level>,
    /// A keyword level whose first spelling is not known for sure ([`LibraryTally::exact`]).
    unsure: bool,
    folders: u64,
    dates: u64,
    keywords: u64,
}

impl LibraryTally {
    /// Counted from every photo.
    pub fn of(cat: &Catalog) -> LibraryTally {
        let mut t = LibraryTally { revision: cat.revision, folders: stamp(), dates: stamp(), keywords: stamp(), ..Default::default() };
        let mut chains = Chains::new();
        for p in cat.photos() {
            let part = t.part(p, &mut chains);
            t.replace(p.id, part);
        }
        t
    }

    /// The catalog revision the counts are of.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bring the counts up to `cat` as it is now from the photos changed since: the photos that
    /// were taken into account (none when no photo changed). `None` when that can't be done (more
    /// than [`MAX_FOLLOWED`] photos changed, or what changed is not known): count whole then; the
    /// counts are left as they were.
    ///
    /// `cat` has to be the catalog the counts were made of, further along.
    pub fn follow(&mut self, cat: &Catalog) -> Option<Vec<PhotoId>> {
        if self.revision == cat.revision {
            return Some(Vec::new());
        }
        let changed = cat.photos_changed_since(self.revision, READS)?;
        if changed.len() > MAX_FOLLOWED {
            return None;
        }
        let mut chains = Chains::new();
        for id in &changed {
            let part = cat.photo(*id).and_then(|p| self.part(p, &mut chains));
            self.replace(*id, part);
        }
        self.revision = cat.revision;
        Some(changed)
    }

    /// Whether the counts are what counting every photo gives. Not so in one case: a keyword
    /// written in two ways (`Body`, `body`) is shown as the photo with the lowest id writes it,
    /// and when that photo goes, which of the others comes first is not known photo by photo.
    /// Count whole then.
    pub fn exact(&self) -> bool {
        !self.unsure
    }

    pub fn counts(&self) -> LibraryCounts {
        self.counts
    }

    /// [`Catalog::folder_tree`]: the folder labels are `cat`'s.
    pub fn folder_tree(&self, cat: &Catalog) -> Vec<FolderNode> {
        cat.folder_tree_of(self.dirs.iter().map(|(d, n)| (&**d, *n)))
    }

    /// [`Catalog::date_groups`].
    pub fn date_groups(&self) -> Vec<DateGroup> {
        crate::query::date_groups_of(self.days.iter().map(|(d, n)| (&**d, *n)))
    }

    /// [`Catalog::keyword_tree`]: the keyword list is `cat`'s.
    pub fn keyword_tree(&self, cat: &Catalog) -> Vec<KeywordNode> {
        let counted = self.levels.iter().filter_map(|(lower, l)| l.path().map(|p| (&**lower, l.photos, &**p)));
        crate::keywords::keyword_tree_of(cat.listed_keywords(), counted)
    }

    /// Changes whenever the photos counted per folder do (a tree built from them is right until then).
    pub fn folders_stamp(&self) -> u64 {
        self.folders
    }

    /// Changes whenever the photos counted per day do.
    pub fn dates_stamp(&self) -> u64 {
        self.dates
    }

    /// Changes whenever the photos counted per keyword level do.
    pub fn keywords_stamp(&self) -> u64 {
        self.keywords
    }

    /// What `p` adds.
    fn part<'c>(&self, p: &'c Photo, chains: &mut Chains<'c>) -> Option<Part> {
        let library = p.in_library();
        let trashed = p.deleted && !p.local;
        if !library {
            return trashed.then(|| Part { trashed, ..Default::default() });
        }
        let interned = |m: &BTreeMap<Arc<str>, usize>, s: &str| m.get_key_value(s).map_or_else(|| Arc::from(s), |(k, _)| k.clone());
        let mut levels: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        for k in &p.meta.keywords {
            let chain = chains.entry(k.as_str()).or_insert_with(|| crate::keywords::levels_of(k));
            for (lower, path) in chain.iter() {
                if !levels.iter().any(|(l, _)| **l == **lower) {
                    levels.push(self.interned_level(lower, path));
                }
            }
        }
        Some(Part {
            library,
            pick: p.flag == Flag::Pick,
            trashed,
            dir: crate::folders::photo_folder(p).map(|d| interned(&self.dirs, d)),
            day: crate::query::capture_day(p).map(|d| interned(&self.days, d)),
            levels: levels.into_boxed_slice(),
        })
    }

    /// A keyword level and a way of writing it, shared with what the tally already holds.
    fn interned_level(&self, lower: &str, path: &str) -> (Arc<str>, Arc<str>) {
        match self.levels.get_key_value(lower) {
            Some((k, l)) => (k.clone(), l.spellings.iter().find(|s| *s.path == *path).map_or_else(|| Arc::from(path), |s| s.path.clone())),
            None => (Arc::from(lower), Arc::from(path)),
        }
    }

    /// Photo `id` adds `new` now (`None`: nothing, or it is gone): what it added before is taken
    /// out. Only what differs is touched, so the trees of what didn't change are kept.
    fn replace(&mut self, id: PhotoId, new: Option<Part>) {
        let old = self.parts.remove(&id);
        let (o, n) = (old.as_ref(), new.as_ref());
        let flag = |p: Option<&Part>, f: fn(&Part) -> bool| p.is_some_and(f);
        for (count, f) in [
            (&mut self.counts.total, (|p: &Part| p.library) as fn(&Part) -> bool),
            (&mut self.counts.picks, |p: &Part| p.pick),
            (&mut self.counts.deleted, |p: &Part| p.trashed),
        ] {
            *count = count.saturating_sub(usize::from(flag(o, f))).saturating_add(usize::from(flag(n, f)));
        }
        let (od, nd) = (o.and_then(|p| p.dir.as_ref()), n.and_then(|p| p.dir.as_ref()));
        if od != nd {
            od.inspect(|d| take_one(&mut self.dirs, d));
            nd.inspect(|d| *self.dirs.entry(Arc::clone(d)).or_default() += 1);
            self.folders = stamp();
        }
        let (od, nd) = (o.and_then(|p| p.day.as_ref()), n.and_then(|p| p.day.as_ref()));
        if od != nd {
            od.inspect(|d| take_one(&mut self.days, d));
            nd.inspect(|d| *self.days.entry(Arc::clone(d)).or_default() += 1);
            self.dates = stamp();
        }
        let none: &[(Arc<str>, Arc<str>)] = &[];
        let (ol, nl) = (o.map_or(none, |p| &p.levels), n.map_or(none, |p| &p.levels));
        if ol != nl {
            for (lower, path) in ol.iter().filter(|l| !nl.contains(l)) {
                self.level_lost(lower, path, id);
            }
            for (lower, path) in nl.iter().filter(|l| !ol.contains(l)) {
                self.level_gained(lower, path, id);
            }
            self.keywords = stamp();
        }
        if let Some(n) = new {
            self.parts.insert(id, n);
        }
    }

    /// Photo `id` carries keyword level `lower`, written `path`.
    fn level_gained(&mut self, lower: &Arc<str>, path: &Arc<str>, id: PhotoId) {
        let level = self.levels.entry(Arc::clone(lower)).or_default();
        level.photos += 1;
        match level.spellings.iter_mut().find(|s| s.path == *path) {
            // (a first photo that went is not known any more: it stays so)
            Some(s) => {
                s.photos += 1;
                s.first = s.first.map(|f| f.min(id));
            }
            None => level.spellings.push(Spelling { path: Arc::clone(path), photos: 1, first: Some(id) }),
        }
        self.unsure |= level.unsure();
    }

    /// Photo `id` no longer carries keyword level `lower` written `path`.
    fn level_lost(&mut self, lower: &Arc<str>, path: &Arc<str>, id: PhotoId) {
        let Some(level) = self.levels.get_mut(lower) else { return };
        level.photos = level.photos.saturating_sub(1);
        if let Some(i) = level.spellings.iter().position(|s| s.path == *path)
            && let Some(s) = level.spellings.get_mut(i)
        {
            s.photos = s.photos.saturating_sub(1);
            if s.photos == 0 {
                level.spellings.swap_remove(i);
            } else if s.first == Some(id) {
                s.first = None;
            }
        }
        self.unsure |= level.unsure();
        if level.photos == 0 {
            self.levels.remove(lower);
        }
    }
}

/// Each keyword as written → its levels ([`crate::keywords::levels_of`]), worked out once: a
/// large library repeats a few thousand keywords tens of thousands of times.
type Chains<'c> = HashMap<&'c str, Vec<(String, String)>>;

/// One photo fewer under `key`; the key goes with its last photo.
fn take_one(m: &mut BTreeMap<Arc<str>, usize>, key: &Arc<str>) {
    if let Some(n) = m.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            m.remove(key);
        }
    }
}
