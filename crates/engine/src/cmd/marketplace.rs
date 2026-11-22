//! The plug-in marketplace behind engine commands: the index is fetched as it is now (no version
//! is pinned in the app), listed and searched, and an entry's module is downloaded, checked
//! against the entry and installed through `plugin.install`'s path. See docs/plugins.md →
//! Marketplace.
//!
//! What is kept ([`lcc_plugins::marketplace::Store`], `marketplace.json` in the plug-ins folder):
//! the user's choices (community entries shown, automatic updates) and the last index fetched.
//!
//! The network is only touched by [`refresh`] and [`download`], which hold no lock while they
//! wait, so the UI runs them on a worker; [`install_downloaded`] needs the session and runs where
//! commands do.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lcc_plugins::marketplace::{Entry, Kind, MAX_INDEX_BYTES, Store, Tier, Waiting, newer, platform, sha256_hex};
use lcc_plugins::{Manager, SignerAnswer};
use serde_json::{Value, json};

use super::plugins::{Loaded, install_module, manager, signer_answer};
use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// The marketplace's store, opened on first use beside the plug-ins (in memory when they are).
fn store() -> MutexGuard<'static, Option<Store>> {
    // the manager's lock is taken and dropped first: the two are never held together
    let dir = {
        let g = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_some() {
            return g;
        }
        drop(g);
        manager().as_ref().and_then(|m| m.dir().map(std::path::Path::to_path_buf))
    };
    let mut g = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    if g.is_none() {
        *g = Some(match dir {
            Some(d) => {
                let (s, why) = Store::open(&d);
                if let Some(why) = why {
                    log::warn!("marketplace: the saved state can't be read and starts empty: {why}");
                }
                s
            }
            None => Store::in_memory(),
        });
    }
    g
}

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = store();
    f(g.get_or_insert_with(Store::in_memory))
}

/// Forgets everything the marketplace keeps, for a test that starts from nothing.
pub fn reset_for_test() {
    *STORE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Store::in_memory());
}

/// The index's address: `<PREFIX>_MARKETPLACE` for this run, else `brand.toml`'s. `None`: this
/// build has no marketplace.
pub fn index_url() -> Option<String> {
    lcc_brand::env("MARKETPLACE")
        .or_else(|| Some(lcc_brand::PLUGINS_MARKETPLACE_INDEX.to_string()))
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

const NONE: &str = "this build has no plug-in marketplace";

/// What is installed under each id: its version, and whether a pixel filter holds it.
fn installed() -> BTreeMap<String, (String, Kind)> {
    let mut out = BTreeMap::new();
    if let Some(m) = manager().as_ref() {
        for p in m.list() {
            let man = p.plugin.manifest();
            out.insert(man.id.clone(), (man.version.clone(), Kind::Plugin));
        }
    }
    for f in crate::filter_plugins::registry().list() {
        out.insert(f.id().to_string(), (f.manifest().version.clone(), Kind::Filter));
    }
    out
}

fn features() -> Vec<String> {
    manager().as_ref().map(|m| m.features().to_vec()).unwrap_or_default()
}

/// Fetches the index now and keeps it (or why it couldn't be read: the last good one stays).
/// Waits for the network, holding no lock: the UI runs it on a worker.
pub fn refresh() -> std::result::Result<(), String> {
    let url = index_url().ok_or(NONE)?;
    let fetcher = {
        let mut g = manager();
        g.get_or_insert_with(Manager::in_memory).fetcher()
    };
    let fetched = fetcher.get(&url, MAX_INDEX_BYTES);
    let mut log = Vec::new();
    let r = with_store(|s| {
        s.record_fetch(chrono::Utc::now().timestamp(), fetched, &mut log);
        if let Err(e) = s.save() {
            log::warn!("marketplace: {e}");
        }
        s.saved.error.clone()
    });
    for line in log {
        log::warn!("{line}");
    }
    r.map_or(Ok(()), Err)
}

/// One entry as `marketplace.list` gives it: the index's fields, and how it stands here.
fn entry_info(e: &Entry, installed: &BTreeMap<String, (String, Kind)>, features: &[String], auto: bool, waiting: Option<&Waiting>) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    let have = installed.get(&e.id);
    let missing = e.missing(features);
    let supported = e.supports(platform());
    if let Some(o) = v.as_object_mut() {
        o.insert("installed".into(), json!(have.map(|(version, _)| version)));
        // an id the other kind of module holds here can't be installed over
        o.insert("conflict".into(), json!(have.is_some_and(|(_, kind)| *kind != e.kind)));
        o.insert("update".into(), json!(have.is_some_and(|(version, kind)| *kind == e.kind && newer(&e.version, version))));
        o.insert("supported".into(), json!(supported));
        o.insert("installable".into(), json!(supported && missing.is_empty()));
        o.insert("missing".into(), json!(missing));
        o.insert("autoUpdate".into(), json!(auto));
        o.insert("waiting".into(), json!(waiting.filter(|w| w.version == e.version)));
    }
    v
}

fn list(_: &mut Session, p: &Value) -> Result<Value> {
    Ok(listing(p))
}

/// `marketplace.list`: the last index fetched, filtered. Fetches nothing.
pub fn listing(p: &Value) -> Value {
    let (installed, features) = (installed(), features());
    let query = str_param(p, "query").unwrap_or("");
    let only_installed = bool_or(p, "installed", false);
    let only_updates = bool_or(p, "updates", false);
    with_store(|s| {
        let saved = &s.saved;
        let community = p.get("community").and_then(Value::as_bool).unwrap_or(saved.community);
        let all: &[Entry] = saved.index.as_ref().map(|i| i.entries.as_slice()).unwrap_or_default();
        let mut hidden = 0usize;
        let mut updates = 0usize;
        let mut entries = Vec::new();
        for e in all {
            let have = installed.contains_key(&e.id);
            let info = entry_info(e, &installed, &features, saved.auto_updates(&e.id), saved.waiting.get(&e.id));
            let update = info["update"] == json!(true);
            updates = updates.saturating_add(usize::from(update));
            // what is installed is always listed: it can be updated and removed whatever the tier shown
            if e.tier == Tier::Community && !community && !have {
                hidden = hidden.saturating_add(usize::from(e.matches(query)));
                continue;
            }
            if !e.matches(query) || (only_installed && !have) || (only_updates && !update) {
                continue;
            }
            entries.push(info);
        }
        json!({
            "available": index_url().is_some(), "fetchedAt": saved.fetched_at, "generated": saved.index.as_ref().map(|i| i.generated.as_str()),
            "error": saved.error, "community": saved.community, "autoUpdate": saved.auto_update, "platform": platform(),
            "source": saved.index.as_ref().and_then(|i| i.source.clone()).or_else(|| Some(lcc_brand::PLUGINS_MARKETPLACE.to_string()).filter(|s| !s.is_empty())),
            "entries": entries, "hiddenCommunity": hidden, "updates": updates, "total": all.len(),
        })
    })
}

fn refresh_cmd(_: &mut Session, p: &Value) -> Result<Value> {
    refresh().map_err(|e| bad("marketplace.refresh", e))?;
    Ok(listing(p))
}

fn set_community(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "marketplace.setCommunity";
    let Some(on) = p.get("on").and_then(Value::as_bool) else { return Err(bad(C, "missing `on`: true or false")) };
    with_store(|s| {
        s.saved.community = on;
        s.save()
    })
    .map_err(|e| bad(C, e))?;
    Ok(json!({"community": on}))
}

fn set_auto_update(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "marketplace.setAutoUpdate";
    let Some(on) = p.get("on").and_then(Value::as_bool) else { return Err(bad(C, "missing `on`: true or false")) };
    let id = str_param(p, "id");
    with_store(|s| {
        match id {
            // one plug-in's own setting, kept only while it differs from the setting for all
            Some(id) if on == s.saved.auto_update => {
                s.saved.auto_update_for.remove(id);
            }
            Some(id) => {
                if s.saved.auto_update_for.len() >= lcc_plugins::marketplace::MAX_ENTRIES && !s.saved.auto_update_for.contains_key(id) {
                    return Err("too many plug-ins have a setting of their own".to_string());
                }
                s.saved.auto_update_for.insert(id.to_string(), on);
            }
            None => {
                s.saved.auto_update = on;
                s.saved.auto_update_for.clear();
            }
        }
        s.save()
    })
    .map_err(|e| bad(C, e))?;
    Ok(json!({"autoUpdate": with_store(|s| s.saved.auto_update), "id": id, "on": on}))
}

/// An entry's module, downloaded and checked against the entry's SHA-256.
#[derive(Clone, Debug)]
pub struct Download {
    pub entry: Entry,
    pub bytes: Vec<u8>,
}

/// Downloads entry `id`'s module (the first half of an install or update), holding no lock while
/// it waits. Refused before anything is fetched: an id the index doesn't list, a platform it
/// doesn't support, host features this version lacks. Refused after: a file that isn't the one
/// the index names (its SHA-256).
pub fn download(id: &str) -> std::result::Result<Download, String> {
    index_url().ok_or(NONE)?;
    let entry = with_store(|s| s.saved.index.as_ref().and_then(|i| i.get(id)).cloned())
        .ok_or_else(|| format!("the marketplace doesn't list {id:?} (marketplace.refresh fetches the list again)"))?;
    if !entry.supports(platform()) {
        return Err(format!("{} isn't available on this system ({})", entry.name, entry.platforms.join(", ")));
    }
    let (fetcher, max, features) = {
        let mut g = manager();
        let m = g.get_or_insert_with(Manager::in_memory);
        (m.fetcher(), m.limits().max_module_bytes as u64, m.features().to_vec())
    };
    let missing = entry.missing(&features);
    if !missing.is_empty() {
        return Err(format!("{} needs {}, which this version of the app doesn't have: update the app", entry.name, missing.join(", ")));
    }
    let bytes = fetcher.get(&entry.url, max).map_err(|e| format!("the download failed: {e}"))?;
    let got = sha256_hex(&bytes);
    if got != entry.sha256 {
        return Err(format!(
            "the downloaded file isn't the one the marketplace lists (SHA-256 {}…, listed {}…): nothing was installed. Try again in an hour, when the list has been rebuilt",
            got.get(..12).unwrap_or(&got),
            entry.sha256.get(..12).unwrap_or(&entry.sha256)
        ));
    }
    Ok(Download { entry, bytes })
}

/// Who answers for an install.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallAs {
    /// The user's own click in the marketplace window (an update then keeps the programs they
    /// allowed, as an install from the Plug-in Manager does).
    pub by_user: bool,
    /// The automatic pass: installs only what asks for nothing the installed version didn't.
    pub auto: bool,
    /// The answer to a file that isn't signed by the key pinned for its plug-in.
    pub signer: SignerAnswer,
    /// Yes to whoever signed this file, or to nobody having (the window's Install Anyway, which
    /// showed both signers).
    pub any_signer: bool,
}

/// The author signature of plug-in module `bytes`, and whether it is what's pinned for `id`.
fn signer_state(bytes: &[u8], id: &str) -> std::result::Result<(lcc_plugins::Signature, bool, Option<String>), String> {
    let inspected = lcc_plugins::Plugin::inspect(bytes, lcc_plugins::Limits::default(), false).map_err(|e| e.to_string())?;
    let g = manager();
    let pin = g.as_ref().and_then(|m| m.pin(id));
    let matches = match &pin {
        None | Some(lcc_plugins::Pin::None) => true,
        Some(lcc_plugins::Pin::Key(k)) => inspected.signature.signer() == Some(*k),
        Some(lcc_plugins::Pin::Unreadable(_)) => false,
    };
    let pinned = pin.and_then(|p| p.signer()).map(|s| s.fingerprint());
    Ok((inspected.signature, matches, pinned))
}

/// Installs a downloaded module (the second half of an install or update), as `plugin.install`
/// does: nothing is granted, an update keeps what the user allowed. The file must be the plug-in,
/// kind and version its entry names. → `{installed: true, plugin, from: the version replaced |
/// null}`, or, when the file isn't signed by the key pinned for the installed plug-in and `how`
/// doesn't say yes, `{installed: false, needs: "signer", reason, signer, pinned}` with nothing
/// installed. With `how.auto`, an update that asks for anything the installed version didn't, or
/// isn't signed as it is, is an error (the automatic pass leaves it for the user).
pub fn install_downloaded(s: &mut Session, d: &Download, how: &InstallAs) -> Result<Value> {
    const C: &str = "marketplace.install";
    let e = &d.entry;
    let is_filter = lcc_filters::is_filter_module(&d.bytes);
    if is_filter != (e.kind == Kind::Filter) {
        return Err(bad(
            C,
            format!(
                "{} is listed as a {} but the file is a {}",
                e.name,
                kind_name(e.kind),
                kind_name(if is_filter { Kind::Filter } else { Kind::Plugin })
            ),
        ));
    }
    let from = installed().get(&e.id).map(|(version, _)| version.clone());
    let mut signer = how.signer.clone();
    let mut by_user = how.by_user;
    if !is_filter {
        let (signature, matches, pinned) = signer_state(&d.bytes, &e.id).map_err(|e| bad(C, e))?;
        if let lcc_plugins::Signature::Invalid(why) = &signature {
            return Err(bad(C, format!("the file's author signature doesn't hold up ({why}): nothing was installed")));
        }
        if !matches && signer == SignerAnswer::None {
            if how.any_signer && !how.auto {
                signer = signature.signer().map_or(SignerAnswer::Unsigned, |s| SignerAnswer::Signer(s.to_base64()));
            } else {
                let file = signature.signer().map(|s| s.fingerprint());
                let reason = format!(
                    "{} {} is {}, but the installed version is signed by {}: install it only if you expect its author to have changed keys",
                    e.name,
                    e.version,
                    file.as_ref().map_or("not signed".to_string(), |f| format!("signed by {f}")),
                    pinned.as_deref().unwrap_or("a key that can't be read"),
                );
                if how.auto {
                    return Err(bad(C, reason));
                }
                return Ok(json!({"installed": false, "needs": "signer", "reason": reason, "signer": file, "pinned": pinned}));
            }
        }
        // the automatic pass keeps the programs the user allowed only for an author it can tell
        // is the same one: a file signed by the pinned key
        if how.auto {
            by_user = matches && signature.signer().is_some() && from.is_some();
        }
    }
    let auto = how.auto;
    let check = |loaded: Loaded<'_>| -> std::result::Result<(), String> {
        let (id, version) = match &loaded {
            Loaded::Plugin(_, p) => (p.manifest().id.as_str(), p.manifest().version.as_str()),
            Loaded::Filter(f) => (f.id(), f.manifest().version.as_str()),
        };
        if id != e.id || version != e.version {
            return Err(format!("the file is {id} {version}, not {} {} as the marketplace lists: nothing was installed", e.id, e.version));
        }
        if let (true, Loaded::Plugin(m, p)) = (auto, &loaded) {
            let Some(old) = m.get(id) else { return Err("it isn't installed".into()) };
            if let Some(more) = p.manifest().permissions.more_than(&old.plugin.manifest().permissions) {
                return Err(format!("version {version} asks for more than the installed version does ({more})"));
            }
        }
        Ok(())
    };
    let plugin = install_module(s, C, &d.bytes, None, false, by_user, &signer, &check)?;
    with_store(|st| {
        if st.saved.waiting.remove(&e.id).is_some()
            && let Err(e) = st.save()
        {
            log::warn!("marketplace: {e}");
        }
    });
    Ok(json!({"installed": true, "plugin": plugin, "from": from}))
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Plugin => "plug-in",
        Kind::Filter => "pixel filter",
    }
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "marketplace.install";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let signer = signer_answer(p, C)?;
    let d = download(id).map_err(|e| bad(C, e))?;
    install_downloaded(s, &d, &InstallAs { signer, ..InstallAs::default() })
}

/// The ids of installed plug-ins the index offers a newer version of that can be installed here;
/// with `auto`, only those set to update by themselves that the automatic pass hasn't already
/// left for the user at that version.
pub fn update_candidates(auto: bool) -> Vec<String> {
    let (installed, features) = (installed(), features());
    with_store(|s| {
        let all: &[Entry] = s.saved.index.as_ref().map(|i| i.entries.as_slice()).unwrap_or_default();
        all.iter()
            .filter(|e| installed.get(&e.id).is_some_and(|(version, kind)| *kind == e.kind && newer(&e.version, version)))
            .filter(|e| e.supports(platform()) && e.missing(&features).is_empty())
            .filter(|e| !auto || (s.saved.auto_updates(&e.id) && s.saved.waiting.get(&e.id).is_none_or(|w| w.version != e.version)))
            .map(|e| e.id.clone())
            .collect()
    })
}

/// Whether any installed plug-in is set to update by itself: only then does the app fetch the
/// index without the marketplace being open.
pub fn auto_update_wanted() -> bool {
    if index_url().is_none() {
        return false;
    }
    let installed = installed();
    with_store(|s| installed.keys().any(|id| s.saved.auto_updates(id)))
}

/// Whether the automatic pass should fetch the index again now (a day after the last fetch).
pub fn refresh_due() -> bool {
    with_store(|s| s.saved.refresh_due(chrono::Utc::now().timestamp()))
}

/// Records that the automatic pass left entry `e`'s update for the user, and why.
pub fn set_waiting(e: &Entry, why: &str) {
    with_store(|s| {
        if s.saved.waiting.len() < lcc_plugins::marketplace::MAX_ENTRIES || s.saved.waiting.contains_key(&e.id) {
            s.saved.waiting.insert(e.id.clone(), Waiting { version: e.version.clone(), why: lcc_plugins::marketplace::plain(why, 400) });
        }
        if let Err(e) = s.save() {
            log::warn!("marketplace: {e}");
        }
    });
}

/// Installs the downloads of an update pass → `{updated: [{id, name, version, from}], waiting:
/// [{id, name, version, why}]}`. With `auto`, an update that can't install unasked waits for the
/// user ([`set_waiting`]) and isn't tried again until the index offers another version.
pub fn install_updates(s: &mut Session, downloads: &[std::result::Result<Download, (String, String)>], auto: bool) -> Value {
    let (mut updated, mut waiting) = (Vec::new(), Vec::new());
    for d in downloads {
        match d {
            Ok(d) => {
                let e = &d.entry;
                let how = InstallAs { auto, ..InstallAs::default() };
                let r = install_downloaded(s, d, &how).map_err(|e| e.to_string()).and_then(|v| match v["installed"] == json!(true) {
                    true => Ok(v),
                    false => Err(v["reason"].as_str().unwrap_or("it needs your answer").to_string()),
                });
                match r {
                    Ok(v) => updated.push(json!({"id": e.id, "name": e.name, "version": e.version, "from": v["from"]})),
                    Err(why) => {
                        if auto {
                            set_waiting(e, &why);
                        }
                        waiting.push(json!({"id": e.id, "name": e.name, "version": e.version, "why": why}));
                    }
                }
            }
            // a download that failed is tried again next time: nothing about it needs the user
            Err((id, why)) => waiting.push(json!({"id": id, "why": why})),
        }
    }
    json!({"updated": updated, "waiting": waiting})
}

/// Downloads every update of [`update_candidates`], each one's failure kept with its id.
pub fn download_updates(auto: bool) -> Vec<std::result::Result<Download, (String, String)>> {
    update_candidates(auto).into_iter().map(|id| download(&id).map_err(|e| (id, e))).collect()
}

fn update_all(s: &mut Session, _: &Value) -> Result<Value> {
    index_url().ok_or_else(|| bad("marketplace.updateAll", NONE))?;
    let downloads = download_updates(false);
    Ok(install_updates(s, &downloads, false))
}

fn auto_update(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "marketplace.autoUpdate";
    index_url().ok_or_else(|| bad(C, NONE))?;
    let refreshed = bool_or(p, "refresh", true);
    if refreshed {
        refresh().map_err(|e| bad(C, e))?;
    }
    let downloads = download_updates(true);
    let mut r = install_updates(s, &downloads, true);
    r["refreshed"] = json!(refreshed);
    Ok(r)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "marketplace.list", "Plug-in Marketplace", [], None, "{query?: words every one of which is in the name, id, author, summary, description or tags, community?: bool (default: the saved choice), installed?: bool = false (only what is installed), updates?: bool = false (only what has an update)}: the plug-ins and pixel filters the marketplace lists, from the index last fetched (it fetches nothing; marketplace.refresh does) → {available: this build has a marketplace, fetchedAt: seconds since 1970 | null (never), generated: when the index last changed, error: why the last fetch failed | null (the last good index stays), community: community entries are listed, autoUpdate, platform: macos|windows|linux|android, source: the marketplace's page, entries: [{id, name, version, kind: plugin|filter, tier: vetted (a maintainer read its source) | community (automatic checks only), summary, description, author, tags, platforms: where it can be installed, url, sha256, size, requires, permissions: the groups it may ask for as the index declares them, network, fs, programs, signer: the author key's fingerprint | null, license, homepage, repository, changelog, released, reviewedBy, reviewedOn, installed: the version installed | null, update: the listed version is above it, supported: on this platform, missing: host features this version lacks, installable, conflict: the other kind of module has this id here, autoUpdate: it updates by itself, waiting: {version, why} an update the automatic pass left for the user | null}], hiddenCommunity: matching community entries not listed, updates: how many installed entries have one, total}. Installed entries are listed whatever the tier shown", always, list),
        cmd!(query "marketplace.refresh", "Refresh Plug-in Marketplace", [], None, "{…marketplace.list's filters}: fetches the index now (https only, at most 8 MB) and keeps it; the index always says the newest release of each plug-in, and no version is pinned in the app. An index that can't be fetched or read is an error and the last good one stays. It waits for the network; the marketplace window runs it on a worker → as marketplace.list", always, refresh_cmd),
        cmd!(query "marketplace.setCommunity", "Show Community Plug-ins", [], None, "{on: bool}: lists community entries too (off by default: only vetted ones). Community entries passed automatic checks only; nobody vouches for what they do. Every plug-in, of either tier, runs in the sandbox with no permission the user didn't allow → {community}", always, set_community),
        cmd!(query "marketplace.install", "Install from Plug-in Marketplace", [], None, "{id, acceptSigner?: the file's signerKey or fingerprint, acceptUnsigned?: bool = false}: downloads the listed version of entry `id` and installs it, or updates the installed one to it, as plugin.install does: nothing is granted (a plug-in asks the user for each permission as it needs it), an update keeps what the user allowed and the version replaced for plugin.rollback. Refused, installing nothing: an id the index doesn't list, a platform the entry doesn't support, host features this version lacks, a file whose SHA-256 isn't the entry's, that is another plug-in, kind or version than listed, an invalid author signature, an older version than the installed one. A file not signed by the key pinned for the installed plug-in installs nothing and gives {installed: false, needs: \"signer\", reason, signer, pinned} until called again with acceptSigner or acceptUnsigned. It waits for the download (at most 5 minutes) → {installed: true, plugin, from: the version replaced | null}. Remove one with plugin.uninstall", always, install),
        cmd!(query "marketplace.updateAll", "Update All Plug-ins", [], None, "{}: installs every update the index last fetched offers for installed plug-ins and pixel filters, each as marketplace.install does. One that needs an answer (another signer) or fails is left, with why → {updated: [{id, name, version, from}], waiting: [{id, name?, version?, why}]}", always, update_all),
        cmd!(query "marketplace.setAutoUpdate", "Update Plug-ins Automatically", [], None, "{on: bool, id?}: turns automatic updates on or off for every installed plug-in, or for plug-in `id` alone (off by default). With it on for any installed plug-in, the app fetches the index when it starts and once a day while it runs, and installs an update by itself only when it is the file the index names, asks for nothing the installed version didn't (no new permission, host, folder or program), and is signed as the installed version is; anything else waits for the user in the marketplace window (marketplace.list → waiting) → {autoUpdate, id, on}", always, set_auto_update),
        cmd!(query "marketplace.autoUpdate", "Run Automatic Plug-in Updates", [], None, "{refresh?: bool = true}: what the app runs by itself when automatic updates are on: fetches the index, then installs the updates of plug-ins set to update automatically that may install unasked (marketplace.setAutoUpdate). Each one left is recorded with why and not tried again until the index offers another version → {refreshed, updated: [{id, name, version, from}], waiting: [{id, name?, version?, why}]}", always, auto_update),
    ]
}
