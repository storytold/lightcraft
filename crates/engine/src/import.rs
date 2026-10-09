//! Import: files and folders (recursive) → photos.
//!
//! 1. Expand folders recursively into supported files (hidden entries and the library's own
//!    folder are skipped), sorted for a stable order.
//! 2. Skip paths already in the catalog.
//! 3. Probe the rest — dimensions, metadata, and the **content hash** of the bytes — in parallel
//!    on native targets.
//! 4. Skip **duplicates by content** (same bytes already in the library, or twice in this batch).
//! 5. *Add* in place (the photo points at the original file), *copy* into the library's
//!    `Originals/YYYY/YYYY-MM-DD/` folder (names made unique) and point at the copy, or *move*
//!    there: placed like a copy (with its XMP sidecars), and the original removed only once its
//!    catalog record is committed and saved (see [`crate::import_move`] for the safety rules).
//! 6. Apply each photo's XMP sidecar (or a raw/DNG file's embedded XMP): the sidecar wins for
//!    metadata and develop settings are restored (see [`crate::sidecar`]). Its capture time fills
//!    in when the file has none (read before step 5, so it also files and names the copy).
//! 7. Optionally apply a preset and add keywords (the import dialog's options).
//! 8. Commit all new photos as one undoable op. (Undoing a move removes the catalog records;
//!    the files stay at the destination — undo never moves files back over a card.)
//!
//! The import dialog first calls [`scan`] (`library.importPreview`): the same expansion, probing
//! and duplicate detection without adding anything, so the user can review the candidates. The
//! probes are kept and reused by the import that follows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde::{Deserialize, Serialize};

use crate::Session;
use crate::media::ProbeInfo;

/// File extensions LightCraft imports (lower case).
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "nrw", "arw", "raf", "orf", "rw2", "rwl", "raw", "pef", "srw", "psd",
    "jxl", "gif", "bmp", "heic", "avif", // containers LightCraft cannot decode but imports as preview only (their embedded JPEG)
    "iiq", "crw", "mrw", "x3f", "kdc", "mos", "erf", "3fr", "fff",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportMode {
    /// Reference the files where they are.
    #[default]
    Add,
    /// Copy the files into the library's `Originals/` folder.
    Copy,
    /// Move the files there: as Copy, then each original (and its sidecars) is removed from the
    /// source once the copy is verified and catalogued.
    Move,
}

/// What an import does with a file the library already has in Recently Deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnDeleted {
    /// Leave it there; the report says so ([`Duplicate::existing_deleted`]).
    #[default]
    Skip,
    /// Bring the trashed photo back, with its edits ([`ImportReport::restored`]).
    Restore,
    /// Delete the trashed record permanently and import the file as a new photo.
    Fresh,
}

impl OnDeleted {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skip" => Some(Self::Skip),
            "restore" => Some(Self::Restore),
            "fresh" => Some(Self::Fresh),
            _ => None,
        }
    }
}

/// What an import does besides adding the photos.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// A file that is in Recently Deleted: skip (default), restore, or import afresh.
    pub on_deleted: OnDeleted,
    pub mode: ImportMode,
    /// Applied to every imported photo (one History entry).
    pub preset: Option<lightcraft_develop::Preset>,
    /// Added to every imported photo.
    pub keywords: Vec<String>,
    /// Browsing a folder: photos come in as `local` (not in the library), and a file with the
    /// same content as one already known is still listed.
    pub local: bool,
    /// Copy: where the copies go (default: the library's `Originals/`).
    pub destination: Option<String>,
    /// Copy: the folders inside the destination.
    pub organize: Organize,
    /// Copy: a file-name template for the copies ([`crate::rename::expand`] tokens: `{name}`,
    /// `{seq:N}`, `{date:%Y%m%d}`, `{camera}`…), numbered from `rename_start`.
    pub rename: Option<String>,
    pub rename_start: usize,
    /// A metadata preset (by name) applied to every imported photo.
    pub metadata_preset: Option<String>,
    /// Copy: raws are copied as DNG (Copy as DNG).
    pub convert_dng: bool,
}

/// How copies are filed in the destination. The date is the capture time, else the time of the
/// import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Organize {
    /// `YYYY/YYYY-MM-DD/` by capture date.
    #[default]
    ByDay,
    /// `YYYY/YYYY-MM/`.
    ByMonth,
    /// All in the destination itself.
    Flat,
    /// A folder template, e.g. `{date:%Y}/{date:%Y%m%d}` → `2026/20260114/`: the template's own
    /// `/` make the levels, each expanded with the file-name tokens ([`crate::rename::expand_folder`];
    /// never outside the destination).
    Template(String),
}

impl Organize {
    /// `date` | `month` | `flat`, or a folder template (anything with a `{`, `/` or `\`).
    pub fn parse(s: &str) -> Option<Organize> {
        match s {
            "date" | "day" | "byDay" => Some(Organize::ByDay),
            "month" | "byMonth" => Some(Organize::ByMonth),
            "flat" | "none" | "intoOneFolder" => Some(Organize::Flat),
            t if t.contains(['{', '/', '\\']) => Some(Organize::Template(t.trim().to_string())),
            _ => None,
        }
    }

    /// The folders (inside the destination) a copy of `p` goes to.
    pub fn folders(&self, p: &Photo) -> Vec<String> {
        let date = p.date();
        let day = date.get(..10).filter(|d| d.len() == 10).unwrap_or("undated");
        let year = day.get(..4).unwrap_or("undated");
        match self {
            Organize::ByDay => vec![year.into(), day.into()],
            Organize::ByMonth => vec![year.into(), day.get(..7).unwrap_or("undated").into()],
            Organize::Flat => Vec::new(),
            Organize::Template(t) => crate::rename::expand_folder(t, p, 1),
        }
    }
}

/// A file found by [`scan`], for the import review.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportCandidate {
    pub path: String,
    pub name: String,
    pub format: String,
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub captured: Option<String>,
    /// `"path"` (already in the library), `"content"` (same bytes in the library or earlier in this
    /// list): skipped by the import.
    pub duplicate: Option<String>,
    /// The photo that already has it.
    pub existing: Option<u64>,
    /// Not readable.
    pub error: Option<String>,
    /// A raw variant that can't be decoded yet (why): it imports as preview only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_only: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Duplicate {
    pub path: String,
    /// The photo that already has these bytes (or this path).
    pub existing: Option<u64>,
    /// `"path"` (already imported from there) or `"content"` (same bytes elsewhere).
    pub reason: &'static str,
    /// That photo is in Recently Deleted (see [`OnDeleted`]).
    #[serde(rename = "existingDeleted", skip_serializing_if = "std::ops::Not::not")]
    pub existing_deleted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: Vec<u64>,
    pub duplicates: Vec<Duplicate>,
    /// [`OnDeleted::Restore`]: photos brought back from Recently Deleted instead of imported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<u64>,
    /// Move: the originals that were moved (and removed from the source).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<Moved>,
    /// Move: originals (or sidecars) left at the source, and why — the photo may still have been
    /// imported (from its copy, or in place).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<Kept>,
    /// (path, error)
    pub failed: Vec<(String, String)>,
    /// Files found after expanding folders.
    pub scanned: usize,
    /// Photos whose metadata/develop settings were read from an XMP sidecar (or embedded XMP).
    pub sidecars: usize,
    /// Per-photo diagnostics from foreign XMP develop settings that could not be fully carried over.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ImportWarning>,
}

/// A structured diagnostic from foreign XMP develop settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportWarningCode {
    /// A named camera profile was found but cannot be applied as Lightroom does.
    CameraProfile,
    /// Lightroom geometry data was found but its saved transform is not retained.
    UprightGeometry,
    /// One or more other XMP develop fields have no LightCraft mapping.
    UnmappedXmp,
}

/// The saved Lightroom Upright mode associated with a foreign geometry transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportUprightMode {
    Off,
    Auto,
    Level,
    Vertical,
    Full,
    Guided,
    Unknown,
}

/// One actionable XMP import diagnostic. `path` is filled when its photo is committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportWarning {
    pub path: String,
    pub code: ImportWarningCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ImportUprightMode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

/// A file moved by an import.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Moved {
    pub from: String,
    pub to: String,
    /// Its sidecars' new paths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<String>,
}

/// A source a Move left where it was.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

/// The develop settings a photo gets on import: raws start from their as-shot white balance with
/// default sharpening / colour noise reduction, and file-embedded lens corrections on (as the
/// camera intended); a user default preset ([`ImportDefaults`]) goes on top.
pub fn import_defaults(p: &Photo) -> lightcraft_develop::DevelopSettings {
    p.import_defaults()
}

/// Does the photo still look as the camera rendered it (its embedded camera preview is then a
/// fair stand-in)? Not when a default preset changed it on import.
pub fn has_import_look(p: &Photo) -> bool {
    *p.develop == p.camera_defaults()
}

/// User defaults applied on import (Settings → Import; saved in the library's `prefs.json`).
/// Presets are referenced by id; a preset that no longer exists is ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportDefaults {
    /// Preset applied to raw files (`None` = the LightCraft default).
    pub raw_preset: Option<String>,
    /// Use a camera's own default (below) when the photo's camera has one.
    pub per_camera: bool,
    /// Per-camera raw defaults, keyed by the camera's make + model as shown in Info
    /// (`"Canon EOS R5"`).
    pub cameras: Vec<CameraDefault>,
    /// Preset applied to non-raw images (JPEG, PNG, TIFF, HEIC…; `None` = none).
    pub other_preset: Option<String>,
    /// Copyright notice given to imported photos that don't carry one (empty = none).
    pub copyright: String,
    /// Creator given to imported photos that don't name one (empty = none).
    pub creator: String,
    /// Metadata preset applied to every imported photo (`metadata.*`; `None` = none).
    pub metadata_preset: Option<String>,
    /// Auto import: a watched folder whose new photos are added as they arrive (`None` = off),
    /// copied into the library's Originals instead of added in place when `auto_copy`, and an
    /// album they go to.
    pub auto_folder: Option<String>,
    pub auto_copy: bool,
    pub auto_album: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CameraDefault {
    /// Make + model (`Meta::camera`).
    pub camera: String,
    /// Preset id; `None` = the LightCraft default for this camera.
    pub preset: Option<String>,
}

impl ImportDefaults {
    /// The preset id that applies to a photo of this kind and camera, if any.
    pub fn preset_for(&self, raw: bool, camera: &str) -> Option<&str> {
        if !raw {
            return self.other_preset.as_deref();
        }
        if self.per_camera
            && let Some(c) = self.cameras.iter().find(|c| !camera.is_empty() && c.camera.eq_ignore_ascii_case(camera))
        {
            return c.preset.as_deref();
        }
        self.raw_preset.as_deref()
    }
}

/// Give a freshly imported photo its default settings: the camera defaults, then the matching
/// default preset ([`ImportDefaults::preset_for`]), remembered as the photo's import look.
pub fn apply_import_defaults(s: &Session, p: &mut Photo) {
    // metadata defaults fill gaps only: the file's own copyright / creator win
    let d = &s.import_defaults;
    if p.meta.copyright.trim().is_empty() && !d.copyright.trim().is_empty() {
        p.meta.copyright = d.copyright.trim().to_string();
    }
    if p.meta.creator.trim().is_empty() && !d.creator.trim().is_empty() {
        p.meta.creator = d.creator.trim().to_string();
    }
    if let Some(mp) = d.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
        crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
    }
    let base = p.camera_defaults();
    // a raw shown from its embedded JPEG gets the rendered-file default, not the raw one
    let raw = p.develops_raw();
    let preset = s.import_defaults.preset_for(raw, &p.meta.camera).and_then(|id| s.presets.iter().find(|x| x.id == id));
    match preset {
        Some(pr) => {
            let look = std::sync::Arc::new(pr.apply(&base, 1.0));
            p.develop = look.clone();
            p.import_look = (*look != base).then_some(look);
        }
        None => {
            p.develop = std::sync::Arc::new(base);
            p.import_look = None;
        }
    }
}

pub fn is_supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Expand files and folders (recursively) into supported files. `skip` (e.g. the library folder)
/// is never descended into. Each folder's walk is bounded ([`crate::walk::Limits::default`]); a
/// walk that stops at a bound is logged.
pub fn expand(paths: &[String], skip: Option<&Path>) -> Vec<String> {
    expand_within(paths, skip, crate::walk::Limits::default()).0
}

/// [`expand`] with the walk bounded by `limits` → (files, whether a walk stopped at a bound).
pub fn expand_within(paths: &[String], skip: Option<&Path>, limits: crate::walk::Limits) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    for p in paths {
        let p = Path::new(p);
        if skip.is_some_and(|s| p == s) {
            continue;
        }
        if p.is_dir() {
            let w = crate::walk::files_in(p, skip, limits, is_supported);
            if w.truncated {
                log::warn!(
                    "import: stopped looking in {} after {} entries (limits: {} entries, {} folder levels)",
                    p.display(),
                    w.entries,
                    limits.max_entries,
                    limits.max_depth
                );
            }
            truncated |= w.truncated;
            out.extend(w.files.into_iter().map(|f| f.to_string_lossy().to_string()));
        } else {
            // explicitly named files are attempted even with an unknown extension (sniffed)
            out.push(p.to_string_lossy().to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    (out, truncated)
}

/// What a path given on the command line at launch becomes ([`launch_path`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPath {
    /// Import this (absolute) file or folder, in the background.
    Import(String),
    /// Not imported: `path` (absolute) is a whole drive or the whole home folder, which is never
    /// walked without being chosen in the app (issue #374).
    Refused { path: String, why: String },
}

/// Resolve a launch argument for importing (issue #374). An empty argument is ignored (`None`);
/// a relative one (`.`, `..`, `photos`; `/` or `\` on Windows, the current drive's root) is
/// resolved against `cwd` — the working folder a launcher picked, often a drive root or
/// `C:\Windows\System32` — and refused when that leaves no folder (`cwd` unknown). A drive
/// or file-system root is replaced by its camera folder (`DCIM`, from `dcim`: a memory card) or
/// refused, and so is the home folder `home`: either would walk most of the disk.
pub fn launch_path(arg: &str, cwd: Option<&Path>, home: Option<&Path>, dcim: impl Fn(&Path) -> Option<PathBuf>) -> Option<LaunchPath> {
    if arg.trim().is_empty() {
        return None;
    }
    let given = Path::new(arg);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        match cwd {
            Some(c) => c.join(given),
            None => return Some(LaunchPath::Refused { path: arg.to_string(), why: "a relative path, and the working folder is unknown".into() }),
        }
    };
    // `.` and `..` resolved by name (no file-system access)
    let mut abs = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                abs.pop();
            }
            other => abs.push(other.as_os_str()),
        }
    }
    let shown = abs.to_string_lossy().to_string();
    if abs.parent().is_none() {
        return Some(match dcim(&abs) {
            Some(d) => LaunchPath::Import(d.to_string_lossy().to_string()),
            None => LaunchPath::Refused { path: shown, why: "a whole drive".into() },
        });
    }
    let key = lightcraft_catalog::query::folder_key;
    if home.is_some_and(|h| key(&h.to_string_lossy()) == key(&shown)) {
        return Some(LaunchPath::Refused { path: shown, why: "the whole home folder".into() });
    }
    Some(LaunchPath::Import(shown))
}

/// Probe files (headers + content hash) as an import would.
pub(crate) fn probe_paths(s: &Session, paths: &[String]) -> Vec<Result<ProbeInfo, String>> {
    probe_all(s.media.file_probe.as_ref(), paths, &ScanProgress::default())
}

fn probe_all(probe: Option<&crate::media::FileProbe>, paths: &[String], progress: &ScanProgress) -> Vec<Result<ProbeInfo, String>> {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(probe) = probe.cloned() else {
        return paths
            .iter()
            .map(|p| {
                Ok(ProbeInfo {
                    format: Path::new(p).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
                    ..Default::default()
                })
            })
            .collect();
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(paths.len());
        if n > 1 {
            let mut results: Vec<Option<Result<ProbeInfo, String>>> = vec![None; paths.len()];
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out = std::sync::Mutex::new(&mut results);
            std::thread::scope(|sc| {
                for _ in 0..n {
                    sc.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= paths.len() {
                                break;
                            }
                            if progress.cancel.load(Relaxed) {
                                break;
                            }
                            let r = probe(&paths[i]);
                            progress.done.fetch_add(1, Relaxed);
                            out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                        }
                    });
                }
            });
            return results.into_iter().map(|r| r.unwrap_or_else(|| Err("not probed".into()))).collect();
        }
    }
    paths
        .iter()
        .map(|p| {
            if progress.cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            let r = probe(p);
            progress.done.fetch_add(1, Relaxed);
            r
        })
        .collect()
}

/// Copy `src` into `dir` as `name` (default: its own name), made unique with -1, -2…; returns the
/// new path. The copy is verified ([`crate::import_move::copy_new`]: a new file, synced and
/// checked against `probe_hash`, the content hash the probe computed — or compared byte for byte
/// with the source when there is none); a bad one is removed and the photo reported as failed.
fn copy_into(dir: &Path, src: &str, name: Option<&str>, probe_hash: Option<&str>) -> Result<String, String> {
    let name = name
        .map(str::to_string)
        .unwrap_or_else(|| Path::new(src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into()));
    let expect = probe_hash.and_then(lightcraft_preview::Hash128::parse);
    crate::import_move::copy_new(Path::new(src), dir, &name, expect).map(|p| p.to_string_lossy().to_string())
}

/// What a [`scan`] needs from the session, so it can run on another thread (a folder on a network
/// share can take minutes to read).
pub struct ScanInput {
    probe: Option<crate::media::FileProbe>,
    skip: Option<PathBuf>,
    /// path → (photo, its summary) for files already in the library.
    by_path: HashMap<PathBuf, (PhotoId, ImportCandidate)>,
    by_hash: HashMap<String, PhotoId>,
    cache: HashMap<String, ProbeInfo>,
}

/// Progress and cancellation of a running [`scan_with`].
#[derive(Default)]
pub struct ScanProgress {
    /// Files to probe, set once the folders are expanded.
    pub total: std::sync::atomic::AtomicUsize,
    pub done: std::sync::atomic::AtomicUsize,
    pub cancel: std::sync::atomic::AtomicBool,
}

/// The result of [`scan_with`]: the candidates, and the probes to keep for the import that follows.
pub struct ScanOutput {
    pub candidates: Vec<ImportCandidate>,
    pub probes: HashMap<String, ProbeInfo>,
}

impl ScanInput {
    pub fn new(s: &mut Session, paths: &[String]) -> (Self, Vec<String>) {
        let skip = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        let mut by_path = HashMap::new();
        let mut by_hash = HashMap::new();
        for p in s.catalog.photos() {
            if let Source::File { path } = &p.source {
                let c = ImportCandidate {
                    path: path.clone(),
                    format: p.format.clone(),
                    kind: p.kind,
                    width: p.width,
                    height: p.height,
                    file_size: p.file_size,
                    captured: p.captured.clone(),
                    duplicate: Some("path".into()),
                    existing: Some(p.id.0),
                    ..Default::default()
                };
                by_path.insert(PathBuf::from(path), (p.id, c));
            }
            if let Some(h) = &p.content_hash {
                by_hash.insert(h.clone(), p.id);
            }
        }
        let input = ScanInput { probe: s.media.file_probe.clone(), skip, by_path, by_hash, cache: std::mem::take(&mut s.import_probes) };
        (input, paths.to_vec())
    }
}

/// Find and probe what importing `paths` would add, marking duplicates (by path, or by content
/// against the library and earlier candidates). Nothing is added; the probes are kept for the
/// import that follows.
pub fn scan(s: &mut Session, paths: &[String]) -> Vec<ImportCandidate> {
    let (input, paths) = ScanInput::new(s, paths);
    let out = scan_with(input, &paths, &ScanProgress::default());
    s.import_probes = out.probes;
    out.candidates
}

/// [`scan`] without the session, so it can run on a worker thread. Stops early (returning what it
/// has) when `progress.cancel` is set.
pub fn scan_with(mut input: ScanInput, paths: &[String], progress: &ScanProgress) -> ScanOutput {
    use std::sync::atomic::Ordering::Relaxed;
    let files = expand(paths, input.skip.as_deref());
    let todo: Vec<String> = files.iter().filter(|f| !input.by_path.contains_key(Path::new(f))).cloned().collect();
    progress.total.store(todo.len(), Relaxed);
    // probes from a preceding `scan` are reused when the file is unchanged (same size)
    let cached: Vec<Option<ProbeInfo>> = todo
        .iter()
        .map(|f| {
            let info = input.cache.remove(f)?;
            let size = std::fs::metadata(f).map(|m| m.len()).ok();
            (size.is_none() || size == Some(info.file_size)).then_some(info)
        })
        .collect();
    let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
    progress.done.store(todo.len() - missing.len(), Relaxed);
    let mut fresh = probe_all(input.probe.as_ref(), &missing, progress).into_iter();
    let probed: Vec<Result<ProbeInfo, String>> =
        cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
    crate::memory::release();
    let mut probes: HashMap<String, Result<ProbeInfo, String>> = todo.into_iter().zip(probed).collect();
    let mut seen_hash: HashMap<String, Option<u64>> = input.by_hash.into_iter().map(|(h, id)| (h, Some(id.0))).collect();
    let mut out = Vec::with_capacity(files.len());
    let mut kept = HashMap::new();
    for f in files {
        if let Some((_, c)) = input.by_path.get(Path::new(&f)) {
            let mut c = c.clone();
            c.name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
            out.push(c);
            continue;
        }
        let name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
        let mut c = ImportCandidate { path: f.clone(), name, ..Default::default() };
        match probes.remove(&f) {
            Some(Ok(info)) => {
                (c.format, c.kind, c.width, c.height, c.file_size, c.captured) =
                    (info.format.clone(), info.kind, info.width, info.height, info.file_size, info.captured.clone());
                c.preview_only = info.preview_only.clone();
                if let Some(h) = &info.content_hash {
                    match seen_hash.get(h) {
                        Some(existing) => {
                            c.duplicate = Some("content".into());
                            c.existing = *existing;
                        }
                        None => {
                            seen_hash.insert(h.clone(), None);
                        }
                    }
                }
                kept.insert(f, info);
            }
            Some(Err(e)) => c.error = Some(e),
            None => c.error = Some("not probed".into()),
        }
        out.push(c);
    }
    ScanOutput { candidates: out, probes: kept }
}

/// Import files/folders. See the module docs.
pub fn import(s: &mut Session, paths: &[String], mode: ImportMode) -> crate::Result<ImportReport> {
    import_with(s, paths, &ImportOptions { mode, ..Default::default() })
}

/// Import files/folders with options (preset, keywords). See the module docs.
///
/// The file-system half ([`ImportJob::prepare`]: expanding folders, probing, reading sidecars,
/// copying / placing files) and the catalog half ([`commit_prepared`]) are separate so the app can
/// run the first on a worker thread; here both run in turn.
pub fn import_with(s: &mut Session, paths: &[String], opts: &ImportOptions) -> crate::Result<ImportReport> {
    let mut job = ImportJob::new(s, opts.clone())?;
    let prepared = job.prepare(paths, &std::sync::atomic::AtomicBool::new(false));
    // probes a scan left for files not imported now stay for a later import
    s.import_probes.extend(std::mem::take(&mut job.cache));
    commit_prepared(s, &job.opts, &job.now, prepared)
}

/// Everything the file-system half of an import needs, detached from the session so it can run
/// on another thread (a network share or a card can take minutes). Built by [`ImportJob::new`];
/// [`ImportJob::prepare`] may be called for several batches in turn (renamed copies keep counting,
/// duplicates are found across batches); each batch is then [`commit_prepared`] on the session.
pub struct ImportJob {
    pub opts: ImportOptions,
    /// The mode in effect (Copy / Move become Add in a library without its own folder).
    mode: ImportMode,
    lib_dir: Option<PathBuf>,
    copy_root: Option<PathBuf>,
    /// path → (photo, a Local browse record?)
    by_path: HashMap<String, (PhotoId, bool)>,
    /// content hash → the photo that has it (`None`: a file earlier in this import)
    by_hash: HashMap<String, Option<PhotoId>>,
    probe: Option<crate::media::FileProbe>,
    file_bytes: Option<crate::merge::ByteReader>,
    /// Probes from a preceding scan (not read again).
    cache: HashMap<String, ProbeInfo>,
    naming: crate::sidecar::SidecarNaming,
    now: String,
    seq: usize,
    /// Files found so far (folders expanded) and handled so far, for progress.
    pub total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The outcome of [`ImportJob::prepare`] for one batch, in file order.
#[derive(Debug, Default)]
pub struct Prepared {
    pub scanned: usize,
    items: Vec<PreparedItem>,
}

#[derive(Debug)]
enum PreparedItem {
    /// A Local (browsed) record imported for real.
    Promote(PhotoId),
    Duplicate {
        path: String,
        existing: Option<PhotoId>,
        reason: &'static str,
        hash: Option<String>,
    },
    Failed(String, String),
    Kept(Kept),
    Ready(Box<ReadyFile>),
}

#[derive(Debug)]
struct ReadyFile {
    path: String,
    stored: String,
    info: ProbeInfo,
    sidecar: Option<crate::sidecar::SidecarData>,
    placed: Option<crate::import_move::Placed>,
}

impl Prepared {
    /// Undo what the batch did on disk that only a commit would make permanent: files placed by a
    /// Move (their sources were never touched). Copies stay, like copies of a failed commit.
    pub fn rollback(&self) {
        for it in &self.items {
            if let PreparedItem::Ready(r) = it
                && let Some(pl) = &r.placed
            {
                crate::import_move::rollback(pl);
            }
        }
    }

    /// Entries in the batch (after expanding folders).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Recheck Add-only entries against the owner catalog immediately before commit.
    /// Preparation may have run while another import added the same path or content.
    pub fn revalidate_add(&mut self, s: &Session) {
        let mut paths = HashMap::new();
        let mut hashes = HashMap::new();
        for photo in s.catalog.photos() {
            if let Source::File { path } = &photo.source {
                paths.insert(normalize_import_path(path), photo.id);
            }
            if let Some(hash) = &photo.content_hash {
                hashes.insert(hash.clone(), photo.id);
            }
        }
        let mut items = Vec::with_capacity(self.items.len());
        for item in std::mem::take(&mut self.items) {
            let PreparedItem::Ready(ready) = item else {
                items.push(item);
                continue;
            };
            let ReadyFile { path, stored, info, sidecar, placed } = *ready;
            let existing_path = paths.get(&normalize_import_path(&stored)).copied().or_else(|| paths.get(&normalize_import_path(&path)).copied());
            let existing_hash = info.content_hash.as_ref().and_then(|hash| hashes.get(hash).copied());
            if let Some(existing) = existing_path.or(existing_hash) {
                if let Some(placed) = &placed {
                    crate::import_move::rollback(placed);
                }
                items.push(PreparedItem::Duplicate {
                    path,
                    existing: Some(existing),
                    reason: if existing_path.is_some() { "path" } else { "content" },
                    hash: info.content_hash,
                });
            } else {
                items.push(PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed })));
            }
        }
        self.items = items;
    }
}

fn normalize_import_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) { path.to_lowercase() } else { path }
}

impl ImportJob {
    /// Snapshot what an import with `opts` needs from the session (no file-system calls). Takes
    /// the probes a preceding scan left ([`Session::import_probes`]).
    pub fn new(s: &mut Session, opts: ImportOptions) -> crate::Result<Self> {
        let mode = opts.mode;
        // Only a library on disk has an `Originals/` folder. The browser build keeps the bytes of
        // every added file in its own storage already, so "copy" there means "add".
        let transfer = matches!(mode, ImportMode::Copy | ImportMode::Move);
        let mode = if transfer && s.library.as_ref().is_some_and(|l| !l.on_disk) { ImportMode::Add } else { mode };
        if mode == ImportMode::Move && opts.local {
            return Err(crate::EngineError::Other("browsing a folder never moves its files".into()));
        }
        let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // where copies go: the chosen folder, else the library's Originals/
        let copy_root = opts
            .destination
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| lib_dir.as_ref().map(|l| l.join("Originals")));
        if matches!(mode, ImportMode::Copy | ImportMode::Move) && copy_root.is_none() {
            return Err(crate::EngineError::Other("copying or moving needs a destination folder or an open library".into()));
        }
        let mut by_path = HashMap::new();
        let mut by_hash = HashMap::new();
        for p in s.catalog.photos() {
            // a trashed photo is replaced by a fresh import, so it doesn't count as a duplicate
            if p.deleted && opts.on_deleted == OnDeleted::Fresh {
                continue;
            }
            if let Source::File { path } = &p.source {
                by_path.insert(path.clone(), (p.id, p.local));
            }
            if let Some(h) = &p.content_hash {
                by_hash.insert(h.clone(), Some(p.id));
            }
        }
        Ok(ImportJob {
            seq: opts.rename_start.max(1),
            opts,
            mode,
            lib_dir,
            copy_root,
            by_path,
            by_hash,
            probe: s.media.file_probe.clone(),
            file_bytes: s.media.file_bytes.clone(),
            cache: std::mem::take(&mut s.import_probes),
            naming: s.xmp.naming,
            now: (s.clock)(),
            total: Default::default(),
            done: Default::default(),
        })
    }

    /// The file-system half of importing `paths`: expand folders, skip known paths, probe, skip
    /// duplicates by content, read sidecars, copy or place (Move) the files — several at once,
    /// with the names and outcomes of one after another. Nothing in the catalog changes. Stops
    /// before the next file once `cancel` is set.
    pub fn prepare(&mut self, paths: &[String], cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        let files = self.expand(paths);
        self.prepare_files(files, cancel)
    }

    /// Expand `paths` (folders recursively, the library's own folder skipped) into the files an
    /// import would handle; counted in [`ImportJob::total`].
    pub fn expand(&self, paths: &[String]) -> Vec<String> {
        let files = expand(paths, self.lib_dir.as_deref());
        self.total.fetch_add(files.len(), std::sync::atomic::Ordering::Relaxed);
        files
    }

    /// When the import started ([`Session::clock`]), for [`commit_prepared`].
    pub fn now(&self) -> &str {
        &self.now
    }

    /// [`ImportJob::prepare`] for files [`ImportJob::expand`] returned.
    pub fn prepare_files(&mut self, files: Vec<String>, cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        use std::sync::atomic::Ordering::Relaxed;
        let opts = self.opts.clone();
        let mode = self.mode;
        let mut out = Prepared { scanned: files.len(), items: Vec::new() };
        let mut todo = Vec::new();
        for f in files {
            match self.by_path.get(f.as_str()).copied() {
                Some((id, true)) if !opts.local => {
                    // a file that was only browsed (Local) joins the library when it is imported for real
                    self.by_path.insert(f, (id, false));
                    out.items.push(PreparedItem::Promote(id));
                    self.done.fetch_add(1, Relaxed);
                }
                Some((id, _)) => {
                    out.items.push(PreparedItem::Duplicate { path: f, existing: Some(id), reason: "path", hash: None });
                    self.done.fetch_add(1, Relaxed);
                }
                None => todo.push(f),
            }
        }

        // files a preceding scan already probed are not read again (a network share is slow to read)
        let cached: Vec<Option<ProbeInfo>> = todo.iter().map(|f| self.cache.remove(f)).collect();
        let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
        let progress = ScanProgress::default();
        let mut fresh = probe_all(self.probe.as_ref(), &missing, &progress).into_iter();
        let probed: Vec<Result<ProbeInfo, String>> =
            cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
        crate::memory::release();
        // Files are planned one by one, in order (duplicates, sidecars, names, folders); their
        // copies or moves — the slow part, reading and writing every byte and verifying it — are
        // collected and run on several threads (issue #367), then their outcomes take their places.
        let mut plan = Plan::default();
        for (path, info) in todo.into_iter().zip(probed) {
            if cancel.load(Relaxed) {
                break;
            }
            let mut info = match info {
                Ok(i) => i,
                Err(e) => {
                    log::warn!("import {path}: {e}");
                    self.done.fetch_add(1, Relaxed);
                    plan.slots.push(Slot::Item(PreparedItem::Failed(path, e)));
                    continue;
                }
            };
            // a copy of a file whose transfer is still pending: a duplicate only if that one succeeds
            if !opts.local
                && let Some(h) = &info.content_hash
                && plan.transfers.iter().any(|t| t.info.content_hash.as_ref() == Some(h))
            {
                self.run_plan(&mut plan, &mut out, cancel);
                if cancel.load(Relaxed) {
                    break;
                }
            }
            if let Some(h) = &info.content_hash
                && !opts.local
                && let Some(existing) = self.by_hash.get(h)
            {
                self.done.fetch_add(1, Relaxed);
                plan.slots.push(Slot::Item(PreparedItem::Duplicate { path, existing: *existing, reason: "content", hash: Some(h.clone()) }));
                continue;
            }
            // the XMP sidecar (or a raw's embedded XMP), read before copying: its capture time files
            // and names the copy when the file itself has none
            let raw = info.kind == MediaKind::Raw;
            let packet = crate::sidecar::find_sidecar(&path, self.naming)
                .and_then(|f| std::fs::read_to_string(f).ok())
                .or_else(|| info.xmp.clone().filter(|_| raw));
            let sidecar = packet.and_then(|x| match crate::sidecar::parse_sidecar(&x, raw) {
                Ok(sc) => Some(sc),
                Err(e) => {
                    log::warn!("import {path}: XMP: {e}");
                    None
                }
            });
            if info.captured.is_none() {
                info.captured = sidecar.as_ref().and_then(|sc| sc.captured.clone());
            }
            let stored = match (mode, &self.copy_root) {
                (ImportMode::Copy | ImportMode::Move, Some(root))
                    if !crate::import_move::inside(Path::new(&path), root)
                        && !self.lib_dir.as_ref().is_some_and(|l| crate::import_move::inside(Path::new(&path), l)) =>
                {
                    // the templates see the photo as it will be catalogued (undated: the import time)
                    let own = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let mut q = Photo::new(PhotoId(0), Source::File { path: path.clone() }, &own, &info.format, 0, 0, &self.now);
                    q.captured = info.captured.clone();
                    q.meta = info.meta.clone();
                    let name = opts.rename.as_deref().filter(|t| !t.trim().is_empty()).map(|t| {
                        let n = crate::rename::expand(t, &q, self.seq);
                        self.seq += 1;
                        n
                    });
                    let folders = opts.organize.folders(&q);
                    let dir = folders.iter().fold(root.to_path_buf(), |d, f| d.join(f));
                    // the name this file gets depends on what a pending transfer of a like-named file
                    // into the same folder ends up with: that one goes first
                    let as_named = name.as_deref().unwrap_or(if own.is_empty() { "photo" } else { own.as_str() });
                    let family = (dir.to_string_lossy().to_lowercase(), name_family(as_named));
                    if plan.transfers.iter().any(|t| t.family == family) {
                        self.run_plan(&mut plan, &mut out, cancel);
                        if cancel.load(Relaxed) {
                            break;
                        }
                    }
                    let how = if mode == ImportMode::Move && !crate::import_move::is_symlink(Path::new(&path)) {
                        How::Place { name: name.unwrap_or(own) }
                    } else {
                        if mode == ImportMode::Move {
                            let reason = "a symbolic link: the file it points to was copied, the link and its target stay";
                            plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                        }
                        How::Copy { name }
                    };
                    // Copy as DNG: the copied raw becomes a DNG (the copy is ours to replace)
                    let dng = opts.convert_dng && mode == ImportMode::Copy && raw && !info.format.eq_ignore_ascii_case("DNG");
                    plan.slots.push(Slot::Transfer);
                    plan.transfers.push(Transfer { path, info, sidecar, dir, how, dng, family });
                    continue;
                }
                (ImportMode::Move, _) => {
                    let reason = "already inside the destination or the library: added where it is";
                    plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                    path.clone()
                }
                _ => path.clone(),
            };
            if let Some(h) = &info.content_hash {
                self.by_hash.insert(h.clone(), None);
            }
            self.done.fetch_add(1, Relaxed);
            plan.slots.push(Slot::Item(PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed: None }))));
        }
        self.run_plan(&mut plan, &mut out, cancel);
        out
    }

    /// Run the transfers `plan` collected (on several threads) and append its items to `out` in
    /// file order. Transfers not started because `cancel` was set leave no item.
    fn run_plan(&mut self, plan: &mut Plan, out: &mut Prepared, cancel: &std::sync::atomic::AtomicBool) {
        let mut done = run_transfers(std::mem::take(&mut plan.transfers), self.file_bytes.as_ref(), cancel, &self.done).into_iter();
        for slot in std::mem::take(&mut plan.slots) {
            let item = match slot {
                Slot::Item(it) => it,
                Slot::Transfer => match done.next().flatten() {
                    Some(it) => it,
                    None => continue,
                },
            };
            if let PreparedItem::Ready(r) = &item
                && let Some(h) = &r.info.content_hash
            {
                self.by_hash.insert(h.clone(), None);
            }
            out.items.push(item);
        }
    }
}

/// A batch's items in file order while its transfers are pending ([`ImportJob::run_plan`]).
#[derive(Default)]
struct Plan {
    slots: Vec<Slot>,
    transfers: Vec<Transfer>,
}

enum Slot {
    Item(PreparedItem),
    /// The outcome of the next of [`Plan::transfers`].
    Transfer,
}

/// A file to copy or move into `dir`, planned in order and run on any thread ([`run_transfer`]).
struct Transfer {
    path: String,
    info: ProbeInfo,
    sidecar: Option<crate::sidecar::SidecarData>,
    dir: PathBuf,
    how: How,
    /// Copy as DNG.
    dng: bool,
    /// The folder (lowercase) and [`name_family`]: transfers of one family can take each other's
    /// names, so they never run at the same time.
    family: (String, String),
}

enum How {
    /// A verified copy, under `name` (default: the file's own).
    Copy { name: Option<String> },
    /// Move: placed (linked or copied) without touching the source.
    Place { name: String },
}

/// The names a file called `name` could end up with, as one key: the transfers number a taken name
/// `IMG-1.JPG`, `IMG-2.JPG`…, and Copy as DNG names its DNG `IMG.dng`, `IMG-2.dng`…, so `IMG.JPG`,
/// `IMG-1.jpg` and `img.dng` are all `img` (case-insensitive: so are some file systems).
fn name_family(name: &str) -> String {
    let mut stem = match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    };
    while let Some((head, tail)) = stem.rsplit_once('-')
        && !head.is_empty()
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        stem = head;
    }
    stem.to_lowercase()
}

/// Threads for `jobs` file reads (probes, copies) at once.
#[cfg(not(target_arch = "wasm32"))]
fn workers(jobs: usize) -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(jobs.max(1))
}

/// Files per batch for a caller that prepares an import in batches ([`ImportJob::prepare_files`]):
/// enough to keep every probe and copy thread busy (a batch waits for its slowest file), few
/// enough that each batch soon joins the catalog.
pub fn batch_size() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        workers(usize::MAX).saturating_mul(2).clamp(8, 16)
    }
    #[cfg(target_arch = "wasm32")]
    {
        8
    }
}

/// Run `transfers` (several at once where threads exist); their outcomes in order, `None` for one
/// not started because `cancel` was set. `done` counts each one finished.
fn run_transfers(
    transfers: Vec<Transfer>,
    file_bytes: Option<&crate::merge::ByteReader>,
    cancel: &std::sync::atomic::AtomicBool,
    done: &std::sync::atomic::AtomicUsize,
) -> Vec<Option<PreparedItem>> {
    use std::sync::atomic::Ordering::Relaxed;
    let one = |t: Transfer| {
        if cancel.load(Relaxed) {
            return None;
        }
        let path = t.path.clone();
        // last-resort guard: a panic is this file's failure, not the import's
        let item = crate::guard::catch("import", || run_transfer(t, file_bytes)).unwrap_or_else(|e| PreparedItem::Failed(path, e));
        done.fetch_add(1, Relaxed);
        Some(item)
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(transfers.len());
        if n > 1 {
            let mut results: Vec<Option<PreparedItem>> = std::iter::repeat_with(|| None).take(transfers.len()).collect();
            let queue = std::sync::Mutex::new(transfers.into_iter().enumerate());
            let out = std::sync::Mutex::new(&mut results);
            // (the tests' injected faults hold for the workers too)
            #[cfg(test)]
            let fault = crate::import_move::injected_fault();
            let work = &|| {
                #[cfg(test)]
                crate::import_move::inject(fault);
                loop {
                    let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
                    let Some((i, t)) = next else { break };
                    let r = one(t);
                    if let Some(slot) = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i) {
                        *slot = r;
                    }
                }
            };
            std::thread::scope(|sc| {
                let handles: Vec<_> =
                    (0..n).filter_map(|i| std::thread::Builder::new().name(format!("lc-import-copy-{i}")).spawn_scoped(sc, work).ok()).collect();
                if handles.is_empty() {
                    // no thread to be had: copy here, one after another
                    work();
                }
                for h in handles {
                    if h.join().is_err() {
                        log::error!("import: a copy worker stopped unexpectedly");
                    }
                }
            });
            return results;
        }
    }
    transfers.into_iter().map(one).collect()
}

/// Copy or move one planned file (and Copy as DNG); its item.
fn run_transfer(t: Transfer, file_bytes: Option<&crate::merge::ByteReader>) -> PreparedItem {
    let Transfer { path, mut info, sidecar, dir, how, dng, .. } = t;
    let mut placed = None;
    let mut stored = match how {
        How::Place { name } => match crate::import_move::place(Path::new(&path), &dir, &name) {
            Ok(pl) => {
                let dst = pl.dst.to_string_lossy().to_string();
                placed = Some(pl);
                dst
            }
            Err(e) => return PreparedItem::Failed(path, e),
        },
        How::Copy { name } => match copy_into(&dir, &path, name.as_deref(), info.content_hash.as_deref()) {
            Ok(p) => p,
            Err(e) => return PreparedItem::Failed(path, e),
        },
    };
    if dng {
        // decoding the raw twice and encoding it takes several times the file's size in memory
        let weight = usize::try_from(info.file_size).unwrap_or(usize::MAX).saturating_mul(6);
        let _permit = crate::memory::work_gate().acquire(weight);
        match crate::cmd::convert::write_dng_with(file_bytes, &stored, String::new()) {
            Ok(dng) => {
                let _ = std::fs::remove_file(&stored);
                stored = dng;
                info.format = "DNG".into();
            }
            Err(e) => log::warn!("import {path}: copy as DNG: {e}"),
        }
    }
    PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed }))
}

/// The catalog half of an import: add the photos [`ImportJob::prepare`] readied (one undoable op;
/// a Move's sources are removed once it is saved). On a failed commit a Move's placed files are
/// taken back.
pub fn commit_prepared(s: &mut Session, opts: &ImportOptions, now: &str, prepared: Prepared) -> crate::Result<ImportReport> {
    let now = now.to_string();
    let mut report = ImportReport { scanned: prepared.scanned, ..Default::default() };
    let mut ops = Vec::new();
    // Move: placed at the destination, sources untouched until the records are committed
    let mut placed: Vec<crate::import_move::Placed> = Vec::new();
    // photos added by this batch, by content (a duplicate of a file earlier in the import names it)
    let mut new_hash: HashMap<String, PhotoId> = HashMap::new();
    // Fresh: trashed photos a new import replaces (by content or by file), each removed once
    let (mut trash_hash, mut trash_path): (HashMap<String, PhotoId>, HashMap<String, PhotoId>) = Default::default();
    if opts.on_deleted == OnDeleted::Fresh {
        for p in s.catalog.photos().filter(|p| p.deleted) {
            if let Some(h) = &p.content_hash {
                trash_hash.insert(h.clone(), p.id);
            }
            if let Source::File { path } = &p.source {
                trash_path.insert(path.clone(), p.id);
            }
        }
    }
    let mut purge: Vec<PhotoId> = Vec::new();
    let mut purged: std::collections::HashSet<PhotoId> = Default::default();
    for it in prepared.items {
        match it {
            PreparedItem::Promote(id) => {
                if s.catalog.photo(id).is_some_and(|p| p.local) {
                    ops.push(Op::SetLocal { id, local: false });
                }
                report.imported.push(id.0);
            }
            PreparedItem::Duplicate { path, existing, reason, hash } => {
                let existing = existing.or_else(|| {
                    let h = hash?;
                    new_hash.get(&h).copied().or_else(|| s.catalog.photos().find(|p| p.content_hash.as_deref() == Some(h.as_str())).map(|p| p.id))
                });
                let trashed = existing.filter(|id| s.catalog.photo(*id).is_some_and(|p| p.deleted));
                match trashed {
                    Some(id) if opts.on_deleted == OnDeleted::Restore => {
                        if !report.restored.contains(&id.0) {
                            ops.push(Op::SetDeleted { id, deleted: false });
                            report.restored.push(id.0);
                        }
                    }
                    _ => report.duplicates.push(Duplicate { path, existing: existing.map(|i| i.0), reason, existing_deleted: trashed.is_some() }),
                }
            }
            PreparedItem::Failed(path, e) => report.failed.push((path, e)),
            PreparedItem::Kept(k) => report.kept.push(k),
            PreparedItem::Ready(r) => {
                let ReadyFile { path, stored, info, sidecar, placed: pl } = *r;
                placed.extend(pl);
                let id = s.catalog.alloc_photo_id();
                if let Some(h) = &info.content_hash {
                    new_hash.insert(h.clone(), id);
                }
                for old in info.content_hash.as_ref().and_then(|h| trash_hash.get(h)).into_iter().chain(trash_path.get(&stored)) {
                    if purged.insert(*old) {
                        purge.push(*old);
                    }
                }
                let sidecar = sidecar.map(|sc| sc.resolve_label(&s.catalog)).filter(|sc| *sc != crate::sidecar::SidecarData::default());
                // a copy is catalogued under its new name
                let name = Path::new(&stored).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                let mut p = Photo::new(id, Source::File { path: stored }, &name, &info.format, info.width, info.height, &now);
                p.kind = info.kind;
                p.file_size = info.file_size;
                p.captured = info.captured;
                p.meta = info.meta;
                p.as_shot_wb = info.as_shot_wb;
                p.content_hash = info.content_hash;
                p.embedded_lens = info.embedded_lens;
                p.preview_only = info.preview_only.clone();
                apply_import_defaults(s, &mut p);
                if let Some(sc) = &sidecar {
                    crate::sidecar::merge_into(&mut p, sc, &now);
                    report.sidecars += 1;
                    report.warnings.extend(sc.warnings.iter().cloned().map(|mut warning| {
                        warning.path = path.clone();
                        warning
                    }));
                }
                if let Some(mp) = opts.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
                    crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
                }
                for k in &opts.keywords {
                    let k = lightcraft_catalog::keywords::clean(k);
                    if !k.is_empty() && !p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
                        p.meta.keywords.push(k);
                    }
                }
                if let Some(preset) = &opts.preset {
                    let d = std::sync::Arc::new(preset.apply(&p.develop, 1.0));
                    let label = format!("Preset: {}", preset.name);
                    p.develop = d.clone();
                    p.edited = Some(now.clone());
                    p.history.push(lightcraft_catalog::HistoryStep { label, settings: d });
                }
                report.imported.push(id.0);
                p.local = opts.local;
                if opts.local {
                    // the state as browsed: a later change keeps the record from being forgotten
                    p.set_local_baseline();
                }
                ops.push(Op::AddPhoto { photo: Box::new(p) });
            }
        }
    }
    // (built against the catalog as it is: one op list for all of them, ahead of the additions)
    if !purge.is_empty() {
        ops.insert(0, s.catalog.delete_photos_permanently_ops(&purge));
    }
    let log0 = s.pending_log.len();
    let (verb, n) = if report.imported.is_empty() && !report.restored.is_empty() {
        ("Restore", report.restored.len())
    } else {
        ("Add", report.imported.len().max(1))
    };
    if !ops.is_empty()
        && let Err(e) = s.commit(&format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }), Op::Batch { ops })
    {
        // nothing was catalogued: take the moves back (their sources were never touched)
        placed.iter().for_each(crate::import_move::rollback);
        return Err(e);
    }
    if !placed.is_empty() {
        finish_moves(s, placed, log0, &mut report);
    }
    Ok(report)
}

/// Move, after the commit: save the library, then remove each source whose destination is
/// still intact. Nothing is removed when the library can't be saved.
fn finish_moves(s: &mut Session, placed: Vec<crate::import_move::Placed>, log0: usize, report: &mut ImportReport) {
    // (saving here clears the pending ops, so the sidecars `execute` would auto-write are written here)
    let auto: Vec<Op> = if s.xmp.auto_write { s.pending_log.get(log0..).map(<[Op]>::to_vec).unwrap_or_default() } else { Vec::new() };
    let before = s.pending_log.len();
    if let Err(e) = s.persist() {
        for p in placed {
            let reason = format!("the library could not be saved ({e}): the original stays; the photo uses the copy at {}", p.dst.display());
            report.kept.push(Kept { path: p.src.to_string_lossy().to_string(), reason });
        }
        return;
    }
    for p in placed {
        let from = p.src.to_string_lossy().to_string();
        match crate::import_move::finish(&p) {
            Ok(kept) => {
                let sidecars = p.sidecars.iter().map(|sc| sc.dst.to_string_lossy().to_string()).collect();
                report.moved.push(Moved { from, to: p.dst.to_string_lossy().to_string(), sidecars });
                report.kept.extend(kept.into_iter().map(|(path, reason)| Kept { path, reason }));
            }
            Err(reason) => {
                log::warn!("import: move {from}: {reason}");
                report.kept.push(Kept { path: from, reason });
            }
        }
    }
    if s.pending_log.len() < before && s.interaction.is_none() && !auto.is_empty() {
        s.auto_write_sidecars(&auto);
    }
}

/// The current local time as ISO 8601 (`YYYY-MM-DDTHH:MM:SS`, UTC on targets without a clock
/// offset); for [`Session::clock`] on native hosts.
pub fn system_clock() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        civil(secs)
    }
    #[cfg(target_arch = "wasm32")]
    {
        "2026-01-01T00:00:00".to_string()
    }
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SS` (UTC), proleptic Gregorian.
pub fn civil(secs: i64) -> String {
    lightcraft_catalog::dates::civil(secs)
}

impl Session {
    /// A thumbnail for an import candidate (not in the catalog): a raw's embedded preview, else a
    /// small render of the file. `slot` makes the job's photo id unique (`u64::MAX - slot`).
    pub fn candidate_thumb_job(&mut self, c: &ImportCandidate, edge: usize, slot: u64) -> Option<crate::media::QuickJob> {
        if c.error.is_some() {
            return None;
        }
        let id = PhotoId(u64::MAX - slot);
        let mut p = Photo::new(id, Source::File { path: c.path.clone() }, &c.name, &c.format, c.width.max(1), c.height.max(1), "");
        p.kind = c.kind;
        if let Some(info) = self.import_probes.get(&c.path) {
            p.as_shot_wb = info.as_shot_wb;
            p.embedded_lens = info.embedded_lens;
            p.preview_only = info.preview_only.clone();
        }
        p.develop = std::sync::Arc::new(p.import_defaults());
        let edge = edge.clamp(64, crate::media::SourceLevel::Thumb.max_edge());
        let level = crate::media::SourceLevel::Thumb;
        let source = self.media.origin_ref(&p.source, level.max_edge());
        let key = lightcraft_preview::Hasher128::new().str(&c.path).u64(c.file_size).u64(edge as u64).finish().0 as u64;
        let small = crate::media::RenderJob {
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: None,
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: crate::media::source_info(&p),
            settings: p.develop.clone(),
            request: lightcraft_pipeline::RenderRequest::fit(edge, edge),
            key,
            cache: None,
            stages: None,
            view_cache: None,
        };
        let embedded = match (&self.media.preview_loader, c.kind) {
            (Some(l), MediaKind::Raw) => Some((c.path.clone(), l.clone(), edge)),
            _ => None,
        };
        Some(crate::media::QuickJob { request_id: 0, photo: id, key, cached: Vec::new(), embedded, small: Some(Box::new(small)) })
    }

    /// Use the system clock for import/edit times (native hosts).
    pub fn with_system_clock(mut self) -> Self {
        self.clock = Box::new(system_clock);
        self
    }
}

#[cfg(test)]
mod prepared_tests {
    use super::*;

    #[test]
    fn revalidate_add_only_converts_matching_ready_item() {
        let mut s = Session::new();
        let existing_path = std::env::temp_dir().join("lightcraft-revalidate-existing.jpg");
        let other_path = std::env::temp_dir().join("lightcraft-revalidate-other.jpg");
        let existing = Photo::new(PhotoId(1), Source::File { path: existing_path.to_string_lossy().into() }, "existing.jpg", "JPG", 1, 1, "now");
        s.commit("existing", Op::AddPhoto { photo: Box::new(existing) }).unwrap();
        let ready = |path: &Path| ReadyFile {
            path: path.to_string_lossy().into(),
            stored: path.to_string_lossy().into(),
            info: ProbeInfo { format: "JPG".into(), ..ProbeInfo::default() },
            sidecar: None,
            placed: None,
        };
        let mut prepared = Prepared {
            scanned: 2,
            items: vec![PreparedItem::Ready(Box::new(ready(&existing_path))), PreparedItem::Ready(Box::new(ready(&other_path)))],
        };
        prepared.revalidate_add(&s);
        assert!(matches!(prepared.items.first(), Some(PreparedItem::Duplicate { reason: "path", .. })));
        assert!(matches!(prepared.items.get(1), Some(PreparedItem::Ready(_))));
    }
}
