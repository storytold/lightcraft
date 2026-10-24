use super::fs::{MoveFs, RealFs, folded_path};
use super::moves::{BatchMoveError, move_all, set_file_ops_to};
use super::sidecar::{PacketVersion, retry_sidecars, retry_sidecars_for_paths};
use super::tokens::{expand, split_ext};
use super::{CatalogError, EngineError, HashMap, HashSet, Op, Path, PathBuf, Photo, PhotoId, RenamePlan, Result, Session, Source};

/// The photos among `ids` a rename handles, in order: photos sharing one file (virtual copies)
/// rename once. No file-system calls.
pub fn rename_photos(cat: &lightcraft_catalog::Catalog, ids: &[PhotoId]) -> Vec<std::sync::Arc<Photo>> {
    let mut seen_paths: HashSet<String> = HashSet::new();
    ids.iter()
        .filter_map(|id| cat.photo(*id))
        .filter(|p| match &p.source {
            Source::File { path } => seen_paths.insert(path.clone()),
            _ => true,
        })
        .cloned()
        .collect()
}

/// [`Session::plan_rename`] for [`rename_photos`], with the "is this name taken on disk?" check
/// supplied: the Rename dialog plans its preview on a worker thread (a check can block on a slow
/// drive). Whether a case-only change names this very file is decided on the disk.
pub fn plan_rename_photos(photos: &[std::sync::Arc<Photo>], template: &str, start: usize, exists: &dyn Fn(&str) -> bool) -> Vec<RenamePlan> {
    plan_rename_core(photos, template, start, &|p| exists(&p.to_string_lossy()), &|a, b| RealFs.same_file(a, b))
}

/// Plan renaming `photos`: `exists` says whether something answers to a path, `same_file` whether
/// two spellings name one file (see [`MoveFs`]).
fn plan_rename_core(
    photos: &[std::sync::Arc<Photo>],
    template: &str,
    start: usize,
    exists: &dyn Fn(&Path) -> bool,
    same_file: &dyn Fn(&Path, &Path) -> bool,
) -> Vec<RenamePlan> {
    let mut taken: HashSet<String> = HashSet::new(); // lower-case target paths/names claimed in this batch
    let mut out = Vec::new();
    for (i, p) in photos.iter().enumerate() {
        let seq = start.saturating_add(i);
        let want = expand(template, p, seq);
        let (stem, ext) = split_ext(&want);
        let (stem, ext) = (stem.to_string(), ext.to_string());
        let candidate = |k: usize| {
            if k == 0 {
                want.clone()
            } else if ext.is_empty() {
                format!("{stem}-{k}")
            } else {
                format!("{stem}-{k}.{ext}")
            }
        };
        let (to, to_path) = match &p.source {
            Source::File { path } => {
                let dir = Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default();
                let mut k = 0;
                loop {
                    let name = candidate(k);
                    let tp = dir.join(&name).to_string_lossy().to_string();
                    let key = folded_path(&tp);
                    // this very file, maybe spelled differently (case-insensitive volume)? By
                    // identity: on a case-sensitive volume `img_1.jpg` may be another photo
                    let (from, to) = (Path::new(path), Path::new(&tp));
                    let same = from == to || (key == folded_path(path) && exists(to) && same_file(from, to));
                    // free: not claimed in this batch and not on disk (unless it is this very file).
                    // A file this batch moves away still counts as taken: simple and safe.
                    if !taken.contains(&key) && (same || !exists(to)) {
                        taken.insert(key);
                        break (name, Some(tp));
                    }
                    k += 1;
                }
            }
            Source::Demo { .. } => {
                let mut k = 0;
                loop {
                    let name = candidate(k);
                    if taken.insert(format!("demo:{}", name.to_lowercase())) {
                        break (name, None);
                    }
                    k += 1;
                }
            }
        };
        let from_path = match &p.source {
            Source::File { path } => Some(path.clone()),
            _ => None,
        };
        out.push(RenamePlan { id: p.id.0, from: p.file_name.clone(), to, from_path, to_path });
    }
    out
}

impl Session {
    /// Plan renaming `ids` with `template` (sequence numbers from `start`), resolving collisions.
    pub fn plan_rename(&self, ids: &[PhotoId], template: &str, start: usize) -> Vec<RenamePlan> {
        self.plan_rename_with(&RealFs, ids, template, start)
    }

    /// [`Session::plan_rename`] on the file system `fs`.
    pub(crate) fn plan_rename_with(&self, fs: &dyn MoveFs, ids: &[PhotoId], template: &str, start: usize) -> Vec<RenamePlan> {
        plan_rename_core(&rename_photos(&self.catalog, ids), template, start, &|p| fs.exists(p), &|a, b| fs.same_file(a, b))
    }

    /// Carry out `plans`: move the files (rolled back on failure), then commit one undoable op
    /// that updates the photos (and their virtual copies).
    ///
    /// When a failure can't be fully rolled back, the files that kept their new names are
    /// committed as a smaller undoable rename, so the library points at them (nothing goes
    /// missing), and the error lists them (old → new).
    pub fn apply_rename(&mut self, plans: &[RenamePlan]) -> Result<usize> {
        self.apply_rename_with(&RealFs, plans)
    }

    /// Complete retained repairs before a file or folder change can stale their exact paths.
    pub(crate) fn repair_sidecars_with(&mut self, fs: &dyn MoveFs) -> Result<()> {
        for repair in self.pending_sidecar_repairs.iter().chain(self.undo.iter().chain(&self.redo).flat_map(|e| &e.retry_sidecars)) {
            repair.validate_repair(fs).map_err(EngineError::Other)?;
        }
        retry_sidecars(fs, &mut self.pending_sidecar_repairs).map_err(EngineError::Other)?;
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            retry_sidecars(fs, &mut entry.retry_sidecars).map_err(EngineError::Other)?;
        }
        Ok(())
    }

    pub(crate) fn repair_sidecars_for_paths(&mut self, paths: &[PathBuf], fs: &dyn MoveFs) -> Result<()> {
        self.validate_sidecar_packets_for_paths(paths, fs)?;
        retry_sidecars_for_paths(fs, &mut self.pending_sidecar_repairs, paths).map_err(EngineError::Other)?;
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            retry_sidecars_for_paths(fs, &mut entry.retry_sidecars, paths).map_err(EngineError::Other)?;
        }
        Ok(())
    }

    /// A healthy shared packet may be saved while another photo's repair is unavailable.
    /// Check its retained content too, without forcing that unrelated physical operation.
    pub(crate) fn validate_sidecar_packets_for_paths(&self, paths: &[PathBuf], fs: &dyn MoveFs) -> Result<()> {
        for repair in self.pending_sidecar_repairs.iter().chain(self.undo.iter().chain(&self.redo).flat_map(|e| &e.retry_sidecars)) {
            if paths.iter().any(|path| repair.touches(fs, path)) {
                repair.validate_repair(fs).map_err(EngineError::Other)?;
            } else {
                for packet in &repair.companions {
                    if paths.iter().any(|path| fs.same_file(path, &packet.path)) {
                        packet.validate(fs).map_err(EngineError::Other)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Only a successful application write updates a retained content version. Atomic writes
    /// replace inodes; aliases that now resolve to the written path observe the new content.
    pub(crate) fn note_sidecar_write(&mut self, path: &Path, bytes: &[u8]) {
        let version = PacketVersion::of(bytes);
        for repair in self.pending_sidecar_repairs.iter_mut().chain(self.undo.iter_mut().chain(&mut self.redo).flat_map(|e| &mut e.retry_sidecars)) {
            for packet in &mut repair.companions {
                if RealFs.same_file(path, &packet.path) {
                    packet.version = Some(version);
                }
            }
        }
    }

    /// [`Session::apply_rename`] on the file system `fs`.
    pub(crate) fn apply_rename_with(&mut self, fs: &dyn MoveFs, plans: &[RenamePlan]) -> Result<usize> {
        self.apply_file_plans(fs, plans, &|n| format!("Rename {n} Photo{}", if n == 1 { "" } else { "s" }))
    }

    /// Carry out `plans` (renames or moves to another folder): move the files with their sidecars
    /// (all or nothing), then commit one undoable op, labelled `label(count)`, that points the
    /// photos (and their virtual copies) at the new paths. Undo moves the files back.
    pub(crate) fn apply_file_plans(&mut self, fs: &dyn MoveFs, plans: &[RenamePlan], label: &dyn Fn(usize) -> String) -> Result<usize> {
        // A partly completed rename can already have the requested photo name. Repair
        // its sidecars before treating that empty move list as a successful retry.
        self.repair_sidecars_with(fs)?;
        let mut ops = Vec::new();
        let by_path: HashMap<&str, &RenamePlan> = plans.iter().filter_map(|p| p.from_path.as_deref().map(|f| (f, p))).collect();
        for p in self.catalog.photos() {
            let plan = match &p.source {
                Source::File { path } => by_path.get(path.as_str()).copied(),
                Source::Demo { .. } => plans.iter().find(|x| x.id == p.id.0 && x.from_path.is_none()),
            };
            let Some(plan) = plan else { continue };
            let source = match &plan.to_path {
                Some(t) if matches!(&p.source, Source::File { path } if Path::new(path) == Path::new(t)) => p.source.clone(),
                Some(t) => Source::File { path: t.clone() },
                None => p.source.clone(),
            };
            if plan.to != p.file_name || source != p.source {
                ops.push(Op::SetFile { id: p.id, file_name: plan.to.clone(), source });
            }
        }
        let n = ops.len();
        let op = Op::Batch { ops };
        let moves: Vec<(String, String)> = plans
            .iter()
            .filter_map(|pl| match (&pl.from_path, &pl.to_path) {
                (Some(a), Some(b)) if Path::new(a) != Path::new(b) => Some((a.clone(), b.clone())),
                _ => None,
            })
            .collect();
        if let Err(e) = move_all(fs, &moves) {
            return Err(EngineError::Other(self.follow_stuck(&op, e, true).0));
        }
        if n > 0
            && let Err(err) = self.commit(&label(n), op)
        {
            let back: Vec<(String, String)> = moves.iter().rev().map(|(a, b)| (b.clone(), a.clone())).collect();
            if let Err(e) = move_all(fs, &back) {
                return Err(EngineError::Other(format!("{err}; the renamed files could not all be moved back: {}", e.message)));
            }
            if matches!(err, EngineError::Catalog(CatalogError::PartlyApplied { .. }))
                && let Some(step) = self.undo.last_mut()
            {
                // (the files are back: what is left of the rollback must point the photos at them, not move them)
                step.op = without_file_moves(std::mem::replace(&mut step.op, Op::Batch { ops: Vec::new() }));
            }
            return Err(err);
        }
        Ok(n)
    }

    /// After a failed batch of moves for `op`: point the photos whose files stayed moved at
    /// their new paths (an undoable "partly" rename when `undoable`, otherwise a plain logged
    /// change), so the library matches the disk. Returns the message and, for a history retry,
    /// the correction's inverse file ops (so a retry doesn't lose the original paths).
    pub(crate) fn follow_stuck(&mut self, op: &Op, e: BatchMoveError, undoable: bool) -> (String, Vec<Op>) {
        let mut msg = e.message;
        let pending_sidecars = e.pending_sidecars;
        if e.stuck.is_empty() && (!undoable || pending_sidecars.is_empty()) {
            return (msg, Vec::new());
        }
        let to: HashSet<&str> = e.stuck.iter().map(|m| m.1.as_str()).collect();
        let mut keep = Vec::new();
        set_file_ops_to(op, &to, &mut keep);
        let list: Vec<String> = e.stuck.iter().map(|(a, b)| format!("{a} → {b}")).collect();
        let n = e.stuck.len();
        let s = if n == 1 { "" } else { "s" };
        let fix = Op::Batch { ops: keep };
        let mut inverses = Vec::new();
        let applied = if undoable {
            let label = if n == 0 { "Repair Rename Sidecars".to_string() } else { format!("Rename {n} Photo{s} (partly)") };
            self.commit(&label, fix).map(|()| {
                if let Some(entry) = self.undo.last_mut() {
                    entry.retry_sidecars.extend(pending_sidecars);
                }
            })
        } else {
            match self.apply_catalog(fix.clone()) {
                Ok(inv) => {
                    inverses = match inv {
                        Op::Batch { ops } => ops,
                        op => vec![op],
                    };
                    self.pending_log.push(fix);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        };
        match applied {
            Ok(()) if n > 0 => msg
                .push_str(&format!(". {n} file{s} could not be moved back and kept the new name; the library now points there: {}", list.join(", "))),
            Ok(()) => {}
            Err(err) => msg.push_str(&format!(
                ". {n} file{s} could not be moved back and the library could not follow ({err}); relink with Find Missing Photos: {}",
                list.join(", ")
            )),
        }
        (msg, inverses)
    }

    /// Physical moves to the final `SetFile` destination of each photo. A merged history
    /// batch can rename the same photo repeatedly; intermediate catalog paths need no file.
    pub(crate) fn file_moves(&self, op: &Op) -> Vec<(String, String)> {
        let mut pending = vec![op];
        let mut order = Vec::new();
        let mut targets = HashMap::new();
        while let Some(op) = pending.pop() {
            match op {
                Op::Batch { ops } => pending.extend(ops.iter().rev()),
                Op::SetFile { id, source, .. } if targets.insert(*id, source).is_none() => {
                    order.push(*id);
                }
                _ => {}
            }
        }
        // Virtual copies share one physical move, keeping the original batch order.
        let mut seen = HashSet::new();
        order
            .into_iter()
            .filter_map(|id| {
                let Source::File { path: to } = targets.get(&id).copied()? else { return None };
                let Source::File { path: from } = &self.catalog.photo(id)?.source else { return None };
                (Path::new(from) != Path::new(to) && seen.insert(from)).then(|| (from.clone(), to.clone()))
            })
            .collect()
    }
}
