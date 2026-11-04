//! Print colour: a composed print page (16-bit RGB encoded with the BT.2020 / BT.709 curve, as the
//! `Rec2020` output space renders it) to the samples a print-ready file carries, in the colour space
//! of the chosen printer / paper profile.
//!
//! - **Target**: a printer / paper ICC profile (RGB, CMYK or grey; matrix or LUT based) converts the
//!   page to the device's own values. When the printer manages colour instead, the page is sent in a
//!   well-known RGB space (sRGB, Adobe RGB) and that space's profile travels with it.
//! - **Rendering intent**: perceptual or relative colorimetric, as for soft proofing ([`ProofIntent`]).
//! - **Black point compensation** (relative colorimetric only): the darkest colour the device holds is
//!   measured by a round trip of black through the profile, and the page's linear values are scaled
//!   so its black lands there instead of every shadow below it clipping to the same ink. Linear
//!   Rec.2020 is linear in XYZ, so with source black 0 and white 1 this is the published BPC mapping
//!   `Y' = Y·(1 − Yb) + Yb` for a neutral destination black. Perceptual tables carry their own black
//!   mapping and are left alone.
//! - **Print adjustment**: brightness and contrast (−100..100) for prints that come out darker or
//!   flatter than the screen, on the sRGB-encoded values with black and white held. It is not shown
//!   on screen (the print is what it corrects).
//!
//! Everything before the profile conversion is per channel, so it folds into one 65,536-entry table
//! indexed by the 16-bit sample; the conversion itself is moxcms's.
//!
//! Sources: ICC.1:2022 (rendering intents, profile classes); Adobe Systems, "Adobe Black Point
//! Compensation" (2006, public white paper) for the black point mapping. Own design otherwise.

use std::sync::Arc;

use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformExecutor, TransformOptions};

use crate::display::MAX_PROFILE_BYTES;
use crate::proof::ProofIntent;
use crate::space::{NamedSpace, Trc};

/// Where a print's colours go.
#[derive(Clone, Debug, PartialEq)]
pub enum PrintTarget {
    /// The printer manages colour: the page is sent in this RGB space, with its profile embedded.
    Managed(NamedSpace),
    /// A printer / paper ICC profile: the page is converted to the device's values.
    Profile(Arc<Vec<u8>>),
}

/// How the page is rendered into the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintColor {
    pub intent: ProofIntent,
    /// Map the page's black to the device's darkest (relative colorimetric only).
    pub black_point_compensation: bool,
    /// −100..100; 0 leaves the page as rendered.
    pub brightness: f32,
    /// −100..100; 0 leaves the page as rendered.
    pub contrast: f32,
}

impl Default for PrintColor {
    fn default() -> Self {
        PrintColor { intent: ProofIntent::Perceptual, black_point_compensation: true, brightness: 0.0, contrast: 0.0 }
    }
}

/// Pixels per conversion batch: bounds the temporary float buffers whatever the page size.
const BATCH: usize = 1 << 16;

/// The darkest black point compensation will lift the page's black to. A profile whose round trip
/// says more is broken or not a print profile; leave the page alone rather than wash it out.
const MAX_BLACK: f32 = 0.5;

type Executor = Arc<dyn TransformExecutor<f32> + Send + Sync>;

/// A page conversion, built once per print job and applied to every page.
pub struct PrintTransform {
    forward: Executor,
    layout: Layout,
    channels: usize,
    icc: Arc<Vec<u8>>,
    /// 16-bit sample → linear Rec.2020 after the print adjustment and black point compensation.
    pre: Vec<f32>,
    black: f32,
    description: Option<String>,
}

impl std::fmt::Debug for PrintTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintTransform")
            .field("channels", &self.channels)
            .field("black", &self.black)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok().flatten()
}

fn rendering_intent(intent: ProofIntent) -> RenderingIntent {
    match intent {
        ProofIntent::Perceptual => RenderingIntent::Perceptual,
        ProofIntent::Relative => RenderingIntent::RelativeColorimetric,
    }
}

/// A slider value as a finite fraction in −1..1 (NaN and infinities count as 0).
fn amount(v: f32) -> f32 {
    if v.is_finite() { (v / 100.0).clamp(-1.0, 1.0) } else { 0.0 }
}

/// The print adjustment on one sRGB-encoded value (0..1): brightness bends the mid-tones with a
/// power curve, contrast steepens (or flattens) them with `e − k·sin(2πe)/2π`, which keeps black,
/// mid grey and white where they are and stays monotonic for |k| < 1.
pub fn print_adjust(e: f32, brightness: f32, contrast: f32) -> f32 {
    let e = if e.is_finite() { e.clamp(0.0, 1.0) } else { 0.0 };
    let (b, c) = (amount(brightness), amount(contrast));
    let e = if b != 0.0 { e.powf(2f32.powf(-0.5 * b)) } else { e };
    let k = 0.6 * c;
    let tau = std::f32::consts::TAU;
    (e - k * (tau * e).sin() / tau).clamp(0.0, 1.0)
}

impl PrintTransform {
    /// Build the conversion for a target. Errors say what's wrong with the profile; nothing here
    /// panics on a hostile one.
    pub fn new(target: &PrintTarget, color: &PrintColor) -> Result<PrintTransform, String> {
        let icc = match target {
            PrintTarget::Managed(n) => Arc::new(crate::icc::write_named(*n)),
            PrintTarget::Profile(bytes) => Arc::clone(bytes),
        };
        if icc.len() > MAX_PROFILE_BYTES {
            return Err("the profile is too large".into());
        }
        let dev = guarded(|| ColorProfile::new_from_slice(&icc).ok()).ok_or("not a valid ICC profile")?;
        let (layout, channels) = match dev.color_space {
            DataColorSpace::Rgb => (Layout::Rgb, 3),
            DataColorSpace::Cmyk => (Layout::Rgba, 4),
            DataColorSpace::Gray => (Layout::Gray, 1),
            _ => return Err("only RGB, CMYK and grey printer profiles can be printed to".into()),
        };
        let lin = crate::icc::linear_rec2020_profile();
        let opts = |i| TransformOptions { rendering_intent: i, ..TransformOptions::default() };
        let forward: Executor = guarded(|| lin.create_transform_f32(Layout::Rgb, &dev, layout, opts(rendering_intent(color.intent))).ok())
            .ok_or("can't convert to this profile")?;

        let black = if color.black_point_compensation && color.intent == ProofIntent::Relative {
            let back: Executor = guarded(|| dev.create_transform_f32(layout, &lin, Layout::Rgb, opts(RenderingIntent::RelativeColorimetric)).ok())
                .ok_or("can't convert from this profile")?;
            device_black(&forward, &back, channels)
        } else {
            0.0
        };

        let decode = Trc::Rec709.lut16();
        let (b, c) = (color.brightness, color.contrast);
        let adjust = amount(b) != 0.0 || amount(c) != 0.0;
        let pre = decode
            .iter()
            .map(|&lin| {
                let lin = if adjust {
                    let e = lightcraft_color::transfer::linear_to_srgb(lin.clamp(0.0, 1.0));
                    lightcraft_color::transfer::srgb_to_linear(print_adjust(e, b, c))
                } else {
                    lin.clamp(0.0, 1.0)
                };
                lin * (1.0 - black) + black
            })
            .collect();
        let description = crate::icc::parse(&icc).and_then(|i| i.description);
        Ok(PrintTransform { forward, layout, channels, icc, pre, black, description })
    }

    /// Samples per pixel of the converted page: 3 (RGB), 4 (CMYK) or 1 (grey).
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The profile the converted samples are in: embed it with them.
    pub fn icc(&self) -> &Arc<Vec<u8>> {
        &self.icc
    }

    /// Whether the samples are CMYK (PDF readers show CMYK images inverted unless told otherwise,
    /// so writers need to know).
    pub fn is_cmyk(&self) -> bool {
        self.layout == Layout::Rgba
    }

    /// The linear black black point compensation lifts the page's black to (0 when off).
    pub fn black(&self) -> f32 {
        self.black
    }

    /// The profile's description, for the job's settings and error messages.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Convert a page: `rgb` is interleaved 16-bit RGB in the BT.2020 / BT.709 encoding; the result
    /// is interleaved 16-bit device samples ([`PrintTransform::channels`] per pixel).
    pub fn apply(&self, rgb: &[u16]) -> Result<Vec<u16>, String> {
        if !rgb.len().is_multiple_of(3) {
            return Err("the page isn't whole RGB pixels".into());
        }
        let pixels = rgb.len() / 3;
        let ch = self.channels;
        let out_len = pixels.checked_mul(ch).ok_or("the page is too large")?;
        let mut out = vec![0u16; out_len];
        let run = |src: &[u16], dst: &mut [u16]| -> Option<()> {
            let lin: Vec<f32> = src.iter().map(|&v| self.pre.get(usize::from(v)).copied().unwrap_or(0.0)).collect();
            let mut dev = vec![0f32; dst.len()];
            self.forward.transform(&lin, &mut dev).ok()?;
            for (d, v) in dst.iter_mut().zip(dev) {
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                *d = (v * 65535.0 + 0.5) as u16;
            }
            Some(())
        };
        let src = rgb.chunks(BATCH * 3);
        let dst = out.chunks_mut(BATCH * ch);
        #[cfg(feature = "parallel")]
        let ok = {
            use rayon::prelude::*;
            let pairs: Vec<_> = src.zip(dst).collect();
            pairs.into_par_iter().all(|(s, d)| guarded(|| run(s, d)).is_some())
        };
        #[cfg(not(feature = "parallel"))]
        let ok = src.zip(dst).all(|(s, d)| guarded(|| run(s, d)).is_some());
        if ok { Ok(out) } else { Err("the profile's tables can't be applied".into()) }
    }
}

/// The darkest the device holds, as linear Rec.2020 luminance: black there and back, relative
/// colorimetric both ways (for a device that reaches true black, 0).
fn device_black(forward: &Executor, back: &Executor, channels: usize) -> f32 {
    let mut device = vec![0f32; channels];
    let mut lin = [0f32; 3];
    let ok = guarded(|| {
        forward.transform(&[0.0; 3], &mut device).ok()?;
        back.transform(&device, &mut lin).ok()
    });
    if ok.is_none() {
        return 0.0;
    }
    // BT.2020 luminance weights
    let y = 0.2627 * lin[0] + 0.6780 * lin[1] + 0.0593 * lin[2];
    if y.is_finite() { y.clamp(0.0, MAX_BLACK) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moxcms::ToneReprCurve;

    /// A page of one 16-bit pixel per linear grey level.
    fn greys(levels: &[f32]) -> Vec<u16> {
        levels.iter().flat_map(|&l| [(Trc::Rec709.from_linear(l) * 65535.0).round() as u16; 3]).collect()
    }

    fn convert(target: &PrintTarget, color: PrintColor, levels: &[f32]) -> Vec<u16> {
        PrintTransform::new(target, &color).unwrap().apply(&greys(levels)).unwrap()
    }

    /// An RGB print profile whose ink can't get darker than linear 0.03: device 0 is 3 % grey.
    fn raised_black_profile() -> Arc<Vec<u8>> {
        let mut p = ColorProfile::new_from_slice(&crate::icc::write_named(NamedSpace::Srgb)).unwrap();
        let (g, e) = (2.2f32, 0.03f32);
        let a = (1.0 - e).powf(1.0 / g);
        // ICC parametric type 4: Y = (aX + b)^g + e for X >= d, Y = cX + f below
        let trc = ToneReprCurve::Parametric(vec![g, a, 0.0, 0.0, 0.0, e, e]);
        p.red_trc = Some(trc.clone());
        p.green_trc = Some(trc.clone());
        p.blue_trc = Some(trc);
        Arc::new(p.encode().unwrap())
    }

    #[test]
    fn managed_srgb_keeps_black_grey_and_white() {
        let t = PrintTransform::new(&PrintTarget::Managed(NamedSpace::Srgb), &PrintColor::default()).unwrap();
        assert_eq!(t.channels(), 3);
        assert_eq!(crate::icc::parse(t.icc()).and_then(|i| i.named), Some(NamedSpace::Srgb));
        let out = t.apply(&greys(&[0.0, 0.18, 1.0])).unwrap();
        let mid = (lightcraft_color::transfer::linear_to_srgb(0.18) * 65535.0) as i32;
        assert!(out[..3].iter().all(|&v| v < 64), "{out:?}");
        assert!(out[3..6].iter().all(|&v| (i32::from(v) - mid).abs() < 300), "{out:?} vs {mid}");
        assert!(out[6..].iter().all(|&v| v > 65400), "{out:?}");
    }

    #[test]
    fn adobe_rgb_target_embeds_its_profile() {
        let t = PrintTransform::new(&PrintTarget::Managed(NamedSpace::AdobeRgb), &PrintColor::default()).unwrap();
        assert_eq!(crate::icc::parse(t.icc()).and_then(|i| i.named), Some(NamedSpace::AdobeRgb));
        // a saturated Rec.2020 green is outside Adobe RGB: it comes out at the space's edge, not wrapped
        let px = [0u16, 65535, 0];
        let out = t.apply(&px).unwrap();
        assert!(out[1] > 60000 && out[0] < 8000 && out[2] < 8000, "{out:?}");
    }

    #[test]
    fn brightness_lifts_the_mid_tones_and_holds_the_ends() {
        let target = PrintTarget::Managed(NamedSpace::Srgb);
        let levels = [0.0, 0.18, 1.0];
        let plain = convert(&target, PrintColor::default(), &levels);
        let bright = convert(&target, PrintColor { brightness: 50.0, ..PrintColor::default() }, &levels);
        let dark = convert(&target, PrintColor { brightness: -50.0, ..PrintColor::default() }, &levels);
        assert!(bright[3] > plain[3] + 2000 && dark[3] + 2000 < plain[3], "{dark:?} {plain:?} {bright:?}");
        for v in [&plain, &bright, &dark] {
            assert!(v[0] < 64 && v[6] > 65400, "{v:?}");
        }
    }

    #[test]
    fn contrast_spreads_shadows_and_highlights_and_stays_monotonic() {
        let target = PrintTarget::Managed(NamedSpace::Srgb);
        let levels = [0.03, 0.6];
        let plain = convert(&target, PrintColor::default(), &levels);
        let more = convert(&target, PrintColor { contrast: 60.0, ..PrintColor::default() }, &levels);
        assert!(more[0] < plain[0] && more[3] > plain[3], "{plain:?} {more:?}");
        for c in [-100.0, 100.0] {
            for b in [-100.0, 100.0] {
                let ramp: Vec<f32> = (0..=64).map(|i| i as f32 / 64.0).map(|e| print_adjust(e, b, c)).collect();
                assert!(ramp.windows(2).all(|w| w[1] >= w[0]), "b {b} c {c}: {ramp:?}");
                assert!(ramp[0] == 0.0 && (ramp[64] - 1.0).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn black_point_compensation_keeps_shadow_detail_above_the_inks_black() {
        let target = PrintTarget::Profile(raised_black_profile());
        let shadows = [0.0, 0.006, 0.012, 0.018, 0.024, 0.5, 1.0];
        let relative = PrintColor { intent: ProofIntent::Relative, black_point_compensation: false, ..PrintColor::default() };
        let clipped = convert(&target, relative, &shadows);
        let kept = convert(&target, PrintColor { black_point_compensation: true, ..relative }, &shadows);
        let reds = |v: &[u16]| v.chunks(3).map(|p| p[0]).collect::<Vec<_>>();
        let (clipped, kept) = (reds(&clipped), reds(&kept));
        // everything below the ink's black prints as (within rounding) the same device value without
        // compensation …
        assert!(clipped[..5].iter().all(|&v| v <= clipped[0] + 16), "{clipped:?}");
        // … and as distinct, rising values with it; white still prints as white
        assert!(kept[..5].windows(2).all(|w| w[1] > w[0]) && kept[4] > kept[0] + 1000, "{kept:?}");
        assert!(kept[6] > 65400 && clipped[6] > 65400, "{kept:?} {clipped:?}");
        let t = PrintTransform::new(&target, &PrintColor { black_point_compensation: true, ..relative }).unwrap();
        assert!((t.black() - 0.03).abs() < 0.01, "{}", t.black());
    }

    #[test]
    fn black_point_compensation_is_relative_colorimetric_only() {
        let target = PrintTarget::Profile(raised_black_profile());
        let levels = [0.0, 0.01, 0.2];
        let with = convert(&target, PrintColor { intent: ProofIntent::Perceptual, black_point_compensation: true, ..PrintColor::default() }, &levels);
        let without =
            convert(&target, PrintColor { intent: ProofIntent::Perceptual, black_point_compensation: false, ..PrintColor::default() }, &levels);
        assert_eq!(with, without);
        let t = PrintTransform::new(&target, &PrintColor { intent: ProofIntent::Perceptual, ..PrintColor::default() }).unwrap();
        assert_eq!(t.black(), 0.0);
    }

    #[test]
    fn hostile_input_is_an_error_not_a_panic() {
        let color = PrintColor::default();
        assert!(PrintTransform::new(&PrintTarget::Profile(Arc::new(vec![])), &color).is_err());
        assert!(PrintTransform::new(&PrintTarget::Profile(Arc::new(vec![0u8; 400])), &color).is_err());
        assert!(PrintTransform::new(&PrintTarget::Profile(Arc::new(vec![0u8; MAX_PROFILE_BYTES + 1])), &color).is_err());
        let good = crate::icc::write_named(NamedSpace::DisplayP3);
        for i in (0..good.len()).step_by(7) {
            let mut b = good.clone();
            b[i] ^= 0x5a;
            if let Ok(t) = PrintTransform::new(&PrintTarget::Profile(Arc::new(b)), &PrintColor { intent: ProofIntent::Relative, ..color }) {
                let _ = t.apply(&greys(&[0.0, 0.5, 1.0]));
            }
        }
        let t = PrintTransform::new(&PrintTarget::Managed(NamedSpace::Srgb), &PrintColor { brightness: f32::NAN, contrast: f32::INFINITY, ..color })
            .unwrap();
        assert!(t.apply(&[1, 2]).is_err(), "half a pixel");
        assert_eq!(t.apply(&greys(&[0.18])).unwrap(), convert(&PrintTarget::Managed(NamedSpace::Srgb), color, &[0.18]));
        assert!(print_adjust(f32::NAN, 50.0, 50.0).is_finite());
    }
}
