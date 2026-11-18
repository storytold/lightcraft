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

pub mod advanced_search;
pub mod dates;
#[cfg(not(target_arch = "wasm32"))]
pub mod db;
pub mod facets;
pub mod folders;
pub mod incoming;
mod index;
pub mod journal;
pub mod keywords;
#[cfg(not(target_arch = "wasm32"))]
pub mod legacy;
#[cfg(not(target_arch = "wasm32"))]
pub mod library;
pub mod local;
pub mod lock;
#[cfg(not(target_arch = "wasm32"))]
mod log_writer;
pub mod model;
pub mod progress;
pub mod query;
pub mod remote;
pub mod rules;
pub mod safe_file;
pub(crate) mod settings_ref;
pub mod stacks;
pub mod store;
pub mod tally;
#[cfg(not(target_arch = "wasm32"))]
pub mod transfer;
#[cfg(not(target_arch = "wasm32"))]
pub mod upgrade_backup;
pub mod vignetting_maps;
pub mod xmp_state;

use std::collections::BTreeMap;
use std::sync::Arc;

pub use dates::{DateRun, GroupBy};
pub use facets::FacetPick;
pub use folders::{FolderNode, FolderRecord};
use index::file_path;
pub use index::{file_name_index_key, folder_index_key, photo_folder_index_key};
pub use journal::{Journal, LoadReport, PersistStats, SnapshotPolicy, SnapshotTiming};
pub use keywords::KeywordNode;
use lcc_develop::DevelopSettings;
pub use local::{DEFAULT_FORGET_DAYS, ForgetPlan, LocalRecords, folder_of};
pub use lock::{LibraryLock, LockError, LockOwner};
pub use model::*;
pub use query::{DateGroup, Filter, MatchOp, Person, RatingOp, Sort, SortKey, Tally, mix64};
pub use remote::{PreviewEntry, RemoteIdentity, RemoteKey, RemoteTable, SyncState};
pub use rules::{Match, Rule, RuleSet};
use serde::{Deserialize, Serialize};
pub use store::{FsStore, MemStore, Store};
pub use vignetting_maps::{ReferenceState, VignettingMapRecord, reference_photo};
pub use xmp_state::{SidecarStat, XmpStamp, XmpStatus};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CatalogError {
    #[error("no such photo {0:?}")]
    NoPhoto(PhotoId),
    #[error("no such collection {0:?}")]
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
    #[error("this catalog was written by a newer version of the app ({0}); update the app to open it. The catalog was left unchanged.")]
    Newer(String),
    /// The folder holds a catalog of another app (e.g. the upstream app this one was forked from):
    /// a format this app doesn't read. Nothing was read and nothing was modified.
    #[error("this catalog was made by another app ({0}); it can't be opened here. It was left unchanged.")]
    Foreign(String),
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
    /// What the Before view shows (see [`Photo::before`]; `None`: the import state). Format
    /// version 16.
    SetBefore {
        id: PhotoId,
        #[serde(default)]
        before: Option<Arc<DevelopSettings>>,
    },
    /// When the photo was last exported (see [`Photo::exported`]; `None`: never, or reset). Format
    /// version 17.
    SetExported {
        id: PhotoId,
        #[serde(default)]
        exported: Option<String>,
    },
    /// Move the History cursor (see [`Photo::history_cursor`]) without changing the steps.
    SetHistoryCursor {
        id: PhotoId,
        #[serde(default)]
        cursor: Option<usize>,
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
    /// The album's Custom Order ([`Album::photo_order`]; empty clears it). Ids that aren't members
    /// and repeats are dropped. Format version 9.
    SetAlbumPhotoOrder {
        id: AlbumId,
        order: Vec<PhotoId>,
    },
    SetAlbumCover {
        id: AlbumId,
        cover: Option<PhotoId>,
    },
    /// Attach, replace or remove an album's saved-creation layout. Format version 7.
    SetAlbumCreation {
        id: AlbumId,
        creation: Option<Creation>,
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
        /// The app version that decided `preview_only` (see [`Photo::preview_checked`]); empty
        /// when it is `None`. Format version 12.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        preview_checked: String,
    },
    /// The lens data a photo's file carries (what Reload finds when it was read after import).
    SetEmbeddedLens {
        id: PhotoId,
        lens: Option<Box<lcc_develop::EmbeddedLens>>,
    },
    /// A raw's as-shot white balance (Kelvin, tint) as its file reads now: what a decode or Reload finds when the
    /// camera's colour matrices give the file a Kelvin scale it was imported without (upstream issue #730).
    /// Bookkeeping, not an edit: the look does not change, only the As Shot readout.
    SetAsShotWb {
        id: PhotoId,
        wb: Option<(f64, f64)>,
    },
    /// A video clip's probed facts, trim and poster (`None` = not a probed clip). Catalog format 10.
    SetVideoMeta {
        id: PhotoId,
        meta: Option<VideoMeta>,
    },
    /// A video clip's trim (seconds; `trim_out: None` = the end). The photo needs [`Photo::video`]. Format 10.
    SetVideoTrim {
        id: PhotoId,
        trim_in: f64,
        trim_out: Option<f64>,
    },
    /// A video clip's poster frame time (`None` = the default, 10 % in). Format 10.
    SetVideoPoster {
        id: PhotoId,
        time: Option<f64>,
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
        location: Option<lcc_geo::SavedLocation>,
    },
    /// Several ops as one step (undo applies the inverses in reverse).
    Batch {
        ops: Vec<Op>,
    },
    /// Store a vignetting map under its id ([`vignetting_maps`]): kept as it is when the id is already there; refused
    /// when the map isn't valid or isn't the id's. The table only grows: the inverse is an empty batch, so undo never
    /// takes a map away from a photo. Format version 18.
    AddVignettingMap {
        id: lcc_develop::VignettingMapId,
        record: Box<VignettingMapRecord>,
    },
    /// Withdraw a vignetting map as a reference, or put it in use again ([`vignetting_maps`]): Apply doesn't pick a
    /// withdrawn one; the photos that use it keep it. The inverse restores the state before; on a map the catalog
    /// doesn't hold it does nothing. Format version 18.
    SetVignettingMapState {
        id: lcc_develop::VignettingMapId,
        withdrawn: bool,
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
    saved_locations: BTreeMap<String, lcc_geo::SavedLocation>,
    /// Flat-field maps by id, each once ([`vignetting_maps`]; format version 18). Append-only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "vignetting_maps::de_table")]
    vignetting_maps: BTreeMap<lcc_develop::VignettingMapId, Arc<VignettingMapRecord>>,
    /// Increments on every applied op, and when a batch fails and its rollback cannot undo it whole (the catalog
    /// changed, so what is keyed by the revision is out of date).
    #[serde(skip)]
    pub revision: u64,
    /// Whole-catalog sort orders kept between queries (see [`query::SortCache`]).
    #[serde(skip)]
    sort_cache: query::SortCache,
    /// Which photos the latest ops touched ([`Catalog::touched_since`]).
    #[serde(skip)]
    changes: Changes,
    /// Lookups derived from the above (never saved), see [`index`].
    #[serde(skip)]
    index: index::Indexes,
}

/// What of a photo an op changed: enough to tell which tallies of the library an edit leaves as they were (a
/// rating changes no folder's count; a slider changes none at all).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Aspects(u16);

impl Aspects {
    pub const NONE: Aspects = Aspects(0);
    pub const RATING: Aspects = Aspects(1);
    pub const FLAG: Aspects = Aspects(1 << 1);
    pub const LABEL: Aspects = Aspects(1 << 2);
    /// Develop settings, versions and history (and a video's trim and poster).
    pub const DEVELOP: Aspects = Aspects(1 << 3);
    /// Title, caption, keywords, people, place and the rest of the metadata (and what remote services and the
    /// sidecar know of the photo, plug-in fields).
    pub const META: Aspects = Aspects(1 << 4);
    pub const CAPTURED: Aspects = Aspects(1 << 5);
    /// Where its file is (and what the file is: its hash, its preview).
    pub const FILE: Aspects = Aspects(1 << 6);
    pub const ANALYSIS: Aspects = Aspects(1 << 7);
    /// Whether it is in the library at all: added, removed, deleted, restored, taken in from Local.
    pub const PRESENCE: Aspects = Aspects(1 << 8);
    /// The keyword list (keywords on their own and their attributes): of no photo, and read by no rule, filter or
    /// sort, only by what lists keywords (the keyword tree, export names).
    pub const KEYWORD_LIST: Aspects = Aspects(1 << 9);
    /// Everything: the photo may count differently for any tally.
    pub const ALL: Aspects = Aspects(u16::MAX);

    pub const fn with(self, other: Aspects) -> Aspects {
        Aspects(self.0 | other.0)
    }

    /// Whether any of `other` is among these.
    pub const fn any_of(self, other: Aspects) -> bool {
        self.0 & other.0 != 0
    }
}

/// The most photos remembered as touched; older changes are then answered with "anything may have changed".
const MAX_TOUCHED: usize = 8192;

/// A short memory of what the latest ops changed, so that what is derived from the catalog (the grid's list) can
/// follow a small edit without being worked out again from every photo.
#[derive(Clone, Debug, Default)]
struct Changes {
    /// The photos ops changed, each with the revision the op made and what of the photo it changed. Oldest first.
    touched: std::collections::VecDeque<(u64, PhotoId, Aspects)>,
    /// What the op being applied changes of its photo.
    now: Aspects,
    /// The latest revision made by an op that changed more than photos of its own (an album, a stack…): what it
    /// changed for which photos is not known.
    broad: u64,
    /// The latest revision made by an op that changed the keyword list ([`Aspects::KEYWORD_LIST`]).
    keyword_list: u64,
    /// Changes up to this revision are no longer all in `touched`.
    floor: u64,
    /// The revision the last op recorded here made. When the catalog's revision is another, it was set from
    /// outside (a library just loaded takes a range of its own, so that nothing kept from another library is
    /// taken for this one's): nothing is known of how the catalog got to where it is.
    seen: u64,
    /// A batch that failed could not be rolled back whole: what it changed stays, so what it noted must too.
    torn: bool,
}

impl PartialEq for Changes {
    /// Two catalogs are the same catalog whatever each remembers of how it got there.
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Catalog {
    /// The Map module's saved locations, by name.
    pub fn saved_locations(&self) -> impl Iterator<Item = &lcc_geo::SavedLocation> {
        self.saved_locations.values()
    }

    pub fn saved_location(&self, name: &str) -> Option<&lcc_geo::SavedLocation> {
        self.saved_locations.get(&name.trim().to_lowercase())
    }

    /// Is a photo position inside a private saved location (its location is left out on export)?
    pub fn is_private_location(&self, gps: (f64, f64)) -> bool {
        self.saved_locations.values().any(|l| l.private && l.contains(lcc_geo::LatLon::new(gps.0, gps.1)))
    }

    pub fn new() -> Catalog {
        Catalog { next_photo: 1, next_album: 1, next_stack: 1, ..Default::default() }
    }

    // ---- ids

    pub fn alloc_photo_id(&mut self) -> PhotoId {
        let id = PhotoId(self.next_photo.max(1));
        self.next_photo = id.0.saturating_add(1);
        id
    }
    pub fn alloc_album_id(&mut self) -> AlbumId {
        let id = AlbumId(self.next_album.max(1));
        self.next_album = id.0.saturating_add(1);
        id
    }
    pub fn alloc_stack_id(&mut self) -> StackId {
        let id = StackId(self.next_stack.max(1));
        self.next_stack = id.0.saturating_add(1);
        id
    }

    // ---- reads

    pub fn photo(&self, id: PhotoId) -> Option<&Arc<Photo>> {
        self.photos.get(&id)
    }
    /// The vignetting map stored under `id`.
    pub fn vignetting_map(&self, id: &lcc_develop::VignettingMapId) -> Option<&Arc<VignettingMapRecord>> {
        self.vignetting_maps.get(id)
    }

    /// Every vignetting map in the catalog, by id.
    pub fn vignetting_maps(&self) -> impl Iterator<Item = (&lcc_develop::VignettingMapId, &Arc<VignettingMapRecord>)> {
        self.vignetting_maps.iter()
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
            Some(Album { smart: Some(rules), .. }) => self.smart_album_has(id, rules, &rules.prepare_one(), p),
            Some(a) => self.album_lists(a.id, p.id),
            None => false,
        }
    }

    /// Whether smart album `id` with `rules` (as [`Filter::prepare`]d) holds `p`.
    fn smart_album_has(&self, id: AlbumId, rules: &Filter, prepared: &query::Prepared, p: &Photo) -> bool {
        // guarded: a smart album testing smart albums can't loop or recurse without end
        !p.deleted && rules::smart_album_holds(id, p.id, || rules.matches_in(p, self, prepared))
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

    /// What is wrong with `filter`'s own album field (not its rules): it names an album that is
    /// gone (a view of an album, saved as a smart album, after that album was deleted), or, for
    /// smart album `owner`, itself or an album that leads back to it.
    pub fn album_filter_problem(&self, filter: &Filter, owner: Option<AlbumId>) -> Option<rules::Problem> {
        let a = filter.album?;
        let problem = |issue, message| rules::Problem { path: Vec::new(), field: Some("album".into()), issue, message };
        if !self.albums.contains_key(&a) {
            return Some(problem(rules::Issue::NoSuchAlbum, format!("its collection filter names collection {}, which is gone", a.0)));
        }
        let owner = owner?;
        (a == owner || self.album_reaches(a, owner)).then(|| {
            problem(rules::Issue::AlbumLoop, format!("its collection filter (collection {}) would make this collection include itself", a.0))
        })
    }

    /// The photos of an album: the stored list, or a smart album's current matches (id order).
    pub fn album_photos(&self, id: AlbumId) -> Vec<PhotoId> {
        match self.albums.get(&id) {
            Some(Album { smart: Some(rules), .. }) => {
                let prepared = rules.prepare();
                self.photos.values().filter(|p| self.smart_album_has(id, rules, &prepared, p)).map(|p| p.id).collect()
            }
            Some(a) => a.photos.clone(),
            None => Vec::new(),
        }
    }

    /// Number of photos shown for an album in the sources list (excludes deleted photos).
    pub fn album_count(&self, id: AlbumId) -> usize {
        match self.albums.get(&id) {
            // counted in place: no id list is built just for its length
            Some(Album { smart: Some(rules), .. }) => {
                let prepared = rules.prepare();
                self.photos.values().filter(|p| self.smart_album_has(id, rules, &prepared, p)).count()
            }
            Some(a) => a.photos.iter().filter(|p| self.photos.get(p).is_some_and(|p| !p.deleted)).count(),
            None => 0,
        }
    }

    /// Smart-album rules must not reference another smart album (no recursion) or deleted photos.
    fn validate_rules(&self, rules: &Filter) -> Result<()> {
        if rules.deleted {
            return Err(CatalogError::Invalid("smart collection rules can't select deleted photos".into()));
        }
        if let Some(a) = rules.album
            && self.albums.get(&a).is_some_and(Album::is_smart)
        {
            return Err(CatalogError::Invalid("smart collection rules can't reference another smart collection".into()));
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
        self.touch(id);
        self.photos.get_mut(&id).map(Arc::make_mut).ok_or(CatalogError::NoPhoto(id))
    }

    /// The op being applied changes photo `id` (or adds or removes it).
    fn touch(&mut self, id: PhotoId) {
        self.index.photo_touched(id, self.photos.len());
        // (the revision the op will make: `apply` counts up once it is done)
        self.changes.touched.push_back((self.revision.saturating_add(1), id, self.changes.now));
        while self.changes.touched.len() > MAX_TOUCHED {
            if let Some((revision, _, _)) = self.changes.touched.pop_front() {
                self.changes.floor = revision;
            }
        }
    }

    /// What of its photos the ops since `revision` changed, taken together ([`Aspects::NONE`] when there were
    /// none). `None` when that is not known ([`Catalog::touched_since`]). A tally of the library that reads only
    /// other things of a photo is still right.
    pub fn aspects_since(&self, revision: u64) -> Option<Aspects> {
        if !self.remembers(revision) {
            return None;
        }
        let photos = self.changes.touched.iter().filter(|(r, _, _)| *r > revision).fold(Aspects::NONE, |all, (_, _, a)| all.with(*a));
        Some(if self.changes.keyword_list > revision { photos.with(Aspects::KEYWORD_LIST) } else { photos })
    }

    /// Whether every change since the catalog was at `revision` is remembered photo by photo.
    fn remembers(&self, revision: u64) -> bool {
        self.changes.seen == self.revision && revision <= self.revision && revision >= self.changes.floor && self.changes.broad <= revision
    }

    /// The photos changed, added or removed since the catalog was at `revision`, each once. `None` when that is not
    /// known: an op since then changed more than photos of its own, or it was too long ago. Then anything may
    /// have changed.
    pub fn touched_since(&self, revision: u64) -> Option<Vec<PhotoId>> {
        if !self.remembers(revision) {
            return None;
        }
        let mut ids: Vec<PhotoId> = self.changes.touched.iter().filter(|(r, _, _)| *r > revision).map(|(_, id, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// What is remembered of the latest changes is of another past when the revision was set from outside.
    fn forget_other_past(&mut self) {
        if self.changes.seen != self.revision {
            self.changes = Changes { floor: self.revision, seen: self.revision, ..Default::default() };
        }
    }

    /// Replace photo `id`'s in-memory plug-in field index ([`Photo::plugin_fields`]): not an op, not
    /// journaled or undoable (the plug-ins' data is the source); smart albums and filters see it at once.
    pub fn set_plugin_fields(&mut self, id: PhotoId, fields: std::collections::BTreeMap<String, serde_json::Value>) -> Result<()> {
        if !self.photos.contains_key(&id) {
            return Err(CatalogError::NoPhoto(id));
        }
        self.forget_other_past();
        // (rules can test plug-in fields: followed like a metadata edit)
        self.changes.now = Aspects::META;
        self.photo_mut(id)?.plugin_fields = fields;
        self.revision += 1;
        self.changes.seen = self.revision;
        Ok(())
    }
    fn album_mut(&mut self, id: AlbumId) -> Result<&mut Album> {
        self.albums.get_mut(&id).ok_or(CatalogError::NoAlbum(id))
    }

    /// Apply an op; returns its inverse. On error the catalog is as it was, the revision included, and nothing is
    /// remembered of the op in what the latest ops touched. The one exception is a batch whose rollback fails: the
    /// catalog did change, so the revision advances and anything may be taken to have changed.
    pub fn apply(&mut self, op: Op) -> Result<Op> {
        // the revision was set from outside: what is remembered is of another past
        self.forget_other_past();
        let broad = self.changes.broad;
        let keyword_list = self.changes.keyword_list;
        self.changes.torn = false;
        let inv = match self.apply_inner(op) {
            Ok(inv) => inv,
            Err(e) => {
                let made = self.revision.saturating_add(1);
                if self.changes.torn {
                    // (a batch the rollback of which failed changed something and says not what: it makes a
                    // revision, so what is kept for the one before is dropped, and anything may have changed, as
                    // after an op of a wider reach)
                    self.changes.torn = false;
                    self.changes.broad = made;
                    self.revision = made;
                    self.changes.seen = made;
                } else {
                    // (everything the failed op, or the rollback of a batch, noted was made for the revision it
                    // would have made: nothing earlier has that one. A note the cap already dropped stays
                    // dropped, which only says less than is known, as the cap does.)
                    while self.changes.touched.back().is_some_and(|(r, _, _)| *r == made) {
                        self.changes.touched.pop_back();
                    }
                    self.changes.broad = broad;
                    self.changes.keyword_list = keyword_list;
                }
                return Err(e);
            }
        };
        self.revision += 1;
        self.changes.seen = self.revision;
        Ok(inv)
    }

    /// [`Self::apply`] for a new edit (not an undo, a redo or the replay of a saved log, which bring
    /// back what was): refused when it would put a smart album on a loop, one that includes itself
    /// through the albums it tests. THE place that rule is kept, whichever command, importer or
    /// task makes the edit, alone or in a batch. A library that already holds a loop still opens,
    /// and an edit that puts no album on one it was not on goes through, also to an album on a loop.
    ///
    /// The check is made on what the edit would do to the albums, before it is done: a refused
    /// edit never touched the library, so there is nothing to take back.
    pub fn apply_new(&mut self, op: Op) -> Result<Op> {
        if let Some((id, name)) = self.would_put_on_a_loop(&op) {
            return Err(CatalogError::Invalid(format!(
                "smart collection “{name}” (collection {}) would include itself: its rules lead back to it through the collections they test",
                id.0
            )));
        }
        self.apply(op)
    }

    /// The album `op` would newly put on a loop, as its id and name: the album the op is about
    /// when that is one of them. `None` when it makes none (or changes no album's rules).
    fn would_put_on_a_loop(&self, op: &Op) -> Option<(AlbumId, String)> {
        if !changes_what_albums_test(op) {
            return None;
        }
        let mut graph = AlbumGraph::of(self.albums.values());
        let now = graph.on_a_loop();
        let mut edited = Vec::new();
        foresee(&mut graph, op, &mut edited);
        let then = graph.on_a_loop();
        let name_of = |id: AlbumId| self.albums.get(&id).map(|a| a.name.clone()).unwrap_or_default();
        let new_edited = edited.iter().find(|(id, _)| then.contains(id) && !now.contains(id));
        match new_edited {
            Some((id, name)) => Some((*id, name.map_or_else(|| name_of(*id), str::to_string))),
            // an album the edit is not about (it tests one that now leads back to it)
            None => then.difference(&now).next().map(|id| (*id, name_of(*id))),
        }
    }

    fn apply_inner(&mut self, op: Op) -> Result<Op> {
        // a photo that comes, goes, is deleted or restored, is imported from Local or gets other content can change
        // whether the photos with its content have the same content as another one (`index::content_key`), when
        // anything asks
        let photo = match &op {
            Op::AddPhoto { photo } => Some(photo.id),
            Op::RemovePhoto { id } | Op::SetDeleted { id, .. } | Op::SetLocal { id, .. } | Op::SetContent { id, .. } => Some(*id),
            _ => None,
        }
        .filter(|_| self.follows_contents());
        let albums = matches!(op, Op::AddAlbum { .. } | Op::RemoveAlbum { .. } | Op::SetAlbumRules { .. });
        let before = photo.and_then(|id| self.content_of(id));
        let inv = self.apply_op(op)?;
        if let Some(id) = photo {
            let after = self.content_of(id);
            self.content_moved(id, before, after);
        }
        if albums {
            self.index.albums_changed();
        }
        Ok(inv)
    }

    fn apply_op(&mut self, op: Op) -> Result<Op> {
        self.index.op_applied(&op);
        // An op that changes one photo says which (`photo_mut`, and adding or removing it below). Most other kinds
        // can change what any photo matches or where it sorts: an album's photos or rules, a stack. The keyword
        // list changes only lists of keywords. Listed kind by kind, so a new one has to be put on one side.
        match &op {
            // (a photo that comes, goes, or changes what it is to the library counts for every tally; so do the
            // rare kinds whose reach nobody has to think about)
            Op::AddPhoto { photo } => {
                self.changes.now = Aspects::ALL;
                self.touch(photo.id);
            }
            Op::RemovePhoto { id } => {
                self.changes.now = Aspects::ALL;
                self.touch(*id);
            }
            Op::SetDeleted { .. }
            | Op::SetLocal { .. }
            | Op::SetContent { .. }
            | Op::SetEmbeddedLens { .. }
            | Op::SetAsShotWb { .. }
            | Op::SetKind { .. }
            | Op::SetVideoMeta { .. } => self.changes.now = Aspects::ALL,
            Op::SetRating { .. } => self.changes.now = Aspects::RATING,
            Op::SetFlag { .. } => self.changes.now = Aspects::FLAG,
            Op::SetLabel { .. } => self.changes.now = Aspects::LABEL,
            Op::SetMeta { .. } | Op::SetXmpStamp { .. } => self.changes.now = Aspects::META,
            Op::SetCaptured { .. } => self.changes.now = Aspects::CAPTURED,
            Op::SetFile { .. } | Op::Relink { .. } | Op::SetSha1 { .. } | Op::SetExported { .. } => self.changes.now = Aspects::FILE,
            Op::SetAnalysis { .. } => self.changes.now = Aspects::ANALYSIS,
            Op::SetDevelop { .. }
            | Op::SetVersions { .. }
            | Op::SetHistory { .. }
            | Op::PushHistory { .. }
            | Op::SetHistoryCursor { .. }
            | Op::SetBefore { .. }
            | Op::SetVideoTrim { .. }
            | Op::SetVideoPoster { .. } => self.changes.now = Aspects::DEVELOP,
            // (not the photo itself, but what is kept beside it: filters test a photo's remote link state)
            Op::SetRemote { photo, .. } => {
                self.changes.now = Aspects::META;
                self.touch(*photo);
            }
            Op::SetPreview { id, .. } => {
                self.changes.now = Aspects::FILE;
                self.touch(*id);
            }
            // (its ops say for themselves, one by one)
            Op::Batch { .. } => {}
            Op::AddAlbum { .. }
            | Op::RemoveAlbum { .. }
            | Op::RenameAlbum { .. }
            | Op::MoveAlbum { .. }
            | Op::SetAlbumOrder { .. }
            | Op::SetAlbumPhotos { .. }
            | Op::SetAlbumPhotoOrder { .. }
            | Op::SetAlbumCover { .. }
            | Op::SetAlbumCreation { .. }
            | Op::SetAlbumRules { .. }
            | Op::AddStack { .. }
            | Op::RemoveStack { .. }
            | Op::SetStack { .. }
            | Op::SetLabelName { .. }
            | Op::SetBrowsed { .. }
            | Op::SetFolderRecord { .. }
            | Op::SetSavedLocation { .. } => self.changes.broad = self.revision.saturating_add(1),
            // (no photo matches, counts or sorts by it: only lists of keywords change)
            Op::SetKeyword { .. } => self.changes.keyword_list = self.revision.saturating_add(1),
            // (its reference photo matches the library filter and the smart-collection rule by it; the photos that
            // use the map say so with their settings)
            Op::AddVignettingMap { record, .. } => {
                if let Some(p) = vignetting_maps::reference_of_record(self, record) {
                    self.changes.now = Aspects::DEVELOP;
                    self.touch(p);
                }
            }
            Op::SetVignettingMapState { id, .. } => {
                if let Some(p) = reference_photo(self, id) {
                    self.changes.now = Aspects::DEVELOP;
                    self.touch(p);
                }
            }
        }
        match &op {
            // (a file name breaks every key's ties)
            Op::AddPhoto { .. } | Op::RemovePhoto { .. } | Op::SetCaptured { .. } | Op::SetFile { .. } | Op::Relink { .. } => {
                self.sort_cache.bump(true)
            }
            Op::SetRating { .. } | Op::SetDevelop { .. } | Op::SetContent { .. } => self.sort_cache.bump(false),
            _ => {}
        }
        Ok(match op {
            Op::AddPhoto { mut photo } => {
                if self.photos.contains_key(&photo.id) {
                    return Err(CatalogError::Invalid(format!("photo {:?} exists", photo.id)));
                }
                photo.trim_history();
                let id = photo.id;
                // the largest id would leave no next one (a damaged log or a hostile op)
                let next = id.0.checked_add(1).ok_or_else(|| CatalogError::Invalid(format!("photo id {} is out of range", id.0)))?;
                self.next_photo = self.next_photo.max(next);
                self.index.file_changed(id, None, file_path(&photo.source));
                self.photos.insert(id, Arc::new(*photo));
                Op::RemovePhoto { id }
            }
            Op::RemovePhoto { id } => {
                let p = self.photos.remove(&id).ok_or(CatalogError::NoPhoto(id))?;
                self.index.file_changed(id, file_path(&p.source), None);
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
                // every edit lands here, also settings built in code (a mask added a command at a
                // time): none keeps a list past its limit (issue #2121)
                let settings = if settings.over_list_limits() {
                    let mut s = (*settings).clone();
                    s.cap_lists();
                    std::sync::Arc::new(s)
                } else {
                    settings
                };
                let p = self.photo_mut(id)?;
                let old = std::mem::replace(&mut p.develop, settings);
                let old_edit = std::mem::replace(&mut p.edited, edited);
                Op::SetDevelop { id, settings: old, label, edited: old_edit }
            }
            Op::SetMeta { id, mut meta } => {
                let p = self.photo_mut(id)?;
                meta.gps_removed = match (p.meta.gps, meta.gps) {
                    (_, Some(_)) => None,
                    (Some(was), None) => Some(was),
                    (None, None) => p.meta.gps_removed,
                };
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
                let old = std::mem::replace(&mut p.history, history);
                let old_cursor = p.history_cursor;
                p.trim_history();
                if p.history_cursor == old_cursor {
                    Op::SetHistory { id, history: old }
                } else {
                    Op::Batch { ops: vec![Op::SetHistory { id, history: old }, Op::SetHistoryCursor { id, cursor: old_cursor }] }
                }
            }
            Op::PushHistory { id, step } => {
                let p = self.photo_mut(id)?;
                let old = p.history.clone();
                let old_cursor = p.history_cursor.take();
                // a new edit made from an earlier step drops the steps after it
                if let Some(c) = old_cursor {
                    p.history.truncate(c.saturating_add(1));
                }
                p.history.push(step);
                if p.history.len() > HISTORY_LIMIT {
                    let n = p.history.len() - HISTORY_LIMIT;
                    p.history.drain(..n);
                }
                if old_cursor.is_none() {
                    Op::SetHistory { id, history: old }
                } else {
                    Op::Batch { ops: vec![Op::SetHistory { id, history: old }, Op::SetHistoryCursor { id, cursor: old_cursor }] }
                }
            }
            Op::SetBefore { id, before } => {
                let p = self.photo_mut(id)?;
                Op::SetBefore { id, before: std::mem::replace(&mut p.before, before) }
            }
            Op::SetExported { id, exported } => {
                let p = self.photo_mut(id)?;
                Op::SetExported { id, exported: std::mem::replace(&mut p.exported, exported) }
            }
            Op::SetHistoryCursor { id, cursor } => {
                let p = self.photo_mut(id)?;
                Op::SetHistoryCursor { id, cursor: std::mem::replace(&mut p.history_cursor, cursor) }
            }
            Op::AddAlbum { album } => {
                if self.albums.contains_key(&album.id) {
                    return Err(CatalogError::Invalid(format!("collection {:?} exists", album.id)));
                }
                if let Some(parent) = album.parent
                    && !self.albums.get(&parent).is_some_and(|a| a.folder)
                {
                    return Err(CatalogError::Invalid("parent must be an existing folder".into()));
                }
                if let Some(rules) = &album.smart {
                    if album.folder || !album.photos.is_empty() {
                        return Err(CatalogError::Invalid("a smart collection holds rules, not photos".into()));
                    }
                    self.validate_rules(rules)?;
                }
                let id = album.id;
                // the largest id would leave no next one (a damaged log or a hostile op)
                let next = id.0.checked_add(1).ok_or_else(|| CatalogError::Invalid(format!("collection id {} is out of range", id.0)))?;
                self.next_album = self.next_album.max(next);
                self.index.album_changed(id, Some(&album.photos));
                self.albums.insert(id, album);
                Op::RemoveAlbum { id }
            }
            Op::RemoveAlbum { id } => {
                if self.albums.values().any(|a| a.parent == Some(id)) {
                    return Err(CatalogError::Invalid("folder is not empty".into()));
                }
                let a = self.albums.remove(&id).ok_or(CatalogError::NoAlbum(id))?;
                self.index.album_changed(id, None);
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
                    // no cycles (bounded: a loop elsewhere in a damaged catalog must not hang this)
                    let mut cur = Some(p);
                    let mut steps = 0usize;
                    while let Some(c) = cur {
                        if c == id || steps > self.albums.len() {
                            return Err(CatalogError::Invalid("cannot move a folder into itself".into()));
                        }
                        cur = self.albums.get(&c).and_then(|a| a.parent);
                        steps += 1;
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
                // the field, not `album_mut`: the index is updated while the album is borrowed
                let a = self.albums.get_mut(&id).ok_or(CatalogError::NoAlbum(id))?;
                if (a.folder || a.smart.is_some()) && !photos.is_empty() {
                    return Err(CatalogError::Invalid(
                        if a.folder { "folders can't hold photos" } else { "smart collections update automatically" }.into(),
                    ));
                }
                // photos taken out leave the custom order: added again, they go at the end
                let kept: Vec<PhotoId> = {
                    let members: std::collections::HashSet<PhotoId> = photos.iter().copied().collect();
                    a.photo_order.iter().copied().filter(|p| members.contains(p)).collect()
                };
                let old_photos = std::mem::replace(&mut a.photos, photos);
                self.index.album_changed(id, Some(&a.photos));
                if kept.len() == a.photo_order.len() {
                    Op::SetAlbumPhotos { id, photos: old_photos }
                } else {
                    let old_order = std::mem::replace(&mut a.photo_order, kept);
                    Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos: old_photos }, Op::SetAlbumPhotoOrder { id, order: old_order }] }
                }
            }
            Op::SetAlbumPhotoOrder { id, order } => {
                let a = self.album_mut(id)?;
                if (a.folder || a.smart.is_some()) && !order.is_empty() {
                    return Err(CatalogError::Invalid("only regular collections have a custom order".into()));
                }
                let members: std::collections::HashSet<PhotoId> = a.photos.iter().copied().collect();
                let mut seen = std::collections::HashSet::new();
                let order: Vec<PhotoId> = order.into_iter().filter(|p| members.contains(p) && seen.insert(*p)).collect();
                Op::SetAlbumPhotoOrder { id, order: std::mem::replace(&mut a.photo_order, order) }
            }
            Op::SetAlbumCover { id, cover } => {
                let a = self.album_mut(id)?;
                Op::SetAlbumCover { id, cover: std::mem::replace(&mut a.cover, cover) }
            }
            Op::SetAlbumCreation { id, creation } => {
                if let Some(c) = &creation {
                    if !CREATION_KINDS.contains(&c.kind.as_str()) {
                        return Err(CatalogError::Invalid(format!("unknown creation kind {:?}", c.kind)));
                    }
                    if c.document.len() > MAX_CREATION_BYTES {
                        return Err(CatalogError::Invalid("the layout document is too large".into()));
                    }
                }
                let a = self.album_mut(id)?;
                if creation.is_some() && (a.folder || a.smart.is_some()) {
                    return Err(CatalogError::Invalid("folders and smart collections can't hold a creation".into()));
                }
                Op::SetAlbumCreation { id, creation: std::mem::replace(&mut a.creation, creation) }
            }
            Op::SetAlbumRules { id, rules } => {
                self.validate_rules(&rules)?;
                let a = self.album_mut(id)?;
                let Some(old) = a.smart.as_mut() else {
                    return Err(CatalogError::Invalid("not a smart collection".into()));
                };
                Op::SetAlbumRules { id, rules: std::mem::replace(old, rules) }
            }
            Op::AddStack { stack } => {
                if self.stacks.contains_key(&stack.id) {
                    return Err(CatalogError::Invalid(format!("stack {:?} exists", stack.id)));
                }
                self.validate_stack(stack.id, &stack.photos)?;
                let id = stack.id;
                // the largest id would leave no next one (a damaged log or a hostile op)
                let next = id.0.checked_add(1).ok_or_else(|| CatalogError::Invalid(format!("stack id {} is out of range", id.0)))?;
                self.next_stack = self.next_stack.max(next);
                self.index.stack_changed(id, &[], &stack.photos);
                self.stacks.insert(id, stack);
                Op::RemoveStack { id }
            }
            Op::RemoveStack { id } => {
                let s = self.stacks.remove(&id).ok_or(CatalogError::NoStack(id))?;
                self.index.stack_changed(id, &s.photos, &[]);
                Op::AddStack { stack: s }
            }
            Op::SetStack { id, photos, collapsed } => {
                if !self.stacks.contains_key(&id) {
                    return Err(CatalogError::NoStack(id));
                }
                self.validate_stack(id, &photos)?;
                let s = self.stacks.get_mut(&id).ok_or(CatalogError::NoStack(id))?;
                let old_photos = std::mem::replace(&mut s.photos, photos);
                self.index.stack_changed(id, &old_photos, &s.photos);
                let old = Op::SetStack { id, photos: old_photos, collapsed: s.collapsed };
                s.collapsed = collapsed;
                old
            }
            Op::SetAnalysis { id, analysis } => {
                let p = self.photo_mut(id)?;
                Op::SetAnalysis { id, analysis: std::mem::replace(&mut p.analysis, analysis) }
            }
            Op::SetCaptured { id, captured } => {
                if let Some(c) = &captured
                    && !dates::is_capture_time(c)
                {
                    return Err(CatalogError::Invalid(format!("not a date: {c}")));
                }
                let p = self.photo_mut(id)?;
                Op::SetCaptured { id, captured: std::mem::replace(&mut p.captured, captured) }
            }
            Op::Relink { id, file_name, source, format } => {
                if let Some(p) = self.photos.get(&id) {
                    self.index.file_changed(id, file_path(&p.source), file_path(&source));
                }
                let p = self.photo_mut(id)?;
                let old_name = std::mem::replace(&mut p.file_name, file_name);
                let old_format = format.map(|f| std::mem::replace(&mut p.format, f));
                Op::Relink { id, file_name: old_name, source: std::mem::replace(&mut p.source, source), format: old_format }
            }
            Op::SetContent { id, width, height, file_size, content_hash, preview_only, preview_checked } => {
                let p = self.photo_mut(id)?;
                Op::SetContent {
                    id,
                    width: std::mem::replace(&mut p.width, width),
                    height: std::mem::replace(&mut p.height, height),
                    file_size: std::mem::replace(&mut p.file_size, file_size),
                    content_hash: std::mem::replace(&mut p.content_hash, content_hash),
                    preview_only: std::mem::replace(&mut p.preview_only, preview_only),
                    preview_checked: std::mem::replace(&mut p.preview_checked, preview_checked),
                }
            }
            Op::SetEmbeddedLens { id, lens } => {
                let p = self.photo_mut(id)?;
                Op::SetEmbeddedLens { id, lens: std::mem::replace(&mut p.embedded_lens, lens) }
            }
            Op::SetAsShotWb { id, wb } => {
                if wb.is_some_and(|(t, tint)| !(t.is_finite() && tint.is_finite())) {
                    return Err(CatalogError::Invalid("as-shot white balance is not a number".into()));
                }
                let p = self.photo_mut(id)?;
                Op::SetAsShotWb { id, wb: std::mem::replace(&mut p.as_shot_wb, wb) }
            }
            Op::SetVideoMeta { id, meta } => {
                if let Some(m) = &meta {
                    m.validate().map_err(CatalogError::Invalid)?;
                }
                let p = self.photo_mut(id)?;
                Op::SetVideoMeta { id, meta: std::mem::replace(&mut p.video, meta) }
            }
            Op::SetVideoTrim { id, trim_in, trim_out } => {
                let p = self.photo_mut(id)?;
                let Some(v) = p.video.as_mut() else { return Err(CatalogError::Invalid("not a video clip".into())) };
                model::validate_trim(v.duration, trim_in, trim_out).map_err(CatalogError::Invalid)?;
                Op::SetVideoTrim { id, trim_in: std::mem::replace(&mut v.trim_in, trim_in), trim_out: std::mem::replace(&mut v.trim_out, trim_out) }
            }
            Op::SetVideoPoster { id, time } => {
                let p = self.photo_mut(id)?;
                let Some(v) = p.video.as_mut() else { return Err(CatalogError::Invalid("not a video clip".into())) };
                model::validate_poster(v.duration, time).map_err(CatalogError::Invalid)?;
                Op::SetVideoPoster { id, time: std::mem::replace(&mut v.poster_time, time) }
            }
            Op::SetFile { id, file_name, source } => {
                if file_name.trim().is_empty() {
                    return Err(CatalogError::Invalid("empty file name".into()));
                }
                if let Some(p) = self.photos.get(&id) {
                    self.index.file_changed(id, file_path(&p.source), file_path(&source));
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
                        let l = lcc_geo::SavedLocation::new(&l.name, l.lat, l.lon, l.radius, l.private)
                            .map_err(|e| CatalogError::Invalid(e.to_string()))?;
                        self.saved_locations.insert(key, l)
                    }
                    None => self.saved_locations.remove(&key),
                };
                Op::SetSavedLocation { name, location: old }
            }
            Op::AddVignettingMap { id, mut record } => {
                if !record.fits(&id) {
                    return Err(CatalogError::Invalid(format!("vignetting map {} isn't a valid map with that id", id.short())));
                }
                if !self.vignetting_maps.contains_key(&id) {
                    // (a link to a photo that isn't here is no link)
                    record.photo = record.photo.filter(|p| self.photos.contains_key(p));
                    self.vignetting_maps.insert(id, Arc::new(*record));
                }
                // the table only grows: undo never takes a map away from the photos that use it
                Op::Batch { ops: Vec::new() }
            }
            Op::SetVignettingMapState { id, withdrawn } => match self.vignetting_maps.get_mut(&id) {
                Some(r) => {
                    let old = r.withdrawn;
                    if old != withdrawn {
                        Arc::make_mut(r).withdrawn = withdrawn;
                    }
                    Op::SetVignettingMapState { id, withdrawn: old }
                }
                None => Op::Batch { ops: Vec::new() },
            },
            Op::Batch { ops } => {
                let mut inverses = Vec::with_capacity(ops.len());
                for op in ops {
                    match self.apply_inner(op) {
                        Ok(inv) => inverses.push(inv),
                        Err(e) => {
                            // roll back what was applied
                            for inv in inverses.into_iter().rev() {
                                if self.apply_inner(inv).is_err() {
                                    self.changes.torn = true;
                                }
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
        let mut c: Catalog = serde_json::from_str(s).map_err(|e| CatalogError::Corrupt(e.to_string()))?;
        c.repair();
        Ok(c)
    }

    /// Make a catalog read from a snapshot hold together where a damaged or hand-edited file left
    /// it inconsistent, so nothing that relies on it can fail (code indexes stacks by the ids it
    /// finds in them, and walks folders up to the top): every photo, album and stack carries the
    /// id it is filed under; the id counters lie past every id in use; a stack keeps only photos
    /// in the catalog, once each, and goes when fewer than two are left; an album whose parent is
    /// missing, not a folder or inside itself moves to the top level; a photo keeps its newest
    /// [`HISTORY_LIMIT`] History steps. Returns what was changed (nothing, for a catalog this build
    /// wrote).
    pub fn repair(&mut self) -> Vec<String> {
        let mut changed = Vec::new();
        for (id, p) in self.photos.iter_mut() {
            if p.id != *id {
                changed.push(format!("photo {} was filed as {}", p.id.0, id.0));
                Arc::make_mut(p).id = *id;
            }
            if p.history.len() > HISTORY_LIMIT {
                let n = Arc::make_mut(p).trim_history();
                changed.push(format!("photo {}: {n} History step(s) beyond the newest {HISTORY_LIMIT} dropped", id.0));
            }
        }
        for (id, a) in self.albums.iter_mut() {
            if a.id != *id {
                changed.push(format!("collection {} was filed as {}", a.id.0, id.0));
                a.id = *id;
            }
        }
        let photos = &self.photos;
        self.stacks.retain(|id, st| {
            if st.id != *id {
                changed.push(format!("stack {} was filed as {}", st.id.0, id.0));
                st.id = *id;
            }
            let mut seen = std::collections::HashSet::new();
            let before = st.photos.len();
            st.photos.retain(|p| photos.contains_key(p) && seen.insert(*p));
            if st.photos.len() != before {
                changed.push(format!("stack {}: {} missing or repeated photo(s) left out", id.0, before - st.photos.len()));
            }
            let keep = st.photos.len() >= 2;
            if !keep {
                changed.push(format!("stack {} had fewer than two photos: removed", id.0));
            }
            keep
        });
        // a photo in two stacks stays in the first
        let mut stacked = std::collections::HashSet::new();
        self.stacks.retain(|id, st| {
            let before = st.photos.len();
            st.photos.retain(|p| stacked.insert(*p));
            if st.photos.len() != before {
                changed.push(format!("stack {}: {} photo(s) already in another stack left out", id.0, before - st.photos.len()));
            }
            st.photos.len() >= 2
        });
        let parents: Vec<(AlbumId, Option<AlbumId>)> = self.albums.values().map(|a| (a.id, a.parent)).collect();
        // A folder chain is shared by every album under it, so each chain is validated once and its
        // verdict is what later albums reused: a valid chain encoded in a flat snapshot made the old
        // per-album walk quadratic in the album count (CWE-407).
        enum Chain {
            Ok,
            Bad,
        }
        let mut chains: std::collections::HashMap<AlbumId, Chain> = std::collections::HashMap::new();
        for (id, parent) in parents {
            let Some(p) = parent else { continue };
            if chains.contains_key(&id) {
                continue;
            }
            // Walk up from `p` once, recording everything seen; what it reaches decides every node
            // walked (a chain already judged by an earlier album stops the walk early — the rest of
            // the chain was judged then).
            let mut walked: Vec<AlbumId> = Vec::new();
            let ok = {
                let mut cur = Some(p);
                let mut on: std::collections::HashSet<AlbumId> = std::collections::HashSet::new();
                loop {
                    match cur {
                        None => break true,
                        Some(c) => {
                            if c == id || !on.insert(c) {
                                break false;
                            }
                            match chains.get(&c) {
                                Some(Chain::Bad) => break false,
                                Some(Chain::Ok) => break true,
                                None => {}
                            }
                            match self.albums.get(&c) {
                                Some(a) if a.folder => {
                                    walked.push(c);
                                    cur = a.parent;
                                }
                                _ => break false,
                            }
                        }
                    }
                }
            };
            for c in walked {
                chains.insert(c, if ok { Chain::Ok } else { Chain::Bad });
            }
            if !ok && let Some(a) = self.albums.get_mut(&id) {
                changed.push(format!("collection {}: its folder {} is missing, not a folder or inside it: moved to the top level", id.0, p.0));
                a.parent = None;
            }
        }
        let past = |last: Option<u64>| last.map_or(1, |v| v.saturating_add(1));
        let (photo, album, stack) = (
            past(self.photos.keys().last().map(|k| k.0)),
            past(self.albums.keys().last().map(|k| k.0)),
            past(self.stacks.keys().last().map(|k| k.0)),
        );
        if self.next_photo < photo || self.next_album < album || self.next_stack < stack {
            changed.push("id counters behind the ids in use: moved past them".into());
            self.next_photo = self.next_photo.max(photo);
            self.next_album = self.next_album.max(album);
            self.next_stack = self.next_stack.max(stack);
        }
        if !changed.is_empty() {
            // stacks and albums changed outside `apply`: the lookups are built again when asked
            self.index = index::Indexes::default();
        }
        changed
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

/// Whether `op` changes which albums a smart album tests: it adds or removes an album, or changes
/// rules (alone or in a batch).
fn changes_what_albums_test(op: &Op) -> bool {
    // listed kind by kind (as in `apply_inner`), so a new kind has to be put on one side
    match op {
        Op::AddAlbum { .. } | Op::RemoveAlbum { .. } | Op::SetAlbumRules { .. } => true,
        Op::Batch { ops } => ops.iter().any(changes_what_albums_test),
        Op::AddPhoto { .. }
        | Op::RemovePhoto { .. }
        | Op::SetDeleted { .. }
        | Op::SetLocal { .. }
        | Op::SetContent { .. }
        | Op::SetEmbeddedLens { .. }
        | Op::SetAsShotWb { .. }
        | Op::SetRating { .. }
        | Op::SetFlag { .. }
        | Op::SetLabel { .. }
        | Op::SetMeta { .. }
        | Op::SetCaptured { .. }
        | Op::SetFile { .. }
        | Op::Relink { .. }
        | Op::SetAnalysis { .. }
        | Op::SetDevelop { .. }
        | Op::SetVersions { .. }
        | Op::SetHistory { .. }
        | Op::PushHistory { .. }
        | Op::SetHistoryCursor { .. }
        | Op::SetBefore { .. }
        | Op::SetExported { .. }
        | Op::RenameAlbum { .. }
        | Op::MoveAlbum { .. }
        | Op::SetAlbumOrder { .. }
        | Op::SetAlbumPhotos { .. }
        | Op::SetAlbumCover { .. }
        | Op::AddStack { .. }
        | Op::RemoveStack { .. }
        | Op::SetStack { .. }
        | Op::SetLabelName { .. }
        | Op::SetBrowsed { .. }
        | Op::SetKeyword { .. }
        | Op::SetFolderRecord { .. }
        | Op::SetAlbumCreation { .. }
        | Op::SetAlbumPhotoOrder { .. }
        | Op::SetKind { .. }
        | Op::SetPreview { .. }
        | Op::SetRemote { .. }
        | Op::SetSavedLocation { .. }
        | Op::SetVideoMeta { .. }
        | Op::SetVideoPoster { .. }
        | Op::SetVideoTrim { .. }
        | Op::SetXmpStamp { .. }
        | Op::SetSha1 { .. }
        | Op::AddVignettingMap { .. }
        | Op::SetVignettingMapState { .. } => false,
    }
}

/// What `op` would do to `graph`, as far as loops go (which album tests which), without the
/// checks `apply` makes: an op it would refuse is refused there. `edited` gets the albums the op
/// is about, with their name when the op brings it.
fn foresee<'a>(graph: &mut AlbumGraph, op: &'a Op, edited: &mut Vec<(AlbumId, Option<&'a str>)>) {
    match op {
        Op::AddAlbum { album } => {
            // an id that is taken stays as it is: `apply` refuses the add
            if let std::collections::hash_map::Entry::Vacant(slot) = graph.leads_to.entry(album.id) {
                slot.insert(album.smart.as_deref().map(Filter::albums_tested));
                edited.push((album.id, Some(album.name.as_str())));
            }
        }
        Op::RemoveAlbum { id } => {
            graph.leads_to.remove(id);
        }
        Op::SetAlbumRules { id, rules } => {
            // only a smart album has rules: `apply` refuses them for another
            if let Some(tested) = graph.leads_to.get_mut(id).filter(|t| t.is_some()) {
                *tested = Some(rules.albums_tested());
                edited.push((*id, None));
            }
        }
        Op::Batch { ops } => ops.iter().for_each(|op| foresee(graph, op, edited)),
        _ => {}
    }
}

/// Which album tests which: what a smart album's photos depend on beyond its own rules. Made in
/// one look at the albums, for questions about all of them.
struct AlbumGraph {
    /// Every album; `Some(albums its rules test)` for a smart album, `None` for any other.
    leads_to: std::collections::HashMap<AlbumId, Option<Vec<AlbumId>>>,
}

impl AlbumGraph {
    fn of<'a>(albums: impl Iterator<Item = &'a Album>) -> AlbumGraph {
        AlbumGraph { leads_to: albums.map(|a| (a.id, a.smart.as_deref().map(Filter::albums_tested))).collect() }
    }

    /// The smart albums that lead back to themselves: those in a ring of albums leading to one
    /// another (a strongly connected part of more than one album), or testing themselves. One
    /// walk over the albums (Tarjan's, with its own stack rather than recursion: a chain of
    /// thousands of albums must not overflow the call stack).
    fn on_a_loop(&self) -> std::collections::BTreeSet<AlbumId> {
        // only what tests an album can be on a ring: number those
        let testing: Vec<(AlbumId, &[AlbumId])> =
            self.leads_to.iter().filter_map(|(id, tested)| tested.as_deref().filter(|t| !t.is_empty()).map(|t| (*id, t))).collect();
        let number: std::collections::HashMap<AlbumId, usize> = testing.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
        let leads: Vec<Vec<usize>> = testing.iter().map(|(_, t)| t.iter().filter_map(|to| number.get(to).copied()).collect()).collect();
        const UNSEEN: usize = usize::MAX;
        let n = testing.len();
        // when each was first reached, the earliest still-open album it leads back to, the open
        // ones in the order reached, and whether each ended up on a ring
        let (mut reached, mut back) = (vec![UNSEEN; n], vec![0usize; n]);
        let (mut open, mut is_open, mut ringed) = (Vec::new(), vec![false; n], vec![false; n]);
        let mut clock = 0usize;
        for root in 0..n {
            if reached[root] != UNSEEN {
                continue;
            }
            // (album, how many of its leads were followed)
            let mut walk: Vec<(usize, usize)> = vec![(root, 0)];
            while let Some(&(v, followed)) = walk.last() {
                if followed == 0 && reached[v] == UNSEEN {
                    reached[v] = clock;
                    back[v] = clock;
                    clock += 1;
                    open.push(v);
                    is_open[v] = true;
                }
                if let Some(&w) = leads[v].get(followed) {
                    if let Some(top) = walk.last_mut() {
                        top.1 += 1;
                    }
                    if reached[w] == UNSEEN {
                        walk.push((w, 0));
                    } else if is_open[w] {
                        back[v] = back[v].min(reached[w]);
                    }
                    continue;
                }
                // every lead of `v` followed: it closes, and with it the ring it is the first of
                walk.pop();
                if let Some(&(parent, _)) = walk.last() {
                    back[parent] = back[parent].min(back[v]);
                }
                if back[v] == reached[v] {
                    let mut ring = Vec::new();
                    while let Some(w) = open.pop() {
                        is_open[w] = false;
                        ring.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if ring.len() > 1 || leads[v].contains(&v) {
                        ring.into_iter().for_each(|w| ringed[w] = true);
                    }
                }
            }
        }
        testing.iter().zip(&ringed).filter(|(_, ringed)| **ringed).map(|((id, _), _)| *id).collect()
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
mod tests_album_loops;
#[cfg(test)]
mod tests_album_order;
#[cfg(test)]
mod tests_background;
#[cfg(test)]
mod tests_custom_order;
#[cfg(test)]
mod tests_folder_index;
#[cfg(test)]
mod tests_folder_records;
#[cfg(test)]
mod tests_folders;
#[cfg(test)]
mod tests_follow;
#[cfg(test)]
mod tests_format_version;
#[cfg(test)]
mod tests_index;
#[cfg(test)]
mod tests_journal;
#[cfg(test)]
mod tests_local;
#[cfg(test)]
mod tests_lock;
#[cfg(test)]
mod tests_log_thread;
#[cfg(test)]
mod tests_repair;
#[cfg(test)]
mod tests_robust;
#[cfg(test)]
mod tests_search_text;
#[cfg(test)]
mod tests_sort_cache;
#[cfg(test)]
mod tests_tally;
#[cfg(test)]
mod tests_torn_append;
#[cfg(test)]
mod tests_upgrade_backup;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_v4;
#[cfg(test)]
mod tests_video;
