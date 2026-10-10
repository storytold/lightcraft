//! IMM-SYNC (two-way metadata sync), IMM-PEOPLE (faces and names) and IMM-SEARCH (smart search)
//! commands. The merge itself is pure and lives in `dac_immich::sync`; this module gathers the
//! catalog side, runs the server side on a worker, and takes the result in (`remote.pump`).
//!
//! A sync run, per account:
//! 1. on the session thread: the linked photos' catalog snapshots and the state of the last run;
//! 2. on a worker: assets changed since the last complete run (`updatedAfter`), plus every linked
//!    asset that changed locally, was never synced or has a conflict, each fetched with its tags;
//!    the three-way merge; unless a dry run, the Immich side is written (asset update, tags);
//! 3. back on the session thread: catalog changes are applied (unless the photo changed again
//!    meanwhile), the new base values, conflicts and the deletion queue are stored in
//!    `<library>/Immich/sync/<account>.json`, and an activity-log entry is added.
//!
//! Deletions never sync on their own: a trashed asset or a photo in Recently Deleted goes into a
//! queue the user confirms (`immich.resolveDeletion`). Offline or auth failures leave everything as
//! it was; a background schedule retries with backoff.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dac_catalog::{Catalog, Op, PhotoId, SyncState as LinkState};
use dac_immich::link::SERVICE;
use dac_immich::sync::{self as merge, Deletion, Field, ItemState, Outcome, RunLog, Side, Sides, Snapshot, SyncConfig, SyncState};
use dac_immich::types::{MetadataSearch, SmartSearch};
use dac_immich::{Asset, ImmichError, people};
use serde_json::{Value, json};

use super::{account_param, failure};
use crate::cmd::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::remote::Msg;
use crate::{Result, Session};

/// Most assets listed per run (1000 per page).
const LIST_PAGES: u32 = 1000;
/// Most changes a dry run reports.
const DRY_CAP: usize = 1000;
/// Most photos one `immich.importPeople` call reads faces for.
const PEOPLE_CAP: usize = 5000;

/// One account's sync: its state between runs and the background schedule.
#[derive(Default)]
pub(crate) struct AccountSync {
    state: Option<SyncState>,
    pub(crate) running: bool,
    last: Option<Value>,
    next_due: Option<Instant>,
    failures: u32,
}

/// One linked pair as the worker gets it.
struct Pair {
    asset: String,
    photo: PhotoId,
    local: Snapshot,
    deleted: bool,
    file_name: String,
    item: ItemState,
}

/// One pair after the worker.
pub(crate) struct PairDone {
    asset: String,
    photo: PhotoId,
    local_used: Snapshot,
    remote: Snapshot,
    remote_updated_at: Option<String>,
    outcome: Outcome,
    pushed: bool,
    error: Option<String>,
}

/// What a sync worker brings back.
pub(crate) struct SyncDone {
    account: String,
    dry_run: bool,
    started: String,
    pairs: Vec<PairDone>,
    newest: Option<String>,
    deletions: Vec<Deletion>,
    missing: Vec<String>,
    checked: u64,
    errors: Vec<String>,
    fatal: Option<ImmichError>,
}

fn excluded(c: &Catalog) -> impl Fn(&str) -> bool + '_ {
    |k: &str| c.keyword_info(k.trim()).is_some_and(|i| !i.include_on_export)
}

fn state_path(s: &Session, account: &str) -> Option<PathBuf> {
    let name: String = account.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).take(150).collect();
    s.immich_dir().map(|d| d.join("sync").join(format!("{name}.json")))
}

/// The account's sync state (loaded from its file on first use; a corrupt file starts over and
/// says so in the log rather than failing every run).
fn state_of<'a>(s: &'a mut Session, account: &str) -> &'a mut SyncState {
    let path = state_path(s, account);
    let entry = s.remote.sync.entry(account.to_string()).or_default();
    entry.state.get_or_insert_with(|| match path.as_ref().map(std::fs::read) {
        Some(Ok(b)) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            log::warn!("Immich sync state {}: {e}; starting over", path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
            SyncState::default()
        }),
        _ => SyncState::default(),
    })
}

fn save_state(s: &mut Session, account: &str) -> std::result::Result<(), String> {
    let Some(path) = state_path(s, account) else { return Ok(()) };
    let Some(st) = s.remote.sync.get(account).and_then(|a| a.state.as_ref()) else { return Ok(()) };
    let json = serde_json::to_vec(st).map_err(|e| e.to_string())?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    // temp file + rename; a failed write (full disk) removes the temp file
    dac_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))
}

fn config_of(s: &mut Session, account: &str) -> SyncConfig {
    s.immich_accounts().ok().and_then(|a| a.get(account).map(|x| x.sync.clone())).unwrap_or_default()
}

/// The confirmed links of `account` (probable links never sync).
fn linked(s: &Session, account: &str) -> Vec<(PhotoId, String)> {
    s.catalog
        .remote_links()
        .iter()
        .filter(|r| r.service == SERVICE && r.account_id == account && r.sync_state != LinkState::Probable)
        .map(|r| (r.photo_id, r.remote_id.clone()))
        .collect()
}

/// Start a run: gather the catalog side, then work on a worker (`background`) or right here.
fn start(s: &mut Session, account: &str, dry_run: bool, full: bool, background: bool) -> Result<Value> {
    if s.remote.sync.get(account).is_some_and(|a| a.running) {
        return Ok(json!({"started": false, "reason": "a sync of this account is already running"}));
    }
    let (_, client) = match s.immich_client(account) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let cfg = config_of(s, account);
    let now = (s.clock)();
    let links = linked(s, account);
    // the catalog side, and when each local change was first seen
    let mut pairs = Vec::with_capacity(links.len());
    let snaps: Vec<(PhotoId, String, Snapshot, bool, String)> = {
        let ex = excluded(&s.catalog);
        links
            .iter()
            .filter_map(|(id, asset)| {
                let p = s.catalog.photo(*id)?;
                Some((*id, asset.clone(), merge::local(p, &cfg, &ex), p.deleted, p.file_name.clone()))
            })
            .collect()
    };
    let st = state_of(s, account);
    for (photo, asset, local, deleted, file_name) in snaps {
        let item = st.items.entry(asset.clone()).or_default();
        item.photo = photo.0;
        let changed = item.base.as_ref().is_some_and(|b| *b != local);
        if !changed {
            item.local_changed_at = None;
        } else if item.local_changed_at.is_none() {
            item.local_changed_at = Some(now.clone());
        }
        pairs.push(Pair { asset, photo, local, deleted, file_name, item: item.clone() });
    }
    let since = if full { None } else { st.synced_until.clone() };
    let kept = st.kept.clone();
    let entry = s.remote.sync.entry(account.to_string()).or_default();
    entry.running = true;
    let job = Job { account: account.to_string(), dry_run, since, cfg, pairs, kept, started: now };
    if background {
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Synced(Box::new(work(&client, job))));
        });
        return Ok(json!({"started": true}));
    }
    let done = work(&client, job);
    Ok(finish(s, done))
}

struct Job {
    account: String,
    dry_run: bool,
    since: Option<String>,
    cfg: SyncConfig,
    pairs: Vec<Pair>,
    kept: BTreeSet<String>,
    started: String,
}

/// The server side of a run (a worker thread, or inline).
fn work(c: &dac_immich::Client, job: Job) -> SyncDone {
    let Job { account, dry_run, since, cfg, pairs, kept, started } = job;
    let mut done = SyncDone {
        account,
        dry_run,
        started,
        pairs: Vec::new(),
        newest: None,
        deletions: Vec::new(),
        missing: Vec::new(),
        checked: 0,
        errors: Vec::new(),
        fatal: None,
    };
    // 1. what changed on the server since the last complete run
    let wanted: BTreeSet<&str> = pairs.iter().map(|p| p.asset.as_str()).collect();
    let mut changed: BTreeSet<String> = BTreeSet::new();
    let q = MetadataSearch { size: Some(1000), updated_after: since.clone(), with_deleted: Some(true), ..Default::default() };
    let listed = c.search_all(&q, LIST_PAGES, |page| {
        for a in page {
            if a.updated_at.as_ref().is_some_and(|u| done.newest.as_ref().is_none_or(|n| u > n)) {
                done.newest = a.updated_at.clone();
            }
            if wanted.contains(a.id.as_str()) {
                changed.insert(a.id.clone());
            }
        }
        true
    });
    if let Err(e) = listed {
        done.fatal = Some(e);
        return done;
    }
    // 2. the candidates, each fetched with its tags
    let mut fetched: Vec<(Pair, Asset)> = Vec::new();
    for p in pairs {
        let local_changed = p.item.base.as_ref().is_none_or(|b| *b != p.local);
        let pending = !p.item.conflicts.is_empty() || !p.item.forced.is_empty();
        if !(since.is_none() || changed.contains(&p.asset) || local_changed || pending || p.deleted) {
            continue;
        }
        done.checked += 1;
        let a = match c.asset(&p.asset) {
            Ok(a) => a,
            Err(ImmichError::NotFound(_)) => {
                done.missing.push(p.asset.clone());
                continue;
            }
            Err(e)
                if e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey | ImmichError::Tls(_) | ImmichError::Untrusted { .. }) =>
            {
                done.fatal = Some(e);
                return done;
            }
            Err(e) => {
                done.errors.push(format!("{}: {e}", p.file_name));
                continue;
            }
        };
        if merge::untouchable(&a) {
            continue;
        }
        if a.is_trashed || p.deleted {
            let both = a.is_trashed && p.deleted;
            if !kept.contains(&p.asset) && !both {
                done.deletions.push(Deletion {
                    asset: p.asset.clone(),
                    photo: p.photo.0,
                    side: if a.is_trashed { "immich" } else { "catalog" }.into(),
                    file_name: p.file_name.clone(),
                    detected: done.started.clone(),
                });
            }
            continue;
        }
        fetched.push((p, a));
    }
    // 3. the merge
    let mut results: Vec<(PairDone, Asset)> = Vec::new();
    for (p, a) in fetched {
        let remote = merge::remote(&a);
        let stuck: BTreeSet<Field> = p.item.conflicts.keys().copied().collect();
        let outcome = merge::reconcile(
            Sides {
                base: p.item.base.as_ref(),
                local: &p.local,
                remote: &remote,
                stuck: &stuck,
                forced: &p.item.forced,
                local_changed_at: p.item.local_changed_at.as_deref(),
                remote_updated_at: a.updated_at.as_deref(),
            },
            &cfg,
        );
        results.push((
            PairDone {
                asset: p.asset,
                photo: p.photo,
                local_used: p.local,
                remote,
                remote_updated_at: a.updated_at.clone(),
                outcome,
                pushed: true,
                error: None,
            },
            a,
        ));
    }
    // 4. the Immich side
    if !dry_run {
        push(c, &cfg, &mut results, &mut done);
    }
    done.pairs = results.into_iter().map(|(p, _)| p).collect();
    done
}

/// Write the outcomes' Immich side: asset updates, then tags (created once, assigned in bulk).
fn push(c: &dac_immich::Client, cfg: &SyncConfig, results: &mut [(PairDone, Asset)], done: &mut SyncDone) {
    for (p, a) in results.iter_mut() {
        if p.outcome.to_remote.is_empty() {
            continue;
        }
        let zone = a.exif_info.as_ref().and_then(|e| e.date_time_original.as_deref()).and_then(merge::zone_of);
        let u = merge::remote_update(&p.outcome.target, &p.outcome.to_remote, cfg, zone);
        if !u.is_empty()
            && let Err(e) = c.update_asset(&p.asset, &u)
        {
            p.pushed = false;
            p.error = Some(e.to_string());
            if e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey) {
                done.fatal.get_or_insert(e);
                return;
            }
        }
    }
    // tags: create the missing ones, then assign / remove per tag
    let wanted: BTreeSet<String> =
        results.iter().filter(|(p, _)| p.pushed).flat_map(|(p, _)| p.outcome.add_tags.iter().map(|k| merge::keyword_to_tag(k))).collect();
    let mut ids: HashMap<String, String> = HashMap::new();
    if !wanted.is_empty() {
        match c.upsert_tags(&wanted.iter().cloned().collect::<Vec<_>>()) {
            Ok(tags) => ids.extend(tags.into_iter().map(|t| (t.value, t.id))),
            Err(e) => {
                done.errors.push(format!("tags: {e}"));
                for (p, _) in results.iter_mut().filter(|(p, _)| !p.outcome.add_tags.is_empty()) {
                    p.pushed = false;
                    p.error = Some(format!("tags: {e}"));
                }
            }
        }
    }
    let mut add: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut remove: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (p, a)) in results.iter().enumerate().filter(|(_, (p, _))| p.pushed) {
        for k in &p.outcome.add_tags {
            if let Some(id) = ids.get(&merge::keyword_to_tag(k)) {
                add.entry(id.clone()).or_default().push(i);
            }
        }
        for k in &p.outcome.remove_tags {
            if let Some(t) = a.tags.iter().find(|t| dac_immich::mapping::tag_to_keyword(&t.value) == *k) {
                remove.entry(t.id.clone()).or_default().push(i);
            }
        }
    }
    for (tag, idx, adding) in add.into_iter().map(|(t, i)| (t, i, true)).chain(remove.into_iter().map(|(t, i)| (t, i, false))) {
        let assets: Vec<String> = idx.iter().filter_map(|i| results.get(*i).map(|(p, _)| p.asset.clone())).collect();
        let r = if adding { c.tag_assets(&tag, &assets) } else { c.untag_assets(&tag, &assets) };
        if let Err(e) = r {
            done.errors.push(format!("tags: {e}"));
            for i in idx {
                if let Some((p, _)) = results.get_mut(i) {
                    p.pushed = false;
                    p.error = Some(format!("tags: {e}"));
                }
            }
        }
    }
}

/// Take a finished run in: catalog changes, the new state, the log → the run's summary.
pub(crate) fn finish(s: &mut Session, d: SyncDone) -> Value {
    let now = (s.clock)();
    let account = d.account.clone();
    let cfg = config_of(s, &account);
    let (mut pulled, mut pushed, mut conflicts) = (0u64, 0u64, 0u64);
    let mut errors = d.errors.clone();
    let mut changes = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut link_ops: Vec<Op> = Vec::new();
    let mut new_items: Vec<(String, ItemState)> = Vec::new();
    {
        let st = state_of(s, &account).clone();
        let ex = excluded(&s.catalog);
        for p in &d.pairs {
            let o = &p.outcome;
            if d.dry_run {
                if !o.is_noop() && changes.len() < DRY_CAP {
                    let vals = |fs: &BTreeSet<Field>, from: &Snapshot| -> Value {
                        Value::Object(fs.iter().map(|f| (f.name().to_string(), from.value(*f))).collect())
                    };
                    changes.push(json!({
                        "photo": p.photo.0,
                        "assetId": p.asset,
                        "toCatalog": vals(&o.to_local, &o.target),
                        "toImmich": vals(&o.to_remote, &o.target),
                        "conflicts": o.conflicts.iter().map(|f| json!({"field": f.name(), "catalog": p.local_used.value(*f), "immich": p.remote.value(*f)})).collect::<Vec<_>>(),
                    }));
                }
                pulled += u64::from(!o.to_local.is_empty());
                pushed += u64::from(!o.to_remote.is_empty());
                conflicts += o.conflicts.len() as u64;
                continue;
            }
            if let Some(e) = &p.error {
                errors.push(format!("photo {}: {e}", p.photo.0));
            }
            if !p.pushed {
                continue;
            }
            let Some(photo) = s.catalog.photo(p.photo) else { continue };
            let mut item = st.items.get(&p.asset).cloned().unwrap_or_default();
            let current = merge::local(photo, &cfg, &ex);
            if !o.to_local.is_empty() {
                if current == p.local_used {
                    ops.extend(merge::local_ops(photo, &o.target, &o.to_local, &cfg, &ex));
                    pulled += 1;
                } else {
                    // edited again while the run was out: the next run sends the newer edit
                    errors.push(format!("photo {}: changed during the sync; synced next time", p.photo.0));
                }
            }
            pushed += u64::from(!o.to_remote.is_empty());
            conflicts += o.conflicts.len() as u64;
            item.photo = p.photo.0;
            item.base = Some(o.target.clone());
            item.local_changed_at = None;
            item.forced.clear();
            let old = std::mem::take(&mut item.conflicts);
            for f in &o.conflicts {
                let detected = old.get(f).map(|c| c.detected.clone()).unwrap_or_else(|| now.clone());
                item.conflicts.insert(*f, merge::ConflictValues { catalog: p.local_used.value(*f), immich: p.remote.value(*f), detected });
            }
            // the link's badge state
            if let Some(r) = s.catalog.remote_of(p.photo).find(|r| r.service == SERVICE && r.account_id == account) {
                let want = if item.conflicts.is_empty() { LinkState::Synced } else { LinkState::Conflict };
                if r.sync_state != want || (r.remote_updated_at != p.remote_updated_at && !o.is_noop()) {
                    let mut r = r.clone();
                    r.sync_state = want;
                    r.remote_updated_at = p.remote_updated_at.clone();
                    r.last_synced_at = Some(now.clone());
                    link_ops.push(Catalog::link_remote_op(r));
                }
            }
            new_items.push((p.asset.clone(), item));
        }
    }
    if !d.dry_run {
        for chunk in [ops, link_ops] {
            if !chunk.is_empty()
                && let Err(e) = s.apply_system(Op::Batch { ops: chunk })
            {
                errors.push(format!("catalog: {e}"));
            }
        }
    }
    let fatal = d.fatal.as_ref().map(failure);
    let st = state_of(s, &account);
    if !d.dry_run {
        for (k, v) in new_items {
            st.items.insert(k, v);
        }
        for del in &d.deletions {
            if !st.deletions.iter().any(|x| x.asset == del.asset) {
                st.deletions.push(del.clone());
            }
        }
        if d.fatal.is_none()
            && let Some(n) = &d.newest
            && st.synced_until.as_ref().is_none_or(|u| n > u)
        {
            st.synced_until = Some(n.clone());
        }
    }
    let mut log_errors = errors.clone();
    if let Some(e) = &d.fatal {
        log_errors.insert(0, e.to_string());
    }
    log_errors.truncate(50);
    st.push_log(RunLog {
        started: d.started.clone(),
        finished: now,
        dry_run: d.dry_run,
        checked: d.checked,
        pulled,
        pushed,
        conflicts,
        errors: log_errors,
    });
    let open_conflicts = st.conflict_count();
    let queued = st.deletions.len();
    if let Err(e) = save_state(s, &account) {
        errors.push(format!("sync state: {e}"));
    }
    let mut v = json!({
        "ok": d.fatal.is_none(),
        "account": account,
        "dryRun": d.dry_run,
        "checked": d.checked,
        "pulled": pulled,
        "pushed": pushed,
        "conflicts": conflicts,
        "openConflicts": open_conflicts,
        "deletionsQueued": queued,
        "missing": d.missing,
        "errors": errors,
    });
    if d.dry_run {
        v["changes"] = json!(changes);
    }
    if let Some(f) = fatal {
        v["error"] = f["error"].clone();
    }
    let entry = s.remote.sync.entry(account).or_default();
    entry.running = false;
    entry.last = Some(v.clone());
    // the schedule: back off after a failure (30 s, 1 min, 2 min … at most 1 h), else the interval
    if d.fatal.as_ref().is_some_and(|e| e.retryable()) {
        entry.failures = entry.failures.saturating_add(1);
        let wait = Duration::from_secs(30u64.saturating_mul(1 << entry.failures.min(7))).min(Duration::from_secs(3600));
        entry.next_due = Some(Instant::now() + wait);
    } else {
        entry.failures = 0;
        entry.next_due = (cfg.interval_minutes > 0).then(|| Instant::now() + Duration::from_secs(u64::from(cfg.interval_minutes) * 60));
    }
    v
}

/// The background schedule (called by `remote.pump` every frame; cheap).
pub(crate) fn tick(s: &mut Session) {
    let due: Vec<String> = match s.remote.accounts.as_ref() {
        Some(a) => a.immich.iter().filter(|a| a.sync.interval_minutes > 0).map(|a| a.id.clone()).collect(),
        None => return,
    };
    for id in due {
        let e = s.remote.sync.entry(id.clone()).or_default();
        if e.running {
            continue;
        }
        match e.next_due {
            None => {
                // first sight this session: wait one interval… or run soon when never synced
                e.next_due = Some(Instant::now() + Duration::from_secs(60));
            }
            Some(t) if Instant::now() >= t => {
                e.next_due = None;
                if let Err(err) = start(s, &id, false, false, true) {
                    log::warn!("Immich background sync: {err}");
                }
                // a start that could not reach the key / server waits a while
                let e = s.remote.sync.entry(id).or_default();
                if !e.running && e.next_due.is_none() {
                    e.next_due = Some(Instant::now() + Duration::from_secs(300));
                }
            }
            Some(_) => {}
        }
    }
}

// ---- commands

fn sync_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.sync";
    let id = account_param(s, p, C)?;
    start(s, &id, bool_or(p, "dryRun", false), bool_or(p, "full", false), bool_or(p, "background", false))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.syncStatus";
    let id = account_param(s, p, C)?;
    let cfg = config_of(s, &id);
    let st = state_of(s, &id).clone();
    let e = s.remote.sync.get(&id);
    let next = e.and_then(|e| e.next_due).map(|t| t.saturating_duration_since(Instant::now()).as_secs());
    let n = p.get("log").and_then(Value::as_u64).unwrap_or(20).min(merge::LOG_CAP as u64) as usize;
    Ok(json!({
        "account": id,
        "running": e.is_some_and(|e| e.running),
        "last": e.and_then(|e| e.last.clone()),
        "nextRunInSeconds": next,
        "syncedUntil": st.synced_until,
        "items": st.items.len(),
        "conflicts": st.conflict_count(),
        "deletions": st.deletions,
        "log": st.log.iter().rev().take(n).collect::<Vec<_>>(),
        "config": cfg,
        "rules": Field::ALL.iter().map(|f| json!({"field": f.name(), "rule": cfg.rule(*f)})).collect::<Vec<_>>(),
    }))
}

fn conflicts(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.syncConflicts";
    let id = account_param(s, p, C)?;
    let st = state_of(s, &id).clone();
    let mut out = Vec::new();
    for (asset, item) in &st.items {
        for (f, c) in &item.conflicts {
            let name = s.catalog.photo(PhotoId(item.photo)).map(|ph| ph.file_name.clone()).unwrap_or_default();
            out.push(json!({
                "photo": item.photo,
                "assetId": asset,
                "fileName": name,
                "field": f.name(),
                "catalog": c.catalog,
                "immich": c.immich,
                "detected": c.detected,
                "decided": item.forced.get(f),
            }));
        }
    }
    Ok(json!({"account": id, "conflicts": out}))
}

/// Decide conflicts: `keep: catalog|immich` for `ids` (photos; default every conflict) and
/// `field` (default every field). The next run applies them (`sync: true` runs it now).
fn resolve(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.resolveConflict";
    let id = account_param(s, p, C)?;
    let side = str_param(p, "keep").and_then(Side::parse).ok_or_else(|| bad(C, "`keep` must be catalog or immich"))?;
    let field = match str_param(p, "field") {
        Some(f) => Some(Field::parse(f).ok_or_else(|| bad(C, format!("unknown field `{f}`")))?),
        None => None,
    };
    let photos: Option<BTreeSet<u64>> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
    let asset = str_param(p, "assetId").map(str::to_string);
    let st = state_of(s, &id);
    let mut n = 0;
    for (a, item) in st.items.iter_mut() {
        if photos.as_ref().is_some_and(|ps| !ps.contains(&item.photo)) || asset.as_ref().is_some_and(|x| x != a) {
            continue;
        }
        let fields: Vec<Field> = item.conflicts.keys().copied().filter(|f| field.is_none_or(|x| x == *f)).collect();
        for f in fields {
            item.forced.insert(f, side);
            n += 1;
        }
    }
    save_state(s, &id).map_err(|e| bad(C, e))?;
    let mut v = json!({"decided": n});
    if n > 0 && bool_or(p, "sync", false) {
        v["sync"] = start(s, &id, false, false, bool_or(p, "background", false))?;
    }
    Ok(v)
}

/// Change the account's sync settings: a partial `SyncConfig` merged into the current one.
fn set_config(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.setSync";
    let id = account_param(s, p, C)?;
    let cur = config_of(s, &id);
    let mut v = serde_json::to_value(&cur).map_err(|e| bad(C, e.to_string()))?;
    if let Some(patch) = p.get("config") {
        dac_develop::presets::deep_merge(&mut v, patch);
    }
    let cfg: SyncConfig = serde_json::from_value(v).map_err(|e| bad(C, format!("`config`: {e}")))?;
    let accs = s.immich_accounts().map_err(|e| bad(C, e))?;
    let a = accs.get_mut(&id).ok_or_else(|| bad(C, "no such account"))?;
    a.sync = cfg.clone();
    s.save_accounts().map_err(|e| bad(C, e))?;
    let e = s.remote.sync.entry(id).or_default();
    e.next_due = (cfg.interval_minutes > 0).then(|| Instant::now() + Duration::from_secs(u64::from(cfg.interval_minutes) * 60));
    Ok(json!({"config": cfg}))
}

/// The deletion queue: `apply` deletes on the other side (a trashed asset's photo goes to Recently
/// Deleted; a deleted photo's asset goes to Immich's trash), `keep` dismisses it.
fn resolve_deletion(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.resolveDeletion";
    let id = account_param(s, p, C)?;
    let apply = match str_param(p, "action") {
        Some("apply") => true,
        Some("keep") => false,
        _ => return Err(bad(C, "`action` must be apply or keep")),
    };
    let assets: BTreeSet<String> =
        p.get("assets").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let st = state_of(s, &id).clone();
    let picked: Vec<Deletion> = st.deletions.iter().filter(|d| assets.is_empty() || assets.contains(&d.asset)).cloned().collect();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    if apply {
        let local: Vec<u64> =
            picked.iter().filter(|d| d.side == "immich").map(|d| d.photo).filter(|ph| s.catalog.photo(PhotoId(*ph)).is_some()).collect();
        if !local.is_empty() {
            s.execute("photo.delete", &json!({"ids": local}))?;
        }
        done.extend(picked.iter().filter(|d| d.side == "immich").map(|d| d.asset.clone()));
        let remote: Vec<String> = picked.iter().filter(|d| d.side == "catalog").map(|d| d.asset.clone()).collect();
        if !remote.is_empty() {
            match s.immich_client(&id).and_then(|(_, c)| c.trash_assets(&remote)) {
                Ok(()) => done.extend(remote),
                Err(e) => failed.push(failure(&e)),
            }
        }
    } else {
        done.extend(picked.iter().map(|d| d.asset.clone()));
    }
    let st = state_of(s, &id);
    st.deletions.retain(|d| !done.contains(&d.asset));
    if !apply {
        st.kept.extend(done.iter().cloned());
    }
    save_state(s, &id).map_err(|e| bad(C, e))?;
    Ok(json!({"resolved": done.len(), "failed": failed}))
}

// ---- people

fn linked_targets(s: &Session, p: &Value, account: &str) -> Vec<(PhotoId, String)> {
    let only: Option<BTreeSet<u64>> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
    linked(s, account).into_iter().filter(|(ph, _)| only.as_ref().is_none_or(|o| o.contains(&ph.0))).collect()
}

/// Read Immich's faces for linked photos into their face regions (marked "from Immich").
fn import_people(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.importPeople";
    let id = account_param(s, p, C)?;
    let mut targets = linked_targets(s, p, &id);
    let more = targets.len() > PEOPLE_CAP;
    targets.truncate(PEOPLE_CAP);
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let people = match client.people_all() {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let hidden = bool_or(p, "includeHidden", false);
    let (mut added, mut removed, mut skipped, mut photos) = (0, 0, 0, 0);
    let mut errors = Vec::new();
    let mut ops = Vec::new();
    for (ph, asset) in targets {
        let faces = match client.faces(&asset) {
            Ok(f) => f,
            Err(e) if e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey | ImmichError::Forbidden(_)) => return Ok(failure(&e)),
            Err(e) => {
                errors.push(format!("{asset}: {e}"));
                continue;
            }
        };
        let Some(photo) = s.catalog.photo(ph) else { continue };
        let (regions, m) = people::regions_with_faces(photo, &faces, hidden);
        skipped += m.skipped;
        if m.added + m.removed > 0 && regions != photo.meta.regions {
            let mut meta = photo.meta.clone();
            meta.regions = regions;
            ops.push(Op::SetMeta { id: ph, meta: Box::new(meta) });
            added += m.added;
            removed += m.removed;
            photos += 1;
        }
    }
    if !ops.is_empty() {
        s.commit("Import People from Immich", Op::Batch { ops })?;
    }
    Ok(json!({
        "ok": true,
        "photos": photos,
        "added": added,
        "removed": removed,
        "skipped": skipped,
        "more": more,
        "errors": errors,
        "people": people.iter().map(|x| json!({"id": x.id, "name": x.name, "birthDate": x.birth_date, "hidden": x.is_hidden})).collect::<Vec<_>>(),
    }))
}

/// Confirm the Immich face regions of the photos (default: selection).
fn confirm_faces(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p);
    let mut ops = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id) else { continue };
        let regions = people::confirm(ph);
        if regions != ph.meta.regions {
            let mut meta = ph.meta.clone();
            meta.regions = regions;
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Confirm Faces from Immich", Op::Batch { ops })?;
    }
    Ok(json!({"confirmed": n}))
}

/// Send names given in the app back to Immich: renames, and with `merge` merges of people now
/// sharing a name. `dryRun` only lists them.
fn push_people(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.pushPeople";
    let id = account_param(s, p, C)?;
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let people = match client.people_all() {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let ids: BTreeSet<PhotoId> = linked(s, &id).into_iter().map(|(p, _)| p).collect();
    let plan = people::names_to_push(ids.iter().filter_map(|i| s.catalog.photo(*i).map(|x| &**x)), &people, bool_or(p, "merge", false));
    let v = json!({
        "renames": plan.renames.iter().map(|(i, n)| json!({"person": i, "name": n})).collect::<Vec<_>>(),
        "merges": plan.merges.iter().map(|(i, o)| json!({"into": i, "people": o})).collect::<Vec<_>>(),
    });
    if bool_or(p, "dryRun", false) {
        return Ok(v);
    }
    if !plan.renames.is_empty()
        && let Err(e) = client.rename_people(&plan.renames)
    {
        return Ok(failure(&e));
    }
    for (into, others) in &plan.merges {
        if let Err(e) = client.merge_people(into, others) {
            return Ok(failure(&e));
        }
    }
    let mut v = v;
    v["ok"] = json!(true);
    Ok(v)
}

// ---- search

/// Immich smart search (CLIP) or metadata search (`mode`: smart|ocr|description|place) → the
/// linked catalog photos. `show` makes them the grid's temporary collection (the filter's photo
/// list, like Find Similar); `saveAs` keeps them as a new album.
fn smart_search(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.smartSearch";
    let id = account_param(s, p, C)?;
    let query = str_param(p, "query").map(str::trim).filter(|q| !q.is_empty()).ok_or_else(|| bad(C, "missing `query`"))?.to_string();
    if query.chars().count() > 1000 {
        return Err(bad(C, "the query is too long (at most 1000 characters)"));
    }
    let size = p.get("size").and_then(Value::as_u64).unwrap_or(250).clamp(1, 1000) as u32;
    let page = p.get("page").and_then(Value::as_u64).unwrap_or(1).clamp(1, 10_000) as u32;
    let (_, client) = match s.immich_client(&id) {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let mode = str_param(p, "mode").unwrap_or("smart");
    let r = match mode {
        "smart" => client.smart_search(&SmartSearch { query: query.clone(), page: Some(page), size: Some(size), kind: Some("IMAGE".into()) }),
        "ocr" | "description" | "place" => {
            let mut q = MetadataSearch { page: Some(page), size: Some(size), ..Default::default() };
            match mode {
                "ocr" => q.ocr = Some(query.clone()),
                "description" => q.description = Some(query.clone()),
                _ => q.city = Some(query.clone()),
            }
            client.search(&q)
        }
        m => return Err(bad(C, format!("unknown mode `{m}` (smart|ocr|description|place)"))),
    };
    let pg = match r {
        Ok(x) => x,
        Err(e) => return Ok(failure(&e)),
    };
    let mut photos: Vec<PhotoId> = Vec::new();
    let mut unmatched = 0;
    for a in &pg.items {
        match s.catalog.photo_of_remote(SERVICE, &id, &a.id) {
            Some(ph) if !photos.contains(&ph) => photos.push(ph),
            Some(_) => {}
            None => unmatched += 1,
        }
    }
    let mut v = json!({
        "ok": true,
        "query": query,
        "mode": mode,
        "photos": photos.iter().map(|x| x.0).collect::<Vec<_>>(),
        "unmatched": unmatched,
        "total": pg.total,
        "nextPage": pg.next_page.and_then(|n| n.parse::<u32>().ok()),
    });
    if bool_or(p, "show", false) {
        // an empty result shows an empty grid, not everything
        s.filter.only = if photos.is_empty() { vec![PhotoId(u64::MAX)] } else { photos.clone() };
        v["shown"] = json!(true);
    }
    if let Some(name) = str_param(p, "saveAs").map(str::trim).filter(|n| !n.is_empty()) {
        let a = s.execute("album.create", &json!({"name": name}))?;
        if !photos.is_empty() {
            s.execute("album.addPhotos", &json!({"id": a["id"], "ids": photos.iter().map(|x| x.0).collect::<Vec<_>>()}))?;
        }
        v["album"] = a["id"].clone();
    }
    Ok(v)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "immich.sync", "Sync Metadata with Immich", ["Library", "Immich"], None, "{account?, dryRun?: bool — only report what would change, full?: bool — look at every linked asset, background?: bool} — two-way metadata sync of linked photos (rating, favourite, archived, description, keywords ↔ tags, location, date; per-field rules in immich.setSync); conflicts wait in immich.syncConflicts, deletions in the queue → {ok, checked, pulled, pushed, conflicts, openConflicts, deletionsQueued, errors, changes? (dry run)} or {started}", always, sync_cmd),
        cmd!(query "immich.syncStatus", "Immich Sync Status", [], None, "{account?, log?: n} → {running, last, nextRunInSeconds, syncedUntil, items, conflicts, deletions, log (activity, newest first), config, rules}", always, status),
        cmd!(query "immich.syncConflicts", "Immich Sync Conflicts", [], None, "{account?} → {conflicts: [{photo, assetId, fileName, field, catalog, immich, detected, decided?}]}", always, conflicts),
        cmd!(query "immich.resolveConflict", "Resolve Immich Sync Conflict", [], None, "{account?, keep: catalog|immich, ids?: photos, assetId?, field?, sync?: bool, background?: bool} — decide conflicts (default: all); the next sync applies them (sync: run it now) → {decided, sync?}", always, resolve),
        cmd!(query "immich.setSync", "Immich Sync Settings", [], None, "{account?, config: {rules?: {rating|favorite|archived|description|keywords|location|captured: {direction: twoWay|toImmich|fromImmich|off, policy: catalog|immich|newest|ask}}, favorite?: pick|fiveStars, description?: caption|title, intervalMinutes?, onPublish?, pushPeople?}} → {config}", always, set_config),
        cmd!(query "immich.resolveDeletion", "Resolve Immich Deletion", [], None, "{account?, assets?: [asset ids] (default all queued), action: apply|keep} — apply: a photo whose asset is in Immich's trash goes to Recently Deleted, an asset whose photo was deleted goes to Immich's trash (never a hard delete); keep: dismiss → {resolved, failed}", always, resolve_deletion),
        cmd!(query "immich.importPeople", "Import People from Immich", ["Library", "Immich"], None, "{account?, ids?: photos (default all linked), includeHidden?: bool} — read Immich's faces and people into the photos' face regions, marked from Immich (unconfirmed ones are replaced on the next import; regions the photo had are kept) → {photos, added, removed, skipped, more, people: [{id, name, birthDate, hidden}]}", always, import_people),
        cmd!(
            "immich.confirmFaces",
            "Confirm Faces from Immich",
            ["Library", "Immich"],
            None,
            "{ids?} — keep the Immich face regions of the photos (default: selection) as confirmed → {confirmed}",
            always,
            confirm_faces
        ),
        cmd!(query "immich.pushPeople", "Send Names to Immich", ["Library", "Immich"], None, "{account?, merge?: bool, dryRun?: bool} — rename Immich people to the names given to their faces in the app; with merge, people that now share a name are merged in Immich → {renames, merges, ok?}", always, push_people),
        cmd!(query "immich.smartSearch", "Immich Smart Search", [], None, "{account?, query, mode?: smart|ocr|description|place, size?, page?, show?: bool — show the results as a temporary collection, saveAs?: album name} — natural-language (CLIP) or metadata search on the server, mapped to linked photos → {photos, unmatched, total, nextPage, shown?, album?}", always, smart_search),
    ]
}
