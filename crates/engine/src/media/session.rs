use super::quick::{QuickJob, content_key, source_info};
use super::render::{RenderJob, RenderResult};
use super::source::{PreviewLoader, RENDER_CACHE_VERSION, SourceLevel, SourceRef, THUMB_SIZES, face_edge, source_edge_needed, thumb_bucket};
use super::{Arc, DevelopSettings, Hash128, Hasher128, MediaKind, Op, Photo, PhotoId, RenderRequest, Rendered, Rgb32f, Source, SourceInfo};

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
