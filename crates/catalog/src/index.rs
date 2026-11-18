//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it, which
//! photos have their file in a folder, which have a file of a given name.
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) update them as they apply, inverses and the
//! rollback of a failed batch included, as those are ops too; so do `AddPhoto`, `RemovePhoto`,
//! `Relink` and `SetFile` for the file indexes. The keyword index is dropped by
//! every op that can change a keyword and rebuilt when next asked.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::{AlbumId, Catalog, Op, Photo, PhotoId, Source, StackId};

#[derive(Default)]
pub(crate) struct Indexes {
    /// Photo → the stack it is in; `None` when the catalog was read with a photo in two stacks
    /// (only a hand-edited file can hold that: ops refuse it), where a scan says which one counts.
    stacks: OnceLock<Option<HashMap<PhotoId, StackId>>>,
    /// Album → its photos (regular albums; a smart album's list is empty).
    albums: OnceLock<HashMap<AlbumId, HashSet<PhotoId>>>,
    /// Folder ([`photo_folder_index_key`], lower-cased) → the photos whose file is directly in it,
    /// whatever their state (library, Recently Deleted, browsed in Local). Demo photos have none.
    folders: OnceLock<HashMap<String, HashSet<PhotoId>>>,
    /// File name, lower-cased ([`file_name_index_key`]) → the photos whose file has it.
    names: OnceLock<HashMap<String, HashSet<PhotoId>>>,
    /// The library's keywords (dropped by any op that can change them, see [`Indexes::op_applied`]).
    keywords: OnceLock<crate::keywords::KeywordIndex>,
    /// The keyword tree built from it (dropped with it).
    keyword_tree: OnceLock<std::sync::Arc<Vec<crate::keywords::KeywordNode>>>,
    /// Photo → its lower-cased free-text search fields ([`crate::query::search_fields_of`]), made by
    /// the first pass over the library that searches text. A photo an op changes is taken out (it is
    /// read from its own fields until the whole index is made again).
    search: OnceLock<HashMap<PhotoId, Box<[Box<str>]>>>,
    /// Photo → the lower-cased text a smart album's Text rule looks through ([`crate::rules::rule_text_of`]).
    rule_text: OnceLock<HashMap<PhotoId, Box<str>>>,
    /// Content ([`content_key`]) → the photos that have it.
    contents: OnceLock<HashMap<String, Vec<PhotoId>>>,
    /// Whether a smart album asks whether a photo has the same content as another ([`Catalog::follows_contents`]).
    contents_asked: OnceLock<bool>,
    /// Reference photo → its Vignetting Calibration state ([`Catalog::vignetting_reference_states`]), dropped by
    /// every op that can change it.
    pub(crate) vignetting_references: OnceLock<HashMap<PhotoId, crate::ReferenceState>>,
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
        f.debug_struct("Indexes")
            .field("stacks", &stacks)
            .field("albums", &self.albums.get().map(HashMap::len))
            .field("folders", &self.folders.get().map(HashMap::len))
            .field("names", &self.names.get().map(HashMap::len))
            .field("keywords", &self.keywords.get().is_some())
            .field("keyword_tree", &self.keyword_tree.get().is_some())
            .field("search", &self.search.get().map(HashMap::len))
            .field("rule_text", &self.rule_text.get().map(HashMap::len))
            .finish()
    }
}

impl Indexes {
    /// `op` was applied (on its own or in a batch): forget what it may have changed.
    pub(crate) fn op_applied(&mut self, op: &Op) {
        // (only the kinds that leave every photo's keywords, its presence and the keyword list as
        // they were keep the keyword index; a new kind drops it)
        let keeps_keywords = matches!(
            op,
            Op::SetRating { .. }
                | Op::SetFlag { .. }
                | Op::SetLabel { .. }
                | Op::SetDevelop { .. }
                | Op::SetVersions { .. }
                | Op::SetHistory { .. }
                | Op::PushHistory { .. }
                | Op::SetHistoryCursor { .. }
                | Op::SetBefore { .. }
                | Op::SetExported { .. }
                | Op::SetPreview { .. }
                | Op::SetCaptured { .. }
                | Op::SetFile { .. }
                | Op::Relink { .. }
                | Op::SetSha1 { .. }
                | Op::SetAnalysis { .. }
                | Op::Batch { .. }
        );
        if !keeps_keywords {
            self.keywords = OnceLock::new();
            self.keyword_tree = OnceLock::new();
        }
        // (which photo is a reference follows the map table, the photos' files and SHA-1s and whether they're
        // deleted: only kinds that change none of them keep it)
        let keeps_references = matches!(
            op,
            Op::SetRating { .. }
                | Op::SetFlag { .. }
                | Op::SetLabel { .. }
                | Op::SetDevelop { .. }
                | Op::SetVersions { .. }
                | Op::SetHistory { .. }
                | Op::PushHistory { .. }
                | Op::SetHistoryCursor { .. }
                | Op::SetBefore { .. }
                | Op::SetExported { .. }
                | Op::SetPreview { .. }
                | Op::SetCaptured { .. }
                | Op::SetMeta { .. }
                | Op::SetAnalysis { .. }
                | Op::Batch { .. }
        );
        if !keeps_references {
            self.vignetting_references = OnceLock::new();
        }
    }

    /// An op changes photo `id` (or adds or removes it), of a library of `photos`: its search text
    /// is made again from the photo when next asked. Once an eighth of the library is not in a
    /// text index, the index is dropped, to be made whole again by the next pass.
    pub(crate) fn photo_touched(&mut self, id: PhotoId, photos: usize) {
        let gone = |len: usize| len.saturating_mul(8) < photos.saturating_mul(7);
        if let Some(m) = self.search.get_mut() {
            m.remove(&id);
            if gone(m.len()) {
                self.search = OnceLock::new();
            }
        }
        if let Some(m) = self.rule_text.get_mut() {
            m.remove(&id);
            if gone(m.len()) {
                self.rule_text = OnceLock::new();
            }
        }
    }

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

    /// Photo `id` had content `old` and has `new` now (`None`: none the library counts).
    fn content_changed(&mut self, id: PhotoId, old: Option<&str>, new: Option<String>) {
        let Some(m) = self.contents.get_mut() else { return };
        if let Some(old) = old
            && let Some(ids) = m.get_mut(old)
        {
            ids.retain(|p| *p != id);
            if ids.is_empty() {
                m.remove(old);
            }
        }
        if let Some(new) = new {
            m.entry(new).or_default().push(id);
        }
    }

    /// An album came, went or got other rules: whether a smart album asks about contents is asked again.
    pub(crate) fn albums_changed(&mut self) {
        self.contents_asked = OnceLock::new();
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
    // `/` and `\` both separate on every platform (a catalog can move between them)
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

/// The content photo `p` stands for in the library: its file's hash, without the `:dup<id>` a Duplicate's carries.
/// `None` for a photo with no hash, a virtual copy (the same photo as its master, not another one), a photo in
/// Recently Deleted and one only browsed in Local.
pub(crate) fn content_key(p: &Photo) -> Option<&str> {
    if p.copy_of.is_some() || p.deleted || p.local {
        return None;
    }
    p.content_hash.as_deref().map(|h| h.split(':').next().unwrap_or(h)).filter(|h| !h.is_empty())
}

impl Catalog {
    /// Whether `p` is this catalog's own photo (not a copy with other values tried against it).
    fn is_own(&self, p: &crate::Photo) -> bool {
        self.photos.get(&p.id).is_some_and(|own| std::ptr::eq(&**own, p))
    }

    /// `p`'s lower-cased free-text search fields, when `p` is this catalog's own photo and they are
    /// kept. `build`: make them for every photo first when they weren't (a pass over the whole
    /// library does; testing one photo shouldn't make them for 300 000).
    pub(crate) fn search_fields(&self, p: &crate::Photo, build: bool) -> Option<&[Box<str>]> {
        if !self.is_own(p) {
            return None;
        }
        let m = if build {
            self.index.search.get_or_init(|| self.photos.values().map(|p| (p.id, crate::query::search_fields_of(p))).collect())
        } else {
            self.index.search.get()?
        };
        m.get(&p.id).map(|f| &**f)
    }

    /// `p`'s lower-cased, trimmed Text-rule text when `p` is this catalog's own photo, kept for
    /// every photo from the first ask (a smart album's count or view asks photo by photo).
    pub(crate) fn rule_text(&self, p: &crate::Photo) -> Option<&str> {
        if !self.is_own(p) {
            return None;
        }
        let m = self.index.rule_text.get_or_init(|| self.photos.values().map(|p| (p.id, crate::rules::rule_text_of(p).into())).collect());
        m.get(&p.id).map(|t| &**t)
    }

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

    fn file_index(&self, key: fn(&str) -> Option<String>) -> HashMap<String, HashSet<PhotoId>> {
        let mut m: HashMap<String, HashSet<PhotoId>> = HashMap::new();
        for p in self.photos.values() {
            if let Some(k) = file_path(&p.source).and_then(key) {
                m.entry(k).or_default().insert(p.id);
            }
        }
        m
    }

    /// The photos whose file is directly in the folder with key `folder_key` (from
    /// [`photo_folder_index_key`] or [`folder_index_key`]), whatever their state, sorted by id.
    /// The grouping ignores letter case: callers compare exact paths themselves. The first query
    /// builds the index in one pass over the library.
    pub fn photos_in_folder_any_case(&self, folder_key: &str) -> Vec<PhotoId> {
        sorted(self.index.folders.get_or_init(|| self.file_index(photo_folder_index_key)).get(folder_key))
    }

    /// The photos whose file is named `name_lower` (lower-cased, as [`file_name_index_key`] gives),
    /// whatever their state, sorted by id. Callers compare exact names themselves.
    pub fn photos_named_any_case(&self, name_lower: &str) -> Vec<PhotoId> {
        sorted(self.index.names.get_or_init(|| self.file_index(file_name_index_key)).get(name_lower))
    }

    /// The library's keywords, by the index (built on first use).
    pub(crate) fn keyword_index(&self) -> &crate::keywords::KeywordIndex {
        self.index.keywords.get_or_init(|| crate::keywords::KeywordIndex::build(self))
    }

    /// The keyword tree, by the index (built by `build` on first use).
    pub(crate) fn cached_keyword_tree(
        &self,
        build: impl FnOnce() -> std::sync::Arc<Vec<crate::keywords::KeywordNode>>,
    ) -> std::sync::Arc<Vec<crate::keywords::KeywordNode>> {
        self.index.keyword_tree.get_or_init(build).clone()
    }

    /// Content → its photos, by the index (built on first use).
    fn contents(&self) -> &HashMap<String, Vec<PhotoId>> {
        self.index.contents.get_or_init(|| {
            let mut m: HashMap<String, Vec<PhotoId>> = HashMap::with_capacity(self.photos.len());
            for p in self.photos.values() {
                if let Some(k) = content_key(p) {
                    m.entry(k.to_string()).or_default().push(p.id);
                }
            }
            m
        })
    }

    /// Whether another photo of the library has the same content as `p` ([`content_key`]).
    pub fn has_same_content(&self, p: &Photo) -> bool {
        content_key(p).is_some_and(|k| self.contents().get(k).is_some_and(|ids| ids.iter().any(|id| *id != p.id)))
    }

    /// Whether ops have to follow photos' contents ([`Catalog::content_moved`]): the index was built (a rule
    /// asked), or a smart album asks (whose count may be followed from a copy of the catalog that built its own).
    /// Else nothing has asked, and a library that never does carries no index.
    pub(crate) fn follows_contents(&self) -> bool {
        self.index.contents.get().is_some()
            || *self.index.contents_asked.get_or_init(|| {
                self.albums.values().any(|a| a.smart.as_ref().and_then(|f| f.rule_set.as_ref()).is_some_and(|r| r.reads("sameContent")))
            })
    }

    /// The content photo `id` stands for now ([`content_key`]).
    pub(crate) fn content_of(&self, id: PhotoId) -> Option<String> {
        self.photos.get(&id).and_then(|p| content_key(p)).map(str::to_string)
    }

    /// The op just applied took photo `id` from content `old` to `new` ([`Catalog::follows_contents`]): the index
    /// follows, and the photos with either content are touched too ([`Catalog::touched_since`]), as whether they
    /// have the same content as another photo may have changed with it.
    pub(crate) fn content_moved(&mut self, id: PhotoId, old: Option<String>, new: Option<String>) {
        if old == new {
            return;
        }
        let built = self.index.contents.get().is_some();
        // (built now if it wasn't: from the catalog as it is, this op included, which holds the same other photos)
        let twins: Vec<PhotoId> =
            [&old, &new].into_iter().flatten().filter_map(|k| self.contents().get(k.as_str())).flatten().copied().filter(|p| *p != id).collect();
        if built {
            self.index.content_changed(id, old.as_deref(), new);
        }
        for p in twins {
            self.touch(p);
        }
    }
}
