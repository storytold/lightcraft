//! Source proxies and render jobs.
//!
//! Sources are decoded (or generated) once per level and cached in memory LRUs: `Thumb` (≤ 512 px
//! long edge) for the grid and filmstrip, `Preview` (≤ 2560 px) for the loupe, `Full` (native
//! resolution, the last one only) for exports larger than a preview. A [`RenderJob`] is
//! self-contained and `Send`: frontends run it on a worker thread; if the source wasn't cached yet
//! the job loads it and hands it back in the [`RenderResult`] so the cache can keep it.
//!
//! Rendered thumbnails are cached too ([`lightcraft_preview::PreviewCache`]: memory LRU, plus a
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

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{Quality, RenderRequest, Rendered, SourceInfo, StageCache};
use lightcraft_preview::{Hash128, Hasher128, Lru, PreviewCache};
use lightcraft_raster::{Histogram, Rgb32f, Rgba8};
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
pub const RENDER_CACHE_VERSION: u64 = 70;

/// Thumbnails render at one of these long edges (so window/cell size changes reuse the cache).
pub const THUMB_SIZES: [usize; 4] = [128, 256, 384, 512];

/// Face tiles are rendered at a multiple of this many pixels ([`crate::Session::face_job`]): the
/// tile's size follows the thumbnail slider and the screen's scale, and without steps every size
/// in between would render (and cache) every face again.
pub const FACE_EDGE_STEP: usize = 16;

/// The edge a face tile asked to be `edge` pixels across is rendered at: the next
/// [`FACE_EDGE_STEP`], 16..=512.
pub fn face_edge(edge: usize) -> usize {
    edge.clamp(16, SourceLevel::Thumb.max_edge()).div_ceil(FACE_EDGE_STEP).saturating_mul(FACE_EDGE_STEP).min(SourceLevel::Thumb.max_edge())
}

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

/// [`FileLoader`] for a thumbnail: a raw file that keeps smaller copies of its raw image (Lightroom's merged DNGs)
/// develops the smallest that covers `max_edge` (set by the app); the flag says it did. Never used for an export,
/// which must render the full image.
pub type ReducedLoader = Arc<dyn Fn(&str, usize) -> Result<(Rgb32f, SourceInfo, bool), String> + Send + Sync>;

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
    pub camera_tone: Option<lightcraft_pipeline::tone::CameraTone>,
    /// The same picture developed from the AI-denoised mosaic, when the photo has one cached.
    pub denoised: Option<Arc<Twin>>,
    /// These pixels came from the photo's smart preview (an offline original), not from its file.
    /// A refresh must never rebuild a proxy from itself: that would stamp its older look as current.
    pub from_proxy: bool,
    /// Developed from a smaller copy of the raw image that the file keeps ([`ReducedLoader`]), not from the image
    /// itself: good for a thumbnail, never for an export.
    pub reduced: bool,
}

impl DecodedSource {
    pub fn new(image: Arc<Rgb32f>, info: Option<SourceInfo>) -> Self {
        DecodedSource { image, info, camera_tone: None, denoised: None, from_proxy: false, reduced: false }
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
        if !lightcraft_denoise::product::blend(&mut mixed, &twin.image, thousandths as f32 / 1000.0) {
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
    /// with any stored camera tone curve. The original's size comes from `header` either way: the
    /// decoder only sees the level it decoded.
    pub fn info_or(&self, header: SourceInfo) -> SourceInfo {
        match &self.info {
            Some(info) => SourceInfo { native_long: info.native_long.or(header.native_long), ..info.clone() },
            None => SourceInfo { camera_tone: self.camera_tone.or(header.camera_tone), ..header },
        }
    }
}

#[derive(Clone)]
pub enum SourceRef {
    Loaded(Box<DecodedSource>),
    /// A larger decoded source of the photo, fitted to `max_edge` by the worker: the thumbnail of
    /// the photo open in the loupe is cut from its preview-level source instead of decoding the
    /// file again (a second or more for a 64 MP raw).
    Fit {
        from: Box<DecodedSource>,
        max_edge: usize,
    },
    Demo {
        scene: Box<lightcraft_scenes::Scene>,
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
        /// A thumbnail's loader, used instead of `loader` (see [`ReducedLoader`]); `None` for every other job.
        reduced: Option<ReducedLoader>,
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
            SourceRef::Fit { from, max_edge } => {
                if from.image.width.max(from.image.height) <= *max_edge {
                    return Ok((**from).clone());
                }
                // Lanczos, not a box average: a box at 2-3:1 halves the contrast of detail near
                // the output's resolution limit and aliases what is finer
                let image = lightcraft_raster::resample::fit(&from.image, *max_edge, *max_edge, lightcraft_raster::resample::Filter::Lanczos3);
                return Ok(DecodedSource { image: Arc::new(image), denoised: None, ..(**from).clone() });
            }
            SourceRef::Demo { scene, max_edge } => crate::demo::scene_pixels(scene, *max_edge)?,
            SourceRef::File { path, max_edge, loader, fallback, denoise, reduced } => {
                let plain = || match (reduced, loader) {
                    (Some(l), _) => {
                        l(path, *max_edge).map(|(image, info, reduced)| DecodedSource { reduced, ..DecodedSource::new(Arc::new(image), Some(info)) })
                    }
                    (None, Some(l)) => l(path, *max_edge).map(|(image, info)| DecodedSource::new(Arc::new(image), Some(info))),
                    (None, None) => Err(format!("no decoder available for {path}")),
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
                                    from_proxy: false,
                                    reduced: false,
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
    /// Decoded preview-level sources with their last use ([`lightcraft_preview::next_tick`]).
    previews: Vec<(PhotoId, DecodedSource, u64)>,
    /// The last full-resolution originals (exports, zoomed views; ~300 MB each at 24 MP), with
    /// their last use. Normally one; see [`Self::set_full_capacity`].
    full: Vec<(PhotoId, DecodedSource, u64)>,
    /// How many full-resolution originals to keep (LRU, at least one).
    full_capacity: usize,
    /// The photo the loupe shows ([`Self::set_on_screen`]): its original is not evicted for the budget.
    on_screen: Option<PhotoId>,
    /// How many previews to keep (LRU).
    pub preview_capacity: usize,
    /// Bytes all decoded sources and rendered previews in memory may take together (the cache
    /// share of [`crate::memory::budget`]); the least recently used entry of any of them goes first.
    budget: usize,
    pub file_loader: Option<FileLoader>,
    /// What grid thumbnails load files with (see [`ReducedLoader`]); `None`: [`Self::file_loader`].
    pub reduced_loader: Option<ReducedLoader>,
    pub file_probe: Option<FileProbe>,
    pub preview_loader: Option<PreviewLoader>,
    /// Reads a photo file's bytes (Photo Merge); `None` = the local file system.
    pub file_bytes: Option<crate::merge::ByteReader>,
    /// The library's smart previews folder (originals offline: render from the proxy).
    pub smart_dir: Option<std::path::PathBuf>,
    /// Whether originals (and smart previews) are on disk: cached and checked off the UI thread
    /// in the app (see [`crate::availability`]).
    pub availability: crate::availability::Availability,
    scenes: Vec<lightcraft_scenes::Scene>,
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
            full: Vec::new(),
            full_capacity: 1,
            on_screen: None,
            preview_capacity: 0,
            budget,
            file_loader: None,
            reduced_loader: None,
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
        self.full.clear();
    }

    /// How many full-resolution originals to keep at once (at least one). Compare and Reference
    /// views zoomed to 1:1 cut windows from two photos' originals: with room for one, every window
    /// render of one side would evict the other side's original and decode its own again. Lowering
    /// it drops the least recently used originals at once.
    pub fn set_full_capacity(&mut self, n: usize) {
        self.full_capacity = n.max(1);
        self.trim_full();
    }

    /// The photo the loupe shows (the frontend sets it every frame; `None` in other views). Its original stays
    /// even over budget: a 64 MP original (797 MB as linear float) alone exceeds the default share, and evicting it
    /// made every pan of a zoomed view decode the raw again. Showing another photo lets it go at once.
    pub fn set_on_screen(&mut self, id: Option<PhotoId>) {
        if self.on_screen != id {
            self.on_screen = id;
            self.enforce_budget();
        }
    }

    fn trim_full(&mut self) {
        while self.full.len() > self.full_capacity {
            let oldest = self.full.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i).unwrap_or(0);
            self.full.remove(oldest);
        }
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
    /// one just decoded) is never evicted here, nor is the original of the photo on screen
    /// ([`Self::set_on_screen`]).
    pub fn enforce_budget(&mut self) {
        // The on-screen original is never evicted here, so it does not count either: counted, a 64 MP original (760 MiB)
        // nearly fills the default share (768 MiB) alone and everything else went instead — the photo's preview and
        // thumbnail sources and the neighbours' were decoded again from the raw on every edit, zoom-out and step.
        let pinned: usize = self.full.iter().filter(|e| Some(e.0) == self.on_screen).map(|e| e.1.bytes()).sum();
        let mut held = self.held().saturating_sub(pinned);
        while held > self.budget {
            let newest_preview = self.previews.iter().map(|e| e.2).max();
            let candidates = [
                self.thumbs.oldest_tick().map(|t| (t, 0)),
                self.previews.iter().filter(|e| Some(e.2) != newest_preview).map(|e| e.2).min().map(|t| (t, 1)),
                self.full.iter().filter(|e| Some(e.0) != self.on_screen).map(|e| e.2).min().map(|t| (t, 2)),
                self.rendered.oldest_tick().map(|t| (t, 3)),
            ];
            let Some((tick, which)) = candidates.into_iter().flatten().min() else { break };
            let freed = match which {
                0 => self.thumbs.pop_oldest(),
                1 => self.previews.iter().position(|e| e.2 == tick).map(|i| self.previews.remove(i).1.bytes()),
                2 => self.full.iter().position(|e| e.2 == tick).map(|i| self.full.remove(i).1.bytes()),
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
                e.2 = lightcraft_preview::next_tick();
                e.1.clone()
            }),
            SourceLevel::Full => self.full.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = lightcraft_preview::next_tick();
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
        let tick = lightcraft_preview::next_tick();
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
            SourceLevel::Full => {
                self.full.retain(|e| e.0 != id);
                self.full.push((id, img, tick));
                self.trim_full();
            }
        }
        self.enforce_budget();
    }

    /// Forget a photo's decoded sources (e.g. after its file changed).
    pub fn forget(&mut self, id: PhotoId) {
        self.thumbs.remove(&id);
        self.previews.retain(|e| e.0 != id);
        self.full.retain(|e| e.0 != id);
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
            SourceLevel::Full => self.full.iter().any(|e| e.0 == id),
        }
    }

    /// Decoded sources held: (thumbnail level, preview level, full size).
    pub fn usage(&self) -> (crate::memory::Usage, crate::memory::Usage, crate::memory::Usage) {
        use crate::memory::Usage;
        let previews = Usage::new(self.previews.len(), self.previews.iter().map(|e| e.1.bytes()).sum());
        let full = Usage::new(self.full.len(), self.full.iter().map(|e| e.1.bytes()).sum());
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
        // a thumbnail of a photo whose preview is decoded (the one in the loupe) is cut from it; not
        // with a denoised picture, which the thumbnail would lose
        if cached
            && level == SourceLevel::Thumb
            && self.denoise.spec(p.id).is_none()
            && let Some(big) = self.get_source(p.id, SourceLevel::Preview)
        {
            return SourceRef::Fit { from: Box::new(big), max_edge: level.max_edge() };
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
            let sharded = crate::smart::sharded_file_path(dir, p);
            let legacy = dir.join(crate::smart::file_name(p));
            let sp = if self.availability.exists(&sharded.to_string_lossy()) == Some(true) { sharded } else { legacy };
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
                    self.scenes = lightcraft_scenes::demo_library();
                }
                match self.scenes.iter().find(|s| s.id == *scene) {
                    Some(s) => SourceRef::Demo { scene: Box::new(s.clone()), max_edge },
                    None => SourceRef::File { path: format!("demo:{scene}"), max_edge, loader: None, fallback: None, denoise: None, reduced: None },
                }
            }
            Source::File { path } => {
                SourceRef::File { path: path.clone(), max_edge, loader: self.file_loader.clone(), fallback: None, denoise: None, reduced: None }
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
    /// Render for this monitor profile ([`Self::with_display`]).
    pub display: Option<Arc<crate::display::Display>>,
    /// Compose the render into a slideshow slide ([`Self::with_slide`]).
    pub slide: Option<Arc<crate::slide::SlideCompose>>,
    /// A face tile that can be cut from the raw's embedded preview, tried before `source`
    /// ([`crate::Session::face_job`]; `None` for every other job).
    pub preview_cut: Option<crate::face_tiles::PreviewCut>,
    /// Hand the result over as a texture on the window's device when the GPU can
    /// ([`Self::to_texture`]).
    pub texture: bool,
    /// Shrink a full-quality result to fit this size (physical pixels) with a Lanczos filter once
    /// it is rendered. The loupe renders a little larger than it is drawn (its edge is rounded up
    /// to a 64 px step) and the window would otherwise shrink the texture with 2-tap bilinear sampling, which blurs fine detail at
    /// small ratios and aliases it at large ones. The caches keep the full render.
    pub display_fit: Option<(usize, usize)>,
}

/// `img` shrunk to fit `max_w × max_h` with a Lanczos filter (see [`RenderJob::display_fit`]).
pub fn fit_for_display(img: &Rgba8, max_w: usize, max_h: usize) -> Rgba8 {
    use lightcraft_raster::resample::{Filter, fit};
    let f = lightcraft_raster::Image {
        width: img.width,
        height: img.height,
        data: img.data.iter().map(|p| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2]), f32::from(p[3])]).collect(),
    };
    let small = fit(&f, max_w.max(1), max_h.max(1), Filter::Lanczos3);
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    Rgba8 { width: small.width, height: small.height, data: small.data.iter().map(|p| [c(p[0]), c(p[1]), c(p[2]), c(p[3])]).collect() }
}

pub struct RenderResult {
    pub request_id: u64,
    pub source_key: Option<Hash128>,
    pub photo: PhotoId,
    pub level: SourceLevel,
    pub key: u64,
    pub rendered: Result<Rendered, String>,
    /// Set when this job's render was tried on the GPU and fell back to the CPU: why
    /// ([`lightcraft_gpu::Fallback::Failed`]). The result is the CPU render either way.
    pub gpu_fallback: Option<String>,
    /// A source that was loaded by this job (to be inserted into the cache).
    pub loaded: Option<DecodedSource>,
    /// Set for a [`QuickJob`]'s stand-in: where the image came from.
    pub quick: Option<QuickSource>,
    /// The display profile (id) the image is encoded for; `None`: sRGB (see
    /// [`crate::display::present`]).
    pub display: Option<u64>,
    /// The result as a texture on the window's device ([`RenderJob::to_texture`]); `rendered` is
    /// then an empty image with an empty histogram.
    pub texture: Option<lightcraft_gpu::GpuTexture>,
}

impl RenderJob {
    /// Render at draft quality (interactive drags). Gets its own result key.
    /// Shrink the full-quality result to fit `w × h` pixels with a Lanczos filter (see
    /// [`Self::display_fit`]). Gets its own result key, so a change of drawn size renders again.
    pub fn with_display_fit(mut self, w: usize, h: usize) -> Self {
        self.display_fit = Some((w, h));
        self.key ^= ((w as u64) << 32 | h as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self
    }

    pub fn draft(mut self) -> Self {
        if self.request.quality != Quality::Draft {
            self.request.quality = Quality::Draft;
            self.key ^= 0x9e37_79b9_7f4a_7c15;
        }
        self
    }

    /// Draw a diagnostic overlay over the result (e.g. Point Color's visualized range). Gets its
    /// own result key.
    pub fn with_overlay(mut self, overlay: lightcraft_pipeline::Overlay) -> Self {
        if self.request.overlay != overlay {
            self.key ^= self.request.overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
            self.request.overlay = overlay;
            self.key ^= overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        // a diagnostic view (visualized range / spots) must never become the photo's cached preview
        if overlay != lightcraft_pipeline::Overlay::None {
            self.view_cache = None;
        }
        self
    }

    /// Soft-proof the result (see [`lightcraft_pipeline::Proof`]). Gets its own result key and is
    /// never kept as the photo's cached preview.
    pub fn with_proof(mut self, proof: Option<lightcraft_pipeline::Proof>) -> Self {
        if self.request.proof != proof {
            let k = |p: Option<lightcraft_pipeline::Proof>| p.map_or(0, |p| p.key()).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
            self.key ^= k(self.request.proof) ^ k(proof);
            self.request.proof = proof;
        }
        if proof.is_some() {
            self.view_cache = None;
        }
        self
    }

    /// Render with some colour grading wheels (shadows, midtones, highlights) switched off for
    /// viewing (the wheel eyes in the Edit panel). The photo's settings are untouched; the result
    /// gets its own key and is never cached as the photo's preview or thumbnail.
    pub fn with_hidden_grading(mut self, hidden: [bool; 3]) -> Self {
        if !hidden.iter().any(|h| *h) {
            return self;
        }
        let mut s = (*self.settings).clone();
        let g = &mut s.grading;
        for (off, wheel) in hidden.into_iter().zip([&mut g.shadows, &mut g.midtones, &mut g.highlights]) {
            if off {
                *wheel = lightcraft_develop::Wheel::default();
            }
        }
        let bits = hidden.iter().enumerate().fold(0u64, |b, (i, h)| b | ((*h as u64) << i));
        self.key ^= s.hash64() ^ self.settings.hash64() ^ bits.wrapping_mul(0xd6e8_feb8_6659_fd93);
        self.settings = Arc::new(s);
        self.cache = None;
        self.view_cache = None;
        self.preview_cut = None;
        self
    }

    /// Hand the result over as a texture on the window's device, with no copy of its pixels back
    /// to the CPU (issue #1228), when the GPU renders it and nothing needs the pixels: a view
    /// that only draws them (the loupe's zoom window), with no overlay, nothing cached and no
    /// display correction left to apply on the CPU. Otherwise the job renders as before. The
    /// result is the same image, so the key stays.
    pub fn to_texture(mut self, on: bool) -> Self {
        self.texture = on;
        self
    }

    /// Whether this job can render into a texture ([`Self::to_texture`]) at all.
    fn texture_wanted(&self) -> bool {
        self.texture
            && self.cache.is_none()
            && self.slide.is_none()
            && self.view_cache.is_none()
            && self.display_fit.is_none()
            && self.display.as_ref().is_none_or(|d| !d.profile.needs_correction() || d.profile.correction_curves().is_some())
            && gpu_takes(&self.settings, &self.request)
            && lightcraft_gpu::texture_handles(&self.request)
    }

    /// Reuse `stages` across this view's renders.
    pub fn with_stages(mut self, stages: Arc<StageCache>) -> Self {
        self.stages = Some(stages);
        self
    }

    /// Render for a monitor profile: into the display's primaries (its gamut, not sRGB's), ready
    /// to show ([`RenderResult::display`]). The histogram and the cached view preview stay sRGB.
    /// Gets its own result key. Thumbnail jobs (rendered-thumbnail cache) are left as they are:
    /// [`crate::display::present`] converts their sRGB results.
    pub fn with_display(mut self, display: Option<Arc<crate::display::Display>>) -> Self {
        // (a slide is composed in sRGB, backdrop colours included; the frontend presents it)
        if self.cache.is_some() || self.slide.is_some() || self.request.depth != lightcraft_pipeline::OutputDepth::U8 {
            return self;
        }
        let k = |d: &Option<Arc<crate::display::Display>>| d.as_ref().map_or(0, |d| d.id()).wrapping_mul(0xd6e8_feb8_6659_fd93);
        self.key ^= k(&self.display) ^ k(&display);
        self.request.display = display.as_ref().map(|d| d.space);
        self.display = display;
        self
    }

    /// Render. A panic inside (a decoder or pipeline bug) is this job's failure: the view waiting
    /// for it is told, instead of waiting forever on a worker that died (the pool doesn't replace
    /// it, so a few such panics used to stop every render).
    pub fn run(self) -> RenderResult {
        let failed = self.failed(String::new());
        crate::guard::catch("the render", || self.run_unguarded()).unwrap_or_else(|e| RenderResult { rendered: Err(e), ..failed })
    }

    /// This job's result when it fails with `e`.
    fn failed(&self, e: String) -> RenderResult {
        RenderResult {
            request_id: self.request_id,
            source_key: self.source_key,
            photo: self.photo,
            level: self.level,
            key: self.key,
            rendered: Err(e),
            gpu_fallback: None,
            loaded: None,
            quick: None,
            display: None,
            texture: None,
        }
    }

    /// Compose the render into a slideshow slide (layout, options, backdrop): the result is the
    /// slide, not the photo. Gets its own result key; the photo's cached view preview stays the
    /// photo. The slide stays sRGB ([`Self::with_display`] leaves it alone).
    pub fn with_slide(mut self, slide: Arc<crate::slide::SlideCompose>) -> Self {
        let k = |s: &Option<Arc<crate::slide::SlideCompose>>| s.as_ref().map_or(0, |s| s.key()).wrapping_mul(0xa24b_aed4_963e_e407);
        self.key ^= k(&self.slide);
        self.key ^= k(&Some(slide.clone()));
        self.slide = Some(slide);
        self
    }

    /// [`Self::run_photo`], composed into its slide when this is a slide job (inside [`Self::run`]'s guard).
    fn run_unguarded(self) -> RenderResult {
        let slide = self.slide.clone();
        let mut r = self.run_photo();
        if let (Some(slide), Ok(rendered)) = (slide, r.rendered.as_mut()) {
            rendered.image = slide.compose(&rendered.image);
            rendered.deep = None;
        }
        r
    }

    /// The photo's render (without slide composition).
    fn run_photo(self) -> RenderResult {
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
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                texture: None,
            };
        }
        // a face tile of an unedited raw: cut from the camera's embedded preview when it is big enough
        // (the sensor data is never read for it), else rendered from `source` below
        if let Some(image) = self.preview_cut.as_ref().and_then(|c| c.cut()) {
            if let Some((cache, key)) = &self.cache {
                cache.put_at(self.cache_generation, *key, Arc::new(image.clone()));
            }
            let histogram = Histogram::of_srgb8(&image);
            return RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                texture: None,
            };
        }
        let was_loaded = matches!(self.source, SourceRef::Loaded(_));
        let want_texture = self.texture_wanted();
        // The app is closing (issue #620): nobody is left to show this, and quitting waits for the
        // jobs that are running. Without this, one that lost the GPU mid-way would render on the CPU.
        let closing = || lightcraft_gpu::shutting_down().then(|| "LightCraft is closing".to_string());
        let source = match closing() {
            Some(e) => Err(e),
            None => self.source.load_source().and_then(|s| closing().map_or(Ok(s), Err)),
        };
        match source {
            Ok(source) => {
                // the plain picture, or (Denoise amount above 0 and a denoised picture cached) the mix the amount asks for
                let src = &source.image_for(self.settings.denoise_amount());
                let info = source.info_or(self.info);
                // Thumbnails (many small jobs side by side) stay on the CPU; views and exports use
                // the GPU when there is one.
                let gpu = self.cache.is_none();
                // a view that only draws the result: a texture on the window's device, no readback
                let mut texture_fallback = None;
                if want_texture {
                    // the display's curves, when they aren't sRGB's: applied on the GPU too
                    let curves = self.display.as_ref().and_then(|d| d.profile.correction_curves());
                    match lightcraft_gpu::try_render_texture(src, &info, &self.settings, &self.request, self.stages.as_deref(), curves) {
                        Ok(t) => {
                            let empty = Rgba8 { width: 0, height: 0, data: Vec::new() };
                            let histogram = Histogram::of_srgb8(&empty);
                            return RenderResult {
                                request_id: self.request_id,
                                source_key: self.source_key,
                                photo: self.photo,
                                level: self.level,
                                key: self.key,
                                rendered: Ok(Rendered { image: empty, histogram, deep: None }),
                                gpu_fallback: None,
                                loaded: (!was_loaded).then_some(source),
                                quick: None,
                                // rendered into the display's primaries, and its curve is sRGB's
                                display: self.display.as_ref().filter(|_| self.request.display.is_some()).map(|d| d.id()),
                                texture: Some(t),
                            };
                        }
                        Err(lightcraft_gpu::Fallback::Failed(why)) => texture_fallback = Some(why),
                        Err(lightcraft_gpu::Fallback::NotTried) => {}
                    }
                }
                let (mut rendered, gpu_fallback) = develop_reporting(src, &info, &self.settings, &self.request, self.stages.as_deref(), gpu);
                let gpu_fallback = texture_fallback.or(gpu_fallback);
                // for a display profile: the image goes to the display's values, the histogram
                // and the cached view preview stay sRGB
                let mut srgb = None;
                let display = match (&self.display, self.request.display) {
                    (Some(d), Some(_)) => {
                        let keep = self.view_cache.is_some() && self.request.quality == Quality::Full;
                        let (s, h) = crate::display::finish_render(&mut rendered.image, d, keep);
                        // (an HDR edit's HDR histogram stays)
                        if let Some(h) = h.filter(|_| rendered.histogram.hdr.is_none()) {
                            rendered.histogram = h;
                        }
                        srgb = s;
                        Some(d.id())
                    }
                    _ => None,
                };
                if let Some((cache, key)) = &self.cache {
                    cache.put_at(self.cache_generation, *key, Arc::new(rendered.image.clone()));
                }
                if let Some((cache, key)) = &self.view_cache
                    && self.request.quality == Quality::Full
                    && (display.is_none() || srgb.is_some())
                {
                    cache.put_deferred_at(self.cache_generation, *key, Arc::new(srgb.unwrap_or_else(|| rendered.image.clone())));
                }
                // the display fit, after the caches have the full render
                if let Some((fw, fh)) = self.display_fit
                    && self.request.quality == Quality::Full
                {
                    let (w, h) = (rendered.image.width, rendered.image.height);
                    if w > fw.max(1) || h > fh.max(1) {
                        rendered.image = fit_for_display(&rendered.image, fw, fh);
                    }
                }
                RenderResult {
                    request_id: self.request_id,
                    source_key: self.source_key,
                    photo: self.photo,
                    level: self.level,
                    key: self.key,
                    rendered: Ok(rendered),
                    gpu_fallback,
                    loaded: (!was_loaded).then_some(source),
                    quick: None,
                    display,
                    texture: None,
                }
            }
            Err(e) => self.failed(e),
        }
    }
}

/// Render `src`: on the GPU when `gpu` is set and a GPU is available (`lightcraft_gpu`), else on
/// the CPU, reusing `stages` either way. Both produce the same image within 1–3 LSB (see
/// `docs/gpu-pipeline.md`); `LIGHTCRAFT_GPU=0` or [`lightcraft_gpu::set_enabled`] forces the CPU.
pub fn develop(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>, gpu: bool) -> Rendered {
    develop_reporting(src, info, s, req, stages, gpu).0
}

/// [`develop`], also saying why the render fell back from the GPU to the CPU when it did (`None`:
/// it rendered on the GPU, or the GPU was not tried). The reason is this render's own, not the
/// process-wide [`lightcraft_gpu::last_fallback`].
pub fn develop_reporting(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
) -> (Rendered, Option<String>) {
    let (mut rendered, fallback) = develop_image(src, info, s, req, stages, gpu);
    // an HDR edit's preview is its SDR rendition: its histogram shows the HDR range instead
    if let Some(h) = lightcraft_pipeline::hdr_histogram(src, info, s, req) {
        rendered.histogram = h;
    }
    (rendered, fallback)
}

/// Whether a view's render is tried on the GPU at all: LUT profiles and refined masks have no GPU
/// stage, and soft proofs render on the CPU.
fn gpu_takes(s: &DevelopSettings, req: &RenderRequest) -> bool {
    !lightcraft_pipeline::lut::is_lut_profile(&s.profile.id) && !s.masks.iter().any(|m| m.visible && m.refine > 0.0) && req.proof.is_none()
}

fn develop_image(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
) -> (Rendered, Option<String>) {
    let fallback = if gpu && gpu_takes(s, req) {
        match lightcraft_gpu::try_render(src, info, s, req, stages) {
            Ok(r) => return (r, None),
            Err(lightcraft_gpu::Fallback::Failed(why)) => Some(why),
            Err(lightcraft_gpu::Fallback::NotTried) => None,
        }
    } else {
        None
    };
    let rendered = match stages {
        Some(st) => lightcraft_pipeline::render_cached(src, info, s, req, st),
        None => lightcraft_pipeline::render(src, info, s, req),
    };
    (rendered, fallback)
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
    /// The stand-in, from the first source that has one. A panic inside is this job's failure
    /// (see [`RenderJob::run`]).
    pub fn run(self) -> RenderResult {
        let (request_id, photo, key) = (self.request_id, self.photo, self.key);
        crate::guard::catch("the render", || self.run_unguarded()).unwrap_or_else(|e| RenderResult {
            request_id,
            source_key: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered: Err(e),
            gpu_fallback: None,
            loaded: None,
            quick: None,
            display: None,
            texture: None,
        })
    }

    fn run_unguarded(self) -> RenderResult {
        let (photo, key) = (self.photo, self.key);
        let request_id = self.request_id;
        let done = |rendered: Result<Rendered, String>, quick| RenderResult {
            request_id,
            source_key: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered,
            gpu_fallback: None,
            loaded: None,
            quick,
            display: None,
            texture: None,
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
    // (They are drawn afresh at every size, so they have no original size.)
    if matches!(p.source, Source::Demo { .. }) {
        return SourceInfo { raw: true, ..Default::default() };
    }
    // the thumbnail and preview levels are smaller copies: noise reduction works in the original's pixels (#960)
    let native_long = Some(p.width.max(p.height) as usize).filter(|&n| n > 0);
    SourceInfo { native_long, ..header_info(p) }
}

fn header_info(p: &Photo) -> SourceInfo {
    // A raw shown from its embedded preview is a rendered (display-referred) JPEG: relative white
    // balance and the display tone curve, like any other rendered file.
    if p.develops_raw() {
        let relative = p.relative_wb();
        let (temp, tint) = if relative { (6500.0, 0.0) } else { p.as_shot_wb.unwrap_or((5500.0, 0.0)) };
        SourceInfo {
            raw: true,
            as_shot_temp: temp,
            as_shot_tint: tint,
            lens: p.embedded_lens,
            relative_wb: relative,
            // (a fitted-look raw edited in Kelvin: its camera's matrices give the scale, see `files::WbScaleOf`)
            legacy_relative_wb: p.fitted_look() && !relative,
            ..Default::default()
        }
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
        let mut job = self.build_job(id, b, b, false, true, Some(b))?;
        // (a thumbnail may develop a smaller copy of the raw image that the file keeps)
        if let SourceRef::File { reduced, .. } = &mut job.source {
            reduced.clone_from(&self.media.reduced_loader);
        }
        Some(job)
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
            display: None,
            slide: None,
            preview_cut: None,
            texture: false,
            display_fit: None,
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
            display: None,
            slide: None,
            preview_cut: None,
            texture: false,
            display_fit: None,
        })
    }

    /// A square close-up of `face` (normalized, in the photo's upright frame) for a People card: the
    /// photo's own look with its crop replaced by a square around the face (room for hair and chin),
    /// at least `edge` pixels across (rounded up to [`FACE_EDGE_STEP`], ≤ 512, so a tile size
    /// between two steps reuses the cached one). An unedited raw's tile is cut from the camera's
    /// embedded preview when that holds the square at [`crate::face_tiles::SOFT_LIMIT`] of the
    /// size asked or more ([`crate::face_tiles`]); else, and for every other photo, the source
    /// level is the smallest that keeps the face sharp, up to the preview. Cached (memory and the
    /// library's thumbnail cache on disk) like [`Self::variant_job`], and `key` likewise follows
    /// the content: a later launch shows the tile without reading the original.
    pub fn face_job(&mut self, id: PhotoId, face: lightcraft_geom::Rect, edge: usize) -> Option<RenderJob> {
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
        let rect = lightcraft_geom::Rect { x0: x0 / w, y0: y0 / h, x1: (x0 + side) / w, y1: (y0 + side) / h };
        let mut settings = (*p.develop).clone();
        settings.crop = lightcraft_develop::Crop { geometry: lightcraft_geom::CropGeometry { rect, angle: 0.0 }, ..Default::default() };
        let edge = face_edge(edge);
        // pixels along the long edge that leave `edge` across the crop
        let needed = (edge as f64 * w.max(h) / side).ceil().min(SourceLevel::Preview.max_edge() as f64);
        let level = SourceLevel::for_size(needed as usize);
        let source = self.media.source_ref(&p, level);
        let ck = Self::variant_key(&p, &settings, edge);
        let preview_cut = self.face_preview_cut(&p, rect, edge, needed as usize);
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
            display: None,
            slide: None,
            preview_cut,
            texture: false,
            display_fit: None,
        })
    }

    /// How a face tile of `p` can come from its embedded preview ([`crate::face_tiles`]): only an
    /// unedited raw (whose grid thumbnail is that preview too) not known to be offline, upright as
    /// imported, with a preview loader installed. `rect` is the square around the face, `edge` the
    /// tile, `needed` the source's long edge the develop render would ask for.
    fn face_preview_cut(&self, p: &Photo, rect: lightcraft_geom::Rect, edge: usize, needed: usize) -> Option<crate::face_tiles::PreviewCut> {
        let (path, loader) = self.embedded_of(p)?;
        if p.develop.orientation != lightcraft_geom::Orientation::Normal || self.media.availability.is_offline(&path) || p.width == 0 || p.height == 0
        {
            return None;
        }
        let want = needed.max(edge).max(1);
        let min = (want as f64 * crate::face_tiles::SOFT_LIMIT).ceil() as usize;
        let aspect = f64::from(p.width) / f64::from(p.height);
        Some(crate::face_tiles::PreviewCut { path, loader, want, min, rect, aspect, edge })
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
        window: lightcraft_pipeline::PixelWindow,
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
        window: lightcraft_pipeline::PixelWindow,
        apply_crop: bool,
    ) -> Option<RenderJob> {
        self.region_job_of(id, full_w, full_h, window, apply_crop, true)
    }

    fn region_job_of(
        &mut self,
        id: PhotoId,
        full_w: usize,
        full_h: usize,
        window: lightcraft_pipeline::PixelWindow,
        apply_crop: bool,
        before: bool,
    ) -> Option<RenderJob> {
        let mut job = self.render_job(id, full_w, full_h, before, apply_crop)?;
        // what the window's spots and Auto Mask strokes read must fit in one render: else the
        // caller keeps the whole-frame render (a window alone would come out wrong)
        let p = self.catalog.photo(id)?;
        let frame = lightcraft_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &job.settings,
            apply_crop,
            p.embedded_lens.as_ref(),
        );
        let ppl = frame.px_per_long(full_w);
        lightcraft_pipeline::spots::window_for_reads_checked(&job.settings, &frame, full_w, full_h, ppl, window.clamped(full_w, full_h))?;
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

    /// Whether `p`'s thumbnail can be the camera's own JPEG, so a frontend can skip the sensor
    /// decode behind it.
    ///
    /// The grid shows this JPEG first in any case ([`QuickJob`]: "grid thumbnails of unedited raws
    /// likewise start from the embedded preview"), and then paid for a render of the raw to replace
    /// it. That render decodes the whole sensor — 50 of a 56 MB Fuji RAF's bytes — for a 512 px
    /// thumbnail the 4 416 × 2 944 embedded JPEG already covers, and on a share at 1.2 MB/s it is
    /// 45 s a photo: a 149-photo folder spent 8.3 GB of traffic on thumbnails, and the queue behind
    /// it delayed the render the user was waiting for.
    ///
    /// Only unedited raws answer `true`: any edit must be rendered, and `has_import_look` is what
    /// says the photo still looks the way it did at import. A photo the user edits re-renders
    /// through the ordinary path, because its thumbnail key (settings hash) changes.
    pub fn embedded_thumb_is_final(&self, p: &Photo) -> bool {
        self.embedded_of(p).is_some()
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
        let cached = vec![(self.media.rendered.clone(), Self::view_key(&p, apply_crop))];
        let h = Hasher128::new().str(&content_key(&p)).str("quick").u64(p.develop.hash64()).u64(apply_crop as u64).finish();
        let key = h.0 as u64;
        let embedded = self.embedded_of(&p).map(|(path, l)| (path, l, max_edge.clamp(1, SourceLevel::Preview.max_edge())));
        Some(QuickJob { request_id: 0, photo: id, key, cached, embedded, small: apply_crop.then(|| Box::new(small)) })
    }

    /// A first grid thumbnail for an unedited raw: the cached render for `job` (the real
    /// thumbnail job, from [`Self::thumb_job`]) if there is one — then the result carries
    /// [`QuickSource::Cached`] and `job.key` and is final — else its embedded preview, else `job`
    /// itself: the grid queues no render behind an unedited raw's quick thumbnail
    /// ([`Self::embedded_thumb_is_final`]), so a preview that can't be read must not leave the
    /// cell empty (issue #1448). `None` for other photos (render them directly).
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
            small: Some(Box::new(job.clone())),
        })
    }

    /// Accept a finished job's loaded source into the cache. What the decode learnt about the file that the catalog
    /// has wrong is corrected too ([`Self::learn_as_shot_wb`]).
    pub fn accept(&mut self, r: &RenderResult) {
        if let Some(src) = &r.loaded
            && r.source_key.is_none_or(|key| self.catalog.photo(r.photo).is_some_and(|p| Hasher128::new().str(&content_key(p)).finish() == key))
        {
            if let Some(info) = &src.info {
                self.learn_as_shot_wb(r.photo, info);
            }
            self.media.insert_source(r.photo, r.level, src.clone());
        }
    }

    /// Record the as-shot white balance a decode of photo `id`'s file read (`info`) when the catalog has another:
    /// a raw imported before its camera's matrices gave it a Kelvin scale keeps 6500 / 0 (the relative scale's
    /// reference) until its file is next decoded (issue #730). Bookkeeping, not an undo step; the look does not
    /// change (the pipeline reads the stored settings against the real as-shot white either way), only what As Shot
    /// says and what a smart preview renders with. Only for raws edited in Kelvin.
    fn learn_as_shot_wb(&mut self, id: PhotoId, info: &SourceInfo) {
        let Some(p) = self.catalog.photo(id) else { return };
        if !info.raw || info.relative_wb || !p.develops_raw() || p.relative_wb() {
            return;
        }
        let read = (info.as_shot_temp, info.as_shot_tint);
        if !(read.0.is_finite() && read.1.is_finite()) || p.as_shot_wb == Some(read) {
            return;
        }
        if let Err(e) = self.apply_system(Op::SetAsShotWb { id, wb: Some(read) }) {
            log::warn!("photo {}: as-shot white balance not recorded: {e}", id.0);
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
        space: lightcraft_pipeline::OutputSpace,
        depth: lightcraft_pipeline::OutputDepth,
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
        space: lightcraft_pipeline::OutputSpace,
        depth: lightcraft_pipeline::OutputDepth,
    ) -> Result<RenderJob, String> {
        let mut job = self.render_job(id, max_w, max_h, false, true).ok_or("no such photo")?;
        // An export renders the full image: never a thumbnail's source developed from a smaller copy of the raw
        // image that the file keeps (Lightroom's merged DNGs)
        let reduced = match &job.source {
            SourceRef::Loaded(s) => s.reduced,
            SourceRef::Fit { from, .. } => from.reduced,
            _ => false,
        };
        if reduced {
            let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
            job.source = self.media.source_ref_uncached(&p, job.level);
        }
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
        if let Some(info) = &r.info {
            self.learn_as_shot_wb(id, info);
        }
        self.media.insert_source(id, level, r);
        Ok(image)
    }
}

/// What an import learns from a file header (set by the app from `lightcraft-codecs`/`-raw`).
#[derive(Clone, Debug, Default)]
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub kind: MediaKind,
    pub file_size: u64,
    pub captured: Option<String>,
    /// The file's modification time (ISO 8601, the clock of [`crate::import::system_clock`]): the
    /// capture date of a file whose metadata has none, as Lightroom does (exports saved without
    /// metadata would otherwise all land under "Unknown Date").
    pub file_modified: Option<String>,
    pub meta: lightcraft_catalog::Meta,
    pub as_shot_wb: Option<(f64, f64)>,
    /// Hash of the file's bytes (hex), for duplicate detection. When it is a 32-digit
    /// [`lightcraft_preview::hash_bytes`] of the whole file (as the native probe computes it),
    /// Import → Copy verifies each copy against it instead of reading the source again.
    pub content_hash: Option<String>,
    /// Lens corrections embedded in the file (DNG opcodes).
    pub embedded_lens: Option<lightcraft_develop::EmbeddedLens>,
    /// The file's embedded XMP packet (raw/DNG files), for develop settings stored inside the file.
    pub xmp: Option<String>,
    /// A raw variant that can't be decoded yet: why. The file is described (and will be shown and
    /// edited) from its embedded preview; see [`lightcraft_catalog::Photo::preview_only`].
    pub preview_only: Option<String>,
}

pub type FileProbe = Arc<dyn Fn(&str) -> Result<ProbeInfo, String> + Send + Sync>;

#[cfg(test)]
mod thumbnail_hash_tests {
    use super::*;

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
                preview_checked: String::new(),
            })
            .unwrap();
        s.media.forget(PhotoId(1));
        s.accept(&old);
        assert_eq!(s.media.source_usage().0, 0);
    }
    /// A panic while rendering (here in the decoder) used to end the pool's worker thread without
    /// an answer, so the view that asked showed "Rendering…" forever. It is the job's failure now,
    /// for renders and for stand-ins, answering the request it came from.
    #[test]
    fn a_render_that_panics_fails_instead_of_never_answering() {
        let mut s = session();
        s.media.file_loader = Some(Arc::new(|_, _| panic!("decoder bug")));
        let mut job = s.thumb_job(PhotoId(1), 256).unwrap();
        job.request_id = 42;
        let (key, level, source_key) = (job.key, job.level, job.source_key);
        let r = job.run();
        assert_eq!((r.request_id, r.photo, r.key, r.level, r.source_key), (42, PhotoId(1), key, level, source_key));
        assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: decoder bug"));
        let quick = QuickJob {
            request_id: 7,
            photo: PhotoId(1),
            key: 9,
            cached: Vec::new(),
            embedded: Some(("x.arw".into(), Arc::new(|_: &str, _: usize| -> Option<Rgba8> { panic!("preview reader bug") }), 256)),
            small: None,
        };
        let r = quick.run();
        assert_eq!((r.request_id, r.key), (7, 9));
        assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loupe job with a display fit returns its image shrunk to the drawn size, aspect kept;
    /// a draft, and a render no larger than the drawn size, are left as rendered.
    #[test]
    fn a_display_fit_shrinks_the_loupe_render_to_the_drawn_size() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let plain = s.loupe_job(id, 1024, 1024, true).unwrap().run().rendered.unwrap().image;
        let long = plain.width.max(plain.height);
        assert!(long >= 600, "the demo renders at least 600 px at 1024: {long}");
        let fitted = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400).run().rendered.unwrap().image;
        assert_eq!(fitted.width.max(fitted.height), 400, "{}x{}", fitted.width, fitted.height);
        let aspect = |i: &Rgba8| i.width as f32 / i.height as f32;
        assert!((aspect(&fitted) - aspect(&plain)).abs() < 0.02, "aspect kept");
        let draft = s.loupe_job(id, 1024, 1024, true).unwrap().draft().with_display_fit(400, 400).run().rendered.unwrap().image;
        assert_eq!(draft.width.max(draft.height), long, "a draft is not shrunk");
        let same = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(plain.width, plain.height).run().rendered.unwrap().image;
        assert_eq!((same.width, same.height), (plain.width, plain.height), "no larger than drawn: left as rendered");
        let a = s.loupe_job(id, 1024, 1024, true).unwrap();
        let b = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400);
        assert_ne!(a.key, b.key, "the fit is part of the result key");
    }

    #[test]
    fn renders_know_the_original_size_whichever_level_they_come_from() {
        // issue #960: noise reduction works in the original's pixels, so a grid cell drawn from the
        // 512-px thumbnail level denoises like a scaled-down export
        let p = Photo::new(PhotoId(1), Source::File { path: "/photos/a.ARW".into() }, "a.ARW", "ARW", 6000, 4000, "");
        assert_eq!(source_info(&p).native_long, Some(6000));
        let decoded = SourceInfo { raw: true, as_shot_temp: 4800.0, ..Default::default() };
        let thumb = DecodedSource::new(Arc::new(Rgb32f::new(8, 8)), Some(decoded.clone()));
        let info = thumb.info_or(source_info(&p));
        assert_eq!(info, SourceInfo { native_long: Some(6000), ..decoded }, "the decoder's facts, the catalog's size");
        let unknown = Photo::new(PhotoId(2), Source::File { path: "/photos/b.jpg".into() }, "b.jpg", "JPEG", 0, 0, "");
        assert_eq!(source_info(&unknown).native_long, None);
        assert_eq!(source_info(&Photo::new(PhotoId(3), Source::Demo { scene: 0 }, "d", "ARW", 600, 400, "")).native_long, None);
    }

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
    fn hidden_grading_wheels_render_neutral_without_touching_the_photo() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        s.execute("develop.set", &serde_json::json!({"values": {"grading.shadows.sat": 60, "grading.highlights.sat": 40}})).unwrap();
        let job = s.loupe_job(id, 64, 64, true).unwrap();
        let same = job.clone().with_hidden_grading([false; 3]);
        assert_eq!(same.key, job.key, "nothing hidden: the job is unchanged");
        assert!(same.view_cache.is_some());
        let hidden = job.clone().with_hidden_grading([true, false, false]);
        assert_eq!(hidden.settings.grading.shadows.sat, 0.0);
        assert_eq!(hidden.settings.grading.highlights.sat, 40.0);
        assert_ne!(hidden.key, job.key, "a hidden wheel gets its own result key");
        assert!(hidden.view_cache.is_none() && hidden.cache.is_none(), "never cached as the photo's preview");
        assert_eq!(s.catalog.photo(id).unwrap().develop.grading.shadows.sat, 60.0, "the edit is untouched");
    }

    #[test]
    fn decoder_info_survives_render_jobs_cache_and_eviction() {
        let tone = lightcraft_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
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
            reduced: None,
        };
        job.info = SourceInfo::default(); // Header facts cannot override decoder facts.
        job.settings = Arc::new(DevelopSettings::default());
        let r = job.clone().run();
        assert_eq!(r.loaded.as_ref().unwrap().info.as_ref(), Some(&info));
        let expected = lightcraft_pipeline::render(&r.loaded.as_ref().unwrap().image, &info, &job.settings, &job.request);
        assert_eq!(r.rendered.as_ref().unwrap().image.data, expected.image.data);
        s.accept(&r);
        job.source = s.media.source_ref(s.catalog.photo(id).unwrap(), job.level);
        let again = job.run();
        assert!(again.loaded.is_none());
        assert_eq!(r.rendered.unwrap().image.data, again.rendered.unwrap().image.data);
        s.media.forget(id);
        assert!(s.media.get_source(id, r.level).is_none());
    }

    /// Issue #730: an ILCE-7M3 ARW from an older catalog (white balance relative to the as-shot look, 6500 / 0 as
    /// its as-shot white, a Custom value on that scale) renders pixel for pixel as it did once the camera's matrices
    /// give it a Kelvin scale; the decode teaches the catalog the real as-shot white (bookkeeping, not an undo step),
    /// and the panel then reads As Shot in Kelvin while the stored settings keep their look.
    #[test]
    fn old_relative_white_balance_on_a_kelvin_camera_keeps_its_look_and_learns_the_as_shot_white() {
        use lightcraft_develop::{WbMode, WbScale};
        let mut s = crate::Session::new();
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: "/photos/DSC00001.ARW".into() }, "DSC00001.ARW", "ARW", 64, 64, "2026-01-01T00:00:00");
        p.kind = MediaKind::Raw;
        p.meta.camera = "SONY ILCE-7M3".into();
        p.as_shot_wb = Some((6500.0, 0.0));
        let mut d = lightcraft_develop::DevelopSettings::for_raw(6500.0, 0.0);
        (d.wb.mode, d.wb.temp, d.wb.tint) = (WbMode::Custom, 5800.0, 3.0);
        assert_eq!(d.wb.scale, WbScale::Legacy);
        p.develop = Arc::new(d.clone());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.selection.ids = vec![id];
        s.selection.active = Some(id);
        // the catalog's view of the source before the file is read: Kelvin scale, as-shot white not yet known
        let header = s.source_info(id);
        assert!(header.raw && !header.relative_wb && header.legacy_relative_wb);
        assert_eq!((header.as_shot_temp, header.as_shot_tint), (6500.0, 0.0));
        // the decode: what `files::load_bytes` reports for this camera (the as-shot white through its matrices)
        let decoded = SourceInfo { raw: true, legacy_relative_wb: true, as_shot_temp: 4986.0, as_shot_tint: -2.0, ..Default::default() };
        let mut pixels = Rgb32f::new(64, 64);
        for (i, px) in pixels.data.iter_mut().enumerate() {
            let v = (i % 64) as f32 / 64.0;
            *px = [0.05 + 0.6 * v, 0.04 + 0.4 * v, 0.03 + 0.3 * v];
        }
        let mut job = s.render_job(id, 64, 64, false, true).unwrap();
        let (loaded_info, loaded_pixels) = (decoded.clone(), pixels.clone());
        job.source = SourceRef::File {
            path: "/photos/DSC00001.ARW".into(),
            max_edge: 64,
            loader: Some(Arc::new(move |_, _| Ok((loaded_pixels.clone(), loaded_info.clone())))),
            fallback: None,
            denoise: None,
            reduced: None,
        };
        let undo_before = s.undo.len();
        let request = job.request;
        let r = job.run();
        let new = r.rendered.as_ref().unwrap().image.data.clone();
        // the old pipeline: the same settings against the relative-scale source facts of an older build
        let old_info = SourceInfo { raw: true, relative_wb: true, as_shot_temp: 6500.0, as_shot_tint: 0.0, ..Default::default() };
        let old = lightcraft_pipeline::render(&pixels, &old_info, &d, &request).image.data;
        assert_eq!(new, old, "the look of an existing edit changed");
        assert_ne!(
            new,
            lightcraft_pipeline::render(&pixels, &decoded, &lightcraft_develop::DevelopSettings::for_raw(4986.0, -2.0), &request).image.data
        );
        // accepting the result records the as-shot white, journaled but not undoable, and the settings are untouched
        s.accept(&r);
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(p.as_shot_wb, Some((4986.0, -2.0)));
        assert_eq!(*p.develop, d);
        assert_eq!(s.undo.len(), undo_before);
        assert!(s.pending_log.iter().any(|op| matches!(op, Op::SetAsShotWb { id: i, wb: Some((4986.0, -2.0)) } if *i == id)));
        assert!(!p.is_edited() || p.develop.wb.mode == WbMode::Custom);
        // the panel reads the white balance in effect in Kelvin: the value the relative edit renders as
        let info = s.source_info(id);
        assert_eq!((info.as_shot_temp, info.as_shot_tint), (4986.0, -2.0));
        let shown = lightcraft_pipeline::local::effective_wb(&info, &d);
        assert_eq!(shown, lightcraft_pipeline::local::legacy_to_kelvin(5800.0, 3.0, (4986.0, -2.0)));
        assert!(shown.0 < 4986.0 && shown.0 > 4000.0, "{shown:?}");
        // accepting again changes nothing more
        let log = s.pending_log.len();
        s.accept(&r);
        assert_eq!(s.pending_log.len(), log);
        // a catalog-only (proxy) render of the same settings before the decode gave the same pixels too
        let proxy = lightcraft_pipeline::render(&pixels, &header, &d, &request).image.data;
        assert_eq!(proxy, old, "a smart preview of an existing edit changed");
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
        m.rendered.put(Hash128(7), Arc::new(lightcraft_raster::Rgba8::new(1000, 1000)));
        assert!(m.held() <= 30 * mb, "{}", m.held());
        // the newest preview (the one on screen) is never evicted, even alone over budget
        m.set_budget(mb);
        assert!(m.get(PhotoId(3), SourceLevel::Preview).is_some());
        assert_eq!(m.usage().1.count, 1);
        assert_eq!(m.usage().0.count, 0);
        assert_eq!(m.rendered.mem_usage().0, 0);
    }

    // Given Compare zoomed to 1:1 (two photos' originals), both stay decoded; back to one view,
    // the least recently used goes, and the budget still evicts them like any source
    #[test]
    fn full_size_originals_are_kept_up_to_the_capacity() {
        let img = |w: usize| Arc::new(Rgb32f::new(w, w));
        let mut m = MediaCache::default();
        let mb = 1 << 20;
        m.set_budget(100 * mb);
        m.insert(PhotoId(1), SourceLevel::Full, img(1000));
        m.insert(PhotoId(2), SourceLevel::Full, img(1000));
        assert!(!m.has_source(PhotoId(1), SourceLevel::Full), "one original by default");
        m.set_full_capacity(2);
        m.insert(PhotoId(1), SourceLevel::Full, img(1000));
        assert!(m.has_source(PhotoId(1), SourceLevel::Full) && m.has_source(PhotoId(2), SourceLevel::Full));
        assert_eq!(m.usage().2.count, 2);
        // re-inserting a held photo replaces it instead of taking a second place
        m.insert(PhotoId(2), SourceLevel::Full, img(1000));
        assert_eq!(m.usage().2.count, 2);
        // 1 is the least recently used now
        m.set_full_capacity(1);
        assert!(!m.has_source(PhotoId(1), SourceLevel::Full) && m.has_source(PhotoId(2), SourceLevel::Full));
        m.set_full_capacity(0);
        assert_eq!(m.usage().2.count, 1, "never less than one");
        m.set_full_capacity(2);
        m.insert(PhotoId(3), SourceLevel::Full, img(1000));
        m.set_budget(15 * mb);
        assert_eq!(m.usage().2.count, 1, "the budget evicts the older original");
        assert!(m.has_source(PhotoId(3), SourceLevel::Full));
    }

    // Given the on-screen original alone over the budget, the photo's other sources and its neighbour's stay
    // (counted, it evicted them all: each edit decoded the raw again for the thumbnail and the whole-frame render)
    #[test]
    fn the_on_screen_original_does_not_evict_the_other_sources() {
        let img = |w: usize| Arc::new(Rgb32f::new(w, w));
        let mut m = MediaCache::default();
        let mb = 1 << 20;
        m.set_budget(20 * mb);
        m.set_on_screen(Some(PhotoId(1)));
        m.insert(PhotoId(1), SourceLevel::Preview, img(500));
        m.insert(PhotoId(1), SourceLevel::Thumb, img(200));
        m.insert(PhotoId(1), SourceLevel::Full, img(1500)); // 27 MB
        m.insert(PhotoId(2), SourceLevel::Preview, img(500)); // a prefetched neighbour
        for (id, level) in [(1, SourceLevel::Preview), (1, SourceLevel::Thumb), (1, SourceLevel::Full), (2, SourceLevel::Preview)] {
            assert!(m.has_source(PhotoId(id), level), "{id} {level:?}");
        }
    }

    // A 64 MP original (797 MB as linear float) is bigger than the default cache share (768 MiB):
    // evicted on insert, every pan of a zoomed view decoded the raw again. The original of the photo
    // on screen stays, whatever is decoded after it (prefetched neighbours), until another is shown.
    #[test]
    fn the_original_on_screen_stays_even_over_budget() {
        let img = |w: usize| Arc::new(Rgb32f::new(w, w));
        let mut m = MediaCache::default();
        let mb = 1 << 20;
        m.set_budget(20 * mb);
        m.set_on_screen(Some(PhotoId(1)));
        m.insert(PhotoId(1), SourceLevel::Preview, img(500));
        m.insert(PhotoId(1), SourceLevel::Full, img(1500)); // 27 MB
        assert!(m.has_source(PhotoId(1), SourceLevel::Full));
        m.insert(PhotoId(2), SourceLevel::Preview, img(500)); // a prefetched neighbour
        m.insert(PhotoId(3), SourceLevel::Thumb, img(100));
        assert!(m.has_source(PhotoId(1), SourceLevel::Full));
        m.set_on_screen(Some(PhotoId(2)));
        assert!(!m.has_source(PhotoId(1), SourceLevel::Full));
        assert!(m.held() <= 20 * mb, "{}", m.held());
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
        use lightcraft_pipeline::PixelWindow;
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
        d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.5 };
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
        use lightcraft_pipeline::PixelWindow;
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let (w, h) = (p.width as usize * 8, p.height as usize * 8);
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.spots.push(lightcraft_develop::Spot {
            points: vec![lightcraft_geom::Point::new(0.2, 0.5)],
            size: 0.01,
            source_offset: Some(lightcraft_geom::Point::new(0.6, 0.0)),
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
        use lightcraft_pipeline::PixelWindow;
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

    // LR-VIEW-ZOOM: what the loupe shows at 1:1 is what an export at full size writes. A window of
    // the zoomed frame (the photo's own pixels) is pixel-for-pixel the export, at the centre and
    // at a corner, with tone, colour and detail edits on a camera-sized photo
    #[test]
    fn one_to_one_window_pixels_are_export_pixels() {
        use lightcraft_pipeline::{OutputDepth, OutputSpace, PixelWindow};
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let (w, h) = (p.width as usize, p.height as usize);
        assert!(w.max(h) > 2560, "larger than the preview cap: {w}×{h}");
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.light.exposure = 0.7;
        d.light.contrast = 25.0;
        d.color.saturation = 20.0;
        s.set_develop(id, d, "Edits").unwrap();
        let export = s.render_export(id, w, h, OutputSpace::Srgb, OutputDepth::U8).unwrap().image;
        assert_eq!((export.width, export.height), (w, h), "a full-size export");
        for win in [PixelWindow { x: w / 2 - 256, y: h / 2 - 256, w: 512, h: 384 }, PixelWindow { x: 0, y: h - 300, w: 400, h: 300 }] {
            let tile = s.region_job(id, w, h, win, true).unwrap().run().rendered.unwrap().image;
            assert_eq!((tile.width, tile.height), (win.w, win.h));
            let mut worst = 0u8;
            for y in 0..win.h {
                for x in 0..win.w {
                    let (a, b) = (tile.data[y * win.w + x], export.data[(win.y + y) * w + win.x + x]);
                    for k in 0..3 {
                        worst = worst.max(a[k].abs_diff(b[k]));
                    }
                }
            }
            assert!(worst <= 1, "window {win:?}: the 1:1 view differs from the export by up to {worst} levels");
        }
    }

    /// The thumbnail of a photo whose preview-level source is decoded is cut from it: the file (here
    /// one nothing can decode) is not read again.
    #[test]
    fn a_thumbnail_is_cut_from_the_decoded_preview() {
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: "/nowhere/a.ARW".into() }, "a.ARW", "ARW", 2400, 1600, "");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
        s.media.insert(p.id, SourceLevel::Preview, Arc::new(Rgb32f::from_fn(2400, 1600, |x, _| [x as f32 / 2400.0, 0.5, 0.25])));
        let job = s.thumb_job(p.id, 512).unwrap();
        assert!(matches!(job.source, SourceRef::Fit { .. }));
        let r = job.run();
        let thumb = r.rendered.as_ref().unwrap();
        assert_eq!((thumb.image.width, thumb.image.height), (512, 341));
        s.accept(&r);
        let cached = s.media.get(p.id, SourceLevel::Thumb).unwrap();
        assert_eq!((cached.width, cached.height), (512, 341), "a thumbnail-sized source is kept, not the preview");
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
