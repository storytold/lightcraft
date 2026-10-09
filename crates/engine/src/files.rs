//! Real files: probing (dimensions + metadata for import) and loading (decode → linear Rec.2020,
//! oriented, downscaled to the requested level). Standard formats go through `lightcraft-codecs`,
//! camera raws through `lightcraft-raw` (demosaic + DNG colour model).

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Meta};
use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

use crate::media::{DenoiseSpec, FileLoader, FileProbe, PairLoader, PreviewLoader, ProbeInfo};

fn meta_of(m: &lightcraft_meta::Metadata) -> (Meta, Option<String>) {
    let shutter = m.exposure_time.map(|t| if t >= 1.0 { format!("{t:.0}") } else { format!("1/{:.0}", 1.0 / t) }).unwrap_or_default();
    let camera = [m.make.clone().unwrap_or_default(), m.model.clone().unwrap_or_default()].join(" ").trim().to_string();
    let meta = Meta {
        camera,
        lens: m.lens_model.clone().unwrap_or_default(),
        focal_mm: m.focal_length.map(|f| f as f32),
        aperture: m.f_number.map(|f| f as f32),
        shutter,
        iso: m.iso,
        location: m.sublocation.clone().unwrap_or_default(),
        city: m.city.clone().unwrap_or_default(),
        state: m.state.clone().unwrap_or_default(),
        country: m.country.clone().unwrap_or_default(),
        alt_text: m.alt_text.clone().unwrap_or_default(),
        extended_description: m.extended_description.clone().unwrap_or_default(),
        gps: m.gps.as_ref().map(|g| (g.latitude, g.longitude)),
        title: m.title.clone().unwrap_or_default(),
        caption: m.caption.clone().unwrap_or_default(),
        copyright: m.copyright.clone().unwrap_or_default(),
        copyright_status: lightcraft_catalog::CopyrightStatus::from_marked(m.copyright_marked),
        usage_terms: m.usage_terms.clone().unwrap_or_default(),
        copyright_url: m.copyright_url.clone().unwrap_or_default(),
        creator: m.artist.clone().unwrap_or_default(),
        keywords: m.keywords.clone(),
        regions: m.regions.clone(),
    };
    (meta, m.capture_time.as_ref().map(|d| d.to_iso()))
}

/// Lens corrections embedded in a raw's `OpcodeList3` (`WarpRectilinear`, `FixVignetteRadial`: a DNG's own, or the
/// raw reader's equivalent of the camera's correction, e.g. Panasonic / Leica RW2 distortion), re-expressed for the
/// default-cropped, EXIF-oriented image. These are the only "profile" corrections LightCraft applies.
pub fn embedded_lens(raw: &lightcraft_raw::RawInfo) -> Option<lightcraft_develop::EmbeddedLens> {
    use lightcraft_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedWarp};
    use lightcraft_geom::Point;
    let (aw, ah) = (raw.active_area.width as f64, raw.active_area.height as f64);
    if aw < 2.0 || ah < 2.0 {
        return None;
    }
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let (cx0, cy0, cw, ch) =
        if c.width == 0 || c.height == 0 { (0.0, 0.0, aw, ah) } else { (c.x as f64, c.y as f64, c.width as f64, c.height as f64) };
    let long = cw.max(ch);
    // opcode centres are relative to the (uncropped) active area, in pixel-index units
    let centre = |rel: [f64; 2]| -> (Point, f64) {
        let (px, py) = (rel[0] * (aw - 1.0), rel[1] * (ah - 1.0));
        let m = [(0.0, 0.0), (aw - 1.0, 0.0), (0.0, ah - 1.0), (aw - 1.0, ah - 1.0)]
            .iter()
            .map(|&(x, y)| (x - px).hypot(y - py))
            .fold(0.0, f64::max)
            .max(1e-9);
        (Point::new((px + 0.5 - cx0) / cw, (py + 0.5 - cy0) / ch), m / long)
    };
    let mut lens = EmbeddedLens::default();
    for op in &raw.opcodes.list3 {
        match op {
            lightcraft_raw::Opcode::WarpRectilinear { planes, center } if !planes.is_empty() && lens.warp.is_none() => {
                let (center, radius) = centre(*center);
                let p = |i: usize| planes[i.min(planes.len() - 1)];
                lens.warp = Some(EmbeddedWarp { planes: [p(0), p(1), p(2)], center, radius });
            }
            lightcraft_raw::Opcode::FixVignetteRadial { k, center } if lens.vignette.is_none() => {
                let (center, radius) = centre(*center);
                lens.vignette = Some(EmbeddedVignette { k: *k, center, radius });
            }
            _ => {}
        }
    }
    if lens.warp.is_none() && lens.vignette.is_none() {
        return None;
    }
    Some(lightcraft_pipeline::optics::reorient_lens(&lens, raw.orientation, cw, ch))
}

fn ext_upper(name: &str) -> String {
    std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
}

/// Probe a file's bytes: kind, dimensions (oriented), metadata. No pixel data is decompressed:
/// raws are described from their headers ([`lightcraft_raw::probe_info`]), other images too
/// ([`lightcraft_codecs::read_header`], which refuses truncated files but can't see damage inside
/// compressed data that is all there: such a file imports and shows as unreadable when rendered).
pub fn probe_bytes(name: &str, bytes: &[u8]) -> Result<ProbeInfo, String> {
    let content_hash = Some(lightcraft_preview::hash_bytes(bytes).to_string());
    let m = lightcraft_meta::extract(bytes);
    let (meta, captured) = meta_of(&m);
    if lightcraft_raw::probe(bytes).is_some() {
        let raw = match lightcraft_raw::probe_info(bytes).map_err(|e| preview_reason(bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // a raw variant we can't decode yet: describe it from its embedded preview
                let (w, h) = embedded_preview_size(bytes).ok_or(format!("unsupported raw ({why}) without an embedded preview"))?;
                return Ok(ProbeInfo {
                    width: w,
                    height: h,
                    format: ext_upper(name),
                    kind: MediaKind::Raw,
                    file_size: bytes.len() as u64,
                    captured,
                    meta,
                    as_shot_wb: None,
                    content_hash,
                    xmp: lightcraft_meta::embedded(bytes).xmp,
                    preview_only: Some(why),
                    ..Default::default()
                });
            }
            Err(Err(e)) => return Err(e),
        };
        let (mut w, mut h) = (raw.crop.width.max(1) as u32, raw.crop.height.max(1) as u32);
        if w <= 1 || h <= 1 {
            (w, h) = (raw.active_area.width as u32, raw.active_area.height as u32);
        }
        if raw.orientation.swaps_axes() {
            std::mem::swap(&mut w, &mut h);
        }
        let (t, tint) = xy_to_temp_tint(lightcraft_raw::color::as_shot_white_xy_of(&raw));
        // Vendor RGB multipliers do not identify an absolute illuminant without camera calibration.
        let relative = crate::camera_preview::file_local_look(raw.format) && !lightcraft_raw::color::has_matrix(&raw.color);
        let as_shot_wb = Some(if relative { (6500.0, 0.0) } else { (t.round(), tint.round()) });
        let embedded_lens = embedded_lens(&raw);
        return Ok(ProbeInfo {
            embedded_lens,
            width: w,
            height: h,
            format: ext_upper(name),
            kind: MediaKind::Raw,
            file_size: bytes.len() as u64,
            captured,
            meta,
            as_shot_wb,
            content_hash,
            xmp: lightcraft_meta::embedded(bytes).xmp,
            preview_only: None,
        });
    }
    let fmt = lightcraft_codecs::sniff(bytes).ok_or("unrecognized file format")?;
    if !fmt.can_decode() {
        return Err(format!("{fmt:?} files are not supported yet"));
    }
    // headers only (issue #367: decoding the pixels was nearly all of an import's CPU time)
    let header = lightcraft_codecs::read_header(bytes).map_err(|e| e.to_string())?;
    let o = Orientation::from_exif(header.orientation);
    let (mut w, mut h) = (header.width, header.height);
    if o.swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    let format = match ext_upper(name).as_str() {
        "JPG" | "JPEG" => "JPEG".to_string(),
        "" => format!("{fmt:?}").to_uppercase(),
        e => e.to_string(),
    };
    Ok(ProbeInfo {
        width: w,
        height: h,
        format,
        kind: MediaKind::Image,
        file_size: bytes.len() as u64,
        captured,
        meta,
        as_shot_wb: None,
        content_hash,
        embedded_lens: None,
        xmp: None,
        preview_only: None,
    })
}

/// Why a raw that failed to decode should show its embedded preview instead (`Ok`), or the error
/// to report (`Err`). Variants we can't decode yet always fall back. So does any CR3 error: the CRX
/// decoder is verified on few bodies, and every CR3 opened from its embedded JPEG before it existed.
/// So does any other failure of a recognised raw (damaged or oversized raw data, unreadable TIFF
/// structure): its preview may still be intact. Only a file that is not a raw reports an error.
fn preview_reason(bytes: &[u8], e: lightcraft_raw::RawError) -> Result<String, String> {
    use lightcraft_raw::RawError;
    match e {
        RawError::Unsupported(why) => Ok(why),
        e if lightcraft_raw::probe(bytes) == Some(lightcraft_raw::RawFormat::Cr3) => Ok(format!("CR3 {e}")),
        RawError::NotRaw => Err(RawError::NotRaw.to_string()),
        e => Ok(e.to_string()),
    }
}

/// Sensor clip level (normalised) for highlight reconstruction.
const HIGHLIGHT_CLIP: f32 = 0.99;

/// The largest block size to bin a raw's mosaic by for a source of at most `max_edge` pixels:
/// the binned image must keep at least 90 % of `max_edge` (a 16 MP sensor still bins 2× for the
/// 2560 px preview). X-Trans can only bin 3× (its 6×6 pattern), and its full demosaic is ~5× the
/// cost of Bayer's, so 3× is accepted down to 75 % (a 24 MP X-Trans preview is then ~2000 px
/// instead of a ~1.2 s full demosaic; zooming in still uses the full-size source).
/// `None` = demosaic at full size.
pub fn bin_factor(raw: &lightcraft_raw::RawImage, max_edge: usize) -> Option<usize> {
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let long = if c.width > 1 && c.height > 1 { c.width.max(c.height) } else { raw.active_area.width.max(raw.active_area.height) };
    let need = (max_edge.saturating_mul(9) / 10).max(1);
    let need3 = (max_edge.saturating_mul(3) / 4).max(1);
    [8usize, 6, 4, 3, 2].into_iter().find(|&k| raw.can_bin(k) && (long / k >= need || (k == 3 && !raw.can_bin(2) && long / k >= need3)))
}

/// Decode a file into a linear Rec.2020 image no larger than `max_edge`, oriented.
///
/// Runs on a rayon worker: its many short parallel loops then start on the worker's own queue
/// instead of each one waking the pool from outside and waiting for it (which costs more than the
/// loops themselves when the machine is busy).
pub fn load_bytes(bytes: &[u8], max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(|_| load_bytes_now(std::borrow::Cow::Borrowed(bytes), max_edge, None).map(|(img, _, info)| (img, info)))
}

/// [`load_bytes`] taking the file's bytes: a raw file's bytes are freed as soon as it is decoded
/// (less memory held while it is developed).
pub fn load_vec(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None).map(|(img, _, info)| (img, info)))
}

/// [`load_vec`] that also develops the photo's cached denoised picture (`spec`) from the same decode: the plain
/// picture, the denoised one (`None` when it cannot be read or does not fit this raw: the plain one is then all
/// there is) and the source facts. Both pictures are the same size.
pub fn load_vec_pair(bytes: Vec<u8>, max_edge: usize, spec: &DenoiseSpec) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, Some(spec)))
}

/// The denoised camera RGB of `raw` from its cached product, as the plain development would have it: the default
/// crop, and `factor` × `factor` blocks averaged into one pixel when the plain one was binned. `None` when the product
/// is missing, damaged, made from something else or not the size of this raw's active area.
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
        product::read_window(&spec.product, &spec.key, Some(window), factor, Some(HIGHLIGHT_CLIP)).ok()
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
) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> {
    if lightcraft_raw::probe(&bytes).is_some() {
        let mut raw = match lightcraft_raw::decode(&bytes).map_err(|e| preview_reason(&bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // show the camera's embedded JPEG (rendered, not raw) until the variant is supported
                return load_embedded_preview(&bytes, max_edge)
                    .map(|(img, info)| (img, None, info))
                    .ok_or(format!("unsupported raw ({why}) without an embedded preview"));
            }
            Err(Err(e)) => return Err(e),
        };
        // Embedded lens corrections are applied by the pipeline ("Enable Profile Corrections"), not baked in
        // (removed before the camera look's binned sensor proxy too, which needs an empty `OpcodeList3`).
        let lens = embedded_lens(&raw.info());
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        let xy = lightcraft_raw::color::as_shot_white_xy(&raw);
        let t = lightcraft_raw::color::camera_transform(&raw, xy);
        // the source's segmentation mattes (DNG semantic masks), read while the preview is fitted
        let (camera_look, mattes) = rayon::join(|| crate::camera_preview::fit_preview(&raw, &bytes, &t), || dng_mattes(&bytes, &raw));
        drop(bytes);
        // Previews and thumbnails bin the mosaic straight to (about) the size they need; only
        // larger levels (exports, 1:1) demosaic the whole sensor.
        let t0 = web_time::Instant::now();
        let bin = bin_factor(&raw, max_edge);
        let binned = match bin {
            Some(k) => raw.develop_binned(k, HIGHLIGHT_CLIP).map_err(|e| e.to_string())?,
            None => None,
        };
        let factor = if binned.is_some() { bin.unwrap_or(1) } else { 1 };
        let img = match binned {
            Some(img) => img,
            None => {
                let method = if max_edge <= 600 { lightcraft_raw::Method::Bilinear } else { lightcraft_raw::Method::Ahd };
                raw.develop(method).map_err(|e| e.to_string())?
            }
        };
        // the same development from the denoised mosaic, when the photo has been denoised
        let twin = denoise.and_then(|spec| denoised_camera_rgb(&raw, spec, factor)).filter(|d| d.width == img.width && d.height == img.height);
        // the samples aren't needed any more (the colour model below reads only the tags)
        raw.data = lightcraft_raw::RawData::U16(Vec::new());
        let wb = t.wb;
        let m = camera_look.as_ref().map(|p| p.matrix.mul(&t.matrix)).unwrap_or(t.matrix).to_f32();
        let hue_sat = camera_look.as_ref().and_then(|p| p.hue_sat.as_ref()).and_then(crate::camera_preview::HueSat::new);
        let gain = 2f32.powf(t.baseline_exposure as f32);
        // A DNG's own profile look (hue/saturation map, look table), DNG spec chapter 6.
        let tables = lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy));
        // camera RGB → what the pipeline takes: clipped highlights rebuilt, white balance and the colour model applied,
        // fitted to `max_edge` and upright (the same for the plain and the denoised picture)
        let finish = |mut img: Rgb32f| {
            let mut stages = vec![("develop", t0.elapsed())];
            stages.push(("transform", t0.elapsed()));
            lightcraft_raw::highlight::reconstruct(&mut img, wb, HIGHLIGHT_CLIP);
            stages.push(("highlights", t0.elapsed()));
            img.map_in_place(|p| {
                let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                let rgb = [
                    m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
                    m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
                    m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
                ];
                let rgb = match &tables {
                    Some(tables) => tables.apply(rgb, gain),
                    None => rgb.map(|v| v * gain),
                };
                hue_sat.as_ref().map_or(rgb, |h| h.apply(rgb)).map(|v| v.max(0.0))
            });
            stages.push(("colour", t0.elapsed()));
            let img = fit(&img, max_edge, max_edge, Filter::Box);
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
        let (temp, tint) = xy_to_temp_tint(xy);
        let relative = crate::camera_preview::file_local_look(raw.format) && t.matrix_is_fallback;
        // White balance re-evaluates the file's own colour model (when it has one and no
        // file-local look matrix sits on top of it)
        let camera_color = (!t.matrix_is_fallback && camera_look.is_none()).then(|| {
            let tags = lightcraft_raw::ColorData { profile: Default::default(), ..raw.color.clone() };
            Arc::new(lightcraft_pipeline::CameraColor { tags, developed_for: xy })
        });
        // Apple ProRAW's profile curve renders in Lightroom's chain, whose Contrast adapts to the
        // image as decoded (at its own exposure, before any edit); other DNG profile curves render
        // on luminance like any camera curve (only ProRAW was measured)
        let proraw = camera_look.is_none() && is_apple_proraw(&raw);
        let camera_tone = camera_look.as_ref().map(|p| p.tone).or_else(|| {
            let curve = raw.color.profile.tone_curve.as_ref()?;
            if !proraw {
                return dng_tone_curve::<32>(curve);
            }
            let tone = dng_tone_curve::<{ lightcraft_pipeline::tone::CAMERA_TONE_KNOTS }>(curve)?
                .per_channel()
                .with_baseline_exposure(t.baseline_exposure as f32);
            Some(tone.with_key(lightcraft_pipeline::tone::lr_key(&img.data, tone.baseline_exposure())))
        });
        let local_tone = local_tone(&raw);
        let (temp, tint) = if relative { (6500.0, 0.0) } else { (temp.round(), tint.round()) };
        return Ok((
            img,
            twin,
            SourceInfo {
                raw: true,
                as_shot_temp: temp,
                as_shot_tint: tint,
                lens,
                relative_wb: relative,
                camera_color,
                camera_tone,
                mattes,
                local_tone,
                proraw,
            },
        ));
    }
    let d = lightcraft_codecs::decode(&bytes, fit_box(max_edge)).map_err(|e| e.to_string())?;
    drop(bytes);
    let img = d.to_working();
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Ok((img.into_oriented(Orientation::from_exif(d.orientation)), None, SourceInfo::default()))
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
fn dng_mattes(bytes: &[u8], raw: &lightcraft_raw::RawImage) -> Option<Arc<lightcraft_pipeline::masks::Mattes>> {
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

/// Apple ProRAW: a DNG whose EXIF `Make` is Apple and that carries a `ProfileToneCurve`, the one
/// camera Lightroom's tone operators were measured on (`lightcraft_pipeline::finish::lr_tone`).
fn is_apple_proraw(raw: &lightcraft_raw::RawImage) -> bool {
    raw.format == lightcraft_raw::RawFormat::Dng
        && raw.color.profile.tone_curve.is_some()
        && raw.metadata.make.as_deref().is_some_and(|m| m.trim().eq_ignore_ascii_case("apple"))
}

/// A DNG `ProfileToneCurve` (linear in, linear out, 1.0 = white after exposure compensation) as the
/// finish stage's camera tone curve: `N` knots, log-spaced over 12 stops below white; above white
/// the camera tone's shoulder continues it. Apple ProRAW's takes
/// [`CAMERA_TONE_KNOTS`](lightcraft_pipeline::tone::CAMERA_TONE_KNOTS) (within 0.03 L* of its
/// 257-point curve) and is then applied per channel, inside Lightroom's own tone chain (see
/// `lightcraft_pipeline::tone`); other DNGs' take 32 and stay luminance curves.
pub(crate) fn dng_tone_curve<const N: usize>(curve: &lightcraft_raw::profile::ToneCurve) -> Option<lightcraft_pipeline::tone::CameraTone> {
    let last = N.saturating_sub(1).max(1) as f32;
    let knots: [[f32; 2]; N] = std::array::from_fn(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / last);
        [x, curve.eval(x).min(0.9995)]
    });
    lightcraft_pipeline::tone::CameraTone::from_knots(&knots)
}

/// Orientation for an embedded preview: its own EXIF orientation when it has one, else the raw file's.
fn preview_orientation(raw_bytes: &[u8], jpeg_orientation: u16) -> Orientation {
    if jpeg_orientation > 1 {
        return Orientation::from_exif(jpeg_orientation);
    }
    lightcraft_meta::extract(raw_bytes).orientation.unwrap_or(Orientation::Normal)
}

/// What stands in for a raw file we can't decode: its largest embedded JPEG, else, for a TIFF-based
/// raw, the file's own first image when the TIFF reader can show it (a reduced RGB copy: the only
/// preview some containers carry). Returns image bytes for [`lightcraft_codecs::decode`].
fn stand_in_image(bytes: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    if let Some(jpeg) = lightcraft_raw::embedded_preview(bytes) {
        return Some(std::borrow::Cow::Owned(jpeg));
    }
    let undecodable = lightcraft_raw::probe(bytes).is_some_and(|f| !f.is_supported());
    (undecodable && lightcraft_codecs::sniff(bytes).is_some_and(|f| f == lightcraft_codecs::Format::Tiff))
        .then_some(std::borrow::Cow::Borrowed(bytes))
}

/// Shared colour interpretation for quick previews, unsupported RAW fallback and camera-look fitting.
pub(crate) fn decode_raw_preview(bytes: &[u8], opts: lightcraft_codecs::DecodeOptions) -> Option<lightcraft_codecs::Decoded> {
    let jpeg = stand_in_image(bytes)?;
    // the colour space is a property of the JPEG; a TIFF stand-in is read by the TIFF reader
    let is_jpeg = matches!(jpeg, std::borrow::Cow::Owned(_));
    match lightcraft_raw::embedded_preview_color_space(bytes).filter(|_| is_jpeg) {
        Some(space) => {
            let space = match space {
                lightcraft_raw::PreviewColorSpace::Srgb => lightcraft_codecs::NamedSpace::Srgb,
                lightcraft_raw::PreviewColorSpace::AdobeRgb => lightcraft_codecs::NamedSpace::AdobeRgb,
            };
            lightcraft_codecs::decode_jpeg_with_fallback(&jpeg, opts, space).ok()
        }
        None => lightcraft_codecs::decode(&jpeg, opts).ok(),
    }
}

/// Oriented size of the embedded preview of a raw file.
fn embedded_preview_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let jpeg = stand_in_image(bytes)?;
    let d = lightcraft_codecs::decode(&jpeg, lightcraft_codecs::DecodeOptions::fit(64, 64)).ok()?;
    let (mut w, mut h) = (d.source_width, d.source_height);
    if preview_orientation(bytes, d.orientation).swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    Some((w, h))
}

/// The embedded preview of a raw file as a working-space image no larger than `max_edge`, oriented.
pub fn load_embedded_preview(bytes: &[u8], max_edge: usize) -> Option<(Rgb32f, SourceInfo)> {
    let d = decode_raw_preview(bytes, fit_box(max_edge))?;
    let img = d.to_working();
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Some((img.oriented(preview_orientation(bytes, d.orientation)), SourceInfo::default()))
}

/// The embedded preview of a raw file for display (sRGB, oriented, no larger than `max_edge`): the
/// loupe and grid show it until the raw itself has been developed ([`crate::media::QuickJob`]).
pub fn embedded_preview_srgb(bytes: &[u8], max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    let mut d = decode_raw_preview(bytes, fit_box(max_edge))?;
    if d.image.width.max(d.image.height) > max_edge {
        d.image = fit(&d.image, max_edge, max_edge, Filter::Box);
        d.alpha = None;
    }
    let o = preview_orientation(bytes, d.orientation);
    Some(d.to_srgb8().oriented(o))
}

/// Decode options fitting into a `max_edge` square; `usize::MAX` (a full-size load) or any edge past
/// `u32::MAX` asks for the largest box rather than wrapping to a small one.
fn fit_box(max_edge: usize) -> lightcraft_codecs::DecodeOptions {
    let e = u32::try_from(max_edge).unwrap_or(u32::MAX);
    lightcraft_codecs::DecodeOptions::fit(e, e)
}

/// Filesystem-backed embedded-preview hook (native).
///
/// Reads the whole file to find the preview, so it holds the memory gate for twice the file's size (the file and the
/// decoded preview): background work (the face scan) waits for room, interactive work is counted and never waits.
pub fn fs_preview_loader() -> PreviewLoader {
    Arc::new(|path: &str, max_edge: usize| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(len.saturating_mul(2)) } else { gate.acquire_urgent(len.saturating_mul(2)) };
        embedded_preview_srgb(&std::fs::read(path).ok()?, max_edge)
    })
}

/// The filesystem-backed [`PairLoader`]: the file is read and decoded once, and developed as usual and from the photo's
/// denoised picture. It holds two pictures at once, so it counts for more at the memory gate than a plain load.
pub fn fs_pair_loader() -> PairLoader {
    Arc::new(|path: &str, max_edge: usize, spec: &DenoiseSpec| {
        let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 4 } else { 10 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec_pair(bytes, max_edge, spec);
        crate::memory::release();
        r
    })
}

/// Filesystem-backed hooks (native). On the web the host installs bytes-based hooks instead.
///
/// Their working memory is bounded by [`crate::memory::work_gate`]: background loads (grid
/// thumbnails, neighbour prefetch: [`crate::memory::in_background`]) and import probes wait while
/// too much is in flight, each counting its file's size times the expansion of decoding it;
/// interactive loads (the loupe, exports) never wait — they are counted, so background work
/// yields to them.
pub fn fs_hooks() -> (FileLoader, FileProbe) {
    let loader: FileLoader = Arc::new(|path: &str, max_edge: usize| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec(bytes, max_edge);
        // the file, the samples and the intermediate images are gone: give their pages back
        crate::memory::release();
        r
    });
    let probe: FileProbe = Arc::new(|path: &str| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let _permit = crate::memory::work_gate().acquire(len);
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        probe_bytes(path, &bytes)
    });
    (loader, probe)
}

impl crate::Session {
    /// Install the filesystem file hooks (desktop, CLI, MCP headless).
    pub fn with_fs(mut self) -> Self {
        let (l, p) = fs_hooks();
        self.media.file_loader = Some(l);
        self.media.file_probe = Some(p);
        self.media.preview_loader = Some(fs_preview_loader());
        self.media.denoise.loader = Some(fs_pair_loader());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};

    /// Issue #367: probes read headers, not pixels, and report what the decode-based probe did:
    /// oriented dimensions (EXIF orientation 6 swaps them), metadata, the error for a truncated
    /// file or one that isn't an image.
    #[test]
    fn image_probe_reads_headers_like_the_decode_did() {
        let (w, h) = (300u32, 200u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        for o in [None, Some(1), Some(6), Some(3), Some(8)] {
            let exif = o.map(lightcraft_codecs::exif::minimal_exif);
            let meta = EncodeMeta { exif: exif.as_deref(), ..Default::default() };
            let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &meta).unwrap();
            let png = lightcraft_codecs::encode_png(&img, &meta).unwrap();
            for (name, bytes) in [("a.jpg", &jpeg), ("a.png", &png)] {
                let p = probe_bytes(name, bytes).unwrap();
                // what the probe computed from a decode before
                let d = lightcraft_codecs::decode(bytes, lightcraft_codecs::DecodeOptions::fit(64, 64)).unwrap();
                let swap = Orientation::from_exif(d.orientation).swaps_axes();
                let want = if swap { (d.source_height, d.source_width) } else { (d.source_width, d.source_height) };
                assert_eq!((p.width, p.height), want, "{name} {o:?}");
                assert_eq!((p.width, p.height), if matches!(o, Some(6 | 8)) { (h, w) } else { (w, h) }, "{name} {o:?}");
                assert_eq!(p.kind, MediaKind::Image);
                assert_eq!(p.format, if name == "a.jpg" { "JPEG" } else { "PNG" });
                assert_eq!(p.file_size, bytes.len() as u64);
                assert!(p.content_hash.is_some());
                assert!(probe_bytes(name, &bytes[..bytes.len() / 2]).is_err(), "{name}: truncated file accepted");
            }
        }
        assert!(probe_bytes("x.jpg", b"not an image").is_err());
    }

    #[test]
    fn cached_denoise_uses_the_same_white_balance_profile_and_orientation_as_plain_raw() {
        use lightcraft_denoise::product;
        use lightcraft_raw::{DngWriteOptions, Method, Orientation, profile::HsvTable, write_dng};
        let dir = crate::tests_xmp::temp_dir("denoise-color-finish");
        let fixture = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lightcraft_raw::decode(&fixture).unwrap();
        raw.orientation = Orientation::Rotate90;
        raw.color.baseline_exposure = 0.5;
        raw.color.profile.look_table =
            Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[23.0, 0.65, 0.8]; 2], srgb_value: false });
        let bytes = write_dng(&raw, &DngWriteOptions::default()).unwrap();
        let decoded = lightcraft_raw::decode(&bytes).unwrap();
        assert!(decoded.color.profile.look_table.is_some());
        // A cache containing the ordinary demosaic must undergo the identical downstream finish.
        let camera_rgb = decoded.develop(Method::Ahd).unwrap();
        let key = "synthetic-color-finish";
        let path = dir.join("same.lcdn");
        product::write(&path, &camera_rgb, key).unwrap();
        let spec = DenoiseSpec { product: path, key: key.into(), make: None };
        let (ordinary, _) = load_bytes(&bytes, 2048).unwrap();
        let (plain, cached, _) = load_vec_pair(bytes, 2048, &spec).unwrap();
        assert_eq!(plain.data, ordinary.data, "requesting a twin must not change the ordinary path");
        let cached = cached.unwrap();
        assert_eq!((plain.width, plain.height), (24, 32));
        assert_eq!((cached.width, cached.height), (plain.width, plain.height));
        let worst = plain.data.iter().zip(&cached.data).flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (a - b).abs())).fold(0.0f32, f32::max);
        assert!(worst < 0.002, "color finish differs beyond cache half-float rounding: {worst}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Luminance (from linear RGB) minus its 9 × 9 local mean: tone differences between a render and a camera JPEG
    /// mostly cancel, edges remain.
    fn detail(w: usize, h: usize, lum: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        let l: Vec<f32> = (0..w * h).map(|i| lum(i % w, i / w)).collect();
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (x0, x1, y0, y1) = (x.saturating_sub(4), (x + 5).min(w), y.saturating_sub(4), (y + 5).min(h));
                let m = (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (xx, yy))).map(|(xx, yy)| l[yy * w + xx]).sum::<f32>();
                out[y * w + x] = l[y * w + x] - m / ((x1 - x0) * (y1 - y0)) as f32;
            }
        }
        out
    }

    /// The integer shift (within ±`r` px) of the `p × p` patch of `a` at `(x, y)` that best matches `b`, with its
    /// normalised cross-correlation, or `None` when the patch has too little detail to tell.
    fn patch_shift(a: &[f32], b: &[f32], w: usize, (x, y): (usize, usize), p: usize, r: i64) -> Option<((i64, i64), f32)> {
        let pa: Vec<f32> = (0..p * p).map(|i| a[(y + i / p) * w + x + i % p]).collect();
        let na = pa.iter().map(|v| v * v).sum::<f32>().sqrt();
        if na < 1e-3 * p as f32 {
            return None;
        }
        let mut best = (f32::MIN, (0, 0));
        for dy in -r..=r {
            for dx in -r..=r {
                let (bx, by) = (x as i64 + dx, y as i64 + dy);
                let (mut dot, mut nb) = (0.0f32, 0.0f32);
                for i in 0..p {
                    for j in 0..p {
                        let v = b[(by as usize + i) * w + bx as usize + j];
                        dot += pa[i * p + j] * v;
                        nb += v * v;
                    }
                }
                let c = dot / (na * nb.sqrt()).max(1e-12);
                if c > best.0 {
                    best = (c, (dx, dy));
                }
            }
        }
        Some((best.1, best.0))
    }

    /// Issue #256: public Panasonic / Leica raws (skipped without the corpus) are corrected for distortion the way the
    /// camera corrected its own JPEG (tag 0x0119): at the corners of the frame, the render with "Enable Profile
    /// Corrections" lines up with the embedded JPEG within 2 px at 640 px, while the uncorrected render of the wide
    /// lenses is far off. A file shot with the camera's correction off (DMC-GH1) carries no correction.
    #[test]
    fn corpus_rw2_distortion_matches_the_camera_jpeg() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        let Ok(gh1) = std::fs::read(dir.join("rw2-panasonic-gh1.rw2")) else {
            eprintln!("skip: {} absent", dir.display());
            return;
        };
        assert_eq!(probe_bytes("gh1.rw2", &gh1).unwrap().embedded_lens, None, "correction off: nothing to apply");
        assert_eq!(load_bytes(&gh1, 320).unwrap().1.lens, None);

        // (file, how far off (px) the uncorrected corners are at the least, if they match within ±R at all)
        let files = [
            ("rw2-panasonic-fz1000m2-4x3.rw2", 8), // 9.1 mm, ~13 % at the corners
            ("rwl-leica-dlux7.rwl", 8),            // 10.9 mm, ~13 %
            ("rw2-panasonic-gx80.rw2", 8),         // 14 mm, ~8 %
            ("rw2-panasonic-gh6.rw2", 3),          // 20 mm, ~1.6 %
            ("rw2-panasonic-s9.rw2", 3),           // 45 mm, 3 % zoom after the correction
            ("rw2-panasonic-g9.rw2", 0),           // 40 mm, ~0.5 %
        ];
        const EDGE: usize = 640;
        const P: usize = 64;
        const R: i64 = 12;
        for (name, off_at_least) in files {
            let bytes = std::fs::read(dir.join(name)).unwrap();
            let probed = probe_bytes(name, &bytes).unwrap();
            assert!(probed.embedded_lens.is_some_and(|l| l.warp.is_some()), "{name}: no distortion correction read");
            let (src, info) = load_bytes(&bytes, 2 * EDGE).unwrap();
            assert_eq!(info.lens, probed.embedded_lens, "{name}: probe and decode disagree");
            // Convert to DNG keeps it: the same correction, as the DNG's own `WarpRectilinear`
            let dng = lightcraft_raw::write_dng(&lightcraft_raw::decode(&bytes).unwrap(), &Default::default()).unwrap();
            assert_eq!(probe_bytes("x.dng", &dng).unwrap().embedded_lens, probed.embedded_lens, "{name}: lost in the DNG");
            let mut s = lightcraft_develop::DevelopSettings::default();
            let render = |s: &lightcraft_develop::DevelopSettings| {
                lightcraft_pipeline::render(&src, &info, s, &lightcraft_pipeline::RenderRequest::fit(EDGE, EDGE)).image
            };
            let before = render(&s);
            s.optics.lens_profile = true;
            let after = render(&s);
            let (w, h) = (after.width, after.height);
            assert_eq!((before.width, before.height), (w, h));
            // the camera JPEG shows the whole sensor; a render in an in-camera aspect ratio is its centre
            let (cam, _) = load_embedded_preview(&bytes, 4 * EDGE).unwrap();
            let (cw, ch) = (cam.width as f64, cam.height as f64);
            let (sw, sh) = if cw / ch > w as f64 / h as f64 { (ch * w as f64 / h as f64, ch) } else { (cw, cw * h as f64 / w as f64) };
            let cam = cam.crop(((cw - sw) / 2.0).round() as usize, ((ch - sh) / 2.0).round() as usize, sw.round() as usize, sh.round() as usize);
            let cam = lightcraft_raster::resample::resize(&cam, w, h, Filter::Mitchell);
            let lin = |v: u8| (v as f32 / 255.0).powf(2.2);
            let lum8 = |im: &lightcraft_raster::Rgba8, x: usize, y: usize| {
                let p = im.data[y * im.width + x];
                0.3 * lin(p[0]) + 0.6 * lin(p[1]) + 0.1 * lin(p[2])
            };
            let reference = detail(w, h, |x, y| {
                let p = cam.data[y * w + x];
                0.3 * p[0] + 0.6 * p[1] + 0.1 * p[2]
            });
            let (db, da) = (detail(w, h, |x, y| lum8(&before, x, y)), detail(w, h, |x, y| lum8(&after, x, y)));
            let m = R as usize + 4;
            let corners = [(m, m), (w - m - P, m), (m, h - m - P), (w - m - P, h - m - P)];
            let mut measured = 0;
            for c in corners {
                let Some(((dx, dy), ca)) = patch_shift(&da, &reference, w, c, P, R) else { continue };
                measured += 1;
                assert!(dx.abs() <= 2 && dy.abs() <= 2 && ca >= 0.5, "{name}: corrected corner {c:?} off by ({dx}, {dy}) px, ncc {ca}");
                if off_at_least > 0
                    && let Some(((bx, by), cb)) = patch_shift(&db, &reference, w, c, P, R)
                {
                    let off = bx.abs().max(by.abs()) >= off_at_least || cb < 0.3;
                    assert!(off, "{name}: uncorrected corner {c:?} already within ({bx}, {by}) px, ncc {cb}");
                }
            }
            assert!(measured >= 2, "{name}: only {measured} corners with detail");
        }
    }

    /// A CR3-shaped file (not decodable yet) whose only content is a `PRVW` preview box.
    fn cr3_with_preview(w: u32, h: u32) -> Vec<u8> {
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        let jpeg = encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let mut b = b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom".to_vec();
        b.extend_from_slice(&((24 + jpeg.len()) as u32).to_be_bytes());
        b.extend_from_slice(b"PRVW\0\0\0\0\0\x01");
        b.extend_from_slice(&(w as u16).to_be_bytes());
        b.extend_from_slice(&(h as u16).to_be_bytes());
        b.extend_from_slice(b"\0\x01");
        b.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
        b.extend_from_slice(&jpeg);
        b
    }

    /// [`cr3_with_preview`] plus a full-size CRX raw track whose `CMP1` coding header is garbage:
    /// the container parses, the raw data doesn't (`RawError::Corrupt`, not `Unsupported`).
    fn cr3_with_corrupt_raw(w: u32, h: u32) -> Vec<u8> {
        let bx = |kind: &[u8; 4], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&(w as u16).to_be_bytes());
        craw[26..28].copy_from_slice(&(h as u16).to_be_bytes());
        craw.extend(bx(b"CMP1", &[0xff; 4]));
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(bx(b"CRAW", &craw));
        let stsz = [0u32, 16, 1].map(u32::to_be_bytes).concat();
        let co64 = [&[0u8, 0, 0, 0, 0, 0, 0, 1][..], &0u64.to_be_bytes()].concat();
        let stbl = [bx(b"stsd", &stsd), bx(b"stsz", &stsz), bx(b"co64", &co64)].concat();
        let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))));
        let preview = cr3_with_preview(w, h);
        [&preview[..24], &bx(b"moov", &trak), &preview[24..]].concat()
    }

    /// A CR3 whose raw data fails to decode, for any reason, still opens from its embedded JPEG:
    /// CR3s did before the CRX decoder, which is verified on few bodies (PR #279 review).
    #[test]
    fn corrupt_cr3_falls_back_to_embedded_preview() {
        let b = cr3_with_corrupt_raw(48, 32);
        let err = lightcraft_raw::decode(&b).expect_err("the fixture's raw data is corrupt");
        assert!(!matches!(err, lightcraft_raw::RawError::Unsupported(_)), "{err}");
        let p = probe_bytes("x.cr3", &b).unwrap();
        assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
        assert!(p.preview_only.is_some_and(|why| why.contains("CR3")));
        let (img, src) = load_bytes(&b, 24).unwrap();
        assert_eq!((img.width, img.height), (24, 16));
        assert!(!src.raw);
        // other formats still report a corrupt file as an error
        let mut dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        dng.truncate(dng.len() / 2);
        assert!(load_bytes(&dng, 24).is_err());
    }

    /// Lightroom's tone operators were measured on Apple ProRAW only: a DNG from another camera
    /// with a `ProfileToneCurve` keeps the camera-curve path it had (the curve at 32 knots on
    /// luminance, the sliders after it), and so does a per-channel curve whose source isn't known
    /// as ProRAW.
    #[test]
    fn only_apple_proraw_takes_lightrooms_tone() {
        use lightcraft_pipeline::{RenderRequest, finish::lr_tone, render};
        use lightcraft_raw::profile::{ProfileLook, ToneCurve};
        let mut raw = lightcraft_raw::decode(&crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        raw.color.profile = ProfileLook { tone_curve: ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.3, 1.0, 1.0]), ..Default::default() };
        let load = |raw: &lightcraft_raw::RawImage, make: &str| {
            let mut raw = raw.clone();
            raw.metadata.make = Some(make.into());
            load_bytes(&lightcraft_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap()
        };
        let ((img, other), (_, apple)) = (load(&raw, "Leica Camera AG"), load(&raw, "Apple"));
        assert!(!other.proraw && !lr_tone(&other), "{other:?}");
        let luminance = raw.color.profile.tone_curve.as_ref().and_then(dng_tone_curve::<32>);
        assert!(luminance.is_some_and(|t| !t.is_per_channel()));
        assert_eq!(other.camera_tone, luminance, "the profile curve on luminance, as before");
        assert!(apple.proraw && lr_tone(&apple) && apple.camera_tone.is_some_and(|t| t.is_per_channel()));
        let mut s = lightcraft_develop::DevelopSettings::default();
        s.light.contrast = 40.0;
        s.light.shadows = 30.0;
        let req = RenderRequest::fit(64, 64);
        let shown = |info: &SourceInfo| render(&img, info, &s, &req).image;
        assert_ne!(shown(&apple), shown(&other), "Lightroom's chain on ProRAW");
        let unknown = SourceInfo { proraw: false, ..apple.clone() };
        assert!(!lr_tone(&unknown));
        let on_luminance = SourceInfo { camera_tone: apple.camera_tone.map(|t| t.on_luminance()), ..unknown.clone() };
        assert_eq!(shown(&unknown), shown(&on_luminance), "the camera-curve path");
        // no profile curve: no camera tone, whatever the make
        raw.color.profile = ProfileLook::default();
        let (_, plain) = load(&raw, "Apple");
        assert!(!plain.proraw && plain.camera_tone.is_none());
    }

    /// Issue #138: a DNG's own profile look (hue/saturation map, look table, tone curve) is
    /// applied when it's developed — Lightroom-converted DNGs rendered flat and muted without it.
    #[test]
    fn dng_profile_look_is_applied() {
        use lightcraft_raw::profile::{HsvTable, ProfileLook, ToneCurve};
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lightcraft_raw::decode(&plain).unwrap();
        let sat = |img: &Rgb32f| {
            img.data.iter().map(|p| (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) / p[0].max(p[1]).max(p[2]).max(1e-6)).sum::<f32>()
                / img.data.len() as f32
        };
        let (before, info) = load_bytes(&plain, 64).unwrap();
        assert!(sat(&before) > 0.05, "the synthetic scene is coloured");
        assert!(info.camera_tone.is_none());
        // a map that removes all saturation, and a tone curve
        let grey = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 0.0, 1.0]; 8], srgb_value: false };
        raw.color.profile = ProfileLook {
            hue_sat_map: [Some(grey), None],
            look_table: None,
            tone_curve: ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.3, 1.0, 1.0]),
            gain_table_map: None,
        };
        let with = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
        let (after, info) = load_bytes(&with, 64).unwrap();
        assert!(sat(&after) < 1e-3, "saturation {} → {}", sat(&before), sat(&after));
        // a saturation-only map keeps brightness roughly (HSV value is kept in ProPhoto RGB)
        let mean = |img: &Rgb32f| img.data.iter().map(|p| p[0].max(p[1]).max(p[2])).sum::<f32>() / img.data.len() as f32;
        assert!((0.8..1.1).contains(&(mean(&after) / mean(&before))), "{} vs {}", mean(&after), mean(&before));
        let tone = info.camera_tone.expect("the DNG tone curve becomes the camera tone");
        assert!((tone.apply(0.18) - 0.3).abs() < 0.01, "{}", tone.apply(0.18));
        // a look table alone changes the render too (applied after exposure)
        raw.color.profile = ProfileLook {
            look_table: Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 0.5]; 2], srgb_value: false }),
            ..Default::default()
        };
        let (dim, _) = load_bytes(&lightcraft_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
        assert!((mean(&dim) / mean(&before) - 0.5).abs() < 0.02, "{} vs {}", mean(&dim), mean(&before));
    }

    /// iPhone ProRAW-style semantic masks drive the Sky mask: Apple's sky matte (here the right
    /// half of the sensor, i.e. the bottom of the photo once turned 90° clockwise — where the sky
    /// heuristic would never look) is used; a matte with an unknown name is ignored.
    #[test]
    fn dng_sky_matte_drives_the_sky_mask() {
        use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
        use lightcraft_pipeline::{RenderRequest, render};
        use lightcraft_tiff::tags::{self as t, photometric};
        use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
        let (w, h) = (16u32, 8u32);
        let image = |ifd: &mut IfdBuilder, w: u32, h: u32, cpp: u16, bits: u16, data: Vec<u8>| {
            ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
            ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits; cpp as usize]));
            ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![cpp]));
            ifd.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![data] });
        };
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::LINEAR_RAW]));
        image(&mut raw, w, h, 3, 16, 12000u16.to_le_bytes().repeat((w * h * 3) as usize));
        let matte = |name: &str, right: bool| {
            let mut m = IfdBuilder::new();
            m.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![t::SUBFILE_SEMANTIC_MASK]));
            m.set(t::PHOTOMETRIC, Value::Short(vec![photometric::MASK]));
            m.set(t::SEMANTIC_NAME, Value::Ascii(name.into()));
            image(&mut m, w / 2, h / 2, 1, 8, (0..w / 2 * h / 2).map(|i| if (i % (w / 2) >= w / 4) == right { 255 } else { 0 }).collect());
            m
        };
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        ifd0.set(t::DNG_VERSION, Value::Byte(vec![1, 6, 0, 0]));
        ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
        ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]));
        ifd0.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
        image(&mut ifd0, 4, 2, 3, 8, vec![128; 24]);
        ifd0.add_sub_ifd(raw);
        ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticskymatte", true));
        ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticsomethingelse", false));
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

        let (img, info) = load_bytes(&bytes, 64).unwrap();
        assert_eq!((img.width, img.height), (8, 16));
        assert!(info.mattes.is_some());
        let sky = Mask {
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Sky }],
            adjust: LocalAdjustments { exposure: -3.0, ..Default::default() },
            ..Default::default()
        };
        let s = DevelopSettings { masks: vec![sky], ..Default::default() };
        let req = RenderRequest::fit(8, 16);
        let green = |s: &DevelopSettings, info: &SourceInfo, y: usize| render(&img, info, s, &req).image.get(4, y)[1] as i32;
        let plain = DevelopSettings::default();
        assert_eq!(green(&s, &info, 2), green(&plain, &info, 2), "no sky at the top");
        assert!(green(&s, &info, 13) + 40 < green(&plain, &info, 13), "the sky at the bottom is darkened");
        // without the matte the heuristic looks at the top of the frame instead
        let heuristic = SourceInfo { mattes: None, ..info.clone() };
        assert!(green(&s, &heuristic, 2) < green(&plain, &heuristic, 2));
        assert_eq!(green(&s, &heuristic, 13), green(&plain, &heuristic, 13));
    }

    /// A DNG's white balance is re-evaluated through its own colour model: the decoder hands the
    /// pipeline the file's colour tags (without the profile look, applied at load) and the white
    /// the pixels were developed for, and a custom white balance renders a neutral differently
    /// from the Bradford adaptation it replaces.
    #[test]
    fn dng_white_balance_uses_the_camera_colour_model() {
        use lightcraft_pipeline::{RenderRequest, render};
        let dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let raw = lightcraft_raw::decode(&dng).unwrap();
        let (img, info) = load_bytes(&dng, 64).unwrap();
        let cc = info.camera_color.as_ref().expect("a DNG with a colour matrix");
        assert_eq!(cc.developed_for, lightcraft_raw::color::as_shot_white_xy(&raw));
        assert!(cc.tags.profile.is_empty());
        assert_eq!(cc.tags.color_matrix, raw.color.color_matrix);
        let mut s = lightcraft_develop::DevelopSettings::default();
        (s.wb.mode, s.wb.temp, s.wb.tint) = (lightcraft_develop::WbMode::Custom, 3000.0, 20.0);
        let req = RenderRequest::fit(32, 32);
        let camera = render(&img, &info, &s, &req).image;
        let adapted = render(&img, &SourceInfo { camera_color: None, ..info.clone() }, &s, &req).image;
        assert_ne!(camera.data, adapted.data);
    }

    /// Apple ProRAW: like Lightroom Classic, the default render ignores the file's
    /// `ProfileGainTableMap` (Apple's local tone mapping); a map that would double every pixel
    /// changes nothing.
    #[test]
    fn dng_gain_table_map_is_not_rendered() {
        use lightcraft_raw::gaintable::GainTableMap;
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lightcraft_raw::decode(&plain).unwrap();
        let (before, _) = load_bytes(&lightcraft_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
        raw.color.profile.gain_table_map = Some(GainTableMap {
            points_v: 1,
            points_h: 1,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [0.2, 0.2, 0.2, 0.2, 0.2],
            gamma: 1.0,
            gains: vec![2.0],
        });
        let with_map = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
        assert!(lightcraft_raw::decode(&with_map).unwrap().color.profile.gain_table_map.is_some(), "the map is kept");
        let (after, _) = load_bytes(&with_map, 64).unwrap();
        assert_eq!(before.data, after.data);
    }

    /// The Profile option "Camera local tone mapping" renders the map by position (in the raw's
    /// active area, whatever the photo's orientation), and only when it's on.
    #[test]
    fn dng_gain_table_map_renders_when_asked() {
        use lightcraft_pipeline::{RenderRequest, render};
        use lightcraft_raw::gaintable::GainTableMap;
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lightcraft_raw::decode(&plain).unwrap();
        // ×1 at the active area's left edge rising to ×4 at its right edge, whatever the colour
        raw.color.profile.gain_table_map = Some(GainTableMap {
            points_v: 1,
            points_h: 2,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0, 0.0],
            gamma: 1.0,
            gains: vec![1.0, 4.0],
        });
        let with_map = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
        let (img, info) = load_bytes(&with_map, 64).unwrap();
        let (_, plain_info) = load_bytes(&plain, 64).unwrap();
        assert!(info.local_tone.is_some() && plain_info.local_tone.is_none());
        let req = RenderRequest::fit(64, 64);
        let mut s = lightcraft_develop::DevelopSettings::default();
        // (darker, so that ×4 never reaches white)
        s.light.exposure = -3.0;
        let off = render(&img, &info, &s, &req).image;
        assert_eq!(off.data, render(&img, &plain_info, &s, &req).image.data, "off: the map changes nothing");
        s.profile.camera_local_tone = true;
        assert!(s.to_json().to_string().contains("camera_local_tone"), "the option is part of the settings (and their hash)");
        let on = render(&img, &info, &s, &req).image;
        // brightness gained, per column: none at the left edge, most at the right
        let column = |im: &lightcraft_raster::Rgba8, x: usize| (0..im.height).map(|y| im.data[y * im.width + x][1] as f32).sum::<f32>();
        let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
        let last = on.width - 1;
        assert!((gain(0) - 1.0).abs() < 0.08, "left ×{}", gain(0));
        assert!(gain(last) > 1.4 && gain(last) > gain(last / 2) && gain(last / 2) > gain(0), "{} {} {}", gain(0), gain(last / 2), gain(last));
        // the photo turned upside down: the map follows the raw, so the brightened side swaps
        s.orientation = lightcraft_geom::Orientation::Rotate180;
        s.profile.camera_local_tone = false;
        let off = render(&img, &info, &s, &req).image;
        s.profile.camera_local_tone = true;
        let on = render(&img, &info, &s, &req).image;
        let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
        assert!(gain(0) > 1.4 && (gain(last) - 1.0).abs() < 0.08, "rotated: {} … {}", gain(0), gain(last));
        // a rendered (non-raw) source never gets it
        let jpeg = SourceInfo { raw: false, ..info.clone() };
        assert!(!lightcraft_pipeline::local_tone::enabled(&jpeg, &s));
    }

    #[test]
    fn unsupported_raw_falls_back_to_embedded_preview() {
        let b = cr3_with_preview(48, 32);
        let p = probe_bytes("x.cr3", &b).unwrap();
        assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
        let (img, src) = load_bytes(&b, 24).unwrap();
        assert_eq!((img.width, img.height), (24, 16));
        assert!(!src.raw);
        // no preview at all: a clear error, not a panic
        assert!(load_bytes(b"\0\0\0\x18ftypcrx \0\0\0\x01", 24).is_err());
    }

    /// Issue #10: a raw variant we can't decode imports as "preview only" with the decoder's
    /// reason, is rendered as the rendered JPEG it is (not as raw), says so to agents, survives a
    /// library reopen, and Reload / Relink clear it once the file decodes (undoably).
    #[test]
    fn unsupported_raw_imports_as_preview_only_and_survives_reopen() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("lc-preview-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (lib, files) = (dir.join("lib"), dir.join("files"));
        std::fs::create_dir_all(&files).unwrap();
        let path = files.join("DSC_0001.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        // the import review already says so
        let scan = s.execute("library.importPreview", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        assert!(scan["candidates"][0]["previewOnly"].as_str().is_some_and(|w| !w.is_empty()), "{scan}");
        let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let p = s.catalog.photo(id).unwrap().clone();
        let why = p.preview_only.clone().expect("marked preview only");
        assert!(why.to_uppercase().contains("CR3"), "the decoder's reason: {why}");
        assert_eq!(p.kind, MediaKind::Raw, "still a raw file (filters, Convert to DNG…)");
        assert!(!p.develops_raw());
        // rendered like the JPEG it is: relative white balance, display tone curve, no raw defaults
        assert_eq!(crate::media::source_info(&p), SourceInfo::default());
        assert_eq!(*p.develop, lightcraft_develop::DevelopSettings::default());
        assert!(s.render_job(id, 48, 48, false, true).unwrap().run().rendered.is_ok());
        // agents see it
        let q = s.execute("catalog.query", &json!({"filter": {}})).unwrap();
        assert_eq!(q["photos"][0]["previewOnly"], json!(why), "{q}");
        let i = s.execute("photo.inspect", &json!({"id": id.0})).unwrap();
        assert_eq!(i["preview_only"], json!(why), "{i}");
        // a library reopen keeps it
        s.persist().unwrap();
        drop(s);
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
        // the file decodes now (here: replaced by a DNG): Reload clears it, undo brings it back
        std::fs::write(&path, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
        let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(r["reloaded"], json!([id.0]), "{r}");
        assert!(s.catalog.photo(id).unwrap().develops_raw());
        assert!(crate::media::source_info(s.catalog.photo(id).unwrap()).raw);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
        // relinking to a file that decodes clears it too
        let dng = files.join("DSC_0001.dng");
        std::fs::write(&dng, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
        s.execute("photo.relink", &json!({"id": id.0, "path": dng.to_string_lossy()})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only, None);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A raw whose TIFF shell holds only a small RGB image (IFD0) next to a private block, as Exif
    /// declares a picture 25x its size.
    fn tiff_shell(w: u32, h: u32, shell: bool) -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![2]));
        ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        ifd0.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![px] });
        ifd0.set_child(
            t::EXIF_IFD,
            IfdBuilder::new().with(t::PIXEL_X_DIMENSION, Value::Long(vec![w * 25])).with(t::PIXEL_Y_DIMENSION, Value::Long(vec![h * 25])),
        );
        let mut b = TiffWriter::default().write(&[ifd0]).unwrap();
        if shell {
            b.extend(std::iter::repeat_n(0x5au8, (w * h * 625 / 8) as usize + 100));
        }
        b
    }

    /// A TIFF raw whose only image is a small thumbnail used to open as a plain image of that size
    /// ("a 150 MP file renders 296x220"), with nothing saying so. Now it is recognised as a raw we can't
    /// decode: preview only, with the reason, still showing that image.
    #[test]
    fn thumbnail_only_tiff_raw_is_preview_only_not_a_plain_image() {
        let b = tiff_shell(24, 16, true);
        let p = probe_bytes("IMG_0001.IIQ", &b).unwrap();
        assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "IIQ", 24, 16));
        let why = p.preview_only.expect("marked preview only");
        assert!(why.contains("600x400") && why.contains("24x16"), "{why}");
        let (img, src) = load_bytes(&b, 12).unwrap();
        assert_eq!((img.width, img.height, src.raw), (12, 8, false));
        assert!(embedded_preview_srgb(&b, 12).is_some());
        // the same pixels without the private block are a small ordinary image
        let plain = probe_bytes("small.tif", &tiff_shell(24, 16, false)).unwrap();
        assert_eq!((plain.kind, plain.preview_only, plain.width, plain.height), (MediaKind::Image, None, 24, 16));
        assert!(lightcraft_raw::probe(&tiff_shell(24, 16, false)).is_none());
    }

    /// Recognised raw containers we don't decode (Minolta MRW here: the preview's first byte is
    /// overwritten in the file) import as preview only with the JPEG's size, instead of failing.
    #[test]
    fn undecodable_container_with_a_preview_imports_as_preview_only() {
        let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        let mut jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        jpeg[0] = 0x02;
        let mut f = b"\0MRM\0\x01\0\0".to_vec();
        f.extend(std::iter::repeat_n(0x11u8, 300));
        f.extend_from_slice(&jpeg);
        f.extend(std::iter::repeat_n(0x22u8, 300));
        let p = probe_bytes("A.MRW", &f).unwrap();
        assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "MRW", 40, 30));
        assert!(p.preview_only.is_some_and(|w| w.contains("Mrw")));
        let (img, src) = load_bytes(&f, 20).unwrap();
        assert_eq!((img.width, img.height, src.raw), (20, 15, false));
        // without a JPEG: a clear error
        let e = probe_bytes("A.MRW", b"\0MRM\0\x01\0\0 nothing here").unwrap_err();
        assert!(e.contains("without an embedded preview"), "{e}");
    }

    /// Any failure of a recognised raw (not only an unsupported variant) can fall back to its preview;
    /// CR3 keeps its own wording.
    #[test]
    fn decode_failures_of_recognised_raws_may_fall_back_to_the_preview() {
        use lightcraft_raw::RawError;
        let cr3 = b"\0\0\0\x18ftypcrx \0\0\0\x01";
        assert_eq!(preview_reason(b"x", RawError::Unsupported("x".into())), Ok("x".to_string()));
        assert!(preview_reason(b"x", RawError::Corrupt("bad strip".into())).is_ok_and(|w| w.contains("bad strip")));
        assert!(preview_reason(b"x", RawError::Limit("too big")).is_ok());
        assert!(preview_reason(b"x", RawError::Tiff(lightcraft_tiff::TiffError::MissingTag(256))).is_ok());
        assert!(preview_reason(b"x", RawError::NotRaw).is_err());
        assert!(preview_reason(cr3, RawError::Corrupt("bad".into())).is_ok_and(|w| w.starts_with("CR3 ")));
    }

    #[test]
    fn quick_jobs_show_embedded_previews_then_cached_renders() {
        use crate::media::QuickSource;
        let dir = std::env::temp_dir().join(format!("lc-quick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let mut s = crate::Session::new().with_fs();
        let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        // loupe: an unedited raw opens on its embedded preview…
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Embedded));
        let img = q.rendered.unwrap().image;
        assert_eq!((img.width, img.height), (96, 64));
        // …and once the loupe has rendered it, on that render (any size)
        let full = s.loupe_job(id, 48, 32, true).unwrap().run();
        assert!(full.rendered.is_ok() && full.quick.is_none());
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Cached));
        assert_eq!(q.rendered.unwrap().image.width, 48);
        // grid: embedded first, then the real thumbnail (final, with the thumbnail job's key)
        let job = s.thumb_job(id, 128).unwrap();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!((q.quick, q.key), (Some(QuickSource::Embedded), job.key));
        drop(job.clone().run());
        assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Cached));
        // an edited raw: no embedded stand-in (it wouldn't show the edit)
        s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 1.0})).unwrap();
        let job = s.thumb_job(id, 128).unwrap();
        assert!(s.quick_thumb_job(&job).is_none());
        assert_eq!(s.quick_view_job(id, 1600, true).unwrap().run().quick, Some(QuickSource::Small));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
