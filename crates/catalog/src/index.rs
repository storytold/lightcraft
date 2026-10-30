//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it, the lower-cased text
//! the free-text search reads, and the text a smart album's Text rule reads (each made for every photo the first time it is needed for every photo).
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) update them as they apply, inverses and the
//! rollback of a failed batch included, as those are ops too. The two texts follow every op that changes
//! a photo's fields, and the photo moving to another place in memory.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::{AlbumId, Catalog, Photo, PhotoId, StackId};

#[derive(Default)]
pub(crate) struct Indexes {
    /// Photo → the stack it is in; `None` when the catalog was read with a photo in two stacks
    /// (only a hand-edited file can hold that: ops refuse it), where a scan says which one counts.
    stacks: OnceLock<Option<HashMap<PhotoId, StackId>>>,
    /// Album → its photos (regular albums; a smart album's list is empty).
    albums: OnceLock<HashMap<AlbumId, HashSet<PhotoId>>>,
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
        f.debug_struct("Indexes").field("stacks", &stacks).field("albums", &self.albums.get().map(HashMap::len)).finish()
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
/// until `Arc::make_mut` copies one that others hold, which `photo_mut` reports ([`Indexes::text_moved`]);
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
}
