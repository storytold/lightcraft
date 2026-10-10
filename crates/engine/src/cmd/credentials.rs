//! Where account secrets are kept (Settings → Connections → Key storage): the system keychain, or
//! an encrypted file opened with a passphrase once per session. Used automatically where the
//! keychain is missing (macOS, Windows, a desktop without a Secret Service) or locked.
//!
//! None of these commands is journaled; `credentials.unlock` takes the passphrase and no result
//! ever contains it or a stored secret.

use std::sync::Arc;

use dac_credentials::{CredError, FileStore, Secret, SecretStore};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::remote::Msg;
use crate::{Result, Session};

fn cred_error(e: &CredError) -> Value {
    json!({"kind": e.kind(), "message": e.to_string()})
}

/// The system keychain's state, probed once per session (or on `refresh`).
fn system_state(s: &mut Session, refresh: bool) -> Value {
    if refresh || s.remote.system_error.is_none() {
        s.remote.system_error = Some(dac_credentials::os_store().err());
    }
    match s.remote.system_error.clone().flatten() {
        None => json!({"state": "ok"}),
        Some(e) => json!({"state": e.kind(), "message": e.to_string()}),
    }
}

/// Is the active store the encrypted file?
fn file_active(s: &Session) -> bool {
    s.remote.store.as_ref().is_some_and(|x| x.name() == FILE_STORE_NAME)
}

const FILE_STORE_NAME: &str = "encrypted file";

/// A [`FileStore`] under a name the status can tell apart.
struct Named(FileStore);

impl SecretStore for Named {
    fn name(&self) -> &'static str {
        FILE_STORE_NAME
    }
    fn get(&self, key: &dac_credentials::Key) -> std::result::Result<Option<Secret>, CredError> {
        self.0.get(key)
    }
    fn set(&self, key: &dac_credentials::Key, secret: &Secret) -> std::result::Result<(), CredError> {
        self.0.set(key, secret)
    }
    fn delete(&self, key: &dac_credentials::Key) -> std::result::Result<bool, CredError> {
        self.0.delete(key)
    }
}

pub(crate) fn adopt_file(s: &mut Session, f: FileStore) {
    s.remote.store = Some(Arc::new(Named(f)));
    s.remote.unlock_error = None;
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let system = system_state(s, bool_or(p, "refresh", false));
    let pref = s.store_pref();
    let active = s.remote.store.as_ref().map(|x| x.name());
    let file = s.remote.secrets_file.clone();
    let exists = file.as_ref().is_some_and(|f| f.exists());
    // the passphrase is needed when nothing else can keep the keys
    let needs = active.is_none() && (pref == "file" || system["state"] != "ok");
    Ok(json!({
        "active": active,
        "preference": pref,
        "system": system,
        "file": {"path": file.map(|f| f.to_string_lossy().to_string()), "exists": exists, "unlocked": file_active(s)},
        "needsPassphrase": needs,
        "unlocking": s.remote.unlocking,
        "error": s.remote.unlock_error.as_ref().map(cred_error),
    }))
}

/// Open (or create) the encrypted key file with `passphrase` and use it for this session.
fn unlock(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "credentials.unlock";
    let pass = str_param(p, "passphrase").filter(|x| !x.is_empty()).ok_or_else(|| bad(C, "missing `passphrase`"))?;
    let path = s.remote.secrets_file.clone().ok_or_else(|| bad(C, "no settings folder for the encrypted key file"))?;
    let created = !path.exists();
    if created && pass.chars().count() < 8 {
        return Err(bad(C, "choose a passphrase of at least 8 characters"));
    }
    let pass = Secret::new(pass);
    if s.remote.unlocking {
        return Ok(json!({"started": false, "reason": "an unlock is already running"}));
    }
    if bool_or(p, "background", false) {
        // deriving the key takes a moment (Argon2id): keep the window responsive
        s.remote.unlocking = true;
        s.remote.unlock_error = None;
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::FileUnlocked(FileStore::open(&path, &pass)));
        });
        return Ok(json!({"started": true, "created": created}));
    }
    match FileStore::open(&path, &pass) {
        Ok(f) => {
            adopt_file(s, f);
            s.set_store_pref("file").map_err(|e| bad(C, e))?;
            Ok(json!({"ok": true, "store": FILE_STORE_NAME, "created": created}))
        }
        Err(e) => Ok(json!({"ok": false, "error": cred_error(&e)})),
    }
}

/// Forget the unlocked file store (the passphrase is asked again).
fn lock(s: &mut Session, _: &Value) -> Result<Value> {
    let was = file_active(s);
    if was {
        s.remote.store = None;
    }
    Ok(json!({"locked": was}))
}

/// Keep keys in the system keychain or the encrypted file from now on. Keys already stored stay
/// where they are: reconnect the accounts after switching.
fn use_store(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "credentials.useStore";
    let store = match str_param(p, "store") {
        Some(x @ ("system" | "file")) => x,
        _ => return Err(bad(C, "`store` must be `system` or `file`")),
    };
    s.set_store_pref(store).map_err(|e| bad(C, e))?;
    if store == "system" && file_active(s) || store == "file" && !file_active(s) {
        s.remote.store = None;
    }
    Ok(json!({"preference": store}))
}

/// Ask the system keychain to unlock (it shows its own prompt); the answer arrives via `remote.pump`.
fn unlock_system(s: &mut Session, _: &Value) -> Result<Value> {
    if s.remote.unlocking {
        return Ok(json!({"started": false, "reason": "an unlock is already running"}));
    }
    let store = match dac_credentials::os_store() {
        Ok(x) => x,
        Err(e) => return Ok(json!({"ok": false, "error": cred_error(&e)})),
    };
    s.remote.unlocking = true;
    s.remote.unlock_error = None;
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        let _ = tx.send(Msg::SystemUnlocked(store.unlock()));
    });
    Ok(json!({"started": true}))
}

/// `remote.pump`'s part: finished unlocks.
pub(crate) fn take_in(s: &mut Session, m: Msg) -> Option<Msg> {
    match m {
        Msg::FileUnlocked(r) => {
            s.remote.unlocking = false;
            match r {
                Ok(f) => {
                    adopt_file(s, f);
                    if let Err(e) = s.set_store_pref("file") {
                        log::warn!("could not save the key storage choice: {e}");
                    }
                }
                Err(e) => s.remote.unlock_error = Some(e),
            }
            None
        }
        Msg::SystemUnlocked(r) => {
            s.remote.unlocking = false;
            match r {
                Ok(()) => {
                    s.remote.system_error = None;
                    if !file_active(s) {
                        s.remote.store = None; // reopened on next use
                    }
                }
                Err(e) => s.remote.unlock_error = Some(e),
            }
            None
        }
        other => Some(other),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "credentials.status", "Key Storage Status", [], None, "{refresh?: bool — probe the system keychain again} → {active: store name|null, preference: system|file, system: {state: ok|unsupported|unavailable|locked, message?}, file: {path, exists, unlocked}, needsPassphrase, unlocking, error?}", always, status),
        cmd!(query "credentials.unlock", "Unlock Key File", [], None, "{passphrase, background?: bool} — open (or create, ≥ 8 characters) the encrypted key file and keep account keys there this session (never journaled) → {ok, store, created} | {ok: false, error: {kind: wrongPassphrase|corrupt|io, message}} | {started} (background: see credentials.status)", always, unlock),
        cmd!(query "credentials.lock", "Lock Key File", [], None, "forget the unlocked key file until its passphrase is given again → {locked}", always, lock),
        cmd!(query "credentials.useStore", "Choose Key Storage", [], None, "{store: system|file} — where account keys are kept from now on (existing keys stay where they are) → {preference}", always, use_store),
        cmd!(query "credentials.unlockSystem", "Unlock System Keychain", [], None, "ask the system keychain to unlock (it shows its own prompt), in the background → {started} | {ok: false, error}", always, unlock_system),
    ]
}
