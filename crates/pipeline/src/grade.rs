//! Video grading: one clip's [`DevelopSettings`] applied to each decoded frame.
//!
//! Frames take the display-referred input path that JPEG/TIFF photos use (linear-light
//! [`Rgb32f`], a non-raw [`SourceInfo`]): no raw stages, white balance as relative multipliers
//! around the as-shot look. Only global controls apply to video ([`video_settings`]); masks,
//! spots, lens corrections, noise reduction, crop and transforms are dropped.
//!
//! Sources: own design (reuses [`crate::render`]).

use crate::{Quality, RenderRequest, SourceInfo};
use lcc_develop::DevelopSettings;
use lcc_raster::{Rgb32f, Rgba8};

/// The widest frame a draft (playback) grade renders.
pub const DRAFT_MAX_WIDTH: usize = 960;

/// The controls video honours, everything else at its default: white balance (relative),
/// Light (exposure, contrast, highlights, shadows, whites, blacks), tone curve, colour
/// (vibrance, saturation, HSL), colour mixer and B&W mix, colour grading, profile and treatment.
/// Section on/off toggles are kept. Dropped: effects (texture, clarity, dehaze), vignette, grain,
/// detail (sharpening, noise reduction), optics, geometry, crop, orientation, masks, spots, red
/// eye, lens blur, enhance, point colour, calibration and HDR.
pub fn video_settings(s: &DevelopSettings) -> DevelopSettings {
    DevelopSettings {
        version: s.version,
        process: s.process,
        profile: s.profile.clone(),
        treatment: s.treatment,
        wb: s.wb,
        light: s.light,
        curve: s.curve.clone(),
        color: s.color,
        mixer: s.mixer,
        bw_mix: s.bw_mix,
        grading: s.grading,
        disabled_sections: s.disabled_sections.clone(),
        ..DevelopSettings::default()
    }
}

/// What a clip keeps of `s`: [`video_settings`] plus its crop and flips ([`video_framing`]).
/// Posters, thumbnails and the stored settings use it; frames are graded by `video_settings`
/// and framed by `video_framing` separately.
pub fn video_develop(s: &DevelopSettings) -> DevelopSettings {
    DevelopSettings { crop: s.crop, ..video_settings(s) }
}

/// The part of `s` that frames a video clip: the crop (with its straighten angle and aspect) and
/// the flips. Everything else (orientation, lens, Upright) stays photo-only.
pub fn video_framing(s: &DevelopSettings) -> DevelopSettings {
    DevelopSettings { crop: s.crop, ..DevelopSettings::default() }
}

/// Whether [`video_framing`] changes the frame at all (a full-frame, unflipped crop does not).
pub fn video_is_framed(s: &DevelopSettings) -> bool {
    let d = DevelopSettings::default();
    s.crop.geometry != d.crop.geometry || s.crop.flip_h || s.crop.flip_v
}

/// Draw a decoded `frame` into a `w × h` output under the clip's [`video_framing`].
pub fn frame_video(frame: &Rgb32f, framing: &DevelopSettings, w: usize, h: usize) -> Rgb32f {
    crate::geometry::Frame::new(frame.width, frame.height, framing, true).sample(frame, w, h)
}

/// The source facts a video frame is graded against: display-referred, relative white balance.
pub fn video_source_info() -> SourceInfo {
    SourceInfo::default()
}

/// The render request for a `w × h` frame: [`Quality::Draft`] fits [`DRAFT_MAX_WIDTH`] (never
/// upscaling), [`Quality::Full`] keeps the frame's size.
pub fn grade_request(w: usize, h: usize, quality: Quality) -> RenderRequest {
    let (w, h) = (w.max(1), h.max(1));
    let (mw, mh) = match quality {
        Quality::Full => (w, h),
        Quality::Draft if w > DRAFT_MAX_WIDTH => {
            let mh = (h as u64 * DRAFT_MAX_WIDTH as u64).div_ceil(w as u64).max(1) as usize;
            (DRAFT_MAX_WIDTH, mh)
        }
        Quality::Draft => (w, h),
    };
    RenderRequest { quality, ..RenderRequest::fit(mw, mh) }
}

/// Grade one frame on the CPU: `settings` reduced to [`video_settings`], rendered sRGB 8-bit.
pub fn grade_frame(img: &Rgb32f, settings: &DevelopSettings, quality: Quality) -> Rgba8 {
    let s = video_settings(settings);
    crate::render(img, &video_source_info(), &s, &grade_request(img.width, img.height, quality)).image
}

/// [`grade_frame`] keeping float precision: the graded frame as *linear* Rec.709 / sRGB-primaries
/// RGB (0..1, [`crate::OutputDepth::F32Linear`]), the same type and convention as the input. For
/// export, where an 8-bit round trip would band 10-bit ProRes. `None` when the render produced no
/// float samples of the frame's size (never expected).
pub fn grade_frame_linear(img: &Rgb32f, settings: &DevelopSettings, quality: Quality) -> Option<Rgb32f> {
    let s = video_settings(settings);
    let req =
        RenderRequest { depth: crate::OutputDepth::F32Linear, space: crate::OutputSpace::Srgb, ..grade_request(img.width, img.height, quality) };
    let deep = crate::render(img, &video_source_info(), &s, &req).deep?;
    let crate::DeepSamples::F32(v) = deep.samples else { return None };
    let (chunks, _) = v.as_chunks::<3>();
    (chunks.len() == deep.width.checked_mul(deep.height)?).then(|| Rgb32f { width: deep.width, height: deep.height, data: chunks.to_vec() })
}

/// Lattice points per axis of a baked grade ([`GradeLut`]).
pub const LUT_SIZE: usize = 33;

/// A clip's grade baked into a 3D LUT for playback: the per-pixel global ops of
/// [`video_settings`] evaluated on a [`LUT_SIZE`]³ lattice and looked up with tetrahedral
/// interpolation. Highlights / Shadows are evaluated point-wise (their edge-aware base taken as
/// the pixel's own luminance, exact on flat areas), so a LUT grade approximates the real one
/// where those are used; texture, clarity and dehaze never enter the LUT path (they are stripped
/// for video). Use the real pipeline ([`grade_frame`]) on pause and export.
///
/// The lattice spans display-encoded (sRGB curve) values of the linear working-space input,
/// 0..1; brighter inputs clamp. Sources: own design; tetrahedral interpolation after
/// Kasson, Nin, Plouffe & Hafner, "Performing color space conversions with three-dimensional
/// linear interpolation" (J. Electronic Imaging, 1995).
#[derive(Clone, Debug, PartialEq)]
pub struct GradeLut {
    /// Display-encoded sRGB output (0..1) per lattice point, red fastest, then green, then blue.
    data: Vec<[f32; 3]>,
    /// The input shaper: linear 0..1 → lattice coordinate, sampled at [`SHAPER_SIZE`] points.
    shaper: Vec<f32>,
}

/// Entries of the [`GradeLut`] input shaper (linear → sRGB-curve lattice coordinate).
const SHAPER_SIZE: usize = 16384;

impl GradeLut {
    /// Bake `settings` (reduced to [`video_settings`]).
    pub fn bake(settings: &DevelopSettings) -> Result<GradeLut, String> {
        use lcc_color::transfer::srgb_to_linear;
        let n = LUT_SIZE;
        let v = video_settings(settings);
        let effective = v.effective();
        let req = RenderRequest::fit(n * n, n);
        let rendition = crate::settings_for(&effective, &req);
        let s: &DevelopSettings = &rendition;
        let at = |i: usize| srgb_to_linear(i as f32 / (n - 1) as f32);
        let lattice = Rgb32f::from_fn(n * n, n, |x, y| [at(x % n), at(x / n), at(y)]);
        let info = video_source_info();
        let plan = crate::plan(&lattice, &info, s, &req);
        if (plan.w, plan.h) != (n * n, n) {
            return Err(format!("grade LUT: lattice planned at {}×{}", plan.w, plan.h));
        }
        let mut lin = lattice;
        crate::lin_cpu(&mut lin, &info, &plan);
        let log_l = std::sync::Arc::new(lin.map(crate::local::log_lum));
        let prep = crate::Prepared {
            img: std::sync::Arc::new(lin),
            log_l: log_l.clone(),
            base: log_l,
            clarity_blur: None,
            texture_blur: None,
            sharpen_blur: None,
            dark: None,
            chroma_blur: None,
            air: 1.0,
            masks: Vec::new(),
            px_per_long: plan.px_per_long,
        };
        // (a grading LUT for video: no plug-in filters)
        let none = crate::plugin_filters::Stack::empty();
        let mut warnings = Vec::new();
        let post = crate::finish::Post { stack: &none, frame: Default::default(), warnings: std::cell::RefCell::new(&mut warnings) };
        let out = crate::finish::finish(&prep, &plan.settings, &plan.frame, &info, crate::OutputSpace::Srgb, None, None, &post);
        if out.data.len() != n * n * n {
            return Err("grade LUT: unexpected output size".into());
        }
        let data = out.data.iter().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]).collect();
        let scale = (n - 1) as f32;
        let shaper = (0..SHAPER_SIZE).map(|i| lcc_color::transfer::linear_to_srgb(i as f32 / (SHAPER_SIZE - 1) as f32) * scale).collect();
        Ok(GradeLut { data, shaper })
    }

    /// The lattice: display-encoded sRGB output (0..1) per point, red fastest, then green, then
    /// blue ([`LUT_SIZE`]³ entries). For a GPU twin of [`GradeLut::lookup`], whose input
    /// coordinate is the sRGB curve of the clamped linear input times `LUT_SIZE - 1`.
    pub fn table(&self) -> &[[f32; 3]] {
        &self.data
    }

    /// Look up one linear working-space colour: display-encoded sRGB, 0..1.
    pub fn lookup(&self, c: [f32; 3]) -> [f32; 3] {
        let n = LUT_SIZE;
        let top = (SHAPER_SIZE - 1) as f32;
        let mut i = [0usize; 3];
        let mut f = [0f32; 3];
        for k in 0..3 {
            let v = if c[k].is_finite() { c[k].clamp(0.0, 1.0) } else { 0.0 };
            let x = self.shaper.get((v * top + 0.5) as usize).copied().unwrap_or(0.0);
            let lo = (x.floor() as usize).min(n - 2);
            i[k] = lo;
            f[k] = x - lo as f32;
        }
        let get = |dr: usize, dg: usize, db: usize| -> [f32; 3] {
            self.data.get((i[0] + dr) + n * ((i[1] + dg) + n * (i[2] + db))).copied().unwrap_or([0.0; 3])
        };
        let (fr, fg, fb) = (f[0], f[1], f[2]);
        // the tetrahedron of the unit cube holding (fr, fg, fb): walk its vertices from 000 to 111
        let (a, b, c2, w) = if fr >= fg {
            if fg >= fb {
                (get(1, 0, 0), get(1, 1, 0), get(1, 1, 1), [1.0 - fr, fr - fg, fg - fb, fb])
            } else if fr >= fb {
                (get(1, 0, 0), get(1, 0, 1), get(1, 1, 1), [1.0 - fr, fr - fb, fb - fg, fg])
            } else {
                (get(0, 0, 1), get(1, 0, 1), get(1, 1, 1), [1.0 - fb, fb - fr, fr - fg, fg])
            }
        } else if fb >= fg {
            (get(0, 0, 1), get(0, 1, 1), get(1, 1, 1), [1.0 - fb, fb - fg, fg - fr, fr])
        } else if fb >= fr {
            (get(0, 1, 0), get(0, 1, 1), get(1, 1, 1), [1.0 - fg, fg - fb, fb - fr, fr])
        } else {
            (get(0, 1, 0), get(1, 1, 0), get(1, 1, 1), [1.0 - fg, fg - fr, fr - fb, fb])
        };
        let o = get(0, 0, 0);
        let mut out = [0f32; 3];
        for k in 0..3 {
            out[k] = w[0] * o[k] + w[1] * a[k] + w[2] * b[k] + w[3] * c2[k];
        }
        out
    }

    /// Grade a frame through the LUT at the size [`grade_request`] gives for `quality`.
    pub fn apply(&self, img: &Rgb32f, quality: Quality) -> Rgba8 {
        let req = grade_request(img.width, img.height, quality);
        let s = DevelopSettings::default();
        let plan = crate::plan(img, &video_source_info(), &s, &req);
        let small;
        let src = if (plan.w, plan.h) == (img.width, img.height) {
            img
        } else {
            small = plan.frame.sample(img, plan.w, plan.h);
            &small
        };
        src.map(|c| {
            let o = self.lookup(c);
            let q = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            [q(o[0]), q(o[1]), q(o[2]), 255]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_develop::{Mask, Spot};

    fn frame() -> Rgb32f {
        lcc_scenes::demo_library()[0].render(320, 180)
    }

    fn allowed() -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.light.exposure = 0.4;
        s.light.contrast = 20.0;
        s.light.highlights = -30.0;
        s.light.shadows = 25.0;
        s.light.whites = 10.0;
        s.light.blacks = -5.0;
        s.wb.temp = 7200.0;
        s.wb.tint = 8.0;
        s.color.vibrance = 15.0;
        s.color.saturation = -10.0;
        s.curve.lights = 12.0;
        s.grading.shadows.hue = 220.0;
        s.grading.shadows.sat = 30.0;
        s
    }

    #[test]
    fn graded_frame_matches_photo_pipeline() {
        let img = frame();
        let s = allowed();
        let photo = crate::render(&img, &SourceInfo::default(), &s, &RenderRequest::fit(320, 180)).image;
        assert_eq!(grade_frame(&img, &s, Quality::Full), photo);
    }

    #[test]
    fn disallowed_controls_are_stripped() {
        let mut s = allowed();
        s.effects.clarity = 50.0;
        s.effects.dehaze = 30.0;
        s.vignette.amount = -40.0;
        s.grain.amount = 50.0;
        s.detail.sharpen_amount = 100.0;
        s.crop.flip_h = true;
        s.masks.push(Mask::default());
        s.spots.push(Spot::default());
        let v = video_settings(&s);
        assert_eq!(v, video_settings(&allowed()));
        assert_eq!(v.light, allowed().light);
        assert!(v.masks.is_empty() && v.spots.is_empty());
        let img = frame();
        assert_eq!(grade_frame(&img, &s, Quality::Full), grade_frame(&img, &allowed(), Quality::Full));
    }

    /// CIE76 ΔE between two sRGB 8-bit colours (D65).
    fn delta_e(a: [u8; 4], b: [u8; 4]) -> f64 {
        let lab = |p: [u8; 4]| {
            let l = |v: u8| lcc_color::transfer::srgb_to_linear(v as f32 / 255.0) as f64;
            let (r, g, b) = (l(p[0]), l(p[1]), l(p[2]));
            let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
            let f = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
            let (fx, fy, fz) = (f(x), f(y), f(z));
            [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
        };
        let (p, q) = (lab(a), lab(b));
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
    }

    /// (mean, 99th percentile) ΔE of the LUT grade against the real one.
    fn lut_error(s: &DevelopSettings) -> (f64, f64) {
        let img = lcc_scenes::demo_library()[0].render(480, 270);
        let lut = GradeLut::bake(s).unwrap();
        let a = lut.apply(&img, Quality::Full);
        let b = grade_frame(&img, s, Quality::Full);
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut d: Vec<f64> = a.data.iter().zip(&b.data).map(|(x, y)| delta_e(*x, *y)).collect();
        d.sort_by(f64::total_cmp);
        (d.iter().sum::<f64>() / d.len() as f64, d[d.len() * 99 / 100])
    }

    #[test]
    fn lut_matches_pipeline_for_pointwise_controls() {
        let mut s = allowed();
        s.light.highlights = 0.0;
        s.light.shadows = 0.0;
        let (mean, p99) = lut_error(&s);
        eprintln!("LUT ΔE (point-wise controls): mean {mean:.2}, p99 {p99:.2}");
        assert!(mean < 1.0 && p99 < 3.0, "mean {mean}, p99 {p99}");
    }

    #[test]
    fn lut_identity_is_near_exact() {
        let (mean, p99) = lut_error(&DevelopSettings::default());
        assert!(mean < 1.0 && p99 < 3.0, "mean {mean}, p99 {p99}");
    }

    #[test]
    fn lut_lookup_survives_hostile_input() {
        let lut = GradeLut::bake(&allowed()).unwrap();
        for c in [[f32::NAN; 3], [f32::INFINITY, -1.0, 1e30], [-0.0, 0.0, 2.0]] {
            assert!(lut.lookup(c).iter().all(|v| v.is_finite()));
        }
    }
}
