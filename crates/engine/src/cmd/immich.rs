//! Talking to somebody's Immich from the engine: configuring servers, browsing them, and
//! importing photos from a self-hosted [Immich](https://immich.app) server.
//!
//! The servers are the whole configuration — URL, display name, and the API key the user created
//! in Immich. Nothing here knows any particular instance: no default host, no fallback, no
//! admin assumptions. Keys live in the user's per-user settings file
//! (`config_dir()/immich.json`, created `0600` on Unix), **never in the library**: the library
//! folder is what people back up and sync, and a key must not travel with it. The file holds the
//! keys unencrypted in v1; the OS-keychain follow-up is tracked with the design spec.
//!
//! Network calls are native only, like `lightcraft-fetch`. On wasm this module ships the
//! settings commands only. The CLI and MCP run `immich.import` synchronously (waiting for the
//! answer is the point); the UI's dialog runs the same command in `background` mode, where the
//! engine's own worker downloads and the session commits, so all three share one import path.
//! The dialog's searches and thumbnails still go over its own worker — they are browsing, not
//! import.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{EngineError, Result, Session};
/// One Immich server the user configured: where it is, what to call it, and the key they created
/// in Immich's Settings ▸ API keys. `verified` is set when a test reached the server and it
/// answered — the UI keeps File ▸ Import from Immich disabled until some server has it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImmichServer {
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub verified: bool,
}

/// The Immich part of the user's per-user settings (the config folder), not of a library.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImmichPrefs {
    pub servers: Vec<ImmichServer>,
}

const MAX_SERVERS: usize = 32;

/// Where the user's Immich settings (the servers and their API keys) are kept: the per-user
/// config folder, never inside a library. A session may override the path (`immich_store`) for
/// tests, so they never read or write the real file.
fn store_path(s: &Session) -> Option<std::path::PathBuf> {
    s.immich_store.clone().or_else(|| crate::config::config_dir().map(|d| d.join("immich.json")))
}

/// The configured servers. A missing file is an empty list; an unreadable or corrupt one is
/// reported and starts empty — the next save replaces it, and no command panics on it.
pub fn load(s: &Session) -> Vec<ImmichServer> {
    let Some(p) = store_path(s) else {
        return Vec::new();
    };
    match std::fs::read_to_string(&p) {
        Ok(text) => match serde_json::from_str::<ImmichPrefs>(&text) {
            Ok(v) => v.servers,
            Err(e) => {
                log::warn!("immich: the settings file {} could not be read ({e}); starting empty", p.display());
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Persist the configured servers. On Unix the file is created `0600` at birth — the temp file
/// and the renamed result alike — so a key never exists in a mode the owner did not choose.
pub fn save(s: &Session) -> Result<()> {
    let p = store_path(s).ok_or_else(|| EngineError::Other("immich: no per-user settings folder is available on this platform".to_string()))?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::Other(format!("immich: could not create the settings folder: {e}")))?;
    }
    let v = serde_json::to_vec_pretty(&ImmichPrefs { servers: s.immich_servers.clone() })
        .map_err(|e| EngineError::Other(format!("immich: could not encode the settings: {e}")))?;
    write_private(&p, &v)
}

/// Write `data` to `path` atomically (temp file in the same folder, then rename), with the file
/// born owner-only on Unix. The temp name is unique per call, so a crashed writer's leftover
/// cannot make the next save fail.
fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static STORE_SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "immich.json".to_string());
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = path.with_file_name(format!(".{name}.{}-{nanos:x}-{:x}.tmp", std::process::id(), STORE_SEQ.fetch_add(1, Ordering::Relaxed)));
    let mut f = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)
        }
    }
    .map_err(|e| EngineError::Other(format!("immich: could not create the settings file: {e}")))?;
    if let Err(e) = f.write_all(data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(EngineError::Other(format!("immich: could not write the settings file: {e}")));
    }
    if let Err(e) = f.sync_all() {
        let _ = std::fs::remove_file(&tmp);
        return Err(EngineError::Other(format!("immich: could not sync the settings file: {e}")));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(EngineError::Other(format!("immich: could not save the settings file: {e}")));
    }
    Ok(())
}

/// One background `immich.import` run (the dialog's path). The engine's own worker threads do
/// the lookups, the downloads and the file-system half of the import (probing, hashing,
/// copying — the same split the app's own importer uses, so no frame waits on a disk); the
/// session thread commits the catalog through the same `library.import` the synchronous path
/// uses — one undo step for the whole import, into the library the run started in. The UI reads
/// the public fields every frame; the CLI and MCP never see this, they wait on the command.
#[cfg(not(target_arch = "wasm32"))]
pub struct ImmichImportJob {
    /// The ids being imported.
    pub total: usize,
    /// Files whose download ended (successfully or not).
    pub done: usize,
    /// The file downloading now ("" between files).
    pub current_file: String,
    /// How much of the current file has arrived, and the announced total (0 when the server did not say).
    pub file_done: u64,
    pub file_total: u64,
    /// One `{id, error}` per asset the server would not give (read while running and at the end).
    pub failed: Vec<Value>,
    /// `None` while it runs; at the end, `{report, failed}` (or `{failed, error}`).
    pub finished: Option<Value>,
    /// True once the user asked to stop it (the finished downloads still join).
    pub cancelled: bool,
    rx: Option<std::sync::mpsc::Receiver<ImportMsg>>,
    client: std::sync::Arc<lightcraft_immich::Client>,
    staging: std::path::PathBuf,
    mode: String,
    /// The album the run joins when it commits (`immich.import`'s `album` / `albumName`, checked
    /// when the run starts so a bad id fails before anything downloads).
    album: Option<u64>,
    album_name: Option<String>,
    /// The library generation when the run started: photo ids are per library, so the completion
    /// refuses to join a library opened after it.
    library: u64,
    /// The commit half once the downloads are in: a worker readies the files, the session commits
    /// what comes back.
    commit: Option<CommitPhase>,
    /// The activity-stack row for this run (docs/background-tasks.md). Held for exactly as long
    /// as the job runs — through the commit, until `finished` is set (then the row is gone).
    activity: Option<crate::activity::TaskGuard>,
}

/// The commit half of a background run (see [`ImmichImportJob::commit`]): what the session waits
/// on, and what it needs to commit the batch the worker readied.
#[cfg(not(target_arch = "wasm32"))]
struct CommitPhase {
    rx: std::sync::mpsc::Receiver<CommitMsg>,
    /// The options (and starting time — [`crate::import::commit_prepared`] wants the original)
    /// the commit runs with, kept here because the `ImportJob` they came from moves to the worker.
    opts: crate::import::ImportOptions,
    now: String,
    /// Files the worker has found and handled so far, for the activity row while it prepares.
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// What the commit worker reports back: the readied files (with the probes a later import could
/// reuse), or the failure when it died (a panic in the file-system half is an error, never a
/// crash).
#[cfg(not(target_arch = "wasm32"))]
pub enum CommitMsg {
    Prepared { batch: crate::import::Prepared, cache: std::collections::HashMap<String, crate::media::ProbeInfo> },
    Failed(String),
}

#[cfg(not(target_arch = "wasm32"))]
enum ImportMsg {
    /// A new file starts downloading: its staged name.
    Start { name: String },
    /// Progress of the current file (bytes arrived, announced total).
    Progress { done: u64, total: u64 },
    /// The run is over: the staged files that should join the library, and the fetch failures.
    Done { paths: Vec<String>, failed: Vec<Value> },
}

#[cfg(not(target_arch = "wasm32"))]
impl ImmichImportJob {
    /// The Cancel button: in-flight transfers stop at the next chunk, no new file is started,
    /// and the finished downloads still join the library.
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.client.set_cancelled(true);
    }

    /// Drop the job because its owner went away (the dialog closed mid-import): stop the worker
    /// and drop what never joined the library. Best effort — the worker may be mid-write.
    pub fn abandon(&mut self) {
        self.cancelled = true;
        self.client.set_cancelled(true);
        let _ = std::fs::remove_dir_all(&self.staging);
    }

    /// How much of the current file has arrived (1.0 when nothing is downloading).
    pub fn file_fraction(&self) -> f32 {
        if self.file_total == 0 {
            return 0.0;
        }
        (self.file_done as f32 / self.file_total as f32).min(1.0)
    }

    /// The `library.import` mode the run commits in.
    pub fn mode(&self) -> &str {
        &self.mode
    }

    /// Where the run's files are staged (cleaned up by the session once they join).
    pub fn staging(&self) -> &std::path::Path {
        &self.staging
    }

    /// The library generation the run started in (photo ids are per library; the session refuses
    /// to commit after a switch).
    pub fn library(&self) -> u64 {
        self.library
    }

    /// What the session needs to commit the batch the worker readied: the options the import
    /// runs with, its timestamp, and the album the run joins.
    pub fn commit_parts(&self) -> (crate::import::ImportOptions, String, Option<u64>, Option<String>) {
        let phase = self.commit.as_ref().expect("the commit is running");
        (phase.opts.clone(), phase.now.clone(), self.album, self.album_name.clone())
    }

    /// The commit worker's outcome, once it is in (the session commits the batch — the catalog
    /// half — with [`Session::poll_immich_import`]).
    pub fn take_commit(&mut self) -> Option<CommitMsg> {
        self.commit.as_ref()?.rx.try_recv().ok()
    }

    /// The run's outcome is in (the session committed, or refused to): remember it and let the
    /// activity row go.
    pub fn finish(&mut self, value: Value) {
        self.finished = Some(value);
        self.commit = None;
        self.activity = None;
    }

    /// Take in what the workers brought back. Returns the staged files exactly once (when the
    /// download worker is done — the failures are in `failed` by then); the session then starts
    /// the commit half and stores the run's outcome in `finished` once that lands.
    pub fn poll(&mut self) -> Option<Vec<String>> {
        let mut done = None;
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    // the worker downloads one file at a time: a new start means the last one ended
                    ImportMsg::Start { name } => {
                        if !self.current_file.is_empty() {
                            self.done += 1;
                        }
                        self.current_file = name;
                        self.file_done = 0;
                        self.file_total = 0;
                    }
                    ImportMsg::Progress { done, total } => {
                        self.file_done = done;
                        if total > 0 {
                            self.file_total = total;
                        }
                    }
                    ImportMsg::Done { paths, failed } => {
                        self.failed = failed;
                        done = Some(paths);
                    }
                }
            }
        }
        // the activity row mirrors the run: files as progress, the current file (with its byte
        // progress) as detail — while the commit readies them, the files it has handled
        if let Some(row) = self.activity.as_ref() {
            if let Some(phase) = self.commit.as_ref() {
                row.progress(
                    phase.done.load(std::sync::atomic::Ordering::Relaxed) as u64,
                    phase.total.load(std::sync::atomic::Ordering::Relaxed) as u64,
                );
            } else {
                row.progress(self.done as u64, self.total as u64);
                if !self.current_file.is_empty() {
                    if self.file_total > 0 {
                        row.detail(&format!("{} · {}", self.current_file, bytes_of(self.file_done, self.file_total)));
                    } else {
                        row.detail(&self.current_file);
                    }
                }
            }
        }
        if done.is_some() {
            self.rx = None;
            self.current_file.clear();
            self.done = self.total;
            if let Some(row) = self.activity.as_ref() {
                row.detail("");
            }
            // the row stays: the commit half is still to run — `finish` lets it go
        }
        done
    }

    /// The downloads are in: set the run up to commit. [`crate::import::ImportJob::prepare`] —
    /// probing, hashing, copying, several files at once — runs on a worker of its own (the app's
    /// own importer uses the same split, so no frame waits on a disk); the session commits the
    /// batch that comes back as one `library.import` undo step.
    pub fn start_commit(&mut self, s: &mut Session, paths: Vec<String>) {
        let mode = if self.mode == "add" { crate::import::ImportMode::Add } else { crate::import::ImportMode::Copy };
        let job = match crate::import::ImportJob::new(s, crate::import::ImportOptions { mode, ..Default::default() }) {
            Ok(job) => job,
            Err(e) => {
                self.finish(json!({ "failed": self.failed.clone(), "error": e.to_string() }));
                return;
            }
        };
        let (total, done) = (job.total.clone(), job.done.clone());
        let (opts, now) = (job.opts.clone(), job.now().to_string());
        let cancel = self.client.cancel_flag();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("lc-immich-commit".into())
            .spawn({
                let cancel = cancel.clone();
                move || run_commit_worker(tx, job, paths, cancel)
            })
            .map_err(|e| format!("immich.import: could not start the commit worker: {e}"));
        match worker {
            // a dead worker simply never answers; the catch in `run_commit_worker` always does
            Ok(_handle) => self.commit = Some(CommitPhase { rx, opts, now, total, done }),
            Err(e) => self.finish(json!({ "failed": self.failed.clone(), "error": e })),
        }
    }
}

/// Download `id`'s original into `staging` under the given name, reporting progress. Returns the
/// staged file, or the failure (as `immich.import` reports it).
#[cfg(not(target_arch = "wasm32"))]
fn download_one(
    c: &lightcraft_immich::Client,
    staging: &std::path::Path,
    id: &str,
    name: &str,
    progress: impl FnMut(u64, u64),
) -> std::result::Result<std::path::PathBuf, Value> {
    let dest = staged_path(staging, name);
    match std::fs::File::create(&dest) {
        Ok(mut f) => c.download_original(id, &mut f, progress).map_err(|e| json!({ "id": id, "error": e.to_string() })).and_then(|n| {
            if n == 0 {
                let _ = std::fs::remove_file(&dest);
                Err(json!({ "id": id, "error": "the server sent an empty file" }))
            } else {
                Ok(dest)
            }
        }),
        Err(e) => Err(json!({ "id": id, "error": format!("could not write the download: {e}") })),
    }
}

/// The name a download lands under: the server's file name when it is usable, else the id — cut
/// at a char boundary (agent-supplied text is not).
#[cfg(not(target_arch = "wasm32"))]
fn name_of(c: &lightcraft_immich::Client, id: &str) -> String {
    let given = c.asset(id).map(|a| a.original_file_name).unwrap_or_default();
    safe_file_name(&given, id)
}

fn valid_url(url: &str) -> bool {
    url.len() <= 2000
        && (url.starts_with("http://") || url.starts_with("https://"))
        && url.chars().all(|c| !c.is_control() && !c.is_whitespace())
        && url.find("//").is_some_and(|i| !url[i + 2..].starts_with('/'))
}

/// `http://host:2283/prefix` → `host:2283` — the display name when the user did not pick one.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    if host.is_empty() { url.to_string() } else { host.to_string() }
}

impl ImmichServer {
    fn normalise(url: &str) -> String {
        url.trim().trim_end_matches('/').to_string()
    }
    fn matches(&self, sel: &str) -> bool {
        !sel.is_empty() && (self.name.eq_ignore_ascii_case(sel) || Self::normalise(&self.url) == Self::normalise(sel))
    }
    /// What commands may show: everything except the key.
    fn public(&self) -> Value {
        json!({
            "name": self.name,
            "url": self.url,
            "insecure": !self.url.starts_with("https://"),
            "key": if self.api_key.is_empty() { "missing" } else { "set" },
            "verified": self.verified,
        })
    }
}

fn current(s: &Session) -> Value {
    json!({ "servers": s.immich_servers.iter().map(ImmichServer::public).collect::<Vec<_>>() })
}

/// Read, add and remove servers. The key goes in and never comes back out.
fn servers(s: &mut Session, p: &Value) -> Result<Value> {
    const CID: &str = "immich.servers";
    if let Some(a) = p.get("add") {
        let url = str_param(a, "url").unwrap_or_default().trim().to_string();
        if !valid_url(&url) {
            return Err(bad(CID, "`url` must be the http:// or https:// address of your Immich server (e.g. https://photos.lan:2283)"));
        }
        let key = str_param(a, "apiKey").unwrap_or_default().trim().to_string();
        if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
            return Err(bad(CID, "`apiKey` is required — create one in Immich under Settings ▸ API keys"));
        }
        let name = match str_param(a, "name").map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) if n.chars().count() <= 100 && !n.chars().any(char::is_control) => n.to_string(),
            Some(_) => return Err(bad(CID, "`name` is too long")),
            None => host_of(&url),
        };
        // a fresh entry is unverified — File ▸ Import from Immich unlocks only after a Test
        let entry = ImmichServer { name, url, api_key: key, verified: false };
        match s.immich_servers.iter_mut().find(|x| ImmichServer::normalise(&x.url) == ImmichServer::normalise(&entry.url)) {
            Some(existing) => *existing = entry,
            None => {
                if s.immich_servers.len() >= MAX_SERVERS {
                    return Err(bad(CID, "too many servers configured (32)"));
                }
                if s.immich_servers.iter().any(|x| x.name.eq_ignore_ascii_case(&entry.name)) {
                    return Err(bad(CID, format!("a server named `{}` already exists — remove it first", entry.name)));
                }
                s.immich_servers.push(entry);
            }
        }
        s.save_immich()?;
    }
    if let Some(rm) = p.get("remove") {
        let sel = [str_param(rm, "name"), str_param(rm, "url")].into_iter().flatten().next().unwrap_or_default().to_string();
        if sel.trim().is_empty() {
            return Err(bad(CID, "`remove` needs a `name` or `url`"));
        }
        let before = s.immich_servers.len();
        s.immich_servers.retain(|x| !x.matches(sel.trim()));
        if s.immich_servers.len() == before {
            return Err(bad(CID, format!("no Immich server `{sel}`")));
        }
        s.save_immich()?;
    }
    Ok(current(s))
}

#[cfg(not(target_arch = "wasm32"))]
fn net(cid: &'static str) -> impl Fn(lightcraft_immich::Error) -> EngineError {
    move |e| EngineError::Other(format!("{cid}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn server_of<'a>(s: &'a Session, p: &Value, cid: &'static str) -> Result<&'a ImmichServer> {
    let sel = str_param(p, "server").unwrap_or("").trim().to_string();
    if s.immich_servers.is_empty() {
        return Err(bad(cid, "no Immich server is configured — immich.servers {add: {url, apiKey}}"));
    }
    if sel.is_empty() {
        return s.immich_servers.first().ok_or_else(|| bad(cid, "no server"));
    }
    s.immich_servers.iter().find(|x| x.matches(&sel)).ok_or_else(|| {
        let names: Vec<&str> = s.immich_servers.iter().map(|x| x.name.as_str()).collect();
        bad(cid, format!("unknown Immich server `{sel}` (configured: {})", names.join(", ")))
    })
}

/// An ISO-8601-ish date parameter: present, bounded, and free of control characters.
#[cfg(not(target_arch = "wasm32"))]
fn date_param(p: &Value, key: &str, cid: &'static str) -> Result<Option<String>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(d)) => {
            let d = d.trim();
            if d.is_empty() {
                return Ok(None);
            }
            if d.len() > 64 || d.chars().any(char::is_control) {
                return Err(bad(cid, format!("`{key}` must be an ISO 8601 date like 2026-01-01 or 2026-01-01T00:00:00Z")));
            }
            Ok(Some(d.to_string()))
        }
        _ => Err(bad(cid, format!("`{key}` must be a date string"))),
    }
}

/// Is the Immich server up, and what does it report? The settings dialog's "Test" button.
/// A passing test marks the server verified (File ▸ Import from Immich needs one that is).
#[cfg(not(target_arch = "wasm32"))]
fn test(s: &mut Session, p: &Value) -> Result<Value> {
    const CID: &str = "immich.test";
    let (url, key) = {
        let srv = server_of(s, p, CID)?;
        (srv.url.clone(), srv.api_key.clone())
    };
    let c = lightcraft_immich::Client::new(&url, &key, Default::default()).map_err(net(CID))?;
    let pong = c.ping().map_err(net(CID))?;
    let version = c.version().map_err(net(CID))?;
    mark_verified(s, &url);
    s.save_immich()?;
    Ok(json!({ "ok": true, "pong": pong, "version": version.to_string() }))
}

/// Record that `server` passed a test. The UI runs its tests on a worker thread (the UI thread
/// never waits for the network), so it calls this with the answer; `immich.test` calls it itself.
/// No network happens here — the claim is only as good as the test that made it.
fn verify(s: &mut Session, p: &Value) -> Result<Value> {
    const CID: &str = "immich.verify";
    let sel = str_param(p, "server").unwrap_or("").trim().to_string();
    if sel.is_empty() {
        return Err(bad(CID, "`server` is the name or URL of a configured Immich server"));
    }
    let url = s.immich_servers.iter().find(|x| x.matches(&sel)).map(|x| x.url.clone()).ok_or_else(|| {
        let names: Vec<&str> = s.immich_servers.iter().map(|x| x.name.as_str()).collect();
        bad(CID, format!("unknown Immich server `{sel}` (configured: {})", names.join(", ")))
    })?;
    mark_verified(s, &url);
    s.save_immich()?;
    Ok(current(s))
}

/// Flag every entry with this URL as verified (there is at most one — `add` replaces by URL).
fn mark_verified(s: &mut Session, url: &str) {
    for x in &mut s.immich_servers {
        if ImmichServer::normalise(&x.url) == ImmichServer::normalise(url) {
            x.verified = true;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn albums(s: &mut Session, p: &Value) -> Result<Value> {
    const CID: &str = "immich.albums";
    let srv = server_of(s, p, CID)?;
    let c = lightcraft_immich::Client::new(&srv.url, &srv.api_key, Default::default()).map_err(net(CID))?;
    let list = c.albums().map_err(net(CID))?;
    Ok(json!({
        "albums": list.iter().map(|a| json!({ "id": a.id, "name": a.album_name, "assetCount": a.asset_count })).collect::<Vec<_>>(),
    }))
}

/// One page of a search. Answers are the crate's tolerant DTOs, so a 3.x server that adds or
/// drops fields changes what is `None`, never the shape of this answer.
#[cfg(not(target_arch = "wasm32"))]
fn browse(s: &mut Session, p: &Value) -> Result<Value> {
    use lightcraft_immich::SearchQuery;
    const CID: &str = "immich.browse";
    let srv = server_of(s, p, CID)?;
    let c = lightcraft_immich::Client::new(&srv.url, &srv.api_key, Default::default()).map_err(net(CID))?;
    let rating = match p.get("rating") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64() {
            Some(n @ 1..=5) => Some(n as u32),
            _ => return Err(bad(CID, "`rating` is a whole number 1…5")),
        },
    };
    let mut q = SearchQuery {
        is_favorite: match p.get("isFavorite") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_bool().ok_or_else(|| bad(CID, "`isFavorite` must be true or false"))?),
        },
        rating,
        created_after: date_param(p, "createdAfter", CID)?,
        created_before: date_param(p, "createdBefore", CID)?,
        updated_after: date_param(p, "updatedAfter", CID)?,
        asset_type: match p.get("type") {
            None | Some(Value::Null) => None,
            Some(Value::String(t)) if t == "IMAGE" || t == "VIDEO" => Some(t.clone()),
            Some(_) => return Err(bad(CID, "`type` is \"IMAGE\" or \"VIDEO\"")),
        },
        page: p.get("page").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100_000) as u32,
        page_size: p.get("pageSize").and_then(Value::as_u64).unwrap_or(60).clamp(1, 1000) as u32,
        ..Default::default()
    };
    if let Some(album) = str_param(p, "album").map(str::trim).filter(|a| !a.is_empty()) {
        let albums = c.albums().map_err(net(CID))?;
        match albums.iter().find(|a| a.id == album || a.album_name.eq_ignore_ascii_case(album)) {
            Some(a) => q.album_ids = vec![a.id.clone()],
            None => return Err(bad(CID, format!("no album `{album}` on the server — immich.albums lists them"))),
        }
    }
    let page = c.search(&q).map_err(net(CID))?;
    Ok(json!({
        "page": page.page,
        "maybeMore": page.maybe_more,
        "assets": page.items.iter().map(|a| json!({
            "id": a.id,
            "checksum": a.checksum,
            "fileName": a.original_file_name,
            "createdAt": a.file_created_at,
            "isFavorite": a.is_favorite,
            "rating": a.rating,
        })).collect::<Vec<_>>(),
    }))
}

/// Strip a server-supplied name down to one safe file name for the staging folder.
#[cfg(not(target_arch = "wasm32"))]
fn safe_file_name(given: &str, id: &str) -> String {
    let base = given.rsplit(['/', '\\']).next().unwrap_or(given);
    let mut out: String =
        base.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let trimmed = out.trim_matches(|c| c == ' ' || c == '.');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        // a byte cut here would panic on a multi-byte id (the name is agent- or server-supplied)
        let short: String = id.chars().take(64).collect();
        return format!("immich-{short}");
    }
    if out.chars().count() > 200 {
        out = out.chars().take(200).collect();
    }
    out
}

/// One fresh staging folder per import: a per-process sequence plus the clock make the name
/// unpredictable (a pid alone is guessable), and on Unix the folder is created `0700`, so a
/// neighbour on the same box cannot list it or plant a file in it before the import pipeline
/// reads it. It is removed again when the import ends.
/// Not inside the library — the pipeline never copies a file that already sits inside the
/// library folder (import.rs treats it as added-in-place), and `copy` is the point here.
#[cfg(not(target_arch = "wasm32"))]
fn staging_dir() -> Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("lightcraft-immich-{}-{nanos:x}-{:x}", std::process::id(), STAGING_SEQ.fetch_add(1, Ordering::Relaxed)));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|e| EngineError::Other(format!("immich.import: could not create the staging folder: {e}")))?;
    Ok(dir)
}

#[cfg(not(target_arch = "wasm32"))]
fn staged_path(staging: &std::path::Path, name: &str) -> std::path::PathBuf {
    let stem = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or_else(|| (name.to_string(), String::new()));
    for n in 0..1000u32 {
        let candidate = if n == 0 { staging.join(name) } else { staging.join(format!("{}-{n}{}", stem.0, stem.1)) };
        if !candidate.exists() {
            return candidate;
        }
    }
    staging.join(format!("immich-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)))
}

/// Download the named assets, then run them through the untouched import pipeline (probe,
/// content-hash dedupe, undoable add/copy). `copy` (the default) places the originals in the
/// library like any other import; `add` keeps the staged files in place.
///
/// `background: true` (the dialog's path) starts the engine's own worker and returns at once;
/// the run then lives in `Session::immich_import`, polled with `poll_immich_import`. Without it
/// (CLI, MCP) the command does the work and waits, like before.
#[cfg(not(target_arch = "wasm32"))]
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const CID: &str = "immich.import";
    let ids: Vec<String> = match p.get("ids").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => {
            let mut v = Vec::with_capacity(a.len());
            for x in a {
                let Some(x) = x.as_str().filter(|x| !x.is_empty() && x.len() <= 500 && !x.chars().any(char::is_control)) else {
                    return Err(bad(CID, "`ids` must be asset id strings from immich.browse"));
                };
                v.push(x.to_string());
            }
            v
        }
        _ => return Err(bad(CID, "`ids` — asset ids from immich.browse — is required")),
    };
    if ids.len() > 10_000 {
        return Err(bad(CID, "too many ids at once (10 000)"));
    }
    let mode = str_param(p, "mode").unwrap_or("copy");
    if mode != "copy" && mode != "add" {
        return Err(bad(CID, "`mode` is \"copy\" (default — into the library) or \"add\" (keep the downloaded files where they are)"));
    }
    let srv = server_of(s, p, CID)?;
    let client = std::sync::Arc::new(lightcraft_immich::Client::new(&srv.url, &srv.api_key, Default::default()).map_err(net(CID))?);
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    // the album the run joins at the end, checked up front so a bad id fails before anything
    // downloads (`library.import` checks it too, but the background completion would only find
    // out after the whole run)
    let album = p.get("album").and_then(Value::as_u64);
    if let Some(a) = album
        && s.catalog.album(lightcraft_catalog::AlbumId(a)).is_none_or(|al| al.folder || al.is_smart())
    {
        return Err(bad(CID, "album must be a regular album"));
    }
    let album_name = str_param(p, "albumName").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    if background {
        if s.immich_import.as_ref().is_some_and(|job| job.finished.is_none()) {
            return Err(EngineError::Other("immich.import: an import is already running".to_string()));
        }
        // one activity-stack row for the whole run (docs/background-tasks.md): the row's ✕,
        // `activity.cancel` and the dialog's Cancel all set the client's cancel flag, which the
        // workers check between files and mid-transfer
        let activity = s.activity.start("import", "Importing from Immich", crate::activity::Cancel::Flag(client.cancel_flag()));
        activity.progress(0, ids.len() as u64);
        let staging = staging_dir()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("lc-immich-import".into())
            .spawn({
                let client = client.clone();
                let staging = staging.clone();
                let ids = ids.clone();
                move || run_download_worker(tx, client, staging, ids)
            })
            .map_err(|e| EngineError::Other(format!("{CID}: could not start the import worker: {e}")))?;
        s.immich_import = Some(ImmichImportJob {
            total: ids.len(),
            done: 0,
            current_file: String::new(),
            file_done: 0,
            file_total: 0,
            failed: Vec::new(),
            finished: None,
            cancelled: false,
            rx: Some(rx),
            client,
            staging,
            mode: mode.to_string(),
            album,
            album_name,
            library: s.library_generation(),
            commit: None,
            activity: Some(activity),
        });
        let _ = worker; // a dead worker simply never answers; its sends fail and the job ends empty
        return Ok(json!({ "importing": ids.len() }));
    }
    let staging = staging_dir()?;
    let mut paths: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for id in &ids {
        let name = name_of(client.as_ref(), id);
        match download_one(client.as_ref(), &staging, id, &name, |_, _| {}) {
            Ok(dest) => paths.push(dest.to_string_lossy().into_owned()),
            Err(f) => failed.push(f),
        }
    }
    if paths.is_empty() {
        let _ = std::fs::remove_dir(&staging);
        return Err(EngineError::Other(format!("{CID}: nothing could be downloaded — {failed:?}")));
    }
    let mut params = json!({ "paths": paths, "mode": mode });
    for key in ["album", "albumName"] {
        if let Some(v) = p.get(key) {
            params[key] = v.clone();
        }
    }
    let report = s.execute("library.import", &params)?;
    if mode == "copy" {
        // the bytes are in the library now; the staged copies have no further use
        for path in &paths {
            if let Err(e) = std::fs::remove_file(path) {
                log::warn!("immich: staging cleanup: {path}: {e}");
            }
        }
        let _ = std::fs::remove_dir(&staging);
    }
    Ok(json!({ "report": report, "failed": failed }))
}

/// The background worker: look up each id, download its original into `staging`, and report.
/// The client's cancel flag stops it between files and mid-transfer; whatever is staged when it
/// exits still joins the library.
#[cfg(not(target_arch = "wasm32"))]
fn run_download_worker(
    tx: std::sync::mpsc::Sender<ImportMsg>,
    client: std::sync::Arc<lightcraft_immich::Client>,
    staging: std::path::PathBuf,
    ids: Vec<String>,
) {
    let mut paths: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for id in &ids {
        if client.cancelled() {
            break;
        }
        let name = name_of(client.as_ref(), id);
        let _ = tx.send(ImportMsg::Start { name: name.clone() });
        // one channel message per megabyte is plenty for a progress bar
        let mut last_mb: u64 = u64::MAX;
        match download_one(client.as_ref(), &staging, id, &name, |done, total| {
            let mb = done / (1024 * 1024);
            if mb != last_mb {
                last_mb = mb;
                let _ = tx.send(ImportMsg::Progress { done, total });
            }
        }) {
            Ok(dest) => paths.push(dest.to_string_lossy().into_owned()),
            Err(f) => failed.push(f),
        }
    }
    let _ = tx.send(ImportMsg::Done { paths, failed });
}

/// The commit worker: ready the staged files — expand, probe, hash, copy/place, the file-system
/// half any import runs ([`crate::import::ImportJob::prepare_files`]) — and hand the batch back.
/// The client's cancel flag stops it between files, like the download worker's. If the session
/// dropped the run meanwhile, what a Move placed is taken back (copies stay, like the ones of a
/// failed commit).
#[cfg(not(target_arch = "wasm32"))]
fn run_commit_worker(
    tx: std::sync::mpsc::Sender<CommitMsg>,
    mut job: crate::import::ImportJob,
    paths: Vec<String>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    // one batch: the commit is one undo step, like the synchronous path's
    let r = crate::guard::catch("immich import", move || {
        let files = job.expand(&paths);
        let batch = job.prepare_files(files, &cancel);
        (batch, std::mem::take(&mut job.cache))
    });
    match r {
        Ok((batch, cache)) => match tx.send(CommitMsg::Prepared { batch, cache }) {
            Ok(()) => {}
            // the session dropped the run meanwhile: take back what a Move placed (copies stay)
            Err(unsent) => {
                if let CommitMsg::Prepared { batch, .. } = unsent.0 {
                    batch.rollback();
                }
            }
        },
        Err(e) => {
            let _ = tx.send(CommitMsg::Failed(e));
        }
    }
}

/// "50 of 100 KB" / "12.3 of 45.6 MB" — the current file's byte progress, for the activity row's
/// detail line.
#[cfg(not(target_arch = "wasm32"))]
fn bytes_of(done: u64, total: u64) -> String {
    let one = |n: u64| -> String { if n >= 1024 * 1024 { format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)) } else { format!("{} KB", n / 1024) } };
    format!("{} of {}", one(done), one(total))
}

pub fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        cmd!(
            query "immich.servers",
            "Immich Servers",
            [],
            None,
            "{add?: {url, apiKey, name?}, remove?: {name|url}} — the self-hosted Immich servers this user talks to; keys live in the per-user settings file (config folder, 0600) — never in a library — unencrypted, and are never echoed. A new or re-added server starts unverified → {servers: [{name, url, insecure, key, verified}]}",
            always,
            servers
        ),
        cmd!(
            query "immich.verify",
            "Verify Immich Server",
            [],
            None,
            "{server} — record that the server passed a test (the UI tests on a worker thread and calls this with the answer; immich.test marks it itself). No network: the claim is only as good as the test behind it → {servers}",
            always,
            verify
        ),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    {
        v.push(cmd!(
            query "immich.test",
            "Test Immich Server",
            [],
            None,
            "{server?} — is it Immich, and what version → {ok, pong, version}",
            always,
            test
        ));
        v.push(cmd!(
            query "immich.albums",
            "Immich Albums",
            [],
            None,
            "{server?} — the server's albums → {albums: [{id, name, assetCount}]}",
            always,
            albums
        ));
        v.push(cmd!(
            query "immich.browse",
            "Browse Immich",
            [],
            None,
            "{server?, album? (name or id), isFavorite?, rating? (1-5), createdAfter?/createdBefore? (ISO date), updatedAfter?, type? (IMAGE|VIDEO), page? (1-based), pageSize? (1-1000, default 60)} — one page of assets on the server → {page, maybeMore, assets: [{id, checksum, fileName, createdAt, isFavorite, rating}]}",
            always,
            browse
        ));
        v.push(cmd!(
            "immich.import",
            "Import from Immich",
            [],
            None,
            "{server?, ids: [assetId...], mode? (\"copy\" default, into the library; \"add\" keeps the files in the staging folder), album?|albumName? (a regular album's id — checked when the run starts; the photos join it), background? (true = run on the engine's worker and return at once, for the UI; poll Session::immich_import, then it is {report, failed}; the run joins the library it started in — a library switched under it ends it with {failed, error})} — download the assets and run the ordinary import over them (dedupe, undo) → {report, failed: [{id, error}]} (synchronous: {importing} once started in background mode)",
            always,
            import
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_names_stay_safe_and_unique() {
        let dir = std::env::temp_dir().join(format!("lc-immich-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(safe_file_name("../../etc/pa:sswd.jpg", "id1"), "pa_sswd.jpg", "the directory is dropped, not flattened");
        assert_eq!(safe_file_name("", "id123456"), "immich-id123456");
        let a = staged_path(&dir, "x.jpg");
        std::fs::File::create(&a).unwrap(); // the worker reserves the name before the next lookup
        let b = staged_path(&dir, "x.jpg");
        assert_eq!(a.file_name().unwrap().to_string_lossy(), "x.jpg");
        assert_eq!(b.file_name().unwrap().to_string_lossy(), "x-1.jpg");
        // an extension that is not one stays in the stem
        assert_eq!(staged_path(&dir, "weird.nope.nope").file_name().unwrap().to_string_lossy(), "weird.nope.nope");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_counts_finished_files_and_shares_the_current_files_bytes() {
        // the activity row has to move while the run goes: one finished file per Start (the
        // worker downloads one file at a time), and the current file's bytes in the detail
        let (tx, rx) = std::sync::mpsc::channel();
        let client = std::sync::Arc::new(lightcraft_immich::Client::new("http://127.0.0.1:1", "k", Default::default()).unwrap());
        let activity = crate::activity::Activity::default();
        let guard = activity.start("import", "Importing from Immich", crate::activity::Cancel::Flag(client.cancel_flag()));
        let mut job = ImmichImportJob {
            total: 2,
            done: 0,
            current_file: String::new(),
            file_done: 0,
            file_total: 0,
            failed: Vec::new(),
            finished: None,
            cancelled: false,
            rx: Some(rx),
            client,
            staging: std::path::PathBuf::from("/nonexistent"),
            mode: "copy".into(),
            album: None,
            album_name: None,
            library: 0,
            commit: None,
            activity: Some(guard),
        };
        tx.send(ImportMsg::Start { name: "a.jpg".into() }).unwrap();
        tx.send(ImportMsg::Progress { done: 50 * 1024, total: 100 * 1024 }).unwrap();
        assert!(job.poll().is_none());
        let row = &activity.list()[0];
        assert_eq!((row.done, row.total), (0, 2), "the first file is still in flight");
        assert_eq!(row.detail, "a.jpg · 50 KB of 100 KB", "the row shows the current file and its bytes");
        tx.send(ImportMsg::Start { name: "b.jpg".into() }).unwrap();
        assert!(job.poll().is_none());
        assert_eq!(job.done, 1, "the first file ended when the next one started");
        assert_eq!(activity.list()[0].done, 1, "the row's count moved");
        tx.send(ImportMsg::Done { paths: vec!["/x/a.jpg".into(), "/x/b.jpg".into()], failed: vec![] }).unwrap();
        assert_eq!(job.poll().expect("the downloads ended"), vec!["/x/a.jpg".to_string(), "/x/b.jpg".to_string()]);
        assert_eq!(job.done, 2, "the run counts every file at the end");
        assert_eq!(activity.list()[0].detail, "", "no stale file name while the commit readies them");
    }
}
