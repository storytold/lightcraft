//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it, which
//! photos have their file in a folder, which have a file of a given name, which photos have the same
//! content, the lower-cased text the free-text search reads, and the text a smart album's Text rule
//! reads (each made for every photo the first time it is needed for every photo).
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) and those that add, remove or point a photo at
//! another file (`AddPhoto`, `RemovePhoto`, `Relink`, `SetFile`) update them as they apply,
//! inverses and the rollback of a failed batch included, as those are ops too. The contents index
//! is kept up to date, and the photos with a photo's content touched, by the ops that change a
//! photo's content as the library sees it, once a rule has built it or while a smart album asks
//! about contents ([`Catalog::follows_contents`], [`Catalog::content_moved`]).
//! The two texts follow every op that changes a photo's fields, and the photo moving to another place in memory.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use crate::{AlbumId, Catalog, Photo, PhotoId, Source, StackId};

#[derive(Default)]
pub(crate) struct Indexes {
    /// Photo → the stack it is in; `None` when the catalog was read with a photo in two stacks
    /// (only a hand-edited file can hold that: ops refuse it), where a scan says which one counts.
    stacks: OnceLock<Option<HashMap<PhotoId, StackId>>>,
    /// Album → its photos (regular albums; a smart album's list is empty).
    albums: OnceLock<HashMap<AlbumId, HashSet<PhotoId>>>,
    /// Content ([`content_key`]) → the photos that have it.
    contents: OnceLock<HashMap<String, Vec<PhotoId>>>,
    /// Whether a smart album asks whether a photo has the same content as another ([`Catalog::follows_contents`]).
    contents_asked: OnceLock<bool>,
    /// Folder (its [`photo_folder_index_key`], lower-cased) → the photos whose file is directly in
    /// it, whatever their state (in the library, in Recently Deleted, browsed in Local). Demo
    /// photos have none.
    folders: OnceLock<HashMap<String, HashSet<PhotoId>>>,
    /// File name, lower-cased ([`file_name_index_key`]) → the photos whose file has it (same
    /// states as `folders`).
    names: OnceLock<HashMap<String, HashSet<PhotoId>>>,
    /// The smart albums on a loop (see [`crate::Catalog::album_on_a_loop`]). Built from the albums'
    /// rules, so forgotten whenever an album is added or removed or its rules change (and by no
    /// other op: photos never change which album tests which).
    looping: OnceLock<BTreeSet<AlbumId>>,
    /// Photo → its lower-cased searchable text ([`PhotoText`]), for the free-text search.
    search: OnceLock<ByPhoto<PhotoText>>,
    /// Photo → the lower-cased text a smart album's Text rule looks through ([`crate::rules::rule_text`]).
    rule_text: OnceLock<ByPhoto<RuleText>>,
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
            .finish()
    }
}

impl Indexes {
    /// An op is about to add or remove an album or change its rules (the undo of any of them is
    /// one of those too): the albums on a loop are worked out again when asked.
    pub(crate) fn forget_loops(&mut self) {
        self.looping.take();
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

    /// Photo `id` is `p` now (`None`: it is gone): the text kept for it is made again.
    pub(crate) fn text_changed(&mut self, id: PhotoId, p: Option<&Photo>) {
        if let Some(m) = self.search.get_mut() {
            match p {
                Some(p) => m.insert(id, PhotoText::of(p)),
                None => m.remove(&id),
            };
        }
        if let Some(m) = self.rule_text.get_mut() {
            match p {
                Some(p) => m.insert(id, RuleText::of(p)),
                None => m.remove(&id),
            };
        }
    }

    /// Photo `id` is about to be changed in place, at `p`: an edit of a photo that others hold
    /// (`Arc::make_mut`) puts it at another address, which its text follows.
    pub(crate) fn text_moved(&mut self, id: PhotoId, p: &Photo) {
        let at = addr(p);
        if let Some(t) = self.search.get_mut().and_then(|m| m.get_mut(&id)) {
            t.at = at;
        }
        if let Some(t) = self.rule_text.get_mut().and_then(|m| m.get_mut(&id)) {
            t.at = at;
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
}

/// A map from photos, for a pass over the library that asks about each one in id order.
type ByPhoto<T> = HashMap<PhotoId, T, std::hash::BuildHasherDefault<IdHasher>>;

/// Hashes a photo id as itself in the table's low bits, which is where the table looks first, so
/// ids that follow each other sit next to each other and a pass in id order walks it in order
/// rather than jumping about (a pass over 300 000 photos spent most of its time on that). The top
/// bits, which tag an entry in the table, are mixed so entries still tell each other apart.
#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) & 0xFE00_0000_0000_0000)
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// Where `p` is in memory. The contract of the kept texts: a text is used only for the photo at the
/// address it was made for. The catalog's own photos (`Arc<Photo>` contents) stay where they are
/// until `Arc::make_mut` copies one that others hold, which `photo_mut` reports ([`Indexes::text_moved`]; `repair`
/// copies them in bulk and drops every lookup instead);
/// every op that changes a searched field makes the text again ([`Indexes::text_changed`]). A copy
/// of a photo (a candidate tried with other values) is at another address, so it is read from its
/// own fields, never from a text kept for the photo with its id. Only ids and addresses are
/// compared; the address is never turned back into a reference. A catalog read from a snapshot
/// (and then repaired) or cloned starts with no texts at all: they are made when asked.
fn addr(p: &Photo) -> usize {
    std::ptr::from_ref(p).addr()
}

/// What the free-text search looks through for one photo, lower-cased as `str::to_lowercase` does
/// for each field alone: file name, title, caption, camera, lens, location, format, then each
/// keyword, then the name of each person (a named face), separated by NUL. A search word may hold
/// white space (a quoted phrase); one that holds NUL is read from the fields one by one
/// ([`PhotoText::field_contains`]), so no word runs over a field.
pub(crate) struct PhotoText {
    /// The address of the photo this was made from ([`addr`]).
    at: usize,
    text: Box<str>,
}

impl PhotoText {
    /// The fields of `p` that the text joins before its keywords and people.
    fn fields(p: &Photo) -> [&String; 7] {
        [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format]
    }

    pub(crate) fn of(p: &Photo) -> PhotoText {
        let fields = Self::fields(p);
        let people = p.people();
        // (exact for ASCII, which most text is: the box takes the buffer without copying it)
        let len = fields
            .iter()
            .map(|f| f.len() + 1)
            .chain(p.meta.keywords.iter().map(|k| k.len() + 1))
            .chain(people.iter().map(|n| n.len() + 1))
            .sum::<usize>()
            .saturating_sub(1);
        let mut text = String::with_capacity(len);
        for (i, field) in fields.into_iter().enumerate() {
            if i > 0 {
                text.push('\0');
            }
            push_lowercase(&mut text, field);
        }
        for k in p.meta.keywords.iter().map(String::as_str).chain(people) {
            text.push('\0');
            push_lowercase(&mut text, k);
        }
        PhotoText { at: addr(p), text: text.into_boxed_str() }
    }

    /// Whether the lower-cased word `t` is in the file name, title, caption, camera, lens,
    /// location, format, a keyword or the name of a person.
    pub(crate) fn contains(&self, t: &str) -> bool {
        self.text.contains(t)
    }

    /// [`PhotoText::contains`] read from `p`'s fields one by one, for a word that holds NUL: the joined text can't tell
    /// the NUL that separates two fields from one inside a field, and a word never runs over two fields.
    pub(crate) fn field_contains(p: &Photo, t: &str) -> bool {
        Self::fields(p)
            .into_iter()
            .map(String::as_str)
            .chain(p.meta.keywords.iter().map(String::as_str))
            .chain(p.people())
            .any(|f| f.to_lowercase().contains(t))
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

/// Push `s` lower-cased as `str::to_lowercase` gives it, without a temporary string for ASCII.
fn push_lowercase(out: &mut String, s: &str) {
    if s.is_ascii() {
        let from = out.len();
        out.push_str(s);
        if let Some(added) = out.get_mut(from..) {
            added.make_ascii_lowercase();
        }
    } else {
        out.push_str(&s.to_lowercase());
    }
}

/// A photo's text for a smart album's Text rule: [`crate::rules::rule_text`].
struct RuleText {
    at: usize,
    text: Box<str>,
}

impl RuleText {
    fn of(p: &Photo) -> RuleText {
        RuleText { at: addr(p), text: crate::rules::rule_text(p).into_boxed_str() }
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
    /// The smart albums that lead back to themselves through the albums they test, worked out once
    /// (by [`crate::AlbumGraph`]) and kept until an album is added, removed or has its rules changed.
    pub(crate) fn looping(&self) -> &BTreeSet<AlbumId> {
        self.index.looping.get_or_init(|| crate::AlbumGraph::of(self.albums.values()).on_a_loop())
    }

    /// Whether the albums on a loop are worked out and kept now (what the tests look at).
    #[cfg(test)]
    pub(crate) fn loops_known(&self) -> bool {
        self.index.looping.get().is_some()
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

    /// `p`'s lower-cased text for the free-text search, when `p` is this catalog's own photo and the
    /// text was made. `build`: make it for every photo first when it wasn't (a pass over the whole
    /// library does; testing one photo shouldn't build it for 300 000).
    pub(crate) fn search_text(&self, p: &Photo, build: bool) -> Option<&PhotoText> {
        let m = if build {
            self.index.search.get_or_init(|| self.photos.values().map(|p| (p.id, PhotoText::of(p))).collect())
        } else {
            self.index.search.get()?
        };
        m.get(&p.id).filter(|t| t.at == addr(p))
    }

    /// [`Catalog::search_text`] for a smart album's Text rule: the lower-cased, trimmed text of
    /// [`crate::rules::rule_text`], made for every photo on first use (a smart album's count or
    /// view asks for it photo by photo).
    pub(crate) fn rule_text_of(&self, p: &Photo) -> Option<&str> {
        let m = self.index.rule_text.get_or_init(|| self.photos.values().map(|p| (p.id, RuleText::of(p))).collect());
        let t = m.get(&p.id)?;
        (t.at == addr(p)).then_some(&*t.text)
    }

    /// Whether the Text rules' text has been made (asking for it makes it, so a test cannot look with
    /// [`Catalog::rule_text_of`]).
    #[cfg(test)]
    pub(crate) fn rule_text_made(&self) -> bool {
        self.index.rule_text.get().is_some()
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
}
