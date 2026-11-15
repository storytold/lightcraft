//! Immich albums as device-sync collections (`docs/sync.md` ▸ Immich collections).
//!
//! For every connected account shown as collections (`immich.collections`), each device that
//! holds the account's key lists the account's albums on a worker, about once a minute
//! (`sync.immichRefresh` at once). Each album becomes a shared collection with the id
//! `immich:<user id>:<album id>` inside the set `immich:<user id>` ("Immich"): the same ids on
//! every device, so their copies merge. Which photos are in an album comes from the server on
//! each device; what a device takes decides what it brings in: link-only previews
//! (`previews`) or originals (`fullQuality`), through the ordinary Immich import. A photo added
//! to such a collection here is uploaded (unless Immich has it already, by checksum) and put
//! into the album; one taken out here is taken out of the album.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lcc_catalog::{Op, PhotoId, Source};
use lcc_immich::link::SERVICE;
use lcc_immich::{Asset, ImmichError};
use lcc_sync::model::Take;
use serde_json::{Value, json};

use crate::Session;
use crate::device_sync::{catalog, now_ms, record, touched};
use crate::remote::{LINK_ONLY, Msg};

/// How often each account's albums are listed again.
const EVERY: Duration = Duration::from_secs(60);
/// The most albums, and photos per album, taken from one listing.
const MAX_ALBUMS: usize = 2000;
const MAX_ASSETS: usize = 50_000;
/// Originals downloaded per pass for collections taken in full quality.
const FETCH_PER_PASS: usize = 4;

/// The set holding an account's albums.
pub(crate) fn set_id(user: &str) -> String {
    format!("immich:{user}")
}

pub(crate) fn collection_id(user: &str, album: &str) -> String {
    format!("immich:{user}:{album}")
}

/// One account's albums, as a worker listed them.
pub(crate) struct Listing {
    pub account: String,
    pub user: String,
    pub albums: Result<Vec<(lcc_immich::types::Album, Vec<Asset>)>, ImmichError>,
}

/// What an upload run did: links to write (photo, asset, checksum) and the first error.
pub(crate) struct Pushed {
    pub account: String,
    pub links: Vec<(PhotoId, String, String)>,
    pub error: Option<String>,
}

/// What one upload run is asked to do for one album.
struct Push {
    album: String,
    add: Vec<(PhotoId, Option<String>, PathBuf, Option<String>, String, String)>,
    remove: Vec<String>,
}

/// The session's side: running workers and when each account was last listed.
#[derive(Default)]
pub(crate) struct CollState {
    pub listing: usize,
    pub pushing: bool,
    pub at: BTreeMap<String, Instant>,
    pub force: bool,
    pub errors: BTreeMap<String, String>,
}

fn in_group(s: &Session) -> bool {
    s.device_sync.lib.as_ref().is_some_and(|l| l.replica.group.as_ref().is_some_and(|g| g.has(&l.replica.device)))
}

/// The accounts shown as collections here: `(local id, user id)`.
fn shown(s: &mut Session) -> Vec<(String, String)> {
    s.immich_accounts().map(|a| a.immich.iter().filter(|x| x.collections && !x.user_id.is_empty()).map(|x| (x.id.clone(), x.user_id.clone())).collect()).unwrap_or_default()
}

/// Every frame from `remote.pump`: starts the listings that are due and the uploads waiting.
pub(crate) fn tick(s: &mut Session) {
    if !in_group(s) {
        return;
    }
    take_shared(s);
    offer_shared(s);
    let accounts = shown(s);
    if let Some(lib) = &mut s.device_sync.lib {
        let here: BTreeSet<String> = accounts.iter().map(|(_, u)| u.clone()).collect();
        if lib.immich_here != here {
            lib.immich_here = here;
            lib.generation = lib.generation.wrapping_add(1);
        }
    }
    let force = std::mem::take(&mut s.remote.coll.force);
    if s.remote.coll.listing == 0 {
        for (account, user) in &accounts {
            let due = force || s.remote.coll.at.get(account).is_none_or(|t| t.elapsed() >= EVERY);
            if !due {
                continue;
            }
            s.remote.coll.at.insert(account.clone(), Instant::now());
            let Ok((_, client)) = s.immich_client(account) else { continue };
            s.remote.coll.listing += 1;
            let task = s.activity.start("immich", "Listing Immich albums", crate::activity::Cancel::Yes);
            let tx = s.remote.sender();
            let (account, user) = (account.clone(), user.clone());
            std::thread::spawn(move || {
                let albums = list(&client, &task);
                drop(task);
                let _ = tx.send(Msg::ImmichAlbums(Box::new(Listing { account, user, albums })));
            });
        }
    }
    if !s.remote.coll.pushing {
        start_push(s, &accounts);
    }
}

fn list(client: &lcc_immich::Client, task: &crate::activity::TaskGuard) -> Result<Vec<(lcc_immich::types::Album, Vec<Asset>)>, ImmichError> {
    let mut out = Vec::new();
    let albums = client.albums()?;
    let total = albums.len().min(MAX_ALBUMS) as u64;
    for (n, a) in albums.into_iter().take(MAX_ALBUMS).enumerate() {
        if task.is_cancelled() {
            return Err(ImmichError::Cancelled);
        }
        task.progress(n as u64, total);
        let q = lcc_immich::types::MetadataSearch { size: Some(1000), album_ids: Some(vec![a.id.clone()]), ..Default::default() };
        let mut assets = Vec::new();
        client.search_all(&q, (MAX_ASSETS / 1000) as u32, |page| {
            assets.extend(page.iter().filter(|x| !x.is_trashed).cloned());
            assets.len() < MAX_ASSETS
        })?;
        out.push((a, assets));
    }
    Ok(out)
}

/// The key a listed asset has in device sync: its photo's key when it is linked here, else the
/// SHA-1 Immich reports.
fn asset_key(s: &Session, account: &str, a: &Asset) -> Option<String> {
    match s.catalog.photo_of_remote(SERVICE, account, &a.id).and_then(|id| s.catalog.photo(id)) {
        Some(p) => record::key_of(p, &s.catalog),
        None => record::sha1_key(&a.checksum),
    }
}

/// A listing came back: the collections and which photos are in them, then what this device
/// takes of each is brought in.
pub(crate) fn finish_listing(s: &mut Session, l: Listing) {
    s.remote.coll.listing = s.remote.coll.listing.saturating_sub(1);
    let albums = match l.albums {
        Ok(a) => {
            s.remote.coll.errors.remove(&l.account);
            a
        }
        Err(e) => {
            s.remote.coll.errors.insert(l.account.clone(), e.to_string());
            return;
        }
    };
    if !in_group(s) {
        return;
    }
    let keyed: Vec<(String, String, Vec<(String, Asset)>)> = albums
        .into_iter()
        .map(|(a, assets)| {
            let keyed = assets.into_iter().filter_map(|x| asset_key(s, &l.account, &x).map(|k| (k, x))).collect();
            (a.id, a.album_name, keyed)
        })
        .collect();
    let Some(mut lib) = s.device_sync.lib.take() else { return };
    let now = now_ms();
    let me = lib.replica.device.clone();
    let set = set_id(&l.user);
    lib.replica.immich_collection(&set, &l.user, None, "Immich", None, now);
    let mut link: Vec<String> = Vec::new();
    let mut copy: Vec<String> = Vec::new();
    let mut fetch: Vec<u64> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (album, name, assets) in &keyed {
        let id = collection_id(&l.user, album);
        if !lib.replica.immich_collection(&id, &l.user, Some(album), name, Some(set.clone()), now) && lib.replica.collection(&id).is_none() {
            continue;
        }
        seen.insert(id.clone());
        // an album that was removed on the server and is back
        let _ = lib.replica.set_immich_gone(&id, false, now);
        let keys: BTreeSet<&String> = assets.iter().map(|(k, _)| k).collect();
        // (what this device is still to send to the album isn't taken out again meanwhile)
        let waiting: BTreeSet<String> = lib.immich_out.iter().filter(|(c, _, _)| *c == id).map(|(_, k, _)| k.clone()).collect();
        for k in &keys {
            if !waiting.contains(*k) {
                lib.replica.set_member(&id, k, true, now);
            }
        }
        let gone: Vec<String> = lib.replica.members_of(&id).filter(|k| !keys.contains(k) && !waiting.contains(*k)).cloned().collect();
        for k in gone {
            lib.replica.set_member(&id, &k, false, now);
        }
        let take = lib.replica.collection(&id).map_or(Take::None, |c| c.take_of(&me));
        if take == Take::None {
            continue;
        }
        for (k, a) in assets {
            match lib.keys.get(k).and_then(|p| s.catalog.photo(*p)) {
                None if take == Take::FullQuality => copy.push(a.id.clone()),
                None => link.push(a.id.clone()),
                Some(p) if take == Take::FullQuality && p.preview_only.as_deref() == Some(LINK_ONLY) && fetch.len() < FETCH_PER_PASS => {
                    fetch.push(p.id.0);
                }
                Some(_) => {}
            }
        }
    }
    // albums gone from the server
    let stale: Vec<String> = lib
        .replica
        .collections
        .iter()
        .filter(|(id, e)| e.c.immich.as_ref().is_some_and(|r| r.account == l.user && !r.album.is_empty()) && !seen.contains(*id) && !e.c.gone())
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        let _ = lib.replica.set_immich_gone(&id, true, now);
    }
    touched(&mut lib);
    catalog::reconcile(s, &mut lib);
    s.device_sync.lib = Some(lib);
    link.sort();
    link.dedup();
    copy.sort();
    copy.dedup();
    if !s.remote.import.active {
        let r = if !link.is_empty() {
            Some(s.execute("immich.import", &json!({"account": l.account, "assets": link, "mode": "link"})))
        } else if !copy.is_empty() {
            let dest = s.immich_dir().map(|d| d.join("originals").to_string_lossy().to_string());
            dest.map(|d| s.execute("immich.import", &json!({"account": l.account, "assets": copy, "mode": "copy", "destination": d})))
        } else {
            None
        };
        if let Some(Err(e)) = r {
            log::warn!("Immich collections: {e}");
        }
    }
    for id in fetch {
        if let Err(e) = s.execute("immich.fetchOriginal", &json!({"ids": [id]})) {
            log::info!("Immich collections: {e}");
        }
    }
}

/// Uploads (or finds by checksum) the photos added here to Immich collections, and takes out of
/// the albums the photos taken out here.
fn start_push(s: &mut Session, accounts: &[(String, String)]) {
    let Some(lib) = &mut s.device_sync.lib else { return };
    if lib.immich_out.is_empty() {
        return;
    }
    let device = lib.replica.device.clone();
    let out = std::mem::take(&mut lib.immich_out);
    let mut jobs: BTreeMap<String, Vec<Push>> = BTreeMap::new();
    let mut by_album: BTreeMap<(String, String), Push> = BTreeMap::new();
    for (coll, key, on) in out {
        let Some(r) = lib.replica.collection(&coll).and_then(|c| c.immich.clone()) else { continue };
        let Some((account, _)) = accounts.iter().find(|(_, u)| *u == r.account) else { continue };
        let Some(p) = lib.keys.get(&key).and_then(|id| s.catalog.photo(*id)) else { continue };
        let linked = s.catalog.remote_of(p.id).find(|x| x.service == SERVICE && x.account_id == *account).map(|x| x.remote_id.clone());
        let push = by_album.entry((account.clone(), r.album.clone())).or_insert_with(|| Push { album: r.album.clone(), add: Vec::new(), remove: Vec::new() });
        if on {
            let path = match &p.source {
                Source::File { path } if p.preview_only.is_none() => PathBuf::from(path),
                // (a link-only photo is on the server already: it is only put into the album)
                _ if linked.is_some() => PathBuf::new(),
                _ => continue,
            };
            let created = p.captured.clone().unwrap_or_else(|| p.imported.clone());
            push.add.push((p.id, linked, path, p.sha1.clone(), p.file_name.clone(), created));
        } else if let Some(a) = linked {
            push.remove.push(a);
        }
    }
    for ((account, _), push) in by_album {
        jobs.entry(account).or_default().push(push);
    }
    let mut runs = Vec::new();
    for (account, pushes) in jobs {
        let Ok((_, client)) = s.immich_client(&account) else { continue };
        runs.push((account, client, pushes));
    }
    if runs.is_empty() {
        return;
    }
    s.remote.coll.pushing = true;
    let task = s.activity.start("immich", "Adding photos to Immich albums", crate::activity::Cancel::Yes);
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        for (account, client, pushes) in runs {
            let (links, error) = push(&client, &device, pushes, &task);
            let _ = tx.send(Msg::ImmichPushed(Box::new(Pushed { account, links, error })));
        }
        drop(task);
        let _ = tx.send(Msg::ImmichPushDone);
    });
}

fn push(client: &lcc_immich::Client, device: &str, pushes: Vec<Push>, task: &crate::activity::TaskGuard) -> (Vec<(PhotoId, String, String)>, Option<String>) {
    let mut links = Vec::new();
    let mut error = None;
    for p in pushes {
        let mut ids = Vec::new();
        for (photo, linked, path, sha1, name, created) in p.add {
            if task.is_cancelled() {
                return (links, error.or(Some("stopped".into())));
            }
            if let Some(a) = linked {
                ids.push(a);
                continue;
            }
            // Immich may have it already (the phone's own upload, another device's): linked, not stored twice
            let found = sha1.as_ref().and_then(|h| {
                client
                    .upload_check(&[lcc_immich::types::UploadCheck { id: photo.0.to_string(), checksum: h.clone() }])
                    .ok()?
                    .into_iter()
                    .find_map(|r| r.asset_id.filter(|a| !a.is_empty()))
            });
            let asset = match found {
                Some(a) => Ok(a),
                None => client
                    .upload(&lcc_immich::client::NewAsset {
                        data: lcc_immich::client::UploadData::File(&path),
                        file_name: &name,
                        mime: lcc_immich::publish::mime_of(&name),
                        device_asset_id: &format!("{device}-{}", photo.0),
                        device_id: device,
                        created: &created,
                        favorite: false,
                        sidecar: None,
                    })
                    .map(|(id, _)| id),
            };
            match asset {
                Ok(a) => {
                    links.push((photo, a.clone(), sha1.unwrap_or_default()));
                    ids.push(a);
                }
                Err(e) => {
                    error.get_or_insert(e.to_string());
                }
            }
        }
        if !ids.is_empty()
            && let Err(e) = client.album_add_checked(&p.album, &ids)
        {
            error.get_or_insert(e.to_string());
        }
        if !p.remove.is_empty()
            && let Err(e) = client.album_remove(&p.album, &p.remove)
        {
            error.get_or_insert(e.to_string());
        }
    }
    (links, error)
}

/// An upload run came back: the photos are linked to their assets.
pub(crate) fn finish_push(s: &mut Session, p: Pushed) {
    let now = (s.clock)();
    let ops: Vec<Op> = p
        .links
        .into_iter()
        .filter(|(photo, _, _)| s.catalog.photo(*photo).is_some() && !s.catalog.remote_of(*photo).any(|r| r.service == SERVICE && r.account_id == p.account))
        .map(|(photo, asset, sha1)| {
            lcc_catalog::Catalog::link_remote_op(lcc_catalog::RemoteIdentity {
                photo_id: photo,
                service: SERVICE.into(),
                account_id: p.account.clone(),
                remote_id: asset,
                remote_checksum: (!sha1.is_empty()).then_some(sha1),
                remote_updated_at: None,
                last_synced_at: Some(now.clone()),
                sync_state: lcc_catalog::SyncState::Synced,
            })
        })
        .collect();
    if !ops.is_empty()
        && let Err(e) = s.apply_system(Op::Batch { ops })
    {
        log::warn!("Immich collections: {e}");
    }
    match p.error {
        Some(e) => s.remote.coll.errors.insert(p.account, e),
        None => s.remote.coll.errors.remove(&p.account),
    };
}

/// `immich.collections {account?, on}`: show (or stop showing) the account's albums as
/// collections in device sync.
pub(crate) fn set_shown(s: &mut Session, p: &Value) -> crate::Result<Value> {
    const C: &str = "immich.collections";
    let id = super::account_param(s, p, C)?;
    let on = p.get("on").and_then(Value::as_bool).ok_or_else(|| super::bad(C, "missing `on` (true or false)"))?;
    let accounts = s.immich_accounts().map_err(|e| super::bad(C, e))?;
    let acc = accounts.get_mut(&id).ok_or_else(|| super::bad(C, "no such account"))?;
    acc.collections = on;
    s.save_accounts().map_err(|e| super::bad(C, e))?;
    if on {
        s.remote.coll.force = true;
    }
    Ok(json!({"account": id, "collections": on}))
}

/// The private connections (not shared) are kept from the other devices; the shared ones are
/// read with their keys once, for the next exchange with each device.
fn offer_shared(s: &mut Session) {
    let Ok(accs) = s.immich_accounts().map(|a| a.immich.clone()) else { return };
    let private: BTreeSet<String> = accs.iter().filter(|a| !a.shared && !a.user_id.is_empty()).map(|a| a.user_id.clone()).collect();
    let users: Vec<String> = accs.iter().filter(|a| a.shared && !a.user_id.is_empty()).map(|a| a.user_id.clone()).collect();
    let Some(lib) = &mut s.device_sync.lib else { return };
    if lib.replica.private_immich != private {
        lib.replica.private_immich = private;
        touched(lib);
    }
    if lib.share_out.as_ref().is_some_and(|(u, _)| *u == users) {
        return;
    }
    let mut out = Vec::new();
    if let Ok(store) = s.secret_store() {
        for a in accs.iter().filter(|a| a.shared && !a.user_id.is_empty()) {
            if let Ok(Some(key)) = store.get(&lcc_immich::accounts::secret_key(&a.id)) {
                out.push(lcc_sync::proto::SharedAccount {
                    url: a.url.clone(),
                    user_id: a.user_id.clone(),
                    user_name: a.user_name.clone(),
                    key: key.expose().to_string(),
                    pinned: a.pinned.clone(),
                });
            }
        }
    }
    if let Some(lib) = &mut s.device_sync.lib {
        lib.share_out = Some((users, out));
        // (a newly shared connection goes to every device again)
        lib.share_sent.clear();
    }
}

/// Connections another device of the group shared: the key goes into this device's key store
/// (never the replica, settings, logs or results), the account into its settings, shared and
/// shown as collections. An account this device already has (same Immich user) is left alone.
fn take_shared(s: &mut Session) {
    let incoming = match &mut s.device_sync.lib {
        Some(lib) if !lib.share_in.is_empty() => std::mem::take(&mut lib.share_in),
        _ => return,
    };
    for a in incoming {
        let Ok(url) = lcc_immich::client::normalize_url(&a.url) else { continue };
        let have = s.immich_accounts().is_ok_and(|x| x.immich.iter().any(|y| y.user_id == a.user_id));
        if have {
            continue;
        }
        let id = lcc_immich::Account::make_id(&url, &a.user_id);
        let stored = s.secret_store().and_then(|st| {
            st.set(&lcc_immich::accounts::secret_key(&id), &lcc_credentials::Secret::new(a.key.clone())).map_err(|e| e.to_string())
        });
        if let Err(e) = stored {
            log::warn!("sync: a shared Immich connection couldn't be stored: {e}");
            continue;
        }
        let acc = lcc_immich::Account {
            id,
            url,
            user_id: a.user_id.clone(),
            user_name: a.user_name.chars().take(200).collect(),
            pinned: a.pinned.clone(),
            collections: true,
            shared: true,
            ..Default::default()
        };
        if let Ok(x) = s.immich_accounts() {
            x.upsert(acc);
        }
        if let Err(e) = s.save_accounts() {
            log::warn!("sync: {e}");
        }
        s.remote.coll.force = true;
    }
}

/// `immich.share {account?, on}`: share the connection (its key included) with the devices this
/// catalog syncs with, or keep it on this device (its collections are then not told).
pub(crate) fn set_shared(s: &mut Session, p: &Value) -> crate::Result<Value> {
    const C: &str = "immich.share";
    let id = super::account_param(s, p, C)?;
    let on = p.get("on").and_then(Value::as_bool).ok_or_else(|| super::bad(C, "missing `on` (true or false)"))?;
    let accounts = s.immich_accounts().map_err(|e| super::bad(C, e))?;
    let acc = accounts.get_mut(&id).ok_or_else(|| super::bad(C, "no such account"))?;
    acc.shared = on;
    s.save_accounts().map_err(|e| super::bad(C, e))?;
    if let Some(lib) = &mut s.device_sync.lib {
        lib.share_out = None;
    }
    offer_shared(s);
    Ok(json!({"account": id, "shared": on}))
}

/// `sync.immichRefresh`: list the Immich albums shown as collections now.
pub(crate) fn refresh(s: &mut Session) -> Value {
    s.remote.coll.force = true;
    tick(s);
    json!({"listing": s.remote.coll.listing, "errors": s.remote.coll.errors})
}
