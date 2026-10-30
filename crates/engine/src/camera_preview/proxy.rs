use super::colour::{PROFILE_PROXY, PROXY, file_local_look};
use super::hue_sat::fit_hue_sat;
use super::look_fit::{FIT_PREVIEW_MAX_PIXELS, SENSOR_CLIP, clip_mask, without_clipped};
use super::pairs::{collect_pairs, fit_matrix};
use super::{CameraTransform, Filter, HsvTable, Mat3, RawFormat, RawImage, Rgb32f, fit, luminance_2020, resize};

/// The embedded preview's long edge must be at least this share of the raw's (and 384 px): a DNG whose preview is a
/// small thumbnail is not fitted. The fit itself runs on a proxy of at most 384 px, so the 1024 px preview
/// Lightroom writes into its DNGs (15% of a 33 MP raw's long edge) is plenty.
pub(super) const MIN_PREVIEW_FRACTION: f64 = 0.1;

/// Lowest preview luminance and channel value a tone-only fit trains on (the other fits use 0.015 and 0.004): a dark
/// night scene has most of its pixels below 0.015, and a curve fitted without them overshoots its shadows.
pub(super) const DARK_FLOOR: (f32, f32) = (0.004, 0.001);

/// Share of the proxy by which the fitted curve may clip more highlights than the plain render and the camera's
/// preview (chosen on the CC0 and user DNGs from the clipped share of their 1440 px renders: it refuses the fits
/// that clip about 1% more than the camera does and passes the ones that clip like the camera).
pub(super) const CLIP_TOLERANCE: f64 = 0.001;

/// Log-luminance correlation (5 × 5 box blurred) the unshifted proxies must reach.
const MIN_STRUCTURE: f64 = 0.85;

/// Whether the sensor proxy and the camera JPEG proxy show the same picture in the same place: their log luminance
/// correlates by at least [`MIN_STRUCTURE`] unshifted, and no shift of more than one proxy pixel (within ±3)
/// correlates better.
pub(super) fn structure_agrees(sensor: &Rgb32f, reference: &Rgb32f) -> bool {
    let (w, h) = (sensor.width, sensor.height);
    if (w, h) != (reference.width, reference.height) || w < 16 || h < 16 || sensor.data.len() != w * h || reference.data.len() != w * h {
        return false;
    }
    let log = |img: &Rgb32f| -> Vec<f64> {
        let l: Vec<f64> =
            img.data.iter().map(|p| f64::from(luminance_2020(p.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }))).max(1e-4).ln()).collect();
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (mut sum, mut n) = (0.0, 0.0);
                for yy in y.saturating_sub(2)..(y + 3).min(h) {
                    for xx in x.saturating_sub(2)..(x + 3).min(w) {
                        if let Some(v) = l.get(yy * w + xx) {
                            sum += v;
                            n += 1.0;
                        }
                    }
                }
                sum / f64::max(n, 1.0)
            })
            .collect()
    };
    let (a, b) = (log(sensor), log(reference));
    const M: usize = 3;
    let corr = |dx: isize, dy: isize| -> f64 {
        let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for y in M..h - M {
            for x in M..w - M {
                let (xx, yy) = (x.saturating_add_signed(dx), y.saturating_add_signed(dy));
                let (Some(&u), Some(&v)) = (a.get(y * w + x), b.get(yy * w + xx)) else { continue };
                n += 1.0;
                sa += u;
                sb += v;
                saa += u * u;
                sbb += v * v;
                sab += u * v;
            }
        }
        let var = (saa - sa * sa / n) * (sbb - sb * sb / n);
        if n < 2.0 || var <= 1e-12 { 0.0 } else { (sab - sa * sb / n) / var.sqrt() }
    };
    let at_zero = corr(0, 0);
    let (mut best, mut shift) = (at_zero, (0isize, 0isize));
    for dy in -(M as isize)..=M as isize {
        for dx in -(M as isize)..=M as isize {
            let c = corr(dx, dy);
            if c > best + 1e-9 {
                (best, shift) = (c, (dx, dy));
            }
        }
    }
    at_zero >= MIN_STRUCTURE && shift.0.abs() <= 1 && shift.1.abs() <= 1
}

/// Same-size proxies of the sensor (white-balanced, baseline exposure, through `transform`'s
/// matrix: the generic camera ≈ sRGB model) and of the file's embedded camera JPEG (linear Rec.2020),
/// and per proxy pixel whether any sensor sample it covers is clipped. `correction` proposes CR3
/// correspondence geometry; it must pass the independent geometry gates without worsening the framing fallback.
pub(super) fn proxies(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    size: usize,
    min_fraction: f64,
    lens: Option<&LensGeometry>,
    correction: Option<&lightcraft_develop::EmbeddedLens>,
) -> Option<(Rgb32f, Rgb32f, Vec<bool>)> {
    let edge = (2 * size).max(384) as u32;
    let decoded = crate::files::decode_raw_preview(
        bytes,
        lightcraft_codecs::DecodeOptions { max_size: Some((edge, edge)), max_pixels: FIT_PREVIEW_MAX_PIXELS },
        None,
    )?;
    let (a, crop) = (raw.active_area, raw.crop.clipped(raw.active_area.width, raw.active_area.height));
    // a preview below `min_fraction` of the crop's long edge (or under 384 px) is refused; 0 = any
    if min_fraction > 0.0 && (decoded.source_width.max(decoded.source_height) as f64) < (min_fraction * crop.width.max(crop.height) as f64).max(384.0)
    {
        return None;
    }
    let mut reference = decoded.to_working();
    if crop.width == 0 || crop.height == 0 || reference.width == 0 || reference.height == 0 {
        return None;
    }
    let matches = |w: usize, h: usize, rw: usize, rh: usize| (rw as f64 / rh as f64 / (w as f64 / h as f64) - 1.0).abs() <= 0.02;
    if !matches(crop.width, crop.height, reference.width, reference.height) {
        // Panasonic previews show the whole active area while the default crop is the in-camera aspect ratio
        if !matches(a.width, a.height, reference.width, reference.height) {
            return None;
        }
        let (sx, sy) = (reference.width as f64 / a.width as f64, reference.height as f64 / a.height as f64);
        let (x, y) = ((crop.x as f64 * sx).round() as usize, (crop.y as f64 * sy).round() as usize);
        let (w, h) = ((crop.width as f64 * sx).round() as usize, (crop.height as f64 * sy).round() as usize);
        if w < 16 || h < 16 || x + w > reference.width || y + h > reference.height {
            return None;
        }
        reference = reference.into_crop(x, y, w, h);
    }
    // Fixed, bounded proxy: the selected look cannot depend on thumbnail/export resolution.
    let k = (crop.width.max(crop.height).div_ceil(edge as usize).max(2)).div_ceil(2) * 2;
    let mut sensor = sensor_proxy(raw, k, edge as usize)?;
    let mut clipped = clip_mask(&sensor);
    // The camera may have corrected its JPEG with the lens warp the file carries (Nikon Z lenses always, at up to 9 %
    // zoom; other cameras when their correction setting is on): the sensor moved into that geometry is a candidate,
    // kept below when it lines up with the JPEG better than the uncorrected one.
    let warped = lens
        .filter(|_| raw.format != RawFormat::Cr3)
        .and_then(|l| Some((into_lens_geometry(&sensor, raw, k, l)?, into_lens_geometry(&clipped, raw, k, l)?)));
    let gain = 2f32.powf(transform.baseline_exposure as f32);
    let to_working = |p: [f32; 3]| transform.matrix.apply_f32(std::array::from_fn(|i| p[i] * transform.wb[i] * gain));
    if raw.format == RawFormat::Cr3 {
        // Orient the camera JPEG and the sensor identically before collecting pixel correspondences.
        sensor = sensor.into_oriented(raw.orientation);
        clipped = clipped.into_oriented(raw.orientation);
        reference = reference.into_oriented(raw.orientation);
        sensor.map_in_place(to_working);
        // Judge the external warp against the established CR3 framing fallback, not
        // against unregistered sensor pixels. This preserves an already better fit.
        let (fallback, fallback_mask) = align_cr3_framing(sensor.clone(), clipped.clone(), &reference);
        let proposed =
            correction.filter(|correction| transform.matrix_is_fallback && lens.is_none() && correction.warp.is_some()).and_then(|correction| {
                align_profile_geometry_against(&sensor, &clipped, &reference, correction, lightcraft_geom::Orientation::Normal, Some(&fallback))
            });
        (sensor, clipped) = proposed.unwrap_or((fallback, fallback_mask));
    }
    let mut sensor = fit(&sensor, size, size, Filter::Box);
    let mut reference_small = reference_at(&reference, &sensor);
    if let Some((warped, warped_clipped)) = warped {
        let candidate = fit(&warped, size, size, Filter::Box);
        let candidate_reference = reference_at(&reference, &candidate);
        if alignment(&candidate, &candidate_reference) > alignment(&sensor, &reference_small) {
            (sensor, reference_small, clipped) = (candidate, candidate_reference, warped_clipped);
        }
    }
    let reference = reference_small;
    // A proxy pixel is clipped when any sample it averages is (outside the CR3 framing the mask is
    // NaN: those pixels are left out of the pairs anyway). Resized to exactly the proxy's size, like
    // the reference, so the mask pairs pixel for pixel.
    let clipped: Vec<bool> = resize(&clipped, sensor.width, sensor.height, Filter::Box).data.iter().map(|p| p[0] > 0.0).collect();
    if raw.format != RawFormat::Cr3 {
        sensor.map_in_place(to_working);
    }
    Some((sensor, reference, clipped))
}

/// The camera JPEG at exactly the sensor proxy's size, so the two pair pixel for pixel. Fitting it
/// into the proxy's box instead keeps its own aspect, which can round a pixel short: an ILCE-7RM4's
/// 1616×1080 preview, decoded at 384×257, fits a 192×128 proxy as 191×128, and `collect_pairs`
/// then rejected every file `lightcraft-cli calibrate` was given. `proxies` has already checked
/// that the aspects agree within 2 %.
pub(super) fn reference_at(reference: &Rgb32f, sensor: &Rgb32f) -> Rgb32f {
    resize(reference, sensor.width, sensor.height, Filter::Box)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Cr3Framing {
    pub(super) scale: f32,
    /// Translation as a fraction of the oriented frame's width and height.
    pub(super) offset: [f32; 2],
}

impl Cr3Framing {
    pub(super) const IDENTITY: Self = Self { scale: 1.0, offset: [0.0; 2] };

    pub(super) fn source(self, x: f32, y: f32, w: usize, h: usize) -> (f32, f32) {
        let (w, h) = (w as f32, h as f32);
        ((x - w * 0.5) * self.scale + w * (0.5 + self.offset[0]), (y - h * 0.5) * self.scale + h * (0.5 + self.offset[1]))
    }
}

/// Register the JPEG's remaining framing on a fixed proxy. Edge directions survive different
/// white balance and monotone camera tone; no target RGB values train this three-parameter fit.
/// A separate third of the edges must confirm a substantial improvement and a close match.
/// A weak, flat or unrelated preview leaves the sensor framing unchanged.
pub(super) fn estimate_cr3_framing(sensor: &Rgb32f, reference: &Rgb32f) -> Option<Cr3Framing> {
    estimate_framing_against(sensor, reference, sensor, None)
}

#[cfg(test)]
thread_local! {
    static LAST_GEOMETRY_SCORES: std::cell::Cell<Option<[f64; 4]>> = const { std::cell::Cell::new(None) };
    static LAST_FRAMED_SCORES: std::cell::Cell<Option<[f64; 4]>> = const { std::cell::Cell::new(None) };
}

/// Return this test thread's last `[train_before, train_after, held_before, held_after]`
/// scores against the original sensor geometry.
#[cfg(test)]
pub(super) fn geometry_gate_scores() -> Option<[f64; 4]> {
    LAST_GEOMETRY_SCORES.get()
}

/// Return this test thread's last `[train_before, train_after, held_before, held_after]`
/// scores comparing the CR3 proposal with the established framing fallback.
#[cfg(test)]
pub(super) fn geometry_framed_scores() -> Option<[f64; 4]> {
    LAST_FRAMED_SCORES.get()
}

/// Judge a proposed lens warp and bounded framing against the original sensor on fixed
/// training and independent held-out edges. Lens-only correction may be sufficient.
fn estimate_framing_against(sensor: &Rgb32f, reference: &Rgb32f, original: &Rgb32f, framed: Option<&Rgb32f>) -> Option<Cr3Framing> {
    #[cfg(test)]
    {
        LAST_GEOMETRY_SCORES.set(None);
        LAST_FRAMED_SCORES.set(None);
    }
    if sensor.width.checked_mul(sensor.height)? != sensor.data.len() || reference.width.checked_mul(reference.height)? != reference.data.len() {
        return None;
    }
    let sensor = fit(sensor, PROXY, PROXY, Filter::Box);
    let (w, h) = (sensor.width, sensor.height);
    if w < 16 || h < 16 {
        return None;
    }
    let reference = fit(reference, w, h, Filter::Box);
    if (reference.width, reference.height) != (w, h) {
        return None;
    }
    let original = fit(original, PROXY, PROXY, Filter::Box);
    if (original.width, original.height) != (w, h) || original.data.len() != sensor.data.len() {
        return None;
    }
    let framed = framed.map(|image| fit(image, PROXY, PROXY, Filter::Box));
    if framed.as_ref().is_some_and(|image| (image.width, image.height) != (w, h) || image.data.len() != sensor.data.len()) {
        return None;
    }
    let mut framed_gradients = framed.as_ref().map(|_| Rgb32f::new(w, h));
    let mut gradients = Rgb32f::new(w, h);
    let mut original_gradients = Rgb32f::new(w, h);
    let mut edges = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let derivative = |image: &Rgb32f| {
                [
                    luminance_2020(image.data[y * w + x + 1]) - luminance_2020(image.data[y * w + x - 1]),
                    luminance_2020(image.data[(y + 1) * w + x]) - luminance_2020(image.data[(y - 1) * w + x]),
                ]
            };
            let d = derivative(&sensor);
            gradients.data[y * w + x] = [d[0], d[1], 0.0];
            let d = derivative(&original);
            original_gradients.data[y * w + x] = [d[0], d[1], 0.0];
            if let (Some(image), Some(gradients)) = (&framed, &mut framed_gradients) {
                let d = derivative(image);
                gradients.data[y * w + x] = [d[0], d[1], 0.0];
            }
            let d = derivative(&reference);
            let magnitude = d[0].hypot(d[1]);
            let value = luminance_2020(reference.data[y * w + x]);
            if (0.02..0.9).contains(&value) && magnitude.is_finite() && magnitude > 0.03 {
                edges.push((x as f32 + 0.5, y as f32 + 0.5, d, magnitude, (x * 17 + y * 11) % 3 == 0));
            }
        }
    }
    if edges.len() < 192 {
        return None;
    }
    let score = |gradients: &Rgb32f, framing: Cr3Framing, held_out: bool, support: Option<(&Rgb32f, Cr3Framing)>| -> Option<f64> {
        let (mut agreement, mut weight, mut valid, mut total) = (0.0, 0.0, 0usize, 0usize);
        for &(x, y, target, magnitude, holdout) in &edges {
            if holdout != held_out {
                continue;
            }
            total += 1;
            let (sx, sy) = framing.source(x, y, w, h);
            if sx < 1.5 || sy < 1.5 || sx > w as f32 - 1.5 || sy > h as f32 - 1.5 {
                continue;
            }
            let sample = gradients.sample_bilinear(sx, sy);
            let length = sample[0].hypot(sample[1]);
            if !length.is_finite() || length <= 0.01 {
                continue;
            }
            // Comparing against framed CR3 fallback uses exactly the same valid
            // edges on both sides, including the same reference weights.
            if let Some((other, map)) = support {
                let (ox, oy) = map.source(x, y, w, h);
                if ox < 1.5 || oy < 1.5 || ox > w as f32 - 1.5 || oy > h as f32 - 1.5 {
                    continue;
                }
                let p = other.sample_bilinear(ox, oy);
                let length = p[0].hypot(p[1]);
                if !length.is_finite() || length <= 0.01 {
                    continue;
                }
            }
            let k = f64::from(magnitude.min(0.25));
            agreement += k * f64::from((sample[0] * target[0] + sample[1] * target[1]) / (length * magnitude));
            weight += k;
            valid += 1;
        }
        (valid >= 64 && valid * 5 >= total * 4 && weight > 0.0).then_some(agreement / weight)
    };
    let before = score(&original_gradients, Cr3Framing::IDENTITY, false, None)?;
    let mut best = (score(&gradients, Cr3Framing::IDENTITY, false, None).unwrap_or(-1.0), Cr3Framing::IDENTITY);
    // At most 10% zoom and two proxy pixels of translation; a coarse search followed by one
    // local refinement prevents unbounded optimisation on the photograph's contents.
    for s in -10..=10 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing { scale: 1.0 + s as f32 * 0.01, offset: [tx as f32 * 0.5 / w as f32, ty as f32 * 0.5 / h as f32] };
                if let Some(value) = score(&gradients, framing, false, None)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let coarse = best.1;
    for s in -5..=5 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing {
                    scale: coarse.scale + s as f32 * 0.002,
                    offset: [coarse.offset[0] + tx as f32 * 0.125 / w as f32, coarse.offset[1] + ty as f32 * 0.125 / h as f32],
                };
                if !(0.9..=1.1).contains(&framing.scale) || framing.offset[0].abs() > 2.0 / w as f32 || framing.offset[1].abs() > 2.0 / h as f32 {
                    continue;
                }
                if let Some(value) = score(&gradients, framing, false, None)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let (held_before, held_after) = (score(&original_gradients, Cr3Framing::IDENTITY, true, None)?, score(&gradients, best.1, true, None)?);
    #[cfg(test)]
    LAST_GEOMETRY_SCORES.set(Some([before, best.0, held_before, held_after]));
    if best.0 < 0.9 || best.0 < before + 0.04 || held_after < 0.9 || held_after < held_before + 0.04 {
        return None;
    }
    if let Some(framed) = &framed_gradients {
        let before = score(framed, Cr3Framing::IDENTITY, false, Some((&gradients, best.1)))?;
        let after = score(&gradients, best.1, false, Some((framed, Cr3Framing::IDENTITY)))?;
        let held_before = score(framed, Cr3Framing::IDENTITY, true, Some((&gradients, best.1)))?;
        let held_after = score(&gradients, best.1, true, Some((framed, Cr3Framing::IDENTITY)))?;
        #[cfg(test)]
        LAST_FRAMED_SCORES.set(Some([before, after, held_before, held_after]));
        if after < before || held_after < held_before {
            return None;
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera JPEG framing {:?}, held-out edge agreement {held_before:.4} -> {held_after:.4}", best.1);
    }
    Some(best.1)
}

/// The sensor proxy and its clip mask (same size), both moved to the camera JPEG's framing.
fn align_cr3_framing(sensor: Rgb32f, clipped: Rgb32f, reference: &Rgb32f) -> (Rgb32f, Rgb32f) {
    let Some(framing) = estimate_cr3_framing(&sensor, reference) else { return (sensor, clipped) };
    let apply = |image: &Rgb32f| {
        let (w, h) = (image.width, image.height);
        let mut aligned = Rgb32f::new(w, h);
        for (i, pixel) in aligned.data.iter_mut().enumerate() {
            let (x, y) = framing.source((i % w) as f32 + 0.5, (i / w) as f32 + 0.5, w, h);
            *pixel = if x >= 0.5 && y >= 0.5 && x <= w as f32 - 0.5 && y <= h as f32 - 0.5 { image.sample_bilinear(x, y) } else { [f32::NAN; 3] };
        }
        aligned
    };
    (apply(&sensor), apply(&clipped))
}

/// Apply a shared source map to colour or a nonnegative clip mask. Invalid borders
/// stay NaN; any bilinear contribution from a clipped source stays positive.
pub(super) fn resample_correspondence(image: &Rgb32f, source: impl Fn(f64, f64) -> lightcraft_geom::Point) -> Rgb32f {
    let (w, h) = (image.width, image.height);
    let mut out = Rgb32f::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    for (i, pixel) in out.data.iter_mut().enumerate() {
        let p = source((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
        *pixel = if p.x.is_finite() && p.y.is_finite() && p.x >= 0.5 && p.y >= 0.5 && p.x <= w as f64 - 0.5 && p.y <= h as f64 - 0.5 {
            image.sample_bilinear(p.x as f32, p.y as f32)
        } else {
            [f32::NAN; 3]
        };
    }
    out
}

/// Propose correspondence geometry only. A resolved lens describes the oriented
/// default crop; undo its orientation to match this un-oriented sensor proxy.
#[cfg(test)]
pub(super) fn align_profile_geometry(
    sensor: &Rgb32f,
    clipped: &Rgb32f,
    reference: &Rgb32f,
    correction: &lightcraft_develop::EmbeddedLens,
    orientation: lightcraft_geom::Orientation,
) -> Option<(Rgb32f, Rgb32f)> {
    align_profile_geometry_against(sensor, clipped, reference, correction, orientation, None)
}

/// Propose lens geometry plus bounded framing and require the original training/held-out gates.
/// When supplied, the framed CR3 baseline must also be preserved or improved on both splits.
fn align_profile_geometry_against(
    sensor: &Rgb32f,
    clipped: &Rgb32f,
    reference: &Rgb32f,
    correction: &lightcraft_develop::EmbeddedLens,
    orientation: lightcraft_geom::Orientation,
    baseline: Option<&Rgb32f>,
) -> Option<(Rgb32f, Rgb32f)> {
    if correction.warp.is_none() || (sensor.width, sensor.height) != (clipped.width, clipped.height) {
        return None;
    }
    let (w, h) = (sensor.width as f64, sensor.height as f64);
    let (ow, oh) = if orientation.swaps_axes() { (h, w) } else { (w, h) };
    let correction = lightcraft_pipeline::optics::reorient_lens(correction, orientation.inverse(), ow, oh);
    let mut warp = lightcraft_pipeline::optics::Warp::identity(w, h);
    warp.lens = Some(correction);
    warp.lens_dist = 1.0;
    let source = |x, y| warp.corrected_to_source(lightcraft_geom::Point::new(x, y), 1);
    let candidate = resample_correspondence(sensor, source);
    let framing = estimate_framing_against(&candidate, reference, sensor, baseline)?;
    let mask = resample_correspondence(clipped, source);
    let frame = |x: f64, y: f64| {
        let (x, y) = framing.source(x as f32, y as f32, sensor.width, sensor.height);
        lightcraft_geom::Point::new(f64::from(x), f64::from(y))
    };
    Some((resample_correspondence(&candidate, frame), resample_correspondence(&mask, frame)))
}

/// Colour training pairs of one raw for a camera profile: white-balanced camera RGB (with the
/// baseline exposure) → its camera JPEG (linear Rec.2020). `None` for formats with their own
/// colour matrices, other files or unusable previews. `correction` may supply accepted CR3 geometry.
pub(crate) fn profile_pairs(
    raw: &RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    correction: Option<&lightcraft_develop::EmbeddedLens>,
) -> Option<Vec<([f64; 3], [f64; 3])>> {
    if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (sensor, reference, clipped) = proxies(raw, bytes, &transform, PROFILE_PROXY, 0.0, lens, correction)?;
    let to_camera = transform.matrix.inverse()?;
    // clipped sensor pixels don't record the scene's colour (see `fit_look`)
    let sensor = without_clipped(&sensor, &clipped).unwrap_or(sensor);
    let (pairs, _) = collect_pairs(&sensor, &reference, 0.05, None, false)?;
    Some(pairs.into_iter().map(|(x, y)| (to_camera.apply(x), y)).collect())
}

/// A camera profile's colour model (matrix from white-balanced camera RGB, hue/saturation table)
/// fitted to pairs pooled from many photos.
pub(crate) fn fit_profile(pairs: &[([f64; 3], [f64; 3])]) -> Option<(Mat3, Option<HsvTable>)> {
    let matrix = fit_matrix(pairs)?;
    Some((matrix, fit_hue_sat(pairs, &matrix)))
}

/// A raw's lens-correction warp, as its camera applied it to the embedded JPEG: the first `WarpRectilinear` of
/// `OpcodeList3` (its green plane; the in-file corrections LightCraft reads have no tangential terms). `None` without
/// one, or for a DNG 1.6 `WarpRectilinear2` chain (phone DNGs, which carry colour matrices and are not fitted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LensGeometry {
    /// `kr0..kr3`: source radius = r * (kr0 + kr1 r^2 + kr2 r^4 + kr3 r^6), r over the farthest corner distance.
    pub(super) k: [f64; 4],
    /// The optical centre, relative to the active area (as the opcode has it).
    pub(super) center: [f64; 2],
}

pub(crate) fn lens_geometry(list3: &[lightcraft_raw::Opcode]) -> Option<LensGeometry> {
    use lightcraft_raw::Opcode;
    let ops: Vec<&Opcode> = lightcraft_raw::opcodes::applied_ops(list3).collect();
    if ops.iter().any(|op| matches!(op, Opcode::WarpRectilinear2 { .. })) {
        return None;
    }
    ops.into_iter().find_map(|op| match op {
        Opcode::WarpRectilinear { planes, center } => {
            let p = planes.get(1).or(planes.first())?;
            let k = [p[0], p[1], p[2], p[3]];
            (k.iter().chain(center).all(|v| v.is_finite()) && (0.0..=1.0).contains(&center[0]) && (0.0..=1.0).contains(&center[1]))
                .then_some(LensGeometry { k, center: *center })
        }
        _ => None,
    })
}

/// How well two same-size proxies line up: the correlation of their high-passed log luminance over the pixels both
/// have (a tone curve or colour change leaves it alone, a geometric offset lowers it). `-1` without enough pixels.
pub(super) fn alignment(a: &Rgb32f, b: &Rgb32f) -> f64 {
    let (w, h) = (a.width, a.height);
    if (b.width, b.height) != (w, h) || w < 3 || h < 3 {
        return -1.0;
    }
    let lum = |img: &Rgb32f| img.data.iter().map(|p| (luminance_2020(*p).max(1e-4) as f64).ln()).collect::<Vec<f64>>();
    let high = |l: &[f64]| -> Vec<f64> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                    return f64::NAN;
                }
                let around = [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1];
                l[i] - around.iter().map(|&j| l[j]).sum::<f64>() / 8.0
            })
            .collect()
    };
    let (ha, hb) = (high(&lum(a)), high(&lum(b)));
    let pairs: Vec<(f64, f64)> = ha.into_iter().zip(hb).filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
    if pairs.len() < 64 {
        return -1.0;
    }
    let n = pairs.len() as f64;
    let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
    let cov: f64 = pairs.iter().map(|p| (p.0 - ma) * (p.1 - mb)).sum();
    let (va, vb): (f64, f64) = (pairs.iter().map(|p| (p.0 - ma).powi(2)).sum(), pairs.iter().map(|p| (p.1 - mb).powi(2)).sum());
    let r = cov / (va * vb).sqrt().max(1e-12);
    if r.is_finite() { r } else { -1.0 }
}

/// A `k`-binned sensor proxy of `raw` ([`sensor_proxy`]: the default crop, or the active area when the crop is
/// smaller than a block, at `1/k`) moved into the camera JPEG's geometry: each pixel samples the proxy where `lens`
/// maps it, and is NaN where that falls outside the proxy (the pairs leave it out). `None` for a proxy of another
/// size (the demosaiced fallback of linear raws).
pub(super) fn into_lens_geometry(img: &Rgb32f, raw: &RawImage, k: usize, lens: &LensGeometry) -> Option<Rgb32f> {
    let a = raw.active_area;
    let c = raw.crop.clipped(a.width, a.height);
    let c = if c.width < k || c.height < k { lightcraft_raw::Rect::new(0, 0, a.width, a.height) } else { c };
    let (w, h) = (img.width, img.height);
    if k == 0 || w != c.width / k || h != c.height / k || w == 0 || h == 0 || a.width < 2 || a.height < 2 {
        return None;
    }
    let (aw, ah) = (a.width as f64 - 1.0, a.height as f64 - 1.0);
    let (cx, cy) = (lens.center[0] * aw, lens.center[1] * ah);
    let m = [(0.0, 0.0), (aw, 0.0), (0.0, ah), (aw, ah)].iter().map(|&(x, y)| (x - cx).hypot(y - cy)).fold(0.0, f64::max).max(1e-9);
    let [k0, k1, k2, k3] = lens.k;
    let kf = k as f64;
    let (x0, y0) = (c.x as f64, c.y as f64);
    let mut out = Rgb32f::new(w, h);
    for (i, px) in out.data.iter_mut().enumerate() {
        // the proxy pixel's centre in active-area pixel-index coordinates, mapped to the sensor
        let (ox, oy) = (x0 + ((i % w) as f64 + 0.5) * kf - 0.5, y0 + ((i / w) as f64 + 0.5) * kf - 0.5);
        let (dx, dy) = ((ox - cx) / m, (oy - cy) / m);
        let r2 = dx * dx + dy * dy;
        let f = k0 + r2 * (k1 + r2 * (k2 + r2 * k3));
        let (sx, sy) = ((cx + m * dx * f + 0.5 - x0) / kf, (cy + m * dy * f + 0.5 - y0) / kf);
        *px = if sx >= 0.5 && sy >= 0.5 && sx <= w as f64 - 0.5 && sy <= h as f64 - 0.5 {
            img.sample_bilinear(sx as f32, sy as f32)
        } else {
            [f32::NAN; 3]
        };
    }
    Some(out)
}

pub(super) fn sensor_proxy(raw: &RawImage, k: usize, edge: usize) -> Option<Rgb32f> {
    Some(match raw.develop_binned(k, SENSOR_CLIP).ok()? {
        Some(sensor) => sensor,
        // Lens opcodes cannot be applied to a binned mosaic: they are defined at full resolution.
        None if (raw.cpp == 3 && raw.cfa.is_none()) || (raw.format == RawFormat::Dng && !raw.opcodes.list3.is_empty()) => {
            fit(&raw.develop(lightcraft_raw::Method::Bilinear).ok()?, edge, edge, Filter::Box)
        }
        None => return None,
    })
}
