use super::quick::QuickSource;
use super::source::{DecodedSource, SourceLevel, SourceRef};
use super::*;

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
            };
        }
        let was_loaded = matches!(self.source, SourceRef::Loaded(_));
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
                let (mut rendered, gpu_fallback) = develop_reporting(src, &info, &self.settings, &self.request, self.stages.as_deref(), gpu);
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

fn develop_image(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    gpu: bool,
) -> (Rendered, Option<String>) {
    // LUT profiles have no GPU stage: they render on the CPU
    let fallback = if gpu
        && !lightcraft_pipeline::lut::is_lut_profile(&s.profile.id)
        && !s.masks.iter().any(|m| m.visible && m.refine > 0.0)
        && req.proof.is_none()
    {
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
