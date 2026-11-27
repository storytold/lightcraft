//! Scene-linear preparation: white balance + exposure, and the spatial planes the per-pixel stage
//! needs (edge-aware base layer for highlights/shadows, clarity/texture bands, dehaze veil),
//! backed by `craft_develop::local`.

use std::sync::Arc;

use lightcraft_color::Xy;
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::blur::{gauss_taps, gaussian, gaussian_taps};
use lightcraft_raster::{Plane, Rgb32f, par_join};

use crate::geometry::Frame;
use crate::{Prepared, Quality, SourceInfo, masks, timed};

pub use craft_develop::local::{
    AIRLIGHT_STEP, BASE_COARSE_STEP, BASE_DETAIL, BASE_DETAIL_MAX, BASE_EPS, BaseKey, CHROMA_SIGMA, CLARITY_EPS, NRC_SAMPLES, NrColor, NrLum,
    NrcNoise, NrcParams, NrcPass, PlaneSigmas, SHARPEN_SIGMA_MAX, SHARPEN_SIGMA_MIN, SharpenKey, airlight_of, base_detail_radius, dark_of,
    detail_scale, fit_nonneg_line, guided_fast_step, log_lum, plane_at, sharpen_gain, sharpen_sigma, texture_sigma,
};

/// White balance (relative to the source's as-shot white) and exposure, in place.
pub fn scene_linear_pre(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 2f32.powf(s.light.exposure as f32));
}

/// White balance only (exposure is applied by the per-pixel stage, see [`crate::Prepared`]).
pub fn white_balance(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 1.0);
}

fn wb_gain(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings, gain: f32) {
    let m = wb_matrix_for(info, s);
    let w = img.width;
    crate::for_rows(&mut img.data, w, |_, row| {
        for p in row.iter_mut() {
            let mut c = *p;
            if let Some(m) = &m {
                c = [
                    m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
                    m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
                    m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
                ];
            }
            *p = [(c[0] * gain).max(0.0), (c[1] * gain).max(0.0), (c[2] * gain).max(0.0)];
        }
    });
}

/// Whether `s` uses the checked Bradford white-balance adaptation (`Process::V10` on) instead
/// of the legacy matrix (V1 to V9, which stay frozen).
pub fn checked_wb(s: &DevelopSettings) -> bool {
    craft_develop::local::checked_wb(&crate::bridge::to_craft_settings(s))
}

/// The white-balance matrix (linear Rec.2020) for the settings, or `None` when the as-shot white
/// is kept.
pub fn wb_matrix_for(info: &SourceInfo, s: &DevelopSettings) -> Option<[[f32; 3]; 3]> {
    let rebalance = |target: craft_color_transform::Xy| {
        let cc = info.camera_color.as_deref()?;
        lightcraft_raw::color::rebalance(&cc.tags, cc.developed_for, crate::bridge::from_craft_xy(target)).map(crate::bridge::to_craft_mat3)
    };
    let cb: Option<&dyn Fn(craft_color_transform::Xy) -> Option<craft_color_transform::Mat3>> =
        (info.camera_color.is_some() && info.raw && !info.relative_wb).then_some(&rebalance);
    craft_develop::local::wb_matrix_for_with(&crate::bridge::to_craft_wb_source(info), &crate::bridge::to_craft_settings(s), cb)
}

pub fn effective_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    craft_develop::local::effective_wb(&crate::bridge::to_craft_wb_source(info), &crate::bridge::to_craft_settings(s))
}

pub fn legacy_white(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<Xy> {
    craft_develop::local::legacy_white(temp, tint, as_shot).map(crate::bridge::from_craft_xy)
}

pub fn legacy_to_kelvin(temp: f64, tint: f64, as_shot: (f64, f64)) -> (f64, f64) {
    craft_develop::local::legacy_to_kelvin(temp, tint, as_shot)
}

pub fn kelvin_to_legacy(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<(f64, f64)> {
    craft_develop::local::kelvin_to_legacy(temp, tint, as_shot)
}

pub fn normalize_wb(d: &mut DevelopSettings, info: &SourceInfo) {
    let mut craft = crate::bridge::to_craft_settings(d);
    craft_develop::local::normalize_wb(&mut craft, &crate::bridge::to_craft_wb_source(info));
    d.wb.mode = crate::bridge::from_craft_wb_mode(craft.wb.mode);
    d.wb.temp = craft.wb.temp;
    d.wb.tint = craft.wb.tint;
    d.wb.scale = crate::bridge::from_craft_wb_scale(craft.wb.scale);
}

pub fn guided(p: &Plane, sigma: f32, eps: f32) -> Plane {
    crate::bridge::from_craft_plane(craft_develop::local::guided(&crate::bridge::to_craft_plane(p), sigma, eps))
}

pub fn guided_fast(p: &Plane, sigma: f32, eps: f32) -> Plane {
    crate::bridge::from_craft_plane(craft_develop::local::guided_fast(&crate::bridge::to_craft_plane(p), sigma, eps))
}

pub fn median_hv(p: &Plane, r: usize) -> Plane {
    crate::bridge::from_craft_plane(craft_develop::local::median_hv(&crate::bridge::to_craft_plane(p), r))
}

pub fn tone_base_detail(s: &DevelopSettings, px_per_long: f64) -> usize {
    craft_develop::local::tone_base_detail(&crate::bridge::to_craft_settings(s), px_per_long)
}

pub fn tone_base(l: &Plane, sigma: f32, detail: usize, keeps_gaps: bool) -> Plane {
    crate::bridge::from_craft_plane(craft_develop::local::tone_base(&crate::bridge::to_craft_plane(l), sigma, detail, keeps_gaps))
}

pub fn detail_free(l: &Plane, r: usize) -> Plane {
    crate::bridge::from_craft_plane(craft_develop::local::detail_free(&crate::bridge::to_craft_plane(l), r))
}

pub fn nr_params(s: &DevelopSettings, scale: f32) -> (Option<NrLum>, Option<NrColor>) {
    craft_develop::local::nr_params(&crate::bridge::to_craft_settings(s), scale)
}

/// The Contrast slider as a 0..=1 weight. Hoisted out of the pixel loop, which used to divide by 100.
pub fn nr_contrast_weight(s: &DevelopSettings) -> f32 {
    let c = (s.detail.nr_contrast / 100.0) as f32;
    if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 }
}

/// How much of a log-luminance residual noise reduction may still apply.
///
/// Contrast 0 returns 1, so the blend is the historical one. At 1 the knee is 0.04 stops and a
/// residual past about 0.08 stops is left alone.
pub fn nr_lum_keep(delta: f32, contrast: f32) -> f32 {
    if contrast == 0.0 {
        return 1.0;
    }
    let knee = 0.04 + (1.0 - contrast) * 0.2;
    1.0 - ((delta.abs() - knee) / knee).clamp(0.0, 1.0)
}

/// Luminance noise reduction with the contrast weight applied to the residual.
fn apply_luminance(img: &mut Rgb32f, nr: NrLum, contrast: f32) {
    let w = img.width;
    let l = img.map(log_lum);
    let f = guided(&l, nr.sigma, nr.eps);
    crate::for_rows(&mut img.data, w, |y, row| {
        for (x, p) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let delta = f.data[i] - l.data[i];
            let d = delta * nr.k * nr_lum_keep(delta, contrast);
            let g = d.exp2();
            *p = p.map(|v| v * g);
        }
    });
}

pub fn denoise(img: &mut Rgb32f, s: &DevelopSettings, scale: f32, colour_noise: Option<&crate::nrc_joint::JointNoise>) {
    let contrast = nr_contrast_weight(s);
    // Contrast 0 is the shared developer's blend, unchanged, including joint colour NR.
    if contrast == 0.0 {
        let mut craft = crate::bridge::take_craft_rgb32f(img);
        craft_develop::local::denoise(&mut craft, &crate::bridge::to_craft_settings(s), scale, colour_noise);
        crate::bridge::put_craft_rgb32f(img, craft);
        return;
    }
    let (lum, _) = nr_params(s, scale);
    if let Some(nr) = lum {
        apply_luminance(img, nr, contrast);
    }
    let mut rest = s.clone();
    rest.detail.nr_luminance = 0.0;
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    craft_develop::local::denoise(&mut craft, &crate::bridge::to_craft_settings(&rest), scale, colour_noise);
    crate::bridge::put_craft_rgb32f(img, craft);
}

/// Luminance noise reduction for the colour-noise model. Contrast 0 uses [`apply_nr`].
pub(crate) fn apply_luminance_nr(img: &mut Rgb32f, lum: Option<NrLum>, contrast: f32) {
    if contrast == 0.0 {
        apply_nr(img, lum, None);
        return;
    }
    if let Some(nr) = lum {
        apply_luminance(img, nr, contrast);
    }
}

pub fn apply_nr(img: &mut Rgb32f, lum: Option<NrLum>, col: Option<NrColor>) {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    craft_develop::local::apply_nr(&mut craft, lum, col);
    crate::bridge::put_craft_rgb32f(img, craft);
}

pub fn nrc_params(s: &DevelopSettings, scale: f32) -> Option<NrcParams> {
    craft_develop::local::nrc_params(&crate::bridge::to_craft_settings(s), scale)
}

pub fn joint_colour_nr(s: &DevelopSettings) -> bool {
    craft_develop::local::joint_colour_nr(&crate::bridge::to_craft_settings(s))
}

#[derive(Clone, Default)]
pub(crate) struct Planes {
    pub key: u64,
    pub pre_nr: u64,
    pub log_l: Option<Arc<Plane>>,
    pub base: Option<(BaseKey, Arc<Plane>)>,
    pub clarity: Option<(u32, Arc<Plane>)>,
    pub texture: Option<(u32, Arc<Plane>)>,
    pub sharpen: Option<(SharpenKey, Arc<Plane>)>,
    pub dark: Option<(u32, Arc<Plane>, f32)>,
    pub chroma: Option<(u32, Arc<Rgb32f>)>,
}

impl Planes {
    pub fn for_other_nr(self) -> Planes {
        Planes { key: 0, pre_nr: self.pre_nr, log_l: None, texture: None, sharpen: None, ..self }
    }
}

pub fn sharpen_key(s: &DevelopSettings, sigma: f32) -> SharpenKey {
    craft_develop::local::sharpen_key(&crate::bridge::to_craft_settings(s), sigma)
}

pub fn base_keeps_gaps(s: &DevelopSettings) -> bool {
    craft_develop::local::base_keeps_gaps(&crate::bridge::to_craft_settings(s))
}

pub fn pixel_sharpen(s: &DevelopSettings) -> bool {
    craft_develop::local::pixel_sharpen(&crate::bridge::to_craft_settings(s))
}

pub fn base_key(s: &DevelopSettings, sigma: f32, detail: usize) -> BaseKey {
    craft_develop::local::base_key(&crate::bridge::to_craft_settings(s), sigma, detail)
}

pub fn sharpen_sigma_raw(s: &DevelopSettings, px_per_long: f64, native_long: f64) -> f32 {
    craft_develop::local::sharpen_sigma_raw(&crate::bridge::to_craft_settings(s), px_per_long, native_long)
}

pub fn plane_sigmas(s: &DevelopSettings, px_per_long: f64, native_long: f64, q: Quality) -> PlaneSigmas {
    craft_develop::local::plane_sigmas(&crate::bridge::to_craft_settings(s), px_per_long, native_long, crate::bridge::to_craft_quality(q))
}

/// The spatial planes the per-pixel stage needs for `img` (white-balanced, before exposure),
/// reusing whatever `planes` already holds for it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    img: Arc<Rgb32f>,
    s: &DevelopSettings,
    frame: &Frame,
    px_per_long: f64,
    native_long: f64,
    q: Quality,
    planes: &mut Planes,
    mattes: Option<&masks::Mattes>,
) -> Prepared {
    let log_l = planes.log_l.get_or_insert_with(|| Arc::new(timed("log_l", || img.map(log_lum)))).clone();
    let PlaneSigmas {
        base: base_sigma,
        base_detail,
        clarity: clarity_sigma,
        texture: texture_sigma,
        sharpen: sharpen_sigma_px,
        dark: dark_sigma,
        chroma: chroma_sigma,
    } = plane_sigmas(s, px_per_long, native_long, q);

    let Planes { base: sb, clarity: sc, texture: st, sharpen: ss, dark: sd, chroma: sch, .. } = planes;
    let chroma_blur = chroma_sigma.map(|sg| match sch {
        Some((k, c)) if *k == sg.to_bits() => c.clone(),
        _ => {
            let c = timed("chroma", || Arc::new(gaussian(&img.map(masks::chromaticity), sg)));
            *sch = Some((sg.to_bits(), c.clone()));
            c
        }
    });
    let l = &log_l;
    let (base, (clarity_blur, ((texture_blur, sharpen_blur), dark))) = par_join(
        || base_sigma.map(|sg| plane_at(sb, base_key(s, sg, base_detail), || timed("base", || tone_base(l, sg, base_detail, base_keeps_gaps(s))))),
        || {
            par_join(
                || clarity_sigma.map(|sg| plane_at(sc, sg.to_bits(), || timed("clarity", || guided_fast(l, sg, CLARITY_EPS)))),
                || {
                    par_join(
                        || {
                            par_join(
                                || texture_sigma.map(|sg| plane_at(st, sg.to_bits(), || timed("texture", || gaussian(l, sg)))),
                                || {
                                    sharpen_sigma_px
                                        .map(|sg| plane_at(ss, sharpen_key(s, sg), || timed("sharpen", || gaussian_taps(l, &gauss_taps(sg)))))
                                },
                            )
                        },
                        || {
                            dark_sigma.map(|sg| match sd {
                                Some((k, d, air)) if *k == sg.to_bits() => (d.clone(), *air),
                                _ => {
                                    let d = timed("dehaze", || Arc::new(gaussian(&img.map(dark_of), sg)));
                                    let air = airlight(&d);
                                    *sd = Some((sg.to_bits(), d.clone(), air));
                                    (d, air)
                                }
                            })
                        },
                    )
                },
            )
        },
    );
    let base = base.unwrap_or_else(|| log_l.clone());
    let (dark, air) = match dark {
        Some((d, air)) => (Some(d), air),
        None => (None, 1.0),
    };
    let ev = s.light.exposure as f32;
    let masks = timed("masks", || masks::evaluate(&s.masks, frame, img.width, img.height, &img, &log_l, ev, mattes));
    Prepared { img, log_l, base, clarity_blur, texture_blur, sharpen_blur, dark, chroma_blur, air, masks, px_per_long }
}

/// The airlight of the whole output frame, estimated on a small render of it (`proxy_w` px wide).
pub(crate) fn frame_airlight(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, proxy_w: usize, proxy_h: usize) -> f32 {
    let mut img = Rgb32f::clone(&frame_proxy(src, frame, proxy_w, proxy_h));
    white_balance(&mut img, info, s);
    let sigma = (0.02 * frame.px_per_long(proxy_w)).max(1.0) as f32;
    airlight(&gaussian(&img.map(dark_of), sigma))
}

const FRAME_PROXIES_KEPT: usize = 4;

fn frame_proxy(src: &Rgb32f, frame: &Frame, w: usize, h: usize) -> Arc<Rgb32f> {
    type Entry = (u64, Frame, usize, usize, Arc<Rgb32f>);
    static CACHE: std::sync::Mutex<Vec<Entry>> = std::sync::Mutex::new(Vec::new());
    let key = crate::optics::fingerprint(src);
    let found = |c: &[Entry]| c.iter().find(|e| e.0 == key && e.1 == *frame && (e.2, e.3) == (w, h)).map(|e| e.4.clone());
    if let Some(img) = found(&CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {
        return img;
    }
    let img = Arc::new(frame.sample(src, w, h));
    let mut c = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    c.push((key, frame.clone(), w, h, img.clone()));
    if c.len() > FRAME_PROXIES_KEPT {
        c.remove(0);
    }
    img
}

/// Airlight estimate: bright end of the dark channel.
pub fn airlight(dark: &Plane) -> f32 {
    craft_develop::local::airlight(&crate::bridge::to_craft_plane(dark))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nr_lum_keep_pins_the_contrast_curve() {
        // Contrast 0 leaves every residual in the blend.
        assert_eq!(nr_lum_keep(0.01, 0.0), 1.0);
        assert_eq!(nr_lum_keep(1.0, 0.0), 1.0);
        // Contrast 100 (weight 1): knee 0.04. A small residual is fully reduced; 0.08 stops is held.
        assert!((nr_lum_keep(0.01, 1.0) - 1.0).abs() < 1e-5);
        assert!(nr_lum_keep(0.08, 1.0).abs() < 1e-5);
        // Contrast 50 (weight 0.5): knee 0.14. Halfway to twice the knee is half the blend.
        assert!((nr_lum_keep(0.01, 0.5) - 1.0).abs() < 1e-5);
        assert!((nr_lum_keep(0.21, 0.5) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn nr_contrast_keeps_fine_texture() {
        let src = Rgb32f::from_fn(48, 16, |x, y| {
            let n = if (x + y) % 2 == 0 { 0.12 } else { -0.12 };
            [0.3 + n; 3]
        });
        let mut flat = DevelopSettings::default();
        flat.detail.nr_luminance = 100.0;
        let mut held = flat.clone();
        held.detail.nr_contrast = 100.0;
        let mut a = src.clone();
        let mut b = src.clone();
        denoise(&mut a, &flat, 1.0, None);
        denoise(&mut b, &held, 1.0, None);
        let spread = |img: &Rgb32f| {
            let row: Vec<f32> = (8..40).map(|x| img.get(x, 8)[0]).collect();
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            row.iter().map(|v| (v - mean).abs()).sum::<f32>()
        };
        assert!(spread(&b) > spread(&a) * 1.4, "contrast {} vs flat {}", spread(&b), spread(&a));
    }
}
