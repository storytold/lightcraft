//! Slideshow slides in the engine: the library's slide layout ([`SlideSettings`], Lightroom
//! Classic's Layout, Options and Backdrop panels; the `slideshow.layout` command), its overlays
//! ([`crate::slide_overlay`], `slideshow.overlays`) and render jobs that compose a photo into its
//! slide ([`SlideCompose`], [`crate::media::RenderJob::with_slide`]).
//!
//! The composer itself is `lightcraft-slideshow` (no UI, no catalog): playback, the Settings
//! preview and `slideshow.preview` all compose through it, so they draw the same pixels.

use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::PhotoId;
use lightcraft_raster::Rgba8;
pub use lightcraft_slideshow::{Aspect, Layout, SlideSettings};

use crate::media::RenderJob;

/// The last rendered background image: (its render key, the image). Shared by the slide jobs of
/// a session, so the backdrop photo is rendered once, not once per slide.
pub type BackdropCache = Mutex<Option<(u64, Arc<Rgba8>)>>;

/// The background image of a slide: a library photo's render job and the shared cache it goes to.
#[derive(Clone)]
pub struct Backdrop {
    pub photo: PhotoId,
    job: Box<RenderJob>,
    cache: Arc<BackdropCache>,
}

impl Backdrop {
    /// The rendered background image (from the cache when it holds this render; otherwise
    /// rendered now, on the calling thread). `None` when it can't be rendered (the slide then
    /// shows the backdrop colour alone).
    pub fn image(&self) -> Option<Arc<Rgba8>> {
        let key = self.job.key;
        if let Some((k, img)) = self.cache.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && *k == key
        {
            return Some(img.clone());
        }
        let img = Arc::new((*self.job).clone().run().rendered.ok()?.image);
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some((key, img.clone()));
        Some(img)
    }
}

/// How to turn a photo's render into its slide: the layout, the slide's size in pixels and the
/// background image, if any.
#[derive(Clone)]
pub struct SlideCompose {
    pub settings: SlideSettings,
    pub width: usize,
    pub height: usize,
    pub backdrop: Option<Backdrop>,
    /// The overlays resolved for the slide's photo (`None`: none draw).
    pub overlays: Option<Arc<crate::slide_overlay::SlideOverlays>>,
}

impl SlideCompose {
    /// What makes the slide's pixels differ (for render keys).
    pub fn key(&self) -> u64 {
        self.settings.pixel_key()
            ^ (self.width as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (self.height as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ self.backdrop.as_ref().map_or(0, |b| b.job.key.rotate_left(29))
            ^ self.overlays.as_ref().map_or(0, |o| o.key().rotate_left(43))
    }

    /// The slide showing `photo` (rendered at [`lightcraft_slideshow::photo_size`], or any size).
    pub fn compose(&self, photo: &Rgba8) -> Rgba8 {
        let bg = self.backdrop.as_ref().and_then(Backdrop::image);
        let Some(o) = &self.overlays else {
            return lightcraft_slideshow::compose(photo, bg.as_deref(), &self.settings, self.width, self.height);
        };
        let (mut img, l) =
            lightcraft_slideshow::compose_with(photo, bg.as_deref(), &self.settings, self.width, self.height, &mut |img, l| o.draw_behind(img, l));
        o.draw_front(&mut img, &l);
        img
    }
}

impl crate::Session {
    /// The aspect (width / height) photo `id` shows at: cropped, straightened, lens-corrected.
    pub fn slide_photo_aspect(&self, id: PhotoId) -> Option<f64> {
        let p = self.catalog.photo(id)?;
        let frame = lightcraft_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &p.develop,
            true,
            p.embedded_lens.as_ref(),
        );
        let a = frame.aspect();
        Some(if a.is_finite() && a > 0.0 { a } else { 1.5 })
    }

    /// Does a slide show nothing but the photo fitted on black (the plain layout and no
    /// overlays)? Playback then shows the photo's own render.
    pub fn slides_plain(&self) -> bool {
        self.slide_layout.is_plain() && !self.slide_overlays.any()
    }

    /// The composition of `settings` at `width × height`, with its background image's render job
    /// (the photo `settings.background_image` names, when it is in the catalog) and the library's
    /// overlays resolved for `photo`.
    pub fn slide_compose(&mut self, settings: &SlideSettings, width: usize, height: usize, photo: Option<PhotoId>) -> Arc<SlideCompose> {
        let overlays = self.slide_overlays.clone();
        self.slide_compose_with(settings, &overlays, width, height, photo)
    }

    /// [`Self::slide_compose`] with `overlays` in place of the library's (a template's preview).
    pub fn slide_compose_with(
        &mut self,
        settings: &SlideSettings,
        overlays: &crate::slide_overlay::OverlaySettings,
        width: usize,
        height: usize,
        photo: Option<PhotoId>,
    ) -> Arc<SlideCompose> {
        let settings = settings.clone().sanitized();
        let backdrop = settings.background_image.filter(|_| settings.background_opacity > 0.0).and_then(|bid| {
            let id = PhotoId(bid);
            let aspect = self.slide_photo_aspect(id)?;
            let fill =
                SlideSettings { zoom_to_fill: true, margin_left: 0.0, margin_right: 0.0, margin_top: 0.0, margin_bottom: 0.0, ..settings.clone() };
            let (w, h) = lightcraft_slideshow::photo_size(&fill, aspect, width, height);
            let job = self.render_job(id, w, h, false, true)?;
            Some(Backdrop { photo: id, job: Box::new(job), cache: self.slide_backdrop.clone() })
        });
        let overlays = overlays.any().then(|| {
            let p = photo.and_then(|id| self.catalog.photo(id));
            Arc::new(crate::slide_overlay::SlideOverlays::resolve(overlays, p.map(|p| &**p)))
        });
        Arc::new(SlideCompose { settings, width, height, backdrop, overlays })
    }

    /// A render job whose result is photo `id`'s slide at `width × height` with the library's
    /// slide layout: the photo is rendered at the size its cell needs (its long edge at most
    /// `photo_edge`, when given), then composed on the worker. The photo's render is kept as its
    /// view preview, like the loupe's.
    pub fn slide_job(&mut self, id: PhotoId, width: usize, height: usize, photo_edge: Option<usize>) -> Option<RenderJob> {
        let settings = self.slide_layout.clone();
        self.slide_job_with(&settings, id, width, height, photo_edge, true)
    }

    /// [`Self::slide_job`] with `settings` in place of the library's layout; `view` keeps the
    /// photo's render as its view preview.
    pub fn slide_job_with(
        &mut self,
        settings: &SlideSettings,
        id: PhotoId,
        width: usize,
        height: usize,
        photo_edge: Option<usize>,
        view: bool,
    ) -> Option<RenderJob> {
        let overlays = self.slide_overlays.clone();
        self.slide_job_look(settings, &overlays, id, width, height, photo_edge, view)
    }

    /// [`Self::slide_job_with`] with `overlays` in place of the library's (a template's preview).
    #[allow(clippy::too_many_arguments)]
    pub fn slide_job_look(
        &mut self,
        settings: &SlideSettings,
        overlays: &crate::slide_overlay::OverlaySettings,
        id: PhotoId,
        width: usize,
        height: usize,
        photo_edge: Option<usize>,
        view: bool,
    ) -> Option<RenderJob> {
        let aspect = self.slide_photo_aspect(id)?;
        let (mut w, mut h) = lightcraft_slideshow::photo_size(settings, aspect, width, height);
        if let Some(edge) = photo_edge.filter(|e| *e > 0 && w.max(h) > *e) {
            let k = edge as f64 / w.max(h) as f64;
            (w, h) = (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1));
        }
        let job = if view { self.loupe_job(id, w.max(8), h.max(8), true)? } else { self.render_job(id, w.max(8), h.max(8), false, true)? };
        let compose = self.slide_compose_with(settings, overlays, width, height, Some(id));
        Some(job.with_slide(compose))
    }

    /// Photo `id`'s slide at `width × height` with `settings`, rendered now on this thread.
    pub fn slide_now(&mut self, settings: &SlideSettings, id: PhotoId, width: usize, height: usize) -> Result<Rgba8, String> {
        let overlays = self.slide_overlays.clone();
        self.slide_now_look(settings, &overlays, id, width, height)
    }

    /// [`Self::slide_now`] with `overlays` in place of the library's.
    pub fn slide_now_look(
        &mut self,
        settings: &SlideSettings,
        overlays: &crate::slide_overlay::OverlaySettings,
        id: PhotoId,
        width: usize,
        height: usize,
    ) -> Result<Rgba8, String> {
        let job = self.slide_job_look(settings, overlays, id, width, height, None, false).ok_or("no such photo")?;
        let r = job.run();
        self.accept(&r);
        r.rendered.map(|r| r.image)
    }
}
