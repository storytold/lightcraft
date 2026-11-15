//! ICC profiles: parsing (via `craft-cms`), matrix/TRC extraction, CMS fallback for LUT/CMYK profiles,
//! and writing matrix/TRC profiles for export embedding.

use crate::space::{NamedSpace, Trc, rgb_to_xyz_d50};
use craft_cms::{Builtin, ColorSpace, Curve, Intent, Pcs, Profile, Transform, TransformOptions};
use lightcraft_color::{D50, Mat3, RgbSpace, bradford};
use serde::{Deserialize, Serialize};

/// Colour model of the profile's device space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IccColorModel {
    Rgb,
    Gray,
    Cmyk,
    Other,
}

/// How a profile can be applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum IccKind {
    /// RGB matrix/TRC (shaper) profile: exact, applied by us.
    MatrixTrc { to_xyz_d50: Mat3, trc: [Trc; 3] },
    /// Grayscale profile with a `kTRC`.
    Gray { trc: Trc },
    /// LUT-based (A2B), CMYK or otherwise needs a full CMS conversion.
    NeedsCms,
}

/// Summary of a parsed ICC profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IccInfo {
    pub model: IccColorModel,
    pub kind: IccKind,
    /// Recognised well-known RGB space (by colorants), if any.
    pub named: Option<NamedSpace>,
    pub description: Option<String>,
}

/// Parse an ICC profile. Returns `None` for malformed or unsupported data (never panics).
pub fn parse(bytes: &[u8]) -> Option<IccInfo> {
    let p = Profile::parse(bytes).ok()?;
    Some(info_of(&p))
}

fn info_of(p: &Profile) -> IccInfo {
    let desc = p.description.trim_end_matches('\0').trim().to_string();
    let description = if desc.is_empty() { None } else { Some(desc) };
    let model = match p.color_space {
        ColorSpace::Rgb => IccColorModel::Rgb,
        ColorSpace::Gray => IccColorModel::Gray,
        ColorSpace::Cmyk => IccColorModel::Cmyk,
        _ => IccColorModel::Other,
    };
    let has_lut = p.a2b[0].is_some() || p.a2b[1].is_some();
    let kind = if model == IccColorModel::Rgb && p.is_matrix_shaper() && p.pcs == Pcs::Xyz {
        let to_xyz_d50 = Mat3(p.matrix.unwrap_or([[0.0; 3]; 3]));
        let trc =
            p.trc.as_ref().map(|ts| [trc_from_icc(&ts[0]), trc_from_icc(&ts[1]), trc_from_icc(&ts[2])]).unwrap_or([Trc::Srgb, Trc::Srgb, Trc::Srgb]);
        if to_xyz_d50.determinant().abs() > 1e-6 && trc.iter().all(valid_trc) { IccKind::MatrixTrc { to_xyz_d50, trc } } else { IccKind::NeedsCms }
    } else if model == IccColorModel::Gray && p.gray_trc.is_some() && !has_lut {
        let trc = p.gray_trc.as_ref().map(trc_from_icc).unwrap_or(Trc::Srgb);
        if valid_trc(&trc) { IccKind::Gray { trc } } else { IccKind::NeedsCms }
    } else {
        IccKind::NeedsCms
    };
    let named = match &kind {
        IccKind::MatrixTrc { to_xyz_d50, .. } => NamedSpace::recognize(to_xyz_d50),
        _ => None,
    };
    IccInfo { model, kind, named, description }
}

fn valid_trc(t: &Trc) -> bool {
    match t {
        Trc::Parametric(p) => matches!(p.len(), 1 | 3 | 4 | 5 | 7) && p.iter().all(|v| v.is_finite()),
        Trc::Gamma(g) => g.is_finite() && *g > 0.0,
        _ => true,
    }
}

fn trc_from_icc(t: &Curve) -> Trc {
    match t {
        Curve::Identity => Trc::Linear,
        Curve::Gamma(g) => Trc::Gamma(*g as f32),
        Curve::Parametric { kind, p } => Trc::Parametric(p[..Curve::param_count(*kind)].iter().map(|&v| v as f32).collect()),
        Curve::Table(v) if v.is_empty() => Trc::Linear,
        Curve::Table(v) if v.len() == 1 => Trc::Gamma(v[0]),
        Curve::Table(v) => Trc::Table(v.iter().map(|&x| (x.clamp(0.0, 1.0) * 65535.0).round() as u16).collect()),
    }
}

fn trc_to_icc(t: &Trc) -> Curve {
    match t {
        Trc::Linear => Curve::Identity,
        Trc::Srgb => Curve::Parametric { kind: 3, p: [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045, 0.0, 0.0] },
        Trc::Gamma(g) => Curve::Gamma(*g as f64),
        Trc::Rec709 => Curve::Parametric { kind: 3, p: [1.0 / 0.45, 1.0 / 1.099, 0.099 / 1.099, 1.0 / 4.5, 0.081, 0.0, 0.0] },
        Trc::Parametric(p) => {
            let kind = match p.len() {
                1 => 0,
                3 => 1,
                4 => 2,
                5 => 3,
                _ => 4,
            };
            let mut arr = [0.0f64; 7];
            for (dst, &src) in arr.iter_mut().zip(p.iter()) {
                *dst = src as f64;
            }
            Curve::Parametric { kind, p: arr }
        }
        Trc::Table(v) => Curve::Table(v.iter().map(|&x| x as f32 / 65535.0).collect()),
    }
}

/// ICC PCS illuminant (D50) as stored in profiles.
const ICC_D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// Build an ICC v4 display-class matrix/TRC profile for `space` with the given encoding curve
/// (D50 PCS, Bradford `chad`), for embedding in exported files.
pub fn write_matrix_trc(space: &RgbSpace, trc: &Trc) -> Vec<u8> {
    let mut p = Builtin::Srgb.profile().clone();
    p.matrix = Some(rgb_to_xyz_d50(space).0);
    let curve = trc_to_icc(trc);
    p.trc = Some([curve.clone(), curve.clone(), curve]);
    p.white_point = ICC_D50;
    p.chad = Some(bradford(space.white, D50).0);
    p.rendering_intent = Intent::Perceptual;
    p.description = match trc {
        t if t.is_linear() => format!("{} (linear)", space.name),
        _ => space.name.to_string(),
    };
    p.copyright = "No copyright, use freely".to_string();
    let encoded = p.with_encoded_bytes();
    align_tags(&fixed_date(encoded.to_bytes().to_vec()))
}

/// Zero the header's creation date/time (ICC.1 §7.2.8, bytes 24..36) so a profile's bytes depend
/// only on its colour space and curve. `moxcms` stamps the wall clock there on every `encode`
/// (ignoring `ColorProfile::creation_date_time`), which made two exports of one photo differ in
/// the embedded profile and nowhere else, so output hashes could not be compared (#180). Readers
/// do not interpret the field; all zeros is the conventional "unspecified" value.
fn fixed_date(mut b: Vec<u8>) -> Vec<u8> {
    if let Some(date) = b.get_mut(24..36) {
        date.fill(0);
    }
    b
}

/// Re-lay a profile's tag data on 4-byte boundaries (ICC.1 §7.3.1). The encoder packs tags back to
/// back, so a tag of odd length (e.g. a `mluc` description with an odd number of characters)
/// misaligns every following tag — which some readers (macOS ImageIO for PNG `iCCP`) reject.
fn align_tags(b: &[u8]) -> Vec<u8> {
    let be = |i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let Some(n) = be(128) else { return b.to_vec() };
    let table_end = 132 + 12 * n;
    let tags: Option<Vec<(usize, usize)>> = (0..n).map(|t| Some((be(132 + 12 * t + 4)?, be(132 + 12 * t + 8)?))).collect();
    let Some(tags) = tags.filter(|t| table_end <= b.len() && t.iter().all(|&(o, l)| o >= table_end && o + l <= b.len())) else {
        return b.to_vec();
    };
    let mut out = b[..table_end].to_vec();
    let mut placed: Vec<((usize, usize), usize)> = Vec::new();
    for (t, &(off, len)) in tags.iter().enumerate() {
        let new_off = match placed.iter().find(|(k, _)| *k == (off, len)) {
            // tags sharing data keep sharing it
            Some(&(_, o)) => o,
            None => {
                while !out.len().is_multiple_of(4) {
                    out.push(0);
                }
                let o = out.len();
                out.extend_from_slice(&b[off..off + len]);
                placed.push(((off, len), o));
                o
            }
        };
        out[132 + 12 * t + 4..132 + 12 * t + 8].copy_from_slice(&(new_off as u32).to_be_bytes());
    }
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    // the profile ID (MD5) would no longer match: clear it (= "not computed")
    out[84..100].fill(0);
    out
}

/// A profile for one of the well-known spaces with its standard curve.
pub fn write_named(n: NamedSpace) -> Vec<u8> {
    write_matrix_trc(&n.rgb_space(), &n.trc())
}

pub(crate) fn linear_rec2020_profile() -> Profile {
    let mut p = Builtin::Rec2020.profile().clone();
    p.trc = Some([Curve::Identity, Curve::Identity, Curve::Identity]);
    p
}

/// Convert device samples (0..1, interleaved, `channels` = 1, 3 or 4 matching the profile's colour
/// model) to linear Rec.2020 through a full CMS transform. `None` if the profile cannot be applied.
pub(crate) fn cms_to_linear_rec2020(icc: &[u8], samples: &[f32], channels: usize) -> Option<Vec<[f32; 3]>> {
    let src = Profile::parse(icc).ok()?;
    let expected = match src.color_space {
        ColorSpace::Rgb => 3,
        ColorSpace::Gray => 1,
        ColorSpace::Cmyk => 4,
        _ => return None,
    };
    if channels != expected {
        return None;
    }
    let dst = linear_rec2020_profile();
    let opts = TransformOptions { intent: Intent::RelativeColorimetric, bpc: false, precise_float: true };
    let t = Transform::with_options(&src, &dst, opts).ok()?;
    let n = samples.len() / channels;
    let mut out = vec![0f32; n * 3];
    const ROWS: usize = 1 << 16;
    let src_chunks = samples.chunks(ROWS * channels);
    let dst_chunks = out.chunks_mut(ROWS * 3);
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        let pairs: Vec<_> = src_chunks.zip(dst_chunks).collect();
        let ok = pairs.into_par_iter().all(|(s, d)| t.convert_f32(s, channels, d, 3, false).is_ok());
        if !ok {
            return None;
        }
    }
    #[cfg(not(feature = "parallel"))]
    for (s, d) in src_chunks.zip(dst_chunks) {
        t.convert_f32(s, channels, d, 3, false).ok()?;
    }
    Some(out.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_color::{DISPLAY_P3, PROPHOTO, SRGB};

    #[test]
    fn write_then_parse_recognises() {
        for n in NamedSpace::ALL {
            let bytes = write_named(n);
            assert!(bytes.len() > 128, "{n:?}");
            let info = parse(&bytes).expect("parses");
            assert_eq!(info.named, Some(n), "{n:?}");
            match info.kind {
                IccKind::MatrixTrc { trc, .. } => assert!(trc[0].approx_eq(&n.trc(), 2e-4), "{n:?} {:?}", trc[0]),
                k => panic!("{k:?}"),
            }
        }
    }

    #[test]
    fn tags_are_aligned() {
        for n in NamedSpace::ALL {
            for trc in [n.trc(), Trc::Linear] {
                let b = write_matrix_trc(&n.rgb_space(), &trc);
                assert_eq!(b.len() % 4, 0, "{n:?}");
                assert_eq!(u32::from_be_bytes(b[0..4].try_into().unwrap()) as usize, b.len());
                let count = u32::from_be_bytes(b[128..132].try_into().unwrap()) as usize;
                for t in 0..count {
                    let off = u32::from_be_bytes(b[132 + 12 * t + 4..132 + 12 * t + 8].try_into().unwrap());
                    assert_eq!(off % 4, 0, "{n:?} tag {t}");
                }
                assert!(parse(&b).is_some_and(|i| i.named == Some(n)), "{n:?}");
            }
        }
    }

    #[test]
    fn linear_profile() {
        let bytes = write_matrix_trc(&PROPHOTO, &Trc::Linear);
        let info = parse(&bytes).unwrap();
        assert_eq!(info.named, Some(NamedSpace::ProPhoto));
        let IccKind::MatrixTrc { trc, .. } = info.kind else { panic!() };
        assert!(trc[1].is_linear());
    }

    #[test]
    fn cms_path_matches_matrix_path() {
        let bytes = write_named(NamedSpace::DisplayP3);
        let px = [0.2f32, 0.5, 0.8];
        let out = cms_to_linear_rec2020(&bytes, &px, 3).unwrap()[0];
        let lin = px.map(|v| Trc::Srgb.to_linear(v));
        let m = DISPLAY_P3.to_space(&lightcraft_color::REC2020);
        let want = m.apply_f32(lin);
        for i in 0..3 {
            assert!((out[i] - want[i]).abs() < 2e-3, "{out:?} vs {want:?}");
        }
        let _ = SRGB;
    }

    /// Two profiles for one space are byte-identical, and the header date is fixed: exports of the
    /// same photo with the same settings hash the same (#180).
    #[test]
    fn written_profiles_are_deterministic() {
        for n in NamedSpace::ALL {
            let a = write_named(n);
            let b = write_named(n);
            assert_eq!(a, b, "{n:?}");
            assert_eq!(&a[24..36], &[0u8; 12], "{n:?} header date/time");
            // the profile ID is "not computed", not a hash of a timestamp
            assert_eq!(&a[84..100], &[0u8; 16], "{n:?} profile ID");
        }
        let lin = write_matrix_trc(&PROPHOTO, &Trc::Linear);
        assert_eq!(lin, write_matrix_trc(&PROPHOTO, &Trc::Linear));
        assert_eq!(&lin[24..36], &[0u8; 12]);
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 200]).is_none());
    }
}
