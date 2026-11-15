//! IMM-PUBLISH wiring (the service itself is `lcc_immich::publish`) and the collections ↔ albums
//! part of IMM-SYNC.
//!
//! - [`opener`]: for a publish run of an Immich service, reads the account's key and what the
//!   service needs per photo (capture time, original file, existing Immich link) on the session
//!   thread, and hands the publish framework a function that opens the service on its worker.
//! - `immich.linkAlbum` / `immich.syncAlbums`: a catalog album linked to an Immich album keeps the
//!   same linked photos on both sides (a set three-way merge against the last sync, like keywords).

use std::collections::{BTreeSet, HashMap};

use lcc_catalog::{AlbumId, PhotoId, Source};
use lcc_immich::link::SERVICE;
use lcc_immich::publish::{ImmichPublish, PhotoInfo, PublishSettings, SendMode};
use lcc_publish::{CollectionConfig, PublishError, PublishService, ServiceConfig};
use serde::Serialize;
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

// ---- collections ↔ albums

use lcc_immich::albums::{self as plan_albums, LocalAlbum, LocalOp, PendingDelete, RemoteAlbum, RemoteOp};
pub use lcc_immich::albums::{AlbumLink, AlbumLinks};
use lcc_immich::sync::{Direction, FieldRule};

fn sync_file(s: &Session, account: &str, ext: &str) -> Option<std::path::PathBuf> {
    let name: String = account.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).take(150).collect();
    s.immich_dir().map(|d| d.join("sync").join(format!("{name}.{ext}")))
}

fn load_json<T: serde::de::DeserializeOwned + Default>(path: Option<std::path::PathBuf>) -> T {
    path.and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_json<T: Serialize + ?Sized>(path: Option<std::path::PathBuf>, v: &T) -> std::result::Result<(), String> {
    let Some(p) = path else { return Ok(()) };
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    // temp file + rename; a failed write (full disk) removes the temp file
    let body = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    lcc_catalog::safe_file::write_atomic(&p, &body).map_err(|e| format!("{}: {e}", p.display()))
}

fn load_links(s: &Session, account: &str) -> AlbumLinks {
    load_json(sync_file(s, account, "albums.json"))
}

fn save_links(s: &Session, account: &str, l: &AlbumLinks) -> std::result::Result<(), String> {
    save_json(sync_file(s, account, "albums.json"), l)
}

pub(crate) fn load_pending(s: &Session, account: &str) -> Vec<PendingDelete> {
    load_json(sync_file(s, account, "album-deletes.json"))
}

fn save_pending(s: &Session, account: &str, p: &[PendingDelete]) -> std::result::Result<(), String> {
    save_json(sync_file(s, account, "album-deletes.json"), p)
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
    if let Some(x) = str_param(p, "immichAlbum")
        && x.trim().is_empty()
    {
        return Err(bad(C, "`immichAlbum` is empty (omit it to use the Immich album of the same name)"));
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
    links.insert(album, AlbumLink { immich: target.clone(), base: None, name: None });
    save_links(s, &id, &links).map_err(|e| bad(C, e))?;
    Ok(json!({"ok": true, "album": album, "immichAlbum": target}))
}

/// Catalog albums that never map to Immich albums: folders, smart albums, the Quick Collection,
/// and published collections (and everything below a publish service's set: publish owns those).
fn unsyncable(s: &Session) -> BTreeSet<u64> {
    let mut roots = BTreeSet::new();
    if let Some(dir) = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
        && let Ok(cfg) = lcc_publish::PublishConfig::load(&dir)
    {
        for svc in &cfg.services {
            roots.extend(svc.set.map(|x| x.0));
            roots.extend(svc.collections.iter().map(|c| c.album.0));
        }
    }
    let mut out = BTreeSet::new();
    for al in s.catalog.albums() {
        let mut skip = al.folder || al.smart.is_some() || al.quick || roots.contains(&al.id.0);
        let mut next = al.parent;
        // bounded: a damaged catalog can't loop
        for _ in 0..32 {
            let Some(p) = next.filter(|_| !skip) else { break };
            skip = roots.contains(&p.0);
            next = s.catalog.album(p).and_then(|a| a.parent);
        }
        if skip {
            out.insert(al.id.0);
        }
    }
    out
}

/// What an album sync worker gets.
struct AlbumJob {
    account: String,
    dry_run: bool,
    rule: FieldRule,
    local: Vec<LocalAlbum>,
    links: AlbumLinks,
    pending: Vec<PendingDelete>,
    /// The account's linked assets (only these count as album members).
    linked: BTreeSet<String>,
    /// The links file of the library the run was started in.
    home: Option<std::path::PathBuf>,
}

/// What it brings back.
pub(crate) struct AlbumsDone {
    account: String,
    dry_run: bool,
    old: AlbumLinks,
    plan: plan_albums::Plan,
    /// Immich albums the run created: catalog album → new Immich album id, and the assets known
    /// to be in it (those the server took; none when the add after the create failed).
    created: Vec<(u64, String, BTreeSet<String>)>,
    errors: Vec<String>,
    fatal: Option<lcc_immich::ImmichError>,
    home: Option<std::path::PathBuf>,
}

/// Start an album run: the catalog side here, the server on a worker (`background`) or inline.
pub(crate) fn start_albums(s: &mut Session, account: &str, dry_run: bool, background: bool) -> Result<Value> {
    if s.remote.sync.get(account).is_some_and(|a| a.albums_running) {
        return Ok(json!({"started": false, "reason": "an album sync of this account is already running"}));
    }
    let rule = s.immich_accounts().ok().and_then(|a| a.get(account).map(|x| x.sync.album_rule())).unwrap_or_default();
    let links = load_links(s, account);
    if rule.direction == Direction::Off && links.is_empty() {
        return Ok(json!({"ok": true, "albums": 0, "changes": {"toImmich": [], "toCatalog": []}}));
    }
    let (_, client) = match s.immich_client(account) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let by_photo: HashMap<PhotoId, String> = s
        .catalog
        .remote_links()
        .iter()
        .filter(|r| r.service == SERVICE && r.account_id == account && r.sync_state != lcc_catalog::SyncState::Probable)
        .map(|r| (r.photo_id, r.remote_id.clone()))
        .collect();
    let skip = unsyncable(s);
    let local: Vec<LocalAlbum> = s
        .catalog
        .albums()
        .filter(|a| !skip.contains(&a.id.0))
        .filter(|a| rule.direction != Direction::Off || links.contains_key(&a.id.0))
        .map(|a| LocalAlbum {
            id: a.id.0,
            name: a.name.clone(),
            assets: a.photos.iter().filter_map(|p| by_photo.get(p).cloned()).collect(),
            unlinked: a.photos.iter().filter(|p| !by_photo.contains_key(p)).count(),
        })
        .collect();
    let job = AlbumJob {
        account: account.to_string(),
        dry_run,
        rule,
        local,
        links,
        pending: load_pending(s, account),
        linked: by_photo.into_values().collect(),
        home: sync_file(s, account, "albums.json"),
    };
    s.remote.sync.entry(account.to_string()).or_default().albums_running = true;
    let task = s.activity.start("immich", "Syncing albums with Immich", crate::activity::Cancel::No);
    let run = move || {
        let d = album_work(&client, job);
        drop(task);
        d
    };
    if background {
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let _ = tx.send(crate::remote::Msg::AlbumsSynced(Box::new(run())));
        });
        return Ok(json!({"started": true}));
    }
    let done = run();
    Ok(finish_albums(s, done))
}

/// The server side of an album run.
fn album_work(c: &lcc_immich::Client, job: AlbumJob) -> AlbumsDone {
    let AlbumJob { account, dry_run, rule, local, links, pending, linked, home } = job;
    let mut done =
        AlbumsDone { account, dry_run, old: links.clone(), plan: Default::default(), created: Vec::new(), errors: Vec::new(), fatal: None, home };
    let list = match c.albums() {
        Ok(v) => v,
        Err(e) => {
            done.fatal = Some(e);
            return done;
        }
    };
    let wanted: BTreeSet<&str> = links.values().map(|l| l.immich.as_str()).collect();
    let mut remote = Vec::new();
    for a in list {
        if rule.direction == Direction::Off && !wanted.contains(a.id.as_str()) {
            continue;
        }
        match c.album_assets(&a.id) {
            Ok(v) => remote.push(RemoteAlbum { assets: v.into_iter().filter(|x| linked.contains(x)).collect(), id: a.id, name: a.album_name }),
            Err(e) => {
                done.fatal = Some(e);
                return done;
            }
        }
    }
    let mut plan = plan_albums::plan(rule, &local, &remote, &links, &pending);
    if !dry_run {
        // what the Immich side did not do, per Immich album: the links' bases are corrected by
        // it below, so a base never holds an asset that is not in the Immich album
        let mut failed: BTreeSet<String> = BTreeSet::new();
        let mut not_added: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut not_removed: HashMap<String, BTreeSet<String>> = HashMap::new();
        // the assets of `asked` a checked add/remove left undone (all of them when the call failed)
        let undone = |r: std::result::Result<Vec<(String, String)>, lcc_immich::ImmichError>, asked: &[String], errors: &mut Vec<String>| match r {
            Ok(refused) => {
                if let Some((_, why)) = refused.first() {
                    errors.push(format!("Immich album: {} of {} photos were refused ({why})", refused.len(), asked.len()));
                }
                refused.into_iter().map(|(id, _)| id).collect::<BTreeSet<String>>()
            }
            Err(e) => {
                errors.push(format!("Immich album: {e}"));
                asked.iter().cloned().collect()
            }
        };
        for op in &plan.remote {
            let r = match op {
                RemoteOp::Create { local, name, assets } => c.create_album(name).map(|a| {
                    let ids: Vec<String> = assets.iter().cloned().collect();
                    let missing = if ids.is_empty() { BTreeSet::new() } else { undone(c.album_add_checked(&a.id, &ids), &ids, &mut done.errors) };
                    done.created.push((*local, a.id, assets.difference(&missing).cloned().collect()));
                }),
                RemoteOp::Rename { id, name } => c.rename_album(id, name),
                RemoteOp::Add { id, assets } => {
                    let missing = undone(c.album_add_checked(id, assets), assets, &mut done.errors);
                    if !missing.is_empty() {
                        not_added.entry(id.clone()).or_default().extend(missing);
                    }
                    Ok(())
                }
                RemoteOp::Remove { id, assets } => {
                    let left = undone(c.album_remove_checked(id, assets), assets, &mut done.errors);
                    if !left.is_empty() {
                        not_removed.entry(id.clone()).or_default().extend(left);
                    }
                    Ok(())
                }
                RemoteOp::Delete { id } => c.delete_album(id),
            };
            if let Err(e) = r {
                done.errors.push(format!("Immich album: {e}"));
                if let RemoteOp::Rename { id, .. } | RemoteOp::Delete { id } = op {
                    failed.insert(id.clone());
                }
            }
        }
        for link in plan.links.values_mut() {
            if let Some(a) = not_added.get(&link.immich) {
                link.not_added(a);
            }
            if let Some(a) = not_removed.get(&link.immich) {
                link.not_removed(a);
            }
        }
        // a failed rename or delete keeps the old link, so the next run tries again
        for (k, old) in &links {
            if failed.contains(&old.immich) {
                plan.links.insert(*k, old.clone());
            }
        }
    }
    done.plan = plan;
    done
}

/// Take an album run in: catalog changes, the links, the summary.
pub(crate) fn finish_albums(s: &mut Session, d: AlbumsDone) -> Value {
    let AlbumsDone { account, dry_run, old, mut plan, created, mut errors, fatal, home } = d;
    let v = if sync_file(s, &account, "albums.json") != home {
        // another library was opened while the worker was out: its album ids and links are not
        // this library's; nothing is applied or saved
        json!({"ok": false, "dryRun": dry_run, "albums": 0, "errors": ["the library changed while the album sync was running; nothing was applied"]})
    } else if let Some(e) = fatal {
        let mut v = failure(&e);
        v["albums"] = json!(0);
        v
    } else {
        let by_asset: HashMap<String, PhotoId> = s
            .catalog
            .remote_links()
            .iter()
            .filter(|r| r.service == SERVICE && r.account_id == account && r.sync_state != lcc_catalog::SyncState::Probable)
            .map(|r| (r.remote_id.clone(), r.photo_id))
            .collect();
        let photos = |a: &[String]| -> Vec<u64> { a.iter().filter_map(|x| by_asset.get(x)).map(|p| p.0).collect() };
        let changes = json!({"toImmich": plan.remote, "toCatalog": plan.local});
        if !dry_run {
            for (local, immich, confirmed) in &created {
                let name = plan
                    .remote
                    .iter()
                    .find_map(|o| match o {
                        RemoteOp::Create { local: l, name, .. } if l == local => Some(name.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                // the base: only what is known to be in the new Immich album
                plan.links.insert(*local, AlbumLink { immich: immich.clone(), base: Some(confirmed.clone()), name: Some(name) });
            }
            for op in plan.local.clone() {
                // assets of an op that have no photo here (the link went away during the run)
                let unmapped = |a: &[String]| -> Vec<String> { a.iter().filter(|x| !by_asset.contains_key(*x)).cloned().collect() };
                let r = match &op {
                    LocalOp::Create { immich, name, assets } => {
                        let ids: Vec<String> = assets.iter().filter(|x| by_asset.contains_key(*x)).cloned().collect();
                        let ph = photos(&ids);
                        s.execute("album.create", &json!({"name": name})).and_then(|a| {
                            let id = a["id"].as_u64().unwrap_or_default();
                            if !ph.is_empty() {
                                s.execute("album.addPhotos", &json!({"id": id, "ids": ph}))?;
                            }
                            // the base: only the assets whose photos are in the new album
                            plan.links
                                .insert(id, AlbumLink { immich: immich.clone(), base: Some(ids.into_iter().collect()), name: Some(name.clone()) });
                            Ok(Value::Null)
                        })
                    }
                    LocalOp::Rename { id, name } => s.execute("album.rename", &json!({"id": id, "name": name})),
                    LocalOp::Add { id, assets } => {
                        let r = s.execute("album.addPhotos", &json!({"id": id, "ids": photos(assets)}));
                        if let Some(l) = plan.links.get_mut(id) {
                            // not in the catalog album: not common to both sides
                            match &r {
                                Ok(_) => l.not_added(&unmapped(assets)),
                                Err(_) => l.not_added(assets),
                            }
                        }
                        r
                    }
                    LocalOp::Remove { id, assets } => {
                        let r = s.execute("album.removePhotos", &json!({"id": id, "ids": photos(assets)}));
                        if r.is_err()
                            && let Some(l) = plan.links.get_mut(id)
                        {
                            // still in the catalog album and (as far as the base knows) once in
                            // both: the next run removes them again
                            l.not_removed(assets);
                        }
                        r
                    }
                    LocalOp::Delete { id } => s.execute("album.delete", &json!({"id": id})),
                };
                if let Err(e) = r {
                    errors.push(format!("catalog album: {e}"));
                    // a failed rename or delete keeps the old link, so the next run tries again
                    if let LocalOp::Rename { id, .. } | LocalOp::Delete { id } = &op
                        && let Some(o) = old.get(id)
                    {
                        plan.links.insert(*id, o.clone());
                    }
                }
            }
            if let Err(e) = save_links(s, &account, &plan.links).and_then(|_| save_pending(s, &account, &plan.pending)) {
                errors.push(e);
            }
        }
        let empty = Vec::new();
        let (to_i, to_c) = (changes["toImmich"].as_array().unwrap_or(&empty), changes["toCatalog"].as_array().unwrap_or(&empty));
        let count = |k: &str| to_i.iter().chain(to_c).filter(|o| o["op"] == k).count();
        json!({
            "ok": errors.is_empty(),
            "dryRun": dry_run,
            "created": count("create"),
            "renamed": count("rename"),
            "membership": count("add") + count("remove"),
            "deleted": count("delete"),
            "pending": plan.pending,
            "kept": plan.kept,
            "conflicts": plan.conflicts,
            "errors": errors,
            "changes": changes,
            "albums": plan.links.len(),
        })
    };
    let e = s.remote.sync.entry(account).or_default();
    e.albums_running = false;
    e.albums_last = Some(v.clone());
    v
}

fn sync_albums(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.syncAlbums";
    let id = account_param(s, p, C)?;
    start_albums(s, &id, bool_or(p, "dryRun", false), bool_or(p, "background", false))
}

/// An album deletion waiting for the user: `apply` deletes the surviving album too (never its
/// photos); `keep` keeps it, unlinked, so the next run puts it back where it was deleted.
fn resolve_album_deletion(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.resolveAlbumDeletion";
    let id = account_param(s, p, C)?;
    let album = p.get("album").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `album` (catalog album id)"))?;
    let apply = match str_param(p, "action") {
        Some("apply") => true,
        Some("keep") => false,
        _ => return Err(bad(C, "`action` must be apply or keep")),
    };
    let mut pending = load_pending(s, &id);
    let Some(pos) = pending.iter().position(|d| d.album == album) else { return Err(bad(C, "no deletion waiting for that album")) };
    let d = pending.remove(pos);
    if apply {
        match d.deleted_on {
            plan_albums::DeletedOn::Catalog => {
                let (_, client) = match s.immich_client(&id) {
                    Ok(x) => x,
                    Err(e) => return Ok(failure(&e)),
                };
                if let Err(e) = client.delete_album(&d.immich) {
                    return Ok(failure(&e));
                }
            }
            plan_albums::DeletedOn::Immich => {
                s.execute("album.delete", &json!({"id": album}))?;
            }
        }
    }
    let mut links = load_links(s, &id);
    links.remove(&album);
    save_links(s, &id, &links).map_err(|e| bad(C, e))?;
    save_pending(s, &id, &pending).map_err(|e| bad(C, e))?;
    Ok(json!({"ok": true, "album": album, "applied": apply}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "immich.linkAlbum", "Link Album with Immich", [], None, "{account?, album: catalog album id, immichAlbum?: id (default: the Immich album of the same name, created if missing), unlink?: bool}: the album's linked photos sync both ways with the Immich album (immich.syncAlbums) → {ok, album, immichAlbum}", always, link_album),
        cmd!(query "immich.syncAlbums", "Sync Albums with Immich", [], None, "{account?, dryRun?: bool, background?: bool}: collections ↔ albums by the account's album rule (immich.setSync config.albums: direction off = only albums linked by hand, membership both ways; toImmich|fromImmich|twoWay = every plain album maps to the Immich album of the same name, with creates, renames, membership and album deletes, never photo deletes; the policy decides deletes and names changed on both sides, ask leaves deletes in `pending`; a catalog album that holds photos without an Immich link is never deleted for an Immich-side delete: it is kept, taken out of the sync and listed in `kept`) → {ok, dryRun, created, renamed, membership, deleted, pending, kept: [{album, name, unlinked}], conflicts, errors, changes: {toImmich, toCatalog}, albums} or {started}", always, sync_albums),
        cmd!(query "immich.resolveAlbumDeletion", "Resolve Immich Album Deletion", [], None, "{account?, album: catalog album id, action: apply|keep}: an album deleted on one side and waiting (immich.syncAlbums → pending): apply deletes the other side's album too (photos stay), keep unlinks it so it is created again where it was deleted → {ok, album, applied}", always, resolve_album_deletion),
    ]
}
