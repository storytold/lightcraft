//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it, which
//! photos have their file in a folder, which have a file of a given name.
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) and those that add, remove or point a photo at
//! another file (`AddPhoto`, `RemovePhoto`, `Relink`, `SetFile`) update them as they apply,
//! inverses and the rollback of a failed batch included, as those are ops too. An op that can
//! change a photo's keywords forgets the keyword index; the next ask builds it anew.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use crate::{AlbumId, Catalog, PhotoId, Source, StackId};

#[derive(Default)]
pub(crate) struct Indexes {
    /// Photo → the stack it is in; `None` when the catalog was read with a photo in two stacks
    /// (only a hand-edited file can hold that: ops refuse it), where a scan says which one counts.
    stacks: OnceLock<Option<HashMap<PhotoId, StackId>>>,
    /// Album → its photos (regular albums; a smart album's list is empty).
    albums: OnceLock<HashMap<AlbumId, HashSet<PhotoId>>>,
    /// Folder (its [`photo_folder_index_key`], lower-cased) → the photos whose file is directly in
    /// it, whatever their state (in the library, in Recently Deleted, browsed in Local). Demo
    /// photos have none.
    folders: OnceLock<HashMap<String, HashSet<PhotoId>>>,
    /// File name, lower-cased ([`file_name_index_key`]) → the photos whose file has it (same
    /// states as `folders`).
    names: OnceLock<HashMap<String, HashSet<PhotoId>>>,
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
        f.debug_struct("Indexes")
            .field("stacks", &stacks)
            .field("albums", &self.albums.get().map(HashMap::len))
            .field("folders", &self.folders.get().map(HashMap::len))
            .field("names", &self.names.get().map(HashMap::len))
            .field("keywords", &keywords)
            .finish()
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

    /// Photo `id`'s file was at `old` and is at `new` now (`None`: no file, or the photo was added
    /// or is gone).
    pub(crate) fn file_changed(&mut self, id: PhotoId, old: Option<&str>, new: Option<&str>) {
        if old == new {
            return;
        }
        if let Some(m) = self.folders.get_mut() {
            rekey(m, id, old.and_then(photo_folder_index_key), new.and_then(photo_folder_index_key));
        }
        if let Some(m) = self.names.get_mut() {
            rekey(m, id, old.and_then(file_name_index_key), new.and_then(file_name_index_key));
        }
    }

    /// The library's keywords changed: the index is forgotten and the next ask builds it anew.
    pub(crate) fn keywords_changed(&mut self) {
        self.keywords.take();
    }
}

fn rekey(m: &mut HashMap<String, HashSet<PhotoId>>, id: PhotoId, old: Option<String>, new: Option<String>) {
    if let Some(k) = old
        && let Some(set) = m.get_mut(&k)
    {
        set.remove(&id);
        if set.is_empty() {
            m.remove(&k);
        }
    }
    if let Some(k) = new {
        m.entry(k).or_default().insert(id);
    }
}

/// Whether `path` is a plain POSIX path, its own identity under [`crate::query::folder_key`]: no
/// `\`, no repeated separator, no `.` or `..` part, no trailing separator.
fn plain_posix(path: &str) -> bool {
    !cfg!(windows)
        && path.starts_with('/')
        && !path.contains('\\')
        && !path.contains("//")
        && !path.contains("/./")
        && !path.contains("/../")
        && !path.ends_with('/')
        && !path.ends_with("/.")
        && !path.ends_with("/..")
}

/// The folder-index key of a file path: its folder as [`crate::folder_of`] spells it, lower-cased
/// (`None` for a bare relative name). A plain POSIX path is cut at its last `/` without building a
/// [`crate::query::folder_key`].
pub fn photo_folder_index_key(path: &str) -> Option<String> {
    let cut = |key: &str| key.rsplit_once('/').map(|(dir, _)| if dir.is_empty() { "/".to_string() } else { dir.to_lowercase() });
    if plain_posix(path) { cut(path) } else { cut(&crate::query::folder_key(path)) }
}

/// The folder-index key of a folder path (as [`photo_folder_index_key`] gives for the files in it).
pub fn folder_index_key(folder: &str) -> String {
    let key = crate::query::folder_key(folder).to_lowercase();
    // a disk's or share's root key ends in `/` (`c:/`, `//srv/share/`); its files' folder key doesn't
    match key.strip_suffix('/') {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => key,
    }
}

/// The name-index key of a file path: its file name, lower-cased (`None`: it has none).
pub fn file_name_index_key(path: &str) -> Option<String> {
    // `/` and `\` both separate on every platform (a catalog can move between them), as for the folder key
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty() && *n != "." && *n != "..").map(str::to_lowercase)
}

/// The path of a photo's file (`None` for a demo scene).
pub(crate) fn file_path(s: &Source) -> Option<&str> {
    match s {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

fn sorted(set: Option<&HashSet<PhotoId>>) -> Vec<PhotoId> {
    let mut v: Vec<PhotoId> = set.map(|s| s.iter().copied().collect()).unwrap_or_default();
    v.sort_unstable();
    v
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

    fn file_index(&self, which: &str, key: fn(&str) -> Option<String>) -> HashMap<String, HashSet<PhotoId>> {
        let t = web_time::Instant::now();
        let mut m: HashMap<String, HashSet<PhotoId>> = HashMap::new();
        for p in self.photos.values() {
            if let Source::File { path } = &p.source
                && let Some(k) = key(path)
            {
                m.entry(k).or_default().insert(p.id);
            }
        }
        log::info!("catalog: {which} index built for {} photos in {:.1} ms", self.photos.len(), t.elapsed().as_secs_f64() * 1e3);
        m
    }

    /// The photos whose file is directly in the folder with key `folder_key` (from
    /// [`photo_folder_index_key`] or [`folder_index_key`]), whatever their state, sorted by id.
    /// The grouping ignores letter case: callers compare exact paths themselves. The first query
    /// builds the index in one pass over the library; it costs about one set entry per photo.
    pub fn photos_in_folder_any_case(&self, folder_key: &str) -> Vec<PhotoId> {
        sorted(self.index.folders.get_or_init(|| self.file_index("folder", photo_folder_index_key)).get(folder_key))
    }

    /// The photos whose file is named `name_lower` (lower-cased, as [`file_name_index_key`] gives),
    /// whatever their state, sorted by id. The grouping ignores letter case: callers compare exact
    /// names themselves. The first query builds the index in one pass over the library; it costs
    /// about one set entry per photo.
    pub fn photos_named_any_case(&self, name_lower: &str) -> Vec<PhotoId> {
        sorted(self.index.names.get_or_init(|| self.file_index("name", file_name_index_key)).get(name_lower))
    }

    /// The library's keywords, by the index (built on first use).
    pub(crate) fn keyword_index(&self) -> &KeywordIndex {
        self.index.keywords.get_or_init(|| KeywordIndex::of(self))
    }
}
