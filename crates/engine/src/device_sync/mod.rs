//! Sync between this app's copies on the local network (Settings ▸ Sync).
//!
//! Three layers:
//!
//! - `lcc-peer` finds the app on other devices, pairs with them and gives an encrypted,
//!   mutually authenticated channel. This module starts its [`lcc_peer::Service`] on demand
//!   (`sync.find`, or at launch when paired devices exist): a session that never asks opens no
//!   sockets.
//! - `lcc-sync` holds what a group of devices shares (the group, its collections with what each
//!   owner allows and each device takes, which photos are in them, each photo's record) and
//!   merges two copies of it. One [`lcc_sync::Replica`] per catalogue, in
//!   `<catalogue>/sync/replica.json`.
//! - This module ties both to the catalogue: [`catalog`] tells the replica what changed here and
//!   writes what other devices changed; [`wire`] runs the exchange with one device on its own
//!   thread (records first, then the files each side still lacks: smart previews or originals).
//!
//! The exchange threads never touch the catalogue. They ask the session ([`Job`]), which answers
//! from `sync.tick` (the app calls it every second while sync runs), so the catalogue has one
//! writer and a slow network never stalls the window.
//!
//! Design and what is not built yet: `docs/sync.md`.

pub(crate) mod catalog;
pub(crate) mod record;
#[cfg(test)]
mod tests;
mod wire;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lcc_catalog::PhotoId;
use lcc_sync::model::{CollectionKind, Grant, Mode, Take};
use lcc_sync::pill::{self, Depth, Location, Pill, Status};
use lcc_sync::proto::{Batch, Hello};
use lcc_sync::replica::Outgoing;
use lcc_sync::{Group, Replica};
use serde::Serialize;
use serde_json::{Value, json};

use crate::activity::{Activity, Cancel, TaskGuard};
use crate::{EngineError, Result, Session};

/// How often a device looks in on each device it syncs with when nothing is known to wait.
const EXCHANGE_EVERY: Duration = Duration::from_secs(20);
/// How soon after a change here it is sent.
const SOON: Duration = Duration::from_secs(2);
/// How long changes to the replica may wait to be written.
const SAVE_EVERY: Duration = Duration::from_secs(5);

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

/// A file the session wants from a device.
#[derive(Clone, Debug)]
pub(crate) struct Want {
    pub key: String,
    pub depth: Depth,
    /// Where it goes when complete.
    pub dest: PathBuf,
}

/// What an exchange thread asks of the session (answered by `sync.tick`).
pub(crate) enum Job {
    /// This device's hello for `peer`.
    Hello { peer: String, reply: Sender<Hello> },
    /// `peer` said hello: an invitation to keep, a device that went elsewhere to note.
    Their { peer: lcc_peer::Peer, hello: Box<Hello> },
    /// What to send `peer` now.
    Prepare { peer: String, reply: Sender<(Batch, Outgoing)> },
    /// `peer` confirmed a batch.
    Confirm { peer: String, out: Outgoing },
    /// Merge what `peer` sent; `Err` when it can't be taken now (asked again next time).
    Apply { peer: lcc_peer::Peer, batch: Box<Batch>, reply: Sender<std::result::Result<(), String>> },
    /// The files to ask `peer` for.
    Wants { peer: String, reply: Sender<Vec<Want>> },
    /// Where the files `peer` asks for are (`None`: not here, or not allowed; `later`: being made).
    Locate { peer: String, wants: Vec<(String, Depth)>, reply: Sender<Vec<std::result::Result<PathBuf, bool>>> },
    /// A file arrived whole and checked.
    Got { key: String, depth: Depth },
    /// `peer` doesn't have the file.
    Missing { peer: String, key: String },
}

/// How the exchanges with one device go.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Link {
    pub running: bool,
    /// When the last exchange finished (Unix seconds).
    pub last: u64,
    pub error: Option<String>,
    /// Records sent and taken, files received, in the last exchange that moved anything.
    pub sent: usize,
    pub received: usize,
    pub files: usize,
}

/// An invitation to a group.
#[derive(Clone, Debug)]
struct Offer {
    peer: lcc_peer::Peer,
    group: Group,
    catalogue: String,
}

/// Shared between the session and the exchange threads.
pub(crate) struct Hub {
    /// This device's id.
    pub me: String,
    pub activity: Activity,
    pub jobs: Mutex<Vec<Job>>,
    pub running: Mutex<HashSet<String>>,
    pub links: Mutex<HashMap<String, Link>>,
    /// The photos whose files travel right now.
    pub moving: Mutex<HashSet<String>>,
    offers: Mutex<Vec<Offer>>,
    tried: Mutex<HashMap<String, Instant>>,
}

impl Hub {
    pub(crate) fn ask(&self, job: Job) {
        lock(&self.jobs).push(job);
    }
}

/// The open catalogue's side of sync.
pub(crate) struct Lib {
    pub dir: PathBuf,
    pub replica: Replica,
    /// The catalogue's revision the replica was last told of.
    pub revision: Option<u64>,
    pub keys: HashMap<String, PhotoId>,
    pub ids: HashMap<PhotoId, String>,
    /// Each shared collection's photos as the catalogue had them when last looked at.
    pub base: HashMap<String, BTreeSet<String>>,
    /// The photos this device should hold more of than it does.
    pub wanting: BTreeSet<String>,
    /// Files a device said it doesn't have (not asked again for a while).
    pub missing: HashMap<(String, String), Instant>,
    pub dirty: bool,
    saved_at: Instant,
    /// Counts up whenever what the pills show may have changed.
    pub generation: u64,
    /// When something last changed here that a device is owed.
    pub changed_at: Option<Instant>,
    /// Each collection's size and a sum of its photo ids when last compared (a cheap "did it change").
    pub album_sig: HashMap<u64, (usize, u64)>,
    pub unfiled_at: Option<Instant>,
    pills: Mutex<(u64, HashMap<PhotoId, Option<Pill>>, HashMap<u64, Option<Pill>>)>,
}

/// The session's sync state.
#[derive(Default)]
pub struct State {
    /// `<settings>/sync/peers.json`; `None` keeps the feature off (tests, `--memory`).
    pub store_path: Option<PathBuf>,
    /// The multicast DNS port; `None` = [`lcc_peer::beacon::PORT`] (5353; tests use their own).
    pub port: Option<u16>,
    /// How often exchanges start; `None` = [`EXCHANGE_EVERY`] (tests go faster).
    pub exchange_every: Option<Duration>,
    service: Option<Arc<lcc_peer::Service>>,
    /// The activity row while looking for devices, and its cancel flag.
    finding: Option<(TaskGuard, Arc<AtomicBool>)>,
    hub: Option<Arc<Hub>>,
    pub(crate) lib: Option<Lib>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").field("store_path", &self.store_path).field("running", &self.service.is_some()).finish_non_exhaustive()
    }
}

/// What a new device is called: the computer's name where the system says it, else the platform.
pub fn default_device_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty() && h != "localhost");
    host.unwrap_or_else(|| {
        if cfg!(target_os = "android") {
            "Android device".into()
        } else if cfg!(target_os = "macos") {
            "Mac".into()
        } else if cfg!(windows) {
            "Windows PC".into()
        } else {
            "Computer".into()
        }
    })
}

impl State {
    /// The service, started if needed (with the hub answering paired devices).
    fn ensure(&mut self, activity: &Activity) -> Result<&Arc<lcc_peer::Service>> {
        if self.service.is_none() {
            let path = self.store_path.clone().ok_or_else(|| EngineError::Other("sync needs a settings folder".into()))?;
            let svc = Arc::new(lcc_peer::Service::start(path, &default_device_name(), self.port.unwrap_or(lcc_peer::beacon::PORT)).map_err(other)?);
            let hub = Arc::new(Hub {
                me: svc.device_id(),
                activity: activity.clone(),
                jobs: Mutex::new(Vec::new()),
                running: Mutex::new(HashSet::new()),
                links: Mutex::new(HashMap::new()),
                moving: Mutex::new(HashSet::new()),
                offers: Mutex::new(Vec::new()),
                tried: Mutex::new(HashMap::new()),
            });
            let h = hub.clone();
            svc.set_hello_handler(Arc::new(move |peer, ch| wire::answer(&h, peer, ch)));
            self.hub = Some(hub);
            self.service = Some(svc);
        }
        self.service.as_ref().ok_or_else(|| EngineError::Other("sync isn't running".into()))
    }

    /// Stops looking for devices if the activity row was cancelled.
    fn reconcile(&mut self) {
        let cancelled = self.finding.as_ref().is_some_and(|(_, flag)| flag.load(Ordering::Relaxed));
        if cancelled {
            self.finding = None;
            if let Some(s) = &self.service {
                s.set_finding(false);
            }
        }
    }

    /// The stored identity and paired devices without starting anything.
    fn stored(&self) -> Option<lcc_peer::Store> {
        let path = self.store_path.as_ref()?;
        path.is_file().then(|| lcc_peer::Store::load_or_create(path, "").ok()).flatten()
    }

    fn device_id(&self) -> Option<String> {
        self.service.as_ref().map(|s| s.device_id()).or_else(|| self.stored().map(|s| s.identity.device_id))
    }

    fn device_name(&self) -> String {
        self.service.as_ref().map(|s| s.status().name).or_else(|| self.stored().map(|s| s.identity.name)).unwrap_or_else(default_device_name)
    }

    fn paired(&self, device: &str) -> Option<lcc_peer::Peer> {
        self.stored().and_then(|st| st.peer(device).cloned())
    }
}

impl Session {
    /// Keep the sync identity and paired devices in `<settings>/sync/peers.json`.
    pub fn with_default_peers(mut self) -> Self {
        self.device_sync.store_path = crate::config::config_dir().map(|d| d.join("sync").join("peers.json"));
        self
    }

    /// The pill of photo `id`: `None` when it isn't in a shared collection.
    pub fn sync_pill(&self, id: PhotoId) -> Option<Pill> {
        let lib = self.device_sync.lib.as_ref()?;
        lib.replica.group.as_ref()?;
        let mut cache = lock(&lib.pills);
        if cache.0 != lib.generation {
            *cache = (lib.generation, HashMap::new(), HashMap::new());
        }
        if let Some(p) = cache.1.get(&id) {
            return with_moving(*p, self.device_sync.hub.as_deref(), lib.ids.get(&id));
        }
        let p = lib.ids.get(&id).and_then(|key| photo_pill(&lib.replica, key));
        cache.1.insert(id, p);
        with_moving(p, self.device_sync.hub.as_deref(), lib.ids.get(&id))
    }

    /// The pill of the catalogue's collection `album`: `None` when it isn't shared.
    pub fn sync_collection_pill(&self, album: lcc_catalog::AlbumId) -> Option<Pill> {
        let lib = self.device_sync.lib.as_ref()?;
        lib.replica.group.as_ref()?;
        let mut cache = lock(&lib.pills);
        if cache.0 != lib.generation {
            *cache = (lib.generation, HashMap::new(), HashMap::new());
        }
        if let Some(p) = cache.2.get(&album.0) {
            return *p;
        }
        let p = lib.replica.collection_of_album(album.0).and_then(|id| collection_pill(lib, id, self.device_sync.hub.as_deref()));
        cache.2.insert(album.0, p);
        p
    }
}

fn with_moving(p: Option<Pill>, hub: Option<&Hub>, key: Option<&String>) -> Option<Pill> {
    let mut p = p?;
    if let (Some(h), Some(k)) = (hub, key) {
        p.syncing = lock(&h.moving).contains(k);
    }
    Some(p)
}

fn photo_pill(r: &Replica, key: &str) -> Option<Pill> {
    let (location, depth) = pill::of_photo(r.collections_of(key), &r.device)?;
    let have = r.photos.get(key).map_or(Depth::None, |e| e.have);
    let status = if have < r.depth_wanted(key) {
        Status::RemoteAhead
    } else if r.photo_ahead(key) {
        Status::LocalAhead
    } else {
        Status::Synced
    };
    Some(Pill { location, depth, status, syncing: false })
}

fn collection_pill(lib: &Lib, id: &str, hub: Option<&Hub>) -> Option<Pill> {
    let r = &lib.replica;
    let c = r.collection(id)?;
    let (location, depth) = pill::of_collection(c, &r.device);
    if location == Location::Local && depth == Depth::None {
        // not shared with anyone: nothing to say
        return None;
    }
    let behind = r.members_of(id).any(|k| lib.wanting.contains(k));
    let status = if behind {
        Status::RemoteAhead
    } else if r.collection_ahead(id) {
        Status::LocalAhead
    } else {
        Status::Synced
    };
    let syncing = status != Status::Synced && hub.is_some_and(|h| !lock(&h.running).is_empty());
    Some(Pill { location, depth, status, syncing })
}

// ── the service: finding, pairing ──────────────────────────────────────────────────────────

pub(crate) fn status(s: &mut Session) -> Result<Value> {
    let mut v = base_status(s)?;
    let online: HashSet<String> = s.device_sync.service.as_ref().map(|svc| svc.online_peers().into_iter().map(|p| p.device_id).collect()).unwrap_or_default();
    let paired: Vec<lcc_peer::Peer> = s.device_sync.stored().map(|st| st.peers).unwrap_or_default();
    let offers: Vec<Value> = s
        .device_sync
        .hub
        .as_ref()
        .map(|h| {
            lock(&h.offers)
                .iter()
                .map(|o| {
                    let me = s.device_sync.device_id().unwrap_or_default();
                    json!({
                        "deviceId": o.peer.device_id,
                        "name": o.peer.name,
                        "catalogue": o.catalogue,
                        "mode": o.group.mode,
                        "keeper": o.group.keeper,
                        // a shared catalogue starts from the keeper's: every other device begins with an empty one
                        "needsEmptyCatalogue": o.group.mode == Mode::Catalogue && o.group.keeper.as_deref() != Some(me.as_str()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let links = s.device_sync.hub.as_ref().map(|h| serde_json::to_value(&*lock(&h.links)).unwrap_or(Value::Null)).unwrap_or_else(|| json!({}));
    let catalogue = catalogue_status(s, &online, &paired);
    if let Some(o) = v.as_object_mut() {
        o.insert("offers".into(), json!(offers));
        o.insert("links".into(), links);
        o.insert("catalogue".into(), catalogue);
    }
    Ok(v)
}

fn catalogue_status(s: &Session, online: &HashSet<String>, paired: &[lcc_peer::Peer]) -> Value {
    let Some(l) = s.library.as_ref().filter(|l| l.on_disk) else { return Value::Null };
    let name = library_name(&l.dir);
    let Some(lib) = s.device_sync.lib.as_ref() else { return json!({"name": name, "group": null, "collections": [], "undecided": [], "photos": s.catalog.photos().count()}) };
    let r = &lib.replica;
    let me = r.device.as_str();
    let group = r.group.as_ref().filter(|g| g.has(me)).map(|g| {
        let devices: Vec<Value> = g
            .devices
            .iter()
            .map(|(id, d)| {
                json!({
                    "deviceId": id, "name": d.v.name, "left": d.v.left, "me": id == me,
                    "online": online.contains(id), "paired": paired.iter().any(|p| p.device_id == *id),
                })
            })
            .collect();
        json!({"id": g.id, "mode": g.mode, "keeper": g.keeper, "devices": devices, "invited": r.invited})
    });
    let mut collections = Vec::new();
    if let Some(g) = r.group.as_ref().filter(|g| g.has(me)) {
        for (id, e) in &r.collections {
            let c = &e.c;
            if c.gone() || !c.listed_for(me, g.mode) || (g.mode == Mode::Collections && c.owner != me && c.grant.v == Grant::None) {
                continue;
            }
            let (location, depth) = pill::of_collection(c, me);
            let takes: serde_json::Map<String, Value> = c.takes.iter().map(|(d, t)| (d.clone(), json!(t.v.within(c.grant.v)))).collect();
            collections.push(json!({
                "id": id,
                "album": r.albums.get(id),
                "name": c.meta.v.name,
                "parent": c.meta.v.parent,
                "kind": c.kind,
                "owner": c.owner,
                "ownerName": g.name_of(&c.owner),
                "mine": c.owner == me,
                "immich": c.is_immich(),
                "grant": c.grant.v,
                "take": c.take_of(me),
                "most": c.grant.v.most(),
                "takes": takes,
                "photos": r.members_of(id).count(),
                "letters": format!("{}{}", location.letter(), depth.letter()),
                "pill": collection_pill(lib, id, s.device_sync.hub.as_deref()),
            }));
        }
    }
    json!({
        "name": name,
        "group": group,
        "collections": collections,
        "undecided": catalog::undecided(s, lib),
        "unkeyed": catalog::unkeyed(s),
        "photos": s.catalog.photos().count(),
    })
}

fn base_status(s: &mut Session) -> Result<Value> {
    let st = &mut s.device_sync;
    st.reconcile();
    if let Some(svc) = &st.service {
        let mut v = serde_json::to_value(svc.status()).map_err(other)?;
        if let Some(o) = v.as_object_mut() {
            o.insert("running".into(), json!(true));
            o.insert("available".into(), json!(true));
        }
        return Ok(v);
    }
    let stored = st.stored();
    Ok(json!({
        "running": false,
        "available": st.store_path.is_some(),
        "deviceId": stored.as_ref().map(|s| s.identity.device_id.clone()),
        "name": stored.as_ref().map_or_else(default_device_name, |s| s.identity.name.clone()),
        "finding": false,
        "found": [],
        "paired": stored.map(|s| s.peers.iter().map(|p| json!({"deviceId": p.device_id, "name": p.name, "online": false, "pairedAt": p.paired_at})).collect::<Vec<_>>()).unwrap_or_default(),
        "pairing": null,
    }))
}

pub(crate) fn find(s: &mut Session, on: bool) -> Result<Value> {
    let activity = s.activity.clone();
    let st = &mut s.device_sync;
    if on {
        st.ensure(&activity)?.set_finding(true);
        if st.finding.is_none() {
            let flag = Arc::new(AtomicBool::new(false));
            let guard = activity.start("sync", "Looking for devices", Cancel::Flag(flag.clone()));
            st.finding = Some((guard, flag));
        }
    } else {
        st.finding = None;
        if let Some(svc) = &st.service {
            svc.set_finding(false);
        }
    }
    status(s)
}

/// Starts the service when paired devices exist (at launch), so they can find each other.
pub(crate) fn start(s: &mut Session) -> Result<Value> {
    let activity = s.activity.clone();
    let st = &mut s.device_sync;
    if st.service.is_none() && st.stored().is_some_and(|store| !store.peers.is_empty()) {
        st.ensure(&activity)?;
    }
    Ok(json!({"running": st.service.is_some()}))
}

pub(crate) fn pair(s: &mut Session, device: &str) -> Result<Value> {
    let a = s.activity.clone();
    s.device_sync.ensure(&a)?.pair(device).map_err(other)?;
    status(s)
}

pub(crate) fn confirm(s: &mut Session, accept: bool) -> Result<Value> {
    let a = s.activity.clone();
    s.device_sync.ensure(&a)?.confirm(accept).map_err(other)?;
    status(s)
}

pub(crate) fn clear_pairing(s: &mut Session) -> Result<Value> {
    if let Some(svc) = &s.device_sync.service {
        svc.clear_pairing();
    }
    status(s)
}

/// Removes a paired device: it also leaves the group of the open catalogue (the others learn it).
pub(crate) fn unpair(s: &mut Session, device: &str) -> Result<Value> {
    ensure_lib(s);
    if let Some(lib) = &mut s.device_sync.lib
        && lib.replica.group.as_ref().is_some_and(|g| g.devices.contains_key(device))
    {
        let _ = lib.replica.leave(device, now_ms());
        touched(lib);
    }
    let removed = match &s.device_sync.service {
        Some(svc) => svc.unpair(device).map_err(other)?,
        None => {
            let path = s.device_sync.store_path.clone().ok_or_else(|| EngineError::Other("sync needs a settings folder".into()))?;
            let mut store = s.device_sync.stored().ok_or_else(|| EngineError::Other("no device is paired".into()))?;
            let removed = store.remove_peer(device);
            if removed {
                store.save(&path).map_err(other)?;
            }
            removed
        }
    };
    if !removed {
        return Err(EngineError::Other(format!("no paired device `{device}`")));
    }
    if let Some(h) = &s.device_sync.hub {
        lock(&h.offers).retain(|o| o.peer.device_id != device);
        lock(&h.links).remove(device);
    }
    status(s)
}

/// This device's name: what other devices show, unique among the devices it syncs with.
pub(crate) fn rename(s: &mut Session, name: &str) -> Result<Value> {
    ensure_lib(s);
    let name = lcc_peer::identity::clean_name(name, "");
    if name.is_empty() {
        return Err(EngineError::Other("the name is empty".into()));
    }
    let taken = s.device_sync.stored().is_some_and(|st| st.peers.iter().any(|p| p.name.trim().to_lowercase() == name.to_lowercase()));
    if taken {
        return Err(EngineError::Other(format!("another device is already called `{name}`; give this one another name")));
    }
    if let Some(lib) = &mut s.device_sync.lib {
        lib.replica.rename_me(&name, now_ms()).map_err(other)?;
        touched(lib);
    }
    let a = s.activity.clone();
    s.device_sync.ensure(&a)?.rename(&name).map_err(other)?;
    status(s)
}

// ── the open catalogue ─────────────────────────────────────────────────────────────────────

/// The open catalogue, if it is a folder on disk.
fn library_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
}

fn library_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Catalogue".into())
}

fn replica_path(dir: &Path) -> PathBuf {
    dir.join("sync").join("replica.json")
}

/// The largest replica file read (about 400 bytes a photo: room for well over a million).
const MAX_REPLICA: u64 = 1 << 30;

fn load_replica(dir: &Path, device: &str) -> Replica {
    let path = replica_path(dir);
    let read = || -> Option<Replica> {
        if std::fs::metadata(&path).ok()?.len() > MAX_REPLICA {
            return None;
        }
        serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
    };
    match read() {
        // (a catalogue copied to another device starts over there: its cursors are the other device's)
        Some(mut r) if r.device == device => {
            r.rebuild();
            r
        }
        Some(_) => Replica::new(device),
        None => {
            if path.is_file() {
                // unreadable: kept aside, and sync starts over (the catalogue itself is untouched)
                let _ = std::fs::rename(&path, path.with_extension("json.damaged"));
                log::warn!("sync: {} couldn't be read; it was set aside and sync starts over", path.display());
            }
            Replica::new(device)
        }
    }
}

fn save_replica(lib: &mut Lib) {
    lib.saved_at = Instant::now();
    if !lib.dirty {
        return;
    }
    // nothing is written for a catalogue that never synced
    if lib.replica.group.is_none() && !replica_path(&lib.dir).is_file() {
        lib.dirty = false;
        return;
    }
    let path = replica_path(&lib.dir);
    let written = serde_json::to_vec(&lib.replica).map_err(|e| e.to_string()).and_then(|bytes| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        lcc_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| e.to_string())
    });
    match written {
        Ok(()) => lib.dirty = false,
        Err(e) => log::warn!("sync: couldn't save {}: {e}", path.display()),
    }
}

/// Something changed that a device may be owed and the pills may show.
pub(crate) fn touched(lib: &mut Lib) {
    lib.dirty = true;
    lib.generation = lib.generation.wrapping_add(1);
    lib.changed_at = Some(Instant::now());
}

/// Loads the open catalogue's replica when the catalogue changed (and saves the one left).
pub(crate) fn ensure_lib(s: &mut Session) {
    let dir = library_dir(s);
    if s.device_sync.lib.as_ref().map(|l| &l.dir) == dir.as_ref() {
        return;
    }
    if let Some(mut old) = s.device_sync.lib.take() {
        save_replica(&mut old);
    }
    let (Some(dir), Some(device)) = (dir, s.device_sync.device_id()) else { return };
    let replica = load_replica(&dir, &device);
    let mut lib = Lib {
        dir,
        replica,
        revision: None,
        keys: HashMap::new(),
        ids: HashMap::new(),
        base: HashMap::new(),
        wanting: BTreeSet::new(),
        missing: HashMap::new(),
        dirty: false,
        saved_at: Instant::now(),
        generation: 1,
        changed_at: None,
        album_sig: HashMap::new(),
        unfiled_at: None,
        pills: Mutex::new((0, HashMap::new(), HashMap::new())),
    };
    catalog::index(s, &mut lib);
    s.device_sync.lib = Some(lib);
}

/// Writes the replica now (the catalogue is being closed, or the app quits).
pub(crate) fn flush(s: &mut Session) {
    if let Some(lib) = &mut s.device_sync.lib {
        save_replica(lib);
    }
}

fn group_err(lib: Option<&Lib>) -> EngineError {
    match lib {
        None => EngineError::Other("only a catalogue on disk can sync, and this device needs a paired device first".into()),
        Some(_) => other(lcc_sync::replica::Refused::NoGroup),
    }
}

/// Starts syncing the open catalogue with a paired device, or asks one more device into the
/// group it already syncs with. `mode` and `keeper` only count for a new group; with
/// `keeper` = the other device, this catalogue must be empty (it is filled from theirs).
pub(crate) fn setup(s: &mut Session, device: &str, mode: Mode, keep_mine: bool) -> Result<Value> {
    let peer = s.device_sync.paired(device).ok_or_else(|| EngineError::Other(format!("not paired with `{device}`")))?;
    let a = s.activity.clone();
    s.device_sync.ensure(&a)?;
    ensure_lib(s);
    let name = s.device_sync.device_name();
    let photos = s.catalog.photos().count();
    let Some(mut lib) = s.device_sync.lib.take() else { return Err(group_err(None)) };
    let me = lib.replica.device.clone();
    let out = (|| -> Result<()> {
        if !lib.replica.group.as_ref().is_some_and(|g| g.has(&me)) {
            if name.trim().to_lowercase() == peer.name.trim().to_lowercase() {
                return Err(other(lcc_sync::replica::Refused::NameTaken(name.clone())));
            }
            if mode == Mode::Catalogue && !keep_mine && photos > 0 {
                return Err(EngineError::Other(
                    "to take over the other device's catalogue this one must be empty: make a new catalogue first (File ▸ New Catalogue), then start the sync from it".into(),
                ));
            }
            let id = lcc_peer::identity::random_id().map_err(other)?;
            let keeper = (mode == Mode::Catalogue).then(|| if keep_mine { me.clone() } else { device.to_string() });
            lib.replica.start_group(&id, mode, keeper, &name, now_ms()).map_err(other)?;
            catalog::register_all(s, &mut lib);
        }
        if !lib.replica.in_group_with(device) {
            lib.replica.invited.insert(device.to_string());
        }
        Ok(())
    })();
    touched(&mut lib);
    lib.revision = None;
    s.device_sync.lib = Some(lib);
    out?;
    sync_now(s);
    status(s)
}

/// Joins the group a paired device invited this catalogue to, or turns the invitation down.
pub(crate) fn answer(s: &mut Session, device: &str, accept: bool) -> Result<Value> {
    let hub = s.device_sync.hub.clone().ok_or_else(|| EngineError::Other("no device asked to sync".into()))?;
    let offer = lock(&hub.offers).iter().find(|o| o.peer.device_id == device).cloned().ok_or_else(|| EngineError::Other("that device didn't ask to sync".into()))?;
    if !accept {
        lock(&hub.offers).retain(|o| o.peer.device_id != device);
        return status(s);
    }
    ensure_lib(s);
    let name = s.device_sync.device_name();
    let photos = s.catalog.photos().count();
    let Some(mut lib) = s.device_sync.lib.take() else { return Err(group_err(None)) };
    let me = lib.replica.device.clone();
    let out = (|| -> Result<()> {
        let keeper = offer.group.keeper.as_deref() == Some(me.as_str());
        if offer.group.mode == Mode::Catalogue && !keeper && photos > 0 {
            return Err(EngineError::Other(
                "this sync shares one catalogue, which starts from the other device's: make a new, empty catalogue first (File ▸ New Catalogue), then join from it".into(),
            ));
        }
        lib.replica.join_group(&offer.group, &name, now_ms()).map_err(other)?;
        catalog::register_all(s, &mut lib);
        Ok(())
    })();
    touched(&mut lib);
    lib.revision = None;
    s.device_sync.lib = Some(lib);
    out?;
    lock(&hub.offers).retain(|o| o.peer.device_id != device);
    sync_now(s);
    status(s)
}

/// Stops syncing the open catalogue: this device leaves the group. Nothing is deleted.
pub(crate) fn leave(s: &mut Session) -> Result<Value> {
    ensure_lib(s);
    let Some(lib) = &mut s.device_sync.lib else { return Err(group_err(None)) };
    let me = lib.replica.device.clone();
    lib.replica.leave(&me, now_ms()).map_err(other)?;
    touched(lib);
    sync_now(s);
    status(s)
}

/// The shared id of a collection named by its shared id or by the catalogue's album id.
fn collection_id(lib: &Lib, p: &Value) -> Option<String> {
    if let Some(id) = p.get("collection").and_then(Value::as_str) {
        return lib.replica.collection(id).map(|_| id.to_string());
    }
    let album = p.get("album").and_then(Value::as_u64)?;
    lib.replica.collection_of_album(album).map(str::to_string)
}

/// What the other devices may have of a collection this device owns (`album`: a collection of
/// the catalogue, shared from now on; `"unfiled"`: the photos in no collection).
pub(crate) fn set_grant(s: &mut Session, p: &Value, grant: Grant) -> Result<Value> {
    ensure_lib(s);
    let Some(mut lib) = s.device_sync.lib.take() else { return Err(group_err(None)) };
    let me = lib.replica.device.clone();
    let out = (|| -> Result<()> {
        if !lib.replica.group.as_ref().is_some_and(|g| g.has(&me)) {
            return Err(group_err(Some(&lib)));
        }
        let now = now_ms();
        let id = if p.get("unfiled").and_then(Value::as_bool) == Some(true) {
            let id = format!("unfiled:{me}");
            if lib.replica.collection(&id).is_none() {
                lib.replica.add_collection(&id, CollectionKind::Unfiled, "Not in a collection", None, Grant::None, None, now);
            }
            id
        } else {
            match collection_id(&lib, p) {
                Some(id) => id,
                None => {
                    let album = p.get("album").and_then(Value::as_u64).ok_or_else(|| EngineError::Other("missing `album` or `collection`".into()))?;
                    catalog::register(s, &mut lib, lcc_catalog::AlbumId(album)).map_err(EngineError::Other)?
                }
            }
        };
        if let Some(a) = lib.replica.albums.get(&id).copied() {
            lib.replica.asked.insert(a);
        }
        lib.replica.set_grant(&id, grant, now).map_err(other)?;
        Ok(())
    })();
    touched(&mut lib);
    lib.revision = None;
    s.device_sync.lib = Some(lib);
    out?;
    status(s)
}

/// What this device takes of a collection another device (or Immich) owns.
pub(crate) fn set_take(s: &mut Session, p: &Value, take: Take) -> Result<Value> {
    ensure_lib(s);
    let Some(mut lib) = s.device_sync.lib.take() else { return Err(group_err(None)) };
    let out = (|| -> Result<()> {
        let id = collection_id(&lib, p).ok_or_else(|| other(lcc_sync::replica::Refused::NoCollection))?;
        lib.replica.set_take(&id, take, now_ms()).map_err(other)?;
        Ok(())
    })();
    touched(&mut lib);
    catalog::reconcile(s, &mut lib);
    s.device_sync.lib = Some(lib);
    out?;
    sync_now(s);
    status(s)
}

/// A new collection of the catalogue stays this device's alone (the person was asked).
pub(crate) fn skip(s: &mut Session, album: u64) -> Result<Value> {
    ensure_lib(s);
    if let Some(lib) = &mut s.device_sync.lib {
        lib.replica.asked.insert(album);
        lib.dirty = true;
    }
    status(s)
}

/// Exchange with every device now, not when it is next due.
pub(crate) fn sync_now(s: &mut Session) {
    if let Some(h) = &s.device_sync.hub {
        lock(&h.tried).clear();
    }
    if let Some(lib) = &mut s.device_sync.lib {
        lib.changed_at = Some(Instant::now() - SOON);
    }
}

/// The pills of some photos (`null` for a photo that isn't shared).
pub(crate) fn pills(s: &mut Session, ids: &[u64]) -> Value {
    let map: serde_json::Map<String, Value> =
        ids.iter().take(10_000).map(|id| (id.to_string(), s.sync_pill(PhotoId(*id)).map_or(Value::Null, |p| json!({"text": p.text(), "pill": p})))).collect();
    Value::Object(map)
}

// ── the tick ───────────────────────────────────────────────────────────────────────────────

/// `sync.tick`, every second or so while sync runs: tells the replica what changed in the
/// catalogue, answers the exchange threads, and starts the exchanges that are due.
pub(crate) fn tick(s: &mut Session) -> Result<Value> {
    let (Some(svc), Some(hub)) = (s.device_sync.service.clone(), s.device_sync.hub.clone()) else { return Ok(json!({"running": false})) };
    ensure_lib(s);
    if let Some(mut lib) = s.device_sync.lib.take() {
        if catalog::scan(s, &mut lib) {
            touched(&mut lib);
        }
        s.device_sync.lib = Some(lib);
    }
    let jobs = std::mem::take(&mut *lock(&hub.jobs));
    let mut handled = 0;
    for job in jobs {
        handled += 1;
        answer_job(s, &hub, job);
    }
    let me = svc.device_id();
    let every = s.device_sync.exchange_every.unwrap_or(EXCHANGE_EVERY);
    let soon = s.device_sync.exchange_every.map_or(SOON, |e| e.min(SOON));
    let mut started = 0;
    for peer in svc.online_peers() {
        let Some(lib) = &s.device_sync.lib else { break };
        let r = &lib.replica;
        let member = r.in_group_with(&peer.device_id);
        let relevant = member || r.invited.contains(&peer.device_id) || r.group.as_ref().is_some_and(|g| g.devices.contains_key(&peer.device_id));
        if lock(&hub.running).contains(&peer.device_id) {
            continue;
        }
        // one side looks in regularly (also when its catalogue shares nothing: the other may invite it);
        // either side starts at once when it has something for the other
        let owes = relevant && lib.changed_at.is_some_and(|t| t.elapsed() >= soon) && (r.has_outgoing(&peer.device_id) || r.invited.contains(&peer.device_id) || (member && !lib.wanting.is_empty()));
        let since = lock(&hub.tried).get(&peer.device_id).map(Instant::elapsed);
        let due = match since {
            None => me < peer.device_id || owes,
            Some(t) => (me < peer.device_id && t >= every) || (owes && t >= soon.max(Duration::from_millis(500))),
        };
        if !due {
            continue;
        }
        lock(&hub.tried).insert(peer.device_id.clone(), Instant::now());
        lock(&hub.running).insert(peer.device_id.clone());
        lock(&hub.links).entry(peer.device_id.clone()).or_default().running = true;
        let (svc2, hub2) = (svc.clone(), hub.clone());
        let id = peer.device_id.clone();
        let spawned = std::thread::Builder::new().name("sync-exchange".into()).spawn(move || wire::start(&svc2, &hub2, peer));
        match spawned {
            Ok(_) => started += 1,
            Err(e) => {
                log::warn!("sync: no thread for an exchange: {e}");
                lock(&hub.running).remove(&id);
            }
        }
    }
    if let Some(lib) = &mut s.device_sync.lib {
        // nothing left to owe: the next change starts the clock again
        if lib.changed_at.is_some_and(|t| t.elapsed() > every.max(Duration::from_secs(60))) {
            lib.changed_at = None;
        }
        if lib.dirty && lib.saved_at.elapsed() >= SAVE_EVERY {
            save_replica(lib);
        }
    }
    Ok(json!({"running": true, "jobs": handled, "started": started}))
}

fn answer_job(s: &mut Session, hub: &Arc<Hub>, job: Job) {
    let name = s.device_sync.device_name();
    let catalogue = library_dir(s).map(|d| library_name(&d)).unwrap_or_default();
    match job {
        Job::Hello { peer, reply } => {
            let (group, invite) = match &s.device_sync.lib {
                Some(lib) => {
                    let r = &lib.replica;
                    let mine = r.group.as_ref().filter(|g| g.has(&r.device));
                    (mine.map(|g| g.id.clone()), mine.filter(|_| r.invited.contains(&peer) && !r.in_group_with(&peer)).cloned())
                }
                None => (None, None),
            };
            let _ = reply.send(Hello { proto: lcc_sync::proto::VERSION, app: lcc_catalog::APP_VERSION.to_string(), name, now_ms: now_ms(), group, catalogue, invite });
        }
        Job::Their { peer, hello } => {
            let mut offers = lock(&hub.offers);
            offers.retain(|o| o.peer.device_id != peer.device_id);
            let Some(lib) = &mut s.device_sync.lib else {
                if let Some(g) = hello.invite {
                    offers.push(Offer { peer, group: g, catalogue: hello.catalogue });
                }
                return;
            };
            let r = &mut lib.replica;
            let mine = r.group.as_ref().filter(|g| g.has(&r.device)).map(|g| g.id.clone());
            match hello.invite {
                Some(g) if mine.as_deref() != Some(g.id.as_str()) || !r.in_group_with(&peer.device_id) => {
                    if mine.as_deref() == Some(g.id.as_str()) {
                        // asked into the group this catalogue is already in (both asked each other): joined
                        return;
                    }
                    offers.push(Offer { peer, group: g, catalogue: hello.catalogue });
                }
                _ => {
                    // a device this catalogue lists that now syncs something else (or nothing) has left
                    if mine.is_some() && hello.group != mine && r.in_group_with(&peer.device_id) && !r.invited.contains(&peer.device_id) {
                        let _ = r.leave(&peer.device_id, now_ms());
                        touched(lib);
                    }
                }
            }
        }
        Job::Prepare { peer, reply } => {
            let out = match &s.device_sync.lib {
                Some(lib) => catalog::prepare(s, lib, &peer),
                None => (Batch::default(), Outgoing::default()),
            };
            let _ = reply.send(out);
        }
        Job::Confirm { peer, out } => {
            if let Some(lib) = &mut s.device_sync.lib {
                lib.replica.confirmed(&peer, &out);
                lib.dirty = true;
                lib.generation = lib.generation.wrapping_add(1);
            }
        }
        Job::Apply { peer, batch, reply } => {
            let Some(mut lib) = s.device_sync.lib.take() else {
                let _ = reply.send(Err("no catalogue is open".into()));
                return;
            };
            let r = catalog::apply(s, &mut lib, &peer, &batch);
            if r.is_ok() && !batch.items.is_empty() {
                lib.dirty = true;
                lib.generation = lib.generation.wrapping_add(1);
            }
            s.device_sync.lib = Some(lib);
            let _ = reply.send(r);
        }
        Job::Wants { peer, reply } => {
            let wants = match &mut s.device_sync.lib {
                Some(lib) if lib.replica.in_group_with(&peer) => {
                    let mut lib = std::mem::replace(&mut s.device_sync.lib, None);
                    let w = lib.as_mut().map(|l| catalog::wants(s, l, &peer)).unwrap_or_default();
                    s.device_sync.lib = lib;
                    w
                }
                _ => Vec::new(),
            };
            let _ = reply.send(wants);
        }
        Job::Locate { peer, wants, reply } => {
            let found = match s.device_sync.lib.take() {
                Some(mut lib) => {
                    let f = catalog::locate(s, &mut lib, &peer, &wants);
                    s.device_sync.lib = Some(lib);
                    f
                }
                None => wants.iter().map(|_| Err(false)).collect(),
            };
            let _ = reply.send(found);
        }
        Job::Got { key, depth } => {
            if let Some(lib) = &mut s.device_sync.lib {
                let have = lib.replica.photos.get(&key).map_or(Depth::None, |e| e.have).max(depth);
                lib.replica.set_have(&key, have);
                if have >= lib.replica.depth_wanted(&key) {
                    lib.wanting.remove(&key);
                }
                touched(lib);
            }
        }
        Job::Missing { peer, key } => {
            if let Some(lib) = &mut s.device_sync.lib {
                lib.missing.insert((peer, key), Instant::now());
            }
        }
    }
}
