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
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

// Model/cache types remain available without linking the optional inference crate.
#[cfg(not(feature = "denoise"))]
extern crate lcc_denoise_core as lcc_denoise;

pub mod activity;
pub mod app_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod apple_photos;
pub mod availability;
mod background_merge;
pub mod base64;
pub mod bounded_read;
pub(crate) mod camera_preview;
pub mod camera_profiles;
pub mod capture_one_catalog;
pub mod capture_one_develop;
pub mod change_report;
pub mod chart_profiling;
pub mod cmd;
pub mod colour_export;
pub mod concepts_store;
pub mod config;
pub mod contact_sheet;
pub mod control_auth;
pub mod creations;
pub mod crs;
pub mod crs_masks;
pub mod crs_point_color;
pub(crate) mod decode_share;
pub mod demo;
pub mod denoise;
pub mod desktop;
pub mod devices;
pub mod disk_delete;
pub mod display;
pub mod dng_update;
pub mod dng_validate;
pub mod doc_path;
pub mod duplicates;
pub mod dust_map;
pub mod email;
pub mod enhance;
mod errors;
pub mod export;
pub mod export_video;
pub mod eye_cull;
pub mod eye_prep;
pub mod eye_store;
pub mod eyes;
pub mod face_download;
pub mod face_tiles;
#[cfg(not(target_arch = "wasm32"))]
mod faces_index;
#[cfg(not(target_arch = "wasm32"))]
mod faces_worker;
pub mod files;
pub mod filter_plugins;
pub mod finishing;
#[cfg(not(target_arch = "wasm32"))]
mod focus_scan;
pub mod folder_watch;
pub mod fonts;
pub mod genfill;
pub mod guard;
pub mod hdr_texture;
pub mod history_groups;
pub mod identity;
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
mod lightroom_print;
mod lightroom_smart;
mod lightroom_sqlite;
pub mod link;
pub mod logging;
pub mod machine_test;
pub mod media;
pub mod memory;
pub mod merge;
pub mod merge_layers;
pub mod midi;
/// The shared AI backend's queue and activity row (#491).
pub mod ml;
mod model_download;
pub mod open_path;
pub mod originals;
mod pdf;
#[cfg(not(target_arch = "wasm32"))]
pub mod peer_exchange;
#[cfg(not(target_arch = "wasm32"))]
pub mod peers;
mod people_smart;
pub mod people_store;
pub mod phash;
pub mod phash_store;
pub mod placement;
pub mod prep;
pub mod preset_import;
pub mod preset_luminar;
pub mod preset_xmp;
pub mod presets;
mod presets_save;
pub mod presets_smart;
pub mod print;
pub mod print_color;
pub mod print_pdf;
pub mod profile_bodies;
pub mod profile_edits;
pub mod profile_refs;
pub mod proof;
#[cfg(not(target_arch = "wasm32"))]
pub mod remote;
pub mod rename;
pub mod render_dng;
pub mod scene_disk;
pub mod segment;
pub mod sensor_maps;
pub mod sidecar;
#[cfg(not(target_arch = "wasm32"))]
mod sidecar_writer;
pub mod slide;
pub mod slide_overlay;
pub mod slide_template;
pub mod slide_title;
pub mod slideshow;
pub mod slideshow_export;
pub mod smart;
pub mod smart_masks;
pub mod source_report;
pub mod sync;
mod sync_same;
pub mod template_folders;
pub mod text;
pub mod timelapse;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod upright_jobs;
pub mod video;
mod view;
pub mod vignetting_calibration_io;
pub mod vignetting_calibration_library;
pub mod vignetting_calibration_placement;
pub mod walk;
pub mod xmp_look;

use std::sync::Arc;

pub use cmd::plugin_filters::ROLL_KEY;
pub use cmd::{CommandInfo, CommandSpec, canonical_id, command_specs, find_command};
pub use errors::{AiModel, ErrorKind};
pub use fonts::{CRAFT_FONTS, CraftFont};
use lcc_catalog::{Catalog, Filter, Op, PhotoId, Sort};
use lcc_develop::DevelopSettings;
pub use media::{RenderJob, SourceLevel};
use serde_json::Value;
pub use view::{Browse, FilterChip, LibrarySource, Selection, SelectionState, filter_chips};
pub use {lcc_catalog as catalog, lcc_develop as develop, lcc_gpu as gpu, lcc_pipeline as pipeline};

/// [`Session::before_step`]: the commit count and the length of the queue of ops not yet written to the library's log.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BeforeStep {
    commits: u64,
    log: usize,
}

/// One step a command has just committed ([`Session::committed_step`]), to take back if the command then fails.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CommittedStep {
    /// [`Session::commits`] with the step committed.
    commits: u64,
    /// The undo stack's length with the step on top.
    undo_len: usize,
    /// Where the step's op waits in the queue for the library's log, while it hasn't been written.
    pending: Option<usize>,
}

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
    #[error("{} is a Lightroom Classic catalog, not a catalog of the app; import it into one with File ▸ Import Lightroom Catalog…{}", path.display(), catalog.as_ref().map_or(String::new(), |c| format!(" ({})", c.display())))]
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
    /// A camera profile file to put back (Replace Camera Profile): its bytes before the step.
    pub profile: Option<chart_profiling::ProfileSwap>,
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
        | Op::SetHistoryCursor { id, .. }
        | Op::SetBefore { id, .. } => Some(*id),
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
    /// Other photos edited live along with the active one (a multi-photo edit: each nudged from
    /// its own value as it goes), with their settings before. They join the same undo step.
    pub others: Vec<(PhotoId, Arc<DevelopSettings>)>,
    /// With Auto Sync on, the other selected photos and their settings when the interaction began:
    /// they follow the active photo live (each set to its value, as at the commit) and go back with it.
    pub along: Vec<(PhotoId, Arc<DevelopSettings>)>,
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
    /// The values Auto (`develop.auto`) last set on each photo this session: while a photo's tone
    /// sliders still hold them, Auto is shown as applied ([`Session::auto_is_current`]). Not saved.
    pub auto_applied: std::collections::HashMap<PhotoId, [f64; 8]>,
    pub catalog: Catalog,
    pub source: LibrarySource,
    /// The sources `library.source` showed, for Go Back / Go Forward (`library.sourceBack`); not saved.
    pub(crate) source_history: view::SourceHistory,
    pub filter: Filter,
    /// The filter `library.toggleFilters` put aside (Library > Enable Filters off); not persisted.
    pub(crate) shelved_filter: Option<Filter>,
    pub sort: Sort,
    pub selection: Selection,
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
    /// Select All (`library.selectAll`) was the last change to the selection, in the view named here: while the
    /// selection is still exactly what the view shows, photos that join the view (an import still adding them)
    /// join the selection too.
    pub(crate) all_selected: Option<String>,
    /// Show Alternatives is on: the original photo, and the view to go back to.
    pub alternatives: Option<cmd::alternatives::Alternatives>,
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
    total: Option<((u64, String), usize, Option<String>)>,
    pub undo: Vec<UndoEntry>,
    /// While a long run (a synchronize) is still putting its steps into the history, the run's
    /// token: the history keeps every step then, so the run can fold them into one whole step
    /// before the bound applies ([`Session::hold_undo`]). Dead (or never set) when no run holds it.
    undo_hold: std::sync::Weak<()>,
    /// Set while a run puts one of its own steps into the history ([`Session::run_step`]).
    in_run_step: bool,
    /// A run has put a step into the history under the current hold: from then on any other
    /// commit (an edit between its steps) ends the hold at once, as the steps can't be folded.
    hold_stepped: bool,
    /// Undo steps committed in this session, ever (the history drops its oldest past 1000, so
    /// its length can't tell how many steps a command just made; see [`Session::commits`]).
    commits: u64,
    /// Undo steps the history has dropped past its 1000 ([`Session::undo_mark`]).
    undo_dropped: u64,
    pub redo: Vec<UndoEntry>,
    /// Exact filesystem repairs from discarded history; catalog inverses stay discarded.
    pub(crate) pending_sidecar_repairs: Vec<rename::SidecarMove>,
    pub interaction: Option<Interaction>,
    /// What the last command changed, for the on-screen overlay ([`change_report`]).
    pub last_change: Option<change_report::ChangeReport>,
    change_seq: u64,
    /// Live changes applied without an undo step (a drag in progress): counted, so the command
    /// that made one reports it.
    silent_changes: u64,
    /// Set by a command whose change must not rewrite the photo's XMP sidecar even with auto-write on
    /// (a catalog-only edit of data the sidecar writer does not emit); consumed when the command ends.
    pub(crate) skip_auto_write: bool,
    /// The library's `people.json` (roles, appearance examples), loaded when first needed.
    pub(crate) people: people_smart::PeopleState,
    pub(crate) concepts: concepts_store::ConceptState,
    pub(crate) appearance: identity::Appearance,
    /// The folder metadata save or read waiting for `folder.metadataPump` ([`cmd::folder_metadata`]); dropped when another library opens.
    pub(crate) folder_metadata: Option<cmd::folder_metadata::FolderMetadataJob>,
    /// The order of Reload from Disk runs, so an older one finishing late is dropped ([`cmd::reload`]).
    pub(crate) reload_tickets: cmd::reload::Tickets,
    /// Photos whose automatic XMP sidecar write failed (their folder offline, read-only or full), written again by
    /// [`Session::retry_unsaved_sidecars`] once their file is reachable.
    pub(crate) unsaved_sidecars: std::collections::BTreeSet<PhotoId>,
    /// The background sidecar writer, when the host turned it on ([`Session::set_background_sidecar_writes`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sidecar_writer: Option<sidecar_writer::Writer>,
    /// How long a file operation waits for the background sidecar writes queued before it ([`Session::settle_sidecar_writes`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sidecar_flush_limit: std::time::Duration,
    /// AI denoise: the model in use, the photos that have their picture and the work in progress.
    pub(crate) denoise: denoise::State,
    /// The shared AI job queue (#491), made on its first job.
    pub(crate) ml: ml::State,
    /// Plug-in filter measurements (`develop.pluginFilter.analyze`) running in the background.
    pub(crate) filter_analyses: cmd::plugin_filters::Analyses,
    /// Where the host keeps face models (one folder each); `None` where there is no file system (the web).
    pub face_models_dir: Option<std::path::PathBuf>,
    /// Where the host keeps shared AI / ML models (CLIP embeddings, scoring/culling/orientation heads); `None` on the web.
    pub ml_models_dir: Option<std::path::PathBuf>,
    /// Face model downloads started this session (the staged files wait in `<face_models_dir>/.downloads`).
    pub face_downloads: face_download::Downloads,
    /// Settings ▸ Lens profiles: the lens database folder and its download ([`lens_download`]).
    pub lens_db: lens_download::LensSettings,
    /// The lens database generation ([`lens_profiles::generation`]) each photo's lens data was last looked up with.
    pub(crate) lens_seen: std::collections::HashMap<PhotoId, u64>,
    /// Whether each photo looked up this session carries its file\'s own lens correction (not a database profile).
    pub(crate) lens_from_file: std::collections::HashMap<PhotoId, bool>,
    /// The lens picked for each photo ([`lcc_develop::Optics::lens`]) when its lens data was last looked up.
    pub(crate) lens_chosen_seen: std::collections::HashMap<PhotoId, Option<lcc_develop::ChosenLens>>,
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
    /// Peer sync: the paired devices and the service that finds them ([`peers`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub peers: peers::State,
    /// Auto updates: the last check, the download in flight, and the settings ([`update`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub updater: update::Updater,
    /// Showing files in the file manager and starting other programs ([`desktop`]); headless
    /// harnesses swap in a [`desktop::Recorder`] so tests never open desktop windows.
    pub desktop: std::sync::Arc<dyn desktop::Desktop>,
    /// Reading focus points for photos imported before they were read (`photo.readFocusPoints`, `focus.pump`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) focus_scan: focus_scan::FocusScan,
    /// Photo ▸ Enhance jobs on worker threads (`enhance.rawDetails`, `enhance.superResolution`, `enhance.pump`).
    pub(crate) enhance: enhance::EnhanceJobs,
    /// Copied develop settings (partial JSON, portable: a flat-field map goes with them) for Paste.
    pub clipboard: Option<Value>,
    /// Flat-field maps that came in with settings (a paste, a preset, an agent's partial) and wait for the edit that
    /// uses them to store them ([`vignetting_calibration_io`]).
    pub(crate) incoming_vignetting_maps: std::collections::BTreeMap<lcc_develop::VignettingMapId, lcc_develop::EmbeddedVignettingMap>,
    /// Where each raw's picture sits on its sensor, read from its header for flat-field matching
    /// ([`Session::photo_placement`]); in the app, by a worker thread ([`vignetting_calibration_placement`]).
    pub vignetting_calibration_placements: vignetting_calibration_placement::Placements,
    /// The flat-field library's folder when it isn't the usual one ([`vignetting_calibration_library::dir`]): `Some(None)` for
    /// none (tests, tools).
    pub vignetting_calibration_dir_override: Option<Option<std::path::PathBuf>>,
    /// The photo [`Session::clipboard`] was copied from: Paste links its masks with the pasted
    /// copies.
    pub clipboard_from: Option<PhotoId>,
    /// The folder on disk the [`LibrarySource::Folder`] view browses.
    pub browse: Option<Browse>,
    /// The folder the [`LibrarySource::LibraryFolder`] view shows: the library's photos imported
    /// from it and from the folders inside it.
    pub library_folder: Option<String>,
    /// Whether the [`LibrarySource::LibraryFolder`] view also shows the photos of the folders
    /// inside the chosen one (Show Photos in Subfolders, on by default).
    pub folder_subfolders: bool,
    /// Lock Filters (on by default): the filter bar's filter stays when another source is chosen.
    /// Off, choosing a source starts from the filter that source had (see `remember_filters`) or none.
    pub lock_filters: bool,
    /// Remember Each Source's Filters Separately: with `lock_filters` off, each source gets back
    /// the filter it had when it was left.
    pub remember_filters: bool,
    /// The filter each source had when it was left (key: [`Session::source_key`]); not saved.
    pub source_filters: std::collections::HashMap<String, Filter>,
    /// Copied metadata (`photo.copyMetadata`): photo.setMeta params.
    pub meta_clipboard: Option<Value>,
    /// The photo that was active before the current one (Paste Settings from Previous).
    pub previous_active: Option<PhotoId>,
    /// Groups last used for Copy (Lightroom remembers them).
    /// What dialogs remember app-wide: Copy / Sync / New Preset ticks, New Preset's group, other
    /// dialogs' last choices ([`app_store::DialogMemory`]; kept in the app store, else the library).
    pub dialogs: app_store::DialogMemory,
    /// The settings the last `preset.update` replaced (`preset.undoUpdate`).
    pub preset_undo: Option<(String, serde_json::Value)>,
    /// Built-in then user presets; its generation tells savers and caches when it changed.
    pub presets: presets::PresetList,
    /// User-created preset folders (subfolders separated by `/`); empty ones stay until deleted.
    pub preset_folders: Vec<String>,
    /// Favourite profile ids (persisted with the library, like preset favourites).
    pub profile_favorites: Vec<String>,
    /// Presets marked At Import (`preset.forImport`): the import pickers list them first (LR-PRE-ONIMPORT).
    /// Changing it bumps the preset list's generation, so it is saved and the pickers rebuild.
    pub preset_at_import: Vec<String>,
    /// Presets hidden from the Presets panel and the pickers (`preset.hide`), kept in the Presets
    /// Manager (LR-PRE-MANAGE). Changing it bumps the preset list's generation.
    pub preset_hidden: Vec<String>,
    /// Built-in presets the user renamed (id → name) or put in another group (id → group) in the
    /// Presets Manager (LR-PRE-MANAGE); `preset.restoreBuiltins` lets go of both. Changing them
    /// bumps the preset list's generation.
    pub preset_builtin_names: std::collections::BTreeMap<String, String>,
    pub preset_builtin_groups: std::collections::BTreeMap<String, String>,
    /// Recently applied profile ids, newest first (at most [`presets::RECENT_PROFILES`]).
    pub profile_recent: Vec<String>,
    /// Profiles hidden from the menus and the browser by the Profile Manager (still listed there and in
    /// `profiles.list {all}`; photos using one keep it).
    pub profile_hidden: Vec<String>,
    /// The Profile Manager's renames, regroupings, order and removed built-in looks.
    pub profile_edits: profile_edits::ProfileEdits,
    /// The camera profile each camera body starts with (`profile_bodies::key` → `dcp:…`, `cal:…` or `lc`),
    /// applied to that body's raws at import ([`profile_bodies`]).
    pub body_profiles: std::collections::BTreeMap<String, String>,
    /// The Import panel's camera profile choice as it stands (Automatic / one for all / per body):
    /// browsed photos preview it as the import will apply it ([`Session::browse_profile`]).
    pub import_profile_choice: profile_bodies::ImportProfile,
    /// Serial numbers read from raws catalogued without one ([`profile_bodies::FileSerials`]).
    pub(crate) file_serials: profile_bodies::FileSerials,
    /// Browsed photos as they render with their body's camera profile ([`Session::rendered_photo`]).
    pub(crate) browse_renders: profile_bodies::BrowseRenders,
    /// The app-wide store of presets, profiles and habits shared by every library (`None`: each
    /// library keeps its own; see [`app_store`]).
    pub app_store: Option<app_store::AppStore>,
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
    /// A top-level command waits until its catalog records are durable before it returns (the
    /// default: an agent or script is told "done" only for what is on disk). The desktop UI turns
    /// it off for its own commands, so a flag or a step never waits for the drive; the control
    /// channel then waits ([`Session::flush_catalog`]) before it replies.
    pub wait_for_catalog_log: bool,
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
    /// The UI is running a plug-in command the user started (`plugin.run`): what it needs that the
    /// user was never asked about goes in `plugin_asks`, instead of an error that says to go to
    /// the Plug-in Manager. Set by the UI only, around its own runs; never by a request.
    #[cfg(not(target_arch = "wasm32"))]
    pub plugin_ask_user: bool,
    /// Permissions plug-in commands the user started need, for the UI to ask about (Allow runs the
    /// command again).
    #[cfg(not(target_arch = "wasm32"))]
    pub plugin_asks: Vec<cmd::plugins::PluginAsk>,
    /// XMP sidecar preferences (persisted with the library).
    pub xmp: sidecar::XmpPrefs,
    /// The library's shared-stem owners with the catalog revision they were built at
    /// ([`Session::stem_owners`]): one pass per catalog change, a hash lookup per photo
    /// otherwise. Not saved.
    pub(crate) stems: std::sync::Mutex<Option<(u64, std::sync::Arc<sidecar::StemOwners>)>>,
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
    /// The Template Browser's folders besides [`template_folders::DEFAULT_FOLDER`], sorted by name
    /// (`print.template.folder.*`), persisted in prefs.json.
    pub print_template_folders: Vec<String>,
    /// Metadata presets (`metadata.*`), persisted in prefs.json.
    pub metadata_presets: Vec<cmd::metadata::MetadataPreset>,
    /// Import presets: named sets of the Import tab's settings (persisted in prefs.json).
    pub import_presets: Vec<cmd::import_tab::ImportPreset>,
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
    /// The adjustments the next new mask starts with, whatever its tool: what was dialed in the
    /// Masking panel while no mask was selected (or a tool armed before its first stroke), as
    /// Lightroom Classic does. Edits of existing masks never change it. Kept across photos.
    pub mask_next: lcc_develop::LocalAdjustments,
    /// [`mask_next`](Self::mask_next) was dialed in since it was last zeroed.
    pub mask_next_set: bool,
    /// Reset Settings on New Mask: once a new mask took [`mask_next`](Self::mask_next), it goes back
    /// to zero, so the next one starts at zero unless something is dialed in again.
    pub mask_reset_new: bool,
    /// Saved colour-label name sets.
    pub label_sets: Vec<cmd::manage::LabelSet>,
    /// File-name templates saved by name (Rename Photos, Export), kept with the library's preferences.
    pub naming_presets: Vec<cmd::manage::NamingPreset>,
    /// Reject Closed and Blurry Eyes running in the background, and what it has finished so far.
    pub(crate) eye_cull: Option<Arc<eye_cull::EyeCullRun>>,
    pub(crate) eye_cull_results: Vec<(PhotoId, std::result::Result<Arc<eyes::EyeReport>, String>)>,
    /// Detect Faces running in the background, and what it has finished so far.
    pub(crate) face_detect: Option<Arc<cmd::face_detect::DetectRun>>,
    pub(crate) face_detect_results: Vec<(PhotoId, std::result::Result<Vec<lcc_faces::yunet::Face>, String>)>,
    /// The last `eyes.analyze` run in the background.
    pub(crate) eye_prep: Option<Arc<eye_prep::EyePrep>>,
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
    /// The keywords ⇧K adds to or removes from the selected photos (Set Keyword Shortcut…).
    pub keyword_shortcut: Vec<String>,
    /// Keyword attributes (synonyms, export options, person) and keywords created before any
    /// photo has them, by keyword path (Keyword List).
    /// File probes from the last import review (`library.importPreview`), reused by the import.
    pub import_probes: std::collections::HashMap<String, media::ProbeInfo>,
    /// The last Synchronize Folder scan, which `folder.synchronize` acts on while it is current
    /// (see [`Session::take_folder_changes`]).
    pub folder_changes: Option<sync::FolderChanges>,
    /// Folder polling preferences (persisted with the library; see [`folder_watch`]).
    pub folder_watch: folder_watch::WatchPrefs,
    /// What the library's folders' listings looked like at the last poll (`folder-stamps.json`).
    pub folder_stamps: folder_watch::Stamps,
    /// New files a poll found and the person hasn't added yet, by folder (the "N new" badge).
    pub folder_new: std::collections::BTreeMap<String, usize>,
    /// Background tasks in flight (imports, exports, preview builds…), for the activity stack and
    /// `activity.list` / `activity.cancel`.
    pub activity: activity::Activity,
    /// Video playback, trim and poster (M14.4/M14.6): the decoder backend and the player.
    pub video: video::VideoState,
    /// Upright analysed on each photo an automatic mode reaches without its own correction ([`upright_jobs`]).
    pub upright_jobs: upright_jobs::UprightJobs,
    /// The last (or running) slideshow export (`slideshow.export`).
    pub slideshow_export: Option<std::sync::Arc<slideshow_export::ExportRun>>,
    /// Generative fill: keys, consent, the running generation, the patch cache ([`genfill`]).
    pub genfill: genfill::GenfillState,
    /// The last (or running) Build Previews.
    pub preview_build: Option<std::sync::Arc<cmd::previews::PreviewBuild>>,
    /// Preparing photos in the background: settings (app-wide and this library's) and the build
    /// it runs ([`prep`]).
    pub prep: prep::PrepState,
    /// The long edge (px) the loupe renders at on this screen, as the frontend last said (0: not
    /// known). The background preparation builds standard previews at least this big, so the
    /// loupe can use them.
    pub loupe_edge: usize,
    /// Finishing an import, kept in the library until it is done ([`finishing`]).
    pub finishing: Option<finishing::Finishing>,
    /// An unfinished plan the library kept, taken up by the next [`Session::finishing_tick`].
    pub finish_pending: Option<finishing::FinishPlan>,
    /// The camera sensor map build or batch Remove Dust running in the background, or the last one.
    pub sensor_job: Option<std::sync::Arc<cmd::sensor::SensorJob>>,
    /// The preview cache drive's free space as last looked up: (when, cache folder, bytes).
    pub(crate) cache_drive_free: std::sync::Mutex<Option<(web_time::Instant, std::path::PathBuf, Option<u64>)>>,
    /// Develop defaults applied on import (persisted in prefs.json).
    pub import_defaults: import::ImportDefaults,
    /// The default camera profile per camera model (`""` = every camera; persisted in prefs.json,
    /// issue #723): the `profile.camera` choice photos of that camera start from when they choose
    /// none. `lc` = the app's own colour, `dcp:<id>` an imported profile, `cal:<id>` a look.
    pub camera_profile_defaults: std::collections::BTreeMap<String, String>,
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
    /// Save `presets.json` on a background thread ([`presets_save`]), where the library's store
    /// allows it: set by the desktop app (a 40 MB preset collection froze the UI for each save).
    /// Off (the default): saves happen inside the command that changed the presets.
    pub background_saves: bool,
    /// The background saver, once started.
    presets_saver: Option<presets_save::Saver>,
    /// Moves a file to the system trash (`photo.deleteFromDisk`). `None` in wasm and sessions without a filesystem.
    pub trash: Option<disk_delete::TrashFn>,
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    /// What the next Auto Import listing looks at (`None`: no active watched folder).
    pub fn auto_import_scope(&self) -> Option<cmd::library::AutoImportScope> {
        let folder = self.import_defaults.auto_active_folder()?.clone();
        Some(cmd::library::AutoImportScope {
            folder,
            subfolders: self.import_defaults.auto_options.subfolders,
            skip: self.library.as_ref().map(|l| l.dir.clone()),
        })
    }

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
            source_history: Default::default(),
            filter: Filter::default(),
            shelved_filter: None,
            sort: Sort::default(),
            selection: Selection::default(),
            visible: Vec::new().into(),
            visible_key: None,
            visible_clock_minute: None,
            hold_view: false,
            base: None,
            all_selected: None,
            alternatives: None,
            view_followed: 0,
            view_worked_out: 0,
            #[cfg(test)]
            never_follow: false,
            visible_gen: 0,
            total: None,
            undo: Vec::new(),
            undo_hold: std::sync::Weak::new(),
            in_run_step: false,
            hold_stepped: false,
            commits: 0,
            undo_dropped: 0,
            redo: Vec::new(),
            pending_sidecar_repairs: Vec::new(),
            interaction: None,
            last_change: None,
            change_seq: 0,
            silent_changes: 0,
            skip_auto_write: false,
            people: Default::default(),
            concepts: Default::default(),
            appearance: Default::default(),
            stems: Default::default(),
            folder_metadata: None,
            reload_tickets: Default::default(),
            unsaved_sidecars: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            sidecar_writer: None,
            #[cfg(not(target_arch = "wasm32"))]
            sidecar_flush_limit: std::time::Duration::from_secs(30),
            denoise: Default::default(),
            ml: Default::default(),
            filter_analyses: Default::default(),
            face_models_dir: None,
            ml_models_dir: None,
            face_downloads: Default::default(),
            lens_db: Default::default(),
            lens_seen: Default::default(),
            lens_from_file: Default::default(),
            lens_chosen_seen: Default::default(),
            place_names: Default::default(),
            face_catalog: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            faces: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            remote: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            peers: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            updater: Default::default(),
            desktop: desktop::default_desktop(),
            #[cfg(not(target_arch = "wasm32"))]
            focus_scan: Default::default(),
            enhance: Default::default(),
            clipboard: None,
            incoming_vignetting_maps: Default::default(),
            vignetting_calibration_placements: Default::default(),
            vignetting_calibration_dir_override: None,
            clipboard_from: None,
            meta_clipboard: None,
            browse: None,
            library_folder: None,
            folder_subfolders: true,
            lock_filters: true,
            remember_filters: false,
            source_filters: Default::default(),
            previous_active: None,
            dialogs: Default::default(),
            preset_undo: None,
            presets: presets::PresetList::new(presets::builtin()),
            preset_folders: Vec::new(),
            profile_favorites: Vec::new(),
            preset_at_import: Vec::new(),
            preset_hidden: Vec::new(),
            preset_builtin_names: Default::default(),
            preset_builtin_groups: Default::default(),
            profile_recent: Vec::new(),
            profile_hidden: Vec::new(),
            profile_edits: Default::default(),
            body_profiles: Default::default(),
            import_profile_choice: Default::default(),
            file_serials: Default::default(),
            browse_renders: Default::default(),
            app_store: None,
            journal: Vec::new(),
            workflow: Default::default(),
            pending_log: Vec::new(),
            media: media::MediaCache::default(),
            clock: Box::new(|| "2026-09-30T12:00:00".to_string()),
            depth: 0,
            wait_for_catalog_log: true,
            active_mask: None,
            segmenter: segment::Segmenter::default(),
            active_spot: None,
            library: None,
            library_identity: Arc::new(()),
            plugin_library: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            plugin_ask_user: false,
            #[cfg(not(target_arch = "wasm32"))]
            plugin_asks: Vec::new(),
            xmp: sidecar::XmpPrefs::default(),
            last_export: None,
            previous_export: Vec::new(),
            last_merge: Default::default(),
            export_presets: Vec::new(),
            print: Default::default(),
            print_templates: Vec::new(),
            print_template_folders: Vec::new(),
            metadata_presets: Vec::new(),
            import_presets: Vec::new(),
            filter_presets: Vec::new(),
            curve_presets: Vec::new(),
            crop_aspects: Default::default(),
            mask_effects: Vec::new(),
            mask_tool_effects: Default::default(),
            mask_next: Default::default(),
            mask_next_set: false,
            mask_reset_new: false,
            label_sets: Vec::new(),
            naming_presets: Vec::new(),
            eye_cull: None,
            eye_cull_results: Vec::new(),
            face_detect: None,
            face_detect_results: Vec::new(),
            eye_prep: None,
            signatures: Default::default(),
            lut_profiles: Vec::new(),
            target_album: None,
            auto_import_seen: Default::default(),
            keyword_sets: Vec::new(),
            keyword_set: None,
            recent_keywords: Vec::new(),
            keyword_shortcut: Vec::new(),
            import_probes: Default::default(),
            folder_changes: None,
            folder_watch: Default::default(),
            folder_stamps: Default::default(),
            folder_new: Default::default(),
            activity: Default::default(),
            video: Default::default(),
            genfill: Default::default(),
            upright_jobs: Default::default(),
            preview_build: None,
            prep: Default::default(),
            loupe_edge: 0,
            finishing: None,
            finish_pending: None,
            cache_drive_free: std::sync::Mutex::new(None),
            slideshow_export: None,
            sensor_job: None,
            import_defaults: import::ImportDefaults::default(),
            camera_profile_defaults: Default::default(),
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
            background_saves: false,
            presets_saver: None,
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
        let id = &*cmd::canonical_id(id);
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
        let (commits_before, silent_before) = (self.commits, self.silent_changes);
        self.depth += 1;
        // last-resort guard: a panic in a command is that command's error, not a crash
        let r = guard::catch(&format!("`{id}`"), || f(self)).unwrap_or_else(|e| Err(EngineError::Other(e)));
        self.depth -= 1;
        // what it changed, for the overlay (before auto versions and follow-ups add their own)
        if self.depth == 0
            && let (Ok(v), Some(params)) = (&r, journal)
        {
            self.report_change(id, params, v, log_start, commits_before, silent_before);
        }
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
        if self.depth == 0 {
            // whatever the command changed, and however it ended: the view points at what is there
            self.reconcile_view();
        }
        if self.depth == 0 && self.library.is_none() && self.app_store.is_some() {
            // no library (`run --shared-settings`): the store is saved after each command
            if let Err(e) = self.save_app_store() {
                log::error!("store: {e}");
                if r.is_ok() {
                    return Err(e);
                }
            }
        }
        if self.depth == 0 && self.library.is_some() {
            // Make the command durable before reporting success. When the command's own records
            // can't be appended, it fails with `NotSaved`: the change stays applied in memory and
            // queued, and the next save (any later command, or the frame loop) retries it. Other
            // persistence trouble (a failed compaction, an older queued change still unwritten by
            // a command that changed nothing) doesn't fail the command; `library.info` reports it.
            let produced = self.pending_log.len() > log_start;
            let mut persisted = self.persist();
            if persisted.is_ok() && produced && self.wait_for_catalog_log {
                persisted = self.flush_catalog();
            }
            if let Err(e @ EngineError::NotSaved(_)) = persisted
                && produced
                && r.is_ok()
            {
                return Err(e);
            }
        }
        r
    }

    /// Fill [`Session::last_change`] when command `id` changed something the user sees: the ops
    /// it logged since `log_start`, or live (undo-less) changes inside an interaction.
    fn report_change(&mut self, id: &str, params: &Value, result: &Value, log_start: usize, commits_before: u64, silent_before: u64) {
        if change_report::is_background(id) {
            return;
        }
        let logged = self.pending_log.get(log_start..).unwrap_or_default();
        let (photos, kind, live) = match change_report::summarize(logged) {
            Some((n, k)) => (n, k, false),
            None if self.silent_changes != silent_before => {
                // a drag or a turning dial: the active photo, and the ones moving along
                let n = self.interaction.as_ref().map_or(1, |i| 1 + i.others.len());
                (n, change_report::ChangeKind::Develop, self.interaction.is_some())
            }
            None => return,
        };
        let label = if self.commits != commits_before {
            self.undo.last().map(|e| e.label.clone())
        } else if live {
            self.interaction.as_ref().map(|i| i.label.clone())
        } else {
            None
        };
        let label = label.or_else(|| find_command(id).map(|c| c.label.to_string())).unwrap_or_else(|| id.to_string());
        let detail = ["undone", "redone"].iter().find_map(|k| result.get(*k).and_then(Value::as_str)).map(str::to_string);
        // the end of a drag commits the value it was moving: still that value
        let control = change_report::control_of(id, params).or_else(|| {
            (id == "develop.endInteraction").then(|| self.last_change.as_ref().filter(|c| c.live).and_then(|c| c.control.clone())).flatten()
        });
        self.change_seq = self.change_seq.wrapping_add(1);
        self.last_change = Some(change_report::ChangeReport {
            seq: self.change_seq,
            command: id.to_string(),
            params: params.clone(),
            label,
            photos,
            kind,
            control,
            detail,
            live,
        });
    }

    /// Hand `presets.json` to the background saver ([`presets_save`]) when this session saves
    /// in the background and `store` can be written from another thread; `known` is the hash
    /// of what the file holds (0: unknown). Whether it was handed over (else: save it now).
    pub(crate) fn save_presets_in_background(&mut self, store: &dyn lcc_catalog::Store, known: u64) -> bool {
        if !self.background_saves {
            return false;
        }
        let Some(writer) = store.background_writer() else { return false };
        if self.presets_saver.is_none() {
            self.presets_saver = presets_save::Saver::start();
        }
        let Some(saver) = self.presets_saver.as_ref() else { return false };
        saver.save(presets_save::Snapshot::of(self), writer, "presets.json", known);
        true
    }

    /// A background save of `presets.json` failed since the last call (its reason, once).
    pub(crate) fn presets_save_failed(&self) -> Option<String> {
        self.presets_saver.as_ref().and_then(presets_save::Saver::take_error)
    }

    /// Wait (bounded) for a background `presets.json` save in progress.
    pub(crate) fn flush_presets(&self) {
        if let Some(s) = &self.presets_saver
            && !s.flush(std::time::Duration::from_secs(20))
        {
            log::warn!("presets: the background save is taking long; not waiting for it");
        }
    }

    pub fn commands(&self) -> Vec<CommandInfo> {
        command_specs().iter().map(|c| c.info(self)).collect()
    }

    // ---------------------------------------------------------------- ops, undo

    /// Apply an op as one undoable step. A new edit: one that would make a smart album include
    /// itself is refused here, whichever command, task or caller asked ([`Catalog::apply_new`]);
    /// undo, redo and opening a library bring back what was and are not held to that.
    pub fn commit(&mut self, label: &str, op: Op) -> Result<()> {
        // the flat-field maps the edit's settings came with go in first (`vignetting_calibration_io`)
        let mut op = self.with_vignetting_maps(op);
        self.mark_batch(&mut op);
        let fwd = op.clone();
        let inv = self.apply_through(op, Catalog::apply_new)?;
        let upright = fwd.clone();
        self.pending_log.push(fwd);
        self.undo.push(UndoEntry {
            label: label.to_string(),
            op: inv,
            folder: None,
            profile: None,
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
        self.commits = self.commits.wrapping_add(1);
        if self.hold_stepped && !self.in_run_step {
            // an edit between a run's steps: they can't be folded any more (the run sees it on
            // its next step or at its end), so the history is cut now, not when the next step comes
            self.undo_hold = std::sync::Weak::new();
            self.hold_stepped = false;
        }
        self.trim_undo();
        for mut dropped in self.redo.drain(..) {
            self.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
        }
        // after the undo step, so a correction worked out now is journalled after the edit it completes
        self.upright_after(&upright);
        // photos whose Smooth Skin now wants the skin found get it
        self.skin_after(&upright);
        self.vignetting_after(&upright);
        Ok(())
    }

    /// [`Catalog::apply`] for what brings back what was (an undo or a redo).
    fn apply_catalog(&mut self, op: Op) -> Result<Op> {
        self.apply_through(op, Catalog::apply)
    }

    /// Apply `op` with `apply` (the catalog's way in for a new edit or for bringing back what
    /// was), then forget what the session cached of the photos it replaced.
    fn apply_through(&mut self, op: Op, apply: fn(&mut Catalog, Op) -> std::result::Result<Op, lcc_catalog::CatalogError>) -> Result<Op> {
        // a sidecar write still queued for a photo whose file moves, goes or changes would land at the old place
        if op_moves_files(&op) {
            self.settle_sidecar_writes()?;
        }
        // Retain only cache IDs while the catalog consumes the operation. Cloning an import
        // batch here also cloned every photo, although commit already keeps the journal copy.
        let mut invalidated = Vec::new();
        // photos whose file was replaced: the faces and ML embeddings made from the old pixels go too (issue #1524)
        let mut replaced = Vec::new();
        let mut added_photo = false;
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
                Op::AddPhoto { photo } => {
                    invalidated.push(photo.id);
                    added_photo = true;
                }
                Op::Batch { ops } => pending.extend(ops),
                _ => {}
            }
        }
        let inv = apply(&mut self.catalog, op)?;
        for id in invalidated {
            self.media.forget(id);
        }
        if added_photo {
            self.ml.photo_added();
        }
        for id in &replaced {
            self.ml.content_replaced(*id);
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

    /// Where things stand before a command commits a step it may have to take back if it then fails
    /// ([`Session::committed_step`]).
    pub(crate) fn before_step(&self) -> BeforeStep {
        BeforeStep { commits: self.commits, log: self.pending_log.len() }
    }

    /// The one step committed since `before`, for [`Session::take_back`]; `None` when no step, or more than one, was
    /// committed since.
    pub(crate) fn committed_step(&self, before: BeforeStep) -> Option<CommittedStep> {
        (self.commits == before.commits.wrapping_add(1) && !self.undo.is_empty()).then(|| CommittedStep {
            commits: self.commits,
            undo_len: self.undo.len(),
            pending: (self.pending_log.len() == before.log.saturating_add(1)).then_some(before.log),
        })
    }

    /// Take back `step`, the step a command committed before it failed, as an undo would, with nothing left to redo.
    /// Only that step: when anything was committed, undone or merged ([`Session::merge_undo`]) since it, the top of the
    /// undo stack may be a step that isn't the command's, and this refuses instead. When the step isn't written to the
    /// library's log yet, it leaves the queue, so neither it nor its reversal reaches the disk; otherwise the reversal
    /// is logged like an undo. A step that can't be taken back stays on the undo stack, and the error says why.
    pub(crate) fn take_back(&mut self, step: CommittedStep) -> Result<()> {
        // A later commit changes the count; an undo, a redo or a merge changes the stack's length (at the cap a commit
        // leaves the length as it was, but changes the count).
        if self.commits != step.commits || self.undo.len() != step.undo_len {
            return Err(EngineError::Other("the step to take back is no longer the last one".into()));
        }
        let mut e = self.undo.pop().ok_or_else(|| EngineError::Other("nothing to take back".into()))?;
        if let Err(err) = self.apply_with_files(&mut e, &rename::RealFs) {
            self.undo.push(e);
            return Err(err);
        }
        self.drop_gone_from_selection();
        self.reconcile_view();
        self.vignetting_after(&e.op);
        // Dropping the queued step, rather than logging its reversal after it, keeps a write cut short (a crash, a full
        // disk) from saving the step without its reversal: each op is its own record in the log.
        match step.pending.filter(|at| at.saturating_add(1) == self.pending_log.len()) {
            Some(at) => self.pending_log.truncate(at),
            None => self.pending_log.push(e.op),
        }
        Ok(())
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

    /// Drop the oldest undo steps past [`UNDO_STEPS`], keeping the sidecar repairs they owed;
    /// nothing while a run holds the history ([`Session::hold_undo`]).
    pub(crate) fn trim_undo(&mut self) {
        if self.undo_hold.strong_count() > 0 {
            return;
        }
        let excess = self.undo.len().saturating_sub(UNDO_STEPS);
        for mut dropped in self.undo.drain(..excess) {
            self.undo_dropped = self.undo_dropped.wrapping_add(1);
            self.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
        }
    }

    /// Do `f`, a run's own step ([`Session::hold_undo`]): the commits it makes keep the hold, and
    /// any other commit after the run's first one ends it. Every commit a run makes for the hold
    /// goes through here.
    pub(crate) fn run_step<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let (was, commits) = (std::mem::replace(&mut self.in_run_step, true), self.commits);
        // a panic in `f` (caught further up, at the command) must not leave the flag set
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.in_run_step = was;
        self.hold_stepped |= self.commits != commits;
        match out {
            Ok(out) => out,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Let a long run that commits many steps (a synchronize) fold them into one step: while the
    /// returned token lives, the history is not cut to [`UNDO_STEPS`], so the run's first steps
    /// are still there when it merges them ([`Session::merge_undo`]) and one Undo takes back the
    /// whole run. The run drops the token when it is done (or abandoned) and calls
    /// [`Session::trim_undo`]; a commit after the token died does it too. Runs that overlap
    /// share one token: the history is cut once the last of them lets go.
    pub(crate) fn hold_undo(&mut self) -> std::sync::Arc<()> {
        if let Some(token) = self.undo_hold.upgrade() {
            return token;
        }
        let token = std::sync::Arc::new(());
        self.undo_hold = std::sync::Arc::downgrade(&token);
        self.hold_stepped = false;
        token
    }

    /// Where the undo history stands, counting the steps it dropped past its cap: the difference
    /// between two marks is how many undo steps were added in between, after any a command folded
    /// into one ([`Session::merge_undo`]), which [`Session::commits`] still counts apart.
    pub(crate) fn undo_mark(&self) -> u64 {
        (self.undo.len() as u64).wrapping_add(self.undo_dropped)
    }

    pub fn merge_undo(&mut self, n: usize, label: &str) {
        if n < 2 || n > self.undo.len() {
            return;
        }
        let Some(entries) = self.undo.get(self.undo.len() - n..) else { return };
        // Folder moves and partially applied file steps stay on their own.
        if entries.iter().any(|e| e.folder.is_some() || e.profile.is_some() || !e.retry_inverse.is_empty() || !e.retry_sidecars.is_empty()) {
            return;
        }
        let tail = self.undo.split_off(self.undo.len() - n);
        let ops = tail.into_iter().rev().map(|e| e.op).collect();
        self.undo.push(UndoEntry {
            label: label.to_string(),
            op: Op::Batch { ops },
            folder: None,
            profile: None,
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
        self.silent_changes = self.silent_changes.wrapping_add(1);
        Ok(())
    }

    pub fn undo_step(&mut self) -> Result<String> {
        self.undo_step_with(&rename::RealFs)
    }

    pub(crate) fn undo_step_with(&mut self, fs: &dyn rename::MoveFs) -> Result<String> {
        let mut e = self.undo.pop().ok_or_else(|| EngineError::Other("nothing to undo".into()))?;
        // a replaced camera profile's file goes back first (nothing else changes when that fails)
        let swapped = match e.profile.as_ref().map(|sw| chart_profiling::apply_swap(self, sw)) {
            Some(Err(err)) => {
                self.undo.push(e);
                return Err(err);
            }
            Some(Ok(back)) => Some(back),
            None => None,
        };
        let redo = match self.apply_with_files(&mut e, fs) {
            Ok(r) => r,
            Err(err) => {
                // (the profile file goes back as it was)
                if let Some(back) = &swapped {
                    let _ = chart_profiling::apply_swap(self, back);
                }
                self.undo.push(e);
                return Err(err);
            }
        };
        self.drop_gone_from_selection();
        self.show_undone(&e.op);
        self.reconcile_view();
        let applied = e.op.clone();
        // AI masks still computing for what was undone stop
        self.segmenter.cancel_recompute(single_photo(&e.op, 0));
        self.pending_log.push(e.op);
        self.redo.push(UndoEntry {
            label: e.label.clone(),
            op: redo,
            folder: e.folder.as_ref().map(FolderMove::reversed),
            profile: swapped,
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
        // a step that brings back Smooth Skin without its skin gets it again (kept for the session: no model runs)
        self.skin_after(&applied);
        self.vignetting_after(&applied);
        Ok(e.label)
    }

    pub fn redo_step(&mut self) -> Result<String> {
        self.redo_step_with(&rename::RealFs)
    }

    pub(crate) fn redo_step_with(&mut self, fs: &dyn rename::MoveFs) -> Result<String> {
        let mut e = self.redo.pop().ok_or_else(|| EngineError::Other("nothing to redo".into()))?;
        // a replaced camera profile's file goes back first (nothing else changes when that fails)
        let swapped = match e.profile.as_ref().map(|sw| chart_profiling::apply_swap(self, sw)) {
            Some(Err(err)) => {
                self.redo.push(e);
                return Err(err);
            }
            Some(Ok(back)) => Some(back),
            None => None,
        };
        let undo = match self.apply_with_files(&mut e, fs) {
            Ok(r) => r,
            Err(err) => {
                // (the profile file goes back as it was)
                if let Some(back) = &swapped {
                    let _ = chart_profiling::apply_swap(self, back);
                }
                self.redo.push(e);
                return Err(err);
            }
        };
        self.drop_gone_from_selection();
        self.show_undone(&e.op);
        self.reconcile_view();
        let applied = e.op.clone();
        // a redone preset or paste computes the AI masks it brings again
        if let Some(id) = single_photo(&e.op, 0) {
            self.want_missing_masks(id);
        }
        self.pending_log.push(e.op);
        self.undo.push(UndoEntry {
            label: e.label.clone(),
            op: undo,
            folder: e.folder.as_ref().map(FolderMove::reversed),
            profile: swapped,
            retry_inverse: Vec::new(),
            retry_sidecars: Vec::new(),
        });
        // a step that brings back Smooth Skin without its skin gets it again (kept for the session: no model runs)
        self.skin_after(&applied);
        self.vignetting_after(&applied);
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
        // Auto Sync carries the edit as asked; each photo then fits its own crop (review of #1990)
        let mut requested = settings;
        // an edit of a preset's provisional masks makes them the photographer's own
        if let Some(before) = self.develop_of(id) {
            lcc_develop::presets::settle_provisional(&before, &mut requested);
        }
        let settings = Arc::new(self.fit_constrained_crop(id, requested.clone()));
        let crop_edited = self.develop_of(id).is_some_and(|d| d.crop.geometry != requested.crop.geometry);
        if let Some(i) = &mut self.interaction
            && i.photo == id
        {
            i.crop_edited |= crop_edited;
            let (original, along) = (i.original.clone(), i.along.clone());
            self.apply_silent(Op::SetDevelop { id, settings, label: label.into(), edited: Some(now) })?;
            if !along.is_empty() {
                self.live_sync(id, &original, &requested, label, &along)?;
            }
            return Ok(());
        }
        // masks Auto Sync carries to the other selected photos are linked with their copies there
        let mut settings = settings;
        if self.auto_sync_applies(id, label) {
            let links = self.link_edited_masks(id, &mut requested);
            if !links.is_empty() {
                let mut s = (*settings).clone();
                for m in &mut s.masks {
                    if let Some((_, l)) = links.iter().find(|(mid, _)| *mid == m.id) {
                        m.link = l.clone();
                    }
                }
                settings = Arc::new(s);
            }
        }
        let own_auto = self.own_auto_wb(id, &requested, label);
        let mut ops = vec![self.develop_op(id, (*settings).clone(), label).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?];
        let (synced, compute) = self.auto_sync_ops(id, &requested, label, &own_auto);
        ops.extend(synced);
        let op = if ops.len() == 1 { ops.remove(0) } else { Op::Batch { ops } };
        self.commit(label, op)?;
        // AI masks Auto Sync carried over are computed on each photo from its own pixels
        for x in compute {
            let _ = if self.segmenter.background { self.compute_masks_later(x, false).map(|_| ()) } else { self.compute_masks(x, false).map(|_| ()) };
        }
        Ok(())
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
        let frame = lcc_pipeline::geometry::Frame::with_lens(
            photo.width.max(1) as usize,
            photo.height.max(1) as usize,
            &d,
            true,
            photo.embedded_lens.as_deref(),
        );
        d.crop.geometry.rect = frame.crop.rect;
        d
    }

    /// With Auto Sync on, the ops that carry an edit of the active photo `id` (to `new`, as asked,
    /// before its crop is fitted) over to the other selected photos: only the settings that changed;
    /// never spot removal or red eye (they belong to one photo's pixels), nor the rendering process
    /// (Sync doesn't carry it either), nor history / snapshot restores. Constrain Crop then fits each
    /// photo's own crop: the active photo's fitted crop is never copied over another's. Masks are
    /// merged into each photo's own ([`lcc_develop::recipe::sync_masks`]): AI selections travel as
    /// recipes, never as the active photo's pixels. Also returns the photos whose AI masks now
    /// need computing.
    ///
    /// White balance goes on each photo as the active photo's mode or value on the photo's own
    /// scale ([`sync_same`]): As Shot its own camera white, Auto its own (`own_auto`, worked out
    /// on its pixels), a Temp or Tint the same number, only the slider that moved.
    fn auto_sync_ops(
        &self,
        id: PhotoId,
        new: &DevelopSettings,
        label: &str,
        own_auto: &std::collections::HashMap<PhotoId, (f64, f64)>,
    ) -> (Vec<Op>, Vec<PhotoId>) {
        let Some(old) = self.develop_of(id) else { return (Vec::new(), Vec::new()) };
        self.auto_sync_ops_from(id, &old, new, label, own_auto)
    }

    /// A live frame of an Auto Sync interaction (LR-AUTOSYNC-LIVE): the other selected photos go
    /// back to their settings at the start and take the edit from `original` to `new` as the
    /// commit will, previewed without an undo step.
    fn live_sync(
        &mut self,
        id: PhotoId,
        original: &DevelopSettings,
        new: &DevelopSettings,
        label: &str,
        along: &[(PhotoId, Arc<DevelopSettings>)],
    ) -> Result<()> {
        for (x, d) in along {
            self.apply_silent(Op::SetDevelop { id: *x, settings: d.clone(), label: label.into(), edited: None })?;
        }
        let (ops, _) = self.auto_sync_ops_from(id, original, new, label, &std::collections::HashMap::new());
        let mut sets = Vec::new();
        let mut stack = ops;
        while let Some(op) = stack.pop() {
            match op {
                Op::Batch { ops } => stack.extend(ops),
                op @ Op::SetDevelop { .. } => sets.push(op),
                _ => {}
            }
        }
        for op in sets {
            self.apply_silent(op)?;
        }
        Ok(())
    }

    fn auto_sync_ops_from(
        &self,
        id: PhotoId,
        old: &DevelopSettings,
        new: &DevelopSettings,
        label: &str,
        own_auto: &std::collections::HashMap<PhotoId, (f64, f64)>,
    ) -> (Vec<Op>, Vec<PhotoId>) {
        if !self.auto_sync_applies(id, label) {
            return (Vec::new(), Vec::new());
        }
        let masks_changed = old.masks != new.masks;
        let mut old_json = old.to_json();
        let mut new_json = new.to_json();
        // False is omitted from stored profile JSON. Keep it explicit for this comparison so
        // switching camera local tone mapping off is a change without copying untouched values.
        old_json["profile"]["camera_local_tone"] = Value::Bool(old.profile.camera_local_tone);
        new_json["profile"]["camera_local_tone"] = Value::Bool(new.profile.camera_local_tone);
        // Compare actual HDR leaves even when persisted defaults omit the whole HDR object.
        old_json["hdr"] = serde_json::json!(old.hdr);
        new_json["hdr"] = serde_json::json!(new.hdr);
        let mut delta = json_delta(&old_json, &new_json).unwrap_or_else(|| Value::Object(Default::default()));
        if let Some(o) = delta.as_object_mut() {
            for k in ["spots", "red_eye", "version", "process", "masks"] {
                o.remove(k);
            }
            // White balance travels whole (mode, temperature, tint and the scale they were written on), as
            // Sync copies it: a lone `temp` landing on a photo still on the legacy relative scale (6500 K = as
            // shot) would be read relative to that photo's own as-shot white and show a different Kelvin (#425).
            if o.contains_key("wb") {
                o.insert("wb".into(), serde_json::json!(new.wb));
            }
        }
        let settings_changed = delta.as_object().is_some_and(|o| !o.is_empty());
        if !settings_changed && !masks_changed {
            return (Vec::new(), Vec::new());
        }
        let wb_changed = delta.get("wb").is_some();
        // a crop goes on each photo fitted to its own frame (its tightness and place, in its own
        // orientation), never the active photo's rectangle on a frame of another shape
        let crop_changed = old.crop != new.crop;
        let adims = cmd::develop::crop_dims(self, id);
        let ainfo = self.source_info(id);
        let mut compute = Vec::new();
        let ops = self
            .selection
            .ids
            .iter()
            .filter(|x| **x != id)
            .filter_map(|x| self.develop_of(*x).map(|d| (*x, d)))
            .filter_map(|(x, d)| {
                let mut synced = if settings_changed { lcc_develop::apply_partial(&d, &delta, 1.0) } else { (*d).clone() };
                if wb_changed {
                    // each photo on its own scale: Kelvin on a raw with one, relative elsewhere
                    lcc_pipeline::local::normalize_wb(&mut synced, &self.source_info(x));
                    sync_same::synced_wb(&ainfo, old, new, &self.source_info(x), &d, own_auto.get(&x).copied(), &mut synced);
                }
                if crop_changed {
                    synced.crop = sync_same::synced_crop(&new.crop, adims, cmd::develop::crop_dims(self, x));
                }
                if masks_changed {
                    let (masks, needs) = lcc_develop::recipe::sync_masks(&old.masks, &new.masks, &d.masks, self.photo_aspect(x));
                    synced.masks = masks;
                    if needs {
                        compute.push(x);
                    }
                }
                upright_jobs::as_recipe(&d, &mut synced);
                let synced = self.fit_constrained_crop(x, synced);
                (synced != *d).then(|| self.develop_op(x, synced, label)).flatten()
            })
            .collect();
        (ops, compute)
    }

    /// The mask that stays selected when the active photo goes from `from` (with mask `mask`
    /// selected) to `to`: the same mask when the active photo stays; else its copy on `to`, the one
    /// linked with it ([`lcc_develop::Mask::link`]). `None` when `to` has no such mask, or the mask
    /// isn't linked.
    pub fn carried_mask(&self, from: Option<PhotoId>, mask: Option<u32>, to: Option<PhotoId>) -> Option<u32> {
        let (mask, to) = (mask?, to?);
        let there = self.develop_of(to)?;
        if from == Some(to) {
            return there.masks.iter().any(|m| m.id == mask).then_some(mask);
        }
        let here = self.develop_of(from?)?;
        let m = here.masks.iter().find(|m| m.id == mask)?;
        if m.link.is_empty() {
            return None;
        }
        there.masks.iter().find(|t| t.link == m.link).map(|t| t.id)
    }

    /// Give the masks of `new` that differ from photo `id`'s (added or edited) and aren't linked yet
    /// a fresh link ([`lcc_develop::Mask::link`]); returns `(mask id, link)` of those.
    fn link_edited_masks(&self, id: PhotoId, new: &mut DevelopSettings) -> Vec<(u32, String)> {
        let Some(old) = self.develop_of(id) else { return Vec::new() };
        let mut out = Vec::new();
        for m in &mut new.masks {
            if m.link.is_empty() && old.masks.iter().find(|o| o.id == m.id) != Some(&*m) {
                m.link = new_mask_link(u64::from(m.id) ^ id.0);
                out.push((m.id, m.link.clone()));
            }
        }
        out
    }

    /// Auto Sync carries an edit of photo `id` labelled `label` to the other selected photos: it is
    /// on, `id` is the active photo of a selection of two or more, and the edit is not a History or
    /// snapshot restore.
    fn auto_sync_applies(&self, id: PhotoId, label: &str) -> bool {
        self.auto_sync
            && self.active() == Some(id)
            && self.selection.ids.len() >= 2
            && !label.starts_with("History:")
            && !label.starts_with("Restore ")
    }

    /// The op that sets a photo's develop settings and appends a History entry (for batches).
    pub fn develop_op(&self, id: PhotoId, settings: DevelopSettings, label: &str) -> Option<Op> {
        let p = self.catalog.photo(id)?;
        let mut settings = settings;
        // old masks (on the corrected frame) go onto the photo, from the frame they were drawn on:
        // masks this edit kept, under the photo's settings before it (a geometry edit then moves
        // the photo, not them); masks it brought in (paste, sync, a preset, a history step, a
        // sidecar), under the settings they came with, where they render now. Kept masks also
        // turn with an orientation change the edit makes (a sync or paste of the Crop group).
        let before = match &self.interaction {
            Some(i) if i.photo == id => Some(i.original.clone()),
            _ => self.develop_of(id),
        };
        let turn = before.as_ref().and_then(|b| orientation_change(b.orientation, settings.orientation));
        if settings.masks.iter().any(|m| !m.on_photo) || turn.is_some() {
            let kept_frame = before.as_ref().and_then(|b| self.mask_frame(id, b));
            let new_frame = self.mask_frame(id, &settings);
            for m in &mut settings.masks {
                let kept = before.as_ref().is_some_and(|b| b.masks.contains(m));
                if let Some(f) = if kept { kept_frame.as_ref() } else { new_frame.as_ref() } {
                    lcc_pipeline::mask_space::attach(m, f);
                }
                if let (true, Some(t)) = (kept, turn) {
                    lcc_pipeline::mask_space::turn(m, t);
                }
            }
        }
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
        let step = lcc_catalog::HistoryStep { label: label.into(), settings: settings.clone(), batch: None };
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

    /// Put the old masks of `d` (on the corrected frame, `Mask::on_photo` false) on photo `id`
    /// before the warp, from the frame of `drawn` (the settings they were drawn under): they render
    /// where they did, and from then on stay on their content when lens corrections, Upright or
    /// Transform change.
    pub fn attach_masks(&self, id: PhotoId, drawn: &DevelopSettings, d: &mut DevelopSettings) {
        if d.masks.iter().all(|m| m.on_photo) {
            return;
        }
        let Some(frame) = self.mask_frame(id, drawn) else { return };
        for m in &mut d.masks {
            lcc_pipeline::mask_space::attach(m, &frame);
        }
    }

    /// The uncropped frame of photo `id` under settings `d`, with its lens: where mask
    /// coordinates go through its warp ([`lcc_pipeline::geometry::Frame::for_mask`]).
    pub(crate) fn mask_frame(&self, id: PhotoId, d: &DevelopSettings) -> Option<lcc_pipeline::geometry::Frame> {
        let p = self.catalog.photo(id)?;
        Some(lcc_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &d.effective(),
            false,
            p.embedded_lens.as_deref(),
        ))
    }

    pub fn begin_interaction(&mut self, label: &str) -> Result<()> {
        self.begin_interaction_with(label, &[])
    }

    /// Begin an interaction that also edits `others` live (`develop.adjust` nudges each from its
    /// own value as the active photo moves; the end commits all of them as one undo step).
    pub fn begin_interaction_with(&mut self, label: &str, others: &[PhotoId]) -> Result<()> {
        if self.interaction.is_some() {
            self.end_interaction()?;
        }
        let id = self.active().ok_or_else(|| EngineError::Other("no active photo".into()))?;
        // old masks go onto the photo before a drag moves the frame under them (they then stay on
        // their content while it does, not only once it ends)
        if let Some(d) = self.develop_of(id).filter(|d| d.masks.iter().any(|m| !m.on_photo)) {
            let mut on = (*d).clone();
            self.attach_masks(id, &d, &mut on);
            self.apply_silent(Op::SetDevelop { id, settings: Arc::new(on), label: label.into(), edited: None })?;
        }
        let original = self.develop_of(id).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let others: Vec<_> = others.iter().filter(|x| **x != id && seen.insert(**x)).filter_map(|x| self.develop_of(*x).map(|d| (*x, d))).collect();
        // Auto Sync's live follow, unless the interaction already names the photos it edits along
        let along = if others.is_empty() && self.auto_sync_applies(id, label) {
            self.selection.ids.iter().filter(|x| **x != id).filter_map(|x| self.develop_of(*x).map(|d| (*x, d))).collect()
        } else {
            Vec::new()
        };
        self.interaction = Some(Interaction { label: label.into(), photo: id, original, crop_edited: false, others, along });
        Ok(())
    }

    /// Commit the interaction as one undo step (no-op if nothing changed).
    pub fn end_interaction(&mut self) -> Result<()> {
        let Some(i) = self.interaction.take() else { return Ok(()) };
        if !i.others.is_empty() {
            return self.end_multi_interaction(i);
        }
        // the photos that followed live go back; the commit below syncs them as one step with the active one
        for (x, d) in &i.along {
            self.apply_silent(Op::SetDevelop { id: *x, settings: d.clone(), label: i.label.clone(), edited: None })?;
        }
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

    /// The end of a multi-photo interaction: every photo it changed, in one undo step (Auto Sync
    /// doesn't run on top: the other photos were edited along already).
    fn end_multi_interaction(&mut self, i: Interaction) -> Result<()> {
        let mut all = vec![(i.photo, i.original.clone())];
        all.extend(i.others.iter().cloned());
        let mut ops = Vec::new();
        for (id, original) in all {
            let Some(cur) = self.develop_of(id) else { continue };
            if *cur == *original {
                continue;
            }
            self.apply_silent(Op::SetDevelop { id, settings: original.clone(), label: i.label.clone(), edited: None })?;
            let mut settings = (*cur).clone();
            // as in a one-photo interaction: a crop nobody edited goes back to the original's
            if !i.crop_edited {
                settings.crop.geometry = original.crop.geometry;
            }
            if let Some(op) = self.develop_op(id, settings, &i.label) {
                ops.push(op);
            }
        }
        match ops.len() {
            0 => Ok(()),
            1 => self.commit(&i.label, ops.remove(0)),
            _ => self.commit(&i.label, Op::Batch { ops }),
        }
    }

    /// The photos a running interaction edits along with the active one.
    pub fn interaction_others(&self) -> Vec<PhotoId> {
        self.interaction.as_ref().map(|i| i.others.iter().map(|(id, _)| *id).collect()).unwrap_or_default()
    }

    /// Set a photo's settings without an undo step while an interaction edits it along.
    pub fn set_develop_silent(&mut self, id: PhotoId, settings: DevelopSettings, label: &str) -> Result<()> {
        let now = (self.clock)();
        self.apply_silent(Op::SetDevelop { id, settings: Arc::new(settings), label: label.into(), edited: Some(now) })
    }

    pub fn cancel_interaction(&mut self) -> Result<()> {
        if let Some(i) = self.interaction.take() {
            for (id, original) in i.others.into_iter().chain(i.along) {
                self.apply_silent(Op::SetDevelop { id, settings: original, label: i.label.clone(), edited: None })?;
            }
            self.apply_silent(Op::SetDevelop { id: i.photo, settings: i.original, label: i.label, edited: None })?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- library view

    /// Shows just `photos` in the grid (none for an empty list), as Find Similar does: a filter put
    /// aside by Enable Filters is forgotten, as for any filter set while it is off.
    pub fn show_only(&mut self, photos: Vec<lcc_catalog::PhotoId>) {
        self.shelved_filter = None;
        self.filter.only_none = photos.is_empty();
        self.filter.only = photos;
    }

    /// The flag filter shows picks and unflagged photos but no rejects (View > Hide Rejected
    /// Photos, `library.hideRejected`).
    pub fn rejects_hidden(&self) -> bool {
        use lcc_catalog::Flag;
        self.filter.flags_shown().as_deref() == Some(&[Flag::Pick, Flag::None])
    }

    /// Library > Enable Filters is off: a filter is put aside (`library.toggleFilters`).
    pub fn filters_off(&self) -> bool {
        self.shelved_filter.is_some()
    }

    /// Makes the view show only albums the library holds: one that is gone is no longer the
    /// source (All Photos is) or the filter's album. THE place this is checked: commands, undo
    /// and redo, opening a library and every look at the grid run it, so whatever removed an
    /// album (a command, an undo, a background task) or named one (a saved filter, a saved view)
    /// need not know. Two lookups: it runs every time.
    ///
    /// An album that comes back (its deletion undone) is not shown again by itself, as after
    /// `album.delete` and undo before: the grid stays where the person was put.
    ///
    /// What is only looked up when it is used is left as it is: the target album (a missing one
    /// means the Quick Collection).
    pub fn reconcile_view(&mut self) {
        let cat = &self.catalog;
        let gone = |a: &lcc_catalog::AlbumId| cat.album(*a).is_none();
        if matches!(&self.source, LibrarySource::Album(a) if gone(a)) {
            self.source = LibrarySource::All;
        }
        if self.filter.album.as_ref().is_some_and(gone) {
            self.filter.album = None;
        }
    }

    /// Photos shown in the grid/filmstrip for the current source, filter and sort.
    pub fn visible(&mut self) -> &[PhotoId] {
        self.reconcile_view();
        let mut key = (
            self.catalog.revision,
            format!("{:?}|{:?}|{:?}|{:?}|{:?}|{}", self.source, self.filter, self.sort, self.browse, self.library_folder, self.folder_subfolders),
        );
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
            lcc_catalog::rules::set_now(Some(now.clone()));
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
            // its own order, a semantic search (ordered by similarity rank), Recently Added (how far back it
            // reaches moves with every import), Missing Photos.
            let own_order = matches!(self.source, LibrarySource::Album(a) if self.sort.key == lcc_catalog::SortKey::Custom
                && self.filter == Filter::default()
                && self.catalog.album(a).is_some_and(|al| !al.is_smart()));
            let whole = own_order || self.filter.semantic.is_some() || matches!(self.source, LibrarySource::RecentlyAdded | LibrarySource::Missing);
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
                return self.visible();
            }
            // a collection shown by capture time is in capture time like every view: filled card by card, it no longer
            // shows one camera after the other; its own order is Custom Order (the catalog query gives it)
            if self.source == LibrarySource::RecentlyAdded {
                // newest import first, whatever the sort direction (the grid groups by import day); inside one
                // import, the sort chosen (a two-camera import by capture time interleaves the bodies)
                // (forward in either direction: the direction flips only which import comes first)
                let (cat, sort) = (&self.catalog, lcc_catalog::Sort { ascending: true, ..self.sort });
                visible.sort_by(|a, b| match (cat.photo(*a), cat.photo(*b)) {
                    (Some(x), Some(y)) => y.imported.cmp(&x.imported).then_with(|| sort.order(x, y)),
                    _ => a.cmp(b),
                });
            }
            if self.source == LibrarySource::Missing {
                // only the photos the query kept (library photos, not Local browse records) are checked
                let (cat, avail) = (&self.catalog, &self.media.availability);
                visible.retain(|id| cmd::missing::is_missing(cat, avail, *id));
            }
            if self.filter.semantic.is_some() && !self.filter.only.is_empty() && self.source != LibrarySource::RecentlyAdded {
                // Under the default sort (CaptureDate) a semantic search shows the closest matches first
                // (their order in `self.filter.only`); under Rating it breaks rating ties by similarity.
                let rank: std::collections::HashMap<PhotoId, usize> = self.filter.only.iter().copied().enumerate().map(|(i, id)| (id, i)).collect();
                match self.sort.key {
                    lcc_catalog::SortKey::CaptureDate => {
                        visible.sort_by_key(|id| rank.get(id).copied().unwrap_or(usize::MAX));
                    }
                    lcc_catalog::SortKey::Rating => {
                        let cat = &self.catalog;
                        let asc = self.sort.ascending;
                        visible.sort_by(|a, b| {
                            let ra = cat.photo(*a).map_or(0, |p| p.rating);
                            let rb = cat.photo(*b).map_or(0, |p| p.rating);
                            let ord = if asc { ra.cmp(&rb) } else { rb.cmp(&ra) };
                            ord.then_with(|| rank.get(a).copied().unwrap_or(usize::MAX).cmp(&rank.get(b).copied().unwrap_or(usize::MAX)))
                        });
                    }
                    _ => {}
                }
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
            if self.all_selected.as_deref() == Some(key.1.as_str()) && self.selection.ids[..] == self.visible[..] && visible[..] != self.visible[..] {
                // still "every photo of this view": the photos that came in are selected too
                self.selection.ids = visible.clone();
                if self.selection.active.is_none_or(|a| !visible.contains(&a)) {
                    self.selection.active = visible.first().copied();
                }
            }
            self.visible = visible.into();
            self.visible_gen = VISIBLE_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.visible_key = Some(key);
            self.visible_clock_minute = clocked.then(|| clock_minute(&now).to_owned());
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
    /// The view's photos counted by culling mark (picks, rejects, stars, colour labels): its source and
    /// filters without the stars / flags / labels filter itself (`library.tally`, the filter bar's counts).
    pub fn tally(&self) -> lcc_catalog::Tally {
        self.catalog.tally(&self.view_filter(self.source, &self.filter.without_quick()))
    }

    /// What the People view counts faces among (issue #1131): the view's source (a folder, a collection,
    /// Missing Photos…) and the filter bar, without its own person picks. Missing Photos, which a filter
    /// can't say, becomes the list of photos found missing.
    pub fn people_filter(&self) -> Filter {
        let base = Filter { person: None, people: Vec::new(), ..self.filter.clone() };
        let mut f = self.view_filter(self.source, &base);
        if self.source == LibrarySource::Missing {
            let (cat, avail) = (&self.catalog, &self.media.availability);
            let mut ids = cat.query(&f, &lcc_catalog::Sort::default());
            ids.retain(|id| cmd::missing::is_missing(cat, avail, *id));
            f.only_none = ids.is_empty();
            f.only = ids;
        }
        f
    }

    pub(crate) fn view_filter(&self, source: LibrarySource, base: &Filter) -> Filter {
        let mut f = source.to_filter(base, &self.catalog);
        if source == LibrarySource::Folder {
            // no folder chosen: nothing (an empty path matches nothing)
            let b = self.browse.clone().unwrap_or_default();
            f.folder = Some(b.path);
            f.subfolders = b.subfolders;
            // the Browse tab's quick filter on "Show Not Imported": only the files an import
            // would take
            if b.not_imported {
                f.in_library = Some(false);
            }
        }
        if source == LibrarySource::LibraryFolder {
            // no folder chosen: nothing (`.` names no folder)
            f.library_folder = Some(self.library_folder.clone().unwrap_or_else(|| ".".into()));
            f.library_folder_only = !self.folder_subfolders;
        }
        if source == LibrarySource::PreviousExport {
            let exported = self.previous_export_filter();
            if f.only_none {
                f.only.clear();
            } else if !f.only.is_empty() {
                let keep: std::collections::HashSet<PhotoId> = exported.into_iter().collect();
                f.only.retain(|id| keep.contains(id));
                if f.only.is_empty() {
                    f.only_none = true;
                }
            } else {
                f.only = exported;
                f.only_none = false;
            }
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
        self.never_follow = true;
        self.visible_key = None;
        let whole = self.visible().to_vec();
        self.never_follow = false;
        (self.base, self.visible_key, self.visible_clock_minute, self.visible, self.visible_gen, self.view_followed, self.view_worked_out) = kept;
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
        self.reconcile_view();
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
        let key = (self.catalog.revision, format!("{:?}|{:?}|{:?}|{}", self.source, self.browse, self.library_folder, self.folder_subfolders));
        let key_changed = self.total.as_ref().map(|total| &total.0) != Some(&key);
        let cached_minute = self.total.as_ref().and_then(|total| total.2.as_deref());
        let current_now = cached_minute.map(|_| (self.clock)());
        let current_minute = current_now.as_deref().map(clock_minute);
        let clock_changed = cached_minute.is_some_and(|cached| current_minute != Some(cached));
        if key_changed || clock_changed {
            // The source count can be requested before `visible`; bind it to the session's clock itself.
            let now = current_now.unwrap_or_else(|| (self.clock)());
            lcc_catalog::rules::set_now(Some(now.clone()));
            let mut f = self.source.to_filter(&Filter::default(), &self.catalog);
            if self.source == LibrarySource::Folder {
                let b = self.browse.clone().unwrap_or_default();
                f.folder = Some(b.path);
                f.subfolders = b.subfolders;
                if b.not_imported {
                    f.in_library = Some(false);
                }
            }
            if self.source == LibrarySource::LibraryFolder {
                f.library_folder = Some(self.library_folder.clone().unwrap_or_else(|| ".".into()));
                f.library_folder_only = !self.folder_subfolders;
            }
            if self.source == LibrarySource::PreviousExport {
                f.only = self.previous_export_filter();
                f.only_none = false;
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
    /// Names the shown source for [`Session::source_filters`] (a folder or collection by its id/path).
    pub(crate) fn source_key(&self) -> String {
        match self.source {
            LibrarySource::LibraryFolder => format!("folder:{}", lcc_catalog::query::folder_key(self.library_folder.as_deref().unwrap_or_default())),
            other => format!("{other:?}"),
        }
    }

    /// After the source changed from `left` (key, filter it had): with Lock Filters off, a new source
    /// starts unfiltered, or with the filter it had when last left when remembering per source.
    pub(crate) fn switch_source_filter(&mut self, left_key: String, left_filter: Filter) {
        if self.lock_filters {
            return;
        }
        let key = self.source_key();
        if key == left_key {
            return;
        }
        if self.remember_filters {
            // (bounded: one entry per source ever left, folders can be many)
            if self.source_filters.len() < 10_000 || self.source_filters.contains_key(&left_key) {
                self.source_filters.insert(left_key, left_filter);
            }
        }
        self.filter = if self.remember_filters { self.source_filters.get(&key).cloned().unwrap_or_default() } else { Filter::default() };
        // one folder / collection at a time, as `library.source` does
        self.filter.album = None;
        if self.source == LibrarySource::LibraryFolder {
            self.filter.library_folder = None;
        }
        self.shelved_filter = None;
    }

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

    /// The photos a Develop slider changes: the active photo, and with Auto Sync on every selected
    /// photo (the active one first).
    pub fn slider_targets(&self) -> Vec<PhotoId> {
        let Some(active) = self.active() else { return Vec::new() };
        if !self.auto_sync || self.selection.ids.len() < 2 || !self.selection.contains(active) {
            return vec![active];
        }
        std::iter::once(active).chain(self.selection.ids.iter().copied().filter(|x| *x != active)).collect()
    }

    /// Targets of photo commands: the explicit `ids` (photo ids) or `id` (a photo id) param, else [`Self::selected`].
    /// An `ids` or `id` that is there but isn't photo ids is an error, never the selection or part of the list
    /// (issue #1506): a mistyped target mustn't turn a command meant for one photo into one for every selected photo.
    pub fn targets(&self, p: &Value) -> Result<Vec<PhotoId>> {
        let invalid = |what: &str| EngineError::BadParams { cmd: String::new(), msg: format!("`{what}` must be photo ids (whole numbers)") };
        match p.get("ids") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                return a
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        v.as_u64().map(PhotoId).ok_or_else(|| EngineError::BadParams {
                            cmd: String::new(),
                            msg: format!("`ids[{i}]` is {v}, not a photo id (whole number)"),
                        })
                    })
                    .collect();
            }
            Some(_) => return Err(invalid("ids")),
        }
        match p.get("id") {
            None | Some(Value::Null) => Ok(self.selected()),
            Some(v) => v.as_u64().map(|id| vec![PhotoId(id)]).ok_or_else(|| invalid("id")),
        }
    }

    /// The photo id in param `key`: `None` when absent or null, an error when it is there but is not
    /// a photo id (a mistyped id must never fall back to the active photo).
    pub fn photo_param(p: &Value, key: &str, cmd: &str) -> Result<Option<PhotoId>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .map(|id| Some(PhotoId(id)))
                .ok_or_else(|| EngineError::BadParams { cmd: cmd.to_string(), msg: format!("`{key}` is {v}, not a photo id (whole number)") }),
        }
    }

    /// The photo in `id`, else the active photo; a malformed `id` is an error, not the active photo.
    pub fn photo_or_active(&self, p: &Value, cmd: &str) -> Result<PhotoId> {
        Self::photo_param(p, "id", cmd)?
            .or(self.active())
            .ok_or_else(|| EngineError::BadParams { cmd: cmd.to_string(), msg: "no photo (select one or give `id`)".into() })
    }

    /// The photos commands act on when a call names none: the selection, else the active photo.
    /// As far as its photos are there.
    pub fn selected(&self) -> Vec<PhotoId> {
        let there = |id: &PhotoId| self.catalog.photo(*id).is_some();
        if self.selection.ids.is_empty() {
            self.selection.active.into_iter().filter(there).collect()
        } else {
            self.selection.ids.iter().copied().filter(there).collect()
        }
    }
}

/// The first entry of an `ids` array that is not a photo id, described for an error message
/// (`ids[1] is "x", not a photo id`), or `None` when every entry is one.
pub fn bad_ids_entry(a: &[Value]) -> Option<String> {
    a.iter().enumerate().find(|(_, v)| v.as_u64().is_none()).map(|(i, v)| format!("`ids[{i}]` is {v}, not a photo id (whole number)"))
}

/// Undo steps kept; the oldest go first.
pub const UNDO_STEPS: usize = 1000;

/// Automatic versions kept per photo.
pub const AUTO_VERSIONS: usize = 20;

/// A new mask link ([`lcc_develop::Mask::link`]): 16 hex digits from the process's random hash
/// keys, a counter and `salt` (unique within a library in practice; no clock, so it builds for wasm).
pub fn new_mask_link(salt: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(n);
    h.write_u64(salt);
    format!("{:016x}", h.finish())
}

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

/// Whether `op` changes where a photo's file is, or removes or replaces it.
fn op_moves_files(op: &Op) -> bool {
    let mut pending = vec![op];
    while let Some(op) = pending.pop() {
        match op {
            Op::SetFile { .. } | Op::Relink { .. } | Op::RemovePhoto { .. } | Op::SetContent { .. } => return true,
            Op::Batch { ops } => pending.extend(ops),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod lightroom_fixture;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_album_loops;
#[cfg(test)]
mod tests_album_missing;
#[cfg(test)]
mod tests_album_order;
#[cfg(test)]
mod tests_album_remove;
#[cfg(test)]
mod tests_albums_local;
#[cfg(test)]
mod tests_app_store;
#[cfg(test)]
mod tests_auto_import;
#[cfg(test)]
mod tests_auto_mask_range;
#[cfg(test)]
mod tests_autosync;
#[cfg(test)]
mod tests_autosync_crop;
#[cfg(test)]
mod tests_autosync_live;
#[cfg(test)]
mod tests_autosync_preset;
#[cfg(test)]
mod tests_autosync_wb;
#[cfg(test)]
mod tests_background_merge;
#[cfg(test)]
mod tests_browse_profile;
#[cfg(test)]
mod tests_camera_layers;
#[cfg(test)]
mod tests_camera_profile_defaults;
#[cfg(test)]
mod tests_capture_sort;
#[cfg(test)]
mod tests_catalog_cmds;
#[cfg(test)]
mod tests_chart_profile;
#[cfg(test)]
mod tests_classic;
#[cfg(test)]
mod tests_color;
#[cfg(test)]
mod tests_concepts;
#[cfg(test)]
mod tests_constrain_crop;
#[cfg(test)]
mod tests_crop_angle;
#[cfg(test)]
mod tests_crop_aspect;
#[cfg(test)]
mod tests_crop_aspects;
#[cfg(test)]
mod tests_crop_same;
#[cfg(test)]
mod tests_cull_suggest;
#[cfg(test)]
mod tests_denoise;
#[cfg(test)]
#[cfg(feature = "denoise")]
mod tests_denoise_own;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_dng_update;
#[cfg(test)]
mod tests_dng_validate;
#[cfg(test)]
mod tests_edit_in;
#[cfg(test)]
mod tests_edit_roundtrip;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_enhance;
#[cfg(test)]
mod tests_export;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_export_video;
#[cfg(test)]
mod tests_eye_store;
#[cfg(test)]
mod tests_face_models;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_face_recognize;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_face_tiles;
#[cfg(test)]
mod tests_facets;
#[cfg(test)]
mod tests_filesystem_history;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_focus_scan;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_folder_metadata;
#[cfg(test)]
mod tests_folder_watch;
#[cfg(test)]
mod tests_folders;
#[cfg(test)]
mod tests_forget_local;
#[cfg(test)]
mod tests_fuji;
#[cfg(test)]
mod tests_genfill;
#[cfg(test)]
mod tests_history_cursor;
#[cfg(test)]
mod tests_history_groups;
#[cfg(test)]
mod tests_hold_view;
#[cfg(test)]
mod tests_hostile_args;
#[cfg(test)]
mod tests_ids_params;
#[cfg(test)]
mod tests_immich;
#[cfg(test)]
mod tests_import;
#[cfg(test)]
mod tests_import_local;
#[cfg(test)]
mod tests_import_move;
#[cfg(test)]
mod tests_import_tab;
#[cfg(test)]
mod tests_keyword_list;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_lens_db;
#[cfg(test)]
mod tests_libops;
#[cfg(test)]
mod tests_library;
#[cfg(test)]
mod tests_lightroom_folder;
#[cfg(test)]
mod tests_lightroom_history;
#[cfg(test)]
mod tests_mask_effects;
#[cfg(test)]
mod tests_mask_geometry;
#[cfg(test)]
mod tests_mask_links;
#[cfg(test)]
mod tests_mask_next;
#[cfg(test)]
mod tests_merge;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_ml;
#[cfg(test)]
mod tests_move_to_share;
#[cfg(test)]
mod tests_naming;
#[cfg(test)]
mod tests_organize;
#[cfg(test)]
#[cfg(feature = "sam")]
mod tests_people_corpus;
#[cfg(test)]
mod tests_people_masks;
#[cfg(test)]
mod tests_people_scope;
#[cfg(test)]
mod tests_persist;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_plugin_develop;
#[cfg(test)]
mod tests_plugin_filters;
#[cfg(test)]
mod tests_plugin_installs;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_plugin_library;
#[cfg(test)]
mod tests_prefs;
#[cfg(test)]
mod tests_preset_interop;
#[cfg(test)]
mod tests_preset_marks;
#[cfg(test)]
mod tests_presets_named;
#[cfg(test)]
mod tests_presets_scale;
#[cfg(test)]
mod tests_preview_prep;
#[cfg(test)]
mod tests_previews;
#[cfg(test)]
mod tests_print;
#[cfg(test)]
mod tests_process;
#[cfg(test)]
mod tests_profile_manager;
#[cfg(test)]
mod tests_recheck;
#[cfg(test)]
mod tests_retouch;
#[cfg(test)]
mod tests_segment;
#[cfg(test)]
mod tests_select_menu;
#[cfg(test)]
mod tests_sensor;
#[cfg(test)]
mod tests_settings_files;
#[cfg(test)]
mod tests_slide;
#[cfg(test)]
mod tests_slideshow_export;
#[cfg(test)]
mod tests_source_report;
#[cfg(test)]
mod tests_spots;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_sync_copies;
#[cfg(test)]
mod tests_target_params;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_timelapse;
#[cfg(test)]
mod tests_upright_jobs;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests_video_posters;
#[cfg(test)]
mod tests_vignetting_calibration_commands;
#[cfg(test)]
mod tests_vignetting_calibration_portable;
#[cfg(test)]
mod tests_vignetting_calibration_render;
/// The turn or flip that takes orientation `from` to `to` (`None` when they are the same): what the
/// masks, on the oriented photo, must follow ([`lcc_pipeline::mask_space::turn`]).
pub(crate) fn orientation_change(from: lcc_geom::Orientation, to: lcc_geom::Orientation) -> Option<lcc_geom::Orientation> {
    use lcc_geom::Orientation;
    if from == to {
        return None;
    }
    let inv = from.inverse();
    let probes = [(0.2, 0.3), (0.7, 0.1), (0.4, 0.9)];
    let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    (0..8u8).map(|k| Orientation::from_parts(k >= 4, k % 4)).find(|t| {
        probes.iter().all(|&(x, y)| {
            let (rx, ry) = inv.map(x, y, 1.0, 1.0);
            let (tx, ty) = from.map(rx, ry, 1.0, 1.0);
            close(t.map(tx, ty, 1.0, 1.0), to.map(rx, ry, 1.0, 1.0))
        })
    })
}

#[cfg(test)]
mod tests_view_follow;
#[cfg(test)]
mod tests_view_reconcile;
#[cfg(test)]
mod tests_xmp;

#[cfg(test)]
mod photo_param_tests {
    use super::*;
    use serde_json::json;

    fn session_with_active() -> (Session, PhotoId) {
        let mut s = Session::new();
        let id = s.catalog.alloc_photo_id();
        let photo =
            lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: "/nonexistent/a.jpg".into() }, "a.jpg", "JPEG", 100, 100, "2026-01-01");
        s.commit("Add test photo", lcc_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        s.selection = Selection::single(id);
        (s, id)
    }

    /// A mistyped `id` is an error naming it, never the active photo.
    #[test]
    fn bad_id_types_error_instead_of_using_the_active_photo() {
        let (mut s, id) = session_with_active();
        assert_eq!(s.photo_or_active(&json!({}), "t").unwrap(), id);
        for bad in [json!("1"), json!(-1), json!(1.5), json!([1]), json!({"id": 1})] {
            let e = s.photo_or_active(&json!({ "id": bad }), "t").unwrap_err().to_string();
            assert!(e.contains("`id`"), "{e}");
            assert!(s.execute("stack.setTop", &json!({ "id": bad })).is_err());
            assert!(s.execute("album.setCover", &json!({ "id": 1, "photo": bad })).is_err());
        }
    }

    /// A bad entry in `ids` is an error naming that entry, not silently dropped.
    #[test]
    fn bad_ids_entries_error_naming_the_entry() {
        let (s, id) = session_with_active();
        let e = s.targets(&json!({"ids": [id.0, "x", 3]})).unwrap_err().to_string();
        assert!(e.contains("ids[1]") && e.contains("\"x\""), "{e}");
        assert_eq!(s.targets(&json!({"ids": [id.0]})).unwrap(), vec![id]);
    }
}
