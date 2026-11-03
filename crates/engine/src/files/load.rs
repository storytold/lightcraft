use super::colour::bin_factor;
use super::preview::{fit_box, load_embedded_preview};
use super::probe::preview_reason;
use super::{Arc, CameraColourModel, DenoiseSpec, Filter, HIGHLIGHT_CLIP, Orientation, Rgb32f, SourceInfo, fit, xy_to_temp_tint};

/// Decode a file into a linear Rec.2020 image no larger than `max_edge`, oriented.
///
/// Runs on a rayon worker: its many short parallel loops then start on the worker's own queue
/// instead of each one waking the pool from outside and waiting for it (which costs more than the
/// loops themselves when the machine is busy).
pub fn load_bytes(bytes: &[u8], max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(|_| load_bytes_now(std::borrow::Cow::Borrowed(bytes), max_edge, None, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_bytes`] taking the file's bytes: a raw file's bytes are freed as soon as it is decoded
/// (less memory held while it is developed).
pub fn load_vec(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_vec`] for a thumbnail: a DNG that keeps reduced-resolution copies of its raw image (Lightroom's HDR and
/// panorama merges) develops the smallest that covers `max_edge` instead of the whole image. The flag says whether
/// it did: such a picture is not the full image, and an export never renders from it.
pub fn load_vec_reduced(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo, bool), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, true).map(|(img, _, info, reduced)| (img, info, reduced)))
}

/// [`load_vec`] that also develops the photo's cached denoised picture (`spec`) from the same decode: the plain
/// picture, the denoised one (`None` when it cannot be read or does not fit this raw: the plain one is then all
/// there is) and the source facts. Both pictures are the same size.
pub fn load_vec_pair(bytes: Vec<u8>, max_edge: usize, spec: &DenoiseSpec) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, Some(spec), false).map(|(img, twin, info, _)| (img, twin, info)))
}

/// The denoised camera RGB of `raw` from its cached product, as the plain development would have it: the default
/// crop, and `factor` × `factor` blocks averaged into one pixel when the plain one was binned (plain means: the
/// plain development's clip mask tells highlight reconstruction which of them held clipped samples). `None` when
/// the product is missing, damaged, made from something else or not the size of this raw's active area.
fn denoised_camera_rgb(raw: &lightcraft_raw::RawImage, spec: &DenoiseSpec, factor: usize) -> Option<Rgb32f> {
    use lightcraft_denoise::product::{self, Window};
    let a = raw.active_area;
    let header = product::read_header_at(&spec.product).ok()?;
    if header.key != spec.key || (header.width, header.height) != (a.width, a.height) {
        return None;
    }
    if raw.opcodes.list3.is_empty() {
        let c = raw.develop_crop(a.width, a.height);
        let window = Window { x: c.x, y: c.y, width: c.width, height: c.height };
        product::read_window(&spec.product, &spec.key, Some(window), factor, None).ok()
    } else {
        // (the plain development is not binned either when the file has opcodes of this kind: `factor` is 1)
        let whole = product::read(&spec.product, &spec.key, 1).ok()?;
        Some(raw.finish_demosaiced(whole))
    }
}

fn load_bytes_now(
    bytes: std::borrow::Cow<'_, [u8]>,
    max_edge: usize,
    denoise: Option<&DenoiseSpec>,
    allow_reduced: bool,
) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo, bool), String> {
    if lightcraft_raw::probe(&bytes).is_some() {
        // a thumbnail (`allow_reduced`, never an export) of a DNG that keeps smaller copies of its raw image
        // (Lightroom's merges) develops the smallest that covers it; a denoised photo's twin needs the full mosaic
        let reduced = (allow_reduced && denoise.is_none() && max_edge < usize::MAX)
            .then(|| lightcraft_raw::decode_reduced(&bytes, max_edge).ok().flatten())
            .flatten();
        let used_reduced = reduced.is_some();
        let decoded = match reduced {
            Some(r) => Ok(r),
            None => lightcraft_raw::decode(&bytes),
        };
        let mut raw = match decoded.map_err(|e| preview_reason(&bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // show the camera's embedded JPEG (rendered, not raw) until the variant is supported
                return load_embedded_preview(&bytes, max_edge)
                    .map(|(img, info)| (img, None, info, false))
                    .ok_or(format!("unsupported raw ({why}) without an embedded preview"));
            }
            Err(Err(e)) => return Err(e),
        };
        // Embedded lens corrections are applied by the pipeline ("Enable Profile Corrections"), not baked in
        // (removed before the camera look's binned sensor proxy too, which needs an empty `OpcodeList3`).
        let info = raw.info();
        let lens = crate::lens_profiles::for_raw(&info, || lightcraft_meta::extract(&bytes));
        // (the camera look still pairs the sensor with the camera JPEG through the warp the camera applied to it)
        let lens_geometry = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        // A DNG's preview already has its lens opcodes applied. Keep them for its single tone fit,
        // then remove them again: the rendered source leaves those corrections to the pipeline.
        let dng_lens_ops = (raw.format == lightcraft_raw::RawFormat::Dng && raw.opcodes.list3.iter().any(|op| op.is_lens_correction()))
            .then(|| raw.opcodes.list3.clone());
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        // Which scale white balance is edited on, and the as-shot white on it, from the file's own colour tags or
        // the camera's measured matrices (`Photo::relative_wb` decides the same from the catalog's camera)
        let scale = WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
        // the source's segmentation mattes (DNG semantic masks), read while the starting colour is fitted
        let ((xy, t, camera_look), mattes) = rayon::join(
            || {
                let removed = dng_lens_ops.map(|ops| std::mem::replace(&mut raw.opcodes.list3, ops));
                // With the opcodes restored, the proxy already includes vignetting; do not apply it twice.
                let embedded = if removed.is_some() { None } else { lens.as_ref() };
                let colour = crate::camera_preview::starting_colour(&mut raw, &bytes, lens_geometry.as_ref(), embedded);
                if let Some(ops) = removed {
                    raw.opcodes.list3 = ops;
                }
                colour
            },
            || dng_mattes(&bytes, &info),
        );
        drop(bytes);
        // Previews and thumbnails bin the mosaic straight to (about) the size they need; only
        // larger levels (exports, 1:1) demosaic the whole sensor.
        let t0 = web_time::Instant::now();
        let bin = bin_factor(&raw, max_edge);
        // (binned: block means, and which colours had a clipped sample in each block)
        let binned = match bin {
            Some(k) => raw.develop_binned_masked(k, HIGHLIGHT_CLIP).map_err(|e| e.to_string())?,
            None => None,
        };
        let factor = if binned.is_some() { bin.unwrap_or(1) } else { 1 };
        let (img, clip_mask) = match binned {
            Some((img, mask)) => (img, Some(mask)),
            None => {
                let method = if max_edge <= 600 { lightcraft_raw::Method::Bilinear } else { lightcraft_raw::Method::Ahd };
                (raw.develop(method).map_err(|e| e.to_string())?, None)
            }
        };
        // the same development from the denoised mosaic, when the photo has been denoised
        let twin = denoise.and_then(|spec| denoised_camera_rgb(&raw, spec, factor)).filter(|d| d.width == img.width && d.height == img.height);
        // the samples aren't needed any more (the colour model below reads only the tags)
        raw.data = lightcraft_raw::RawData::U16(Vec::new());
        let hue_sat = camera_look.as_ref().and_then(|p| p.hue_sat.as_ref()).and_then(crate::camera_preview::HueSat::new);
        // A DNG's own profile look (hue/saturation map, look table), DNG spec chapter 6.
        let tables = lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy));
        let colour = CameraColourModel {
            wb: t.wb,
            matrix: camera_look.as_ref().map(|p| p.matrix.mul(&t.matrix)).unwrap_or(t.matrix),
            gain: 2f32.powf(t.baseline_exposure as f32),
            tables: tables.as_ref(),
            hue_sat: hue_sat.as_ref(),
        };
        // camera RGB → what the pipeline takes: clipped highlights rebuilt, white balance and the colour model applied,
        // fitted to `max_edge` and upright (the same for the plain and the denoised picture)
        let finish = |mut img: Rgb32f| {
            let mut stages = vec![("develop", t0.elapsed())];
            stages.push(("transform", t0.elapsed()));
            match &clip_mask {
                Some(mask) => colour.rebuild_highlights_masked(&mut img, mask),
                None => colour.rebuild_highlights(&mut img),
            }
            stages.push(("highlights", t0.elapsed()));
            colour.apply(&mut img);
            stages.push(("colour", t0.elapsed()));
            // a full-size render keeps the picture (fitting it to its own size would copy it, ~390 MB at 33 MP)
            let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Box) } else { img };
            stages.push(("fit", t0.elapsed()));
            let img = img.into_oriented(raw.orientation);
            stages.push(("orient", t0.elapsed()));
            (img, stages)
        };
        let (img, stages) = finish(img);
        if lightcraft_pipeline::profiling() {
            let mut prev = std::time::Duration::ZERO;
            let parts: Vec<String> = stages
                .iter()
                .map(|(n, t)| {
                    let d = *t - prev;
                    prev = *t;
                    format!("{n} {:.1}", d.as_secs_f64() * 1e3)
                })
                .collect();
            eprintln!("[profile] raw source {}×{} (max {max_edge}, ms after decode): {}", img.width, img.height, parts.join(", "));
        }
        let twin = twin.map(|picture| finish(picture).0);
        // White balance re-evaluates the file's own colour model (when it has one and no file-local look matrix sits
        // on top of it). A model's measured matrices never do: they only give the Kelvin scale its numbers, and the
        // picture is adapted from the as-shot white as every fitted look is (so settings from before that scale
        // existed render exactly as they did, `lightcraft_pipeline::local::legacy_white`).
        let camera_color = (scale.own_matrix && !t.matrix_is_fallback && (camera_look.is_none() || raw.format == lightcraft_raw::RawFormat::Dng))
            .then(|| {
                let tags = lightcraft_raw::ColorData { profile: Default::default(), ..raw.color.clone() };
                Arc::new(lightcraft_pipeline::CameraColor { tags, developed_for: xy })
            });
        let camera_tone = camera_look.as_ref().map(|p| p.tone).or_else(|| raw.color.profile.tone_curve.as_ref().and_then(dng_tone_curve));
        let local_tone = local_tone(&raw);
        let (temp, tint) = scale.as_shot(xy);
        return Ok((
            img,
            twin,
            SourceInfo {
                raw: true,
                as_shot_temp: temp,
                as_shot_tint: tint,
                lens,
                relative_wb: scale.relative,
                legacy_relative_wb: scale.legacy_relative,
                camera_color,
                camera_tone,
                mattes,
                local_tone,
                // (the catalog knows the original's size: `media::source_info`)
                native_long: None,
                alpha: None,
            },
            used_reduced,
        ));
    }
    let d = lightcraft_codecs::decode(&bytes, fit_box(max_edge)).map_err(|e| e.to_string())?;
    drop(bytes);
    let img = d.to_working();
    let orientation = Orientation::from_exif(d.orientation);
    // the transparency, fitted and oriented with the pixels, for exports that keep it (issue #2976);
    // a channel that is opaque everywhere is not kept (a decoder's fit brings 1.0 back as 0.99999994:
    // that is not transparency, and the photo must render as an opaque one)
    const OPAQUE: f32 = 1.0 - 1e-4;
    let alpha = d.alpha.filter(|a| (a.width, a.height) == (img.width, img.height) && a.data.iter().any(|&v| v < OPAQUE)).map(|a| {
        let a = if a.width.max(a.height) > max_edge { fit(&a, max_edge, max_edge, Filter::Mitchell) } else { a };
        Arc::new(a.into_oriented(orientation))
    });
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Ok((img.into_oriented(orientation), None, SourceInfo { alpha, ..SourceInfo::default() }, false))
}

/// Which white-balance scale a raw is edited on, decided from the file alone, the way the catalog decides it from
/// the photo's format and camera (`lightcraft_catalog::relative_wb_camera`; a test keeps the two in step).
///
/// A file with colour matrices of its own (DNG) has an absolute scale from its tags. A file without
/// (`camera_preview::file_local_look`) has one when its camera's matrices are measured
/// (`lightcraft_raw::spectral`), else when its camera's white-balance presets give one
/// (`lightcraft_raw::kelvin_presets`): either turns its as-shot multipliers into Kelvin and tint, nothing else — the
/// look stays the file's fit. Any other raw is edited relative to its as-shot look (6500 K / 0 means as shot).
pub(crate) struct WbScaleOf {
    pub(super) own_matrix: bool,
    pub(super) relative: bool,
    /// Kelvin through the camera's measured matrices or presets: edited on the relative scale before those were used.
    pub(super) legacy_relative: bool,
    /// The as-shot white through the measured matrices or the presets, when that is where the scale comes from.
    pub(super) scale_white: Option<lightcraft_color::Xy>,
    /// The camera's measured matrices, when they give the scale.
    camera: Option<&'static lightcraft_raw::spectral::Camera>,
}

/// The white balance a DNG of a raw's camera RGB (Photo Merge) declares so that it shows as its source does
/// ([`WbScaleOf::dng_white`]).
pub(crate) enum DngWhite {
    /// Colour tags with the camera's measured matrices: the DNG reads every white through them, as the source does.
    Matrices(Box<lightcraft_raw::ColorData>),
    /// The white the source's scale shows at As Shot.
    At(lightcraft_color::Xy),
}

impl WbScaleOf {
    pub(crate) fn new(
        format: lightcraft_raw::RawFormat,
        color: &lightcraft_raw::ColorData,
        wb_multipliers: Option<[f32; 3]>,
        meta: &lightcraft_meta::Metadata,
    ) -> Self {
        let own_matrix = lightcraft_raw::color::has_matrix(color);
        let fitted = crate::camera_preview::file_local_look(format) && !own_matrix;
        let model = meta.model.as_deref().filter(|_| fitted);
        let camera = model.and_then(|model| lightcraft_raw::spectral::find(meta.make.as_deref(), model));
        let presets = camera.is_none() && model.is_some_and(|model| lightcraft_raw::kelvin_presets::covers(meta.make.as_deref(), model));
        let scale_white = match camera {
            Some(c) => lightcraft_raw::spectral::as_shot_white_through(c, color, wb_multipliers),
            None if presets => lightcraft_raw::kelvin_presets::as_shot_white(color, wb_multipliers),
            None => None,
        };
        let covered = camera.is_some() || presets;
        WbScaleOf { own_matrix, relative: fitted && !covered, legacy_relative: fitted && covered, scale_white, camera }
    }

    /// What a DNG of this raw's camera RGB declares so that it shows the white balance as this scale does: the
    /// camera's measured matrices when they give the scale, else the white the scale shows at As Shot (the presets'
    /// reading; 6500 K / 0 on the relative scale; else `own`, the white the file's tags give).
    pub(crate) fn dng_white(&self, color: &lightcraft_raw::ColorData, own: lightcraft_color::Xy) -> DngWhite {
        if let Some(camera) = self.camera {
            let mut c = lightcraft_raw::ColorData { profile: Default::default(), kelvin_presets: None, ..color.clone() };
            if camera.fill(&mut c) {
                return DngWhite::Matrices(Box::new(c));
            }
        }
        if self.relative {
            return DngWhite::At(lightcraft_color::cct::temp_tint_to_xy(6500.0, 0.0));
        }
        DngWhite::At(self.scale_white.unwrap_or(own))
    }
    /// The as-shot temperature and tint on the file's scale: the relative scale's reference, the white through
    /// the measured matrices or the presets, else `own` (the white the file's own tags give). Whole numbers, as the
    /// catalog keeps them. A covered camera whose file names no white (no multipliers, or no presets) falls back to
    /// `own`, D65.
    pub(super) fn as_shot(&self, own: lightcraft_color::Xy) -> (f64, f64) {
        if self.relative {
            return (6500.0, 0.0);
        }
        let (t, tint) = xy_to_temp_tint(self.scale_white.unwrap_or(own));
        (t.round(), tint.round())
    }
}

/// The raw's gain table map with where its developed picture (the default crop, oriented) sits in
/// the active area: rendered when the photo's "Camera local tone mapping" option is on.
fn local_tone(raw: &lightcraft_raw::RawImage) -> Option<Arc<lightcraft_pipeline::local_tone::LocalTone>> {
    let map = raw.color.profile.gain_table_map.clone()?;
    let a = raw.active_area;
    let (aw, ah) = (a.width.max(1) as f64, a.height.max(1) as f64);
    let c = raw.develop_crop(a.width, a.height);
    let rect = [c.x as f64 / aw, c.y as f64 / ah, c.width as f64 / aw, c.height as f64 / ah];
    let placement = lightcraft_raw::gaintable::SourcePlacement { rect, orientation: raw.orientation };
    Some(Arc::new(lightcraft_pipeline::local_tone::LocalTone { map, placement }))
}

/// The semantic masks of a DNG that AI masks understand, over the developed image (default crop,
/// oriented like it). Person mattes without a confidently selected pixel are left out (the
/// Subject mask then falls back to its heuristic); a sky matte counts even when empty (no sky).
fn dng_mattes(bytes: &[u8], raw: &lightcraft_raw::RawInfo) -> Option<Arc<lightcraft_pipeline::masks::Mattes>> {
    use lightcraft_pipeline::masks::{MatteKind, Mattes};
    if raw.format != lightcraft_raw::RawFormat::Dng {
        return None;
    }
    let mut mattes = Mattes::default();
    for m in lightcraft_raw::semantic_masks(bytes) {
        let Some(kind) = matte_kind(&m.name) else { continue };
        let Some(img) = m.developed(raw.active_area, raw.crop) else { continue };
        if kind != MatteKind::Sky && !img.data.iter().any(|&v| v >= 128) {
            continue;
        }
        mattes.push(kind, img.into_oriented(raw.orientation));
    }
    (!mattes.is_empty()).then(|| Arc::new(mattes))
}

/// What a semantic mask selects, by its `SemanticName`: Apple's (iPhone ProRAW) are named after
/// their auxiliary image types, `urn:com:apple:photo:<year>:aux:<type>`. Other names: `None`.
fn matte_kind(name: &str) -> Option<lightcraft_pipeline::masks::MatteKind> {
    use lightcraft_pipeline::masks::MatteKind::*;
    let kind = name.strip_prefix("urn:com:apple:photo:")?.rsplit(':').next()?;
    Some(match kind {
        "semanticskymatte" => Sky,
        "semanticskinmatte" => Skin,
        "semantichairmatte" => Hair,
        "semanticteethmatte" => Teeth,
        "semanticglassesmatte" => Glasses,
        "portraiteffectsmatte" => Person,
        _ => return None,
    })
}

/// A DNG `ProfileToneCurve` (linear in, linear out, 1.0 = white after exposure compensation) as the
/// finish stage's camera tone curve: 32 knots, log-spaced over 12 stops below white; above white
/// the camera tone's shoulder continues it.
pub(crate) fn dng_tone_curve(curve: &lightcraft_raw::profile::ToneCurve) -> Option<lightcraft_pipeline::tone::CameraTone> {
    let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
        [x, curve.eval(x).min(0.9995)]
    });
    lightcraft_pipeline::tone::CameraTone::new(knots)
}
