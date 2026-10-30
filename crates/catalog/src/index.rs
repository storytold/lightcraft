//! Lookups derived from the catalog's own data, so questions asked once per photo of a big library
//! don't scan a list each time: which stack a photo is in, whether a regular album holds it, which
//! photos have the same content.
//!
//! They are never saved (the on-disk format is unchanged). Each is built on first use from what
//! the catalog holds, so a catalog read from a snapshot, replayed from the journal or cloned starts
//! without them and builds them when asked. Once built, [`crate::Catalog::apply`] keeps them up to
//! date: the ops that change stacks or album photos (`AddStack`, `RemoveStack`, `SetStack`,
//! `AddAlbum`, `RemoveAlbum`, `SetAlbumPhotos`) update them as they apply, inverses and the
//! rollback of a failed batch included, as those are ops too. The contents index is kept up to
//! date, and the photos with a photo's content touched, by the ops that change a photo's content
//! as the library sees it, once a rule has built it or while a smart album asks about contents
//! ([`Catalog::follows_contents`], [`Catalog::content_moved`]).

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
    /// Content ([`content_key`]) → the photos that have it.
    contents: OnceLock<HashMap<String, Vec<PhotoId>>>,
    /// Whether a smart album asks whether a photo has the same content as another ([`Catalog::follows_contents`]).
    contents_asked: OnceLock<bool>,
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
