//! Facts about the selection that enablement checks and panels ask every frame.
//!
//! Walking a big selection photo by photo costs milliseconds per thousand photos, so a menu that
//! asks "is any selected photo in a stack?" for every command would stall a 300,000-photo
//! selection on every frame. The answers are worked out once for a given selection and catalog
//! revision, and only when asked, stopping at the first photo that settles them.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use lightcraft_catalog::PhotoId;

use crate::{Selection, SelectionState, Session};

/// Selections shorter than this are fingerprinted on every ask (microseconds); longer ones are
/// fingerprinted once per frame ([`Session::begin_frame`]) and between frames only when a command ran
/// or the list's length or ends changed.
const FINGERPRINT_EVERY_ASK: usize = 1024;

/// What a [`SelectionFacts`] was worked out for: the exact ids (their fingerprint), the active photo
/// (it stands in for an empty selection) and the catalog as it was.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    ids: Fp,
    active: Option<PhotoId>,
    revision: u64,
}

/// What names a list of ids: its length (so no two lengths share one, whatever the hash) and its hash.
type Fp = (usize, u64);

/// A fast, order-sensitive, non-cryptographic hash of the ids (Fx-style).
fn fingerprint(ids: &[PhotoId]) -> u64 {
    ids.iter().fold(ids.len() as u64, |h, id| (h.rotate_left(5) ^ id.0).wrapping_mul(0x517c_c1b7_2722_0a95))
}

/// The last fingerprint of `Selection::ids`, and what it was for.
struct Memo {
    len: usize,
    first: Option<PhotoId>,
    last: Option<PhotoId>,
    hash: u64,
    /// Not worked out again until the frame ends, a command runs or `len`, `first` or `last` differ.
    trusted: bool,
}

/// What [`Session`] keeps about the selection between asks; the selection itself stays a plain
/// `ids` list that anything may change.
#[derive(Default)]
pub(crate) struct SelectionCache {
    memo: Option<Memo>,
    index: Option<Arc<SelectionIndex>>,
    facts: Option<Arc<SelectionFacts>>,
}

impl SelectionCache {
    fn fingerprint(&mut self, sel: &Selection) -> Fp {
        let ids = sel.ids.as_slice();
        let (first, last) = (ids.first().copied(), ids.last().copied());
        if let Some(m) = &self.memo
            && m.trusted
            && (m.len, m.first, m.last) == (ids.len(), first, last)
        {
            return (m.len, m.hash);
        }
        let hash = fingerprint(ids);
        self.memo = Some(Memo { len: ids.len(), first, last, hash, trusted: ids.len() >= FINGERPRINT_EVERY_ASK });
        (ids.len(), hash)
    }
}

/// The selected photos as a set, for a whole frame of asks (a grid draws hundreds of cells).
pub struct SelectionIndex {
    ids: Fp,
    members: OnceLock<HashSet<PhotoId>>,
}

/// The selection with its membership index: `contains` and `state_of` in constant time.
pub struct SelectionView<'a> {
    sel: &'a Selection,
    index: Arc<SelectionIndex>,
}

impl SelectionView<'_> {
    pub fn contains(&self, id: PhotoId) -> bool {
        self.index.members.get_or_init(|| self.sel.ids.iter().copied().collect()).contains(&id)
    }

    /// [`Selection::state_of`], without walking the ids.
    pub fn state_of(&self, id: PhotoId) -> SelectionState {
        if self.sel.active == Some(id) {
            SelectionState::Active
        } else if self.contains(id) {
            SelectionState::Selected
        } else {
            SelectionState::NotSelected
        }
    }
}

/// Answers about the photos a command acts on when a call names none: the selection, else the
/// active photo ([`Session::selected_ids`]).
pub struct SelectionFacts {
    key: Key,
    in_library: OnceLock<bool>,
    stacked: OnceLock<bool>,
    outdated: OnceLock<bool>,
    deleted: OnceLock<bool>,
}

impl SelectionFacts {
    fn new(key: Key) -> Self {
        SelectionFacts { key, in_library: OnceLock::new(), stacked: OnceLock::new(), outdated: OnceLock::new(), deleted: OnceLock::new() }
    }

    /// Some photo is in the library: not a browsed (Local) one. A photo the catalog doesn't have counts as in the library.
    pub fn any_in_library(&self, s: &Session) -> bool {
        *self.in_library.get_or_init(|| s.selected_ids().iter().any(|id| s.catalog.photo(*id).is_none_or(|p| !p.local)))
    }

    /// Some photo is in a stack.
    pub fn any_stacked(&self, s: &Session) -> bool {
        *self.stacked.get_or_init(|| {
            if s.selection.ids.is_empty() {
                s.selection.active.is_some_and(|id| s.catalog.stack_of(id).is_some())
            } else if s.selection.ids.len() <= s.catalog.stacks().count() {
                s.selection.ids.iter().any(|id| s.catalog.stack_of(*id).is_some())
            } else {
                // more selected photos than stacks: ask the selection's index about each stacked photo
                let view = s.selection_view();
                s.catalog.stacks().any(|st| st.photos.iter().any(|p| view.contains(*p)))
            }
        })
    }

    /// Some photo is on an older rendering process.
    pub fn any_outdated_process(&self, s: &Session) -> bool {
        *self.outdated.get_or_init(|| s.selected_ids().iter().any(|id| s.catalog.photo(*id).is_some_and(|p| p.develop.process.is_outdated())))
    }

    /// Some selected photo, or the active one, is in Recently Deleted.
    pub fn any_deleted(&self, s: &Session) -> bool {
        *self
            .deleted
            .get_or_init(|| s.selection.ids.iter().copied().chain(s.selection.active).any(|id| s.catalog.photo(id).is_some_and(|p| p.deleted)))
    }
}

impl Session {
    /// The photos commands act on when a call names none, without copying: the selection, else the active photo.
    pub fn selected_ids(&self) -> &[PhotoId] {
        if self.selection.ids.is_empty() { self.selection.active.as_slice() } else { &self.selection.ids }
    }

    /// How many photos [`Self::selected_ids`] has.
    pub fn selected_count(&self) -> usize {
        self.selected_ids().len()
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, SelectionCache> {
        self.selection_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts a frame: a long selection is fingerprinted again at the next ask, so changes made
    /// between frames by anything (the UI sets `selection.ids` directly) are seen.
    pub fn begin_frame(&self) {
        if let Some(m) = &mut self.cache().memo {
            m.trusted = false;
        }
    }

    /// Names the photos [`Self::selected_ids`] has (the active photo only when it stands in for an empty selection):
    /// another value after any change to them, so a cache keyed on it can't go stale.
    pub fn selection_key(&self) -> (Fp, Option<PhotoId>) {
        let ids = self.cache().fingerprint(&self.selection);
        (ids, if self.selection.ids.is_empty() { self.selection.active } else { None })
    }

    /// The selection with a membership index, rebuilt only when the selected ids changed.
    pub fn selection_view(&self) -> SelectionView<'_> {
        let mut c = self.cache();
        let ids = c.fingerprint(&self.selection);
        let index = match &c.index {
            Some(i) if i.ids == ids => i.clone(),
            _ => {
                let i = Arc::new(SelectionIndex { ids, members: OnceLock::new() });
                c.index = Some(i.clone());
                i
            }
        };
        SelectionView { sel: &self.selection, index }
    }

    /// The facts about the selection, shared while neither it nor the catalog changes.
    pub fn selection_facts(&self) -> Arc<SelectionFacts> {
        let mut c = self.cache();
        let key = Key { ids: c.fingerprint(&self.selection), active: self.selection.active, revision: self.catalog.revision };
        match &c.facts {
            Some(f) if f.key == key => f.clone(),
            _ => {
                let f = Arc::new(SelectionFacts::new(key));
                c.facts = Some(f.clone());
                f
            }
        }
    }
}
