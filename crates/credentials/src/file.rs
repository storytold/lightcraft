//! The fallback store: one JSON file whose payload is the secrets map encrypted with
//! XChaCha20-Poly1305 under a key derived from a passphrase with Argon2id (RFC 9106).
//!
//! Sources: RFC 9106 (Argon2), RFC 8439 and draft-irtf-cfrg-xchacha (XChaCha20-Poly1305);
//! file layout is own design.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{CredError, Key, Secret, SecretStore};

const FORMAT: u32 = 1;
/// Bound to the ciphertext so a payload can't be moved into another kind of file.
const AAD: &[u8] = b"credentials-file-v1";
/// Largest file read.
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// Argon2id cost for new files (OWASP's minimum recommendation: 19 MiB, 2 passes).
const M_KIB: u32 = 19 * 1024;
const T: u32 = 2;
const P: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: u32,
    kdf: String,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
    nonce: String,
    data: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

fn random<const N: usize>() -> Result<[u8; N], CredError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| CredError::Io(format!("no randomness: {e}")))?;
    Ok(b)
}

fn derive(passphrase: &Secret, salt: &[u8], m_kib: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; 32]>, CredError> {
    // bound the cost read from the file: a doctored header must not exhaust memory or time
    if !(8..=1024 * 1024).contains(&m_kib) || !(1..=16).contains(&t) || !(1..=16).contains(&p) {
        return Err(CredError::Corrupt("key derivation parameters out of range".into()));
    }
    let params = argon2::Params::new(m_kib, t, p, Some(32)).map_err(|e| CredError::Corrupt(e.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon.hash_password_into(passphrase.expose().as_bytes(), salt, key.as_mut()).map_err(|e| CredError::Corrupt(e.to_string()))?;
    Ok(key)
}

type Map = BTreeMap<String, String>;

fn map_key(k: &Key) -> String {
    format!("{}\n{}", k.service, k.account)
}

/// Secrets in a passphrase-encrypted file. Every call reads and (for changes) rewrites the file,
/// so several handles on one file stay consistent.
pub struct FileStore {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: Vec<u8>,
    lock: Mutex<()>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore").field("path", &self.path).finish_non_exhaustive()
    }
}

impl FileStore {
    /// Open `path` with `passphrase`, or create an empty store there (written on the first
    /// [`SecretStore::set`]). A wrong passphrase for an existing file is [`CredError::WrongPassphrase`].
    pub fn open(path: &Path, passphrase: &Secret) -> Result<FileStore, CredError> {
        match read_envelope(path)? {
            Some(env) => {
                let salt = unhex(&env.salt).ok_or_else(|| CredError::Corrupt("bad salt".into()))?;
                let key = derive(passphrase, &salt, env.m_kib, env.t, env.p)?;
                let store = FileStore { path: path.to_path_buf(), key, salt, lock: Mutex::new(()) };
                store.decrypt(&env)?; // proves the passphrase
                Ok(store)
            }
            None => {
                let salt = random::<16>()?.to_vec();
                let key = derive(passphrase, &salt, M_KIB, T, P)?;
                Ok(FileStore { path: path.to_path_buf(), key, salt, lock: Mutex::new(()) })
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, CredError> {
        XChaCha20Poly1305::new_from_slice(self.key.as_ref()).map_err(|_| CredError::Corrupt("bad key length".into()))
    }

    fn decrypt(&self, env: &Envelope) -> Result<Map, CredError> {
        if env.format != FORMAT || env.kdf != "argon2id" {
            return Err(CredError::Corrupt(format!("unknown format {}", env.format)));
        }
        let nonce: [u8; 24] = unhex(&env.nonce).and_then(|n| n.try_into().ok()).ok_or_else(|| CredError::Corrupt("bad nonce".into()))?;
        let data = unhex(&env.data).ok_or_else(|| CredError::Corrupt("bad payload".into()))?;
        let plain =
            Zeroizing::new(self.cipher()?.decrypt(&XNonce::from(nonce), Payload { msg: &data, aad: AAD }).map_err(|_| CredError::WrongPassphrase)?);
        let map: Map = serde_json::from_slice(&plain).map_err(|_| CredError::Corrupt("bad payload".into()))?;
        Ok(map)
    }

    fn load(&self) -> Result<Map, CredError> {
        match read_envelope(&self.path)? {
            Some(env) => {
                if unhex(&env.salt).as_deref() != Some(&self.salt[..]) {
                    return Err(CredError::Corrupt("the file was replaced while open; open it again".into()));
                }
                self.decrypt(&env)
            }
            None => Ok(Map::new()),
        }
    }

    fn save(&self, map: &Map) -> Result<(), CredError> {
        let plain = Zeroizing::new(serde_json::to_vec(map).map_err(|e| CredError::Io(e.to_string()))?);
        let nonce = random::<24>()?;
        let data =
            self.cipher()?.encrypt(&XNonce::from(nonce), Payload { msg: &plain, aad: AAD }).map_err(|_| CredError::Io("encryption failed".into()))?;
        let env = Envelope {
            format: FORMAT,
            kdf: "argon2id".into(),
            m_kib: M_KIB,
            t: T,
            p: P,
            salt: hex(&self.salt),
            nonce: hex(&nonce),
            data: hex(&data),
        };
        let text = serde_json::to_vec_pretty(&env).map_err(|e| CredError::Io(e.to_string()))?;
        write_atomic(&self.path, &text)
    }
}

fn io(path: &Path, e: std::io::Error) -> CredError {
    CredError::Io(format!("{}: {e}", path.display()))
}

fn read_envelope(path: &Path) -> Result<Option<Envelope>, CredError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    if meta.len() > MAX_FILE {
        return Err(CredError::Corrupt("file too large".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| io(path, e))?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| CredError::Corrupt("not a credentials file".into()))
}

/// Write to a temporary file (owner-only on Unix), sync, then rename over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CredError> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(|e| io(&tmp, e))?;
    f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| io(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))
}

impl SecretStore for FileStore {
    fn name(&self) -> &'static str {
        "encrypted file"
    }

    fn get(&self, key: &Key) -> Result<Option<Secret>, CredError> {
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(self.load()?.get(&map_key(key)).map(|s| Secret::new(s.clone())))
    }

    fn set(&self, key: &Key, secret: &Secret) -> Result<(), CredError> {
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.load()?;
        map.insert(map_key(key), secret.expose().to_string());
        self.save(&map)?;
        log::info!("credentials: stored a secret for {} in the encrypted file", key.service);
        Ok(())
    }

    fn delete(&self, key: &Key) -> Result<bool, CredError> {
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.load()?;
        let removed = map.remove(&map_key(key)).is_some();
        if removed {
            self.save(&map)?;
            log::info!("credentials: removed a secret for {} from the encrypted file", key.service);
        }
        Ok(removed)
    }
}
