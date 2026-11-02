use super::render::fit_cached;
use super::render::{RenderJob, RenderResult};
use super::source::{DecodedSource, PreviewLoader, RENDER_CACHE_VERSION, SourceLevel, SourceRef};
use super::{Arc, Deserialize, Hash128, Hasher128, Histogram, Photo, PhotoId, PreviewCache, Rendered, Rgb32f, Rgba8, Serialize, Source, SourceInfo};

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
    /// the sensor data (crop, tone and colour edits as they will look, white balance as shot): what an
    /// edited photo that was never rendered shows at once, until its render arrives.
    Draft,
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
    /// the cache (memory only, never its disk), the key for this photo and edge, and the cache's
    /// generation when the job was made (a Discard Preview Cache since then keeps the picture out).
    pub embedded_cache: Option<(Arc<PreviewCache>, Hash128, u64)>,
    /// Whether [`Self::embedded_cache`] keeps the decoded preview on disk too: a grid thumbnail,
    /// so a folder seen before fills in from small files at once instead of every raw being read
    /// and its camera JPEG decoded again (the loupe's large stand-ins stay in memory).
    pub embedded_on_disk: bool,
    /// Import tiles need this many pixels when a render can provide them. `None` accepts any
    /// camera JPEG immediately (ordinary transient stand-ins and preview-only imports).
    /// An undersized JPEG remains the fallback if the render fails or is unavailable.
    pub minimum_embedded_edge: Option<usize>,
    /// Last resort: run this thumbnail job.
    pub small: Option<Box<RenderJob>>,
    /// The photo's latest pictures (aliases, [`PreviewCache::get_latest`]), tried after `cached` and
    /// before `embedded` ([`QuickSource::Last`]).
    pub latest: Vec<(Arc<PreviewCache>, Hash128)>,
    /// Shrink a picture taken from the caches to fit this (a loupe view standing in for a grid
    /// thumbnail is not kept at 2 000 px for a 256 px cell).
    pub fit: Option<(usize, usize)>,
    /// A fast draft of an edited raw with its settings ([`QuickSource::Draft`]): the camera JPEG at
    /// (path, loader, max edge) developed by this job, tried after `latest` and before `small`.
    pub draft: Option<(String, PreviewLoader, usize, Box<RenderJob>)>,
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
        };
        let rendered = |image: Rgba8| {
            let histogram = Histogram::of_srgb8(&image);
            Ok(Rendered { image, histogram, deep: None })
        };
        let fitted = |img: Arc<Rgba8>| match self.fit {
            Some((w, h)) if img.width > w.max(1) || img.height > h.max(1) => fit_cached(&img, w, h).unwrap_or_else(|| Arc::unwrap_or_clone(img)),
            _ => Arc::unwrap_or_clone(img),
        };
        for (cache, k) in &self.cached {
            if let Some(img) = cache.get(*k) {
                return done(rendered(fitted(img)), Some(QuickSource::Cached));
            }
        }
        for (cache, k) in &self.latest {
            if let Some(img) = cache.get_latest(*k) {
                return done(rendered(fitted(img)), Some(QuickSource::Last));
            }
        }
        if let Some((path, loader, edge, job)) = self.draft.as_ref()
            && let Some(r) = draft_from_camera_jpeg(path, loader, *edge, job)
        {
            return RenderResult { request_id, key, quick: Some(QuickSource::Draft), ..r };
        }
        let mut embedded_fallback = None;
        if let Some((path, loader, edge)) = &self.embedded {
            let kept =
                self.embedded_cache.as_ref().and_then(|(cache, k, _)| if self.embedded_on_disk { cache.get(*k) } else { cache.get_memory(*k) });
            let image = match kept {
                Some(img) => Some(Arc::unwrap_or_clone(img)),
                None => {
                    let image = loader(path, *edge);
                    // written for the generation the job was made in: a cache cleared meanwhile stays clear
                    if let (Some(image), Some((cache, k, generation))) = (&image, &self.embedded_cache) {
                        if self.embedded_on_disk {
                            cache.put_deferred_at(*generation, *k, Arc::new(image.clone()));
                        } else {
                            cache.put_memory_at(*generation, *k, Arc::new(image.clone()));
                        }
                    }
                    image
                }
            };
            if let Some(image) = image {
                if self.minimum_embedded_edge.is_none_or(|minimum| image.width.max(image.height) >= minimum) {
                    return done(rendered(image), Some(QuickSource::Embedded));
                }
                embedded_fallback = Some(image);
            }
        }
        if let Some(job) = self.small {
            let r = job.run();
            if r.rendered.is_ok() || embedded_fallback.is_none() {
                return RenderResult { request_id, key, quick: Some(QuickSource::Small), ..r };
            }
        }
        if let Some(image) = embedded_fallback {
            return done(rendered(image), Some(QuickSource::Embedded));
        }
        done(Err("no quick preview".into()), None)
    }
}

/// The longest edge a draft from the camera JPEG is developed at ([`QuickSource::Draft`]).
pub const DRAFT_EDGE: usize = 1600;

/// Develop `job` (an edited raw's render job) from its camera JPEG (`loader` at `path`, at most
/// `edge` px) instead of its sensor data: a draft with its settings, white balance as shot (the
/// JPEG's colours are the camera's, on no Kelvin scale). `None`: the JPEG can't be read, or the
/// render failed.
fn draft_from_camera_jpeg(path: &str, loader: &PreviewLoader, edge: usize, job: &RenderJob) -> Option<RenderResult> {
    let jpeg = loader(path, edge.clamp(1, DRAFT_EDGE))?;
    if jpeg.width == 0 || jpeg.height == 0 {
        return None;
    }
    let lin: Vec<f32> = (0..=255u8).map(|v| lightcraft_color::transfer::srgb_to_linear(f32::from(v) / 255.0)).collect();
    let to_working = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING);
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
    settings.wb = lightcraft_develop::WhiteBalance::default();
    let mut job = job.clone();
    job.source = SourceRef::Loaded(Box::new(DecodedSource::new(Arc::new(image), Some(info.clone()))));
    job.info = info;
    job.settings = Arc::new(settings);
    job.cache = None;
    job.view_cache = None;
    job.latest = None;
    job.stages = None;
    job.preview_cut = None;
    let mut r = job.draft().run();
    r.loaded = None;
    r.source_key = None;
    r.rendered.is_ok().then_some(r)
}

/// The thumbnail cache's key for the camera JPEG standing for the thumbnail of cache key `k`.
pub(super) fn embedded_thumb_key(k: Hash128) -> Hash128 {
    Hasher128::new().str("embedded-thumb").u64((k.0 >> 64) as u64).u64(k.0 as u64).finish()
}

/// The alias of photo `p`'s latest loupe view, whatever settings it was rendered with
/// ([`PreviewCache::put_latest`]): an edited photo's first picture while the render for its current
/// settings is on its way (notes 3, 11 and 31 of 2026-10-24). The photo's id is part of it: virtual
/// copies share their file, but each shows its own edit.
pub fn latest_view_key(p: &Photo, apply_crop: bool) -> Hash128 {
    Hasher128::new().str(&content_key(p)).str("latest-view").u64(p.id.0).u64(apply_crop as u64).u64(RENDER_CACHE_VERSION).finish()
}

/// The alias of photo `p`'s latest grid thumbnail, whatever settings it was rendered with (one per
/// photo, virtual copies too).
pub fn latest_thumb_key(p: &Photo) -> Hash128 {
    Hasher128::new().str(&content_key(p)).str("latest-thumb").u64(p.id.0).u64(RENDER_CACHE_VERSION).finish()
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
