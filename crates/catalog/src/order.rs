//! The whole library in each sort order, kept so that a view switch picks what matches out of an order that
//! is already there instead of sorting every photo again ([`Catalog::query_keeping`]).
//!
//! One list per sort key (ascending: a descending order is that one read backwards, as [`Sort::order`]
//! reverses a total order), built on first use and never saved. A photo is named by its place in the
//! catalog's id order, so a query marks what matches in one pass over the photos (which come in that order)
//! and reads the marks along the list, without looking a photo up by id. Each list remembers the revision it
//! was right for. When asked again, [`Catalog::photos_changed_since`] names the photos an op added, removed or
//! changed in what the key's comparison reads (`reads`: its own field, the file name that breaks ties, and
//! whether the photo is in the catalog), so a rating edit moves a photo only in the rating order. Up to
//! `IN_PLACE` such photos are moved in the list itself (a search for each, a shift of the part between); more,
//! or a photo that came or went, are taken out and put back in one pass over the list. When that is not known
//! (too long ago, the catalog was loaded) the photos that came or went are found by comparing ids and the list is
//! sorted again from where it was ([`Catalog::remade`]: nearly in order, so about one pass), or dropped when the
//! query is of a small part of the library. A catalog copy starts without them, like the other [`crate::index`]
//! lookups.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::query::{Sort, SortKey};
use crate::{Aspects, Catalog, Photo, PhotoId};

/// A place that is no photo's.
const GONE: u32 = u32::MAX;

/// Every photo in one sort key's ascending order.
#[derive(Clone, Debug)]
pub(crate) struct Order {
    /// The photos' ids, in id order: the place of a photo in `sorted` is its place here.
    ids: Vec<PhotoId>,
    /// Places in `ids`, in sort order.
    sorted: Vec<u32>,
}

impl Order {
    /// `photos` (in id order) sorted by `sort`.
    fn of<'a>(photos: impl Iterator<Item = &'a Photo>, sort: &Sort) -> Order {
        let photos: Vec<&Photo> = photos.collect();
        let mut sorted: Vec<u32> = (0..u32::try_from(photos.len()).unwrap_or(u32::MAX)).collect();
        // (the order is total: no two photos tie, so the sort need not keep equal ones as they were)
        sorted.sort_unstable_by(|a, b| match (photos.get(*a as usize), photos.get(*b as usize)) {
            (Some(a), Some(b)) => sort.order(a, b),
            _ => a.cmp(b),
        });
        Order { ids: photos.iter().map(|p| p.id).collect(), sorted }
    }
}

/// An order and the revision of the catalog it is right for (outside the order, so that nothing having
/// changed moves it on without copying the lists).
#[derive(Debug)]
struct Kept {
    revision: u64,
    order: Arc<Order>,
}

#[derive(Debug, Default)]
struct Held {
    kept: HashMap<SortKey, Kept>,
    /// The keys a query of a middling share of the library had to sort for itself, for want of an order (see
    /// [`Catalog::order_for`]): asked again, they get one.
    missed: HashSet<SortKey>,
    /// The orders being made on another thread ([`Catalog::prebuild_order`]), by key. Each is filled by its worker
    /// and read here: a catalog that is dropped, or [`Orders::forget`]s, drops them, so a result never reaches
    /// another catalog (a library that was switched to) or an order that was forgotten.
    pending: HashMap<SortKey, Arc<Mutex<Option<Built>>>>,
}

/// An order made on a thread of its own, from the photos as they were at `revision`.
#[derive(Debug)]
struct Built {
    revision: u64,
    order: Order,
}

/// Below this many photos an order is sorted when first asked for (about 50 ms at most): a thread is not worth it.
const PREBUILD_FROM: usize = 50_000;

#[derive(Debug, Default)]
pub(crate) struct Orders(Mutex<Held>, AtomicUsize);

impl Orders {
    /// Forget every order (the catalog was changed other than by ops).
    pub(crate) fn forget(&mut self) {
        if let Ok(h) = self.0.get_mut() {
            *h = Held::default();
        }
    }

    /// The order kept for `key`, to tell whether it is made again (`Arc::ptr_eq`).
    #[cfg(test)]
    pub(crate) fn held(&self, key: SortKey) -> Option<Arc<Order>> {
        self.0.lock().ok()?.kept.get(&key).map(|k| k.order.clone())
    }

    /// Whether an order is being made for `key`.
    #[cfg(test)]
    pub(crate) fn pending(&self, key: SortKey) -> bool {
        self.0.lock().is_ok_and(|h| h.pending.contains_key(&key))
    }

    /// Waits until the order being made for `key` is there (for a test); panics after a minute.
    #[cfg(test)]
    pub(crate) fn wait_built(&self, key: SortKey) {
        let by = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !self.ready(key) {
            assert!(std::time::Instant::now() < by, "the order for {key:?} was not made in a minute");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Whether `key` has an order that is kept, or one that was made on another thread and is waiting to be used.
    fn ready(&self, key: SortKey) -> bool {
        let Ok(held) = self.0.lock() else { return false };
        held.kept.contains_key(&key) || held.pending.get(&key).is_none_or(|slot| slot.lock().is_ok_and(|b| b.is_some()))
    }

    /// How many orders are kept.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.0.lock().map_or(0, |h| h.kept.len())
    }
}

/// What a sort key's comparison reads of a photo, as the aspects of the ops that change it ([`Sort::order`]: the
/// key's own field, then the file name, then the id). The import date is set when a photo is added and by no
/// other op, and the file size only by `SetContent`, which says [`Aspects::ALL`]: those read nothing more.
fn reads(key: SortKey) -> Aspects {
    let key_reads = match key {
        SortKey::CaptureDate => Aspects::CAPTURED,
        SortKey::EditDate => Aspects::DEVELOP,
        SortKey::Rating => Aspects::RATING,
        SortKey::ImportDate | SortKey::FileName | SortKey::FileSize | SortKey::Random => Aspects::NONE,
    };
    // (the file name breaks every tie but a name sort's own; a photo that comes or goes is in every order)
    key_reads.with(Aspects::FILE).with(Aspects::PRESENCE)
}

/// Up to this many photos touched, an order is followed by moving each in the list; more are worked out together.
const IN_PLACE: usize = 16;

/// `sort` made ascending: the one order that is kept for its key.
fn ascending(sort: &Sort) -> Sort {
    Sort { ascending: true, ..*sort }
}

/// The work of sorting `n` things, in comparisons.
fn sorting(n: usize) -> f64 {
    let n = n as f64;
    if n < 2.0 { 0.0 } else { n * n.log2() }
}

impl Catalog {
    /// The photos that are `wanted`, in `sort` order. From the kept order of the whole library when one is kept
    /// or worth making ([`Catalog::order_for`]); else the matches are sorted alone. `None` for a shuffle (never
    /// kept), a library of more than `u32::MAX` photos, or when the matches can't be paired with the photos.
    pub(crate) fn pick_in_order(&self, sort: &Sort, wanted: impl Fn(&Photo) -> bool) -> Option<Vec<PhotoId>> {
        if sort.key == SortKey::Random || u32::try_from(self.len()).is_err() {
            return None;
        }
        // (the photos come in id order, as `ids` of an order does)
        let marks: Vec<bool> = self.photos().map(|p| wanted(p)).collect();
        let matching = marks.iter().filter(|m| **m).count();
        let Some(order) = self.order_for(sort, marks.len(), matching) else {
            let mut v: Vec<&Photo> = self.photos().zip(&marks).filter(|(_, m)| **m).map(|(p, _)| p.as_ref()).collect();
            v.sort_by(|a, b| sort.order(a, b));
            return Some(v.into_iter().map(|p| p.id).collect());
        };
        if marks.len() != order.ids.len() {
            return None;
        }
        let pick = |at: &u32| {
            let at = *at as usize;
            if *marks.get(at)? { order.ids.get(at).copied() } else { None }
        };
        Some(if sort.ascending { order.sorted.iter().filter_map(pick).collect() } else { order.sorted.iter().rev().filter_map(pick).collect() })
    }

    /// The order kept for `sort`'s key, brought up to date now; made when that pays: a query of `matching` of the
    /// `n` photos is better sorted alone while an order is not kept, as sorting every photo costs `n·log n` to its
    /// `m·log m`. Under 1/16 of that, never (a folder, a search); under 1/4, the second time the key is asked
    /// (a view that comes back is worth it); beyond, at once. `None`: sort the matches alone.
    fn order_for(&self, sort: &Sort, n: usize, matching: usize) -> Option<Arc<Order>> {
        let sort = ascending(sort);
        let mut held = self.index.orders.0.lock().ok()?;
        let share = sorting(matching) / sorting(n).max(1.0);
        // (an order made on another thread, once it is there, is as one that was kept: brought up to date below)
        let ready = if held.kept.contains_key(&sort.key) { None } else { held.pending.get(&sort.key).cloned() };
        if let Some(slot) = ready
            && let Ok(mut built) = slot.lock()
            && let Some(Built { revision, order }) = built.take()
        {
            held.pending.remove(&sort.key);
            held.kept.insert(sort.key, Kept { revision, order: Arc::new(order) });
        }
        if let Some(kept) = held.kept.get_mut(&sort.key) {
            // what changed is no longer known: a small query is not worth making the whole order again
            if share < 1.0 / 16.0 && kept.revision != self.revision && self.photos_changed_since(kept.revision, reads(sort.key)).is_none() {
                held.kept.remove(&sort.key);
                return None;
            }
            self.catch_up(&sort, kept);
            return Some(kept.order.clone());
        }
        if share < 1.0 / 16.0 || (share < 0.25 && held.missed.insert(sort.key)) {
            return None;
        }
        // (an order being made on another thread is overtaken: its result is not wanted)
        held.pending.remove(&sort.key);
        let order = Arc::new(self.sorted_afresh(&sort));
        held.kept.insert(sort.key, Kept { revision: self.revision, order: order.clone() });
        Some(order)
    }

    /// How many times every photo was sorted from scratch for this catalog's orders (a diagnostic: what a kept order
    /// and [`Catalog::prebuild_order`] are there to avoid).
    #[doc(hidden)]
    pub fn full_sorts(&self) -> usize {
        self.index.orders.1.load(Ordering::Relaxed)
    }

    /// Whether the order for `key` is there to be used: kept, or made on another thread and waiting (a diagnostic).
    /// `true` too when none is being made.
    #[doc(hidden)]
    pub fn order_ready(&self, key: SortKey) -> bool {
        self.index.orders.ready(key)
    }

    /// Starts making the order for `sort`'s key on a thread of its own, when the library is big enough for the first
    /// sort to be felt and no order is kept or being made: the sort that a frame would otherwise wait for (the
    /// first big view in a sort after the library was opened) is then done by the time it is asked for. The thread
    /// gets the photos as they are now (they are shared, not copied; an edit after this copies the photo it edits)
    /// and its result is tagged with the revision they were at: asked for, it is brought up to date like any kept
    /// order, so it is never wrong; asked for before it is ready, the query sorts as it did. Nothing is started
    /// where a thread can't be (it only costs that sort).
    pub fn prebuild_order(&self, sort: &Sort) {
        self.prebuild_order_holding(sort, || ());
    }

    /// [`Self::prebuild_order`] with something the thread holds while it works: `hold` is called once, on the calling
    /// thread, only when a thread is about to start (not when nothing is to be made), and what it returns is moved
    /// into the thread and dropped after the order is handed over. The engine passes an activity row, so a sort that
    /// takes seconds on a big library is listed for as long as it runs.
    pub fn prebuild_order_holding<H: Send + 'static>(&self, sort: &Sort, hold: impl FnOnce() -> H) {
        if sort.key == SortKey::Random || self.len() < PREBUILD_FROM || u32::try_from(self.len()).is_err() {
            return;
        }
        let Ok(mut held) = self.index.orders.0.lock() else { return };
        if held.kept.contains_key(&sort.key) || held.pending.contains_key(&sort.key) {
            return;
        }
        let sort = ascending(sort);
        let photos: Vec<Arc<Photo>> = self.photos().cloned().collect();
        let revision = self.revision;
        let slot = Arc::new(Mutex::new(None));
        let theirs = Arc::downgrade(&slot);
        let holding = hold();
        let work = move || {
            let _holding = holding;
            let order = Order::of(photos.iter().map(|p| p.as_ref()), &sort);
            // (nobody to hand it to when the catalog is gone)
            if let Some(slot) = theirs.upgrade()
                && let Ok(mut s) = slot.lock()
            {
                *s = Some(Built { revision, order });
            }
        };
        // (a thread that can't start drops `work`, and with it what it holds)
        if std::thread::Builder::new().name("kept-order".into()).spawn(work).is_ok() {
            held.pending.insert(sort.key, slot);
        }
    }

    /// Brings `kept` up to date: the ops since its revision changed nothing it reads (it stays as it is, not
    /// copied), a few photos (each is moved in the list itself), or many or the photos in the catalog (it is
    /// worked out again from the old one).
    fn catch_up(&self, sort: &Sort, kept: &mut Kept) {
        let Some(touched) = self.photos_changed_since(kept.revision, reads(sort.key)) else {
            // (no op since, in a library just loaded: right as it is)
            if kept.revision == self.revision && kept.order.ids.len() == self.len() {
                return;
            }
            kept.order = Arc::new(self.remade(sort, &kept.order));
            kept.revision = self.revision;
            return;
        };
        if !(touched.is_empty() && kept.order.ids.len() == self.len()) {
            let moved = touched.len() <= IN_PLACE && self.move_in_place(sort, &mut kept.order, &touched);
            if !moved {
                kept.order = Arc::new(self.rebuilt(sort, &kept.order, &touched));
            }
        }
        kept.revision = self.revision;
    }

    /// Moves the `touched` photos (few, sorted by id, each once) to where they sort now, in `order` itself: no
    /// copy of it unless a query's result still shares it, a search for each in the list, and a shift of the part
    /// of the list between. `false` when that does not apply (a photo came or went: the list is of other photos),
    /// with `order` as it was or made afresh.
    fn move_in_place(&self, sort: &Sort, order: &mut Arc<Order>, touched: &[PhotoId]) -> bool {
        if order.ids.len() != self.len() {
            return false;
        }
        let places: Option<Vec<u32>> = touched
            .iter()
            .map(|id| if self.photo(*id).is_some() { order.ids.binary_search(id).ok().and_then(|i| u32::try_from(i).ok()) } else { None })
            .collect();
        let Some(places) = places else { return false };
        let Order { ids, sorted } = Arc::make_mut(order);
        // all out first: what is left is in order, so each goes back by a search
        for at in &places {
            let Some(from) = sorted.iter().position(|a| a == at) else {
                *order = Arc::new(self.sorted_afresh(sort));
                return true;
            };
            sorted.remove(from);
        }
        for at in places {
            let key = |at: u32| ids.get(at as usize).and_then(|id| self.photo(*id));
            let Some(p) = key(at) else { continue };
            let to = sorted.partition_point(|other| key(*other).is_some_and(|o| sort.order(o, p).is_lt()));
            sorted.insert(to, at);
        }
        if sorted.len() != ids.len() {
            *order = Arc::new(self.sorted_afresh(sort));
        }
        true
    }

    /// `old` worked out again after the ops that touched the photos `touched` (sorted by id, each once), from
    /// the list of `old` and one pass over the photos: those taken out and put back by their keys.
    fn rebuilt(&self, sort: &Sort, old: &Order, touched: &[PhotoId]) -> Order {
        let photos: Vec<&Photo> = self.photos().map(|p| p.as_ref()).collect();
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        // where each photo of the old list is now (the touched ones are taken out, to go back by their new keys)
        let mut now = places_now(&old.ids, &ids);
        for id in touched {
            if let Ok(i) = old.ids.binary_search(id)
                && let Some(n) = now.get_mut(i)
            {
                *n = GONE;
            }
        }
        let kept: Vec<u32> = old.sorted.iter().filter_map(|at| now.get(*at as usize).copied().filter(|n| *n != GONE)).collect();
        let mut moved: Vec<u32> = touched.iter().filter_map(|id| ids.binary_search(id).ok()).filter_map(|i| u32::try_from(i).ok()).collect();
        let key = |at: u32| photos.get(at as usize).copied();
        moved.sort_unstable_by(|a, b| match (key(*a), key(*b)) {
            (Some(a), Some(b)) => sort.order(a, b),
            _ => a.cmp(b),
        });
        // each goes in by a search, the ones before it already in place
        let mut sorted = Vec::with_capacity(kept.len().saturating_add(moved.len()));
        let mut from = 0;
        for at in moved {
            let Some(p) = key(at) else { continue };
            let tail = kept.get(from..).unwrap_or_default();
            let to = from + tail.partition_point(|other| key(*other).is_some_and(|o| sort.order(o, p).is_lt()));
            sorted.extend_from_slice(kept.get(from..to).unwrap_or_default());
            sorted.push(at);
            from = to;
        }
        sorted.extend_from_slice(kept.get(from..).unwrap_or_default());
        if sorted.len() == ids.len() { Order { ids, sorted } } else { self.sorted_afresh(sort) }
    }

    /// `old` worked out again when what changed since is not known (too many ops ago, the catalog was loaded): the
    /// photos that came or went are found by comparing the ids (both lists are in id order), and the rest is
    /// sorted from where `old` had them. Sorting that is nearly in order takes one pass and a merge, where
    /// sorting every photo from scratch takes `n·log n` comparisons, and a photo whose key changed is put right
    /// as well (the sort does not rely on `old` being right).
    fn remade(&self, sort: &Sort, old: &Order) -> Order {
        let photos: Vec<&Photo> = self.photos().map(|p| p.as_ref()).collect();
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let now = places_now(&old.ids, &ids);
        let mut placed = vec![false; ids.len()];
        let mut sorted: Vec<u32> = Vec::with_capacity(ids.len());
        for at in &old.sorted {
            if let Some(n) = now.get(*at as usize).copied().filter(|n| *n != GONE) {
                if let Some(p) = placed.get_mut(n as usize) {
                    *p = true;
                }
                sorted.push(n);
            }
        }
        // (the photos that came go after, in id order: runs for the sort to merge in)
        sorted.extend((0..u32::try_from(ids.len()).unwrap_or(u32::MAX)).filter(|at| !placed.get(*at as usize).copied().unwrap_or(true)));
        // (the order is total, so a stable sort gives the one order; it is the one that finds the runs already there)
        sorted.sort_by(|a, b| match (photos.get(*a as usize), photos.get(*b as usize)) {
            (Some(a), Some(b)) => sort.order(a, b),
            _ => a.cmp(b),
        });
        Order { ids, sorted }
    }

    /// Every photo, sorted.
    fn sorted_afresh(&self, sort: &Sort) -> Order {
        self.index.orders.1.fetch_add(1, Ordering::Relaxed);
        Order::of(self.photos().map(|p| p.as_ref()), sort)
    }

    /// Every photo's id in `sort`, made afresh from every photo, never from a kept order: what a kept order
    /// has to equal.
    #[cfg(test)]
    pub(crate) fn sorted_from_scratch(&self, sort: &Sort) -> Vec<PhotoId> {
        let order = self.sorted_afresh(&ascending(sort));
        let ids = order.sorted.iter().filter_map(|at| order.ids.get(*at as usize).copied());
        if sort.ascending { ids.collect() } else { ids.rev().collect() }
    }

    /// The order `sort` keeps now, as the ids a query of everything gives (ascending), for a test to compare.
    #[cfg(test)]
    pub(crate) fn kept_ids(&self, sort: &Sort) -> Option<Vec<PhotoId>> {
        self.pick_in_order(sort, |_| true)
    }
}

/// Where each of the `old` ids (sorted) is in `new` (sorted), [`GONE`] for one that is not there.
fn places_now(old: &[PhotoId], new: &[PhotoId]) -> Vec<u32> {
    let mut j = 0;
    old.iter()
        .map(|id| {
            while new.get(j).is_some_and(|n| n < id) {
                j += 1;
            }
            match new.get(j) {
                Some(n) if n == id => u32::try_from(j).unwrap_or(GONE),
                _ => GONE,
            }
        })
        .collect()
}
