//! A persistent library: a directory holding the catalog (op log + snapshot), user presets, the
//! last view state and the preview cache.
//!
//! ```text
//! <Name> Library/
//!   catalog.snap   catalog.log      (lcc-catalog journal)
//!   presets.json   view.json        (user presets + favourites; last source/sort/selection)
//!   prefs.json     (library preferences: XMP sidecars, import defaults, cache size, last export)
//!   thumbs/        (rendered thumbnail cache, safe to delete)
//!   Originals/     (photos imported with "copy into library")
//! ```
//!
//! The files live in [`Store`]s: a directory ([`FsStore`]) natively, or any other implementation
//! via [`Session::open_library_in`] (the browser build keeps them in OPFS or IndexedDB).
//!
//! Every top-level [`Session::execute`] persists the ops it produced (fsynced) before returning,
//! so a crash loses at most the command in flight. When that write fails the command returns
//! [`EngineError::NotSaved`]: its change stays applied in memory and queued, and every later save
//! retries the queue ([`Session::unsaved`] reports it meanwhile), so nothing is lost once the
//! disk is writable again. The log is compacted into a snapshot when it
//! grows (see [`lcc_catalog::SnapshotPolicy`]; written by a worker thread on native, see
//! [`lcc_catalog::journal`]) and on [`Session::close_library`].

use std::path::{Path, PathBuf};

use lcc_catalog::{FsStore, Journal, LibraryLock, LoadReport, Store};
use lcc_develop::Preset;
use serde::{Deserialize, Serialize};

use crate::{EngineError, LibrarySource, Result, Selection, Session};

/// The catalog migration (v3 → v4) running in this process, if any: what frontends show while
/// [`migrate_library`] runs on a worker thread.
pub fn migration_progress() -> Option<lcc_catalog::progress::MigrationProgress> {
    lcc_catalog::progress::migration()
}

/// Whether opening the library in `dir` first upgrades its catalog (a one-time step that takes
/// tens of seconds for a library of hundreds of thousands of photos).
#[cfg(not(target_arch = "wasm32"))]
pub fn needs_migration(dir: &Path) -> bool {
    lcc_catalog::library::needs_migration(dir)
}

/// Upgrade the catalog of the library in `dir` without opening it in a session, so a frontend
/// can run it on a worker thread and show [`migration_progress`]; [`Session::open_library`] is
/// then a plain open. The library is locked meanwhile (another process using it: `LibraryInUse`).
/// `Ok(true)` when it was upgraded.
#[cfg(not(target_arch = "wasm32"))]
pub fn migrate_library(dir: &Path) -> Result<bool> {
    let _lock = LibraryLock::acquire(dir, &program_name()).map_err(|e| EngineError::LibraryInUse(e.to_string()))?;
    Ok(lcc_catalog::library::migrate(dir)?)
}

/// Prepare the library in `dir` for opening without a session, so a frontend can run it on a
/// worker thread and show [`migration_progress`] while its window is up: the v3 → v4 migration
/// ([`migrate_library`]), or a v4 store's format bump — the byte-verified backup copy after an
/// update, which over a store of gigabytes is most of the wait. [`Session::open_library`]
/// afterwards finds the current format and is quick. The library is locked meanwhile (another
/// process using it: `LibraryInUse`). `Ok(true)` when something was upgraded.
#[cfg(not(target_arch = "wasm32"))]
pub fn prepare_library(dir: &Path) -> Result<bool> {
    if needs_migration(dir) {
        return migrate_library(dir);
    }
    let _lock = LibraryLock::acquire(dir, &program_name()).map_err(|e| EngineError::LibraryInUse(e.to_string()))?;
    Ok(lcc_catalog::library::prepare_upgrade(dir)?)
}

/// The library folder `path` names. A library is a folder; naming one of its own files
/// (`catalog.snap`, `catalog.log`, `catalog.lock`) means the folder that holds it. Any other
/// existing file is an error that says so, instead of the OS's "File exists" from creating the
/// folder.
pub fn resolve_dir(path: &Path) -> Result<PathBuf> {
    if !path.is_file() {
        return Ok(path.to_path_buf());
    }
    let own = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| [lcc_catalog::journal::SNAPSHOT, lcc_catalog::journal::LOG, lcc_catalog::lock::LOCK].contains(&n));
    if !own {
        return Err(EngineError::Other(format!("{} is a file; a catalog is a folder", path.display())));
    }
    Ok(match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from("."),
    })
}

/// Whether `dir` already holds a library (its catalog files).
pub fn is_library(dir: &Path) -> bool {
    dir.join(lcc_catalog::journal::SNAPSHOT).is_file() || dir.join(lcc_catalog::journal::LOG).is_file()
}

/// Library directory name inside the user's Pictures folder.
pub const DEFAULT_NAME: &str = lcc_brand::LIBRARY_DEFAULT;

/// The default library location: `{ENV_PREFIX}_LIBRARY` if set, else `~/Pictures/<`[`DEFAULT_NAME`]`>`
/// (`%USERPROFILE%\Pictures\…` on Windows; the host's [`lcc_brand::PlatformDirs::pictures`] on Android). When that folder doesn't exist but a library under a
/// previous default name does ([`lcc_brand::library_default_names`]), that one is used, so an
/// existing library keeps opening after a rename.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(p) = lcc_brand::env_os("LIBRARY").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    // a host without a home folder (Android) says where pictures go
    if let Some(d) = lcc_brand::platform_dirs() {
        return Some(default_in(&d.pictures));
    }
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE") } else { std::env::var_os("HOME") }?;
    Some(default_in(&PathBuf::from(home).join("Pictures")))
}

/// The default library folder inside `pictures`: the current name, unless only a previous one exists.
pub fn default_in(pictures: &Path) -> PathBuf {
    let current = pictures.join(DEFAULT_NAME);
    if current.exists() {
        return current;
    }
    lcc_brand::library_default_names().into_iter().skip(1).map(|n| pictures.join(n)).find(|p| p.is_dir()).unwrap_or(current)
}

pub struct Library {
    /// The library directory, or a descriptive pseudo-path when the library isn't on disk.
    pub dir: PathBuf,
    /// The library is a real directory: photos can be copied into `Originals/` and thumbnails are
    /// cached in `thumbs/`.
    pub on_disk: bool,
    journal: Journal,
    /// The activity-stack row of the background compaction in flight (issue #448): held exactly
    /// while [`Journal::snapshot_running`], by [`Session::sync_compaction_row`].
    compaction_row: Option<crate::activity::TaskGuard>,
    /// presets.json, view.json, prefs.json (and the unfinished import, [`crate::finishing`]).
    pub(crate) files: Box<dyn Store>,
    /// What happened when the catalog was loaded.
    pub report: LoadReport,
    /// Last persistence error (shown by the UI; ops stay pending and are retried).
    pub last_error: Option<String>,
    /// Why the last append of queued ops failed; `None` once a save succeeds. While set, changes
    /// exist only in memory ([`Session::unsaved`]).
    pub unsaved_error: Option<String>,
    /// Why the last write of presets.json failed; `None` once it is written. While set, the preset
    /// changes exist only in memory and count as unsaved ([`Session::unsaved`], issue #2266).
    presets_error: Option<String>,
    /// When the frame loop may retry a failed append ([`Session::persist_if_dirty`] backs off).
    retry_at: Option<web_time::Instant>,
    /// What forgetting untouched Local records did when the library opened.
    pub forgot_local: Option<lcc_catalog::ForgetPlan>,
    /// What presets.json holds (see [`PresetsSaved`]).
    presets_saved: PresetsSaved,
    view_written: Vec<u8>,
    /// Settings files that were unreadable or damaged when the library opened (`library.info` →
    /// `settingsWarnings`; shown by the UI once, see [`Session::take_library_warnings`]).
    pub settings_warnings: Vec<String>,
    warnings_reported: usize,
    /// Settings files that couldn't be read or set aside: never overwritten this session.
    blocked: Vec<&'static str>,
    /// Keeps other processes out of this library while it's open (last: released after the
    /// journal's background snapshot has landed).
    lock: Option<LibraryLock>,
    /// The library keeps its own presets, profiles and habits (its prefs.json says so, or there is
    /// no app store); otherwise the app store holds them ([`crate::app_store`]).
    pub own_store: bool,
    /// The library asked to keep its own (`storeWithLibrary` in prefs.json).
    pub store_with_library: bool,
    /// While the app store holds them: the library's own copies of those prefs.json keys, as read,
    /// written back unchanged.
    frozen: serde_json::Map<String, serde_json::Value>,
}

/// Where a library's files live (see [`Session::open_library_in`]).
pub struct LibraryStores {
    /// Shown as the library's location (`library.info`).
    pub dir: PathBuf,
    /// The catalog journal (`catalog.snap`, `catalog.log`).
    pub catalog: Box<dyn Store>,
    /// presets.json, view.json, prefs.json.
    pub files: Box<dyn Store>,
    /// `dir` is a real directory (enables `Originals/` copies and the `thumbs/` disk cache).
    pub on_disk: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct PresetsFile {
    pub user: Vec<Preset>,
    pub folders: Vec<String>,
    pub favorites: Vec<String>,
    /// Presets marked At Import, and those hidden from the panel and pickers (any preset, built-in too).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub at_import: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Built-in presets renamed or moved to another group in the Presets Manager.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub builtin_names: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub builtin_groups: std::collections::BTreeMap<String, String>,
    /// Favourite profiles, and the recently applied ones (newest first).
    pub profile_favorites: Vec<String>,
    pub profile_recent: Vec<String>,
    /// Profiles the Profile Manager hides.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    profile_hidden: Vec<String>,
    /// The Profile Manager's renames, regroupings, order and removed looks.
    #[serde(skip_serializing_if = "crate::profile_edits::ProfileEdits::is_empty")]
    profile_edits: crate::profile_edits::ProfileEdits,
    /// Each camera body's camera profile.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    body_profiles: std::collections::BTreeMap<String, String>,
}

/// The state presets.json was last written (or read) from. Saving checks the preset list's
/// generation and the small profile lists before serializing anything: with a 10 000-preset
/// collection presets.json is ~40 MB, and serializing it after every command made each slider
/// tick take seconds.
#[derive(Default)]
pub(crate) struct PresetsSaved {
    generation: u64,
    folders: Vec<String>,
    profile_favorites: Vec<String>,
    profile_recent: Vec<String>,
    profile_hidden: Vec<String>,
    profile_edits: crate::profile_edits::ProfileEdits,
    body_profiles: std::collections::BTreeMap<String, String>,
    at_import: Vec<String>,
    hidden: Vec<String>,
    builtin_names: std::collections::BTreeMap<String, String>,
    builtin_groups: std::collections::BTreeMap<String, String>,
    /// Hash of the bytes written: a list borrowed mutably but left as it was isn't rewritten.
    hash: u64,
}

impl PresetsSaved {
    pub(crate) fn state_of(s: &Session) -> PresetsSaved {
        PresetsSaved {
            generation: s.presets.generation(),
            folders: s.preset_folders.clone(),
            profile_favorites: s.profile_favorites.clone(),
            profile_recent: s.profile_recent.clone(),
            profile_hidden: s.profile_hidden.clone(),
            profile_edits: s.profile_edits.clone(),
            body_profiles: s.body_profiles.clone(),
            at_import: s.preset_at_import.clone(),
            hidden: s.preset_hidden.clone(),
            builtin_names: s.preset_builtin_names.clone(),
            builtin_groups: s.preset_builtin_groups.clone(),
            hash: 0,
        }
    }

    /// `s` changed its presets or profile lists since this state.
    pub(crate) fn differs(&self, s: &Session) -> bool {
        self.generation != s.presets.generation()
            || self.folders != s.preset_folders
            || self.profile_favorites != s.profile_favorites
            || self.profile_recent != s.profile_recent
            || self.profile_hidden != s.profile_hidden
            || self.profile_edits != s.profile_edits
            || self.body_profiles != s.body_profiles
            || self.at_import != s.preset_at_import
            || self.hidden != s.preset_hidden
            || self.builtin_names != s.preset_builtin_names
            || self.builtin_groups != s.preset_builtin_groups
    }
}

pub(crate) fn bytes_hash(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

#[cfg(test)]
thread_local! {
    static PRESETS_SERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread serialized the preset list (tests).
#[cfg(test)]
pub(crate) fn presets_serialized() -> usize {
    PRESETS_SERIALIZED.with(|c| c.get())
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct ViewFile {
    source: LibrarySource,
    browse: Option<crate::Browse>,
    /// The folder a `libraryFolder` source shows.
    library_folder: Option<String>,
    /// Show Photos in Subfolders turned off (`false` is the default, so it is written only when off).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    folder_direct: bool,
    /// Lock Filters turned off (`true` is the default, so it is written only when off).
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_filters: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    remember_filters: bool,
    // No filter: a library opens unfiltered. A date, keyword or person left over from the last session
    // would silently hide photos, with only a small badge to say so.
    sort: lcc_catalog::Sort,
    selection: Selection,
    /// The photos of the last export (the Previous Export source).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    previous_export: Vec<lcc_catalog::PhotoId>,
}

impl Library {
    /// A small file of the library's own (`people.json`): `None` when missing.
    pub(crate) fn read_file(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }

    /// Replace a small file of the library's own atomically.
    pub(crate) fn write_file(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.write_atomic(name, data)
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }
    #[cfg(test)]
    pub(crate) fn journal_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }
    /// presets.json is written at the next persist, whatever it holds.
    pub(crate) fn forget_presets_saved(&mut self) {
        self.presets_saved = PresetsSaved::default();
    }
    pub fn thumbs_dir(&self) -> PathBuf {
        self.dir.join("thumbs")
    }
    pub fn originals_dir(&self) -> PathBuf {
        self.dir.join("Originals")
    }
}

/// The library in `dir`'s stable id (its catalog settings' `libraryId`), made and saved the first
/// time. When it can't be saved, an id derived from the folder stands in (stable while that lasts).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn library_id(dir: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut cs = lcc_catalog::library::CatalogSettings::load(dir);
    if let Some(id) = cs.library_id.as_ref().filter(|s| !s.is_empty()) {
        return id.clone();
    }
    let hash = |fresh: bool| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dir.hash(&mut h);
        if fresh {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default().hash(&mut h);
            std::process::id().hash(&mut h);
        }
        format!("{:016x}", h.finish())
    };
    let id = hash(true);
    cs.library_id = Some(id.clone());
    match cs.save(dir) {
        Ok(()) => id,
        Err(e) => {
            log::warn!("library id: {e}");
            format!("path-{}", hash(false))
        }
    }
}

/// The folder stamps of the last poll ([`crate::folder_watch`]).
const FOLDER_STAMPS: &str = "folder-stamps.json";

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PrefsFile {
    xmp: crate::sidecar::XmpPrefs,
    /// Parameters of the last export (for Export with Previous).
    #[serde(alias = "last_export")]
    last_export: Option<serde_json::Value>,
    /// Options of the last Photo Merge, per command.
    last_merge: std::collections::BTreeMap<String, serde_json::Value>,
    /// The user's export presets.
    export_presets: Vec<crate::export::ExportPreset>,
    /// Print templates.
    print_templates: Vec<crate::print::template::PrintTemplate>,
    /// Print Template Browser folders, empty ones included.
    print_template_folders: Vec<String>,
    /// Metadata presets.
    metadata_presets: Vec<crate::cmd::metadata::MetadataPreset>,
    /// Import presets (the Import tab's settings, by name).
    import_presets: Vec<crate::cmd::import_tab::ImportPreset>,
    /// Filter presets.
    filter_presets: Vec<crate::cmd::filters::FilterPreset>,
    /// Point-curve presets.
    curve_presets: Vec<crate::cmd::curves::CurvePreset>,
    /// The crop tool's saved and hidden aspect ratios.
    crop_aspects: crate::cmd::crop_aspects::CropAspects,
    /// Mask effect presets.
    mask_effects: Vec<crate::cmd::mask_effects::MaskEffect>,
    /// The effect new masks of each kind start with.
    mask_tool_effects: std::collections::BTreeMap<String, String>,
    /// Colour-label name sets.
    label_sets: Vec<crate::cmd::manage::LabelSet>,
    /// File-name templates saved by name (Rename Photos, Export).
    naming_presets: Vec<crate::cmd::manage::NamingPreset>,
    /// Imported LUT profiles.
    lut_profiles: Vec<crate::cmd::lut_profiles::LutProfile>,
    /// Keyword sets, the one in use, recent keywords.
    keyword_sets: Vec<crate::cmd::keywords::KeywordSet>,
    keyword_set: Option<String>,
    recent_keywords: Vec<String>,
    /// The single keyword shortcut of older libraries; read, written empty.
    keyword_shortcut: Option<String>,
    keyword_shortcuts: Vec<String>,
    /// Develop defaults for imported photos.
    import: crate::import::ImportDefaults,
    /// The default camera profile per camera model (`""` = every camera; issue #723).
    camera_profile_defaults: std::collections::BTreeMap<String, String>,
    /// Thumbnail disk cache budget (MB, 0 = default).
    cache_mb: u32,
    /// This library's own background preparation settings (`None`: the app-wide ones, [`crate::prep`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    prep: Option<crate::prep::PrepSettings>,
    /// A background build of this library's previews was started and has not run to its end: the
    /// preparation takes it up again, also after a restart. Set and cleared by
    /// [`Session::prep_tick`] only; the rules are at [`crate::prep::PrepState::unfinished`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    prep_unfinished: bool,
    /// Folder for smart previews (default: `Smart Previews` in the library).
    smart_previews_dir: Option<String>,
    /// Days after which untouched Local records of unbrowsed folders are forgotten (missing =
    /// the default, 0 = never).
    forget_local_days: Option<u32>,
    /// Preview store settings (standard size, 1:1 discard, previews built at import).
    previews: crate::cmd::previews::PreviewPrefs,
    /// Slideshow playback settings.
    slideshow: crate::slideshow::PlaybackSettings,
    /// Slideshow slide layout (margins, options, backdrop).
    slide_layout: crate::slide::SlideSettings,
    /// Slideshow overlays.
    slide_overlays: crate::slide_overlay::OverlaySettings,
    /// Slideshow intro and ending screens.
    slide_titles: crate::slide_title::TitleSettings,
    /// A view holds the photos it shows until it is refreshed (`library.holdView`).
    hold_view: bool,
    /// Folder polling (issue #123).
    folder_watch: crate::folder_watch::WatchPrefs,
    /// Keep presets, profiles and habits with this library instead of the app store
    /// ([`crate::app_store`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    store_with_library: bool,
    /// What dialogs remember (while the library keeps its own; else in the app store).
    #[serde(skip_serializing_if = "crate::app_store::DialogMemory::is_default", deserialize_with = "crate::app_store::lenient_dialogs")]
    dialogs: crate::app_store::DialogMemory,
}

/// The keys of `store`'s prefs.json the app store holds, as they are on disk.
fn read_frozen(store: &mut dyn Store) -> serde_json::Map<String, serde_json::Value> {
    let mut frozen = serde_json::Map::new();
    if let Ok(Some(raw)) = store.read("prefs.json")
        && let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&raw)
    {
        for k in crate::app_store::SHARED_PREFS_KEYS {
            if let Some(v) = map.get(k) {
                frozen.insert(k.to_string(), v.clone());
            }
        }
    }
    frozen
}

pub(crate) fn presets_json(s: &Session) -> String {
    #[cfg(test)]
    PRESETS_SERIALIZED.with(|c| c.set(c.get() + 1));
    // (borrowed: the 10 000+ presets of an imported collection are not copied to be written)
    crate::presets_save::Snapshot::of(s).json()
}

/// Finish a background compaction that is done (cheap otherwise). A failed one lost nothing (the
/// log is kept whole) and is retried later; it is reported like other persistence errors.
fn poll_compaction(lib: &mut Library) {
    if let Err(e) = lib.journal.poll() {
        log::error!("library: compaction: {e}");
        lib.last_error = Some(format!("compaction: {e}"));
    }
}

/// How long the frame loop waits before retrying a failed append (commands retry at once).
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// `a` and `b` name the same directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// This program, for the lock owner note (the app's display name, or the executable's name).
fn program_name() -> String {
    let exe = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()));
    match exe.as_deref() {
        None => lcc_brand::DISPLAY_NAME.into(),
        Some(n) if n == lcc_brand::BINARY || n == crate::legacy::BINARY => lcc_brand::DISPLAY_NAME.into(),
        Some(other) => other.to_string(),
    }
}

/// Problems reading the settings files (`presets.json`, `prefs.json`, `view.json`) when the library
/// opened (issue #103). A missing file is not a problem (defaults). A file that doesn't parse is
/// kept as `<name>.corrupt-<unix time>` before the defaults are used (so the next save can't
/// lose it); a file that can't be read at all (locked by another program, I/O error) is never
/// written this session, so its content survives until the library is opened again.
#[derive(Default)]
struct SettingsLoad {
    warnings: Vec<String>,
    /// Files not to overwrite this session.
    blocked: Vec<&'static str>,
}

impl SettingsLoad {
    fn read<T: serde::de::DeserializeOwned>(&mut self, store: &mut dyn Store, name: &'static str) -> Option<T> {
        let bytes = match store.read(name) {
            Ok(b) => b?,
            Err(e) => {
                log::error!("library: {name}: {e}");
                self.blocked.push(name);
                self.warnings.push(format!(
                    "{name} couldn't be read ({e}). the app uses the defaults for now and won't overwrite the file; reopen the library to try again."
                ));
                return None;
            }
        };
        let err = match serde_json::from_slice(&bytes) {
            Ok(v) => return Some(v),
            Err(e) => e,
        };
        let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let keep = format!("{name}.corrupt-{secs}");
        log::error!("library: {name} is damaged: {err}");
        match store.write_atomic(&keep, &bytes) {
            Ok(()) => self.warnings.push(format!("{name} is damaged ({err}). It was kept as {keep}, and the defaults are used.")),
            Err(w) => {
                self.blocked.push(name);
                self.warnings.push(format!(
                    "{name} is damaged ({err}) and couldn't be set aside ({w}). the app uses the defaults and won't overwrite the file."
                ));
            }
        }
        None
    }
}

/// The activity-stack row of [`Session::keep_edited_previews_last`]'s work.
pub const EDITED_PREVIEWS_LABEL: &str = "Finding edited photos";

/// Most photos [`Session::keep_edited_previews_last`] looks through as the library opens (some
/// 3 µs each, so under 10 ms; issue #979); a larger library is looked through in the background.
pub const INLINE_EDITED_PHOTOS: usize = 2_000;

#[cfg(test)]
thread_local! {
    /// Photos [`edited_preview_keys`] looked at on this thread (tests: opening a library looks at
    /// none on the thread that opens it).
    pub(crate) static EDITED_LOOKED_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The latest-preview keys of the edited photos among `photos` ([`Session::keep_edited_previews_last`]):
/// `None` when `stop` said so. `stop` is asked, and `progress` told (done, of), every few thousand photos.
fn edited_preview_keys(
    photos: &[std::sync::Arc<lcc_catalog::Photo>],
    stop: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Option<Vec<lcc_preview::Hash128>> {
    const STEP: usize = 4096;
    let mut keys = Vec::new();
    // (a camera is looked up by name once, not once per photo: most of the time this took)
    let mut relative: std::collections::HashMap<(bool, &str, &str, &str), bool> = std::collections::HashMap::new();
    for (i, p) in photos.iter().enumerate() {
        if i % STEP == 0 {
            if stop() {
                return None;
            }
            progress(i, photos.len());
        }
        #[cfg(test)]
        EDITED_LOOKED_AT.with(|n| n.set(n.get().saturating_add(1)));
        let rel = *relative.entry(p.relative_wb_key()).or_insert_with(|| p.relative_wb());
        if !crate::import::looks_as_shot_given(p, p.camera_defaults_as(rel)) {
            keys.extend([crate::media::latest_view_key(p, true), crate::media::latest_thumb_key(p)]);
        }
    }
    progress(photos.len(), photos.len());
    Some(keys)
}

/// How long each stage of opening a library took: logged when the whole took a while (issue
/// #979: a large catalog spent a minute between being read and the library opening, with no
/// timings for that part), and always with `<PREFIX>_PROFILE` set.
struct OpenStages {
    at: web_time::Instant,
    start: web_time::Instant,
    took: Vec<(&'static str, f64)>,
}

impl OpenStages {
    /// Opening that took longer than this logs its stages.
    const SLOW_MS: f64 = 2000.0;

    fn start() -> Self {
        let now = web_time::Instant::now();
        OpenStages { at: now, start: now, took: Vec::new() }
    }

    fn mark(&mut self, stage: &'static str) {
        let now = web_time::Instant::now();
        self.took.push((stage, now.duration_since(self.at).as_secs_f64() * 1000.0));
        self.at = now;
    }

    fn report(&self) {
        let total = self.start.elapsed().as_secs_f64() * 1000.0;
        if total < Self::SLOW_MS && !lcc_pipeline::profiling() {
            return;
        }
        let parts: Vec<String> = self.took.iter().map(|(s, ms)| format!("{s} {ms:.1}")).collect();
        log::info!("library: opened in {total:.1} ms ({})", parts.join(", "));
    }
}

impl Session {
    /// Take over what the thumbnail cache wrote under its name of before (`.jpg` files, which
    /// photo galleries took for the person's photos: issues #610, #1066), once for a cache
    /// folder that was not made or tidied by this build yet. On a thread of its own, with a row
    /// in the activity stack that can stop it: a large cache is some 100 000 files to rename.
    fn tidy_thumbnail_cache(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            let cache = self.media.rendered.clone();
            if !cache.disk().is_some_and(|d| d.needs_tidying()) {
                return;
            }
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let task = self.activity.start("cache", "Tidying the thumbnail cache", crate::activity::Cancel::Flag(stop.clone()));
            let spawned = std::thread::Builder::new().name("lc-thumbs-tidy".into()).spawn(move || {
                // the row stays while this runs (it is dropped with the closure, however that ends)
                let _row = task;
                if let Some(disk) = cache.disk() {
                    // (also given up when the library is closed or another is opened: the cache
                    // is retired then, and it is gone on with when this library opens again)
                    let stopped = || stop.load(Ordering::Relaxed) || cache.generation() == u64::MAX;
                    let taken = disk.take_over_old(&stopped, &|done, of| _row.progress(done as u64, of as u64));
                    log::info!("thumbnail cache: {taken} thumbnails taken over under their new name in {}", disk.dir().display());
                }
            });
            // (not started: the thumbnails are still found one by one, and it is tried at the next open)
            if let Err(e) = spawned {
                log::warn!("thumbnail cache: could not be tidied: {e}");
            }
        }
    }

    /// Check, without closing the library open now, that [`Session::open_library`] can be expected
    /// to open `dir`: not a Lightroom catalog, not some other file, a readable folder (or one not
    /// made yet), not locked by another program. Returns the library folder ([`resolve_dir`]).
    /// Nothing is created; a library folder that exists gets its lock taken and released again.
    /// Opening can still fail afterwards (a damaged catalog, a disk going away).
    pub fn check_library(&self, dir: impl AsRef<Path>) -> Result<PathBuf> {
        let dir = dir.as_ref().to_path_buf();
        if let Some(place) = crate::lightroom_folder::lightroom_catalog_at(&dir) {
            return Err(EngineError::LightroomCatalog { path: dir, catalog: place.catalog });
        }
        let dir = resolve_dir(&dir)?;
        if !dir.exists() {
            return Ok(dir);
        }
        if !dir.is_dir() {
            return Err(EngineError::Other(format!("{} is not a folder", dir.display())));
        }
        std::fs::read_dir(&dir).map_err(|e| EngineError::Other(format!("can't open catalog {}: {e}", dir.display())))?;
        // the library open in this session keeps its lock when reopened; any other must be free
        let current = self.library.as_ref().is_some_and(|l| l.on_disk && same_dir(&l.dir, &dir));
        if !current && is_library(&dir) {
            drop(LibraryLock::acquire(&dir, &program_name()).map_err(|e| EngineError::LibraryInUse(e.to_string()))?);
        }
        Ok(dir)
    }

    /// Open (or create) the library at `dir` into this session, replacing its catalog. With
    /// `seed_demo`, a newly created library starts with the procedural demo photos.
    ///
    /// The library is locked for this session ([`lcc_catalog::lock`]): if another process
    /// has it open, this fails with [`EngineError::LibraryInUse`] and nothing is read or changed.
    /// Reopening the library this session already has open keeps its lock.
    ///
    /// A Lightroom Classic catalog folder, one of its `.lrdata` / `.lrcat-data` folders or an
    /// `.lrcat` file is refused with [`EngineError::LightroomCatalog`] before anything is
    /// created ([`crate::lightroom_folder`]); a library of the app always opens, whatever else
    /// its folder holds.
    pub fn open_library(&mut self, dir: impl AsRef<Path>, seed_demo: bool) -> Result<&LoadReport> {
        let dir = dir.as_ref().to_path_buf();
        if let Some(place) = crate::lightroom_folder::lightroom_catalog_at(&dir) {
            return Err(EngineError::LightroomCatalog { path: dir, catalog: place.catalog });
        }
        let dir = resolve_dir(&dir)?;
        let open = || FsStore::open(&dir).map_err(|e| EngineError::Other(format!("can't open catalog {}: {e}", dir.display())));
        let stores = LibraryStores { catalog: Box::new(open()?), files: Box::new(open()?), on_disk: true, dir: dir.clone() };
        // the library open in this session (same directory): hand its lock over
        let reused = self.library.as_mut().filter(|l| same_dir(&l.dir, &dir)).and_then(|l| l.lock.take());
        let reusing = reused.is_some();
        let mut lock = match reused {
            Some(l) => Some(l),
            None => Some(LibraryLock::acquire(&dir, &program_name()).map_err(|e| EngineError::LibraryInUse(e.to_string()))?),
        };
        if let Err(e) = self.open_stores(stores, seed_demo, &mut lock) {
            if reusing && let Some(lib) = self.library.as_mut() {
                lib.lock = lock.take();
            }
            return Err(e);
        }
        self.loaded_report()
    }

    /// Open (or create) a library whose files live in `stores` (e.g. browser storage), replacing
    /// this session's catalog. Not locked: the host guards against a second opener (see
    /// [`Session::open_library`] for directories).
    pub fn open_library_in(&mut self, stores: LibraryStores, seed_demo: bool) -> Result<&LoadReport> {
        self.open_stores(stores, seed_demo, &mut None)?;
        self.loaded_report()
    }

    fn loaded_report(&self) -> Result<&LoadReport> {
        match &self.library {
            Some(lib) => Ok(&lib.report),
            None => Err(EngineError::Other("library closed while opening".into())),
        }
    }

    /// Open `stores`; the new library takes `lock` once nothing can fail any more.
    fn open_stores(&mut self, stores: LibraryStores, seed_demo: bool, lock: &mut Option<LibraryLock>) -> Result<()> {
        // the old library's queued sidecar writes are made against its catalog; one still stuck (a share that stopped
        // answering) keeps this library open, with its retries, rather than lose or misplace its result. First, before
        // anything of the session changes.
        self.settle_sidecar_writes()?;
        let LibraryStores { dir, catalog, mut files, on_disk } = stores;
        self.media.smart_dir = on_disk.then(|| crate::smart::dir(&dir));
        // a damaged catalog-settings.json is set aside here, before anything else reads it, so
        // the user is told (the defaults of a new library, backups on, stand in)
        #[cfg(not(target_arch = "wasm32"))]
        let catalog_settings_problem = if on_disk { lcc_catalog::library::CatalogSettings::load_checked(&dir).1 } else { None };
        #[cfg(target_arch = "wasm32")]
        let catalog_settings_problem: Option<String> = None;
        // the current library may be these same files: let its background snapshot land first
        if let Some(old) = self.library.as_mut()
            && let Err(e) = old.journal.wait()
        {
            log::error!("library: {e}");
        }
        // Photo ids belong to one library; a folder's metadata job and denoise work must not carry into the next.
        self.folder_metadata = None;
        self.denoise.library_changed();
        self.media.denoise.clear();
        // what was learned about the old library's faces and ML embeddings is saved, and none of it carries over (photo ids are per library)
        #[cfg(not(target_arch = "wasm32"))]
        self.faces.library_changed();
        self.ml.library_changed();
        let mut stages = OpenStages::start();
        let (mut journal, mut catalog, report) = Journal::open(catalog)?;
        stages.mark("catalog");
        // A loaded catalog counts revisions from 0, like every other one. Caches (sidebar counts,
        // keyword tree, the grid's list…) are keyed on the revision, so give each library loaded
        // in this process its own range: otherwise an empty library opened after one that was
        // never edited this session would look unchanged and show the old library's numbers.
        static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        catalog.revision = LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed).wrapping_add(1) << 32;
        // (a sidecar waiting for a retry is a photo of the old library)
        self.unsaved_sidecars.clear();
        self.catalog = catalog;
        // (nothing of the list the last library showed is this one's)
        self.refresh_view();
        self.library_generation = crate::LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            self.pending_sidecar_repairs.append(&mut entry.retry_sidecars);
        }
        self.undo.clear();
        // a run of the old library no longer holds the new one's history
        self.undo_hold = std::sync::Weak::new();
        self.redo.clear();
        self.interaction = None;
        self.pending_log.clear();
        self.selection = Selection::default();
        self.previous_export.clear();
        self.source = LibrarySource::All;
        self.library_folder = None;
        // what names albums and photos by number means others in another library: no filter
        // (as after a restart; the view file keeps none), no target album, no photo come from
        self.filter = Default::default();
        self.shelved_filter = None;
        self.target_album = None;
        self.previous_active = None;
        self.active_mask = None;
        self.active_spot = None;
        if report.created && seed_demo {
            crate::demo::load(self);
            journal.snapshot(&self.catalog)?;
        }
        // G19: the plug-in metadata field index (in memory only) from the plug-ins' own data, this
        // library's (plug-in data is kept per library: photo ids are per library)
        #[cfg(not(target_arch = "wasm32"))]
        {
            let id = if on_disk { library_id(&dir) } else { String::new() };
            self.plugin_library = id;
            crate::cmd::plugins::sync_fields_with(self, false);
        }
        stages.mark("plug-in fields");
        let mut settings = SettingsLoad::default();
        settings.warnings.extend(catalog_settings_problem);
        // preferences
        let prefs = settings.read::<PrefsFile>(files.as_mut(), "prefs.json").unwrap_or_default();
        self.xmp = prefs.xmp;
        self.last_export = prefs.last_export;
        self.last_merge = prefs.last_merge;
        self.export_presets = prefs.export_presets;
        self.print_templates = prefs.print_templates;
        self.print_template_folders = prefs.print_template_folders;
        crate::template_folders::tidy_folders(&mut self.print_template_folders, &mut self.print_templates);
        self.metadata_presets = prefs.metadata_presets;
        self.import_presets = prefs.import_presets;
        self.filter_presets = prefs.filter_presets;
        self.curve_presets = prefs.curve_presets;
        self.crop_aspects = prefs.crop_aspects.sanitized();
        self.mask_effects = prefs.mask_effects;
        self.mask_tool_effects = prefs.mask_tool_effects;
        self.label_sets = prefs.label_sets;
        self.naming_presets =
            prefs.naming_presets.into_iter().filter(|p| !p.name.trim().is_empty()).take(crate::cmd::manage::MAX_NAMING_PRESETS).collect();
        self.lut_profiles = prefs.lut_profiles;
        crate::cmd::lut_profiles::register_all(self, on_disk.then(|| dir.join("Profiles")).as_deref());
        self.keyword_sets = prefs.keyword_sets;
        self.keyword_set = prefs.keyword_set;
        self.recent_keywords = prefs.recent_keywords;
        self.keyword_shortcut =
            if prefs.keyword_shortcuts.is_empty() { prefs.keyword_shortcut.into_iter().collect() } else { prefs.keyword_shortcuts };
        self.import_defaults = prefs.import;
        self.camera_profile_defaults = prefs.camera_profile_defaults;
        // photos with no camera-profile choice of their own start from these (issue #723)
        lcc_raw::dcp::install_user_defaults(self.camera_profile_defaults.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        self.cache_mb = prefs.cache_mb;
        self.prep.opened_library();
        self.prep.library = prefs.prep.map(crate::prep::PrepSettings::sanitized);
        self.prep.unfinished = prefs.prep_unfinished;
        self.preview_prefs = prefs.previews;
        #[cfg(not(target_arch = "wasm32"))]
        if on_disk {
            // the catalog's own settings hold them (catalog-settings.json); prefs.json only for
            // libraries written before they moved, migrated on the next save
            let cs = lcc_catalog::library::CatalogSettings::load(&dir);
            if let Some(v) = cs.previews.and_then(|v| serde_json::from_value(v).ok()) {
                self.preview_prefs = v;
            }
        }
        self.slideshow = prefs.slideshow.sanitized();
        self.slide_layout = prefs.slide_layout.sanitized();
        self.slide_overlays = prefs.slide_overlays.sanitized();
        self.slide_titles = prefs.slide_titles.sanitized();
        self.forget_local_days = prefs.forget_local_days.unwrap_or(lcc_catalog::DEFAULT_FORGET_DAYS);
        self.hold_view = prefs.hold_view;
        self.dialogs = prefs.dialogs.clone();
        self.folder_watch = prefs.folder_watch.sanitized();
        // (a damaged file only costs a fresh start: the next poll records the listings again)
        let watch: crate::folder_watch::WatchState = files
            .read(FOLDER_STAMPS)
            .ok()
            .flatten()
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| log::warn!("library: {FOLDER_STAMPS}: {e}")).ok())
            .unwrap_or_default();
        self.folder_stamps = watch.stamps;
        self.folder_new = watch.new;
        self.refresh_view();
        self.smart_previews_dir = prefs.smart_previews_dir.filter(|_| on_disk).map(PathBuf::from);
        if let Some(d) = &self.smart_previews_dir {
            self.media.smart_dir = Some(d.clone());
        }
        // presets
        if let Some(f) = settings.read::<PresetsFile>(files.as_mut(), "presets.json") {
            self.preset_folders = f.folders;
            self.preset_at_import = f.at_import;
            self.preset_hidden = f.hidden;
            self.preset_builtin_names = f.builtin_names;
            self.preset_builtin_groups = f.builtin_groups;
            for p in &mut self.presets {
                p.favorite = p.builtin && f.favorites.contains(&p.id) || (!p.builtin && p.favorite);
            }
            let mut ids: std::collections::HashSet<String> = self.presets.iter().map(|p| p.id.clone()).collect();
            for u in f.user {
                if ids.insert(u.id.clone()) {
                    self.presets.push(u);
                }
            }
            self.apply_builtin_edits();
            let known = |id: &String| crate::presets::profile(id).is_some() || self.lut_profiles.iter().any(|p| &p.id == id);
            self.profile_favorites = f.profile_favorites.into_iter().filter(known).collect();
            self.profile_recent = f.profile_recent.into_iter().filter(known).take(crate::presets::RECENT_PROFILES).collect();
            // (a hidden id this build doesn't know is kept: a LUT folder may come back)
            self.profile_hidden = f.profile_hidden.into_iter().take(10_000).collect();
            self.profile_edits = f.profile_edits.bounded();
            self.body_profiles = f.body_profiles.into_iter().take(10_000).collect();
        }
        stages.mark("settings + presets");
        // view state
        if let Some(v) = settings.read::<ViewFile>(files.as_mut(), "view.json") {
            self.source = v.source;
            self.browse = v.browse;
            self.library_folder = v.library_folder.filter(|f| !f.trim().is_empty());
            self.folder_subfolders = !v.folder_direct;
            self.lock_filters = v.lock_filters.unwrap_or(true);
            self.remember_filters = v.remember_filters;
            if self.source == LibrarySource::LibraryFolder {
                // a folder that is gone (or none): everything, not an empty grid
                let f = lcc_catalog::Filter { library_folder: self.library_folder.clone(), ..Default::default() };
                let any = self.library_folder.is_some() && !self.catalog.query(&f, &lcc_catalog::Sort::default()).is_empty();
                if !any {
                    self.source = LibrarySource::All;
                    self.library_folder = None;
                }
            }
            self.sort = v.sort;
            self.previous_export = v.previous_export.into_iter().filter(|id| self.catalog.photo(*id).is_some()).collect();
            self.selection = v.selection;
            // each photo once: the file is plain JSON, and a selection never repeats one
            let mut seen = std::collections::HashSet::new();
            self.selection.ids.retain(|id| self.catalog.photo(*id).is_some() && seen.insert(*id));
            self.selection.active = self.selection.active.filter(|id| self.catalog.photo(*id).is_some());
        }
        // the saved view may name an album the library no longer has
        self.reconcile_view();
        // not on Missing Photos: working that view out checks every original (minutes on a
        // sleeping NAS), and the app checks them in the background once it runs
        if self.selection.active.is_none()
            && self.source != LibrarySource::Missing
            && let Some(first) = self.visible_cloned().first()
        {
            self.selection = Selection::single(*first);
        }
        stages.mark("view");
        // presets.json is as read: saved again only after the list changes (hash 0 = unknown bytes)
        let presets_saved = PresetsSaved::state_of(self);
        // photo ids are per library: drop decoded sources of the previous one
        self.media.clear_sources();
        if on_disk {
            self.media.attach_disk_cache(&dir.join("thumbs"), self.cache_bytes_in(&dir.join("thumbs")));
            stages.mark("thumbnail cache");
            self.keep_edited_previews_last();
            stages.mark("edited previews");
            self.tidy_thumbnail_cache();
        }
        stages.mark("thumbnail cache tidy");
        let view_written = self.view_json();
        let store_with_library = prefs.store_with_library;
        let own_store = store_with_library || self.app_store.is_none();
        let frozen = if own_store { serde_json::Map::new() } else { read_frozen(files.as_mut()) };
        let upgraded = upgrade_notice(&report);
        self.library = Some(Library {
            dir,
            on_disk,
            journal,
            compaction_row: None,
            files,
            report,
            last_error: None,
            unsaved_error: None,
            presets_error: None,
            retry_at: None,
            forgot_local: None,
            presets_saved,
            view_written,
            settings_warnings: unlocked_warning(lock.as_ref()).into_iter().chain(upgraded).chain(settings.warnings).collect(),
            warnings_reported: 0,
            blocked: settings.blocked,
            lock: lock.take(),
            own_store,
            store_with_library,
            frozen,
        });
        if !own_store {
            // the store's presets, profiles and habits (this library's joined it the first time)
            self.adopt_app_store(&self.library.as_ref().map(|l| l.dir.clone()).unwrap_or_default(), on_disk);
        }
        stages.mark("app store");
        // forget untouched Local records of folders not browsed for a while (journaled at once)
        if self.forget_local_days > 0 {
            let plan = self.forget_local(false, None);
            if !plan.evict.is_empty() {
                self.compact_soon();
            } else if let Err(e) = self.persist() {
                log::error!("library: {e}");
            }
            if let Some(lib) = self.library.as_mut() {
                lib.forgot_local = Some(plan);
            }
        }
        stages.mark("forget local");
        // keywords read whole from a list a line long (before they were split) become one each
        if self.split_multiline_keywords() > 0
            && let Err(e) = self.persist()
        {
            log::error!("library: {e}");
        }
        stages.mark("keywords");
        // smart previews written by an older camera-look fit are brought up to date in the background
        #[cfg(not(target_arch = "wasm32"))]
        if on_disk {
            crate::cmd::previews::refresh_stale_smart_previews(self);
        }
        stages.mark("smart previews");
        // an import still being finished when the app closed picks up where it stopped
        self.load_finishing();
        stages.mark("unfinished import");
        self.library_identity = std::sync::Arc::new(());
        stages.report();
        Ok(())
    }

    /// Photos whose keywords hold line breaks (a camera preset's whole keyword list read as one
    /// keyword by an earlier version) get one keyword a line, journaled, not an undo step. Returns
    /// how many photos changed.
    pub fn split_multiline_keywords(&mut self) -> usize {
        let fix: Vec<(lcc_catalog::PhotoId, lcc_catalog::Meta)> = self
            .catalog
            .photos()
            .filter(|p| p.meta.keywords.iter().any(|k| k.contains(['\r', '\n', '\u{2028}', '\u{2029}'])))
            .map(|p| {
                let mut meta = p.meta.clone();
                meta.keywords = lcc_catalog::keywords::from_file(&meta.keywords, &[]);
                (p.id, meta)
            })
            .collect();
        let n = fix.len();
        for (id, meta) in fix {
            let op = lcc_catalog::Op::SetMeta { id, meta: Box::new(meta) };
            match self.catalog.apply(op.clone()) {
                Ok(_) => self.pending_log.push(op),
                Err(e) => log::warn!("split keywords: {e}"),
            }
        }
        if n > 0 {
            log::info!("split the line-separated keywords of {n} photo(s) into one keyword a line");
        }
        n
    }

    /// Write pending ops to the log (fsynced), compact when due, and save changed presets.
    /// Called after every top-level command; cheap when nothing changed. Fails (with
    /// [`EngineError::NotSaved`]) only when the queued ops couldn't be written; they stay queued.
    pub fn persist(&mut self) -> Result<()> {
        let Some(lib) = self.library.as_mut() else { return Ok(()) };
        if !self.pending_log.is_empty() {
            let mut appended = lib.journal.append(&self.pending_log);
            if appended.is_err() && lib.journal.lost() > 0 && self.interaction.is_none() {
                // a log write failed after earlier changes were handed over (natively the log is
                // written by a thread of its own): a snapshot now saves them and the queued ones
                let queued = self.pending_log.len() as u64;
                if lib.journal.snapshot_with_unlogged(&self.catalog, queued).is_ok() && lib.journal.lost() == 0 {
                    log::warn!("library: a catalog log write failed; a snapshot saved every change");
                    appended = Ok(());
                }
            }
            if let Err(e) = appended {
                // the ops stay queued (and applied in memory): the next persist retries them
                let reason = e.to_string();
                log::error!("library: {} change(s) not written to disk: {reason}", self.pending_log.len());
                lib.last_error = Some(reason.clone());
                lib.unsaved_error = Some(reason.clone());
                lib.retry_at = Some(web_time::Instant::now() + RETRY_BACKOFF);
                return Err(EngineError::NotSaved(reason));
            }
            if lib.unsaved_error.take().is_some() {
                log::info!("library: queued changes written to disk");
            }
            lib.retry_at = None;
            self.pending_log.clear();
            lib.last_error = None;
        }
        if self.pending_log.is_empty()
            && let Err(e) = lib.journal.check_log()
        {
            // a log write on its thread failed: the snapshot below saves what it lost
            lib.last_error = Some(e.to_string());
            lib.unsaved_error = Some(e.to_string());
        }
        poll_compaction(lib);
        // Never snapshot mid-interaction: the catalog then holds an uncommitted preview value.
        // The snapshot is written by a worker thread (natively); appends go on meanwhile.
        // A failed compaction loses nothing (the log is kept whole and compacted later): it is
        // reported (`library.info` → `lastError`), not returned.
        if lib.journal.wants_snapshot()
            && self.interaction.is_none()
            && let Err(e) = lib.journal.snapshot_in_background(&self.catalog)
        {
            log::error!("library: compaction: {e}");
            lib.last_error = Some(format!("compaction: {e}"));
        }
        if self.pending_log.is_empty() && lib.journal.lost() == 0 && lib.unsaved_error.is_some() && lib.retry_at.is_none() {
            // what a failed log write lost is in the snapshot now
            lib.unsaved_error = None;
        }
        self.sync_compaction_row();
        if self.uses_app_store() {
            // the presets live in the app store: the library's presets.json stays as it was
            if let Err(e) = self.save_app_store() {
                log::error!("store: {e}");
                if let Some(lib) = self.library.as_mut() {
                    lib.last_error = Some(e.to_string());
                }
            }
            return Ok(());
        }
        // a background save that failed: the presets count as unsaved and are saved again now
        if let Some(e) = self.presets_save_failed()
            && let Some(lib) = self.library.as_mut()
        {
            log::error!("library: presets: {e}");
            lib.presets_error = Some(format!("presets: {e}"));
            lib.presets_saved = PresetsSaved::default();
        }
        let due = self.library.as_ref().is_some_and(|l| l.presets_saved.differs(self) && !l.blocked.contains(&"presets.json"));
        if !due {
            return Ok(());
        }
        // off the UI thread when it can be (`presets_save`): the bytes are made and compared there
        if let Some(lib) = self.library.take() {
            let known = lib.presets_saved.hash;
            let handed = self.save_presets_in_background(lib.files.as_ref(), known);
            self.library = Some(lib);
            if handed {
                let mut state = PresetsSaved::state_of(self);
                state.hash = known;
                if let Some(lib) = self.library.as_mut() {
                    lib.presets_saved = state;
                    // (a failure of this save sets it again on the next persist)
                    lib.presets_error = None;
                }
                return Ok(());
            }
        }
        let presets = presets_json(self);
        let mut state = PresetsSaved::state_of(self);
        state.hash = bytes_hash(presets.as_bytes());
        let Some(lib) = self.library.as_mut() else { return Ok(()) };
        if state.hash == lib.presets_saved.hash {
            lib.presets_saved = state;
            lib.presets_error = None;
        } else if let Err(e) = lib.files.write_atomic("presets.json", presets.as_bytes()) {
            // the state stays stale: retried by the next command and the frame loop (backing off);
            // meanwhile the presets count as unsaved, so the UI warns and a backup refuses
            log::error!("library: presets: {e}");
            lib.presets_error = Some(format!("presets: {e}"));
            lib.retry_at = Some(web_time::Instant::now() + RETRY_BACKOFF);
        } else {
            if lib.presets_error.take().is_some() {
                log::info!("library: presets written to disk");
            }
            lib.presets_saved = state;
        }
        Ok(())
    }

    /// Persist if commands left ops pending (cheap; frontends call it once per frame for state
    /// changed outside [`Session::execute`]).
    pub fn persist_if_dirty(&mut self) {
        let backing_off = self.library.as_ref().and_then(|l| l.retry_at).is_some_and(|t| web_time::Instant::now() < t);
        let presets_unsaved = self.library.as_ref().is_some_and(|l| l.presets_error.is_some());
        if self.library.is_some() && (!self.pending_log.is_empty() || presets_unsaved) && !backing_off {
            let _ = self.persist();
        } else if let Some(lib) = self.library.as_mut() {
            poll_compaction(lib);
        }
        self.sync_compaction_row();
    }

    /// Show a background compaction in the activity stack while it runs, and drop the row when
    /// it has landed (or failed). The snapshot itself is written on a worker thread.
    fn sync_compaction_row(&mut self) {
        let Some(lib) = self.library.as_mut() else { return };
        if !lib.journal.snapshot_running() {
            lib.compaction_row = None;
        } else if lib.compaction_row.is_none() {
            lib.compaction_row = Some(self.activity.start("compaction", "Optimizing the catalog", crate::activity::Cancel::No));
        }
    }

    /// Changes applied in memory but not yet written to disk because saving failed: the number of
    /// queued ops (plus one for presets.json) and why (`None` when everything is saved, or no library
    /// is open). Retried by every command and by [`Session::persist_if_dirty`].
    pub fn unsaved(&self) -> Option<(usize, &str)> {
        let lib = self.library.as_ref()?;
        let ops = lib.unsaved_error.as_deref().filter(|_| !self.pending_log.is_empty());
        let presets = lib.presets_error.as_deref();
        let n = ops.map_or(0, |_| self.pending_log.len()) + usize::from(presets.is_some());
        ops.or(presets).map(|why| (n, why))
    }

    /// Flush everything and write a snapshot (on quit). Ends an open interaction first.
    ///
    /// The snapshot is written even when appending the queued ops fails: it is a fresh file
    /// holding everything in memory, so it saves those ops too (the log handle may be the only
    /// thing that's broken). Fails only if nothing could be saved; the ops then stay queued.
    pub fn close_library(&mut self) -> Result<()> {
        if self.library.is_none() {
            return Ok(());
        }
        let _ = self.end_interaction();
        // the automatic sidecar writes and ML embeddings still queued are saved before the library goes
        self.flush_sidecar_writes();
        self.ml.save_store_now();
        // the device-sync replica of this catalogue is written now, not on the next tick
        #[cfg(not(target_arch = "wasm32"))]
        crate::device_sync::flush(self);
        let persisted = self.persist();
        // a presets.json save on its way finishes first (it is this library's), and a failed one
        // is made here, in the close
        self.flush_presets();
        if self.presets_saver.is_some() {
            let saves = std::mem::replace(&mut self.background_saves, false);
            let _ = self.persist();
            self.background_saves = saves;
        }
        self.save_view();
        let unlogged = if persisted.is_err() { self.pending_log.len() as u64 } else { 0 };
        let Some(lib) = self.library.as_mut() else { return Ok(()) };
        // (ops a failed log write lost count with the queued ones: the snapshot saves both)
        let before = lib.journal.seq().saturating_add(lib.journal.lost());
        let snapshot = lib.journal.snapshot_with_unlogged(&self.catalog, unlogged);
        if snapshot.is_err() {
            // at least everything handed to the log's thread is written and synced
            let _ = lib.journal.flush();
        }
        if unlogged > 0 && lib.journal.seq() == before + unlogged {
            // the snapshot holds the queued ops: they are saved, never append them again
            log::info!("library: {unlogged} queued change(s) saved in the closing snapshot");
            self.pending_log.clear();
            lib.unsaved_error = None;
            lib.retry_at = None;
            lib.last_error = None;
        }
        let result = match (persisted, snapshot) {
            (_, Ok(())) => Ok(()),
            // neither the log nor the snapshot took the queued ops
            (Err(EngineError::NotSaved(e)), Err(s)) if lib.journal.seq().saturating_add(lib.journal.lost()) == before => {
                Err(EngineError::NotSaved(format!("{e}; snapshot: {s}")))
            }
            (_, Err(s)) => Err(s.into()),
        };
        if result.is_ok() {
            self.library_identity = std::sync::Arc::new(());
        }
        result
    }

    /// Wait until every change handed to the catalog log is durable (natively the log is written
    /// by a thread of its own). An error: a write failed and the changes it lost are saved by
    /// the next save (a snapshot). A no-op without a library.
    pub fn flush_catalog(&mut self) -> Result<()> {
        let Some(lib) = self.library.as_mut() else { return Ok(()) };
        lib.journal.flush().map_err(|e| {
            let reason = e.to_string();
            lib.last_error = Some(reason.clone());
            lib.unsaved_error = Some(reason.clone());
            EngineError::NotSaved(reason)
        })
    }

    /// Let go of the open library's lock (and with it the owner note, `catalog.lock.owner`) while
    /// the library stays open in this session. For a host that ends the process without running
    /// destructors (`std::process::exit`), after [`Session::close_library`] has saved: the OS would
    /// free the lock anyway, but only the lock's `Drop` removes the note. Nothing else is
    /// protected after this, so call it only when the process is about to end. A no-op without a
    /// library or a lock.
    pub fn release_library_lock(&mut self) {
        drop(self.library.as_mut().and_then(|l| l.lock.take()));
    }

    fn view_json(&self) -> Vec<u8> {
        let view = ViewFile {
            source: self.source,
            browse: self.browse.clone(),
            // only while it is shown: a leftover would be a stale choice nobody made
            library_folder: self.library_folder.clone().filter(|_| self.source == LibrarySource::LibraryFolder),
            folder_direct: !self.folder_subfolders,
            lock_filters: (!self.lock_filters).then_some(false),
            remember_filters: self.remember_filters,
            sort: self.sort,
            selection: self.selection.clone(),
            previous_export: self.previous_export.clone(),
        };
        serde_json::to_vec_pretty(&view).unwrap_or_default()
    }

    /// Save the view state (source, sort, selection) if it changed since it was last
    /// written. Native hosts get this from [`Session::close_library`]; the browser host calls it
    /// periodically, since a tab can be closed without notice.
    pub fn save_view(&mut self) {
        if self.library.is_none() {
            return;
        }
        let v = self.view_json();
        let Some(lib) = self.library.as_mut() else { return };
        if v != lib.view_written && !lib.blocked.contains(&"view.json") {
            match lib.files.write_atomic("view.json", &v) {
                Ok(()) => lib.view_written = v,
                Err(e) => log::error!("library: view: {e}"),
            }
        }
    }

    /// Save the folder stamps of the last poll ([`crate::folder_watch`]; no-op for in-memory
    /// sessions).
    pub fn save_folder_stamps(&mut self) -> Result<()> {
        let v = serde_json::to_vec(&crate::folder_watch::WatchStateRef { stamps: &self.folder_stamps, new: &self.folder_new }).unwrap_or_default();
        let Some(lib) = self.library.as_mut() else { return Ok(()) };
        lib.files.write_atomic(FOLDER_STAMPS, &v).map_err(|e| EngineError::Other(format!("{FOLDER_STAMPS}: {e}")))
    }

    /// Save the library preferences (no-op for in-memory sessions).
    pub fn save_prefs(&mut self) -> Result<()> {
        let store = self.save_app_store();
        let v = serde_json::to_value(PrefsFile {
            xmp: self.xmp,
            last_export: self.last_export.clone(),
            last_merge: self.last_merge.clone(),
            export_presets: self.export_presets.clone(),
            print_templates: self.print_templates.clone(),
            print_template_folders: self.print_template_folders.clone(),
            metadata_presets: self.metadata_presets.clone(),
            import_presets: self.import_presets.clone(),
            filter_presets: self.filter_presets.clone(),
            curve_presets: self.curve_presets.clone(),
            crop_aspects: self.crop_aspects.clone(),
            mask_effects: self.mask_effects.clone(),
            mask_tool_effects: self.mask_tool_effects.clone(),
            label_sets: self.label_sets.clone(),
            naming_presets: self.naming_presets.clone(),
            lut_profiles: self.lut_profiles.clone(),
            keyword_sets: self.keyword_sets.clone(),
            keyword_set: self.keyword_set.clone(),
            recent_keywords: self.recent_keywords.clone(),
            keyword_shortcut: None,
            keyword_shortcuts: self.keyword_shortcut.clone(),
            import: self.import_defaults.clone(),
            camera_profile_defaults: self.camera_profile_defaults.clone(),
            cache_mb: self.cache_mb,
            prep: self.prep.library,
            prep_unfinished: self.prep.unfinished,
            smart_previews_dir: self.smart_previews_dir.as_ref().map(|d| d.to_string_lossy().to_string()),
            forget_local_days: Some(self.forget_local_days),
            // on disk they live in the catalog's settings (below)
            previews: if self.library.as_ref().is_some_and(|l| l.on_disk) { Default::default() } else { self.preview_prefs },
            slideshow: self.slideshow.clone(),
            slide_layout: self.slide_layout.clone(),
            slide_overlays: self.slide_overlays.clone(),
            slide_titles: self.slide_titles.clone(),
            hold_view: self.hold_view,
            folder_watch: self.folder_watch,
            store_with_library: self.library.as_ref().is_some_and(|l| l.store_with_library),
            dialogs: self.dialogs.clone(),
        });
        // while the app store holds them, the library's own copies are written back as read
        let v = match (v, self.library.as_ref()) {
            (Ok(serde_json::Value::Object(mut map)), Some(lib)) if !lib.own_store => {
                for k in crate::app_store::SHARED_PREFS_KEYS {
                    match lib.frozen.get(k) {
                        Some(old) => map.insert(k.to_string(), old.clone()),
                        None => map.remove(k),
                    };
                }
                // auto import stays with the library
                let mut import = lib.frozen.get("import").cloned().unwrap_or_else(|| serde_json::json!({}));
                if let serde_json::Value::Object(im) = &mut import {
                    im.insert("autoFolder".into(), serde_json::to_value(&self.import_defaults.auto_folder).unwrap_or_default());
                    im.insert("autoCopy".into(), serde_json::Value::Bool(self.import_defaults.auto_copy));
                    im.insert("autoPaused".into(), serde_json::Value::Bool(self.import_defaults.auto_paused));
                    im.insert("autoOptions".into(), serde_json::to_value(&self.import_defaults.auto_options).unwrap_or_default());
                    im.insert("autoAlbum".into(), serde_json::to_value(&self.import_defaults.auto_album).unwrap_or_default());
                }
                map.insert("import".into(), import);
                serde_json::to_vec_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
            }
            (Ok(v), _) => serde_json::to_vec_pretty(&v).unwrap_or_default(),
            (Err(_), _) => Vec::new(),
        };
        let Some(lib) = self.library.as_mut() else { return store };
        if v.is_empty() {
            return Err(EngineError::Other("prefs: couldn't be serialized".into()));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if lib.on_disk {
            let mut cs = lcc_catalog::library::CatalogSettings::load(&lib.dir);
            let v = serde_json::to_value(self.preview_prefs).ok();
            if cs.previews != v {
                cs.previews = v;
                cs.save(&lib.dir).map_err(|e| EngineError::Other(format!("catalog settings: {e}")))?;
            }
        }
        if lib.blocked.contains(&"prefs.json") {
            return Err(EngineError::Other(
                "prefs: prefs.json couldn't be read when the library opened, so it isn't overwritten; reopen the library to save preferences".into(),
            ));
        }
        lib.files.write_atomic("prefs.json", &v).map_err(|e| EngineError::Other(format!("prefs: {e}")))?;
        store
    }

    /// Warnings about the open library not handed out yet: its settings files, and a lock that
    /// couldn't be taken (the UI shows each once; the CLI and MCP print them on stderr).
    pub fn take_library_warnings(&mut self) -> Vec<String> {
        let mut out = self.take_store_warnings();
        let Some(lib) = self.library.as_mut() else { return out };
        out.extend(lib.settings_warnings.get(lib.warnings_reported..).unwrap_or_default().iter().cloned());
        lib.warnings_reported = lib.settings_warnings.len();
        out
    }

    /// The edited photos' latest previews (their last loupe view and grid thumbnail) are the last
    /// pictures a full disk cache lets go: the only pictures of their edits (a photo as shot has its
    /// camera JPEG). A full cache otherwise lost every rendered edit, and the loupe had nothing but
    /// the unedited camera JPEG.
    ///
    /// Telling which photos are edited asks each photo's camera defaults, which look its camera up
    /// by name in the camera tables and profiles: most of a minute for 300 000 photos, which every
    /// opening of the library waited for (issue #979). It is worked out on a thread of its own with
    /// a row in the activity stack that can stop it; the cache prunes nothing until it is known.
    /// A library of up to [`INLINE_EDITED_PHOTOS`] photos is looked through at once (a moment).
    pub fn keep_edited_previews_last(&self) {
        let cache = self.media.rendered.clone();
        let Some(d) = cache.disk() else { return };
        // (shared, not copied: the photos are reference-counted)
        let photos: Vec<std::sync::Arc<lcc_catalog::Photo>> = self.catalog.photos().cloned().collect();
        if cfg!(target_arch = "wasm32") || photos.len() <= INLINE_EDITED_PHOTOS {
            d.keep_last(edited_preview_keys(&photos, &|| false, &|_, _| ()).unwrap_or_default());
            return;
        }
        d.keep_last_pending();
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let task = self.activity.start("cache", EDITED_PREVIEWS_LABEL, crate::activity::Cancel::Flag(stop.clone()));
            let worker = cache.clone();
            let spawned = std::thread::Builder::new().name("lc-keep-edited".into()).spawn(move || {
                // the row stays while this runs (it is dropped with the closure, however that ends)
                let row = task;
                // (also given up when the library is closed or another is opened: the cache is retired then)
                let stopped = || stop.load(Ordering::Relaxed) || worker.generation() == u64::MAX;
                let Some(d) = worker.disk() else { return };
                match edited_preview_keys(&photos, &stopped, &|done, of| row.progress(done as u64, of as u64)) {
                    Some(keys) => d.keep_last(keys),
                    None => d.keep_last_unknown(),
                }
            });
            // (not started: worked out here, as before)
            if let Err(e) = spawned {
                log::warn!("thumbnail cache: edited photos looked through on this thread: {e}");
                let photos: Vec<std::sync::Arc<lcc_catalog::Photo>> = self.catalog.photos().cloned().collect();
                d.keep_last(edited_preview_keys(&photos, &|| false, &|_, _| ()).unwrap_or_default());
            }
        }
    }

    /// The thumbnail disk cache budget in bytes ([`Session::cache_mb`], else the default, which
    /// grows with the library: [`crate::media::default_disk_cache_bytes`]).
    pub fn cache_bytes(&self) -> u64 {
        match (self.cache_mb, self.media.rendered.disk()) {
            (0, Some(d)) => self.cache_bytes_in(d.dir()),
            (0, None) => crate::media::default_disk_cache_bytes(self.catalog.len()),
            (mb, _) => u64::from(mb) << 20,
        }
    }

    /// [`Self::cache_bytes`] for a cache in `dir`: Automatic is a share of its drive's free space
    /// ([`crate::media::automatic_disk_cache_bytes`]; looked up at most once a minute).
    pub fn cache_bytes_in(&self, dir: &std::path::Path) -> u64 {
        if self.cache_mb != 0 {
            return u64::from(self.cache_mb) << 20;
        }
        let used = self.media.rendered.disk().filter(|d| d.dir() == dir).and_then(|d| d.known_size()).unwrap_or(0);
        crate::media::automatic_disk_cache_bytes(self.catalog.len(), self.cache_drive_free(dir), used)
    }

    /// What Automatic would make the preview cache now (Settings shows it), whatever is chosen.
    pub fn automatic_cache_bytes(&self) -> u64 {
        match self.media.rendered.disk() {
            Some(d) => crate::media::automatic_disk_cache_bytes(self.catalog.len(), self.cache_drive_free(d.dir()), d.known_size().unwrap_or(0)),
            None => crate::media::default_disk_cache_bytes(self.catalog.len()),
        }
    }

    /// The free space of the drive holding `dir` (`None`: unknown), looked up again after a minute.
    fn cache_drive_free(&self, dir: &std::path::Path) -> Option<u64> {
        let mut known = self.cache_drive_free.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, d, free)) = known.as_ref()
            && d == dir
            && at.elapsed() < std::time::Duration::from_secs(60)
        {
            return *free;
        }
        let free = lcc_sysmem::free_disk_space(dir);
        *known = Some((web_time::Instant::now(), dir.to_path_buf(), free));
        free
    }

    /// Bring the disk cache's budget up to date with the library (photos were added since it was
    /// set): cheap, nothing is read or written. Called before previews are built.
    pub fn fit_disk_cache(&mut self) {
        let want = self.cache_bytes();
        if let Some(d) = self.media.rendered.disk()
            && d.budget() != want
        {
            d.set_budget(want);
        }
    }

    /// Change the thumbnail disk cache budget (MB, 0 = default) in place: the cached thumbnails
    /// stay (the next write trims to the new size), and so do the textures shown from them.
    /// Saved with the library preferences.
    pub fn set_cache_mb(&mut self, mb: u32) -> Result<()> {
        self.cache_mb = mb;
        if let Some(lib) = self.library.as_ref().filter(|l| l.on_disk) {
            let dir = lib.thumbs_dir();
            self.media.set_disk_cache_bytes(&dir, self.cache_bytes());
        }
        self.save_prefs()
    }

    /// Save, then start compacting in the background (natively; synchronously elsewhere): after
    /// a change that shrinks the catalog a lot, so the smaller snapshot replaces the old one.
    /// Errors are logged and reported like other persistence errors (nothing is lost).
    pub fn compact_soon(&mut self) {
        if let Err(e) = self.persist() {
            log::error!("library: {e}");
            return;
        }
        if self.interaction.is_some() {
            return;
        }
        let Some(lib) = self.library.as_mut() else { return };
        if let Err(e) = lib.journal.snapshot_in_background(&self.catalog) {
            log::error!("library: compaction: {e}");
            lib.last_error = Some(format!("compaction: {e}"));
        }
        self.sync_compaction_row();
    }

    /// Compact the log into a snapshot now. Natively the snapshot is written on a worker thread
    /// (as the automatic compaction is): serialising a large catalog on the calling thread — the
    /// UI's — froze the window for tens of seconds on a 28 000-photo library (issue #448). The
    /// compaction finishes through the frame loop's [`Session::persist_if_dirty`]; a failure
    /// loses nothing (the log is kept whole) and is retried once the log has grown again.
    pub fn compact_library(&mut self) -> Result<()> {
        self.persist()?;
        if self.interaction.is_some() {
            return Err(EngineError::Other("can't compact during an interaction".into()));
        }
        if let Some(lib) = self.library.as_mut()
            && let Err(e) = lib.journal.snapshot_in_background(&self.catalog)
        {
            log::error!("library: compaction: {e}");
            lib.last_error = Some(format!("compaction: {e}"));
        }
        self.sync_compaction_row();
        Ok(())
    }
}

/// Catalog-level operations (native: a catalog is a folder with a v4 store).
#[cfg(not(target_arch = "wasm32"))]
impl Session {
    /// The open catalog's folder, when it is one on disk with a v4 store (backup, integrity
    /// test, optimise, catalog settings need it).
    pub fn catalog_dir(&self) -> Option<&Path> {
        self.library.as_ref().filter(|l| l.on_disk && l.journal.has_db()).map(|l| l.dir.as_path())
    }

    /// Run `f` on the open catalog's journal and catalog, after the queued ops are written (so a
    /// checkpoint holds them).
    fn with_catalog_journal<T>(&mut self, f: impl FnOnce(&mut Journal, &lcc_catalog::Catalog) -> lcc_catalog::Result<T>) -> Result<T> {
        if self.interaction.is_some() {
            return Err(EngineError::Other("finish the edit in progress first".into()));
        }
        self.persist()?;
        let lib = self.library.as_mut().filter(|l| l.on_disk && l.journal.has_db());
        let lib = lib.ok_or_else(|| EngineError::Other("no catalog on disk is open (an in-memory or browser library has no backups)".into()))?;
        Ok(f(&mut lib.journal, &self.catalog)?)
    }

    /// The open catalog's settings (`catalog-settings.json`); the defaults when none is on disk.
    pub fn catalog_settings(&self) -> lcc_catalog::library::CatalogSettings {
        self.catalog_dir().map(lcc_catalog::library::CatalogSettings::load).unwrap_or_default()
    }

    /// Save the open catalog's settings.
    pub fn set_catalog_settings(&mut self, settings: &lcc_catalog::library::CatalogSettings) -> Result<()> {
        let dir = self.catalog_dir().ok_or_else(|| EngineError::Other("no catalog on disk is open".into()))?.to_path_buf();
        settings.save(&dir)?;
        Ok(())
    }

    /// Back up the catalog into `root` (default: the catalog settings' backup folder). Returns
    /// the backup's folder.
    pub fn backup_catalog(&mut self, root: Option<&Path>) -> Result<PathBuf> {
        let settings = self.catalog_settings();
        let dir = self.catalog_dir().map(Path::to_path_buf).unwrap_or_default();
        let root = root.map(Path::to_path_buf).unwrap_or_else(|| settings.backup_root(&dir));
        self.with_catalog_journal(|j, c| j.backup(c, &root, settings.keep_backups))
    }

    /// Test the catalog's integrity (a checkpoint first).
    pub fn check_catalog_integrity(&mut self) -> Result<lcc_catalog::library::IntegrityReport> {
        self.with_catalog_journal(|j, c| j.check_integrity(c))
    }

    /// Optimise the catalog: its store rewritten, indexes rebuilt, compacted.
    pub fn optimize_catalog(&mut self) -> Result<lcc_catalog::library::OptimizeReport> {
        self.with_catalog_journal(|j, c| j.optimize(c))
    }

    /// Whether quitting should back the catalog up: its schedule says one is due and something
    /// changed since the last backup (ops waiting to be written count as a change). A library
    /// opened, looked through and closed again is not backed up, and its user isn't asked to.
    pub fn catalog_backup_wanted(&self) -> bool {
        if self.catalog_dir().is_none() {
            return false;
        }
        let settings = self.catalog_settings();
        let now = lcc_catalog::library::now_secs();
        if !self.pending_log.is_empty() || self.interaction.is_some() {
            return settings.backup_due(now);
        }
        let mark = self.library.as_ref().and_then(|l| l.journal.backup_mark().ok());
        settings.backup_wanted(now, mark.as_deref())
    }

    /// The exit-time backup: when the catalog's schedule says one is due (with the integrity
    /// test and optimise when set). `Ok(None)`: not due, or no catalog on disk.
    pub fn backup_catalog_if_due(&mut self) -> Result<Option<PathBuf>> {
        if self.catalog_dir().is_none() || !self.catalog_settings().backup_due(lcc_catalog::library::now_secs()) {
            return Ok(None);
        }
        let _ = self.end_interaction();
        self.with_catalog_journal(|j, c| j.backup_if_due(c))
    }
}

/// The warning for a library opened without its one-program-at-a-time lock (issue #171): its
/// file system can't lock `catalog.lock` (some network shares). It opens anyway, as documented —
/// refusing would lock the user out — but the user must learn that a second program could open it.
/// What an upgrade of the library's catalog did and how to go back (`None` when it wasn't
/// upgraded, or no copy was kept: a new or in-memory library).
pub const UPGRADE_NOTICE_START: &str = "This catalog was upgraded";

/// See [`UPGRADE_NOTICE_START`]: the text always begins with it, so the UI can title it as news, not a problem.
fn upgrade_notice(report: &LoadReport) -> Option<String> {
    let (from, kept) = (report.upgraded_from?, report.upgrade_backup.as_ref()?);
    Some(format!(
        "{UPGRADE_NOTICE_START} from format v{from} to v{}, so older versions of the app can't open the library \
         itself any more. A copy of it as it was is kept, unchanged, in {}: to go back to an older version, quit, copy the files \
         in that folder over the library's own (or open the folder in the older version). Changes made since the upgrade are \
         not in the copy.",
        lcc_catalog::journal::VERSION,
        kept.display()
    ))
}

fn unlocked_warning(lock: Option<&LibraryLock>) -> Option<String> {
    lock.filter(|l| !l.held()).map(|_| {
        "This library could not be locked (its catalog.lock file can't be locked where it is stored, e.g. on some network \
         shares), so it is open without protection against a second program: use it in one app or command at \
         a time, or changes made in one of them can be lost."
            .to_string()
    })
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    /// `--library <dir>/catalog.snap` failed with the OS's "File exists" (creating a folder over
    /// the file); it opens the library that file belongs to.
    #[test]
    fn a_librarys_own_file_opens_its_folder_and_any_other_file_is_a_clear_error() {
        let dir = std::env::temp_dir().join(format!("lc-resolve-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&dir, false).unwrap();
        s.close_library().unwrap();
        drop(s);
        assert!(is_library(&dir));
        let snap = dir.join(lcc_catalog::journal::SNAPSHOT);
        assert_eq!(resolve_dir(&snap).unwrap(), dir);
        let mut s = Session::new().with_fs();
        s.open_library(&snap, false).unwrap();
        assert_eq!(s.library.as_ref().map(|l| l.dir.clone()), Some(dir.clone()));
        drop(s);

        let other = dir.join("notes.txt");
        std::fs::write(&other, b"x").unwrap();
        let e = Session::new().with_fs().open_library(&other, false).err().unwrap().to_string();
        assert!(e.contains("is a file"), "{e}");
        assert!(!e.contains("File exists"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host that ends the process without destructors releases the lock by hand: the owner note
    /// goes and another opener is let in, while the session keeps its library.
    #[test]
    fn release_library_lock_removes_the_owner_note() {
        let dir = std::env::temp_dir().join(format!("lc-release-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&dir, false).unwrap();
        s.close_library().unwrap();
        assert!(dir.join(lcc_catalog::lock::OWNER).exists(), "close_library keeps the lock");
        assert!(matches!(LibraryLock::acquire(&dir, "other"), Err(lcc_catalog::lock::LockError::InUse(_))));
        s.release_library_lock();
        assert!(!dir.join(lcc_catalog::lock::OWNER).exists());
        drop(LibraryLock::acquire(&dir, "other").unwrap());
        s.release_library_lock();
        assert!(s.library.is_some(), "the library stays open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `check_library` refuses what `open_library` would, without touching the open library.
    #[test]
    fn check_library_refuses_locked_libraries_files_and_lightroom_catalogs() {
        let dir = std::env::temp_dir().join(format!("lc-check-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&b).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&b, false).unwrap();
        s.close_library().unwrap();
        s.open_library(&a, false).unwrap();
        assert_eq!(s.check_library(&a).unwrap(), a, "the open library keeps its lock");
        assert_eq!(s.check_library(&b).unwrap(), b);
        assert_eq!(s.check_library(dir.join("new")).unwrap(), dir.join("new"));
        assert!(!dir.join("new").exists(), "nothing created");
        let held = LibraryLock::acquire(&b, "other").unwrap();
        assert!(matches!(s.check_library(&b), Err(EngineError::LibraryInUse(_))));
        drop(held);
        assert!(s.check_library(&b).is_ok(), "the probe released its lock");
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(s.check_library(dir.join("notes.txt")).is_err());
        std::fs::write(dir.join("Cat.lrcat"), b"x").unwrap();
        assert!(matches!(s.check_library(dir.join("Cat.lrcat")), Err(EngineError::LightroomCatalog { .. })));
        assert!(s.library.as_ref().is_some_and(|l| l.dir == a));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
