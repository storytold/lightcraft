//! Source proxies and render jobs.
//!
//! Sources are decoded (or generated) once per level and cached in memory LRUs: `Thumb` (≤ 512 px
//! long edge) for the grid and filmstrip, `Preview` (≤ 2560 px) for the loupe, `Full` (native
//! resolution, the last one only) for exports larger than a preview. A [`RenderJob`] is
//! self-contained and `Send`: frontends run it on a worker thread; if the source wasn't cached yet
//! the job loads it and hands it back in the [`RenderResult`] so the cache can keep it.
//!
//! Rendered thumbnails are cached too ([`dac_preview::PreviewCache`]: memory LRU, plus a
//! disk cache in the library's `thumbs/` folder), keyed by the photo's content hash, the develop
//! settings hash, the size and [`RENDER_CACHE_VERSION`] — so reopening a library shows its grid
//! without decoding a single original, and an edit simply produces a new key.
//!
//! Opening a photo shows something at once ([`QuickJob`]): the loupe's last render for the current
//! settings (kept in the same cache under a size-independent "view" key), else the camera's
//! embedded JPEG for an unedited raw, else a thumbnail-level render — while the real render is
//! prepared. Grid thumbnails of unedited raws likewise start from the embedded preview.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};

use dac_catalog::{MediaKind, Photo, PhotoId, Source};
use dac_develop::DevelopSettings;
use dac_pipeline::{Quality, RenderRequest, Rendered, SourceInfo, StageCache};
use dac_preview::{Hash128, Hasher128, Lru, PreviewCache};
use dac_raster::{Histogram, Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};

const SETTINGS_HASH_ENTRIES: usize = 1024;

#[derive(Default)]
struct SettingsHashes {
    entries: HashMap<PhotoId, (Weak<DevelopSettings>, u64)>,
    order: VecDeque<PhotoId>,
    #[cfg(test)]
    computations: usize,
}

impl SettingsHashes {
    fn get(&mut self, id: PhotoId, settings: &Arc<DevelopSettings>) -> u64 {
        if let Some((old, hash)) = self.entries.get(&id)
            && old.as_ptr() == Arc::as_ptr(settings)
        {
            return *hash;
        }
        let hash = settings.hash64();
        #[cfg(test)]
        {
            self.computations += 1;
        }
        if !self.entries.contains_key(&id) {
            if self.entries.len() >= SETTINGS_HASH_ENTRIES
                && let Some(oldest) = self.order.pop_front()
            {
                self.entries.remove(&oldest);
            }
            self.order.push_back(id);
        }
        self.entries.insert(id, (Arc::downgrade(settings), hash));
        hash
    }
}

/// Bump when the pipeline's output changes, to invalidate cached thumbnails.
pub const RENDER_CACHE_VERSION: u64 = 22;

/// Thumbnails render at one of these long edges (so window/cell size changes reuse the cache).
pub const THUMB_SIZES: [usize; 4] = [128, 256, 384, 512];

pub fn thumb_bucket(long_edge: usize) -> usize {
    THUMB_SIZES.iter().copied().find(|s| *s >= long_edge).unwrap_or(512)
}

/// Memory budgets.
const THUMB_SOURCE_BYTES: usize = 384 << 20;
const RENDERED_MEM_BYTES: usize = 128 << 20;
/// Disk budget for the thumbnail cache.
pub const DISK_CACHE_BYTES: u64 = 2 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLevel {
    Thumb,
    Preview,
    /// The original at its native resolution (exports larger than a preview).
    Full,
}

impl SourceLevel {
    pub fn max_edge(self) -> usize {
        match self {
            SourceLevel::Thumb => 512,
            SourceLevel::Preview => 2560,
            SourceLevel::Full => usize::MAX,
        }
    }
    /// Smallest level that can serve an output of `long_edge` pixels.
    pub fn for_size(long_edge: usize) -> SourceLevel {
        match long_edge {
            0..=520 => SourceLevel::Thumb,
            521..=2560 => SourceLevel::Preview,
            _ => SourceLevel::Full,
        }
    }
}

/// Pixels along the source's long edge that an output `out_long` pixels long needs: a crop shows
/// only part of the photo, so its pixels come from a source that many times larger (the source
/// level is chosen from this, or a tight crop shown at its own pixels would be a stretched preview).
fn source_edge_needed(p: &Photo, settings: &DevelopSettings, apply_crop: bool, out_long: usize) -> usize {
    let (w, h) = (f64::from(p.width.max(1)), f64::from(p.height.max(1)));
    let r = settings.crop.geometry.rect;
    let shown = if apply_crop { (r.width() * w).max(r.height() * h) } else { w.max(h) };
    if !shown.is_finite() || shown < 1.0 {
        return out_long;
    }
    let needed = out_long as f64 * w.max(h) / shown;
    if needed.is_finite() { needed.min(usize::MAX as f64 / 2.0) as usize } else { out_long }
}

/// Decodes a file into a linear Rec.2020 image no larger than `max_edge` (set by the app).
pub type FileLoader = Arc<dyn Fn(&str, usize) -> Result<(Rgb32f, SourceInfo), String> + Send + Sync>;

/// A raw file's embedded (camera-rendered) preview as display sRGB, oriented, no larger than
/// `max_edge` (set by the app). `None`: no usable preview.
pub type PreviewLoader = Arc<dyn Fn(&str, usize) -> Option<Rgba8> + Send + Sync>;

/// Makes a photo's denoised picture when it is not cached yet (an export does not wait for the background queue):
/// `Ok(true)` once it is there, `Ok(false)` when denoise does not apply to this photo (render it plain), `Err` when it
/// should have worked and did not.
pub type MakeProduct = Arc<dyn Fn() -> Result<bool, String> + Send + Sync>;

/// A raw photo's cached AI-denoised picture (see [`crate::denoise`]): where the file is and the key it must have been
/// made with. A file that is missing, damaged or made for something else is a cache miss, never an error.
#[derive(Clone)]
pub struct DenoiseSpec {
    pub product: std::path::PathBuf,
    pub key: String,
    /// Run before the picture is read, by a job that must not render without it (an export).
    pub make: Option<MakeProduct>,
}

/// Decodes a raw file once and develops it twice: as usual, and from the denoised picture in `spec` when that can be
/// read (`None` when it cannot). Both are at most `max_edge` long and the same size.
pub type PairLoader = Arc<dyn Fn(&str, usize, &DenoiseSpec) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> + Send + Sync>;

/// A photo's denoised picture next to its plain one, and the last mix of the two that was asked for.
pub struct Twin {
    pub image: Arc<Rgb32f>,
    /// The Amount (in thousandths) and the picture mixed for it, so dragging other sliders keeps one source picture.
    mix: std::sync::Mutex<Option<(u32, Arc<Rgb32f>)>>,
}

impl Twin {
    pub fn new(image: Arc<Rgb32f>) -> Twin {
        Twin { image, mix: std::sync::Mutex::new(None) }
    }

    /// Bytes reserved for the twin and its lazy mix, before either cache cost can grow.
    fn bytes(&self) -> usize {
        self.image.data.len().saturating_mul(24)
    }
}

/// Pixels and the source interpretation learned while decoding; they must be cached together.
#[derive(Clone)]
pub struct DecodedSource {
    pub image: Arc<Rgb32f>,
    pub info: Option<SourceInfo>,
    /// A smart preview's stored camera tone curve: the one decoder fact its pixels need that the
    /// catalog's header facts lack (used when `info` is `None`).
    pub camera_tone: Option<dac_pipeline::tone::CameraTone>,
    /// The same picture developed from the AI-denoised mosaic, when the photo has one cached.
    pub denoised: Option<Arc<Twin>>,
}

impl DecodedSource {
    pub fn new(image: Arc<Rgb32f>, info: Option<SourceInfo>) -> Self {
        DecodedSource { image, info, camera_tone: None, denoised: None }
    }

    /// The picture to develop for a Denoise `amount` (0 to 1, see [`DevelopSettings::denoise_amount`]): the plain one,
    /// the denoised one, or a mix of the two. A mix is made once per amount and kept, so the stages after it are
    /// reused until the amount changes.
    pub fn image_for(&self, amount: f32) -> Arc<Rgb32f> {
        let Some(twin) = &self.denoised else { return self.image.clone() };
        if amount.is_nan() || amount <= 0.0 || twin.image.width != self.image.width || twin.image.height != self.image.height {
            return self.image.clone();
        }
        if amount >= 1.0 {
            return twin.image.clone();
        }
        let thousandths = (amount * 1000.0).round().clamp(1.0, 999.0) as u32;
        let mut memo = twin.mix.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((t, img)) = memo.as_ref()
            && *t == thousandths
        {
            return img.clone();
        }
        let mut mixed = (*self.image).clone();
        if !dac_denoise::product::blend(&mut mixed, &twin.image, thousandths as f32 / 1000.0) {
            return self.image.clone();
        }
        let mixed = Arc::new(mixed);
        *memo = Some((thousandths, mixed.clone()));
        mixed
    }

    /// Cache cost: the picture, its segmentation mattes, its denoised twin and room for the lazy mix.
    pub fn bytes(&self) -> usize {
        let mattes = self.info.as_ref().and_then(|i| i.mattes.as_ref()).map_or(0, |m| m.bytes());
        let mattes = mattes + self.info.as_ref().and_then(|i| i.local_tone.as_ref()).map_or(0, |t| t.map.gains.len() * 4);
        self.image.data.len() * 12 + 64 + std::mem::size_of::<SourceInfo>() + mattes + self.denoised.as_ref().map_or(0, |t| t.bytes())
    }

    /// What to render these pixels against: the decoder's facts, else `header` (the catalog's)
    /// with any stored camera tone curve.
    pub fn info_or(&self, header: SourceInfo) -> SourceInfo {
        self.info.clone().unwrap_or_else(|| SourceInfo { camera_tone: self.camera_tone.or(header.camera_tone), ..header })
    }
}

#[derive(Clone)]
pub enum SourceRef {
    Loaded(Box<DecodedSource>),
    Demo {
        scene: Box<dac_scenes::Scene>,
        max_edge: usize,
    },
    File {
        path: String,
        max_edge: usize,
        loader: Option<FileLoader>,
        /// The photo's smart preview, used when the original can't be read (offline drive). The
        /// UI thread never checks whether the original is there; the render worker finds out.
        fallback: Option<std::path::PathBuf>,
        /// The photo has a cached denoised picture: load it with the plain one (see [`PairLoader`]).
        denoise: Option<(DenoiseSpec, PairLoader)>,
    },
    /// A smart preview standing in for a missing original.
    Smart {
        path: std::path::PathBuf,
    },
}

impl SourceRef {
    pub fn load(&self) -> Result<Arc<Rgb32f>, String> {
        self.load_source().map(|s| s.image)
    }

    pub fn load_source(&self) -> Result<DecodedSource, String> {
        let image = match self {
            SourceRef::Loaded(a) => return Ok((**a).clone()),
            SourceRef::Demo { scene, max_edge } => crate::demo::scene_pixels(scene, *max_edge),
            SourceRef::File { path, max_edge, loader, fallback, denoise } => {
                let plain = || match loader {
                    Some(l) => l(path, *max_edge).map(|(image, info)| DecodedSource::new(Arc::new(image), Some(info))),
                    None => Err(format!("no decoder available for {path}")),
                };
                // the denoised picture comes with the plain one from a single decode; when it cannot be read the
                // pair loader gives the plain picture alone (the cached file is never a reason to fail a render)
                let r = match denoise {
                    Some((spec, pair)) => {
                        // a job that needs the denoised picture has it made first; if that fails the job fails (no smart
                        // preview stands in: an export must not quietly come out without the denoise it was asked for)
                        let made = match &spec.make {
                            Some(make) => make()?,
                            None => true,
                        };
                        if made {
                            pair(path, *max_edge, spec).and_then(|(image, twin, info)| {
                                if spec.make.is_some() && twin.is_none() {
                                    return Err("the cached AI Denoise picture could not be read; clear the denoise cache and retry".into());
                                }
                                Ok(DecodedSource {
                                    image: Arc::new(image),
                                    info: Some(info),
                                    camera_tone: None,
                                    denoised: twin.map(|t| Arc::new(Twin::new(Arc::new(t)))),
                                })
                            })
                        } else {
                            plain()
                        }
                    }
                    None => plain(),
                };
                // An offline original renders from its smart preview (no decoder facts: header ones).
                return match (r, fallback) {
                    #[cfg(not(target_arch = "wasm32"))]
                    (Err(e), Some(sp)) if denoise.as_ref().is_none_or(|(spec, _)| spec.make.is_none()) && crate::smart::is_valid(sp) => {
                        crate::smart::load(sp).map_err(|e2| format!("{e}; smart preview: {e2}"))
                    }
                    (r, _) => r,
                };
            }
            #[cfg(not(target_arch = "wasm32"))]
            SourceRef::Smart { path } => return crate::smart::load(path),
            #[cfg(target_arch = "wasm32")]
            SourceRef::Smart { .. } => return Err("smart previews are not available here".into()),
        };
        Ok(DecodedSource::new(image, None))
    }
}

pub struct MediaCache {
    settings_hashes: SettingsHashes,
    /// Decoded thumbnail-level sources (LRU by bytes).
    thumbs: Lru<PhotoId, DecodedSource>,
    /// Decoded preview-level sources with their last use ([`dac_preview::next_tick`]).
    previews: Vec<(PhotoId, DecodedSource, u64)>,
    /// The last full-resolution original (exports; one at a time: ~300 MB at 24 MP).
    full: Option<(PhotoId, DecodedSource, u64)>,
    /// How many previews to keep (LRU).
    pub preview_capacity: usize,
    /// Bytes all decoded sources and rendered previews in memory may take together (the cache
    /// share of [`crate::memory::budget`]); the least recently used entry of any of them goes first.
    budget: usize,
    pub file_loader: Option<FileLoader>,
    pub file_probe: Option<FileProbe>,
    pub preview_loader: Option<PreviewLoader>,
    /// Reads a photo file's bytes (Photo Merge); `None` = the local file system.
    pub file_bytes: Option<crate::merge::ByteReader>,
    /// The library's smart previews folder (originals offline: render from the proxy).
    pub smart_dir: Option<std::path::PathBuf>,
    /// Whether originals (and smart previews) are on disk: cached and checked off the UI thread
    /// in the app (see [`crate::availability`]).
    pub availability: crate::availability::Availability,
    scenes: Vec<dac_scenes::Scene>,
    /// Rendered thumbnails (memory, plus disk once a library is attached).
    pub rendered: Arc<PreviewCache>,
    /// Which photos have a cached denoised picture, and how to read it.
    pub denoise: DenoiseIndex,
}

/// The photos that have a cached denoised picture for the denoise model in use. Only memory: nothing here touches the
/// file system, so it can be asked on the UI thread.
#[derive(Default)]
pub struct DenoiseIndex {
    ready: std::collections::HashMap<PhotoId, DenoiseSpec>,
    /// Reads the pictures (`None`: this build cannot, and nothing is ever ready).
    pub loader: Option<PairLoader>,
}

impl DenoiseIndex {
    /// What to load for `id` besides its plain picture, and how.
    pub fn get(&self, id: PhotoId) -> Option<(&DenoiseSpec, &PairLoader)> {
        self.ready.get(&id).zip(self.loader.as_ref())
    }

    pub fn spec(&self, id: PhotoId) -> Option<&DenoiseSpec> {
        self.ready.get(&id)
    }

    pub fn set(&mut self, id: PhotoId, spec: DenoiseSpec) {
        self.ready.insert(id, spec);
    }

    pub fn remove(&mut self, id: PhotoId) -> bool {
        self.ready.remove(&id).is_some()
    }

    pub fn clear(&mut self) {
        self.ready.clear();
    }

    pub fn len(&self) -> usize {
        self.ready.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn ids(&self) -> Vec<PhotoId> {
        let mut v: Vec<PhotoId> = self.ready.keys().copied().collect();
        v.sort();
        v
    }
}

impl Default for MediaCache {
    fn default() -> Self {
        let budget = crate::memory::cache_share(crate::memory::budget());
        MediaCache {
            settings_hashes: SettingsHashes::default(),
            thumbs: Lru::new(THUMB_SOURCE_BYTES.min(budget)),
            previews: Vec::new(),
            full: None,
            preview_capacity: 0,
            budget,
            file_loader: None,
            file_probe: None,
            preview_loader: None,
            file_bytes: None,
            smart_dir: None,
            availability: Default::default(),
            scenes: Vec::new(),
            rendered: Arc::new(PreviewCache::memory(rendered_budget(budget))),
            denoise: DenoiseIndex::default(),
        }
    }
}

/// Memory for rendered previews out of the cache share.
fn rendered_budget(share: usize) -> usize {
    RENDERED_MEM_BYTES.min(share / 4)
}

impl MediaCache {
    /// Keep rendered thumbnails on disk in `dir` as well. The cache this replaces is retired
    /// (its files stay): render jobs still holding it can't write through it any more, even
    /// into a directory a later clear emptied (`dir` may be the same one).
    pub fn attach_disk_cache(&mut self, dir: &std::path::Path, disk_bytes: u64) {
        self.rendered.retire();
        self.rendered = Arc::new(PreviewCache::with_disk(rendered_budget(self.budget), dir, disk_bytes));
    }

    /// Change the disk budget of the cache in `dir`: in place when that cache is attached (its
    /// thumbnails are keyed by content, so they and the textures shown from them stay valid,
    /// and the same cache object keeps its generation), else attach it.
    pub fn set_disk_cache_bytes(&mut self, dir: &std::path::Path, disk_bytes: u64) {
        match self.rendered.disk().filter(|d| d.dir() == dir) {
            Some(d) => d.set_budget(disk_bytes),
            None => self.attach_disk_cache(dir, disk_bytes),
        }
    }

    /// Forget every decoded source (photo ids changed meaning, e.g. another library was opened).
    pub fn clear_sources(&mut self) {
        self.settings_hashes = SettingsHashes::default();
        self.thumbs = Lru::new(THUMB_SOURCE_BYTES.min(self.budget));
        self.previews.clear();
        self.full = None;
    }

    /// Bytes the caches may hold together.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Change the bytes the caches may hold together, evicting at once if needed.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.thumbs.set_budget(THUMB_SOURCE_BYTES.min(bytes));
        self.rendered.set_mem_budget(rendered_budget(bytes));
        self.enforce_budget();
    }

    /// Bytes held by decoded sources and rendered previews in memory.
    pub fn held(&self) -> usize {
        let (t, p, f) = self.usage();
        t.bytes + p.bytes + f.bytes + self.rendered.mem_usage().1
    }

    /// Evict least recently used entries across the source caches and the rendered previews
    /// until they fit the budget. The newest preview-level source (the photo on screen, or the
    /// one just decoded) is never evicted here.
    pub fn enforce_budget(&mut self) {
        let mut held = self.held();
        while held > self.budget {
            let newest_preview = self.previews.iter().map(|e| e.2).max();
            let candidates = [
                self.thumbs.oldest_tick().map(|t| (t, 0)),
                self.previews.iter().filter(|e| Some(e.2) != newest_preview).map(|e| e.2).min().map(|t| (t, 1)),
                self.full.as_ref().map(|e| (e.2, 2)),
                self.rendered.oldest_tick().map(|t| (t, 3)),
            ];
            let Some((tick, which)) = candidates.into_iter().flatten().min() else { break };
            let freed = match which {
                0 => self.thumbs.pop_oldest(),
                1 => self.previews.iter().position(|e| e.2 == tick).map(|i| self.previews.remove(i).1.bytes()),
                2 => self.full.take().map(|e| e.1.bytes()),
                _ => self.rendered.evict_oldest(),
            };
            match freed {
                Some(b) if b > 0 => held = held.saturating_sub(b),
                _ => break,
            }
        }
    }

    pub fn get(&mut self, id: PhotoId, level: SourceLevel) -> Option<Arc<Rgb32f>> {
        self.get_source(id, level).map(|s| s.image)
    }

    fn get_source(&mut self, id: PhotoId, level: SourceLevel) -> Option<DecodedSource> {
        match level {
            SourceLevel::Thumb => self.thumbs.get(&id).cloned(),
            SourceLevel::Preview => self.previews.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = dac_preview::next_tick();
                e.1.clone()
            }),
            SourceLevel::Full => self.full.as_mut().filter(|e| e.0 == id).map(|e| {
                e.2 = dac_preview::next_tick();
                e.1.clone()
            }),
        }
    }

    /// The facts of photo `id`'s cached sources: the decoder's when one has them, else `header`
    /// (with a smart preview's stored camera tone curve).
    fn source_facts(&self, id: PhotoId, header: SourceInfo) -> SourceInfo {
        let cached = || {
            self.thumbs
                .peek(&id)
                .into_iter()
                .chain(self.previews.iter().filter(|e| e.0 == id).map(|e| &e.1))
                .chain(self.full.iter().filter(|e| e.0 == id).map(|e| &e.1))
        };
        match cached().find(|s| s.info.is_some()).or_else(|| cached().next()) {
            Some(s) => s.info_or(header),
            None => header,
        }
    }

    pub fn insert(&mut self, id: PhotoId, level: SourceLevel, img: Arc<Rgb32f>) {
        self.insert_source(id, level, DecodedSource::new(img, None));
    }

    fn insert_source(&mut self, id: PhotoId, level: SourceLevel, img: DecodedSource) {
        let tick = dac_preview::next_tick();
        match level {
            SourceLevel::Thumb => {
                let cost = img.bytes();
                self.thumbs.insert(id, img, cost);
            }
            SourceLevel::Preview => {
                self.previews.retain(|e| e.0 != id);
                self.previews.push((id, img, tick));
                let cap = if self.preview_capacity == 0 { 4 } else { self.preview_capacity };
                while self.previews.len() > cap {
                    let oldest = self.previews.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i).unwrap_or(0);
                    self.previews.remove(oldest);
                }
            }
            SourceLevel::Full => self.full = Some((id, img, tick)),
        }
        self.enforce_budget();
    }

    /// Forget a photo's decoded sources (e.g. after its file changed).
    pub fn forget(&mut self, id: PhotoId) {
        self.thumbs.remove(&id);
        self.previews.retain(|e| e.0 != id);
        if self.full.as_ref().is_some_and(|e| e.0 == id) {
            self.full = None;
        }
    }

    /// (decoded thumbnail sources, bytes).
    pub fn source_usage(&self) -> (usize, usize) {
        (self.thumbs.len(), self.thumbs.cost())
    }

    /// Whether the decoded source of `id` at `level` is held.
    pub fn has_source(&self, id: PhotoId, level: SourceLevel) -> bool {
        match level {
            SourceLevel::Thumb => self.thumbs.contains(&id),
            SourceLevel::Preview => self.previews.iter().any(|e| e.0 == id),
            SourceLevel::Full => self.full.as_ref().is_some_and(|e| e.0 == id),
        }
    }

    /// Decoded sources held: (thumbnail level, preview level, full size).
    pub fn usage(&self) -> (crate::memory::Usage, crate::memory::Usage, crate::memory::Usage) {
        use crate::memory::Usage;
        let previews = Usage::new(self.previews.len(), self.previews.iter().map(|e| e.1.bytes()).sum());
        let full = self.full.as_ref().map(|e| Usage::new(1, e.1.bytes())).unwrap_or_default();
        (Usage::new(self.thumbs.len(), self.thumbs.cost()), previews, full)
    }

    /// What a render of photo `id` with settings `s` adds to its identity for the denoised picture it will use: 0 unless
    /// it uses one (the Denoise amount is above 0 and the photo has a cached denoised picture).
    pub fn denoise_salt(&self, id: PhotoId, s: &DevelopSettings) -> u64 {
        match self.denoise.spec(id) {
            Some(spec) if s.denoise_amount() > 0.0 => Hasher128::new().str(&spec.key).finish().0 as u64 | 1,
            _ => 0,
        }
    }

    /// How to load photo `p` at `level`: from the cache when its source is there, else from its file (with its denoised
    /// picture beside it when it has one), its smart preview or the demo scene.
    pub fn source_ref(&mut self, p: &Photo, level: SourceLevel) -> SourceRef {
        self.source_ref_with(p, level, true)
    }

    /// [`Self::source_ref`] that never takes a decoded source from memory: for a job that needs something the cached
    /// one lacks (an export, whose denoised picture is made when it runs).
    pub fn source_ref_uncached(&mut self, p: &Photo, level: SourceLevel) -> SourceRef {
        self.source_ref_with(p, level, false)
    }

    fn source_ref_with(&mut self, p: &Photo, level: SourceLevel, cached: bool) -> SourceRef {
        let mut r = self.source_ref_plain(p, level, cached);
        if let (SourceRef::File { denoise, .. }, Some((spec, loader))) = (&mut r, self.denoise.get(p.id)) {
            *denoise = Some((spec.clone(), loader.clone()));
        }
        r
    }

    fn source_ref_plain(&mut self, p: &Photo, level: SourceLevel, cached: bool) -> SourceRef {
        if cached && let Some(a) = self.get_source(p.id, level) {
            return SourceRef::Loaded(Box::new(a));
        }
        // Procedural scenes have a nominal size: "full" is that size, not unbounded.
        let max_edge = level.max_edge().min(match (&p.source, level) {
            (Source::Demo { .. }, SourceLevel::Full) => p.width.max(p.height).max(1) as usize,
            _ => usize::MAX,
        });
        // the original is offline: its smart preview, when there is one. Only cached answers are
        // used here (no file-system call on the UI thread in the app); an original not known to
        // be offline is tried first and its smart preview is the render worker's fallback.
        #[cfg(not(target_arch = "wasm32"))]
        if let (Source::File { path }, Some(dir)) = (&p.source, &self.smart_dir) {
            let sp = dir.join(crate::smart::file_name(p));
            // (a proxy cut short fails to load, like the offline original; the worker's fallback
            // in `SourceRef::load` only uses a proxy that passes `smart::is_valid`)
            if self.availability.is_offline(path) && self.availability.exists(&sp.to_string_lossy()) == Some(true) {
                return SourceRef::Smart { path: sp };
            }
            let mut r = self.origin_ref(&p.source, max_edge);
            if let SourceRef::File { fallback, .. } = &mut r {
                *fallback = Some(sp);
            }
            return r;
        }
        self.origin_ref(&p.source, max_edge)
    }

    /// How to load `origin` at most `max_edge` pixels long, ignoring decoded sources in memory (a
    /// render worker in another wasm instance builds its sources from this).
    pub fn origin_ref(&mut self, origin: &Source, max_edge: usize) -> SourceRef {
        match origin {
            Source::Demo { scene } => {
                if self.scenes.is_empty() {
                    self.scenes = dac_scenes::demo_library();
                }
                match self.scenes.iter().find(|s| s.id == *scene) {
                    Some(s) => SourceRef::Demo { scene: Box::new(s.clone()), max_edge },
                    None => SourceRef::File { path: format!("demo:{scene}"), max_edge, loader: None, fallback: None, denoise: None },
                }
            }
            Source::File { path } => {
                SourceRef::File { path: path.clone(), max_edge, loader: self.file_loader.clone(), fallback: None, denoise: None }
            }
        }
    }
}

/// Everything needed to render one photo, detached from the session.
#[derive(Clone)]
pub struct RenderJob {
    /// Assigned by a frontend to distinguish late completions, even for an identical render key.
    pub request_id: u64,
    pub cache_generation: u64,
    /// Content identity for validating a decoded source returned after a reload.
    pub source_key: Option<Hash128>,
    pub photo: PhotoId,
    pub level: SourceLevel,
    pub source: SourceRef,
    /// Where the photo's pixels come from (for executors that can't share `source`, e.g. a web
    /// worker).
    pub origin: Source,
    pub info: SourceInfo,
    pub settings: Arc<DevelopSettings>,
    pub request: RenderRequest,
    /// Identifies the result for caching: hash of settings + request.
    pub key: u64,
    /// Rendered-thumbnail cache and this job's key in it.
    pub cache: Option<(Arc<PreviewCache>, Hash128)>,
    /// Intermediate results of this view's previous renders (set by the frontend for the loupe):
    /// slider drags then only redo the stages the changed setting feeds.
    pub stages: Option<Arc<StageCache>>,
    /// Keep a full-quality result here as the photo's view preview ([`crate::Session::loupe_job`]).
    pub view_cache: Option<(Arc<PreviewCache>, Hash128)>,
}

pub struct RenderResult {
    pub request_id: u64,
    pub source_key: Option<Hash128>,
    pub photo: PhotoId,
    pub level: SourceLevel,
    pub key: u64,
    pub rendered: Result<Rendered, String>,
    /// A source that was loaded by this job (to be inserted into the cache).
    pub loaded: Option<DecodedSource>,
    /// Set for a [`QuickJob`]'s stand-in: where the image came from.
    pub quick: Option<QuickSource>,
}

impl RenderJob {
    /// Render at draft quality (interactive drags). Gets its own result key.
    pub fn draft(mut self) -> Self {
        if self.request.quality != Quality::Draft {
            self.request.quality = Quality::Draft;
            self.key ^= 0x9e37_79b9_7f4a_7c15;
        }
        self
    }

    /// Draw a diagnostic overlay over the result (e.g. Point Color's visualized range). Gets its
    /// own result key.
    pub fn with_overlay(mut self, overlay: dac_pipeline::Overlay) -> Self {
        if self.request.overlay != overlay {
            self.key ^= self.request.overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
            self.request.overlay = overlay;
            self.key ^= overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        // a diagnostic view (visualized range / spots) must never become the photo's cached preview
        if overlay != dac_pipeline::Overlay::None {
            self.view_cache = None;
        }
        self
    }

    /// Soft-proof the result (see [`dac_pipeline::Proof`]). Gets its own result key and is
    /// never kept as the photo's cached preview.
    pub fn with_proof(mut self, proof: Option<dac_pipeline::Proof>) -> Self {
        if self.request.proof != proof {
            let k = |p: Option<dac_pipeline::Proof>| p.map_or(0, |p| p.key()).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
            self.key ^= k(self.request.proof) ^ k(proof);
            self.request.proof = proof;
        }
        if proof.is_some() {
            self.view_cache = None;
        }
        self
    }

    /// Reuse `stages` across this view's renders.
    pub fn with_stages(mut self, stages: Arc<StageCache>) -> Self {
        self.stages = Some(stages);
        self
    }

    pub fn run(self) -> RenderResult {
        if let Some((cache, key)) = &self.cache
            && let Some(img) = cache.get_at(self.cache_generation, *key)
        {
            let image = Arc::unwrap_or_clone(img);
            let histogram = Histogram::of_srgb8(&image);
            return RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None }),
                loaded: None,
                quick: None,
            };
        }
        let was_loaded = matches!(self.source, SourceRef::Loaded(_));
        match self.source.load_source() {
            Ok(source) => {
                // the plain picture, or (Denoise amount above 0 and a denoised picture cached) the mix the amount asks for
                let src = &source.image_for(self.settings.denoise_amount());
                let info = source.info_or(self.info);
                // Thumbnails (many small jobs side by side) stay on the CPU; views and exports use
                // the GPU when there is one.
                let gpu = self.cache.is_none();
                let rendered = develop(src, &info, &self.settings, &self.request, self.stages.as_deref(), gpu);
                if let Some((cache, key)) = &self.cache {
                    cache.put_at(self.cache_generation, *key, Arc::new(rendered.image.clone()));
                }
                if let Some((cache, key)) = &self.view_cache
                    && self.request.quality == Quality::Full
                {
                    cache.put_deferred_at(self.cache_generation, *key, Arc::new(rendered.image.clone()));
                }
                RenderResult {
                    request_id: self.request_id,
                    source_key: self.source_key,
                    photo: self.photo,
                    level: self.level,
                    key: self.key,
                    rendered: Ok(rendered),
                    loaded: (!was_loaded).then_some(source),
                    quick: None,
                }
            }
            Err(e) => RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Err(e),
                loaded: None,
                quick: None,
            },
        }
    }
}

/// Render `src`: on the GPU when `gpu` is set and a GPU is available (`dac_gpu`), else on
/// the CPU, reusing `stages` either way. Both produce the same image within 1–3 LSB (see
/// `docs/gpu-pipeline.md`); `{ENV_PREFIX}_GPU=0` or [`dac_gpu::set_enabled`] forces the CPU.
pub fn develop(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>, gpu: bool) -> Rendered {
    // LUT profiles have no GPU stage: they render on the CPU
    if gpu
        && !dac_pipeline::lut::is_lut_profile(&s.profile.id)
        && !s.masks.iter().any(|m| m.visible && m.refine > 0.0)
        && req.proof.is_none()
        && let Some(r) = dac_gpu::render(src, info, s, req, stages)
    {
        return r;
    }
    match stages {
        Some(st) => dac_pipeline::render_cached(src, info, s, req, st),
        None => dac_pipeline::render(src, info, s, req),
    }
}

/// Where a [`QuickJob`]'s stand-in image came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuickSource {
    /// The photo's own develop render for its current settings (cached view or thumbnail render).
    Cached,
    /// The camera's embedded JPEG (unedited raws).
    Embedded,
    /// A thumbnail-level develop render made for the occasion.
    Small,
}

/// Something to show *now* for a photo while its real render is prepared. Tried in order: cached
/// renders, the embedded preview of an unedited raw, a thumbnail-level render.
#[derive(Clone)]
pub struct QuickJob {
    pub request_id: u64,
    pub photo: PhotoId,
    pub key: u64,
    /// Cached renders to try first, in order: (cache, key).
    pub cached: Vec<(Arc<PreviewCache>, Hash128)>,
    /// Embedded preview of an unedited raw: (path, loader, max edge).
    pub embedded: Option<(String, PreviewLoader, usize)>,
    /// Last resort: run this thumbnail job.
    pub small: Option<Box<RenderJob>>,
}

impl QuickJob {
    pub fn run(self) -> RenderResult {
        let (photo, key) = (self.photo, self.key);
        let request_id = self.request_id;
        let done = |rendered: Result<Rendered, String>, quick| RenderResult {
            request_id,
            source_key: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered,
            loaded: None,
            quick,
        };
        let rendered = |image: Rgba8| {
            let histogram = Histogram::of_srgb8(&image);
            Ok(Rendered { image, histogram, deep: None })
        };
        for (cache, k) in &self.cached {
            if let Some(img) = cache.get(*k) {
                return done(rendered(Arc::unwrap_or_clone(img)), Some(QuickSource::Cached));
            }
        }
        if let Some((path, loader, edge)) = &self.embedded
            && let Some(image) = loader(path, *edge)
        {
            return done(rendered(image), Some(QuickSource::Embedded));
        }
        if let Some(job) = self.small {
            let r = job.run();
            return RenderResult { request_id, key, quick: Some(QuickSource::Small), ..r };
        }
        done(Err("no quick preview".into()), None)
    }
}

/// What identifies a photo's pixels for caching: its content hash, else its source.
pub fn content_key(p: &Photo) -> String {
    match (&p.content_hash, &p.source) {
        (Some(h), _) => h.clone(),
        (None, Source::Demo { scene }) => format!("demo:{scene}"),
        (None, Source::File { path }) => format!("file:{path}:{}", p.file_size),
    }
}

pub fn source_info(p: &Photo) -> SourceInfo {
    // Procedural demo scenes are scene-referred HDR (like raw files): use the filmic tone map.
    if matches!(p.source, Source::Demo { .. }) {
        return SourceInfo { raw: true, ..Default::default() };
    }
    // A raw shown from its embedded preview is a rendered (display-referred) JPEG: relative white
    // balance and the display tone curve, like any other rendered file.
    if p.develops_raw() {
        let (temp, tint) = if p.relative_wb() { (6500.0, 0.0) } else { p.as_shot_wb.unwrap_or((5500.0, 0.0)) };
        SourceInfo { raw: true, as_shot_temp: temp, as_shot_tint: tint, lens: p.embedded_lens, relative_wb: p.relative_wb(), ..Default::default() }
    } else {
        SourceInfo::default()
    }
}

impl crate::Session {
    /// Build a render job for `id` fitting `max_w × max_h`. `before` renders the unedited look.
    pub fn render_job(&mut self, id: PhotoId, max_w: usize, max_h: usize, before: bool, apply_crop: bool) -> Option<RenderJob> {
        self.build_job(id, max_w, max_h, before, apply_crop, None)
    }

    /// A grid/filmstrip thumbnail job: the long edge is rounded up to one of [`THUMB_SIZES`]
    /// (≥ `long_edge`) and the result comes from / goes to the thumbnail cache (memory + disk).
    pub fn thumb_job(&mut self, id: PhotoId, long_edge: usize) -> Option<RenderJob> {
        let b = thumb_bucket(long_edge);
        self.build_job(id, b, b, false, true, Some(b))
    }

    fn build_job(
        &mut self,
        id: PhotoId,
        max_w: usize,
        max_h: usize,
        before: bool,
        apply_crop: bool,
        thumb_bucket: Option<usize>,
    ) -> Option<RenderJob> {
        let p = self.catalog.photo(id)?.clone();
        let settings = if before { Arc::new(self.before_settings(&p)) } else { p.develop.clone() };
        let level = SourceLevel::for_size(if thumb_bucket.is_some() {
            max_w.max(max_h) // (grid thumbnails of tight crops stay cheap)
        } else {
            source_edge_needed(&p, &settings, apply_crop, max_w.max(max_h))
        });
        let source = self.media.source_ref(&p, level);
        let request = RenderRequest { apply_crop, ..RenderRequest::fit(max_w, max_h) };
        // the photo id is part of the key: two photos with the same settings and size must not
        // share a result (a view slot showing photo A would otherwise look current for photo B)
        // …and so is the file's content: a file changed on disk (Reload) renders afresh
        let content_key = content_key(&p);
        let source_key = Hasher128::new().str(&content_key).finish();
        let content = source_key.0 as u64;
        // Temporary "before" settings must not replace the photo's cached develop hash.
        let settings_hash = if before { settings.hash64() } else { self.media.settings_hashes.get(id, &settings) };
        let salt = self.media.denoise_salt(id, &settings);
        let key = settings_hash
            ^ ((max_w as u64) << 40)
            ^ ((max_h as u64) << 20)
            ^ (apply_crop as u64)
            ^ (level as u64) << 60
            ^ id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ content.rotate_left(17)
            ^ salt;
        let cache = thumb_bucket.map(|b| {
            let mut k = Hasher128::new();
            k.str(&content_key).u64(settings_hash).u64(b as u64).u64(RENDER_CACHE_VERSION).u64(crate::camera_profiles::cache_key());
            // (only a render that uses a denoised picture has it in its key, so every other thumbnail stays valid)
            if salt != 0 {
                k.u64(salt);
            }
            (self.media.rendered.clone(), k.finish())
        });
        Some(RenderJob {
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(source_key),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: source_info(&p),
            settings,
            request,
            key,
            cache,
            stages: None,
            view_cache: None,
        })
    }

    /// A render of `id` with `settings` in place of its own, fitting `max_w × max_h` (the loupe's
    /// temporary preview while hovering a preset or profile; nothing is committed or cached).
    pub fn preview_job(&mut self, id: PhotoId, max_w: usize, max_h: usize, apply_crop: bool, settings: &DevelopSettings) -> Option<RenderJob> {
        let mut job = self.render_job(id, max_w, max_h, false, apply_crop)?;
        job.settings = Arc::new(settings.clone());
        job.key = settings.hash64() ^ ((max_w as u64) << 40) ^ ((max_h as u64) << 20) ^ (apply_crop as u64) ^ (job.level as u64) << 60;
        Some(job)
    }

    /// Cache key of a variant thumbnail ([`Self::variant_job`]).
    pub fn variant_key(p: &Photo, settings: &DevelopSettings, edge: usize) -> Hash128 {
        Hasher128::new()
            .str(&content_key(p))
            .str("variant")
            .u64(settings.hash64())
            .u64(edge as u64)
            .u64(RENDER_CACHE_VERSION)
            .u64(crate::camera_profiles::cache_key())
            .finish()
    }

    /// A thumbnail of `id` rendered with `settings` instead of its own (profile and preset
    /// browsers): from the thumbnail-level source, long edge `edge` (≤ 512), cropped. The result
    /// is cached (memory + the library's disk cache) under the photo's content and the settings
    /// hash, so a variant renders once; `key` is derived from the same hash, so a frontend can
    /// keep one texture per variant.
    pub fn variant_job(&mut self, id: PhotoId, settings: &DevelopSettings, edge: usize) -> Option<RenderJob> {
        let p = self.catalog.photo(id)?.clone();
        let edge = edge.clamp(16, SourceLevel::Thumb.max_edge());
        let level = SourceLevel::Thumb;
        let source = self.media.source_ref(&p, level);
        let ck = Self::variant_key(&p, settings, edge);
        Some(RenderJob {
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(Hasher128::new().str(&content_key(&p)).finish()),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: source_info(&p),
            settings: Arc::new(settings.clone()),
            request: RenderRequest { apply_crop: true, ..RenderRequest::fit(edge, edge) },
            key: (ck.0 as u64) ^ ((ck.0 >> 64) as u64),
            cache: Some((self.media.rendered.clone(), ck)),
            stages: None,
            view_cache: None,
        })
    }

    /// A square close-up of `face` (normalized, in the photo's upright frame) for a People card: the
    /// photo's own look with its crop replaced by a square around the face (room for hair and chin),
    /// `edge` pixels across (≤ 512). The source level is the smallest that keeps the face sharp, up
    /// to the preview. Cached like [`Self::variant_job`], and `key` likewise follows the content.
    pub fn face_job(&mut self, id: PhotoId, face: dac_geom::Rect, edge: usize) -> Option<RenderJob> {
        let p = self.catalog.photo(id)?.clone();
        // `face` is on the upright (EXIF-oriented) photo; the crop below is in the frame after the
        // user's Rotate Left/Right, so carry the box (and the photo's size) over to it.
        let orient = p.develop.orientation;
        let face = orient.map_norm_rect(face);
        let (w, h) = (f64::from(p.width.max(1)), f64::from(p.height.max(1)));
        let (w, h) = if orient.swaps_axes() { (h, w) } else { (w, h) };
        let side = ((face.x1 - face.x0) * w).max((face.y1 - face.y0) * h) * 1.7;
        // finite, within the photo, and never wider than its short side (so the clamps below are valid)
        let side = if side.is_finite() { side.clamp(1.0, w.min(h)) } else { w.min(h) };
        let (cx, cy) = ((face.x0 + face.x1) / 2.0 * w, (face.y0 + face.y1) / 2.0 * h);
        let (cx, cy) = (if cx.is_finite() { cx } else { w / 2.0 }, if cy.is_finite() { cy } else { h / 2.0 });
        let (x0, y0) = (cx.clamp(side / 2.0, w - side / 2.0) - side / 2.0, cy.clamp(side / 2.0, h - side / 2.0) - side / 2.0);
        let rect = dac_geom::Rect { x0: x0 / w, y0: y0 / h, x1: (x0 + side) / w, y1: (y0 + side) / h };
        let mut settings = (*p.develop).clone();
        settings.crop = dac_develop::Crop { geometry: dac_geom::CropGeometry { rect, angle: 0.0 }, ..Default::default() };
        let edge = edge.clamp(16, SourceLevel::Thumb.max_edge());
        // pixels along the long edge that leave `edge` across the crop
        let needed = (edge as f64 * w.max(h) / side).ceil().min(SourceLevel::Preview.max_edge() as f64);
        let level = SourceLevel::for_size(needed as usize);
        let source = self.media.source_ref(&p, level);
        let ck = Self::variant_key(&p, &settings, edge);
        Some(RenderJob {
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(Hasher128::new().str(&content_key(&p)).finish()),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: source_info(&p),
            settings: Arc::new(settings),
            request: RenderRequest { apply_crop: true, ..RenderRequest::fit(edge, edge) },
            key: (ck.0 as u64) ^ ((ck.0 >> 64) as u64),
            cache: Some((self.media.rendered.clone(), ck)),
            stages: None,
            view_cache: None,
        })
    }

    /// Size-independent cache key of a photo's view render (loupe) for its current settings.
    fn view_key(p: &Photo, apply_crop: bool) -> Hash128 {
        Hasher128::new()
            .str(&content_key(p))
            .str("view")
            .u64(p.develop.hash64())
            .u64(apply_crop as u64)
            .u64(RENDER_CACHE_VERSION)
            .u64(crate::camera_profiles::cache_key())
            .finish()
    }

    /// Key of the photo's 1:1 preview (Build 1:1 Previews): kept apart from the standard view
    /// preview so it can be discarded on its own (`library.discardPreviews`).
    pub(crate) fn full_view_key(p: &Photo) -> Hash128 {
        let v = Self::view_key(p, true).0;
        Hasher128::new().u64(v as u64).u64((v >> 64) as u64).str("1:1").finish()
    }

    /// The loupe's render job: like [`Self::render_job`], and a full-quality result is kept as the
    /// photo's view preview (memory, and disk with a library) for [`Self::quick_view_job`].
    pub fn loupe_job(&mut self, id: PhotoId, max_w: usize, max_h: usize, apply_crop: bool) -> Option<RenderJob> {
        let mut job = self.render_job(id, max_w, max_h, false, apply_crop)?;
        let p = self.catalog.photo(id)?;
        job.view_cache = Some((self.media.rendered.clone(), Self::view_key(p, apply_crop)));
        Some(job)
    }

    /// A render of one window of the loupe's frame, for a view zoomed past what one whole-frame
    /// render can hold: the frame is `full_w × full_h` (the size the loupe would draw it at) and
    /// the result is `window`'s pixels of it, at that scale, from the source level that size needs.
    /// Nothing is cached here (a window is only worth keeping while it is on screen). `None`: no
    /// such photo, or the window reads (spots, Auto Mask strokes) more than one render can hold.
    pub fn region_job(
        &mut self,
        id: PhotoId,
        full_w: usize,
        full_h: usize,
        window: dac_pipeline::PixelWindow,
        apply_crop: bool,
    ) -> Option<RenderJob> {
        self.region_job_of(id, full_w, full_h, window, apply_crop, false)
    }

    /// [`Self::region_job`] of the photo without its edits (its crop kept): the Before side.
    pub fn region_job_before(
        &mut self,
        id: PhotoId,
        full_w: usize,
        full_h: usize,
        window: dac_pipeline::PixelWindow,
        apply_crop: bool,
    ) -> Option<RenderJob> {
        self.region_job_of(id, full_w, full_h, window, apply_crop, true)
    }

    fn region_job_of(
        &mut self,
        id: PhotoId,
        full_w: usize,
        full_h: usize,
        window: dac_pipeline::PixelWindow,
        apply_crop: bool,
        before: bool,
    ) -> Option<RenderJob> {
        let mut job = self.render_job(id, full_w, full_h, before, apply_crop)?;
        // what the window's spots and Auto Mask strokes read must fit in one render: else the
        // caller keeps the whole-frame render (a window alone would come out wrong)
        let p = self.catalog.photo(id)?;
        let frame = dac_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &job.settings,
            apply_crop,
            p.embedded_lens.as_ref(),
        );
        let ppl = frame.px_per_long(full_w);
        dac_pipeline::spots::window_for_reads_checked(&job.settings, &frame, full_w, full_h, ppl, window.clamped(full_w, full_h))?;
        job.request.window = Some(window);
        job.key = Hasher128::new()
            .u64(job.key)
            .str("window")
            .u64(window.x as u64)
            .u64(window.y as u64)
            .u64(window.w as u64)
            .u64(window.h as u64)
            .finish()
            .0 as u64;
        Some(job)
    }

    /// The embedded preview of an unedited raw (path, loader), when the app installed a loader.
    fn embedded_of(&self, p: &Photo) -> Option<(String, PreviewLoader)> {
        match (&p.source, &self.media.preview_loader) {
            (Source::File { path }, Some(l)) if p.kind == MediaKind::Raw && crate::import::has_import_look(p) => Some((path.clone(), l.clone())),
            _ => None,
        }
    }

    /// The camera's embedded preview of a raw file, for the face scan. Faces are read from the unedited picture, so the
    /// photo's edits do not matter, and the camera's own JPEG (oriented, display-ready) costs a fraction of decoding the
    /// raw. `None` for anything else, and where the host installed no loader.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn scan_preview_of(&self, p: &Photo) -> Option<(String, PreviewLoader)> {
        match (&p.source, &self.media.preview_loader) {
            (Source::File { path }, Some(l)) if p.kind == MediaKind::Raw => Some((path.clone(), l.clone())),
            _ => None,
        }
    }

    /// Something to show in the loupe right away for `id` (see [`QuickJob`]): its cached view
    /// render, else the embedded preview of an unedited raw, else a cached or fresh thumbnail.
    pub fn quick_view_job(&mut self, id: PhotoId, max_edge: usize, apply_crop: bool) -> Option<QuickJob> {
        let p = self.catalog.photo(id)?.clone();
        let small = self.thumb_job(id, THUMB_SIZES[THUMB_SIZES.len() - 1])?;
        // (the thumbnail job itself starts with the cached thumbnail, after the sharper embedded preview)
        let mut cached = vec![(self.media.rendered.clone(), Self::view_key(&p, apply_crop))];
        // a 1:1 preview (Build 1:1 Previews) stands in when there is no standard one
        if apply_crop {
            cached.push((self.media.rendered.clone(), Self::full_view_key(&p)));
        }
        let h = Hasher128::new().str(&content_key(&p)).str("quick").u64(p.develop.hash64()).u64(apply_crop as u64).finish();
        let key = h.0 as u64;
        let embedded = self.embedded_of(&p).map(|(path, l)| (path, l, max_edge.clamp(1, SourceLevel::Preview.max_edge())));
        Some(QuickJob { request_id: 0, photo: id, key, cached, embedded, small: apply_crop.then(|| Box::new(small)) })
    }

    /// A first grid thumbnail for an unedited raw: the cached render for `job` (the real
    /// thumbnail job, from [`Self::thumb_job`]) if there is one — then the result carries
    /// [`QuickSource::Cached`] and `job.key` and is final — else its embedded preview. `None` for
    /// other photos (render them directly).
    pub fn quick_thumb_job(&mut self, job: &RenderJob) -> Option<QuickJob> {
        let p = self.catalog.photo(job.photo)?.clone();
        let (path, l) = self.embedded_of(&p)?;
        let edge = job.request.max_w.max(job.request.max_h);
        Some(QuickJob {
            request_id: 0,
            photo: job.photo,
            key: job.key,
            cached: job.cache.clone().into_iter().collect(),
            embedded: Some((path, l, edge)),
            small: None,
        })
    }

    /// Accept a finished job's loaded source into the cache.
    pub fn accept(&mut self, r: &RenderResult) {
        if let Some(src) = &r.loaded
            && r.source_key.is_none_or(|key| self.catalog.photo(r.photo).is_some_and(|p| Hasher128::new().str(&content_key(p)).finish() == key))
        {
            self.media.insert_source(r.photo, r.level, src.clone());
        }
    }

    /// Synchronous render (CLI, MCP, tests).
    pub fn render_now(&mut self, id: PhotoId, max_w: usize, max_h: usize) -> Result<Rendered, String> {
        let job = self.render_job(id, max_w, max_h, false, true).ok_or("no such photo")?;
        let r = job.run();
        self.accept(&r);
        r.rendered
    }

    /// Synchronous render for export: like [`Self::render_now`], into colour space `space` with
    /// sample format `depth`.
    pub fn render_export(
        &mut self,
        id: PhotoId,
        max_w: usize,
        max_h: usize,
        space: dac_pipeline::OutputSpace,
        depth: dac_pipeline::OutputDepth,
    ) -> Result<Rendered, String> {
        let r = self.export_job(id, max_w, max_h, space, depth)?.run();
        self.accept(&r);
        r.rendered
    }

    /// The job [`Self::render_export`] runs, to run elsewhere (e.g. a background export thread).
    pub fn export_job(
        &mut self,
        id: PhotoId,
        max_w: usize,
        max_h: usize,
        space: dac_pipeline::OutputSpace,
        depth: dac_pipeline::OutputDepth,
    ) -> Result<RenderJob, String> {
        let mut job = self.render_job(id, max_w, max_h, false, true).ok_or("no such photo")?;
        // An export is made from the denoised picture its Denoise amount asks for, made when the job runs if the
        // background queue has not got to it. (A source kept in memory may lack the picture, so the job reads the file.)
        if let Some(wanted) = self.denoise_for_export(id, &job.settings)? {
            let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
            job.source = self.media.source_ref_uncached(&p, job.level);
            if let SourceRef::File { denoise, .. } = &mut job.source {
                *denoise = Some(wanted);
            }
        }
        job.request.space = space;
        job.request.depth = depth;
        job.key ^= (space as u64 + 1).wrapping_mul(0xa076_1d64_78bd_642f) ^ (depth as u64 + 1).wrapping_mul(0xe703_7ed1_a0b4_28db);
        Ok(job)
    }

    /// Prefer decoder facts to header-only metadata for pixel-statistics commands.
    pub fn source_info(&self, id: PhotoId) -> SourceInfo {
        let header = self.catalog.photo(id).map(|p| source_info(p)).unwrap_or_default();
        self.media.source_facts(id, header)
    }

    /// The source proxy for pixel-statistics commands (auto tone/WB), loading synchronously.
    pub fn source_now(&mut self, id: PhotoId, level: SourceLevel) -> Result<Arc<Rgb32f>, String> {
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let r = self.media.source_ref(&p, level).load_source()?;
        let image = r.image.clone();
        self.media.insert_source(id, level, r);
        Ok(image)
    }
}

/// What an import learns from a file header (set by the app from `dac-codecs`/`-raw`).
#[derive(Clone, Debug, Default)]
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub kind: MediaKind,
    pub file_size: u64,
    pub captured: Option<String>,
    pub meta: dac_catalog::Meta,
    pub as_shot_wb: Option<(f64, f64)>,
    /// Hash of the file's bytes (hex), for duplicate detection. When it is a 32-digit
    /// [`dac_preview::hash_bytes`] of the whole file (as the native probe computes it),
    /// Import → Copy verifies each copy against it instead of reading the source again.
    pub content_hash: Option<String>,
    /// Lens corrections embedded in the file (DNG opcodes).
    pub embedded_lens: Option<dac_develop::EmbeddedLens>,
    /// The file's embedded XMP packet (raw/DNG files), for develop settings stored inside the file.
    pub xmp: Option<String>,
    /// A raw variant that can't be decoded yet: why. The file is described (and will be shown and
    /// edited) from its embedded preview; see [`dac_catalog::Photo::preview_only`].
    pub preview_only: Option<String>,
}

pub type FileProbe = Arc<dyn Fn(&str) -> Result<ProbeInfo, String> + Send + Sync>;

#[cfg(test)]
mod thumbnail_hash_tests {
    use super::*;
    use dac_catalog::Op;

    fn session() -> crate::Session {
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: "hash-fixture.jpg".into() }, "fixture.jpg", "JPEG", 6000, 4000, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s
    }
    #[test]
    fn hashes_match_uncached_keys_and_are_not_recomputed_for_sizes_metadata_or_before() {
        let mut s = session();
        for edge in [256, 384, 512, 256] {
            let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
            let b = thumb_bucket(edge);
            // Independent original formula: never asks the new hash cache for expected values.
            let settings = p.develop.hash64();
            let content = Hasher128::new().str(&content_key(&p)).finish().0 as u64;
            let expected =
                settings ^ ((b as u64) << 40) ^ ((b as u64) << 20) ^ 1 ^ p.id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ content.rotate_left(17);
            let disk = Hasher128::new()
                .str(&content_key(&p))
                .u64(settings)
                .u64(b as u64)
                .u64(RENDER_CACHE_VERSION)
                .u64(crate::camera_profiles::cache_key())
                .finish();
            let job = s.thumb_job(p.id, edge).unwrap();
            assert_eq!(job.key, expected);
            assert_eq!(job.cache.unwrap().1, disk);
        }
        s.catalog.apply(Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
        s.thumb_job(PhotoId(1), 256).unwrap();
        s.render_job(PhotoId(1), 256, 256, true, true).unwrap();
        s.thumb_job(PhotoId(1), 256).unwrap();
        assert_eq!(s.media.settings_hashes.computations, 1);
        let mut edit = DevelopSettings::default();
        edit.light.exposure = 1.0;
        let undo = s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        s.thumb_job(PhotoId(1), 256).unwrap();
        s.thumb_job(PhotoId(1), 384).unwrap();
        assert_eq!(s.media.settings_hashes.computations, 2);
        s.catalog.apply(undo).unwrap();
        s.thumb_job(PhotoId(1), 256).unwrap();
        assert_eq!(s.media.settings_hashes.computations, 3);
    }
    #[test]
    fn hash_cache_is_bounded_weak_and_shared_settings_remain_independent() {
        let mut hashes = SettingsHashes::default();
        let shared = Arc::new(DevelopSettings::default());
        for id in 1..=SETTINGS_HASH_ENTRIES as u64 * 3 {
            assert_eq!(hashes.get(PhotoId(id), &shared), shared.hash64());
        }
        assert_eq!(hashes.entries.len(), SETTINGS_HASH_ENTRIES);
        assert_eq!(hashes.order.len(), SETTINGS_HASH_ENTRIES);
        assert_eq!(Arc::strong_count(&shared), 1);
        let mut edit = (*shared).clone();
        edit.light.exposure = 1.0;
        let edit = Arc::new(edit);
        let a = PhotoId(10000);
        let b = PhotoId(10001);
        let initial = hashes.get(a, &shared);
        assert_eq!(hashes.get(b, &shared), initial);
        assert_ne!(hashes.get(a, &edit), initial);
        assert_eq!(hashes.get(b, &shared), initial);
        let mut s = session();
        s.thumb_job(PhotoId(1), 256).unwrap();
        s.media.clear_sources();
        assert!(s.media.settings_hashes.entries.is_empty());
        assert!(s.media.settings_hashes.order.is_empty());
    }
    #[test]
    fn source_from_before_content_reload_is_not_accepted() {
        let mut s = session();
        s.media.file_loader = Some(Arc::new(|_, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
        let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
        assert!(old.loaded.is_some());
        s.catalog
            .apply(Op::SetContent {
                id: PhotoId(1),
                width: 6000,
                height: 4000,
                file_size: 99,
                content_hash: Some("replacement".into()),
                preview_only: None,
            })
            .unwrap();
        s.media.forget(PhotoId(1));
        s.accept(&old);
        assert_eq!(s.media.source_usage().0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denoise_mix_is_reserved_before_the_source_enters_the_cache() {
        let mut plain = Rgb32f::new(8, 8);
        plain.data.fill([1.0; 3]);
        let mut source = DecodedSource::new(Arc::new(plain), None);
        source.denoised = Some(Arc::new(Twin::new(Arc::new(Rgb32f::new(8, 8)))));
        let reserved = source.bytes();
        assert!(reserved >= 3 * 8 * 8 * 12);
        assert_eq!(source.image_for(0.5).data[0], [0.5; 3]);
        assert_eq!(source.bytes(), reserved, "creating a mix cannot evade the LRU's recorded cost");
        let _ = source.image_for(0.25);
        assert_eq!(source.bytes(), reserved, "changing Amount replaces the reserved mix");
    }

    #[test]
    fn decoder_info_survives_render_jobs_cache_and_eviction() {
        let tone = dac_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
            let x = 0.01 * (i + 1) as f32;
            [x, (x * 2.0).min(0.9)]
        }))
        .unwrap();
        let info = SourceInfo { raw: true, relative_wb: true, camera_tone: Some(tone), ..Default::default() };
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let mut job = s.render_job(id, 64, 64, false, true).unwrap();
        let loaded_info = info.clone();
        job.source = SourceRef::File {
            path: "synthetic.arw".into(),
            max_edge: 64,
            loader: Some(Arc::new(move |_, _| {
                let mut image = Rgb32f::new(64, 64);
                image.data.fill([0.1; 3]);
                Ok((image, loaded_info.clone()))
            })),
            fallback: None,
            denoise: None,
        };
        job.info = SourceInfo::default(); // Header facts cannot override decoder facts.
        job.settings = Arc::new(DevelopSettings::default());
        let r = job.clone().run();
        assert_eq!(r.loaded.as_ref().unwrap().info.as_ref(), Some(&info));
        let expected = dac_pipeline::render(&r.loaded.as_ref().unwrap().image, &info, &job.settings, &job.request);
        assert_eq!(r.rendered.as_ref().unwrap().image.data, expected.image.data);
        s.accept(&r);
        job.source = s.media.source_ref(s.catalog.photo(id).unwrap(), job.level);
        let again = job.run();
        assert!(again.loaded.is_none());
        assert_eq!(r.rendered.unwrap().image.data, again.rendered.unwrap().image.data);
        s.media.forget(id);
        assert!(s.media.get_source(id, r.level).is_none());
    }

    #[test]
    fn budget_evicts_least_recently_used_across_caches() {
        let img = |w: usize| Arc::new(Rgb32f::new(w, w));
        let mut m = MediaCache::default();
        let mb = 1 << 20;
        m.set_budget(30 * mb);
        // 3 previews of 12 MB (1000² × 12 B) don't fit 30 MB: the least recently used goes
        m.insert(PhotoId(1), SourceLevel::Preview, img(1000));
        m.insert(PhotoId(2), SourceLevel::Preview, img(1000));
        assert!(m.get(PhotoId(1), SourceLevel::Preview).is_some()); // 1 is now more recent than 2
        m.insert(PhotoId(3), SourceLevel::Preview, img(1000));
        assert!(m.get(PhotoId(2), SourceLevel::Preview).is_none());
        assert!(m.get(PhotoId(1), SourceLevel::Preview).is_some() && m.get(PhotoId(3), SourceLevel::Preview).is_some());
        // a thumbnail source and a rendered preview compete in the same budget
        m.insert(PhotoId(4), SourceLevel::Thumb, img(500));
        m.rendered.put(Hash128(7), Arc::new(dac_raster::Rgba8::new(1000, 1000)));
        assert!(m.held() <= 30 * mb, "{}", m.held());
        // the newest preview (the one on screen) is never evicted, even alone over budget
        m.set_budget(mb);
        assert!(m.get(PhotoId(3), SourceLevel::Preview).is_some());
        assert_eq!(m.usage().1.count, 1);
        assert_eq!(m.usage().0.count, 0);
        assert_eq!(m.rendered.mem_usage().0, 0);
    }

    #[test]
    fn work_gate_waits_for_room() {
        use crate::memory::WorkGate;
        let g = Arc::new(WorkGate::new(100));
        let a = g.acquire(80);
        // more than fits waits until `a` is released; urgent work never waits
        let urgent = g.acquire_urgent(500);
        assert_eq!(g.usage().0, 580);
        drop(urgent);
        let g2 = g.clone();
        let t = std::thread::spawn(move || {
            let _b = g2.acquire(50);
            g2.usage().0
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(g.usage().0, 80, "the second holder is still waiting");
        drop(a);
        assert_eq!(t.join().unwrap(), 50);
        assert_eq!(g.usage().0, 0);
        // a single holder may exceed the limit
        drop(g.acquire(1000));
    }

    #[test]
    fn levels_cover_sizes_and_large_exports_use_the_original() {
        assert_eq!(SourceLevel::for_size(256), SourceLevel::Thumb);
        assert_eq!(SourceLevel::for_size(2560), SourceLevel::Preview);
        assert_eq!(SourceLevel::for_size(2561), SourceLevel::Full);
        let mut s = crate::Session::with_demo();
        let p = s.catalog.photos().next().unwrap().clone();
        let long = p.width.max(p.height) as usize;
        assert!(long > 2560, "demo photos are camera-sized");
        // a full-size export reads the source at its native size, not an upscaled preview
        let job = s.render_job(p.id, long, long, false, true).unwrap();
        assert_eq!(job.level, SourceLevel::Full);
        match &job.source {
            SourceRef::Demo { max_edge, .. } => assert_eq!(*max_edge, long),
            _ => panic!("demo source expected"),
        }
        let loupe = s.render_job(p.id, 1600, 1600, false, true).unwrap();
        assert_eq!(loupe.level, SourceLevel::Preview);
        assert_ne!(loupe.key, s.render_job(p.id, 1600, 1600, false, true).unwrap().draft().key);
        // two photos with identical settings and size never share a result key
        let plain: Vec<_> = s.catalog.photos().filter(|p| !p.is_edited()).map(|p| p.id).take(2).collect();
        let (a, b) = (s.render_job(plain[0], 1600, 1600, false, true).unwrap(), s.render_job(plain[1], 1600, 1600, false, true).unwrap());
        assert_ne!(a.key, b.key);
    }

    // Issue #323: a window of a 1:1 view is its own job: the window's pixels, at the zoom scale,
    // from the original, with a key of its own
    #[test]
    fn region_jobs_render_a_window_of_the_zoomed_frame() {
        use dac_pipeline::PixelWindow;
        let mut s = crate::Session::with_demo();
        let p = s.catalog.photos().next().unwrap().clone();
        let (w, h) = (p.width as usize, p.height as usize);
        assert!(w.max(h) > 2560, "demo photos are camera-sized");
        let win = PixelWindow { x: w / 3, y: h / 3, w: 300, h: 200 };
        let job = s.region_job(p.id, w, h, win, true).unwrap();
        assert_eq!(job.level, SourceLevel::Full, "a 1:1 window reads the original");
        let img = job.run().rendered.unwrap().image;
        assert_eq!((img.width, img.height), (300, 200));
        // other windows, and the whole-frame job, never share a key
        let other = s.region_job(p.id, w, h, PixelWindow { x: win.x + 64, ..win }, true).unwrap();
        let whole = s.render_job(p.id, w, h, false, true).unwrap();
        let again = s.region_job(p.id, w, h, win, true).unwrap();
        assert_ne!(other.key, again.key);
        assert_ne!(whole.key, again.key);
        assert_eq!(s.region_job(p.id, w, h, win, true).unwrap().key, again.key, "the same window keeps its key");
        // a window never writes the photo's view preview or the thumbnail cache
        assert!(again.view_cache.is_none() && again.cache.is_none());
    }

    // Issue #323: a tight crop shown at its own pixels needs the original, not the 2560 px preview
    #[test]
    fn a_tight_crop_reads_the_source_level_its_pixels_need() {
        let mut s = crate::Session::with_demo();
        let p = s.catalog.photos().next().unwrap().clone();
        let long = p.width.max(p.height) as usize;
        assert!(long > 2560);
        let id = p.id;
        let mut d = (*s.develop_of(id).unwrap()).clone();
        // the uncropped photo at 1600 px: the preview is plenty
        assert_eq!(s.render_job(id, 1600, 1600, false, true).unwrap().level, SourceLevel::Preview);
        // a crop to 30 % of each side shown 1600 px long needs 1600 / 0.3 px of the source
        d.crop.geometry.rect = dac_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.5 };
        s.set_develop(id, d, "Crop").unwrap();
        assert_eq!(s.render_job(id, 1600, 1600, false, true).unwrap().level, SourceLevel::Full);
        // …but with the crop tool open (the whole photo shown) it does not
        assert_eq!(s.render_job(id, 1600, 1600, false, false).unwrap().level, SourceLevel::Preview);
        // and a crop shown small enough still reads the preview
        assert_eq!(s.render_job(id, 600, 600, false, true).unwrap().level, SourceLevel::Preview);
    }

    // a window whose spot reads from further away than a render can hold is refused
    #[test]
    fn a_region_job_is_refused_when_a_spot_reads_beyond_what_fits() {
        use dac_pipeline::PixelWindow;
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let (w, h) = (p.width as usize * 8, p.height as usize * 8);
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.spots.push(dac_develop::Spot {
            points: vec![dac_geom::Point::new(0.2, 0.5)],
            size: 0.01,
            source_offset: Some(dac_geom::Point::new(0.6, 0.0)),
            ..Default::default()
        });
        s.set_develop(id, d, "Spot").unwrap();
        let win = PixelWindow { x: (w as f64 * 0.2) as usize - 100, y: h / 2 - 100, w: 400, h: 300 };
        assert!(s.region_job(id, w, h, win, true).is_none());
        // a window elsewhere is fine
        assert!(s.region_job(id, w, h, PixelWindow { x: 5000, y: 5000, w: 400, h: 300 }, true).is_some());
    }

    // Issue #323: the Before side of a Before/After view at 1:1 is a window too, of the photo
    // without its edits (the crop is kept), with a key of its own
    #[test]
    fn a_before_window_shows_the_unedited_look() {
        use dac_pipeline::PixelWindow;
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let (w, h) = (p.width as usize, p.height as usize);
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.light.exposure = 2.0;
        s.set_develop(id, d, "Exposure").unwrap();
        let win = PixelWindow { x: w / 3, y: h / 3, w: 200, h: 150 };
        let after = s.region_job(id, w, h, win, true).unwrap();
        let before = s.region_job_before(id, w, h, win, true).unwrap();
        assert_ne!(after.key, before.key);
        let (a, b) = (after.run().rendered.unwrap().image, before.run().rendered.unwrap().image);
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mean = |i: &Rgba8| i.data.iter().map(|p| p[1] as f64).sum::<f64>() / i.data.len() as f64;
        assert!(mean(&a) > mean(&b) + 10.0, "two stops brighter after: {} vs {}", mean(&a), mean(&b));
    }

    #[test]
    fn has_source_follows_what_the_session_accepted() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let long = p.width.max(p.height) as usize;
        assert!(!s.media.has_source(id, SourceLevel::Full));
        let r = s.render_job(id, long, long, false, true).unwrap().run();
        s.accept(&r);
        assert!(s.media.has_source(id, SourceLevel::Full));
        assert!(!s.media.has_source(id, SourceLevel::Preview));
        assert!(!s.media.has_source(PhotoId(id.0 + 1), SourceLevel::Full));
    }

    #[test]
    fn variant_jobs_have_their_own_keys_and_hit_the_cache() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let mine = (*s.develop_of(id).unwrap()).clone();
        let with = |pid: &str| {
            let mut d = mine.clone();
            d.profile.id = pid.into();
            d
        };
        let (a, b) = (with("lc.vivid"), with("lc.bw.sepia"));
        let ja = s.variant_job(id, &a, 128).unwrap();
        let jb = s.variant_job(id, &b, 128).unwrap();
        assert_ne!(ja.key, jb.key, "keys differ per settings");
        assert_eq!(ja.key, s.variant_job(id, &a, 128).unwrap().key, "stable");
        assert_ne!(ja.key, s.variant_job(id, &a, 256).unwrap().key, "and per size");
        assert_ne!(ja.key, s.thumb_job(id, 128).unwrap().key, "not the photo's own thumbnail");
        assert_eq!(ja.level, SourceLevel::Thumb);
        let (cache, ck) = ja.cache.clone().unwrap();
        assert!(cache.get(ck).is_none());
        let r = ja.run();
        s.accept(&r);
        let first = r.rendered.unwrap().image;
        assert!(first.width.max(first.height) <= 128);
        assert!(cache.get(ck).is_some(), "rendered variants are cached");
        assert!(cache.get(jb.cache.as_ref().unwrap().1).is_none());
        // a second job for the same variant is served from the cache, pixel for pixel
        let again = s.variant_job(id, &a, 128).unwrap().run();
        assert!(again.loaded.is_none());
        assert_eq!(again.rendered.unwrap().image.as_bytes(), first.as_bytes());
        let other = jb.run().rendered.unwrap().image;
        assert_ne!(other.as_bytes(), first.as_bytes());
        // the photo's own settings never changed
        assert_eq!(*s.develop_of(id).unwrap(), mine);
        // hover previews: a loupe-sized job with other settings, its own key
        let pv = s.preview_job(id, 800, 600, true, &a).unwrap();
        assert_ne!(pv.key, s.render_job(id, 800, 600, false, true).unwrap().key);
        assert_eq!(*pv.settings, a);
    }
}
