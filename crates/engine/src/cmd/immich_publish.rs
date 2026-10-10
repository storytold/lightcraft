//! IMM-PUBLISH wiring (the service itself is `dac_immich::publish`) and the collections ↔ albums
//! part of IMM-SYNC.
//!
//! - [`opener`]: for a publish run of an Immich service, reads the account's key and what the
//!   service needs per photo (capture time, original file, existing Immich link) on the session
//!   thread, and hands the publish framework a function that opens the service on its worker.
//! - `immich.linkAlbum` / `immich.syncAlbums`: a catalog album linked to an Immich album keeps the
//!   same linked photos on both sides (a set three-way merge against the last sync, like keywords).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dac_catalog::{AlbumId, PhotoId, Source};
use dac_immich::link::SERVICE;
use dac_immich::publish::{ImmichPublish, PhotoInfo, PublishSettings, SendMode};
use dac_publish::{CollectionConfig, PublishError, PublishService, ServiceConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{account_param, failure};
use crate::cmd::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

/// Opens a publish service on a worker thread.
pub type Opener = Box<dyn FnOnce() -> std::result::Result<Box<dyn PublishService>, PublishError> + Send>;

/// The device id Immich records for assets the app uploads.
const DEVICE: &str = "desktop-publish";

/// `Some` for an Immich service: its opener, with what it needs per photo of `photos`.
pub fn opener(s: &mut Session, svc: &ServiceConfig, coll: &CollectionConfig, photos: &[PhotoId]) -> std::result::Result<Option<Opener>, String> {
    if svc.kind != dac_immich::publish::KIND {
        return Ok(None);
    }
    let settings: PublishSettings = serde_json::from_value(svc.settings.clone()).map_err(|e| format!("Immich service settings: {e}"))?;
    let (_, client) = s.immich_client(&settings.account).map_err(|e| e.to_string())?;
    let mut info = HashMap::new();
    for id in photos {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let created = p.captured.clone().unwrap_or_else(|| p.imported.clone());
        let created = if created.len() == 19 { format!("{created}.000Z") } else { created };
        let linked = s.catalog.remote_of(*id).find(|r| r.service == SERVICE && r.account_id == settings.account).map(|r| r.remote_id.clone());
        let original = match &p.source {
            Source::File { path } if settings.send != SendMode::Rendered => Some(std::path::PathBuf::from(path)),
            _ => None,
        };
        info.insert(
            *id,
            PhotoInfo { created, favorite: p.flag == dac_catalog::Flag::Pick, original, original_name: p.file_name.clone(), linked, xmp: None },
        );
    }
    let name = s.catalog.album(coll.album).map(|a| a.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| coll.folder.clone());
    Ok(Some(Box::new(move || Ok(Box::new(ImmichPublish::new(client, settings, &name, DEVICE, info)) as Box<dyn PublishService>))))
}

/// Links each original a run uploaded (or found) to its catalog photo on the service's account, so
/// sync works without a separate `immich.link`. Photos already linked on that account are left
/// alone. `published`: (photo, remote id `r:<render>+o:<original>`).
pub fn original_link_ops(s: &Session, svc: &ServiceConfig, published: &[(PhotoId, String)]) -> Vec<dac_catalog::Op> {
    if svc.kind != dac_immich::publish::KIND {
        return Vec::new();
    }
    let Ok(settings) = serde_json::from_value::<PublishSettings>(svc.settings.clone()) else { return Vec::new() };
    let now = dac_catalog::rules::now();
    let mut ops = Vec::new();
    for (photo, remote) in published {
        let Some(original) = dac_immich::publish::parse_remote(remote).1 else { continue };
        if s.catalog.photo(*photo).is_none() || s.catalog.remote_of(*photo).any(|r| r.service == SERVICE && r.account_id == settings.account) {
            continue;
        }
        ops.push(dac_catalog::Catalog::link_remote_op(dac_catalog::RemoteIdentity {
            photo_id: *photo,
            service: SERVICE.to_string(),
            account_id: settings.account.clone(),
            remote_id: original.to_string(),
            remote_checksum: None,
            remote_updated_at: None,
            last_synced_at: Some(now.clone()),
            sync_state: dac_catalog::SyncState::Synced,
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

// ---- collections ↔ albums

/// A catalog album linked to an Immich album.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AlbumLink {
    pub immich: String,
    /// Asset ids both sides had after the last sync.
    pub base: Option<BTreeSet<String>>,
}

type AlbumLinks = BTreeMap<u64, AlbumLink>;

fn links_path(s: &Session, account: &str) -> Option<std::path::PathBuf> {
    let name: String = account.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).take(150).collect();
    s.immich_dir().map(|d| d.join("sync").join(format!("{name}.albums.json")))
}

fn load_links(s: &Session, account: &str) -> AlbumLinks {
    links_path(s, account).and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_links(s: &Session, account: &str, l: &AlbumLinks) -> std::result::Result<(), String> {
    let Some(p) = links_path(s, account) else { return Ok(()) };
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(l).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

/// The set three-way merge: kept when both have it, or one side added it since `base`.
pub fn merge_sets(base: Option<&BTreeSet<String>>, l: &BTreeSet<String>, r: &BTreeSet<String>) -> BTreeSet<String> {
    match base {
        None => l.union(r).cloned().collect(),
        Some(b) => l.union(r).filter(|k| (l.contains(*k) && r.contains(*k)) || !b.contains(*k)).cloned().collect(),
    }
}

fn link_album(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.linkAlbum";
    let id = account_param(s, p, C)?;
    let album = p.get("album").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `album` (catalog album id)"))?;
    let al = s.catalog.album(AlbumId(album)).ok_or_else(|| bad(C, "no such album"))?;
    if al.folder || al.smart.is_some() {
        return Err(bad(C, "only plain albums hold photos"));
    }
    let name = al.name.clone();
    let mut links = load_links(s, &id);
    if bool_or(p, "unlink", false) {
        links.remove(&album);
        save_links(s, &id, &links).map_err(|e| bad(C, e))?;
        return Ok(json!({"unlinked": album}));
    }
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let target = match str_param(p, "immichAlbum") {
        Some(x) => x.to_string(),
        None => {
            let found = match client.albums() {
                Ok(a) => a.into_iter().find(|a| a.album_name == name).map(|a| a.id),
                Err(e) => return Ok(failure(&e)),
            };
            match found {
                Some(x) => x,
                None => match client.create_album(&name) {
                    Ok(a) => a.id,
                    Err(e) => return Ok(failure(&e)),
                },
            }
        }
    };
    links.insert(album, AlbumLink { immich: target.clone(), base: None });
    save_links(s, &id, &links).map_err(|e| bad(C, e))?;
    Ok(json!({"ok": true, "album": album, "immichAlbum": target}))
}

fn sync_albums(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.syncAlbums";
    let id = account_param(s, p, C)?;
    let dry = bool_or(p, "dryRun", false);
    let mut links = load_links(s, &id);
    if links.is_empty() {
        return Ok(json!({"ok": true, "albums": []}));
    }
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    // asset id ↔ photo for this account's confirmed links
    let by_asset: HashMap<String, PhotoId> = s
        .catalog
        .remote_links()
        .iter()
        .filter(|r| r.service == SERVICE && r.account_id == id && r.sync_state != dac_catalog::SyncState::Probable)
        .map(|r| (r.remote_id.clone(), r.photo_id))
        .collect();
    let by_photo: HashMap<PhotoId, String> = by_asset.iter().map(|(a, p)| (*p, a.clone())).collect();
    let mut out = Vec::new();
    for (album, link) in links.iter_mut() {
        let Some(al) = s.catalog.album(AlbumId(*album)) else { continue };
        let local: BTreeSet<String> = al.photos.iter().filter_map(|p| by_photo.get(p).cloned()).collect();
        let remote: BTreeSet<String> = match client.album_assets(&link.immich) {
            Ok(v) => v.into_iter().filter(|a| by_asset.contains_key(a)).collect(),
            Err(e) => return Ok(failure(&e)),
        };
        let set = merge_sets(link.base.as_ref(), &local, &remote);
        let to_add_remote: Vec<String> = set.difference(&remote).cloned().collect();
        let to_remove_remote: Vec<String> = remote.difference(&set).cloned().collect();
        let to_add_local: Vec<u64> = set.difference(&local).filter_map(|a| by_asset.get(a)).map(|p| p.0).collect();
        let to_remove_local: Vec<u64> = local.difference(&set).filter_map(|a| by_asset.get(a)).map(|p| p.0).collect();
        out.push(json!({"album": album, "immichAlbum": link.immich, "addToImmich": to_add_remote.len(), "removeFromImmich": to_remove_remote.len(), "addToCatalog": to_add_local, "removeFromCatalog": to_remove_local}));
        if dry {
            continue;
        }
        let r = client.album_add(&link.immich, &to_add_remote).and_then(|_| client.album_remove(&link.immich, &to_remove_remote));
        if let Err(e) = r {
            return Ok(failure(&e));
        }
        if !to_add_local.is_empty() {
            s.execute("album.addPhotos", &json!({"id": album, "ids": to_add_local}))?;
        }
        if !to_remove_local.is_empty() {
            s.execute("album.removePhotos", &json!({"id": album, "ids": to_remove_local}))?;
        }
        link.base = Some(set);
    }
    if !dry {
        save_links(s, &id, &links).map_err(|e| bad(C, e))?;
    }
    Ok(json!({"ok": true, "albums": out}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "immich.linkAlbum", "Link Album with Immich", [], None, "{account?, album: catalog album id, immichAlbum?: id (default: the Immich album of the same name, created if missing), unlink?: bool} — the album's linked photos sync both ways with the Immich album (immich.syncAlbums) → {ok, album, immichAlbum}", always, link_album),
        cmd!(query "immich.syncAlbums", "Sync Albums with Immich", ["Library", "Immich"], None, "{account?, dryRun?: bool} — linked albums keep the same linked photos on both sides (added on one side since the last sync: added on the other; removed: removed) → {ok, albums: [{album, immichAlbum, addToImmich, removeFromImmich, addToCatalog, removeFromCatalog}]}", always, sync_albums),
    ]
}
