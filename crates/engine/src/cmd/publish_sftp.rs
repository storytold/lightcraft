//! The SFTP publish service (kind [`dac_publish::KIND_SFTP`]): each published collection is a
//! folder on an SFTP server, each photo a file in it. The server is one of the web module's
//! saved upload servers (`web.saveServer`, its password or key passphrase in the keychain) or an
//! inline `{host, port?, user, path, keyFile?, knownFingerprint?}` without a password (key
//! files only). Upload and delete reuse `dac_webgallery::sftp`.
//!
//! Settings: `{server: saved server name | {host, …}}`. The remote id is the file's path
//! relative to the server's folder (`<collection folder>/<file name>`).

use dac_publish::{CollectionConfig, PublishError, PublishService, Published, ServiceConfig, Upload};
use dac_webgallery::sftp::{self, Auth, Server};
use serde_json::Value;

use crate::Session;
use crate::cmd::immich::publish::Opener;

/// Check an SFTP service's settings (`publish.createService` / `updateService`).
pub fn check_settings(settings: &Value) -> std::result::Result<(), String> {
    match settings.get("server") {
        Some(Value::String(n)) if !n.trim().is_empty() => Ok(()),
        Some(v @ Value::Object(_)) => {
            let s: Server = serde_json::from_value(v.clone()).map_err(|e| format!("invalid `server`: {e}"))?;
            if s.host.trim().is_empty() || s.user.trim().is_empty() {
                return Err("an SFTP server needs `host` and `user`".into());
            }
            Ok(())
        }
        _ => Err("an SFTP service needs `server` (a saved upload server's name, see web.saveServer, or {host, port, user, path, keyFile})".into()),
    }
}

/// Opens the service for a run: the server and its login are resolved now (they need the session).
pub fn opener(s: &mut Session, svc: &ServiceConfig, coll: &CollectionConfig) -> std::result::Result<Option<Opener>, String> {
    if svc.kind != dac_publish::KIND_SFTP {
        return Ok(None);
    }
    const C: &str = "publish.run";
    let server = crate::cmd::web::server_param(s, &svc.settings, C).map_err(|e| e.to_string())?;
    let auth = crate::cmd::web::server_auth(s, &server, None, C).map_err(|e| e.to_string())?;
    let folder = coll.folder.clone();
    let store = crate::cmd::web::store_path_of(s);
    Ok(Some(Box::new(move || Ok(Box::new(SftpPublish { server, auth, folder, store }) as Box<dyn PublishService>))))
}

struct SftpPublish {
    server: Server,
    auth: Auth,
    folder: String,
    /// The web settings file, where a saved server's host key is remembered on first use.
    store: Option<std::path::PathBuf>,
}

impl SftpPublish {
    fn rel(&self, name: &str) -> String {
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if self.folder.trim().is_empty() { name.to_string() } else { format!("{}/{name}", self.folder.trim()) }
    }
}

impl PublishService for SftpPublish {
    fn kind(&self) -> &'static str {
        dac_publish::KIND_SFTP
    }

    fn publish(&mut self, up: &Upload<'_>) -> Result<Published, PublishError> {
        let rel = self.rel(up.file_name);
        let mut files = vec![(rel.clone(), up.bytes.to_vec())];
        for (ext, bytes) in up.sidecars {
            files.push((dac_publish::hard_drive::with_ext(&rel, ext), bytes.clone()));
        }
        let done = sftp::upload(&self.server, self.auth.clone(), &files, &mut |_, _| true).map_err(PublishError::Io)?;
        // trust on first use: later uploads (and runs) refuse a different host key
        if self.server.known_fingerprint.is_empty() && !done.fingerprint.is_empty() {
            self.server.known_fingerprint = done.fingerprint.clone();
            crate::cmd::web::remember_fingerprint(self.store.as_deref(), &self.server.name, &done.fingerprint);
        }
        if let Some(prev) = up.previous.filter(|p| *p != rel) {
            sftp::remove(&self.server, self.auth.clone(), &[prev.to_string()]).map_err(PublishError::Io)?;
        }
        Ok(Published { remote_id: rel })
    }

    fn remove(&mut self, remote_id: &str) -> Result<(), PublishError> {
        // and its XMP sidecar, if one was written
        let mut rels = vec![remote_id.to_string()];
        let xmp = dac_publish::hard_drive::with_ext(remote_id, "xmp");
        if xmp != remote_id {
            rels.push(xmp);
        }
        sftp::remove(&self.server, self.auth.clone(), &rels).map(|_| ()).map_err(PublishError::Io)
    }
}
