//! Immich commands (IMM-CONNECT, IMM-LINK, IMM-IMPORT, IMM-EXTLIB) and `remote.pump`, which runs
//! the background work of [`crate::remote`]. See plan/immich.md and docs/immich.md.
//!
//! Commands that talk to the server directly (`immich.test`, `immich.connect`, `immich.browse`,
//! `immich.thumbnail`, `immich.libraries`, `immich.status {check}`) block for at most the
//! connection timeouts; the app runs the slow ones (listing, downloads) as background jobs
//! (`immich.link`, `immich.import`, `immich.fetchOriginal`) whose results `remote.pump` takes in.
//! API keys are passed to `immich.connect`/`immich.test` only, which are never journaled; no
//! result ever contains a key.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lcc_catalog::{Op, PhotoId, Source};
use lcc_credentials::Secret;
use lcc_immich::link::{self, Index, SERVICE};
use lcc_immich::types::MetadataSearch;
use lcc_immich::{Account, ImmichError, extlib, mapping};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::remote::{self, Downloaded, LINK_ONLY, Msg};
use crate::{Result, Session};

/// Most pages one link pass lists (1000 assets each).
const LINK_PAGES: u32 = 10_000;
const PAGE_SIZE: u32 = 1000;

#[path = "immich_collections.rs"]
pub(crate) mod collections;
#[path = "immich_publish.rs"]
pub mod publish;
#[path = "immich_sidecars.rs"]
pub(crate) mod sidecars;
#[path = "immich_sync.rs"]
pub(crate) mod sync;
#[path = "immich_versions.rs"]
pub(crate) mod versions;
/// Most assets one import takes.
const MAX_IMPORT: usize = 5000;

/// A failed server call as a result the UI and agents can act on (not a command error: the
/// command worked, the server did not).
fn failure(e: &ImmichError) -> Value {
    let mut v = json!({"ok": false, "error": {"kind": e.kind(), "message": e.to_string(), "retryable": e.retryable()}});
    if let ImmichError::Untrusted { host, fingerprint } = e {
        v["error"]["host"] = json!(host);
        v["error"]["fingerprint"] = json!(fingerprint);
    }
    v
}

/// The key a background call's result is kept under: `<command>:<account>`.
pub(crate) fn ask_key(command: &str, account: &str) -> String {
    format!("{command}:{account}")
}

/// Run a server call off the session thread, for a command's `background: true`: `work` runs on
/// a worker under an activity row (`label`), and its result waits under `key` until
/// `immich.answer {key}` takes it. One call per key at a time. The window never waits for the
/// network: these calls block for up to the connection timeouts when a server is slow or gone.
pub(crate) fn ask(s: &mut Session, key: String, label: &str, work: impl FnOnce(&crate::activity::TaskGuard) -> Value + Send + 'static) -> Value {
    if !s.remote.asking.insert(key.clone()) {
        return json!({"started": false, "key": key, "reason": "already running"});
    }
    s.remote.answers.remove(&key);
    let task = s.activity.start("immich", label, crate::activity::Cancel::No);
    let tx = s.remote.sender();
    let k = key.clone();
    std::thread::spawn(move || {
        let value = work(&task);
        drop(task);
        let _ = tx.send(Msg::Answer { key: k, value });
    });
    json!({"started": true, "key": key})
}

/// The result of a `background: true` call: `{done: false, running}` while it is out, then
/// `{done: true, result}` once (taken unless `keep`).
fn answer(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.answer";
    let key = str_param(p, "key").ok_or_else(|| bad(C, "missing `key` (as the background call returned it)"))?.to_string();
    // take in what the workers finished, so a caller that only polls this still gets its answer
    pump(s, &json!({"sync": false, "sha1": false}))?;
    let result = if bool_or(p, "keep", false) { s.remote.answers.get(&key).cloned() } else { s.remote.answers.remove(&key) };
    Ok(match result {
        Some(r) => json!({"key": key, "done": true, "result": r}),
        None => json!({"key": key, "done": false, "running": s.remote.asking.contains(&key)}),
    })
}

fn account_json(s: &Session, a: &Account) -> Value {
    let links = s.catalog.remote_links().iter().filter(|r| r.service == SERVICE && r.account_id == a.id);
    let (mut linked, mut probable) = (0u64, 0u64);
    for r in links {
        if r.sync_state == lcc_catalog::SyncState::Probable {
            probable += 1;
        } else {
            linked += 1;
        }
    }
    let missing = a.permissions.as_deref().map(lcc_immich::client::missing_permissions).unwrap_or_default();
    json!({
        "id": a.id,
        "url": a.url,
        "userId": a.user_id,
        "userName": a.user_name,
        "email": a.email,
        "version": a.version.map(|v| v.to_string()),
        "permissions": a.permissions,
        "missingPermissions": missing.iter().map(|(f, p)| json!({"feature": f, "permissions": p})).collect::<Vec<_>>(),
        "pinned": a.pinned,
        "collections": a.collections,
        "shared": a.shared,
        "pathMaps": a.path_maps,
        "linked": linked,
        "probable": probable,
        "linkedUntil": load_link_cursor(s, &a.id),
        "link": s.remote.links.get(&a.id),
        "apiKeyPage": lcc_immich::client::api_key_page(&a.url),
        "warning": lcc_immich::client::plain_http_warning(&a.url),
    })
}

fn account_param(s: &mut Session, p: &Value, c: &str) -> Result<String> {
    let accs = s.immich_accounts().map_err(|e| bad(c, e))?;
    match str_param(p, "account") {
        Some(id) => accs.get(id).map(|a| a.id.clone()).ok_or_else(|| bad(c, format!("no connected account `{id}`"))),
        None => match accs.immich.as_slice() {
            [one] => Ok(one.id.clone()),
            [] => Err(bad(c, "no Immich account is connected (immich.connect)")),
            _ => Err(bad(c, "several accounts are connected: give `account`")),
        },
    }
}

/// Contact the server: version, user, permissions.
fn probe(p: &Value, c: &str) -> Result<std::result::Result<(String, lcc_immich::ServerStatus, Option<String>), ImmichError>> {
    let url = str_param(p, "url").ok_or_else(|| bad(c, "missing `url`"))?;
    let key = str_param(p, "apiKey").map(str::trim).filter(|k| !k.is_empty()).ok_or_else(|| bad(c, "missing `apiKey`"))?;
    let pinned = str_param(p, "pinned").map(str::to_string).filter(|f| !f.is_empty());
    let opts = lcc_immich::ServerOptions { pinned: pinned.clone(), ..lcc_immich::ServerOptions::default() };
    Ok(lcc_immich::Client::new(url, Secret::new(key), &opts).and_then(|cl| cl.status().map(|st| (cl.base().to_string(), st, pinned))))
}

fn status_json(base: &str, st: &lcc_immich::ServerStatus) -> Value {
    json!({
        "ok": true,
        "url": base,
        "version": st.version.to_string(),
        "user": {"id": st.user.id, "name": st.user.name, "email": st.user.email, "isAdmin": st.user.is_admin},
        "permissions": st.permissions,
        "missingPermissions": st.permissions.as_deref().map(lcc_immich::client::missing_permissions).unwrap_or_default()
            .iter().map(|(f, p)| json!({"feature": f, "permissions": p})).collect::<Vec<_>>(),
        "warning": lcc_immich::client::plain_http_warning(base),
    })
}

fn test(_: &mut Session, p: &Value) -> Result<Value> {
    Ok(match probe(p, "immich.test")? {
        Ok((base, st, _)) => status_json(&base, &st),
        Err(e) => failure(&e),
    })
}

fn connect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.connect";
    if bool_or(p, "background", false) {
        // the probe runs on a worker; `remote.pump` stores the key and the account
        let url = str_param(p, "url").ok_or_else(|| bad(C, "missing `url`"))?.to_string();
        let key = str_param(p, "apiKey").map(str::trim).filter(|k| !k.is_empty()).ok_or_else(|| bad(C, "missing `apiKey`"))?.to_string();
        let pinned = str_param(p, "pinned").map(str::to_string).filter(|f| !f.is_empty());
        if s.remote.connecting {
            return Ok(json!({"started": false, "reason": "a connection attempt is already running"}));
        }
        // fail now (not after the server answered) when there is nowhere to keep the key
        s.secret_store().map_err(|e| bad(C, format!("can't store the API key: {e}")))?;
        s.remote.connecting = true;
        s.remote.connected = None;
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let opts = lcc_immich::ServerOptions { pinned: pinned.clone(), ..lcc_immich::ServerOptions::default() };
            let key = Secret::new(key);
            let r = lcc_immich::Client::new(&url, key.clone(), &opts)
                .and_then(|cl| cl.status().map(|st| remote::Probed { base: cl.base().to_string(), status: st, pinned, key }));
            let _ = tx.send(Msg::Connected(Box::new(r)));
        });
        return Ok(json!({"started": true}));
    }
    let (base, st, pinned) = match probe(p, C)? {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let key = str_param(p, "apiKey").map(str::trim).unwrap_or_default().to_string();
    adopt(s, remote::Probed { base, status: st, pinned, key: Secret::new(key) }).map_err(|e| bad(C, e))
}

/// Store a probed server's key and account → the `immich.connect` result.
fn adopt(s: &mut Session, pr: remote::Probed) -> std::result::Result<Value, String> {
    let remote::Probed { base, status: st, pinned, key } = pr;
    let id = Account::make_id(&base, &st.user.id);
    let store = s.secret_store().map_err(|e| format!("can't store the API key: {e}"))?;
    store.set(&lcc_immich::accounts::secret_key(&id), &key).map_err(|e| format!("can't store the API key: {e}"))?;
    let acc = Account {
        id: id.clone(),
        url: base.clone(),
        user_id: st.user.id.clone(),
        user_name: st.user.name.clone(),
        email: st.user.email.clone(),
        version: Some(st.version),
        permissions: st.permissions.clone(),
        pinned,
        ..Account::default()
    };
    s.immich_accounts()?.upsert(acc);
    s.save_accounts()?;
    // a check made with the old (or missing) key no longer applies
    s.remote.checks.remove(&id);
    let mut v = status_json(&base, &st);
    v["account"] = json!(id);
    Ok(v)
}

fn disconnect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.disconnect";
    let id = account_param(s, p, C)?;
    if let Ok(store) = s.secret_store()
        && let Err(e) = store.delete(&lcc_immich::accounts::secret_key(&id))
    {
        log::warn!("could not delete the stored Immich key: {e}");
    }
    s.immich_accounts().map_err(|e| bad(C, e))?.remove(&id);
    s.save_accounts().map_err(|e| bad(C, e))?;
    s.remote.links.remove(&id);
    let mut forgot = 0;
    if bool_or(p, "forgetLinks", false) {
        let keys: Vec<_> = s.catalog.remote_links().iter().filter(|r| r.service == SERVICE && r.account_id == id).map(|r| r.photo_id).collect();
        let ops: Vec<Op> =
            keys.iter().map(|ph| Op::SetRemote { photo: *ph, service: SERVICE.into(), account_id: id.clone(), record: None }).collect();
        forgot = ops.len();
        if !ops.is_empty() {
            s.commit("Forget Immich Links", Op::Batch { ops })?;
        }
    }
    Ok(json!({"disconnected": id, "forgotLinks": forgot}))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let accs = s.immich_accounts().map_err(|e| bad("immich.status", e))?.immich.clone();
    let check = bool_or(p, "check", false);
    let background = bool_or(p, "background", false);
    let mut out = Vec::new();
    for a in &accs {
        let mut v = account_json(s, a);
        if check && background {
            if !s.remote.checking.contains(&a.id) {
                match s.immich_client(&a.id) {
                    Ok((_, c)) => {
                        s.remote.checking.insert(a.id.clone());
                        let tx = s.remote.sender();
                        let account = a.id.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Checked { account, result: c.status() });
                        });
                    }
                    Err(e) => {
                        s.remote.checks.insert(a.id.clone(), failure(&e));
                    }
                }
            }
        } else if check {
            v["server"] = match s.immich_client(&a.id).and_then(|(_, c)| c.status()) {
                Ok(st) => {
                    if let Some(acc) = s.immich_accounts().ok().and_then(|x| x.get_mut(&a.id)) {
                        acc.version = Some(st.version);
                        acc.permissions = st.permissions.clone();
                    }
                    status_json(&a.url, &st)
                }
                Err(e) => failure(&e),
            };
        }
        if v.get("server").is_none()
            && let Some(c) = s.remote.checks.get(&a.id)
        {
            v["server"] = c.clone();
        }
        v["checking"] = json!(s.remote.checking.contains(&a.id));
        out.push(v);
    }
    if check && !background {
        let _ = s.save_accounts();
    }
    let st = match &s.remote.store {
        Some(x) => x.name(),
        None => s.secret_store().map(|x| x.name()).unwrap_or("unavailable"),
    };
    Ok(json!({"accounts": out, "secretStore": st, "connecting": s.remote.connecting, "connected": s.remote.connected}))
}

/// The server session's auth state for one account: elevated (locked folders visible), the
/// unlock secrets the account has, and when the elevation ends.
fn auth_status(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.authStatus";
    let id = account_param(s, p, C)?;
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let account = id.clone();
    let work = move || match client.auth_status() {
        Ok(st) => json!({
            "ok": true,
            "account": account,
            "isElevated": st.is_elevated,
            "password": st.password,
            "pinCode": st.pin_code,
            "expiresAt": st.expires_at,
            "pinExpiresAt": st.pin_expires_at,
        }),
        Err(e) => failure(&e),
    };
    if bool_or(p, "background", false) {
        return Ok(ask(s, ask_key(C, &id), "Checking Immich's locked folders", move |_| work()));
    }
    Ok(work())
}

/// Unlock the server session of `account` (locked-folder assets become visible to its listings,
/// so link, import and sync see them too). The secret is the Immich account's PIN code (4–6
/// digits, sent as `pinCode`) or its password (anything else, sent as `password`). Like
/// `immich.connect`/`immich.test`, this command is never journaled: the secret exists only for
/// this call and is kept neither in the journal, the log nor any result.
fn unlock_session(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.unlockSession";
    let id = account_param(s, p, C)?;
    let secret = str_param(p, "secret")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(C, "missing `secret` (the Immich account's PIN or password)"))?;
    let digits = secret.chars().count();
    let unlock = if (4..=6).contains(&digits) && secret.bytes().all(|b| b.is_ascii_digit()) {
        lcc_immich::types::SessionUnlock { pin_code: Some(secret.to_string()), password: None }
    } else {
        lcc_immich::types::SessionUnlock { pin_code: None, password: Some(secret.to_string()) }
    };
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let account = id.clone();
    // (the secret lives in this closure only: on the worker as on the session thread it is sent
    // once and dropped)
    let work = move || match client.unlock_session(&unlock) {
        Ok(()) => json!({"ok": true, "account": account}),
        Err(e) => failure(&e),
    };
    if bool_or(p, "background", false) {
        return Ok(ask(s, ask_key(C, &id), "Unlocking Immich's locked folders", move |_| work()));
    }
    Ok(work())
}

/// A file name for per-account state kept with the library.
fn account_file_name(account: &str) -> String {
    account.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).take(150).collect()
}

/// Where the link pass's cursor for `account` is kept: with the library (`<library>/Immich/link/`),
/// because it says which of the server's assets *this catalog* has been matched against. (It was
/// kept per machine: a second catalog then never saw the assets the first one's pass had listed.)
/// `None`: no folder to keep it in; every pass then lists everything.
fn link_cursor_path(s: &Session, account: &str) -> Option<PathBuf> {
    s.immich_dir().map(|d| d.join("link").join(format!("{}.json", account_file_name(account))))
}

/// The newest `updatedAt` a complete link pass of this catalog has seen.
fn load_link_cursor(s: &Session, account: &str) -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(link_cursor_path(s, account)?).ok()?).ok()?;
    v["linkedUntil"].as_str().map(str::to_string)
}

fn save_link_cursor(path: &Path, until: &str) -> std::result::Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    lcc_catalog::safe_file::write_atomic(path, json!({"linkedUntil": until}).to_string().as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Start a link pass for `account` (incremental from its last complete pass unless `full`).
fn start_link(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.link";
    let id = account_param(s, p, C)?;
    start_link_pass(s, &id, bool_or(p, "full", false))
}

/// The link pass: list the server's assets on a worker (those changed since this catalog's last
/// complete pass, or all of them with `full`), page by page into `remote.pump`, which links them.
/// It holds an activity row with the count so far and can be cancelled; a cancelled or failed
/// pass leaves the cursor where it was.
pub(crate) fn start_link_pass(s: &mut Session, id: &str, full: bool) -> Result<Value> {
    let id = id.to_string();
    if s.remote.links.get(&id).is_some_and(|l| l.active) {
        return Ok(json!({"started": false, "reason": "a link pass is already running"}));
    }
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let home = link_cursor_path(s, &id);
    let since = if full { None } else { load_link_cursor(s, &id) };
    s.remote.links.insert(id.clone(), remote::LinkProgress { active: true, ..Default::default() });
    let task = s.activity.start("immich", "Linking photos with Immich", crate::activity::Cancel::Yes);
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        let q =
            MetadataSearch { size: Some(PAGE_SIZE), with_exif: Some(true), updated_after: since, kind: Some("IMAGE".into()), ..Default::default() };
        let mut newest: Option<String> = None;
        let mut seen = 0u64;
        let r = client.search_all(&q, LINK_PAGES, |page| {
            for a in page {
                if a.updated_at.as_ref().is_some_and(|u| newest.as_ref().is_none_or(|n| u > n)) {
                    newest = a.updated_at.clone();
                }
            }
            seen = seen.saturating_add(page.len() as u64);
            // (the server does not say how many there are in all)
            task.progress(seen, 0);
            !task.is_cancelled() && tx.send(Msg::LinkPage { account: id.clone(), assets: page.to_vec() }).is_ok()
        });
        // a pass that was stopped did not see everything: the cursor must not move
        let result = if task.is_cancelled() { Err(ImmichError::Cancelled) } else { r.map(|_| newest) };
        drop(task);
        let _ = tx.send(Msg::LinkDone { account: id, result, home });
    });
    Ok(json!({"started": true}))
}

/// Ask the connected servers whether they already have the photos that were just imported or
/// hashed ([`remote::State::link_queue`]), by checksum (`POST /assets/bulk-upload-check`), on a
/// worker; what they have comes back as a link page and is linked like a link pass's. A failed
/// lookup (offline) is dropped: Full Link Pass catches up.
fn start_link_check(s: &mut Session) {
    if s.remote.link_checking || s.remote.link_queue.is_empty() {
        return;
    }
    let queue = std::mem::take(&mut s.remote.link_queue);
    let accounts: Vec<String> = match s.immich_accounts() {
        Ok(a) => a.immich.iter().map(|a| a.id.clone()).collect(),
        Err(_) => return,
    };
    let mut jobs = Vec::new();
    for acc in accounts {
        let items: Vec<lcc_immich::types::UploadCheck> = queue
            .iter()
            .filter_map(|id| {
                let p = s.catalog.photo(*id)?;
                let sha1 = p.sha1.clone()?;
                let linked = s.catalog.remote_of(*id).any(|r| r.service == SERVICE && r.account_id == acc);
                (!p.deleted && !linked).then(|| lcc_immich::types::UploadCheck { id: id.0.to_string(), checksum: sha1 })
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        // (no key at hand, a locked key file: nothing to ask with)
        let Ok((_, client)) = s.immich_client(&acc) else { continue };
        jobs.push((acc, client, items));
    }
    if jobs.is_empty() {
        return;
    }
    s.remote.link_checking = true;
    let task = s.activity.start("immich", "Looking up new photos in Immich", crate::activity::Cancel::Yes);
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        let total: u64 = jobs.iter().map(|(_, _, i)| i.len() as u64).sum();
        let mut done = 0u64;
        for (account, client, items) in jobs {
            let mut assets = Vec::new();
            'account: for chunk in items.chunks(500) {
                if task.is_cancelled() {
                    break 'account;
                }
                let found = match client.upload_check(chunk) {
                    Ok(r) => r,
                    Err(e) => {
                        log::info!("Immich checksum lookup: {e}");
                        break 'account;
                    }
                };
                for id in found.into_iter().filter_map(|r| r.asset_id).filter(|id| !id.is_empty()) {
                    // the asset itself (its checksum, trash state and `updatedAt`) decides the link
                    match client.asset(&id) {
                        Ok(a) => assets.push(a),
                        Err(e) if e.retryable() => break 'account,
                        Err(_) => {}
                    }
                }
                done = done.saturating_add(chunk.len() as u64);
                task.progress(done, total);
            }
            if !assets.is_empty() {
                let _ = tx.send(Msg::LinkPage { account, assets });
            }
        }
        drop(task);
        let _ = tx.send(Msg::LinkChecked);
    });
}

fn confirm(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let accounts: Vec<String> = match str_param(p, "account") {
        Some(a) => vec![a.to_string()],
        None => s.immich_accounts().map(|a| a.immich.iter().map(|x| x.id.clone()).collect()).unwrap_or_default(),
    };
    let ops: Vec<Op> = ids.iter().flat_map(|id| accounts.iter().filter_map(|a| link::confirm_op(&s.catalog, *id, a))).collect();
    let n = ops.len();
    if n > 0 {
        s.commit("Confirm Immich Link", Op::Batch { ops })?;
    }
    Ok(json!({"confirmed": n}))
}

fn unlink(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let ops: Vec<Op> = ids
        .iter()
        .flat_map(|id| {
            s.catalog
                .remote_of(*id)
                .filter(|r| r.service == SERVICE && str_param(p, "account").is_none_or(|a| a == r.account_id))
                .map(|r| Op::SetRemote { photo: r.photo_id, service: SERVICE.into(), account_id: r.account_id.clone(), record: None })
                .collect::<Vec<_>>()
        })
        .collect();
    let n = ops.len();
    if n > 0 {
        s.commit("Remove Immich Link", Op::Batch { ops })?;
    }
    Ok(json!({"removed": n}))
}

/// The photo's Immich assets and their web pages (Metadata panel → Open in Immich).
fn links_of(s: &mut Session, p: &Value) -> Result<Value> {
    let id = s.targets(p)?.first().copied().ok_or_else(|| bad("immich.links", "no photo"))?;
    let accs = s.immich_accounts().map(|a| a.clone()).unwrap_or_default();
    let out: Vec<Value> = s
        .catalog
        .remote_of(id)
        .filter(|r| r.service == SERVICE)
        .map(|r| {
            let base =
                accs.get(&r.account_id).map(|a| a.url.clone()).unwrap_or_else(|| r.account_id.split('#').next().unwrap_or_default().to_string());
            json!({
                "account": r.account_id,
                "assetId": r.remote_id,
                "state": if r.sync_state == lcc_catalog::SyncState::Probable { "probable" } else { "linked" },
                "url": lcc_immich::client::asset_web_url(&base, &r.remote_id),
            })
        })
        .collect();
    Ok(
        json!({"id": id.0, "state": link::link_state(&s.catalog, id), "links": out, "linkOnly": s.catalog.photo(id).is_some_and(|p| p.preview_only.as_deref() == Some(LINK_ONLY))}),
    )
}

fn asset_json(s: &Session, index: &Index, account: &str, a: &lcc_immich::Asset) -> Value {
    let in_catalog = s.catalog.photo_of_remote(SERVICE, account, &a.id).or_else(|| index.has_sha1(&a.checksum));
    json!({
        "id": a.id,
        "fileName": a.original_file_name,
        "captured": a.local_capture(),
        "favorite": a.is_favorite,
        "rating": a.exif_info.as_ref().and_then(|e| e.rating),
        "size": a.file_size(),
        "type": a.kind,
        "inCatalog": in_catalog.map(|p| p.0),
    })
}

/// One page of a source for the Import dialog: `timeline`, `favorites`, `album` (`id`) or
/// `person` (`id`); `albums` and `people` list those.
fn browse(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.browse";
    let id = account_param(s, p, C)?;
    let source = str_param(p, "source").unwrap_or("timeline");
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let r = match source {
        "albums" => client.albums().map(|a| json!({"albums": a.iter().map(|x| json!({"id": x.id, "name": x.album_name, "assets": x.asset_count, "thumbnail": x.album_thumbnail_asset_id})).collect::<Vec<_>>()})),
        "people" => client.people().map(|a| json!({"people": a.iter().filter(|x| !x.is_hidden).map(|x| json!({"id": x.id, "name": x.name})).collect::<Vec<_>>()})),
        "timeline" | "favorites" | "album" | "person" => {
            let page = p.get("page").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100_000) as u32;
            let size = p.get("size").and_then(Value::as_u64).unwrap_or(100).clamp(1, 1000) as u32;
            let mut q = MetadataSearch { page: Some(page), size: Some(size), with_exif: Some(true), order: Some("desc".into()), ..Default::default() };
            let target = str_param(p, "id").map(str::to_string);
            match source {
                "favorites" => q.is_favorite = Some(true),
                "album" => q.album_ids = Some(vec![target.ok_or_else(|| bad(C, "missing album `id`"))?]),
                "person" => q.person_ids = Some(vec![target.ok_or_else(|| bad(C, "missing person `id`"))?]),
                _ => {}
            }
            client.search(&q).map(|pg| {
                let index = Index::new(&s.catalog);
                json!({
                    "assets": pg.items.iter().map(|a| asset_json(s, &index, &id, a)).collect::<Vec<_>>(),
                    "total": pg.total,
                    "nextPage": pg.next_page.and_then(|n| n.parse::<u32>().ok()),
                })
            })
        }
        other => return Err(bad(C, format!("unknown source `{other}` (timeline|favorites|album|person|albums|people)"))),
    };
    Ok(match r {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => failure(&e),
    })
}

/// Save an asset's thumbnail to a file (`path`, else the Immich cache folder) → {path}.
fn thumbnail(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.thumbnail";
    let id = account_param(s, p, C)?;
    let asset = str_param(p, "assetId").ok_or_else(|| bad(C, "missing `assetId`"))?.to_string();
    let dest = match str_param(p, "path") {
        Some(x) => PathBuf::from(x),
        None => {
            s.immich_dir().ok_or_else(|| bad(C, "no folder for downloads: give `path`"))?.join("thumbs").join(format!("{}.jpg", safe_name(&asset)))
        }
    };
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    match client.thumbnail(&asset, str_param(p, "size").unwrap_or("thumbnail")) {
        Ok(bytes) => {
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d).map_err(|e| bad(C, format!("{}: {e}", d.display())))?;
            }
            std::fs::write(&dest, bytes).map_err(|e| bad(C, format!("{}: {e}", dest.display())))?;
            Ok(json!({"ok": true, "path": dest.to_string_lossy()}))
        }
        Err(e) => Ok(failure(&e)),
    }
}

/// A file or folder name from server data: no separators, no `..`, bounded.
fn safe_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { '_' } else { c }).take(200).collect();
    let n = n.trim().trim_start_matches('.').to_string();
    if n.is_empty() { "asset".into() } else { n }
}

/// Import assets (`assets`: ids) from Immich: `copy` downloads the originals into `destination`
/// (a dated folder inside it) and imports them; `link` catalogs a preview and downloads the
/// original on first Develop. Assets whose checksum is already in the catalog are skipped.
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.import";
    let id = account_param(s, p, C)?;
    if s.remote.import.active {
        return Err(bad(C, "an Immich import is already running"));
    }
    let ids: Vec<String> =
        p.get("assets").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if ids.is_empty() {
        return Err(bad(C, "missing `assets` (asset ids)"));
    }
    if ids.len() > MAX_IMPORT {
        return Err(bad(C, format!("at most {MAX_IMPORT} assets per import")));
    }
    let link_only = match str_param(p, "mode").unwrap_or("copy") {
        "copy" => false,
        "link" => true,
        m => return Err(bad(C, format!("unknown mode `{m}` (copy|link)"))),
    };
    let root = match str_param(p, "destination") {
        Some(d) => PathBuf::from(d),
        None if link_only => s.immich_dir().ok_or_else(|| bad(C, "no folder for link-only previews"))?.join("previews"),
        None => return Err(bad(C, "missing `destination` folder")),
    };
    let day: String = (s.clock)().chars().take(10).collect();
    let dir = if link_only { root } else { root.join(day) };
    let album_name = str_param(p, "albumName").map(str::to_string);
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    // the catalog's checksums, to skip duplicates before downloading
    let index = Index::new(&s.catalog);
    let linked: BTreeSet<String> =
        s.catalog.remote_links().iter().filter(|r| r.service == SERVICE && r.account_id == id).map(|r| r.remote_id.clone()).collect();
    s.remote.import = remote::ImportProgress { active: true, requested: ids.len(), ..Default::default() };
    let task = s.activity.start("immich", "Importing from Immich", crate::activity::Cancel::Yes);
    let total = ids.len() as u64;
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        let mut items = Vec::new();
        let mut skipped = 0;
        // assets the catalog already has: not downloaded again, but still part of what the user
        // asked for (they are linked, and join the album, in `finish_import`)
        let mut already_linked = Vec::new();
        let mut present = Vec::new();
        let mut used: BTreeSet<PathBuf> = BTreeSet::new();
        for (n, aid) in ids.into_iter().enumerate() {
            // stopped: what was downloaded so far is still imported
            if task.is_cancelled() {
                break;
            }
            task.progress(n as u64, total);
            if linked.contains(&aid) {
                skipped += 1;
                already_linked.push(aid);
                continue;
            }
            let asset = match client.asset(&aid) {
                Ok(a) => a,
                Err(e) => {
                    items.push((lcc_immich::Asset { id: aid, ..Default::default() }, Err(e)));
                    continue;
                }
            };
            if index.has_sha1(&asset.checksum).is_some() {
                skipped += 1;
                present.push(asset);
                continue;
            }
            task.detail(&asset.original_file_name);
            let r = if link_only {
                let stem = Path::new(&asset.original_file_name).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                let dest = dir.join(safe_name(&asset.id)).join(format!("{}.jpg", safe_name(&stem)));
                client.thumbnail(&asset.id, "preview").and_then(|b| {
                    dest.parent().map(std::fs::create_dir_all).transpose().map_err(|e| ImmichError::Io(e.to_string()))?;
                    std::fs::write(&dest, b).map_err(|e| ImmichError::Io(format!("{}: {e}", dest.display())))?;
                    Ok(dest)
                })
            } else {
                let mut dest = dir.join(safe_name(&asset.original_file_name));
                let mut n = 1;
                while dest.exists() || used.contains(&dest) {
                    let p = Path::new(&asset.original_file_name);
                    let stem = p.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    let ext = p.extension().map(|x| format!(".{}", x.to_string_lossy())).unwrap_or_default();
                    dest = dir.join(safe_name(&format!("{stem}-{n}{ext}")));
                    n += 1;
                }
                used.insert(dest.clone());
                std::fs::create_dir_all(&dir)
                    .map_err(|e| ImmichError::Io(format!("{}: {e}", dir.display())))
                    .and_then(|_| client.download_original(&asset.id, &dest, task.cancel_flag()).map(|_| dest))
            };
            // a download the user stopped is not a failure to report
            if matches!(r, Err(ImmichError::Cancelled)) {
                break;
            }
            items.push((asset, r));
        }
        drop(task);
        let _ = tx.send(Msg::Downloaded(Box::new(Downloaded { account: id, link_only, album_name, items, skipped, already_linked, present })));
    });
    Ok(json!({"started": true}))
}

/// Download the original of a link-only photo (in the background).
fn fetch_original(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.fetchOriginal";
    let id = s.targets(p)?.first().copied().ok_or_else(|| bad(C, "no photo"))?;
    let Some(ph) = s.catalog.photo(id) else { return Err(bad(C, "no such photo")) };
    if ph.preview_only.as_deref() != Some(LINK_ONLY) {
        return Ok(json!({"started": false, "reason": "the photo's original is already here"}));
    }
    let ph_name = ph.file_name.clone();
    if s.remote.fetching.contains(&id) {
        return Ok(json!({"started": false, "reason": "already downloading"}));
    }
    let Some(r) = s.catalog.remote_of(id).find(|r| r.service == SERVICE).cloned() else { return Err(bad(C, "the photo is not linked to Immich")) };
    let dir = s.immich_dir().ok_or_else(|| bad(C, "no folder for downloads"))?.join("originals").join(safe_name(&r.remote_id));
    let (_, client) = match s.immich_client(&r.account_id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    s.remote.fetching.insert(id);
    s.remote.fetch_errors.remove(&id);
    let task = s.activity.start("immich", "Downloading the original from Immich", crate::activity::Cancel::Yes);
    task.set_unit(crate::activity::Unit::Bytes);
    task.detail(&ph_name);
    let tx = s.remote.sender();
    std::thread::spawn(move || {
        let result = client.asset(&r.remote_id).and_then(|a| {
            std::fs::create_dir_all(&dir).map_err(|e| ImmichError::Io(e.to_string()))?;
            let dest = dir.join(safe_name(&a.original_file_name));
            let mut progress = |done: u64, total: Option<u64>| task.progress(done, total.or(a.file_size()).unwrap_or(0));
            client.download_original_with(&a.id, &dest, task.cancel_flag(), Some(&mut progress)).map(|_| dest)
        });
        drop(task);
        let _ = tx.send(Msg::Original { photo: id, account: r.account_id, result });
    });
    Ok(json!({"started": true}))
}

/// Take in what the background workers finished; start the next SHA-1 batch. Cheap: the app
/// calls it every frame.
pub fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    let now = (s.clock)();
    for m in s.remote.drain() {
        let Some(m) = super::credentials::take_in(s, m) else { continue };
        match m {
            Msg::FileUnlocked(_) | Msg::SystemUnlocked(_) => {}
            Msg::Connected(r) => {
                s.remote.connecting = false;
                let v = match *r {
                    Ok(pr) => adopt(s, pr).unwrap_or_else(|e| json!({"ok": false, "error": {"kind": "keyStorage", "message": e, "retryable": true}})),
                    Err(e) => failure(&e),
                };
                s.remote.connected = Some(v);
            }
            Msg::Checked { account, result } => {
                s.remote.checking.remove(&account);
                let v = match result {
                    Ok(st) => {
                        let url = match s.immich_accounts().ok().and_then(|x| x.get_mut(&account)) {
                            Some(acc) => {
                                acc.version = Some(st.version);
                                acc.permissions = st.permissions.clone();
                                acc.url.clone()
                            }
                            None => String::new(),
                        };
                        if let Err(e) = s.save_accounts() {
                            log::warn!("could not save the Immich accounts: {e}");
                        }
                        status_json(&url, &st)
                    }
                    Err(e) => failure(&e),
                };
                s.remote.checks.insert(account, v);
            }
            Msg::Sha1(done) => {
                s.remote.sha1_busy = false;
                for (id, path, h) in done {
                    s.remote.sha1_first.remove(&id);
                    // the photo may have moved or been relinked meanwhile
                    let same =
                        s.catalog.photo(id).is_some_and(|ph| matches!(&ph.source, Source::File { path: x } if *x == path) && ph.sha1.is_none());
                    match h {
                        Some(h) if same => {
                            if let Err(e) = s.apply_system(Op::SetSha1 { id, sha1: Some(h) }) {
                                log::warn!("SHA-1 of photo {}: {e}", id.0);
                            }
                            s.remote.sha1_done += 1;
                            // now that it has a checksum: does a connected server have it?
                            s.remote.link_queue.push(id);
                        }
                        Some(_) => {}
                        None => {
                            s.remote.sha1_failed.insert(id);
                        }
                    }
                }
            }
            Msg::LinkPage { account, assets } => {
                let maps = s.immich_accounts().ok().and_then(|a| a.get(&account).map(|x| x.path_maps.clone())).unwrap_or_default();
                let index = Index::with_path_maps(&s.catalog, &maps);
                let (ops, found) = link::link_ops(&s.catalog, &index, &account, &assets, &now);
                let pr = s.remote.links.entry(account).or_default();
                pr.seen += assets.len() as u64;
                pr.linked += found.iter().filter(|m| m.kind != link::MatchKind::Probable).count() as u64;
                pr.probable += found.iter().filter(|m| m.kind == link::MatchKind::Probable).count() as u64;
                for op in ops {
                    if let Err(e) = s.apply_system(op) {
                        log::warn!("Immich link: {e}");
                    }
                }
            }
            Msg::LinkChecked => s.remote.link_checking = false,
            Msg::Answer { key, value } => {
                s.remote.asking.remove(&key);
                s.remote.answers.insert(key, value);
            }
            Msg::People(d) => {
                let key = ask_key("immich.importPeople", &d.account);
                let v = sync::finish_people(s, *d);
                s.remote.asking.remove(&key);
                s.remote.answers.insert(key, v);
            }
            Msg::LinkDone { account, result, home } => {
                // the cursor is this catalog's: a pass started in another library does not move it
                let cursor = link_cursor_path(s, &account).filter(|p| Some(p) == home.as_ref());
                let old = load_link_cursor(s, &account);
                let pr = s.remote.links.entry(account.clone()).or_default();
                pr.active = false;
                pr.finished_at = Some(now.clone());
                match result {
                    Ok(newest) => {
                        pr.error = None;
                        pr.error_kind = None;
                        if let (Some(n), Some(path)) = (newest, cursor)
                            && old.as_ref().is_none_or(|u| n > *u)
                            && let Err(e) = save_link_cursor(&path, &n)
                        {
                            log::warn!("could not save the Immich link cursor: {e}");
                        }
                    }
                    Err(e) => {
                        pr.error = Some(e.to_string());
                        pr.error_kind = Some(e.kind().into());
                    }
                }
            }
            Msg::Downloaded(d) => finish_import(s, *d, &now),
            Msg::ImmichAlbums(l) => collections::finish_listing(s, *l),
            Msg::ImmichPushed(p) => collections::finish_push(s, *p),
            Msg::ImmichPushDone => s.remote.coll.pushing = false,
            Msg::Synced(d) => {
                sync::finish(s, *d);
            }
            Msg::Sidecars(d) => {
                sidecars::finish(s, *d);
            }
            Msg::Version(d) => {
                versions::finish(s, *d);
            }
            Msg::Original { photo, account, result } => {
                s.remote.fetching.remove(&photo);
                match result {
                    Ok(path) => {
                        if let Err(e) = adopt_original(s, photo, &account, &path) {
                            s.remote.fetch_errors.insert(photo, e.to_string());
                        }
                    }
                    Err(e) => {
                        s.remote.fetch_errors.insert(photo, e.to_string());
                    }
                }
            }
        }
    }
    if bool_or(p, "sync", true) {
        sync::tick(s);
    }
    start_link_check(s);
    collections::tick(s);
    // the next SHA-1 batch
    if bool_or(p, "sha1", true) && !s.remote.sha1_busy && s.remote.sha1_idle_at != Some(s.catalog.revision) {
        let todo = remote::sha1_pending(s, remote::SHA1_BATCH);
        if todo.is_empty() {
            s.remote.sha1_idle_at = Some(s.catalog.revision);
        } else {
            s.remote.sha1_busy = true;
            remote::spawn_sha1(s.remote.sender(), todo);
        }
    }
    Ok(json!({
        "sha1": {"active": s.remote.sha1_busy, "hashed": s.remote.sha1_done, "failed": s.remote.sha1_failed.len()},
        "links": s.remote.links,
        "import": s.remote.import,
        "fetching": s.remote.fetching.iter().map(|i| i.0).collect::<Vec<_>>(),
        "fetchErrors": s.remote.fetch_errors.iter().map(|(k, v)| json!({"id": k.0, "error": v})).collect::<Vec<_>>(),
        "connecting": s.remote.connecting,
        "checking": s.remote.checking.iter().collect::<Vec<_>>(),
        "unlocking": s.remote.unlocking,
        "sidecars": s.remote.sidecars.json(),
        "asking": s.remote.asking.iter().collect::<Vec<_>>(),
        "answers": s.remote.answers.keys().collect::<Vec<_>>(),
    }))
}

fn finish_import(s: &mut Session, d: Downloaded, now: &str) {
    let mut ok: Vec<(lcc_immich::Asset, PathBuf)> = Vec::new();
    let mut failed = Vec::new();
    for (a, r) in d.items {
        match r {
            Ok(p) => ok.push((a, p)),
            Err(e) => failed.push((if a.original_file_name.is_empty() { a.id.clone() } else { a.original_file_name.clone() }, e.to_string())),
        }
    }
    let mut imported = 0;
    // the album the import made for `album_name`, if it made one
    let mut album: Option<u64> = None;
    if !ok.is_empty() {
        let paths: Vec<String> = ok.iter().map(|(_, p)| p.to_string_lossy().to_string()).collect();
        let mut params = json!({"paths": paths, "mode": "add"});
        if let Some(n) = &d.album_name {
            params["albumName"] = json!(n);
        }
        match s.execute("library.import", &params) {
            Ok(rep) => {
                album = rep["album"].as_u64();
                let ids: Vec<u64> = rep["imported"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
                let mut ops = Vec::new();
                for id in ids.iter().map(|i| PhotoId(*i)) {
                    let Some(ph) = s.catalog.photo(id) else { continue };
                    let Source::File { path } = &ph.source else { continue };
                    let Some((a, _)) = ok.iter().find(|(_, p)| p.to_string_lossy() == path.as_str()) else { continue };
                    ops.extend(mapping::ops(a, ph));
                    if d.link_only {
                        // the preview is not the original: no SHA-1 for it, and it is shown as a preview
                        ops.push(Op::SetSha1 { id, sha1: None });
                        ops.push(Op::SetContent {
                            id,
                            width: ph.width,
                            height: ph.height,
                            file_size: a.file_size().unwrap_or(ph.file_size),
                            content_hash: ph.content_hash.clone(),
                            preview_only: Some(LINK_ONLY.into()),
                            preview_checked: lcc_catalog::APP_VERSION.to_string(),
                        });
                    }
                    ops.push(lcc_catalog::Catalog::link_remote_op(lcc_catalog::RemoteIdentity {
                        photo_id: id,
                        service: SERVICE.into(),
                        account_id: d.account.clone(),
                        remote_id: a.id.clone(),
                        remote_checksum: (!a.checksum.is_empty()).then(|| a.checksum.clone()),
                        remote_updated_at: a.updated_at.clone(),
                        last_synced_at: Some(now.to_string()),
                        sync_state: lcc_catalog::SyncState::Synced,
                    }));
                    imported += 1;
                }
                if !ops.is_empty()
                    && let Err(e) = s.apply_system(Op::Batch { ops })
                {
                    failed.push(("metadata".into(), e.to_string()));
                }
                for (path, e) in rep["failed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| Some((x.get(0)?.as_str()?.to_string(), x.get(1)?.as_str()?.to_string())))
                {
                    failed.push((path, e));
                }
            }
            Err(e) => failed.push(("import".into(), e.to_string())),
        }
    }
    // Assets the catalog already had were not downloaded, but they belong to what was asked for:
    // each is linked to its photo by checksum (unless it or the photo is linked already), and the
    // photos join the album like the new ones. Photos in Recently Deleted stay out of it.
    let mut have: Vec<PhotoId> = d.already_linked.iter().filter_map(|a| s.catalog.photo_of_remote(SERVICE, &d.account, a)).collect();
    if !d.present.is_empty() {
        let index = Index::new(&s.catalog);
        have.extend(d.present.iter().filter_map(|a| index.has_sha1(&a.checksum)));
        let (ops, _) = link::link_ops(&s.catalog, &index, &d.account, &d.present, now);
        if !ops.is_empty()
            && let Err(e) = s.apply_system(Op::Batch { ops })
        {
            failed.push(("link".into(), e.to_string()));
        }
    }
    have.retain(|p| s.catalog.photo(*p).is_some_and(|ph| !ph.deleted));
    have.sort();
    have.dedup();
    if let Some(name) = d.album_name.as_deref().map(str::trim).filter(|n| !n.is_empty())
        && !have.is_empty()
    {
        let ids: Vec<u64> = have.iter().map(|p| p.0).collect();
        let r = match album {
            Some(a) => Ok(a),
            None => s.execute("album.create", &json!({"name": name})).map(|a| a["id"].as_u64().unwrap_or_default()),
        }
        .and_then(|a| s.execute("album.addPhotos", &json!({"id": a, "ids": ids})));
        if let Err(e) = r {
            failed.push(("album".into(), e.to_string()));
        }
    }
    s.remote.import =
        remote::ImportProgress { active: false, requested: s.remote.import.requested, imported, skipped: d.skipped, present: have.len(), failed };
}

/// Newly imported photos whose original an Immich link-only photo already stands for: the
/// link-only photo takes the file (as when its original is downloaded) and the new entry goes,
/// so the catalog holds the photo once. → `(new id, the link-only photo that took its file)`.
pub(crate) fn merge_into_link_only(s: &mut Session, ids: &[PhotoId]) -> Vec<(PhotoId, PhotoId)> {
    let mut by_sha1: std::collections::HashMap<String, (PhotoId, String)> = std::collections::HashMap::new();
    for p in s.catalog.photos().filter(|p| p.preview_only.as_deref() == Some(LINK_ONLY) && !p.deleted) {
        for r in s.catalog.remote_of(p.id).filter(|r| r.service == SERVICE) {
            if let Some(d) = r.remote_checksum.as_deref().and_then(lcc_hash::Sha1Digest::parse) {
                by_sha1.entry(d.to_hex()).or_insert((p.id, r.account_id.clone()));
            }
        }
    }
    if by_sha1.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(*id) else { continue };
        if ph.preview_only.is_some() || ph.copy_of.is_some() {
            continue;
        }
        let (Some(sha1), Source::File { path }) = (ph.sha1.clone(), ph.source.clone()) else { continue };
        let Some((keep, account)) = by_sha1.remove(&sha1) else { continue };
        let albums = s.catalog.albums_of(*id);
        if let Err(e) = adopt_original(s, keep, &account, Path::new(&path)) {
            log::warn!("import: the Immich photo couldn't take `{path}`: {e}");
            continue;
        }
        for a in albums {
            if let Some(al) = s.catalog.album(a) {
                let mut photos: Vec<PhotoId> = al.photos.iter().copied().filter(|p| p != id).collect();
                if !photos.contains(&keep) {
                    photos.push(keep);
                }
                let _ = s.apply_system(Op::SetAlbumPhotos { id: a, photos });
            }
        }
        if let Err(e) = s.apply_system(Op::RemovePhoto { id: *id }) {
            log::warn!("import: {e}");
        }
        out.push((*id, keep));
    }
    out
}

/// A link-only photo's original arrived: point the photo at it.
fn adopt_original(s: &mut Session, id: PhotoId, account: &str, path: &Path) -> Result<()> {
    let abs = path.to_string_lossy().to_string();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| abs.clone());
    let format = path.extension().map(|e| e.to_string_lossy().to_uppercase());
    let mut ops = vec![Op::Relink { id, file_name: name, source: Source::File { path: abs.clone() }, format }];
    let info = if s.media.file_probe.is_some() { crate::import::probe_paths(s, std::slice::from_ref(&abs)).pop().and_then(|r| r.ok()) } else { None };
    let ph = s.catalog.photo(id).ok_or_else(|| bad("immich.fetchOriginal", "the photo is gone"))?;
    match info {
        Some(info) => {
            let sha1 = info.sha1.clone();
            let mut info = info;
            info.preview_only = None;
            ops.push(Op::SetContent {
                id,
                width: info.width,
                height: info.height,
                file_size: info.file_size,
                content_hash: info.content_hash.clone(),
                preview_only: None,
                preview_checked: String::new(),
            });
            ops.push(Op::SetSha1 { id, sha1 });
            // the preview was a JPEG; the original may be a raw, a HEIF…
            if ph.kind != info.kind || ph.format != info.format {
                ops.push(Op::SetKind { id, kind: info.kind, format: info.format.clone() });
            }
        }
        None => ops.push(Op::SetContent {
            id,
            width: ph.width,
            height: ph.height,
            file_size: ph.file_size,
            content_hash: ph.content_hash.clone(),
            preview_only: None,
            preview_checked: String::new(),
        }),
    }
    let _ = account;
    s.apply_system(Op::Batch { ops })?;
    Ok(())
}

/// The folders of library photos (distinct parents), at most `cap`.
fn photo_folders(s: &Session, cap: usize) -> Vec<String> {
    let mut set = BTreeSet::new();
    // (link-only previews live in the app's cache, not in a library folder)
    for p in s.catalog.photos().filter(|p| p.in_library() && p.preview_only.as_deref() != Some(LINK_ONLY)) {
        if let Source::File { path } = &p.source
            && let Some(parent) = Path::new(path).parent()
        {
            set.insert(parent.to_string_lossy().to_string());
            if set.len() >= cap {
                break;
            }
        }
    }
    set.into_iter().collect()
}

/// External libraries and which catalog folders they cover (IMM-EXTLIB setup helper).
fn libraries(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.libraries";
    let id = account_param(s, p, C)?;
    let (acc, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    // the catalog's part here; the server's on the worker
    let folders = photo_folders(s, 2000);
    let work = move || {
        let libs = match client.libraries() {
            Ok(l) => l,
            Err(e) => return failure(&e),
        };
        let cov = extlib::coverage(&folders, &libs, &acc.path_maps);
        json!({
            "ok": true,
            "libraries": libs,
            "pathMaps": acc.path_maps,
            "suggested": extlib::suggest(&folders, &libs),
            "coverage": cov,
        })
    };
    if bool_or(p, "background", false) {
        return Ok(ask(s, ask_key(C, &id), "Reading Immich's external libraries", move |_| work()));
    }
    Ok(work())
}

fn set_path_maps(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.setPathMaps";
    let id = account_param(s, p, C)?;
    let maps: Vec<extlib::PathMap> = serde_json::from_value(p.get("pathMaps").cloned().unwrap_or(Value::Array(vec![])))
        .map_err(|e| bad(C, format!("`pathMaps` must be [{{container, local}}]: {e}")))?;
    if maps.iter().any(|m| m.container.trim().is_empty() || m.local.trim().is_empty()) {
        return Err(bad(C, "every row needs a container path and a local folder"));
    }
    let accs = s.immich_accounts().map_err(|e| bad(C, e))?;
    let a = accs.get_mut(&id).ok_or_else(|| bad(C, "no such account"))?;
    a.path_maps = maps.clone();
    s.save_accounts().map_err(|e| bad(C, e))?;
    Ok(json!({"pathMaps": maps}))
}

/// Write XMP sidecars for the photos in folders an external library covers (by the path mapping),
/// so Immich picks up ratings, descriptions and keywords on its next scan.
fn write_sidecars(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.writeSidecars";
    let id = account_param(s, p, C)?;
    let maps = s.immich_accounts().map_err(|e| bad(C, e))?.get(&id).map(|a| a.path_maps.clone()).unwrap_or_default();
    if maps.is_empty() {
        return Err(bad(C, "no path mapping: map the external library's folders first (immich.setPathMaps)"));
    }
    let ids: Vec<u64> = s
        .catalog
        .photos()
        .filter(|ph| ph.in_library() && ph.copy_of.is_none())
        .filter(|ph| matches!(&ph.source, Source::File { path } if extlib::to_container(&maps, path).is_some()))
        .map(|ph| ph.id.0)
        .collect();
    if ids.is_empty() {
        return Ok(json!({"written": 0, "failed": []}));
    }
    let r = s.execute("photo.saveMetadataToFile", &json!({"ids": ids}))?;
    let mut out = json!({"written": r["written"].as_array().map(Vec::len).unwrap_or(0) + r["merged"].as_array().map(Vec::len).unwrap_or(0), "failed": r["failed"]});
    // Immich's library scan skips files that didn't change: ask it to re-read the linked assets
    if bool_or(p, "refresh", true) {
        let assets: Vec<String> = ids
            .iter()
            .filter_map(|i| s.catalog.remote_of(PhotoId(*i)).find(|r| r.service == SERVICE && r.account_id == id).map(|r| r.remote_id.clone()))
            .collect();
        if !assets.is_empty() {
            match s.immich_client(&id) {
                Ok((_, c)) => {
                    // new sidecars are found by the discovery job (admin keys); changed ones by a refresh
                    if let Err(e) = c.discover_sidecars() {
                        out["discoverError"] = failure(&e)["error"].clone();
                    }
                    match c.refresh_metadata(&assets) {
                        Ok(()) => out["refreshed"] = json!(assets.len()),
                        Err(e) => out["refreshError"] = failure(&e)["error"].clone(),
                    }
                }
                Err(e) => out["refreshError"] = failure(&e)["error"].clone(),
            }
        }
    }
    Ok(out)
}

/// Ask Immich to rescan the external libraries that cover mapped folders (or `library`), so it
/// reads new files and the XMP sidecars just written. Immich scans in the background.
fn scan_libraries(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.scanLibraries";
    let id = account_param(s, p, C)?;
    let (acc, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let library = str_param(p, "library").map(str::to_string);
    let folders = if library.is_some() { Vec::new() } else { photo_folders(s, 2000) };
    let work = move || {
        let ids: Vec<String> = match library {
            Some(l) => vec![l],
            None => {
                let libs = match client.libraries() {
                    Ok(l) => l,
                    Err(e) => return failure(&e),
                };
                let cov = extlib::coverage(&folders, &libs, &acc.path_maps);
                let used: BTreeSet<String> = cov.iter().filter_map(|c| c.library.clone()).collect();
                libs.into_iter().map(|l| l.id).filter(|l| used.contains(l)).collect()
            }
        };
        for l in &ids {
            if let Err(e) = client.scan_library(l) {
                return failure(&e);
            }
        }
        json!({"ok": true, "scanned": ids})
    };
    if bool_or(p, "background", false) {
        return Ok(ask(s, ask_key(C, &id), "Rescanning Immich's external libraries", move |_| work()));
    }
    Ok(work())
}

pub fn specs() -> Vec<CommandSpec> {
    let mut v = sync::specs();
    v.extend(publish::specs());
    v.extend(versions::specs());
    v.extend(sidecars::specs());
    v.extend(base_specs());
    v
}

fn base_specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "remote.pump", "Remote Work", [], None, "{sha1?: bool}: cheap, every frame: takes in finished background work (SHA-1 back-fill of originals, Immich link passes, imports, original downloads, background calls), starts the next SHA-1 batch and asks the connected servers for the checksums of photos just imported or hashed → {sha1, links, import, fetching, fetchErrors, asking, answers}", always, pump),
        cmd!(query "immich.test", "Test Immich Connection", [], None, "{url, apiKey, pinned?}: contact the server (not saved; never journaled) → {ok, url, version, user, permissions, missingPermissions} or {ok: false, error: {kind, message, retryable, fingerprint?}}", always, test),
        cmd!(query "immich.connect", "Connect Immich Server", [], None, "{url, apiKey, pinned?: certificate fingerprint the user confirmed, background?: bool}: check the server (version ≥ 3.0) and key, store the key in the secret store (system keychain or the unlocked key file) and the account in settings (never journaled) → {ok, account, …} or {ok: false, error}; background → {started}, the result arrives as immich.status `connected`", always, connect),
        cmd!(
            "immich.disconnect",
            "Disconnect Immich Server",
            [],
            None,
            "{account?, forgetLinks?: bool}: remove the account and its stored key → {disconnected, forgotLinks}",
            always,
            disconnect
        ),
        cmd!(
            "immich.collections",
            "Show Immich Albums as Collections",
            [],
            None,
            "{account?, on: bool}: the account's albums show as collections in device sync (Settings ▸ Sync, under \"Immich\"); each device then picks per album none, previews or fullQuality → {account, collections}",
            always,
            collections::set_shown
        ),
        cmd!(
            "immich.share",
            "Share Immich Connection with Synced Devices",
            [],
            None,
            "{account?, on: bool}: share the connection with the devices this catalog syncs with: its API key goes to them over the encrypted device-sync channel into their key storage (never the catalog, replica, logs or results), and its albums' collections are told to them. off: the connection stays on this device and its collections are not told (devices that already got the key keep it: revoke the key in Immich to stop them) → {account, shared}",
            always,
            collections::set_shared
        ),
        cmd!(query "immich.status", "Immich Status", [], None, "{check?: bool (also contact each server), background?: bool (do that on workers; results in later calls' `server`)} → {accounts: [{id, url, userName, version, permissions, linked, probable, link, server?, checking}], secretStore, connecting, connected?}", always, status),
        cmd!(query "immich.answer", "Immich Background Result", [], None, "{key, keep?: bool}: the result of a call made with background: true (immich.libraries, immich.authStatus, immich.unlockSession, immich.scanLibraries, immich.importPeople, immich.pushPeople; they return {started, key}); taken once unless keep → {done: true, result} or {done: false, running}; remote.pump lists the waiting keys in `answers`", always, answer),
        cmd!(query "immich.authStatus", "Immich Auth Status", [], None, "{account?, background?: bool (on a worker: {started, key}, the result through immich.answer)}: whether this app's server session is elevated (locked folders visible to its listings), and the unlock secrets the account has → {ok, account, isElevated, password, pinCode, expiresAt?, pinExpiresAt?} or {ok: false, error}", always, auth_status),
        cmd!(query "immich.unlockSession", "Unlock Immich Locked Folders", [], None, "{account?, secret, background?: bool (on a worker: {started, key}, the result through immich.answer)}: elevate the server session with the account's PIN (4–6 digits, sent as pinCode) or password (anything else), so link, import and sync see locked-folder assets too (never journaled; the session locks again later) → {ok, account} or {ok: false, error}", always, unlock_session),
        cmd!(
            "immich.link",
            "Link Photos with Immich",
            ["Library", "Immich"],
            None,
            "{account?, full?: bool}: start a background pass that lists the server's assets (those changed since this catalog's last complete pass; all of them with full: the pass to run after adding photos the server has had for a while, should the automatic checksum lookup at import have been missed) and links them to catalog photos by SHA-1, else as probable by name + capture time + size; holds an activity row, can be cancelled → {started}",
            always,
            start_link
        ),
        cmd!(
            "immich.confirmLink",
            "Confirm Immich Link",
            [],
            None,
            "{ids?, account?}: confirm probable links of the photos (default: selection) → {confirmed}",
            always,
            confirm
        ),
        cmd!("immich.unlink", "Remove Immich Link", [], None, "{ids?, account?}: remove the photos' Immich links → {removed}", always, unlink),
        cmd!(query "immich.links", "Immich Links", [], None, "{id?} → {state: linked|probable|none, links: [{account, assetId, state, url}], linkOnly}", always, links_of),
        cmd!(query "immich.browse", "Browse Immich", [], None, "{account?, source: timeline|favorites|album|person|albums|people, id?, page?, size?} → {assets: [{id, fileName, captured, favorite, rating, inCatalog}], nextPage} | {albums} | {people}", always, browse),
        cmd!(query "immich.thumbnail", "Immich Thumbnail", [], None, "{account?, assetId, size?: thumbnail|preview, path?}: save an asset's thumbnail → {path}", always, thumbnail),
        cmd!(
            "immich.import",
            "Import from Immich",
            [],
            None,
            "{account?, assets: [ids], mode: copy|link, destination? (copy: required; a dated folder is made inside), albumName?}: in the background: copy downloads originals and imports them; link catalogs a preview and downloads the original on first Develop; rating, favourite (pick), description, tags and GPS are taken over; assets already in the catalog (linked, or the same checksum) are not downloaded again: they are linked to their photos and added to the album → {started}; progress in remote.pump `import` {active, requested, imported, skipped, present, failed}",
            always,
            import
        ),
        cmd!(
            "immich.fetchOriginal",
            "Download Original from Immich",
            ["Library", "Immich"],
            None,
            "{id?}: download a link-only photo's original (in the background) → {started}",
            always,
            fetch_original
        ),
        cmd!(query "immich.libraries", "Immich External Libraries", [], None, "{account?, background?: bool (on a worker: {started, key}, the result through immich.answer)} → {libraries, pathMaps, suggested, coverage: [{folder, container, library, libraryName, importPath}]}", always, libraries),
        cmd!(
            "immich.setPathMaps",
            "Set Immich Path Mapping",
            [],
            None,
            "{account?, pathMaps: [{container, local}]}: how Immich's container paths map to local folders",
            always,
            set_path_maps
        ),
        cmd!(
            "immich.writeSidecars",
            "Write XMP for Immich",
            [],
            None,
            "{account?, refresh?: bool = true}: write XMP sidecars for photos in mapped external-library folders and ask Immich to re-read the linked ones (refresh-metadata), so it shows their ratings, descriptions and keywords → {written, failed, refreshed?, refreshError?}",
            always,
            write_sidecars
        ),
        cmd!(
            "immich.scanLibraries",
            "Rescan Immich External Libraries",
            [],
            None,
            "{account?, library?, background?: bool (on a worker: {started, key}, the result through immich.answer)}: ask Immich to rescan the external libraries covering mapped folders (or `library`), so it reads new files and XMP sidecars → {ok, scanned: [library ids]}",
            always,
            scan_libraries
        ),
    ]
}
