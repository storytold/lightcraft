//! The whole library in each sort order, kept so that a view switch picks what matches out of an order that
//! is already there instead of sorting every photo again ([`Catalog::query_keeping`]).
//!
//! One list per sort key (ascending: a descending order is that one read backwards, as [`Sort::order`]
//! reverses a total order), built on first use and never saved. A photo is named by its place in the
//! catalog's id order, so a query marks what matches in one pass over the photos (which come in that order)
//! and reads the marks along the list, without looking a photo up by id. Each list remembers the revision it
//! was made at. When asked again, [`Catalog::photos_changed_since`] names the photos an op added, removed or
//! changed in what an order reads ([`Aspects::ORDER`]) since: those are taken out and put back where they sort
//! now, in one pass over the list. When that is not known (too long ago, the catalog was loaded) or the result
//! is not one place per photo, the list is made again from every photo. A catalog copy starts without them,
//! like the other [`crate::index`] lookups.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::query::{Sort, SortKey};
use crate::{Aspects, Catalog, Photo, PhotoId};

/// A place that is no photo's.
const GONE: u32 = u32::MAX;

/// Every photo in one sort key's ascending order, as the catalog was at `revision`.
#[derive(Debug)]
struct Order {
    revision: u64,
    /// The photos' ids, in id order: the place of a photo in `sorted` is its place here.
    ids: Vec<PhotoId>,
    /// Places in `ids`, in sort order.
    sorted: Vec<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct Orders(Mutex<HashMap<SortKey, Arc<Order>>>);

impl Orders {
    /// Forget every order (the catalog was changed other than by ops).
    pub(crate) fn forget(&mut self) {
        if let Ok(m) = self.0.get_mut() {
            m.clear();
        }
    }
}

/// `sort` made ascending: the one order that is kept for its key.
fn ascending(sort: &Sort) -> Sort {
    Sort { ascending: true, ..*sort }
}

impl Catalog {
    /// The photos that are `wanted`, in `sort` order, taken from the kept order of the whole library. `None` when
    /// none is kept for `sort` (a shuffle is never kept; a library of more than `u32::MAX` photos, or a lock a
    /// panic poisoned, has none either).
    pub(crate) fn pick_in_order(&self, sort: &Sort, wanted: impl Fn(&Photo) -> bool) -> Option<Vec<PhotoId>> {
        let order = self.kept_order(sort)?;
        // (the photos come in id order, as `ids` does)
        let marks: Vec<bool> = self.photos().map(|p| wanted(p)).collect();
        if marks.len() != order.ids.len() {
            return None;
        }
        let pick = |at: &u32| {
            let at = *at as usize;
            if *marks.get(at)? { order.ids.get(at).copied() } else { None }
        };
        Some(if sort.ascending { order.sorted.iter().filter_map(pick).collect() } else { order.sorted.iter().rev().filter_map(pick).collect() })
    }

    /// The order kept for `sort`'s key, made or brought up to date now.
    fn kept_order(&self, sort: &Sort) -> Option<Arc<Order>> {
        if sort.key == SortKey::Random || u32::try_from(self.len()).is_err() {
            return None;
        }
        let sort = ascending(sort);
        let mut kept = self.index.orders.0.lock().ok()?;
        let order = match kept.get(&sort.key) {
            Some(old) => self.followed(&sort, old),
            None => self.sorted_afresh(&sort),
        };
        let order = Arc::new(order);
        kept.insert(sort.key, order.clone());
        Some(order)
    }

    /// `old`, after the ops since its revision (`old` itself when they changed nothing it reads).
    fn followed(&self, sort: &Sort, old: &Arc<Order>) -> Order {
        let Some(touched) = self.photos_changed_since(old.revision, Aspects::ORDER) else { return self.sorted_afresh(sort) };
        if touched.is_empty() && old.ids.len() == self.len() {
            return Order { revision: self.revision, ids: old.ids.clone(), sorted: old.sorted.clone() };
        }
        let photos: Vec<&Photo> = self.photos().map(|p| p.as_ref()).collect();
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        // where each photo of the old list is now (the touched ones are taken out, to go back by their new keys)
        let mut now = places_now(&old.ids, &ids);
        for id in &touched {
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
        if sorted.len() == ids.len() { Order { revision: self.revision, ids, sorted } } else { self.sorted_afresh(sort) }
    }

    /// Every photo, sorted.
    fn sorted_afresh(&self, sort: &Sort) -> Order {
        let photos: Vec<&Photo> = self.photos().map(|p| p.as_ref()).collect();
        let mut sorted: Vec<u32> = (0..u32::try_from(photos.len()).unwrap_or(u32::MAX)).collect();
        // (the order is total: no two photos tie, so the sort need not keep equal ones as they were)
        sorted.sort_unstable_by(|a, b| match (photos.get(*a as usize), photos.get(*b as usize)) {
            (Some(a), Some(b)) => sort.order(a, b),
            _ => a.cmp(b),
        });
        Order { revision: self.revision, ids: photos.iter().map(|p| p.id).collect(), sorted }
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
