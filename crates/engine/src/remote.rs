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

use dac_catalog::{PhotoId, Source};
use dac_credentials::{CredError, SecretStore};
use dac_immich::{Account, Accounts, Asset, ImmichError};

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
    },
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
        result: Result<dac_immich::ServerStatus, ImmichError>,
    },
    /// `credentials.unlock {background}`: the encrypted file opened (or not).
    FileUnlocked(Result<dac_credentials::FileStore, CredError>),
    /// `credentials.unlockSystem`: the keychain's own unlock prompt was answered.
    SystemUnlocked(Result<(), CredError>),
}

/// A server that answered `immich.connect`'s probe, with the key that opened it.
pub(crate) struct Probed {
    pub base: String,
    pub status: dac_immich::ServerStatus,
    pub pinned: Option<String>,
    pub key: dac_credentials::Secret,
}

/// What an import download job brings back.
pub(crate) struct Downloaded {
    pub account: String,
    pub link_only: bool,
    pub album_name: Option<String>,
    /// (asset, where it was saved) or the error.
    pub items: Vec<(Asset, Result<PathBuf, ImmichError>)>,
    pub skipped: usize,
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
    pub failed: Vec<(String, String)>,
}

pub struct State {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// The SHA-1 back-fill: a batch in flight; files that could not be read (not retried this session).
    pub(crate) sha1_busy: bool,
    pub(crate) sha1_failed: HashSet<PhotoId>,
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
    pub(crate) import: ImportProgress,
    pub(crate) fetching: HashSet<PhotoId>,
    pub(crate) fetch_errors: HashMap<PhotoId, String>,
    /// The encrypted key file ([`dac_credentials::FileStore`]), used where the system keychain is
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
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        State {
            tx,
            rx,
            sha1_busy: false,
            sha1_failed: HashSet::new(),
            sha1_done: 0,
            sha1_idle_at: None,
            connections_path: None,
            accounts: None,
            store: None,
            cache_dir: None,
            links: HashMap::new(),
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
/// their master's file and are skipped), minus those that failed this session.
pub(crate) fn sha1_pending(s: &crate::Session) -> Vec<(PhotoId, String)> {
    s.catalog
        .photos()
        .filter(|p| p.sha1.is_none() && p.copy_of.is_none() && !p.local && !s.remote.sha1_failed.contains(&p.id))
        .filter(|p| p.preview_only.as_deref() != Some(LINK_ONLY))
        .filter_map(|p| match &p.source {
            Source::File { path } => Some((p.id, path.clone())),
            Source::Demo { .. } => None,
        })
        .collect()
}

/// Hash `batch` on a worker thread.
pub(crate) fn spawn_sha1(tx: Sender<Msg>, batch: Vec<(PhotoId, String)>) {
    std::thread::spawn(move || {
        let out = batch
            .into_iter()
            .map(|(id, path)| {
                let h = dac_hash::sha1_file(Path::new(&path)).map(|d| d.to_hex()).ok();
                (id, path, h)
            })
            .collect();
        let _ = tx.send(Msg::Sha1(out));
    });
}

/// Why a link-only photo shows a preview ([`dac_catalog::Photo::preview_only`]): the original stays
/// on the server until the photo is opened in Develop.
pub const LINK_ONLY: &str = "Immich link only: the original is downloaded when the photo is opened in Develop";

/// What [`crate::Session::secret_store`] says when the keys need the encrypted file's passphrase.
pub const NEEDS_PASSPHRASE: &str = "unlock the encrypted key file with its passphrase (Settings → Connections)";

/// A secret store kept in memory (tests, and hosts that must not touch the keychain).
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<HashMap<dac_credentials::Key, dac_credentials::Secret>>);

impl SecretStore for MemorySecrets {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, key: &dac_credentials::Key) -> Result<Option<dac_credentials::Secret>, dac_credentials::CredError> {
        Ok(self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key).cloned())
    }
    fn set(&self, key: &dac_credentials::Key, secret: &dac_credentials::Secret) -> Result<(), dac_credentials::CredError> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.clone(), secret.clone());
        Ok(())
    }
    fn delete(&self, key: &dac_credentials::Key) -> Result<bool, dac_credentials::CredError> {
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
        self
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
        match dac_credentials::os_store() {
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
    pub fn immich_client(&mut self, account: &str) -> Result<(Account, dac_immich::Client), ImmichError> {
        let acc = self
            .immich_accounts()
            .map_err(ImmichError::Io)?
            .get(account)
            .cloned()
            .ok_or_else(|| ImmichError::NotFound(format!("account {account}")))?;
        let store = self.secret_store().map_err(ImmichError::Io)?;
        let key = store.get(&dac_immich::accounts::secret_key(&acc.id)).map_err(|e| ImmichError::Io(e.to_string()))?.ok_or(ImmichError::NoKey)?;
        let opts = dac_immich::ServerOptions { pinned: acc.pinned.clone(), ..dac_immich::ServerOptions::standard() };
        let c = dac_immich::Client::new(&acc.url, key, &opts)?;
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
