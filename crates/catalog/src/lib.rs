//! The library (catalog).
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
#[cfg(not(target_arch = "wasm32"))]
pub mod db;
pub mod folders;
pub mod journal;
pub mod keywords;
#[cfg(not(target_arch = "wasm32"))]
pub mod library;
pub mod local;
pub mod lock;
pub mod model;
pub mod progress;
pub mod query;
pub mod remote;
pub mod rules;
pub mod safe_file;
pub(crate) mod settings_ref;
pub mod stacks;
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod transfer;
pub mod xmp_state;

use std::collections::BTreeMap;
use std::sync::Arc;

use dac_develop::DevelopSettings;
pub use dates::{DateRun, GroupBy};
pub use folders::{FolderNode, FolderRecord};
pub use journal::{Journal, LoadReport, PersistStats, SnapshotPolicy, SnapshotTiming};
pub use keywords::KeywordNode;
pub use local::{DEFAULT_FORGET_DAYS, ForgetPlan, folder_of};
pub use lock::{LibraryLock, LockError, LockOwner};
pub use model::*;
pub use query::{DateGroup, Filter, Person, RatingOp, Sort, SortKey, mix64};
pub use remote::{PreviewEntry, RemoteIdentity, RemoteKey, RemoteTable, SyncState};
pub use rules::{Match, Rule, RuleSet};
use serde::{Deserialize, Serialize};
pub use store::{FsStore, MemStore, Store};
pub use xmp_state::{SidecarStat, XmpStamp, XmpStatus};

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
    /// The library was written by a newer version of the app (a newer catalog format, or a change this
    /// version doesn't know). Nothing was read into the session and nothing was modified.
    #[error("this library was written by a newer version of the app ({0}); update the app to open it. The library was left unchanged.")]
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
        lens: Option<Box<dac_develop::EmbeddedLens>>,
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
    /// What kind of file the photo is and its format, when the file behind it changed kind (a
    /// link-only Immich photo whose raw original arrived). Format version 4.
    SetKind {
        id: PhotoId,
        kind: MediaKind,
        format: String,
    },
    /// The original file's SHA-1 (40 hex digits; `None` = unknown). Format version 4.
    SetSha1 {
        id: PhotoId,
        sha1: Option<String>,
    },
    /// The state at the last XMP read/write (see [`xmp_state`]). Format version 4.
    SetXmpStamp {
        id: PhotoId,
        stamp: Option<xmp_state::XmpStamp>,
    },
    /// Link a photo to a remote asset (`record: Some`, replacing its link on that account) or
    /// unlink it (`None`). Format version 4.
    SetRemote {
        photo: PhotoId,
        service: String,
        account_id: String,
        record: Option<Box<RemoteIdentity>>,
    },
    /// The previews index entry of a photo (`None`: no preview). Not an undo step. Format version 4.
    SetPreview {
        id: PhotoId,
        entry: Option<PreviewEntry>,
    },
    /// List a keyword in the library's keyword list with these attributes, or take it off the list
    /// (`None`; photos that carry it keep it). Format version 5.
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
    /// A saved location of the Map module, by name (case-insensitive); `None` = delete it.
    /// Format version 6.
    SetSavedLocation {
        name: String,
        location: Option<dac_geo::SavedLocation>,
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
    /// Links to assets on remote services (see [`remote`]).
    #[serde(default, skip_serializing_if = "RemoteTable::is_empty")]
    remote: RemoteTable,
    /// The previews index.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    previews: BTreeMap<PhotoId, PreviewEntry>,
    /// Keywords listed on their own or given attributes, by lower-case path (see [`keywords`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    keyword_list: BTreeMap<String, keywords::ListedKeyword>,
    /// What the library keeps about its folders (folder identity → record), see [`folders`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    folder_records: BTreeMap<String, FolderRecord>,
    /// The Map module's saved locations, by lower-case name (format version 6).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    saved_locations: BTreeMap<String, dac_geo::SavedLocation>,
    /// Increments on every applied op.
    #[serde(skip)]
    pub revision: u64,
    /// Whole-catalog sort orders kept between queries (see [`query::SortCache`]).
    #[serde(skip)]
    sort_cache: query::SortCache,
}

impl Catalog {
    /// The Map module's saved locations, by name.
    pub fn saved_locations(&self) -> impl Iterator<Item = &dac_geo::SavedLocation> {
        self.saved_locations.values()
    }

    pub fn saved_location(&self, name: &str) -> Option<&dac_geo::SavedLocation> {
        self.saved_locations.get(&name.trim().to_lowercase())
    }

    /// Is a photo position inside a private saved location (its location is left out on export)?
    pub fn is_private_location(&self, gps: (f64, f64)) -> bool {
        self.saved_locations.values().any(|l| l.private && l.contains(dac_geo::LatLon::new(gps.0, gps.1)))
    }

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
    /// Every remote link (see [`remote`]).
    pub fn remote_links(&self) -> &RemoteTable {
        &self.remote
    }
    /// A photo's remote links.
    pub fn remote_of(&self, id: PhotoId) -> impl Iterator<Item = &RemoteIdentity> {
        self.remote.of_photo(id)
    }
    /// The photo linked to a remote asset.
    pub fn photo_of_remote(&self, service: &str, account_id: &str, remote_id: &str) -> Option<PhotoId> {
        self.remote.photo_of(service, account_id, remote_id)
    }
    /// The op that links (or relinks) a photo to a remote asset.
    pub fn link_remote_op(r: RemoteIdentity) -> Op {
        Op::SetRemote { photo: r.photo_id, service: r.service.clone(), account_id: r.account_id.clone(), record: Some(Box::new(r)) }
    }
    /// The previews index entry of a photo.
    pub fn preview_entry(&self, id: PhotoId) -> Option<&PreviewEntry> {
        self.previews.get(&id)
    }
    /// Photos with a given SHA-1 (hex, any case).
    pub fn photos_with_sha1(&self, sha1: &str) -> Vec<PhotoId> {
        self.photos.values().filter(|p| p.sha1.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(sha1))).map(|p| p.id).collect()
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

    /// Albums (regular and smart) that contain the photo.
    pub fn albums_of(&self, id: PhotoId) -> Vec<AlbumId> {
        let Some(p) = self.photos.get(&id) else { return Vec::new() };
        self.albums.values().filter(|a| self.album_contains(a.id, p)).map(|a| a.id).collect()
    }

    /// Whether album `id` contains `p` (a smart album evaluates its rules; deleted photos are in
    /// no smart album).
    pub fn album_contains(&self, id: AlbumId, p: &Photo) -> bool {
        match self.albums.get(&id) {
            // guarded: a smart album testing smart albums can't loop or recurse without end
            Some(Album { smart: Some(rules), .. }) => !p.deleted && rules::smart_album_holds(id, p.id, || rules.matches(p, self)),
            Some(a) => a.photos.contains(&p.id),
            None => false,
        }
    }

    /// Whether smart album `from` tests `to`, directly or through the smart albums it tests (the
    /// rules of `from` would then change when those of `to` do). Loops in saved rules end the
    /// search, they don't repeat it.
    pub fn album_reaches(&self, from: AlbumId, to: AlbumId) -> bool {
        let mut seen: std::collections::HashSet<AlbumId> = std::collections::HashSet::new();
        let mut next = vec![from];
        while let Some(a) = next.pop() {
            if !seen.insert(a) {
                continue;
            }
            let tested = self.albums.get(&a).and_then(|al| al.smart.as_deref()).map(Filter::albums_tested).unwrap_or_default();
            if tested.contains(&to) {
                return true;
            }
            next.extend(tested);
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

    /// A loop through `filter`'s own album field (not its rules): smart album `owner` filtered to
    /// itself, or to an album that leads back to it.
    pub fn album_filter_problem(&self, filter: &Filter, owner: Option<AlbumId>) -> Option<rules::Problem> {
        let (a, owner) = (filter.album?, owner?);
        (a == owner || self.album_reaches(a, owner)).then(|| rules::Problem {
            path: Vec::new(),
            field: Some("album".into()),
            issue: rules::Issue::AlbumLoop,
            message: format!("its album filter (album {}) would make this album include itself", a.0),
        })
    }

    /// The photos of an album: the stored list, or a smart album's current matches (id order).
    pub fn album_photos(&self, id: AlbumId) -> Vec<PhotoId> {
        match self.albums.get(&id) {
            Some(Album { smart: Some(_), .. }) => self.photos.values().filter(|p| self.album_contains(id, p)).map(|p| p.id).collect(),
            Some(a) => a.photos.clone(),
            None => Vec::new(),
        }
    }

    /// Number of photos shown for an album in the sources list (excludes deleted photos).
    pub fn album_count(&self, id: AlbumId) -> usize {
        match self.albums.get(&id) {
            // counted in place: no id list is built just for its length
            Some(Album { smart: Some(_), .. }) => self.photos.values().filter(|p| self.album_contains(id, p)).count(),
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
        match &op {
            Op::AddPhoto { .. } | Op::RemovePhoto { .. } | Op::SetCaptured { .. } => self.sort_cache.bump(true),
            Op::SetRating { .. } | Op::SetDevelop { .. } | Op::SetFile { .. } | Op::Relink { .. } | Op::SetContent { .. } => {
                self.sort_cache.bump(false)
            }
            _ => {}
        }
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
                    // no cycles
                    let mut cur = Some(p);
                    while let Some(c) = cur {
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
                Op::SetEmbeddedLens { id, lens: std::mem::replace(&mut p.embedded_lens, lens) }
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
            Op::SetKind { id, kind, format } => {
                let p = self.photo_mut(id)?;
                Op::SetKind { id, kind: std::mem::replace(&mut p.kind, kind), format: std::mem::replace(&mut p.format, format) }
            }
            Op::SetSha1 { id, sha1 } => {
                if let Some(s) = &sha1
                    && (s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err(CatalogError::Invalid(format!("not a SHA-1: {s}")));
                }
                let sha1 = sha1.map(|s| s.to_ascii_lowercase());
                let p = self.photo_mut(id)?;
                Op::SetSha1 { id, sha1: std::mem::replace(&mut p.sha1, sha1) }
            }
            Op::SetXmpStamp { id, stamp } => {
                let p = self.photo_mut(id)?;
                Op::SetXmpStamp { id, stamp: std::mem::replace(&mut p.xmp, stamp) }
            }
            Op::SetRemote { photo, service, account_id, record } => {
                let key = (photo, service, account_id);
                let old = match record {
                    Some(r) => {
                        if r.key() != key {
                            return Err(CatalogError::Invalid("remote link doesn't match its key".into()));
                        }
                        if r.service.is_empty() || r.remote_id.is_empty() {
                            return Err(CatalogError::Invalid("remote link without a service or remote id".into()));
                        }
                        if !self.photos.contains_key(&photo) {
                            return Err(CatalogError::NoPhoto(photo));
                        }
                        self.remote.upsert(*r).map_err(CatalogError::Invalid)?
                    }
                    None => self.remote.remove(&key),
                };
                let (photo, service, account_id) = key;
                Op::SetRemote { photo, service, account_id, record: old.map(Box::new) }
            }
            Op::SetPreview { id, entry } => {
                let old = match entry {
                    Some(e) => {
                        if !self.photos.contains_key(&id) {
                            return Err(CatalogError::NoPhoto(id));
                        }
                        self.previews.insert(id, e)
                    }
                    None => self.previews.remove(&id),
                };
                Op::SetPreview { id, entry: old }
            }
            Op::SetFolderRecord { folder, record } => {
                let key = folders::record_key(&folder).ok_or_else(|| CatalogError::Invalid(format!("not a folder: {folder}")))?;
                let old = match record.filter(|r| !r.is_empty()) {
                    Some(r) => self.folder_records.insert(key.clone(), r),
                    None => self.folder_records.remove(&key),
                };
                Op::SetFolderRecord { folder: key, record: old }
            }
            Op::SetSavedLocation { name, location } => {
                let key = name.trim().to_lowercase();
                if key.is_empty() {
                    return Err(CatalogError::Invalid("a saved location needs a name".into()));
                }
                let old = match location {
                    Some(l) => {
                        let l = dac_geo::SavedLocation::new(&l.name, l.lat, l.lon, l.radius, l.private)
                            .map_err(|e| CatalogError::Invalid(e.to_string()))?;
                        self.saved_locations.insert(key, l)
                    }
                    None => self.saved_locations.remove(&key),
                };
                Op::SetSavedLocation { name, location: old }
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
        for (photo, service, account_id) in self.remote.keys_of(id) {
            ops.push(Op::SetRemote { photo, service, account_id, record: None });
        }
        if self.previews.contains_key(&id) {
            ops.push(Op::SetPreview { id, entry: None });
        }
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
        for id in ids {
            for (photo, service, account_id) in self.remote.keys_of(*id) {
                ops.push(Op::SetRemote { photo, service, account_id, record: None });
            }
            if self.previews.contains_key(id) {
                ops.push(Op::SetPreview { id: *id, entry: None });
            }
        }
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

#[cfg(test)]
mod tests;
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
mod tests_robust;
#[cfg(test)]
mod tests_sort_cache;
#[cfg(test)]
mod tests_torn_append;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_v4;
