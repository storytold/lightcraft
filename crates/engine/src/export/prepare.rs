use super::batch::{card_turn_now, card_turn_waiting, export_gpu, export_mix, run_batch, uses_helper};
use super::encoding::{encode_gain_map_jpeg, encode_rendered, export_metadata, removes_location};
use super::options::ExportFormat;
use super::params::apple_photos_unsupported;
use super::{DeepImage, ExportOptions, Metadata, OutputDepth, lap, lap_clock};

/// One exported file.
pub struct Exported {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Files written next to it, named like it with another extension: `(extension, bytes)` (the
    /// XMP sidecar of an `Original` export).
    pub sidecars: Vec<(&'static str, Vec<u8>)>,
    /// Set when the render was tried on the GPU and fell back to the CPU: why (see
    /// `docs/gpu-pipeline.md`). The file holds the CPU render either way.
    pub gpu_fallback: Option<String>,
    /// The graphics card that rendered it (its name), `None` when the processor did: with two
    /// cards, which one took this photo.
    pub gpu_device: Option<String>,
}

/// Output size of photo `p` under `o` (its cropped full size when `o.resize` is `None`).
pub fn output_size(p: &lightcraft_catalog::Photo, o: &ExportOptions) -> (usize, usize) {
    let (w, h) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
    // whole pixels first: a crop's float noise (5999.999… for 6000) must not decide a half-pixel tie
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    match &o.resize {
        Some(r) => r.apply(w, h),
        None => (w as usize, h as usize),
    }
}

/// Render photo `id` at the requested size and encode it (or copy / convert its original for
/// [`ExportFormat::Original`] / [`ExportFormat::Dng`]).
pub fn export_photo(session: &mut crate::Session, id: lightcraft_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<Exported, String> {
    prepare_export(session, id, o, seq)?.run()
}

/// One photo's export, set up from the session ([`prepare_export`]). [`PreparedExport::run`] does
/// the heavy part (read, decode, render, encode) without the session, so it can run on another
/// thread.
pub struct PreparedExport {
    pub photo: lightcraft_catalog::PhotoId,
    pub file_name: String,
    /// The folder of the photo's original on disk ([`DestinationMode::SameAsOriginal`]); `None`
    /// for a demo photo or one in the browser's storage.
    pub source_dir: Option<String>,
    work: Work,
    /// The library's originals, which [`run_batch`] never writes over (shared by a batch).
    pub(super) guard: std::sync::Arc<crate::originals::OriginalGuard>,
    /// Estimated working memory of [`Self::run`] (bytes), held from [`crate::memory::export_gate`]
    /// while it runs beside other photos of a batch.
    pub(super) weight: usize,
    /// Its renders may use the GPU ([`export_gpu`] when it was prepared).
    pub(super) gpu: bool,
    /// When the GPU is busy with other photos it renders on the processor instead of waiting
    /// ([`export_mix`] when it was prepared).
    pub(super) mix: bool,
}

struct RenderWork {
    job: crate::media::RenderJob,
    /// The HDR rendition of a gain map JPEG (`job` renders its SDR base).
    hdr_job: Option<crate::media::RenderJob>,
    meta: Option<Metadata>,
    opts: ExportOptions,
    /// Source pixels the render reads (its GPU memory, for a gain map's turn at the device).
    src_px: usize,
}

enum Work {
    Render(Box<RenderWork>),
    /// [`ExportFormat::Original`] (the file's bytes + an XMP sidecar) or [`ExportFormat::Dng`] (the
    /// raw data re-encoded as a lossless DNG with the edits in its XMP).
    File {
        path: String,
        read: Option<crate::merge::ByteReader>,
        packet: String,
        dng: Option<DngCompression>,
        /// Lossy DNG: JPEG quality and the long edge to scale to (`None`: full size).
        quality: u8,
        lossy_edge: Option<u32>,
        /// The original must not carry a location ([`removes_location`]).
        remove_location: bool,
        label: String,
        size: (usize, usize),
    },
}

/// Set up the export of photo `id` at 1-based position `seq` of a batch. (For a whole batch use
/// [`prepare_batch`]: it looks at the library's originals once.)
pub fn prepare_export(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
) -> Result<PreparedExport, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    prepare_guarded(session, id, o, seq, guard)
}

/// [`prepare_export`] for each of `ids` in order (`{seq}` = position, from 1).
pub fn prepare_batch(session: &mut crate::Session, ids: &[lightcraft_catalog::PhotoId], o: &ExportOptions) -> Result<Vec<PreparedExport>, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    ids.iter().enumerate().map(|(i, id)| prepare_guarded(session, *id, o, i + 1, guard.clone())).collect()
}

fn prepare_guarded(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    let file_name = o.file_name_for(p, seq);
    let source_dir = original_folder(&p.source);
    // HDR output only for photos edited in HDR; the rest of the batch exports as usual
    let sdr_opts;
    let o = if o.hdr && !p.develop.hdr.enabled {
        sdr_opts = ExportOptions { hdr: false, ..o.clone() };
        &sdr_opts
    } else {
        o
    };
    let (full_px, long) = ((p.width as usize).saturating_mul(p.height as usize), p.width.max(p.height) as usize);
    let (work, weight) = if o.format.is_rendered() {
        let (w, h) = output_size(p, o);
        let (nw, nh) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
        let native_px = (nw.round().max(1.0) * nh.round().max(1.0)) as usize;
        let label = p.file_name.clone();
        let meta = export_metadata(p, &session.catalog, o);
        let gain_map = o.hdr_output() && o.format == ExportFormat::Jpeg;
        let job = session.export_job(id, w, h, o.effective_space(), o.effective_depth())?;
        // a gain map also holds the HDR rendition (float), the SDR one in linear light and the
        // stages the two renders share (counted before the check below: review of PR #3565)
        let weight = with_gain_map(render_weight(full_px, long, job.level, w.saturating_mul(h)), (w, h), gain_map);
        if let Some(why) = too_big_for_memory((w, h), native_px, weight, crate::memory::total_ram()) {
            return Err(format!("{label}: {why}"));
        }
        let hdr_job = if gain_map { Some(session.export_job(id, w, h, o.effective_space(), OutputDepth::F32Hdr)?) } else { None };
        let src_px = source_px(full_px, long, job.level);
        (Work::Render(Box::new(RenderWork { job, hdr_job, meta, opts: o.clone(), src_px })), weight)
    } else {
        let lightcraft_catalog::Source::File { path } = &p.source else {
            return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
        };
        let dng = (o.format == ExportFormat::Dng).then_some(o.dng_compression);
        // the file's bytes; a DNG also decodes the raw data (16-bit samples) and writes it again
        let weight = (p.file_size as usize).saturating_add(if dng.is_some() { full_px.saturating_mul(6) } else { 0 });
        let work = Work::File {
            path: path.clone(),
            read: session.media.file_bytes.clone(),
            packet: crate::sidecar::sidecar_packet_with(p, &session.catalog, removes_location(p, &session.catalog, o)),
            dng,
            quality: o.quality,
            lossy_edge: o.resize.is_some().then(|| {
                let (w, h) = output_size(p, o);
                u32::try_from(w.max(h)).unwrap_or(u32::MAX)
            }),
            remove_location: removes_location(p, &session.catalog, o),
            label: p.file_name.clone(),
            size: (p.width as usize, p.height as usize),
        };
        (work, weight)
    };
    Ok(PreparedExport { photo: id, file_name, source_dir, work, guard, weight, gpu: export_gpu(), mix: export_mix() })
}

/// The folder a photo's original is in, for [`DestinationMode::SameAsOriginal`]. None for a demo
/// photo, and on the web, where an original lives in the browser's storage, not in a folder.
fn original_folder(source: &lightcraft_catalog::Source) -> Option<String> {
    let lightcraft_catalog::Source::File { path } = source else { return None };
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let dir = std::path::Path::new(path).parent()?.to_string_lossy().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// Why an export of `out` pixels (`w`, `h`), from a photo of `native_px` pixels at its own size,
/// can't be made on a computer with `ram` bytes, given its working memory `weight`: an export
/// enlarged past the photo's size that needs more memory than the computer has would fail to
/// allocate, which ends the process rather than the export. Full-size and smaller exports, and
/// computers whose memory isn't known, are never refused.
pub(super) fn too_big_for_memory(out: (usize, usize), native_px: usize, weight: usize, ram: Option<usize>) -> Option<String> {
    let ram = ram?;
    if out.0.saturating_mul(out.1) <= native_px || weight <= ram {
        return None;
    }
    let gb = |b: usize| b as f64 / (1u64 << 30) as f64;
    Some(format!(
        "exporting at {} × {} needs about {:.0} GB of memory, more than this computer has ({:.0} GB); choose a smaller size",
        out.0,
        out.1,
        gb(weight).ceil(),
        gb(ram)
    ))
}

/// `weight` with what a gain map JPEG of `out` pixels adds ([`GAIN_MAP_BYTES_PER_PX`]) when `gain_map`.
pub(super) fn with_gain_map(weight: usize, out: (usize, usize), gain_map: bool) -> usize {
    if gain_map { weight.saturating_add(out.0.saturating_mul(out.1).saturating_mul(GAIN_MAP_BYTES_PER_PX)) } else { weight }
}

/// Working memory a gain map JPEG adds per output pixel ([`encode_gain_map_jpeg`]): the float HDR
/// rendition (12 B), the SDR one in linear light (12 B) and the stages its two renders share.
const GAIN_MAP_BYTES_PER_PX: usize = 48;

/// Working memory of rendering a photo of `full_px` pixels (long edge `long`) from source level
/// `level` into `out_px` output pixels: the decoded source (linear RGB f32, 12 B/px) plus the
/// pipeline's planes and the encoded output (~36 B per output pixel). Measured: a 24 MP export
/// peaks at ~1.2 GB, one at 2048 px from the 2560 px preview at ~150 MB.
pub(super) fn render_weight(full_px: usize, long: usize, level: crate::media::SourceLevel, out_px: usize) -> usize {
    source_px(full_px, long, level).saturating_mul(12).saturating_add(out_px.saturating_mul(36))
}

/// Pixels of the source a render of a photo of `full_px` pixels (long edge `long`) reads from
/// source level `level`.
fn source_px(full_px: usize, long: usize, level: crate::media::SourceLevel) -> usize {
    let edge = level.max_edge().min(long.max(1));
    let scale = edge as f64 / long.max(1) as f64;
    (full_px as f64 * scale * scale).min(usize::MAX as f64 / 64.0) as usize
}

/// The error of a photo whose export was cancelled before it was done ([`PreparedExport::run_until`]).
pub const CANCELLED: &str = "export cancelled";

/// Whether an export's render job decodes its original when it runs (no cached picture stands in
/// for it): [`PreparedExport::run`] then decodes it first, outside the photo's turn at the GPU.
fn needs_source(job: &crate::media::RenderJob) -> bool {
    job.cache.is_none() && job.view_cache.is_none() && job.preview_cut.is_none() && !matches!(job.source, crate::media::SourceRef::Loaded(_))
}

thread_local! {
    /// This thread is inside a photo's renders ([`TurnMark`]): what holds its turn at the GPU.
    static IN_RENDERS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Marks the renders of one photo (what may hold its turn at the GPU) on this thread.
struct TurnMark;

impl TurnMark {
    fn enter() -> Self {
        let _ = IN_RENDERS.try_with(|c| c.set(c.get().saturating_add(1)));
        TurnMark
    }
}

impl Drop for TurnMark {
    fn drop(&mut self) {
        let _ = IN_RENDERS.try_with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Encodes that ran inside a photo's renders (tests: an encode must never hold the GPU turn).
#[cfg(test)]
pub(crate) static ENCODES_IN_TURN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Count an encode that runs inside a photo's renders (tests).
pub(super) fn note_encode() {
    #[cfg(test)]
    if IN_RENDERS.try_with(|c| c.get() > 0).unwrap_or(false) {
        ENCODES_IN_TURN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl PreparedExport {
    pub fn run(self) -> Result<Exported, String> {
        self.run_until(&|| false)
    }

    /// [`Self::run`], giving up ([`CANCELLED`]) between its steps (decode, renders, encode) once
    /// `stop()` says so: a cancelled export stops within a step, not after the photo.
    pub fn run_until(self, stop: &dyn Fn() -> bool) -> Result<Exported, String> {
        let check = || if stop() { Err(CANCELLED.to_string()) } else { Ok(()) };
        let file_name = self.file_name;
        match self.work {
            Work::Render(w) => {
                let RenderWork { mut job, mut hdr_job, meta, opts, src_px } = *w;
                // A gain map's HDR rendition renders on the GPU too when the SDR one can: from the
                // stages the SDR render left on the device, in one turn at the GPU (the memory
                // those stages hold is never another export's to take meanwhile). Asked on the
                // photo's own card (inside its `BatchTurn`, or on the processor in `on_cpu`).
                let (gpu, has_hdr) = (self.gpu, hdr_job.is_some());
                let hdr_on_gpu = move || {
                    gpu && has_hdr
                        && lightcraft_gpu::enabled()
                        && lightcraft_gpu::handles(OutputDepth::F32Hdr, false, lightcraft_pipeline::Overlay::None)
                };
                let out_px = job.request.max_w.saturating_mul(job.request.max_h);
                let mut t = lap_clock();
                // 1. The original is decoded (and AI-denoised when asked) before the photo's turn
                // at the GPU, on this lane's thread: other photos render on the GPU meanwhile
                // (note 28: the turn used to cover the decode and the encode too, so photos side
                // by side took turns at an idle card).
                check()?;
                if needs_source(&job) {
                    let src = job.source.load_source()?;
                    if let Some(hj) = &mut hdr_job {
                        hj.source = crate::media::SourceRef::Loaded(Box::new(src.clone()));
                    }
                    job.source = crate::media::SourceRef::Loaded(Box::new(src));
                    lap("decode", &mut t);
                }
                // 2. The renders (the GPU's part): the SDR rendition, and a gain map's HDR one
                // (the image, a gain map's HDR rendition, why the GPU fell back, the card that rendered it)
                type Renders = (lightcraft_pipeline::Rendered, Option<DeepImage>, Option<String>, Option<String>);
                let renders = || -> Result<Renders, String> {
                    let mut t = lap_clock();
                    lap("waiting for the GPU", &mut t);
                    // (cancelled while it waited for its turn: give the turn back at once)
                    check()?;
                    let _turn = TurnMark::enter();
                    let hdr_on_gpu = hdr_on_gpu();
                    // (nothing left over from an earlier photo on this lane: `gpuDevice` is this one's)
                    let _ = lightcraft_gpu::take_rendered_on();
                    // A gain map's HDR rendition is the same photo rendered a second time: it starts
                    // from the SDR render's decoded original and its stages (on the device, or when
                    // it renders on the CPU, the white-balanced, denoised, retouched image and
                    // spatial planes), instead of decoding and redoing them (half of such an
                    // export's work).
                    let stages = hdr_job.as_ref().map(|_| std::sync::Arc::new(lightcraft_pipeline::StageCache::default()));
                    if let Some(st) = &stages {
                        // (a GPU render leaves host copies of what the CPU's HDR render starts from)
                        st.keep_host_stages(!hdr_on_gpu);
                        job.stages = Some(st.clone());
                    }
                    let mut result = job.run();
                    lap("render", &mut t);
                    let loaded = result.loaded.take();
                    let r = result.rendered?;
                    let gpu_fallback = result.gpu_fallback;
                    let hdr = match hdr_job {
                        Some(mut hj) => {
                            check()?;
                            if let Some(src) = loaded {
                                hj.source = crate::media::SourceRef::Loaded(Box::new(src));
                            }
                            if let Some(st) = stages {
                                if !hdr_on_gpu {
                                    // (the GPU's device copies are no use to the CPU's HDR render)
                                    st.clear_extension();
                                }
                                hj.stages = Some(st);
                            }
                            let hr = hj.run();
                            // (an HDR-only fallback to the CPU is reported too: review of PR #3532)
                            let fell_back = hr.gpu_fallback;
                            let hr = hr.rendered?;
                            lap("HDR render", &mut t);
                            Some((hr.deep.ok_or("the HDR render has no float samples")?, fell_back))
                        }
                        None => None,
                    };
                    // the card that made the image (`None`: the processor did)
                    let device = lightcraft_gpu::take_rendered_on();
                    let (hdr, hdr_fallback) = hdr.map_or((None, None), |(d, f)| (Some(d), f));
                    Ok((r, hdr, gpu_fallback.or(hdr_fallback), device))
                };
                let (r, hdr, gpu_fallback, gpu_device) = if self.gpu && self.mix {
                    // the GPU and the processor at once: this photo takes a card that is free (the
                    // helper first, then the screen's), else it renders on the processor rather
                    // than wait (Test my machine picks it)
                    // (the turn routes this thread's renders to its card while it is held)
                    match card_turn_now(src_px, out_px) {
                        Some(_turn) => renders(),
                        None => {
                            lap("the GPU is busy: rendering on the processor", &mut t);
                            // (a panic is caught inside `on_cpu`, which then gives the lane the GPU back)
                            lightcraft_gpu::on_cpu(|| crate::guard::catch("rendering on the processor", renders)).and_then(|r| r)
                        }
                    }
                } else if self.gpu && uses_helper() {
                    // two cards: the photo waits for whichever has room first (the helper first),
                    // holding that card's turn for both its renditions
                    match card_turn_waiting(src_px, out_px, stop) {
                        Some(_turn) => renders(),
                        // no GPU to wait for (the renders go to the processor by themselves), or cancelled
                        None => {
                            check()?;
                            renders()
                        }
                    }
                } else if hdr_on_gpu() {
                    // one card: as before two cards, each render queues at the card after its plan
                    lightcraft_gpu::in_one_turn(src_px, out_px, renders)
                } else {
                    renders()
                }?;
                lap("renders", &mut t);
                check()?;
                // 3. The encode, on this lane's thread after its turn (the next photo renders meanwhile)
                let bytes = match &hdr {
                    Some(hdr) => encode_gain_map_jpeg(&r.image, hdr, &opts, meta.as_ref())?,
                    None => encode_rendered(&r, &opts, meta.as_ref())?,
                };
                lap("encode", &mut t);
                Ok(Exported { file_name, bytes, width: r.image.width, height: r.image.height, sidecars: Vec::new(), gpu_fallback, gpu_device })
            }
            Work::File { path, read, packet, dng, quality, lossy_edge, label, size, remove_location } => {
                check()?;
                let bytes = match &read {
                    Some(r) => r(&path)?,
                    None => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                };
                // (cancelled while the original was read: no decode or DNG write after it)
                check()?;
                let Some(dng) = dng else {
                    // the file is copied byte for byte: refuse rather than leak a location it embeds
                    if remove_location {
                        let metadata = lightcraft_meta::try_extract(&bytes)
                            .map_err(|e| format!("{label}: can't verify location metadata in this original file: {e}"))?;
                        if !bytes.starts_with(b"8BPS") {
                            let embedded = lightcraft_meta::embedded(&bytes);
                            if let Some(xmp) = embedded.xmp.as_deref() {
                                lightcraft_meta::parse_xmp(xmp).map_err(|e| {
                                    format!("{label}: can't verify location metadata in this original file: malformed embedded XMP: {e}")
                                })?;
                            }
                        }
                        let embeds_place_name =
                            [metadata.sublocation.as_deref(), metadata.city.as_deref(), metadata.state.as_deref(), metadata.country.as_deref()]
                                .into_iter()
                                .flatten()
                                .any(|value| !value.trim().is_empty());
                        if metadata.gps.is_some() || embeds_place_name {
                            return Err(format!(
                                "{label}: can't remove the location from an original file that embeds GPS or place names; export as JPEG or TIFF, or as DNG for a raw photo"
                            ));
                        }
                    }
                    return Ok(Exported {
                        file_name,
                        bytes,
                        width: size.0,
                        height: size.1,
                        sidecars: vec![("xmp", packet.into_bytes())],
                        gpu_fallback: None,
                        gpu_device: None,
                    });
                };
                if lightcraft_raw::probe(&bytes).is_none() {
                    return Err(format!("{label}: DNG export needs a raw photo"));
                }
                let mut raw = lightcraft_raw::decode(&bytes).map_err(|e| format!("{label}: {e}"))?;
                crate::camera_preview::with_starting_colour(&mut raw, &bytes);
                drop(bytes);
                let compression = match dng {
                    DngCompression::Lossless => lightcraft_raw::DngCompression::Lj92 { tile: 256 },
                    DngCompression::Deflate => lightcraft_raw::DngCompression::Deflate { tile: 256, half: false },
                    DngCompression::Uncompressed => lightcraft_raw::DngCompression::Uncompressed,
                    DngCompression::Lossy => {
                        lightcraft_raw::DngCompression::LossyJpeg { tile: 256, quality: quality.clamp(1, 100), long_edge: lossy_edge }
                    }
                };
                let dng = lightcraft_raw::write_dng(&raw, &lightcraft_raw::DngWriteOptions { xmp: Some(packet), compression, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                Ok(Exported {
                    file_name,
                    bytes: dng,
                    width: raw.width,
                    height: raw.height,
                    sidecars: Vec::new(),
                    gpu_fallback: None,
                    gpu_device: None,
                })
            }
        }
    }
}

/// How an exported DNG stores its raw data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DngCompression {
    /// Lossless JPEG (the usual DNG compression).
    #[default]
    Lossless,
    /// Deflate (zip) tiles with prediction.
    Deflate,
    Uncompressed,
    /// Lossy DNG: demosaiced linear data as JPEG tiles at the export quality, scaled to the resize
    /// setting when there is one (Lightroom's "Use Lossy Compression").
    Lossy,
}

/// The destination of a batch: a folder (with `ExportOptions::subfolder` and the conflict policy
/// applied to each file together with its sidecars; with [`DestinationMode::SameAsOriginal`] each
/// photo's own folder instead of `dir`), or one exact file path (single photo; an ordinary file
/// already there is replaced). Either way a catalogued original (or its sidecar)
/// is never written over: see [`crate::originals`].
#[derive(Clone, Debug, Default)]
pub struct Destination {
    pub dir: String,
    pub exact: Option<String>,
}

/// What happens after an export has written its files (Lightroom's Post-Processing): nothing, or
/// adding them to Apple Photos ([`ExportOptions::add_to_photos`]). Made by
/// [`crate::Session::after_export`] before the export starts, so an export asking for something
/// this session can't do fails before writing anything; for Photos it holds the import slot
/// (a [`crate::apple_photos::Reservation`]) until the files are handed over, and frees it when
/// dropped unused (the export failed or never ran). `Send`: a background export takes it to its
/// worker.
#[derive(Default)]
pub struct AfterExport {
    #[cfg(not(target_arch = "wasm32"))]
    apple_photos: Option<crate::apple_photos::Reservation>,
}

impl AfterExport {
    /// Add the files to Apple Photos with `runner`, into `album` when it isn't blank, reserving
    /// the import slot of `imports` now. (Front ends get this from
    /// [`crate::Session::after_export`], which checks the platform first.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn apple_photos(runner: crate::apple_photos::Runner, album: &str, imports: &crate::apple_photos::Imports) -> Result<Self, String> {
        Ok(Self { apple_photos: Some(imports.reserve(runner, album)?) })
    }

    /// Nothing to do.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_none(&self) -> bool {
        self.apple_photos.is_none()
    }

    /// Nothing to do.
    #[cfg(target_arch = "wasm32")]
    pub fn is_none(&self) -> bool {
        true
    }

    /// Do it now for an export's per-photo results ([`run_batch`]'s: only the files written
    /// count), waiting for Photos: for a worker thread (the app's background export) or a caller
    /// without a window (MCP, lightcraft-cli). `None` when there was nothing to do, else the
    /// outcome to report as the export's `applePhotos`: the finished job (`{job, running: false,
    /// requested, album}` with `{imported, ids, warning?}`, `{error}`, or `{skipped}` when the
    /// export was cancelled or wrote nothing). Never fails the export: its files are written
    /// either way.
    pub fn run(self, files: &[serde_json::Value], cancelled: bool) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.run(files, cancelled));
        }
        let _ = (files, cancelled);
        None
    }

    /// Like [`AfterExport::run`], without waiting: Photos works on a worker thread and the
    /// outcome is the running job (`{job, running: true, requested, album}`; its result comes
    /// from `export.photosImports`, and the desktop app announces it). For a caller on the UI
    /// thread, which must not wait minutes for Photos.
    pub fn start(self, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.start(files));
        }
        let _ = files;
        None
    }

    /// [`AfterExport::start`] where the session answers on a UI thread
    /// ([`crate::Session::apple_photos_background`]), else [`AfterExport::run`].
    pub fn finish(self, s: &crate::Session, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if s.apple_photos_background {
            return self.start(files);
        }
        let _ = s;
        self.run(files, false)
    }
}

impl crate::Session {
    /// What an export with `o` does after writing its files; an error, before anything is written,
    /// when it asks for Apple Photos and this platform or session has no way to reach it, or
    /// another import is reserved or running. For Photos the import slot is reserved from now on.
    pub fn after_export(&self, o: &ExportOptions) -> Result<AfterExport, String> {
        if !o.add_to_photos {
            return Ok(AfterExport::default());
        }
        if let Some(why) = apple_photos_unsupported() {
            return Err(why.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let runner = self.apple_photos.clone().ok_or_else(|| crate::apple_photos::unavailable().to_string())?;
            let mut after = AfterExport::apple_photos(runner, &o.photos_album, &self.apple_photos_imports)?;
            after.apple_photos = after.apple_photos.map(|r| r.shown_in(self.activity.clone()));
            Ok(after)
        }
        #[cfg(target_arch = "wasm32")]
        Err("Apple Photos is only available on macOS".into())
    }
}

/// Export `ids` in order ([`prepare_batch`] + [`run_batch`]), stopping at the first error.
pub fn export_batch(
    session: &mut crate::Session,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let items = prepare_batch(session, ids, o)?;
    run_batch(items, o, to, write, exists, true, &mut |_, _| true)
}
