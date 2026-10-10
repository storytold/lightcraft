//! SFTP upload of a generated site (pure Rust: russh + russh-sftp, ring crypto backend).
//!
//! Host keys are trust-on-first-use: with no `known_fingerprint` the server's key is accepted and
//! its SHA-256 fingerprint returned so the caller can store it with the server preset; with one,
//! a different key refuses the connection (the error names both fingerprints).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// An upload server preset (the secret lives in the keychain, never here).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The remote folder the site goes into (created when missing).
    pub path: String,
    /// A private key file; empty = password authentication.
    pub key_file: String,
    /// The server's host key fingerprint (`SHA256:…`), filled on first connection.
    pub known_fingerprint: String,
}

/// How to authenticate.
pub enum Auth {
    Password(String),
    /// A private key's text (OpenSSH / PEM) and its passphrase.
    Key {
        pem: String,
        passphrase: Option<String>,
    },
}

/// What an upload did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploaded {
    pub files: usize,
    pub bytes: u64,
    /// The server's host key fingerprint (store it in the preset).
    pub fingerprint: String,
}

struct Handler {
    known: String,
    seen: Arc<Mutex<String>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
            PublicKeyOrCertificate::Certificate(c) => c.public_key().fingerprint(HashAlg::Sha256).to_string(),
        };
        let ok = self.known.is_empty() || self.known == fp;
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = fp;
        Ok(ok)
    }
}

/// Joins a remote folder and a site-relative path (`/` separators; `..` and empty parts dropped).
pub fn remote_path(base: &str, rel: &str) -> String {
    let parts = base.split('/').chain(rel.split('/')).filter(|p| !p.is_empty() && *p != "." && *p != "..");
    let joined = parts.collect::<Vec<_>>().join("/");
    if base.starts_with('/') { format!("/{joined}") } else { joined }
}

/// The folders (outermost first) that must exist for `files` under `base`.
pub fn folders(base: &str, files: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: String| {
        if !p.is_empty() && p != "/" && !out.contains(&p) {
            out.push(p);
        }
    };
    let root = remote_path(base, "");
    let mut acc = String::new();
    for part in root.split('/') {
        if part.is_empty() {
            acc.push('/');
            continue;
        }
        acc = if acc.is_empty() || acc.ends_with('/') { format!("{acc}{part}") } else { format!("{acc}/{part}") };
        add(acc.clone());
    }
    for (rel, _) in files {
        let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
        parts.pop();
        let mut sub = String::new();
        for p in parts {
            sub = if sub.is_empty() { p.to_string() } else { format!("{sub}/{p}") };
            add(remote_path(base, &sub));
        }
    }
    out
}

/// Uploads `files` (site-relative path, bytes) to `server`. `progress(done, total)` returning false
/// cancels between files.
pub fn upload(server: &Server, auth: Auth, files: &[(String, Vec<u8>)], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Uploaded, String> {
    if server.host.trim().is_empty() || server.user.trim().is_empty() {
        return Err("the upload server needs a host and a user name".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("cannot start the upload: {e}"))?;
    rt.block_on(run(server, auth, files, progress))
}

async fn run(server: &Server, auth: Auth, files: &[(String, Vec<u8>)], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Uploaded, String> {
    let seen = Arc::new(Mutex::new(String::new()));
    let handler = Handler { known: server.known_fingerprint.trim().to_string(), seen: seen.clone() };
    let config = Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(60)), ..client::Config::default() });
    let port = if server.port == 0 { 22 } else { server.port };
    let host = server.host.trim();
    let fingerprint = || seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let connect = tokio::time::timeout(Duration::from_secs(30), client::connect(config, (host, port), handler)).await;
    let mut session = match connect {
        Err(_) => return Err(format!("connecting to {host}:{port} timed out")),
        Ok(Err(e)) => {
            let fp = fingerprint();
            if !server.known_fingerprint.is_empty() && !fp.is_empty() && fp != server.known_fingerprint.trim() {
                return Err(format!(
                    "the host key of {host} changed (expected {}, got {fp}); refusing to upload. If the server was reinstalled, clear the stored fingerprint.",
                    server.known_fingerprint.trim()
                ));
            }
            return Err(format!("cannot connect to {host}:{port}: {e}"));
        }
        Ok(Ok(s)) => s,
    };
    let user = server.user.trim();
    let ok = match auth {
        Auth::Password(p) => session.authenticate_password(user, p).await.map_err(|e| format!("login failed: {e}"))?.success(),
        Auth::Key { pem, passphrase } => {
            let key = russh::keys::decode_secret_key(&pem, passphrase.as_deref()).map_err(|e| format!("cannot read the private key: {e}"))?;
            let hash = session.best_supported_rsa_hash().await.map_err(|e| format!("login failed: {e}"))?.flatten();
            session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| format!("login failed: {e}"))?
                .success()
        }
    };
    if !ok {
        return Err(format!("{host} rejected the login for {user}"));
    }
    let channel = session.channel_open_session().await.map_err(|e| format!("cannot open an SFTP channel: {e}"))?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| format!("the server offers no SFTP: {e}"))?;
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| format!("cannot start SFTP: {e}"))?;
    for dir in folders(&server.path, files) {
        if !sftp.try_exists(dir.clone()).await.unwrap_or(false) {
            sftp.create_dir(dir.clone()).await.map_err(|e| format!("cannot create {dir}: {e}"))?;
        }
    }
    let total = files.len();
    let mut bytes = 0u64;
    for (i, (rel, data)) in files.iter().enumerate() {
        if !progress(i, total) {
            return Err("upload cancelled".into());
        }
        let path = remote_path(&server.path, rel);
        let mut f = sftp.create(path.clone()).await.map_err(|e| format!("cannot write {path}: {e}"))?;
        f.write_all(data).await.map_err(|e| format!("cannot write {path}: {e}"))?;
        f.shutdown().await.map_err(|e| format!("cannot write {path}: {e}"))?;
        bytes = bytes.saturating_add(data.len() as u64);
    }
    progress(total, total);
    let _ = sftp.close().await;
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    Ok(Uploaded { files: total, bytes, fingerprint: fingerprint() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_stay_under_the_base() {
        assert_eq!(remote_path("/var/www/g", "images/large/0001.jpg"), "/var/www/g/images/large/0001.jpg");
        assert_eq!(remote_path("site", "../../etc/passwd"), "site/etc/passwd");
        assert_eq!(remote_path("", "index.html"), "index.html");
        let files = vec![("index.html".to_string(), vec![]), ("images/large/1.jpg".to_string(), vec![]), ("images/thumb/1.jpg".to_string(), vec![])];
        assert_eq!(folders("/www/g", &files), vec!["/www", "/www/g", "/www/g/images", "/www/g/images/large", "/www/g/images/thumb"]);
        assert_eq!(folders("", &files), vec!["images", "images/large", "images/thumb"]);
    }

    #[test]
    fn missing_host_is_an_error_not_a_hang() {
        let r = upload(&Server::default(), Auth::Password(String::new()), &[], &mut |_, _| true);
        assert!(r.unwrap_err().contains("host"));
    }

    #[test]
    fn unreachable_server_is_an_error() {
        // port 1 on localhost: nothing listens
        let s = Server { host: "127.0.0.1".into(), port: 1, user: "u".into(), ..Server::default() };
        assert!(upload(&s, Auth::Password("x".into()), &[], &mut |_, _| true).is_err());
    }
}
