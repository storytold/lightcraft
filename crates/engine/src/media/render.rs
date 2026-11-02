use super::quick::QuickSource;
use super::source::{DecodedSource, SourceLevel, SourceRef, VIEW_REUSE_SLACK};
use super::{
    Arc, DevelopSettings, Hash128, Histogram, PhotoId, PreviewCache, Quality, RenderRequest, Rendered, Rgb32f, Rgba8, Source, SourceInfo, StageCache,
};

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
    /// Shown on screen at once and not kept: the result may stay on the GPU as a texture
    /// ([`Self::on_screen`]).
    pub on_screen: bool,
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

/// A render left on the GPU for the window to draw ([`RenderResult::frame`]).
#[cfg(not(target_arch = "wasm32"))]
pub type ScreenFrame = lightcraft_gpu::DisplayFrame;
/// A render left on the GPU for the window to draw (never, in the browser build).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
pub struct ScreenFrame {
    pub width: usize,
    pub height: usize,
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
    /// The result as a texture on the window's GPU device ([`RenderJob::on_screen`]); `rendered`
    /// then holds its histogram and an empty image.
    pub frame: Option<ScreenFrame>,
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

    /// The result is only drawn (an interactive draft of a view): when the GPU renderer runs on the
    /// window's device it may hand back a texture ([`RenderResult::frame`]) instead of reading the
    /// pixels back. Ignored where pixels are needed: thumbnails (cached), a full-quality render
    /// kept as the view preview, and a monitor profile (corrected on the CPU).
    pub fn on_screen(mut self) -> Self {
        self.on_screen = true;
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

    /// The view render kept for this job's photo and settings ([`Self::view_cache`]), fitted to the
    /// size asked for, when it is at least that big, or unchanged when it covers the job's display
    /// fit (else `None`: render). Only for what the cached
    /// view is: a full-quality, 8-bit, whole-frame SDR render (the cache holds sRGB; a display
    /// profile is applied to it like to any cached picture, [`crate::display::present`]).
    pub fn cached_view(&self) -> Option<Rgba8> {
        let (cache, key) = self.view_cache.as_ref()?;
        if self.request.quality != Quality::Full
            || self.request.depth != lightcraft_pipeline::OutputDepth::U8
            || self.request.window.is_some()
            || self.settings.hdr.enabled
            || self.slide.is_some()
        {
            return None;
        }
        let img = cache.get_at(self.cache_generation, *key)?;
        fit_cached(&img, self.request.max_w, self.request.max_h).or_else(|| {
            // the loupe at rest asks for at least the preview level and shows the render shrunk to
            // the drawn size (the display fit): a cached view that covers the drawn size is enough
            let (fw, fh) = self.display_fit?;
            let (w, h) = (img.width, img.height);
            let s = (fw as f64 / w.max(1) as f64).min(fh as f64 / h.max(1) as f64);
            (w > 0 && h > 0 && fw > 0 && fh > 0 && s <= 1.0 + VIEW_REUSE_SLACK).then(|| Arc::unwrap_or_clone(img))
        })
    }

    /// The photo's render (without slide composition).
    fn run_photo(self) -> RenderResult {
        // the loupe: a cached view render of these settings at least this big is the render (built
        // previews and the photos prepared ahead are used, not rendered again; PERF-PREVIEW-REUSE)
        if let Some(mut image) = self.cached_view() {
            // as for a render: the histogram of the full picture, then the display fit
            let histogram = Histogram::of_srgb8(&image);
            if let Some((fw, fh)) = self.display_fit
                && (image.width > fw.max(1) || image.height > fh.max(1))
            {
                image = fit_for_display(&image, fw, fh);
            }
            return RenderResult {
                request_id: self.request_id,
                source_key: self.source_key,
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None }),
                gpu_fallback: None,
                loaded: None,
                quick: Some(QuickSource::Cached),
                display: None,
                frame: None,
            };
        }
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
                photo: self.photo,
                level: self.level,
                key: self.key,
                rendered: Ok(Rendered { image, histogram, deep: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
                frame: None,
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
                // Views and exports use the GPU when there is one (waiting for the device if it is
                // still being created). Thumbnails (grid, filmstrip, preset and profile previews)
                // use it once it is ready - ~20-30 % faster than the CPU at 256 px with six jobs
                // side by side (`crates/gpu/tests/it/thumbs_bench.rs`), and the cores stay free for
                // decoding - but never wait for it: at startup they render on the CPU.
                let gpu = self.cache.is_none() || lightcraft_gpu::ready();
                // a clipping preview tests sRGB values, like the histogram: rendered in sRGB, then
                // converted for the display below
                let clip_view = self.request.overlay.is_clipping();
                let request = if clip_view { RenderRequest { display: None, ..self.request } } else { self.request };
                let pixels_needed =
                    self.cache.is_some() || self.display.is_some() || (self.view_cache.is_some() && self.request.quality == Quality::Full);
                let frame = (self.on_screen && !pixels_needed && gpu)
                    .then(|| develop_on_screen(src, &info, &self.settings, &request, self.stages.as_deref()));
                let (mut rendered, gpu_fallback) = match frame {
                    Some(OnScreen::Frame(frame, histogram)) => {
                        return RenderResult {
                            request_id: self.request_id,
                            source_key: self.source_key,
                            photo: self.photo,
                            level: self.level,
                            key: self.key,
                            rendered: Ok(Rendered {
                                image: lightcraft_raster::Rgba8 { width: 0, height: 0, data: Vec::new() },
                                histogram,
                                deep: None,
                            }),
                            gpu_fallback: None,
                            loaded: (!was_loaded).then_some(source),
                            quick: None,
                            display: None,
                            frame: Some(frame),
                        };
                    }
                    Some(OnScreen::Pixels(r)) => (r, None),
                    None => develop_reporting(src, &info, &self.settings, &request, self.stages.as_deref(), gpu),
                };
                // for a display profile: the image goes to the display's values, the histogram
                // and the cached view preview stay sRGB
                let mut srgb = None;
                let display = match (&self.display, self.request.display) {
                    // a diagnostic view: the histogram stays the photo's (taken before the overlay
                    // was drawn), and nothing goes to the view cache
                    (Some(d), Some(_)) if self.request.overlay != lightcraft_pipeline::Overlay::None => {
                        let converted = if clip_view { d.profile.from_srgb(&mut rendered.image) } else { d.profile.correct(&mut rendered.image) };
                        if let Err(e) = converted {
                            log::warn!("display profile: {e}");
                        }
                        Some(d.id())
                    }
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
                    frame: None,
                }
            }
            Err(e) => self.failed(e),
        }
    }
}

/// `img` fitted into `max_w × max_h` (its aspect kept), when it is big enough to need no
/// enlarging (within [`VIEW_REUSE_SLACK`]): as it is, or shrunk by averaging.
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
    Some(shrink_rgba(img, nw, nh))
}

/// `img` shrunk to `nw × nh` (no larger than it), each pixel the average of the ones it covers.
fn shrink_rgba(img: &Rgba8, nw: usize, nh: usize) -> Rgba8 {
    let (w, h) = (img.width, img.height);
    let (nw, nh) = (nw.clamp(1, w.max(1)), nh.clamp(1, h.max(1)));
    // the source columns / rows each output column / row covers: [start, end)
    let spans = |n: usize, of: usize| -> Vec<(usize, usize)> {
        (0..n)
            .map(|i| {
                let a = i.saturating_mul(of) / n;
                let b = (i.saturating_add(1).saturating_mul(of) / n).max(a.saturating_add(1)).min(of);
                (a, b)
            })
            .collect()
    };
    let (xs, ys) = (spans(nw, w), spans(nh, h));
    let mut data = Vec::with_capacity(nw.saturating_mul(nh));
    let mut row = vec![[0u32; 4]; nw];
    for &(y0, y1) in &ys {
        row.iter_mut().for_each(|r| *r = [0; 4]);
        for y in y0..y1 {
            let Some(line) = img.data.get(y * w..(y + 1) * w) else { continue };
            for (acc, &(x0, x1)) in row.iter_mut().zip(&xs) {
                for px in line.get(x0..x1).unwrap_or(&[]) {
                    for (a, v) in acc.iter_mut().zip(px) {
                        *a += u32::from(*v);
                    }
                }
            }
        }
        for (acc, &(x0, x1)) in row.iter().zip(&xs) {
            let n = (x1.saturating_sub(x0).saturating_mul(y1.saturating_sub(y0))).max(1) as u32;
            data.push(acc.map(|a| ((a + n / 2) / n).min(255) as u8));
        }
    }
    Rgba8 { width: nw, height: nh, data }
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
    // (LUT profiles, Refine Edges and soft proofs render on the GPU too)
    count_render(false);
    let fallback = if gpu {
        match lightcraft_gpu::try_render(src, info, s, req, stages) {
            Ok(r) => {
                count_render(true);
                return (r, None);
            }
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

thread_local! {
    /// This thread's renders are background work ([`background_gpu`]).
    static BACKGROUND_GPU: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Renders so far, by who they were for: `[interactive, background]` (all), then of those the ones
/// on the screen's card (`[2, 3]`) and on the second card (`[4, 5]`).
static RENDERS: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// Count one render (`gpu`: one more that the GPU rendered, on this thread's card) for this
/// thread's kind of work.
fn count_render(gpu: bool) {
    let card = match lightcraft_gpu::current_role() {
        _ if !gpu => 0,
        lightcraft_gpu::Role::Primary => 2,
        lightcraft_gpu::Role::Helper => 4,
    };
    let i = usize::from(rendering_in_background()) + card;
    if let Some(n) = RENDERS.get(i) {
        n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(test)]
    let _ = THREAD_RENDERS.try_with(|c| {
        let mut n = c.get();
        if let Some(k) = n.get_mut(i) {
            *k += 1;
        }
        c.set(n);
    });
}

#[cfg(test)]
thread_local! {
    /// [`RENDERS`] of this thread only (tests run side by side).
    static THREAD_RENDERS: std::cell::Cell<[u64; 6]> = const { std::cell::Cell::new([0; 6]) };
}

/// This thread's renders so far, as [`RENDERS`] (tests).
#[cfg(test)]
pub(crate) fn thread_render_counts() -> [u64; 6] {
    THREAD_RENDERS.with(std::cell::Cell::get)
}

/// Renders since the process started, by who they were for (see [`background_gpu`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderCounts {
    /// For what is on screen (the loupe and its drafts, the grid in view, compare, an export's
    /// own photo when it is not a batch).
    pub interactive: u64,
    /// Work done ahead ([`background_gpu`]).
    pub background: u64,
    /// Of `interactive` / `background`, those the screen's card (the editing card) rendered.
    pub interactive_editing: u64,
    pub background_editing: u64,
    /// Of `interactive` / `background`, those the second graphics card rendered
    /// (`interactive_helper` stays 0: what is on screen never renders there).
    pub interactive_helper: u64,
    pub background_helper: u64,
}

/// See [`RenderCounts`].
pub fn render_counts() -> RenderCounts {
    let n = |i: usize| RENDERS.get(i).map_or(0, |n| n.load(std::sync::atomic::Ordering::Relaxed));
    RenderCounts {
        interactive: n(0),
        background: n(1),
        interactive_editing: n(2),
        background_editing: n(3),
        interactive_helper: n(4),
        background_helper: n(5),
    }
}

/// Is this thread rendering background work ([`background_gpu`])?
pub fn rendering_in_background() -> bool {
    BACKGROUND_GPU.try_with(std::cell::Cell::get).unwrap_or(false)
}

/// Resets [`BACKGROUND_GPU`] when [`background_gpu`] ends, also when its work panicked.
struct BackgroundMark(bool);

impl BackgroundMark {
    fn set() -> Self {
        BackgroundMark(BACKGROUND_GPU.try_with(|c| c.replace(true)).unwrap_or(false))
    }
}

impl Drop for BackgroundMark {
    fn drop(&mut self) {
        let was = self.0;
        let _ = BACKGROUND_GPU.try_with(|c| c.set(was));
    }
}

/// Run `f`, work done ahead of the user rather than for what is on screen (finding eyes ahead of
/// culling, a smart preview's original read), as background GPU work: its renders take turns at
/// the GPU as batch work ([`lightcraft_gpu::as_batch`]) behind the photo on screen, which never
/// waits for them, on the second graphics card when it is ready (design §8 of 2026-10-31: the
/// second card first, always). The photo on screen, its drafts and the window's textures never
/// run in here. A panic inside `f` is its error (`what` names the work), so the thread's routing
/// is restored whatever happens. Without a second card (one card, or Settings ▸ Second graphics
/// card: Off) it is batch work on the one card, as before.
///
/// Work that renders photos one by one takes a turn per photo instead ([`background_photo`]).
pub fn background_gpu<R>(what: &str, f: impl FnOnce() -> R) -> std::result::Result<R, String> {
    let _mark = BackgroundMark::set();
    lightcraft_gpu::as_batch(|| lightcraft_gpu::on_helper(|| crate::guard::catch(what, f)))
}

/// [`background_gpu`] for a job pool's background jobs ([`lightcraft_preview::pool::JobPool::run_background_with`]:
/// the grid's thumbnails rendered behind their stand-ins): `f` runs once, a panic goes through
/// (the job's own guard catches it).
pub fn as_background_gpu(f: &mut dyn FnMut()) {
    let _mark = BackgroundMark::set();
    lightcraft_gpu::as_batch(|| lightcraft_gpu::on_helper(f));
}

/// [`background_gpu`] for the renders of one photo (a source of `src_px` pixels, results of at most
/// `out_px`; Build Previews, the background preparation, the import's previews): they wait for a
/// turn on whichever card has room first — the second card, else the screen's card while nobody
/// touches the interface (its gate yields to it, §8) — and all run on that card. Without a GPU
/// (or while LightCraft closes) they render on the CPU.
pub fn background_photo<R>(what: &str, src_px: usize, out_px: usize, f: impl FnOnce() -> R) -> std::result::Result<R, String> {
    let _mark = BackgroundMark::set();
    lightcraft_gpu::as_batch(|| match lightcraft_gpu::wait_batch_turn_any(src_px, out_px, &lightcraft_gpu::shutting_down) {
        Some(turn) => {
            let r = crate::guard::catch(what, f);
            drop(turn);
            r
        }
        None => lightcraft_gpu::on_cpu(|| crate::guard::catch(what, f)),
    })
}

/// What [`develop_on_screen`] made.
enum OnScreen {
    /// A texture on the window's device, and the histogram (never in the browser build).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Frame(ScreenFrame, Histogram),
    /// Pixels (GPU or CPU).
    Pixels(Rendered),
}

/// [`develop`] for a render that is only drawn: a texture on the window's device when the GPU
/// renderer can hand one over, else the pixels.
fn develop_on_screen(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>) -> OnScreen {
    #[cfg(not(target_arch = "wasm32"))]
    match lightcraft_gpu::render_display(src, info, s, req, stages) {
        // an HDR edit's histogram shows the HDR range, as `develop_reporting` gives it
        Some(lightcraft_gpu::Output::Display { frame, histogram }) => {
            count_render(false);
            count_render(true);
            OnScreen::Frame(frame, lightcraft_pipeline::hdr_histogram(src, info, s, req).unwrap_or(histogram))
        }
        Some(lightcraft_gpu::Output::Image(mut r)) => {
            count_render(false);
            count_render(true);
            if let Some(h) = lightcraft_pipeline::hdr_histogram(src, info, s, req) {
                r.histogram = h;
            }
            OnScreen::Pixels(r)
        }
        None => OnScreen::Pixels(develop(src, info, s, req, stages, false)),
    }
    #[cfg(target_arch = "wasm32")]
    OnScreen::Pixels(develop(src, info, s, req, stages, false))
}
