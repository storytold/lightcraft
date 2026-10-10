//! The LightCraft library (catalog).
//!
//! State changes only through [`Op`]s. [`Catalog::apply`] returns the inverse op, which gives:
//! - **persistence**: ops are appended to a log (JSON lines) and replayed on load after the last
//!   snapshot — crash-safe and diff-friendly (see [`journal`]);
//! - **undo/redo**: the engine keeps inverse ops;
//! - **determinism**: replaying the log reproduces the state exactly (property-tested).
//!
//! **Catalog format version** ([`journal::VERSION`], see [`journal`] → *Format versions*): adding
//! an [`Op`] variant or a serialized field means bumping it. Newer builds read every older format
//! (and upgrade it on open); older builds refuse a newer library with [`CatalogError::Newer`]
//! instead of reading part of it.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod dates;
pub mod folders;
pub mod journal;
pub mod keywords;
pub mod local;
pub mod lock;
pub mod model;
pub mod query;
pub mod rules;
pub mod safe_file;
pub mod stacks;
pub mod store;

use std::collections::BTreeMap;
use std::sync::Arc;

pub use dates::{DateRun, GroupBy};
pub use folders::{FolderNode, FolderRecord};
pub use journal::{Journal, LoadReport, PersistStats, SnapshotPolicy, SnapshotTiming};
pub use keywords::KeywordNode;
use lightcraft_develop::DevelopSettings;
pub use local::{DEFAULT_FORGET_DAYS, ForgetPlan, folder_of};
pub use lock::{LibraryLock, LockError, LockOwner};
pub use model::*;
pub use query::{DateGroup, Filter, Person, RatingOp, Sort, SortKey, mix64};
pub use rules::{Match, Rule, RuleSet};
use serde::{Deserialize, Serialize};
pub use store::{FsStore, MemStore, Store};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CatalogError {
    #[error("no such photo {0:?}")]
    NoPhoto(PhotoId),
    #[error("no such album {0:?}")]
    NoAlbum(AlbumId),
    #[error("no such stack {0:?}")]
    NoStack(StackId),
    #[error("invalid: {0}")]
    Invalid(String),
    /// A keyword of that name is there already: moving or renaming onto it would merge the two.
    #[error("there is a keyword “{0}” already")]
    KeywordExists(String),
    #[error("corrupt catalog data: {0}")]
    Corrupt(String),
    #[error("catalog storage: {0}")]
    Io(String),
    /// The library was written by a newer LightCraft (a newer catalog format, or a change this
    /// version doesn't know). Nothing was read into the session and nothing was modified.
    #[error("this library was written by a newer version of LightCraft ({0}); update LightCraft to open it. The library was left unchanged.")]
    Newer(String),
}

pub type Result<T> = std::result::Result<T, CatalogError>;

/// Maximum History entries kept per photo.
pub const HISTORY_LIMIT: usize = 200;

/// Every catalog mutation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    AddPhoto {
        photo: Box<Photo>,
    },
    RemovePhoto {
        id: PhotoId,
    },
    SetRating {
        id: PhotoId,
        rating: u8,
    },
    SetFlag {
        id: PhotoId,
        flag: Flag,
    },
    SetLabel {
        id: PhotoId,
        label: Option<ColorLabel>,
    },
    SetDevelop {
        id: PhotoId,
        settings: Arc<DevelopSettings>,
        label: String,
        edited: Option<String>,
    },
    SetMeta {
        id: PhotoId,
        meta: Box<Meta>,
    },
    SetDeleted {
        id: PhotoId,
        deleted: bool,
    },
    /// Browsed-only (`true`) or part of the library.
    SetLocal {
        id: PhotoId,
        local: bool,
    },
    SetVersions {
        id: PhotoId,
        versions: Vec<Version>,
    },
    SetHistory {
        id: PhotoId,
        history: Vec<HistoryStep>,
    },
    /// Append one History entry (dropping the oldest beyond [`HISTORY_LIMIT`]). Logged instead of
    /// a full `SetHistory` so each edit costs one step in the op log.
    PushHistory {
        id: PhotoId,
        step: HistoryStep,
    },
    AddAlbum {
        album: Album,
    },
    RemoveAlbum {
        id: AlbumId,
    },
    RenameAlbum {
        id: AlbumId,
        name: String,
    },
    MoveAlbum {
        id: AlbumId,
        parent: Option<AlbumId>,
    },
    /// Where the album stands among its siblings (`None`: by name). Format version 3.
    SetAlbumOrder {
        id: AlbumId,
        order: Option<u32>,
    },
    SetAlbumPhotos {
        id: AlbumId,
        photos: Vec<PhotoId>,
    },
    SetAlbumCover {
        id: AlbumId,
        cover: Option<PhotoId>,
    },
    /// Replace a smart album's rules.
    SetAlbumRules {
        id: AlbumId,
        rules: Box<Filter>,
    },
    AddStack {
        stack: Stack,
    },
    RemoveStack {
        id: StackId,
    },
    /// Replace a stack's members (first = top) and collapsed state.
    SetStack {
        id: StackId,
        photos: Vec<PhotoId>,
        collapsed: bool,
    },
    /// Set a photo's capture time (ISO 8601 local time; `None` = unknown).
    SetCaptured {
        id: PhotoId,
        captured: Option<String>,
    },
    /// Assisted culling scores.
    SetAnalysis {
        id: PhotoId,
        analysis: Option<crate::Analysis>,
    },
    /// Rename a photo: its file name and the source it points to. Applying the op never touches
    /// the disk — the engine moves the file before it commits (and on undo/redo).
    SetFile {
        id: PhotoId,
        file_name: String,
        source: Source,
    },
    /// Point a photo at its file's new location (a moved or renamed original found again). Unlike
    /// [`Op::SetFile`], undo and redo never move files.
    Relink {
        id: PhotoId,
        file_name: String,
        source: Source,
        /// The file's format when it changes too (Convert to DNG).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    /// What a photo's file is now (after it changed on disk: Reload).
    SetContent {
        id: PhotoId,
        width: u32,
        height: u32,
        file_size: u64,
        #[serde(default)]
        content_hash: Option<String>,
        /// Why the file can only be shown from its embedded preview (see [`Photo::preview_only`]);
        /// `None` = its raw data decodes (or it isn't a raw).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview_only: Option<String>,
    },
    /// The lens data a photo's file carries (what Reload finds when it was read after import).
    SetEmbeddedLens {
        id: PhotoId,
        lens: Option<Box<lightcraft_develop::EmbeddedLens>>,
    },
    /// The name shown for a colour label (`None` = its colour's name).
    SetLabelName {
        label: ColorLabel,
        name: Option<String>,
    },
    /// When a Local folder was last browsed (ISO 8601; `None` = forget the time). Not an undo
    /// step: it drives forgetting untouched Local records (see [`local`]).
    SetBrowsed {
        folder: String,
        at: Option<String>,
    },
    /// List a keyword in the library's keyword list with these attributes, or take it off the list
    /// (`None`; photos that carry it keep it). Format version 4.
    SetKeyword {
        path: String,
        info: Option<keywords::KeywordInfo>,
    },
    /// What the library keeps about one of its folders (see [`folders::FolderRecord`]), under
    /// its identity ([`query::folder_key`]); `None` or an empty record = keep nothing.
    SetFolderRecord {
        folder: String,
        record: Option<FolderRecord>,
    },
    /// Several ops as one step (undo applies the inverses in reverse).
    Batch {
        ops: Vec<Op>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    photos: BTreeMap<PhotoId, Arc<Photo>>,
    albums: BTreeMap<AlbumId, Album>,
    next_photo: u64,
    next_album: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    stacks: BTreeMap<StackId, Stack>,
    #[serde(default)]
    next_stack: u64,
    /// Custom colour label names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    label_names: BTreeMap<ColorLabel, String>,
    /// When each Local folder was last browsed (folder path → ISO 8601), see [`local`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    browsed: BTreeMap<String, String>,
    /// Keywords listed on their own or given attributes, by lower-case path (see [`keywords`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    keyword_list: BTreeMap<String, keywords::ListedKeyword>,
    /// What the library keeps about its folders (folder identity → record), see [`folders`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    folder_records: BTreeMap<String, FolderRecord>,
    /// Increments on every applied op.
    #[serde(skip)]
    pub revision: u64,
}

impl Catalog {
    pub fn new() -> Catalog {
        Catalog { next_photo: 1, next_album: 1, next_stack: 1, ..Default::default() }
    }

    // ---- ids

    pub fn alloc_photo_id(&mut self) -> PhotoId {
        let id = PhotoId(self.next_photo.max(1));
        self.next_photo = id.0 + 1;
        id
    }
    pub fn alloc_album_id(&mut self) -> AlbumId {
        let id = AlbumId(self.next_album.max(1));
        self.next_album = id.0 + 1;
        id
    }
    pub fn alloc_stack_id(&mut self) -> StackId {
        let id = StackId(self.next_stack.max(1));
        self.next_stack = id.0 + 1;
        id
    }

    // ---- reads

    pub fn photo(&self, id: PhotoId) -> Option<&Arc<Photo>> {
        self.photos.get(&id)
    }
    pub fn photos(&self) -> impl Iterator<Item = &Arc<Photo>> {
        self.photos.values()
    }
    pub fn len(&self) -> usize {
        self.photos.len()
    }
    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }
    pub fn album(&self, id: AlbumId) -> Option<&Album> {
        self.albums.get(&id)
    }
    pub fn albums(&self) -> impl Iterator<Item = &Album> {
        self.albums.values()
    }
    /// The albums and folders directly inside `parent` (`None`: the top level), in the order the
    /// sidebar lists them: folders first, then by their place if the user ordered them (those
    /// without one after, by name), else by name.
    pub fn album_children(&self, parent: Option<AlbumId>) -> Vec<&Album> {
        let mut kids: Vec<&Album> = self.albums.values().filter(|a| a.parent == parent).collect();
        kids.sort_by_cached_key(|a| album_order_key(a));
        kids
    }
    /// [`Self::album_children`] of every folder (and of the top level, under `None`) in one pass:
    /// what a tree view that draws all of them wants each frame.
    pub fn album_children_by_parent(&self) -> std::collections::HashMap<Option<AlbumId>, Vec<&Album>> {
        let mut map: std::collections::HashMap<Option<AlbumId>, Vec<&Album>> = std::collections::HashMap::new();
        for a in self.albums.values() {
            map.entry(a.parent).or_default().push(a);
        }
        map.values_mut().for_each(|kids| kids.sort_by_cached_key(|a| album_order_key(a)));
        map
    }
    /// Whether anything inside `parent` has a place of its own (the folder is ordered by hand).
    pub fn album_children_are_ordered(&self, parent: Option<AlbumId>) -> bool {
        self.albums.values().any(|a| a.parent == parent && a.order.is_some())
    }
    /// The Quick Collection, once something was added to it.
    pub fn quick_collection(&self) -> Option<AlbumId> {
        self.albums.values().find(|a| a.quick).map(|a| a.id)
    }

    /// Albums (regular and smart) that contain the photo; not the folders those albums are in.
    pub fn albums_of(&self, id: PhotoId) -> Vec<AlbumId> {
        let Some(p) = self.photos.get(&id) else { return Vec::new() };
        self.albums.values().filter(|a| !a.folder && self.album_contains(a.id, p)).map(|a| a.id).collect()
    }

    /// Whether album `id` contains `p` (a smart album evaluates its rules; deleted photos are in
    /// no smart album). A folder contains what the albums inside it do ([`Self::album_members`]).
    pub fn album_contains(&self, id: AlbumId, p: &Photo) -> bool {
        match self.albums.get(&id) {
            // a folder first: one that also has rules (a damaged library) is still a folder.
            // Within a pass over the library its photos were gathered once; for one photo, no
            // list is built
            Some(a) if a.folder => match gathered(self, id, || self.folder_photos(id)) {
                Some(folder) => folder.holds(self, p),
                None => self.albums.values().any(|m| !m.folder && self.album_is_within(m, id) && self.album_contains(m.id, p)),
            },
            // guarded: a smart album testing smart albums can't loop or recurse without end
            Some(Album { smart: Some(rules), .. }) => !p.deleted && rules::smart_album_holds(id, p.id, || rules.matches(p, self)),
            Some(a) => a.photos.contains(&p.id),
            None => false,
        }
    }

    /// The albums and smart albums whose photos `id` shows: those inside a folder, at any depth
    /// (the folders themselves left out), or an album alone. Empty when there is no such album.
    pub fn album_members(&self, id: AlbumId) -> Vec<AlbumId> {
        match self.albums.get(&id) {
            Some(a) if a.folder => self.albums.values().filter(|m| !m.folder && self.album_is_within(m, id)).map(|m| m.id).collect(),
            Some(a) => vec![a.id],
            None => Vec::new(),
        }
    }

    /// Whether `album` sits inside `folder`, directly or through the folders between them, as the
    /// sidebar lists it: a parent that is no folder, or is gone, leads nowhere. Parents that lead
    /// round in a ring (a damaged library) end the search after one pass round it.
    fn album_is_within(&self, album: &Album, folder: AlbumId) -> bool {
        let mut parent = album.parent;
        for _ in 0..=self.albums.len() {
            match parent {
                Some(p) if p == folder => return true,
                Some(p) => match self.albums.get(&p) {
                    Some(between) if between.folder => parent = between.parent,
                    _ => return false,
                },
                None => return false,
            }
        }
        false
    }

    /// What folder `id` shows, gathered for a question about many photos.
    fn folder_photos(&self, id: AlbumId) -> FolderPhotos {
        let mut shown = FolderPhotos::default();
        for member in self.album_members(id).iter().filter_map(|m| self.albums.get(m)) {
            match member.smart {
                Some(_) => shown.smart.push(member.id),
                None => shown.listed.extend(member.photos.iter().copied()),
            }
        }
        shown
    }

    /// Whether smart album `from` tests `to`, directly or through the smart albums it tests and the
    /// folders they are in (the rules of `from` would then change when those of `to` do). Loops in
    /// saved rules end the search, they don't repeat it.
    pub fn album_reaches(&self, from: AlbumId, to: AlbumId) -> bool {
        self.album_search(self.album_leads_to(from), |a| a == to)
    }

    /// Whether a smart album with `rules` would include itself if it were in folder `parent`: its
    /// rules lead (as in [`Self::album_reaches`]) to that folder or to one around it.
    pub fn smart_album_would_loop_in(&self, rules: &Filter, parent: AlbumId) -> bool {
        let Some(inside) = self.albums.get(&parent) else { return false };
        self.album_search(rules.albums_tested(), |a| {
            self.albums.get(&a).is_some_and(|f| f.folder) && (a == parent || self.album_is_within(inside, a))
        })
    }

    /// The smart album that would include itself if album or folder `id` were moved into folder
    /// `parent`: `id` itself or, for a folder, one inside it ([`Self::smart_album_would_loop_in`]).
    /// `None` when the move makes no loop.
    pub fn album_move_would_loop(&self, id: AlbumId, parent: AlbumId) -> Option<AlbumId> {
        let moved = self.album_members(id);
        moved
            .into_iter()
            .find(|m| self.albums.get(m).and_then(|a| a.smart.as_deref()).is_some_and(|rules| self.smart_album_would_loop_in(rules, parent)))
    }

    /// The albums whose photos decide those of `a`: the albums a smart album tests, or the albums
    /// inside a folder.
    fn album_leads_to(&self, a: AlbumId) -> Vec<AlbumId> {
        match self.albums.get(&a) {
            Some(al) if al.folder => self.album_members(a),
            Some(al) => al.smart.as_deref().map(Filter::albums_tested).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Whether `found` holds for one of `start` or for an album they lead to
    /// ([`Self::album_leads_to`]); each album is looked at once.
    fn album_search(&self, start: Vec<AlbumId>, found: impl Fn(AlbumId) -> bool) -> bool {
        let mut seen: std::collections::HashSet<AlbumId> = std::collections::HashSet::new();
        let mut next = start;
        while let Some(a) = next.pop() {
            if !seen.insert(a) {
                continue;
            }
            if found(a) {
                return true;
            }
            next.extend(self.album_leads_to(a));
        }
        false
    }

    /// What is wrong with a saved smart album's rules now (`RuleSet::check`, after
    /// `RuleSet::upgrade`): typically a rule testing an album that has since been deleted. Empty
    /// for a sound smart album, a plain album or no album.
    pub fn smart_album_problems(&self, id: AlbumId) -> Vec<rules::Problem> {
        let Some(filter) = self.albums.get(&id).and_then(|a| a.smart.as_deref()) else { return Vec::new() };
        let mut out: Vec<rules::Problem> = self.album_filter_problem(filter, Some(id)).into_iter().collect();
        if let Some(mut rules) = filter.rule_set.clone() {
            rules.upgrade();
            out.extend(rules.check_for(self, Some(id)));
        }
        out
    }

    /// What is wrong with `filter`'s own album field (not its rules): it names an album that is
    /// gone (a view of an album, saved as a smart album, after that album was deleted), or, for
    /// smart album `owner`, itself or an album that leads back to it.
    pub fn album_filter_problem(&self, filter: &Filter, owner: Option<AlbumId>) -> Option<rules::Problem> {
        let a = filter.album?;
        let problem = |issue, message| rules::Problem { path: Vec::new(), field: Some("album".into()), issue, message };
        if !self.albums.contains_key(&a) {
            return Some(problem(rules::Issue::NoSuchAlbum, format!("its album filter names album {}, which is gone", a.0)));
        }
        let owner = owner?;
        (a == owner || self.album_reaches(a, owner))
            .then(|| problem(rules::Issue::AlbumLoop, format!("its album filter (album {}) would make this album include itself", a.0)))
    }

    /// The photos of an album: the stored list, or a smart album's current matches (id order). A
    /// folder's are those of the albums inside it, each once, without the deleted ones (id order).
    pub fn album_photos(&self, id: AlbumId) -> Vec<PhotoId> {
        match self.albums.get(&id) {
            Some(a) if a.folder => gathering(|| self.photos.values().filter(|p| !p.deleted && self.album_contains(id, p)).map(|p| p.id).collect()),
            Some(Album { smart: Some(_), .. }) => gathering(|| self.photos.values().filter(|p| self.album_contains(id, p)).map(|p| p.id).collect()),
            Some(a) => a.photos.clone(),
            None => Vec::new(),
        }
    }

    /// Number of photos shown for an album in the sources list (excludes deleted photos).
    pub fn album_count(&self, id: AlbumId) -> usize {
        match self.albums.get(&id) {
            Some(a) if a.folder => gathering(|| self.photos.values().filter(|p| !p.deleted && self.album_contains(id, p)).count()),
            // counted in place: no id list is built just for its length
            Some(Album { smart: Some(_), .. }) => gathering(|| self.photos.values().filter(|p| self.album_contains(id, p)).count()),
            Some(a) => a.photos.iter().filter(|p| self.photos.get(p).is_some_and(|p| !p.deleted)).count(),
            None => 0,
        }
    }

    /// Smart-album rules must not reference another smart album (no recursion) or deleted photos.
    fn validate_rules(&self, rules: &Filter) -> Result<()> {
        if rules.deleted {
            return Err(CatalogError::Invalid("smart album rules can't select deleted photos".into()));
        }
        if let Some(a) = rules.album
            && self.albums.get(&a).is_some_and(Album::is_smart)
        {
            return Err(CatalogError::Invalid("smart album rules can't reference another smart album".into()));
        }
        Ok(())
    }

    /// The name of a colour label: its custom name, else the colour (`Red`).
    pub fn label_name(&self, l: ColorLabel) -> String {
        self.label_names.get(&l).cloned().unwrap_or_else(|| format!("{l:?}"))
    }
    /// A listed keyword's attributes (any case), `None` when it isn't listed.
    pub fn keyword_info(&self, path: &str) -> Option<&keywords::KeywordInfo> {
        self.keyword_list.get(&keywords::clean(path).to_lowercase()).map(|k| &k.info)
    }
    /// How the keyword list spells a listed keyword (any case), `None` when it isn't listed.
    pub fn listed_path(&self, path: &str) -> Option<&str> {
        self.keyword_list.get(&keywords::clean(path).to_lowercase()).map(|k| k.path.as_str())
    }
    /// The keyword list: keywords listed on their own or given attributes, by path.
    pub fn listed_keywords(&self) -> impl Iterator<Item = &keywords::ListedKeyword> {
        self.keyword_list.values()
    }
    /// The custom name of a colour label, if any.
    pub fn custom_label_name(&self, l: ColorLabel) -> Option<&str> {
        self.label_names.get(&l).map(String::as_str)
    }
    /// The label a name stands for: a custom name first, then a colour's own name (any case).
    pub fn label_from_name(&self, name: &str) -> Option<ColorLabel> {
        let name = name.trim();
        // any case, accents included (Été / été), as rules compare names
        let name_lower = name.to_lowercase();
        self.label_names.iter().find(|(_, n)| n.trim().to_lowercase() == name_lower).map(|(l, _)| *l).or_else(|| ColorLabel::parse(name))
    }

    // ---- writes

    fn photo_mut(&mut self, id: PhotoId) -> Result<&mut Photo> {
        self.photos.get_mut(&id).map(Arc::make_mut).ok_or(CatalogError::NoPhoto(id))
    }
    fn album_mut(&mut self, id: AlbumId) -> Result<&mut Album> {
        self.albums.get_mut(&id).ok_or(CatalogError::NoAlbum(id))
    }

    /// Apply an op; returns its inverse. On error nothing changes.
    pub fn apply(&mut self, op: Op) -> Result<Op> {
        let inv = self.apply_inner(op)?;
        self.revision += 1;
        Ok(inv)
    }

    fn apply_inner(&mut self, op: Op) -> Result<Op> {
        Ok(match op {
            Op::AddPhoto { photo } => {
                if self.photos.contains_key(&photo.id) {
                    return Err(CatalogError::Invalid(format!("photo {:?} exists", photo.id)));
                }
                let id = photo.id;
                self.next_photo = self.next_photo.max(id.0 + 1);
                self.photos.insert(id, Arc::new(*photo));
                Op::RemovePhoto { id }
            }
            Op::RemovePhoto { id } => {
                let p = self.photos.remove(&id).ok_or(CatalogError::NoPhoto(id))?;
                // album membership is restored by the batch the engine builds (see `delete_permanently`)
                Op::AddPhoto { photo: Box::new((*p).clone()) }
            }
            Op::SetRating { id, rating } => {
                if rating > 5 {
                    return Err(CatalogError::Invalid("rating must be 0..=5".into()));
                }
                let p = self.photo_mut(id)?;
                let old = std::mem::replace(&mut p.rating, rating);
                Op::SetRating { id, rating: old }
            }
            Op::SetFlag { id, flag } => {
                let p = self.photo_mut(id)?;
                Op::SetFlag { id, flag: std::mem::replace(&mut p.flag, flag) }
            }
            Op::SetLabel { id, label } => {
                let p = self.photo_mut(id)?;
                Op::SetLabel { id, label: std::mem::replace(&mut p.label, label) }
            }
            Op::SetDevelop { id, settings, label, edited } => {
                let p = self.photo_mut(id)?;
                let old = std::mem::replace(&mut p.develop, settings);
                let old_edit = std::mem::replace(&mut p.edited, edited);
                Op::SetDevelop { id, settings: old, label, edited: old_edit }
            }
            Op::SetMeta { id, meta } => {
                let p = self.photo_mut(id)?;
                Op::SetMeta { id, meta: Box::new(std::mem::replace(&mut p.meta, *meta)) }
            }
            Op::SetDeleted { id, deleted } => {
                let p = self.photo_mut(id)?;
                Op::SetDeleted { id, deleted: std::mem::replace(&mut p.deleted, deleted) }
            }
            Op::SetLocal { id, local } => {
                let p = self.photo_mut(id)?;
                Op::SetLocal { id, local: std::mem::replace(&mut p.local, local) }
            }
            Op::SetVersions { id, versions } => {
                let p = self.photo_mut(id)?;
                Op::SetVersions { id, versions: std::mem::replace(&mut p.versions, versions) }
            }
            Op::SetHistory { id, history } => {
                let p = self.photo_mut(id)?;
                Op::SetHistory { id, history: std::mem::replace(&mut p.history, history) }
            }
            Op::PushHistory { id, step } => {
                let p = self.photo_mut(id)?;
                let old = p.history.clone();
                p.history.push(step);
                if p.history.len() > HISTORY_LIMIT {
                    let n = p.history.len() - HISTORY_LIMIT;
                    p.history.drain(..n);
                }
                Op::SetHistory { id, history: old }
            }
            Op::AddAlbum { album } => {
                if self.albums.contains_key(&album.id) {
                    return Err(CatalogError::Invalid(format!("album {:?} exists", album.id)));
                }
                if let Some(parent) = album.parent
                    && !self.albums.get(&parent).is_some_and(|a| a.folder)
                {
                    return Err(CatalogError::Invalid("parent must be an existing folder".into()));
                }
                if let Some(rules) = &album.smart {
                    if album.folder || !album.photos.is_empty() {
                        return Err(CatalogError::Invalid("a smart album holds rules, not photos".into()));
                    }
                    self.validate_rules(rules)?;
                }
                let id = album.id;
                self.next_album = self.next_album.max(id.0 + 1);
                self.albums.insert(id, album);
                Op::RemoveAlbum { id }
            }
            Op::RemoveAlbum { id } => {
                if self.albums.values().any(|a| a.parent == Some(id)) {
                    return Err(CatalogError::Invalid("folder is not empty".into()));
                }
                let a = self.albums.remove(&id).ok_or(CatalogError::NoAlbum(id))?;
                Op::AddAlbum { album: a }
            }
            Op::RenameAlbum { id, name } => {
                let a = self.album_mut(id)?;
                Op::RenameAlbum { id, name: std::mem::replace(&mut a.name, name) }
            }
            Op::MoveAlbum { id, parent } => {
                if let Some(p) = parent {
                    if p == id || !self.albums.get(&p).is_some_and(|a| a.folder) {
                        return Err(CatalogError::Invalid("parent must be another folder".into()));
                    }
                    // no cycles (parents already in a ring, in a damaged library, end the walk)
                    let mut cur = Some(p);
                    for _ in 0..=self.albums.len() {
                        let Some(c) = cur else { break };
                        if c == id {
                            return Err(CatalogError::Invalid("cannot move a folder into itself".into()));
                        }
                        cur = self.albums.get(&c).and_then(|a| a.parent);
                    }
                }
                let a = self.album_mut(id)?;
                if a.parent == parent {
                    return Ok(Op::MoveAlbum { id, parent });
                }
                // its place belonged to the old neighbours: the undo gives it back with the folder
                let old_order = a.order.take();
                let old_parent = std::mem::replace(&mut a.parent, parent);
                match old_order {
                    Some(_) => Op::Batch { ops: vec![Op::MoveAlbum { id, parent: old_parent }, Op::SetAlbumOrder { id, order: old_order }] },
                    None => Op::MoveAlbum { id, parent: old_parent },
                }
            }
            Op::SetAlbumOrder { id, order } => {
                let a = self.album_mut(id)?;
                Op::SetAlbumOrder { id, order: std::mem::replace(&mut a.order, order) }
            }
            Op::SetAlbumPhotos { id, photos } => {
                let a = self.album_mut(id)?;
                if (a.folder || a.smart.is_some()) && !photos.is_empty() {
                    return Err(CatalogError::Invalid(
                        if a.folder { "folders can't hold photos" } else { "smart albums update automatically" }.into(),
                    ));
                }
                Op::SetAlbumPhotos { id, photos: std::mem::replace(&mut a.photos, photos) }
            }
            Op::SetAlbumCover { id, cover } => {
                let a = self.album_mut(id)?;
                Op::SetAlbumCover { id, cover: std::mem::replace(&mut a.cover, cover) }
            }
            Op::SetAlbumRules { id, rules } => {
                self.validate_rules(&rules)?;
                let a = self.album_mut(id)?;
                let Some(old) = a.smart.as_mut() else {
                    return Err(CatalogError::Invalid("not a smart album".into()));
                };
                Op::SetAlbumRules { id, rules: std::mem::replace(old, rules) }
            }
            Op::AddStack { stack } => {
                if self.stacks.contains_key(&stack.id) {
                    return Err(CatalogError::Invalid(format!("stack {:?} exists", stack.id)));
                }
                self.validate_stack(stack.id, &stack.photos)?;
                let id = stack.id;
                self.next_stack = self.next_stack.max(id.0 + 1);
                self.stacks.insert(id, stack);
                Op::RemoveStack { id }
            }
            Op::RemoveStack { id } => {
                let s = self.stacks.remove(&id).ok_or(CatalogError::NoStack(id))?;
                Op::AddStack { stack: s }
            }
            Op::SetStack { id, photos, collapsed } => {
                if !self.stacks.contains_key(&id) {
                    return Err(CatalogError::NoStack(id));
                }
                self.validate_stack(id, &photos)?;
                let s = self.stacks.get_mut(&id).ok_or(CatalogError::NoStack(id))?;
                let old = Op::SetStack { id, photos: std::mem::replace(&mut s.photos, photos), collapsed: s.collapsed };
                s.collapsed = collapsed;
                old
            }
            Op::SetAnalysis { id, analysis } => {
                let p = self.photo_mut(id)?;
                Op::SetAnalysis { id, analysis: std::mem::replace(&mut p.analysis, analysis) }
            }
            Op::SetCaptured { id, captured } => {
                if let Some(c) = &captured
                    && stacks::iso_seconds(c).is_none()
                {
                    return Err(CatalogError::Invalid(format!("not a date: {c}")));
                }
                let p = self.photo_mut(id)?;
                Op::SetCaptured { id, captured: std::mem::replace(&mut p.captured, captured) }
            }
            Op::Relink { id, file_name, source, format } => {
                let p = self.photo_mut(id)?;
                let old_name = std::mem::replace(&mut p.file_name, file_name);
                let old_format = format.map(|f| std::mem::replace(&mut p.format, f));
                Op::Relink { id, file_name: old_name, source: std::mem::replace(&mut p.source, source), format: old_format }
            }
            Op::SetContent { id, width, height, file_size, content_hash, preview_only } => {
                let p = self.photo_mut(id)?;
                Op::SetContent {
                    id,
                    width: std::mem::replace(&mut p.width, width),
                    height: std::mem::replace(&mut p.height, height),
                    file_size: std::mem::replace(&mut p.file_size, file_size),
                    content_hash: std::mem::replace(&mut p.content_hash, content_hash),
                    preview_only: std::mem::replace(&mut p.preview_only, preview_only),
                }
            }
            Op::SetEmbeddedLens { id, lens } => {
                let p = self.photo_mut(id)?;
                Op::SetEmbeddedLens { id, lens: std::mem::replace(&mut p.embedded_lens, lens.map(|l| *l)).map(Box::new) }
            }
            Op::SetFile { id, file_name, source } => {
                if file_name.trim().is_empty() {
                    return Err(CatalogError::Invalid("empty file name".into()));
                }
                let p = self.photo_mut(id)?;
                let old_name = std::mem::replace(&mut p.file_name, file_name);
                Op::SetFile { id, file_name: old_name, source: std::mem::replace(&mut p.source, source) }
            }
            Op::SetLabelName { label, name } => {
                let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
                let old = match name {
                    Some(n) => self.label_names.insert(label, n),
                    None => self.label_names.remove(&label),
                };
                Op::SetLabelName { label, name: old }
            }
            Op::SetKeyword { path, info } => {
                let path = keywords::clean(&path);
                if path.is_empty() {
                    return Err(CatalogError::Invalid("keyword names can't be empty".into()));
                }
                let key = path.to_lowercase();
                let old = match info {
                    Some(info) => self.keyword_list.insert(key, keywords::ListedKeyword { path: path.clone(), info }),
                    None => self.keyword_list.remove(&key),
                };
                match old {
                    Some(old) => Op::SetKeyword { path: old.path, info: Some(old.info) },
                    None => Op::SetKeyword { path, info: None },
                }
            }
            Op::SetBrowsed { folder, at } => {
                let folder = crate::query::folder_key(&folder);
                let old = match at {
                    Some(t) => self.browsed.insert(folder.clone(), t),
                    None => self.browsed.remove(&folder),
                };
                Op::SetBrowsed { folder, at: old }
            }
            Op::SetFolderRecord { folder, record } => {
                let key = folders::record_key(&folder).ok_or_else(|| CatalogError::Invalid(format!("not a folder: {folder}")))?;
                let old = match record.filter(|r| !r.is_empty()) {
                    Some(r) => self.folder_records.insert(key.clone(), r),
                    None => self.folder_records.remove(&key),
                };
                Op::SetFolderRecord { folder: key, record: old }
            }
            Op::Batch { ops } => {
                let mut inverses = Vec::with_capacity(ops.len());
                for op in ops {
                    match self.apply_inner(op) {
                        Ok(inv) => inverses.push(inv),
                        Err(e) => {
                            // roll back what was applied
                            for inv in inverses.into_iter().rev() {
                                let _ = self.apply_inner(inv);
                            }
                            return Err(e);
                        }
                    }
                }
                inverses.reverse();
                Op::Batch { ops: inverses }
            }
        })
    }

    /// Ops that remove a photo permanently including album and stack memberships (one undoable
    /// batch).
    pub fn delete_permanently_ops(&self, id: PhotoId) -> Op {
        let mut ops: Vec<Op> = self
            .albums
            .values()
            .filter(|a| a.photos.contains(&id))
            .map(|a| Op::SetAlbumPhotos { id: a.id, photos: a.photos.iter().copied().filter(|p| *p != id).collect() })
            .collect();
        ops.extend(self.remove_from_stacks_ops(&[id]));
        ops.push(Op::RemovePhoto { id });
        Op::Batch { ops }
    }

    /// [`Self::delete_permanently_ops`] for several photos at once. One op list built against the
    /// catalog as it is now: albums lose all of them in one write each, and stacks are shortened
    /// (or dissolved) once, however many of their photos go.
    pub fn delete_photos_permanently_ops(&self, ids: &[PhotoId]) -> Op {
        let gone: std::collections::HashSet<PhotoId> = ids.iter().copied().collect();
        let mut ops: Vec<Op> = self
            .albums
            .values()
            .filter(|a| a.photos.iter().any(|p| gone.contains(p)))
            .map(|a| Op::SetAlbumPhotos { id: a.id, photos: a.photos.iter().copied().filter(|p| !gone.contains(p)).collect() })
            .collect();
        ops.extend(self.remove_from_stacks_ops(ids));
        ops.extend(gone.iter().map(|id| Op::RemovePhoto { id: *id }));
        Op::Batch { ops }
    }

    // ---- persistence

    /// Full snapshot as JSON.
    pub fn to_snapshot(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_snapshot(s: &str) -> Result<Catalog> {
        serde_json::from_str(s).map_err(|e| CatalogError::Corrupt(e.to_string()))
    }

    /// Replay an op log (JSON lines) on top of `self`. A torn final line (crash mid-write) is ignored.
    pub fn replay(&mut self, log: &str) -> Result<usize> {
        let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut n = 0;
        for (i, line) in lines.iter().enumerate() {
            match serde_json::from_str::<Op>(line) {
                Ok(op) => {
                    self.apply(op)?;
                    n += 1;
                }
                Err(e) if i + 1 == lines.len() => {
                    let _ = e;
                    break;
                }
                Err(e) => return Err(CatalogError::Corrupt(format!("log line {}: {e}", i + 1))),
            }
        }
        Ok(n)
    }

    pub fn op_to_log_line(op: &Op) -> String {
        let mut s = serde_json::to_string(op).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// How siblings are listed: folders first, then the ones with a place of their own by it, then by
/// name; the id settles any tie.
fn album_order_key(a: &Album) -> (bool, bool, u32, String, AlbumId) {
    (!a.folder, a.order.is_none(), a.order.unwrap_or(0), a.name.to_lowercase(), a.id)
}

/// The photos a folder of albums shows: what the albums inside it list, and the smart albums
/// inside it, whose rules are asked per photo.
#[derive(Default)]
struct FolderPhotos {
    listed: std::collections::HashSet<PhotoId>,
    smart: Vec<AlbumId>,
}

impl FolderPhotos {
    fn holds(&self, cat: &Catalog, p: &Photo) -> bool {
        self.listed.contains(&p.id) || self.smart.iter().any(|s| cat.album_contains(*s, p))
    }
}

thread_local! {
    /// The folders asked about during a pass over the library ([`gathering`]), by catalog (where
    /// it is, which holds for the pass: it is borrowed throughout) and id: `None` outside one.
    static GATHERED: std::cell::RefCell<Option<std::collections::HashMap<(usize, AlbumId), std::rc::Rc<FolderPhotos>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `pass`, a question put to many photos of one catalog: each folder of albums it asks about
/// (the one shown, or one a smart album is limited to) has its photos gathered once, not looked
/// up album by album for every photo. Forgotten when the outermost pass ends, however it ends.
pub(crate) fn gathering<T>(pass: impl FnOnce() -> T) -> T {
    struct Forget(bool);
    impl Drop for Forget {
        fn drop(&mut self) {
            if self.0 {
                GATHERED.with_borrow_mut(|g| *g = None);
            }
        }
    }
    let outermost = GATHERED.with_borrow_mut(|g| g.is_none().then(|| *g = Some(Default::default())).is_some());
    let _forget = Forget(outermost);
    pass()
}

/// Folder `id` of `cat`'s photos during a pass ([`gathering`]), made by `gather` the first time;
/// `None` outside one.
fn gathered(cat: &Catalog, id: AlbumId, gather: impl FnOnce() -> FolderPhotos) -> Option<std::rc::Rc<FolderPhotos>> {
    let key = (std::ptr::from_ref(cat) as usize, id);
    if let Some(known) = GATHERED.with_borrow(|g| g.as_ref().map(|folders| folders.get(&key).cloned()))? {
        return Some(known);
    }
    // gathered with nothing borrowed, then kept
    let folder = std::rc::Rc::new(gather());
    GATHERED.with_borrow_mut(|g| g.as_mut().map(|folders| folders.insert(key, folder.clone())));
    Some(folder)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_album_folder_photos;
#[cfg(test)]
mod tests_album_order;
#[cfg(test)]
mod tests_background;
#[cfg(test)]
mod tests_folder_records;
#[cfg(test)]
mod tests_folders;
#[cfg(test)]
mod tests_format_version;
#[cfg(test)]
mod tests_journal;
#[cfg(test)]
mod tests_local;
#[cfg(test)]
mod tests_lock;
#[cfg(test)]
mod tests_torn_append;
