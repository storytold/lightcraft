use super::*;

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
pub(super) const THUMB_SOURCE_BYTES: usize = 384 << 20;

pub(super) const RENDERED_MEM_BYTES: usize = 128 << 20;

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
pub(super) fn source_edge_needed(p: &Photo, settings: &DevelopSettings, apply_crop: bool, out_long: usize) -> usize {
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
