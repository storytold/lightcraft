/// The white-balanced camera RGB that `model` (white-balanced camera RGB → linear Rec.2020) renders
/// neutral, as bright as it renders `[1, 1, 1]`: what fully clipped highlights are rebuilt to, so they come out
/// neutral whatever the colour model does to the camera's own neutral (a look fitted to the camera's JPEG or a
/// camera profile often tints it by a few per cent, which any darkening of the highlights would show).
/// `[1, 1, 1]` (the camera's own neutral, as before this existed) when the model can't be trusted with it: not
/// invertible or nearly so, an answer far from the camera's neutral (a sane colour model moves white by a few per
/// cent), or one that isn't neutral through the `f32` matrix the decode applies.
pub(crate) fn clipped_white(model: &lightcraft_color::Mat3) -> [f32; 3] {
    const CAMERA: [f32; 3] = [1.0; 3];
    /// Largest condition number (max row sum norm) of a model trusted for this.
    const CONDITION: f64 = 1e3;
    /// Largest factor between the answer and the camera's neutral, per channel.
    const SPREAD: f64 = 4.0;
    let norm = |m: &lightcraft_color::Mat3| m.0.iter().map(|row| row.iter().map(|v| v.abs()).sum::<f64>()).fold(0.0, f64::max);
    let Some(inverse) = model.inverse() else { return CAMERA };
    let condition = norm(model) * norm(&inverse);
    if !condition.is_finite() || condition > CONDITION {
        return CAMERA;
    }
    let luma = |v: [f64; 3]| {
        let c = model.apply(v);
        (0..3).map(|i| c[i] * f64::from(lightcraft_color::LUMA_2020[i])).sum::<f64>()
    };
    let n = inverse.apply([1.0; 3]);
    let (target, own) = (luma([1.0; 3]), luma(n));
    if !(target > 0.0 && own > 0.0 && target.is_finite() && own.is_finite()) {
        return CAMERA;
    }
    let white = n.map(|v| v * target / own);
    if !white.iter().all(|v| v.is_finite() && (1.0 / SPREAD..=SPREAD).contains(v)) {
        return CAMERA;
    }
    let white = white.map(|v| v as f32);
    // neutral through the matrix as the decode applies it
    let m = model.to_f32();
    let out: [f32; 3] = std::array::from_fn(|i| m[i][0] * white[0] + m[i][1] * white[1] + m[i][2] * white[2]);
    let (lo, hi) = (out[0].min(out[1]).min(out[2]), out[0].max(out[1]).max(out[2]));
    if lo > 0.0 && hi.is_finite() && hi - lo <= 1e-3 * hi { white } else { CAMERA }
}

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
