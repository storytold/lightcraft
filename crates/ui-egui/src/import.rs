//! The import review dialog (File → Import Photos… / Import from Folder… / Import from Device):
//! the source it scanned (a scanned folder is *not* added to Local), the files found under it as
//! a grid of thumbnails with checkboxes (duplicates marked and unchecked; Shift-click sets a
//! range), the destination (add in place / copy or move into the library's `Originals/` or a
//! chosen folder, filed by day, by month, into one folder or by a custom folder template,
//! optionally renamed), an album (existing or new), a preset, an optional batch-edit recipe
//! ("Batch edits": develop controls set on every new photo) and keywords to apply. Importing
//! runs on a worker thread (files are probed, copied or moved there) and its batches join the
//! catalog between frames, with a row and ✕ in the activity stack (issue #345); the whole import is
//! one undo step.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use lcc_engine::import::{ImportCandidate, ImportWarning, ScanInput, ScanOutput, ScanProgress, scan_with_subfolders};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

use crate::LccApp;
use crate::icons::Icon;
use crate::render::Slot;
use crate::theme::Tokens;
use crate::widgets::register;

/// Files per batch in the browser build (each batch joins the catalog as it is ready, so the
/// progress window updates); the desktop's worker uses [`lcc_engine::import::batch_size`].
#[cfg(target_arch = "wasm32")]
const BATCH: usize = 8;

/// The review dialog's first size (points). It can be resized; the photo grid takes the height.
pub(crate) const DIALOG_SIZE: [f32; 2] = [960.0, 720.0];
/// Memory keys: where the photo grid ended and where the dialog's content (its buttons) ended, last
/// frame. Their difference is the room the grid leaves below it.
const GRID_BOTTOM: &str = "import-grid-bottom";
const CONTENT_BOTTOM: &str = "import-content-bottom";
/// Room below the grid before it has been measured (more than the options and buttons take).
const DEFAULT_BELOW_GRID: f32 = 360.0;

/// Height of the photo grid: what is `available` once `below` (options and buttons) is left free,
/// in whole points so the window never grows by a fraction each frame, and at least one `row`.
pub(crate) fn grid_height(available: f32, below: f32, row: f32) -> f32 {
    let h = (available - below.max(0.0)).floor();
    if h.is_finite() { h.max(row) } else { (DIALOG_SIZE[1] - DEFAULT_BELOW_GRID).max(row) }
}

/// Remember where the review dialog's content ends this frame (after its buttons), for
/// [`grid_height`] on the next.
pub(crate) fn note_dialog_bottom(ui: &egui::Ui) {
    if !ui.is_sizing_pass() {
        let y = ui.min_rect().bottom();
        ui.ctx().data_mut(|m| m.insert_temp(egui::Id::new(CONTENT_BOTTOM), y));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportDialog {
    /// The files / folders that were scanned (shown as the source).
    pub sources: Vec<String>,
    /// When the source is a folder, whether to search recursively into its subfolders.
    pub subfolders: bool,
    pub candidates: Vec<ImportCandidate>,
    pub checked: Vec<bool>,
    /// Copy into the library (else add in place).
    pub copy: bool,
    /// With `copy`: move instead — the originals are removed from the source once each copy is
    /// verified and catalogued (`library.import` mode `move`).
    pub move_files: bool,
    /// Existing album to add to.
    pub album: Option<u64>,
    /// …or a new album with this name.
    pub new_album: String,
    /// Preset id ("" = none).
    pub preset: String,
    /// The camera profile every raw of its camera gets ("" = automatic: each body's own; ported
    /// from OneSlips, LR-PROF-IMPORT-APPLY).
    pub camera_profile: String,
    /// With `camera_profile` automatic: a profile per camera body for this import (body key → id).
    pub per_body: std::collections::BTreeMap<String, String>,
    /// The preset stack (ids, applied in order after `preset`; the UI keeps them all here:
    /// [`ImportDialog::fold_preset`]).
    pub presets: Vec<String>,
    /// A preset's own camera profile replaces each raw's body profile (`presetProfile`; off: each
    /// raw keeps its body's profile through the presets).
    pub preset_profile: bool,
    /// An optional batch-edit recipe: develop control id → value (only controls moved off their
    /// default are kept), set on every photo this import adds, on top of its sidecar, camera
    /// default and preset (the `recipe` param of `library.import`).
    pub recipe: std::collections::BTreeMap<String, f64>,
    /// Name typed for "Save as Preset" (the recipe saved for reuse).
    pub recipe_name: String,
    /// Comma-separated keywords.
    pub keywords: String,
    /// Copy: destination folder ("" = the library's Originals/).
    pub destination: String,
    /// Copy: `date` (YYYY/YYYY-MM-DD), `month`, `flat` or `custom` ([`ImportDialog::folder_template`]).
    pub organize: String,
    /// Copy, `custom`: the folder template, e.g. `{date:%Y}/{date:%Y%m%d}`.
    pub folder_template: String,
    /// Copy: file-name template for the copies ("" = keep the names).
    pub rename: String,
    /// Metadata preset name ("" = none).
    pub metadata_preset: String,
    /// When true, an inline creator for a new metadata preset is shown.
    #[serde(default)]
    pub creating_meta_preset: bool,
    #[serde(default)]
    pub new_meta_name: String,
    #[serde(default)]
    pub new_meta_creator: String,
    #[serde(default)]
    pub new_meta_copyright: String,
    /// Copy: raws are copied as DNG.
    pub dng: bool,
    /// Per candidate: the library already has it in Recently Deleted ([`ImportDialog::set_trashed`]).
    pub trashed: Vec<bool>,
    /// What to do with those: `restore` (the default: the photo comes back with its edits), `fresh`
    /// (imported as a new photo, the old record deleted) or "" (leave them) — the `onDeleted` param.
    pub on_deleted: String,
    /// The scan's folder walk stopped at a depth or entry limit: there may be more photos than the
    /// review lists ([`ScanOutput::truncated`]).
    pub scan_limited: bool,
    /// Folders the scan couldn't list ([`ScanOutput::unreadable`]).
    pub unreadable: usize,
    /// Files in the scanned folders that are not a supported photo type, XMP sidecars not counted
    /// ([`ScanOutput::unsupported`]).
    pub unsupported: usize,
    /// "Don't Import Suspected Duplicates" unchecked: files whose content the library already has
    /// can be checked and come in as new photos (the `importDuplicates` param).
    pub import_duplicates: bool,
    /// The candidate clicked last: where a Shift-click range starts ([`ImportDialog::click`]).
    #[serde(skip)]
    pub last_clicked: Option<usize>,
    /// The source is a connected Immich server (browsed in the dialog) instead of the scanned files.
    pub immich: bool,
    /// The library the scan was made in ([`lcc_engine::Session::library_generation`]): its duplicate
    /// decisions and album ids belong to that catalog, so the review is not shown or confirmed in another.
    #[serde(skip)]
    pub library: Option<u64>,
    /// The review of the device's camera roll: added in place, with the roll's watch offered.
    #[serde(skip)]
    pub camera_roll: bool,
}

/// A candidate's file type, as the review groups them: its format, or its extension when the
/// probe named none (`JPEG` and `JPG` are one type).
fn file_type(c: &ImportCandidate) -> String {
    let f = if c.format.trim().is_empty() {
        std::path::Path::new(&c.path).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        c.format.clone()
    };
    match f.trim().to_uppercase().as_str() {
        "JPEG" => "JPG".into(),
        "TIFF" => "TIF".into(),
        "" => "?".into(),
        other => other.to_string(),
    }
}

impl ImportDialog {
    /// A clean review dialog with default import settings.
    pub fn empty() -> Self {
        ImportDialog { on_deleted: "restore".into(), subfolders: true, ..Default::default() }
    }

    /// Put the single `preset` (older settings, scripts) at the top of the stack: the UI shows
    /// one ordered list.
    pub fn fold_preset(&mut self) {
        if !self.preset.is_empty() {
            let p = std::mem::take(&mut self.preset);
            self.presets.insert(0, p);
        }
    }

    /// The preset stack and camera profile choices of `from` (the ones kept between imports), for
    /// a new review. A body's choice whose profile is gone goes back to automatic.
    pub fn take_stack_of(&mut self, from: &ImportDialog) {
        self.preset.clone_from(&from.preset);
        self.presets.clone_from(&from.presets);
        self.preset_profile = from.preset_profile;
        self.fold_preset();
        self.camera_profile = Some(from.camera_profile.clone()).filter(|id| lcc_engine::profile_bodies::known_profile(id)).unwrap_or_default();
        self.per_body =
            from.per_body.iter().filter(|(_, id)| lcc_engine::profile_bodies::known_profile(id)).map(|(k, v)| (k.clone(), v.clone())).collect();
    }

    /// Whether any of the scanned sources is a directory.
    pub fn has_folder_source(&self) -> bool {
        self.sources.iter().any(|s| std::path::Path::new(s).is_dir())
    }

    pub fn new(candidates: Vec<ImportCandidate>) -> Self {
        let checked = candidates.iter().map(|c| c.duplicate.is_none() && c.error.is_none()).collect();
        ImportDialog { candidates, checked, on_deleted: "restore".into(), subfolders: true, ..Default::default() }
    }

    /// Whether the library the scan was made in is still the open one (a review that names none counts).
    fn in_library(&self, app: &LccApp) -> bool {
        self.library.is_none_or(|g| g == app.session.library_generation())
    }
    /// Mark the candidates whose photo is in Recently Deleted. They are checked when the choice
    /// for them (`on_deleted`) imports them — by default it restores them, so a photo deleted by
    /// mistake comes back by importing its file again (issue #706).
    pub fn set_trashed(&mut self, trashed: Vec<bool>) {
        self.trashed = trashed;
        for i in 0..self.candidates.len() {
            if !self.is_trashed(i) {
                continue;
            }
            let on = self.checked_by_default(i);
            if let Some(c) = self.checked.get_mut(i) {
                *c = on;
            }
        }
    }
    /// Candidate `i` is a file whose photo is only browsed (Local): the scan offers it (it isn't in the
    /// library). Importing it brings that record into the library where the file is, whatever the
    /// transfer mode.
    pub fn is_browsed(&self, i: usize) -> bool {
        self.candidates.get(i).is_some_and(|c| c.error.is_none() && c.duplicate.is_none() && c.local)
    }
    /// Candidate `i` can be checked: a readable file the library doesn't have, or one whose photo
    /// is in Recently Deleted — even with "Leave them", a click picks it (issue #1285). A browsed
    /// photo can't be copied or moved: the engine would add it in place, so copying leaves it out.
    pub fn importable(&self, i: usize) -> bool {
        self.candidates.get(i).is_some_and(|c| c.error.is_none() && (c.duplicate.is_none() || self.is_trashed(i)))
            && !(self.copy && self.candidates.get(i).is_some_and(|c| c.local))
            || (self.import_duplicates && self.is_suspected_duplicate(i))
    }
    /// Candidate `i` has the same content as a photo the library has (not in Recently Deleted) or
    /// as a file earlier in the list: Lightroom's "suspected duplicate".
    pub fn is_suspected_duplicate(&self, i: usize) -> bool {
        self.candidates.get(i).is_some_and(|c| c.error.is_none() && c.duplicate.as_deref() == Some("content")) && !self.is_trashed(i)
    }
    /// Tick or untick "Don't Import Suspected Duplicates" (`skip`): the suspected duplicates are
    /// unchecked, or checked to be imported as new photos.
    pub fn set_skip_duplicates(&mut self, skip: bool) {
        self.import_duplicates = !skip;
        for i in 0..self.candidates.len() {
            if self.is_suspected_duplicate(i)
                && let Some(c) = self.checked.get_mut(i)
            {
                *c = !skip;
            }
        }
    }
    /// Candidate `i` is checked by Check All and the file type toggles: with "Leave them", a file in
    /// Recently Deleted is only checked by hand.
    pub fn checked_by_default(&self, i: usize) -> bool {
        self.importable(i) && !(self.is_trashed(i) && self.on_deleted.is_empty())
    }
    /// The file types among the candidates (`ARW`, `JPG`…, upper case, sorted) and how many of each.
    pub fn file_types(&self) -> Vec<(String, usize)> {
        let mut types: std::collections::BTreeMap<String, usize> = Default::default();
        for c in &self.candidates {
            *types.entry(file_type(c)).or_default() += 1;
        }
        types.into_iter().collect()
    }
    /// Any file of type `ty` is checked.
    pub fn type_checked(&self, ty: &str) -> bool {
        self.candidates.iter().zip(&self.checked).enumerate().any(|(i, (c, on))| *on && self.importable(i) && file_type(c) == ty)
    }
    /// Check (the importable ones) or uncheck every file of type `ty`: the raws only, say, and not
    /// the JPEGs kept beside them (issue #344).
    pub fn check_type(&mut self, ty: &str, on: bool) {
        for i in 0..self.candidates.len() {
            let same = self.candidates.get(i).is_some_and(|c| file_type(c) == ty);
            let importable = self.checked_by_default(i);
            if same && let Some(c) = self.checked.get_mut(i) {
                *c = on && importable;
            }
        }
    }
    /// Candidate `i` is a file whose photo is in Recently Deleted.
    pub fn is_trashed(&self, i: usize) -> bool {
        self.trashed.get(i).copied().unwrap_or(false)
    }
    /// Choose what happens to the files in Recently Deleted (`""`, `restore` or `fresh`); they are
    /// checked once there is a choice and unchecked again without one.
    pub fn set_on_deleted(&mut self, choice: &str) {
        // switching between restore and fresh keeps the user's per-cell choices
        let changed = self.on_deleted.is_empty() != choice.is_empty();
        self.on_deleted = choice.to_string();
        for i in (0..self.candidates.len()).filter(|_| changed) {
            if self.is_trashed(i)
                && let Some(c) = self.checked.get_mut(i)
            {
                *c = !choice.is_empty();
            }
        }
    }
    /// A click on candidate `i` toggles its checkbox. With `shift`, every importable candidate
    /// from the last clicked one to `i` takes `i`'s new state, as in a file manager: click the
    /// first, Shift-click the last, and the whole range is checked (or unchecked).
    pub fn click(&mut self, i: usize, shift: bool) {
        if !self.importable(i) {
            return;
        }
        let Some(on) = self.checked.get(i).map(|c| !c) else { return };
        let from = if shift { self.last_clicked.filter(|a| *a < self.checked.len()).unwrap_or(i) } else { i };
        for k in from.min(i)..=from.max(i) {
            if self.importable(k)
                && let Some(c) = self.checked.get_mut(k)
            {
                *c = on;
            }
        }
        self.last_clicked = Some(i);
    }
    pub fn selected_paths(&self) -> Vec<String> {
        self.candidates
            .iter()
            .zip(&self.checked)
            .enumerate()
            .filter(|(i, (_, on))| **on && self.importable(*i))
            .map(|(_, (c, _))| c.path.clone())
            .collect()
    }
    /// The `organize` param of `library.import` (`None` = the default, by day); an unusable
    /// custom folder template is an error.
    /// The `library.import` parameters these options give (without `paths`, the album and a
    /// metadata preset still being created: [`start`] adds those).
    pub fn params(&self) -> Result<Value, String> {
        let mode = match (self.copy, self.move_files) {
            (false, _) => "add",
            (true, false) => "copy",
            (true, true) => "move",
        };
        let mut params = json!({"mode": mode, "keywords": self.keywords()});
        if !self.recipe.is_empty() {
            params["recipe"] = recipe_items(&self.recipe);
        }
        if !self.on_deleted.is_empty() {
            params["onDeleted"] = json!(self.on_deleted);
        }
        if self.import_duplicates {
            params["importDuplicates"] = json!(true);
        }
        if !self.preset.is_empty() {
            params["preset"] = json!(self.preset);
        }
        if !self.presets.is_empty() {
            params["presets"] = json!(self.presets);
        }
        if self.preset_profile {
            params["presetProfile"] = json!(true);
        }
        if !self.camera_profile.is_empty() {
            params["cameraProfile"] = json!({"all": self.camera_profile});
        } else if !self.per_body.is_empty() {
            params["cameraProfile"] = json!({"perBody": self.per_body});
        }
        if !self.metadata_preset.is_empty() {
            params["metadataPreset"] = json!(self.metadata_preset);
        }
        if let Some(a) = self.album {
            params["album"] = json!(a);
        }
        if self.copy {
            if !self.destination.trim().is_empty() {
                params["destination"] = json!(self.destination.trim());
            }
            if let Some(o) = self.organize_param()? {
                params["organize"] = json!(o);
            }
            if !self.rename.trim().is_empty() {
                params["rename"] = json!(self.rename.trim());
                params["renameStart"] = json!(1);
            }
            if self.dng && !self.move_files {
                params["dng"] = json!(true);
            }
        }
        Ok(params)
    }

    pub fn organize_param(&self) -> Result<Option<String>, String> {
        match self.organize.as_str() {
            "" => Ok(None),
            "custom" => {
                let t = self.folder_template.trim();
                if let Some(e) = lcc_engine::rename::folder_template_error(t) {
                    return Err(e);
                }
                // a plain folder name ("Imports") is a one-level template too
                Ok(Some(if t.contains(['{', '/', '\\']) { t.to_string() } else { format!("{t}/") }))
            }
            o => Ok(Some(o.to_string())),
        }
    }
    /// Keywords or a preset are set while a checked browsed photo would join the library as it is:
    /// the engine applies them to new files only, so the review says so.
    pub(crate) fn options_skip_browsed(&self) -> bool {
        let has_options = !self.keywords().is_empty() || !self.preset.is_empty() || !self.metadata_preset.is_empty();
        has_options && !self.copy && (0..self.candidates.len()).any(|i| self.checked.get(i).copied().unwrap_or(false) && self.is_browsed(i))
    }
    fn keywords(&self) -> Vec<String> {
        self.keywords.split(',').map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
    }
}

/// A running import (see the module docs). The file-system work — expanding folders, probing,
/// reading sidecars, copying or placing files — runs on a worker thread
/// ([`lcc_engine::import::ImportJob`]); each batch it readies is added to the catalog on the
/// UI thread between frames, so a slow drive never stalls the window.
#[derive(Debug, Default)]
pub struct ImportTask {
    /// Files and folders to import (handed to the worker when the task starts).
    queue: Vec<String>,
    pub total: usize,
    pub done: usize,
    params: Value,
    pub imported: usize,
    /// Brought back from Recently Deleted (`onDeleted: restore`).
    pub restored: usize,
    pub duplicates: usize,
    /// The library photos the skipped duplicates match (shown when nothing new was added).
    existing: Vec<u64>,
    pub failed: usize,
    /// Move: originals moved, and sources left in place (reported by the engine with a reason).
    pub moved: usize,
    pub kept: usize,
    /// Structured diagnostics from foreign XMP develop settings.
    pub warnings: Vec<ImportWarning>,
    undo0: usize,
    first: Option<u64>,
    /// Reading a folder for the Local view: the photos stay out of the library, and nothing is
    /// selected or announced as added.
    browse: bool,
    /// Cancel was pressed: no further files are started; batches already readied are added.
    pub cancelled: bool,
    /// Auto Import (the watched folder): the selection stays as it is, a short toast when done.
    pub(crate) auto: bool,
    /// Android's watched camera roll: like `auto`, and the toast says where the photos came from.
    camera_roll: bool,
    /// Auto Import: the files it was handed (tried again later when the import can't start).
    auto_paths: Vec<String>,
    /// Auto Import: the selection to keep.
    keep_selection: Option<lcc_engine::Selection>,
    /// The selection and source as the import last left them: a change between frames is the
    /// user choosing another photo or view, which the end of the import doesn't take away.
    left: Option<(lcc_engine::Selection, lcc_engine::LibrarySource)>,
    /// The user chose another photo or view while the import ran.
    moved_on: bool,
    run: Option<ImportRun>,
    /// The import's row in the activity stack (its ✕ and `activity.cancel` set the run's cancel flag).
    pub(crate) guard: Option<lcc_engine::activity::TaskGuard>,
    /// The Import tab's: once the photos are in, their previews are built unattended
    /// (`none` | `standard` | `full`: `library.finishImport`).
    pub finish_previews: Option<String>,
    /// The photos this import added or restored, in order (for the finishing).
    ids: Vec<u64>,
}

impl ImportTask {
    /// An import of `paths` (files or folders) with `library.import` params (without `paths`).
    pub fn new(paths: Vec<String>, params: Value, undo0: usize, browse: bool) -> Self {
        ImportTask { total: paths.len(), queue: paths, params, undo0, browse, ..Default::default() }
    }

    /// Reading a folder for the Local view (not an import into the library).
    pub fn is_browse(&self) -> bool {
        self.browse
    }

    /// An Auto Import (see [`ImportTask::auto`]).
    pub fn auto(mut self) -> Self {
        self.auto = true;
        self
    }

    /// The watched camera roll's import (see [`ImportTask::camera_roll`]).
    pub fn camera_roll(mut self) -> Self {
        self.camera_roll = true;
        self
    }

    /// `{done, total, imported, cancelled}` for `ui.inspect`.
    pub fn status(&self) -> Value {
        json!({"done": self.done, "total": self.total, "imported": self.imported, "cancelled": self.cancelled})
    }
}

/// Where an import's batches are readied: a worker thread, or (browser build: no threads, and no
/// slow drives) inline, one batch per frame.
enum Runner {
    #[cfg(not(target_arch = "wasm32"))]
    Thread(std::sync::mpsc::Receiver<lcc_engine::import::Prepared>),
    #[cfg(target_arch = "wasm32")]
    Inline { job: Box<lcc_engine::import::ImportJob>, queue: Vec<String>, files: Option<std::collections::VecDeque<String>> },
}

struct ImportRun {
    runner: Runner,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    opts: lcc_engine::import::ImportOptions,
    now: String,
    album: Option<u64>,
    album_name: Option<String>,
}

impl std::fmt::Debug for ImportRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRun").field("cancel", &self.cancel.load(Ordering::Relaxed)).finish()
    }
}

impl Drop for ImportRun {
    /// A dropped import (replaced, or the app closing) stops its worker at the next file.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ImportRun {
    fn start(
        job: lcc_engine::import::ImportJob,
        queue: Vec<String>,
        album: Option<u64>,
        album_name: Option<String>,
        ctx: &egui::Context,
    ) -> Result<Self, String> {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (total, done) = (job.total.clone(), job.done.clone());
        let (opts, now) = (job.opts.clone(), job.now().to_string());
        let runner = Self::spawn(job, queue, cancel.clone(), ctx)?;
        Ok(ImportRun { runner, cancel, total, done, opts, now, album, album_name })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(
        mut job: lcc_engine::import::ImportJob,
        queue: Vec<String>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ctx: &egui::Context,
    ) -> Result<Runner, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let work = move || {
            let files = job.expand(&queue);
            for chunk in files.chunks(lcc_engine::import::batch_size()) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let prepared = match lcc_engine::guard::catch("import", || job.prepare_files(chunk.to_vec(), &cancel)) {
                    Ok(p) => p,
                    Err(_) => break, // logged; the files readied so far are added
                };
                if let Err(unsent) = tx.send(prepared) {
                    // the import was dropped: nothing will add these, so take back what a Move placed
                    unsent.0.rollback();
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        };
        std::thread::Builder::new().name("lc-import".into()).spawn(work).map(|_| Runner::Thread(rx)).map_err(|e| format!("could not start: {e}"))
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(
        job: lcc_engine::import::ImportJob,
        queue: Vec<String>,
        _cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        _ctx: &egui::Context,
    ) -> Result<Runner, String> {
        Ok(Runner::Inline { job: Box::new(job), queue, files: None })
    }

    /// Readied batches (at most a few per frame). `None` once the worker is done.
    fn next_batches(&mut self) -> Option<Vec<lcc_engine::import::Prepared>> {
        match &mut self.runner {
            #[cfg(not(target_arch = "wasm32"))]
            Runner::Thread(rx) => {
                let mut out = Vec::new();
                while out.len() < 4 {
                    match rx.try_recv() {
                        Ok(p) => out.push(p),
                        Err(std::sync::mpsc::TryRecvError::Empty) => return Some(out),
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return if out.is_empty() { None } else { Some(out) },
                    }
                }
                Some(out)
            }
            #[cfg(target_arch = "wasm32")]
            Runner::Inline { job, queue, files } => {
                if self.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if files.is_none() {
                    *files = Some(job.expand(queue).into());
                    return Some(Vec::new());
                }
                let Some(files) = files.as_mut() else { return None };
                if files.is_empty() {
                    return None;
                }
                let n = files.len().min(BATCH);
                let batch: Vec<String> = files.drain(..n).collect();
                Some(vec![job.prepare_files(batch, &self.cancel)])
            }
        }
    }
}

/// A folder scan running on a worker thread (a network share can take minutes to read; the
/// window must keep answering meanwhile).
pub struct ScanTask {
    pub(crate) progress: std::sync::Arc<ScanProgress>,
    rx: std::sync::mpsc::Receiver<ScanOutput>,
    /// Open the review with "copy into the library" checked (a camera / card).
    pub copy: bool,
    /// Open the review copying into this folder, all into it with no subfolders (a folder's menu
    /// ▸ Import to This Folder…); implies `copy`.
    pub destination: Option<String>,
    /// Open the review with this album chosen (an album's context menu ▸ Import…).
    pub album: Option<u64>,
    /// Browsing a folder (Local): the photos are read in place instead of opening the review.
    pub(crate) browse: bool,
    /// What is being scanned (the review's source).
    sources: Vec<String>,
    /// The scan's row in the activity stack (its ✕ and `activity.cancel` set `progress.cancel`).
    guard: lcc_engine::activity::TaskGuard,
    /// Whether subfolders are being scanned.
    pub subfolders: bool,
    /// The device's camera roll is being scanned (`file.addCameraRoll`): the review offers to
    /// keep watching it.
    pub(crate) camera_roll: bool,
}

/// The activity row of a scan: the review's ("Scanning folder") or the Local view's ("Reading folder").
fn scan_guard(app: &LccApp, progress: &ScanProgress, browse: bool) -> lcc_engine::activity::TaskGuard {
    let label = if browse { "Reading folder" } else { "Scanning folder" };
    app.session.activity.start("scan", label, lcc_engine::activity::Cancel::Flag(progress.cancel.clone()))
}

/// An import and a Synchronize Folder never run at once: each readies its files against the
/// library as it was when it started, so both could add the same file.
pub(crate) fn busy_synchronizing(app: &LccApp) -> Result<(), String> {
    if app.sync_run.is_some() { Err(crate::i18n::tr("A folder is being synchronized").to_string()) } else { Ok(()) }
}

/// Settings → Import → "Show the import review when a camera or card is connected", called with
/// each device scan while it's on. The first look only notes the cards already in. A card that
/// shows up after that opens Import from Device once nothing else is open, and a card that is
/// taken out and put back in asks again.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn cards_seen(app: &mut LccApp, now: Vec<lcc_engine::devices::Device>) {
    let Some(known) = app.ui.known_cards.as_mut() else {
        app.ui.known_cards = Some(now.into_iter().map(|d| d.root).collect());
        return;
    };
    known.retain(|root| now.iter().any(|d| d.root == *root));
    let Some(card) = now.into_iter().find(|d| !known.contains(&d.root)) else { return };
    // busy: ask when the window is free (the card isn't noted until then)
    if app.import.is_some() || app.scan.is_some() || app.sync_run.is_some() || app.lightroom.is_some() || app.ui.dialog.is_some() {
        return;
    }
    known.push(card.root);
    if let Err(e) = app.run("file.addFromDevice", json!({"path": card.path})) {
        log::warn!("import from {}: {e}", card.name);
    }
}

/// Scan `paths` in the background, then open the review dialog (see [`poll_scan`]).
pub fn open(app: &mut LccApp, paths: Vec<String>) -> Result<Value, String> {
    open_with_subfolders(app, paths, true)
}

/// Scan `paths` in the background with `subfolders` option, then open the review dialog.
pub fn open_with_subfolders(app: &mut LccApp, paths: Vec<String>, subfolders: bool) -> Result<Value, String> {
    if app.scan.is_some() {
        return Err("a scan is already running".into());
    }
    if app.review_pending.is_some() {
        return Err("an import review is waiting for the open dialog to close".into());
    }
    let sources = paths.clone();
    let (input, paths) = ScanInput::new(&mut app.session, &paths);
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let sub = subfolders;
    let job = move || {
        let _ = tx.send(scan_with_subfolders(input, &paths, &p, sub));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, false);
    app.scan =
        Some(ScanTask { progress, rx, copy: false, destination: None, album: None, browse: false, sources, guard, subfolders, camera_roll: false });
    Ok(json!({"scanning": true}))
}

/// Re-scan `d.sources` in the background with the given `subfolders` setting.
pub fn rescan_with_subfolders(app: &mut LccApp, d: &ImportDialog, subfolders: bool) -> Result<Value, String> {
    if let Some(t) = app.scan.take() {
        t.progress.cancel.store(true, Ordering::Relaxed);
    }
    if d.sources.is_empty() {
        return Ok(Value::Null);
    }
    let (input, paths) = ScanInput::new(&mut app.session, &d.sources);
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let sources = d.sources.clone();
    let copy = d.copy;
    let album = d.album;
    let sub = subfolders;
    let job = move || {
        let _ = tx.send(scan_with_subfolders(input, &paths, &p, sub));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask { progress, rx, copy, destination: None, album, browse: false, sources, guard, subfolders, camera_roll: false });
    Ok(json!({"scanning": true}))
}

/// Show a folder's photos in the Local view (`library.browse` from the UI): the view switches at
/// once, the folder is listed and read on a worker thread (a network share can take minutes),
/// and the photos then join the view in small batches under a progress window. A browse already
/// running is replaced; the import review's scan is not.
pub fn browse(app: &mut LccApp, path: &str, subfolders: Option<bool>, show: Option<&str>) -> Result<Value, String> {
    let mut lap = crate::now_ms();
    let dir = std::path::absolute(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    if !dir.is_dir() {
        return Err(format!("{path}: not a folder"));
    }
    if app.scan.as_ref().is_some_and(|t| !t.browse) || app.import.as_ref().is_some_and(|t| !t.browse) {
        return Err("an import is running".into());
    }
    let dir_s = dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    // subfolders: given, it wins; left out, a re-browse of the same folder keeps the choice and a
    // new folder starts with subfolders in — a folder picked to import means the tree under it
    // (the engine's library.browse applies the same rule)
    let subfolders = match subfolders {
        Some(given) => given,
        None => app.session.browse.as_ref().is_none_or(|b| if b.path == dir_s { b.subfolders } else { true }),
    };
    // the quick filter: given, it wins; left out, a re-browse of the same folder keeps the choice
    // and a new folder starts on Show Not Imported (the engine's library.browse applies the same
    // rule with the choice as it stands)
    let not_imported = match show {
        Some("all") => false,
        Some("notImported") | Some(_) => true,
        // same folder keeps the choice; a new folder starts on Show Not Imported
        None => app.session.browse.as_ref().is_none_or(|b| if b.path == dir_s { b.not_imported } else { true }),
    };
    let running = app.scan.as_ref().is_some_and(|t| t.browse) || app.import.as_ref().is_some_and(|t| t.browse);
    if running && app.session.browse.as_ref().is_some_and(|b| b.path == dir_s && b.subfolders == subfolders) {
        // already reading this folder: clicking it again must not restart the progress
        app.session.source = lcc_engine::LibrarySource::Folder;
        app.session.keep_active_in_view();
        return Ok(json!({"path": dir_s, "subfolders": subfolders, "scanning": true}));
    }
    if let Some(t) = app.scan.take() {
        t.progress.cancel.store(true, Ordering::Relaxed);
    }
    app.import = None;
    app.session.browse = Some(lcc_engine::Browse { path: dir_s.clone(), subfolders, not_imported });
    app.session.source = lcc_engine::LibrarySource::Folder;
    app.lap(&mut lap, "browse.start");
    // the photos the library already knows from this folder show at once: keep or pick the active one
    app.session.keep_active_in_view();
    app.lap(&mut lap, "browse.view");
    let (input, _) = ScanInput::new(&mut app.session, std::slice::from_ref(&dir_s));
    app.lap(&mut lap, "browse.library");
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let root = dir_s.clone();
    let sort = app.session.sort;
    let job = move || {
        // a folder that can't be listed (no permission: macOS asks per folder; gone; offline) is
        // said so, not shown as an empty folder (issue #966)
        if let Err(e) = std::fs::read_dir(&root) {
            log::warn!("browse {root}: {e}");
            let error = lcc_engine::import::ImportCandidate { path: root, error: Some(e.to_string()), ..Default::default() };
            let _ = tx.send(lcc_engine::import::ScanOutput { candidates: vec![error], unreadable: 1, ..Default::default() });
            return;
        }
        let files: Vec<String> = if subfolders {
            lcc_engine::import::expand(&[root], None)
        } else {
            let mut v: Vec<String> = std::fs::read_dir(&root)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|f| {
                            f.is_file() && lcc_engine::import::is_supported(f) && !f.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
                        })
                        .map(|f| f.to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        };
        // Nothing is read here: the import that follows reads the files and adds them batch by
        // batch, first screen first, so the view fills while the rest is read (PERF-CULL-1).
        // Every listed file is a candidate (the ones the library knows as it knows them),
        // so the folder's known photos still count as listed and aren't forgotten.
        let candidates = lcc_engine::import::browse_candidates(&input, files, &sort);
        p.total.store(candidates.len(), Ordering::Relaxed);
        p.done.store(candidates.len(), Ordering::Relaxed);
        let _ = tx.send(lcc_engine::import::ScanOutput { candidates, ..Default::default() });
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, true);
    app.scan = Some(ScanTask {
        progress,
        rx,
        copy: false,
        destination: None,
        album: None,
        browse: true,
        sources: Vec::new(),
        guard,
        subfolders,
        camera_roll: false,
    });
    app.renderer.forget_imports();
    app.lap(&mut lap, "browse.rest");
    Ok(json!({"path": dir_s, "subfolders": subfolders, "scanning": true}))
}

/// Collect a finished scan and open the review (called every frame).
pub fn poll_scan(app: &mut LccApp, ctx: &egui::Context) {
    show_pending_review(app, ctx);
    let Some(task) = app.scan.as_ref() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let out = match task.rx.try_recv() {
        Ok(o) => o,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            if task.guard.is_cancelled() {
                // ✕: the worker stops at its next file; don't wait for it (a NAS read can take a while)
                app.scan = None;
                return;
            }
            let (total, done) = (task.progress.total.load(Ordering::Relaxed), task.progress.done.load(Ordering::Relaxed));
            task.guard.progress(done as u64, total as u64);
            // while the folders are listed the total isn't known: say what is happening instead
            task.guard.detail(if total == 0 { crate::i18n::tr("Looking for photos…") } else { "" });
            return;
        }
        Err(_) => {
            app.scan = None;
            app.toast(ctx, crate::i18n::tr("Scan failed"));
            return;
        }
    };
    let Some(task) = app.scan.take() else { return };
    if task.progress.cancel.load(Ordering::Relaxed) {
        return;
    }
    app.session.import_probes = out.probes;
    if task.browse {
        // the folder couldn't be listed: said so (the photos the library knows from there stay)
        if out.unreadable > 0
            && let [c] = out.candidates.as_slice()
            && let Some(e) = &c.error
        {
            app.toast(ctx, crate::i18n::tr_format!("Could not read {name}: {e}", name = c.path, e = e));
            return;
        }
        let mut lap = crate::now_ms();
        // the folder and the ones its photos are in count as browsed now, as `library.browse` records them, so
        // their untouched Local photos aren't forgotten while the folder is in use
        if let Some(root) = app.session.browse.as_ref().map(|b| b.path.clone()) {
            let shown: Vec<String> = out.candidates.iter().map(|c| c.path.clone()).collect();
            app.session.note_browsed(&root, &shown);
        }
        // records of files gone from the folder (deleted or moved by another app) that nothing was changed on go,
        // as `library.browse` forgets them (issue #1392)
        if let Some(b) = app.session.browse.clone() {
            let listed: Vec<String> = out.candidates.iter().map(|c| c.path.clone()).collect();
            app.lap(&mut lap, "browse.noted");
            app.session.forget_gone_from(&b.path, b.subfolders, &listed);
            app.lap(&mut lap, "browse.forgetGone");
        }
        // what the library doesn't know yet joins the Local view
        let queue: Vec<String> =
            out.candidates.iter().filter(|c| c.duplicate != Some("path".into()) && !c.local && c.error.is_none()).map(|c| c.path.clone()).collect();
        if !queue.is_empty() {
            let undo0 = app.session.undo.len();
            let total = queue.len();
            let _ = total;
            app.import = Some(ImportTask::new(queue, json!({"mode": "add", "local": true}), undo0, true));
        }
        return;
    }
    let dialog_was_import = matches!(app.ui.dialog, Some(crate::state::Dialog::Import { .. }));
    // (only the Import dialog is taken: any other dialog the person has open stays, and the review
    // waits behind it)
    let mut d = match app.ui.dialog.take() {
        Some(crate::state::Dialog::Import { opts }) => {
            let mut d = *opts;
            d.candidates = out.candidates;
            d.checked = (0..d.candidates.len()).map(|i| d.checked_by_default(i)).collect();
            d
        }
        other => {
            app.ui.dialog = other;
            let mut d = ImportDialog::new(out.candidates);
            // the preset stack and camera profiles used last (the Import tab's, kept between sessions)
            d.take_stack_of(&app.ui.settings.import_tab.opts);
            d
        }
    };
    (d.scan_limited, d.unreadable, d.unsupported) = (out.truncated, out.unreadable, out.unsupported);
    let trashed = d
        .candidates
        .iter()
        .map(|c| c.duplicate.is_some() && c.existing.is_some_and(|id| app.session.catalog.photo(lcc_catalog::PhotoId(id)).is_some_and(|p| p.deleted)))
        .collect();
    d.set_trashed(trashed);
    d.copy = task.copy || task.destination.is_some();
    if let Some(dest) = task.destination {
        d.destination = dest;
        // the files go into that folder itself, not into dated subfolders of it
        d.organize = "flat".into();
    }
    // only an album the review offers (it may have been deleted while the scan ran)
    if task.album.is_some() {
        d.album = task.album.filter(|id| app.session.catalog.album(lcc_catalog::AlbumId(*id)).is_some_and(|a| !a.folder && !a.is_smart()));
    }
    d.sources = task.sources;
    d.subfolders = task.subfolders;
    if task.camera_roll {
        // the roll's files stay where the camera puts them: a copy would only double them
        d.camera_roll = true;
        d.copy = false;
        d.move_files = false;
    }
    if d.metadata_preset.is_empty()
        && let Some(mp) = &app.session.import_defaults.metadata_preset
        && app.session.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(mp))
    {
        d.metadata_preset = mp.clone();
    }
    d.library = Some(app.session.library_generation());
    if d.candidates.is_empty() && !dialog_was_import && !d.has_folder_source() {
        app.toast(ctx, crate::i18n::tr("No photos found"));
        return;
    }
    app.renderer.forget_imports();
    app.review_pending = Some(crate::state::Dialog::Import { opts: Box::new(d) });
    show_pending_review(app, ctx);
}

/// Open the review of a finished scan once no other dialog is open (a face model's licence, Settings…
/// opened while the scan ran stays), checked again shortly while it waits. A review whose library was
/// closed meanwhile (Settings can open another) is dropped.
fn show_pending_review(app: &mut LccApp, ctx: &egui::Context) {
    if app.review_pending.is_none() {
        return;
    }
    if let Some(crate::state::Dialog::Import { opts }) = &app.review_pending
        && !opts.in_library(app)
    {
        app.review_pending = None;
        return;
    }
    // (a review over the Import screen's own review takes its place: picking another source there
    // is what asked for it; any other dialog stays and the review waits)
    if app.ui.dialog.is_none() || matches!(app.ui.dialog, Some(crate::state::Dialog::Import { .. })) {
        app.ui.dialog = app.review_pending.take();
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Tests: a review scan that never ends while the returned value lives, so a drop meets it running.
#[cfg(test)]
pub(crate) fn stall_scan(app: &mut LccApp) -> Box<dyn std::any::Any> {
    stall_scan_of(app, false)
}

/// Tests: the same, for a Local folder browse scan (`browse`) instead of the review's scan.
#[cfg(test)]
pub(crate) fn stall_scan_of(app: &mut LccApp, browse: bool) -> Box<dyn std::any::Any> {
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel::<ScanOutput>();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask {
        progress,
        rx,
        copy: false,
        destination: None,
        album: None,
        browse,
        sources: Vec::new(),
        guard,
        subfolders: true,
        camera_roll: false,
    });
    Box::new(tx)
}

/// Whether an import or any scan (the review's, or a Local folder browse) is running, or a finished scan's
/// review waits for a dialog to close, which a drop of photos must not replace: `open` refuses while a scan
/// runs, and a browse that finishes replaces `app.import`.
pub fn running(app: &LccApp) -> bool {
    app.import.is_some() || app.scan.is_some() || app.review_pending.is_some()
}

/// Whether a folder's menu can start an import review now: not while a scan or an import runs (a
/// Local folder being read counts), a finished scan's review waits for a dialog to close, or a
/// Lightroom catalog import runs (as for File ▸ Import from Folder…).
pub fn can_open(app: &LccApp) -> bool {
    !running(app) && !crate::lightroom_import::is_running(app)
}

impl ScanTask {
    /// `{done, total}` for `ui.inspect` (total is 0 while the folders are still being listed).
    pub fn status(&self) -> Value {
        json!({"done": self.progress.done.load(Ordering::Relaxed), "total": self.progress.total.load(Ordering::Relaxed)})
    }
}

/// Start importing the dialog's checked files (the dialog's OK / `ui.dialog.confirm`). Refused while another
/// import runs: replacing `app.import` would stop its worker.
pub fn start(app: &mut LccApp, d: &ImportDialog) -> Result<Value, String> {
    busy_synchronizing(app)?;
    #[cfg(not(target_arch = "wasm32"))]
    if d.immich {
        return crate::panels::connections::start_import(app);
    }
    if !d.in_library(app) {
        return Err("another catalog was opened since the photos were scanned: nothing was imported".into());
    }
    // a browse scan that finishes starts its own import, replacing this one
    if app.import.is_some() || app.scan.as_ref().is_some_and(|t| t.browse) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    let queue = d.selected_paths();
    if queue.is_empty() {
        return Err("no photos selected".into());
    }
    let undo0 = app.session.undo.len();
    let mut album = d.album;
    if album.is_none() && !d.new_album.trim().is_empty() {
        let r = app.session.execute("album.create", &json!({"name": d.new_album.trim()})).map_err(|e| e.to_string())?;
        album = r["id"].as_u64();
    }
    let mut params = d.params()?;
    if let Some(a) = album {
        params["album"] = json!(a);
    }
    let mut meta_preset = d.metadata_preset.clone();
    if d.creating_meta_preset && !d.new_meta_name.trim().is_empty() {
        let name = d.new_meta_name.trim().to_string();
        let mut fields = serde_json::Map::new();
        if !d.new_meta_creator.trim().is_empty() {
            fields.insert("creator".into(), json!(d.new_meta_creator.trim()));
        }
        if !d.new_meta_copyright.trim().is_empty() {
            fields.insert("copyright".into(), json!(d.new_meta_copyright.trim()));
        }
        let _ = app.session.execute(
            "metadata.savePreset",
            &json!({
                "name": name,
                "fields": fields,
            }),
        );
        meta_preset = name;
    }
    if !meta_preset.is_empty() {
        params["metadataPreset"] = json!(meta_preset);
    }
    let total = queue.len();
    app.import = Some(ImportTask::new(queue, params, undo0, false));
    app.renderer.forget_imports();
    remember_stack(app, d);
    Ok(json!({"importing": total}))
}

/// Keep the preset stack and camera profile choices an import ran with for the next one (the
/// Import tab and the review share them, kept between sessions).
fn remember_stack(app: &mut LccApp, d: &ImportDialog) {
    let kept = &mut app.ui.settings.import_tab;
    let mut now = ImportDialog::default();
    now.take_stack_of(d);
    let same = kept.opts.preset.is_empty()
        && kept.opts.presets == now.presets
        && kept.opts.preset_profile == now.preset_profile
        && kept.opts.camera_profile == now.camera_profile
        && kept.opts.per_body == now.per_body;
    if !same {
        kept.opts.preset.clear();
        kept.opts.presets = now.presets;
        kept.opts.preset_profile = now.preset_profile;
        kept.opts.camera_profile = now.camera_profile;
        // (the review started from these, so other bodies' choices come back with it)
        kept.opts.per_body = now.per_body;
        kept.preset_name.clear();
    }
}

/// Start importing `paths` (files or folders) in the background, e.g. dropped on the window.
pub fn start_paths(app: &mut LccApp, paths: Vec<String>) -> Result<Value, String> {
    start_paths_with(app, paths, json!({"mode": "add"}))
}

/// [`start_paths`] with the import's own `library.import` params (the paths are added to them).
fn start_paths_with(app: &mut LccApp, paths: Vec<String>, params: Value) -> Result<Value, String> {
    busy_synchronizing(app)?;
    if running(app) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    let undo0 = app.session.undo.len();
    let total = paths.len();
    app.import = Some(ImportTask::new(paths, params, undo0, false));
    Ok(json!({"importing": total}))
}

/// Start importing `paths` in place, without review, the way the watched camera roll adds new
/// photos (Android, `camera_roll::watch_tick`): as [`start_paths_with`] with the library's import
/// settings ([`dropped_import_params`]), the selection stays as it is, and the toast when it is
/// done says where the photos came from. Refused while another import runs or a folder
/// synchronizes.
pub(crate) fn start_camera_roll(app: &mut LccApp, paths: Vec<String>) -> Result<Value, String> {
    let params = dropped_import_params(app);
    busy_synchronizing(app)?;
    if running(app) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    let undo0 = app.session.undo.len();
    let total = paths.len();
    app.import = Some(ImportTask::new(paths, params, undo0, false).auto().camera_roll());
    Ok(json!({"importing": total}))
}

/// The `library.import` params of a drop imported without review: added in place, with the
/// library's import settings (its presets, camera defaults, creator and copyright apply to every
/// import; the metadata preset is the one the review would start with).
fn dropped_import_params(app: &LccApp) -> Value {
    let mut p = json!({"mode": "add"});
    if let Some(mp) = &app.session.import_defaults.metadata_preset
        && app.session.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(mp))
    {
        p["metadataPreset"] = json!(mp);
    }
    p
}

/// Photos or folders dropped on the window. They are scanned and reviewed exactly as File ▸ Import
/// from Folder… does (one walk with progress, then the review), or, with Settings ▸ Import ▸ Import
/// dropped photos without review, added at once on a worker thread with the library's import
/// settings. A drop while an import or any
/// scan runs is refused (a browse that finishes replaces `app.import`). Errors are shown, and a
/// frame is asked for so the scan or import starts even if the window then sits idle (no input
/// follows a drop).
pub fn drop_paths(app: &mut LccApp, ctx: &egui::Context, paths: Vec<String>) {
    ctx.request_repaint();
    if app.lightroom.is_some() {
        app.toast(ctx, crate::i18n::tr("Finish the catalog import before adding photos"));
        return;
    }
    // an import (or a scan, a review waiting behind a dialog) stays the one that runs: a drop
    // meanwhile would replace it
    if running(app) {
        app.toast(ctx, crate::i18n::tr("An import is running"));
        return;
    }
    // the import review first, unless Settings ▸ Import says to add dropped photos at once
    let r = if app.ui.settings.import_dropped_directly {
        let params = dropped_import_params(app);
        start_paths_with(app, paths, params).map(|_| ())
    } else {
        busy_synchronizing(app).and_then(|_| open(app, paths).map(|_| ()))
    };
    if let Err(e) = r {
        log::warn!("drop: {e}");
        app.toast(ctx, crate::i18n::tr_format!("Import failed: {e}", e = e));
    }
}

/// The activity kind of a browse's folder read (`library.browse` in the Import tab): a scan, not
/// [`lcc_engine::activity::Activity::user_work`].
pub(crate) const BROWSE_READ_KIND: &str = "scan";

/// Advance the import in progress (called every frame): start its worker, then add the batches it
/// has readied to the catalog.
pub fn tick(app: &mut LccApp, ctx: &egui::Context) {
    let Some(mut task) = app.import.take() else { return };
    if task.run.is_none() {
        let mut p = task.params.clone();
        p["paths"] = json!(task.queue);
        if task.auto {
            task.auto_paths = task.queue.clone();
        }
        let started = lcc_engine::cmd::library::import_params(&app.session, &p)
            .and_then(|req| Ok((lcc_engine::import::ImportJob::new(&mut app.session, req.opts)?, req.album, req.album_name)))
            .map_err(|e| e.to_string())
            .and_then(|(job, album, name)| ImportRun::start(job, std::mem::take(&mut task.queue), album, name, ctx));
        if task.auto {
            task.keep_selection = Some(app.session.selection.clone());
        }
        match started {
            Ok(run) => {
                // (reading a folder to browse it is not the user's own work the way an import is:
                // the photos being culled meanwhile are prepared, and their neighbours made ahead;
                // as an "import" it held all of that back for the whole read, 90 s for a wedding)
                let (kind, label) = if task.browse { (BROWSE_READ_KIND, "Reading folder") } else { ("import", "Importing") };
                task.guard = Some(app.session.activity.start(kind, label, lcc_engine::activity::Cancel::Flag(run.cancel.clone())));
                task.run = Some(run);
            }
            Err(e) => {
                log::warn!("import: {e}");
                // Auto Import's files weren't read: the next scans try them again (#1302)
                if task.auto {
                    app.session.auto_import_retry(&task.auto_paths);
                }
                app.toast(ctx, crate::i18n::tr_format!("Import failed: {e}", e = e));
                return;
            }
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let Some(run) = task.run.as_mut() else { return };
    let batches = run.next_batches();
    let finished = batches.is_none();
    if task.left.as_ref().is_some_and(|(sel, source)| *sel != app.session.selection || *source != app.session.source) {
        task.moved_on = true;
    }
    for prepared in batches.into_iter().flatten() {
        commit_batch(app, &mut task, prepared);
        // arriving photos don't take over the selection
        if let Some(sel) = &task.keep_selection {
            app.session.selection = sel.clone();
        }
        // a Local folder still on screen: its first photo becomes active once there is one (#1447)
        if task.browse && app.session.source == lcc_engine::LibrarySource::Folder {
            app.session.keep_active_in_view();
        }
    }
    task.left = Some((app.session.selection.clone(), app.session.source));
    if let Some(run) = &task.run {
        task.total = run.total.load(Ordering::Relaxed).max(task.total);
        task.done = run.done.load(Ordering::Relaxed);
    }
    if let Some(guard) = &task.guard {
        guard.progress(task.done as u64, task.total as u64);
        // ✕ in the stack (or `activity.cancel`): no further files are started
        if guard.is_cancelled() {
            task.cancelled = true;
        }
    }
    if !finished {
        app.import = Some(task);
        return;
    }
    finish(app, ctx, task);
}

/// Add one readied batch to the catalog (an undo step merged into the import's at the end).
fn commit_batch(app: &mut LccApp, task: &mut ImportTask, prepared: lcc_engine::import::Prepared) {
    let Some(run) = task.run.as_ref() else { return };
    let n = prepared.len();
    let (opts, now, album, album_name) = (&run.opts, run.now.as_str(), run.album, run.album_name.clone());
    let r = app.session.execute_fn("library.import", |s| {
        let report = lcc_engine::import::commit_prepared(s, opts, now, prepared)?;
        lcc_engine::cmd::library::import_batch_done(s, report, album, album_name.as_deref())
    });
    match r {
        Ok(v) => {
            // a new album (named in the params) is created with the first photos; later batches join it
            if let (Some(a), Some(run)) = (v["album"].as_u64(), task.run.as_mut()) {
                run.album = Some(a);
            }
            let len = |k: &str| v[k].as_array().map_or(0, Vec::len);
            task.imported += len("imported");
            task.restored += len("restored");
            task.duplicates += len("duplicates");
            task.existing.extend(v["duplicates"].as_array().into_iter().flatten().filter_map(|d| d["existing"].as_u64()));
            task.failed += len("failed");
            task.moved += len("moved");
            task.kept += len("kept");
            task.warnings.extend(v["warnings"].as_array().into_iter().flatten().filter_map(|warning| serde_json::from_value(warning.clone()).ok()));
            for k in v["kept"].as_array().into_iter().flatten() {
                log::warn!("import: kept {}: {}", k["path"].as_str().unwrap_or(""), k["reason"].as_str().unwrap_or(""));
            }
            task.ids.extend(["imported", "restored"].iter().flat_map(|k| v[*k].as_array().into_iter().flatten().filter_map(Value::as_u64)));
            if task.first.is_none() {
                task.first = ["imported", "restored"].iter().find_map(|k| v[*k].as_array().and_then(|a| a.first()).and_then(Value::as_u64));
            }
        }
        Err(e) => {
            log::warn!("import: {e}");
            task.failed += n;
        }
    }
}

/// The import is done (or cancelled): one undo step, show and select the first photo (unless the
/// user chose another meanwhile), say what happened.
fn finish(app: &mut LccApp, ctx: &egui::Context, task: ImportTask) {
    // a cancelled Auto Import: the files it never read are tried again by the next scans (#1302)
    if task.auto && task.cancelled {
        app.session.auto_import_retry(&task.auto_paths);
    }
    let steps = app.session.undo.len().saturating_sub(task.undo0);
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let label = if task.imported == 0 && task.restored > 0 {
        crate::i18n::tr_format!("Restore {} Photo{}", task.restored, plural(task.restored))
    } else if task.restored > 0 {
        crate::i18n::tr_format!("Add {} Photo{}, restore {}", task.imported, plural(task.imported), task.restored)
    } else {
        crate::i18n::tr_format!("Add {} Photo{}", task.imported, if task.imported == 1 { "" } else { "s" })
    };
    app.session.merge_undo(steps, &label);
    // the Import tab's: the new photos' previews, built in the background (one activity row)
    if let Some(previews) = task.finish_previews.as_deref()
        && !task.ids.is_empty()
        && let Err(e) = app.session.finish_import(&task.ids, previews)
    {
        log::warn!("import: finishing: {e}");
    }
    if task.auto {
        if !task.warnings.is_empty() {
            let line = crate::import_report::record(app, task.imported + task.restored, &task.warnings, false);
            app.toast_for(
                ctx,
                format!("{} {line} · {}", crate::i18n::tr("Auto Import:"), crate::i18n::tr("File ▸ More Imports ▸ Import Warnings…")),
                8.0,
            );
        } else if task.imported > 0 {
            let msg = if task.camera_roll {
                crate::i18n::tr_format!("{n} new photo{} from the camera roll", plural(task.imported), n = task.imported)
            } else {
                crate::i18n::tr_format!("Auto Import: added {} photo{}", task.imported, plural(task.imported))
            };
            app.toast(ctx, msg);
        }
        return;
    }
    if task.browse {
        if !task.warnings.is_empty() {
            let line = crate::import_report::record(app, task.imported + task.restored, &task.warnings, false);
            app.toast_for(ctx, format!("{line} · {}", crate::i18n::tr("File ▸ More Imports ▸ Import Warnings…")), 8.0);
        } else if task.failed > 0 {
            app.toast(ctx, crate::i18n::tr_format!("{} photo{} not readable", task.failed, if task.failed == 1 { "" } else { "s" }));
        }
        return;
    }
    if let Some(f) = task.first.filter(|_| !task.moved_on) {
        // a view the new photos aren't in (an album, Picks, a Local folder) would leave the active
        // photo off screen: show All Photos, as Lightroom shows the import's own collection (#1446)
        if !app.session.visible().contains(&lcc_catalog::PhotoId(f)) {
            let _ = app.run("library.source", json!({"kind": "all"}));
        }
        let _ = app.run("library.select", json!({"ids": [f]}));
    }
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    // nothing new, only files the library already has: show where they are (Recently Deleted is
    // easy to miss, and the side panel that lists it starts collapsed)
    if task.imported == 0
        && task.restored == 0
        && task.failed == 0
        && task.warnings.is_empty()
        && !task.cancelled
        && show_existing(app, ctx, &task.existing)
    {
        return;
    }
    let mut msg = if task.cancelled {
        crate::i18n::tr_format!("Import cancelled · {} photo{} added", task.imported, plural(task.imported))
    } else if task.imported == 0 && task.restored > 0 {
        crate::i18n::tr_format!("Restored {} photo{} from Recently Deleted", task.restored, plural(task.restored))
    } else {
        crate::i18n::tr_format!("Imported {} photo{}", task.imported, plural(task.imported))
    };
    if task.restored > 0 && (task.imported > 0 || task.cancelled) {
        msg.push_str(&crate::i18n::tr_format!(" · {} restored from Recently Deleted", task.restored));
    }
    if task.params["mode"] == "move" {
        msg.push_str(&crate::i18n::tr_format!(" · {} moved", task.moved));
        if task.kept > 0 {
            msg.push_str(&crate::i18n::tr_format!(" · {} original{} left at the source", task.kept, plural(task.kept)));
        }
    }
    if task.duplicates > 0 {
        msg.push_str(&crate::i18n::tr_format!(" · {} duplicate{} skipped", task.duplicates, if task.duplicates == 1 { "" } else { "s" }));
        let deleted = deleted_of(app, &task.existing).len();
        if deleted > 0 {
            msg.push_str(&crate::i18n::tr_format!(" ({} in Recently Deleted)", deleted));
        }
    }
    if task.failed > 0 {
        msg.push_str(&crate::i18n::tr_format!(" · {} not readable", task.failed));
    }
    if !task.warnings.is_empty() {
        // the details open in their own window; the toast stays one line
        let line = crate::import_report::record(app, task.imported + task.restored, &task.warnings, true);
        msg.push_str(" · ");
        msg.push_str(&line);
    }
    app.toast(ctx, msg);
}

/// The photos among `ids` that are in Recently Deleted (each once).
fn deleted_of(app: &LccApp, ids: &[u64]) -> Vec<u64> {
    let mut v: Vec<u64> = ids.iter().copied().filter(|&id| app.session.catalog.photo(lcc_catalog::PhotoId(id)).is_some_and(|p| p.deleted)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Select the library photos an import skipped as duplicates of, in Recently Deleted (side panel
/// opened, with how to get them back) or in All Photos, and say so. `false` when there are none.
fn show_existing(app: &mut LccApp, ctx: &egui::Context, existing: &[u64]) -> bool {
    let deleted = deleted_of(app, existing);
    let (kind, mut ids) = if deleted.is_empty() { ("all", existing.to_vec()) } else { ("recentlyDeleted", deleted) };
    ids.sort_unstable();
    ids.dedup();
    ids.retain(|&id| app.session.catalog.photo(lcc_catalog::PhotoId(id)).is_some());
    if ids.is_empty() || app.run("library.source", json!({"kind": kind})).is_err() {
        return false;
    }
    let _ = app.run("library.select", json!({"ids": ids}));
    app.ui.left_panel = true;
    let n = ids.len();
    let plural = if n == 1 { "" } else { "s" };
    let msg = if kind == "all" {
        crate::i18n::tr_format!("Already in the catalog: {} photo{} selected in All Photos", n, plural)
    } else {
        crate::i18n::tr_format!(
            "Already in the catalog, in Recently Deleted: {} photo{} selected · right-click ▸ Restore, or Delete Permanently to import afresh",
            n,
            plural
        )
    };
    app.toast_for(ctx, msg, 6.0);
    true
}

/// Skipped candidates: (already in the library, identical copies within the source, in Recently
/// Deleted). Files of photos in Recently Deleted are not "already in the library", and a file with
/// the same bytes as one earlier in the list (no library photo) is a copy in the source.
pub(crate) fn dup_counts(d: &ImportDialog) -> (usize, usize, usize) {
    let trashed = d.trashed.iter().filter(|t| **t).count();
    let in_source = d.candidates.iter().filter(|c| c.duplicate.as_deref() == Some("content") && c.existing.is_none()).count();
    // (a browsed photo isn't offered while copying: it counts as already there)
    let browsed = if d.copy { (0..d.candidates.len()).filter(|i| d.is_browsed(*i)).count() } else { 0 };
    let dups = (d.candidates.iter().filter(|c| c.duplicate.is_some()).count() + browsed).saturating_sub(trashed).saturating_sub(in_source);
    (dups, in_source, trashed)
}

/// The one line that says what the scan left out, for the review's header: a folder walk that
/// stopped at its limit, folders it couldn't read, files that are not a supported photo type.
/// `None` when it left nothing out (the review then shows no line at all).
pub(crate) fn scan_note(d: &ImportDialog) -> Option<String> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut parts: Vec<String> = Vec::new();
    if d.scan_limited {
        parts.push(crate::i18n::tr("Stopped at the scan limit: there may be more photos").to_string());
    }
    if d.unreadable > 0 {
        parts.push(crate::i18n::tr_format!("{n} folder{} could not be read", plural(d.unreadable), n = d.unreadable));
    }
    if d.unsupported > 0 {
        parts.push(crate::i18n::tr_format!("{n} file{} skipped (not a supported photo type)", plural(d.unsupported), n = d.unsupported));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The dialog body: options, then the candidate grid.
pub fn body(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    let t = Tokens::get(ui.ctx());
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the source: files scanned from disk, or a connected Immich server
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new(crate::i18n::tr("Import from")).color(t.text_label));
            if crate::widgets::text_button(ui, "importSource:files", crate::i18n::tr("Files"), !d.immich).clicked() {
                d.immich = false;
            }
            if crate::widgets::text_button(ui, "importSource:immich", crate::i18n::tr("Immich"), d.immich).clicked() && !d.immich {
                d.immich = true;
                crate::panels::connections::open_source(app, None);
            }
        });
        if d.immich {
            crate::panels::connections::import_source(app, ui);
            return;
        }
        if d.candidates.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No files chosen yet.")).color(t.text_dim));
            if crate::widgets::text_button(ui, "importChooseFiles", crate::i18n::tr("Choose Files…"), false).clicked() {
                app.ui.dialog = None;
                let _ = app.run("file.addPhotos", json!({}));
            }
            return;
        }
    }
    let n = d.candidates.len();
    let (dups, in_source, trashed) = dup_counts(d);
    let sel = d.selected_paths().len();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} found · {sel} selected", n = n, sel = sel)).color(t.text));
        if dups > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {dups} already in the catalog", dups = dups)).color(t.text_dim));
        }
        if in_source > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {in_source} identical copies", in_source = in_source)).color(t.text_dim))
                .on_hover_text(crate::i18n::tr("Files with the same content as another file in this import: only the first is imported."));
        }
        if trashed > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {trashed} in Recently Deleted", trashed = trashed)).color(t.text_dim));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::text_button(ui, "importNone", crate::i18n::tr("Uncheck All"), false).clicked() {
                d.checked.iter_mut().for_each(|c| *c = false);
            }
            if crate::widgets::text_button(ui, "importAll", crate::i18n::tr("Check All"), false).clicked() {
                for i in 0..n {
                    d.checked[i] = d.checked_by_default(i);
                }
            }
        });
    });
    // what the scan left out, when it left something out
    if let Some(note) = scan_note(d) {
        let r = ui.label(egui::RichText::new(note).color(t.caution).small());
        register(ui.ctx(), "label:importScanNote", r.rect);
    }
    // with more than one file type: a toggle per type checks or unchecks all of its files
    let types = d.file_types();
    if types.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("File types:")).color(t.text_dim));
            for (ty, count) in &types {
                let on = d.type_checked(ty);
                let r = crate::widgets::text_button(ui, &format!("importType:{ty}"), &format!("{ty} · {count}"), on)
                    .on_hover_text(crate::i18n::tr(if on { "Uncheck the files of this type" } else { "Check the files of this type" }));
                if r.clicked() {
                    d.check_type(ty, !on);
                }
            }
        });
    }
    // candidate grid
    let cell = 116.0;
    let avail = ui.available_width();
    let cols = ((avail + 6.0) / (cell + 6.0)).floor().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    // as tall as the window leaves after the options and buttons below it (measured last frame),
    // so resizing the dialog resizes the grid
    let below = ui.ctx().data(|m| {
        let y = |key: &str| m.get_temp::<f32>(egui::Id::new(key));
        y(CONTENT_BOTTOM).zip(y(GRID_BOTTOM)).map(|(content, grid)| content - grid)
    });
    let grid_h = grid_height(ui.available_height(), below.unwrap_or(DEFAULT_BELOW_GRID), cell + 26.0);
    let grid =
        egui::ScrollArea::vertical().id_salt("import-grid").max_height(grid_h).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (cell + 26.0)), Sense::hover());
            for i in 0..n {
                let (c, r) = (i % cols, i / cols);
                let local = Rect::from_min_size(pos2(c as f32 * (cell + 6.0), r as f32 * (cell + 26.0)), vec2(cell, cell + 20.0));
                if !local.intersects(viewport.expand(cell)) {
                    continue;
                }
                let rect = local.translate(area.min.to_vec2());
                candidate_cell(app, ui, d, i, rect);
            }
        });
    if !ui.is_sizing_pass() {
        ui.ctx().data_mut(|m| m.insert_temp(egui::Id::new(GRID_BOTTOM), grid.inner_rect.bottom()));
    }
    ui.add_space(4.0);
    // options
    if !d.sources.is_empty() {
        field(ui, "Source", |ui| {
            // (whether each source is a folder is checked off the UI thread, once)
            let joined = d.sources.join("\n");
            let summary = crate::panels::left::fs_cached(ui, "import-source", &joined, f64::INFINITY, |s| {
                inspect_sources(&s.split('\n').map(str::to_string).collect::<Vec<_>>())
            })
            .map(|summary| summary.display(d.subfolders))
            .unwrap_or_else(|| crate::i18n::tr_format!("{} item{}", d.sources.len(), if d.sources.len() == 1 { "" } else { "s" }));
            let r = ui.label(egui::RichText::new(summary).color(t.text_label)).on_hover_text(joined);
            register(ui.ctx(), "label:importSource", r.rect);
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr("Importing scans the source for photos; it doesn't add the folder to Favorites."))
                .color(t.text_dim)
                .small(),
        );
    }
    field(ui, "Transfer", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if crate::widgets::text_button(ui, "importAdd", crate::i18n::tr("Add in place"), !d.copy)
            .on_hover_text(crate::i18n::tr("Reference the files where they are"))
            .clicked()
        {
            d.copy = false;
            d.move_files = false;
        }
        let can_copy = app.session.library.as_ref().is_some_and(|l| l.on_disk) || app.services.pick_folder.is_some();
        let r =
            ui.add_enabled_ui(can_copy, |ui| crate::widgets::text_button(ui, "importCopy", crate::i18n::tr("Copy"), d.copy && !d.move_files)).inner;
        if r.on_hover_text(crate::i18n::tr("Copy the files (into the catalog's Originals/, or a folder you choose)")).clicked() {
            d.copy = true;
            d.move_files = false;
        }
        let r =
            ui.add_enabled_ui(can_copy, |ui| crate::widgets::text_button(ui, "importMove", crate::i18n::tr("Move"), d.copy && d.move_files)).inner;
        if r.on_hover_text(crate::i18n::tr("Move the files (into the catalog's Originals/, or a folder you choose), removing them from the source"))
            .clicked()
        {
            d.copy = true;
            d.move_files = true;
        }
    });
    let r = ui.label(
        egui::RichText::new(crate::i18n::tr(match (d.copy, d.move_files) {
            (true, false) => {
                "Copy: the files are copied to the folder below and the catalog uses the copies; the originals are left as they are."
            }
            (true, true) => {
                "Move: the files are moved to the folder below; each original (and its XMP sidecar) is removed from the source only after its copy is verified. Duplicates and files that fail stay where they are."
            }
            _ => "Add in place: the catalog references the files where they are; nothing is copied or moved.",
        }))
        .color(if d.copy && d.move_files { t.caution } else { t.text_dim })
        .small(),
    );
    register(ui.ctx(), "label:importModeHelp", r.rect);
    if d.copy {
        field(ui, if d.move_files { "Move to" } else { "Copy to" }, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let shown = if d.destination.trim().is_empty() { crate::i18n::tr("Catalog Originals").to_string() } else { d.destination.clone() };
            ui.label(egui::RichText::new(shown).color(Tokens::get(ui.ctx()).text_label));
            if app.services.pick_folder.is_some()
                && crate::widgets::text_button(ui, "importDest", crate::i18n::tr("Choose…"), false).clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                d.destination = f;
            }
            if !d.destination.is_empty() && crate::widgets::text_button(ui, "importDestReset", crate::i18n::tr("Library"), false).clicked() {
                d.destination.clear();
            }
        });
        field(ui, "Folders", |ui| {
            let opts = [
                ("date", "By day (YYYY/YYYY-MM-DD)"),
                ("month", "By month (YYYY/YYYY-MM)"),
                ("flat", "Into one folder"),
                ("custom", "Custom template…"),
            ];
            let key = if d.organize.is_empty() { "date".to_string() } else { d.organize.clone() };
            let cur = opts.iter().find(|o| o.0 == key).map_or(opts[0].1, |o| o.1);
            let combo = crate::widgets::combo_box("import-organize").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (k, label) in opts {
                    let r = ui.selectable_label(key == k, crate::i18n::tr(label));
                    register(ui.ctx(), format!("button:importOrganize-{k}"), r.rect);
                    if r.clicked() {
                        d.organize = k.to_string();
                        if k == "custom" && d.folder_template.trim().is_empty() {
                            d.folder_template = DEFAULT_FOLDER_TEMPLATE.into();
                        }
                    }
                }
            });
            register(ui.ctx(), "combo:importOrganize", combo.response.rect);
        });
        if d.organize == "custom" {
            let folders_id = egui::Id::new("import-folder-template");
            let tags_open = field(ui, "Template", |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = tag_field_width(ui);
                let r = ui.add(egui::TextEdit::singleline(&mut d.folder_template).id(folders_id).hint_text(DEFAULT_FOLDER_TEMPLATE).desired_width(w));
                register(ui.ctx(), "field:importFolderTemplate", r.rect);
                tag_toggle(ui, "importFolders")
            });
            if tags_open {
                tag_help(ui, "importFolders", &mut d.folder_template, folders_id);
            }
            let t = Tokens::get(ui.ctx());
            match lcc_engine::rename::folder_template_error(&d.folder_template) {
                Some(e) => {
                    ui.label(egui::RichText::new(e).color(t.caution));
                }
                None => unknown_tags_warning(ui, &d.folder_template),
            }
            ui.label(
                egui::RichText::new(crate::i18n::tr(
                    "Each / starts a folder level; tags are filled in per photo (a level with missing metadata is \"unknown\").",
                ))
                .color(t.text_dim)
                .small(),
            );
        }
        if !d.move_files {
            field(ui, "Raw files", |ui| {
                let r = ui.checkbox(&mut d.dng, crate::i18n::tr("Copy as DNG"));
                register(ui.ctx(), "check:importDng", r.rect);
            });
        }
        let rename_id = egui::Id::new("import-rename");
        let tags_open = field(ui, "Rename", |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = tag_field_width(ui);
            let r = ui.add(
                egui::TextEdit::singleline(&mut d.rename)
                    .id(rename_id)
                    .hint_text(crate::i18n::tr("keep names, or e.g. {date}_{seq:3}"))
                    .desired_width(w),
            );
            register(ui.ctx(), "field:importRename", r.rect);
            tag_toggle(ui, "importRename")
        });
        if tags_open {
            tag_help(ui, "importRename", &mut d.rename, rename_id);
        }
        unknown_tags_warning(ui, &d.rename);
        if let Some(example) = example_destination(app, d) {
            let t = Tokens::get(ui.ctx());
            let r = ui.label(egui::RichText::new(crate::i18n::tr_format!("Example: {example}", example = example)).color(t.text_dim));
            register(ui.ctx(), "label:importExample", r.rect);
            ui.label(
                egui::RichText::new(crate::i18n::tr("Folders and {date} use the capture time; a photo without one uses today's date."))
                    .color(t.text_dim)
                    .small(),
            );
        }
    }
    field(ui, "Duplicates", |ui| {
        let mut skip = !d.import_duplicates;
        let r = ui.checkbox(&mut skip, crate::i18n::tr("Don't Import Suspected Duplicates"));
        register(ui.ctx(), "check:importSkipDuplicates", r.rect);
        if r.changed() {
            d.set_skip_duplicates(skip);
        }
    });
    if d.options_skip_browsed() {
        let t = Tokens::get(ui.ctx());
        ui.label(
            egui::RichText::new(crate::i18n::tr("Keywords and presets apply to new files only, not to photos already in Local"))
                .color(t.text_dim)
                .small(),
        );
    }
    if d.trashed.iter().any(|t| *t) {
        field(ui, "In Recently Deleted", |ui| {
            let cur = match d.on_deleted.as_str() {
                "restore" => "Restore them",
                "fresh" => "Import as new",
                _ => "Leave them",
            };
            let mut pick = None;
            crate::widgets::combo_box("import-on-deleted").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (v, label) in [("restore", "Restore them"), ("fresh", "Import as new"), ("", "Leave them")] {
                    if ui.selectable_label(d.on_deleted == v, crate::i18n::tr(label)).clicked() {
                        pick = Some(v);
                    }
                }
            });
            if let Some(v) = pick {
                d.set_on_deleted(v);
            }
            let t = Tokens::get(ui.ctx());
            ui.label(
                egui::RichText::new(crate::i18n::tr("Restore keeps their edits; Import as new deletes the old record first."))
                    .color(t.text_dim)
                    .small(),
            );
        });
    }
    let mut albums = crate::album_picker::entries_from(&app.session.catalog, |_| None);
    albums.retain(|a| a.folder || !a.smart);
    // Catalog changes while the review is open must not leave a hidden, unselectable destination.
    d.album = d.album.filter(|id| albums.iter().any(|a| a.id == *id && !a.folder));
    field(ui, "Collection", |ui| {
        ui.vertical(|ui| {
            let mut chosen = d.album;
            let mut extra = None;
            let empty = crate::i18n::tr(if d.new_album.is_empty() { "None" } else { "New collection" });
            let response = crate::album_picker::AlbumPicker::new("import-album", &mut chosen, &albums)
                .width(ui.available_width())
                .empty_text(empty)
                .show_with_choices(ui, |ui| {
                    let none = ui.selectable_label(d.album.is_none() && d.new_album.is_empty(), crate::i18n::tr("None"));
                    register(ui.ctx(), "importAlbum:none", none.rect);
                    if none.clicked() {
                        extra = Some(false);
                    }
                    let new = ui.selectable_label(d.album.is_none() && !d.new_album.is_empty(), crate::i18n::tr("New collection…"));
                    register(ui.ctx(), "importAlbum:new", new.rect);
                    if new.clicked() {
                        extra = Some(true);
                    }
                    ui.separator();
                    extra.is_some()
                });
            if response.changed() {
                d.album = chosen;
                d.new_album.clear();
            }
            if let Some(new) = extra {
                d.album = None;
                if !new {
                    d.new_album.clear();
                } else if d.new_album.is_empty() {
                    d.new_album = crate::i18n::tr("Imported Photos").into();
                }
            }
            if d.album.is_none() && !d.new_album.is_empty() {
                let r = ui.add(egui::TextEdit::singleline(&mut d.new_album).desired_width(f32::INFINITY));
                register(ui.ctx(), "field:importAlbumName", r.rect);
            }
        });
    });
    preset_stack_section(app, ui, d, false);
    camera_profile_section(app, ui, d, false);
    field(ui, "Metadata", |ui| metadata_preset_picker(app, ui, d));
    field(ui, "Keywords", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.keywords).hint_text(crate::i18n::tr("comma, separated")).desired_width(f32::INFINITY));
        register(ui.ctx(), "field:importKeywords", r.rect);
    });
    recipe_section(app, ui, d);
}

/// The develop controls offered in the import review's optional batch-edit recipe: the everyday
/// adjustments, not a second editor. Each must be one a recipe may name (`library.import` refuses
/// the others: no white balance, geometry or crop); a test holds the list to that. Anything else is
/// reached by choosing or saving a preset.
const RECIPE_CONTROLS: &[&str] = &[
    "light.exposure",
    "light.contrast",
    "light.highlights",
    "light.shadows",
    "light.whites",
    "light.blacks",
    "color.vibrance",
    "color.saturation",
    "effects.clarity",
    "effects.texture",
    "effects.dehaze",
    "detail.sharpenAmount",
    "grain.amount",
    "vignette.amount",
];

/// A recipe as the `recipe` param of `library.import` and `preset.saveRecipe`.
fn recipe_items(recipe: &std::collections::BTreeMap<String, f64>) -> Value {
    Value::Array(recipe.iter().map(|(id, v)| json!({"id": id, "value": v})).collect())
}

/// Record a slider's new value in the recipe: a control back at its default is no longer named.
fn note_recipe_value(recipe: &mut std::collections::BTreeMap<String, f64>, spec: &lcc_develop::controls::ControlSpec, value: f64) {
    if (value - spec.default).abs() < 1e-9 {
        recipe.remove(spec.id);
    } else {
        recipe.insert(spec.id.to_string(), value);
    }
}

/// The optional "Batch edits" section of the import review: a small recipe of develop controls set
/// on every photo this import adds, on top of its XMP sidecar, camera default and the chosen preset.
/// The recipe lives only in the dialog until the import runs, so cancelling changes neither the
/// catalog nor the preset collections; "Save as Preset" is the explicit way to keep one for reuse.
pub(crate) fn recipe_section(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    let t = Tokens::get(ui.ctx());
    let open_id = egui::Id::new("import-recipe-open");
    let mut open = ui.ctx().data(|m| m.get_temp::<bool>(open_id)).unwrap_or(false);
    field(ui, "Batch edits", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if crate::widgets::text_button(ui, "importRecipeToggle", if open { "Hide adjustments" } else { "Adjust…" }, open).clicked() {
            open = !open;
        }
        let n = d.recipe.len();
        let summary = if n == 0 { crate::i18n::tr("None").to_string() } else { crate::i18n::tr_format!("Adjustments: {n}", n = n) };
        ui.label(egui::RichText::new(summary).color(if n == 0 { t.text_dim } else { t.text_label }));
    });
    ui.ctx().data_mut(|m| m.insert_temp(open_id, open));
    if !open {
        return;
    }
    ui.label(
        egui::RichText::new(crate::i18n::tr(
            "Set on every photo this import adds, after its XMP sidecar and the preset above; photos the catalog already has are left alone. A slider left at its default changes nothing. Nothing is saved unless you save it as a preset.",
        ))
        .color(t.text_dim)
        .small(),
    );
    for &id in RECIPE_CONTROLS {
        let Some(spec) = lcc_develop::controls::find(id) else { continue };
        let v = d.recipe.get(id).copied().unwrap_or(spec.default);
        if let Some(nv) = crate::widgets::slider(ui, spec, v, true, None).value {
            note_recipe_value(&mut d.recipe, spec, nv);
        }
    }
    field(ui, "Recipe", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        crate::text_field::TextField::singleline("field:importRecipeName", &mut d.recipe_name)
            .hint(crate::i18n::tr("name for a saved preset"))
            .width((ui.available_width() - 110.0).clamp(80.0, 160.0))
            .show(ui);
        let can = !d.recipe.is_empty() && !d.recipe_name.trim().is_empty();
        let b = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Save as Preset")));
        register(ui.ctx(), "button:importRecipeSave", b.rect);
        if b.clicked() {
            let params = json!({"name": d.recipe_name.trim(), "recipe": recipe_items(&d.recipe)});
            match app.run("preset.saveRecipe", params) {
                Ok(_) => app.toast(ui.ctx(), crate::i18n::tr("Saved as a preset")),
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
    });
}

/// The metadata preset to apply on import: None, the saved presets, New Preset… (with the inline
/// name, creator and copyright fields that save one) and Import Presets…. Shared by the Import
/// screen and the review's form.
pub(crate) fn metadata_preset_picker(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let cur = if d.creating_meta_preset {
                crate::i18n::tr("New preset…").to_string()
            } else if d.metadata_preset.is_empty() {
                crate::i18n::tr("None").to_string()
            } else {
                d.metadata_preset.clone()
            };
            let combo = crate::widgets::combo_box("import-metadata").selected_text(cur).height(300.0).show_ui(ui, |ui| {
                let none_btn = ui.selectable_label(!d.creating_meta_preset && d.metadata_preset.is_empty(), crate::i18n::tr("None"));
                register(ui.ctx(), "button:importMetaNone", none_btn.rect);
                if none_btn.clicked() {
                    d.metadata_preset.clear();
                    d.creating_meta_preset = false;
                }
                for m in &app.session.metadata_presets {
                    let r = ui.selectable_label(!d.creating_meta_preset && d.metadata_preset == m.name, &m.name);
                    register(ui.ctx(), format!("button:importMetaPreset-{}", m.name), r.rect);
                    if r.clicked() {
                        d.metadata_preset = m.name.clone();
                        d.creating_meta_preset = false;
                    }
                }
                ui.separator();
                let new_btn = ui.selectable_label(d.creating_meta_preset, crate::i18n::tr("New Preset…"));
                register(ui.ctx(), "button:importMetaNewPreset", new_btn.rect);
                if new_btn.clicked() {
                    d.creating_meta_preset = true;
                    if d.new_meta_creator.is_empty() {
                        d.new_meta_creator = app.session.import_defaults.creator.clone();
                    }
                    if d.new_meta_copyright.is_empty() {
                        d.new_meta_copyright = app.session.import_defaults.copyright.clone();
                    }
                }
                let import_btn = ui.button(crate::i18n::tr("Import Presets…"));
                register(ui.ctx(), "button:importMetaImportPresets", import_btn.rect);
                if import_btn.clicked() {
                    let _ = app.run("file.importPresets", json!({}));
                }
            });
            register(ui.ctx(), "combo:importMetadata", combo.response.rect);
        });
        if d.creating_meta_preset {
            ui.add_space(3.0);
            let t = Tokens::get(ui.ctx());
            egui::Frame::new().fill(t.inset).inner_margin(egui::Margin::symmetric(8, 6)).corner_radius(4.0).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(crate::i18n::tr("Preset Name:")).color(t.text_dim).small());
                        let r =
                            ui.add(egui::TextEdit::singleline(&mut d.new_meta_name).hint_text(crate::i18n::tr("Preset name")).desired_width(180.0));
                        register(ui.ctx(), "field:importMetaPresetName", r.rect);
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(crate::i18n::tr("Creator:")).color(t.text_dim).small());
                        let r = ui
                            .add(egui::TextEdit::singleline(&mut d.new_meta_creator).hint_text(crate::i18n::tr("Creator name")).desired_width(180.0));
                        register(ui.ctx(), "field:importMetaPresetCreator", r.rect);
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(crate::i18n::tr("Copyright:")).color(t.text_dim).small());
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut d.new_meta_copyright).hint_text(crate::i18n::tr("© Year Name")).desired_width(180.0),
                        );
                        register(ui.ctx(), "field:importMetaPresetCopyright", r.rect);
                    });
                    ui.horizontal(|ui| {
                        let can_save = !d.new_meta_name.trim().is_empty();
                        let save_btn = ui.add_enabled(can_save, egui::Button::new(crate::i18n::tr("Save Preset")));
                        register(ui.ctx(), "button:importMetaPresetSave", save_btn.rect);
                        if save_btn.clicked() {
                            let name = d.new_meta_name.trim().to_string();
                            let mut fields = serde_json::Map::new();
                            if !d.new_meta_creator.trim().is_empty() {
                                fields.insert("creator".into(), json!(d.new_meta_creator.trim()));
                            }
                            if !d.new_meta_copyright.trim().is_empty() {
                                fields.insert("copyright".into(), json!(d.new_meta_copyright.trim()));
                            }
                            let _ = app.session.execute(
                                "metadata.savePreset",
                                &json!({
                                    "name": name,
                                    "fields": fields,
                                }),
                            );
                            d.metadata_preset = name;
                            d.creating_meta_preset = false;
                        }
                        let cancel_btn = ui.button(crate::i18n::tr("Cancel"));
                        register(ui.ctx(), "button:importMetaPresetCancel", cancel_btn.rect);
                        if cancel_btn.clicked() {
                            d.creating_meta_preset = false;
                        }
                    });
                });
            });
        }
    });
}

pub(crate) fn candidate_cell(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, i: usize, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let c = d.candidates[i].clone();
    let ok = d.importable(i);
    let img = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
    let resp = ui.interact(img, egui::Id::new(("import-cell", i)), Sense::click());
    crate::widgets::announce(&resp, c.name.clone());
    register(ui.ctx(), format!("import:{i}"), img);
    let p = ui.painter();
    p.rect_filled(img, 3.0, t.skeleton);
    // thumbnail (background job; the grid only asks for the cells in view)
    let slot = Slot::Import(i as u32);
    if !app.renderer.textures.contains_key(&slot)
        && let Some(job) = app.session.candidate_thumb_job(&c, 192, i as u64)
    {
        app.renderer.request_quick(slot, job, 4);
    }
    if let Some(tex) = app.renderer.textures.get(&slot) {
        let [tw, th] = tex.size;
        let s = ((img.width() - 8.0) / tw as f32).min((img.height() - 8.0) / th as f32);
        let fit = Rect::from_center_size(img.center(), vec2(tw as f32 * s, th as f32 * s));
        let tint = if ok { Color32::WHITE } else { Color32::from_gray(110) };
        p.image(tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
    }
    let on = d.checked[i] && ok;
    if on {
        p.rect_stroke(img, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    }
    // checkbox
    let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(16.0, 16.0));
    p.rect(cb, 3.0, if on { t.accent } else { Color32::from_black_alpha(150) }, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Inside);
    if on {
        p.line_segment([cb.left_center() + vec2(3.5, 0.5), cb.center_bottom() + vec2(-1.0, -4.0)], Stroke::new(2.0, Color32::WHITE));
        p.line_segment([cb.center_bottom() + vec2(-1.0, -4.0), cb.right_top() + vec2(-3.5, 4.0)], Stroke::new(2.0, Color32::WHITE));
    }
    let in_trash = c.existing.is_some_and(|id| app.session.catalog.photo(lcc_catalog::PhotoId(id)).is_some_and(|p| p.deleted));
    let badge = match (&c.duplicate, &c.error) {
        // checked with "Leave them", the file goes without `onDeleted`: the engine restores it
        (Some(_), _) if in_trash && on => Some(if d.on_deleted == "fresh" { "Will be replaced" } else { "Will be restored" }),
        (Some(_), _) if in_trash => Some("In Recently Deleted"),
        (Some(r), _) if r == "path" => Some("In catalog"),
        (None, None) if d.copy && d.is_browsed(i) => Some("In catalog"),
        (Some(_), _) if on => Some("Will be copied"),
        (Some(_), _) => Some("Duplicate"),
        (None, Some(_)) => Some("Unreadable"),
        _ => None,
    };
    if let Some(b) = badge {
        let g = p.layout_no_wrap(crate::i18n::tr(b).to_string(), t.semibold(10.0), Color32::WHITE);
        let br = badge_rect(img, cb, g.size());
        p.rect_filled(br, 3.0, Color32::from_rgba_unmultiplied(170, 60, 50, 220));
        p.galley(br.min + vec2(4.0, 2.0), g, Color32::WHITE);
    }
    let name = if c.name.chars().count() > 18 { format!("{}…", c.name.chars().take(17).collect::<String>()) } else { c.name.clone() };
    p.text(pos2(rect.left() + 2.0, img.bottom() + 9.0), Align2::LEFT_CENTER, name, t.font(10.5), if ok { t.text_label } else { t.text_dim });
    let tip = format!(
        "{}\n{} × {} · {} · {:.1} MB{}",
        c.path,
        c.width,
        c.height,
        c.format,
        c.file_size as f64 / 1e6,
        // a year or a month alone shows as such ("1983", "April 1983")
        c.captured
            .as_deref()
            .map(|d| format!(
                "\n{}",
                if lcc_catalog::dates::partial_capture(d).is_some() { crate::i18n::display_time(d) } else { d.replace('T', " ") }
            ))
            .unwrap_or_default()
    );
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && ok {
        let shift = ui.input(|input| input.modifiers.shift);
        d.click(i, shift);
    }
}

/// Where a cell's badge goes: top right, or under the checkbox when the text is too wide to sit
/// beside it — drawn over the checkbox it hid it (issue #1285).
fn badge_rect(img: Rect, cb: Rect, text: Vec2) -> Rect {
    let size = text + vec2(8.0, 4.0);
    let left = img.right() - size.x - 4.0;
    let top = if left < cb.right() + 4.0 { cb.bottom() + 4.0 } else { img.top() + 6.0 };
    Rect::from_min_size(pos2(left, top), size)
}

/// Reserve the actual translated tag button width beside a template field.
pub(crate) fn tag_field_width(ui: &egui::Ui) -> f32 {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(crate::i18n::tr("Tags").to_string(), t.semibold(11.5), t.text);
    (ui.available_width() - (galley.size().x + 18.0).max(37.0) - ui.spacing().item_spacing.x).max(80.0)
}

/// The "Tags" toggle beside a template field (`button:<key>Tags`); returns whether the tag help
/// ([`tag_help`]) is open.
pub(crate) fn tag_toggle(ui: &mut egui::Ui, key: &str) -> bool {
    let id = egui::Id::new((key, "tags-open"));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let r = crate::widgets::text_button(ui, &format!("{key}Tags"), crate::i18n::tr("Tags"), open)
        .on_hover_text(crate::i18n::tr("Show the template tags; click one to insert it"));
    if r.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
    }
    open
}

/// "Unknown tag {x} stays as typed" under a template field, when it has one.
pub(crate) fn unknown_tags_warning(ui: &mut egui::Ui, template: &str) {
    let unknown = lcc_engine::rename::unknown_tokens(template);
    if !unknown.is_empty() {
        let s = if unknown.len() == 1 { "" } else { "s" };
        let text = crate::i18n::tr_format!(
            "Unknown tag{s} {} stay{} as typed (see Tags)",
            unknown.join(" "),
            if unknown.len() == 1 { "s" } else { "" },
            s = s
        );
        ui.label(egui::RichText::new(text).color(Tokens::get(ui.ctx()).caution));
    }
}

/// Insert `tag` into `text` at the text cursor of the field `edit_id` (replacing its selection);
/// appended when the field never had a cursor. The cursor ends up after the tag.
pub(crate) fn insert_at_cursor(ctx: &egui::Context, edit_id: egui::Id, text: &mut String, tag: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::text_edit::TextEditState::load(ctx, edit_id).unwrap_or_default();
    let n = text.chars().count();
    let (a, b) = state.cursor.char_range().map_or((n, n), |r| {
        let (x, y) = (r.primary.index.0.min(n), r.secondary.index.0.min(n));
        (x.min(y), x.max(y))
    });
    let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
    let (ba, bb) = (byte(a), byte(b));
    text.replace_range(ba..bb, tag);
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + tag.chars().count()))));
    state.store(ctx, edit_id);
    ctx.memory_mut(|m| m.request_focus(edit_id));
}

/// The template tag help: every tag (from the engine's one list, `rename::TOKENS`) with its
/// meaning and an example, the `{date:…}` directives and how templates behave. Clicking a tag
/// (`button:<key>Tag-<i>`) inserts it at the cursor of the field `edit_id`.
pub(crate) fn tag_help(ui: &mut egui::Ui, key: &str, text: &mut String, edit_id: egui::Id) {
    use lcc_engine::rename::{DATE_DIRECTIVES, TEMPLATE_NOTES, TOKENS, token_example};
    let t = Tokens::get(ui.ctx());
    egui::Frame::new().fill(t.inset).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
        ui.label(
            egui::RichText::new(crate::i18n::tr("Click a tag to insert it. Examples are for IMG_0042.CR3 taken 14 Jan 2026, 05:58:48."))
                .color(t.text_dim)
                .small(),
        );
        egui::ScrollArea::vertical().id_salt((key, "tags")).max_height(170.0).show(ui, |ui| {
            egui::Grid::new((key, "tag-grid")).num_columns(3).spacing([10.0, 2.0]).show(ui, |ui| {
                for (i, tok) in TOKENS.iter().enumerate() {
                    let r = ui.add(egui::Button::new(egui::RichText::new(tok.tag).monospace().color(t.text)).small()).on_hover_text(
                        if tok.aliases.is_empty() {
                            crate::i18n::tr("Insert at the cursor").to_string()
                        } else {
                            crate::i18n::tr_format!("Insert at the cursor (also written {})", tok.aliases.join(", "))
                        },
                    );
                    register(ui.ctx(), format!("button:{key}Tag-{i}"), r.rect);
                    if r.clicked() {
                        insert_at_cursor(ui.ctx(), edit_id, text, tok.tag);
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr(tok.meaning)).color(t.text_label));
                    ui.label(egui::RichText::new(token_example(tok.tag)).monospace().color(t.text_dim));
                    ui.end_row();
                }
            });
            let directives: Vec<String> = DATE_DIRECTIVES.iter().map(|(d, m)| format!("{d} {}", crate::i18n::tr(m))).collect();
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{{date:…}} directives: {}", directives.join(" · "))).color(t.text_dim).small());
            for n in TEMPLATE_NOTES {
                ui.label(egui::RichText::new(format!("• {}", crate::i18n::tr(n))).color(t.text_dim).small());
            }
        });
    });
}

/// The folder template the Custom choice starts with (`2026/20260114/`).
pub const DEFAULT_FOLDER_TEMPLATE: &str = "{date:%Y}/{date:%Y%m%d}";

/// Where the first selected photo would be copied to (destination, folders, name), for the
/// dialog's example line; `None` without a photo or with an unusable folder template.
pub fn example_destination(app: &LccApp, d: &ImportDialog) -> Option<String> {
    let c = d.candidates.iter().zip(&d.checked).enumerate().find(|(i, (_, on))| **on && d.importable(*i)).map(|(_, (c, _))| c)?;
    let organize = match d.organize_param().ok()? {
        Some(o) => lcc_engine::import::Organize::parse(&o)?,
        None => Default::default(),
    };
    let mut q = lcc_catalog::Photo::new(
        lcc_catalog::PhotoId(0),
        lcc_catalog::Source::File { path: c.path.clone() },
        &c.name,
        &c.format,
        0,
        0,
        // a photo without a capture time is dated by the import (now)
        &(app.session.clock)(),
    );
    q.captured = c.captured.clone();
    // the probe's metadata, so {camera}, {title}… preview as they will import
    if let Some(info) = app.session.import_probes.get(&c.path) {
        q.meta = info.meta.clone();
    }
    let name = if d.rename.trim().is_empty() { c.name.clone() } else { lcc_engine::rename::expand(d.rename.trim(), &q, 1) };
    let root = if d.destination.trim().is_empty() {
        app.session.library.as_ref().map_or_else(|| "Originals".to_string(), |l| l.dir.join("Originals").to_string_lossy().to_string())
    } else {
        d.destination.trim().trim_end_matches(['/', '\\']).to_string()
    };
    let sep = std::path::MAIN_SEPARATOR_STR;
    let mut parts = vec![root];
    parts.extend(organize.folders(&q));
    parts.push(name);
    Some(parts.join(sep))
}

/// The import source in a few words: a folder's name, a file's name, or "N files and folders".
pub fn source_summary(sources: &[String]) -> String {
    source_summary_with_subfolders(sources, true)
}

/// The import source in a few words, taking the `subfolders` toggle into account for folders.
pub fn source_summary_with_subfolders(sources: &[String], subfolders: bool) -> String {
    inspect_sources(sources).display(subfolders)
}

/// Facts cached by the filesystem worker, formatted in the UI thread's active language.
/// Caching a translated String on the worker would always use its default language (English)
/// and also retain that language after the user changes the UI language.
#[derive(Clone)]
enum SourceSummary {
    Folder(String),
    File(String),
    Many { files: usize, folders: usize },
}

impl SourceSummary {
    fn display(&self, subfolders: bool) -> String {
        match self {
            Self::Folder(name) => {
                if subfolders {
                    crate::i18n::tr_format!("Folder “{}” (and its subfolders)", name)
                } else {
                    crate::i18n::tr_format!("Folder “{}”", name)
                }
            }
            Self::File(name) => name.clone(),
            Self::Many { files, folders } => match (files, folders) {
                (_, 0) => crate::i18n::tr_format!("{} files", files),
                (0, f) => {
                    if subfolders {
                        crate::i18n::tr_format!("{f} folders (and their subfolders)", f = f)
                    } else {
                        crate::i18n::tr_format!("{f} folder{}", if *f == 1 { "" } else { "s" }, f = f)
                    }
                }
                (_, f) => crate::i18n::tr_format!("{} files and {f} folder{}", files, if *f == 1 { "" } else { "s" }, f = f),
            },
        }
    }
}

fn inspect_sources(sources: &[String]) -> SourceSummary {
    let name = |p: &str| std::path::Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().to_string());
    match sources {
        [one] if std::path::Path::new(one).is_dir() => SourceSummary::Folder(name(one)),
        [one] => SourceSummary::File(name(one)),
        many => {
            let folders = many.iter().filter(|p| std::path::Path::new(p.as_str()).is_dir()).count();
            SourceSummary::Many { files: many.len().saturating_sub(folders), folders }
        }
    }
}

/// A labelled row (fixed label column).
fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(78.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(78.0);
            ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

/// The camera profiles an import can give (id, label), for raws of `camera` (`None`: any camera).
fn camera_profile_options(camera: Option<&str>) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> =
        vec![(String::new(), crate::i18n::tr("Automatic").to_string()), ("lc".into(), crate::i18n::tr("Built-in colour").to_string())];
    for (pid, p) in lcc_raw::dcp::library() {
        if camera.is_some_and(|c| !lcc_raw::dcp::covers_model(&p.model, c)) {
            continue;
        }
        let name = if p.name.is_empty() { pid.trim_start_matches("dcp:").to_string() } else { p.name.clone() };
        options.push((pid, format!("{name} · {}", p.model)));
    }
    let cals: Vec<_> = match camera {
        Some(c) => lcc_raw::dcp::calibrated_for_camera(c).into_iter().collect(),
        None => lcc_raw::dcp::calibrated(),
    };
    for c in cals {
        options.extend(c.looks.iter().map(|(pid, p)| (pid.clone(), format!("{} · {}", p.name, p.model))));
    }
    options
}

/// A camera profile menu across the column, its list as wide as its longest name (within the
/// screen): a narrow menu cut "R5 C · 74 (4)"'s profiles to a few letters.
fn profile_combo(ui: &mut egui::Ui, id: &str, options: &[(String, String)], value: &mut String) {
    let cur = options.iter().find(|(o, _)| o == value).map(|(_, n)| n.clone()).unwrap_or_else(|| value.clone());
    let list_w = combo_list_width(ui, options.iter().map(|(_, n)| n.as_str()));
    let width = ui.available_width().max(120.0);
    let combo = crate::widgets::combo_box(id.to_string()).selected_text(cur).width(width).height(300.0).show_ui(ui, |ui| {
        ui.set_min_width(list_w);
        for (o, name) in options {
            ui.selectable_value(value, o.clone(), name);
        }
    });
    register(ui.ctx(), format!("combo:{id}"), combo.response.rect);
}

/// The width a drop-down's list needs to show the longest of `names` whole, within the screen.
pub(crate) fn combo_list_width<'a>(ui: &egui::Ui, names: impl Iterator<Item = &'a str>) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let t = Tokens::get(ui.ctx());
    let widest = names.map(|n| ui.painter().layout_no_wrap(n.to_string(), font.clone(), t.text).size().x).fold(0.0_f32, f32::max);
    let want = widest + ui.spacing().button_padding.x * 2.0 + ui.spacing().scroll.bar_width + 12.0;
    want.min((ui.ctx().content_rect().width() - 32.0).max(120.0))
}

/// Camera Profile: automatic (each body's own, else the camera's default) with a row per camera
/// body found on the card to choose another for this import, or one profile for all raws of its
/// camera (ported from OneSlips, LR-PROF-IMPORT-APPLY).
pub(crate) fn camera_profile_section(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, screen: bool) {
    // (the full-screen Import and the dialog lay rows out their own way)
    let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        if screen {
            crate::dialog_frame::form_row(ui, label, |ui| add(ui));
        } else {
            field(ui, label, |ui| add(ui));
        }
    };
    row(ui, "Camera Profile", &mut |ui| profile_combo(ui, "importCameraProfile", &camera_profile_options(None), &mut d.camera_profile));
    // browsed photos preview the choice as the import will apply it (`Session::browse_profile`)
    app.session.import_profile_choice = if d.camera_profile.is_empty() {
        lcc_engine::profile_bodies::ImportProfile::PerBody(d.per_body.clone())
    } else {
        lcc_engine::profile_bodies::ImportProfile::All(d.camera_profile.clone())
    };
    if !d.camera_profile.is_empty() {
        return;
    }
    let choice = lcc_engine::profile_bodies::ImportProfile::PerBody(d.per_body.clone());
    let rows = if screen && crate::import_tab::showing(app) {
        // the Import tab: the browsed photos it would import now (its Take)
        let take = app.ui.settings.import_tab.take();
        let (picks, selected, all) = app.session.import_targets();
        let ids = match take {
            lcc_engine::cmd::import_tab::Take::Picks => picks,
            lcc_engine::cmd::import_tab::Take::Selected => selected,
            lcc_engine::cmd::import_tab::Take::All => all,
        };
        let photos: Vec<&lcc_catalog::Photo> = ids.iter().filter_map(|id| app.session.catalog.photo(*id).map(|p| &**p)).collect();
        lcc_engine::profile_bodies::plan_photos(&app.session, &photos, &choice)
    } else {
        let checked: Vec<_> = d.candidates.iter().zip(&d.checked).filter(|(_, c)| **c).map(|(c, _)| c.clone()).collect();
        lcc_engine::profile_bodies::plan(&app.session, &checked, &choice)
    };
    for body in rows {
        let label = format!("{} · {} ({})", body.camera, body.serial.trim_start_matches("file:"), body.photos);
        // (the body under the Camera Profile row: the label column is too narrow for it)
        row(ui, "", &mut |ui: &mut egui::Ui| {
            let mut body_row = |ui: &mut egui::Ui| {
                ui.label(egui::RichText::new(label.as_str()).color(Tokens::get(ui.ctx()).text_dim));
                let mut v = d.per_body.get(&body.body).cloned().unwrap_or_default();
                let mut options = camera_profile_options(Some(&body.camera));
                if let Some(first) = options.first_mut() {
                    // automatic names what it gives this body
                    first.1 = format!("{} ({})", crate::i18n::tr("Automatic"), lcc_engine::profile_bodies::profile_name(&body.profile));
                }
                profile_combo(ui, &format!("importBodyProfile:{}", body.body), &options, &mut v);
                if v.is_empty() {
                    d.per_body.remove(&body.body);
                } else {
                    d.per_body.insert(body.body.clone(), v);
                }
            };
            // (a narrow settings column: the body's name above its menu)
            if screen {
                ui.vertical(|ui| body_row(ui));
            } else {
                body_row(ui);
            }
        });
    }
}

/// The develop presets the import applies, in order (ported from OneSlips' Import panel stack,
/// LR-IMP-WINDOW / LR-PRESET-PICKER): one list, the first row being what a single `preset` named.
/// Each row moves earlier or later or goes; Add puts a preset at the end. With presets on the
/// list, "Presets may change the camera profile" (off: each raw keeps its body's profile).
/// Shared by the Import tab, the full-window review and the review's form.
pub(crate) fn preset_stack_section(app: &mut LccApp, ui: &mut egui::Ui, d: &mut ImportDialog, screen: bool) {
    let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        if screen {
            crate::dialog_frame::form_row(ui, label, |ui| add(ui));
        } else {
            field(ui, label, |ui| add(ui));
        }
    };
    d.fold_preset();
    let t = Tokens::get(ui.ctx());
    let n = d.presets.len();
    // (index, -1 earlier / 1 later / 0 remove)
    let mut action: Option<(usize, i32)> = None;
    for i in 0..n {
        let id = d.presets.get(i).cloned().unwrap_or_default();
        let name = app
            .session
            .presets
            .iter()
            .find(|p| p.id == id)
            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
            .unwrap_or_else(|| crate::i18n::tr("(missing preset)").to_string());
        row(ui, if i == 0 { "Presets" } else { "" }, &mut |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let sz = vec2(22.0, 22.0);
                if crate::widgets::icon_button(ui, &format!("importPresetUp-{i}"), Icon::ChevronUp, sz, false, i > 0, "Apply earlier").clicked() {
                    action = Some((i, -1));
                }
                if crate::widgets::icon_button(ui, &format!("importPresetDown-{i}"), Icon::ChevronDown, sz, false, i + 1 < n, "Apply later").clicked()
                {
                    action = Some((i, 1));
                }
                if crate::widgets::icon_button(ui, &format!("importPresetRemove-{i}"), Icon::Close, sz, false, true, "Remove from the stack")
                    .clicked()
                {
                    action = Some((i, 0));
                }
                ui.add_space(4.0);
                let r = ui.add(egui::Label::new(egui::RichText::new(format!("{}. {name}", i + 1)).color(t.text)).truncate());
                register(ui.ctx(), format!("label:importPreset-{i}"), r.rect);
            });
        });
    }
    match action {
        Some((i, 0)) if i < d.presets.len() => {
            d.presets.remove(i);
        }
        Some((i, -1)) if i > 0 && i < d.presets.len() => d.presets.swap(i, i - 1),
        Some((i, 1)) if i + 1 < d.presets.len() => d.presets.swap(i, i + 1),
        _ => {}
    }
    let add_label = crate::i18n::tr(if d.presets.is_empty() { "Add a preset…" } else { "Add another preset…" }).to_string();
    let mut added: Option<String> = None;
    let mut manage = false;
    row(ui, if n == 0 { "Presets" } else { "" }, &mut |ui: &mut egui::Ui| {
        let manage_w =
            ui.painter().layout_no_wrap(crate::i18n::tr("Manage Presets…").to_string(), egui::TextStyle::Button.resolve(ui.style()), t.text).size().x
                + ui.spacing().button_padding.x * 2.0;
        let combo_w = (ui.available_width() - manage_w - ui.spacing().item_spacing.x - 2.0).max(120.0);
        let combo = crate::widgets::combo_box("import-preset")
            .selected_text(add_label.as_str())
            .width(combo_w)
            .height(300.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                if let Some(pid) = crate::panels::presets::picker_rows(ui, "import-preset", &app.session, None) {
                    added = Some(pid);
                    ui.close();
                }
            });
        register(ui.ctx(), "combo:importPresetAdd", combo.response.rect);
        // the presets themselves: folders, hide, import (Presets Manager)
        if crate::widgets::text_button(ui, "importPresetsManage", "Manage Presets…", false).clicked() {
            manage = true;
        }
    });
    if manage && let Err(e) = app.run("dialog.presetsManager", serde_json::json!({})) {
        app.toast(ui.ctx(), e);
    }
    if let Some(pid) = added {
        d.presets.push(pid);
    }
    let help = |ui: &mut egui::Ui, text: &str| {
        if screen {
            crate::dialog_frame::help(ui, text);
        } else {
            ui.label(egui::RichText::new(crate::i18n::tr(text)).color(t.text_dim).small());
        }
    };
    if d.presets.len() > 1 {
        help(ui, "Applied in this order: each sets its settings on top of the one before, as its own History step.");
    }
    if !d.presets.is_empty() {
        let text = crate::i18n::tr("Presets may change the camera profile").to_string();
        let r = if screen { crate::dialog_frame::checkbox(ui, &mut d.preset_profile, text) } else { ui.checkbox(&mut d.preset_profile, text) };
        register(ui.ctx(), "check:importPresetProfile", r.rect);
        if !d.preset_profile {
            help(ui, "Each raw keeps its camera body's profile, whatever profile a preset names.");
        }
    }
}

/// Start the Auto Import a `library.autoImportScan` result `r` asks for, or, when another import
/// took `app.import` meanwhile, release the files the scan marked as tried so the next scan offers
/// them again rather than dropping them until a Synchronize Folder (#1302).
pub(crate) fn auto_import_take(app: &mut LccApp, r: &Value) {
    let Some(mut params) = r.get("import").cloned() else { return };
    let paths: Vec<String> = params["paths"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if app.import.is_some() {
        app.session.auto_import_retry(&paths);
        return;
    }
    if let Some(o) = params.as_object_mut() {
        o.remove("paths");
    }
    let undo0 = app.session.undo.len();
    app.import = Some(ImportTask::new(paths, params, undo0, false).auto());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_height_follows_the_window() {
        // the grid takes what the options and buttons leave, in whole points
        assert_eq!(grid_height(700.0, 300.0, 142.0), 400.0);
        assert_eq!(grid_height(1000.5, 300.0, 142.0), 700.0);
        // never less than one row of thumbnails
        assert_eq!(grid_height(200.0, 300.0, 142.0), 142.0);
        // a bad measurement doesn't take the grid away or make it endless
        assert_eq!(grid_height(700.0, -50.0, 142.0), 700.0);
        assert_eq!(grid_height(700.0, f32::NAN, 142.0), 700.0);
        assert_eq!(grid_height(f32::INFINITY, 300.0, 142.0), DIALOG_SIZE[1] - DEFAULT_BELOW_GRID);
    }

    /// Six candidates, all checked; candidate 3 is a duplicate (unchecked, not importable).
    fn dialog() -> ImportDialog {
        let mut candidates: Vec<ImportCandidate> = (0..6).map(|i| ImportCandidate { path: format!("img{i}.jpg"), ..Default::default() }).collect();
        candidates[3].duplicate = Some("content".into());
        ImportDialog::new(candidates)
    }

    /// Unticking "Don't Import Suspected Duplicates" checks the content duplicates (they can then be
    /// imported as copies); a file at a path the library has stays out.
    #[test]
    fn unticking_dont_import_duplicates_checks_the_copies() {
        let mut d = dialog();
        d.candidates[5].duplicate = Some("path".into());
        d.checked[5] = false;
        d.set_skip_duplicates(false);
        assert!(d.checked[3] && d.importable(3) && !d.importable(5), "{:?}", d.checked);
        d.set_skip_duplicates(true);
        assert!(!d.checked[3] && !d.importable(3));
    }

    /// A file whose photo is only browsed (Local) comes from the scan as an ordinary candidate that
    /// still names its record: offered when adding in place; copying or moving would not copy it
    /// (the engine adds it where it is), so there it counts as already in the library.
    #[test]
    fn a_browsed_photo_is_importable_only_when_adding_in_place() {
        let mut d = dialog();
        d.candidates[5].local = true;
        assert!(d.is_browsed(5) && d.importable(5));
        assert_eq!(dup_counts(&d), (0, 1, 0), "adding in place: not skipped");
        d.copy = true;
        assert!(!d.importable(5), "copying leaves it where it is");
        assert!(!d.selected_paths().contains(&"img5.jpg".to_string()));
        assert_eq!(dup_counts(&d), (1, 1, 0));
        assert!(!d.is_browsed(3), "a duplicate isn't browsed");
        // its library twin is in Recently Deleted: still offered in place ("Import as new" brings it in), never copied
        d.copy = false;
        d.candidates[4].local = true;
        d.candidates[4].duplicate = Some("content".into());
        d.set_trashed(vec![false, false, false, false, true, false]);
        assert!(d.importable(4));
        d.copy = true;
        assert!(!d.importable(4), "copying would add it where it is");
    }

    /// A file type's toggle is on only for files that can be imported now.
    #[test]
    fn a_file_type_toggle_ignores_browsed_photos_that_copying_cannot_import() {
        let mut c = ImportCandidate { path: "a.png".into(), local: true, ..Default::default() };
        c.format = "PNG".into();
        let mut d = ImportDialog::new(vec![c]);
        d.copy = true;
        assert!(!d.type_checked("PNG"));
        d.copy = false;
        assert!(d.type_checked("PNG"));
    }

    /// The review warns that keywords and presets skip a browsed photo only when it matters.
    #[test]
    fn keywords_and_presets_are_flagged_only_when_a_browsed_photo_would_skip_them() {
        let c = ImportCandidate { path: "a.png".into(), local: true, ..Default::default() };
        let mut d = ImportDialog::new(vec![c]);
        assert!(!d.options_skip_browsed(), "nothing set, nothing to skip");
        d.keywords = "beach".into();
        assert!(d.options_skip_browsed());
        d.checked[0] = false;
        assert!(!d.options_skip_browsed(), "unchecked: not imported anyway");
        d.checked[0] = true;
        d.copy = true;
        assert!(!d.options_skip_browsed(), "copying doesn't offer it");
    }

    /// Identical copies inside the source folder are not "already in the library" (a fresh library
    /// said "8 already in the library").
    #[test]
    fn copies_within_the_source_are_not_counted_as_in_the_library() {
        let mut d = dialog();
        assert_eq!(dup_counts(&d), (0, 1, 0), "same bytes as an earlier file, no library photo");
        d.candidates[4].duplicate = Some("content".into());
        d.candidates[4].existing = Some(7);
        d.candidates[5].duplicate = Some("path".into());
        d.candidates[5].existing = Some(8);
        assert_eq!(dup_counts(&d), (2, 1, 0));
    }

    /// A toggle per file type: the raws only, without the JPEGs beside them (issue #344).
    #[test]
    fn file_types_check_and_uncheck_together() {
        let mut candidates: Vec<ImportCandidate> =
            ["a.ARW", "a.JPG", "b.arw", "b.jpeg", "c.png"].iter().map(|p| ImportCandidate { path: p.to_string(), ..Default::default() }).collect();
        candidates[2].format = "ARW".into();
        candidates[3].format = "JPEG".into();
        candidates[2].duplicate = Some("content".into());
        let mut d = ImportDialog::new(candidates);
        assert_eq!(d.file_types(), [("ARW".to_string(), 2), ("JPG".to_string(), 2), ("PNG".to_string(), 1)]);
        assert!(d.type_checked("JPG"));
        d.check_type("JPG", false);
        assert_eq!(d.checked, [true, false, false, false, true]);
        assert!(!d.type_checked("JPG"));
        d.check_type("PNG", false);
        d.check_type("ARW", true);
        // the duplicate stays unchecked: it isn't importable
        assert_eq!(d.checked, [true, false, false, false, false]);
        d.check_type("JPG", true);
        assert_eq!(d.checked, [true, true, false, true, false]);
        assert_eq!(d.selected_paths(), ["a.ARW", "a.JPG", "b.jpeg"]);
        // an unknown type changes nothing
        d.check_type("CR3", false);
        assert_eq!(d.checked, [true, true, false, true, false]);
    }

    #[test]
    fn click_toggles_one_candidate() {
        let mut d = dialog();
        d.click(1, false);
        assert_eq!(d.checked, [true, false, true, false, true, true]);
        d.click(1, false);
        assert_eq!(d.checked, [true, true, true, false, true, true]);
        d.click(3, false);
        assert_eq!(d.checked, [true, true, true, false, true, true], "a duplicate can't be checked");
    }

    #[test]
    fn shift_click_sets_the_range_to_the_clicked_state() {
        let mut d = dialog();
        d.click(1, false);
        d.click(4, true);
        assert_eq!(d.checked, [true, false, false, false, false, true], "1..=4 unchecked, the duplicate left alone");
        assert_eq!(d.selected_paths(), ["img0.jpg", "img5.jpg"]);
        // backwards from the new anchor (4) re-checks 2..=4
        d.click(2, true);
        assert_eq!(d.checked, [true, false, true, false, true, true]);
    }

    #[test]
    fn shift_click_without_an_earlier_click_toggles_one() {
        let mut d = dialog();
        d.click(2, true);
        assert_eq!(d.checked, [true, true, false, false, true, true]);
    }

    /// With "Leave them", a file in Recently Deleted stays out of Check All and the type toggles,
    /// but a click still checks it: it is imported (restored) on its own (issue #1285).
    #[test]
    fn leave_them_keeps_a_trashed_file_out_until_it_is_clicked() {
        let mut d = dialog();
        d.set_trashed(vec![false, false, false, true, false, false]);
        assert!(d.checked[3], "restored by default");
        d.set_on_deleted("");
        assert!(!d.checked[3]);
        d.check_type("JPG", true);
        assert!(!d.checked[3], "the type toggle leaves it out");
        d.click(3, false);
        assert!(d.checked[3], "a click checks it");
        assert!(d.selected_paths().contains(&"img3.jpg".to_string()));
        d.click(3, false);
        assert!(!d.checked[3]);
        // a plain duplicate (not in Recently Deleted) still can't be checked
        d.set_trashed(vec![false; 6]);
        d.click(3, false);
        assert!(!d.checked[3]);
    }

    /// A badge too wide to fit beside the checkbox goes under it instead of covering it (issue #1285).
    #[test]
    fn a_badge_never_covers_the_checkbox() {
        let img = Rect::from_min_size(pos2(100.0, 50.0), vec2(116.0, 116.0));
        let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(16.0, 16.0));
        // "Will be restored" fits beside it, top right
        let short = badge_rect(img, cb, vec2(76.0, 12.0));
        assert_eq!(short.right(), img.right() - 4.0);
        assert_eq!(short.top(), img.top() + 6.0);
        // "In Recently Deleted" doesn't
        let long = badge_rect(img, cb, vec2(96.0, 12.0));
        assert!(!long.intersects(cb), "{long:?} covers {cb:?}");
        assert_eq!(long.right(), img.right() - 4.0);
        assert!(img.contains_rect(long));
    }

    /// The dialog offers only controls a recipe may name: `library.import` refuses the others
    /// (white balance, geometry and crop, per-photo controls), so one offered here would make every
    /// import with it fail. Each is moved off its default, as a slider drag does.
    #[test]
    fn every_offered_recipe_control_is_accepted_by_the_import() {
        let session = lcc_engine::Session::new();
        let mut d = dialog();
        for id in RECIPE_CONTROLS {
            let spec = lcc_develop::controls::find(id).unwrap_or_else(|| panic!("{id} is not a develop control"));
            let moved = if spec.default < spec.max { spec.max } else { spec.min };
            note_recipe_value(&mut d.recipe, spec, moved);
        }
        assert_eq!(d.recipe.len(), RECIPE_CONTROLS.len(), "all named, none repeated");
        let params = json!({"paths": ["img0.jpg"], "recipe": recipe_items(&d.recipe)});
        let request = lcc_engine::cmd::library::import_params(&session, &params).unwrap_or_else(|e| panic!("refused: {e}"));
        assert_eq!(request.opts.recipe.len(), RECIPE_CONTROLS.len());
    }

    /// A slider moved off its default names its control in the recipe; moved back, it no longer does.
    #[test]
    fn a_slider_back_at_its_default_leaves_the_recipe() {
        let spec = lcc_develop::controls::find("light.exposure").unwrap();
        let mut recipe = std::collections::BTreeMap::new();
        note_recipe_value(&mut recipe, spec, 0.5);
        assert_eq!(recipe.get("light.exposure"), Some(&0.5));
        note_recipe_value(&mut recipe, spec, 1.25);
        assert_eq!(recipe.get("light.exposure"), Some(&1.25), "a new value replaces the old");
        note_recipe_value(&mut recipe, spec, spec.default);
        assert!(recipe.is_empty(), "{recipe:?}");
    }

    /// `start` hands `library.import` a `recipe` only when the dialog has one.
    #[test]
    fn start_passes_the_recipe_only_when_the_dialog_has_one() {
        let mut app = crate::LccApp::new(lcc_engine::Session::new(), crate::Services { png: None, ..Default::default() });
        let mut d = dialog();
        start(&mut app, &d).unwrap();
        let params = app.import.take().map(|t| t.params).unwrap();
        assert!(params.get("recipe").is_none(), "{params}");
        d.recipe.insert("light.exposure".into(), 0.5);
        d.recipe.insert("color.vibrance".into(), -20.0);
        start(&mut app, &d).unwrap();
        let params = app.import.take().map(|t| t.params).unwrap();
        assert_eq!(params["recipe"], json!([{"id": "color.vibrance", "value": -20.0}, {"id": "light.exposure", "value": 0.5}]), "{params}");
    }

    #[test]
    fn shift_click_with_a_stale_anchor_toggles_one() {
        let mut d = dialog();
        d.last_clicked = Some(99);
        d.click(2, true);
        assert_eq!(d.checked, [true, true, false, false, true, true]);
    }
}
