use super::commit::{How, Plan, Slot, Transfer, commit_prepared, name_family, run_transfers, system_clock};
use super::files::{expand, probe_all};
use super::types::{ImportCandidate, ImportMode, ImportOptions, ImportReport, Kept, OnDeleted};
use super::*;

/// Import files/folders. See the module docs.
pub fn import(s: &mut Session, paths: &[String], mode: ImportMode) -> crate::Result<ImportReport> {
    import_with(s, paths, &ImportOptions { mode, ..Default::default() })
}

/// Import files/folders with options (preset, keywords). See the module docs.
///
/// The file-system half ([`ImportJob::prepare`]: expanding folders, probing, reading sidecars,
/// copying / placing files) and the catalog half ([`commit_prepared`]) are separate so the app can
/// run the first on a worker thread; here both run in turn.
pub fn import_with(s: &mut Session, paths: &[String], opts: &ImportOptions) -> crate::Result<ImportReport> {
    let mut job = ImportJob::new(s, opts.clone())?;
    let prepared = job.prepare(paths, &std::sync::atomic::AtomicBool::new(false));
    // probes a scan left for files not imported now stay for a later import
    s.import_probes.extend(std::mem::take(&mut job.cache));
    commit_prepared(s, &job.opts, &job.now, prepared)
}

/// Everything the file-system half of an import needs, detached from the session so it can run
/// on another thread (a network share or a card can take minutes). Built by [`ImportJob::new`];
/// [`ImportJob::prepare`] may be called for several batches in turn (renamed copies keep counting,
/// duplicates are found across batches); each batch is then [`commit_prepared`] on the session.
pub struct ImportJob {
    pub opts: ImportOptions,
    /// The mode in effect (Copy / Move become Add in a library without its own folder).
    mode: ImportMode,
    lib_dir: Option<PathBuf>,
    copy_root: Option<PathBuf>,
    /// The library as it was when the import was set up, until it is looked up
    /// ([`ImportJob::look_up_library`], with the first files): setting an import up is the
    /// session's thread, and a table of a library of millions of photos takes seconds. With it,
    /// whether a photo in Recently Deleted is passed over (an import that replaces such photos,
    /// as the options said when the import was set up).
    pub(super) library: Option<(lightcraft_catalog::Catalog, bool)>,
    /// path → (photo, a Local browse record?)
    pub(super) by_path: HashMap<String, (PhotoId, bool)>,
    /// content hash → the photo that has it (`None`: a file earlier in this import)
    pub(super) by_hash: HashMap<String, Option<PhotoId>>,
    pub(super) probe: Option<crate::media::FileProbe>,
    file_bytes: Option<crate::merge::ByteReader>,
    /// Probes from a preceding scan (not read again).
    cache: HashMap<String, ProbeInfo>,
    naming: crate::sidecar::SidecarNaming,
    now: String,
    seq: usize,
    /// Files found so far (folders expanded) and handled so far, for progress.
    pub total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Of those, the files that have to be read first (not probed by a preceding scan) and the
    /// ones read so far: the step before they are handled.
    pub read_total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub read_done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The outcome of [`ImportJob::prepare`] for one batch, in file order.
#[derive(Debug, Default)]
pub struct Prepared {
    pub scanned: usize,
    pub(super) items: Vec<PreparedItem>,
}

#[derive(Debug)]
pub(super) enum PreparedItem {
    /// A Local (browsed) record imported for real.
    Promote(PhotoId),
    Duplicate {
        path: String,
        existing: Option<PhotoId>,
        reason: &'static str,
        hash: Option<String>,
    },
    Failed(String, String),
    Kept(Kept),
    Ready(Box<ReadyFile>),
}

#[derive(Debug)]
pub(super) struct ReadyFile {
    pub(super) path: String,
    pub(super) stored: String,
    pub(super) info: ProbeInfo,
    pub(super) sidecar: Option<crate::sidecar::SidecarData>,
    pub(super) placed: Option<crate::import_move::Placed>,
}

impl Prepared {
    /// Undo what the batch did on disk that only a commit would make permanent: files placed by a
    /// Move (their sources were never touched). Copies stay, like copies of a failed commit.
    pub fn rollback(&self) {
        for it in &self.items {
            if let PreparedItem::Ready(r) = it
                && let Some(pl) = &r.placed
            {
                crate::import_move::rollback(pl);
            }
        }
    }

    /// Entries in the batch (after expanding folders).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Recheck Add-only entries against the owner catalog immediately before commit.
    /// Preparation may have run while another import added the same path or content.
    pub fn revalidate_add(&mut self, s: &Session) {
        let mut paths = HashMap::new();
        let mut hashes = HashMap::new();
        for photo in s.catalog.photos() {
            if let Source::File { path } = &photo.source {
                paths.insert(normalize_import_path(path), photo.id);
            }
            if let Some(hash) = &photo.content_hash {
                hashes.insert(hash.clone(), photo.id);
            }
        }
        let mut items = Vec::with_capacity(self.items.len());
        for item in std::mem::take(&mut self.items) {
            let PreparedItem::Ready(ready) = item else {
                items.push(item);
                continue;
            };
            let ReadyFile { path, stored, info, sidecar, placed } = *ready;
            let existing_path = paths.get(&normalize_import_path(&stored)).copied().or_else(|| paths.get(&normalize_import_path(&path)).copied());
            let existing_hash = info.content_hash.as_ref().and_then(|hash| hashes.get(hash).copied());
            if let Some(existing) = existing_path.or(existing_hash) {
                if let Some(placed) = &placed {
                    crate::import_move::rollback(placed);
                }
                items.push(PreparedItem::Duplicate {
                    path,
                    existing: Some(existing),
                    reason: if existing_path.is_some() { "path" } else { "content" },
                    hash: info.content_hash,
                });
            } else {
                items.push(PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed })));
            }
        }
        self.items = items;
    }
}

fn normalize_import_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) { path.to_lowercase() } else { path }
}

impl ImportJob {
    /// Snapshot what an import with `opts` needs from the session (no file-system calls). Takes
    /// the probes a preceding scan left ([`Session::import_probes`]).
    pub fn new(s: &mut Session, opts: ImportOptions) -> crate::Result<Self> {
        let mode = opts.mode;
        // Only a library on disk has an `Originals/` folder. The browser build keeps the bytes of
        // every added file in its own storage already, so "copy" there means "add".
        let transfer = matches!(mode, ImportMode::Copy | ImportMode::Move);
        let mode = if transfer && s.library.as_ref().is_some_and(|l| !l.on_disk) { ImportMode::Add } else { mode };
        if mode == ImportMode::Move && opts.local {
            return Err(crate::EngineError::Other("browsing a folder never moves its files".into()));
        }
        let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // where copies go: the chosen folder, else the library's Originals/
        let copy_root = opts
            .destination
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| lib_dir.as_ref().map(|l| l.join("Originals")));
        if matches!(mode, ImportMode::Copy | ImportMode::Move) && copy_root.is_none() {
            return Err(crate::EngineError::Other("copying or moving needs a destination folder or an open library".into()));
        }
        let fresh = opts.on_deleted == OnDeleted::Fresh;
        Ok(ImportJob {
            seq: opts.rename_start.max(1),
            opts,
            mode,
            lib_dir,
            copy_root,
            library: Some((s.catalog.clone(), fresh)),
            by_path: HashMap::new(),
            by_hash: HashMap::new(),
            probe: s.media.file_probe.clone(),
            file_bytes: s.media.file_bytes.clone(),
            cache: std::mem::take(&mut s.import_probes),
            naming: s.xmp.naming,
            now: (s.clock)(),
            total: Default::default(),
            done: Default::default(),
            read_total: Default::default(),
            read_done: Default::default(),
        })
    }

    /// The file-system half of importing `paths`: expand folders, skip known paths, probe, skip
    /// duplicates by content, read sidecars, copy or place (Move) the files — several at once,
    /// with the names and outcomes of one after another. Nothing in the catalog changes. Stops
    /// before the next file once `cancel` is set.
    pub fn prepare(&mut self, paths: &[String], cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        let files = self.expand(paths);
        self.prepare_files(files, cancel)
    }

    /// Expand `paths` (folders recursively, the library's own folder skipped) into the files an
    /// import would handle; counted in [`ImportJob::total`].
    pub fn expand(&self, paths: &[String]) -> Vec<String> {
        let files = expand(paths, self.lib_dir.as_deref());
        self.total.fetch_add(files.len(), std::sync::atomic::Ordering::Relaxed);
        files
    }

    /// When the import started ([`Session::clock`]), for [`commit_prepared`].
    pub fn now(&self) -> &str {
        &self.now
    }

    /// What the library held when the import was set up, by path and by content: made once, with
    /// the first files (on the thread that prepares them), from the library as it was then. The
    /// snapshot comes back: the files the import is adding are ranked against it for the ownership
    /// of the shared stem sidecars they read ([`crate::sidecar::ImportStems`]).
    fn look_up_library(&mut self) -> Option<lightcraft_catalog::Catalog> {
        let (library, fresh) = self.library.take()?;
        for p in library.photos() {
            // a trashed photo is replaced by a fresh import, so it doesn't count as a duplicate
            if p.deleted && fresh {
                continue;
            }
            if let Source::File { path } = &p.source {
                self.by_path.insert(path.clone(), (p.id, p.local));
            }
            if let Some(h) = &p.content_hash {
                self.by_hash.insert(h.clone(), Some(p.id));
            }
        }
        Some(library)
    }

    /// [`ImportJob::prepare`] for files [`ImportJob::expand`] returned.
    pub fn prepare_files(&mut self, files: Vec<String>, cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        use std::sync::atomic::Ordering::Relaxed;
        // the library as it was, kept for the ownership of the shared stem sidecars below
        let library = self.look_up_library();
        let opts = self.opts.clone();
        let mode = self.mode;
        let mut out = Prepared { scanned: files.len(), items: Vec::new() };
        let mut todo = Vec::new();
        for f in files {
            match self.by_path.get(f.as_str()).copied() {
                Some((id, true)) if !opts.local => {
                    // a file that was only browsed (Local) joins the library when it is imported for real
                    self.by_path.insert(f, (id, false));
                    out.items.push(PreparedItem::Promote(id));
                    self.done.fetch_add(1, Relaxed);
                }
                Some((id, _)) => {
                    out.items.push(PreparedItem::Duplicate { path: f, existing: Some(id), reason: "path", hash: None });
                    self.done.fetch_add(1, Relaxed);
                }
                None => todo.push(f),
            }
        }

        // files a preceding scan already probed are not read again (a network share is slow to read),
        // as long as they are still there, as they were: a file gone or changed since is probed again
        let cached: Vec<Option<ProbeInfo>> = todo
            .iter()
            .map(|f| {
                // (the web's files are not std::fs files: kept as probed)
                self.cache
                    .remove(f)
                    .filter(|info| cfg!(target_arch = "wasm32") || std::fs::metadata(f).is_ok_and(|m| m.is_file() && m.len() == info.file_size))
            })
            .collect();
        let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
        // Reading the files is the slow part of adding them where they are: it is counted on its
        // own, and `cancel` stops it too (a large library takes long enough to want both).
        self.read_total.fetch_add(missing.len(), Relaxed);
        let mut fresh = probe_all(self.probe.as_ref(), &missing, &self.read_done, cancel).into_iter();
        let probed: Vec<Result<ProbeInfo, String>> =
            cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
        crate::memory::release();
        // Who owns the shared stem sidecar (`IMG_0001.xmp`) among the library's files and the ones
        // arriving with this import: a JPEG or a TIFF arriving beside its raw reads its own sidecar
        // and never the raw's packet (issue #1905).
        let stems = crate::sidecar::ImportStems::for_import(
            library.as_ref(),
            todo.iter().zip(&probed).filter_map(|(path, info)| info.as_ref().ok().map(|i| (path.as_str(), i.kind))),
        );
        // Files are planned one by one, in order (duplicates, sidecars, names, folders); their
        // copies or moves — the slow part, reading and writing every byte and verifying it — are
        // collected and run on several threads (issue #367), then their outcomes take their places.
        let mut plan = Plan::default();
        for (path, info) in todo.into_iter().zip(probed) {
            if cancel.load(Relaxed) {
                break;
            }
            let mut info = match info {
                Ok(i) => i,
                Err(e) => {
                    log::warn!("import {path}: {e}");
                    self.done.fetch_add(1, Relaxed);
                    plan.slots.push(Slot::Item(PreparedItem::Failed(path, e)));
                    continue;
                }
            };
            // a copy of a file whose transfer is still pending: a duplicate only if that one succeeds
            if !opts.local
                && let Some(h) = &info.content_hash
                && plan.transfers.iter().any(|t| t.info.content_hash.as_ref() == Some(h))
            {
                self.run_plan(&mut plan, &mut out, cancel);
                if cancel.load(Relaxed) {
                    break;
                }
            }
            if let Some(h) = &info.content_hash
                && !opts.local
                && let Some(existing) = self.by_hash.get(h)
            {
                self.done.fetch_add(1, Relaxed);
                plan.slots.push(Slot::Item(PreparedItem::Duplicate { path, existing: *existing, reason: "content", hash: Some(h.clone()) }));
                continue;
            }
            // the XMP sidecar (or a raw's embedded XMP), read before copying: its capture time files
            // and names the copy when the file itself has none
            let raw = info.kind == MediaKind::Raw;
            let packet = crate::sidecar::find_sidecar_for_import(&stems, self.naming, &path)
                .and_then(|f| std::fs::read_to_string(f).ok())
                .or_else(|| info.xmp.clone().filter(|_| raw));
            let target = crate::crs::Target::for_file(info.kind, &info.format, info.preview_only.is_some(), &info.meta.camera);
            let sidecar = packet.and_then(|x| match crate::sidecar::parse_sidecar(&x, target) {
                Ok(sc) => Some(sc),
                Err(e) => {
                    log::warn!("import {path}: XMP: {e}");
                    None
                }
            });
            if info.captured.is_none() {
                info.captured = sidecar.as_ref().and_then(|sc| sc.captured.clone());
            }
            // no date in the file or its sidecar: the file's own date, as Lightroom does
            if info.captured.is_none() {
                info.captured = info.file_modified.clone();
            }
            let stored = match (mode, &self.copy_root) {
                (ImportMode::Copy | ImportMode::Move, Some(root))
                    if !crate::import_move::inside(Path::new(&path), root)
                        && !self.lib_dir.as_ref().is_some_and(|l| crate::import_move::inside(Path::new(&path), l)) =>
                {
                    // the templates see the photo as it will be catalogued (undated: the import time)
                    let own = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let mut q = Photo::new(PhotoId(0), Source::File { path: path.clone() }, &own, &info.format, 0, 0, &self.now);
                    q.captured = info.captured.clone();
                    q.meta = info.meta.clone();
                    let name = opts.rename.as_deref().filter(|t| !t.trim().is_empty()).map(|t| {
                        let n = crate::rename::expand(t, &q, self.seq);
                        self.seq += 1;
                        n
                    });
                    let folders = opts.organize.folders(&q);
                    let dir = folders.iter().fold(root.to_path_buf(), |d, f| d.join(f));
                    // the name this file gets depends on what a pending transfer of a like-named file
                    // into the same folder ends up with: that one goes first
                    let as_named = name.as_deref().unwrap_or(if own.is_empty() { "photo" } else { own.as_str() });
                    let family = (dir.to_string_lossy().to_lowercase(), name_family(as_named));
                    if plan.transfers.iter().any(|t| t.family == family) {
                        self.run_plan(&mut plan, &mut out, cancel);
                        if cancel.load(Relaxed) {
                            break;
                        }
                    }
                    let how = if mode == ImportMode::Move && !crate::import_move::is_symlink(Path::new(&path)) {
                        How::Place { name: name.unwrap_or(own) }
                    } else {
                        if mode == ImportMode::Move {
                            let reason = "a symbolic link: the file it points to was copied, the link and its target stay";
                            plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                        }
                        How::Copy { name }
                    };
                    // Copy as DNG: the copied raw becomes a DNG (the copy is ours to replace)
                    let dng = opts.convert_dng && mode == ImportMode::Copy && raw && !info.format.eq_ignore_ascii_case("DNG");
                    plan.slots.push(Slot::Transfer);
                    plan.transfers.push(Transfer { path, info, sidecar, dir, how, dng, family });
                    continue;
                }
                (ImportMode::Move, _) => {
                    let reason = "already inside the destination or the library: added where it is";
                    plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                    path.clone()
                }
                _ => path.clone(),
            };
            if let Some(h) = &info.content_hash {
                self.by_hash.insert(h.clone(), None);
            }
            self.done.fetch_add(1, Relaxed);
            plan.slots.push(Slot::Item(PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed: None }))));
        }
        self.run_plan(&mut plan, &mut out, cancel);
        out
    }

    /// Run the transfers `plan` collected (on several threads) and append its items to `out` in
    /// file order. Transfers not started because `cancel` was set leave no item.
    fn run_plan(&mut self, plan: &mut Plan, out: &mut Prepared, cancel: &std::sync::atomic::AtomicBool) {
        let mut done = run_transfers(std::mem::take(&mut plan.transfers), self.file_bytes.as_ref(), cancel, &self.done).into_iter();
        for slot in std::mem::take(&mut plan.slots) {
            let item = match slot {
                Slot::Item(it) => it,
                Slot::Transfer => match done.next().flatten() {
                    Some(it) => it,
                    None => continue,
                },
            };
            if let PreparedItem::Ready(r) = &item
                && let Some(h) = &r.info.content_hash
            {
                self.by_hash.insert(h.clone(), None);
            }
            out.items.push(item);
        }
    }
}

impl Session {
    /// A thumbnail for an import candidate (not in the catalog): a raw's embedded preview, else a
    /// small render of the file. `slot` makes the job's photo id unique (`u64::MAX - slot`).
    pub fn candidate_thumb_job(&mut self, c: &ImportCandidate, edge: usize, slot: u64) -> Option<crate::media::QuickJob> {
        if c.error.is_some() {
            return None;
        }
        let id = PhotoId(u64::MAX - slot);
        let mut p = Photo::new(id, Source::File { path: c.path.clone() }, &c.name, &c.format, c.width.max(1), c.height.max(1), "");
        p.kind = c.kind;
        if let Some(info) = self.import_probes.get(&c.path) {
            p.as_shot_wb = info.as_shot_wb;
            p.embedded_lens = info.embedded_lens;
            p.preview_only = info.preview_only.clone();
        }
        p.develop = std::sync::Arc::new(p.import_defaults());
        let edge = edge.clamp(64, crate::media::SourceLevel::Thumb.max_edge());
        let level = crate::media::SourceLevel::Thumb;
        let source = self.media.origin_ref(&p.source, level.max_edge());
        let key = lightcraft_preview::Hasher128::new().str(&c.path).u64(c.file_size).u64(edge as u64).finish().0 as u64;
        let small = crate::media::RenderJob {
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: None,
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: crate::media::source_info(&p),
            settings: p.develop.clone(),
            request: lightcraft_pipeline::RenderRequest::fit(edge, edge),
            key,
            cache: None,
            stages: None,
            view_cache: None,
            display: None,
            slide: None,
            preview_cut: None,
            display_fit: None,
        };
        let embedded = match (&self.media.preview_loader, c.kind) {
            (Some(l), MediaKind::Raw) => Some((c.path.clone(), l.clone(), edge)),
            _ => None,
        };
        Some(crate::media::QuickJob { request_id: 0, photo: id, key, cached: Vec::new(), embedded, small: Some(Box::new(small)) })
    }

    /// Use the system clock for import/edit times (native hosts).
    pub fn with_system_clock(mut self) -> Self {
        self.clock = Box::new(system_clock);
        self
    }
}
