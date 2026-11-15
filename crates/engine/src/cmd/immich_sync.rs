//! IMM-SYNC (two-way metadata sync), IMM-PEOPLE (faces and names) and IMM-SEARCH (smart search)
//! commands. The merge itself is pure and lives in `lcc_immich::sync`; this module gathers the
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

use lcc_catalog::{Catalog, Op, PhotoId, SyncState as LinkState};
use lcc_immich::link::SERVICE;
use lcc_immich::sync::{self as merge, Deletion, Field, ItemState, Outcome, RunLog, Side, Sides, Snapshot, SyncConfig, SyncState};
use lcc_immich::types::{MetadataSearch, SmartSearch};
use lcc_immich::{Asset, ImmichError, people};
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
    /// The file `state` was loaded from. The state belongs to one library: when another one is
    /// opened (or the library is closed) this no longer matches and the state is loaded again.
    state_file: Option<PathBuf>,
    pub(crate) running: bool,
    last: Option<Value>,
    /// The last dry run's result (Preview Changes), with its list of changes.
    preview: Option<Value>,
    next_due: Option<Instant>,
    failures: u32,
    /// An album run (`immich.syncAlbums`) is on a worker.
    pub(crate) albums_running: bool,
    /// The last album run's summary.
    pub(crate) albums_last: Option<Value>,
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

/// What [`finish`] needs to take a pair's pull back when the catalog write fails.
struct PullUndo {
    /// The fields the catalog batch was to write.
    fields: BTreeSet<Field>,
    old_base: Option<Snapshot>,
    forced: BTreeMap<Field, Side>,
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
    /// RT.C3: the stack merge (None: skipped, e.g. the key lacks stack permissions).
    stacks: Option<StackSync>,
    /// The state file of the library the run was started in ([`finish`] drops a run whose library
    /// is no longer the open one).
    home: Option<PathBuf>,
    /// Queued deletions that are no longer true: the asset is out of Immich's trash again and the
    /// photo is not in Recently Deleted.
    restored: Vec<String>,
}

/// The stack part of a run.
pub(crate) struct StackSync {
    plan: lcc_immich::stacks::Plan,
    base: Vec<lcc_immich::stacks::Group>,
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
///
/// The state is cached in the session per account, but it is one library's: the bases of catalog
/// A say nothing about catalog B's photos. So the cache remembers which file it came from and is
/// dropped as soon as the session's library (hence the file) is another one.
fn state_of<'a>(s: &'a mut Session, account: &str) -> &'a mut SyncState {
    let path = state_path(s, account);
    let entry = s.remote.sync.entry(account.to_string()).or_default();
    if entry.state_file != path {
        entry.state = None;
        entry.last = None;
        entry.preview = None;
        entry.albums_last = None;
        entry.state_file = path.clone();
    }
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
    // (never write one library's state into another's file)
    let Some(st) = s.remote.sync.get(account).filter(|a| a.state_file.as_ref() == Some(&path)).and_then(|a| a.state.as_ref()) else {
        return Ok(());
    };
    let json = serde_json::to_vec(st).map_err(|e| e.to_string())?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    // temp file + rename; a failed write (full disk) removes the temp file
    lcc_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))
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

/// When photo `id`'s metadata was last edited, as far as the catalog knows, for the "newest
/// wins" policy. The catalog keeps no per-field edit time; what it has is the XMP stamp: when the
/// photo's rating, flag and metadata are exactly what was last written to (or read from) its
/// sidecar, that file's modification time is when they were last changed, or shortly after
/// (auto-write XMP). `None` (no sidecar, the catalog changed since, or the stamp is older than
/// the last sync and so can't be the edit): the caller falls back to the time the run noticed.
///
/// Without this the "newest" of an edit made on Monday and synced on Friday was Friday, and it
/// beat anything done in Immich in between.
fn edit_time(s: &Session, id: PhotoId, last_run: Option<&str>) -> Option<String> {
    let p = s.catalog.photo(id)?;
    let stamp = p.xmp.filter(|x| x.fingerprint == p.xmp_fingerprint())?;
    let t = lcc_catalog::dates::local(stamp.file?.mtime)?;
    let at = merge::instant(&t)?;
    if last_run.and_then(merge::instant).is_some_and(|run| at <= run) {
        return None;
    }
    Some(t)
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
    // photos whose catalog side differs from the base and has no change time yet
    let unstamped: Vec<PhotoId> = {
        let st = state_of(s, account);
        snaps
            .iter()
            .filter(|(_, asset, local, ..)| st.items.get(asset).is_some_and(|i| i.local_changed_at.is_none() && i.base.as_ref().is_some_and(|b| b != local)))
            .map(|(photo, ..)| *photo)
            .collect()
    };
    // the last real run: an edit this run is the first to see was made after it
    let last_run = state_of(s, account).log.iter().rev().find(|l| !l.dry_run).map(|l| l.started.clone());
    let edited: HashMap<PhotoId, String> = unstamped.into_iter().filter_map(|id| Some((id, edit_time(s, id, last_run.as_deref())?))).collect();
    let st = state_of(s, account);
    for (photo, asset, local, deleted, file_name) in snaps {
        let item = st.items.entry(asset.clone()).or_default();
        item.photo = photo.0;
        let changed = item.base.as_ref().is_some_and(|b| *b != local);
        if !changed {
            item.local_changed_at = None;
        } else if item.local_changed_at.is_none() {
            // when the edit was made, where the catalog knows; else when this run noticed it
            item.local_changed_at = Some(edited.get(&photo).cloned().unwrap_or_else(|| now.clone()));
        }
        pairs.push(Pair { asset, photo, local, deleted, file_name, item: item.clone() });
    }
    let since = if full { None } else { st.synced_until.clone() };
    let kept = st.kept.clone();
    let queued: BTreeSet<String> = st.deletions.iter().map(|d| d.asset.clone()).collect();
    let base_stacks = st.stacks.clone();
    let asset_of: HashMap<PhotoId, String> = links.iter().map(|(p, a)| (*p, a.clone())).collect();
    let catalog_stacks: Vec<Vec<PhotoId>> = s.catalog.stacks().map(|st| st.photos.clone()).collect();
    let local_stacks = lcc_immich::stacks::local_groups(&catalog_stacks, |p| asset_of.get(&p).cloned());
    let entry = s.remote.sync.entry(account.to_string()).or_default();
    entry.running = true;
    let home = state_path(s, account);
    let job = Job { account: account.to_string(), dry_run, since, cfg, pairs, kept, queued, started: now, local_stacks, base_stacks, home };
    let task =
        s.activity.start("immich", if dry_run { "Previewing the Immich sync" } else { "Syncing metadata with Immich" }, crate::activity::Cancel::No);
    let run = move || {
        let d = work(&client, job);
        drop(task);
        d
    };
    if background {
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Synced(Box::new(run())));
        });
        return Ok(json!({"started": true}));
    }
    let done = run();
    Ok(finish(s, done))
}

struct Job {
    account: String,
    dry_run: bool,
    since: Option<String>,
    cfg: SyncConfig,
    pairs: Vec<Pair>,
    kept: BTreeSet<String>,
    /// Assets with a deletion waiting in the queue: looked at every run, so a deletion that was
    /// undone (restored from the trash) leaves the queue.
    queued: BTreeSet<String>,
    home: Option<PathBuf>,
    started: String,
    /// The catalog's stacks over this account's linked assets, and the last sync's.
    local_stacks: Vec<lcc_immich::stacks::Group>,
    base_stacks: Vec<lcc_immich::stacks::Group>,
}

/// The server side of a run (a worker thread, or inline).
fn work(c: &lcc_immich::Client, job: Job) -> SyncDone {
    let Job { account, dry_run, since, cfg, pairs, kept, queued, started, local_stacks, base_stacks, home } = job;
    let linked_assets: BTreeSet<String> = pairs.iter().map(|p| p.asset.clone()).collect();
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
        stacks: None,
        home,
        restored: Vec::new(),
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
        let waiting = queued.contains(&p.asset);
        if !(since.is_none() || changed.contains(&p.asset) || local_changed || pending || p.deleted || waiting) {
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
        if waiting && (a.is_trashed == p.deleted) {
            // restored on the side that had deleted it (or gone on both): nothing left to confirm
            done.restored.push(p.asset.clone());
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
    // 5. stacks
    done.stacks = sync_stacks(c, &local_stacks, &base_stacks, &linked_assets, dry_run, &mut done.errors);
    done
}

/// RT.C3: mirror stacks of linked assets (three-way, `lcc_immich::stacks`); the Immich side is
/// written here, the catalog side in [`finish`]. A key without stack permissions skips this
/// part with a clear message; the metadata sync still counts.
fn sync_stacks(
    c: &lcc_immich::Client,
    local: &[lcc_immich::stacks::Group],
    base: &[lcc_immich::stacks::Group],
    linked: &BTreeSet<String>,
    dry_run: bool,
    errors: &mut Vec<String>,
) -> Option<StackSync> {
    use lcc_immich::stacks;
    const PERMS: &str =
        "stack sync skipped: the API key lacks the stack permissions (stack.read, stack.create, stack.delete); add them to the key in Immich";
    let remote_all = match c.stacks() {
        Ok(v) => v,
        Err(ImmichError::Forbidden(_)) => {
            errors.push(PERMS.into());
            return None;
        }
        Err(e) => {
            errors.push(format!("stacks: {e}"));
            return None;
        }
    };
    let remote = stacks::remote_groups(&remote_all, linked);
    let plan = stacks::reconcile(local, &remote, base);
    let base = stacks::next_base(local, &remote, &plan);
    if dry_run || plan.to_remote.is_empty() && plan.remove_remote.is_empty() {
        return Some(StackSync { plan, base });
    }
    let index = stacks::stack_index(&remote_all);
    let stack_of = |a: &str| index.get(a).cloned();
    let mut gone: BTreeSet<String> = BTreeSet::new();
    let mut delete: Vec<String> = Vec::new();
    for g in &plan.remove_remote {
        if let Some(id) = stack_of(&g.primary)
            && gone.insert(id.clone())
        {
            delete.push(id);
        }
    }
    // a new group replaces every remote stack it overlaps
    for g in &plan.to_remote {
        for m in &g.members {
            if let Some(id) = stack_of(m)
                && gone.insert(id.clone())
            {
                delete.push(id);
            }
        }
    }
    let mut ok = true;
    if !delete.is_empty()
        && let Err(e) = c.delete_stacks(&delete)
    {
        ok = false;
        errors.push(if matches!(e, ImmichError::Forbidden(_)) { PERMS.to_string() } else { format!("stacks: {e}") });
    }
    for g in &plan.to_remote {
        if !ok {
            break;
        }
        let mut ids = vec![g.primary.clone()];
        ids.extend(g.members.iter().filter(|m| **m != g.primary).cloned());
        if let Err(e) = c.stack(&ids) {
            ok = false;
            errors.push(if matches!(e, ImmichError::Forbidden(_)) { PERMS.to_string() } else { format!("stack: {e}") });
        }
    }
    // a failed write: keep the old base, so the next run tries again
    ok.then_some(StackSync { plan, base })
}

/// Write the outcomes' Immich side: asset updates, then tags (created once, assigned in bulk).
///
/// A pair stays `pushed` only when everything it had to send arrived: its asset update and each
/// of its tag changes. A pair the run never reached (the server went away part-way) or whose tags
/// were not sent is marked not pushed, so [`finish`] keeps its old base and the next run sends
/// the change again instead of reading the catalog's edit as "already synced".
fn push(c: &lcc_immich::Client, cfg: &SyncConfig, results: &mut [(PairDone, Asset)], done: &mut SyncDone) {
    let mut stopped = false;
    for (p, a) in results.iter_mut() {
        if p.outcome.to_remote.is_empty() {
            continue;
        }
        if stopped {
            // not reached
            p.pushed = false;
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
                stopped = true;
            }
        }
    }
    let has_tags = |p: &PairDone| !p.outcome.add_tags.is_empty() || !p.outcome.remove_tags.is_empty();
    if stopped {
        // the tag step is not run either: a pair whose tags are still to send is not done
        for (p, _) in results.iter_mut().filter(|(p, _)| p.pushed && has_tags(p)) {
            p.pushed = false;
        }
        return;
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
                if e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey) {
                    done.fatal.get_or_insert(e);
                    for (p, _) in results.iter_mut().filter(|(p, _)| p.pushed && has_tags(p)) {
                        p.pushed = false;
                    }
                    return;
                }
            }
        }
    }
    let mut add: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut remove: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut unknown: Vec<(usize, String)> = Vec::new();
    for (i, (p, a)) in results.iter().enumerate().filter(|(_, (p, _))| p.pushed) {
        for k in &p.outcome.add_tags {
            match ids.get(&merge::keyword_to_tag(k)) {
                Some(id) => add.entry(id.clone()).or_default().push(i),
                // the server did not hand the tag back: it was not added
                None => unknown.push((i, k.clone())),
            }
        }
        for k in &p.outcome.remove_tags {
            if let Some(t) = a.tags.iter().find(|t| lcc_immich::mapping::tag_to_keyword(&t.value) == *k) {
                remove.entry(t.id.clone()).or_default().push(i);
            }
        }
    }
    for (i, k) in unknown {
        if let Some((p, _)) = results.get_mut(i) {
            p.pushed = false;
            p.error = Some(format!("tags: the server did not create the tag for {k}"));
        }
    }
    let mut gone = false;
    for (tag, idx, adding) in add.into_iter().map(|(t, i)| (t, i, true)).chain(remove.into_iter().map(|(t, i)| (t, i, false))) {
        let r = if gone {
            // the server went away: the tag changes not sent keep their pairs open
            Err(None)
        } else {
            let assets: Vec<String> = idx.iter().filter_map(|i| results.get(*i).map(|(p, _)| p.asset.clone())).collect();
            if adding { c.tag_assets(&tag, &assets) } else { c.untag_assets(&tag, &assets) }.map_err(Some)
        };
        let Err(e) = r else { continue };
        if let Some(e) = &e {
            done.errors.push(format!("tags: {e}"));
        }
        for i in idx {
            if let Some((p, _)) = results.get_mut(i) {
                p.pushed = false;
                if let Some(e) = &e {
                    p.error = Some(format!("tags: {e}"));
                }
            }
        }
        if let Some(e) = e
            && (e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey))
        {
            done.fatal.get_or_insert(e);
            gone = true;
        }
    }
}

/// Take a finished run in: catalog changes, the new state, the log → the run's summary.
pub(crate) fn finish(s: &mut Session, d: SyncDone) -> Value {
    let now = (s.clock)();
    let account = d.account.clone();
    if state_path(s, &account) != d.home {
        // another library was opened while the worker was out: its photo ids and bases are not
        // this library's. Nothing is applied or recorded; that library's next run starts from
        // its own saved state.
        let entry = s.remote.sync.entry(account.clone()).or_default();
        entry.running = false;
        let v = json!({
            "ok": false,
            "account": account,
            "dryRun": d.dry_run,
            "errors": ["the library changed while the sync was running; nothing was applied"],
        });
        entry.last = Some(v.clone());
        return v;
    }
    let cfg = config_of(s, &account);
    let (mut pulled, mut pushed, mut conflicts) = (0u64, 0u64, 0u64);
    let mut errors = d.errors.clone();
    let mut changes = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut link_ops: Vec<Op> = Vec::new();
    // each pair's new state, with what to take back should the catalog write fail
    let mut new_items: Vec<(String, ItemState, PullUndo)> = Vec::new();
    // something was left behind that only the server's listing would bring up again: the
    // cursor (`synced_until`) must not pass it
    let mut unsent = false;
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
                // not (fully) sent: the old base stays, so the next run sends the change again.
                // A pair the next run would not look at by itself (the catalog still equals the
                // base, nothing decided or in conflict) is found again through the listing
                unsent |= st
                    .items
                    .get(&p.asset)
                    .is_some_and(|i| i.base.as_ref() == Some(&p.local_used) && i.forced.is_empty() && i.conflicts.is_empty());
                continue;
            }
            let Some(photo) = s.catalog.photo(p.photo) else { continue };
            let mut item = st.items.get(&p.asset).cloned().unwrap_or_default();
            let current = merge::local(photo, &cfg, &ex);
            let mut unapplied = BTreeSet::new();
            if !o.to_local.is_empty() {
                if current == p.local_used {
                    ops.extend(merge::local_ops(photo, &o.target, &o.to_local, &cfg, &ex));
                    pulled += 1;
                } else {
                    // edited again while the run was out: nothing is written over the newer edit,
                    // and the base does not move for the fields that were not taken
                    unapplied = o.to_local.clone();
                    errors.push(format!("photo {}: changed during the sync; synced next time", p.photo.0));
                }
            }
            pushed += u64::from(!o.to_remote.is_empty());
            conflicts += o.conflicts.len() as u64;
            let undo = PullUndo {
                fields: if unapplied.is_empty() { o.to_local.clone() } else { BTreeSet::new() },
                old_base: item.base.clone(),
                forced: item.forced.clone(),
            };
            item.photo = p.photo.0;
            item.base = Some(merge::next_base(o, undo.old_base.as_ref(), &p.local_used, &p.remote, &unapplied));
            item.local_changed_at = None;
            // a decision for a field that was not taken waits for the next run
            item.forced.retain(|f, _| unapplied.contains(f));
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
            new_items.push((p.asset.clone(), item, undo));
        }
    }
    if !d.dry_run {
        if !ops.is_empty()
            && let Err(e) = s.apply_system(Op::Batch { ops })
        {
            // nothing was written into the catalog: no pulled field may count as synced
            errors.push(format!("catalog: {e}"));
            pulled = 0;
            let by_asset: HashMap<&str, &PairDone> = d.pairs.iter().map(|p| (p.asset.as_str(), p)).collect();
            for (asset, item, undo) in new_items.iter_mut().filter(|(_, _, u)| !u.fields.is_empty()) {
                let Some(p) = by_asset.get(asset.as_str()) else { continue };
                item.base = Some(merge::next_base(&p.outcome, undo.old_base.as_ref(), &p.local_used, &p.remote, &undo.fields));
                for f in &undo.fields {
                    if let Some(side) = undo.forced.get(f) {
                        item.forced.insert(*f, *side);
                    }
                }
            }
            unsent = true;
        }
        if !link_ops.is_empty()
            && let Err(e) = s.apply_system(Op::Batch { ops: link_ops })
        {
            errors.push(format!("catalog: {e}"));
        }
    }
    let mut stacks_changed = 0u64;
    if !d.dry_run
        && let Some(ss) = &d.stacks
    {
        let photo_of = |s: &Session, a: &str| s.catalog.photo_of_remote(SERVICE, &account, a);
        let mut sops: Vec<Op> = Vec::new();
        for g in &ss.plan.remove_local {
            let photos: Vec<PhotoId> = g.members.iter().filter_map(|a| photo_of(s, a)).collect();
            sops.extend(s.catalog.remove_from_stacks_ops(&photos));
        }
        if !sops.is_empty() {
            stacks_changed += ss.plan.remove_local.len() as u64;
            if let Err(e) = s.apply_system(Op::Batch { ops: std::mem::take(&mut sops) }) {
                errors.push(format!("catalog stacks: {e}"));
            }
        }
        for g in &ss.plan.to_local {
            let Some(top) = photo_of(s, &g.primary) else { continue };
            let photos: Vec<PhotoId> = g.members.iter().filter_map(|a| photo_of(s, a)).collect();
            let same = s.catalog.stack_of(top).is_some_and(|st| photos.iter().all(|p| st.photos.contains(p)));
            let op = if same { s.catalog.set_top_ops(top) } else { s.catalog.group_ops(top, &photos, false) };
            if let Some(op) = op {
                stacks_changed += 1;
                if let Err(e) = s.apply_system(op) {
                    errors.push(format!("catalog stacks: {e}"));
                }
            }
        }
    }
    let fatal = d.fatal.as_ref().map(failure);
    let st = state_of(s, &account);
    if !d.dry_run
        && let Some(ss) = &d.stacks
    {
        st.stacks = ss.base.clone();
    }
    if !d.dry_run {
        for (k, v, _) in new_items {
            st.items.insert(k, v);
        }
        // the queue only holds deletions that are still true
        st.deletions.retain(|x| !d.restored.contains(&x.asset));
        for del in &d.deletions {
            match st.deletions.iter_mut().find(|x| x.asset == del.asset) {
                // (restored on one side, deleted on the other since)
                Some(x) if x.side != del.side => *x = del.clone(),
                Some(_) => {}
                None => st.deletions.push(del.clone()),
            }
        }
        // the cursor only moves past a run that left nothing behind for the listing
        if d.fatal.is_none()
            && !unsent
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
        "stacks": d.stacks.as_ref().map(|ss| json!({
            "toCatalog": ss.plan.to_local.len() + ss.plan.remove_local.len(),
            "toImmich": ss.plan.to_remote.len() + ss.plan.remove_remote.len(),
            "conflicts": ss.plan.conflicts.len(),
            "applied": stacks_changed,
        })),
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
    if d.dry_run {
        entry.preview = Some(v.clone());
    }
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
                // new assets on the server are linked first-come: the link pass rides along
                if let Err(err) = super::start_link_pass(s, &id, false) {
                    log::warn!("Immich background link pass: {err}");
                }
                if let Err(err) = start(s, &id, false, false, true) {
                    log::warn!("Immich background sync: {err}");
                }
                if config_of(s, &id).album_rule().direction != merge::Direction::Off
                    && let Err(err) = super::publish::start_albums(s, &id, false, true)
                {
                    log::warn!("Immich background album sync: {err}");
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
    let (dry, background) = (bool_or(p, "dryRun", false), bool_or(p, "background", false));
    let mut v = start(s, &id, dry, bool_or(p, "full", false), background)?;
    // the album rule rides along with every real run (Sync Now, the schedule)
    if !dry && config_of(s, &id).album_rule().direction != merge::Direction::Off {
        v["albums"] = super::publish::start_albums(s, &id, false, background)?;
    }
    Ok(v)
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
        "preview": e.and_then(|e| e.preview.clone()),
        "nextRunInSeconds": next,
        "syncedUntil": st.synced_until,
        "items": st.items.len(),
        "conflicts": st.conflict_count(),
        "deletions": st.deletions,
        "log": st.log.iter().rev().take(n).collect::<Vec<_>>(),
        "config": cfg,
        "rules": Field::ALL.iter().map(|f| json!({"field": f.name(), "rule": cfg.rule(*f)})).collect::<Vec<_>>(),
        "albumRule": cfg.album_rule(),
        "albumsRunning": e.is_some_and(|e| e.albums_running),
        "albumsLast": e.and_then(|e| e.albums_last.clone()),
        "albumDeletions": super::publish::load_pending(s, &id),
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
        lcc_develop::presets::deep_merge(&mut v, patch);
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
    // `assets` omitted (or null) = every queued deletion; a list = exactly those, so `[]` resolves
    // nothing. Anything else is refused rather than guessed at: `apply` is destructive.
    let assets: Option<BTreeSet<String>> = match p.get("assets") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().filter_map(Value::as_str).map(str::to_string).collect()),
        Some(_) => return Err(bad(C, "`assets` must be a list of asset ids (omit it for all queued)")),
    };
    let st = state_of(s, &id).clone();
    let picked: Vec<Deletion> = st.deletions.iter().filter(|d| assets.as_ref().is_none_or(|a| a.contains(&d.asset))).cloned().collect();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    // deletions that are no longer true (the asset is back out of Immich's trash, the photo back
    // out of Recently Deleted): dropped from the queue, never applied
    let mut restored: Vec<String> = Vec::new();
    if apply {
        // the queue may be old: look again before deleting anything
        let client = if picked.is_empty() { None } else { Some(s.immich_client(&id).map(|(_, c)| c)) };
        let mut go: Vec<&Deletion> = Vec::new();
        for d in &picked {
            let photo_deleted = s.catalog.photo(PhotoId(d.photo)).is_some_and(|p| p.deleted);
            if d.side == "catalog" {
                if photo_deleted || s.catalog.photo(PhotoId(d.photo)).is_none() {
                    go.push(d);
                } else {
                    restored.push(d.asset.clone());
                }
                continue;
            }
            match client.as_ref().map(|c| c.as_ref().map_err(Clone::clone).and_then(|c| c.asset(&d.asset))) {
                Some(Ok(a)) if a.is_trashed => go.push(d),
                Some(Ok(_)) => restored.push(d.asset.clone()),
                // deleted for good in Immich: the deletion stands
                Some(Err(ImmichError::NotFound(_))) => go.push(d),
                // can't tell (offline, key): nothing is deleted on a guess
                Some(Err(e)) => failed.push(failure(&e)),
                None => {}
            }
        }
        let local: Vec<u64> = go.iter().filter(|d| d.side == "immich").map(|d| d.photo).filter(|ph| s.catalog.photo(PhotoId(*ph)).is_some()).collect();
        if !local.is_empty() {
            s.execute("photo.delete", &json!({"ids": local}))?;
        }
        done.extend(go.iter().filter(|d| d.side == "immich").map(|d| d.asset.clone()));
        let remote: Vec<String> = go.iter().filter(|d| d.side == "catalog").map(|d| d.asset.clone()).collect();
        if !remote.is_empty() {
            match client {
                Some(Ok(c)) => match c.trash_assets(&remote) {
                    Ok(()) => done.extend(remote),
                    Err(e) => failed.push(failure(&e)),
                },
                Some(Err(e)) => failed.push(failure(&e)),
                None => {}
            }
        }
    } else {
        done.extend(picked.iter().map(|d| d.asset.clone()));
    }
    let st = state_of(s, &id);
    st.deletions.retain(|d| !done.contains(&d.asset) && !restored.contains(&d.asset));
    if !apply {
        st.kept.extend(done.iter().cloned());
    }
    save_state(s, &id).map_err(|e| bad(C, e))?;
    Ok(json!({"resolved": done.len(), "restored": restored.len(), "failed": failed}))
}

// ---- people

fn linked_targets(s: &Session, p: &Value, account: &str) -> Vec<(PhotoId, String)> {
    let only: Option<BTreeSet<u64>> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
    linked(s, account).into_iter().filter(|(ph, _)| only.as_ref().is_none_or(|o| o.contains(&ph.0))).collect()
}

/// What the server side of `immich.importPeople` brings back.
pub(crate) struct PeopleDone {
    pub(crate) account: String,
    hidden: bool,
    more: bool,
    people: Vec<lcc_immich::types::Person>,
    faces: Vec<(PhotoId, Vec<lcc_immich::types::Face>)>,
    errors: Vec<String>,
    /// A failure that ends the run (offline, key, permissions): nothing is written.
    fatal: Option<ImmichError>,
}

/// The server side: the people, then each linked asset's faces (`task`: progress and cancel).
fn people_work(
    client: &lcc_immich::Client,
    account: String,
    targets: Vec<(PhotoId, String)>,
    hidden: bool,
    more: bool,
    task: Option<&crate::activity::TaskGuard>,
) -> PeopleDone {
    let mut d = PeopleDone { account, hidden, more, people: Vec::new(), faces: Vec::new(), errors: Vec::new(), fatal: None };
    match client.people_all() {
        Ok(x) => d.people = x,
        Err(e) => {
            d.fatal = Some(e);
            return d;
        }
    }
    let total = targets.len() as u64;
    for (n, (ph, asset)) in targets.into_iter().enumerate() {
        if let Some(t) = task {
            if t.is_cancelled() {
                break;
            }
            t.progress(n as u64, total);
        }
        match client.faces(&asset) {
            Ok(f) => d.faces.push((ph, f)),
            Err(e) if e.retryable() || matches!(e, ImmichError::BadKey | ImmichError::NoKey | ImmichError::Forbidden(_)) => {
                d.fatal = Some(e);
                return d;
            }
            Err(e) => d.errors.push(format!("{asset}: {e}")),
        }
    }
    d
}

/// Take the faces in: the photos' regions, as one undo step.
pub(crate) fn finish_people(s: &mut Session, d: PeopleDone) -> Value {
    if let Some(e) = &d.fatal {
        return failure(e);
    }
    let (mut added, mut removed, mut skipped, mut photos) = (0, 0, 0, 0);
    let mut ops = Vec::new();
    for (ph, faces) in &d.faces {
        let Some(photo) = s.catalog.photo(*ph) else { continue };
        let (regions, m) = people::regions_with_faces(photo, faces, d.hidden);
        skipped += m.skipped;
        if m.added + m.removed > 0 && regions != photo.meta.regions {
            let mut meta = photo.meta.clone();
            meta.regions = regions;
            ops.push(Op::SetMeta { id: *ph, meta: Box::new(meta) });
            added += m.added;
            removed += m.removed;
            photos += 1;
        }
    }
    let mut errors = d.errors;
    if !ops.is_empty()
        && let Err(e) = s.commit("Import People from Immich", Op::Batch { ops })
    {
        errors.push(format!("catalog: {e}"));
        (added, removed, photos) = (0, 0, 0);
    }
    json!({
        "ok": true,
        "photos": photos,
        "added": added,
        "removed": removed,
        "skipped": skipped,
        "more": d.more,
        "errors": errors,
        "people": d.people.iter().map(|x| json!({"id": x.id, "name": x.name, "birthDate": x.birth_date, "hidden": x.is_hidden})).collect::<Vec<_>>(),
    })
}

/// Read Immich's faces for linked photos into their face regions (marked "from Immich"). With
/// `background` the server is read on a worker (one request per photo: minutes on a large
/// library) and the result arrives through `immich.answer`.
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
    let hidden = bool_or(p, "includeHidden", false);
    if bool_or(p, "background", false) {
        let key = super::ask_key(C, &id);
        if !s.remote.asking.insert(key.clone()) {
            return Ok(json!({"started": false, "key": key, "reason": "already running"}));
        }
        s.remote.answers.remove(&key);
        let task = s.activity.start("immich", "Importing people from Immich", crate::activity::Cancel::Yes);
        let tx = s.remote.sender();
        std::thread::spawn(move || {
            let d = people_work(&client, id, targets, hidden, more, Some(&task));
            drop(task);
            let _ = tx.send(Msg::People(Box::new(d)));
        });
        return Ok(json!({"started": true, "key": key}));
    }
    let d = people_work(&client, id, targets, hidden, more, None);
    Ok(finish_people(s, d))
}

/// Confirm the Immich face regions of the photos (default: selection).
fn confirm_faces(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
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
    // the catalog's part here (the photos are shared, not copied); the server's in `work`
    let photos: Vec<std::sync::Arc<lcc_catalog::Photo>> =
        linked(s, &id).into_iter().map(|(p, _)| p).collect::<BTreeSet<PhotoId>>().into_iter().filter_map(|i| s.catalog.photo(i).cloned()).collect();
    let (merge_people, dry_run) = (bool_or(p, "merge", false), bool_or(p, "dryRun", false));
    let work = move || {
        let people = match client.people_all() {
            Ok(x) => x,
            Err(e) => return failure(&e),
        };
        let plan = people::names_to_push(photos.iter().map(|x| &**x), &people, merge_people);
        let mut v = json!({
            "renames": plan.renames.iter().map(|(i, n)| json!({"person": i, "name": n})).collect::<Vec<_>>(),
            "merges": plan.merges.iter().map(|(i, o)| json!({"into": i, "people": o})).collect::<Vec<_>>(),
        });
        if dry_run {
            return v;
        }
        if !plan.renames.is_empty()
            && let Err(e) = client.rename_people(&plan.renames)
        {
            return failure(&e);
        }
        for (into, others) in &plan.merges {
            if let Err(e) = client.merge_people(into, others) {
                return failure(&e);
            }
        }
        v["ok"] = json!(true);
        v
    };
    if bool_or(p, "background", false) {
        return Ok(super::ask(s, super::ask_key(C, &id), "Sending names to Immich", move |_| work()));
    }
    Ok(work())
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
        s.show_only(photos.clone());
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
        cmd!(query "immich.sync", "Sync Metadata with Immich", ["Library", "Immich"], None, "{account?, dryRun?: bool (only report what would change), full?: bool (look at every linked asset), background?: bool}: two-way metadata sync of linked photos (rating, favourite, archived, description, keywords ↔ tags, location, date; per-field rules in immich.setSync); conflicts wait in immich.syncConflicts, deletions in the queue → {ok, checked, pulled, pushed, conflicts, openConflicts, deletionsQueued, errors, changes? (dry run)} or {started}", always, sync_cmd),
        cmd!(query "immich.syncStatus", "Immich Sync Status", [], None, "{account?, log?: n} → {running, last, preview (the last dry run, with its changes), nextRunInSeconds, syncedUntil, items, conflicts, deletions, log (activity, newest first), config, rules}", always, status),
        cmd!(query "immich.syncConflicts", "Immich Sync Conflicts", [], None, "{account?} → {conflicts: [{photo, assetId, fileName, field, catalog, immich, detected, decided?}]}", always, conflicts),
        cmd!(query "immich.resolveConflict", "Resolve Immich Sync Conflict", [], None, "{account?, keep: catalog|immich, ids?: photos, assetId?, field?, sync?: bool, background?: bool}: decide conflicts (default: all); the next sync applies them (sync: run it now) → {decided, sync?}", always, resolve),
        cmd!(query "immich.setSync", "Immich Sync Settings", [], None, "{account?, config: {rules?: {rating|favorite|archived|description|keywords|location|captured: {direction: twoWay|toImmich|fromImmich|off, policy: catalog|immich|newest|ask}}, favorite?: pick|fiveStars, description?: caption|title, intervalMinutes?, onPublish?, pushPeople?, albums?: {direction, policy} (collections ↔ albums, immich.syncAlbums; default off = only albums linked by hand, policy ask)}} → {config}", always, set_config),
        cmd!(query "immich.resolveDeletion", "Resolve Immich Deletion", [], None, "{account?, assets?: [asset ids] (omitted = all queued; [] = none), action: apply|keep}: apply: a photo whose asset is in Immich's trash goes to Recently Deleted, an asset whose photo was deleted goes to Immich's trash (never a hard delete); each is checked first, and a deletion that was undone meanwhile (the asset restored from the trash, the photo restored) is dropped, not applied; keep: dismiss → {resolved, restored, failed}", always, resolve_deletion),
        cmd!(query "immich.importPeople", "Import People from Immich", [], None, "{account?, ids?: photos (default all linked), includeHidden?: bool, background?: bool (on a worker, with an activity row that can be cancelled: {started, key}, the result through immich.answer)}: read Immich's faces and people into the photos' face regions, marked from Immich (unconfirmed ones are replaced on the next import; regions the photo had are kept) → {photos, added, removed, skipped, more, people: [{id, name, birthDate, hidden}]}", always, import_people),
        cmd!(
            "immich.confirmFaces",
            "Confirm Faces from Immich",
            [],
            None,
            "{ids?}: keep the Immich face regions of the photos (default: selection) as confirmed → {confirmed}",
            always,
            confirm_faces
        ),
        cmd!(query "immich.pushPeople", "Send Names to Immich", [], None, "{account?, merge?: bool, dryRun?: bool, background?: bool (on a worker: {started, key}, the result through immich.answer)}: rename Immich people to the names given to their faces in the app; with merge, people that now share a name are merged in Immich → {renames, merges, ok?}", always, push_people),
        cmd!(query "immich.smartSearch", "Immich Smart Search", [], None, "{account?, query, mode?: smart|ocr|description|place, size?, page?, show?: bool (show the results as a temporary collection), saveAs?: album name}: natural-language (CLIP) or metadata search on the server, mapped to linked photos → {photos, unmatched, total, nextPage, shown?, album?}", always, smart_search),
    ]
}
