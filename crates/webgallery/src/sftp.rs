//! SFTP upload of a generated site (pure Rust: russh + russh-sftp, ring crypto backend).
//!
//! Host keys are pinned: the server's key is checked against `known_fingerprint` during the key
//! exchange, before any credential is sent. With no pinned key the connection is refused (the
//! error names the fingerprint, see [`host_key`]) so the user can confirm it and pin it; a
//! different key refuses the connection too (the error names both fingerprints).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

pub use crate::settings::Server;

/// How to authenticate.
#[derive(Clone)]
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
        // nothing pinned: refuse, never authenticate to a key the user has not confirmed
        let ok = !self.known.is_empty() && self.known == fp;
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

/// Connects to `server` without logging in and returns its host key's SHA-256 fingerprint, for the
/// user to confirm before it is pinned. Sends no credentials.
pub fn host_key(server: &Server) -> Result<String, String> {
    if server.host.trim().is_empty() {
        return Err("the upload server needs a host".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("cannot start the connection: {e}"))?;
    rt.block_on(async {
        let seen = Arc::new(Mutex::new(String::new()));
        // a pin no fingerprint matches: the key is recorded, then the handshake is refused
        let handler = Handler { known: "-".into(), seen: seen.clone() };
        let config = Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(60)), ..client::Config::default() });
        let port = if server.port == 0 { 22 } else { server.port };
        let host = server.host.trim();
        match tokio::time::timeout(Duration::from_secs(30), client::connect(config, (host, port), handler)).await {
            Err(_) => return Err(format!("connecting to {host}:{port} timed out")),
            Ok(Ok(s)) => {
                let _ = s.disconnect(russh::Disconnect::ByApplication, "", "en").await;
            }
            Ok(Err(_)) => {}
        }
        let fp = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if fp.is_empty() { Err(format!("cannot connect to {host}:{port}")) } else { Ok(fp) }
    })
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

/// Logs in to `server` and leaves without touching any file, so a caller about to spend minutes
/// rendering what it will upload learns first that the server is unreachable, its host key isn't
/// confirmed, the login is rejected or it has no SFTP. The errors are the upload's own.
pub fn check(server: &Server, auth: Auth) -> Result<(), String> {
    if server.host.trim().is_empty() || server.user.trim().is_empty() {
        return Err("the upload server needs a host and a user name".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("cannot start the upload: {e}"))?;
    rt.block_on(async {
        connect(server, auth).await?.close().await;
        Ok(())
    })
}

/// A logged-in SFTP session.
struct Conn {
    session: client::Handle<Handler>,
    sftp: SftpSession,
    fingerprint: String,
}

impl Conn {
    async fn close(self) {
        let _ = self.sftp.close().await;
        let _ = self.session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    }
}

async fn connect(server: &Server, auth: Auth) -> Result<Conn, String> {
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
            if server.known_fingerprint.trim().is_empty() && !fp.is_empty() {
                return Err(format!(
                    "the host key of {host} is not trusted yet; its fingerprint is {fp}. Check it with the server's administrator, then confirm it to upload."
                ));
            }
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
    Ok(Conn { session, sftp, fingerprint: fingerprint() })
}

async fn run(server: &Server, auth: Auth, files: &[(String, Vec<u8>)], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Uploaded, String> {
    let conn = connect(server, auth).await?;
    let sftp = &conn.sftp;
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
    let fingerprint = conn.fingerprint.clone();
    conn.close().await;
    Ok(Uploaded { files: total, bytes, fingerprint })
}

/// Deletes the files at `rels` (relative to the server's folder); one already gone is not an
/// error. Returns the server's host key fingerprint.
pub fn remove(server: &Server, auth: Auth, rels: &[String]) -> Result<String, String> {
    if server.host.trim().is_empty() || server.user.trim().is_empty() {
        return Err("the upload server needs a host and a user name".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("cannot start the upload: {e}"))?;
    rt.block_on(async {
        let conn = connect(server, auth).await?;
        for rel in rels {
            let path = remote_path(&server.path, rel);
            if conn.sftp.try_exists(path.clone()).await.unwrap_or(false) {
                conn.sftp.remove_file(path.clone()).await.map_err(|e| format!("cannot delete {path}: {e}"))?;
            }
        }
        let fp = conn.fingerprint.clone();
        conn.close().await;
        Ok(fp)
    })
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
    fn a_missing_or_unreachable_host_is_an_error_not_a_hang() {
        // missing host is an error not a hang
        {
            let r = upload(&Server::default(), Auth::Password(String::new()), &[], &mut |_, _| true);
            assert!(r.unwrap_err().contains("host"));
        }
        // unreachable server is an error
        {
            // port 1 on localhost: nothing listens
            let s = Server { host: "127.0.0.1".into(), port: 1, user: "u".into(), ..Server::default() };
            assert!(upload(&s, Auth::Password("x".into()), &[], &mut |_, _| true).is_err());
        }
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An SSH server on localhost that counts login attempts (and rejects them all).
    fn serve(logins: Arc<AtomicUsize>) -> (u16, String) {
        struct Ssh(Arc<AtomicUsize>);
        impl russh::server::Handler for Ssh {
            type Error = russh::Error;
            async fn auth_password(&mut self, _: &str, _: &str) -> Result<russh::server::Auth, Self::Error> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(russh::server::Auth::reject())
            }
        }
        let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[9u8; 32]));
        let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let config = Arc::new(russh::server::Config {
                    keys: vec![key],
                    auth_rejection_time: Duration::from_millis(1),
                    auth_rejection_time_initial: Some(Duration::ZERO),
                    ..Default::default()
                });
                loop {
                    let Ok((sock, _)) = listener.accept().await else { return };
                    let (config, logins) = (config.clone(), logins.clone());
                    tokio::spawn(async move {
                        if let Ok(s) = russh::server::run_stream(config, sock, Ssh(logins)).await {
                            let _ = s.await;
                        }
                    });
                }
            });
        });
        (port, fp)
    }

    fn server(port: u16, pin: &str) -> Server {
        Server { host: "127.0.0.1".into(), port, user: "u".into(), known_fingerprint: pin.into(), ..Server::default() }
    }

    #[test]
    fn an_unpinned_or_changed_host_key_is_refused_before_any_login() {
        // unpinned host key is refused before any login
        {
            let logins = Arc::new(AtomicUsize::new(0));
            let (port, fp) = serve(logins.clone());
            let e = upload(&server(port, ""), Auth::Password("secret".into()), &[], &mut |_, _| true).unwrap_err();
            assert!(e.contains(&fp) && e.contains("not trusted"), "{e}");
            assert!(remove(&server(port, ""), Auth::Password("secret".into()), &["a".into()]).is_err());
            assert_eq!(logins.load(Ordering::SeqCst), 0, "credentials sent to an unconfirmed host key");
        }
        // changed host key is refused before any login
        {
            let logins = Arc::new(AtomicUsize::new(0));
            let (port, fp) = serve(logins.clone());
            let pin = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
            let e = upload(&server(port, pin), Auth::Password("secret".into()), &[], &mut |_, _| true).unwrap_err();
            assert!(e.contains("changed") && e.contains(&fp) && e.contains(pin), "{e}");
            assert_eq!(logins.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn check_fails_as_the_upload_would() {
        let auth = || Auth::Password("secret".into());
        assert!(check(&Server::default(), auth()).unwrap_err().contains("host"));
        let logins = Arc::new(AtomicUsize::new(0));
        let (port, fp) = serve(logins.clone());
        let e = check(&server(port, ""), auth()).unwrap_err();
        assert!(e.contains(&fp) && e.contains("not trusted"), "{e}");
        let e = check(&server(port, "SHA256:AAAA"), auth()).unwrap_err();
        assert!(e.contains("changed") && e.contains(&fp), "{e}");
        assert_eq!(logins.load(Ordering::SeqCst), 0, "credentials sent to an unconfirmed host key");
        let e = check(&server(port, &fp), auth()).unwrap_err();
        assert!(e.contains("rejected the login") || e.contains("login failed"), "{e}");
        assert_eq!(logins.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn host_key_shows_the_fingerprint_without_logging_in_and_a_confirmed_pin_logs_in() {
        let logins = Arc::new(AtomicUsize::new(0));
        let (port, fp) = serve(logins.clone());
        assert_eq!(host_key(&server(port, "")).unwrap(), fp);
        assert_eq!(logins.load(Ordering::SeqCst), 0);
        // pinned: the login goes ahead (and this server rejects it)
        let e = upload(&server(port, &fp), Auth::Password("secret".into()), &[], &mut |_, _| true).unwrap_err();
        assert!(e.contains("rejected the login") || e.contains("login failed"), "{e}");
        assert_eq!(logins.load(Ordering::SeqCst), 1);
    }
}
