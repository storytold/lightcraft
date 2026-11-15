//! Background work for remote services, run off the session thread: the SHA-1 back-fill of
//! originals (the link key for Immich), and the Immich jobs (link pass, import downloads, fetching
//! the original of a link-only photo). Workers send results over a channel; the per-frame
//! `remote.pump` command takes them in and writes the catalog, so the window never waits for the
//! network or the disk.
//!
//! Secrets: the API key is read from the secret store when a job starts and lives only in the
//! job's client; it is never logged, journaled, or put into a command's result.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use lcc_catalog::{PhotoId, Source};
use lcc_credentials::{CredError, SecretStore};
use lcc_immich::{Account, Accounts, Asset, ImmichError};

/// Files hashed per back-fill batch (one worker thread).
pub(crate) const SHA1_BATCH: usize = 32;

/// A unit of finished background work.
pub(crate) enum Msg {
    Sha1(Vec<(PhotoId, String, Option<String>)>),
    LinkPage {
        account: String,
        assets: Vec<Asset>,
    },
    LinkDone {
        account: String,
        result: Result<Option<String>, ImmichError>,
        /// The cursor file of the library the pass was started in (the cursor is the catalog's).
        home: Option<PathBuf>,
    },
    /// The checksum lookup of newly imported / hashed photos finished ([`State::link_queue`]).
    LinkChecked,
    /// A server call made with `background: true` finished: its result, kept under `key` until
    /// `immich.answer` takes it.
    Answer {
        key: String,
        value: serde_json::Value,
    },
    /// `immich.importPeople {background}`: the faces were read; the catalog is written on the
    /// session thread.
    People(Box<crate::cmd::immich::sync::PeopleDone>),
    Downloaded(Box<Downloaded>),
    Original {
        photo: PhotoId,
        account: String,
        result: Result<PathBuf, ImmichError>,
    },
    /// `immich.connect {background}`: the server answered (or not); the key goes to the store.
    Connected(Box<Result<Probed, ImmichError>>),
    /// `immich.status {check, background}`: one account's server status.
    Checked {
        account: String,
        result: Result<lcc_immich::ServerStatus, ImmichError>,
    },
    /// `immich.uploadVersion`: an Edit In version upload finished.
    Version(Box<crate::cmd::immich::versions::VersionDone>),
    /// `immich.sync {background}`: a sync run finished.
    Synced(Box<crate::cmd::immich::sync::SyncDone>),
    /// `immich.pullSidecars` / `immich.pushSidecars` {background}: a sidecar run finished.
    Sidecars(Box<SidecarsDone>),
    /// The Immich albums shown as collections were listed (device sync).
    ImmichAlbums(Box<crate::cmd::immich::collections::Listing>),
    /// Photos added to Immich collections here were uploaded (or found) and put into the albums.
    ImmichPushed(Box<crate::cmd::immich::collections::Pushed>),
    ImmichPushDone,
    /// `credentials.unlock {background}`: the encrypted file opened (or not).
    FileUnlocked(Result<lcc_credentials::FileStore, CredError>),
    /// `credentials.unlockSystem`: the keychain's own unlock prompt was answered.
    SystemUnlocked(Result<(), CredError>),
}

/// A server that answered `immich.connect`'s probe, with the key that opened it.
pub(crate) struct Probed {
    pub base: String,
    pub status: lcc_immich::ServerStatus,
    pub pinned: Option<String>,
    pub key: lcc_credentials::Secret,
}

/// Which way a sidecar run goes (IMM-SIDECARS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidecarDirection {
    /// Immich → catalog (and optionally the local sidecar files).
    Pull,
    /// Catalog → Immich (external-library files, and the supported metadata of managed assets).
    Push,
}

impl SidecarDirection {
    pub fn name(self) -> &'static str {
        match self {
            SidecarDirection::Pull => "pull",
            SidecarDirection::Push => "push",
        }
    }
}

/// One photo's part of a sidecar run.
#[derive(Clone, Debug)]
pub(crate) struct SidecarItem {
    pub photo: PhotoId,
    pub asset: String,
    pub file_name: String,
    /// Pull: the asset's `updatedAt`, for the local-edit check.
    pub updated_at: Option<String>,
    /// Pull: the sidecar's XMP as the server served it.
    pub xmp: Option<String>,
    /// Push: a metadata or tag change was sent to Immich.
    pub pushed: bool,
    pub error: Option<String>,
    /// A failure that ends the run (offline, bad key, TLS): the rest would fail the same way.
    pub fatal: Option<ImmichError>,
    /// Why the photo was left alone (`noSidecar`, `locked`, `trashed`, …).
    pub skipped: Option<&'static str>,
}

impl Default for SidecarItem {
    fn default() -> Self {
        SidecarItem {
            photo: PhotoId(0),
            asset: String::new(),
            file_name: String::new(),
            updated_at: None,
            xmp: None,
            pushed: false,
            error: None,
            fatal: None,
            skipped: None,
        }
    }
}

/// What a sidecar worker brings back. The pull's catalog changes are made on the session thread
/// (`sidecars::finish`); the push's Immich writes already happened on the worker.
pub(crate) struct SidecarsDone {
    pub direction: SidecarDirection,
    pub items: Vec<SidecarItem>,
    pub errors: Vec<String>,
    pub fatal: Option<ImmichError>,
    pub dry_run: bool,
    /// Pull: write the sidecars next to the originals too.
    pub write_files: bool,
    /// Push: external-library assets whose sidecar files the app just wrote (Immich re-reads them).
    pub refresh: Vec<String>,
    /// Pull: a photo edited here wins unless `overwrite`.
    pub overwrite: bool,
    /// Pull: there were more linked photos than one run covers (the cap is reported, not hidden).
    pub truncated: bool,
    /// Push: the external-library files written (and the local failures / skips) on the session
    /// thread before the worker started.
    pub local_written: Vec<String>,
    pub local_failed: Vec<serde_json::Value>,
    pub local_skipped: Vec<serde_json::Value>,
}

/// One photo's sidecar push work (the session builds it from the catalog; the worker sends it).
#[derive(Clone, Debug)]
pub(crate) struct SidecarPush {
    pub photo: PhotoId,
    pub asset: String,
    pub update: lcc_immich::types::AssetUpdate,
    /// The asset tags the catalog wants (`parent/child`).
    pub tags: Vec<String>,
    /// The catalog capture time, to which the worker adds the asset's own zone.
    pub captured: Option<String>,
    /// `full`: replace the whole sidecar through a temporary asset (IMM-SIDECARS).
    pub full: Option<FullSidecar>,
}

/// A full sidecar replacement for a managed asset: the app's packet, and a tiny unique placeholder
/// image to upload with it (Immich's API has no way to write an existing asset's sidecar; a
/// source asset's sidecar can be copied onto it, then the source is removed).
#[derive(Clone, Debug)]
pub(crate) struct FullSidecar {
    /// The app's XMP packet for the photo.
    pub packet: Vec<u8>,
    /// A unique placeholder image (never a user photo; deleted after the copy).
    pub placeholder: Vec<u8>,
    pub file_name: String,
    pub device_asset_id: String,
    /// ISO 8601 capture/import time for the placeholder's `fileCreatedAt`.
    pub created: String,
}

/// One photo's sidecar pull work.
#[derive(Clone, Debug)]
pub(crate) struct SidecarPull {
    pub photo: PhotoId,
    pub asset: String,
    pub file_name: String,
}

/// `immich.pullSidecars` / `immich.pushSidecars`: the run in flight and the last run's summary.
#[derive(Default)]
pub(crate) struct SidecarProgress {
    pub active: bool,
    /// `pull`, `push` or empty.
    pub direction: &'static str,
    /// Counts and the first errors of the last finished run (never every item).
    pub last: Option<serde_json::Value>,
}

impl SidecarProgress {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "active": self.active,
            "direction": self.direction,
            "last": self.last,
        })
    }
}

/// What an import download job brings back.
pub(crate) struct Downloaded {
    pub account: String,
    pub link_only: bool,
    pub album_name: Option<String>,
    /// (asset, where it was saved) or the error.
    pub items: Vec<(Asset, Result<PathBuf, ImmichError>)>,
    pub skipped: usize,
    /// Asked-for assets that are linked to a catalog photo already (not downloaded).
    pub already_linked: Vec<String>,
    /// Asked-for assets whose checksum the catalog already has (not downloaded): to be linked.
    pub present: Vec<Asset>,
}

/// Progress of one account's link pass.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkProgress {
    pub active: bool,
    pub seen: u64,
    pub linked: u64,
    pub probable: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// The last import from Immich.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub active: bool,
    pub requested: usize,
    pub imported: usize,
    pub skipped: usize,
    /// Of the skipped: assets whose photo the catalog already had (now linked, and in the album).
    pub present: usize,
    pub failed: Vec<(String, String)>,
}

pub struct State {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// The SHA-1 back-fill: a batch in flight; files that could not be read (not retried this session).
    pub(crate) sha1_busy: bool,
    pub(crate) sha1_failed: HashSet<PhotoId>,
    /// Photos to hash before the rest (device sync was told their SHA-1 and checks it here).
    pub(crate) sha1_first: std::collections::BTreeSet<PhotoId>,
    pub(crate) sha1_done: u64,
    /// Catalog revision when nothing was left to hash (skip the scan until it changes).
    pub(crate) sha1_idle_at: Option<u64>,
    /// Where `connections.json` lives (`None`: accounts are kept in memory only, as in tests).
    pub connections_path: Option<PathBuf>,
    pub(crate) accounts: Option<Accounts>,
    /// The secret store (the OS keychain unless the host set another).
    pub(crate) store: Option<Arc<dyn SecretStore>>,
    /// Where link-only previews and fetched originals go when the library is not a folder.
    pub cache_dir: Option<PathBuf>,
    pub(crate) links: HashMap<String, LinkProgress>,
    /// Photos that were just imported or got their SHA-1: `remote.pump` asks each connected
    /// server (on a worker) whether it has their checksums and links the ones it has. The link
    /// pass alone would never find them: it lists only assets changed since its last run, and
    /// these assets may have been on the server for years.
    pub(crate) link_queue: Vec<PhotoId>,
    /// That lookup is on a worker.
    pub(crate) link_checking: bool,
    /// Server calls running on workers for a command's `background: true` (their keys), and the
    /// finished ones' results until `immich.answer` takes them.
    pub(crate) asking: HashSet<String>,
    pub(crate) answers: HashMap<String, serde_json::Value>,
    pub(crate) import: ImportProgress,
    pub(crate) fetching: HashSet<PhotoId>,
    pub(crate) fetch_errors: HashMap<PhotoId, String>,
    /// The encrypted key file ([`lcc_credentials::FileStore`]), used where the system keychain is
    /// missing or locked, or when the user prefers it (`None`: no settings folder, as in tests).
    pub secrets_file: Option<PathBuf>,
    /// Where the store preference (`credentials.json`) lives.
    pub credentials_prefs: Option<PathBuf>,
    /// `system` (default) or `file`, loaded on first use.
    pub(crate) store_pref: Option<String>,
    /// Why the system keychain can't be used (probed once per session, or on `refresh`).
    pub(crate) system_error: Option<Option<CredError>>,
    /// Background credential work: an unlock running; the last unlock's error.
    pub(crate) unlocking: bool,
    pub(crate) unlock_error: Option<CredError>,
    /// `immich.connect {background}`: running; its last result (never with the key).
    pub(crate) connecting: bool,
    pub(crate) connected: Option<serde_json::Value>,
    /// `immich.status {check, background}`: accounts being checked; each one's last server status.
    pub(crate) checking: HashSet<String>,
    pub(crate) checks: HashMap<String, serde_json::Value>,
    /// IMM-SYNC per account: state between runs, the running flag, the schedule.
    pub(crate) sync: HashMap<String, crate::cmd::immich::sync::AccountSync>,
    /// IMM-SIDECARS: the sidecar pull/push in flight and the last run's summary.
    pub(crate) sidecars: SidecarProgress,
    /// Finished Edit In version uploads not yet shown (`immich.versionResults`).
    pub(crate) versions: Vec<serde_json::Value>,
    /// Immich albums as device-sync collections: workers and schedule.
    pub(crate) coll: crate::cmd::immich::collections::CollState,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        State {
            tx,
            rx,
            sha1_busy: false,
            sha1_failed: HashSet::new(),
            sha1_first: Default::default(),
            coll: Default::default(),
            sha1_done: 0,
            sha1_idle_at: None,
            connections_path: None,
            accounts: None,
            store: None,
            cache_dir: None,
            links: HashMap::new(),
            link_queue: Vec::new(),
            link_checking: false,
            asking: HashSet::new(),
            answers: HashMap::new(),
            import: ImportProgress::default(),
            fetching: HashSet::new(),
            fetch_errors: HashMap::new(),
            secrets_file: None,
            credentials_prefs: None,
            store_pref: None,
            system_error: None,
            unlocking: false,
            unlock_error: None,
            connecting: false,
            connected: None,
            checking: HashSet::new(),
            checks: HashMap::new(),
            sync: HashMap::new(),
            sidecars: SidecarProgress::default(),
            versions: Vec::new(),
        }
    }
}

impl State {
    pub(crate) fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }
    pub(crate) fn drain(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }
}

/// The photos whose SHA-1 is still unknown: library photos from files (virtual copies share
/// their master's file and are skipped), minus those that failed this session; at most `limit`
/// (one batch: a large library is not copied path by path for every batch).
pub(crate) fn sha1_pending(s: &crate::Session, limit: usize) -> Vec<(PhotoId, String)> {
    let first = s.remote.sha1_first.iter().filter_map(|id| s.catalog.photo(*id));
    let mut seen = std::collections::HashSet::new();
    first
        .chain(s.catalog.photos())
        .filter(|p| seen.insert(p.id))
        .filter(|p| p.sha1.is_none() && p.copy_of.is_none() && !p.local && !s.remote.sha1_failed.contains(&p.id))
        .filter(|p| p.preview_only.as_deref() != Some(LINK_ONLY))
        .filter_map(|p| match &p.source {
            Source::File { path } => Some((p.id, path.clone())),
            Source::Demo { .. } => None,
        })
        .take(limit)
        .collect()
}

/// Hash `batch` on a worker thread.
pub(crate) fn spawn_sha1(tx: Sender<Msg>, batch: Vec<(PhotoId, String)>) {
    std::thread::spawn(move || {
        let out = batch
            .into_iter()
            .map(|(id, path)| {
                let h = lcc_hash::sha1_file(Path::new(&path)).map(|d| d.to_hex()).ok();
                (id, path, h)
            })
            .collect();
        let _ = tx.send(Msg::Sha1(out));
    });
}

/// Why a link-only photo shows a preview ([`lcc_catalog::Photo::preview_only`]): the original stays
/// on the server until the photo is opened in Develop.
pub const LINK_ONLY: &str = "Immich link only: the original is downloaded when the photo is opened in Develop";

/// What [`crate::Session::secret_store`] says when the keys need the encrypted file's passphrase.
pub const NEEDS_PASSPHRASE: &str = "unlock the encrypted key file with its passphrase (Settings → Connections)";

/// A secret store kept in memory (tests, and hosts that must not touch the keychain).
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<HashMap<lcc_credentials::Key, lcc_credentials::Secret>>);

impl SecretStore for MemorySecrets {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, key: &lcc_credentials::Key) -> Result<Option<lcc_credentials::Secret>, lcc_credentials::CredError> {
        Ok(self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key).cloned())
    }
    fn set(&self, key: &lcc_credentials::Key, secret: &lcc_credentials::Secret) -> Result<(), lcc_credentials::CredError> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.clone(), secret.clone());
        Ok(())
    }
    fn delete(&self, key: &lcc_credentials::Key) -> Result<bool, lcc_credentials::CredError> {
        Ok(self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key).is_some())
    }
}

impl crate::Session {
    /// Keep connected accounts in `<config>/connections.json` (the desktop app, CLI and MCP).
    pub fn with_default_connections(mut self) -> Self {
        self.remote.connections_path = crate::config::config_dir().map(|d| d.join("connections.json"));
        self.remote.cache_dir = crate::config::config_dir().map(|d| d.join("immich"));
        self.remote.secrets_file = crate::config::config_dir().map(|d| d.join("credentials.enc"));
        self.remote.credentials_prefs = crate::config::config_dir().map(|d| d.join("credentials.json"));
        // the paired devices of peer sync live beside the connected accounts
        self.with_default_peers()
    }

    /// Use `store` for account secrets instead of the OS keychain.
    pub fn set_secret_store(&mut self, store: Arc<dyn SecretStore>) {
        self.remote.store = Some(store);
    }

    /// The connected Immich accounts (loaded on first use).
    pub fn immich_accounts(&mut self) -> Result<&mut Accounts, String> {
        if self.remote.accounts.is_none() {
            let a = match &self.remote.connections_path {
                Some(p) => Accounts::load(p)?,
                None => Accounts::default(),
            };
            self.remote.accounts = Some(a);
        }
        self.remote.accounts.as_mut().ok_or_else(|| "no accounts".to_string())
    }

    pub(crate) fn save_accounts(&mut self) -> Result<(), String> {
        let (Some(path), Some(a)) = (self.remote.connections_path.clone(), self.remote.accounts.as_ref()) else { return Ok(()) };
        a.save(&path)
    }

    /// The secret store: one set by the host or unlocked this session, else the system keychain
    /// (unless the user prefers the encrypted file). The error says what to do: unlock the
    /// encrypted file in Settings → Connections (`credentials.unlock`).
    pub(crate) fn secret_store(&mut self) -> Result<Arc<dyn SecretStore>, String> {
        if let Some(s) = &self.remote.store {
            return Ok(s.clone());
        }
        if self.store_pref() == "file" {
            return Err(NEEDS_PASSPHRASE.into());
        }
        match lcc_credentials::os_store() {
            Ok(s) => {
                let s: Arc<dyn SecretStore> = Arc::from(s);
                self.remote.system_error = Some(None);
                self.remote.store = Some(s.clone());
                Ok(s)
            }
            Err(e) => {
                let msg = format!("{e}; {NEEDS_PASSPHRASE}");
                self.remote.system_error = Some(Some(e));
                Err(msg)
            }
        }
    }

    /// `system` or `file`: where the user wants keys kept.
    pub(crate) fn store_pref(&mut self) -> String {
        if self.remote.store_pref.is_none() {
            let v = self
                .remote
                .credentials_prefs
                .as_ref()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["store"].as_str().map(str::to_string))
                .filter(|s| s == "file")
                .unwrap_or_else(|| "system".into());
            self.remote.store_pref = Some(v);
        }
        self.remote.store_pref.clone().unwrap_or_else(|| "system".into())
    }

    pub(crate) fn set_store_pref(&mut self, pref: &str) -> Result<(), String> {
        self.remote.store_pref = Some(pref.to_string());
        let Some(p) = self.remote.credentials_prefs.clone() else { return Ok(()) };
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::json!({"store": pref}).to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// A client for a connected account, with its stored key.
    pub fn immich_client(&mut self, account: &str) -> Result<(Account, lcc_immich::Client), ImmichError> {
        let acc = self
            .immich_accounts()
            .map_err(ImmichError::Io)?
            .get(account)
            .cloned()
            .ok_or_else(|| ImmichError::NotFound(format!("account {account}")))?;
        let store = self.secret_store().map_err(ImmichError::Io)?;
        let key = store.get(&lcc_immich::accounts::secret_key(&acc.id)).map_err(|e| ImmichError::Io(e.to_string()))?.ok_or(ImmichError::NoKey)?;
        let opts = lcc_immich::ServerOptions { pinned: acc.pinned.clone(), ..lcc_immich::ServerOptions::standard() };
        let c = lcc_immich::Client::new(&acc.url, key, &opts)?;
        Ok((acc, c))
    }

    /// Where Immich downloads go: `<library>/Immich` for a library on disk, else the cache folder.
    pub(crate) fn immich_dir(&self) -> Option<PathBuf> {
        match &self.library {
            Some(l) if l.on_disk => Some(l.dir.join("Immich")),
            _ => self.remote.cache_dir.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use lcc_catalog::{Op, Photo, PhotoId, Source};

    /// Gap 26: each SHA-1 batch takes only a batch of paths, not a copy of every unhashed path in
    /// the library (which made each pump of a 100k-photo library copy 100k strings).
    #[test]
    fn sha1_pending_stops_at_the_batch() {
        let mut s = crate::Session::new();
        for i in 0..100u64 {
            let p = Photo::new(PhotoId(i + 1), Source::File { path: format!("/x/{i}.jpg") }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00");
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let first = super::sha1_pending(&s, super::SHA1_BATCH);
        assert_eq!(first.len(), super::SHA1_BATCH);
        s.remote.sha1_failed.extend(first.iter().map(|(id, _)| *id));
        let next = super::sha1_pending(&s, super::SHA1_BATCH);
        assert!(next.iter().all(|(id, _)| !s.remote.sha1_failed.contains(id)));
        assert_eq!(super::sha1_pending(&s, 1000).len(), 100 - super::SHA1_BATCH);
    }
}
