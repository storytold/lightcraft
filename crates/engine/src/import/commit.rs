use super::defaults::apply_import_defaults;
use super::files::copy_into;
use super::job::{Prepared, PreparedItem, ReadyFile};
use super::types::{Duplicate, FileFacts, ImportOptions, ImportReport, Kept, Moved, OnDeleted};
use super::{HashMap, Op, Path, PathBuf, Photo, PhotoId, ProbeInfo, Session, Source};

/// A batch's items in file order while its transfers are pending ([`ImportJob::run_plan`]).
#[derive(Default)]
pub(super) struct Plan {
    pub(super) slots: Vec<Slot>,
    pub(super) transfers: Vec<Transfer>,
    /// The content hashes and name families of [`Plan::transfers`], so each new file is checked against them in
    /// constant time rather than by scanning the pending transfers.
    pub(super) pending_hashes: std::collections::HashSet<String>,
    pub(super) pending_families: std::collections::HashSet<(String, String)>,
}

pub(super) enum Slot {
    Item(PreparedItem),
    /// The outcome of the next of [`Plan::transfers`].
    Transfer,
}

/// A file to copy or move into `dir`, planned in order and run on any thread ([`run_transfer`]).
pub(super) struct Transfer {
    pub(super) path: String,
    pub(super) info: ProbeInfo,
    pub(super) sidecar: Option<crate::sidecar::SidecarData>,
    pub(super) dir: PathBuf,
    pub(super) how: How,
    /// Copy as DNG.
    pub(super) dng: bool,
}

pub(super) enum How {
    /// A verified copy, under `name` (default: the file's own).
    Copy { name: Option<String> },
    /// Move: placed (linked or copied) without touching the source.
    Place { name: String },
}

/// The names a file called `name` could end up with, as one key: the transfers number a taken name
/// `IMG-1.JPG`, `IMG-2.JPG`…, and Copy as DNG names its DNG `IMG.dng`, `IMG-2.dng`…, so `IMG.JPG`,
/// `IMG-1.jpg` and `img.dng` are all `img` (case-insensitive: so are some file systems).
pub(super) fn name_family(name: &str) -> String {
    let mut stem = match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    };
    while let Some((head, tail)) = stem.rsplit_once('-')
        && !head.is_empty()
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        stem = head;
    }
    stem.to_lowercase()
}

/// Threads for `jobs` file reads (probes, copies) at once.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn workers(jobs: usize) -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(jobs.max(1))
}

/// Files per batch for a caller that prepares an import in batches ([`ImportJob::prepare_files`]):
/// enough to keep every probe and copy thread busy (a batch waits for its slowest file), few
/// enough that each batch soon joins the catalog.
pub fn batch_size() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        workers(usize::MAX).saturating_mul(2).clamp(8, 16)
    }
    #[cfg(target_arch = "wasm32")]
    {
        8
    }
}

/// Run `transfers` (several at once where threads exist); their outcomes in order, `None` for one
/// not started because `cancel` was set. `done` counts each one finished.
pub(super) fn run_transfers(
    transfers: Vec<Transfer>,
    file_bytes: Option<&crate::merge::ByteReader>,
    cancel: &std::sync::atomic::AtomicBool,
    done: &std::sync::atomic::AtomicUsize,
) -> Vec<Option<PreparedItem>> {
    use std::sync::atomic::Ordering::Relaxed;
    let one = |t: Transfer| {
        if cancel.load(Relaxed) {
            return None;
        }
        let path = t.path.clone();
        // last-resort guard: a panic is this file's failure, not the import's
        let item = crate::guard::catch("import", || run_transfer(t, file_bytes)).unwrap_or_else(|e| PreparedItem::Failed(path, e));
        done.fetch_add(1, Relaxed);
        Some(item)
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(transfers.len());
        if n > 1 {
            let mut results: Vec<Option<PreparedItem>> = std::iter::repeat_with(|| None).take(transfers.len()).collect();
            let queue = std::sync::Mutex::new(transfers.into_iter().enumerate());
            let out = std::sync::Mutex::new(&mut results);
            // (the tests' injected faults hold for the workers too)
            #[cfg(test)]
            let fault = crate::import_move::injected_fault();
            let work = &|| {
                #[cfg(test)]
                crate::import_move::inject(fault);
                loop {
                    let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
                    let Some((i, t)) = next else { break };
                    let r = one(t);
                    if let Some(slot) = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i) {
                        *slot = r;
                    }
                }
            };
            std::thread::scope(|sc| {
                let handles: Vec<_> =
                    (0..n).filter_map(|i| std::thread::Builder::new().name(format!("lc-import-copy-{i}")).spawn_scoped(sc, work).ok()).collect();
                if handles.is_empty() {
                    // no thread to be had: copy here, one after another
                    work();
                }
                for h in handles {
                    if h.join().is_err() {
                        log::error!("import: a copy worker stopped unexpectedly");
                    }
                }
            });
            return results;
        }
    }
    transfers.into_iter().map(one).collect()
}

/// Copy or move one planned file (and Copy as DNG); its item.
fn run_transfer(t: Transfer, file_bytes: Option<&crate::merge::ByteReader>) -> PreparedItem {
    let Transfer { path, mut info, sidecar, dir, how, dng, .. } = t;
    let mut placed = None;
    let mut stored = match how {
        How::Place { name } => match crate::import_move::place(Path::new(&path), &dir, &name) {
            Ok(pl) => {
                let dst = pl.dst.to_string_lossy().to_string();
                placed = Some(pl);
                dst
            }
            Err(e) => return PreparedItem::Failed(path, e),
        },
        How::Copy { name } => match copy_into(&dir, &path, name.as_deref(), info.content_hash.as_deref()) {
            Ok(p) => p,
            Err(e) => return PreparedItem::Failed(path, e),
        },
    };
    if dng {
        // decoding the raw twice and encoding it takes several times the file's size in memory
        let weight = usize::try_from(info.file_size).unwrap_or(usize::MAX).saturating_mul(6);
        let _permit = crate::memory::work_gate().acquire(weight);
        match crate::cmd::convert::write_dng_with(file_bytes, &stored, String::new()) {
            Ok(dng) => {
                let _ = std::fs::remove_file(&stored);
                stored = dng;
                info.format = "DNG".into();
            }
            Err(e) => log::warn!("import {path}: copy as DNG: {e}"),
        }
    }
    PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed }))
}

/// A trashed photo's file is not where the library had it (never on the web, where paths are
/// storage keys, not files).
fn trashed_file_is_gone(cat: &lightcraft_catalog::Catalog, id: PhotoId) -> bool {
    !cfg!(target_arch = "wasm32")
        && match cat.photo(id).map(|p| &p.source) {
            Some(Source::File { path }) => !Path::new(path).exists(),
            _ => false,
        }
}

/// What a new record takes from its file, as a file import gives it: the file's metadata and
/// capture time, the import defaults, then its XMP sidecar. Returns the sidecar when it said
/// anything (labels read through the library's label names).
pub(crate) fn start_from_file(s: &Session, p: &mut Photo, file: FileFacts, now: &str) -> Option<crate::sidecar::SidecarData> {
    let FileFacts { meta, captured, orientation, sidecar } = file;
    p.meta = meta;
    p.captured = captured;
    apply_import_defaults(s, p);
    let sc = sidecar.map(|sc| sc.resolve_label(&s.catalog)).filter(|sc| *sc != crate::sidecar::SidecarData::default())?;
    // the photo is upright as the file's own orientation turns it, when the file states one
    // (after the check above: an orientation alone isn't a sidecar that said something)
    let sc = crate::sidecar::SidecarData { orientation: orientation.or(sc.orientation), ..sc };
    crate::sidecar::merge_into(p, &sc, now);
    Some(sc)
}

/// The catalog half of an import: add the photos [`ImportJob::prepare`] readied (one undoable op;
/// a Move's sources are removed once it is saved). On a failed commit a Move's placed files are
/// taken back.
pub fn commit_prepared(s: &mut Session, opts: &ImportOptions, now: &str, prepared: Prepared) -> crate::Result<ImportReport> {
    let now = now.to_string();
    let mut report = ImportReport { scanned: prepared.scanned, ..Default::default() };
    let mut ops = Vec::new();
    // Move: placed at the destination, sources untouched until the records are committed
    let mut placed: Vec<crate::import_move::Placed> = Vec::new();
    // photos added by this batch, by content (a duplicate of a file earlier in the import names it)
    let mut new_hash: HashMap<String, PhotoId> = HashMap::new();
    // Fresh: trashed photos a new import replaces (by content or by file), each removed once
    let (mut trash_hash, mut trash_path): (HashMap<String, PhotoId>, HashMap<String, PhotoId>) = Default::default();
    if opts.on_deleted == OnDeleted::Fresh {
        for p in s.catalog.photos().filter(|p| p.deleted) {
            if let Some(h) = &p.content_hash {
                trash_hash.insert(h.clone(), p.id);
            }
            if let Source::File { path } = &p.source {
                trash_path.insert(path.clone(), p.id);
            }
        }
    }
    let mut purge: Vec<PhotoId> = Vec::new();
    let mut purged: std::collections::HashSet<PhotoId> = Default::default();
    for it in prepared.items {
        match it {
            PreparedItem::Promote(id, fresh) => {
                // the file was replaced since it was browsed: the record takes what it is now before it joins the library
                if let Some(info) = &fresh {
                    crate::cmd::convert::reload_ops(&s.catalog, id, info, &mut ops, &mut Vec::new());
                }
                if s.catalog.photo(id).is_some_and(|p| p.local) {
                    ops.push(Op::SetLocal { id, local: false });
                }
                // "Import as new": a trashed photo with the same content is replaced, as for a new file
                let content =
                    fresh.as_ref().and_then(|i| i.content_hash.clone()).or_else(|| s.catalog.photo(id).and_then(|p| p.content_hash.clone()));
                if let Some(old) = content.as_ref().and_then(|h| trash_hash.get(h))
                    && purged.insert(*old)
                {
                    purge.push(*old);
                }
                report.imported.push(id.0);
            }
            PreparedItem::Duplicate { path, existing, reason, hash, file } => {
                let existing = existing.or_else(|| {
                    let h = hash?;
                    new_hash
                        .get(&h)
                        .copied()
                        .or_else(|| s.catalog.photos().find(|p| !p.local && p.content_hash.as_deref() == Some(h.as_str())).map(|p| p.id))
                });
                let trashed = existing.filter(|id| s.catalog.photo(*id).is_some_and(|p| p.deleted));
                match trashed {
                    // (browsing a folder in Local only looks: it never restores)
                    Some(id) if opts.on_deleted == OnDeleted::Restore && !opts.local => {
                        if !report.restored.contains(&id.0) {
                            ops.push(Op::SetDeleted { id, deleted: false });
                            // matched by content from another path, and the file the record
                            // points at is gone (moved, renamed): the photo follows the file
                            // (not onto a path another record holds, a file only browsed in Local)
                            if reason == "content"
                                && trashed_file_is_gone(&s.catalog, id)
                                && !s.catalog.photos().any(|p| matches!(&p.source, Source::File { path: held } if *held == path))
                            {
                                ops.push(crate::cmd::missing::relink_op(id, &path));
                            }
                            report.restored.push(id.0);
                        }
                    }
                    _ => {
                        report.duplicates.push(Duplicate { path, existing: existing.map(|i| i.0), reason, existing_deleted: trashed.is_some(), file })
                    }
                }
            }
            PreparedItem::Failed(path, e) => report.failed.push((path, e)),
            PreparedItem::Kept(k) => report.kept.push(k),
            PreparedItem::Ready(r) => {
                let ReadyFile { path, stored, info, sidecar, placed: pl } = *r;
                placed.extend(pl);
                let id = s.catalog.alloc_photo_id();
                if let Some(h) = &info.content_hash {
                    new_hash.insert(h.clone(), id);
                }
                for old in info.content_hash.as_ref().and_then(|h| trash_hash.get(h)).into_iter().chain(trash_path.get(&stored)) {
                    if purged.insert(*old) {
                        purge.push(*old);
                    }
                }
                // a copy is catalogued under its new name
                let name = Path::new(&stored).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                let mut p = Photo::new(id, Source::File { path: stored }, &name, &info.format, info.width, info.height, &now);
                p.kind = info.kind;
                p.file_size = info.file_size;
                p.as_shot_wb = info.as_shot_wb;
                p.content_hash = info.content_hash;
                p.embedded_lens = info.embedded_lens.map(Box::new);
                p.set_preview_only(info.preview_only.clone());
                if let Some(sc) =
                    start_from_file(s, &mut p, FileFacts { meta: info.meta, captured: info.captured, orientation: info.orientation, sidecar }, &now)
                {
                    report.sidecars += 1;
                    report.warnings.extend(sc.warnings.into_iter().map(|mut warning| {
                        warning.path = path.clone();
                        warning
                    }));
                }
                if let Some(mp) = opts.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
                    crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
                }
                for k in &opts.keywords {
                    let k = lightcraft_catalog::keywords::clean(k);
                    if !k.is_empty() && !p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
                        p.meta.keywords.push(k);
                    }
                }
                if let Some(preset) = &opts.preset {
                    let d = std::sync::Arc::new(crate::presets::apply_preset_to_photo(preset, &p.develop, None, 1.0, &crate::media::source_info(&p)));
                    let label = format!("Preset: {}", preset.name);
                    p.develop = d.clone();
                    p.edited = Some(now.clone());
                    p.history.push(lightcraft_catalog::HistoryStep { label, settings: d });
                }
                // the batch-edit recipe goes on top of everything above (sidecar, camera default,
                // preset): only the controls it names change, through the same specs the sliders use
                if !opts.recipe.is_empty() {
                    let mut d = (*p.develop).clone();
                    for (id, v) in &opts.recipe {
                        // (`ImportJob::new` refused any id `set` would not write, and `set` clamps to the range)
                        lightcraft_develop::controls::set(&mut d, id, *v);
                    }
                    let d = std::sync::Arc::new(d);
                    p.develop = d.clone();
                    p.edited = Some(now.clone());
                    p.history.push(lightcraft_catalog::HistoryStep { label: "Import adjustments".into(), settings: d });
                }
                report.imported.push(id.0);
                p.local = opts.local;
                if opts.local {
                    // the state as browsed: a later change keeps the record from being forgotten
                    p.set_local_baseline();
                }
                ops.push(Op::AddPhoto { photo: Box::new(p) });
            }
        }
    }
    // (built against the catalog as it is: one op list for all of them, ahead of the additions)
    if !purge.is_empty() {
        ops.insert(0, s.catalog.delete_photos_permanently_ops(&purge));
    }
    let log0 = s.pending_log.len();
    let (verb, n) = if report.imported.is_empty() && !report.restored.is_empty() {
        ("Restore", report.restored.len())
    } else {
        ("Add", report.imported.len().max(1))
    };
    if !ops.is_empty()
        && let Err(e) = s.commit(&format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }), Op::Batch { ops })
    {
        // nothing was catalogued: take the moves back (their sources were never touched)
        placed.iter().for_each(crate::import_move::rollback);
        return Err(e);
    }
    if !placed.is_empty() {
        finish_moves(s, placed, log0, &mut report);
    }
    Ok(report)
}

/// Move, after the commit: save the library, then remove each source whose destination is
/// still intact. Nothing is removed when the library can't be saved: saved is on disk, not just
/// handed to the catalog log's thread, which looks at the folder only when it writes (issue #3240),
/// so the log is flushed first.
fn finish_moves(s: &mut Session, placed: Vec<crate::import_move::Placed>, log0: usize, report: &mut ImportReport) {
    // (saving here clears the pending ops, so the sidecars `execute` would auto-write are written here)
    let auto: Vec<Op> = if s.xmp.auto_write { s.pending_log.get(log0..).map(<[Op]>::to_vec).unwrap_or_default() } else { Vec::new() };
    let before = s.pending_log.len();
    if let Err(e) = s.persist().and_then(|()| s.flush_log()) {
        for p in placed {
            let reason = format!("the library could not be saved ({e}): the original stays; the photo uses the copy at {}", p.dst.display());
            report.kept.push(Kept { path: p.src.to_string_lossy().to_string(), reason });
        }
        return;
    }
    for p in placed {
        let from = p.src.to_string_lossy().to_string();
        match crate::import_move::finish(&p) {
            Ok(kept) => {
                let sidecars = p.sidecars.iter().map(|sc| sc.dst.to_string_lossy().to_string()).collect();
                report.moved.push(Moved { from, to: p.dst.to_string_lossy().to_string(), sidecars });
                report.kept.extend(kept.into_iter().map(|(path, reason)| Kept { path, reason }));
            }
            Err(reason) => {
                log::warn!("import: move {from}: {reason}");
                report.kept.push(Kept { path: from, reason });
            }
        }
    }
    if s.pending_log.len() < before && s.interaction.is_none() && !auto.is_empty() {
        s.auto_write_sidecars(&auto);
    }
}

/// The current local time as ISO 8601 (`YYYY-MM-DDTHH:MM:SS`, UTC on targets without a clock
/// offset); for [`Session::clock`] on native hosts.
pub fn system_clock() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        local_civil(secs)
    }
    #[cfg(target_arch = "wasm32")]
    {
        "2026-01-01T00:00:00".to_string()
    }
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SS` (UTC), proleptic Gregorian.
pub fn civil(secs: i64) -> String {
    lightcraft_catalog::dates::civil(secs)
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SS` in the host's time zone: the local civil time every date
/// the catalog stores is in (`captured`, `imported`, an album's `created`, an edit's `edited`).
///
/// The offset is the one in force *at that instant*, not today's, so a photo from July keeps
/// summer time when it is read in January. A host whose zone cannot be read falls back to UTC, as
/// the web build does (issue #2284: a file with no capture date was dated by its modification time
/// in UTC, which put an evening photo on the next day).
#[cfg(not(target_arch = "wasm32"))]
pub fn local_civil(secs: i64) -> String {
    civil_in(secs, &system_zone())
}

/// The host's zone. It holds the whole rule set, so each instant still gets the offset that was in
/// force then. jiff keeps the zone it read for five minutes (a cheap shared handle, so an import
/// does not re-read the system database per file) and reads it again after that, so a host whose
/// zone is changed while LightCraft runs dates its files in the new zone from then on, not in the
/// one it started in.
#[cfg(not(target_arch = "wasm32"))]
fn system_zone() -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::system()
}

/// [`local_civil`] in a given zone. `jiff::TimeZone::system` falls back to a zone that behaves
/// like UTC when the host has none to read, and an instant outside the range it can represent
/// (far outside the range of a file date) keeps its UTC reading rather than failing an import.
#[cfg(not(target_arch = "wasm32"))]
fn civil_in(secs: i64, zone: &jiff::tz::TimeZone) -> String {
    let offset = jiff::Timestamp::from_second(secs).map(|t| i64::from(zone.to_offset_info(t).offset().seconds())).unwrap_or(0);
    civil(secs.saturating_add(offset))
}

#[cfg(target_arch = "wasm32")]
pub fn local_civil(secs: i64) -> String {
    civil(secs)
}

/// The instant at which the host's zone reads `civil` (`YYYY-MM-DDTHH:MM:SS`): the inverse of
/// [`local_civil`]. Tests pin a file's modification time with it, so that an undated file takes
/// the local date they mean whatever zone the machine is in.
#[cfg(not(target_arch = "wasm32"))]
pub fn local_instant(civil: &str) -> Option<i64> {
    instant_in(civil, &system_zone())
}

/// [`local_instant`] in a given zone. `None` when the text is not a civil date-time, or when the
/// zone has no such moment: an hour a spring-forward removes is not a time anything happened at,
/// and jiff's ordinary conversion would answer with the instant an hour later, which reads back as
/// a different time and so is not an inverse at all. An hour a fall-back repeats takes the first
/// of the two.
#[cfg(not(target_arch = "wasm32"))]
fn instant_in(civil: &str, zone: &jiff::tz::TimeZone) -> Option<i64> {
    let when: jiff::civil::DateTime = civil.trim().parse().ok()?;
    let ambiguous = zone.to_ambiguous_zoned(when);
    match ambiguous.offset() {
        jiff::tz::AmbiguousOffset::Gap { .. } => None,
        _ => ambiguous.earlier().ok().map(|z| z.timestamp().as_second()),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn local_instant(civil: &str) -> Option<i64> {
    lightcraft_catalog::dates::iso_seconds(civil)
}

// (the clock is the host's only natively: the web build's is UTC, and has no jiff)
#[cfg(all(test, not(target_arch = "wasm32")))]
mod clock_tests {
    use super::*;

    /// Issue #2284: an instant becomes the civil time the host's own zone shows for it, with the
    /// offset in force *then*. The reported case is a JPEG with no capture date whose modification
    /// time is 2026-10-18 20:15:11 in UTC-5: dated by `civil` it read 01:15 the next day.
    #[test]
    fn local_civil_reads_the_hosts_zone_at_that_instant() {
        let zone = jiff::tz::TimeZone::system();
        // the report, then one in each half of the year (so a zone with DST uses both offsets),
        // one on a year boundary, and one before the epoch
        for iso in ["2026-10-19T01:15:11Z", "2026-07-04T12:00:00Z", "2026-01-15T23:30:00Z", "2027-01-01T00:00:00Z", "1969-12-31T23:59:59Z"] {
            let secs = lightcraft_meta::DateTime::parse_iso(iso).expect("a test timestamp").unix_seconds();
            let zoned = jiff::Timestamp::from_second(secs).expect("in range").to_zoned(zone.clone());
            assert_eq!(local_civil(secs), zoned.strftime("%Y-%m-%dT%H:%M:%S").to_string(), "{iso}");
        }
    }

    /// The reported numbers, against a fixed UTC-5 zone, so they are checked on any host: a CI
    /// runner set to UTC would make the test above agree with itself and prove nothing.
    #[test]
    fn an_evening_in_utc_minus_5_stays_that_evening() {
        let secs = lightcraft_meta::DateTime::parse_iso("2026-10-19T01:15:11Z").expect("a test timestamp").unix_seconds();
        assert_eq!(civil(secs), "2026-10-19T01:15:11", "the UTC reading the file used to be dated by");
        let zone = jiff::tz::TimeZone::fixed(jiff::tz::offset(-5));
        assert_eq!(civil_in(secs, &zone), "2026-10-18T20:15:11");
        // and the other way, so the sign cannot be read backwards
        assert_eq!(civil_in(secs, &jiff::tz::TimeZone::fixed(jiff::tz::offset(2))), "2026-10-19T03:15:11");
    }

    /// `local_instant` undoes `local_civil`, so a fixture can pin a file to a local date.
    #[test]
    fn local_instant_is_the_inverse_of_local_civil() {
        for iso in ["2026-01-14T05:58:48", "2026-07-04T12:00:00", "2026-10-18T20:15:11"] {
            let secs = local_instant(iso).expect("a local instant");
            assert_eq!(local_civil(secs), iso);
        }
        assert_eq!(local_instant("not a date"), None);
    }

    /// The hour a spring-forward removes is no instant at all, so there is nothing to answer with:
    /// jiff's ordinary conversion returns the hour after the gap, which would pin a file to a time
    /// it does not read back as. The hour a fall-back repeats takes the first of the two.
    #[test]
    fn a_local_time_a_spring_forward_removed_has_no_instant() {
        let Ok(zone) = jiff::tz::TimeZone::get("America/New_York") else {
            lightcraft_testkit::skip!(System, "no IANA time zone database to read America/New_York from");
            return;
        };
        // 2026-03-08: 02:00 becomes 03:00, so nothing happened at 02:30
        assert_eq!(instant_in("2026-03-08T02:30:00", &zone), None);
        // the hours either side of the gap are ordinary, and round-trip
        for iso in ["2026-03-08T01:30:00", "2026-03-08T03:30:00"] {
            let secs = instant_in(iso, &zone).expect("either side of the gap");
            assert_eq!(civil_in(secs, &zone), iso);
        }
        // 2026-11-01: 02:00 becomes 01:00, so 01:30 happens twice; the first is the one in summer
        // time, 05:30 UTC, and it reads back as what was asked for
        let fold = instant_in("2026-11-01T01:30:00", &zone).expect("the repeated hour");
        assert_eq!(fold, lightcraft_meta::DateTime::parse_iso("2026-11-01T05:30:00Z").expect("a test timestamp").unix_seconds());
        assert_eq!(civil_in(fold, &zone), "2026-11-01T01:30:00");
    }

    /// Whatever the host's zone, what it produces is a date the rest of the catalog can read back,
    /// and it differs from UTC only by a whole number of minutes no larger than a real offset.
    #[test]
    fn local_civil_is_a_catalog_date_within_a_real_offset_of_utc() {
        let secs = lightcraft_meta::DateTime::parse_iso("2026-10-19T01:15:11Z").expect("a test timestamp").unix_seconds();
        let local = local_civil(secs);
        let read = lightcraft_catalog::dates::iso_seconds(&local).expect("a date the catalog can read");
        let shift = read - secs;
        assert_eq!(shift % 60, 0, "{local}");
        assert!(shift.abs() <= 18 * 3600, "{local}");
        assert_eq!(lightcraft_catalog::dates::normalize_iso(&local).as_deref(), Some(local.as_str()));
    }
}
