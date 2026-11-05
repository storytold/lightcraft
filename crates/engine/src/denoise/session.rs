#[cfg(doc)]
use super::model::Model;
use super::model::STACK;
use super::products::{Active, JobSpec, MakeError, Progress, eligible, make_product, product_key, product_path, products_in};
use super::settings::{DIR, SETTINGS_TTL, installed_models, read_settings};
use super::state::{AUTO_LOOK, Failure, MAX_QUEUE, Outcome, Pace, PhotoState, Running};
use super::{
    Arc, DenoiseSpec, DevelopSettings, HashMap, HashSet, MakeProduct, Mutex, Ordering, PairLoader, Path, PathBuf, Photo, PhotoId, PoisonError,
    Selection, Session, Source, Value, json, product,
};

impl Session {
    /// Where the active model's tiles run (see [`Model::device`]); `{kind: "none"}` while no model is loaded.
    pub(crate) fn denoise_device(&self) -> Value {
        let loaded = self.denoise.active.as_ref().and_then(|a| a.model.lock().unwrap_or_else(PoisonError::into_inner).clone());
        match loaded {
            Some(m) => m.device(self.denoise.settings.run_on()),
            None => json!({"kind": "none"}),
        }
    }

    /// The folder the products of the open library are kept in; `None` without a library on disk.
    pub(crate) fn denoise_products_dir(&self) -> Option<PathBuf> {
        self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(DIR))
    }

    /// Read which model is in use (at most every couple of seconds): the manifest and file of the model the settings
    /// name, if it is installed.
    pub(crate) fn denoise_refresh_active(&mut self, force: bool) {
        let fresh = self.denoise.active_seen.is_some_and(|t| t.elapsed() < SETTINGS_TTL);
        if fresh && !force {
            return;
        }
        self.denoise.active_seen = Some(web_time::Instant::now());
        self.denoise.settings = self.denoise.models_dir.as_deref().map(read_settings).unwrap_or_default();
        let chosen = self.denoise.models_dir.as_deref().and_then(|dir| {
            let id = self.denoise.settings.model.clone()?;
            installed_models(dir).into_iter().find(|i| i.manifest.id == id)
        });
        let found = chosen.map(|i| (i.fingerprint(), i));
        let same = match (&self.denoise.active, &found) {
            (None, None) => true,
            (Some(a), Some((fp, _))) => a.fingerprint == *fp,
            _ => false,
        };
        if same {
            return;
        }
        // another model: what was made with the old one is not this one's, and what failed may work now
        self.denoise.active = found.map(|(fingerprint, i)| Active {
            id: i.manifest.id.clone(),
            manifest: i.manifest,
            onnx: i.onnx,
            fingerprint,
            model: Arc::new(Mutex::new(None)),
        });
        if let Some(r) = self.denoise.running.take() {
            r.progress.cancel_now();
        }
        self.denoise.queue.clear();
        self.denoise.failed.clear();
        self.denoise.dirty = true;
    }

    fn denoise_spec(&self, p: &Photo) -> Option<(DenoiseSpec, String)> {
        let (dir, active) = (self.denoise_products_dir()?, self.denoise.active.as_ref()?);
        let key = product_key(&active.fingerprint, p);
        Some((DenoiseSpec { product: product_path(&dir, &key), key: key.clone(), make: None }, key))
    }

    /// Make the index of ready photos from the products on disk (a directory listing and a hash per photo: no file is
    /// opened). Pictures that appeared or vanished drop the decoded sources they affect. The photo being made is its
    /// job's to report (see [`Self::denoise_check_photo`]).
    pub(crate) fn denoise_reindex(&mut self) {
        self.denoise.dirty = false;
        self.denoise.reindexed = Some((self.catalog.revision, web_time::Instant::now()));
        let running = self.denoise.running.as_ref().map(|r| r.photo);
        let mut now: HashMap<PhotoId, DenoiseSpec> = HashMap::new();
        if let (Some(dir), Some(active)) = (self.denoise_products_dir(), self.denoise.active.as_ref()) {
            let names: HashSet<String> =
                products_in(&dir).into_iter().filter_map(|(p, _, _)| p.file_stem().and_then(|s| s.to_str()).map(str::to_string)).collect();
            if !names.is_empty() {
                for p in self.catalog.photos().filter(|p| eligible(p) && running != Some(p.id)) {
                    let key = product_key(&active.fingerprint, p);
                    if names.contains(&key) {
                        now.insert(p.id, DenoiseSpec { product: product_path(&dir, &key), key, make: None });
                    }
                }
            }
        }
        let mut changed = false;
        for id in self.media.denoise.ids() {
            if !now.contains_key(&id) {
                self.media.denoise.remove(id);
                self.media.forget(id);
                changed = true;
            }
        }
        for (id, spec) in now {
            if self.media.denoise.spec(id).is_none_or(|old| old.key != spec.key) {
                self.media.denoise.set(id, spec);
                self.media.forget(id);
                changed = true;
            }
        }
        if changed {
            self.denoise.generation += 1;
        }
    }

    /// A photo whose file changed (its content key is not the one its index entry was made for) has no picture.
    ///
    /// The photo whose picture is being made is left to its job: the worker saves the picture before it reports, so the
    /// file is on disk while the job still counts as running. Finding it here would make the photo Ready while the session
    /// is busy with it (#719); [`Self::denoise_poll`] makes it Ready in the pump that takes the job's result instead.
    fn denoise_check_photo(&mut self, id: PhotoId) {
        if self.denoise.running.as_ref().is_some_and(|r| r.photo == id) {
            return;
        }
        let Some(p) = self.catalog.photo(id).cloned() else { return };
        let Some((spec, _)) = self.denoise_spec(&p) else { return };
        match self.media.denoise.spec(id) {
            Some(have) if have.key != spec.key => {
                self.media.denoise.remove(id);
                self.media.forget(id);
                self.denoise.generation += 1;
            }
            None if spec.product.is_file() && product::is_current(&spec.product, &spec.key) => {
                // made by an export, or by another session
                self.media.denoise.set(id, spec);
                self.media.forget(id);
                self.denoise.generation += 1;
            }
            _ => {}
        }
    }

    /// How photo `id` stands.
    pub fn denoise_photo_state(&self, id: PhotoId) -> PhotoState {
        let Some(p) = self.catalog.photo(id) else { return PhotoState::NotApplicable };
        if !eligible(p) {
            return PhotoState::NotApplicable;
        }
        if self.denoise.active.is_none() {
            // the model folder has not been looked at yet (just started): not "no model" until it has been
            return if self.denoise.active_seen.is_none() { PhotoState::Idle } else { PhotoState::NoModel };
        }
        if self.denoise_products_dir().is_none() {
            return PhotoState::Failed {
                why: "AI Denoise keeps its pictures in the library folder, and this session has none.".into(),
                unsupported: true,
            };
        }
        if self.media.denoise.spec(id).is_some() {
            return PhotoState::Ready;
        }
        if let Some(r) = self.denoise.running.as_ref().filter(|r| r.photo == id) {
            return PhotoState::Running { done: r.progress.done.load(Ordering::Relaxed), total: r.progress.total.load(Ordering::Relaxed) };
        }
        if let Some(ahead) = self.denoise.queue.iter().position(|q| *q == id) {
            return PhotoState::Queued { ahead: ahead + usize::from(self.denoise.running.is_some()) };
        }
        let key = self.denoise_spec(p).map(|s| s.1);
        match self.denoise.failed.get(&id) {
            Some(f) if Some(&f.key) == key.as_ref() => PhotoState::Failed { why: f.why.clone(), unsupported: f.unsupported },
            _ => PhotoState::Idle,
        }
    }

    /// Ask for photos' pictures to be made (the ones that are not made, queued or known to be impossible). With `retry`
    /// a photo that failed before is tried again. A photo that is not raw is skipped. Returns how many were queued.
    pub(crate) fn denoise_enqueue(&mut self, ids: &[PhotoId], front: bool, retry: bool) -> usize {
        self.denoise_refresh_active(false);
        if self.denoise.active.is_none() || self.denoise_products_dir().is_none() {
            return 0;
        }
        let mut added = 0;
        for id in ids {
            let Some(p) = self.catalog.photo(*id).cloned() else { continue };
            if !eligible(&p) || self.media.denoise.spec(*id).is_some() || self.denoise.queue.contains(id) {
                continue;
            }
            if self.denoise.running.as_ref().is_some_and(|r| r.photo == *id) || self.denoise.queue.len() >= MAX_QUEUE {
                continue;
            }
            let Some((_, key)) = self.denoise_spec(&p) else { continue };
            if retry {
                self.denoise.failed.remove(id);
            } else if self.denoise.failed.get(id).is_some_and(|f| f.key == key) {
                continue;
            }
            if front {
                self.denoise.queue.push_front(*id);
            } else {
                self.denoise.queue.push_back(*id);
            }
            added += 1;
        }
        added
    }

    /// Stop the job that is running, and with `all` forget everything that waits.
    pub(crate) fn denoise_cancel(&mut self, photo: Option<PhotoId>, all: bool) -> usize {
        let mut n = 0;
        if let Some(r) = &self.denoise.running
            && photo.is_none_or(|p| p == r.photo)
        {
            r.progress.cancel_now();
            n += 1;
        }
        let before = self.denoise.queue.len();
        match photo {
            Some(p) => self.denoise.queue.retain(|q| *q != p),
            None if all => self.denoise.queue.clear(),
            None => {}
        }
        n + before - self.denoise.queue.len()
    }

    /// The photos the automatic queue looks at: the one open and the ones selected, those with a Denoise amount.
    fn denoise_wanted(&self) -> Vec<PhotoId> {
        let Selection { active, ids, .. } = &self.selection;
        active
            .iter()
            .chain(ids.iter().take(AUTO_LOOK))
            .copied()
            .filter(|id| self.catalog.photo(*id).is_some_and(|p| p.develop.denoise_amount() > 0.0))
            .collect()
    }

    fn denoise_job_spec(&self, p: &Photo, parallel: usize) -> Option<JobSpec> {
        let Source::File { path } = &p.source else { return None };
        let (spec, _) = self.denoise_spec(p)?;
        let active = self.denoise.active.as_ref()?;
        Some(JobSpec {
            path: path.clone(),
            read: self.media.file_bytes.clone(),
            product: spec.product,
            key: spec.key,
            manifest: active.manifest.clone(),
            onnx: active.onnx.clone(),
            model: active.model.clone(),
            loader: self.denoise.loader.clone(),
            parallel,
            run_on: self.denoise.settings.run_on(),
        })
    }

    /// Start the next queued photo on its own thread.
    fn denoise_start_next(&mut self, parallel: usize) {
        while let Some(id) = self.denoise.queue.pop_front() {
            let Some(p) = self.catalog.photo(id).cloned() else { continue };
            if !eligible(&p) || self.media.denoise.spec(id).is_some() {
                continue;
            }
            let Some(spec) = self.denoise_job_spec(&p, parallel) else { continue };
            // another session or an export may have made it since it was queued
            if product::is_current(&spec.product, &spec.key) {
                self.denoise_check_photo(id);
                continue;
            }
            let progress = Arc::new(Progress::default());
            let outcome: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
            let (key, product) = (spec.key.clone(), spec.product.clone());
            let (worker_progress, worker_outcome) = (progress.clone(), outcome.clone());
            let started = std::thread::Builder::new().name("denoise".into()).stack_size(STACK).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_product(&spec, Some(&worker_progress))));
                let o = match result {
                    Ok(Ok(_)) => Outcome::Done,
                    Ok(Err(MakeError::Cancelled)) => Outcome::Cancelled,
                    Ok(Err(MakeError::Unsupported(why))) => Outcome::Failed(why, true),
                    Ok(Err(MakeError::Failed(why))) => Outcome::Failed(why, false),
                    Err(_) => Outcome::Failed("the denoiser stopped unexpectedly".into(), false),
                };
                *worker_outcome.lock().unwrap_or_else(PoisonError::into_inner) = Some(o);
            });
            match started {
                Ok(_) => {
                    self.denoise.running = Some(Running { photo: id, key, product, progress, outcome, started: web_time::Instant::now() });
                }
                Err(e) => {
                    self.denoise.failed.insert(id, Failure { key, why: format!("could not start the denoiser: {e}"), unsupported: false });
                    continue;
                }
            }
            return;
        }
    }

    /// Take in the running job's result.
    fn denoise_poll(&mut self) {
        let Some(r) = &self.denoise.running else { return };
        let Some(outcome) = r.outcome.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
        let Some(r) = self.denoise.running.take() else { return };
        match outcome {
            Outcome::Done => {
                self.denoise.made += 1;
                self.denoise_check_photo(r.photo);
                self.denoise_evict(Some(&r.product));
            }
            Outcome::Failed(why, unsupported) => {
                self.denoise.failed.insert(r.photo, Failure { key: r.key, why, unsupported });
            }
            Outcome::Cancelled => {}
        }
        // the picture's state changed: the interface draws it again
        self.denoise.generation += 1;
    }

    /// Keep the cache within its limit: the pictures nobody asks for go first, the oldest first; `keep` (the one just
    /// made) never goes. The index forgets what was deleted.
    pub(crate) fn denoise_evict(&mut self, keep: Option<&Path>) {
        let Some(models) = self.denoise.models_dir.clone() else { return };
        self.denoise_evict_to(read_settings(&models).cache_bytes(), keep);
    }

    /// [`Self::denoise_evict`] for a limit of `limit` bytes.
    pub(crate) fn denoise_evict_to(&mut self, limit: u64, keep: Option<&Path>) {
        let Some(dir) = self.denoise_products_dir() else { return };
        let mut files = products_in(&dir);
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= limit {
            return;
        }
        // the pictures photos use now (a Denoise amount above 0) are the last to go
        let wanted: HashSet<PathBuf> = self
            .media
            .denoise
            .ids()
            .into_iter()
            .filter(|id| self.catalog.photo(*id).is_some_and(|p| p.develop.denoise_amount() > 0.0))
            .filter_map(|id| self.media.denoise.spec(id).map(|s| s.product.clone()))
            .collect();
        files.sort_by_key(|(path, _, modified)| (wanted.contains(path), *modified));
        let mut gone: HashSet<PathBuf> = HashSet::new();
        for (path, size, _) in files {
            if total <= limit {
                break;
            }
            if keep == Some(path.as_path()) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                gone.insert(path);
            }
        }
        if gone.is_empty() {
            return;
        }
        for id in self.media.denoise.ids() {
            if self.media.denoise.spec(id).is_some_and(|s| gone.contains(&s.product)) {
                self.media.denoise.remove(id);
                self.media.forget(id);
            }
        }
        self.denoise.generation += 1;
    }

    /// Delete every product of this library (they are made again when wanted).
    pub(crate) fn denoise_clear(&mut self) -> (usize, u64) {
        self.denoise_cancel(None, true);
        let (mut files, mut bytes) = (0, 0);
        if let Some(dir) = self.denoise_products_dir() {
            for (path, size, _) in products_in(&dir) {
                if std::fs::remove_file(&path).is_ok() {
                    files += 1;
                    bytes += size;
                }
            }
        }
        self.denoise.failed.clear();
        self.denoise.touch();
        (files, bytes)
    }

    /// Per frame: take in what finished, look at what the user is looking at, and keep one job going. Cheap when
    /// nothing is happening (no file is opened unless a photo with a Denoise amount lacks its picture).
    pub(crate) fn denoise_pump(&mut self, pace: Pace) {
        crate::cmd::denoise::finish_downloads(self);
        self.denoise_refresh_active(false);
        // photos that changed (a replaced file has another key) must not keep the picture of what they were
        let stale = !self.media.denoise.is_empty()
            && self.denoise.reindexed.is_none_or(|(rev, at)| rev != self.catalog.revision && at.elapsed() > SETTINGS_TTL);
        if self.denoise.dirty || stale {
            self.denoise_reindex();
        }
        self.denoise_poll();
        if self.denoise.active.is_none() || self.denoise_products_dir().is_none() {
            return;
        }
        let (cap, auto) = (self.denoise.settings.threads, self.denoise.settings.auto());
        let wanted = self.denoise_wanted();
        for id in &wanted {
            self.denoise_check_photo(*id);
        }
        if auto {
            // the photo being edited goes first
            let (open, others): (Vec<PhotoId>, Vec<PhotoId>) = wanted.into_iter().partition(|id| self.selection.active == Some(*id));
            self.denoise_enqueue(&others, false, false);
            self.denoise_enqueue(&open, true, false);
        }
        if self.denoise.running.is_none() && pace != Pace::Pause {
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
            self.denoise_start_next(pace.parallel(cores, cap));
        }
    }

    /// The statistics `denoise.status` reports besides the queue (reads the cache folder).
    pub(crate) fn denoise_cache_usage(&self) -> (usize, u64) {
        match self.denoise_products_dir() {
            Some(dir) => {
                let files = products_in(&dir);
                (files.len(), files.iter().map(|f| f.1).sum())
            }
            None => (0, 0),
        }
    }

    /// Are pictures made without being asked for photos being looked at (Settings ▸ AI Denoise)?
    pub fn denoise_auto(&self) -> bool {
        self.denoise.settings.auto()
    }

    /// Is a picture being made, or waiting for its turn with a model to make it? (Headless runs wait for this.)
    pub fn denoise_busy(&self) -> bool {
        self.denoise.running.is_some() || (!self.denoise.queue.is_empty() && self.denoise.active.is_some())
    }

    /// Keep AI Denoise's row in the activity stack up to date (each `denoise.pump`): there while a photo is being made or
    /// waits its turn, with the running photo's tiles and its file name; gone when the queue is idle. It has no ✕: what is
    /// made follows the photos' Denoise amounts and Settings ▸ AI Denoise.
    pub(crate) fn denoise_track(&mut self) {
        if !self.denoise_busy() {
            self.denoise.task = None;
            return;
        }
        let task = self.denoise.task.get_or_insert_with(|| {
            let t = self.activity.start("denoise", "AI Denoise", crate::activity::Cancel::No);
            t.set_unit(crate::activity::Unit::Percent);
            t
        });
        match &self.denoise.running {
            Some(r) => {
                task.progress(r.progress.done.load(Ordering::Relaxed) as u64, r.progress.total.load(Ordering::Relaxed) as u64);
                task.detail(self.catalog.photo(r.photo).map_or("", |p| p.file_name.as_str()));
            }
            None => {
                task.progress(0, 0);
                task.detail("");
            }
        }
    }

    /// Seconds the running job has been going, and its progress.
    pub(crate) fn denoise_running_json(&self) -> Value {
        match &self.denoise.running {
            Some(r) => json!({
                "photo": r.photo.0,
                "done": r.progress.done.load(Ordering::Relaxed),
                "total": r.progress.total.load(Ordering::Relaxed),
                "seconds": r.started.elapsed().as_secs_f64(),
            }),
            None => Value::Null,
        }
    }

    pub(crate) fn denoise_made(&self) -> u64 {
        self.denoise.made
    }

    /// The running job's product path and key (tests stand in for the worker's save).
    #[cfg(test)]
    pub(crate) fn denoise_running_product(&self) -> Option<(PathBuf, String)> {
        self.denoise.running.as_ref().map(|r| (r.product.clone(), r.key.clone()))
    }

    /// What an export of photo `id` with `settings` needs for its Denoise amount: its denoised picture, made now when the
    /// queue has not got to it. `Ok(None)` when denoise does not apply (amount 0, not a Bayer raw), `Err` when it should
    /// have and cannot (no model chosen, nowhere to keep the picture, the model failing).
    pub(crate) fn denoise_for_export(&mut self, id: PhotoId, settings: &DevelopSettings) -> Result<Option<(DenoiseSpec, PairLoader)>, String> {
        if settings.denoise_amount() <= 0.0 {
            return Ok(None);
        }
        let Some(p) = self.catalog.photo(id).cloned() else { return Ok(None) };
        let Some(loader) = self.media.denoise.loader.clone() else { return Ok(None) };
        if !eligible(&p) {
            return Ok(None);
        }
        self.denoise_refresh_active(false);
        let name = &p.file_name;
        if self.denoise.active.is_none() {
            return Err(format!("{name}: Denoise is on for this photo but no denoise model is chosen; choose one in Settings, or set Denoise to 0"));
        }
        if self.denoise_products_dir().is_none() {
            return Err(format!("{name}: Denoise needs a library on disk to keep its pictures in"));
        }
        self.denoise_check_photo(id);
        let Some(job) = self.denoise_job_spec(&p, Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None)) else {
            return Ok(None);
        };
        let label = name.clone();
        let spec = job.clone();
        let make: MakeProduct = Arc::new(move || match make_product(&spec, None) {
            Ok(_) => Ok(true),
            Err(MakeError::Unsupported(_)) => Ok(false),
            Err(MakeError::Failed(why)) => Err(format!("{label}: could not denoise: {why}")),
            Err(MakeError::Cancelled) => Err(format!("{label}: denoise was cancelled")),
        });
        Ok(Some((DenoiseSpec { product: job.product, key: job.key, make: Some(make) }, loader)))
    }
}
