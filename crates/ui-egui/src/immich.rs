//! File ▸ Import from Immich…: browse a self-hosted [Immich](https://immich.app) server and
//! import the chosen assets into the library.
//!
//! The dialog is the state (filters, the browsed page, the selection — it travels with
//! [`crate::state::Dialog`] so it serializes with the UI state); its workers are the app-level
//! [`ImmichTask`], like the import review's [`crate::import::ImportTask`]: every network call runs
//! on a worker thread ([`lightcraft_immich`], native only) and its results arrive over a channel
//! that [`tick`] drains between frames — the UI thread never waits for the network. Downloads
//! (originals) land in a staging folder under the system temp dir; batches of a few finished
//! downloads join the catalog through the untouched `library.import` command, so de-duplication,
//! the undo step and the durable save behave exactly like any other import.
//!
//! Cancel is a flag: an in-flight transfer stops at its next chunk, started batches stay
//! imported, and nothing new is started.
//!
//! Server entries (URL + API key) are the engine's `immich.servers` command's business; this
//! module only reads [`lightcraft_engine::Session::immich_servers`]. Keys never enter a log line,
//! a widget id or an error the dialog shows — the client crate's `Error` Display already keeps
//! them out.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_immich::{Client, Limits, SearchQuery};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::panels::dialogs::field;
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

/// Assets per `library.import` commit while importing.
const COMMIT_BATCH: usize = 8;
/// Assets browsed per page (the engine's own default for `immich.browse`).
const PAGE_SIZE: u32 = 60;
/// Thumbnails fetched at once; the rest wait for the next frame.
const THUMB_IN_FLIGHT: usize = 4;
/// The selection cap, as in the engine's `immich.import`.
const MAX_IMPORT_IDS: usize = 10_000;
/// Longest thumbnail edge fetched and decoded.
const THUMB_EDGE: u32 = 192;

/// The dialog's state: filters, the browsed page, the selection. The workers, the textures and
/// the import progress live in [`ImmichTask`] (app-level); this is what [`Dialog::Immich`] carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImmichDialog {
    /// The chosen server (its name); "" when none is configured.
    pub server: String,
    /// The album to browse (its id); "" = all of the server.
    pub album_id: String,
    /// …and its name, for the combo's label.
    pub album_name: String,
    /// Favorites only.
    pub favorite: bool,
    /// 0 = any, else 1…5.
    pub rating: u32,
    /// "From" date, typed as `YYYY-MM-DD` (sent as the search's `createdAfter`).
    pub from_date: String,
    /// The page currently shown.
    pub assets: Vec<ImmichAsset>,
    /// Per asset: selected for import.
    pub checked: Vec<bool>,
    /// The asset clicked last: where a Shift-click range starts (as in [`crate::import::ImportDialog::click`]).
    #[serde(skip)]
    pub last_clicked: Option<usize>,
    /// The 1-based page on screen.
    pub page: u32,
    /// A full page came back: Next is worth a click.
    pub maybe_more: bool,
    /// The inline error line (a bad key, an unreachable server, a bad date).
    pub error: Option<String>,
    /// The inline status line (the Test button's answer, …).
    pub info: Option<String>,
}

/// One browsed asset, as far as the grid cares.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImmichAsset {
    pub id: String,
    pub file_name: String,
    pub created_at: Option<String>,
    pub is_favorite: bool,
    pub rating: Option<u32>,
}

/// Parse the dialog's "from" date: `YYYY`, `YYYY-MM` or `YYYY-MM-DD` — a partial date means the
/// start of that year or month ("2024" is 2024-01-01, "2024-06" is 2024-06-01). `None` for
/// anything else (an empty field is not an error — the caller only calls this on non-empty text).
pub fn parse_date(text: &str) -> Option<String> {
    let t = text.trim();
    let b = t.as_bytes();
    // the separators each present length has, and the month/day it defaults to
    let (month, day) = match b.len() {
        4 => ("01", "01"),
        7 => (t.get(5..7)?, "01"),
        10 => (t.get(5..7)?, t.get(8..10)?),
        _ => return None,
    };
    if (b.len() >= 7 && b.get(4) != Some(&b'-')) || (b.len() == 10 && b.get(7) != Some(&b'-')) {
        return None;
    }
    let num = |part: &str| {
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse::<u32>().ok()
    };
    let (year, month, day) = (num(t.get(0..4)?)?, num(month)?, num(day)?);
    if (1900..=2999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day) {
        Some(format!("{year:04}-{month:02}-{day:02}"))
    } else {
        None
    }
}

impl ImmichDialog {
    /// A click on asset `i` toggles its checkbox; with `shift`, every asset from the last clicked
    /// one to `i` takes `i`'s new state (the file-manager gesture, like [`crate::import::ImportDialog::click`]).
    pub fn click(&mut self, i: usize, shift: bool) {
        if i >= self.assets.len() {
            return;
        }
        let Some(on) = self.checked.get(i).map(|c| !c) else { return };
        let from = if shift { self.last_clicked.filter(|a| *a < self.checked.len()).unwrap_or(i) } else { i };
        for k in from.min(i)..=from.max(i) {
            if let Some(c) = self.checked.get_mut(k) {
                *c = on;
            }
        }
        self.last_clicked = Some(i);
    }
    /// The selected asset ids (in grid order).
    pub fn selected_ids(&self) -> Vec<String> {
        self.assets.iter().zip(&self.checked).filter(|(_, on)| **on).map(|(a, _)| a.id.clone()).collect()
    }
    /// The dialog's filters as a search, for `page` (1-based, clamped). The mapping is pure so it
    /// is testable without a server.
    pub fn query_for(&self, page: u32) -> SearchQuery {
        SearchQuery {
            album_ids: if self.album_id.is_empty() { Vec::new() } else { vec![self.album_id.clone()] },
            is_favorite: self.favorite.then_some(true),
            rating: (self.rating >= 1 && self.rating <= 5).then_some(self.rating),
            created_after: parse_date(&self.from_date),
            // LightCraft is a photo app: movies belong to the movie editors, not here
            asset_type: Some("IMAGE".into()),
            page: page.clamp(1, 100_000),
            page_size: PAGE_SIZE.min(1000),
            ..Default::default()
        }
    }
}

/// One album as the combo shows it: (id, name, asset count).
type AlbumRow = (String, String, u32);

/// What a worker brought back for the UI thread to apply.
enum Event {
    Albums {
        generation: u64,
        result: Result<Vec<AlbumRow>, String>,
    },
    Search {
        generation: u64,
        page: u32,
        result: Result<(Vec<ImmichAsset>, bool), String>,
    },
    /// A thumbnail's decoded pixels; `None` = fetch or decode failed (noted, not retried).
    Thumb {
        id: String,
        image: Option<lightcraft_raster::Rgba8>,
    },
    /// One download ended: its staging path, or the reason it failed.
    Downloaded {
        id: String,
        path: Option<String>,
        error: Option<String>,
    },
    /// The import worker is done (finished, cancelled or dead); commit what is left and close out.
    ImportFinished,
}

/// An import run: its client (whose cancel flag the Cancel button raises) and whether its worker
/// has been spawned yet (the first [`tick`] after the button does that, with a real context).
struct ImportRun {
    client: Arc<Client>,
    started: bool,
}

impl Drop for ImportRun {
    /// A dropped import (dialog closed, or the task replaced) stops its worker at the next chunk.
    fn drop(&mut self) {
        self.client.set_cancelled(true);
    }
}

/// The Immich dialog's workers and their state (native only — `lightcraft-immich` does not exist
/// on wasm). The dialog opens with it; closing the dialog calls [`close`], which drops it, and
/// [`ImportRun`]'s Drop flags stop any running worker.
pub struct ImmichTask {
    /// Bumped on every server switch: late answers from the old server are dropped.
    generation: Arc<AtomicU64>,
    rx: Receiver<Event>,
    tx: Sender<Event>,
    /// The server's albums.
    pub albums: Vec<AlbumRow>,
    albums_busy: bool,
    /// A search is in flight.
    searching: bool,
    /// Reload the album list on the next tick (the dialog opened, or the server changed).
    need_albums: bool,
    /// The thumbnails that arrived, by asset id.
    pub thumbs: HashMap<String, egui::TextureHandle>,
    thumb_failed: HashSet<String>,
    thumb_busy: Arc<AtomicUsize>,
    import: Option<ImportRun>,
    pub(crate) import_done: bool,
    /// `(asset id, file name)` for the running import.
    import_ids: Vec<(String, String)>,
    /// Downloads that wait for the next `library.import` commit.
    staged: Vec<(String, String)>,
    staging: PathBuf,
    pub total: usize,
    pub done: usize,
    pub imported: usize,
    /// Duplicates the import skipped (already in the library).
    skipped: usize,
    /// One line per asset that could not be imported.
    pub failed: Vec<String>,
    cancelled: bool,
    undo0: usize,
    /// `(url, api key)` the dialog's current server — the workers' target and the thumbs'.
    server_url: String,
    server_key: String,
}

impl std::fmt::Debug for ImmichTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImmichTask").field("searching", &self.searching).field("importing", &self.import.is_some()).finish()
    }
}

impl ImmichTask {
    /// `{phase, total, done, failed}` for `ui.inspect`.
    pub fn status(&self) -> Value {
        let phase = if self.import.is_some() {
            "importing"
        } else if self.import_done {
            "finished"
        } else {
            "browsing"
        };
        json!({"phase": phase, "total": self.total, "done": self.done, "failed": self.failed.len()})
    }

    /// A worker is still expected to answer (keep asking for frames).
    fn busy(&self) -> bool {
        self.searching || self.albums_busy || self.import.is_some() || self.thumb_busy.load(Ordering::Relaxed) > 0
    }

    /// Ask for one cell's thumbnail, bounded to [`THUMB_IN_FLIGHT`] at once; the grid asks every
    /// frame, so throttled ones start moments later.
    fn request_thumb(&mut self, id: &str, ctx: &egui::Context) {
        if self.server_url.is_empty() || self.thumbs.contains_key(id) || self.thumb_failed.contains(id) {
            return;
        }
        // claim a slot (check and add are one step, so the bound holds between frames)
        if self.thumb_busy.fetch_add(1, Ordering::Relaxed) >= THUMB_IN_FLIGHT {
            self.thumb_busy.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        let tx = self.tx.clone();
        let (id, url, key) = (id.to_string(), self.server_url.clone(), self.server_key.clone());
        let spawned = spawn("lc-immich-thumb", ctx, move || {
            let image = lightcraft_engine::guard::catch("immich thumbnail", || {
                let c = Client::new(&url, &key, Limits::default()).map_err(|e| e.to_string())?;
                let mut buf: Vec<u8> = Vec::new();
                c.thumbnail(&id, "preview", &mut buf, |_| {}).map_err(|e| e.to_string())?;
                lightcraft_codecs::decode_thumbnail(&buf, THUMB_EDGE).map(|th| th.image).map_err(|e| e.to_string())
            })
            .flatten()
            .ok();
            let _ = tx.send(Event::Thumb { id, image });
            // the slot is released in [`tick`], when the event is applied — releasing it here
            // as well would count every thumbnail twice and let the bound drift open
        });
        if spawned.is_err() {
            self.thumb_busy.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Thumbnails asked for but not answered yet (for tests: the bound must hold).
    pub fn thumbs_in_flight(&self) -> usize {
        self.thumb_busy.load(Ordering::Relaxed)
    }
}

/// The configured server a dialog names (by name, or the lone one when it names none).
fn server_url_key(session: &lightcraft_engine::Session, sel: &str) -> Result<(String, String), String> {
    let list = &session.immich_servers;
    if list.is_empty() {
        return Err("no Immich server is configured yet — add and test one in Settings ▸ Integrations".into());
    }
    let sel = sel.trim();
    let found = if sel.is_empty() {
        list.first()
    } else {
        list.iter().find(|s| s.name.eq_ignore_ascii_case(sel) || s.url.trim_end_matches('/') == sel.trim_end_matches('/'))
    };
    let s = found.ok_or_else(|| {
        format!("unknown Immich server `{sel}` (configured: {})", list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "))
    })?;
    Ok((s.url.clone(), s.api_key.clone()))
}

/// Spawn `f` on its own thread; it repaints once it is done. A spawn failure is an error the
/// caller shows (no thread silently missing).
fn spawn(name: &str, ctx: &egui::Context, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            f();
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start: {e}"))
}

/// Write an error / status line into the open Immich dialog (if it is still the open one).
fn set_dialog_lines(app: &mut LightcraftApp, error: Option<String>, info: Option<String>) {
    if let Some(Dialog::Immich { opts }) = app.ui.dialog.as_mut() {
        opts.error = error;
        opts.info = info;
    }
}

/// Ask for the albums of `url` on a worker ([`tick`] applies [`Event::Albums`]).
fn start_albums(task: &mut ImmichTask, ctx: &egui::Context) {
    let (generation, tx) = (task.generation.load(Ordering::Relaxed), task.tx.clone());
    let (url, key) = (task.server_url.clone(), task.server_key.clone());
    task.albums_busy = true;
    if let Err(e) = spawn("lc-immich-albums", ctx, move || {
        let r = lightcraft_engine::guard::catch("immich albums", move || {
            let c = Client::new(&url, &key, Limits::default()).map_err(|e| e.to_string())?;
            let list = c.albums().map_err(|e| e.to_string())?;
            Ok(list.into_iter().map(|a| (a.id, a.album_name, a.asset_count)).collect::<Vec<_>>())
        });
        let _ = tx.send(Event::Albums { generation, result: r.flatten() });
    }) {
        task.albums_busy = false;
        log::warn!("immich: {e}");
    }
}

/// Ask for one search page on a worker ([`tick`] applies [`Event::Search`]).
fn start_search(task: &mut ImmichTask, query: SearchQuery, ctx: &egui::Context) {
    let (generation, tx) = (task.generation.load(Ordering::Relaxed), task.tx.clone());
    let (url, key) = (task.server_url.clone(), task.server_key.clone());
    let page = query.page;
    task.searching = true;
    if let Err(e) = spawn("lc-immich-search", ctx, move || {
        let r = lightcraft_engine::guard::catch("immich search", move || {
            let c = Client::new(&url, &key, Limits::default()).map_err(|e| e.to_string())?;
            let paged = c.search(&query).map_err(|e| e.to_string())?;
            let assets = paged
                .items
                .into_iter()
                .map(|a| ImmichAsset {
                    id: a.id,
                    file_name: a.original_file_name,
                    created_at: a.file_created_at,
                    is_favorite: a.is_favorite,
                    rating: a.rating,
                })
                .collect();
            Ok((assets, paged.maybe_more))
        });
        let _ = tx.send(Event::Search { generation, page, result: r.flatten() });
    }) {
        task.searching = false;
        log::warn!("immich: {e}");
    }
}

/// Strip a server-supplied name down to one safe file name (the engine's `immich.import` does the
/// same for its staging folder).
fn safe_file_name(given: &str, id: &str) -> String {
    let base = given.rsplit(['/', '\\']).next().unwrap_or(given);
    let mut out: String =
        base.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let trimmed = out.trim_matches(|c| c == ' ' || c == '.');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        let short: String = id.chars().take(64).collect();
        return format!("immich-{short}");
    }
    if out.chars().count() > 200 {
        out = out.chars().take(200).collect();
    }
    out
}

/// A unique path in the staging folder for `name` (`-1`, `-2`, … when taken; the asset id last).
fn staged_path(staging: &std::path::Path, name: &str, id: &str) -> PathBuf {
    let stem = name
        .rsplit_once('.')
        .filter(|(s, e)| !s.is_empty() && !e.is_empty() && e.len() <= 8)
        .map_or_else(|| (name.to_string(), String::new()), |(s, e)| (s.to_string(), format!(".{e}")));
    for n in 0..1000u32 {
        let candidate = if n == 0 { staging.join(name) } else { staging.join(format!("{}-{n}{}", stem.0, stem.1)) };
        if !candidate.exists() {
            return candidate;
        }
    }
    let short: String = id.chars().take(64).collect();
    staging.join(format!("immich-{short}.bin"))
}

/// Download the chosen originals to `staging` (one [`Event::Downloaded`] per asset, then
/// [`Event::ImportFinished`]). Runs on the worker; the client's cancel flag stops it between
/// files and mid-transfer. Whatever is still in the folder when this thread exits never joined
/// the catalog, and is removed best-effort.
fn spawn_import(tx: Sender<Event>, client: Arc<Client>, staging: PathBuf, jobs: Vec<(String, String)>, ctx: &egui::Context) -> Result<(), String> {
    spawn("lc-immich-import", ctx, move || {
        for (id, given) in &jobs {
            if client.cancelled() {
                break;
            }
            let dest = staged_path(&staging, &safe_file_name(given, id), id);
            match std::fs::File::create(&dest) {
                Ok(mut file) => match client.download_original(id, &mut file, |_| {}) {
                    Ok(n) if n > 0 => {
                        let _ = tx.send(Event::Downloaded { id: id.clone(), path: Some(dest.to_string_lossy().into_owned()), error: None });
                    }
                    Ok(_) => {
                        let _ = std::fs::remove_file(&dest);
                        let _ = tx.send(Event::Downloaded { id: id.clone(), path: None, error: Some("the server sent an empty file".into()) });
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&dest);
                        let _ = tx.send(Event::Downloaded { id: id.clone(), path: None, error: Some(e.to_string()) });
                    }
                },
                Err(e) => {
                    let _ = tx.send(Event::Downloaded { id: id.clone(), path: None, error: Some(format!("could not write the download: {e}")) });
                }
            }
        }
        // ImportFinished is handled (and the folder cleaned) on the UI side: files a cancelled
        // run left behind must stay until tick has offered them to the library.
        let _ = tx.send(Event::ImportFinished);
    })
}

/// Add one batch of downloaded originals to the catalog (the same `library.import` a disk import
/// runs; one undo step per batch, merged into one when the import ends).
fn commit_batch(app: &mut LightcraftApp, task: &mut ImmichTask) {
    let n = task.staged.len().min(COMMIT_BATCH);
    if n == 0 {
        return;
    }
    let batch: Vec<(String, String)> = task.staged.drain(..n).collect();
    let paths: Vec<&str> = batch.iter().map(|(_, p)| p.as_str()).collect();
    match app.session.execute_fn("library.import", |s| s.execute("library.import", &json!({"paths": paths, "mode": "copy"}))) {
        Ok(v) => {
            let len = |k: &str| v[k].as_array().map_or(0, Vec::len);
            task.imported += len("imported");
            task.skipped += len("duplicates");
            for entry in v["failed"].as_array().into_iter().flatten() {
                // the report's `failed` is (path, reason) pairs: the file name and the library's
                // own reason ("Heif files are not supported yet"), never a generic stand-in
                let (path, reason) = entry
                    .as_array()
                    .map(|a| (a.first().and_then(Value::as_str).unwrap_or(""), a.get(1).and_then(Value::as_str).unwrap_or("import failed")))
                    .unwrap_or(("", "import failed"));
                let name = path.rsplit('/').next().unwrap_or(path);
                task.failed.push(format!("{name}: {reason}"));
            }
            task.done += n;
            // the bytes are in the library now; the staged copies have no further use
            for (_, path) in &batch {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!("immich: staging cleanup: {path}: {e}");
                }
            }
        }
        Err(e) => {
            log::warn!("immich import: {e}");
            task.done += n;
            for (_, path) in &batch {
                let name = std::path::Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                task.failed.push(format!("{name}: {e}"));
            }
        }
    }
}

/// The import ended (finished or cancelled): merge its batches into one undo step.
fn finish_import(app: &mut LightcraftApp, task: &mut ImmichTask) {
    if let Some(run) = task.import.take() {
        drop(run);
    }
    task.import_done = true;
    let steps = app.session.undo.len().saturating_sub(task.undo0);
    if task.imported > 0 {
        let plural = if task.imported == 1 { "" } else { "s" };
        app.session.merge_undo(steps, &crate::i18n::tr_format!("Add {} Photo{}", task.imported, plural));
    }
}

/// Distinguishes staging folders inside one process (parallel tests, a reopen): one dialog's
/// close must not purge another's in-flight downloads.
static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open the dialog (File ▸ Import from Immich…). A running import is stopped first; the dialog
/// opens fresh.
pub fn open(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    close(app);
    let wanted = p.get("server").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let server = if wanted.is_empty() {
        app.session.immich_servers.first().map(|s| s.name.clone()).unwrap_or_default()
    } else {
        server_url_key(&app.session, &wanted)?;
        wanted
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let staging = std::env::temp_dir().join(format!("lc-immich-ui-{}-{}", std::process::id(), TASK_SEQ.fetch_add(1, Ordering::Relaxed)));
    let (url, key) = server_url_key(&app.session, &server).unwrap_or_default();
    app.immich_task = Some(ImmichTask {
        generation: Arc::new(AtomicU64::new(1)),
        rx,
        tx,
        albums: Vec::new(),
        albums_busy: false,
        searching: false,
        need_albums: !server.is_empty(),
        thumbs: HashMap::new(),
        thumb_failed: HashSet::new(),
        thumb_busy: Arc::new(AtomicUsize::new(0)),
        import: None,
        import_done: false,
        import_ids: Vec::new(),
        staged: Vec::new(),
        staging,
        total: 0,
        done: 0,
        imported: 0,
        skipped: 0,
        failed: Vec::new(),
        cancelled: false,
        undo0: 0,
        server_url: url,
        server_key: key,
    });
    app.ui.dialog = Some(Dialog::Immich { opts: Box::new(ImmichDialog { server, ..Default::default() }) });
    Ok(json!({"dialog": "immich"}))
}

/// Close the dialog: stop any running worker and let go of the task. Completed batches stay
/// imported (they joined the catalog as they arrived).
pub fn close(app: &mut LightcraftApp) {
    if let Some(task) = app.immich_task.take() {
        purge_staging(&task.staging);
    }
}

/// Drop everything still in a staging folder (files that never joined the library); best effort.
fn purge_staging(staging: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(staging) {
        for entry in rd.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(staging);
}

/// Cancel the import in progress: in-flight transfers stop at the next chunk, no new file is
/// started, and the downloads that are already complete still join the library.
pub fn cancel(app: &mut LightcraftApp) {
    if let Some(task) = app.immich_task.as_mut() {
        task.cancelled = true;
        if let Some(run) = &task.import {
            run.client.set_cancelled(true);
        }
    }
}

/// Start importing the selected assets (the dialog's Import button / `ui.dialog.confirm`); the
/// dialog stays open and shows the progress. The worker starts on the next [`tick`], where there
/// is a repaint context to hand it.
pub fn start_import(app: &mut LightcraftApp, d: &ImmichDialog) -> Result<Value, String> {
    let ids = d.selected_ids();
    if ids.is_empty() {
        return Err("no photos selected".into());
    }
    if ids.len() > MAX_IMPORT_IDS {
        return Err(format!("too many photos selected ({}); select at most {MAX_IMPORT_IDS}", ids.len()));
    }
    let (url, key) = server_url_key(&app.session, &d.server)?;
    let client = Arc::new(Client::new(&url, &key, Limits::default()).map_err(|e| e.to_string())?);
    let task = app.immich_task.as_mut().ok_or("the Immich dialog is not open")?;
    if task.import.is_some() {
        return Err("the import is already running".into());
    }
    if let Err(e) = std::fs::create_dir_all(&task.staging) {
        return Err(format!("could not create the download folder: {e}"));
    }
    let mut import_ids: Vec<(String, String)> = Vec::with_capacity(ids.len());
    for id in &ids {
        let name = d.assets.iter().find(|a| a.id == *id).map(|a| a.file_name.clone()).unwrap_or_default();
        import_ids.push((id.clone(), name));
    }
    task.import = Some(ImportRun { client, started: false });
    task.import_ids = import_ids;
    task.import_done = false;
    task.total = ids.len();
    task.done = 0;
    task.imported = 0;
    task.skipped = 0;
    task.failed.clear();
    task.staged.clear();
    task.cancelled = false;
    task.undo0 = app.session.undo.len();
    Ok(json!({"importing": ids.len()}))
}

/// Advance the Immich dialog (called every frame): start the jobs the dialog asked for, apply
/// what its workers brought back, and commit finished download batches to the catalog.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    // a settings-tab Test that finished while nobody was looking gets its verify here
    verify_fresh_settings_tests(app, ctx);
    let Some(mut task) = app.immich_task.take() else { return };
    // the Cancel button asks for this; the work happens here, not in the paint pass
    if take_flag(ctx, CANCEL_FLAG) {
        task.cancelled = true;
        if let Some(run) = &task.import {
            run.client.set_cancelled(true);
        }
    }
    // (re)load the albums when the dialog opened or the server changed
    if task.need_albums && !task.albums_busy && !task.server_url.is_empty() {
        task.need_albums = false;
        start_albums(&mut task, ctx);
    }
    // the import's worker starts here, not on the button's click
    let mut to_spawn = None;
    if let Some(run) = task.import.as_mut().filter(|r| !r.started) {
        run.started = true;
        to_spawn = Some((run.client.clone(), std::mem::take(&mut task.import_ids)));
    }
    if let Some((client, jobs)) = to_spawn {
        let (tx, staging) = (task.tx.clone(), task.staging.clone());
        if let Err(e) = spawn_import(tx, client, staging, jobs, ctx) {
            task.import = None;
            set_dialog_lines(app, Some(e), None);
        }
    }
    let mut handled = 0usize;
    while let Ok(ev) = task.rx.try_recv() {
        handled += 1;
        match ev {
            Event::Albums { generation, result } => {
                if generation != task.generation.load(Ordering::Relaxed) {
                    continue;
                }
                task.albums_busy = false;
                match result {
                    Ok(list) => task.albums = list,
                    Err(e) => set_dialog_lines(app, Some(e), None),
                }
            }
            Event::Search { generation, page, result } => {
                if generation != task.generation.load(Ordering::Relaxed) {
                    continue;
                }
                task.searching = false;
                match result {
                    Ok((assets, more)) => {
                        if let Some(Dialog::Immich { opts }) = app.ui.dialog.as_mut() {
                            opts.assets = assets;
                            opts.checked = vec![false; opts.assets.len()];
                            opts.last_clicked = None;
                            opts.page = page;
                            opts.maybe_more = more;
                            opts.error = None;
                        }
                    }
                    Err(e) => set_dialog_lines(app, Some(e), None),
                }
            }
            Event::Thumb { id, image } => {
                task.thumb_busy.fetch_sub(1, Ordering::Relaxed);
                match image {
                    Some(img) => {
                        let color = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
                        let tex = ctx.load_texture(format!("immich-thumb-{id}"), color, egui::TextureOptions::LINEAR);
                        if task.thumbs.len() > 3000 {
                            task.thumbs.clear();
                        }
                        task.thumbs.insert(id, tex);
                    }
                    None => {
                        task.thumb_failed.insert(id);
                    }
                }
            }
            Event::Downloaded { id, path, error } => match (path, error) {
                (Some(path), _) => task.staged.push((id, path)),
                (None, Some(e)) => {
                    task.failed.push(format!("{id}: {e}"));
                    task.done += 1;
                }
                (None, None) => {
                    task.failed.push(format!("{id}: the download failed"));
                    task.done += 1;
                }
            },
            Event::ImportFinished => {
                while !task.staged.is_empty() {
                    commit_batch(app, &mut task);
                }
                finish_import(app, &mut task);
                // whatever is left never made it into the library: drop it with the run
                purge_staging(&task.staging);
            }
        }
        while task.staged.len() >= COMMIT_BATCH {
            commit_batch(app, &mut task);
        }
        if handled >= 64 {
            ctx.request_repaint();
            break;
        }
    }
    if task.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
    app.immich_task = Some(task);
}

/// The dialog body: server / album / filters, the paged asset grid with selectable thumbnails,
/// and the footer. While importing it shows the progress instead.
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImmichDialog) {
    let t = Tokens::get(ui.ctx());
    let importing = app.immich_task.as_ref().is_some_and(|x| x.import.is_some());
    let finished = app.immich_task.as_ref().is_some_and(|x| x.import_done);
    if importing || finished {
        import_progress(app, ui);
        return;
    }

    let servers: Vec<String> = app.session.immich_servers.iter().map(|s| s.name.clone()).collect();
    let albums: Vec<AlbumRow> = app.immich_task.as_ref().map(|x| x.albums.clone()).unwrap_or_default();
    let searching = app.immich_task.as_ref().is_some_and(|x| x.searching);

    if servers.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("No Immich server is configured yet.")).color(t.text_label));
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "Add your server in Settings ▸ Integrations, then Test it — File ▸ Import from Immich unlocks once it answers.",
            ))
            .color(t.text_dim)
            .small(),
        );
        return;
    }

    // Row 1: the server, and Test against it
    let mut picked_server: Option<String> = None;
    field(ui, "Server", |ui| {
        let cur = if d.server.is_empty() { crate::i18n::tr("None").to_string() } else { d.server.clone() };
        let combo = egui::ComboBox::from_id_salt("immich-server").selected_text(cur).show_ui(ui, |ui| {
            for name in &servers {
                let r = ui.selectable_label(&d.server == name, name);
                register(ui.ctx(), format!("button:immichServer-{name}"), r.rect);
                if r.clicked() {
                    picked_server = Some(name.clone());
                }
            }
        });
        register(ui.ctx(), "combo:immichServer", combo.response.rect);
    });
    if let Some(name) = picked_server {
        d.server = name;
        d.album_id.clear();
        d.album_name.clear();
        d.assets.clear();
        d.checked.clear();
        d.page = 1;
        d.maybe_more = false;
        d.error = None;
        d.info = None;
        if let Some(task) = app.immich_task.as_mut() {
            task.generation.fetch_add(1, Ordering::Relaxed);
            task.albums.clear();
            task.albums_busy = false;
            task.thumbs.clear();
            task.thumb_failed.clear();
            task.searching = false;
            match server_url_key(&app.session, &d.server) {
                Ok((u, k)) => {
                    task.server_url = u;
                    task.server_key = k;
                    task.need_albums = true;
                }
                Err(e) => d.error = Some(e),
            }
        }
    }
    // Row 2: the album to browse
    let mut picked_album: Option<Option<(String, String)>> = None;
    field(ui, "Album", |ui| {
        let cur = if d.album_id.is_empty() { crate::i18n::tr("All").to_string() } else { d.album_name.clone() };
        let combo = egui::ComboBox::from_id_salt("immich-album").selected_text(cur).height(260.0).show_ui(ui, |ui| {
            let r = ui.selectable_label(d.album_id.is_empty(), crate::i18n::tr("All albums"));
            register(ui.ctx(), "button:immichAlbum-all", r.rect);
            if r.clicked() {
                picked_album = Some(None);
            }
            for (id, name, count) in &albums {
                let r = ui.selectable_label(&d.album_id == id, format!("{name} ({count})"));
                register(ui.ctx(), format!("button:immichAlbum-{id}"), r.rect);
                if r.clicked() {
                    picked_album = Some(Some((id.clone(), name.clone())));
                }
            }
        });
        register(ui.ctx(), "combo:immichAlbum", combo.response.rect);
    });
    match picked_album {
        Some(None) => {
            d.album_id.clear();
            d.album_name.clear();
        }
        Some(Some((id, name))) => {
            d.album_id = id;
            d.album_name = name;
        }
        None => {}
    }
    // Row 3: favorite · rating · from · Search
    let mut search_page: Option<u32> = None;
    ui.horizontal(|ui| {
        let r = ui.checkbox(&mut d.favorite, crate::i18n::tr("Favorites only"));
        register(ui.ctx(), "check:immichFavorite", r.rect);
        let cur = if d.rating == 0 { crate::i18n::tr("Any rating").to_string() } else { "★".repeat(d.rating as usize) };
        let mut rate: Option<u32> = None;
        egui::ComboBox::from_id_salt("immich-rating").selected_text(cur).show_ui(ui, |ui| {
            if ui.selectable_label(d.rating == 0, crate::i18n::tr("Any rating")).clicked() {
                rate = Some(0);
            }
            for n in 1..=5u32 {
                if ui.selectable_label(d.rating == n, "★".repeat(n as usize)).clicked() {
                    rate = Some(n);
                }
            }
        });
        if let Some(n) = rate {
            d.rating = n;
        }
        let r = ui
            .add(egui::TextEdit::singleline(&mut d.from_date).hint_text(crate::i18n::tr("From (YYYY, YYYY-MM or YYYY-MM-DD)")).desired_width(170.0));
        register(ui.ctx(), "field:immichFrom", r.rect);
        let r = ui.add_enabled(!searching, egui::Button::new(crate::i18n::tr("Search")));
        register(ui.ctx(), "button:immichSearch", r.rect);
        if r.clicked() {
            if !d.from_date.trim().is_empty() && parse_date(&d.from_date).is_none() {
                d.error = Some(crate::i18n::tr("Enter the date as YYYY, YYYY-MM or YYYY-MM-DD, or leave it empty.").to_string());
            } else {
                search_page = Some(1);
            }
        }
    });
    // paging and the status lines
    ui.horizontal(|ui| {
        let prev = ui.add_enabled(d.page > 1 && !searching, egui::Button::new("‹"));
        register(ui.ctx(), "button:immichPrev", prev.rect);
        let next = ui.add_enabled(d.maybe_more && !searching, egui::Button::new("›"));
        register(ui.ctx(), "button:immichNext", next.rect);
        ui.label(egui::RichText::new(crate::i18n::tr_format!("Page {}", d.page)).color(t.text_dim));
        if searching {
            ui.label(egui::RichText::new(crate::i18n::tr("Searching…")).color(t.text_dim));
        } else if !d.assets.is_empty() {
            let sel = d.selected_ids().len();
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{} on this page · {} selected", d.assets.len(), sel)).color(t.text_dim));
        }
        if prev.clicked() {
            search_page = Some(d.page.saturating_sub(1).max(1));
        }
        if next.clicked() {
            search_page = Some(d.page + 1);
        }
    });
    if let Some(page) = search_page {
        let query = d.query_for(page);
        match server_url_key(&app.session, &d.server) {
            Ok(_) => {
                if let Some(task) = app.immich_task.as_mut() {
                    start_search(task, query, ui.ctx());
                }
            }
            Err(e) => d.error = Some(e),
        }
    }
    if let Some(e) = &d.error {
        let r = ui.label(egui::RichText::new(e.as_str()).color(t.caution));
        register(ui.ctx(), "label:immichError", r.rect);
    }
    if let Some(s) = &d.info {
        ui.label(egui::RichText::new(s.as_str()).color(t.text_dim));
    }
    // the asset grid
    asset_grid(app, ui, d);
    // footer: the selection and the Import button (`ui.dialog.confirm` starts the same import)
    ui.add_space(4.0);
    let n = d.selected_ids().len();
    let plural = if n == 1 { "" } else { "s" };
    let label = crate::i18n::tr_format!("Import {} Photo{}", n, plural);
    let mut clicked = false;
    let h = ui.spacing().interact_size.y.max(24.0);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let r = ui.add_enabled(n > 0 && !searching, egui::Button::new(label.as_str()));
        register(ui.ctx(), "button:immichImport", r.rect);
        clicked = r.clicked();
    });
    if clicked {
        match start_import(app, d) {
            Ok(_) => d.error = None,
            Err(e) => d.error = Some(e),
        }
    }
}

/// Set a button's request flag (egui memory, so it belongs to this app — a second app in the
/// same process, like a parallel test, never sees it).
fn request_flag(ctx: &egui::Context, flag: &str) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(flag), true));
}

/// Read and clear a request flag.
fn take_flag(ctx: &egui::Context, flag: &str) -> bool {
    let id = egui::Id::new(flag);
    let v = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if v {
        ctx.data_mut(|d| d.insert_temp(id, false));
    }
    v
}

/// The Cancel button sets this; [`tick`] (which owns the task) takes it.
const CANCEL_FLAG: &str = "immich-cancel-requested";

/// The import phase of the dialog: the progress bar, the failures, and Cancel.
fn import_progress(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(task) = &app.immich_task else { return };
    let frac = task.done as f32 / task.total.max(1) as f32;
    let head: String = if task.import_done {
        crate::i18n::tr("Done.").to_string()
    } else if task.cancelled {
        crate::i18n::tr("Stopping…").to_string()
    } else {
        crate::i18n::tr_format!("Adding photos… {} of {}", task.done, task.total)
    };
    ui.label(egui::RichText::new(head).color(t.text));
    if !task.import_done {
        ui.add(egui::ProgressBar::new(frac).desired_width(420.0));
    }
    let summary = crate::i18n::tr_format!("Imported {} · {} already in the library · {} failed", task.imported, task.skipped, task.failed.len());
    let r = ui.label(egui::RichText::new(summary).color(t.text_dim));
    register(ui.ctx(), "label:immichSummary", r.rect);
    if !task.failed.is_empty() {
        egui::ScrollArea::vertical().id_salt("immich-failed").max_height(90.0).show(ui, |ui| {
            for line in &task.failed {
                ui.label(egui::RichText::new(line.as_str()).color(t.caution).small());
            }
        });
    }
    if !task.import_done {
        let r = ui.add_enabled(!task.cancelled, egui::Button::new(crate::i18n::tr("Cancel")));
        register(ui.ctx(), "button:immichCancel", r.rect);
        if r.clicked() {
            request_flag(ui.ctx(), CANCEL_FLAG);
        }
    }
}

/// The browsed assets as a selectable grid of thumbnails (fetched on workers, bounded in flight;
/// the grid only asks for the cells in view).
fn asset_grid(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImmichDialog) {
    let n = d.assets.len();
    if n == 0 {
        return;
    }
    let cell = 116.0;
    let avail = ui.available_width();
    let cols = ((avail + 6.0) / (cell + 6.0)).floor().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    egui::ScrollArea::vertical().id_salt("immich-grid").max_height(330.0).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (cell + 22.0)), Sense::hover());
        for i in 0..n {
            let (c, r) = (i % cols, i / cols);
            let local = Rect::from_min_size(pos2(c as f32 * (cell + 6.0), r as f32 * (cell + 22.0)), vec2(cell, cell + 18.0));
            if !local.intersects(viewport.expand(cell)) {
                continue;
            }
            let rect = local.translate(area.min.to_vec2());
            asset_cell(app, ui, d, i, rect);
        }
    });
}

fn asset_cell(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImmichDialog, i: usize, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let Some(a) = d.assets.get(i).cloned() else { return };
    let img = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
    let resp = ui.interact(img, egui::Id::new(("immich-cell", i)), Sense::click());
    register(ui.ctx(), format!("immich:{i}"), img);
    let p = ui.painter();
    p.rect_filled(img, 3.0, Color32::from_gray(30));
    // thumbnail (a worker job; the grid only asks for the cells in view)
    if let Some(task) = app.immich_task.as_mut() {
        task.request_thumb(&a.id, ui.ctx());
        if let Some(tex) = task.thumbs.get(&a.id) {
            let [tw, th] = tex.size();
            let (tw, th) = (tw as f32, th as f32);
            let s = ((img.width() - 8.0) / tw).min((img.height() - 8.0) / th);
            let fit = Rect::from_center_size(img.center(), vec2(tw * s, th * s));
            p.image(tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
    let on = d.checked.get(i).copied().unwrap_or(false);
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
    if a.is_favorite {
        let g = p.layout_no_wrap("★".to_string(), t.font(12.0), Color32::from_rgb(250, 200, 60));
        p.galley(pos2(img.right() - g.size().x - 6.0, img.top() + 4.0), g, Color32::WHITE);
    }
    let name: String = a.file_name.chars().take(18).collect();
    let name = if a.file_name.chars().count() > 18 { format!("{name}…") } else { name };
    p.text(pos2(rect.left() + 2.0, img.bottom() + 8.0), Align2::LEFT_CENTER, name, t.font(10.5), t.text_label);
    let mut tip = a.file_name.clone();
    if let Some(c) = &a.created_at {
        tip.push('\n');
        tip.push_str(&c.replace('T', " "));
    }
    let resp = resp.on_hover_text(tip);
    if resp.clicked() {
        let shift = ui.input(|input| input.modifiers.shift);
        d.click(i, shift);
    }
}

// ---------------------------------------------------------------------------
// Settings ▸ Integrations
// ---------------------------------------------------------------------------

/// One Test outcome from the Integrations tab: written by the worker thread, read every frame.
#[derive(Clone, Debug)]
struct SettingsTest {
    /// The test is still running.
    pending: bool,
    /// A success (the server's version) or the failure's error line.
    ok: bool,
    message: String,
    /// A success whose `immich.verify` has not run yet — the first [`tick`] after it runs it.
    fresh: bool,
}

/// The tab's test outcomes, in egui memory so they live as long as the app (never on disk).
fn settings_tests(ctx: &egui::Context) -> Option<Arc<Mutex<HashMap<String, SettingsTest>>>> {
    ctx.data(|d| d.get_temp::<Arc<Mutex<HashMap<String, SettingsTest>>>>(egui::Id::new("immich-settings-tests")))
}

/// Settings ▸ Integrations: the Immich servers — add one, test it, remove it. A server that
/// passed a test unlocks File ▸ Import from Immich; a new or re-added server starts locked.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::panels::settings::{heading, hint, row};

    heading(ui, t, "Immich");
    hint(
        ui,
        t,
        "Import from a self-hosted Immich server (File ▸ Import from Immich…). Add your server and Test it — the menu item unlocks once the test passes.",
    );

    let tests = {
        let id = egui::Id::new("immich-settings-tests");
        let tests = ui.data(|d| d.get_temp::<Arc<Mutex<HashMap<String, SettingsTest>>>>(id)).unwrap_or_else(|| Arc::new(Mutex::new(HashMap::new())));
        ui.data_mut(|d| d.insert_temp(id, tests.clone()));
        tests
    };
    let servers: Vec<(String, String, String, bool)> =
        app.session.immich_servers.iter().map(|s| (s.name.clone(), s.url.clone(), s.api_key.clone(), s.verified)).collect();
    let snapshot: HashMap<String, SettingsTest> = tests.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let mut run_test: Option<(String, String, String)> = None; // (name, url, key)
    let mut remove: Option<String> = None;
    for (name, url, key, verified) in &servers {
        let state = snapshot.get(name);
        let status = match state {
            Some(s) if s.pending => crate::i18n::tr("Testing…").to_string(),
            Some(s) => s.message.clone(),
            None if *verified => crate::i18n::tr("Tested — ready").to_string(),
            None => crate::i18n::tr("Not tested yet").to_string(),
        };
        let pending = state.is_some_and(|s| s.pending);
        let ok = state.map(|s| s.ok).unwrap_or(*verified);
        row(ui, t, name, |ui| {
            ui.label(egui::RichText::new(url.as_str()).color(t.text_dim));
            let r = ui.label(egui::RichText::new(status).color(if ok { t.text_dim } else { t.caution }));
            register(ui.ctx(), format!("label:immichSettingsStatus-{name}"), r.rect);
            let r = ui.add_enabled(!pending, egui::Button::new(crate::i18n::tr("Test")));
            register(ui.ctx(), format!("button:immichSettingsTest-{name}"), r.rect);
            if r.clicked() {
                run_test = Some((name.clone(), url.clone(), key.clone()));
            }
            let r = ui.add(egui::Button::new(crate::i18n::tr("Remove")));
            register(ui.ctx(), format!("button:immichSettingsRemove-{name}"), r.rect);
            if r.clicked() {
                remove = Some(name.clone());
            }
        });
    }

    // the add form: the address and the key the user created in Immich (never shown, never saved
    // anywhere but the engine's prefs once Add succeeds)
    let (url_id, key_id) = (egui::Id::new("immich-settings-url"), egui::Id::new("immich-settings-key"));
    let mut add_url = ui.data(|d| d.get_temp::<String>(url_id)).unwrap_or_default();
    let mut add_key = ui.data(|d| d.get_temp::<String>(key_id)).unwrap_or_default();
    let mut add_clicked = false;
    heading(ui, t, "Add a server");
    hint(ui, t, "Create the key in Immich under Settings ▸ API keys.");
    row(ui, t, "Server address", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut add_url).hint_text("https://photos.example.com:2283").desired_width(300.0));
        register(ui.ctx(), "field:immichSettingsUrl", r.rect);
    });
    row(ui, t, "API key", |ui| {
        // plain text, not a password field: the user is copying a key they just created, and
        // a masked field invites paste mistakes that only surface as a 401 later
        let r = ui.add(egui::TextEdit::singleline(&mut add_key).desired_width(300.0));
        register(ui.ctx(), "field:immichSettingsKey", r.rect);
    });
    row(ui, t, "", |ui| {
        let ready = !add_url.trim().is_empty() && !add_key.trim().is_empty();
        let r = ui.add_enabled(ready, egui::Button::new(crate::i18n::tr("Add server")));
        register(ui.ctx(), "button:immichSettingsAdd", r.rect);
        add_clicked = r.clicked();
    });

    if let Some((name, url, key)) = run_test {
        tests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), SettingsTest { pending: true, ok: false, message: String::new(), fresh: false });
        let (ctx, tests2, name2) = (ui.ctx().clone(), tests.clone(), name.clone());
        let spawned = spawn("lc-immich-settings-test", &ctx, move || {
            let r: Result<String, String> = lightcraft_engine::guard::catch("immich settings test", || {
                let c = Client::new(&url, &key, Limits::default()).map_err(|e| e.to_string())?;
                let pong = c.ping().map_err(|e| e.to_string())?;
                let v = c.version().map_err(|e| e.to_string())?;
                Ok(format!("{pong} — Immich {v}"))
            })
            .flatten();
            let (ok, message) = match r {
                Ok(m) => (true, m),
                Err(e) => (false, e),
            };
            tests2.lock().unwrap_or_else(|e| e.into_inner()).insert(name2, SettingsTest { pending: false, ok, message, fresh: ok });
        });
        if let Err(e) = spawned {
            tests.lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
            settings_message(ui, false, e);
        }
    }
    if let Some(name) = remove {
        match app.run("immich.servers", json!({ "remove": { "name": name } })) {
            Ok(_) => {
                tests.lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
                settings_message(ui, true, crate::i18n::tr("Removed.").to_string());
            }
            Err(e) => settings_message(ui, false, e),
        }
    }
    if add_clicked {
        match app.run("immich.servers", json!({ "add": { "url": add_url.trim(), "apiKey": add_key.trim() } })) {
            Ok(_) => {
                add_url.clear();
                add_key.clear();
                settings_message(ui, true, crate::i18n::tr("Added. Test it to unlock Import from Immich.").to_string());
            }
            Err(e) => settings_message(ui, false, e),
        }
    }
    ui.data_mut(|d| {
        d.insert_temp(url_id, add_url);
        d.insert_temp(key_id, add_key);
    });
    if let Some((ok, text)) = ui.data(|d| d.get_temp::<Option<(bool, String)>>(egui::Id::new("immich-settings-msg"))).flatten() {
        let r = ui.label(egui::RichText::new(text).color(if ok { t.text_dim } else { t.caution }));
        register(ui.ctx(), "label:immichSettingsMessage", r.rect);
    }
}

/// The tab's status line (the last action's outcome), in egui memory.
fn settings_message(ui: &mut egui::Ui, ok: bool, text: String) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new("immich-settings-msg"), Some((ok, text))));
}

/// A settings-tab Test that finished successfully gets its `immich.verify` here — every frame,
/// so the menu unlocks even if the user closed Settings while the test was running.
fn verify_fresh_settings_tests(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(tests) = settings_tests(ctx) else { return };
    let mut to_verify: Vec<String> = Vec::new();
    {
        let mut map = tests.lock().unwrap_or_else(|e| e.into_inner());
        for (name, test) in map.iter_mut() {
            if test.ok && test.fresh && !test.pending {
                to_verify.push(name.clone());
                test.fresh = false;
            }
        }
    }
    for name in to_verify {
        let _ = app.run("immich.verify", json!({ "server": name }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_accepts_a_year_a_month_or_a_full_day() {
        assert_eq!(parse_date("2026-01-31").as_deref(), Some("2026-01-31"));
        assert_eq!(parse_date("  2026-01-31 ").as_deref(), Some("2026-01-31"));
        // partial dates mean the start of that year or month
        assert_eq!(parse_date("2026").as_deref(), Some("2026-01-01"));
        assert_eq!(parse_date("2026-06").as_deref(), Some("2026-06-01"));
        assert_eq!(parse_date("2026-6"), None, "the month is padded");
        assert_eq!(parse_date("2026-1-31"), None, "padded only");
        assert_eq!(parse_date("2026-13-01"), None, "month 13");
        assert_eq!(parse_date("2026-00-10"), None, "month 0");
        assert_eq!(parse_date("2026-04-32"), None, "day over 31");
        assert_eq!(parse_date("2026-01-31T10:00"), None, "no timestamps");
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("2026-01-3"), None);
        assert_eq!(parse_date("202"), None, "a short year");
        assert_eq!(parse_date("26"), None);
        // a full-width lookalike cannot slip past the byte checks
        assert_eq!(parse_date("2026－01-31"), None);
    }

    #[test]
    fn filters_map_to_a_search_query() {
        let mut d = ImmichDialog {
            server: "home".into(),
            album_id: "alb-1".into(),
            favorite: true,
            rating: 4,
            from_date: "2026-02-01".into(),
            ..Default::default()
        };
        let q = d.query_for(3);
        assert_eq!(q.album_ids, vec!["alb-1".to_string()]);
        assert_eq!(q.is_favorite, Some(true));
        assert_eq!(q.rating, Some(4));
        assert_eq!(q.created_after.as_deref(), Some("2026-02-01"));
        assert_eq!(q.asset_type.as_deref(), Some("IMAGE"), "movies are not photos");
        assert_eq!((q.page, q.page_size), (3, 60));
        // unchecked filters are simply not sent
        d.favorite = false;
        d.rating = 0;
        d.from_date = "keep typing".into();
        d.album_id.clear();
        let q = d.query_for(0);
        assert!(q.album_ids.is_empty());
        assert_eq!(q.is_favorite, None, "an unchecked box is no filter, not favorites-off");
        assert_eq!(q.rating, None);
        assert_eq!(q.created_after, None, "an unparseable date is not sent");
        assert_eq!(q.page, 1, "pages are 1-based");
        // a hostile page is clamped, not trusted
        assert_eq!(d.query_for(u32::MAX).page, 100_000);
    }

    #[test]
    fn click_toggles_one_and_shift_sets_a_range() {
        let mut d = ImmichDialog {
            assets: (0..6).map(|i| ImmichAsset { id: format!("a{i}"), file_name: format!("{i}.jpg"), ..Default::default() }).collect(),
            checked: vec![false; 6],
            ..Default::default()
        };
        d.click(1, false);
        assert_eq!(d.checked, [false, true, false, false, false, false]);
        d.click(1, false);
        assert_eq!(d.checked, [false, false, false, false, false, false]);
        d.click(1, false);
        d.click(4, true);
        assert_eq!(d.checked, [false, true, true, true, true, false]);
        assert_eq!(d.selected_ids(), ["a1", "a2", "a3", "a4"]);
        // backwards from the new anchor (4) un-checks 2..=4
        d.click(2, true);
        assert_eq!(d.checked, [false, true, false, false, false, false]);
        // a stale anchor (a page went by) toggles just one
        d.last_clicked = Some(99);
        d.click(5, true);
        assert_eq!(d.checked, [false, true, false, false, false, true]);
        // out of range is ignored
        d.click(99, false);
        assert_eq!(d.checked, [false, true, false, false, false, true]);
    }

    #[test]
    fn staged_names_stay_safe_and_unique() {
        let dir = std::env::temp_dir().join(format!("lc-immich-ui-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(safe_file_name("../../etc/pa:sswd.jpg", "id1"), "pa_sswd.jpg", "the directory is dropped, not flattened");
        assert_eq!(safe_file_name("", "id123456"), "immich-id123456");
        let a = staged_path(&dir, "x.jpg", "id1");
        std::fs::File::create(&a).unwrap(); // the worker reserves the name before the next lookup
        let b = staged_path(&dir, "x.jpg", "id2");
        assert_eq!(a.file_name().unwrap().to_string_lossy(), "x.jpg");
        assert_eq!(b.file_name().unwrap().to_string_lossy(), "x-1.jpg");
        // an extension that is not one stays in the stem
        assert_eq!(staged_path(&dir, "weird.nope.nope", "id3").file_name().unwrap().to_string_lossy(), "weird.nope.nope");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
