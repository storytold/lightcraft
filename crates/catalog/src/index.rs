//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it.
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) update them as they apply, inverses and the
//! rollback of a failed batch included, as those are ops too. An op that can change a photo's
//! keywords forgets the keyword index; the next ask builds it anew.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use crate::{AlbumId, Catalog, PhotoId, StackId};

#[derive(Default)]
pub(crate) struct Indexes {
    /// Photo → the stack it is in; `None` when the catalog was read with a photo in two stacks
    /// (only a hand-edited file can hold that: ops refuse it), where a scan says which one counts.
    stacks: OnceLock<Option<HashMap<PhotoId, StackId>>>,
    /// Album → its photos (regular albums; a smart album's list is empty).
    albums: OnceLock<HashMap<AlbumId, HashSet<PhotoId>>>,
    /// The library's keywords, and the photos that have them.
    keywords: OnceLock<KeywordIndex>,
}

/// A copy builds its own when asked (copies are made to be saved, not queried).
impl Clone for Indexes {
    fn clone(&self) -> Self {
        Indexes::default()
    }
}

/// Derived from the rest of the catalog: two catalogs with the same data are equal whatever was
/// built.
impl PartialEq for Indexes {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Indexes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stacks = self.stacks.get().map(|m| m.as_ref().map(HashMap::len));
        let keywords = self.keywords.get().map(|i| i.counts().count());
        f.debug_struct("Indexes").field("stacks", &stacks).field("albums", &self.albums.get().map(HashMap::len)).field("keywords", &keywords).finish()
    }
}

impl Indexes {
    /// Stack `id` held `old` and holds `new` now (empty: it was added, or is gone).
    pub(crate) fn stack_changed(&mut self, id: StackId, old: &[PhotoId], new: &[PhotoId]) {
        let Some(Some(m)) = self.stacks.get_mut() else { return };
        for p in old {
            if m.get(p) == Some(&id) {
                m.remove(p);
            }
        }
        for p in new {
            m.insert(*p, id);
        }
    }

    /// Album `id` holds `photos` now (`None`: it is gone).
    pub(crate) fn album_changed(&mut self, id: AlbumId, photos: Option<&[PhotoId]>) {
        let Some(m) = self.albums.get_mut() else { return };
        match photos {
            Some(photos) => {
                m.insert(id, photos.iter().copied().collect());
            }
            None => {
                m.remove(&id);
            }
        }
    }

    /// The library's keywords changed: the index is forgotten and the next ask builds it anew.
    pub(crate) fn keywords_changed(&mut self) {
        self.keywords.take();
    }
}

/// One keyword of the index: a photo's spelling of it, or a keyword-list entry no photo has.
pub(crate) struct Entry {
    /// As first written: the name a suggestion offers.
    pub(crate) name: String,
    /// `name` in lower case: what suggestion matching and ordering compare.
    pub(crate) lower: String,
    /// Applications on library photos; 0 for a keyword only the list has.
    pub(crate) count: usize,
    /// The row of the first spelling (by name) that folds to the same `lower`.
    pub(crate) group: usize,
}

/// The library's keywords. `keyword_suggestions` runs on every selection change and reads them here.
pub(crate) struct KeywordIndex {
    /// Photo keywords by name, then keyword-list entries no photo has.
    entries: Vec<Entry>,
    /// `entries[..used]` are on photos.
    used: usize,
    /// Exact keyword spelling → its row.
    by_name: HashMap<String, usize>,
    /// Lower-case keyword → the row of the first spelling (by name) that folds to it; a keyword
    /// the list holds alone, its own row.
    by_lower: HashMap<String, usize>,
    /// Lower-case keyword → the photos that have it, however it is capitalized.
    photos: HashMap<String, Vec<PhotoId>>,
}

impl KeywordIndex {
    /// The keywords of the library `c` holds.
    fn of(c: &Catalog) -> Self {
        let mut count_of: BTreeMap<String, usize> = Default::default();
        let mut photos: HashMap<String, Vec<PhotoId>> = Default::default();
        for p in c.photos().filter(|p| p.in_library()) {
            for k in &p.meta.keywords {
                if let Some(count) = count_of.get_mut(k) {
                    *count += 1;
                } else {
                    count_of.insert(k.clone(), 1);
                }
                // a photo's keywords are applied together, so its id is the last one the key
                // holds: it lands once however many times the photo applies the keyword, and
                // whatever spellings it uses
                let l = k.to_lowercase();
                if !photos.get(&l).is_some_and(|ids| ids.last() == Some(&p.id)) {
                    photos.entry(l).or_default().push(p.id);
                }
            }
        }
        let listed = c.keyword_list.iter().filter(|(key, _)| !photos.contains_key(key.as_str()));
        let mut entries: Vec<Entry> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut by_lower: HashMap<String, usize> = HashMap::new();
        for (name, count) in count_of {
            let lower = name.to_lowercase();
            let group = *by_lower.entry(lower.clone()).or_insert(entries.len());
            by_name.insert(name.clone(), entries.len());
            entries.push(Entry { name, lower, count, group });
        }
        let used = entries.len();
        for (key, l) in listed {
            let name = l.path.clone();
            let group = entries.len();
            by_name.insert(name.clone(), group);
            by_lower.insert(key.clone(), group);
            entries.push(Entry { name, lower: key.clone(), count: 0, group });
        }
        KeywordIndex { entries, used, by_name, by_lower, photos }
    }

    /// Every keyword a photo has, as it was first written, with its application count; sorted by name.
    pub(crate) fn counts(&self) -> impl Iterator<Item = (&str, usize)> {
        self.entries[..self.used].iter().map(|e| (e.name.as_str(), e.count))
    }

    /// The vocabulary suggestions score: every keyword a photo has, then the keyword list's.
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The photos that have `keyword`, however it is capitalized; empty for one no photo has.
    pub(crate) fn photos_of(&self, keyword: &str) -> &[PhotoId] {
        self.photos.get(&keyword.to_lowercase()).map_or(&[], Vec::as_slice)
    }

    /// The row of `keyword` spelled exactly so; every keyword on a library photo has one.
    pub(crate) fn row_of(&self, keyword: &str) -> Option<usize> {
        self.by_name.get(keyword).copied()
    }

    /// The group row of `keyword`, however capitalized.
    pub(crate) fn group_of(&self, keyword: &str) -> Option<usize> {
        self.by_lower.get(&keyword.to_lowercase()).copied()
    }
}

impl Catalog {
    /// The stack photo `photo` is in, by the index (built on first use).
    pub(crate) fn stack_id_of(&self, photo: PhotoId) -> Option<StackId> {
        let m = self.index.stacks.get_or_init(|| {
            let mut m = HashMap::new();
            for s in self.stacks.values() {
                for p in &s.photos {
                    if m.insert(*p, s.id).is_some_and(|other| other != s.id) {
                        return None;
                    }
                }
            }
            Some(m)
        });
        match m {
            Some(m) => m.get(&photo).copied(),
            // in id order, the first stack listing the photo has it
            None => self.stacks.values().find(|s| s.photos.contains(&photo)).map(|s| s.id),
        }
    }

    /// Whether album `album`'s own list holds `photo`, by the index (built on first use).
    pub(crate) fn album_lists(&self, album: AlbumId, photo: PhotoId) -> bool {
        let m = self.index.albums.get_or_init(|| self.albums.values().map(|a| (a.id, a.photos.iter().copied().collect())).collect());
        m.get(&album).is_some_and(|s| s.contains(&photo))
    }

    /// The library's keywords, by the index (built on first use).
    pub(crate) fn keyword_index(&self) -> &KeywordIndex {
        self.index.keywords.get_or_init(|| KeywordIndex::of(self))
    }
}
