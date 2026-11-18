//! IMM-PUBLISH wiring (the service itself is `lcc_immich::publish`).
//!
//! - [`opener`]: for a publish run of an Immich service, reads the account's key and what the
//!   service needs per photo (capture time, original file, existing Immich link) on the session
//!   thread, and hands the publish framework a function that opens the service on its worker.
//!
//! Immich albums meet the catalog as device-sync collections (`immich_collections.rs`); the
//! name-matching album rule (`immich.linkAlbum`, `immich.syncAlbums`) was replaced by them.

use std::collections::HashMap;

use lcc_catalog::{PhotoId, Source};
use lcc_immich::link::SERVICE;
use lcc_immich::publish::{ImmichPublish, PhotoInfo, PublishSettings, SendMode};
use lcc_publish::{CollectionConfig, PublishError, PublishService, ServiceConfig};
use serde_json::Value;

use crate::Session;
use crate::cmd::CommandSpec;

/// Opens a publish service on a worker thread.
pub type Opener = Box<dyn FnOnce() -> std::result::Result<Box<dyn PublishService>, PublishError> + Send>;

/// The device id Immich records for assets the app uploads.
const DEVICE: &str = "desktop-publish";

/// `Some` for an Immich service: its opener, with what it needs per photo of `photos`.
pub fn opener(s: &mut Session, svc: &ServiceConfig, coll: &CollectionConfig, photos: &[PhotoId]) -> std::result::Result<Option<Opener>, String> {
    if svc.kind != lcc_immich::publish::KIND {
        return Ok(None);
    }
    let settings: PublishSettings = serde_json::from_value(svc.settings.clone()).map_err(|e| format!("Immich service settings: {e}"))?;
    let (_, client) = s.immich_client(&settings.account).map_err(|e| e.to_string())?;
    let mut info = HashMap::new();
    for id in photos {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let created = p.captured.clone().unwrap_or_else(|| p.imported.clone());
        // a year or a month alone (`1983-04`) is sent as the start of its period, a time the server reads
        let created = lcc_catalog::dates::capture_start(&created).into_owned();
        let created = if created.len() == 19 { format!("{created}.000Z") } else { created };
        let linked = s.catalog.remote_of(*id).find(|r| r.service == SERVICE && r.account_id == settings.account).map(|r| r.remote_id.clone());
        let original = match &p.source {
            Source::File { path } if settings.send != SendMode::Rendered => Some(std::path::PathBuf::from(path)),
            _ => None,
        };
        // the whole XMP goes with an original: the develop settings and metadata another instance
        // pulls back (IMM-SIDECARS). Immich has no endpoint that updates a managed asset's sidecar
        // after the upload, so this is where a managed original's sidecar content is set.
        let xmp = original.as_ref().map(|_| crate::sidecar::Packets::of(p, &s.catalog).with_gps.into_bytes());
        info.insert(
            *id,
            PhotoInfo { created, favorite: p.flag == lcc_catalog::Flag::Pick, original, original_name: p.file_name.clone(), linked, xmp },
        );
    }
    let name = album_name(&s.catalog, svc, coll);
    let state = s.immich_dir().map(|d| d.join("publish").join(state_name(&svc.id, coll)));
    Ok(Some(Box::new(move || {
        let mut p = ImmichPublish::new(client, settings, &name, DEVICE, info);
        if let Some(st) = state {
            p = p.with_album_state(st);
        }
        Ok(Box::new(p) as Box<dyn PublishService>)
    })))
}

/// The file that remembers a collection's Immich album id.
fn state_name(service: &str, coll: &CollectionConfig) -> String {
    let svc: String = service.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(64).collect();
    format!("{svc}-{}.album", coll.album.0)
}

/// The Immich album name of a published collection: its current name, prefixed by the collection
/// sets it sits in below the service's own set (`Set / Sub / Collection`; Immich albums are flat).
pub fn album_name(catalog: &lcc_catalog::Catalog, svc: &ServiceConfig, coll: &CollectionConfig) -> String {
    let Some(al) = catalog.album(coll.album) else { return coll.folder.clone() };
    let own = if al.name.trim().is_empty() { coll.folder.clone() } else { al.name.trim().to_string() };
    let mut parts = vec![own];
    let mut next = al.parent;
    // bounded: a damaged catalog can't loop
    for _ in 0..32 {
        let Some(id) = next.filter(|id| Some(*id) != svc.set) else { break };
        let Some(set) = catalog.album(id) else { break };
        if !set.name.trim().is_empty() {
            parts.push(set.name.trim().to_string());
        }
        next = set.parent;
    }
    parts.reverse();
    parts.join(" / ")
}

/// Links each original a run uploaded (or found) to its catalog photo on the service's account, so
/// sync works without a separate `immich.link`. Photos already linked on that account are left
/// alone. `published`: (photo, remote id `r:<render>+o:<original>`).
pub fn original_link_ops(s: &Session, svc: &ServiceConfig, published: &[(PhotoId, String)]) -> Vec<lcc_catalog::Op> {
    if svc.kind != lcc_immich::publish::KIND {
        return Vec::new();
    }
    let Ok(settings) = serde_json::from_value::<PublishSettings>(svc.settings.clone()) else { return Vec::new() };
    let now = lcc_catalog::rules::now();
    let mut ops = Vec::new();
    for (photo, remote) in published {
        let Some(original) = lcc_immich::publish::parse_remote(remote).1 else { continue };
        if s.catalog.photo(*photo).is_none() || s.catalog.remote_of(*photo).any(|r| r.service == SERVICE && r.account_id == settings.account) {
            continue;
        }
        ops.push(lcc_catalog::Catalog::link_remote_op(lcc_catalog::RemoteIdentity {
            photo_id: *photo,
            service: SERVICE.to_string(),
            account_id: settings.account.clone(),
            remote_id: original.to_string(),
            remote_checksum: None,
            remote_updated_at: None,
            last_synced_at: Some(now.clone()),
            sync_state: lcc_catalog::SyncState::Synced,
        }));
    }
    ops
}

/// Check an Immich service's settings (`publish.createService` / `updateService`).
pub fn check_settings(settings: &Value) -> std::result::Result<(), String> {
    let st: PublishSettings = serde_json::from_value(settings.clone()).map_err(|e| format!("Immich service settings: {e}"))?;
    if st.account.trim().is_empty() {
        return Err("an Immich service needs `account` (a connected account, immich.status)".into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    Vec::new()
}
