//! Source proxies and render jobs.
//!
//! Sources are decoded (or generated) once per level and cached in memory LRUs: `Thumb` (≤ 512 px
//! long edge) for the grid and filmstrip, `Preview` (≤ 2560 px) for the loupe, `Full` (native
//! resolution, the last one only) for exports larger than a preview. A [`RenderJob`] is
//! self-contained and `Send`: frontends run it on a worker thread; if the source wasn't cached yet
//! the job loads it and hands it back in the [`RenderResult`] so the cache can keep it.
//!
//! Rendered thumbnails are cached too ([`lcc_preview::PreviewCache`]: memory LRU, plus a
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

use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lcc_develop::DevelopSettings;
use lcc_pipeline::{Quality, RenderRequest, Rendered, SourceInfo, StageCache};
use lcc_preview::{Hash128, Hasher128, Lru, PreviewCache};
use lcc_raster::{Histogram, Rgb32f, Rgba8};
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
pub const RENDER_CACHE_VERSION: u64 = 80;

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
/// What one photo's previews take on disk, about: its standard preview (a 2048 px JPEG, ~1 MB)
/// and its grid thumbnails. The default disk budget grows by this per photo of the library, so
/// the built previews of a whole wedding do not prune each other while they are made.
pub const DISK_CACHE_PER_PHOTO: u64 = 3 << 19;

/// The default disk budget for a library of `photos`: [`DISK_CACHE_BYTES`], or room for every
/// photo's previews ([`DISK_CACHE_PER_PHOTO`]) when that is more.
pub fn default_disk_cache_bytes(photos: usize) -> u64 {
    DISK_CACHE_BYTES.max((photos as u64).saturating_mul(DISK_CACHE_PER_PHOTO))
}

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
/// `s` without its crop box and straighten angle (flips and the aspect lock kept): what an
/// uncropped render (the crop tool's view) depends on.
fn without_crop(s: &DevelopSettings) -> DevelopSettings {
    let mut s = s.clone();
    s.crop.geometry = lcc_geom::CropGeometry::default();
    s
}

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
pub type FileLoader = Arc<dyn Fn(&str, usize, LoadOptions) -> Result<(Rgb32f, SourceInfo), String> + Send + Sync>;

/// The photo's settings a decode depends on: decoded sources are cached with them ([`DecodedSource::options`]) and
/// decoded again when they change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadOptions {
    /// Where a raw without colour matrices takes its starting colour from (`Profile::camera_colour`).
    pub colour: lcc_develop::CameraColour,
    /// Replace a raw's single outlier photosites before developing it (`detail.nr_outliers`).
    pub outliers: bool,
    /// How a raw's colour model is chosen (`Profile::camera`): empty for its camera's default, `lc` for the app's own
    /// colour, `dcp:<id>` for an imported camera profile (`.dcp`).
    pub camera: String,
    /// Whether the camera profile's own look (tone curve, look table) comes with it ("As profile",
    /// [`DevelopSettings::keeps_camera_look`]); false: its calibration only, under one of the app's looks. Always true
    /// from [`LoadOptions::for_photo`] when the photo's camera profile has no look, so a change of look doesn't decode
    /// those photos again.
    pub camera_look: bool,
    /// Correct the raw's hot, dead and stuck pixels (`sensor.pixelFix`): the camera body's known ones
    /// and the clear ones this photo shows (`crate::files::repair_pixels`).
    pub pixel_fix: bool,
    /// The file being decoded, set by the filesystem loaders: names the camera body when the raw has
    /// no serial number (`crate::sensor_maps::body_key`). Not a setting: [`DecodedSource::serves`]
    /// ignores it.
    pub path: Option<String>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions { colour: Default::default(), outliers: false, camera: String::new(), camera_look: true, pixel_fix: true, path: None }
    }
}

impl LoadOptions {
    pub fn of(s: &DevelopSettings) -> LoadOptions {
        LoadOptions {
            colour: s.profile.camera_colour,
            outliers: s.detail.nr_outliers,
            camera: s.profile.camera.clone(),
            camera_look: s.keeps_camera_look(),
            pixel_fix: s.sensor.fixes_pixels(),
            path: None,
        }
    }

    /// [`Self::of`] photo `p`'s settings, `camera_look` true when its camera profile has no look of its own (then the
    /// look chosen makes no difference to the decode).
    pub fn for_photo(p: &Photo) -> LoadOptions {
        let mut o = LoadOptions::of(&p.develop);
        if !o.camera_look && !crate::camera_profiles::photo_profile(p).is_some_and(|d| d.has_look()) {
            o.camera_look = true;
        }
        o
    }

    /// These options for a decode of the file at `path`.
    pub fn at(self, path: &str) -> LoadOptions {
        LoadOptions { path: Some(path.to_string()), ..self }
    }
}

/// [`FileLoader`] for a thumbnail: a raw file that keeps smaller copies of its raw image (Lightroom's merged DNGs)
/// develops the smallest that covers `max_edge` (set by the app); the flag says it did. Never used for an export,
/// which must render the full image.
pub type ReducedLoader = Arc<dyn Fn(&str, usize, LoadOptions) -> Result<(Rgb32f, SourceInfo, bool), String> + Send + Sync>;

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
pub type PairLoader = Arc<dyn Fn(&str, usize, &DenoiseSpec, LoadOptions) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> + Send + Sync>;

/// A photo's denoised picture next to its plain one, and the last mix of the two that was asked for.
pub struct Twin {
    pub image: Arc<Rgb32f>,
    /// The denoised product it was read from ([`DenoiseSpec::key`]); `None` when not known.
    pub key: Option<String>,
    /// The Amount (in thousandths) and the picture mixed for it, so dragging other sliders keeps one source picture.
    mix: std::sync::Mutex<Option<(u32, Arc<Rgb32f>)>>,
}

impl Twin {
    pub fn new(image: Arc<Rgb32f>) -> Twin {
        Twin { image, key: None, mix: std::sync::Mutex::new(None) }
    }

    /// A twin read from the denoised product with key `key`.
    pub fn from_product(image: Arc<Rgb32f>, key: &str) -> Twin {
        Twin { key: Some(key.to_string()), ..Twin::new(image) }
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
    pub camera_tone: Option<lcc_pipeline::tone::CameraTone>,
    /// The same picture developed from the AI-denoised mosaic, when the photo has one cached.
    pub denoised: Option<Arc<Twin>>,
    /// The settings the file was decoded with.
    pub options: LoadOptions,
    /// These pixels came from the photo's smart preview (an offline original), not from its file.
    /// A refresh must never rebuild a proxy from itself: that would stamp its older look as current.
    pub from_proxy: bool,
    /// Developed from a smaller copy of the raw image that the file keeps ([`ReducedLoader`]), not from the image
    /// itself: good for a thumbnail, never for an export.
    pub reduced: bool,
}

impl DecodedSource {
    pub fn new(image: Arc<Rgb32f>, info: Option<SourceInfo>) -> Self {
        DecodedSource { image, info, camera_tone: None, denoised: None, options: LoadOptions::default(), from_proxy: false, reduced: false }
    }

    /// Whether these pixels are what decoding with `options` gives: decoded with them, or from a source the options
    /// don't change (a rendered file, a smart preview or a demo scene has no mosaic for the outlier filter or a camera
    /// profile, and a raw without measured colour to start from has nothing for the Camera colour option).
    pub fn serves(&self, options: &LoadOptions) -> bool {
        let info = self.info.as_ref();
        // a smart preview's stored facts say raw, but it has no mosaic: no option changes its pixels
        self.from_proxy
            || ((self.options.outliers == options.outliers || !info.is_some_and(|i| i.raw))
                && (self.options.colour == options.colour || !info.is_some_and(|i| i.colorimetric))
                && ((self.options.camera == options.camera && self.options.camera_look == options.camera_look) || !info.is_some_and(|i| i.raw))
                && (self.options.pixel_fix == options.pixel_fix || !info.is_some_and(|i| i.raw)))
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
        if !lcc_denoise::product::blend(&mut mixed, &twin.image, thousandths as f32 / 1000.0) {
            return self.image.clone();
        }
        let mixed = Arc::new(mixed);
        *memo = Some((thousandths, mixed.clone()));
        mixed
    }

    /// Cache cost: the picture, its segmentation mattes and transparency, its denoised twin and room for the lazy mix.
    pub fn bytes(&self) -> usize {
        let mattes = self.info.as_ref().and_then(|i| i.mattes.as_ref()).map_or(0, |m| m.bytes());
        let mattes = mattes + self.info.as_ref().and_then(|i| i.local_tone.as_ref()).map_or(0, |t| t.map.gains.len() * 4);
        let mattes = mattes + self.info.as_ref().and_then(|i| i.alpha.as_ref()).map_or(0, |a| a.data.len() * 4);
        self.image.data.len() * 12 + 64 + std::mem::size_of::<SourceInfo>() + mattes + self.denoised.as_ref().map_or(0, |t| t.bytes())
    }

    /// What to render these pixels against: the decoder's facts, else `header` (the catalog's)
    /// with any stored camera tone curve. The original's size comes from `header` either way: the
    /// decoder only sees the level it decoded. The photo's generative fill patches always come from
    /// `header` too (the session knows them, the decoder doesn't).
    pub fn info_or(&self, header: SourceInfo) -> SourceInfo {
        match &self.info {
            Some(info) => SourceInfo {
                native_long: info.native_long.or(header.native_long),
                generated: header.generated,
                avoid: header.avoid,
                identity: header.identity.or(info.identity),
                // (the catalog's maps, as the session knows them: the decoder doesn't)
                vignetting_maps: header.vignetting_maps,
                ..info.clone()
            },
            None => SourceInfo { camera_tone: self.camera_tone.or(header.camera_tone), ..header },
        }
    }
}

#[derive(Clone)]
pub enum SourceRef {
    Loaded(Box<DecodedSource>),
    /// A larger decoded source of the photo, fitted to `max_edge` by the worker: the thumbnail of
    /// the photo open in the loupe is cut from its preview- or full-level source instead of decoding
    /// the file again (a second or more for a 64 MP raw).
    Fit {
        from: Box<DecodedSource>,
        max_edge: usize,
    },
    Demo {
        scene: Box<lcc_scenes::Scene>,
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
        /// The photo's settings the decode depends on.
        options: LoadOptions,
        /// A thumbnail's loader, used instead of `loader` (see [`ReducedLoader`]); `None` for every other job.
        reduced: Option<ReducedLoader>,
    },
    /// A smart preview standing in for a missing original.
    Smart {
        path: std::path::PathBuf,
    },
    /// A video clip: its poster frame at `time` (`None`: [`lcc_video::default_poster_time`]),
    /// at most `max_edge` long (M14.3). A clip that can't be decoded gives [`video_placeholder`].
    Video {
        path: String,
        time: Option<f64>,
        max_edge: usize,
    },
}

impl SourceRef {
    /// Whether the photo's original isn't there: its drive isn't mounted, or it was moved or deleted. A file (a
    /// photo or a clip) asks the file system: for render workers, never the UI thread. A smart preview is only ever
    /// the source when its original is known to be missing. Never in the browser build (no file system there: its
    /// originals are always "there").
    pub fn original_missing(&self) -> bool {
        match self {
            SourceRef::File { path, .. } | SourceRef::Video { path, .. } => !cfg!(target_arch = "wasm32") && !std::path::Path::new(path).exists(),
            SourceRef::Smart { .. } => !cfg!(target_arch = "wasm32"),
            _ => false,
        }
    }

    pub fn load(&self) -> Result<Arc<Rgb32f>, String> {
        self.load_source().map(|s| s.image)
    }

    pub fn load_source(&self) -> Result<DecodedSource, String> {
        self.load_source_of(None)
    }

    /// [`Self::load_source`] for a job whose photo's file content is `content` (its [`RenderJob::source_key`]): a
    /// decode is shared only with jobs for the same content, so a photo reloaded after its file changed never takes
    /// a decode of the old file that is still running or held (review of upstream PR #3569).
    pub fn load_source_of(&self, content: Option<Hash128>) -> Result<DecodedSource, String> {
        let image = match self {
            SourceRef::Loaded(a) => return Ok((**a).clone()),
            SourceRef::Fit { from, max_edge } => {
                if from.image.width.max(from.image.height) <= *max_edge {
                    return Ok((**from).clone());
                }
                // Lanczos, not a box average: a box at 2-3:1 halves the contrast of detail near
                // the output's resolution limit and aliases what is finer
                let image = lcc_raster::resample::fit(&from.image, *max_edge, *max_edge, lcc_raster::resample::Filter::Lanczos3);
                // the transparency is fitted with the pixels (the same size and filter)
                let fitted = |p: &Arc<lcc_raster::Plane>| {
                    Arc::new(lcc_raster::resample::resize(p, image.width, image.height, lcc_raster::resample::Filter::Lanczos3))
                };
                let info =
                    from.info.clone().map(|i| SourceInfo { alpha: i.alpha.as_ref().map(fitted), infrared: i.infrared.as_ref().map(fitted), ..i });
                return Ok(DecodedSource { image: Arc::new(image), denoised: None, info, ..(**from).clone() });
            }
            SourceRef::Demo { scene, max_edge } => crate::demo::scene_pixels(scene, *max_edge)?,
            SourceRef::File { path, max_edge, loader, fallback, denoise, options, reduced } => {
                // (a job that comes while another decodes the same pixels waits for that decode)
                let decode = || Self::decode_file(path, *max_edge, loader.as_ref(), fallback.as_deref(), denoise.as_ref(), options, reduced.as_ref());
                return match share_key(path, content, *max_edge, loader.as_ref(), fallback.as_deref(), denoise.as_ref(), options, reduced.as_ref()) {
                    Some(key) => {
                        let mut anchors: crate::decode_share::Anchors = Vec::new();
                        if let Some(l) = loader {
                            anchors.push(Box::new(Arc::downgrade(l)));
                        }
                        if let Some(l) = reduced {
                            anchors.push(Box::new(Arc::downgrade(l)));
                        }
                        if let Some((_, l)) = denoise {
                            anchors.push(Box::new(Arc::downgrade(l)));
                        }
                        crate::decode_share::decode_once(&key, anchors, decode)
                    }
                    None => decode(),
                };
            }
            SourceRef::Video { path, time, max_edge } => Arc::new(video_poster(path, *time, *max_edge)?),
            #[cfg(not(target_arch = "wasm32"))]
            SourceRef::Smart { path } => return crate::smart::load(path),
            #[cfg(target_arch = "wasm32")]
            SourceRef::Smart { .. } => return Err("smart previews are not available here".into()),
        };
        Ok(DecodedSource::new(image, None))
    }

    /// Decode a [`SourceRef::File`] (its fields).
    fn decode_file(
        path: &str,
        max_edge: usize,
        loader: Option<&FileLoader>,
        fallback: Option<&std::path::Path>,
        denoise: Option<&(DenoiseSpec, PairLoader)>,
        options: &LoadOptions,
        reduced: Option<&ReducedLoader>,
    ) -> Result<DecodedSource, String> {
        let plain = || match (reduced, loader) {
            (Some(l), _) => l(path, max_edge, options.clone()).map(|(image, info, reduced)| DecodedSource {
                options: options.clone(),
                reduced,
                ..DecodedSource::new(Arc::new(image), Some(info))
            }),
            (None, Some(l)) => l(path, max_edge, options.clone())
                .map(|(image, info)| DecodedSource { options: options.clone(), ..DecodedSource::new(Arc::new(image), Some(info)) }),
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
                    pair(path, max_edge, spec, options.clone()).and_then(|(image, twin, info)| {
                        if spec.make.is_some() && twin.is_none() {
                            return Err("the cached AI Denoise picture could not be read; clear the denoise cache and retry".into());
                        }
                        Ok(DecodedSource {
                            image: Arc::new(image),
                            info: Some(info),
                            camera_tone: None,
                            denoised: twin.map(|t| Arc::new(Twin::from_product(Arc::new(t), &spec.key))),
                            options: options.clone(),
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
        match (r, fallback) {
            #[cfg(not(target_arch = "wasm32"))]
            (Err(e), Some(sp)) if denoise.is_none_or(|(spec, _)| spec.make.is_none()) && crate::smart::is_valid(sp) => {
                crate::smart::load(sp).map_err(|e2| format!("{e}; smart preview: {e2}"))
            }
            (r, _) => r,
        }
    }
}

/// What a [`SourceRef::File`] decode's pixels depend on, as a key for [`crate::decode_share`]
/// (the loaders by identity: the session keeps one of each). `None`: not shared (a decode that
/// makes the photo's denoised picture first, for an export).
fn share_key(
    path: &str,
    content: Option<Hash128>,
    max_edge: usize,
    loader: Option<&FileLoader>,
    fallback: Option<&std::path::Path>,
    denoise: Option<&(DenoiseSpec, PairLoader)>,
    options: &LoadOptions,
    reduced: Option<&ReducedLoader>,
) -> Option<String> {
    if denoise.is_some_and(|(spec, _)| spec.make.is_some()) {
        return None;
    }
    let at = |p: Option<*const ()>| p.map_or(0, |p| p as usize);
    let loader = at(loader.map(|l| Arc::as_ptr(l) as *const ()));
    let reduced = at(reduced.map(|l| Arc::as_ptr(l) as *const ()));
    let pair = at(denoise.map(|(_, l)| Arc::as_ptr(l) as *const ()));
    let denoised = denoise.map(|(spec, _)| (spec.key.as_str(), spec.product.as_path()));
    Some(format!("{path}|{content:?}|{max_edge}|{loader:x}|{reduced:x}|{pair:x}|{denoised:?}|{options:?}|{fallback:?}"))
}

/// A clip's poster frame (linear Rec.709, the JPEG/TIFF input path), `max_edge` long at most. An
/// unreadable file is an error (offline drive: the tile says so); a clip we can't decode
/// (unsupported codec, damaged stream, the web build) is [`video_placeholder`], not an error,
/// so a grid full of such clips shows film tiles rather than a stream of failures.
pub fn video_poster(path: &str, time: Option<f64>, max_edge: usize) -> Result<Rgb32f, String> {
    let side = u32::try_from(max_edge.clamp(16, 16_384)).unwrap_or(16_384);
    match lcc_video::poster(std::path::Path::new(path), time, side) {
        Ok(f) if f.image.width > 0 && f.image.height > 0 => Ok(f.image),
        Ok(_) => Ok(video_placeholder(side as usize)),
        Err(lcc_video::VideoError::Io(e)) => Err(format!("{path}: {e}")),
        Err(e) => {
            log::debug!("{path}: video poster unavailable ({e}); showing a placeholder");
            Ok(video_placeholder(side as usize))
        }
    }
}

/// A stand-in poster for a clip that can't be decoded: a dark 16:9 tile with a film-strip glyph
/// (sprocket holes along two bands, a light frame between them). Drawn in code (original work).
pub fn video_placeholder(max_edge: usize) -> Rgb32f {
    let w = max_edge.clamp(16, 512);
    let h = (w * 9 / 16).max(9);
    let mut img = Rgb32f::new(w, h);
    let (bg, strip, light) = (0.02f32, 0.006f32, 0.25f32);
    let (sx0, sx1) = (w * 30 / 100, w * 70 / 100);
    let (sy0, sy1) = (h * 20 / 100, h * 80 / 100);
    let band = ((sx1 - sx0) / 7).max(1);
    let hole = (band / 2).max(1);
    for y in 0..h {
        for x in 0..w {
            let mut v = bg;
            if (sx0..sx1).contains(&x) && (sy0..sy1).contains(&y) {
                v = strip;
                let in_band = x < sx0 + band || x >= sx1 - band;
                if in_band {
                    let bx = if x < sx0 + band { x - sx0 } else { x - (sx1 - band) };
                    let pitch = (hole * 2).max(2);
                    let hy = (y - sy0) % pitch;
                    if (band - hole) / 2 <= bx && bx < (band - hole) / 2 + hole && hy < hole {
                        v = light;
                    }
                } else if x >= sx0 + band + hole && x < sx1 - band - hole && y >= sy0 + hole * 2 && y < sy1 - hole * 2 {
                    v = light * 0.5;
                }
            }
            if let Some(px) = img.data.get_mut(y * w + x) {
                *px = [v, v, v];
            }
        }
    }
    img
}

/// What identifies the pixels a photo's renders start from: [`content_key`], and for a video
/// its poster time too (a new poster frame is new pixels: its thumbnail and view are rendered afresh).
pub fn pixels_key(p: &Photo) -> String {
    match (&p.video, p.kind) {
        (Some(v), MediaKind::Video) => match v.poster_time {
            Some(t) => format!("{}#poster={t}", content_key(p)),
            None => content_key(p),
        },
        _ => content_key(p),
    }
}

pub struct MediaCache {
    /// Decoded photos of recent Photo Merge previews.
    pub merge_frames: crate::merge::FrameCache,
    settings_hashes: SettingsHashes,
    /// Each photo's placement for its current settings ([`crate::Session::placement_of`]), by
    /// (photo, cropped): computed once per settings, not per frame or per job.
    placements: HashMap<(PhotoId, bool), (std::sync::Weak<DevelopSettings>, crate::placement::Placement)>,
    /// Decoded thumbnail-level sources (LRU by bytes).
    thumbs: Lru<PhotoId, DecodedSource>,
    /// Decoded preview-level sources with their last use ([`lcc_preview::next_tick`]).
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
    scenes: Vec<lcc_scenes::Scene>,
    /// Rendered thumbnails (memory, plus disk once a library is attached).
    pub rendered: Arc<PreviewCache>,
    /// Which photos have a cached denoised picture, and how to read it.
    pub denoise: DenoiseIndex,
    /// What decodes have shown about each photo's file that the Edit panel offers options for (see
    /// [`ProfileFacts`]): kept when the decoded sources go, so those rows do not vanish while a source is decoded
    /// again or evicted, which would move every control under them.
    profile_facts: HashMap<PhotoId, ProfileFacts>,
    /// The camera bodies' corrections the decoded sources were made with ([`crate::sensor_maps::generation`]).
    sensor_generation: u64,
    /// The same per body ([`crate::sensor_maps::body_corrections`]).
    sensor_bodies: HashMap<String, u64>,
    /// The renderer version in grid thumbnails' keys: [`RENDER_CACHE_VERSION`] (tests change it to stand for an
    /// update that bumps it).
    pub(crate) thumb_version: u64,
}

/// File facts that decide whether the Edit panel's Profile section shows an option: a camera model with measured
/// colour (Calibrated camera colour) and a raw carrying its own local tone mapping. They belong to the file, not to
/// a decode, so once any decode has seen one it stays until the photo's sources are forgotten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileFacts {
    pub colorimetric: bool,
    pub local_tone: bool,
    /// A DNG of rendered RGB: the Profiles panel offers no camera profile for it (issue #763).
    pub rendered_rgb: bool,
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
            merge_frames: crate::merge::FrameCache::default(),
            settings_hashes: SettingsHashes::default(),
            placements: HashMap::new(),
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
            profile_facts: HashMap::new(),
            sensor_generation: 0,
            sensor_bodies: HashMap::new(),
            thumb_version: RENDER_CACHE_VERSION,
        }
    }
}

/// Memory for rendered previews out of the cache share.
fn rendered_budget(share: usize) -> usize {
    // a quarter of the share, so a big budget keeps a whole shoot's previews, and at least
    // RENDERED_MEM_BYTES where the share allows
    (share / 4).max(RENDERED_MEM_BYTES.min(share / 2))
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
        self.placements.clear();
        self.thumbs = Lru::new(THUMB_SOURCE_BYTES.min(self.budget));
        self.previews.clear();
        self.full.clear();
        self.profile_facts.clear();
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
        // a camera body's corrections changed (a pixel was confirmed): that body's raws decoded before are out of
        // date, and only those
        let generation = crate::sensor_maps::generation();
        if generation != self.sensor_generation {
            let bodies = crate::sensor_maps::body_corrections();
            if self.sensor_generation != 0 {
                let before = &self.sensor_bodies;
                let current = |s: &DecodedSource| {
                    let body = s.info.as_ref().and_then(|i| i.pixels.as_ref()).filter(|p| p.on).and_then(|p| p.body.as_deref());
                    body.is_none_or(|b| before.get(b).copied().unwrap_or(0) == bodies.get(b).copied().unwrap_or(0))
                };
                self.thumbs.retain_entries(|_, s| current(s));
                self.previews.retain(|e| current(&e.1));
                self.full.retain(|e| current(&e.1));
            }
            self.sensor_bodies = bodies;
            self.sensor_generation = generation;
        }
        match level {
            SourceLevel::Thumb => self.thumbs.get(&id).cloned(),
            SourceLevel::Preview => self.previews.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = lcc_preview::next_tick();
                e.1.clone()
            }),
            SourceLevel::Full => self.full.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = lcc_preview::next_tick();
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

    /// A decoded source with what the decoder reported about it (tests: photos of another as-shot white).
    #[cfg(test)]
    pub(crate) fn insert_with_info(&mut self, id: PhotoId, level: SourceLevel, img: Arc<Rgb32f>, info: SourceInfo) {
        self.insert_source(id, level, DecodedSource::new(img, Some(info)));
    }

    pub(crate) fn insert_source(&mut self, id: PhotoId, level: SourceLevel, img: DecodedSource) {
        if let Some(info) = &img.info {
            let f = self.profile_facts.entry(id).or_default();
            f.colorimetric |= info.colorimetric;
            f.local_tone |= info.local_tone.is_some();
            f.rendered_rgb |= info.rendered_rgb;
        }
        let tick = lcc_preview::next_tick();
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
        self.profile_facts.remove(&id);
    }

    /// What decodes of `id` have shown so far, with what the cached sources say now (see [`ProfileFacts`]).
    pub fn profile_facts(&self, id: PhotoId) -> ProfileFacts {
        let now = self.source_facts(id, SourceInfo::default());
        let seen = self.profile_facts.get(&id).copied().unwrap_or_default();
        ProfileFacts {
            colorimetric: seen.colorimetric || now.colorimetric,
            local_tone: seen.local_tone || now.local_tone.is_some(),
            rendered_rgb: seen.rendered_rgb || now.rendered_rgb,
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
        let wanted = LoadOptions::for_photo(p);
        let mut r = self.source_ref_plain(p, level, cached, &wanted);
        if let SourceRef::File { options, .. } = &mut r {
            *options = wanted;
        }
        if let (SourceRef::File { denoise, .. }, Some((spec, loader))) = (&mut r, self.denoise.get(p.id)) {
            *denoise = Some((spec.clone(), loader.clone()));
        }
        r
    }

    fn source_ref_plain(&mut self, p: &Photo, level: SourceLevel, cached: bool, wanted: &LoadOptions) -> SourceRef {
        // (a source decoded with other settings is decoded again, and the new one replaces it in the cache)
        if cached
            && let Some(a) = self.get_source(p.id, level)
            && a.serves(wanted)
        {
            return SourceRef::Loaded(Box::new(a));
        }
        if p.kind == MediaKind::Video
            && let Source::File { path } = &p.source
        {
            let max_edge = level.max_edge().min(p.width.max(p.height).max(1) as usize);
            return SourceRef::Video { path: path.clone(), time: p.video.as_ref().and_then(|v| v.poster_time), max_edge };
        }
        // a thumbnail of a photo whose preview or full source is decoded (the one in the loupe) is
        // cut from it; not with a denoised picture, which the thumbnail would lose
        if cached
            && level == SourceLevel::Thumb
            && self.denoise.spec(p.id).is_none()
            && let Some(big) =
                [SourceLevel::Preview, SourceLevel::Full].into_iter().find_map(|level| self.get_source(p.id, level).filter(|b| b.serves(wanted)))
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
    /// render worker in another wasm instance builds its sources from this). Without a photo's load options: they are
    /// [`Self::source_ref`]'s to add.
    pub fn origin_ref(&mut self, origin: &Source, max_edge: usize) -> SourceRef {
        match origin {
            Source::Demo { scene } => {
                if self.scenes.is_empty() {
                    self.scenes = lcc_scenes::demo_library();
                }
                match self.scenes.iter().find(|s| s.id == *scene) {
                    Some(s) => SourceRef::Demo { scene: Box::new(s.clone()), max_edge },
                    None => SourceRef::File {
                        path: format!("demo:{scene}"),
                        max_edge,
                        loader: None,
                        fallback: None,
                        denoise: None,
                        options: LoadOptions::default(),
                        reduced: None,
                    },
                }
            }
            Source::File { path } => SourceRef::File {
                path: path.clone(),
                max_edge,
                loader: self.file_loader.clone(),
                fallback: None,
                denoise: None,
                options: LoadOptions::default(),
                reduced: None,
            },
        }
    }
}

/// The error of a render skipped because a newer request replaced it ([`RenderJob::superseded`]).
pub const SUPERSEDED: &str = "replaced by a newer render";

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
    /// Keep a full-quality result also as the photo's latest picture of its kind, whatever its
    /// settings ([`PreviewCache::put_latest_with`]; the alias, [`latest_view_key`] or
    /// [`latest_thumb_key`]): what an edited photo shows while the render for its current settings
    /// is on its way, never the unedited camera JPEG.
    pub latest: Option<(Arc<PreviewCache>, Hash128)>,
    /// A grid thumbnail of the photo's own settings: kept as the photo's last thumbnail too, and shown in its place
    /// when the original is missing ([`KeptThumb`]). `None` for every other job.
    pub kept: Option<KeptThumb>,
    /// Where the result lies in its photo and the look it has ([`crate::placement`]): what lets it
    /// stand in, at once, for the photo with another crop. Set for the photo's own renders; `None`
    /// for others (a variant, a face tile, a hover preview).
    pub placement: Option<crate::placement::Placement>,
    /// Render for this monitor profile ([`Self::with_display`]).
    pub display: Option<Arc<crate::display::Display>>,
    /// Compose the render into a slideshow slide ([`Self::with_slide`]).
    pub slide: Option<Arc<crate::slide::SlideCompose>>,
    /// A face tile that can be cut from the raw's embedded preview, tried before `source`
    /// ([`crate::Session::face_job`]; `None` for every other job).
    pub preview_cut: Option<crate::face_tiles::PreviewCut>,
    /// Shrink a full-quality result to fit this size (physical pixels) with a Lanczos filter once
    /// it is rendered. The loupe renders a little larger than it is drawn (its edge is rounded up
    /// to a 64 px step) and the window would otherwise shrink the texture with 2-tap bilinear sampling, which blurs fine detail at
    /// small ratios and aliases it at large ones. The caches keep the full render (a fit made on
    /// the GPU reads it back as well for the view cache, unless [`Self::drawn_view_cache`]).
    pub display_fit: Option<(usize, usize)>,
    /// Set by the frontend when a newer request for the same view replaced this one while it ran
    /// (an interactive draft on a photo whose first full render is decoding). Checked once the
    /// source is decoded: the render is then skipped (the decoded source still comes back for the
    /// session to keep), so the draft gets the processor and the GPU.
    pub superseded: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// A slider drag's update: a fit made on the GPU leaves its drawn-size image in the view cache
    /// instead of reading the full render back too (+3 ms an update; the next render at rest
    /// replaces it). Not part of [`Self::key`]: the same render, cached at another size.
    pub drawn_view_cache: bool,
    /// An HDR view's render for an HDR display that uploads at this headroom: rendered on the
    /// GPU, it comes back as the texture's texels made there ([`RenderResult::hdr_texels`]) and
    /// without float samples; on the CPU, as usual. Set by the frontend.
    pub hdr_texels: Option<f32>,
    /// Shown on screen at once and not kept: the result may stay on the GPU as a texture
    /// ([`Self::on_screen`]).
    pub on_screen: bool,
}

/// A render left on the GPU for the window to draw ([`RenderResult::frame`]).
#[cfg(not(target_arch = "wasm32"))]
pub type ScreenFrame = lcc_gpu::DisplayFrame;
/// A render left on the GPU for the window to draw (never, in the browser build).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
pub struct ScreenFrame {
    pub width: usize,
    pub height: usize,
}

/// The least the automatic preview cache takes where the drive has room for it.
pub const AUTO_DISK_CACHE_MIN: u64 = 20 << 30;
/// The share of the cache drive's room (free space and what the cache holds) the automatic size takes.
pub const AUTO_DISK_CACHE_SHARE: f64 = 0.15;
/// The most of the drive's free space the automatic size may claim beyond what the cache holds.
pub const AUTO_DISK_CACHE_FREE_SHARE: f64 = 0.5;
/// What the automatic size always leaves free on the cache drive.
const CACHE_DRIVE_RESERVE: u64 = 10 << 30;

/// The automatic preview cache size: a budget sized from the library alone (1.5 MB a photo) let a
/// wedding's previews prune each other and every edit's previews replace the last ones, so
/// reopening the library rendered everything again. Now: a share ([`AUTO_DISK_CACHE_SHARE`]) of
/// the room on the cache drive (`free` bytes plus the cache's own `used`), at least
/// [`AUTO_DISK_CACHE_MIN`] and room for every photo of the library, never more than leaves
/// [`CACHE_DRIVE_RESERVE`] (or half the room, on a small drive) free, and never growing by more
/// than half of what is free ([`AUTO_DISK_CACHE_FREE_SHARE`]): on a nearly full drive the minimum
/// gives way. `free` unknown: the minimum, or the library's room when that is more.
pub fn automatic_disk_cache_bytes(photos: usize, free: Option<u64>, used: u64) -> u64 {
    let by_photos = default_disk_cache_bytes(photos);
    let Some(free) = free else { return by_photos.max(AUTO_DISK_CACHE_MIN) };
    let room = free.saturating_add(used);
    let share = (room as f64 * AUTO_DISK_CACHE_SHARE) as u64;
    let want = by_photos.max(AUTO_DISK_CACHE_MIN).max(share);
    let cap = room.saturating_sub(CACHE_DRIVE_RESERVE).max(room / 2);
    let most = used.saturating_add((free as f64 * AUTO_DISK_CACHE_FREE_SHARE) as u64);
    want.min(cap).max(DISK_CACHE_BYTES.min(room)).min(most)
}

/// A cached view render this much smaller than asked for still serves the loupe (rounding: a fit
/// of the same frame into a box one pixel different).
const VIEW_REUSE_SLACK: f64 = 0.01;

/// `img` fitted into `max_w × max_h` (its aspect kept), when it is big enough to need no enlarging
/// (within [`VIEW_REUSE_SLACK`]): as it is, or shrunk.
pub fn fit_cached(img: &Rgba8, max_w: usize, max_h: usize) -> Option<Rgba8> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || max_w == 0 || max_h == 0 {
        return None;
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    if !s.is_finite() || s > 1.0 + VIEW_REUSE_SLACK {
        return None;
    }
    let (nw, nh) = (((w as f64 * s).round() as usize).clamp(1, w), ((h as f64 * s).round() as usize).clamp(1, h));
    if nw == w && nh == h {
        return Some(img.clone());
    }
    Some(fit_for_display(img, nw, nh))
}

/// `img` shrunk to fit `max_w × max_h` with a Lanczos filter (see [`RenderJob::display_fit`]).
pub fn fit_for_display(img: &Rgba8, max_w: usize, max_h: usize) -> Rgba8 {
    lcc_raster::resample::fit_rgba8(img, max_w.max(1), max_h.max(1), lcc_raster::resample::Filter::Lanczos3)
}

pub struct RenderResult {
    pub request_id: u64,
    pub source_key: Option<Hash128>,
    pub photo: PhotoId,
    pub level: SourceLevel,
    pub key: u64,
    pub rendered: Result<Rendered, String>,
    /// Set when this job's render was tried on the GPU and fell back to the CPU: why
    /// ([`lcc_gpu::Fallback::Failed`]). The result is the CPU render either way.
    pub gpu_fallback: Option<String>,
    /// A source that was loaded by this job (to be inserted into the cache).
    pub loaded: Option<DecodedSource>,
    /// Set for a [`QuickJob`]'s stand-in: where the image came from.
    pub quick: Option<QuickSource>,
    /// The display profile (id) the image is encoded for; `None`: sRGB (see
    /// [`crate::display::present`]).
    pub display: Option<u64>,
    /// An HDR view's texture texels made on the GPU, and the headroom they were made for
    /// ([`RenderJob::hdr_texels`]); its render then has no float samples.
    pub hdr_texels: Option<(Vec<u16>, f32)>,
    /// The result as a texture on the window's GPU device ([`RenderJob::on_screen`]); `rendered`
    /// then holds its histogram and an empty (0 × 0) image.
    pub frame: Option<ScreenFrame>,
    /// Where the picture lies in its photo ([`RenderJob::placement`]; for a stand-in, the one of the
    /// picture it is).
    pub placement: Option<crate::placement::Placement>,
}

impl RenderJob {
    /// The result is only drawn (an interactive draft of a view): when the GPU renderer runs on the
    /// window's device it may hand back a texture ([`RenderResult::frame`]) instead of reading the
    /// pixels back. Ignored where pixels are needed: thumbnails (cached), a render kept as the view
    /// preview (other than a drag's drawn-size one), a monitor profile (corrected on the CPU), a
    /// slide, transparency and HDR texels. The display fit is then the window's drawing filter.
    pub fn on_screen(mut self) -> Self {
        self.on_screen = true;
        self
    }

    /// Render at draft quality (interactive drags). Gets its own result key.
    /// Shrink the full-quality result to fit `w × h` pixels with a Lanczos filter (see
    /// [`Self::display_fit`]). Gets its own result key, so a change of drawn size renders again.
    pub fn with_display_fit(mut self, w: usize, h: usize) -> Self {
        self.display_fit = Some((w, h));
        self.key ^= ((w as u64) << 32 | h as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self
    }

    /// The view render kept for this job's photo and settings ([`Self::view_cache`]), fitted to the
    /// size asked for, when it is at least that big (else `None`: render). Only for what the cached
    /// view is: a full-quality, 8-bit, whole-frame sRGB render of the photo (no HDR, display
    /// profile, transparency or slide).
    pub fn cached_view(&self) -> Option<Rgba8> {
        let (cache, key) = self.view_cache.as_ref()?;
        if self.request.quality != Quality::Full
            || self.request.depth != lcc_pipeline::OutputDepth::U8
            || self.request.window.is_some()
            || self.request.alpha
            || self.request.display.is_some()
            || self.display.is_some()
            || self.hdr_texels.is_some()
            || self.settings.hdr.enabled
            || self.slide.is_some()
        {
            return None;
        }
        let img = cache.get_at(self.cache_generation, *key)?;
        fit_cached(&img, self.request.max_w, self.request.max_h)
    }

    /// See [`Self::drawn_view_cache`].
    pub fn with_drawn_view_cache(mut self) -> Self {
        self.drawn_view_cache = true;
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
    pub fn with_overlay(mut self, overlay: lcc_pipeline::Overlay) -> Self {
        if self.request.overlay != overlay {
            self.key ^= self.request.overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
            self.request.overlay = overlay;
            self.key ^= overlay.key().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        // a diagnostic view (visualized range / spots) must never become the photo's cached preview
        if overlay != lcc_pipeline::Overlay::None {
            self.view_cache = None;
            self.latest = None;
            self.kept = None;
        }
        self
    }

    /// Soft-proof the result (see [`lcc_pipeline::Proof`]). Gets its own result key and is
    /// never kept as the photo's cached preview.
    pub fn with_proof(mut self, proof: Option<lcc_pipeline::Proof>) -> Self {
        if self.request.proof != proof {
            let k = |p: Option<lcc_pipeline::Proof>| p.map_or(0, |p| p.key()).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
            self.key ^= k(self.request.proof) ^ k(proof);
            self.request.proof = proof;
        }
        if proof.is_some() {
            self.view_cache = None;
            self.latest = None;
            self.kept = None;
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
                *wheel = lcc_develop::Wheel::default();
            }
        }
        let bits = hidden.iter().enumerate().fold(0u64, |b, (i, h)| b | ((*h as u64) << i));
        self.key ^= s.hash64() ^ self.settings.hash64() ^ bits.wrapping_mul(0xd6e8_feb8_6659_fd93);
        self.settings = Arc::new(s);
        self.cache = None;
        self.view_cache = None;
        self.latest = None;
        self.kept = None;
        self.preview_cut = None;
        self
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
        if self.cache.is_some() || self.slide.is_some() || self.request.depth != lcc_pipeline::OutputDepth::U8 {
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
        // thumbnails go to the second graphics card when there is one: the editing card keeps the views
        let thumb = self.cache.is_some();
        let run = || crate::guard::catch("the render", || self.run_unguarded()).unwrap_or_else(|e| RenderResult { rendered: Err(e), ..failed });
        if thumb { lcc_gpu::prefer_helper(run) } else { run() }
    }

    /// This job's result when it fails with `e`.
    fn failed(&self, e: String) -> RenderResult {
        RenderResult {
            request_id: self.request_id,
            source_key: self.source_key,
            placement: self.placement,
            photo: self.photo,
            level: self.level,
            key: self.key,
            rendered: Err(e),
            gpu_fallback: None,
            loaded: None,
            quick: None,
            display: None,
            hdr_texels: None,
            frame: None,
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
        // the loupe: a cached view render of these settings at least this big is the render (a
        // built preview, or the render kept from an earlier visit, is not rendered again)
        if let Some(mut image) = self.cached_view() {
            if let Some((fw, fh)) = self.display_fit
                && (image.width > fw.max(1) || image.height > fh.max(1))
            {
                image = fit_for_display(&image, fw, fh);
            }
            let histogram = Histogram::of_srgb8(&image);
            return RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                placement: self.placement,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage: None, alpha: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                hdr_texels: None,
                frame: None,
            };
        }
        if let Some((cache, key)) = &self.cache
            && let Some(img) = cache.get_at(self.cache_generation, *key)
        {
            if let Some(kept) = &self.kept {
                kept.keep(*key, img.clone());
            }
            let image = Arc::unwrap_or_clone(img);
            let histogram = Histogram::of_srgb8(&image);
            return RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                placement: self.placement,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage: None, alpha: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                hdr_texels: None,
                frame: None,
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
                placement: self.placement,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage: None, alpha: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                hdr_texels: None,
                frame: None,
            };
        }
        let was_loaded = matches!(self.source, SourceRef::Loaded(_));
        // The app is closing (issue #620): nobody is left to show this, and quitting waits for the
        // jobs that are running. Without this, one that lost the GPU mid-way would render on the CPU.
        let closing = || lcc_gpu::shutting_down().then(|| "the app is closing".to_string());
        let source = match closing() {
            Some(e) => Err(e),
            None => self.source.load_source_of(self.source_key).and_then(|s| closing().map_or(Ok(s), Err)),
        };
        match source {
            Ok(source) if self.superseded.as_ref().is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed)) => {
                RenderResult { loaded: (!was_loaded).then_some(source), ..self.failed(SUPERSEDED.into()) }
            }
            Ok(source) => {
                // the plain picture, or (Denoise amount above 0 and a denoised picture cached) the mix the amount asks for
                let src = &source.image_for(self.settings.denoise_amount());
                // (the result if the transparency can't follow the render, made before `self.info` moves)
                let alpha_failed = self.request.alpha.then(|| self.failed("the transparency did not follow the render".into()));
                let info = source.info_or(self.info);
                // an export that keeps transparency develops the colour without the white the pixels
                // were flattened onto, and carries the alpha through the same geometry (issue #2976)
                let straight =
                    if self.request.alpha { info.alpha.as_deref().and_then(|a| lcc_codecs::unflatten(src, a)).map(Arc::new) } else { None };
                let src = straight.as_ref().unwrap_or(src);
                // (a straight picture is this job's own: no stage of an earlier render applies to it)
                let stages = if straight.is_some() { None } else { self.stages.as_deref() };
                // an over-budget brush mask is an error for the caller, not a view missing its later strokes
                if let Err(e) = lcc_pipeline::check_brush_budget(src, &info, &self.settings, &self.request) {
                    return RenderResult {
                        request_id: self.request_id,
                        source_key: self.source_key,
                        placement: self.placement,
                        photo: self.photo,
                        level: self.level,
                        key: self.key,
                        rendered: Err(e),
                        gpu_fallback: None,
                        loaded: (!was_loaded).then_some(source),
                        quick: None,
                        display: None,
                        hdr_texels: None,
                        frame: None,
                    };
                }
                // Views and exports use the GPU when there is one (waiting for the device if it is
                // still being created). Thumbnails (grid, filmstrip, preset and profile previews)
                // use it once it is ready, and never wait for it: at startup they render on the CPU.
                let gpu = self.cache.is_none() || lcc_gpu::ready();
                // the display fit on the GPU when the render is there and nothing is done to its
                // full-size image first (a monitor profile's conversion); else below, on the CPU
                let fit =
                    self.display_fit.filter(|_| self.request.quality == Quality::Full && (self.display.is_none() || self.request.display.is_none()));
                // an HDR view's texture straight from the GPU, when it renders there
                let mut hdr_texels = None;
                // the full-size render when the GPU fitted it, for the view cache
                let mut unfitted = None;
                let texels = self.hdr_texels.filter(|_| gpu && gpu_renders_faithfully(&self.settings, &self.request));
                let (mut rendered, gpu_fallback) = match texels
                    .map(|h| (h, lcc_gpu::try_render_hdr_texels(src, &info, &self.settings, &self.request, stages, h)))
                {
                    Some((headroom, Ok((r, t)))) => {
                        hdr_texels = Some((t, headroom));
                        (r, None)
                    }
                    _ if self.on_screen
                        && gpu
                        && self.cache.is_none()
                        && (self.display.is_none() || self.request.display.is_none())
                        && self.slide.is_none()
                        && straight.is_none()
                        // (a draft is never kept as the view preview)
                        && (self.view_cache.is_none() || self.drawn_view_cache || self.request.quality != Quality::Full) =>
                    {
                        match on_screen(src, &info, &self.settings, &self.request, stages) {
                            Some(OnScreen::Frame(frame, histogram)) => {
                                return RenderResult {
                                    request_id: self.request_id,
                                    source_key: self.source_key,
                                    placement: self.placement,
                                    photo: self.photo,
                                    level: self.level,
                                    key: self.key,
                                    rendered: Ok(Rendered {
                                        image: Rgba8 { width: 0, height: 0, data: Vec::new() },
                                        histogram,
                                        deep: None,
                                        warnings: Vec::new(),
                                        coverage: None,
                                        alpha: None,
                                    }),
                                    gpu_fallback: None,
                                    loaded: (!was_loaded).then_some(source),
                                    quick: None,
                                    display: None,
                                    hdr_texels: None,
                                    frame: Some(frame),
                                };
                            }
                            Some(OnScreen::Pixels(r)) => (r, None),
                            None => {
                                let (r, fallback, _) = develop_reporting_fitted(src, &info, &self.settings, &self.request, stages, gpu, fit, false);
                                (r, fallback)
                            }
                        }
                    }
                    _ => {
                        let keep_full = self.view_cache.is_some() && !self.drawn_view_cache;
                        let (r, fallback, full) = develop_reporting_fitted(src, &info, &self.settings, &self.request, stages, gpu, fit, keep_full);
                        unfitted = full;
                        (r, fallback)
                    }
                };
                if straight.is_some() {
                    rendered.alpha = lcc_pipeline::render_alpha(src, &info, &self.settings, &self.request)
                        .filter(|a| (a.width, a.height) == (rendered.image.width, rendered.image.height));
                    // (the colour is unflattened: without its alpha the file would be wrong at soft edges)
                    if let (None, Some(failed)) = (&rendered.alpha, alpha_failed) {
                        return failed;
                    }
                }
                // for a display profile: the image goes to the display's values, the histogram
                // and the cached view preview stay sRGB
                let mut srgb = None;
                let display = match (&self.display, self.request.display) {
                    (Some(d), Some(_)) => {
                        let keep = self.view_cache.is_some() && self.request.quality == Quality::Full;
                        let (s, h) = crate::display::finish_render(&mut rendered.image, d, keep, rendered.coverage.as_deref());
                        // (an HDR edit's HDR histogram stays)
                        if let Some(h) = h.filter(|_| rendered.histogram.hdr.is_none()) {
                            rendered.histogram = h;
                        }
                        srgb = s;
                        Some(d.id())
                    }
                    _ => None,
                };
                let full_quality = self.request.quality == Quality::Full;
                let meta = placement_bytes(self.placement.as_ref());
                if let Some((cache, key)) = &self.cache {
                    let img = Arc::new(rendered.image.clone());
                    cache.put_at(self.cache_generation, *key, img.clone());
                    if let Some(kept) = self.kept.as_ref().filter(|_| full_quality) {
                        kept.keep(*key, img.clone());
                    }
                    if let Some((latest, alias)) = self.latest.as_ref().filter(|_| full_quality && self.view_cache.is_none()) {
                        latest.put_latest_with(self.cache_generation, *alias, *key, img, false, meta.as_deref());
                    }
                }
                if let Some((cache, key)) = &self.view_cache
                    && full_quality
                    && (display.is_none() || srgb.is_some())
                {
                    // (one series per photo: a drag's updates replace each other on the way to disk)
                    let full = Arc::new(srgb.or_else(|| unfitted.take()).unwrap_or_else(|| rendered.image.clone()));
                    cache.put_deferred_series(self.cache_generation, self.photo.0, *key, full.clone());
                    if let Some((latest, alias)) = &self.latest {
                        latest.put_latest_with(self.cache_generation, *alias, *key, full, false, meta.as_deref());
                    }
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
                    placement: self.placement,
                    photo: self.photo,
                    level: self.level,
                    key: self.key,
                    rendered: Ok(rendered),
                    gpu_fallback,
                    loaded: (!was_loaded).then_some(source),
                    quick: None,
                    display,
                    hdr_texels,
                    frame: None,
                }
            }
            Err(e) => self.kept_or_failed(e),
        }
    }

    /// A grid thumbnail whose original is missing shows the photo's kept thumbnail ([`KeptThumb`]), fitted to the
    /// cell; a photo with none, or whose file is there but could not be read, fails with `e`. The picture is not
    /// cached under this job's key: once the original is back, its thumbnail renders afresh.
    fn kept_or_failed(&self, e: String) -> RenderResult {
        let Some(img) = self.kept.as_ref().filter(|_| self.source.original_missing()).and_then(KeptThumb::picture) else {
            return self.failed(e);
        };
        let (w, h) = (self.request.max_w, self.request.max_h);
        let image = if img.width > w.max(1) || img.height > h.max(1) {
            fit_cached(&img, w, h).unwrap_or_else(|| Arc::unwrap_or_clone(img))
        } else {
            Arc::unwrap_or_clone(img)
        };
        let histogram = Histogram::of_srgb8(&image);
        RenderResult {
            rendered: Ok(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage: None, alpha: None }),
            quick: Some(QuickSource::Kept),
            // (made with the look and crop the photo had then, which may not be its current ones)
            placement: None,
            ..self.failed(String::new())
        }
    }
}

/// Render `src`: on the GPU when `gpu` is set and a GPU is available (`lcc_gpu`), else on
/// the CPU, reusing `stages` either way. Both produce the same image within 1–3 LSB (see
/// `docs/gpu-pipeline.md`); `{ENV_PREFIX}_GPU=0` or [`lcc_gpu::set_enabled`] forces the CPU.
pub fn develop(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>, gpu: bool) -> Rendered {
    develop_reporting(src, info, s, req, stages, gpu).0
}

/// [`develop`], also saying why the render fell back from the GPU to the CPU when it did (`None`:
/// it rendered on the GPU, or the GPU was not tried). The reason is this render's own, not the
/// process-wide [`lcc_gpu::last_fallback`].
pub fn develop_reporting(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
) -> (Rendered, Option<String>) {
    let (rendered, fallback, _) = develop_reporting_fitted(src, info, s, req, stages, gpu, None, false);
    (rendered, fallback)
}

/// [`develop_reporting`], shrunk to fit `fit` on the GPU when it renders there
/// ([`lcc_gpu::try_render_fitted`]); a CPU render comes back at its own size.
fn develop_reporting_fitted(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
    fit: Option<(usize, usize)>,
    keep_full: bool,
) -> (Rendered, Option<String>, Option<Rgba8>) {
    let (mut rendered, fallback, full) = develop_image(src, info, s, req, stages, gpu, fit, keep_full);
    // an HDR edit's preview is its SDR rendition: its histogram shows the HDR range instead
    // (with the view's stage cache: a drag of an HDR edit redoes only what the slider feeds)
    let hdr = match stages {
        Some(c) => lcc_pipeline::hdr_histogram_cached(src, info, s, req, c),
        None => lcc_pipeline::hdr_histogram(src, info, s, req),
    };
    if let Some(h) = hdr {
        rendered.histogram = h;
    }
    for w in &rendered.warnings {
        log::warn!("render: {w}");
    }
    (rendered, fallback, full)
}

/// What [`on_screen`] made.
enum OnScreen {
    /// A texture on the window's device, and the histogram.
    Frame(ScreenFrame, Histogram),
    /// Pixels from the GPU (it could not hand over a texture for this render).
    Pixels(Rendered),
}

/// A render that is only drawn, on the GPU: a texture on the window's device when the GPU renderer
/// can hand one over, else its pixels. `None`: the GPU did not render it (the CPU must).
fn on_screen(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>) -> Option<OnScreen> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !lcc_gpu::on_window_device() {
            return None;
        }
        let (mut rendered, frame) = lcc_gpu::try_render_display(src, info, s, req, stages).ok()?;
        // an HDR edit's preview histogram shows the HDR range, as from `develop_reporting_fitted`
        let hdr = match stages {
            Some(c) => lcc_pipeline::hdr_histogram_cached(src, info, s, req, c),
            None => lcc_pipeline::hdr_histogram(src, info, s, req),
        };
        if let Some(h) = hdr {
            rendered.histogram = h;
        }
        Some(match frame {
            Some(f) => OnScreen::Frame(f, rendered.histogram),
            None => OnScreen::Pixels(rendered),
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src, info, s, req, stages);
        None
    }
}

/// Whether the GPU pipeline renders `s` for `req` like the CPU does. Every GPU caller (previews,
/// exports, video frames) checks this first. Since GPU-FULL every 8-bit and HDR render does
/// (`lcc_gpu::handles` declines the rest itself).
pub fn gpu_renders_faithfully(_s: &DevelopSettings, _req: &RenderRequest) -> bool {
    // LUT profiles, Refine Edges, mask Feather and matrix soft proofs all render on the GPU
    // (an output-device proof's table is declined by `lcc_gpu::handles` itself)
    true
}

fn develop_image(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
    fit: Option<(usize, usize)>,
    keep_full: bool,
) -> (Rendered, Option<String>, Option<Rgba8>) {
    let fallback = if gpu && gpu_renders_faithfully(s, req) {
        let r = match fit.filter(|_| keep_full) {
            Some(size) => lcc_gpu::try_render_fitted_keeping_full(src, info, s, req, stages, size),
            None => lcc_gpu::try_render_fitted(src, info, s, req, stages, fit).map(|r| (r, None)),
        };
        match r {
            Ok((r, full)) => return (r, None, full),
            Err(lcc_gpu::Fallback::Failed(why)) => Some(why),
            Err(lcc_gpu::Fallback::NotTried) => None,
        }
    } else {
        None
    };
    let rendered = match stages {
        Some(st) => lcc_pipeline::render_cached(src, info, s, req, st),
        None => lcc_pipeline::render(src, info, s, req),
    };
    (rendered, fallback, None)
}

/// Run `f`, the renders of one photo made ahead of the user (a source of `src_px` pixels, results
/// of at most `out_px`; Build Previews, the background preparation, the import's previews), as
/// batch work: they wait for a turn on whichever graphics card has room and would finish them
/// soonest ([`lcc_gpu::wait_batch_turn_any`]: with a second card, both cards get photos; the
/// editing card only while the interface is idle, behind the photo on screen) and all run on that
/// card. Without a GPU (or while the app closes) they render on the processor. A panic inside `f`
/// is its error (`what` names the work); the thread's routing is restored whatever happens.
pub fn background_photo<R>(what: &str, src_px: usize, out_px: usize, f: impl FnOnce() -> R) -> Result<R, String> {
    lcc_gpu::as_batch(|| match lcc_gpu::wait_batch_turn_any(src_px, out_px, &lcc_gpu::shutting_down) {
        Some(turn) => {
            let r = crate::guard::catch(what, f);
            drop(turn);
            r
        }
        None => lcc_gpu::on_cpu(|| crate::guard::catch(what, f)),
    })
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
    /// The photo's latest render, made with earlier settings (an edited photo whose render for its
    /// current settings is on its way: its edit as last seen, never the unedited camera JPEG).
    Last,
    /// A fast draft of an edited raw with its settings, developed from the camera's JPEG instead of
    /// the sensor data (crop, tone and colour edits as they will look, white balance as shot): what
    /// an edited photo that was never rendered shows at once, until its render arrives.
    Draft,
    /// The photo's kept grid thumbnail ([`KeptThumb`]): its original is missing. Shown until the original is
    /// back; made with whatever look and crop the photo had then.
    Kept,
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
    /// Where a decoded [`Self::embedded`] preview is kept in memory, so stepping back to a photo
    /// shows its camera JPEG again without reading and decoding it (culling goes back and forth):
    /// the cache (memory only, never its disk) and the key for this photo and edge.
    pub embedded_cache: Option<(Arc<PreviewCache>, Hash128)>,
    /// Whether [`Self::embedded_cache`] keeps the decoded preview on disk too: a grid thumbnail,
    /// so a folder seen before fills in from small files at once instead of every raw being read
    /// and its camera JPEG decoded again (the loupe's large stand-ins stay in memory).
    pub embedded_on_disk: bool,
    /// Last resort: run this thumbnail job.
    pub small: Option<Box<RenderJob>>,
    /// The photo's latest pictures (aliases, [`PreviewCache::get_latest_with`]), tried after
    /// `cached` and before `embedded` ([`QuickSource::Last`]).
    pub latest: Vec<(Arc<PreviewCache>, Hash128)>,
    /// Shrink a picture taken from the caches to fit this (a loupe view standing in for a grid
    /// thumbnail is not kept at 2 000 px for a 256 px cell).
    pub fit: Option<(usize, usize)>,
    /// A fast draft of an edited raw with its settings ([`QuickSource::Draft`]): the camera JPEG at
    /// (path, loader, max edge) developed by this job, tried after `latest` and before `small`.
    pub draft: Option<(String, PreviewLoader, usize, Box<RenderJob>)>,
    /// The placement of the picture asked for (the photo's current crop and look). A latest picture
    /// stands in only when it was made with a look close to this one and holds every pixel of the
    /// view ([`crate::placement::may_stand_in`]: never the browse-time preview of a photo imported
    /// with a preset); `None`: any latest picture does.
    pub want: Option<crate::placement::Placement>,
    /// The grid thumbnail job's kept thumbnail ([`KeptThumb`]): a thumbnail of the photo's own settings shown from
    /// `cached` or `embedded` is kept as it. Set by [`crate::Session::quick_thumb_job`] only (`None`: the loupe's
    /// stand-ins, whose `cached` are views, never a grid thumbnail).
    pub kept: Option<KeptThumb>,
}

impl QuickJob {
    /// The stand-in, from the first source that has one. A panic inside is this job's failure
    /// (see [`RenderJob::run`]).
    pub fn run(self) -> RenderResult {
        let (request_id, photo, key) = (self.request_id, self.photo, self.key);
        crate::guard::catch("the render", || self.run_unguarded()).unwrap_or_else(|e| RenderResult {
            request_id,
            source_key: None,
            placement: None,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered: Err(e),
            gpu_fallback: None,
            loaded: None,
            quick: None,
            display: None,
            hdr_texels: None,
            frame: None,
        })
    }

    fn run_unguarded(self) -> RenderResult {
        let (photo, key) = (self.photo, self.key);
        let request_id = self.request_id;
        let want = self.want;
        let done = |rendered: Result<Rendered, String>, quick, placement| RenderResult {
            request_id,
            source_key: None,
            placement,
            photo,
            level: SourceLevel::Thumb,
            key,
            rendered,
            gpu_fallback: None,
            loaded: None,
            quick,
            display: None,
            hdr_texels: None,
            frame: None,
        };
        let rendered = |image: Rgba8| {
            let histogram = Histogram::of_srgb8(&image);
            Ok(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage: None, alpha: None })
        };
        let fitted = |img: Arc<Rgba8>| match self.fit {
            Some((w, h)) if img.width > w.max(1) || img.height > h.max(1) => fit_cached(&img, w, h).unwrap_or_else(|| Arc::unwrap_or_clone(img)),
            _ => Arc::unwrap_or_clone(img),
        };
        let kept = self.kept.as_ref();
        for (cache, k) in &self.cached {
            if let Some(img) = cache.get(*k) {
                if let Some(kept) = kept {
                    kept.keep(*k, img.clone());
                }
                return done(rendered(fitted(img)), Some(QuickSource::Cached), want);
            }
        }
        // a latest picture with the look asked for first, then the thumbnail of the photo's settings
        // (built already), and only then a latest picture with a look close to it: a photo edited
        // since (an Auto Sync edit) shows its edit at once, never the picture from before it while
        // its thumbnail is there
        let latest: Vec<(Arc<Rgba8>, Option<crate::placement::Placement>)> = self
            .latest
            .iter()
            .filter_map(|(cache, k)| cache.get_latest_with(*k))
            .map(|(img, meta)| (img, placement_from(meta.as_deref())))
            .filter(|(img, have)| match (have, &want) {
                (_, None) => true,
                (Some(h), Some(w)) => crate::placement::may_stand_in(h, w, STAND_IN_SLACK) && crate::placement::fits_shape(h, img.width, img.height),
                // (made before pictures said where they lie: its crop and look are unknown)
                (None, Some(_)) => false,
            })
            .collect();
        let exact = |have: &Option<crate::placement::Placement>| match (have, &want) {
            (Some(h), Some(w)) => h.look == w.look,
            _ => true,
        };
        if let Some((img, have)) = latest.iter().find(|(_, have)| exact(have)) {
            return done(rendered(fitted(img.clone())), Some(QuickSource::Last), *have);
        }
        if let Some(job) = self.small.as_deref()
            && let Some((cache, k)) = &job.cache
            && let Some(img) = cache.get_at(job.cache_generation, *k)
        {
            return done(rendered(Arc::unwrap_or_clone(img)), Some(QuickSource::Small), want.or(job.placement));
        }
        if let Some((img, have)) = latest.into_iter().next() {
            return done(rendered(fitted(img)), Some(QuickSource::Last), have);
        }
        if let Some((path, loader, edge, job)) = self.draft.as_ref()
            && let Some(r) = draft_from_camera_jpeg(path, loader, *edge, job)
        {
            return RenderResult { request_id, key, quick: Some(QuickSource::Draft), placement: want, ..r };
        }
        if let Some((path, loader, edge)) = &self.embedded {
            if let Some((cache, k)) = &self.embedded_cache
                && let Some(img) = if self.embedded_on_disk { cache.get(*k) } else { cache.get_in_memory(*k) }
            {
                if let Some(kept) = kept.filter(|_| self.embedded_on_disk) {
                    kept.keep(*k, img.clone());
                }
                return done(rendered(Arc::unwrap_or_clone(img)), Some(QuickSource::Embedded), want);
            }
            if let Some(image) = loader(path, *edge) {
                if let Some((cache, k)) = &self.embedded_cache {
                    if self.embedded_on_disk {
                        let img = Arc::new(image.clone());
                        cache.put_deferred(*k, img.clone());
                        if let Some(kept) = kept {
                            kept.keep(*k, img);
                        }
                    } else {
                        cache.put_memory(*k, Arc::new(image.clone()));
                    }
                }
                return done(rendered(image), Some(QuickSource::Embedded), want);
            }
        }
        if let Some(job) = self.small {
            let r = job.run();
            // (the photo's kept thumbnail, its original missing, says so and claims no placement)
            if r.quick == Some(QuickSource::Kept) {
                return RenderResult { request_id, key, ..r };
            }
            return RenderResult { request_id, key, quick: Some(QuickSource::Small), placement: want.or(r.placement), ..r };
        }
        done(Err("no quick preview".into()), None, None)
    }
}

/// How far (in texture units) a latest picture may fall short of the view it stands in for: the
/// rounding of whole pixels, never a visible strip.
const STAND_IN_SLACK: f64 = 0.004;

/// The longest edge a draft from the camera JPEG is developed at ([`QuickSource::Draft`]).
pub const DRAFT_EDGE: usize = 1600;

/// Develop `job` (an edited raw's render job) from its camera JPEG (`loader` at `path`, at most
/// `edge` px) instead of its sensor data: a draft with its settings, white balance as shot (the
/// JPEG's colours are the camera's, on no Kelvin scale). `None`: the JPEG can't be read, or the
/// render failed.
fn draft_from_camera_jpeg(path: &str, loader: &PreviewLoader, edge: usize, job: &RenderJob) -> Option<RenderResult> {
    let jpeg = loader(path, edge.clamp(1, DRAFT_EDGE))?;
    if jpeg.width == 0 || jpeg.height == 0 || jpeg.data.len() != jpeg.width.saturating_mul(jpeg.height) {
        return None;
    }
    let lin: Vec<f32> = (0..=255u8).map(|v| lcc_color::transfer::srgb_to_linear(f32::from(v) / 255.0)).collect();
    let to_working = lcc_color::SRGB.to_space(&lcc_color::WORKING);
    let px = |c: u8| lin.get(usize::from(c)).copied().unwrap_or(0.0);
    let data = jpeg.data.iter().map(|p| to_working.apply_f32([px(p[0]), px(p[1]), px(p[2])])).collect();
    let image = Rgb32f { width: jpeg.width, height: jpeg.height, data };
    let info = SourceInfo {
        raw: false,
        as_shot_temp: 6500.0,
        as_shot_tint: 0.0,
        relative_wb: true,
        native_long: job.info.native_long,
        ..Default::default()
    };
    let mut settings = (*job.settings).clone();
    settings.wb = lcc_develop::WhiteBalance::default();
    let mut job = job.clone();
    job.source = SourceRef::Loaded(Box::new(DecodedSource::new(Arc::new(image), Some(info.clone()))));
    job.info = info;
    job.settings = Arc::new(settings);
    job.cache = None;
    job.view_cache = None;
    job.latest = None;
    job.kept = None;
    job.stages = None;
    job.on_screen = false;
    job.preview_cut = None;
    job.hdr_texels = None;
    let mut r = job.draft().run();
    r.loaded = None;
    r.source_key = None;
    r.rendered.is_ok().then_some(r)
}

/// The alias of photo `p`'s latest loupe view, whatever settings it was rendered with
/// ([`PreviewCache::put_latest_with`]): an edited photo's first picture while the render for its
/// current settings is on its way.
pub fn latest_view_key(p: &Photo, apply_crop: bool) -> Hash128 {
    Hasher128::new().str(&pixels_key(p)).str("latest-view").u64(apply_crop as u64).u64(RENDER_CACHE_VERSION).finish()
}

/// The alias of photo `p`'s latest grid thumbnail, whatever settings it was rendered with.
pub fn latest_thumb_key(p: &Photo) -> Hash128 {
    Hasher128::new().str(&pixels_key(p)).str("latest-thumb").u64(RENDER_CACHE_VERSION).finish()
}

/// The alias of photo `p`'s last grid thumbnail whatever made it: its settings, the renderer version, the camera
/// profiles and the sensor maps ([`KeptThumb`]). Its picture is read only when the original is missing (the record
/// kept with it is read whenever a thumbnail is kept). The photo's id is in it: a virtual copy shares its master's
/// pixels, not its look.
pub fn kept_thumb_key(p: &Photo) -> Hash128 {
    Hasher128::new().str(&pixels_key(p)).str("kept-thumb").u64(p.id.0).finish()
}

/// The sensor-map part of photo `p`'s preview keys: only its own camera body's corrections change it.
pub(crate) fn sensor_key(p: &Photo) -> u64 {
    crate::sensor_maps::preview_key(p.develops_raw(), &p.meta.camera_serial, &p.file_name)
}

/// A grid thumbnail's cache key: the photo's pixels (`content_key`), its settings, the bucket, the renderer
/// `version`, the camera profiles, the `sensor` maps ([`sensor_key`]), and (a render from a denoised picture) its
/// `salt`.
fn thumb_key(content_key: &str, settings_hash: u64, bucket: usize, version: u64, sensor: u64, salt: u64) -> Hash128 {
    let mut k = Hasher128::new();
    k.str(content_key).u64(settings_hash).u64(bucket as u64).u64(version).u64(crate::camera_profiles::cache_key()).u64(sensor);
    // (only a render that uses a denoised picture has it in its key, so every other thumbnail stays valid)
    if salt != 0 {
        k.u64(salt);
    }
    k.finish()
}

/// How many renderer versions back a missing photo's thumbnail is looked for ([`KeptThumb::picture`]).
pub(crate) const KEPT_VERSIONS: u64 = 8;

/// Where a grid thumbnail of the photo's own settings keeps its photo's last picture, and how to find one an
/// earlier version of the app rendered: what the cell shows while the original is missing (a drive that isn't
/// mounted), when an update or a camera profile has changed the thumbnail's key since the photo was last seen.
/// Never read for a photo whose original is there: that one renders afresh.
#[derive(Clone)]
pub struct KeptThumb {
    cache: Arc<PreviewCache>,
    /// The cache's generation when the job was made: a job from before a clear writes nothing.
    generation: u64,
    /// [`kept_thumb_key`].
    alias: Hash128,
    content_key: String,
    settings_hash: u64,
    bucket: usize,
    version: u64,
    /// [`sensor_key`].
    sensor: u64,
    salt: u64,
}

impl KeptThumb {
    /// `img`, this job's thumbnail kept under `real` (just rendered, or shown from the cache), is also the photo's
    /// kept thumbnail, unless the kept one has this look already at this size or larger. So a cell shown from the
    /// cache keeps what it shows (a thumbnail cached before photos kept one; a look an undo or a reset went back to),
    /// without a write on every visit or when the filmstrip shows the photo smaller: the record kept with the copy
    /// names its look and size. (The copy itself is looked for too: the disk cache may have let it go and kept the
    /// record.)
    fn keep(&self, real: Hash128, img: Arc<Rgba8>) {
        let mut record = thumb_key(&self.content_key, self.settings_hash, 0, self.version, self.sensor, self.salt).0.to_le_bytes().to_vec();
        let look = record.clone();
        record.extend_from_slice(&(self.bucket as u64).to_le_bytes());
        let has = self.cache.latest_meta(self.alias);
        let same_look = has.as_deref().and_then(|r| r.get(..16)) == Some(&look[..]);
        let size = has.as_deref().and_then(|r| r.get(16..24)).and_then(|b| <[u8; 8]>::try_from(b).ok()).map(u64::from_le_bytes);
        if same_look && size.is_some_and(|s| s >= self.bucket as u64) && self.cache.has(self.alias) {
            return;
        }
        self.cache.put_latest_with(self.generation, self.alias, real, img, false, Some(&record));
    }

    /// The picture to show for a photo whose original is missing: its kept thumbnail, else a thumbnail of these
    /// settings cached by this version or one of the [`KEPT_VERSIONS`] before it (this bucket first, then the
    /// others), rendered or the camera JPEG kept in its place. Those older ones are looked for under today's camera
    /// profiles and sensor maps: one cached before either changed is not found. `None`: nothing of this photo is
    /// cached.
    fn picture(&self) -> Option<Arc<Rgba8>> {
        if let Some(img) = self.cache.get_latest(self.alias) {
            return Some(img);
        }
        let buckets = std::iter::once(self.bucket).chain(THUMB_SIZES.into_iter().filter(|b| *b != self.bucket));
        let buckets: Vec<usize> = buckets.collect();
        (0..=KEPT_VERSIONS)
            .filter_map(|back| self.version.checked_sub(back))
            .flat_map(|v| buckets.iter().map(move |b| (v, *b)))
            .map(|(v, b)| thumb_key(&self.content_key, self.settings_hash, b, v, self.sensor, self.salt))
            .flat_map(|k| [k, embedded_thumb_key(k)])
            .filter(|k| self.cache.has(*k))
            .find_map(|k| self.cache.get(k))
    }
}

/// A placement as the record kept with a latest picture ([`PreviewCache::put_latest_with`]).
pub(crate) fn placement_bytes(p: Option<&crate::placement::Placement>) -> Option<Vec<u8>> {
    serde_json::to_vec(p?).ok()
}

/// The placement kept with a latest picture (`None`: none kept, or unreadable).
fn placement_from(bytes: Option<&[u8]>) -> Option<crate::placement::Placement> {
    serde_json::from_slice(bytes?).ok()
}

/// The thumbnail cache's key for the camera JPEG standing for the thumbnail of cache key `k`.
fn embedded_thumb_key(k: Hash128) -> Hash128 {
    Hasher128::new().str("embedded-thumb").u64((k.0 >> 64) as u64).u64(k.0 as u64).finish()
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
    let info = source_info_plain(p);
    // faces are never a source for content-aware fill
    let faces: Vec<[f64; 4]> = p
        .meta
        .regions
        .iter()
        .filter(|r| r.kind == lcc_meta::RegionKind::Face)
        .map(|r| [r.rect.x0, r.rect.y0, r.rect.x1, r.rect.y1])
        .filter(|b| b.iter().all(|v| v.is_finite()))
        .take(256)
        .collect();
    SourceInfo { avoid: (!faces.is_empty()).then(|| Arc::new(faces)), identity: source_identity(p), ..info }
}

/// [`SourceInfo::identity`] of `p`: its pixels' key ([`pixels_key`]), the same at every decoded
/// size (`None` for the procedural demo scenes, drawn afresh at each size).
fn source_identity(p: &Photo) -> Option<lcc_pipeline::SourceIdentity> {
    if matches!(p.source, Source::Demo { .. }) {
        return None;
    }
    let key = Hasher128::new().str("source-identity").str(&pixels_key(p)).finish().0 as u64;
    Some(lcc_pipeline::SourceIdentity { key, size: [p.width as usize, p.height as usize] })
}

fn source_info_plain(p: &Photo) -> SourceInfo {
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
            lens: p.embedded_lens.as_deref().copied(),
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
    /// What a render of `p` is interpreted against: [`source_info`], the photo's generated patches and the catalog's
    /// flat-field maps its settings use.
    pub fn job_info(&self, p: &Photo) -> SourceInfo {
        SourceInfo { generated: self.genfill_store(p), vignetting_maps: self.vignetting_calibration_store(p), ..source_info(p) }
    }

    /// The catalog's flat-field maps photo `p` uses (its settings, History steps, Versions, Before), for its renders;
    /// `None` when it uses none.
    pub fn vignetting_calibration_store(&self, p: &Photo) -> Option<lcc_pipeline::vignetting_calibration::VignettingMapStore> {
        let maps: std::collections::BTreeMap<lcc_develop::VignettingMapId, Arc<lcc_develop::EmbeddedVignettingMap>> =
            p.vignetting_map_ids().filter_map(|id| self.catalog.vignetting_map(id).map(|r| (id.clone(), Arc::new(r.map.clone())))).collect();
        (!maps.is_empty()).then(|| lcc_pipeline::vignetting_calibration::VignettingMapStore(Arc::new(maps)))
    }

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
        let p = self.rendered_photo_of(id)?;
        let settings = if before { Arc::new(self.before_settings(&p)) } else { p.develop.clone() };
        // a clip's poster is graded and cropped like its frames (`lcc_pipeline::grade::video_develop`)
        let settings = if p.kind == MediaKind::Video { Arc::new(lcc_pipeline::grade::video_develop(&settings)) } else { settings };
        // the crop tool's uncropped view doesn't depend on the crop box or the straighten angle: a
        // drag of either must not re-render it (issue #60)
        let uncropped = !apply_crop && !settings.crop.geometry.is_identity();
        let settings = if uncropped { Arc::new(without_crop(&settings)) } else { settings };
        let level = SourceLevel::for_size(if thumb_bucket.is_some() {
            max_w.max(max_h) // (grid thumbnails of tight crops stay cheap)
        } else {
            source_edge_needed(&p, &settings, apply_crop, max_w.max(max_h))
        });
        let source = self.media.source_ref(&p, level);
        // previews shrink as exports do once they shrink a lot (full quality only): sharpening tuned in the
        // loupe then matches the export's Lanczos-3 downscale; grid thumbnails stay soft
        let downscale = if thumb_bucket.is_none() { lcc_pipeline::Downscale::Preview } else { lcc_pipeline::Downscale::Smooth };
        let request = RenderRequest { apply_crop, downscale, ..RenderRequest::fit(max_w, max_h) };
        // the photo id is part of the key: two photos with the same settings and size must not
        // share a result (a view slot showing photo A would otherwise look current for photo B)
        // …and so is the file's content: a file changed on disk (Reload) renders afresh
        let content_key = pixels_key(&p);
        let source_key = Hasher128::new().str(&content_key).finish();
        let content = source_key.0 as u64;
        // Temporary "before" settings must not replace the photo's cached develop hash.
        let settings_hash = if before || uncropped { settings.hash64() } else { self.media.settings_hashes.get(id, &settings) };
        // (AI masks on an automatic selection re-render when the Subject or scene model changes)
        let models = if lcc_pipeline::auto_mask::needed(&settings) {
            lcc_pipeline::auto_mask::model_generation().wrapping_add(1).wrapping_mul(0x2545_f491_4f6c_dd1d)
        } else {
            0
        };
        let salt = self.media.denoise_salt(id, &settings) ^ models;
        let key = settings_hash
            ^ ((max_w as u64) << 40)
            ^ ((max_h as u64) << 20)
            ^ (apply_crop as u64)
            ^ (level as u64) << 60
            ^ id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ content.rotate_left(17)
            ^ salt;
        let version = self.media.thumb_version;
        let sensor = sensor_key(&p);
        let cache = thumb_bucket.map(|b| (self.media.rendered.clone(), thumb_key(&content_key, settings_hash, b, version, sensor, salt)));
        // a thumbnail of the photo's own settings is its latest grid picture too, and the one its cell
        // keeps showing while the original is missing
        let latest = cache.as_ref().filter(|_| !before).map(|(c, _)| (c.clone(), latest_thumb_key(&p)));
        let kept = cache.as_ref().zip(thumb_bucket).filter(|_| !before && apply_crop).map(|((c, _), bucket)| KeptThumb {
            cache: c.clone(),
            generation: c.generation(),
            alias: kept_thumb_key(&p),
            content_key: content_key.clone(),
            settings_hash,
            bucket,
            version,
            sensor,
            salt,
        });
        let placement = (!before).then(|| self.placement_with(&p, &settings, apply_crop));
        Some(RenderJob {
            drawn_view_cache: false,
            on_screen: false,
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(source_key),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: self.job_info(&p),
            settings,
            request,
            key,
            cache,
            stages: None,
            view_cache: None,
            latest,
            kept,
            placement,
            display: None,
            slide: None,
            preview_cut: None,
            display_fit: None,
            hdr_texels: None,
            superseded: None,
        })
    }

    /// Where a render of photo `id` with its current settings lies in it, and its look
    /// ([`crate::placement::Placement`]; `apply_crop`: cropped, as the loupe and grid show it).
    pub fn placement_of(&mut self, id: PhotoId, apply_crop: bool) -> Option<crate::placement::Placement> {
        let p = self.catalog.photo(id)?.clone();
        let settings = p.develop.clone();
        Some(self.placement_with(&p, &settings, apply_crop))
    }

    fn placement_with(&mut self, p: &Photo, settings: &Arc<DevelopSettings>, apply_crop: bool) -> crate::placement::Placement {
        if let Some((s, pl)) = self.media.placements.get(&(p.id, apply_crop))
            && s.as_ptr() == Arc::as_ptr(settings)
        {
            return *pl;
        }
        let pl = crate::placement::Placement::of(p, settings, apply_crop);
        if self.media.placements.len() >= 2 * SETTINGS_HASH_ENTRIES {
            self.media.placements.clear();
        }
        self.media.placements.insert((p.id, apply_crop), (Arc::downgrade(settings), pl));
        pl
    }

    /// A render of `id` with `settings` in place of its own, fitting `max_w × max_h` (the loupe's
    /// temporary preview while hovering a preset or profile; nothing is committed or cached).
    pub fn preview_job(&mut self, id: PhotoId, max_w: usize, max_h: usize, apply_crop: bool, settings: &DevelopSettings) -> Option<RenderJob> {
        let mut job = self.render_job(id, max_w, max_h, false, apply_crop)?;
        job.settings = Arc::new(settings.clone());
        job.latest = None;
        job.kept = None;
        job.placement = None;
        job.key = settings.hash64() ^ ((max_w as u64) << 40) ^ ((max_h as u64) << 20) ^ (apply_crop as u64) ^ (job.level as u64) << 60;
        Some(job)
    }

    /// Cache key of a variant thumbnail ([`Self::variant_job`]).
    pub fn variant_key(p: &Photo, settings: &DevelopSettings, edge: usize) -> Hash128 {
        Hasher128::new()
            .str(&pixels_key(p))
            .str("variant")
            .u64(settings.hash64())
            .u64(edge as u64)
            .u64(RENDER_CACHE_VERSION)
            .u64(crate::camera_profiles::cache_key())
            .u64(sensor_key(p))
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
            drawn_view_cache: false,
            on_screen: false,
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(Hasher128::new().str(&pixels_key(&p)).finish()),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: self.job_info(&p),
            settings: Arc::new(settings.clone()),
            request: RenderRequest { apply_crop: true, ..RenderRequest::fit(edge, edge) },
            key: (ck.0 as u64) ^ ((ck.0 >> 64) as u64),
            cache: Some((self.media.rendered.clone(), ck)),
            stages: None,
            view_cache: None,
            display: None,
            slide: None,
            preview_cut: None,
            display_fit: None,
            hdr_texels: None,
            superseded: None,
            latest: None,
            kept: None,
            placement: None,
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
    pub fn face_job(&mut self, id: PhotoId, face: lcc_geom::Rect, edge: usize) -> Option<RenderJob> {
        let p = self.rendered_photo_of(id)?;
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
        let rect = lcc_geom::Rect { x0: x0 / w, y0: y0 / h, x1: (x0 + side) / w, y1: (y0 + side) / h };
        let mut settings = (*p.develop).clone();
        settings.crop = lcc_develop::Crop { geometry: lcc_geom::CropGeometry { rect, angle: 0.0 }, ..Default::default() };
        let edge = face_edge(edge);
        // pixels along the long edge that leave `edge` across the crop
        let needed = (edge as f64 * w.max(h) / side).ceil().min(SourceLevel::Preview.max_edge() as f64);
        let level = SourceLevel::for_size(needed as usize);
        let source = self.media.source_ref(&p, level);
        let ck = Self::variant_key(&p, &settings, edge);
        let preview_cut = self.face_preview_cut(&p, rect, edge, needed as usize);
        Some(RenderJob {
            drawn_view_cache: false,
            on_screen: false,
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: Some(Hasher128::new().str(&pixels_key(&p)).finish()),
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: self.job_info(&p),
            settings: Arc::new(settings),
            request: RenderRequest { apply_crop: true, ..RenderRequest::fit(edge, edge) },
            key: (ck.0 as u64) ^ ((ck.0 >> 64) as u64),
            cache: Some((self.media.rendered.clone(), ck)),
            stages: None,
            view_cache: None,
            display: None,
            slide: None,
            preview_cut,
            display_fit: None,
            hdr_texels: None,
            superseded: None,
            latest: None,
            kept: None,
            placement: None,
        })
    }

    /// How a face tile of `p` can come from its embedded preview ([`crate::face_tiles`]): only an
    /// unedited raw (whose grid thumbnail is that preview too) not known to be offline, upright as
    /// imported, with a preview loader installed. `rect` is the square around the face, `edge` the
    /// tile, `needed` the source's long edge the develop render would ask for.
    fn face_preview_cut(&self, p: &Photo, rect: lcc_geom::Rect, edge: usize, needed: usize) -> Option<crate::face_tiles::PreviewCut> {
        let (path, loader) = self.embedded_of(p)?;
        if p.develop.orientation != lcc_geom::Orientation::Normal || self.media.availability.is_offline(&path) || p.width == 0 || p.height == 0 {
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
            .str(&pixels_key(p))
            .str("view")
            .u64(if apply_crop { p.develop.hash64() } else { without_crop(&p.develop).hash64() })
            .u64(apply_crop as u64)
            .u64(RENDER_CACHE_VERSION)
            .u64(crate::camera_profiles::cache_key())
            .u64(sensor_key(p))
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
        let p = self.rendered_photo_of(id)?;
        job.view_cache = Some((self.media.rendered.clone(), Self::view_key(&p, apply_crop)));
        job.latest = Some((self.media.rendered.clone(), latest_view_key(&p, apply_crop)));
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
        window: lcc_pipeline::PixelWindow,
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
        window: lcc_pipeline::PixelWindow,
        apply_crop: bool,
    ) -> Option<RenderJob> {
        self.region_job_of(id, full_w, full_h, window, apply_crop, true)
    }

    fn region_job_of(
        &mut self,
        id: PhotoId,
        full_w: usize,
        full_h: usize,
        window: lcc_pipeline::PixelWindow,
        apply_crop: bool,
        before: bool,
    ) -> Option<RenderJob> {
        let mut job = self.render_job(id, full_w, full_h, before, apply_crop)?;
        // what the window's spots and Auto Mask strokes read must fit in one render: else the
        // caller keeps the whole-frame render (a window alone would come out wrong)
        let p = self.catalog.photo(id)?;
        let frame = lcc_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &job.settings,
            apply_crop,
            p.embedded_lens.as_deref(),
        );
        let ppl = frame.px_per_long(full_w);
        lcc_pipeline::spots::window_for_reads_checked(&job.settings, &frame, full_w, full_h, ppl, window.clamped(full_w, full_h))?;
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
        self.embedded_stand_in_of(p).filter(|_| crate::import::has_import_look(p))
    }

    /// The embedded preview of any raw (path, loader), when the app installed a loader: the
    /// cheapest full picture of the photo, shown until the app's render of it arrives. A photo
    /// with any setting changed (a body's default camera profile picked for a whole folder is
    /// enough) otherwise had no picture in the grid, the filmstrip or the loupe until a full raw
    /// render, seconds a photo for a 45 MP CR3. Only an unedited raw's is its final picture
    /// ([`Self::embedded_thumb_is_final`]); for the others a render replaces it.
    fn embedded_stand_in_of(&self, p: &Photo) -> Option<(String, PreviewLoader)> {
        match (&p.source, &self.media.preview_loader) {
            (Source::File { path }, Some(l)) if p.kind == MediaKind::Raw => Some((path.clone(), l.clone())),
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

    /// The photo's 1:1 preview (Library → Previews → Build 1:1 Previews) for its current look, if
    /// it is held in memory: what a zoomed loupe shows at once while its tiles render. With
    /// `load`, one held only on disk is read into memory on a background thread for next time.
    pub fn full_preview_in_memory(&self, id: PhotoId, load: bool) -> Option<Arc<lcc_raster::Rgba8>> {
        let p = self.rendered_photo_of(id)?;
        let key = Self::full_view_key(&p);
        if let Some(img) = self.media.rendered.get_in_memory(key) {
            return Some(img);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if load && self.media.rendered.contains(key) {
            let cache = self.media.rendered.clone();
            if std::thread::Builder::new().name("full-preview-load".into()).spawn(move || drop(cache.get(key))).is_err() {
                log::warn!("could not start a preview loader thread");
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = load;
        None
    }

    /// Something to show in the loupe right away for `id` (see [`QuickJob`]): its cached view
    /// render, else its latest render with earlier settings when it may stand in (a close look, cut
    /// to the current crop), else for an edited raw a fast draft of its edit from the camera JPEG,
    /// else the camera JPEG of a raw as shot, else a cached or fresh thumbnail. An edited photo never
    /// gets the camera's JPEG: it is unedited and uncropped ("land on a photo I just edited and you
    /// see the camera JPEG").
    pub fn quick_view_job(&mut self, id: PhotoId, max_edge: usize, apply_crop: bool) -> Option<QuickJob> {
        let p = self.rendered_photo_of(id)?;
        let small = self.thumb_job(id, THUMB_SIZES[THUMB_SIZES.len() - 1])?;
        // (the thumbnail job itself starts with the cached thumbnail, after the sharper embedded preview)
        let mut cached = vec![(self.media.rendered.clone(), Self::view_key(&p, apply_crop))];
        // a 1:1 preview (Build 1:1 Previews) stands in when there is no standard one
        if apply_crop {
            cached.push((self.media.rendered.clone(), Self::full_view_key(&p)));
        }
        let h = Hasher128::new().str(&pixels_key(&p)).str("quick").u64(p.develop.hash64()).u64(apply_crop as u64).finish();
        let key = h.0 as u64;
        let as_shot = crate::import::looks_as_shot(&p);
        let want = self.placement_with(&p, &p.develop, apply_crop);
        // an edited photo: its last view, else its last grid thumbnail (its edit, if older); a photo
        // as shot has its camera JPEG, sharper than a thumbnail
        let latest = if as_shot {
            Vec::new()
        } else {
            vec![(self.media.rendered.clone(), latest_view_key(&p, apply_crop)), (self.media.rendered.clone(), latest_thumb_key(&p))]
        };
        let camera_jpeg = self.embedded_stand_in_of(&p);
        // an edited raw never rendered: a fast draft with its settings from the camera JPEG
        let draft = match (&camera_jpeg, as_shot) {
            (Some((path, l)), false) => {
                let e = max_edge.clamp(1, DRAFT_EDGE);
                self.render_job(id, e, e, false, apply_crop).map(|j| (path.clone(), l.clone(), e, Box::new(j)))
            }
            _ => None,
        };
        let embedded = camera_jpeg.filter(|_| as_shot).map(|(path, l)| (path, l, max_edge.clamp(1, SourceLevel::Preview.max_edge())));
        let embedded_cache = embedded
            .as_ref()
            .map(|(_, _, edge)| (self.media.rendered.clone(), Hasher128::new().str(&pixels_key(&p)).str("embedded").u64(*edge as u64).finish()));
        Some(QuickJob {
            request_id: 0,
            photo: id,
            key,
            cached,
            embedded,
            embedded_cache,
            embedded_on_disk: false,
            small: apply_crop.then(|| Box::new(small)),
            latest,
            fit: None,
            draft,
            want: Some(want),
            kept: None,
        })
    }

    /// The camera's embedded JPEG of a raw as shot at up to `max_edge` pixels (its full size for a
    /// camera that stores one), for the loupe zoomed to 100 % while the app renders the window on
    /// screen: a focus check needs the pixels at once, and a 45 MP render of the window takes
    /// seconds. Not kept in any cache (a full-size picture is 180 MB). `None` for other photos.
    pub fn embedded_full_job(&mut self, id: PhotoId, max_edge: usize) -> Option<QuickJob> {
        let p = self.catalog.photo(id)?.clone();
        if !crate::import::looks_as_shot(&p) {
            return None;
        }
        let (path, l) = self.embedded_stand_in_of(&p)?;
        let key = Hasher128::new().str(&pixels_key(&p)).str("embedded-full").u64(max_edge as u64).finish().0 as u64;
        Some(QuickJob {
            request_id: 0,
            photo: id,
            key,
            cached: Vec::new(),
            embedded: Some((path, l, max_edge.max(1))),
            embedded_cache: None,
            embedded_on_disk: false,
            small: None,
            latest: Vec::new(),
            fit: None,
            draft: None,
            want: None,
            kept: None,
        })
    }

    /// A first grid thumbnail for a raw: the cached render for `job` (the real
    /// thumbnail job, from [`Self::thumb_job`]) if there is one — then the result carries
    /// [`QuickSource::Cached`] and `job.key` and is final — else its embedded preview, else `job`
    /// itself: the grid queues no render behind an unedited raw's quick thumbnail
    /// ([`Self::embedded_thumb_is_final`]), so a preview that can't be read must not leave the
    /// cell empty (issue #1448). For an edited raw the camera JPEG is only a stand-in: the grid
    /// renders the real thumbnail behind it ([`Self::embedded_thumb_is_final`]). `None` for other
    /// photos (render them directly).
    ///
    /// An edited photo (not as shot, [`crate::import::looks_as_shot`]) never gets its camera JPEG
    /// (unedited, uncropped): its stand-in is its own thumbnail for these settings, else its latest
    /// thumbnail or view with earlier settings when it may stand in, shrunk to the cell
    /// ([`QuickSource::Last`]), else a fast draft of its edit from the camera JPEG
    /// ([`QuickSource::Draft`]). `None` when it has none of them: its render is the first picture.
    pub fn quick_thumb_job(&mut self, job: &RenderJob) -> Option<QuickJob> {
        let p = self.catalog.photo(job.photo)?.clone();
        let edge = job.request.max_w.max(job.request.max_h);
        if !crate::import::looks_as_shot(&p) {
            let c = self.media.rendered.clone();
            let latest = vec![(c.clone(), latest_thumb_key(&p)), (c.clone(), latest_view_key(&p, true))];
            let exact = job.cache.clone();
            // not rendered with these settings yet: a fast draft with them from the camera JPEG (a
            // raw), after a latest picture that may stand in (one with another look may not); with
            // neither, its render is its first picture
            let draft = if exact.iter().any(|(c, k)| c.has(*k)) {
                None
            } else {
                let jpeg = self.embedded_stand_in_of(&p);
                if jpeg.is_none() && !latest.iter().any(|(c, k)| c.has(*k)) {
                    return None;
                }
                jpeg.map(|(path, l)| {
                    let mut j = job.clone();
                    j.cache = None;
                    j.latest = None;
                    j.kept = None;
                    (path, l, edge, Box::new(j))
                })
            };
            return Some(QuickJob {
                request_id: 0,
                photo: job.photo,
                key: job.key,
                cached: exact.into_iter().collect(),
                embedded: None,
                embedded_cache: None,
                embedded_on_disk: false,
                small: None,
                latest,
                fit: Some((edge, edge)),
                draft,
                want: job.placement,
                kept: job.kept.clone(),
            });
        }
        let (path, l) = self.embedded_stand_in_of(&p)?;
        Some(QuickJob {
            request_id: 0,
            photo: job.photo,
            key: job.key,
            cached: job.cache.clone().into_iter().collect(),
            embedded: Some((path, l, edge)),
            // the camera JPEG is this thumbnail (an unedited raw): kept with the rendered ones, under
            // a key of its own, so the folder's next visit reads it from the thumbnail cache
            embedded_cache: job.cache.as_ref().map(|(c, k)| (c.clone(), embedded_thumb_key(*k))),
            embedded_on_disk: true,
            small: Some(Box::new(job.clone())),
            latest: Vec::new(),
            fit: None,
            draft: None,
            want: None,
            kept: job.kept.clone(),
        })
    }

    /// Accept a finished job's loaded source into the cache. What the decode learnt about the file that the catalog
    /// has wrong is corrected too ([`Self::learn_as_shot_wb`], [`Self::learn_file_lens`]).
    pub fn accept(&mut self, r: &RenderResult) {
        if let Some(src) = &r.loaded
            && r.source_key.is_none_or(|key| self.catalog.photo(r.photo).is_some_and(|p| Hasher128::new().str(&pixels_key(p)).finish() == key))
            // (not a source decoded with settings the photo no longer has: one decoded for them replaces it)
            && self.rendered_photo_of(r.photo).is_none_or(|p| src.serves(&LoadOptions::for_photo(&p)))
        {
            if let Some(info) = &src.info {
                self.learn_as_shot_wb(r.photo, info);
                self.learn_file_lens(r.photo, info);
            }
            self.media.insert_source(r.photo, r.level, src.clone());
        }
    }

    /// [`Self::accept`] for a render replaced before it finished ([`SUPERSEDED`]): its source was built before it ran,
    /// so it is taken only while it is still what the photo's renders load. A plain source of a photo whose denoised
    /// picture became ready meanwhile is not (the readiness cleared the photo's sources; keeping this one would show
    /// the photo without its noise reduction). Nor is one whose denoised picture came from another product than the
    /// photo's current one (a denoise redone meanwhile), nor one with a denoised picture once the photo has none
    /// (review of upstream PR #3569).
    pub fn accept_replaced(&mut self, r: &RenderResult) {
        let current = self.media.denoise.get(r.photo).map(|(spec, _)| spec.key.as_str());
        let stale = r.loaded.as_ref().is_some_and(|src| {
            !src.from_proxy
                && match (&src.denoised, current) {
                    (None, None) => false,
                    (None, Some(_)) | (Some(_), None) => true,
                    (Some(t), Some(key)) => t.key.as_deref() != Some(key),
                }
        });
        if stale {
            return;
        }
        self.accept(r);
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

    /// Record the file's own lens correction a decode of photo `id` read (`info`) when the catalog holds another: a raw
    /// imported before its camera's in-file correction was read renders with the decode's lens, while the Optics panel
    /// and the crop read the catalog's copy and said there was no profile until Reload. Bookkeeping, not an undo step.
    /// A decode without a file correction (none, or a database profile matched by name) changes nothing, and a lens the
    /// user picked keeps the copy their pick resolved to ([`crate::lens_profiles::resolve_picked`]).
    fn learn_file_lens(&mut self, id: PhotoId, info: &SourceInfo) {
        let Some(lens) = info.lens.filter(|_| info.lens_from_file) else { return };
        let Some(p) = self.catalog.photo(id) else { return };
        if p.embedded_lens.as_deref() == Some(&lens) || p.develop.optics.lens.is_some() {
            return;
        }
        if let Err(e) = self.apply_system(Op::SetEmbeddedLens { id, lens: Some(Box::new(lens)) }) {
            log::warn!("photo {}: lens correction not recorded: {e}", id.0);
        }
    }

    /// Synchronous render (CLI, MCP, tests).
    pub fn render_now(&mut self, id: PhotoId, max_w: usize, max_h: usize) -> Result<Rendered, String> {
        lcc_pipeline::check_output_size(max_w, max_h)?;
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
        space: lcc_pipeline::OutputSpace,
        depth: lcc_pipeline::OutputDepth,
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
        space: lcc_pipeline::OutputSpace,
        depth: lcc_pipeline::OutputDepth,
    ) -> Result<RenderJob, String> {
        self.export_job_with_alpha(id, max_w, max_h, space, depth, false)
    }

    /// [`Self::export_job`]; with `alpha`, for a file that keeps transparency (PNG, TIFF, WebP): the
    /// render is of the source's colour without the white its transparent areas are flattened onto,
    /// with [`Rendered::alpha`] (issue #2976). A source without transparency renders as without.
    pub fn export_job_with_alpha(
        &mut self,
        id: PhotoId,
        max_w: usize,
        max_h: usize,
        space: lcc_pipeline::OutputSpace,
        depth: lcc_pipeline::OutputDepth,
        alpha: bool,
    ) -> Result<RenderJob, String> {
        lcc_pipeline::check_output_size(max_w, max_h)?;
        let mut job = self.render_job(id, max_w, max_h, false, true).ok_or("no such photo")?;
        // An export renders the full image: never a thumbnail's source developed from a smaller copy of the raw
        // image that the file keeps (Lightroom's merged DNGs)
        let reduced = match &job.source {
            SourceRef::Loaded(s) => s.reduced,
            SourceRef::Fit { from, .. } => from.reduced,
            _ => false,
        };
        if reduced {
            let p = self.rendered_photo_of(id).ok_or("no such photo")?;
            job.source = self.media.source_ref_uncached(&p, job.level);
        }
        // An export is made from the denoised picture its Denoise amount asks for, made when the job runs if the
        // background queue has not got to it. (A source kept in memory may lack the picture, so the job reads the file.)
        if let Some(wanted) = self.denoise_for_export(id, &job.settings)? {
            let p = self.rendered_photo_of(id).ok_or("no such photo")?;
            job.source = self.media.source_ref_uncached(&p, job.level);
            if let SourceRef::File { denoise, .. } = &mut job.source {
                *denoise = Some(wanted);
            }
        }
        job.request.space = space;
        job.request.depth = depth;
        job.request.alpha = alpha;
        // an export shrinks with a sharp filter: a preview's soft one leaves a small export blurred (#2970)
        job.request.downscale = lcc_pipeline::Downscale::Sharp;
        job.key ^=
            (space as u64 + 1).wrapping_mul(0xa076_1d64_78bd_642f) ^ (depth as u64 + 1).wrapping_mul(0xe703_7ed1_a0b4_28db) ^ 0x9e37_79b9_7f4a_7c15;
        if alpha {
            job.key ^= 0x8ebc_6af0_9c88_c6e3;
        }
        Ok(job)
    }

    /// Prefer decoder facts to header-only metadata for pixel-statistics commands.
    pub fn source_info(&self, id: PhotoId) -> SourceInfo {
        let header = self.catalog.photo(id).map(|p| source_info(p)).unwrap_or_default();
        self.media.source_facts(id, header)
    }

    /// The source proxy for pixel-statistics commands (auto tone/WB), loading synchronously.
    pub fn source_now(&mut self, id: PhotoId, level: SourceLevel) -> Result<Arc<Rgb32f>, String> {
        let p = self.rendered_photo_of(id).ok_or("no such photo")?;
        let r = self.media.source_ref(&p, level).load_source()?;
        let image = r.image.clone();
        if let Some(info) = &r.info {
            self.learn_as_shot_wb(id, info);
            self.learn_file_lens(id, info);
        }
        self.media.insert_source(id, level, r);
        Ok(image)
    }

    /// [`Self::source_now`] for several photos at once, each with the facts its decoder gave
    /// ([`Self::source_info`], read before another photo can push it out of the cache): the
    /// ones not in memory are decoded in parallel, so a selection costs about one decode per core,
    /// not one after another. In the order of `ids`. Callers pass a few at a time (all of them are
    /// held decoded until this returns).
    pub(crate) fn sources_now(&mut self, ids: &[PhotoId], level: SourceLevel) -> Vec<(PhotoId, Result<(Arc<Rgb32f>, SourceInfo), String>)> {
        use rayon::prelude::*;
        let refs: Vec<(PhotoId, Option<SourceRef>)> =
            ids.iter().map(|&id| (id, self.rendered_photo_of(id).map(|p| self.media.source_ref(&p, level)))).collect();
        let loaded: Vec<(PhotoId, Result<DecodedSource, String>)> =
            refs.into_par_iter().map(|(id, r)| (id, r.ok_or_else(|| "no such photo".to_string()).and_then(|r| r.load_source()))).collect();
        loaded
            .into_iter()
            .map(|(id, r)| {
                let r = r.map(|r| {
                    let image = r.image.clone();
                    if let Some(info) = &r.info {
                        self.learn_as_shot_wb(id, info);
                        self.learn_file_lens(id, info);
                    }
                    // read before the insert: the cache's budget may evict this very entry, and
                    // the header facts alone lack the decoder's camera colour
                    let facts = match r.info {
                        Some(_) => r.info_or(self.catalog.photo(id).map(|p| source_info(p)).unwrap_or_default()),
                        None => self.source_info(id),
                    };
                    self.media.insert_source(id, level, r);
                    (image, facts)
                });
                (id, r)
            })
            .collect()
    }
}

/// What an import learns from a file header (set by the app from `lcc-codecs`/`-raw`).
#[derive(Clone, Debug, Default)]
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub kind: MediaKind,
    pub file_size: u64,
    pub captured: Option<String>,
    /// The file's modification time (RFC 3339 in this computer's time zone, the clock time photos
    /// are sorted by): the capture date of a file whose metadata has none, as Lightroom does
    /// (exports saved without metadata would otherwise all land under "Unknown Date").
    pub file_modified: Option<String>,
    /// [`ProbeInfo::captured`] is [`ProbeInfo::file_modified`] standing in for a capture time that
    /// neither the file nor its sidecar has (set by the import, which dates such files so).
    pub captured_is_file_date: bool,
    pub meta: lcc_catalog::Meta,
    pub rating: u8,
    pub flag: lcc_catalog::Flag,
    pub label: Option<lcc_catalog::ColorLabel>,
    pub label_text: Option<String>,
    pub as_shot_wb: Option<(f64, f64)>,
    /// Hash of the file's bytes (hex), for duplicate detection. When it is a 32-digit
    /// [`lcc_preview::hash_bytes`] of the whole file (as the native probe computes it),
    /// Import → Copy verifies each copy against it instead of reading the source again.
    pub content_hash: Option<String>,
    /// Lens corrections embedded in the file (DNG opcodes).
    pub embedded_lens: Option<lcc_develop::EmbeddedLens>,
    /// Whether `embedded_lens` is the file's own correction (not a lens database profile).
    pub lens_from_file: bool,
    /// The file's embedded XMP packet (raw/DNG files), for develop settings stored inside the file.
    pub xmp: Option<String>,
    /// The file's Exif orientation, when it states one: how its stored pixels turn upright (`width` and `height` are
    /// the upright size).
    pub orientation: Option<lcc_geom::Orientation>,
    /// A raw variant that can't be decoded yet: why. The file is described (and will be shown and
    /// edited) from its embedded preview; see [`lcc_catalog::Photo::preview_only`].
    pub preview_only: Option<String>,
    /// SHA-1 of the file's bytes (lowercase hex), computed in the same read pass as
    /// `content_hash`: what Immich stores for its assets (IMM-LINK).
    pub sha1: Option<String>,
    /// A video clip's stream facts ([`crate::files::probe_video_file`]); `None` for photos and
    /// for clips whose container could not be read.
    pub video: Option<lcc_catalog::VideoMeta>,
    /// A video clip's duration in seconds.
    pub duration: Option<f64>,
}

pub type FileProbe = Arc<dyn Fn(&str) -> Result<ProbeInfo, String> + Send + Sync>;

#[cfg(test)]
mod thumbnail_hash_tests {
    use super::*;

    /// A session with one JPEG. Each call names its own file: the decode share
    /// ([`crate::decode_share`]) is process-wide and keyed by path, so tests running in parallel
    /// must never share one (issue #1068: a test took another's decode, or lost its own).
    fn session() -> crate::Session {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: format!("hash-fixture-{n}.jpg") }, "fixture.jpg", "JPEG", 6000, 4000, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s
    }
    /// A raw with its body's camera profile picked (every photo of a folder browsed before import,
    /// for a body with a default profile) still shows its camera JPEG first, in the grid and the
    /// loupe, until its render replaces it; only an unedited raw's is final.
    #[test]
    fn a_raw_with_a_camera_profile_still_shows_its_camera_preview_first() {
        let mut s = crate::Session::new();
        s.media.preview_loader =
            Some(Arc::new(|_: &str, edge: usize| Some(Rgba8 { width: edge.min(8), height: 2, data: vec![[9, 8, 7, 255]; edge.min(8) * 2] })));
        let mut raw = Photo::new(PhotoId(1), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
        raw.kind = MediaKind::Raw;
        let mut d = raw.camera_defaults();
        d.profile.camera = "dcp:body-71".into();
        raw.develop = Arc::new(d);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
        assert!(!s.embedded_thumb_is_final(&p), "its render replaces the camera JPEG");
        let q = s.quick_view_job(PhotoId(1), 64, true).unwrap();
        assert!(q.embedded.is_some(), "the loupe's stand-in is the camera JPEG");
        assert_eq!(q.run().quick, Some(QuickSource::Embedded));
        let thumb = s.thumb_job(PhotoId(1), 256).unwrap();
        let q = s.quick_thumb_job(&thumb).expect("the grid's stand-in");
        assert_eq!(q.run().quick, Some(QuickSource::Embedded));
        assert!(s.embedded_full_job(PhotoId(1), 8192).is_some(), "framed and exposed as shot: a fair focus check");
    }

    /// An edited raw (here: exposure) never stands in with its camera JPEG, which is unedited and
    /// uncropped: its first picture is a fast draft of the edit made from that JPEG.
    #[test]
    fn an_edited_raw_shows_a_draft_of_its_edit_not_its_camera_preview() {
        let mut s = crate::Session::new();
        s.media.preview_loader =
            Some(Arc::new(|_: &str, edge: usize| Some(Rgba8 { width: edge.min(8), height: 2, data: vec![[9, 8, 7, 255]; edge.min(8) * 2] })));
        let mut raw = Photo::new(PhotoId(1), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
        raw.kind = MediaKind::Raw;
        let mut d = raw.camera_defaults();
        d.light.exposure = 0.3;
        raw.develop = Arc::new(d);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
        assert!(!s.embedded_thumb_is_final(&p));
        let q = s.quick_view_job(PhotoId(1), 64, true).unwrap();
        assert!(q.embedded.is_none() && q.draft.is_some(), "the loupe's stand-in is a draft of the edit");
        assert_eq!(q.run().quick, Some(QuickSource::Draft));
        let thumb = s.thumb_job(PhotoId(1), 256).unwrap();
        let q = s.quick_thumb_job(&thumb).expect("the grid's stand-in");
        assert_eq!(q.run().quick, Some(QuickSource::Draft));
        assert!(s.embedded_full_job(PhotoId(1), 8192).is_none(), "an edit is shown rendered");
    }

    /// An unedited raw's camera JPEG is read once for the loupe and kept in memory for a step
    /// back; the full-size one (zoomed in) is made at the size asked for and kept nowhere; an
    /// edited raw or another file has none.
    #[test]
    fn camera_previews_are_kept_for_a_revisit_and_full_size_ones_are_not() {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = crate::Session::new();
        let r = reads.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, edge: usize| {
            r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Rgba8 { width: edge.min(64), height: 2, data: vec![[9, 8, 7, 255]; edge.min(64) * 2] })
        }));
        let mut raw = Photo::new(PhotoId(1), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
        raw.kind = MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let jpeg = Photo::new(PhotoId(2), Source::File { path: "b.jpg".into() }, "b.jpg", "JPEG", 600, 400, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(jpeg) }).unwrap();
        for _ in 0..3 {
            let q = s.quick_view_job(PhotoId(1), 48, true).unwrap();
            let r = q.run();
            assert_eq!(r.quick, Some(QuickSource::Embedded));
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1, "read once, then from memory");
        let full = s.embedded_full_job(PhotoId(1), 8192).unwrap();
        assert!(full.embedded_cache.is_none() && full.cached.is_empty() && full.small.is_none());
        assert_eq!(full.embedded.as_ref().map(|e| e.2), Some(8192));
        assert_ne!(full.key, s.quick_view_job(PhotoId(1), 48, true).unwrap().key);
        assert_eq!(full.run().quick, Some(QuickSource::Embedded));
        assert!(s.embedded_full_job(PhotoId(2), 8192).is_none(), "not a raw");
        let mut edited = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        edited.light.exposure = 1.0;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edited), label: "edit".into(), edited: None }).unwrap();
        assert!(s.embedded_full_job(PhotoId(1), 8192).is_none(), "an edit is shown rendered");
    }

    #[test]
    fn hashes_match_uncached_keys_and_are_not_recomputed_for_sizes_metadata_or_before() {
        let _keys = crate::sensor_maps::keep_generation();
        let mut s = session();
        for edge in [256, 384, 512, 256] {
            let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
            let b = thumb_bucket(edge);
            // Independent original formula: never asks the new hash cache for expected values.
            let settings = p.develop.hash64();
            let content = Hasher128::new().str(&pixels_key(&p)).finish().0 as u64;
            let expected =
                settings ^ ((b as u64) << 40) ^ ((b as u64) << 20) ^ 1 ^ p.id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ content.rotate_left(17);
            let disk = Hasher128::new()
                .str(&pixels_key(&p))
                .u64(settings)
                .u64(b as u64)
                .u64(RENDER_CACHE_VERSION)
                .u64(crate::camera_profiles::cache_key())
                .u64(sensor_key(&p))
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
    /// Issue #60: straightening or moving the crop box in the crop tool doesn't re-render its
    /// uncropped view; the cropped view (and anything else) still changes with the crop.
    #[test]
    fn the_uncropped_view_ignores_the_crop_box_and_angle() {
        let mut s = session();
        let keys = |s: &mut crate::Session| {
            let open = s.loupe_job(PhotoId(1), 800, 600, false).unwrap();
            let shut = s.loupe_job(PhotoId(1), 800, 600, true).unwrap();
            (open.key, open.view_cache.unwrap().1, shut.key)
        };
        let before = keys(&mut s);
        let mut edit = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        edit.crop.geometry = lcc_geom::CropGeometry { rect: lcc_geom::Rect::new(0.2, 0.2, 0.7, 0.8), angle: 7.5 };
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit.clone()), label: "crop".into(), edited: None }).unwrap();
        let cropped = keys(&mut s);
        assert_eq!((before.0, before.1), (cropped.0, cropped.1), "the uncropped view is the same render");
        assert_ne!(before.2, cropped.2, "the cropped view follows the crop");
        edit.light.exposure = 1.0;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        assert_ne!(keys(&mut s).0, cropped.0, "other edits still re-render it");
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
    /// The Remove Hot Pixels box decides how a raw is decoded (#1345): the loader is told, and a source decoded with
    /// the other setting is not reused. A rendered file has no mosaic, so its source stays.
    #[test]
    fn hot_pixel_box_decodes_the_raw_again() {
        let mut s = session();
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        let raw = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let is_raw = raw.clone();
        s.media.file_loader = Some(Arc::new(move |_, _, options: LoadOptions| {
            log.lock().unwrap().push(options.outliers);
            Ok((Rgb32f::new(8, 8), SourceInfo { raw: is_raw.load(std::sync::atomic::Ordering::Relaxed), ..Default::default() }))
        }));
        let set = |s: &mut crate::Session, on: bool| {
            let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
            d.detail.nr_outliers = on;
            // a new edit each time, so a render cached for earlier settings is not simply shown again
            d.light.exposure += 0.1;
            s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "box".into(), edited: None }).unwrap();
        };
        let render = |s: &mut crate::Session| {
            let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
            s.accept(&r);
        };
        render(&mut s);
        render(&mut s);
        assert_eq!(*asked.lock().unwrap(), [false], "decoded once, then cached");
        set(&mut s, true);
        render(&mut s);
        render(&mut s);
        assert_eq!(*asked.lock().unwrap(), [false, true], "switched on: decoded again with the filter, then cached");
        set(&mut s, false);
        render(&mut s);
        assert_eq!(*asked.lock().unwrap(), [false, true, false]);
        // not a raw: the box changes nothing, the cached source is kept
        raw.store(false, std::sync::atomic::Ordering::Relaxed);
        s.media.forget(PhotoId(1));
        set(&mut s, false);
        render(&mut s);
        set(&mut s, true);
        render(&mut s);
        assert_eq!(*asked.lock().unwrap(), [false, true, false, false]);
    }
    #[test]
    fn source_from_before_content_reload_is_not_accepted() {
        let mut s = session();
        s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
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
    /// The photo's camera-profile choice (a camera's default, the app's own colour, an imported `.dcp`) is part of the
    /// decode of a raw: the loader is told, and a source decoded with another choice is decoded again. A file with no
    /// mosaic has nothing the choice would change, so its source is kept.
    #[test]
    fn camera_profile_choice_decodes_a_raw_again() {
        let set = |s: &mut crate::Session, id: &str| {
            let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
            d.profile.camera = id.into();
            s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "edit".into(), edited: None }).unwrap();
        };
        let render = |s: &mut crate::Session| {
            let r = s.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
            s.accept(&r);
        };
        for raw in [true, false] {
            let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let log = seen.clone();
            let mut s = session();
            s.media.file_loader = Some(Arc::new(move |_, _, options: LoadOptions| {
                log.lock().unwrap().push(options.camera);
                Ok((Rgb32f::new(8, 8), SourceInfo { raw, ..SourceInfo::default() }))
            }));
            render(&mut s);
            render(&mut s);
            assert_eq!(*seen.lock().unwrap(), [""], "decoded once, then cached");
            set(&mut s, "dcp:neutral");
            render(&mut s);
            render(&mut s);
            let decodes = seen.lock().unwrap().clone();
            if raw {
                assert_eq!(decodes, ["", "dcp:neutral"], "the choice reaches the loader and the new source is cached");
            } else {
                assert_eq!(decodes, [""], "no mosaic: the cached source is kept");
            }
        }
    }

    /// A raw catalogued before its camera's in-file lens correction was read renders with the correction its decode
    /// reads, so the decode records it in the catalog (the Optics panel and the crop read that copy). Bookkeeping, not
    /// an undo step; a decode without a file correction (a database profile by name) leaves the catalog's copy alone.
    #[test]
    fn a_decode_records_the_files_own_lens_correction() {
        let corr = |k: f64| lcc_develop::EmbeddedLens {
            vignette: Some(lcc_develop::EmbeddedVignette { k: [k, 0.0, 0.0, 0.0, 0.0], center: lcc_geom::Point { x: 0.5, y: 0.5 }, radius: 1.0 }),
            ..Default::default()
        };
        let decode = |from_file: bool| {
            let mut s = session();
            s.media.file_loader = Some(Arc::new(move |_, _, _| {
                Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, lens: Some(corr(0.2)), lens_from_file: from_file, ..SourceInfo::default() }))
            }));
            let r = s.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
            s.accept(&r);
            s
        };
        let s = decode(true);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().embedded_lens.as_deref(), Some(&corr(0.2)), "the file's correction is recorded");
        assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
        let s = decode(false);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().embedded_lens, None, "a database profile is not the file's own");
    }

    /// The photo's Camera colour option is part of the decode: a source decoded with another option is decoded again
    /// (and a late render of the old one is not taken back into the cache); one the option doesn't change is reused.
    #[test]
    fn camera_colour_decodes_again_only_when_it_changes_the_source() {
        use lcc_develop::CameraColour;
        let decodes = |colorimetric: bool| {
            let seen = Arc::new(std::sync::Mutex::new(Vec::<LoadOptions>::new()));
            let log = seen.clone();
            let loader: FileLoader = Arc::new(move |_, _, options| {
                log.lock().unwrap().push(options);
                Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, colorimetric, ..SourceInfo::default() }))
            });
            (loader, seen)
        };
        let set = |s: &mut crate::Session, colour: CameraColour| {
            let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
            d.profile.camera_colour = colour;
            s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "edit".into(), edited: None }).unwrap();
        };
        let render = |s: &mut crate::Session| {
            let r = s.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
            s.accept(&r);
        };

        let mut s = session();
        let (loader, seen) = decodes(true);
        s.media.file_loader = Some(loader);
        render(&mut s);
        // a job made before the option changes, finishing after it
        let late = s.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        render(&mut s);
        assert_eq!(seen.lock().unwrap().len(), 1, "the decoded source is reused while the option stays");
        set(&mut s, CameraColour::Colorimetric);
        render(&mut s);
        assert_eq!(seen.lock().unwrap().len(), 2, "a new option decodes again");
        assert_eq!(seen.lock().unwrap()[1].colour, CameraColour::Colorimetric, "with the option");
        render(&mut s);
        assert_eq!(seen.lock().unwrap().len(), 2, "and the new source is reused");
        // the late job re-decodes with the old option (its source was the cached one: decode it from the file)
        let mut late = late;
        let path = s.catalog.photo(PhotoId(1)).unwrap().source.clone();
        late.source = s.media.origin_ref(&path, 64);
        let late = late.run();
        assert_eq!(late.loaded.as_ref().map(|l| l.options.colour), Some(CameraColour::Auto));
        s.accept(&late);
        let n = seen.lock().unwrap().len();
        render(&mut s);
        assert_eq!(seen.lock().unwrap().len(), n, "the Colorimetric source stayed in the cache");

        // a camera without measured colour: the option changes nothing, so the source is kept
        let mut s = session();
        let (loader, seen) = decodes(false);
        s.media.file_loader = Some(loader);
        render(&mut s);
        set(&mut s, CameraColour::Colorimetric);
        render(&mut s);
        assert_eq!(seen.lock().unwrap().len(), 1, "no second decode where the option has no effect");
    }

    /// A panic while rendering (here in the decoder) used to end the pool's worker thread without
    /// an answer, so the view that asked showed "Rendering…" forever. It is the job's failure now,
    /// for renders and for stand-ins, answering the request it came from.
    #[test]
    fn a_render_that_panics_fails_instead_of_never_answering() {
        let mut s = session();
        s.media.file_loader = Some(Arc::new(|_, _, _| panic!("decoder bug")));
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
            embedded_cache: None,
            embedded_on_disk: false,
            small: None,
            latest: Vec::new(),
            fit: None,
            draft: None,
            want: None,
            kept: None,
        };
        let r = quick.run();
        assert_eq!((r.request_id, r.key), (7, 9));
        assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with one camera-sized photo (larger than the preview level) whose original is
    /// already decoded: a textured picture, made in milliseconds (a full-size demo scene takes
    /// ten seconds to generate).
    fn camera_sized() -> (crate::Session, PhotoId, usize, usize) {
        let (w, h) = (3000usize, 2000usize);
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: "/nowhere/a.jpg".into() }, "a.jpg", "JPEG", w as u32, h as u32, "");
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
        let px = |x: usize, y: usize| {
            let t = ((x * 7 + y * 13) % 97) as f32 / 97.0;
            [0.1 + 0.5 * x as f32 / w as f32 + 0.1 * t, 0.2 + 0.4 * y as f32 / h as f32, 0.15 + 0.3 * t]
        };
        s.media.insert(p.id, SourceLevel::Full, Arc::new(Rgb32f::from_fn(w, h, px)));
        (s, p.id, w, h)
    }

    /// A photo edited since its last view (an Auto Sync edit, its thumbnail built with the edit)
    /// steps in with the edit, never with the picture from before it.
    #[test]
    fn a_step_shows_the_thumbnail_of_the_edit_before_the_view_from_before_it() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        // an edited photo (an as-shot picture never stands in for an edit at all) seen in the loupe: its
        // latest view has that look
        s.execute("develop.set", &serde_json::json!({"values": {"light.contrast": 10}})).unwrap();
        assert!(s.loupe_job(id, 800, 800, true).unwrap().run().rendered.is_ok());
        // edited since (a change small enough that the old view would pass as close to it)
        s.execute("develop.set", &serde_json::json!({"values": {"light.exposure": 0.8}})).unwrap();
        let want = s.placement_of(id, true).unwrap();
        let q = s.quick_view_job(id, 800, true).unwrap();
        let r = q.run();
        assert_eq!(r.quick, Some(QuickSource::Last), "no thumbnail of the edit yet: the view from before stands in");
        assert_ne!(r.placement.map(|p| p.look), Some(want.look));
        // its thumbnail with the edit is built: that is what the step shows
        assert!(s.thumb_job(id, THUMB_SIZES[THUMB_SIZES.len() - 1]).unwrap().run().rendered.is_ok());
        let r = s.quick_view_job(id, 800, true).unwrap().run();
        assert!(matches!(r.quick, Some(QuickSource::Small | QuickSource::Last)), "{:?}", r.quick);
        assert_eq!(r.placement.map(|p| p.look), Some(want.look), "the built thumbnail of the edit, not the view from before it");
        // and once a view of the edit is made, that
        assert!(s.loupe_job(id, 800, 800, true).unwrap().run().rendered.is_ok());
        let r = s.quick_view_job(id, 800, true).unwrap().run();
        assert!(matches!(r.quick, Some(QuickSource::Cached | QuickSource::Last)), "{:?}", r.quick);
        assert_eq!(r.placement.map(|p| p.look), Some(want.look));
    }

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
        // the photo's view preview keeps the render, not the drawn size (on the GPU the fit is
        // made there, and the full image read back too for it)
        let job = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400);
        let (cache, key) = job.view_cache.clone().expect("a loupe job keeps the view preview");
        let shown = job.run().rendered.unwrap().image;
        assert_eq!(shown.width.max(shown.height), 400);
        let kept = cache.get_in_memory(key).expect("the view preview is cached");
        assert_eq!((kept.width, kept.height), (plain.width, plain.height), "the view preview is the full render");
        // a drag's update may cache its drawn size instead: the same render, so the same key (the
        // release finds the drag's last render)
        let at_rest = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400);
        assert_eq!(at_rest.clone().with_drawn_view_cache().key, at_rest.key);
    }

    // Given a photo whose view render of these settings is kept (a built preview, an earlier
    // visit), when the loupe asks for it at that size or smaller, then it is the kept picture,
    // fitted, with no render; a larger size, a draft or other settings render
    #[test]
    fn the_loupe_uses_a_kept_view_render_at_least_as_big() {
        let _keys = crate::sensor_maps::keep_generation();
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let job = s.loupe_job(id, 1024, 1024, true).unwrap();
        let (cache, key) = job.view_cache.clone().expect("a loupe job keeps the view preview");
        let plain = job.run().rendered.unwrap().image;
        let t0 = std::time::Instant::now();
        while cache.get_in_memory(key).is_none() && t0.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let same = s.loupe_job(id, 1024, 1024, true).unwrap();
        assert!(same.cached_view().is_some(), "the kept render serves the same size");
        assert_eq!(same.run().rendered.unwrap().image.data, plain.data);
        let small = s.loupe_job(id, 512, 512, true).unwrap().cached_view().expect("and a smaller one");
        assert!(small.width.max(small.height) <= 512);
        assert!(s.loupe_job(id, 2048, 2048, true).unwrap().cached_view().is_none() || plain.width.max(plain.height) >= 2048, "a larger view renders");
        assert!(s.loupe_job(id, 512, 512, true).unwrap().draft().cached_view().is_none(), "a draft renders");
        s.execute("develop.set", &serde_json::json!({"values": {"light.exposure": 1.0}})).unwrap();
        assert!(s.loupe_job(id, 512, 512, true).unwrap().cached_view().is_none(), "other settings render");
        // the default disk budget makes room for every photo's previews, from a floor
        assert_eq!(default_disk_cache_bytes(0), DISK_CACHE_BYTES);
        assert!(default_disk_cache_bytes(50_000) > DISK_CACHE_BYTES && default_disk_cache_bytes(usize::MAX) == u64::MAX);
        // the automatic size: a share of the drive, from 20 GB, never the drive's last 10 GB
        assert_eq!(automatic_disk_cache_bytes(10, None, 0), AUTO_DISK_CACHE_MIN);
        assert_eq!(automatic_disk_cache_bytes(10, Some(1000 << 30), 0), 150 << 30);
        assert!(automatic_disk_cache_bytes(10, Some(12 << 30), 0) <= 6 << 30, "a nearly full drive keeps room");
        // nearly full: the 20 GB minimum (and the 2 GB floor) give way to half of what is free,
        // plus what the cache already holds
        for (free, used) in [(5u64 << 30, 0u64), (1 << 30, 0), (3 << 30, 1 << 30), (0, 0), (100 << 20, 50 << 20)] {
            for photos in [0, 10, 100_000] {
                let auto = automatic_disk_cache_bytes(photos, Some(free), used);
                assert!(auto <= used + free / 2, "{photos} photos, {free} free, {used} used: {auto}");
            }
        }
        assert!(automatic_disk_cache_bytes(10, Some(5 << 30), 0) <= (5 << 30) / 2);
        for (n, free, used) in [(usize::MAX, Some(u64::MAX), u64::MAX), (0, Some(0), 0), (1, Some(1), 1)] {
            let _ = automatic_disk_cache_bytes(n, free, used);
        }
        // hostile sizes
        assert!(fit_cached(&plain, 0, 10).is_none() && fit_cached(&Rgba8::new(0, 0), 10, 10).is_none());
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
        assert_eq!(info, SourceInfo { native_long: Some(6000), identity: source_identity(&p), ..decoded }, "the decoder's facts, the catalog's size");
        assert_eq!(info.identity.map(|i| i.size), Some([6000, 4000]), "which photo, at every size");
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
        let tone = lcc_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
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
            loader: Some(Arc::new(move |_, _, _| {
                let mut image = Rgb32f::new(64, 64);
                image.data.fill([0.1; 3]);
                Ok((image, loaded_info.clone()))
            })),
            fallback: None,
            denoise: None,
            options: Default::default(),
            reduced: None,
        };
        job.info = SourceInfo::default(); // Header facts cannot override decoder facts.
        job.settings = Arc::new(DevelopSettings::default());
        let r = job.clone().run();
        assert_eq!(r.loaded.as_ref().unwrap().info.as_ref(), Some(&info));
        let expected = lcc_pipeline::render(&r.loaded.as_ref().unwrap().image, &info, &job.settings, &job.request);
        assert_eq!(r.rendered.as_ref().unwrap().image.data, expected.image.data);
        s.accept(&r);
        job.source = s.media.source_ref(s.catalog.photo(id).unwrap(), job.level);
        let again = job.run();
        assert!(again.loaded.is_none());
        assert_eq!(r.rendered.unwrap().image.data, again.rendered.unwrap().image.data);
        s.media.forget(id);
        assert!(s.media.get_source(id, r.level).is_none());
    }

    /// A replaced render's plain source is not kept once the photo's denoised picture is ready: the
    /// photo's next render must load the pair, not develop the plain picture (review of upstream PR #3569).
    #[test]
    fn a_replaced_renders_plain_source_is_not_kept_once_the_denoised_picture_is_ready() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let mut job = s.render_job(id, 64, 64, false, true).unwrap();
        job.source = SourceRef::File {
            path: "synthetic.arw".into(),
            max_edge: 64,
            loader: Some(Arc::new(|_, _, _| Ok((Rgb32f::new(64, 64), SourceInfo::default())))),
            fallback: None,
            denoise: None,
            options: Default::default(),
            reduced: None,
        };
        let r = job.run();
        assert!(r.loaded.as_ref().is_some_and(|src| src.denoised.is_none()));
        // the denoised picture became ready while the render ran
        let pair: PairLoader = Arc::new(|_, _, _, _| Err("not read in this test".into()));
        s.media.denoise.loader = Some(pair);
        s.media.denoise.set(id, DenoiseSpec { product: "denoised.bin".into(), key: "k".into(), make: None });
        s.media.forget(id);
        s.accept_replaced(&r);
        assert!(!s.media.has_source(id, r.level), "the plain source is not kept");
        // without a denoised picture it is kept, as before
        s.media.denoise.remove(id);
        s.accept_replaced(&r);
        assert!(s.media.has_source(id, r.level));
    }

    /// A replaced render whose denoised picture came from an older product (the denoise was redone while it ran) is
    /// not kept: later renders would show the old picture (review of upstream PR #3569). One from the current
    /// product is.
    #[test]
    fn a_replaced_renders_source_from_an_older_denoise_is_not_kept() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let pair: PairLoader = Arc::new(|_, _, _, _| Ok((Rgb32f::new(64, 64), Some(Rgb32f::new(64, 64)), SourceInfo::default())));
        s.media.denoise.loader = Some(pair.clone());
        let spec = |key: &str| DenoiseSpec { product: format!("{key}.bin").into(), key: key.into(), make: None };
        let mut job = s.render_job(id, 64, 64, false, true).unwrap();
        job.source = SourceRef::File {
            path: "synthetic-twin.arw".into(),
            max_edge: 64,
            loader: Some(Arc::new(|_, _, _| Ok((Rgb32f::new(64, 64), SourceInfo::default())))),
            fallback: None,
            denoise: Some((spec("old"), pair)),
            options: Default::default(),
            reduced: None,
        };
        let r = job.run();
        assert!(r.loaded.as_ref().is_some_and(|src| src.denoised.as_ref().is_some_and(|t| t.key.as_deref() == Some("old"))));
        // the denoise was redone while the render ran
        s.media.denoise.set(id, spec("new"));
        s.media.forget(id);
        s.accept_replaced(&r);
        assert!(!s.media.has_source(id, r.level), "the old product's picture is not kept");
        // the same product: kept
        s.media.denoise.set(id, spec("old"));
        s.accept_replaced(&r);
        assert!(s.media.has_source(id, r.level));
    }

    /// A decode is shared only with jobs for the same file content: after the photo's file changed (a new content
    /// key), a decode of the old file that is still held is not taken (review of upstream PR #3569).
    #[test]
    fn a_decode_is_shared_only_with_jobs_for_the_same_file_content() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let decodes = Arc::new(AtomicUsize::new(0));
        let d = decodes.clone();
        // (one loader for all three jobs: the share key names it by address)
        let loader: FileLoader = Arc::new(move |_, _, _| {
            d.fetch_add(1, Ordering::SeqCst);
            Ok((Rgb32f::new(64, 64), SourceInfo::default()))
        });
        let mut job = |content: u128| {
            let mut job = s.render_job(id, 64, 64, false, true).unwrap();
            job.source = SourceRef::File {
                path: "synthetic-content-key.arw".into(),
                max_edge: 64,
                loader: Some(loader.clone()),
                fallback: None,
                denoise: None,
                options: Default::default(),
                reduced: None,
            };
            job.source_key = Some(Hash128(content));
            job
        };
        let (a, same, changed) = (job(1), job(1), job(2));
        let first = a.run();
        assert_eq!(decodes.load(Ordering::SeqCst), 1);
        let again = same.run();
        assert_eq!(decodes.load(Ordering::SeqCst), 1, "the same content takes the held decode");
        let other = changed.run();
        assert_eq!(decodes.load(Ordering::SeqCst), 2, "a changed file is decoded again");
        drop((first, again, other));
    }

    /// Issue #730: an ILCE-7M3 ARW from an older catalog (white balance relative to the as-shot look, 6500 / 0 as
    /// its as-shot white, a Custom value on that scale) renders pixel for pixel as it did once the camera's matrices
    /// give it a Kelvin scale; the decode teaches the catalog the real as-shot white (bookkeeping, not an undo step),
    /// and the panel then reads As Shot in Kelvin while the stored settings keep their look.
    #[test]
    fn old_relative_white_balance_on_a_kelvin_camera_keeps_its_look_and_learns_the_as_shot_white() {
        use lcc_develop::{WbMode, WbScale};
        let mut s = crate::Session::new();
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: "/photos/DSC00001.ARW".into() }, "DSC00001.ARW", "ARW", 64, 64, "2026-01-01T00:00:00");
        p.kind = MediaKind::Raw;
        p.meta.camera = "SONY ILCE-7M3".into();
        p.as_shot_wb = Some((6500.0, 0.0));
        let mut d = lcc_develop::DevelopSettings::for_raw(6500.0, 0.0);
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
            loader: Some(Arc::new(move |_, _, _| Ok((loaded_pixels.clone(), loaded_info.clone())))),
            fallback: None,
            denoise: None,
            options: LoadOptions::of(&d),
            reduced: None,
        };
        let undo_before = s.undo.len();
        let request = job.request;
        let r = job.run();
        let new = r.rendered.as_ref().unwrap().image.data.clone();
        // the old pipeline: the same settings against the relative-scale source facts of an older build
        let old_info = SourceInfo { raw: true, relative_wb: true, as_shot_temp: 6500.0, as_shot_tint: 0.0, ..Default::default() };
        let old = lcc_pipeline::render(&pixels, &old_info, &d, &request).image.data;
        // the two paths reach the pipeline at different scales; colour NR blurs before it divides the scale out,
        // so float rounding may move a channel by one 8-bit level, never more
        assert_eq!(new.len(), old.len());
        let worst = new.iter().zip(&old).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y))).max().unwrap_or(0);
        assert!(worst <= 1, "the look of an existing edit changed (by {worst} levels)");
        assert_ne!(new, lcc_pipeline::render(&pixels, &decoded, &lcc_develop::DevelopSettings::for_raw(4986.0, -2.0), &request).image.data);
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
        let shown = lcc_pipeline::local::effective_wb(&info, &d);
        assert_eq!(shown, lcc_pipeline::local::legacy_to_kelvin(5800.0, 3.0, (4986.0, -2.0)));
        assert!(shown.0 < 4986.0 && shown.0 > 4000.0, "{shown:?}");
        // accepting again changes nothing more
        let log = s.pending_log.len();
        s.accept(&r);
        assert_eq!(s.pending_log.len(), log);
        // a catalog-only (proxy) render of the same settings before the decode gave the same pixels too
        let proxy = lcc_pipeline::render(&pixels, &header, &d, &request).image.data;
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
        m.rendered.put(Hash128(7), Arc::new(lcc_raster::Rgba8::new(1000, 1000)));
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

    /// Issue #2576: work weighed with saturating maths can ask for `usize::MAX` bytes. Adding to
    /// that overflowed (a panic with the gate's mutex held in a debug build, a wrapped count that
    /// let everything in for a release one). The count is now exact past `usize::MAX`: later work
    /// waits, whichever permit is released first, and even on a gate whose limit is `usize::MAX`.
    #[test]
    fn work_gate_counts_past_usize_max_instead_of_overflowing() {
        use crate::memory::WorkGate;
        // a holder of one byte, on its own thread; still waiting 50 ms later
        let waiting = |g: &Arc<WorkGate>| {
            let g2 = g.clone();
            let t = std::thread::spawn(move || {
                let _b = g2.acquire(1);
                g2.usage().0
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert!(!t.is_finished(), "the next holder waits");
            t
        };

        // the huge permit released first: the urgent one is still counted
        let g = Arc::new(WorkGate::new(100));
        let huge = g.acquire(usize::MAX);
        let urgent = g.acquire_urgent(101);
        assert_eq!(g.usage().0, usize::MAX, "reported saturated");
        drop(huge);
        assert_eq!(g.usage().0, 101);
        let t = waiting(&g);
        drop(urgent);
        assert_eq!(t.join().unwrap(), 1);
        assert_eq!(g.usage().0, 0);

        // the urgent permit released first
        let huge = g.acquire(usize::MAX);
        drop(g.acquire_urgent(1));
        assert_eq!(g.usage().0, usize::MAX);
        let t = waiting(&g);
        drop(huge);
        assert_eq!(t.join().unwrap(), 1);
        assert_eq!(g.usage().0, 0);

        // a gate without a practical limit is still full when it holds usize::MAX
        let g = Arc::new(WorkGate::new(usize::MAX));
        let huge = g.acquire(usize::MAX);
        let t = waiting(&g);
        drop(huge);
        assert_eq!(t.join().unwrap(), 1);
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
        use lcc_pipeline::PixelWindow;
        let (mut s, id, w, h) = camera_sized();
        let p = s.catalog.photo(id).unwrap().clone();
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
        d.crop.geometry.rect = lcc_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.5 };
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
        use lcc_pipeline::PixelWindow;
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        let p = s.catalog.photo(id).unwrap().clone();
        let (w, h) = (p.width as usize * 8, p.height as usize * 8);
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.spots.push(lcc_develop::Spot {
            points: vec![lcc_geom::Point::new(0.2, 0.5)],
            size: 0.01,
            source_offset: Some(lcc_geom::Point::new(0.6, 0.0)),
            auto: None,
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
        use lcc_pipeline::PixelWindow;
        let (mut s, id, w, h) = camera_sized();
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

    /// The thumbnail of a photo whose preview-level source is decoded is cut from it: the file (here
    /// one nothing can decode) is not read again.
    #[test]
    fn a_thumbnail_is_cut_from_the_decoded_preview() {
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: "/nowhere/a.ARW".into() }, "a.ARW", "ARW", 2400, 1600, "");
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
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

    /// In Detail and the loupe the source is held at the full level: the thumbnail, which every
    /// edit queues again, is cut from it too instead of decoding the original again.
    #[test]
    fn a_thumbnail_is_cut_from_the_decoded_full_source() {
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: "/nowhere/a.ARW".into() }, "a.ARW", "ARW", 2400, 1600, "");
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
        s.media.insert(p.id, SourceLevel::Full, Arc::new(Rgb32f::from_fn(2400, 1600, |x, _| [x as f32 / 2400.0, 0.5, 0.25])));
        let job = s.thumb_job(p.id, 512).unwrap();
        assert!(matches!(job.source, SourceRef::Fit { .. }), "cut from the full source, not decoded from the file");
        let r = job.run();
        let thumb = r.rendered.as_ref().unwrap();
        assert_eq!((thumb.image.width, thumb.image.height), (512, 341));
        s.accept(&r);
        let cached = s.media.get(p.id, SourceLevel::Thumb).unwrap();
        assert_eq!((cached.width, cached.height), (512, 341), "a thumbnail-sized source is kept, not the full source");
    }

    #[test]
    fn has_source_follows_what_the_session_accepted() {
        let mut s = crate::Session::with_demo();
        let id = s.active().unwrap();
        // (the thumbnail level: its demo scene is small and quick to generate)
        assert!(!s.media.has_source(id, SourceLevel::Thumb));
        let job = s.render_job(id, 256, 256, false, true).unwrap();
        assert_eq!(job.level, SourceLevel::Thumb);
        s.accept(&job.run());
        assert!(s.media.has_source(id, SourceLevel::Thumb));
        assert!(!s.media.has_source(id, SourceLevel::Preview));
        assert!(!s.media.has_source(PhotoId(id.0 + 1), SourceLevel::Thumb));
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
