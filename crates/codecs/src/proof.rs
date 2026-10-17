//! Soft proofing against an output-device ICC profile (a printer/paper profile, RGB or CMYK, matrix
//! or LUT based): a 3D table that takes a colour in the pipeline's working space (linear Rec.2020)
//! through the device and back, so the loupe shows what the print can hold.
//!
//! - Forward with the chosen intent (perceptual or relative colorimetric), back with relative
//!   colorimetric: colours outside the device gamut come back mapped into it.
//! - **Simulate Paper & Ink**: absolute colorimetric rendering of the way back. The paper's white
//!   (the profile's media white point) replaces the screen's white, and the ink's black is the
//!   darkest the device tables give (no black-point compensation). The white is applied as a
//!   per-channel scale in linear Rec.2020 (a von Kries adaptation in RGB, close enough for the
//!   near-neutral whites of papers).
//! - Each entry carries a gamut flag: how far the relative-colorimetric round trip moved the colour.
//!
//! Sources: ICC.1:2022 (rendering intents, media white point, absolute colorimetric scaling);
//! the transforms are moxcms's. Own design otherwise.

use lcc_color::{D50, D65, REC2020, bradford};
use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformOptions};

use crate::display::MAX_PROFILE_BYTES;

/// Rendering intent of the forward (image → device) conversion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProofIntent {
    #[default]
    Perceptual,
    Relative,
}

impl ProofIntent {
    pub fn parse(s: &str) -> Option<ProofIntent> {
        match s.to_ascii_lowercase().as_str() {
            "perceptual" => Some(ProofIntent::Perceptual),
            "relative" | "relativecolorimetric" => Some(ProofIntent::Relative),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ProofIntent::Perceptual => "perceptual",
            ProofIntent::Relative => "relative",
        }
    }
}

/// A proof table: `n³` entries over working-space RGB encoded with the sRGB curve (index
/// `(r * n + g) * n + b`), each the simulated linear Rec.2020 colour plus a gamut distance
/// (`[3]`, 0 = in gamut; colours whose round trip moved them more than ~0.02 are outside).
#[derive(Clone, Debug, PartialEq)]
pub struct ProofTable {
    pub n: usize,
    pub data: Vec<[f32; 4]>,
    pub description: Option<String>,
}

/// Grid size of [`ProofTable`]s.
pub const PROOF_GRID: usize = 33;

fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok().flatten()
}

/// Build the proof table for an output profile. Errors say what's wrong with the file; nothing here
/// panics on a hostile profile.
pub fn proof_table(bytes: &[u8], intent: ProofIntent, paper_ink: bool) -> Result<ProofTable, String> {
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err("the profile is too large".into());
    }
    let dev = guarded(|| ColorProfile::new_from_slice(bytes).ok()).ok_or("not a valid ICC profile")?;
    let (layout, ch) = match dev.color_space {
        DataColorSpace::Rgb => (Layout::Rgb, 3),
        DataColorSpace::Cmyk => (Layout::Rgba, 4),
        DataColorSpace::Gray => (Layout::Gray, 1),
        _ => return Err("only RGB, CMYK and grey output profiles can be proofed".into()),
    };
    let lin = crate::icc::linear_rec2020_profile();
    let opts = |i| TransformOptions { rendering_intent: i, ..TransformOptions::default() };
    let fwd_i = match intent {
        ProofIntent::Perceptual => RenderingIntent::Perceptual,
        ProofIntent::Relative => RenderingIntent::RelativeColorimetric,
    };
    let rel = RenderingIntent::RelativeColorimetric;
    let fwd = guarded(|| lin.create_transform_f32(Layout::Rgb, &dev, layout, opts(fwd_i)).ok()).ok_or("can't convert to this profile")?;
    let fwd_rel = guarded(|| lin.create_transform_f32(Layout::Rgb, &dev, layout, opts(rel)).ok()).ok_or("can't convert to this profile")?;
    let back = guarded(|| dev.create_transform_f32(layout, &lin, Layout::Rgb, opts(rel)).ok()).ok_or("can't convert from this profile")?;

    let n = PROOF_GRID;
    let grid: Vec<f32> = (0..n * n * n)
        .flat_map(|i| [i / (n * n), (i / n) % n, i % n].map(|k| lcc_color::transfer::srgb_to_linear(k as f32 / (n - 1) as f32)))
        .collect();
    let round_trip = |there: &dyn moxcms::TransformExecutor<f32>| -> Option<Vec<f32>> {
        let mut device = vec![0f32; n * n * n * ch];
        there.transform(&grid, &mut device).ok()?;
        let mut out = vec![0f32; n * n * n * 3];
        back.transform(&device, &mut out).ok()?;
        Some(out)
    };
    let sim = guarded(|| round_trip(fwd.as_ref())).ok_or("the profile's tables can't be applied")?;
    let colorimetric = guarded(|| round_trip(fwd_rel.as_ref())).ok_or("the profile's tables can't be applied")?;
    let paper = if paper_ink { paper_white(&dev) } else { [1.0; 3] };
    let enc = |v: f32| lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
    let data = (0..n * n * n)
        .map(|i| {
            let c = |v: &[f32], k: usize| v.get(i * 3 + k).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
            let dist = (0..3).map(|k| (enc(c(&colorimetric, k)) - enc(c(&grid, k))).abs()).fold(0.0f32, f32::max);
            [c(&sim, 0) * paper[0], c(&sim, 1) * paper[1], c(&sim, 2) * paper[2], dist]
        })
        .collect();
    let description = crate::icc::parse(bytes).and_then(|i| i.description);
    Ok(ProofTable { n, data, description })
}

/// The paper's white in linear Rec.2020 (screen white = (1, 1, 1)), from the media white point.
fn paper_white(p: &ColorProfile) -> [f32; 3] {
    let Some(w) = p.media_white_point else { return [1.0; 3] };
    let to_xyz = bradford(D65, D50).mul(&REC2020.to_xyz());
    let Some(from_xyz) = to_xyz.inverse() else { return [1.0; 3] };
    // the PCS white is D50 = Rec.2020 (1, 1, 1) after adaptation; the paper is relative to it
    let d50 = to_xyz.apply([1.0; 3]);
    let s = [w.x / d50[0], w.y / d50[1], w.z / d50[2]];
    let rgb = from_xyz.apply([d50[0] * s[0], d50[1] * s[1], d50[2] * s[2]]);
    if rgb.iter().all(|v| v.is_finite() && *v > 0.05 && *v <= 1.5) { rgb.map(|v| (v as f32).min(1.0)) } else { [1.0; 3] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::NamedSpace;

    fn at(t: &ProofTable, rgb: [usize; 3]) -> [f32; 4] {
        t.data[(rgb[0] * t.n + rgb[1]) * t.n + rgb[2]]
    }

    #[test]
    fn srgb_proof_flags_wide_colours_and_keeps_grey() {
        let t = proof_table(&crate::icc::write_named(NamedSpace::Srgb), ProofIntent::Relative, false).unwrap();
        let top = t.n - 1;
        // Rec.2020 pure green is far outside sRGB; mid grey and white are inside and unchanged
        assert!(at(&t, [0, top, 0])[3] > 0.05, "{:?}", at(&t, [0, top, 0]));
        let g = at(&t, [16, 16, 16]);
        assert!(g[3] < 0.01, "{g:?}");
        let lin = lcc_color::transfer::srgb_to_linear(16.0 / 32.0);
        assert!((0..3).all(|k| (g[k] - lin).abs() < 0.01), "{g:?} vs {lin}");
        let w = at(&t, [top; 3]);
        assert!((0..3).all(|k| (w[k] - 1.0).abs() < 0.01), "{w:?}");
    }

    #[test]
    fn paper_white_dims_and_tints_white() {
        let mut p = ColorProfile::new_from_slice(&crate::icc::write_named(NamedSpace::AdobeRgb)).unwrap();
        // a warm, slightly dull paper
        p.media_white_point = Some(moxcms::Xyzd { x: 0.9642 * 0.92, y: 0.90, z: 0.8249 * 0.80 });
        let bytes = p.encode().unwrap();
        let top = PROOF_GRID - 1;
        let plain = proof_table(&bytes, ProofIntent::Perceptual, false).unwrap();
        let paper = proof_table(&bytes, ProofIntent::Perceptual, true).unwrap();
        let (w0, w1) = (at(&plain, [top; 3]), at(&paper, [top; 3]));
        assert!((0..3).all(|k| (w0[k] - 1.0).abs() < 0.01), "{w0:?}");
        assert!(w1[1] < 0.95 && w1[2] < w1[0], "paper white is dimmer and warmer: {w1:?}");
    }

    #[test]
    fn hostile_profiles_are_errors() {
        assert!(proof_table(&[], ProofIntent::Perceptual, false).is_err());
        assert!(proof_table(&[0u8; 400], ProofIntent::Relative, true).is_err());
        let good = crate::icc::write_named(NamedSpace::DisplayP3);
        for i in (0..good.len()).step_by(11) {
            let mut b = good.clone();
            b[i] ^= 0x5a;
            let _ = proof_table(&b, ProofIntent::Perceptual, true);
        }
    }
}
