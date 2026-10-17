//! The the app engine façade.
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
extern crate lcc_denoise_core as lcc_denoise;

pub mod activity;
#[cfg(not(target_arch = "wasm32"))]
pub mod apple_photos;
pub mod availability;
pub(crate) mod camera_preview;
pub mod camera_profiles;
pub mod cmd;
pub mod config;
pub mod contact_sheet;
pub mod control_auth;
pub mod creations;
pub mod crs;
pub mod crs_masks;
pub mod demo;
pub mod denoise;
pub mod desktop;
pub mod devices;
pub mod disk_delete;
pub mod display;
mod errors;
pub mod export;
pub mod export_video;
pub mod face_download;
pub mod face_tiles;
#[cfg(not(target_arch = "wasm32"))]
mod faces_index;
#[cfg(not(target_arch = "wasm32"))]
mod faces_worker;
pub mod files;
pub mod filter_plugins;
#[cfg(not(target_arch = "wasm32"))]
mod focus_scan;
pub mod fonts;
pub mod guard;
pub mod history_groups;
pub mod import;
mod import_move;
pub mod legacy;
pub mod lens_download;
pub mod lens_profiles;
pub mod library;
mod lightroom_archive;
pub mod lightroom_catalog;
pub mod lightroom_folder;
pub mod lightroom_job;
mod lightroom_smart;
mod lightroom_sqlite;
pub mod logging;
pub mod media;
pub mod memory;
pub mod merge;
mod model_download;
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
#[cfg(not(target_arch = "wasm32"))]
pub mod remote;
pub mod rename;
pub mod render_dng;
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
pub mod timelapse;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod video;
mod view;
pub mod walk;

use std::sync::Arc;

pub use cmd::{CommandInfo, CommandSpec, command_specs, find_command};
pub use errors::{AiModel, ErrorKind};
pub use fonts::{CRAFT_FONTS, CraftFont};
use lcc_catalog::{Catalog, Filter, Op, PhotoId, Sort};
use lcc_develop::DevelopSettings;
pub use media::{RenderJob, SourceLevel};
use serde_json::Value;
pub use view::{Browse, FilterChip, LibrarySource, Selection, SelectionState, filter_chips};
pub use {lcc_catalog as catalog, lcc_develop as develop, lcc_gpu as gpu, lcc_pipeline as pipeline};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("command `{0}` is not available right now: {1}")]
    Disabled(String, String),
    #[error("invalid parameters for `{cmd}`: {msg}")]
    BadParams { cmd: String, msg: String },
    #[error("{0}")]
    Catalog(#[from] lcc_catalog::CatalogError),
    /// The command's change is applied (in memory, undoable) but its journal records could not
    /// be written. They stay queued and are written by the next successful save.
    #[error("saved in memory but not written to disk: {0}; the app will retry")]
    NotSaved(String),
    /// Another process (the app, the CLI, another computer) has the library open.
    #[error("{0}")]
    LibraryInUse(String),
    /// The folder (or file) asked for is a Lightroom Classic catalog or part of one, not a
    /// library of the app (issue #891): nothing was created or opened. `catalog`: the `.lrcat`
    /// to import with `library.importLightroom`, when one was found.
    #[error("{} is a Lightroom Classic catalog, not a library of the app; import it into a library with File ▸ Import Lightroom Catalog…{}", path.display(), catalog.as_ref().map_or(String::new(), |c| format!(" ({})", c.display())))]
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
}

/// The most photos an edit may touch for the view to follow it photo by photo; a larger edit is worked out whole.
const MAX_FOLLOWED: usize = 256;

/// Source of [`Session::visible_shared`] generations (process-wide, so two sessions never share one).
static VISIBLE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static LIBRARY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct Session {
    library_generation: u64,
    /// Auto Sync: edits to the active photo also change the other selected photos (the settings
    /// that changed, nothing else).
    pub auto_sync: bool,
    /// The values Auto (`develop.auto`) last set on each photo this session: while a photo's tone
    /// sliders still hold them, Auto is shown as applied ([`Session::auto_is_current`]). Not saved.
    pub auto_applied: std::collections::HashMap<PhotoId, [f64; 8]>,
    pub catalog: Catalog,
    pub source: LibrarySource,
    pub filter: Filter,
    /// The filter Library ▸ Enable Filters (`filter.toggleEnabled`) switched off, kept to restore.
    pub filter_suspended: Option<Filter>,
    pub sort: Sort,
    pub selection: Selection,
    /// The grid order for the current source/filter/sort (cached by catalog revision).
    /// Shared, so frontends can hold it across frames without copying ([`Session::visible_shared`]).
    visible: Arc<[PhotoId]>,
    visible_key: Option<(u64, String)>,
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
    /// Photos in the current source with no filter on (cached like `visible`).
    total: Option<((u64, String), usize)>,
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
    /// Photos whose automatic XMP sidecar write failed (their folder offline, read-only or full), written again by
    /// [`Session::retry_unsaved_sidecars`] once their file is reachable.
    pub(crate) unsaved_sidecars: std::collections::BTreeSet<PhotoId>,
    /// AI denoise: the model in use, the photos that have their picture and the work in progress.
    pub(crate) denoise: denoise::State,
    /// Plug-in filter measurements (`develop.pluginFilter.analyze`) running in the background.
    pub(crate) filter_analyses: cmd::plugin_filters::Analyses,
    /// Where the host keeps face models (one folder each); `None` where there is no file system (the web).
    pub face_models_dir: Option<std::path::PathBuf>,
    /// Face model downloads started this session (the staged files wait in `<face_models_dir>/.downloads`).
    pub face_downloads: face_download::Downloads,
    /// Settings ▸ Lens profiles: the lens database folder and its download ([`lens_download`]).
    pub lens_db: lens_download::LensSettings,
    /// The lens database generation ([`lens_profiles::generation`]) each photo's lens data was last looked up with.
    pub(crate) lens_seen: std::collections::HashMap<PhotoId, u64>,
    /// Whether each photo looked up this session carries its file\'s own lens correction (not a database profile).
    pub(crate) lens_from_file: std::collections::HashMap<PhotoId, bool>,
    /// Reverse geocoding: the parsed place names and their download (`metadata.*PlaceNames`).
    pub place_names: cmd::geocode::PlaceNames,
    /// The user's own list of models to download (`catalog.json` in the models folder), as last read.
    pub(crate) face_catalog: lcc_faces::catalog::Catalog,
    /// The loaded recognition model and the face embeddings made with it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) faces: faces_index::FacesState,
    /// Remote services: SHA-1 back-fill and Immich jobs ([`remote`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub remote: remote::State,
    /// Auto updates: the last check, the download in flight, and the settings ([`update`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub updater: update::Updater,
    /// Showing files in the file manager and starting other programs ([`desktop`]); headless
    /// harnesses swap in a [`desktop::Recorder`] so tests never open desktop windows.
    pub desktop: std::sync::Arc<dyn desktop::Desktop>,
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
    pub copy_groups: Vec<lcc_develop::SettingsGroup>,
    /// Built-in then user presets; its generation tells savers and caches when it changed.
    pub presets: presets::PresetList,
    /// Favourite profile ids (persisted with the library, like preset favourites).
    pub profile_favorites: Vec<String>,
    /// Recently applied profile ids, newest first (at most [`presets::RECENT_PROFILES`]).
    pub profile_recent: Vec<String>,
    /// Executed commands (actions / debugging / replay).
    pub journal: Vec<(String, Value)>,
    /// Actions and Edit In presets (P4.4/P4.5, [`cmd::actions`]).
    pub workflow: cmd::actions::Workflow,
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
    /// The stable id of the open library as plug-ins' data is kept (`""`: none). The plug-in
    /// manager is process-wide; each call made for this session points it at this library first.
    pub(crate) plugin_library: String,
    /// XMP sidecar preferences (persisted with the library).
    pub xmp: sidecar::XmpPrefs,
    /// Parameters of the last export (`app.export` params, minus targets), persisted in prefs.json.
    pub last_export: Option<serde_json::Value>,
    /// The photos of the last export (the Previous Export source), persisted in view.json.
    pub previous_export: Vec<PhotoId>,
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
    /// The crop tool's saved and hidden aspect ratios (`crop.saveAspect`…), persisted in prefs.json.
    pub crop_aspects: cmd::crop_aspects::CropAspects,
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
    pub target_album: Option<lcc_catalog::AlbumId>,
    /// Auto import: files seen in the watched folder and their size then (a file is imported once
    /// its size held between two scans).
    pub auto_import_seen: std::collections::HashMap<String, u64>,
    /// Keyword sets (⌥1–⌥9 apply the current set's keywords), the one in use (`None` = Recent
    /// Keywords) and the recently added keywords, newest first.
    pub keyword_sets: Vec<cmd::keywords::KeywordSet>,
    pub keyword_set: Option<String>,
    pub recent_keywords: Vec<String>,
    /// The keyword ⇧K toggles on the selected photos (Keywording ▸ keyword shortcut).
    pub keyword_shortcut: Option<String>,
    /// Keyword attributes (synonyms, export options, person) and keywords created before any
    /// photo has them, by keyword path (Keyword List).
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
    /// Video playback, trim and poster (M14.4/M14.6): the decoder backend and the player.
    pub video: video::VideoState,
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
    /// The preview store settings (persisted in prefs.json).
    pub preview_prefs: cmd::previews::PreviewPrefs,
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
    /// (lcc-cli, a script) never ends in the middle of one.
    #[cfg(not(target_arch = "wasm32"))]
    pub apple_photos_background: bool,
    /// Moves a file to the system trash (`photo.deleteFromDisk`). `None` in wasm and sessions without a filesystem.
    pub trash: Option<disk_delete::TrashFn>,
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

    /// The sliders Auto sets, in [`lcc_pipeline::auto::AutoTone`] order.
    pub fn auto_values(d: &DevelopSettings) -> [f64; 8] {
        let (l, c) = (&d.light, &d.color);
        [l.exposure, l.contrast, l.highlights, l.shadows, l.whites, l.blacks, c.vibrance, c.saturation]
    }

    /// Whether `id`'s tone sliders still hold what Auto set on it this session (any edit to them
    /// makes Auto available again).
    pub fn auto_is_current(&self, id: PhotoId) -> bool {
        let Some(applied) = self.auto_applied.get(&id) else { return false };
        self.develop_of(id).is_some_and(|d| Self::auto_values(&d) == *applied)
    }

    pub fn new() -> Session {
        // installed pixel-filter plug-ins become the renders' filter host
        let _ = filter_plugins::registry();
        Session {
            library_generation: LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            auto_sync: false,
            auto_applied: Default::default(),
            catalog: Catalog::new(),
            source: LibrarySource::All,
            filter: Filter::default(),
            filter_suspended: None,
            sort: Sort::default(),
            selection: Selection::default(),
            visible: Vec::new().into(),
            visible_key: None,
            hold_view: false,
            base: None,
            view_followed: 0,
            view_worked_out: 0,
            #[cfg(test)]
            never_follow: false,
            visible_gen: 0,
            total: None,
            undo: Vec::new(),
            commits: 0,
            redo: Vec::new(),
            pending_sidecar_repairs: Vec::new(),
            interaction: None,
            skip_auto_write: false,
            unsaved_sidecars: Default::default(),
            denoise: Default::default(),
            filter_analyses: Default::default(),
            face_models_dir: None,
            face_downloads: Default::default(),
            lens_db: Default::default(),
            lens_seen: Default::default(),
            lens_from_file: Default::default(),
            place_names: Default::default(),
            face_catalog: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            faces: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            remote: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            updater: Default::default(),
            desktop: desktop::default_desktop(),
            #[cfg(not(target_arch = "wasm32"))]
            focus_scan: Default::default(),
            clipboard: None,
            meta_clipboard: None,
            browse: None,
            library_folder: None,
            previous_active: None,
            copy_groups: lcc_develop::SettingsGroup::default_copy(),
            presets: presets::PresetList::new(presets::builtin()),
            profile_favorites: Vec::new(),
            profile_recent: Vec::new(),
            journal: Vec::new(),
            workflow: Default::default(),
            pending_log: Vec::new(),
            media: media::MediaCache::default(),
            clock: Box::new(|| "2026-09-30T12:00:00".to_string()),
            depth: 0,
            active_mask: None,
            segmenter: segment::Segmenter::default(),
            active_spot: None,
            library: None,
            library_identity: Arc::new(()),
            plugin_library: String::new(),
            xmp: sidecar::XmpPrefs::default(),
            last_export: None,
            previous_export: Vec::new(),
            last_merge: Default::default(),
            export_presets: Vec::new(),
            print: Default::default(),
            print_templates: Vec::new(),
            metadata_presets: Vec::new(),
            filter_presets: Vec::new(),
            curve_presets: Vec::new(),
            crop_aspects: Default::default(),
            mask_effects: Vec::new(),
            mask_tool_effects: Default::default(),
            label_sets: Vec::new(),
            signatures: Default::default(),
            lut_profiles: Vec::new(),
            target_album: None,
            auto_import_seen: Default::default(),
            keyword_sets: Vec::new(),
            keyword_set: None,
            recent_keywords: Vec::new(),
            keyword_shortcut: None,
            before: Default::default(),
            import_probes: Default::default(),
            folder_changes: None,
            activity: Default::default(),
            video: Default::default(),
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
            forget_local_days: lcc_catalog::DEFAULT_FORGET_DAYS,
            preview_prefs: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos: None,
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos_imports: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            apple_photos_background: false,
            trash: None,
        }
    }

    /// A session with the procedurally generated demo library loaded.
    /// The open library's id as plug-in data is kept (`""`: none); see [`cmd::plugins::export_hooks`].
    pub fn plugin_library(&self) -> &str {
        &self.plugin_library
    }

    pub fn with_demo() -> Session {
        let mut s = Session::new();
        demo::load(&mut s);
        s
    }

    /// Run a command by id. THE entry point for every frontend.
    pub fn execute(&mut self, id: &str, params: &Value) -> Result<Value> {
        let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        // a command that validates the photos a call names isn't held back by the selection
        cmd::video_gate_call(self, id, params).map_err(|why| EngineError::Disabled(id.to_string(), why))?;
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
                // a video's decoded source is its poster frame: a new poster time (or new stream facts) needs a new one
                Op::SetVideoPoster { id, .. } | Op::SetVideoMeta { id, .. } => invalidated.push(*id),
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
    pub fn face_view(&self, id: PhotoId, rect: lcc_geom::Rect) -> lcc_geom::Rect {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(v) = self.faces.index.view(id.0, &rect) {
            return v;
        }
        let _ = id;
        rect
    }

    /// Apply an op that is the app's own bookkeeping rather than something the user did (faces found by the background
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
        let name = lcc_catalog::dates::display_time(&created);
        versions.push(lcc_catalog::Version { name, created, settings: p.develop.clone(), auto: true });
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

    /// Apply and journal (so it is saved) without an undo step: a change the app made by itself, not the user.
    pub(crate) fn apply_unrecorded(&mut self, op: Op) -> Result<()> {
        let fwd = op.clone();
        self.catalog.apply(op)?;
        self.pending_log.push(fwd);
        Ok(())
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
        self.drop_gone_from_selection();
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
        self.drop_gone_from_selection();
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

    /// An undo or redo that removes photos from the library (undoing an import, a virtual copy, a
    /// duplicate…) takes them out of the selection too: a selection never names photos that don't
    /// exist (found by the SIL harness, docs/testing-sil.md).
    fn drop_gone_from_selection(&mut self) {
        let catalog = &self.catalog;
        self.selection.ids.retain(|&id| catalog.photo(id).is_some());
        if self.selection.active.is_some_and(|a| !self.selection.ids.contains(&a)) {
            self.selection.active = self.selection.ids.first().copied();
            self.active_mask = None;
            self.active_spot = None;
        }
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
            cmd::folders::folder_on_disk(&f.from, &f.to).map_err(|e| EngineError::Other(format!("can't move the folder back: {e}")))?;
        }
        let undo_folder = || {
            if let Some(f) = folder {
                let _ = cmd::folders::folder_on_disk(&f.to, &f.from);
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
                if let Some(f) = folder.filter(|f| !f.from.is_empty() && !f.to.is_empty()) {
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
        let settings = Arc::new(settings);
        if let Some(i) = &self.interaction
            && i.photo == id
        {
            return self.apply_silent(Op::SetDevelop { id, settings, label: label.into(), edited: Some(now) });
        }
        let mut ops = vec![self.develop_op(id, (*settings).clone(), label).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?];
        ops.extend(self.auto_sync_ops(id, &settings, label));
        let op = if ops.len() == 1 { ops.remove(0) } else { Op::Batch { ops } };
        self.commit(label, op)
    }

    /// With Auto Sync on, the ops that carry an edit of the active photo `id` (to `new`) over to the
    /// other selected photos: only the settings that changed; never spot removal or red eye (they
    /// belong to one photo's pixels), nor the rendering process (Sync doesn't carry it either), nor
    /// history / snapshot restores.
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
        let Some(mut delta) = json_delta(&old.to_json(), &new.to_json()) else { return Vec::new() };
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
                let synced = lcc_develop::apply_partial(&d, &delta, 1.0);
                (synced != *d).then(|| self.develop_op(x, synced, label)).flatten()
            })
            .collect()
    }

    /// The op that sets a photo's develop settings and appends a History entry (for batches).
    pub fn develop_op(&self, id: PhotoId, settings: DevelopSettings, label: &str) -> Option<Op> {
        let p = self.catalog.photo(id)?;
        // a clip takes only the global grading controls and the crop: presets, paste and sync drop the rest
        let settings = Arc::new(if p.kind == lcc_catalog::MediaKind::Video { lcc_pipeline::grade::video_develop(&settings) } else { settings });
        // An edit made at a past History step (not a move between steps) rewrites the history:
        // the later groups are kept and replayed on top (see `history_groups::rewrite`).
        if let Some(c) = p.history_cursor.filter(|c| c.saturating_add(1) < p.history.len())
            && !label.starts_with("History:")
            && let Some(op) = self.history_rewrite_op(id, c, &settings, label)
        {
            return Some(op);
        }
        let step = lcc_catalog::HistoryStep { label: label.into(), settings: settings.clone() };
        Some(Op::Batch {
            ops: vec![Op::SetDevelop { id, settings, label: label.into(), edited: Some((self.clock)()) }, Op::PushHistory { id, step }],
        })
    }

    /// The op that records `settings` (labelled `label`) as an edit made at History step `at` of
    /// photo `id`: the settings land there and the later groups are replayed on top (one undo
    /// step). The cursor stays on the new step unless it is the latest.
    pub fn history_rewrite_op(&self, id: PhotoId, at: usize, settings: &Arc<DevelopSettings>, label: &str) -> Option<Op> {
        let p = self.catalog.photo(id)?;
        let r = history_groups::rewrite(&p.history, at, (**settings).clone(), label)?;
        let (mut history, mut at) = (r.history, r.at);
        let over = history.len().saturating_sub(lcc_catalog::HISTORY_LIMIT);
        history.drain(..over);
        at = at.saturating_sub(over);
        let cursor = (at.saturating_add(1) < history.len()).then_some(at);
        let set = Op::SetDevelop { id, settings: settings.clone(), label: label.into(), edited: Some((self.clock)()) };
        Some(Op::Batch { ops: vec![set, Op::SetHistory { id, history }, Op::SetHistoryCursor { id, cursor }] })
    }

    pub fn begin_interaction(&mut self, label: &str) -> Result<()> {
        if self.interaction.is_some() {
            self.end_interaction()?;
        }
        let id = self.active().ok_or_else(|| EngineError::Other("no active photo".into()))?;
        let original = self.develop_of(id).unwrap_or_default();
        self.interaction = Some(Interaction { label: label.into(), photo: id, original });
        Ok(())
    }

    /// Commit the interaction as one undo step (no-op if nothing changed).
    pub fn end_interaction(&mut self) -> Result<()> {
        let Some(i) = self.interaction.take() else { return Ok(()) };
        let Some(cur) = self.develop_of(i.photo) else { return Ok(()) };
        if *cur == *i.original {
            return Ok(());
        }
        // Restore the original silently, then commit the final value as one step.
        self.apply_silent(Op::SetDevelop { id: i.photo, settings: i.original.clone(), label: i.label.clone(), edited: None })?;
        self.set_develop(i.photo, (*cur).clone(), &i.label)
    }

    pub fn cancel_interaction(&mut self) -> Result<()> {
        if let Some(i) = self.interaction.take() {
            self.apply_silent(Op::SetDevelop { id: i.photo, settings: i.original, label: i.label, edited: None })?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- library view

    /// Photos shown in the grid/filmstrip for the current source, filter and sort.
    pub fn visible(&mut self) -> &[PhotoId] {
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
            // the view fills in as the background checks find files gone
            key.1.push_str(&format!("|{}", self.media.availability.generation()));
        }
        if self.visible_key.as_ref() != Some(&key) {
            // "in the last N days" rules count back from the session's clock
            lcc_catalog::rules::set_now(Some((self.clock)()));
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
            let own_order = matches!(self.source, LibrarySource::Album(a) if self.sort.key == lcc_catalog::SortKey::CaptureDate
                && self.filter == Filter::default()
                && self.catalog.album(a).is_some_and(|al| !al.is_smart()));
            let whole = own_order || matches!(self.source, LibrarySource::RecentlyAdded | LibrarySource::Missing);
            // A view that asks about "the last N days" (itself, or through a smart album it looks into) changes
            // with the clock as well as with edits: a photo nobody touched can stop matching. It is asked whole.
            let clocked = f.depends_on_now()
                || (!f.albums_tested().is_empty() && self.catalog.albums().any(|al| al.smart.as_deref().is_some_and(Filter::depends_on_now)));
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
                return self.visible();
            }
            if matches!(self.source, LibrarySource::Album(_))
                && self.sort.key == lcc_catalog::SortKey::CaptureDate
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
                        (Some(x), Some(y)) => lcc_catalog::query::cmp_name(x, y),
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
            #[cfg(not(target_arch = "wasm32"))]
            if let LibrarySource::Album(a) = self.source
                && let Some(sections) = cmd::publish::sections(self, a)
            {
                // a published collection: new, then modified, then published photos (stable,
                // so each section keeps the sort); the grid shows each under a header
                visible.sort_by_key(|id| sections.get(id).copied().unwrap_or(u8::MAX));
                // photos removed from the collection (or deleted) since they were published are
                // no longer members; they're listed last, under Deleted Photos to Remove
                let shown: std::collections::HashSet<PhotoId> = visible.iter().copied().collect();
                let mut gone: Vec<PhotoId> = sections
                    .iter()
                    .filter(|(id, r)| **r == 3 && !shown.contains(id) && self.catalog.photo(**id).is_some())
                    .map(|(id, _)| *id)
                    .collect();
                gone.sort();
                visible.extend(gone);
            }
            self.visible = visible.into();
            self.visible_gen = VISIBLE_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.visible_key = Some(key);
        }
        &self.visible
    }

    /// The Previous Export source's photos as a filter (nothing exported yet: an id no photo has).
    fn previous_export_filter(&self) -> Vec<PhotoId> {
        if self.previous_export.is_empty() { vec![PhotoId(u64::MAX)] } else { self.previous_export.clone() }
    }

    /// Remember `ids` as the last export (the Previous Export source); saved with the view.
    pub fn record_export(&mut self, ids: &[PhotoId]) {
        self.previous_export = ids.iter().copied().filter(|id| self.catalog.photo(*id).is_some()).collect();
        // (a held or followed view of Previous Export starts over: its photos are not the catalog's to follow)
        self.base = None;
        self.visible_key = None;
        self.total = None;
        self.save_view();
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
        if source == LibrarySource::PreviousExport {
            f.only = self.previous_export_filter();
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
        let kept = (self.base.clone(), self.visible_key.clone(), self.visible.clone(), self.visible_gen, self.view_followed, self.view_worked_out);
        self.never_follow = true;
        self.visible_key = None;
        let whole = self.visible().to_vec();
        self.never_follow = false;
        (self.base, self.visible_key, self.visible, self.visible_gen, self.view_followed, self.view_worked_out) = kept;
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
        if self.total.as_ref().map(|t| &t.0) != Some(&key) {
            let mut f = self.source.to_filter(&Filter::default(), &self.catalog);
            if self.source == LibrarySource::Folder {
                let b = self.browse.clone().unwrap_or_default();
                f.folder = Some(b.path);
                f.subfolders = b.subfolders;
            }
            if self.source == LibrarySource::LibraryFolder {
                f.library_folder = Some(self.library_folder.clone().unwrap_or_else(|| ".".into()));
            }
            if self.source == LibrarySource::PreviousExport {
                f.only = self.previous_export_filter();
            }
            let n = self.catalog.query(&f, &self.sort).len();
            self.total = Some((key, n));
        }
        self.total.as_ref().map(|t| t.1)
    }

    pub fn visible_cloned(&mut self) -> Vec<PhotoId> {
        self.visible().to_vec()
    }

    /// One page of the visible photos: how many there are in all, and up to `limit` of them from
    /// `offset` on (past the end: none). What a frontend that shows a window of a large view asks
    /// for, instead of copying the whole list (a million ids at the unfiltered view of a large
    /// library).
    pub fn visible_page(&mut self, offset: usize, limit: usize) -> (usize, Vec<PhotoId>) {
        let all = self.visible();
        let page = all.get(offset.min(all.len())..).unwrap_or(&[]);
        (all.len(), page.iter().take(limit).copied().collect())
    }

    /// Where `id` is in the visible photos (the page to ask for to show it).
    pub fn visible_position(&mut self, id: PhotoId) -> Option<usize> {
        self.visible().iter().position(|x| *x == id)
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
mod tests_catalog_cmds;
#[cfg(test)]
mod tests_classic;
#[cfg(test)]
mod tests_color;
#[cfg(test)]
mod tests_crop_angle;
#[cfg(test)]
mod tests_crop_aspects;
#[cfg(test)]
mod tests_denoise;
#[cfg(test)]
mod tests_edit_in;
#[cfg(test)]
mod tests_edit_roundtrip;
#[cfg(test)]
mod tests_export;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_export_video;
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
mod tests_fuji;
#[cfg(test)]
mod tests_history_cursor;
#[cfg(test)]
mod tests_history_groups;
#[cfg(test)]
mod tests_hold_view;
#[cfg(test)]
mod tests_immich;
#[cfg(test)]
mod tests_import;
#[cfg(test)]
mod tests_import_move;
#[cfg(test)]
mod tests_keyword_list;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_lens_db;
#[cfg(test)]
mod tests_libops;
#[cfg(test)]
mod tests_library;
#[cfg(test)]
mod tests_lightroom_folder;
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
mod tests_plugin_filters;
#[cfg(test)]
mod tests_prefs;
#[cfg(test)]
mod tests_presets_scale;
#[cfg(test)]
mod tests_previews;
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
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_timelapse;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_video_posters;
#[cfg(test)]
mod tests_view_follow;
#[cfg(test)]
mod tests_xmp;
