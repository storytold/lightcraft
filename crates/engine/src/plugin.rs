//! Plug-ins: WebAssembly modules that edit the library through the same commands as the menus,
//! the CLI, the control channel and the MCP server, inside the sandbox of the shared
//! `craft-plugin` crate (RFC-0046). This module is LightCraft's side of it: which commands a
//! plug-in may call ([`policy`], [`refusal`]), one undo step per run (rolled back when the run
//! fails), read-only access checked after every call, each plug-in's settings (kept in the user's
//! settings folder) and the installed plug-ins (the user's `Plug-ins` folder, or this session
//! only). The `plugin.*` commands are in `cmd::plugins`. See `docs/plugins.md`.
//!
//! A run works on the open library and the photos it names or the selection; it never gets a
//! file, a folder, another library, the undo history, the clipboard or the person's presets and
//! preferences (see [`ALLOWED`]).

mod host;
#[cfg(not(target_arch = "wasm32"))]
mod net;
mod runtime;

use std::sync::OnceLock;

pub use craft_plugin;
use craft_plugin::fetch::{self, Fetch};
use craft_plugin::{API_VERSION, AppInfo, Instance, KIND_COMMAND, Limits, Manifest, Policy, RunOutput, Source, UiChange, bundle, catalog, github};
use serde_json::{Map, Value, json};

use crate::cmd::bad;
use crate::{EngineError, LibrarySource, Result, Selection, Session, UndoEntry};
use host::EngineHost;
pub use runtime::{InstalledPlugin, PluginPrefs, Runtime};

/// LightCraft as manifests name it: command plug-ins of every shared ABI version, and only
/// manifests that name `lightcraft` in `apps`.
pub const APP: AppInfo<'static> = AppInfo {
    id: "lightcraft",
    version: env!("CARGO_PKG_VERSION"),
    api: 1..=API_VERSION,
    kinds: &[KIND_COMMAND],
    accept_legacy: false,
    kind_api: &[],
};

/// What one plug-in may keep in its settings (keys and JSON values, in bytes), and its longest key.
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
pub const MAX_SETTING_KEY: usize = 128;
/// Catalogs a person may add.
pub const MAX_CATALOGS: usize = 32;
/// Undo steps one run may make. The history keeps its last 1000 steps
/// ([`Session::commit`]); a run stays well inside that, so every step it made can still be put
/// back when it fails, and folded into its one step when it succeeds.
pub const MAX_RUN_STEPS: u64 = 500;
/// Messages a run may show (`notify`), and their length in characters.
const MAX_NOTES: usize = 20;
const MAX_NOTE_CHARS: usize = 1000;
/// Least instruction budget `plugin.run {fuel}` may ask for.
const MIN_FUEL: u64 = 1000;
/// The activity stack's kind and label for a plug-in download (English; the frontend translates it).
pub const DOWNLOAD_KIND: &str = "download";
pub const DOWNLOAD_LABEL: &str = "Downloading a plug-in";

/// The only commands a plug-in may call: everything else is refused (`*` in [`policy`]), so a
/// command added to LightCraft later is refused until someone decides it is safe here.
///
/// What is on it edits the library the person has open, through undoable steps, on the photos a
/// call names (`ids` / `id`) or the selection: ratings, flags, labels, metadata, keywords,
/// albums, stacks, develop settings, masks, spots, versions; it selects photos and changes the
/// view; and it reads (queries). What is not, and why:
/// - files and folders on disk: import, export, print, slideshow export, rename, move, delete,
///   relink, reload, DNG conversion, XMP sidecars, external editors, folder commands
///   (`library.*` but the view, `folder.*`, `photo.delete*`, `photo.rename`, `photo.moveToFolder`…);
/// - the library itself and where it lives (`library.info`, `library.compact`, previews, caches);
/// - the undo history (`edit.*`, `history.clear`) and the interactive drags (`develop.*Interaction`),
///   which would split a run into several steps;
/// - the person's own things kept outside one photo: the develop and metadata clipboards,
///   presets, profiles, effect presets, keyword and label sets, the target album, Auto Sync,
///   preferences (`prefs.*`, `app.*`);
/// - downloads, models and long background work (`denoise.*`, `faces.*`, `segment.*`, `merge.*`,
///   `lens.*`, `photo.analyze`, `metadata.reverseGeocode`), and the activity stack;
/// - commands that change every photo that has something, not the ones asked for (renaming,
///   merging, moving or deleting a keyword or a person; `keyword.purgeUnused`; `stack.auto`, which
///   stacks the whole view when one photo is selected; expanding or collapsing every stack).
pub const ALLOWED: &[&str] = &[
    // reading
    "catalog.query",
    "catalog.stats",
    "photo.inspect",
    "develop.get",
    "develop.controls",
    "albums.list",
    "album.ruleFields",
    "presets.list",
    "profiles.list",
    "profiles.menu",
    "keyword.list",
    "keyword.info",
    "keyword.suggest",
    "label.names",
    "history.list",
    "metadata.presets",
    "filter.presets",
    "curve.presets",
    "library.state",
    "library.groups",
    // the selection and the view
    "library.select",
    "library.selectAll",
    "library.selectNone",
    "library.selectBy",
    "library.next",
    "library.previous",
    "library.up",
    "library.down",
    "library.source",
    "library.filter",
    "library.clearFilter",
    "library.sort",
    // culling and metadata
    "photo.rate",
    "photo.increaseRating",
    "photo.decreaseRating",
    "photo.flag",
    "photo.pick",
    "photo.reject",
    "photo.unflag",
    "photo.cycleFlag",
    "photo.increaseFlag",
    "photo.decreaseFlag",
    "photo.label",
    "photo.clearLabel",
    "photo.rotateLeft",
    "photo.rotateRight",
    "photo.flipHorizontal",
    "photo.flipVertical",
    "photo.setMeta",
    "photo.setCaptureTime",
    "photo.setRegion",
    "photo.removeRegion",
    "photo.virtualCopy",
    "metadata.applyPreset",
    "keyword.create",
    "keyword.edit",
    // albums and stacks
    "album.create",
    "album.createSmart",
    "album.addPhotos",
    "album.removePhotos",
    "album.rename",
    "album.setRules",
    "album.setCover",
    "stack.group",
    "stack.ungroup",
    "stack.split",
    "stack.remove",
    "stack.setTop",
    "stack.moveUp",
    "stack.moveDown",
    "stack.toggle",
    // develop settings
    "develop.set",
    "develop.adjust",
    "develop.quickAdjust",
    "develop.auto",
    "develop.autoBwMix",
    "develop.wb",
    "develop.wbPick",
    "develop.curve",
    "develop.profile",
    "develop.treatment",
    "develop.reset",
    "develop.resetControl",
    "develop.resetSection",
    "develop.sectionEnabled",
    "develop.sync",
    "develop.merge",
    "develop.hdr",
    "develop.targeted",
    "develop.matchExposure",
    "develop.updateProcess",
    "preset.apply",
    "filter.applyPreset",
    "curve.applyPreset",
    "curve.reset",
    "crop.set",
    "crop.aspect",
    "crop.rotateAspect",
    "crop.straighten",
    "crop.autoStraighten",
    "crop.reset",
    "geometry.upright",
    "geometry.guides",
    "pointColor.pick",
    "pointColor.delete",
    "lensBlur.brushStroke",
    "lensBlur.brushReset",
    "lensBlur.findSubject",
    // masks, spots, red eye
    "mask.add",
    "mask.addComponent",
    "mask.adjust",
    "mask.applyEffect",
    "mask.brushStroke",
    "mask.component",
    "mask.delete",
    "mask.deleteAll",
    "mask.duplicate",
    "mask.invert",
    "mask.move",
    "mask.rename",
    "mask.select",
    "mask.update",
    "mask.visible",
    "mask.refine",
    "mask.refineDetail",
    "mask.sampleColor",
    "spot.add",
    "spot.delete",
    "spot.select",
    "spot.update",
    "spot.refreshSource",
    "spot.findBlemishes",
    "spot.findDust",
    "redeye.add",
    "redeye.catchlight",
    "redeye.delete",
    // versions and history steps
    "version.create",
    "version.rename",
    "version.restore",
    "version.update",
    "version.delete",
    "history.restore",
    "history.previous",
    "history.next",
];

/// Parameter names that name a file or folder on disk, at any depth. No allowed command needs
/// one; `library.source` and `library.filter` take a library folder's `path` / `libraryFolder`,
/// which a plug-in learns no other way and is refused here like any path.
const FILE_KEYS: &[&str] = &["path", "paths", "file", "files", "dir", "folder", "destination", "libraryFolder"];

/// The rules every plug-in run follows: [`craft_plugin::ALWAYS_DENIED`], and every command but
/// [`ALLOWED`].
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| Policy::new(&["*"], ALLOWED))
}

/// Why plug-in `m` may not call `command` with `params`, before it runs: a refused command, one
/// outside its manifest's `permissions.commands`, or parameters that name a file.
pub fn refusal(m: &Manifest, command: &str, params: &Value) -> Option<String> {
    if let Err(why) = craft_plugin::check_call(m, policy(), command) {
        return Some(why);
    }
    if file_key_inside(params, 0) {
        return Some(format!(
            "plug-ins can't name files or folders (`{command}` was given one of {}): work on the photos in the library",
            FILE_KEYS.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        ));
    }
    None
}

/// Whether `v` holds one of [`FILE_KEYS`] at any depth.
fn file_key_inside(v: &Value, depth: usize) -> bool {
    // serde_json refuses nesting past 128, so nothing deeper reaches here
    depth < 128
        && match v {
            Value::Object(m) => m.iter().any(|(k, v)| FILE_KEYS.contains(&k.as_str()) || file_key_inside(v, depth + 1)),
            Value::Array(a) => a.iter().any(|v| file_key_inside(v, depth + 1)),
            _ => false,
        }
}

/// A plug-in error as the engine reports it.
fn failed(name: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("{name}: {e}"))
}

/// Store (`Some`) or forget (`None`) plug-in `id`'s setting `key` in `all`, within
/// [`MAX_SETTINGS_BYTES`] and [`MAX_SETTING_KEY`].
fn set_setting(
    all: &mut std::collections::BTreeMap<String, Map<String, Value>>,
    id: &str,
    key: &str,
    value: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(text) = value else {
        if let Some(mine) = all.get_mut(id) {
            mine.remove(key);
            if mine.is_empty() {
                all.remove(id);
            }
        }
        return Ok(());
    };
    if key.is_empty() || key.chars().count() > MAX_SETTING_KEY || key.chars().any(char::is_control) {
        return Err(format!("a setting's key must be 1–{MAX_SETTING_KEY} characters without control characters"));
    }
    let too_big = || format!("a plug-in's settings are limited to {} KB", MAX_SETTINGS_BYTES / 1024);
    if key.len().saturating_add(text.len()) > MAX_SETTINGS_BYTES {
        return Err(too_big());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("a setting must be JSON: {e}"))?;
    let others: usize = all.get(id).map_or(0, |mine| {
        mine.iter().filter(|(k, _)| k.as_str() != key).map(|(k, v)| k.len().saturating_add(v.to_string().len())).fold(0, usize::saturating_add)
    });
    if others.saturating_add(key.len()).saturating_add(v.to_string().len()) > MAX_SETTINGS_BYTES {
        return Err(too_big());
    }
    all.entry(id.to_string()).or_default().insert(key.to_string(), v);
    Ok(())
}

// ---------- running ----------

/// The photos a plug-in sees as the selection: their ids (the active one, when there is one, is
/// also the run's `active`).
fn selection_json(s: &Session) -> Value {
    json!(s.selected().iter().map(|id| id.0).collect::<Vec<_>>())
}

/// What a run changes outside the catalog and puts back when it fails: the selection and the
/// view. (Its catalog edits are undo steps of its own, undone with the app's undo.)
struct View {
    selection: Selection,
    source: LibrarySource,
    filter: lightcraft_catalog::Filter,
    sort: lightcraft_catalog::Sort,
    active_mask: Option<u32>,
    active_spot: Option<usize>,
    previous_active: Option<lightcraft_catalog::PhotoId>,
}

impl View {
    fn take(s: &Session) -> View {
        View {
            selection: s.selection.clone(),
            source: s.source,
            filter: s.filter.clone(),
            sort: s.sort,
            active_mask: s.active_mask,
            active_spot: s.active_spot,
            previous_active: s.previous_active,
        }
    }

    fn restore(self, s: &mut Session) {
        s.selection = self.selection;
        s.source = self.source;
        s.filter = self.filter;
        s.sort = self.sort;
        s.active_mask = self.active_mask;
        s.active_spot = self.active_spot;
        s.previous_active = self.previous_active;
    }
}

/// Undo every step in `s.undo` (during a run it holds the run's steps only), newest first, with
/// the app's own undo. An error when one can't be undone (its files couldn't be moved back);
/// the steps after it are then left in place.
fn undo_all(s: &mut Session) -> std::result::Result<(), String> {
    while !s.undo.is_empty() {
        s.undo_step().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// What a run sets aside while it runs: the library's undo and redo history, the selection and
/// the view, and the plug-ins' settings.
struct Saved {
    view: View,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    settings: std::collections::BTreeMap<String, Map<String, Value>>,
}

impl Saved {
    /// Set them aside, leaving the run an empty history to record its steps in.
    fn take(s: &mut Session) -> Saved {
        Saved {
            view: View::take(s),
            undo: std::mem::take(&mut s.undo),
            redo: std::mem::take(&mut s.redo),
            settings: s.plugins.prefs.settings.clone(),
        }
    }

    /// Drop what the run's own calls undid (a read-only plug-in's refused edits): it isn't the
    /// person's to redo.
    fn drop_redo(s: &mut Session, redo: Vec<UndoEntry>) {
        for mut dropped in redo {
            s.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
        }
    }

    /// The run failed: undo its steps with the app's undo, and put everything back.
    fn put_back(self, s: &mut Session) -> std::result::Result<(), String> {
        let undone = undo_all(s);
        let run_redo = std::mem::replace(&mut s.redo, self.redo);
        Saved::drop_redo(s, run_redo);
        s.undo = self.undo;
        self.view.restore(s);
        s.plugins.prefs.settings = self.settings;
        undone
    }

    /// The run succeeded: its steps become one step of the library's history, named `label`.
    fn fold(self, s: &mut Session, label: String) {
        let steps = std::mem::replace(&mut s.undo, self.undo);
        let run_redo = std::mem::take(&mut s.redo);
        Saved::drop_redo(s, run_redo);
        if steps.is_empty() {
            s.redo = self.redo;
        } else {
            let mut ops: Vec<lightcraft_catalog::Op> = steps.into_iter().rev().map(|e| e.op).collect();
            let op = if ops.len() == 1 { ops.remove(0) } else { lightcraft_catalog::Op::Batch { ops } };
            s.undo.push(UndoEntry { label, op, folder: None, retry_inverse: Vec::new(), retry_sidecars: Vec::new() });
            if s.undo.len() > 1000 {
                let mut dropped = s.undo.remove(0);
                s.pending_sidecar_repairs.append(&mut dropped.retry_sidecars);
            }
            // a new edit: what could be redone is gone, as after any edit (Session::commit)
            Saved::drop_redo(s, self.redo);
        }
        if s.plugins.prefs.settings != self.settings {
            s.plugins.save_prefs();
        }
    }
}

/// Run plug-in `id`'s `command` (default: its first) with `params` on the open library, as one
/// undo step named after the plug-in. A run that fails (a trap, an exhausted budget, a refusal it
/// passes on, a non-zero status) leaves the library, the selection and the view, and the undo
/// history as they were.
pub fn run(s: &mut Session, id: &str, command: Option<&str>, params: &Value, fuel: Option<u64>) -> Result<Value> {
    let cmd = "plugin.run";
    if s.interaction.is_some() {
        return Err(bad(cmd, "a slider drag or a brush stroke is under way: finish it first"));
    }
    let p = s.plugins.get(id).cloned().ok_or_else(|| bad(cmd, format!("no plug-in `{id}` is installed (see plugin.list)")))?;
    let m = &p.manifest;
    let api = m.target(&APP).map_err(|e| bad(cmd, format!("{} can't run here: {e}", m.name)))?.api();
    let c = m.command(command).ok_or_else(|| bad(cmd, format!("{} has no command `{}`", m.name, command.unwrap_or_default())))?;
    let params = c.resolve_params(params).map_err(|e| bad(cmd, e))?;
    let mut limits = Limits::default();
    if let Some(f) = fuel {
        limits.fuel = f.clamp(MIN_FUEL, limits.fuel);
    }
    let mut instance = Instance::new(&p.wasm, &limits).map_err(|e| failed(&m.name, e))?;

    let input =
        json!({"api": api, "command": c.id, "params": params, "selection": selection_json(s), "active": s.active().map(|id| id.0)}).to_string();
    s.plugins.last = Some((m.id.clone(), c.id.clone(), params));
    let label = if m.commands.len() > 1 { format!("{}: {}", m.name, c.label.trim_end_matches(['…', '.'])) } else { m.name.clone() };

    // The run's commands record their steps in an empty history; on success they become one step
    // of the library's own history ([`Saved::fold`]), on failure they are undone ([`Saved::put_back`])
    let saved = Saved::take(s);
    let start = s.commits();
    let mut host = EngineHost::new(s, m, start);
    let ran = crate::guard::catch(&format!("plug-in {}", m.name), || instance.call(&input, &mut host));
    let notes = host.finish();
    let ran: std::result::Result<RunOutput, String> = match ran {
        // (no allowed command moves files; should one ever, its step isn't folded into another)
        Ok(Ok(_)) if s.undo.iter().any(|e| e.folder.is_some() || !e.retry_inverse.is_empty() || !e.retry_sidecars.is_empty()) => {
            Err("the run moved files on disk, which plug-ins may not do".into())
        }
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e.to_string()),
        Err(panicked) => Err(panicked),
    };
    let out = match ran {
        Ok(out) => out,
        Err(e) => {
            return Err(match saved.put_back(s) {
                Ok(()) => failed(&m.name, e),
                Err(u) => failed(&m.name, format!("{e}; not everything it changed could be put back: {u}")),
            });
        }
    };
    saved.fold(s, label);

    let output = serde_json::from_str::<Value>(&out.output).unwrap_or(Value::String(out.output));
    // Panels (`ui_show`) aren't shown in LightCraft yet: say so rather than drop it silently
    let panel = matches!(out.ui, Some(UiChange::Show(_))).then_some("LightCraft doesn't show plug-in panels yet");
    Ok(
        json!({"plugin": m.id, "command": c.id, "output": output, "notes": notes, "logs": out.logs, "calls": out.calls, "fuel": out.fuel, "panel": panel}),
    )
}

// ---------- installing ----------

/// Where a bundle to install comes from (`plugin.install`'s parameters).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// A `.craftplugin` file on this computer.
    Path(String),
    /// The bundle itself (base64 in the parameters), with a name to remember it by.
    Data { bytes: Vec<u8>, name: String },
    /// A GitHub repository's release: `owner/repo` or `owner/repo@tag`.
    Repo(String),
    /// A catalog's package (its newest release for LightCraft, or `version`).
    Catalog { url: String, id: String, version: Option<String> },
}

impl Request {
    /// The source `p` names: `path`, `data` (base64), `repo` or `catalog` + `id`.
    pub fn parse(cmd: &str, p: &Value) -> Result<Request> {
        let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
        if let Some(path) = text("path") {
            return Ok(Request::Path(path.into()));
        }
        if let Some(data) = text("data") {
            if data.len() > bundle::MAX_BUNDLE_BYTES / 3 * 4 + 4 {
                return Err(bad(cmd, format!("the bundle is larger than {} MB", bundle::MAX_BUNDLE_BYTES >> 20)));
            }
            let bytes = base64_decode(data).ok_or_else(|| bad(cmd, "`data` isn't base64"))?;
            return Ok(Request::Data { bytes, name: text("name").unwrap_or("data").chars().take(200).collect() });
        }
        if let Some(spec) = text("repo") {
            return Ok(Request::Repo(spec.into()));
        }
        if let Some(url) = text("catalog") {
            let id = text("id").ok_or_else(|| bad(cmd, "`id` required with `catalog` (a package of the catalog)"))?;
            return Ok(Request::Catalog { url: url.into(), id: id.into(), version: text("version").map(str::to_string) });
        }
        Err(bad(cmd, "pass `path`, `data` (base64), `repo` (owner/repo or owner/repo@tag) or `catalog` with `id`"))
    }

    /// Whether getting the bundle goes over the network (a repository, or a catalog that isn't a
    /// local `file://` one).
    pub fn needs_network(&self) -> bool {
        match self {
            Request::Repo(_) => true,
            Request::Catalog { url, .. } => !url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("file://")),
            Request::Path(_) | Request::Data { .. } => false,
        }
    }
}

/// A bundle obtained for installing: its bytes, where it came from and, for a repository, whether
/// a catalog in use lists it.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub bytes: Vec<u8>,
    pub source: Source,
    /// `Some(true)` from a catalog; for a repository, whether a catalog in use lists it.
    pub in_catalog: Option<bool>,
}

/// A bundle checked and shown for review (`plugin.install {review: true}`), kept so the install
/// that follows doesn't download it again.
pub(crate) struct Pending {
    pub sha256: String,
    /// The `plugin.install` parameters that named it.
    pub request: Value,
    pub candidate: Candidate,
}

/// Read the bundle `req` names, downloading over `f` (catalogs in use: `catalogs`, to tell
/// whether a repository is listed in one). Needs no session, so a frontend can run it on a
/// worker thread ([`plugin_fetch`] makes `f`) and hand the result to [`hold`].
pub fn obtain_with(req: &Request, f: &mut dyn Fetch, catalogs: &[String]) -> std::result::Result<Candidate, String> {
    match req {
        Request::Path(path) => {
            let bytes = read_bundle_file(path).map_err(|e| format!("{path}: {e}"))?;
            Ok(Candidate { bytes, source: Source::File { path: path.clone() }, in_catalog: None })
        }
        Request::Data { bytes, name } => Ok(Candidate { bytes: bytes.clone(), source: Source::File { path: name.clone() }, in_catalog: None }),
        Request::Repo(spec) => {
            let repo = github::parse_repo_spec(spec)?;
            let plan = fetch::resolve_repo(f, &repo)?;
            let bytes = fetch::download_repo(f, &plan)?;
            let source = Source::Github { repo: plan.repo.slug(), tag: plan.repo.tag.clone().unwrap_or_default(), asset: plan.asset.clone() };
            let in_catalog = catalogs.iter().any(|url| {
                fetch::fetch_catalog(f, url)
                    .is_ok_and(|(cat, _)| cat.packages.iter().any(|pkg| pkg.repo_ref().is_some_and(|r| r.same_repo(&repo.owner, &repo.name))))
            });
            Ok(Candidate { bytes, source, in_catalog: Some(in_catalog) })
        }
        Request::Catalog { url, id, version } => {
            let (cat, _) = fetch::fetch_catalog(f, url)?;
            let package = cat.package(id).ok_or_else(|| format!("{url} has no package `{id}`"))?;
            let release = match version {
                Some(v) => package.release(v).ok_or_else(|| format!("{id} has no version {v} in {url}"))?,
                None => package.latest_for(&APP).ok_or_else(|| format!("{id} has no release for LightCraft {}", APP.version))?,
            };
            release.target(&package.kind, &APP)?;
            let bytes = fetch::download_release(f, url, package, release)?;
            let (_, manifest) = bundle::open(&bytes)?;
            package.check_manifest(release, &manifest)?;
            Ok(Candidate { bytes, source: Source::Catalog { catalog: url.clone(), package: id.clone() }, in_catalog: Some(true) })
        }
    }
}

/// [`obtain_with`] from a command: a download is listed in the activity stack while it runs (and
/// can be cancelled there).
pub(crate) fn obtain(s: &Session, cmd: &str, req: &Request) -> Result<Candidate> {
    let catalogs = s.plugins.prefs.catalogs.clone();
    if !req.needs_network() {
        let mut f = plugin_fetch(s.plugins.network, std::sync::Arc::default());
        return obtain_with(req, f.as_mut(), &catalogs).map_err(|e| bad(cmd, e));
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _task = s.activity.start(DOWNLOAD_KIND, DOWNLOAD_LABEL, crate::activity::Cancel::Flag(cancel.clone()));
    let mut f = plugin_fetch(s.plugins.network, cancel);
    obtain_with(req, f.as_mut(), &catalogs).map_err(|e| bad(cmd, e))
}

/// The HTTPS client plug-in installs download with: LightCraft's own (`lightcraft-fetch`), which
/// stops once `cancel` is set, when `network` (a session's [`Runtime::network`]); else, and on
/// the web, one that refuses (local files and `file://` catalogs still read).
pub fn plugin_fetch(network: bool, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Box<dyn Fetch + Send> {
    #[cfg(not(target_arch = "wasm32"))]
    if network {
        return Box::new(net::NetFetch::new(cancel));
    }
    let _ = (network, cancel);
    Box::new(NoNetwork)
}

/// Longest part of a URL an error repeats.
const SHOWN_URL_CHARS: usize = 120;

/// The fetcher of sessions without network access: local files and `file://` catalogs still read.
struct NoNetwork;

impl Fetch for NoNetwork {
    fn get(&mut self, url: &str, _max: usize) -> std::result::Result<Option<Vec<u8>>, String> {
        let shown: String = url.chars().take(SHOWN_URL_CHARS).collect();
        Err(format!("there is no network access here to read {shown}: install the plug-in from a file"))
    }
}

/// The bundle file at `path`, refused past [`bundle::MAX_BUNDLE_BYTES`] before it is read whole.
#[cfg(not(target_arch = "wasm32"))]
fn read_bundle_file(path: &str) -> std::result::Result<Vec<u8>, String> {
    crate::read_file::read_regular(path, bundle::MAX_BUNDLE_BYTES as u64).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
fn read_bundle_file(_path: &str) -> std::result::Result<Vec<u8>, String> {
    Err("the browser has no file paths: pass the bundle as `data` (base64)".into())
}

/// What an install confirmation shows about `c`: the manifest's permissions, kind and commands,
/// the source, the SHA-256, whether a catalog lists it and what it replaces. An error when the
/// bundle isn't a plug-in LightCraft can run.
pub(crate) fn review(s: &Session, cmd: &str, c: &Candidate) -> Result<(Manifest, Value)> {
    let (_, manifest) = bundle::open(&c.bytes).map_err(|e| bad(cmd, e))?;
    manifest.target(&APP).map_err(|e| bad(cmd, format!("{} can't be installed: {e}", manifest.name)))?;
    c.source.check().map_err(|e| bad(cmd, e))?;
    let old = s.plugins.get(&manifest.id);
    let mut v = serde_json::to_value(&manifest).unwrap_or_else(|_| json!({"id": manifest.id}));
    v["kind"] = json!(manifest.kind);
    v["sha256"] = json!(bundle::sha256_hex(&c.bytes));
    v["source"] = serde_json::to_value(&c.source).unwrap_or(Value::Null);
    v["inCatalog"] = json!(c.in_catalog);
    v["replaces"] = json!(old.map(|o| &o.manifest.version));
    v["permissionsChanged"] = json!(old.is_some_and(|o| craft_plugin::permissions::changed(&o.manifest, &manifest)));
    v["stored"] = json!(s.plugins.has_store());
    Ok((manifest, v))
}

/// Install `c` (already reviewed): into the user's folder when the session has one, else for the
/// session. Replaces an installed plug-in with the same id; its settings stay.
pub(crate) fn install(s: &mut Session, cmd: &str, c: Candidate) -> Result<InstalledPlugin> {
    let (manifest, _) = review(s, cmd, &c)?;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(store) = &s.plugins.store {
        let (i, _) = store.install_for(&APP, &c.bytes, c.source).map_err(|e| bad(cmd, e))?;
        let plugin = InstalledPlugin {
            manifest: i.manifest,
            sha256: i.record.sha256,
            source: i.record.source,
            stored: true,
            wasm: std::sync::Arc::new(i.bundle.wasm),
        };
        return Ok(s.plugins.keep(plugin));
    }

    let sha256 = bundle::sha256_hex(&c.bytes);
    let (b, _) = bundle::open(&c.bytes).map_err(|e| bad(cmd, e))?;
    Ok(s.plugins.keep(InstalledPlugin { manifest, sha256, source: c.source, stored: false, wasm: std::sync::Arc::new(b.wasm) }))
}

/// Keep `c`, named by the `plugin.install` parameters `request` (without `review` and `sha256`),
/// for the install that follows its review (one at a time). Returns its SHA-256, which that
/// install passes. A frontend that downloaded `c` itself ([`obtain_with`]) hands it over here.
pub fn hold(s: &mut Session, request: Value, c: Candidate) -> String {
    let sha256 = bundle::sha256_hex(&c.bytes);
    s.plugins.pending = Some(Pending { sha256: sha256.clone(), request: install_request(&request), candidate: c });
    sha256
}

/// `plugin.install`'s parameters as [`hold`] and the install after a review compare them.
pub fn install_request(p: &Value) -> Value {
    let mut request: Map<String, Value> = p.as_object().cloned().unwrap_or_default();
    request.remove("review");
    request.remove("sha256");
    Value::Object(request)
}

/// The bundle reviewed last, when it is `sha256` and `request` named it.
pub(crate) fn take_held(s: &mut Session, sha256: &str, request: &Value) -> Option<Candidate> {
    let held = s.plugins.pending.take_if(|h| h.sha256.eq_ignore_ascii_case(sha256) && h.request == *request)?;
    Some(held.candidate)
}

/// Remove plug-in `id` and its settings; whether it was installed.
pub(crate) fn remove(s: &mut Session, cmd: &str, id: &str) -> Result<bool> {
    let Some(p) = s.plugins.get(id).cloned() else { return Ok(false) };
    #[cfg(not(target_arch = "wasm32"))]
    if p.stored
        && let Some(store) = &s.plugins.store
    {
        store.remove(&p.manifest.id).map_err(|e| bad(cmd, e))?;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = cmd;
    s.plugins.forget(&p.manifest.id);
    s.plugins.save_prefs();
    Ok(true)
}

/// The catalog `url`'s packages LightCraft can run, with their newest release for it (and the
/// version installed here).
pub(crate) fn catalog_packages(s: &Session, f: &mut dyn Fetch, url: &str) -> std::result::Result<(Vec<Value>, Vec<String>), String> {
    let (mut packages, warnings) = catalog_packages_with(f, url)?;
    for pkg in &mut packages {
        let installed = pkg["id"].as_str().and_then(|id| s.plugins.get(id)).map(|p| p.manifest.version.clone());
        pkg["installed"] = json!(installed);
    }
    Ok((packages, warnings))
}

/// [`catalog_packages`] without a session (`installed` is left out), for a frontend's worker
/// thread.
pub fn catalog_packages_with(f: &mut dyn Fetch, url: &str) -> std::result::Result<(Vec<Value>, Vec<String>), String> {
    let (cat, warnings) = fetch::fetch_catalog(f, url)?;
    let packages = cat
        .packages
        .iter()
        .filter_map(|pkg| {
            let release = pkg.latest_for(&APP)?;
            Some(json!({
                "catalog": url, "id": pkg.id, "name": pkg.name, "version": release.version, "author": pkg.author,
                "description": pkg.description, "license": pkg.license, "repo": pkg.repo, "kind": pkg.kind,
                "sha256": release.sha256,
            }))
        })
        .collect();
    Ok((packages, warnings))
}

/// Whether `url` is a catalog address LightCraft reads (`https://` on GitHub's hosts, or
/// `file://`), or why not.
pub(crate) fn check_catalog_url(url: &str) -> std::result::Result<(), String> {
    catalog::check_url(url, None).map(|_| ())
}

/// Where plug-in `p` has a newer release: the install parameters that get it and its version (a
/// tag, for a repository). `None` when it is up to date or came from a file.
pub(crate) fn update_for(f: &mut dyn Fetch, p: &InstalledPlugin) -> std::result::Result<Option<(Value, String)>, String> {
    match &p.source {
        Source::Catalog { catalog: url, package: id } => {
            let (cat, _) = fetch::fetch_catalog(f, url)?;
            let release = cat.package(id).and_then(|pkg| pkg.latest_for(&APP));
            Ok(release
                .filter(|r| bundle::newer(&r.version, &p.manifest.version))
                .map(|r| (json!({"catalog": url, "id": id, "version": r.version}), r.version.clone())))
        }
        Source::Github { repo, tag: installed, .. } => {
            let mut repo = github::parse_repo_spec(repo)?;
            repo.tag = None;
            let plan = fetch::resolve_repo(f, &repo)?;
            let tag = plan.repo.tag.clone().unwrap_or_default();
            Ok((tag != *installed).then(|| (json!({"repo": format!("{}@{tag}", plan.repo.slug())}), tag)))
        }
        Source::File { .. } | Source::Other { .. } => Ok(None),
    }
}

/// Standard base64 (padding optional; whitespace ignored), or `None`.
pub(crate) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut ended = false;
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                ended = true;
                continue;
            }
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        if ended {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
