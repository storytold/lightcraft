//! The LightCraft engine façade.
//!
//! Every user-visible action is a command with a stable id (`photo.rate`, `develop.set`,
//! `album.create`, `mask.add`…) and JSON parameters. The egui UI, the CLI, the control channel and
//! the MCP server all go through [`Session::execute`].
//!
//! State: a [`Catalog`] (mutated only by ops, so every change is undoable and journaled), the
//! library view (filter/sort/source), the selection, the develop clipboard, presets, and caches
//! of decoded source proxies. Rendering is done by [`RenderJob`]s that are `Send` so frontends can
//! run them off the UI thread.
#![forbid(unsafe_code)]

// Model/cache types remain available without linking the optional inference crate.
#[cfg(not(feature = "denoise"))]
extern crate lightcraft_denoise_core as lightcraft_denoise;

pub mod activity;
#[cfg(not(target_arch = "wasm32"))]
pub mod apple_photos;
pub mod availability;
pub(crate) mod camera_preview;
pub mod camera_profiles;
pub mod cmd;
pub mod config;
pub mod contact_sheet;
pub mod crs;
pub mod crs_masks;
pub mod demo;
pub mod denoise;
pub mod devices;
pub mod display;
pub mod doc_path;
mod errors;
pub mod export;
pub mod face_download;
pub mod face_tiles;
#[cfg(not(target_arch = "wasm32"))]
mod faces_index;
#[cfg(not(target_arch = "wasm32"))]
mod faces_worker;
pub mod files;
#[cfg(not(target_arch = "wasm32"))]
mod focus_scan;
pub mod fonts;
pub mod guard;
pub mod import;
mod import_move;
pub mod lens_profiles;
pub mod library;
mod lightroom_archive;
pub mod lightroom_catalog;
pub mod lightroom_folder;
pub mod lightroom_job;
mod lightroom_sqlite;
pub mod logging;
pub mod map;
pub mod media;
pub mod memory;
pub mod merge;
mod model_download;
pub mod open_path;
pub mod originals;
mod pdf;
pub mod preset_import;
pub mod preset_luminar;
pub mod presets;
pub mod print;
pub mod print_color;
pub mod print_pdf;
pub mod profile_refs;
pub mod proof;
pub mod rename;
pub mod segment;
pub mod sidecar;
pub mod slide;
pub mod slide_overlay;
pub mod slide_template;
pub mod slide_title;
pub mod slideshow;
pub mod slideshow_export;
pub mod smart;
pub mod sync;
pub mod text;
mod view;
pub mod walk;

use std::sync::Arc;

pub use cmd::{CommandInfo, CommandSpec, command_specs, find_command};
pub use errors::{AiModel, ErrorKind};
pub use fonts::{CRAFT_FONTS, CraftFont};
use lightcraft_catalog::{Catalog, Filter, Op, PhotoId, Sort};
use lightcraft_develop::DevelopSettings;
pub use media::{RenderJob, SourceLevel};
use serde_json::Value;
pub use view::{Browse, FilterChip, LibrarySource, Selection, SelectionState, filter_chips};
pub use {lightcraft_catalog as catalog, lightcraft_develop as develop, lightcraft_gpu as gpu, lightcraft_pipeline as pipeline};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("command `{0}` is not available right now: {1}")]
    Disabled(String, String),
    #[error("invalid parameters for `{cmd}`: {msg}")]
    BadParams { cmd: String, msg: String },
    #[error("{0}")]
    Catalog(#[from] lightcraft_catalog::CatalogError),
    /// The command's change is applied (in memory, undoable) but its journal records could not
    /// be written. They stay queued and are written by the next successful save.
    #[error("saved in memory but not written to disk: {0}; LightCraft will retry")]
    NotSaved(String),
    /// Another process (the app, `lightcraft-cli`, another computer) has the library open.
    #[error("{0}")]
    LibraryInUse(String),
    /// The folder (or file) asked for is a Lightroom Classic catalog or part of one, not a
    /// LightCraft library (issue #891): nothing was created or opened. `catalog`: the `.lrcat`
    /// to import with `library.importLightroom`, when one was found.
    #[error("{} is a Lightroom Classic catalog, not a LightCraft library; import it into a LightCraft library with File ▸ Import Lightroom Catalog…{}", path.display(), catalog.as_ref().map_or(String::new(), |c| format!(" ({})", c.display())))]
    LightroomCatalog { path: std::path::PathBuf, catalog: Option<std::path::PathBuf> },
    /// The work was cancelled (by the person, an agent or a newer request): not a failure.
    #[error("{0}")]
    Cancelled(String),
    /// The feature needs `model`, which is not installed (or still downloading): a frontend
    /// offers its download (terms first) on [`ErrorKind::ModelMissing`], not on the message.
    #[error("{msg}")]
    ModelMissing { model: AiModel, msg: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// One undo step: the inverse op and a label.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    pub op: Op,
    /// A folder to rename on disk (`from` → `to`) before `op` is applied: Rename / Move Folder
    /// (whose `op` relinks the photos inside). Never overwrites; refused when `to` exists. An
    /// empty `to` removes the (empty) folder `from`, an empty `from` creates `to` (Create Folder).
    pub folder: Option<FolderMove>,
    /// Inverses of logged path corrections from a partially failed history step.
    pub(crate) retry_inverse: Vec<Op>,
    /// Sidecars still separated from their photo after a failed move or rollback.
    pub(crate) retry_sidecars: Vec<rename::SidecarMove>,
}

/// The one photo an undo step changes, when it changes exactly one through its edit, rating, flag,
/// label, metadata or versions (`depth` bounds nested batches).
fn single_photo(op: &Op, depth: usize) -> Option<PhotoId> {
    match op {
        Op::SetDevelop { id, .. }
        | Op::SetRating { id, .. }
        | Op::SetFlag { id, .. }
        | Op::SetLabel { id, .. }
        | Op::SetMeta { id, .. }
        | Op::SetVersions { id, .. }
        | Op::SetHistory { id, .. }
        | Op::PushHistory { id, .. }
        | Op::SetHistoryCursor { id, .. } => Some(*id),
        Op::Batch { ops } if depth < 8 => {
            let mut ids = ops.iter().map(|o| single_photo(o, depth + 1));
            let first = ids.next()??;
            ids.all(|id| id == Some(first)).then_some(first)
        }
        _ => None,
    }
}

/// A folder renamed or moved on disk as part of an undo step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderMove {
    pub from: String,
    pub to: String,
}

impl FolderMove {
    fn reversed(&self) -> Self {
        Self { from: self.to.clone(), to: self.from.clone() }
    }
}

/// An in-progress slider drag / brush stroke: one undo step when it ends.
#[derive(Clone, Debug)]
pub struct Interaction {
    pub label: String,
    pub photo: PhotoId,
    pub original: Arc<DevelopSettings>,
    /// Whether an edit during the interaction changed the crop itself. If none did, the crop the
    /// photo shows was fitted by Constrain Crop, and the final commit starts again from the original
    /// crop, so Auto Sync carries the edit and not the active photo's fitted crop.
    pub crop_edited: bool,
}

/// The most photos an edit may touch for the view to follow it photo by photo; a larger edit is worked out whole.
const MAX_FOLLOWED: usize = 256;

/// The established clock precision for relative-date views and sidebar counts.
fn clock_minute(now: &str) -> &str {
    now.get(..16).unwrap_or(now)
}

/// Whether a query can change membership without a catalog edit, including the smart albums it tests.
fn filter_depends_on_clock(filter: &Filter, catalog: &Catalog) -> bool {
    if filter.depends_on_now() {
        return true;
    }
    let mut pending = filter.albums_tested();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(smart) = catalog.album(id).and_then(|album| album.smart.as_deref()) else { continue };
        if smart.depends_on_now() {
            return true;
        }
        pending.extend(smart.albums_tested());
    }
    false
}

/// Source of [`Session::visible_shared`] generations (process-wide, so two sessions never share one).
static VISIBLE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static LIBRARY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct Session {
    library_generation: u64,
    /// Auto Sync: edits to the active photo also change the other selected photos (the settings
    /// that changed, nothing else).
    pub auto_sync: bool,
    pub catalog: Catalog,
    pub source: LibrarySource,
    pub filter: Filter,
    /// The filter `library.toggleFilters` put aside (Library > Enable Filters off); not persisted.
    pub(crate) shelved_filter: Option<Filter>,
    pub sort: Sort,
    pub selection: Selection,
    /// A view's own filter over the library's (the Map view's location filter: Tagged,
    /// Untagged, Visible on Map): `visible` keeps only the photos these rules match. It is not
    /// part of `filter` (never saved, not shown as filter chips, not in smart albums made from
    /// the view); the view that sets it clears it when it is left.
    pub narrow: Option<lightcraft_catalog::RuleSet>,
    /// The grid order for the current source/filter/sort (cached by catalog revision).
    /// Shared, so frontends can hold it across frames without copying ([`Session::visible_shared`]).
    visible: Arc<[PhotoId]>,
    visible_key: Option<(u64, String)>,
    /// The minute used for a cached view whose rules depend on the session clock.
    visible_clock_minute: Option<String>,
    /// A view holds the photos it shows until it is refreshed (`library.holdView`): an edit that makes a photo stop
    /// matching a smart album's rules, Picks or the filter bar leaves it where it is. Off: the photo goes at once.
    pub hold_view: bool,
    /// The view last worked out, the catalog's revision then, and the photos in it before stacks are arranged.
    /// A small edit is followed from it: only the photos the edit touched are asked about again and put at their
    /// place in the order ([`Session::visible`]). It is also what a held view holds ([`Session::hold_view`]).
    /// Another source, filter or sort is another view, and so is a smart album whose rules were changed.
    base: Option<(String, u64, Vec<PhotoId>)>,
    /// How many times the view followed an edit from the list it had, and how many times it was worked out from
    /// every photo (`ui.inspect` → `perf`): what tells a slow library from a slow edit.
    pub view_followed: u64,
    pub view_worked_out: u64,
    /// Tests: work the view out whole every time (what following has to equal).
    #[cfg(test)]
    never_follow: bool,
    /// Identifies the current `visible` list: a new value (unique in the process) every time it
    /// is recomputed, so frontends can key their per-view caches on it instead of hashing the ids.
    visible_gen: u64,
    /// `visible` before [`Session::narrow`] (the same list when there is none), its generation,
    /// and what `visible` was last narrowed from (base generation, narrow rules).
    visible_base: Arc<[PhotoId]>,
    visible_base_gen: u64,
    narrow_key: Option<(u64, String)>,
    /// Photos in the current source with no filter on (cached like `visible`).
    total: Option<((u64, String), usize, Option<String>)>,
    pub undo: Vec<UndoEntry>,
    /// Undo steps committed in this session, ever (the history drops its oldest past 1000, so
    /// its length can't tell how many steps a command just made; see [`Session::commits`]).
    commits: u64,
    pub redo: Vec<UndoEntry>,
    /// Exact filesystem repairs from discarded history; catalog inverses stay discarded.
    pub(crate) pending_sidecar_repairs: Vec<rename::SidecarMove>,
    pub interaction: Option<Interaction>,
    /// Set by a command whose change must not rewrite the photo's XMP sidecar even with auto-write on
    /// (a catalog-only edit of data the sidecar writer does not emit); consumed when the command ends.
    pub(crate) skip_auto_write: bool,
    /// The order of Reload from Disk runs, so an older one finishing late is dropped ([`cmd::reload`]).
    pub(crate) reload_tickets: cmd::reload::Tickets,
    /// Photos whose automatic XMP sidecar write failed (their folder offline, read-only or full), written again by
    /// [`Session::retry_unsaved_sidecars`] once their file is reachable.
    pub(crate) unsaved_sidecars: std::collections::BTreeSet<PhotoId>,
    /// AI denoise: the model in use, the photos that have their picture and the work in progress.
    pub(crate) denoise: denoise::State,
    /// Where the host keeps face models (one folder each); `None` where there is no file system (the web).
    pub face_models_dir: Option<std::path::PathBuf>,
    /// Face model downloads started this session (the staged files wait in `<face_models_dir>/.downloads`).
    pub face_downloads: face_download::Downloads,
    /// Reverse geocoding: the parsed place names and their download (`metadata.*PlaceNames`).
    pub place_names: cmd::geocode::PlaceNames,
    /// The user's own list of models to download (`catalog.json` in the models folder), as last read.
    pub(crate) face_catalog: lightcraft_faces::catalog::Catalog,
    /// The loaded recognition model and the face embeddings made with it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) faces: faces_index::FacesState,
    /// Reading focus points for photos imported before they were read (`photo.readFocusPoints`, `focus.pump`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) focus_scan: focus_scan::FocusScan,
    /// Copied develop settings (partial JSON) for Paste.
    pub clipboard: Option<Value>,
    /// The folder on disk the [`LibrarySource::Folder`] view browses.
    pub browse: Option<Browse>,
    /// The folder the [`LibrarySource::LibraryFolder`] view shows: the library's photos imported
    /// from it and from the folders inside it.
    pub library_folder: Option<String>,
    /// Copied metadata (`photo.copyMetadata`): photo.setMeta params.
    pub meta_clipboard: Option<Value>,
    /// The photo that was active before the current one (Paste Settings from Previous).
    pub previous_active: Option<PhotoId>,
    /// Groups last used for Copy (Lightroom remembers them).
    pub copy_groups: Vec<lightcraft_develop::SettingsGroup>,
    /// Built-in then user presets; its generation tells savers and caches when it changed.
    pub presets: presets::PresetList,
    /// Favourite profile ids (persisted with the library, like preset favourites).
    pub profile_favorites: Vec<String>,
    /// Recently applied profile ids, newest first (at most [`presets::RECENT_PROFILES`]).
    pub profile_recent: Vec<String>,
    /// Executed commands (actions / debugging / replay).
    pub journal: Vec<(String, Value)>,
    /// Ops applied since the last `drain_log` (for persistence).
    pending_log: Vec<Op>,
    pub media: media::MediaCache,
    /// Current time provider (ISO 8601); injectable for tests and wasm.
    pub clock: Box<dyn Fn() -> String + Send>,
    depth: u32,
    /// Selected mask (Masking panel), by mask id.
    pub active_mask: Option<u32>,
    /// AI masks (SAM 3): the model and the last photo prepared for it.
    pub segmenter: segment::Segmenter,
    /// Selected spot (Remove panel), by index into the active photo's spots.
    pub active_spot: Option<usize>,
    /// The persistent library this session writes to (`None` = in-memory only).
    pub library: Option<library::Library>,
    /// Identity of current library opening; refreshed only after a successful open.
    pub(crate) library_identity: Arc<()>,
    /// XMP sidecar preferences (persisted with the library).
    pub xmp: sidecar::XmpPrefs,
    /// Parameters of the last export (`app.export` params, minus targets), persisted in prefs.json.
    pub last_export: Option<serde_json::Value>,
    /// The options of the last Photo Merge started, per command (`merge.hdr`, `merge.panorama`,
    /// `merge.hdrPanorama`): the merge dialog opens with them and "… with Last Settings" uses
    /// them. Persisted in prefs.json.
    pub last_merge: std::collections::BTreeMap<String, serde_json::Value>,
    /// The user's export presets (built-ins: [`export::builtin_presets`]), persisted in prefs.json.
    pub export_presets: Vec<export::ExportPreset>,
    /// The Print module's current layout, image settings, page, guides and job (`print.*`). Lives
    /// with the session, so the app, CLI, control channel and MCP all see the same print.
    pub print: print::template::PrintSettings,
    /// Saved print templates (`print.template.*`), persisted in prefs.json.
    pub print_templates: Vec<print::template::PrintTemplate>,
    /// Metadata presets (`metadata.*`), persisted in prefs.json.
    pub metadata_presets: Vec<cmd::metadata::MetadataPreset>,
    /// Saved filter-bar settings (`filter.*`), persisted in prefs.json.
    pub filter_presets: Vec<cmd::filters::FilterPreset>,
    /// The user's point-curve presets (`curve.*`; built-ins: [`cmd::curves::builtin_presets`]),
    /// persisted in prefs.json.
    pub curve_presets: Vec<cmd::curves::CurvePreset>,
    /// Mask effect presets (the Masking panel's Effect menu).
    pub mask_effects: Vec<cmd::mask_effects::MaskEffect>,
    /// The effect a new mask of each kind (`brush`, `linear`…) starts with: the one last applied
    /// to a mask of that kind.
    pub mask_tool_effects: std::collections::BTreeMap<String, String>,
    /// Saved colour-label name sets.
    pub label_sets: Vec<cmd::manage::LabelSet>,
    /// Look-alike signatures by content key (Find Similar).
    pub signatures: std::collections::HashMap<String, [f32; 64]>,
    /// Imported `.cube` LUT profiles.
    pub lut_profiles: Vec<cmd::lut_profiles::LutProfile>,
    /// The target album B adds to (`None` = the Quick Collection).
    pub target_album: Option<lightcraft_catalog::AlbumId>,
    /// Photos whose GPS position was set or cleared by a command in this session (map drops,
    /// pins moved, Remove GPS, the Info panel): their sidecars take LightCraft's position as it
    /// is, so a cleared one (or an undone geotag) is removed from the sidecar too. Other photos'
    /// sidecar GPS is never removed (LightCraft doesn't read GPS from sidecars).
    pub gps_edited: std::collections::HashSet<PhotoId>,
    /// Auto import: files seen in the watched folder and their size then (a file is imported once
    /// its size held between two scans).
    pub auto_import_seen: std::collections::HashMap<String, u64>,
    /// Keyword sets (⌥1–⌥9 apply the current set's keywords), the one in use (`None` = Recent
    /// Keywords) and the recently added keywords, newest first.
    pub keyword_sets: Vec<cmd::keywords::KeywordSet>,
    pub keyword_set: Option<String>,
    pub recent_keywords: Vec<String>,
    /// Before/After: the "before" settings chosen per photo (this session; default: the photo's
    /// import state). See `cmd/before.rs`.
    pub before: std::collections::HashMap<PhotoId, Arc<DevelopSettings>>,
    /// File probes from the last import review (`library.importPreview`), reused by the import.
    pub import_probes: std::collections::HashMap<String, media::ProbeInfo>,
    /// The last Synchronize Folder scan, which `folder.synchronize` acts on while it is current
    /// (see [`Session::take_folder_changes`]).
    pub folder_changes: Option<sync::FolderChanges>,
    /// Background tasks in flight (imports, exports, preview builds…), for the activity stack and
    /// `activity.list` / `activity.cancel`.
    pub activity: activity::Activity,
    /// The last (or running) slideshow export (`slideshow.export`).
    pub slideshow_export: Option<std::sync::Arc<slideshow_export::ExportRun>>,
    /// The last (or running) Build Previews.
    pub preview_build: Option<std::sync::Arc<cmd::previews::PreviewBuild>>,
    /// Develop defaults applied on import (persisted in prefs.json).
    pub import_defaults: import::ImportDefaults,
    /// Disk budget of the library's thumbnail cache in MB (0 = default; persisted in prefs.json).
    pub cache_mb: u32,
    /// Slideshow playback settings (`slideshow.playback`; persisted in prefs.json).
    pub slideshow: slideshow::PlaybackSettings,
    /// Slideshow slide layout: Layout, Options and Backdrop (`slideshow.layout`; persisted in
    /// prefs.json).
    pub slide_layout: slide::SlideSettings,
    /// Slideshow overlays (identity plate, rating stars, text overlays, watermark).
    pub slide_overlays: slide_overlay::OverlaySettings,
    /// Slideshow intro and ending screens (`slideshow.titles`; persisted in prefs.json).
    pub slide_titles: slide_title::TitleSettings,
    /// Where user slideshow templates are kept (`<config>/slideshow-templates`;
    /// [`Session::with_default_slide_templates`]). `None`: the built-in templates only.
    pub slide_templates_dir: Option<std::path::PathBuf>,
    /// The slideshow settings before the last template was applied ("Previous settings"; this
    /// session only: library preferences have no undo).
    pub slide_templates_previous: Option<slide_template::SlideLook>,
    /// The templates as last listed.
    pub(crate) slide_templates_cache: slide_template::Cache,
    /// The background image the slides last rendered (shared by slide jobs: rendered once).
    pub slide_backdrop: std::sync::Arc<slide::BackdropCache>,
    /// Where smart previews are kept when not in the library folder (persisted in prefs.json).
    pub smart_previews_dir: Option<std::path::PathBuf>,
    /// Untouched Local records of folders not browsed for this many days are forgotten when the
    /// library opens (0 = never; persisted in prefs.json). See `cmd/browse.rs`.
    pub forget_local_days: u32,
    /// The map's tile source (persisted in prefs.json), tile cache folder and tile service.
    pub map: map::MapState,
    /// Adds exported files to Apple Photos ([`apple_photos`]): `/usr/bin/osascript`, installed by
    /// [`Session::with_fs`] on macOS; `None` elsewhere and in sessions without a file system. Tests
    /// put a fake here.
    #[cfg(not(target_arch = "wasm32"))]
    pub apple_photos: Option<apple_photos::Runner>,
    /// This session's Apple Photos imports, running or finished (`export.photosImports`).
    #[cfg(not(target_arch = "wasm32"))]
    pub apple_photos_imports: apple_photos::Imports,
    /// Apple Photos imports run on a worker by default (`export.addToPhotos` without `wait`,
    /// `app.export` without `background`): set by the desktop app, which answers control requests
    /// on its UI thread. Off (the default), they wait for Photos, so a one-shot process
    /// (lightcraft-cli, a script) never ends in the middle of one.
    #[cfg(not(target_arch = "wasm32"))]
    pub apple_photos_background: bool,
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    pub fn library_generation(&self) -> u64 {
        self.library_generation
    }

    pub fn new() -> Session {
        Session {
            library_generation: LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            auto_sync: false,
            catalog: Catalog::new(),
            source: LibrarySource::All,
            filter: Filter::default(),
            shelved_filter: None,
            sort: Sort::default(),
            selection: Selection::default(),
            narrow: None,
            visible: Vec::new().into(),
            visible_key: None,
            visible_clock_minute: None,
            hold_view: false,
            base: None,
            view_followed: 0,
            view_worked_out: 0,
            #[cfg(test)]
            never_follow: false,
            visible_gen: 0,
            visible_base: Vec::new().into(),
            visible_base_gen: 0,
            narrow_key: None,
            total: None,
            undo: Vec::new(),
            commits: 0,
            redo: Vec::new(),
            pending_sidecar_repairs: Vec::new(),
            interaction: None,
            skip_auto_write: false,
            reload_tickets: Default::default(),
            unsaved_sidecars: Default::default(),
            denoise: Default::default(),
            face_models_dir: None,
            face_downloads: Default::default(),
            place_names: Default::default(),
            face_catalog: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            faces: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            focus_scan: Default::default(),
            clipboard: None,
            meta_clipboard: None,
            browse: None,
            library_folder: None,
            previous_active: None,
            copy_groups: lightcraft_develop::SettingsGroup::default_copy(),
            presets: presets::PresetList::new(presets::builtin()),
            profile_favorites: Vec::new(),
            profile_recent: Vec::new(),
            journal: Vec::new(),
            pending_log: Vec::new(),
            media: media::MediaCache::default(),
            clock: Box::new(|| "2026-09-30T12:00:00".to_string()),
            depth: 0,
            active_mask: None,
            segmenter: segment::Segmenter::default(),
            active_spot: None,
            library: None,
            library_identity: Arc::new(()),
            xmp: sidecar::XmpPrefs::default(),
            last_export: None,
            last_merge: Default::default(),
            export_presets: Vec::new(),
            print: Default::default(),
            print_templates: Vec::new(),
            metadata_presets: Vec::new(),
            filter_presets: Vec::new(),
            curve_presets: Vec::new(),
            mask_effects: Vec::new(),
            mask_tool_effects: Default::default(),
            label_sets: Vec::new(),
            signatures: Default::default(),
            lut_profiles: Vec::new(),
            target_album: None,
            gps_edited: Default::default(),
            auto_import_seen: Default::default(),
            keyword_sets: Vec::new(),
            keyword_set: None,
            recent_keywords: Vec::new(),
            before: Default::default(),
            import_probes: Default::default(),
            folder_changes: None,
            activity: Default::default(),
            preview_build: None,
            slideshow_export: None,
            import_defaults: import::ImportDefaults::default(),
            cache_mb: 0,
            slideshow: slideshow::PlaybackSettings::default(),
            slide_layout: slide::SlideSettings::default(),
            slide_overlays: slide_overlay::OverlaySettings::default(),
            slide_titles: slide_title::TitleSettings::default(),
            slide_templates_dir: None,
            slide_templates_previous: None,
            slide_templates_cache: None,
            slide_backdrop: Default::default(),
            smart_previews_dir: None,
            forget_local_days: lightcraft_catalog::DEFAULT_FORGET_DAYS,
            map: map::MapState::default(),
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos: None,
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos_imports: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos_background: false,
        }
    }

    /// A session with the procedurally generated demo library loaded.
    pub fn with_demo() -> Session {
        let mut s = Session::new();
        demo::load(&mut s);
        s
    }

    /// Run a command by id. THE entry point for every frontend.
    pub fn execute(&mut self, id: &str, params: &Value) -> Result<Value> {
        let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        // a command that validates the photos a call names isn't held back by the selection
        if !(spec.explicit_targets && cmd::names_photos(params)) {
            (spec.enabled)(self).map_err(|why| EngineError::Disabled(id.to_string(), why))?;
        }
        let empty = Value::Object(Default::default());
        let params = if params.is_null() { &empty } else { params };
        self.run_command(id, spec.journal.then_some(params), |s| (spec.run)(s, params)).map_err(|e| match e {
            // a shared parameter check ([`Session::targets`]) names the command it ran for
            EngineError::BadParams { cmd, msg } if cmd.is_empty() => EngineError::BadParams { cmd: id.to_string(), msg },
            e => e,
        })
    }

    /// Run `f` as command `id` with what [`Session::execute`] does around every command: the
    /// panic guard, auto versions, XMP sidecar auto-write and the durable save (a failed save is
    /// `NotSaved`). Not journaled. The app's import task commits its batches this way.
    pub fn execute_fn(&mut self, id: &str, f: impl FnOnce(&mut Session) -> Result<Value>) -> Result<Value> {
        self.run_command(id, None, f)
    }

    fn run_command(&mut self, id: &str, journal: Option<&Value>, f: impl FnOnce(&mut Session) -> Result<Value>) -> Result<Value> {
        let log_start = self.pending_log.len();
        let was_active = self.active();
        self.depth += 1;
        // last-resort guard: a panic in a command is that command's error, not a crash
        let r = guard::catch(&format!("`{id}`"), || f(self)).unwrap_or_else(|e| Err(EngineError::Other(e)));
        self.depth -= 1;
        if self.depth == 0 && was_active.is_some() && self.active() != was_active {
            self.previous_active = was_active;
            if let Some(left) = was_active {
                self.auto_version(left);
            }
        }
        if r.is_ok()
            && let Some(params) = journal
            && self.depth == 0
        {
            self.journal.push((id.to_string(), params.clone()));
            if self.journal.len() > 10_000 {
                self.journal.drain(..1000);
            }
        }
        if self.depth == 0 {
            let skip = std::mem::take(&mut self.skip_auto_write);
            if r.is_ok() && !skip && self.xmp.auto_write && self.interaction.is_none() && self.pending_log.len() > log_start {
                self.auto_write_sidecars_since(log_start);
            }
        }
        if self.depth == 0 && self.library.is_some() {
            // Make the command durable before reporting success. When the command's own records
            // can't be appended, it fails with `NotSaved`: the change stays applied in memory and
            // queued, and the next save (any later command, or the frame loop) retries it. Other
            // persistence trouble (a failed compaction, an older queued change still unwritten by
            // a command that changed nothing) doesn't fail the command; `library.info` reports it.
            let produced = self.pending_log.len() > log_start;
            if let Err(e @ EngineError::NotSaved(_)) = self.persist()
                && produced
                && r.is_ok()
            {
                return Err(e);
            }
        }
        r
    }

    pub fn commands(&self) -> Vec<CommandInfo> {
        command_specs().iter().map(|c| c.info(self)).collect()
    }

    // ---------------------------------------------------------------- ops, undo

    /// Apply an op as one undoable step.
    pub fn commit(&mut self, label: &str, op: Op) -> Result<()> {
        let fwd = op.clone();
        let inv = self.apply_catalog(op)?;
        self.pending_log.push(fwd);
        self.undo.push(UndoEntry { label: label.to_string(), op: inv, folder: None, retry_inverse: Vec::new(), retry_sidecars: Vec::new() });
        self.commits = self.commits.wrapping_add(1);
        if self.undo.len() > 1000 {
            let mut dropped = self.undo.remove(0);
            self.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
        }
        for mut dropped in self.redo.drain(..) {
            self.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
        }
        Ok(())
    }

    fn apply_catalog(&mut self, op: Op) -> Result<Op> {
        // Retain only cache IDs while the catalog consumes the operation. Cloning an import
        // batch here also cloned every photo, although commit already keeps the journal copy.
        let mut invalidated = Vec::new();
        // photos whose file was replaced: the faces made from the old pixels go too (issue #1524)
        let mut replaced = Vec::new();
        let mut pending = vec![&op];
        while let Some(op) = pending.pop() {
            match op {
                Op::SetContent { id, .. } => {
                    invalidated.push(*id);
                    replaced.push(*id);
                }
                Op::SetFile { id, .. } | Op::Relink { id, .. } | Op::RemovePhoto { id } => invalidated.push(*id),
                Op::AddPhoto { photo } => invalidated.push(photo.id),
                Op::Batch { ops } => pending.extend(ops),
                _ => {}
            }
        }
        let inv = self.catalog.apply(op)?;
        for id in invalidated {
            self.media.forget(id);
        }
        #[cfg(not(target_arch = "wasm32"))]
        for id in replaced {
            self.faces.content_replaced(id);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = replaced;
        Ok(inv)
    }

    /// [`Session::commit`] for an op that goes with a folder already renamed on disk; the undo
    /// step renames it back (`folder` is the undo direction: current place → old place).
    pub(crate) fn commit_with_folder(&mut self, label: &str, op: Op, folder: FolderMove) -> Result<()> {
        self.commit(label, op)?;
        if let Some(e) = self.undo.last_mut() {
            e.folder = Some(folder);
        }
        Ok(())
    }

    /// The box to cut a face's picture from: the detector's, when the scan has looked at the face (every face is then shown
    /// equally close, however loosely or tightly its own region was drawn), else the region's own box.
    pub fn face_view(&self, id: PhotoId, rect: lightcraft_geom::Rect) -> lightcraft_geom::Rect {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(v) = self.faces.index.view(id.0, &rect) {
            return v;
        }
        let _ = id;
        rect
    }

    /// Apply an op that is LightCraft's own bookkeeping rather than something the user did (faces found by the background
    /// scan, a raw's as-shot white read by a decode, raws rechecked after an update): journaled like any op, but not
    /// an undo step, and it leaves the redo stack alone.
    pub(crate) fn apply_system(&mut self, op: Op) -> Result<()> {
        let fwd = op.clone();
        self.catalog.apply(op)?;
        self.pending_log.push(fwd);
        Ok(())
    }

    /// Leaving photo `id` after editing it: keep its settings as an automatic version (when they
    /// differ from its latest version; at most [`AUTO_VERSIONS`] auto versions, oldest dropped).
    /// Saved with the library but not an undo step.
    pub fn auto_version(&mut self, id: PhotoId) {
        let Some(p) = self.catalog.photo(id) else { return };
        if !p.is_edited() || p.versions.last().is_some_and(|v| *v.settings == *p.develop) || self.interaction.is_some() {
            return;
        }
        let mut versions = p.versions.clone();
        let created = (self.clock)();
        let name = lightcraft_catalog::dates::display_time(&created);
        versions.push(lightcraft_catalog::Version { name, created, settings: p.develop.clone(), auto: true });
        let autos = versions.iter().filter(|v| v.auto).count();
        if autos > AUTO_VERSIONS
            && let Some(i) = versions.iter().position(|v| v.auto)
        {
            versions.remove(i);
        }
        let op = Op::SetVersions { id, versions };
        if self.catalog.apply(op.clone()).is_ok() {
            self.pending_log.push(op);
        }
    }

    /// Fold the last `n` undo steps into one (commands that commit step by step because each op
    /// depends on the state the previous one left).
    /// How many undo steps were committed in this session so far: what a command that commits
    /// several steps compares before and after to merge them ([`Session::merge_undo`]).
    pub fn commits(&self) -> u64 {
        self.commits
    }

    pub fn merge_undo(&mut self, n: usize, label: &str) {
        if n < 2 || n > self.undo.len() {
            return;
        }
        let Some(entries) = self.undo.get(self.undo.len() - n..) else { return };
        // Folder moves and partially applied file steps stay on their own.
        if entries.iter().any(|e| e.folder.is_some() || !e.retry_inverse.is_empty() || !e.retry_sidecars.is_empty()) {
            return;
        }
        let tail = self.undo.split_off(self.undo.len() - n);
        let ops = tail.into_iter().rev().map(|e| e.op).collect();
        self.undo.push(UndoEntry {
            label: label.to_string(),
            op: Op::Batch { ops },
            folder: None,
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
    }

    /// Apply without recording undo (interactive previews).
    fn apply_silent(&mut self, op: Op) -> Result<()> {
        self.catalog.apply(op)?;
        Ok(())
    }

    pub fn undo_step(&mut self) -> Result<String> {
        self.undo_step_with(&rename::RealFs)
    }

    pub(crate) fn undo_step_with(&mut self, fs: &dyn rename::MoveFs) -> Result<String> {
        let mut e = self.undo.pop().ok_or_else(|| EngineError::Other("nothing to undo".into()))?;
        let redo = match self.apply_with_files(&mut e, fs) {
            Ok(r) => r,
            Err(err) => {
                self.undo.push(e);
                return Err(err);
            }
        };
        self.show_undone(&e.op);
        self.pending_log.push(e.op);
        self.redo.push(UndoEntry {
            label: e.label.clone(),
            op: redo,
            folder: e.folder.as_ref().map(FolderMove::reversed),
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
        Ok(e.label)
    }

    pub fn redo_step(&mut self) -> Result<String> {
        self.redo_step_with(&rename::RealFs)
    }

    pub(crate) fn redo_step_with(&mut self, fs: &dyn rename::MoveFs) -> Result<String> {
        let mut e = self.redo.pop().ok_or_else(|| EngineError::Other("nothing to redo".into()))?;
        let undo = match self.apply_with_files(&mut e, fs) {
            Ok(r) => r,
            Err(err) => {
                self.redo.push(e);
                return Err(err);
            }
        };
        self.show_undone(&e.op);
        self.pending_log.push(e.op);
        self.undo.push(UndoEntry {
            label: e.label.clone(),
            op: undo,
            folder: e.folder.as_ref().map(FolderMove::reversed),
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
        Ok(e.label)
    }

    /// After an undo or redo that changed one photo (its edit, rating, flag, label, metadata or
    /// versions), that photo becomes the active one, as in Lightroom Classic, so the change is on
    /// screen (issue #293). It joins the selection if it isn't in it; nothing changes for steps
    /// that touch several photos, albums or files, or for a photo not in the current view.
    fn show_undone(&mut self, op: &Op) {
        let Some(id) = single_photo(op, 0) else { return };
        if self.selection.active == Some(id) || !self.visible().contains(&id) {
            return;
        }
        if !self.selection.contains(id) {
            self.selection = Selection { ids: vec![id], active: Some(id) };
        } else {
            self.selection.active = Some(id);
        }
        self.active_mask = None;
        self.active_spot = None;
    }

    /// Apply an undo/redo op, first renaming the step's folder and moving the files its renames
    /// imply (all or nothing). Files that can't be moved back after a failure are reported and the
    /// library follows them (a logged change outside the undo history), so none goes missing; the
    /// folder is only moved back when no file was left behind at its new path.
    fn apply_with_files(&mut self, entry: &mut UndoEntry, fs: &dyn rename::MoveFs) -> Result<Op> {
        // The entry is detached from self.undo/self.redo: include it before any global
        // repair can retire, then retry revalidates it before moving its own packets.
        rename::validate_sidecars(fs, &entry.retry_sidecars).map_err(|e| EngineError::Other(format!("can't move the files back: {e}")))?;
        // Repair discarded file steps before another path change can stale their exact paths.
        // Unrelated metadata history must remain usable while a sidecar is still unwritable.
        if entry.folder.is_some() || !entry.retry_sidecars.is_empty() || !self.file_moves(&entry.op).is_empty() {
            self.repair_sidecars_with(fs).map_err(|e| EngineError::Other(format!("can't move the files back: {e}")))?;
        }
        rename::retry_sidecars(fs, &mut entry.retry_sidecars).map_err(|e| EngineError::Other(format!("can't move the files back: {e}")))?;
        let op = &entry.op;
        let folder = entry.folder.as_ref();
        if let Some(f) = folder {
            cmd::browse::folder_on_disk(&f.from, &f.to).map_err(|e| EngineError::Other(format!("can't move the folder back: {e}")))?;
        }
        let undo_folder = || {
            if let Some(f) = folder {
                let _ = cmd::browse::folder_on_disk(&f.to, &f.from);
            }
        };
        let moves = self.file_moves(op);
        if let Err(mut e) = rename::move_all(fs, &moves) {
            entry.retry_sidecars.append(&mut e.pending_sidecars);
            if e.stuck.is_empty() {
                undo_folder();
            }
            let (message, mut inverses) = self.follow_stuck(op, e, false);
            // A later successful retry observes the corrected paths. Retain their old paths
            // now, so the opposite history direction still restores the entire original step.
            inverses.append(&mut entry.retry_inverse);
            entry.retry_inverse = inverses;
            return Err(EngineError::Other(format!("can't move the files back: {message}")));
        }
        match self.apply_catalog(op.clone()) {
            Ok(inv) => {
                if let Some(f) = folder {
                    cmd::browse::follow_folder(self, &f.from, &f.to);
                }
                if entry.retry_inverse.is_empty() {
                    Ok(inv)
                } else {
                    let mut ops = match inv {
                        Op::Batch { ops } => ops,
                        op => vec![op],
                    };
                    ops.append(&mut entry.retry_inverse);
                    Ok(Op::Batch { ops })
                }
            }
            Err(e) => {
                let back: Vec<(String, String)> = moves.iter().rev().map(|(a, b)| (b.clone(), a.clone())).collect();
                if let Err(mut be) = rename::move_all(fs, &back) {
                    entry.retry_sidecars.append(&mut be.pending_sidecars);
                    return Err(EngineError::Other(format!("{e}; the files could not all be moved back: {}", be.message)));
                }
                undo_folder();
                Err(e)
            }
        }
    }

    /// Ops applied since the last call, for the op-log store.
    pub fn drain_log(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.pending_log)
    }

    // ---------------------------------------------------------------- develop edits

    /// The photo being edited (the active photo).
    pub fn active(&self) -> Option<PhotoId> {
        self.selection.active.filter(|id| self.catalog.photo(*id).is_some())
    }

    pub fn develop_of(&self, id: PhotoId) -> Option<Arc<DevelopSettings>> {
        self.catalog.photo(id).map(|p| p.develop.clone())
    }

    /// Change a photo's develop settings. During an interaction the change is previewed without an
    /// undo step; otherwise it is committed with a history entry.
    pub fn set_develop(&mut self, id: PhotoId, settings: DevelopSettings, label: &str) -> Result<()> {
        let now = (self.clock)();
        // Auto Sync carries the edit as asked; each photo then fits its own crop (review of #1990)
        let requested = settings;
        let settings = Arc::new(self.fit_constrained_crop(id, requested.clone()));
        let crop_edited = self.develop_of(id).is_some_and(|d| d.crop.geometry != requested.crop.geometry);
        if let Some(i) = &mut self.interaction
            && i.photo == id
        {
            i.crop_edited |= crop_edited;
            return self.apply_silent(Op::SetDevelop { id, settings, label: label.into(), edited: Some(now) });
        }
        let mut ops = vec![self.develop_op(id, (*settings).clone(), label).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?];
        ops.extend(self.auto_sync_ops(id, &requested, label));
        let op = if ops.len() == 1 { ops.remove(0) } else { Op::Batch { ops } };
        self.commit(label, op)
    }

    /// "Constrain Crop" keeps the stored crop inside the warped photo, as Lightroom does (issue
    /// #1927): turning it on, or an edit that changes the warp (Geometry, Upright, lens corrections,
    /// their sections' eyes) while it is on, shrinks the crop about its centre until no blank corner
    /// is left, so the Crop tool shows what the render keeps. An edit of the crop itself is left as
    /// drawn (the render still constrains it).
    fn fit_constrained_crop(&self, id: PhotoId, mut d: DevelopSettings) -> DevelopSettings {
        if !d.geometry.constrain_crop {
            return d;
        }
        let Some(photo) = self.catalog.photo(id) else { return d };
        let before = &photo.develop;
        let turned_on = !before.geometry.constrain_crop;
        let warp_changed = before.geometry != d.geometry
            || before.optics != d.optics
            || before.orientation != d.orientation
            || ["geometry", "optics"].iter().any(|s| before.section_enabled(s) != d.section_enabled(s));
        let crop_edited = before.crop.geometry != d.crop.geometry;
        if !(turned_on || (warp_changed && !crop_edited)) {
            return d;
        }
        let frame = lightcraft_pipeline::geometry::Frame::with_lens(
            photo.width.max(1) as usize,
            photo.height.max(1) as usize,
            &d,
            true,
            photo.embedded_lens.as_ref(),
        );
        d.crop.geometry.rect = frame.crop.rect;
        d
    }

    /// With Auto Sync on, the ops that carry an edit of the active photo `id` (to `new`, as asked,
    /// before its crop is fitted) over to the other selected photos: only the settings that changed;
    /// never spot removal or red eye (they belong to one photo's pixels), nor the rendering process
    /// (Sync doesn't carry it either), nor history / snapshot restores. Constrain Crop then fits each
    /// photo's own crop: the active photo's fitted crop is never copied over another's.
    fn auto_sync_ops(&self, id: PhotoId, new: &DevelopSettings, label: &str) -> Vec<Op> {
        if !self.auto_sync
            || self.active() != Some(id)
            || self.selection.ids.len() < 2
            || label.starts_with("History:")
            || label.starts_with("Restore ")
        {
            return Vec::new();
        }
        let Some(old) = self.develop_of(id) else { return Vec::new() };
        let mut old_json = old.to_json();
        let mut new_json = new.to_json();
        // False is omitted from stored profile JSON. Keep it explicit for this comparison so
        // switching camera local tone mapping off is a change without copying untouched values.
        old_json["profile"]["camera_local_tone"] = Value::Bool(old.profile.camera_local_tone);
        new_json["profile"]["camera_local_tone"] = Value::Bool(new.profile.camera_local_tone);
        // Compare actual HDR leaves even when persisted defaults omit the whole HDR object.
        old_json["hdr"] = serde_json::json!(old.hdr);
        new_json["hdr"] = serde_json::json!(new.hdr);
        let Some(mut delta) = json_delta(&old_json, &new_json) else { return Vec::new() };
        if let Some(o) = delta.as_object_mut() {
            for k in ["spots", "red_eye", "version", "process"] {
                o.remove(k);
            }
            if o.is_empty() {
                return Vec::new();
            }
        }
        self.selection
            .ids
            .iter()
            .filter(|x| **x != id)
            .filter_map(|x| self.develop_of(*x).map(|d| (*x, d)))
            .filter_map(|(x, d)| {
                let synced = self.fit_constrained_crop(x, lightcraft_develop::apply_partial(&d, &delta, 1.0));
                (synced != *d).then(|| self.develop_op(x, synced, label)).flatten()
            })
            .collect()
    }

    /// The op that sets a photo's develop settings and appends a History entry (for batches).
    pub fn develop_op(&self, id: PhotoId, settings: DevelopSettings, label: &str) -> Option<Op> {
        self.catalog.photo(id)?;
        let settings = Arc::new(settings);
        let step = lightcraft_catalog::HistoryStep { label: label.into(), settings: settings.clone() };
        Some(Op::Batch {
            ops: vec![Op::SetDevelop { id, settings, label: label.into(), edited: Some((self.clock)()) }, Op::PushHistory { id, step }],
        })
    }

    pub fn begin_interaction(&mut self, label: &str) -> Result<()> {
        if self.interaction.is_some() {
            self.end_interaction()?;
        }
        let id = self.active().ok_or_else(|| EngineError::Other("no active photo".into()))?;
        let original = self.develop_of(id).unwrap_or_default();
        self.interaction = Some(Interaction { label: label.into(), photo: id, original, crop_edited: false });
        Ok(())
    }

    /// Commit the interaction as one undo step (no-op if nothing changed).
    pub fn end_interaction(&mut self) -> Result<()> {
        let Some(i) = self.interaction.take() else { return Ok(()) };
        let Some(cur) = self.develop_of(i.photo) else { return Ok(()) };
        if *cur == *i.original {
            return Ok(());
        }
        // Restore the original silently, then commit the final value as one step. A crop nobody
        // edited goes back to the original's: Constrain Crop fits it again for the final warp, and
        // Auto Sync carries the edit, not the active photo's fitted crop (review of #1990).
        self.apply_silent(Op::SetDevelop { id: i.photo, settings: i.original.clone(), label: i.label.clone(), edited: None })?;
        let mut last = (*cur).clone();
        if !i.crop_edited {
            last.crop.geometry = i.original.crop.geometry;
        }
        self.set_develop(i.photo, last, &i.label)
    }

    pub fn cancel_interaction(&mut self) -> Result<()> {
        if let Some(i) = self.interaction.take() {
            self.apply_silent(Op::SetDevelop { id: i.photo, settings: i.original, label: i.label, edited: None })?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- library view

    /// Library > Enable Filters is off: a filter is put aside (`library.toggleFilters`).
    pub fn filters_off(&self) -> bool {
        self.shelved_filter.is_some()
    }

    /// Photos shown in the grid/filmstrip for the current source, filter and sort, narrowed by
    /// [`Session::narrow`] when a view sets it.
    pub fn visible(&mut self) -> &[PhotoId] {
        self.visible_base();
        let key = (self.visible_base_gen, self.narrow.as_ref().map(|n| format!("{n:?}")).unwrap_or_default());
        if self.narrow_key.as_ref() != Some(&key) {
            self.visible = match &self.narrow {
                None => self.visible_base.clone(),
                Some(rules) => {
                    lightcraft_catalog::rules::set_now(Some((self.clock)()));
                    let cat = &self.catalog;
                    self.visible_base.iter().copied().filter(|id| cat.photo(*id).is_some_and(|p| rules.matches(p, cat))).collect::<Vec<_>>().into()
                }
            };
            self.visible_gen = VISIBLE_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.narrow_key = Some(key);
        }
        &self.visible
    }

    /// The photos for the current source, filter and sort before [`Session::narrow`], with their
    /// generation (as [`Session::visible_shared`]): what the Map view counts and pins.
    pub fn visible_base_shared(&mut self) -> (u64, Arc<[PhotoId]>) {
        self.visible_base();
        (self.visible_base_gen, self.visible_base.clone())
    }

    fn visible_base(&mut self) {
        let mut key =
            (self.catalog.revision, format!("{:?}|{:?}|{:?}|{:?}|{:?}", self.source, self.filter, self.sort, self.browse, self.library_folder));
        // What a held view is told apart by: what is shown and how, and for a smart album its rules (rules that were
        // changed ask something else). Not the checks of Missing Photos below: they fill the view in, they are
        // no other view.
        let asks = matches!(self.source, LibrarySource::Picks)
            || matches!(self.source, LibrarySource::Album(a) if self.catalog.album(a).is_some_and(|al| al.is_smart()));
        let rules = match self.source {
            LibrarySource::Album(a) if asks => self.catalog.album(a).map(|al| format!("{:?}", al.smart)).unwrap_or_default(),
            _ => String::new(),
        };
        let view = format!("{}|{rules}", key.1);
        if self.source == LibrarySource::Missing && self.media.availability.is_background() {
            // the view fills in as the background checks find files gone (a file found there at
            // its first check changes nothing it shows)
            key.1.push_str(&format!("|{}", self.media.availability.gone_generation()));
        }
        let key_changed = self.visible_key.as_ref() != Some(&key);
        // A time-dependent cache needs one clock read to test its minute stamp. A new view also
        // needs one value for the query, even when its previous cached view did not use the clock.
        let current_now = (key_changed || self.visible_clock_minute.is_some()).then(|| (self.clock)());
        let current_minute = current_now.as_deref().map(clock_minute);
        let clock_changed = self.visible_clock_minute.as_deref().is_some_and(|cached| current_minute != Some(cached));
        if key_changed || clock_changed {
            // Use this same value to evaluate relative rules and stamp the result.
            let now = current_now.unwrap_or_else(|| (self.clock)());
            lightcraft_catalog::rules::set_now(Some(now.clone()));
            let f = self.view_filter(self.source, &self.filter);
            // A held view keeps the photos it shows that stopped matching what is asked of them (a smart album's
            // rules, a flag, the filter bar), as long as they are still where the view looks: in the library for
            // a view that is only a question, else in the album or folder shown. Taking a photo out of an album
            // or the library takes it out of the view at once, held or not.
            // (only a view that asks something can lose a photo to an edit: the others are queried as ever, at no
            // cost for the hold)
            let holds = self.hold_view && (asks || self.filter != Filter::default());
            let within = holds.then(|| {
                if asks {
                    self.view_filter(LibrarySource::All, &Filter::default())
                } else if self.source == LibrarySource::RecentlyAdded {
                    // (how far back it reaches was worked out for `f`: not a second pass over the library)
                    Filter { imported_from: f.imported_from.clone(), ..Default::default() }
                } else {
                    self.view_filter(self.source, &Filter::default())
                }
            });
            // Views whose order or photos are not the query's alone are worked out whole each time: an album in
            // its own order, Recently Added (how far back it reaches moves with every import), Missing Photos.
            let own_order = matches!(self.source, LibrarySource::Album(a) if self.sort.key == lightcraft_catalog::SortKey::CaptureDate
                && self.filter == Filter::default()
                && self.catalog.album(a).is_some_and(|al| !al.is_smart()));
            let whole = own_order || matches!(self.source, LibrarySource::RecentlyAdded | LibrarySource::Missing);
            // A view that asks about "the last N days" (itself, or through a smart album it looks into) changes
            // with the clock as well as with edits: a photo nobody touched can stop matching. It is asked whole.
            let clocked = filter_depends_on_clock(&f, &self.catalog);
            let base = self.base.take().filter(|(of, _, _)| *of == view);
            // A small edit to the same view: the photos it touched are taken out, asked about again, and put back
            // at their place in the order. The others neither stopped matching nor moved.
            #[cfg(test)]
            let clocked = clocked || self.never_follow;
            let followed = base.as_ref().filter(|_| !whole && !clocked).and_then(|(_, revision, ids)| {
                let touched = self.catalog.touched_since(*revision).filter(|touched| touched.len() <= MAX_FOLLOWED)?;
                Some(self.catalog.follow(ids, &touched, &f, &self.sort, within.as_ref()))
            });
            match followed {
                Some(_) => self.view_followed += 1,
                None => self.view_worked_out += 1,
            }
            let mut visible = match (followed, &base, &within) {
                (Some(ids), _, _) => ids,
                (None, Some((_, _, held)), Some(within)) => {
                    let keep: std::collections::HashSet<PhotoId> = held.iter().copied().collect();
                    self.catalog.query_keeping(&f, &self.sort, &keep, within)
                }
                _ => self.catalog.query(&f, &self.sort),
            };
            // what the view shows now is what the next edit is followed from, and what it holds until it is
            // refreshed (taken before stacks are arranged, so a photo folded into a stack is held with it)
            self.base = (!whole || holds).then(|| (view, self.catalog.revision, visible.clone()));
            if visible.is_empty() && self.source == LibrarySource::LibraryFolder && !self.folder_holds_photos() {
                // the shown folder lost its last photo (deleted, moved, removed): everything, not
                // an empty grid under the name of a folder that is gone from the sidebar
                self.source = LibrarySource::All;
                self.library_folder = None;
                return self.visible_base();
            }
            if matches!(self.source, LibrarySource::Album(_))
                && self.sort.key == lightcraft_catalog::SortKey::CaptureDate
                && let LibrarySource::Album(a) = self.source
                && let Some(al) = self.catalog.album(a)
                && !al.is_smart()
                && self.filter == Filter::default()
            {
                // each photo's place looked up, not searched for (a big album sorted in O(n log n))
                let mut place: std::collections::HashMap<PhotoId, usize> = std::collections::HashMap::with_capacity(al.photos.len());
                for (i, id) in al.photos.iter().enumerate() {
                    place.entry(*id).or_insert(i);
                }
                visible.sort_by_key(|id| place.get(id).copied().unwrap_or(usize::MAX));
                if !self.sort.ascending {
                    visible.reverse();
                }
            }
            if self.source == LibrarySource::RecentlyAdded {
                // newest import first, whatever the sort direction (the grid groups by import day);
                // a batch stays in file order (name, then id)
                let cat = &self.catalog;
                visible.sort_by(|a, b| {
                    let key = |id: &PhotoId| cat.photo(*id).map(|p| p.imported.clone()).unwrap_or_default();
                    let name = |id: &PhotoId| cat.photo(*id).map(|p| p.file_name.as_str());
                    let by_name = match (name(a), name(b)) {
                        (Some(x), Some(y)) => lightcraft_catalog::query::cmp_name(x, y),
                        _ => std::cmp::Ordering::Equal,
                    };
                    key(b).cmp(&key(a)).then(by_name).then(a.cmp(b))
                });
            }
            if self.source == LibrarySource::Missing {
                // only the photos the query kept (library photos, not Local browse records) are checked
                let (cat, avail) = (&self.catalog, &self.media.availability);
                visible.retain(|id| cmd::missing::is_missing(cat, avail, *id));
            }
            if self.source != LibrarySource::RecentlyDeleted {
                visible = self.catalog.arrange_stacks(&visible);
            }
            self.visible_base = visible.into();
            self.visible_base_gen = VISIBLE_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.visible_key = Some(key);
            self.visible_clock_minute = clocked.then(|| clock_minute(&now).to_owned());
        }
    }

    /// `source` as a filter over `base` (the filter bar's, or none), with the folder it shows.
    fn view_filter(&self, source: LibrarySource, base: &Filter) -> Filter {
        let mut f = source.to_filter(base, &self.catalog);
        if source == LibrarySource::Folder {
            // no folder chosen: nothing (an empty path matches nothing)
            let b = self.browse.clone().unwrap_or_default();
            f.folder = Some(b.path);
            f.subfolders = b.subfolders;
        }
        if source == LibrarySource::LibraryFolder {
            // no folder chosen: nothing (`.` names no folder)
            f.library_folder = Some(self.library_folder.clone().unwrap_or_else(|| ".".into()));
        }
        f
    }

    /// Let go of what a held view kept: the view shows what matches now ([`Session::hold_view`]).
    pub fn refresh_view(&mut self) {
        self.base = None;
        self.visible_key = None;
    }

    /// [`Session::visible`] worked out from every photo, whatever was followed before: what following an edit
    /// has to equal.
    /// Nothing the session keeps is changed by asking: the next edit is followed from where the last one left the
    /// view, so a list followed many times over is compared, not only one followed once.
    #[cfg(test)]
    pub(crate) fn visible_in_full(&mut self) -> Vec<PhotoId> {
        let kept = (
            self.base.clone(),
            self.visible_key.clone(),
            self.visible_clock_minute.clone(),
            self.visible.clone(),
            self.visible_gen,
            self.view_followed,
            self.view_worked_out,
        );
        let kept_narrow = (self.visible_base.clone(), self.visible_base_gen, self.narrow_key.clone());
        self.never_follow = true;
        self.visible_key = None;
        let whole = self.visible().to_vec();
        self.never_follow = false;
        (self.base, self.visible_key, self.visible_clock_minute, self.visible, self.visible_gen, self.view_followed, self.view_worked_out) = kept;
        (self.visible_base, self.visible_base_gen, self.narrow_key) = kept_narrow;
        whole
    }

    /// Whether the library still holds a photo imported from the folder a `LibraryFolder` source
    /// shows.
    fn folder_holds_photos(&self) -> bool {
        let f = Filter { library_folder: self.library_folder.clone(), ..Default::default() };
        self.library_folder.is_some() && !self.catalog.query(&f, &Sort::default()).is_empty()
    }

    /// Photos in the current source (folder, album, …) before the filter bar and search narrow
    /// them; `None` where that is not a separate number (Missing Photos).
    pub fn source_total(&mut self) -> Option<usize> {
        if self.source == LibrarySource::Missing {
            return None;
        }
        if self.filter == Filter::default() && !self.hold_view {
            // nothing narrows the source: what the view lists is all of it, and is counted already (not a view
            // that holds photos that stopped matching: those are not of the source any more)
            let _ = self.visible();
            if let Some((_, revision, ids)) = &self.base
                && *revision == self.catalog.revision
            {
                return Some(ids.len());
            }
        }
        let key = (self.catalog.revision, format!("{:?}|{:?}|{:?}", self.source, self.browse, self.library_folder));
        let key_changed = self.total.as_ref().map(|total| &total.0) != Some(&key);
        let cached_minute = self.total.as_ref().and_then(|total| total.2.as_deref());
        let current_now = cached_minute.map(|_| (self.clock)());
        let current_minute = current_now.as_deref().map(clock_minute);
        let clock_changed = cached_minute.is_some_and(|cached| current_minute != Some(cached));
        if key_changed || clock_changed {
            // The source count can be requested before `visible`; bind it to the session's clock itself.
            let now = current_now.unwrap_or_else(|| (self.clock)());
            lightcraft_catalog::rules::set_now(Some(now.clone()));
            let mut f = self.source.to_filter(&Filter::default(), &self.catalog);
            if self.source == LibrarySource::Folder {
                let b = self.browse.clone().unwrap_or_default();
                f.folder = Some(b.path);
                f.subfolders = b.subfolders;
            }
            if self.source == LibrarySource::LibraryFolder {
                f.library_folder = Some(self.library_folder.clone().unwrap_or_else(|| ".".into()));
            }
            // (how many, not which: nothing to put in order)
            let n = self.catalog.count(&f);
            let clocked = filter_depends_on_clock(&f, &self.catalog);
            self.total = Some((key, n, clocked.then(|| clock_minute(&now).to_owned())));
        }
        self.total.as_ref().map(|t| t.1)
    }

    pub fn visible_cloned(&mut self) -> Vec<PhotoId> {
        self.visible().to_vec()
    }

    /// After the source changes, the active photo follows the rule every source shares: it stays
    /// when the view shows it, else the view's first photo becomes the active (and only selected)
    /// one, or nothing when the view is empty. A view never keeps an active photo it doesn't show
    /// (issue #1447: a Local folder kept a library photo active, so Detail and the info showed a
    /// photo the grid didn't).
    pub fn keep_active_in_view(&mut self) {
        let active = self.selection.active;
        let vis = self.visible();
        if active.is_some_and(|a| vis.contains(&a)) {
            return;
        }
        let first = vis.first().copied();
        self.selection = first.map(Selection::single).unwrap_or_default();
    }

    /// The visible photos without copying them, with their generation: equal generations mean
    /// the very same list (same catalog revision, source, filter, sort), so a frontend can cache
    /// whatever it derives from the list under that number.
    pub fn visible_shared(&mut self) -> (u64, Arc<[PhotoId]>) {
        self.visible();
        (self.visible_gen, self.visible.clone())
    }

    /// Targets of photo commands: the explicit `ids` (photo ids) or `id` (a photo id) param, else [`Self::selected`].
    /// An `ids` or `id` that is there but isn't photo ids is an error, never the selection or part of the list
    /// (issue #1506): a mistyped target mustn't turn a command meant for one photo into one for every selected photo.
    pub fn targets(&self, p: &Value) -> Result<Vec<PhotoId>> {
        let invalid = |what: &str| EngineError::BadParams { cmd: String::new(), msg: format!("`{what}` must be photo ids (whole numbers)") };
        match p.get("ids") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => return a.iter().map(|v| v.as_u64().map(PhotoId).ok_or_else(|| invalid("ids"))).collect(),
            Some(_) => return Err(invalid("ids")),
        }
        match p.get("id") {
            None | Some(Value::Null) => Ok(self.selected()),
            Some(v) => v.as_u64().map(|id| vec![PhotoId(id)]).ok_or_else(|| invalid("id")),
        }
    }

    /// The photos commands act on when a call names none: the selection, else the active photo.
    pub fn selected(&self) -> Vec<PhotoId> {
        if self.selection.ids.is_empty() { self.selection.active.into_iter().collect() } else { self.selection.ids.clone() }
    }
}

/// Automatic versions kept per photo.
pub const AUTO_VERSIONS: usize = 20;

/// The parts of `new` that differ from `old` (objects recurse; anything else is taken whole).
pub fn json_delta(old: &Value, new: &Value) -> Option<Value> {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let m: serde_json::Map<String, Value> =
                b.iter().filter_map(|(k, nv)| json_delta(a.get(k).unwrap_or(&Value::Null), nv).map(|d| (k.clone(), d))).collect();
            (!m.is_empty()).then_some(Value::Object(m))
        }
        (a, b) if a == b => None,
        (_, b) => Some(b.clone()),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_album_order;
#[cfg(test)]
mod tests_album_remove;
#[cfg(test)]
mod tests_albums_local;
#[cfg(test)]
mod tests_color;
#[cfg(test)]
mod tests_constrain_crop;
#[cfg(test)]
mod tests_crop_angle;
#[cfg(test)]
mod tests_crop_aspect;
#[cfg(test)]
mod tests_denoise;
#[cfg(test)]
mod tests_export;
#[cfg(test)]
mod tests_face_models;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_face_recognize;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_face_tiles;
#[cfg(test)]
mod tests_filesystem_history;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_focus_scan;
#[cfg(test)]
mod tests_folders;
#[cfg(test)]
mod tests_forget_local;
#[cfg(test)]
mod tests_history_cursor;
#[cfg(test)]
mod tests_hold_view;
#[cfg(test)]
mod tests_ids_params;
#[cfg(test)]
mod tests_import;
#[cfg(test)]
mod tests_import_move;
#[cfg(test)]
mod tests_keyword_list;
#[cfg(test)]
mod tests_libops;
#[cfg(test)]
mod tests_library;
#[cfg(test)]
mod tests_lightroom_folder;
#[cfg(test)]
mod tests_location;
#[cfg(test)]
mod tests_mask_effects;
#[cfg(test)]
mod tests_merge;
#[cfg(test)]
mod tests_move_to_share;
#[cfg(test)]
mod tests_organize;
#[cfg(test)]
mod tests_persist;
#[cfg(test)]
mod tests_prefs;
#[cfg(test)]
mod tests_presets_scale;
#[cfg(test)]
mod tests_print;
#[cfg(test)]
mod tests_process;
#[cfg(test)]
mod tests_recheck;
#[cfg(test)]
mod tests_segment;
#[cfg(test)]
mod tests_settings_files;
#[cfg(test)]
mod tests_slide;
#[cfg(test)]
mod tests_slideshow_export;
#[cfg(test)]
mod tests_spots;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_view_follow;
#[cfg(test)]
mod tests_xmp;
