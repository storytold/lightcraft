use super::defaults::apply_import_defaults;
use super::files::copy_into;
use super::job::{Prepared, PreparedItem, ReadyFile};
use super::types::{Duplicate, ImportOptions, ImportReport, Kept, Moved, OnDeleted};
use super::{HashMap, Op, Path, PathBuf, Photo, PhotoId, ProbeInfo, Session, Source};

/// A batch's items in file order while its transfers are pending ([`ImportJob::run_plan`]).
#[derive(Default)]
pub(super) struct Plan {
    pub(super) slots: Vec<Slot>,
    pub(super) transfers: Vec<Transfer>,
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
    /// The folder (lowercase) and [`name_family`]: transfers of one family can take each other's
    /// names, so they never run at the same time.
    pub(super) family: (String, String),
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
            PreparedItem::Promote(id) => {
                if s.catalog.photo(id).is_some_and(|p| p.local) {
                    ops.push(Op::SetLocal { id, local: false });
                }
                report.imported.push(id.0);
            }
            PreparedItem::Duplicate { path, existing, reason, hash, file } => {
                let existing = existing.or_else(|| {
                    let h = hash?;
                    new_hash.get(&h).copied().or_else(|| s.catalog.photos().find(|p| p.content_hash.as_deref() == Some(h.as_str())).map(|p| p.id))
                });
                let trashed = existing.filter(|id| s.catalog.photo(*id).is_some_and(|p| p.deleted));
                match trashed {
                    // (browsing a folder in Local only looks: it never restores)
                    Some(id) if opts.on_deleted == OnDeleted::Restore && !opts.local => {
                        if !report.restored.contains(&id.0) {
                            ops.push(Op::SetDeleted { id, deleted: false });
                            // matched by content from another path, and the file the record
                            // points at is gone (moved, renamed): the photo follows the file
                            if reason == "content" && trashed_file_is_gone(&s.catalog, id) {
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
                let sidecar = sidecar.map(|sc| sc.resolve_label(&s.catalog)).filter(|sc| *sc != crate::sidecar::SidecarData::default());
                // a copy is catalogued under its new name
                let name = Path::new(&stored).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                let mut p = Photo::new(id, Source::File { path: stored }, &name, &info.format, info.width, info.height, &now);
                p.kind = info.kind;
                p.file_size = info.file_size;
                p.captured = info.captured;
                p.meta = info.meta;
                p.as_shot_wb = info.as_shot_wb;
                p.content_hash = info.content_hash;
                p.embedded_lens = info.embedded_lens;
                p.set_preview_only(info.preview_only.clone());
                apply_import_defaults(s, &mut p);
                if let Some(sc) = &sidecar {
                    crate::sidecar::merge_into(&mut p, sc, &now);
                    report.sidecars += 1;
                    report.warnings.extend(sc.warnings.iter().cloned().map(|mut warning| {
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
                    let d = std::sync::Arc::new(preset.apply(&p.develop, 1.0));
                    let label = format!("Preset: {}", preset.name);
                    p.develop = d.clone();
                    p.edited = Some(now.clone());
                    p.history.push(lightcraft_catalog::HistoryStep { label, settings: d });
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
/// still intact. Nothing is removed when the library can't be saved.
fn finish_moves(s: &mut Session, placed: Vec<crate::import_move::Placed>, log0: usize, report: &mut ImportReport) {
    // (saving here clears the pending ops, so the sidecars `execute` would auto-write are written here)
    let auto: Vec<Op> = if s.xmp.auto_write { s.pending_log.get(log0..).map(<[Op]>::to_vec).unwrap_or_default() } else { Vec::new() };
    let before = s.pending_log.len();
    if let Err(e) = s.persist() {
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
        civil(secs)
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
