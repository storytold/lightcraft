//! Background rendering: engine [`RenderJob`]s run on a [`JobPool`]; results become textures.
//!
//! Each visible thing that needs pixels is a *slot* (a grid thumbnail, the loupe, the "before"
//! image…). Requests are deduplicated per slot (a newer request replaces a queued older one) and
//! prioritised (loupe first, then on-screen thumbnails, then prefetch); thumbnails scrolled far
//! out of view are dropped from the queue. Thumbnail jobs hit the engine's preview cache (memory +
//! disk) before rendering. On wasm the jobs run inline, one per frame, unless the host installs a
//! [`RenderOffload`] (the browser build's Web Workers): then queued jobs are handed to it as it has
//! room, and its results are collected every frame.
//!
//! Stand-ins ([`QuickJob`]s, once per photo and settings): [`Slot::Preview`] holds what the loupe
//! shows until [`Slot::Main`] has the photo's render (cached view render, embedded camera JPEG or
//! a thumbnail render); [`Slot::ThumbQuick`] holds a raw's embedded preview in the grid until its
//! rendered thumbnail arrives (a cached thumbnail found by the quick job becomes the
//! [`Slot::Thumb`] texture directly).

use std::collections::HashMap;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::Session;
use lightcraft_engine::media::{QuickJob, QuickSource, RenderJob, RenderResult};
use lightcraft_engine::pipeline::StageCache;
use lightcraft_preview::JobPool;
use lightcraft_raster::Histogram;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Thumb(PhotoId),
    /// A thumbnail's stand-in (embedded preview of an unedited raw).
    ThumbQuick(PhotoId),
    Main,
    /// The loupe's stand-in until `Main` has the photo.
    Preview,
    Before,
    Compare(u8),
    /// Background preparation of a neighbouring photo (no texture: its decoded source and view
    /// render are cached by the engine). 0 = next, 1 = previous.
    Prefetch(u8),
    /// A variant thumbnail (profile / preset browsers), by its job key ([`Session::variant_job`]).
    /// Kept in a bounded LRU ([`VARIANT_TEXTURES`]).
    Variant(u64),
    /// The loupe while hovering a preset or profile: the photo with the hovered look (nothing is
    /// committed).
    Hover,
    /// An import candidate's thumbnail in the import review dialog (by index).
    Import(u32),
    /// The second window's view of the active photo.
    Second,
}

pub struct Tex {
    pub key: u64,
    pub photo: PhotoId,
    pub tex: egui::TextureHandle,
    pub size: [usize; 2],
    pub histogram: Option<Histogram>,
    pub ms: f64,
    /// Set for stand-ins: where the image came from.
    pub quick: Option<QuickSource>,
    /// CPU copy of the pixels (only with [`Renderer::keep_pixels`]; for headless screenshots).
    pub pixels: Option<std::sync::Arc<egui::ColorImage>>,
}

/// Runs render jobs outside this thread's job pool (the browser build: Web Workers, each with its
/// own wasm instance). The [`Renderer`] keeps the queue (priorities, per-slot de-duplication) and
/// hands jobs over one at a time.
pub trait RenderOffload {
    /// Start `job` if an executor is free; otherwise give it back (`Some`: it stays queued).
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob>;
    /// Finished jobs since the last call: (slot, result, run time in ms).
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)>;
}

struct Queued {
    slot: Slot,
    priority: u32,
    seq: u64,
    job: RenderJob,
}

/// Variant thumbnails kept as textures (LRU).
pub const VARIANT_TEXTURES: usize = 96;
/// Priority of variant thumbnail jobs: below on-screen grid thumbnails and the loupe.
const VARIANT_PRIORITY: u32 = 6;

pub struct Renderer {
    #[cfg(test)]
    pub(crate) thumb_jobs_built: usize,
    request_ids: HashMap<Slot, u64>,
    /// Request id of the pixels each interactive view (loupe, before, hover) shows: a draft that
    /// finishes after a newer request was made still replaces older pixels, so a long slider drag
    /// keeps updating the loupe instead of waiting for the drag to pause.
    shown_ids: HashMap<Slot, u64>,
    /// Requests made before this id belong to a library or preview cache that is gone.
    epoch: u64,
    preview_generation: Option<(std::sync::Weak<lightcraft_preview::PreviewCache>, u64)>,
    /// Last thumbnail inputs; weak identities do not retain photo histories or force deep clones.
    /// (photo, size bucket, job key, whether the request went through an embedded stand-in)
    thumb_inputs: HashMap<PhotoId, (std::sync::Weak<lightcraft_catalog::Photo>, usize, u64, bool)>,
    pool: JobPool<Slot, RenderResult>,
    /// Jobs run elsewhere (see [`RenderOffload`]); `queue` holds the ones not started yet.
    offload: Option<Box<dyn RenderOffload>>,
    queue: Vec<Queued>,
    seq: u64,
    /// Slot → (key, priority) of the request in flight (queued or running).
    pending: HashMap<Slot, (u64, u32)>,
    pub textures: HashMap<Slot, Tex>,
    pub last_main_ms: f64,
    /// Jobs finished since start (for inspect/perf).
    pub completed: u64,
    /// Keep a CPU copy of every texture so the UI can be rasterized headlessly (set when the app
    /// is driven by the control channel).
    pub keep_pixels: bool,
    /// Per-view intermediate results (loupe and "before"), so slider drags redo only what changed.
    stages: HashMap<Slot, Arc<StageCache>>,
    /// Slot → key of the last quick job requested for it (each is tried once).
    quick_tried: HashMap<Slot, u64>,
    /// Jobs that failed (unreadable or missing file…), by slot and job key, with the error: not
    /// requested again until the key changes (an edit, another size) — the grid asks every frame.
    failed: HashMap<Slot, (u64, String, u64)>,
    /// The catalog revision seen at the last poll: a failure is retried once the catalog changes
    /// (a relinked or re-imported file).
    catalog_rev: u64,
    /// Prefetch slot → key of the last job submitted (each runs once).
    prefetched: HashMap<Slot, u64>,
    /// Variant key → frame it was last asked for (LRU of [`Slot::Variant`] textures).
    variant_used: HashMap<u64, u64>,
    /// Frames polled so far.
    frame: u64,
    /// Since when nothing has been pending, and whether the GPU pool was trimmed since.
    #[cfg(not(target_arch = "wasm32"))]
    idle: Option<(std::time::Instant, bool)>,
}

/// After this long without renders the GPU renderer's pool of recycled buffers is freed.
#[cfg(not(target_arch = "wasm32"))]
const IDLE_TRIM: std::time::Duration = std::time::Duration::from_secs(3);

impl Default for Renderer {
    fn default() -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { JobPool::<Slot, RenderResult>::default_threads().min(6) };
        Self::with_worker_threads(threads)
    }
}

impl Renderer {
    /// Explicit worker count for hosts and bounded validation runs (one to six on native).
    /// Browser hosts continue to use inline jobs or their installed worker offload.
    pub fn with_worker_threads(threads: usize) -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { threads.clamp(1, 6) };
        Renderer {
            #[cfg(test)]
            thumb_jobs_built: 0,
            request_ids: HashMap::new(),
            shown_ids: HashMap::new(),
            epoch: 0,
            preview_generation: None,
            pool: JobPool::new(threads),
            thumb_inputs: HashMap::new(),
            offload: None,
            queue: Vec::new(),
            seq: 0,
            pending: HashMap::new(),
            textures: HashMap::new(),
            last_main_ms: 0.0,
            completed: 0,
            keep_pixels: false,
            stages: HashMap::new(),
            quick_tried: HashMap::new(),
            failed: HashMap::new(),
            catalog_rev: 0,
            prefetched: HashMap::new(),
            variant_used: HashMap::new(),
            frame: 0,
            #[cfg(not(target_arch = "wasm32"))]
            idle: None,
        }
    }
}

impl Renderer {
    /// Would asking for this thumbnail again (at `priority`) change nothing?
    pub fn thumb_current(&self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some(&(ref old, size, key, quick)) = self.thumb_inputs.get(&photo.id) else { return false };
        if old.as_ptr() != Arc::as_ptr(photo) || size != bucket {
            return false;
        }
        let slot = Slot::Thumb(photo.id);
        if quick && !self.textures.contains_key(&slot) {
            // Stand-in path (`grid::request_thumb`): the embedded preview is still on its way, or
            // the real thumbnail is requested at the background priority whatever the caller's.
            return self.pending.get(&Slot::ThumbQuick(photo.id)).is_some_and(|p| p.0 == key)
                || !self.request_needed(slot, key, crate::panels::grid::BACKGROUND_THUMB_PRIORITY);
        }
        !self.request_needed(slot, key, priority)
    }

    /// Remember the inputs of the thumbnail just requested (`quick`: through a stand-in).
    pub fn remember_thumb(&mut self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, key: u64, quick: bool) {
        if self.thumb_inputs.len() >= 1024 && !self.thumb_inputs.contains_key(&photo.id) {
            self.thumb_inputs.retain(|_, (p, _, _, _)| p.strong_count() > 0);
            if self.thumb_inputs.len() >= 1024
                && let Some(id) = self.thumb_inputs.keys().next().copied()
            {
                self.thumb_inputs.remove(&id);
            }
        }
        self.thumb_inputs.insert(photo.id, (Arc::downgrade(photo), bucket, key, quick));
    }

    fn request_needed(&self, slot: Slot, key: u64, priority: u32) -> bool {
        // A matching old texture must not mask a different newer request (e.g. undo).
        if let Some(&(k, p)) = self.pending.get(&slot) {
            return k != key || (p != priority && self.is_queued(slot));
        }
        if self.textures.get(&slot).is_some_and(|t| t.key == key) || self.failed.get(&slot).is_some_and(|f| f.0 == key && f.2 == self.catalog_rev) {
            return false;
        }
        true
    }
    /// Run jobs through `offload` from now on (instead of the job pool / inline).
    pub fn set_offload(&mut self, offload: Box<dyn RenderOffload>) {
        self.offload = Some(offload);
    }

    fn is_queued(&self, slot: Slot) -> bool {
        self.queue.iter().any(|q| q.slot == slot) || self.pool.is_queued(slot)
    }

    /// Is the slot's current texture (or pending request) already for `key`?
    /// Key of what `slot` shows or is rendering next (the pending request wins).
    pub fn wanted(&self, slot: Slot) -> Option<u64> {
        self.pending.get(&slot).map(|p| p.0).or_else(|| self.textures.get(&slot).map(|t| t.key))
    }

    pub fn is_current(&self, slot: Slot, key: u64) -> bool {
        self.wanted(slot) == Some(key)
    }

    /// Request a render for `slot` (no-op if already current or pending at the same priority).
    pub fn request(&mut self, slot: Slot, mut job: RenderJob, priority: u32) {
        if !self.request_needed(slot, job.key, priority) {
            return;
        }
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        self.request_ids.insert(slot, job.request_id);
        // (an offload keeps its own per-view stage caches; the flag tells it to)
        let job =
            if matches!(slot, Slot::Main | Slot::Before | Slot::Hover) { job.with_stages(self.stages.entry(slot).or_default().clone()) } else { job };
        if self.offload.is_some() {
            self.seq += 1;
            self.queue.retain(|q| q.slot != slot);
            self.queue.push(Queued { slot, priority, seq: self.seq, job });
            self.dispatch(); // an idle worker starts now rather than next frame
            return;
        }
        let key = job.key;
        let background = matches!(slot, Slot::Thumb(_) | Slot::ThumbQuick(_) | Slot::Prefetch(_));
        self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || if background { lightcraft_engine::memory::in_background(|| job.run()) } else { job.run() }),
        );
    }

    /// Request a stand-in for `slot` (once per job key).
    pub fn request_quick(&mut self, slot: Slot, mut job: QuickJob, priority: u32) {
        if self.quick_tried.get(&slot) == Some(&job.key) {
            return;
        }
        self.quick_tried.insert(slot, job.key);
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        self.request_ids.insert(slot, job.request_id);
        let key = job.key;
        self.pool.submit(slot, key, priority, Box::new(move || job.run()));
    }

    /// Prepare a photo in the background (a [`Slot::Prefetch`] job runs once per key; a newer one
    /// replaces it while queued). Memory stays bounded by the engine's source caches.
    pub fn prefetch(&mut self, slot: Slot, mut job: RenderJob, priority: u32) {
        if self.prefetched.get(&slot) == Some(&job.key) {
            return;
        }
        self.prefetched.insert(slot, job.key);
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        self.request_ids.insert(slot, job.request_id);
        let key = job.key;
        self.pool.submit(slot, key, priority, Box::new(move || lightcraft_engine::memory::in_background(|| job.run())));
    }

    /// Is a request for `slot` queued or running?
    /// Drop every texture and cached stage (another library was opened: photo ids changed meaning).
    pub fn forget_all(&mut self) {
        self.request_ids.clear();
        self.shown_ids.clear();
        self.epoch = lightcraft_preview::next_tick();
        self.preview_generation = None;
        self.pool.reprioritize(|_, _| None);
        self.thumb_inputs.clear();
        self.textures.clear();
        self.stages.clear();
        self.quick_tried.clear();
        self.failed.clear();
        self.prefetched.clear();
        self.pending.clear();
        self.queue.clear();
    }

    pub fn is_pending(&self, slot: Slot) -> bool {
        self.pending.contains_key(&slot)
    }

    /// The texture to show for a grid/filmstrip thumbnail: the rendered one, else its stand-in.
    pub fn thumb(&self, id: PhotoId) -> Option<&Tex> {
        self.textures.get(&Slot::Thumb(id)).or_else(|| self.textures.get(&Slot::ThumbQuick(id)))
    }

    pub fn queued(&self) -> usize {
        self.pool.queued() + self.queue.len()
    }

    /// Hand queued jobs to the offload (best first) while it has room.
    fn dispatch(&mut self) {
        let Some(off) = self.offload.as_mut() else { return };
        while let Some(i) = self.queue.iter().enumerate().max_by_key(|(_, q)| (q.priority, q.seq)).map(|(i, _)| i) {
            let q = self.queue.swap_remove(i);
            if let Some(job) = off.try_start(q.slot, q.job) {
                self.queue.push(Queued { job, ..q });
                break;
            }
        }
    }

    /// Requests queued or running.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Why the last render for `slot` failed (until a render with another key succeeds).
    pub fn failure(&self, slot: Slot) -> Option<&str> {
        self.failed.get(&slot).map(|f| f.1.as_str())
    }

    /// The slots with a render pending (diagnostics: `ui.inspect`).
    pub fn pending_slots(&self) -> Vec<String> {
        self.pending.keys().map(|s| format!("{s:?}")).collect()
    }

    /// Thumbnail textures currently loaded.
    /// A variant thumbnail ([`Slot::Variant`]): its texture once rendered; until then the job is
    /// queued at low priority (below on-screen grid thumbnails). Call it every frame the variant
    /// is visible: variants not asked for during the last frames leave the queue, and textures
    /// beyond [`VARIANT_TEXTURES`] are evicted least recently used first.
    pub fn variant(&mut self, job: RenderJob) -> Option<&Tex> {
        let key = job.key;
        let slot = Slot::Variant(key);
        self.variant_used.insert(key, self.frame);
        if !self.textures.contains_key(&slot) {
            self.request(slot, job, VARIANT_PRIORITY);
        }
        self.textures.get(&slot)
    }

    /// Variant thumbnail textures currently loaded.
    pub fn variant_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Variant(_))).count()
    }

    /// Drop variant jobs nobody asked for in the last frames, and the least recently used
    /// variant textures beyond the budget.
    fn evict_variants(&mut self) {
        let now = self.frame;
        let stale = |used: Option<&u64>| used.is_none_or(|f| now.saturating_sub(*f) > 2);
        let used = &self.variant_used;
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Variant(k) if stale(used.get(k)) => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.request_ids.remove(&s);
        }
        let request_ids = &mut self.request_ids;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Variant(k) if stale(used.get(&k)) => {
                pending.remove(&q.slot);
                request_ids.remove(&q.slot);
                false
            }
            _ => true,
        });
        let n = self.variant_textures();
        if n > VARIANT_TEXTURES {
            let mut have: Vec<(u64, u64)> = self
                .textures
                .keys()
                .filter_map(|s| if let Slot::Variant(k) = s { Some((self.variant_used.get(k).copied().unwrap_or(0), *k)) } else { None })
                .collect();
            have.sort_unstable();
            for (_, k) in have.into_iter().take(n - VARIANT_TEXTURES) {
                self.textures.remove(&Slot::Variant(k));
                self.variant_used.remove(&k);
            }
        }
        if self.variant_used.len() > 4 * VARIANT_TEXTURES {
            let textures = &self.textures;
            let pending = &self.pending;
            self.variant_used.retain(|k, _| textures.contains_key(&Slot::Variant(*k)) || pending.contains_key(&Slot::Variant(*k)));
        }
    }

    pub fn thumb_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Thumb(_) | Slot::ThumbQuick(_))).count()
    }

    /// Collect finished jobs into textures. Returns true if anything changed.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut Session) -> bool {
        let cache = &session.media.rendered;
        let generation = cache.generation();
        if self.preview_generation.as_ref().is_some_and(|(old, g)| old.as_ptr() != Arc::as_ptr(cache) || *g != generation) {
            self.forget_all();
        }
        self.preview_generation = Some((Arc::downgrade(cache), generation));
        self.catalog_rev = session.catalog.revision;
        self.frame += 1;
        // wasm: run one job per frame on this thread, timed with the host clock
        #[cfg(target_arch = "wasm32")]
        let inline_ms = {
            let t0 = crate::now_ms();
            self.pool.run_inline(1);
            crate::now_ms() - t0
        };
        #[cfg(not(target_arch = "wasm32"))]
        let inline_ms = 0.0;
        let mut finished = Vec::new();
        while let Some(done) = self.pool.try_recv() {
            finished.push((done.slot, done.result, if done.ms > 0.0 { done.ms } else { inline_ms }));
        }
        if let Some(off) = self.offload.as_mut() {
            finished.extend(off.finished());
        }
        self.dispatch();
        let mut changed = false;
        for (mut slot, r, ms) in finished {
            self.completed += 1;
            // Ignore obsolete pixels, failures and decoded sources, even for the same render key;
            // an interactive view still takes a superseded draft that is newer than what it shows.
            if self.request_ids.get(&slot) != Some(&r.request_id) {
                let newer_draft = matches!(slot, Slot::Main | Slot::Before | Slot::Hover)
                    && self.request_ids.contains_key(&slot)
                    && r.request_id >= self.epoch
                    && self.shown_ids.get(&slot).is_none_or(|shown| r.request_id > *shown)
                    && r.rendered.is_ok();
                if !newer_draft {
                    continue;
                }
            } else {
                self.request_ids.remove(&slot);
                session.accept(&r);
            }
            if matches!(slot, Slot::Main | Slot::Before | Slot::Hover) {
                self.shown_ids.insert(slot, r.request_id);
            }
            if self.pending.get(&slot).is_some_and(|p| p.0 == r.key) {
                self.pending.remove(&slot);
            }
            let rendered = match r.rendered {
                Ok(x) => {
                    self.failed.remove(&slot);
                    x
                }
                Err(e) => {
                    if !matches!(slot, Slot::Prefetch(_)) {
                        log::warn!("render {slot:?}: {e}");
                        self.failed.insert(slot, (r.key, e, self.catalog_rev));
                    }
                    continue;
                }
            };
            if matches!(slot, Slot::Prefetch(_)) {
                continue;
            }
            if let Slot::ThumbQuick(id) = slot {
                if self.textures.contains_key(&Slot::Thumb(id)) {
                    continue; // the real thumbnail won the race
                }
                if r.quick == Some(QuickSource::Cached) {
                    // the photo's own cached thumbnail: final
                    slot = Slot::Thumb(id);
                }
            }
            if let Slot::Thumb(id) = slot {
                self.textures.remove(&Slot::ThumbQuick(id));
            }
            let img = &rendered.image;
            let color = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
            let pixels = self.keep_pixels.then(|| color.clone());
            let name = format!("{slot:?}");
            match self.textures.get_mut(&slot) {
                Some(t) => {
                    t.tex.set(color, egui::TextureOptions::LINEAR);
                    t.pixels = pixels;
                    t.key = r.key;
                    t.photo = r.photo;
                    t.size = [img.width, img.height];
                    t.histogram = Some(rendered.histogram);
                    t.ms = ms;
                    t.quick = r.quick;
                }
                None => {
                    let tex = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
                    self.textures.insert(
                        slot,
                        Tex {
                            key: r.key,
                            photo: r.photo,
                            tex,
                            size: [img.width, img.height],
                            histogram: Some(rendered.histogram),
                            ms,
                            quick: r.quick,
                            pixels,
                        },
                    );
                }
            }
            if slot == Slot::Main {
                self.last_main_ms = ms;
            }
            changed = true;
        }
        self.evict_variants();
        #[cfg(not(target_arch = "wasm32"))]
        self.trim_when_idle(ctx);
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        changed
    }

    /// Free the GPU renderer's recycled buffers once nothing has rendered for [`IDLE_TRIM`] (they
    /// are reallocated by the next render; while working, renders reuse them).
    #[cfg(not(target_arch = "wasm32"))]
    fn trim_when_idle(&mut self, ctx: &egui::Context) {
        if !self.pending.is_empty() {
            self.idle = None;
            return;
        }
        let now = std::time::Instant::now();
        let (since, trimmed) = *self.idle.get_or_insert((now, false));
        if trimmed {
            return;
        }
        let waited = now.duration_since(since);
        if waited >= IDLE_TRIM {
            lightcraft_engine::gpu::trim_pool(0);
            lightcraft_engine::memory::release();
            self.idle = Some((since, true));
        } else {
            ctx.request_repaint_after(IDLE_TRIM - waited);
        }
    }

    /// What the renderer holds: per-view stage caches (CPU images and GPU buffers) and textures.
    pub fn memory(&self) -> serde_json::Value {
        let cpu: usize = self.stages.values().map(|s| s.bytes()).sum();
        let gpu: usize = self.stages.values().map(|s| lightcraft_engine::gpu::stage_bytes(s)).sum();
        let tex: usize = self.textures.values().map(|t| t.size[0] * t.size[1] * 4).sum();
        let copies: usize = self.textures.values().filter_map(|t| t.pixels.as_ref()).map(|p| p.pixels.len() * 4).sum();
        serde_json::json!({
            "stageCaches": {"count": self.stages.len(), "cpuBytes": cpu, "gpuBytes": gpu},
            "textures": {"count": self.textures.len(), "bytes": tex, "cpuCopyBytes": copies},
        })
    }

    /// CPU copies of the current textures by id (see [`Self::keep_pixels`]).
    pub fn cpu_textures(&self) -> HashMap<egui::TextureId, crate::softpaint::CpuTexture> {
        self.textures.values().filter_map(|t| Some((t.tex.id(), crate::softpaint::CpuTexture::linear(t.pixels.clone()?)))).collect()
    }

    /// Drop the import review thumbnails (a new review, or the dialog closed).
    pub fn forget_imports(&mut self) {
        self.pool.reprioritize(|s, p| if matches!(s, Slot::Import(_)) { None } else { Some(p) });
        self.queue.retain(|q| !matches!(q.slot, Slot::Import(_)));
        self.pending.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.request_ids.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.textures.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::Import(_)));
    }

    /// Drop thumbnails that aren't in `keep` (bounded memory for huge libraries), and queued
    /// thumbnail jobs for photos that scrolled out of `keep`.
    pub fn evict_thumbs(&mut self, keep: &std::collections::HashSet<PhotoId>, max: usize) {
        self.thumb_inputs.retain(|id, _| keep.contains(id));
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id) && p <= 11 => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.request_ids.remove(&s);
            self.quick_tried.remove(&s);
        }
        let request_ids = &mut self.request_ids;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Thumb(id) if !keep.contains(&id) && q.priority <= 10 => {
                pending.remove(&q.slot);
                request_ids.remove(&q.slot);
                false
            }
            _ => true,
        });
        let thumbs = self.thumb_textures();
        if thumbs <= max {
            return;
        }
        self.textures.retain(|s, _| !matches!(s, Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::ThumbQuick(id) if !keep.contains(id)));
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;
    use lightcraft_catalog::{Catalog, Op, Photo, Source};

    struct Blocked;
    impl RenderOffload for Blocked {
        fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
            Some(job)
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            vec![]
        }
    }
    fn photo() -> Arc<Photo> {
        Arc::new(Photo::new(PhotoId(1), Source::File { path: "audit.jpg".into() }, "audit.jpg", "JPEG", 6000, 4000, "2026-01-01"))
    }
    #[test]
    fn snapshot_tracks_cow_edits_undo_reload_and_replacement() {
        let mut catalog = Catalog::default();
        catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let old = catalog.photo(PhotoId(1)).unwrap().clone();
        let mut r = Renderer::default();
        r.remember_thumb(&old, 384, 77, false);
        r.pending.insert(Slot::Thumb(PhotoId(1)), (77, 10));
        assert!(r.thumb_current(&old, 384, 10));
        assert!(!r.thumb_current(&old, 512, 10));
        let mut settings = lightcraft_develop::DevelopSettings::default();
        settings.light.exposure = 1.0;
        let undo = catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings), label: "edit".into(), edited: None }).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog.apply(undo).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog
            .apply(Op::SetContent {
                id: PhotoId(1),
                width: 6000,
                height: 4000,
                file_size: 99,
                content_hash: Some("changed".into()),
                preview_only: None,
            })
            .unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        assert!(!r.thumb_current(&photo(), 384, 10));
        r.forget_all();
        assert!(r.thumb_inputs.is_empty());
    }
    #[test]
    fn queue_promotions_running_work_and_cancellation() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut session = Session::new();
        session.catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
        let job = session.thumb_job(p.id, 384).unwrap();
        let key = job.key;
        let mut r = Renderer::default();
        r.set_offload(Box::new(Blocked));
        r.remember_thumb(&p, 384, key, false);
        r.request(slot, job, 5);
        assert!(r.thumb_current(&p, 384, 5));
        assert!(!r.thumb_current(&p, 384, 10));
        r.request(slot, session.thumb_job(p.id, 384).unwrap(), 10);
        assert_eq!(r.pending.get(&slot), Some(&(key, 10)));
        r.queue.clear(); // running work: cannot reprioritize, just as Renderer::request
        assert!(r.thumb_current(&p, 384, 5));
        r.pending.clear();
        assert!(!r.thumb_current(&p, 384, 10));
        r.evict_thumbs(&Default::default(), 600);
        assert!(r.thumb_inputs.is_empty());
    }
    #[test]
    fn failures_retry_after_revision_and_quick_only_is_not_final() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut r = Renderer::default();
        r.remember_thumb(&p, 384, 77, false);
        r.pending.insert(Slot::ThumbQuick(p.id), (77, 11));
        assert!(!r.thumb_current(&p, 384, 10));
        r.failed.insert(slot, (77, "no decoder".into(), 0));
        assert!(r.thumb_current(&p, 384, 10));
        r.catalog_rev = 1;
        assert!(!r.thumb_current(&p, 384, 10));
    }

    #[derive(Default)]
    struct Work {
        jobs: Vec<(Slot, RenderJob)>,
        finished: Vec<(Slot, RenderResult, f64)>,
    }
    struct Manual(std::rc::Rc<std::cell::RefCell<Work>>);
    impl RenderOffload for Manual {
        fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
            self.0.borrow_mut().jobs.push((slot, job));
            None
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }
    fn fixture() -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        fixture_with(Session::new(), (*photo()).clone())
    }
    fn fixture_with(mut s: Session, photo: Photo) -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        s.media.file_loader = Some(Arc::new(|_, _| {
            let mut image = lightcraft_raster::Rgb32f::new(8, 8);
            image.data.fill([0.3, 0.2, 0.1]);
            Ok((image, Default::default()))
        }));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let mut app = crate::LightcraftApp::new(s, crate::Services::default());
        app.renderer.keep_pixels = true;
        let work = std::rc::Rc::new(std::cell::RefCell::new(Work::default()));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        let ctx = egui::Context::default();
        app.renderer.poll(&ctx, &mut app.session);
        (app, work, ctx)
    }
    fn take_job(work: &std::rc::Rc<std::cell::RefCell<Work>>) -> (Slot, RenderJob) {
        work.borrow_mut().jobs.pop().unwrap()
    }
    fn finish(app: &mut crate::LightcraftApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, ctx: &egui::Context, slot: Slot, result: RenderResult) {
        work.borrow_mut().finished.push((slot, result, 0.0));
        app.renderer.poll(ctx, &mut app.session);
    }
    fn request(app: &mut crate::LightcraftApp) {
        crate::panels::grid::request_thumb(app, PhotoId(1), 256, 10);
    }

    #[test]
    fn unchanged_ready_and_running_frames_build_no_jobs() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
    }

    #[test]
    fn unchanged_queued_frames_build_no_jobs() {
        let (mut app, _, _) = fixture();
        app.renderer.set_offload(Box::new(Blocked));
        request(&mut app);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert_eq!(app.renderer.queue.len(), 1);
    }

    #[test]
    fn resizing_the_disk_cache_keeps_textures_and_skips_rebuilds() {
        let lib = std::env::temp_dir().join(format!("lc-ui-thumb-resize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let (mut app, work, ctx) = fixture_with(s, (*photo()).clone());
        request(&mut app);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        let cache = app.session.media.rendered.clone();
        app.session.execute("library.preferences", &serde_json::json!({"cacheMb": 300})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        // thumbnails are keyed by content: a new budget leaves them (and the cache) valid
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        assert!(Arc::ptr_eq(&cache, &app.session.media.rendered));
        for _ in 0..30 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
        // an explicit clear still drops them
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn raw_quick_path_frames_build_no_jobs() {
        // an unedited raw: the grid shows its embedded preview first (held here until released),
        // then queues the real thumbnail behind everything on screen
        let gate = Arc::new(std::sync::Mutex::new(()));
        let held = gate.lock().unwrap();
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let mut raw = (*photo()).clone();
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, _, ctx) = fixture_with(s, raw);
        app.renderer.set_offload(Box::new(Blocked));
        let quick = Slot::ThumbQuick(PhotoId(1));
        // the stand-in is running
        for _ in 0..180 {
            request(&mut app);
        }
        assert!(app.renderer.is_pending(quick));
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        drop(held);
        let t0 = std::time::Instant::now();
        while !app.renderer.textures.contains_key(&quick) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the embedded preview never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.renderer.poll(&ctx, &mut app.session);
        }
        // the stand-in is shown: one more job queues the real thumbnail at background priority,
        // and the grid asking at its own (higher) priority every frame does not rebuild it
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 2);
        assert_eq!(app.renderer.queue.len(), 1);
        assert_eq!(app.renderer.pending.get(&Slot::Thumb(PhotoId(1))).map(|p| p.1), Some(crate::panels::grid::BACKGROUND_THUMB_PRIORITY));
    }

    #[test]
    fn closing_import_review_cancels_queued_and_running_identities() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        app.renderer.request(Slot::Import(0), job.clone(), 10);
        let (slot, running) = take_job(&work);
        app.renderer.set_offload(Box::new(Blocked));
        app.renderer.request(Slot::Import(1), job, 10);
        app.renderer.forget_imports();
        assert!(!app.renderer.is_pending(Slot::Import(0)));
        assert!(!app.renderer.is_pending(Slot::Import(1)));
        assert!(app.renderer.queue.is_empty());
        work.borrow_mut().finished.push((slot, running.run(), 0.0));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        app.renderer.poll(&ctx, &mut app.session);
        assert!(!app.renderer.textures.contains_key(&slot));
        assert_eq!(app.session.media.source_usage().0, 0);
    }

    #[test]
    fn loupe_shows_superseded_drafts_newer_than_its_pixels_during_a_drag() {
        let (mut app, work, ctx) = fixture();
        let mut jobs = Vec::new();
        for exposure in [0.0, 1.0, 2.0] {
            let mut edit = lightcraft_develop::DevelopSettings::default();
            edit.light.exposure = exposure;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
            let job = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().draft();
            app.renderer.request(Slot::Main, job, 100);
            jobs.push(take_job(&work).1);
        }
        let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
        let mut jobs = jobs.into_iter();
        let (first, second, last) = (jobs.next().unwrap(), jobs.next().unwrap(), jobs.next().unwrap());
        // the first draft lands while the newest is still rendering: the loupe moves on
        finish(&mut app, &work, &ctx, Slot::Main, first.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[0]));
        assert!(app.renderer.is_pending(Slot::Main));
        finish(&mut app, &work, &ctx, Slot::Main, last.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
        // a draft older than the pixels shown never replaces them
        finish(&mut app, &work, &ctx, Slot::Main, second.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
        assert!(!app.renderer.is_pending(Slot::Main));
    }

    #[test]
    fn library_reset_rejects_old_job_even_when_photo_id_and_key_are_reused() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, old) = take_job(&work);
        let old_result = old.run();
        app.renderer.forget_all();
        let photo = app.session.catalog.photo(PhotoId(1)).unwrap().as_ref().clone();
        app.session = lightcraft_engine::Session::new();
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        app.session.media.file_loader = Some(Arc::new(|_, _| {
            let mut image = lightcraft_raster::Rgb32f::new(8, 8);
            image.data.fill([0.1, 0.2, 0.9]);
            Ok((image, Default::default()))
        }));
        app.renderer.poll(&ctx, &mut app.session);
        request(&mut app);
        let (_, new) = take_job(&work);
        assert_eq!(old_result.key, new.key);
        assert_ne!(old_result.request_id, new.request_id);
        let expected = new.clone().run().rendered.unwrap().image.as_bytes();
        finish(&mut app, &work, &ctx, slot, new.run());
        finish(&mut app, &work, &ctx, slot, old_result);
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn late_results_and_errors_cannot_replace_newest_pixels_or_sources() {
        for order in [0, 1, 2] {
            let (mut app, work, ctx) = fixture();
            request(&mut app);
            let (slot, old) = take_job(&work);
            let mut edit = lightcraft_develop::DevelopSettings::default();
            edit.light.exposure = 1.0;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
            request(&mut app);
            let (_, new) = take_job(&work);
            let expected = new.clone().run().rendered.unwrap().image.as_bytes();
            if order != 0 {
                finish(&mut app, &work, &ctx, slot, new.run());
                // An obsolete source and error must not poison the current successful request.
                let mut stale = old.run();
                if order == 2 {
                    stale.rendered = Err("old failure".into());
                }
                finish(&mut app, &work, &ctx, slot, stale);
            } else {
                finish(&mut app, &work, &ctx, slot, old.run());
                assert!(app.renderer.thumb(PhotoId(1)).is_none());
                assert_eq!(app.session.media.source_usage().0, 0);
                finish(&mut app, &work, &ctx, slot, new.run());
            }
            assert_eq!(
                app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
                expected
            );
            assert!(app.renderer.failure(slot).is_none());
        }
    }

    #[test]
    fn undo_to_existing_texture_supersedes_pending_edit() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, first) = take_job(&work);
        let key = first.key;
        finish(&mut app, &work, &ctx, slot, first.run());
        let original = app.renderer.thumb(PhotoId(1)).unwrap().pixels.clone().unwrap();
        let mut edit = lightcraft_develop::DevelopSettings::default();
        edit.light.exposure = 1.0;
        let undo =
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        request(&mut app);
        let (_, edited) = take_job(&work);
        app.session.catalog.apply(undo).unwrap();
        request(&mut app);
        let (_, restored) = take_job(&work);
        assert_eq!(restored.key, key);
        finish(&mut app, &work, &ctx, slot, restored.run());
        finish(&mut app, &work, &ctx, slot, edited.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels, original.pixels);
    }

    #[test]
    fn explicit_clear_repairs_same_key_bad_pixels_and_rejects_old_same_key_job() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        let expected = job.clone().run().rendered.unwrap().image;
        let (cache, key) = job.cache.clone().unwrap();
        let mut bad = expected.clone();
        bad.data.fill([255, 0, 255, 255]);
        cache.put(key, Arc::new(bad.clone()));
        request(&mut app);
        let (slot, corrupt) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, corrupt.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels[0].to_array(), [255, 0, 255, 255]);
        // An old job can have the SAME render key and stale decoded pixels.
        let mut old = job.clone();
        old.source = lightcraft_engine::media::SourceRef::Loaded(Box::new(lightcraft_engine::media::DecodedSource::new(
            Arc::new(lightcraft_raster::Rgb32f::new(8, 8)),
            None,
        )));
        app.renderer.textures.clear();
        app.renderer.request(slot, old, 10);
        let (_, old) = take_job(&work);
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        request(&mut app);
        let (_, repaired) = take_job(&work);
        assert_eq!(repaired.key, job.key);
        finish(&mut app, &work, &ctx, slot, repaired.run());
        finish(&mut app, &work, &ctx, slot, old.run());
        assert_eq!(cache.get(key).unwrap().as_bytes(), expected.as_bytes());
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected.as_bytes()
        );
    }
}
