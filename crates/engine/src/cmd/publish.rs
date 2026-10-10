//! Publish services (LRC-LIB-PUBLISH; model in `dac-publish`). A service (Hard Drive now) holds
//! published collections, each a regular album inside a collection set named after the service;
//! `publish.run` renders the collection's new and modified photos with the service's export
//! settings, sends them, and takes removed photos off the service. What was published where is a
//! remote link (`remote_identity`, service `publish`), written as app bookkeeping (not an undo
//! step: undo can't unpublish a file).
//!
//! The app runs a publish in three steps so the slow middle runs on a worker thread:
//! [`plan`] (needs the session) → [`Plan::execute`] (render + send, no session) → [`apply`].

use std::path::PathBuf;

use dac_catalog::{Album, AlbumId, Catalog, Op, PhotoId, RemoteIdentity, SyncState};
use dac_publish::{CollectionConfig, PublishConfig, ServiceConfig, Upload};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::export::{ExportOptions, PreparedExport};
use crate::{Result, Session};

fn lib_dir(s: &Session, c: &str) -> Result<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(c, "publish services need a library on disk"))
}

fn load(s: &Session, c: &str) -> Result<(PathBuf, PublishConfig)> {
    let dir = lib_dir(s, c)?;
    let cfg = PublishConfig::load(&dir).map_err(|e| bad(c, e.to_string()))?;
    Ok((dir, cfg))
}

fn save(dir: &std::path::Path, cfg: &PublishConfig, c: &str) -> Result<()> {
    cfg.save(dir).map_err(|e| bad(c, e.to_string()))
}

/// `service`: a service id, or its name.
fn service_id(cfg: &PublishConfig, p: &Value, c: &str) -> Result<String> {
    let want = str_param(p, "service").map(str::trim).ok_or_else(|| bad(c, "missing `service`"))?;
    cfg.services
        .iter()
        .find(|s| s.id == want)
        .or_else(|| cfg.services.iter().find(|s| s.name.eq_ignore_ascii_case(want)))
        .map(|s| s.id.clone())
        .ok_or_else(|| bad(c, format!("no publish service `{want}` (see publish.services)")))
}

/// `collection`: a published collection's album id.
fn collection(cfg: &PublishConfig, p: &Value, c: &str) -> Result<(ServiceConfig, CollectionConfig)> {
    let album = p.get("collection").and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(c, "missing `collection` (an album id)"))?;
    let svc = cfg.service_of(album).ok_or_else(|| bad(c, format!("album {} is not a published collection", album.0)))?;
    let coll = svc.collection(album).cloned().ok_or_else(|| bad(c, "no such collection"))?;
    Ok((svc.clone(), coll))
}

fn service_json(cat: &Catalog, svc: &ServiceConfig) -> Value {
    let colls: Vec<Value> = svc
        .collections
        .iter()
        .map(|c| {
            let st = dac_publish::status(cat, &svc.id, c.album);
            json!({
                "album": c.album.0,
                "name": cat.album(c.album).map(|a| a.name.clone()).unwrap_or_else(|| c.folder.clone()),
                "folder": c.folder,
                "new": st.new.len(),
                "modified": st.modified.len(),
                "published": st.published.len(),
                "toRemove": st.to_remove.len(),
            })
        })
        .collect();
    let caps = match svc.kind.as_str() {
        dac_publish::KIND_HARD_DRIVE => json!({"comments": false, "likes": false}),
        dac_publish::KIND_IMMICH => json!({"comments": false, "likes": true}),
        dac_publish::KIND_SFTP => json!({"comments": false, "likes": false}),
        _ => json!(null),
    };
    json!({
        "id": svc.id, "kind": svc.kind, "name": svc.name, "settings": svc.settings, "export": svc.export,
        "set": svc.set.map(|a| a.0), "collections": colls, "capabilities": caps,
    })
}

fn services(s: &Session) -> Result<Value> {
    let (_, cfg) = load(s, "publish.services")?;
    Ok(json!({"services": cfg.services.iter().map(|x| service_json(&s.catalog, x)).collect::<Vec<_>>(), "kinds": dac_publish::KINDS}))
}

/// Check a service's settings and export params.
fn check(kind: &str, settings: &Value, export: &Value, c: &str) -> Result<()> {
    if !dac_publish::KINDS.contains(&kind) && !plugin_kind(kind) {
        return Err(bad(c, format!("unknown kind `{kind}` (known: {})", dac_publish::KINDS.join(", "))));
    }
    if kind == dac_publish::KIND_HARD_DRIVE {
        let dir = settings.get("dir").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty());
        let dir = dir.ok_or_else(|| bad(c, "a Hard Drive service needs `dir` (the folder it publishes to)"))?;
        if !std::path::Path::new(dir).is_absolute() {
            return Err(bad(c, format!("`{dir}` is not an absolute folder path")));
        }
    }
    if kind == dac_publish::KIND_IMMICH {
        super::immich::publish::check_settings(settings).map_err(|e| bad(c, e))?;
    }
    if kind == dac_publish::KIND_SFTP {
        sftp::check_settings(settings).map_err(|e| bad(c, e))?;
    }
    if !export.is_object() {
        return Err(bad(c, "`export` must be an object of app.export params"));
    }
    if let Some(k) = crate::export::TARGET_PARAMS.iter().find(|k| export.get(**k).is_some()) {
        return Err(bad(c, format!("`export` can't hold `{k}`: the service decides where files go")));
    }
    ExportOptions::validate(c, export)
}

fn export_param(p: &Value) -> Value {
    p.get("export").cloned().unwrap_or_else(|| json!({"format": "jpeg", "quality": 90, "longEdge": 2048}))
}

fn create_service(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.createService";
    let (dir, mut cfg) = load(s, C)?;
    let kind = str_param(p, "kind").unwrap_or(dac_publish::KIND_HARD_DRIVE).to_string();
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Hard Drive").to_string();
    if cfg.services.iter().any(|x| x.name.eq_ignore_ascii_case(&name)) {
        return Err(bad(C, format!("a publish service is already called `{name}`")));
    }
    if cfg.services.len() >= dac_publish::config::MAX_SERVICES {
        return Err(bad(C, "too many publish services"));
    }
    let settings = match str_param(p, "dir") {
        Some(d) => json!({"dir": d.trim()}),
        None => p.get("settings").cloned().unwrap_or_else(|| json!({})),
    };
    let export = export_param(p);
    check(&kind, &settings, &export, C)?;
    // the collection set that shows the service's collections in the Collections panel
    let set = s.catalog.alloc_album_id();
    let mut album = Album::new(set, format!("{name} (Published)"));
    album.folder = true;
    s.commit("New Publish Service", Op::AddAlbum { album })?;
    let id = cfg.alloc_id();
    cfg.services.push(ServiceConfig { id: id.clone(), kind, name, settings, export, set: Some(set), collections: Vec::new() });
    save(&dir, &cfg, C)?;
    Ok(cfg.service(&id).map(|x| service_json(&s.catalog, x)).unwrap_or(Value::Null))
}

fn update_service(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.updateService";
    let (dir, mut cfg) = load(s, C)?;
    let id = service_id(&cfg, p, C)?;
    let Some(svc) = cfg.service(&id).cloned() else { return Err(bad(C, "no such service")) };
    let mut next = svc.clone();
    if let Some(n) = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        next.name = n.to_string();
    }
    if let Some(d) = str_param(p, "dir") {
        next.settings["dir"] = json!(d.trim());
    }
    if let Some(e) = p.get("export") {
        next.export = e.clone();
    }
    check(&next.kind, &next.settings, &next.export, C)?;
    if next.name != svc.name
        && let Some(set) = next.set.filter(|a| s.catalog.album(*a).is_some())
    {
        s.commit("Rename Publish Service", Op::RenameAlbum { id: set, name: format!("{} (Published)", next.name) })?;
    }
    if let Some(x) = cfg.service_mut(&id) {
        *x = next;
    }
    save(&dir, &cfg, C)?;
    Ok(cfg.service(&id).map(|x| service_json(&s.catalog, x)).unwrap_or(Value::Null))
}

fn delete_service(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.deleteService";
    let (dir, mut cfg) = load(s, C)?;
    let id = service_id(&cfg, p, C)?;
    let Some(svc) = cfg.service(&id).cloned() else { return Err(bad(C, "no such service")) };
    // the links go (published files stay where they are); the albums go as one undo step
    for c in &svc.collections {
        unlink_collection(s, &svc.id, c.album)?;
    }
    let mut ops: Vec<Op> = svc.collections.iter().filter(|c| s.catalog.album(c.album).is_some()).map(|c| Op::RemoveAlbum { id: c.album }).collect();
    if let Some(set) = svc.set.filter(|a| s.catalog.album(*a).is_some()) {
        ops.push(Op::RemoveAlbum { id: set });
    }
    if !ops.is_empty() {
        s.commit("Delete Publish Service", Op::Batch { ops })?;
    }
    cfg.services.retain(|x| x.id != id);
    save(&dir, &cfg, C)?;
    services(s)
}

fn unlink_collection(s: &mut Session, service: &str, album: AlbumId) -> Result<()> {
    let ops: Vec<Op> = dac_publish::state::links(&s.catalog, service, album)
        .map(|r| Op::SetRemote { photo: r.photo_id, service: r.service.clone(), account_id: r.account_id.clone(), record: None })
        .collect();
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    Ok(())
}

fn create_collection(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.createCollection";
    let (dir, mut cfg) = load(s, C)?;
    let id = service_id(&cfg, p, C)?;
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?.to_string();
    let folder = dac_publish::hard_drive::safe_name(&name).ok_or_else(|| bad(C, format!("`{name}` can't be a collection name")))?;
    let Some(svc) = cfg.service(&id) else { return Err(bad(C, "no such service")) };
    if svc.collections.iter().any(|c| c.folder.eq_ignore_ascii_case(&folder)) {
        return Err(bad(C, format!("`{}` already has a collection `{name}`", svc.name)));
    }
    if svc.collections.len() >= dac_publish::config::MAX_COLLECTIONS {
        return Err(bad(C, "too many collections in this service"));
    }
    let parent = svc.set.filter(|a| s.catalog.album(*a).is_some_and(|al| al.folder));
    let album = s.catalog.alloc_album_id();
    let mut al = Album::new(album, name);
    al.parent = parent;
    let mut ops = vec![Op::AddAlbum { album: al }];
    let photos = if super::bool_or(p, "addSelected", false) { s.targets(&Value::Null) } else { Vec::new() };
    if !photos.is_empty() {
        ops.push(Op::SetAlbumCover { id: album, cover: photos.first().copied() });
        ops.push(Op::SetAlbumPhotos { id: album, photos });
    }
    s.commit("New Published Collection", Op::Batch { ops })?;
    if let Some(x) = cfg.service_mut(&id) {
        x.collections.push(CollectionConfig { album, folder });
    }
    save(&dir, &cfg, C)?;
    status_json(s, &cfg, album)
}

fn delete_collection(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.deleteCollection";
    let (dir, mut cfg) = load(s, C)?;
    let (svc, coll) = collection(&cfg, p, C)?;
    unlink_collection(s, &svc.id, coll.album)?;
    if s.catalog.album(coll.album).is_some() {
        s.commit("Delete Published Collection", Op::RemoveAlbum { id: coll.album })?;
    }
    if let Some(x) = cfg.service_mut(&svc.id) {
        x.collections.retain(|c| c.album != coll.album);
    }
    save(&dir, &cfg, C)?;
    services(s)
}

fn status_json(s: &Session, cfg: &PublishConfig, album: AlbumId) -> Result<Value> {
    let svc = cfg.service_of(album).ok_or_else(|| bad("publish.status", "not a published collection"))?;
    let st = dac_publish::status(&s.catalog, &svc.id, album);
    let ids = |v: &[PhotoId]| v.iter().map(|i| i.0).collect::<Vec<_>>();
    Ok(json!({
        "service": svc.id, "collection": album.0,
        "name": s.catalog.album(album).map(|a| a.name.clone()),
        "new": ids(&st.new), "modified": ids(&st.modified), "published": ids(&st.published), "toRemove": ids(&st.to_remove),
        "pending": st.pending(),
    }))
}

fn mark_up_to_date(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.markUpToDate";
    let (_, cfg) = load(s, C)?;
    let (svc, coll) = collection(&cfg, p, C)?;
    let st = dac_publish::status(&s.catalog, &svc.id, coll.album);
    let want: Option<Vec<PhotoId>> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(PhotoId).collect());
    let acct = dac_publish::account_id(&svc.id, coll.album);
    let mut ops = Vec::new();
    for id in st.modified.iter().filter(|i| want.as_ref().is_none_or(|w| w.contains(i))) {
        let (Some(photo), Some(link)) = (s.catalog.photo(*id), s.catalog.remote_links().get(&(*id, dac_publish::SERVICE.to_string(), acct.clone())))
        else {
            continue;
        };
        let mut r = link.clone();
        r.remote_checksum = Some(dac_publish::fingerprint(photo));
        ops.push(Catalog::link_remote_op(r));
    }
    let n = ops.len();
    if n > 0 {
        s.apply_system(Op::Batch { ops })?;
    }
    let mut out = status_json(s, &cfg, coll.album)?;
    out["marked"] = json!(n);
    Ok(out)
}

/// One photo to render and send.
pub struct Job {
    pub photo: PhotoId,
    prepared: PreparedExport,
    previous: Option<String>,
    fingerprint: String,
}

/// A publish run, set up from the session ([`plan`]); [`Plan::execute`] needs no session.
pub struct Plan {
    pub service: ServiceConfig,
    pub collection: CollectionConfig,
    pub jobs: Vec<Job>,
    /// Photos to take off the service: (photo, remote id).
    pub removals: Vec<(PhotoId, String)>,
    /// Photos that couldn't be prepared (missing original, …).
    pub failed: Vec<(PhotoId, String)>,
    /// Opens a service the session had to set up (Immich: its key and per-photo data).
    pub open: Option<super::immich::publish::Opener>,
}

/// What a run did, for [`apply`].
pub struct Outcome {
    pub service: String,
    pub album: AlbumId,
    pub published: Vec<(PhotoId, String, String)>,
    pub removed: Vec<PhotoId>,
    pub failed: Vec<(PhotoId, String)>,
    pub cancelled: bool,
}

/// Set up the publish of a collection: its new and modified photos rendered with the service's
/// export settings, and the photos to remove.
pub fn plan(s: &mut Session, album: AlbumId) -> Result<Plan> {
    const C: &str = "publish.run";
    let (_, cfg) = load(s, C)?;
    let (svc, coll) = collection(&cfg, &json!({"collection": album.0}), C)?;
    let opts = ExportOptions::from_params(&svc.export)?;
    let st = dac_publish::status(&s.catalog, &svc.id, album);
    let acct = dac_publish::account_id(&svc.id, album);
    let link = |cat: &Catalog, id: PhotoId| cat.remote_links().get(&(id, dac_publish::SERVICE.to_string(), acct.clone())).cloned();
    let mut jobs = Vec::new();
    let mut failed = Vec::new();
    for (i, id) in st.new.iter().chain(&st.modified).enumerate() {
        let Some(fp) = s.catalog.photo(*id).map(|p| dac_publish::fingerprint(p)) else { continue };
        let previous = link(&s.catalog, *id).map(|l| l.remote_id);
        match crate::export::prepare_export(s, *id, &opts, i + 1) {
            Ok(prepared) => jobs.push(Job { photo: *id, prepared, previous, fingerprint: fp }),
            Err(e) => failed.push((*id, e)),
        }
    }
    let removals = st.to_remove.iter().filter_map(|id| link(&s.catalog, *id).map(|l| (*id, l.remote_id))).collect();
    let photos: Vec<PhotoId> = jobs.iter().map(|j: &Job| j.photo).collect();
    let open = match sftp::opener(s, &svc, &coll).map_err(|e| bad(C, e))? {
        Some(o) => Some(o),
        None => super::immich::publish::opener(s, &svc, &coll, &photos).map_err(|e| bad(C, e))?,
    };
    Ok(Plan { service: svc, collection: coll, jobs, removals, failed, open })
}

impl Plan {
    /// Photos to send plus photos to remove.
    pub fn len(&self) -> usize {
        self.jobs.len() + self.removals.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Render and send (no session needed: run it on a worker thread). `progress(done, total)`
    /// returns false to stop.
    pub fn execute(self, progress: &mut dyn FnMut(usize, usize) -> bool) -> Outcome {
        let total = self.len();
        let mut out = Outcome {
            service: self.service.id.clone(),
            album: self.collection.album,
            published: Vec::new(),
            removed: Vec::new(),
            failed: self.failed,
            cancelled: false,
        };
        let opened = match self.open {
            Some(open) => open(),
            None => open_any(&self.service, &self.collection),
        };
        let mut svc = match opened {
            Ok(x) => x,
            Err(e) => {
                out.failed.extend(self.jobs.iter().map(|j| (j.photo, e.to_string())));
                return out;
            }
        };
        let mut done = 0;
        for (photo, remote) in self.removals {
            if !progress(done, total) {
                out.cancelled = true;
                return out;
            }
            match svc.remove(&remote) {
                Ok(()) => out.removed.push(photo),
                Err(e) => out.failed.push((photo, e.to_string())),
            }
            done += 1;
        }
        for job in self.jobs {
            if !progress(done, total) {
                out.cancelled = true;
                return out;
            }
            let r = job.prepared.run().and_then(|x| {
                let up =
                    Upload { photo: job.photo, file_name: &x.file_name, bytes: &x.bytes, sidecars: &x.sidecars, previous: job.previous.as_deref() };
                svc.publish(&up).map_err(|e| e.to_string())
            });
            match r {
                Ok(p) => out.published.push((job.photo, p.remote_id, job.fingerprint)),
                Err(e) => out.failed.push((job.photo, e)),
            }
            done += 1;
        }
        progress(done, total);
        out
    }
}

/// Record a run's links (app bookkeeping, not an undo step).
pub fn apply(s: &mut Session, out: Outcome) -> Result<Value> {
    let acct = dac_publish::account_id(&out.service, out.album);
    let now = dac_catalog::rules::now();
    let mut ops = Vec::new();
    for (photo, remote_id, fp) in &out.published {
        if s.catalog.photo(*photo).is_none() {
            continue;
        }
        ops.push(Catalog::link_remote_op(RemoteIdentity {
            photo_id: *photo,
            service: dac_publish::SERVICE.into(),
            account_id: acct.clone(),
            remote_id: remote_id.clone(),
            remote_checksum: Some(fp.clone()),
            remote_updated_at: Some(now.clone()),
            last_synced_at: Some(now.clone()),
            sync_state: SyncState::Synced,
        }));
    }
    // Immich: the uploaded originals become the photos' Immich links (sync needs no immich.link)
    if let Ok((_, cfg)) = load(s, "publish.run")
        && let Some(svc) = cfg.service(&out.service)
    {
        let sent: Vec<(PhotoId, String)> = out.published.iter().map(|(p, r, _)| (*p, r.clone())).collect();
        ops.extend(super::immich::publish::original_link_ops(s, svc, &sent));
    }
    for photo in &out.removed {
        ops.push(Op::SetRemote { photo: *photo, service: dac_publish::SERVICE.into(), account_id: acct.clone(), record: None });
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    Ok(json!({
        "collection": out.album.0,
        "published": out.published.iter().map(|(p, r, _)| json!({"id": p.0, "remoteId": r})).collect::<Vec<_>>(),
        "removed": out.removed.iter().map(|p| p.0).collect::<Vec<_>>(),
        "failed": out.failed.iter().map(|(p, e)| json!({"id": p.0, "error": e})).collect::<Vec<_>>(),
        "cancelled": out.cancelled,
    }))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.run";
    let (_, cfg) = load(s, C)?;
    let albums: Vec<AlbumId> = match (p.get("collection"), str_param(p, "service")) {
        (Some(_), _) => vec![collection(&cfg, p, C)?.1.album],
        (None, Some(_)) => {
            let id = service_id(&cfg, p, C)?;
            cfg.service(&id).map(|x| x.collections.iter().map(|c| c.album).collect()).unwrap_or_default()
        }
        (None, None) => return Err(bad(C, "give `collection` or `service`")),
    };
    let mut runs = Vec::new();
    for a in albums {
        let plan = plan(s, a)?;
        let out = plan.execute(&mut |_, _| true);
        runs.push(apply(s, out)?);
    }
    Ok(json!({"runs": runs}))
}

fn comments(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "publish.comments";
    let (_, cfg) = load(s, C)?;
    let (svc, coll) = collection(&cfg, p, C)?;
    let id = p.get("id").and_then(Value::as_u64).map(PhotoId).or(s.active()).ok_or_else(|| bad(C, "missing `id`"))?;
    let acct = dac_publish::account_id(&svc.id, coll.album);
    let Some(link) = s.catalog.remote_links().get(&(id, dac_publish::SERVICE.to_string(), acct)).cloned() else {
        return Ok(json!({"comments": [], "supported": true, "published": false}));
    };
    let mut service = open_any(&svc, &coll).map_err(|e| bad(C, e.to_string()))?;
    let caps = service.capabilities();
    let list = service.comments(&link.remote_id).map_err(|e| bad(C, e.to_string()))?;
    Ok(json!({"comments": list, "supported": caps.comments, "published": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "publish.services", "Publish Services", [], None,
            "{} → {services: [{id, kind, name, settings, export, set, collections: [{album, name, folder, new, modified, published, toRemove}], capabilities}], kinds}",
            always, |s, _| services(s)),
        cmd!(
            "publish.createService",
            "Set Up Publish Service…",
            [],
            None,
            "{kind?: hardDrive (default), name?, dir: absolute folder (Hard Drive), export?: app.export params without targets (default JPEG, 2048 px)} — also makes the service's collection set → the service",
            always,
            create_service
        ),
        cmd!(
            "publish.updateService",
            "Edit Publish Service",
            [],
            None,
            "{service: id | name, name?, dir?, export?} → the service",
            always,
            update_service
        ),
        cmd!(
            "publish.deleteService",
            "Delete Publish Service",
            [],
            None,
            "{service: id | name} — removes its collections (published files stay) → publish.services",
            always,
            delete_service
        ),
        cmd!(
            "publish.createCollection",
            "Create Published Collection…",
            [],
            None,
            "{service: id | name, name, addSelected?: bool} — a published collection (an album in the service's set) → publish.status",
            always,
            create_collection
        ),
        cmd!(
            "publish.deleteCollection",
            "Delete Published Collection",
            [],
            None,
            "{collection: albumId} — forgets what was published (files stay) → publish.services",
            always,
            delete_collection
        ),
        cmd!(query "publish.status", "Publish Status", [], None,
        "{collection: albumId} → {service, collection, name, new, modified, published, toRemove (photo ids), pending}",
        always, |s, p| {
            let (_, cfg) = load(s, "publish.status")?;
            let (_, coll) = collection(&cfg, p, "publish.status")?;
            status_json(s, &cfg, coll.album)
        }),
        cmd!(
            "publish.run",
            "Publish",
            [],
            None,
            "{collection: albumId} or {service: id | name} (all its collections) — renders new and modified photos with the service's export settings, sends them, removes photos taken out → {runs: [{collection, published: [{id, remoteId}], removed, failed: [{id, error}], cancelled}]}",
            always,
            run
        ),
        cmd!(
            "publish.markUpToDate",
            "Mark as Up-to-Date",
            [],
            None,
            "{collection: albumId, ids?: photo ids (default: all modified)} — modified photos count as published again → publish.status + {marked}",
            always,
            mark_up_to_date
        ),
        cmd!(query "publish.comments", "Published Photo Comments", [], None,
            "{collection: albumId, id?: photo (default active)} → {comments: [{author, text, date}], supported, published}",
            always, comments),
    ]
}

// P4.3: plug-in publish services (`plugin:<id>` kinds, see `cmd::plugins`).
#[cfg(not(target_arch = "wasm32"))]
fn plugin_kind(kind: &str) -> bool {
    super::plugins::is_plugin_kind(kind)
}
#[cfg(target_arch = "wasm32")]
fn plugin_kind(_: &str) -> bool {
    false
}
fn open_any(
    svc: &dac_publish::ServiceConfig,
    coll: &dac_publish::CollectionConfig,
) -> std::result::Result<Box<dyn dac_publish::PublishService>, dac_publish::PublishError> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = super::plugins::open_plugin_service(svc) {
        return r;
    }
    dac_publish::open_service(svc, coll)
}

#[path = "publish_sftp.rs"]
mod sftp;

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
