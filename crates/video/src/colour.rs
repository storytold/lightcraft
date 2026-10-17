//! Stream colour description and Y'CbCr → linear Rec.709 RGB (HDR tone-mapped to SDR).

use filmcraft_color::{ColorInfo, Gamut, Matrix, Primaries, REFERENCE_WHITE_NITS, Range, ToneMap, Transfer};
use filmcraft_frame::VideoFrame;
use lcc_raster::Rgb32f;

/// Nominal peak of HLG displays / PQ masters without metadata, cd/m².
const HDR_PEAK_NITS: f64 = 1000.0;

/// H.273 colour code points from one source (the `colr` box or the VUI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Codes {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    /// None when the source doesn't say (QuickTime `nclc`).
    pub full_range: Option<bool>,
}

fn primaries_from_code(c: u16) -> Option<Primaries> {
    match c {
        1 => Some(Primaries::Bt709),
        5 => Some(Primaries::Bt601_625),
        6 | 7 => Some(Primaries::Bt601_525),
        9 => Some(Primaries::Bt2020),
        11 | 12 => Some(Primaries::P3D65),
        _ => None,
    }
}

pub fn apply(color: &mut ColorInfo, c: Codes) {
    let small = |v: u16| u8::try_from(v).ok();
    if let Some(p) = primaries_from_code(c.primaries) {
        color.primaries = p;
    }
    if let Some(t) = small(c.transfer).and_then(Transfer::from_code) {
        color.transfer = t;
    }
    if let Some(m) = small(c.matrix).and_then(Matrix::from_code) {
        color.matrix = m;
    }
    if let Some(full) = c.full_range {
        color.range = if full { Range::Full } else { Range::Limited };
    }
}

/// The colour of a stream: size-based defaults, overridden by the bitstream's VUI, overridden by
/// the container's `colr` box (unknown or "unspecified" code points keep the earlier value).
pub fn resolve(width: u32, height: u32, vui: Option<Codes>, colr: Option<Codes>) -> ColorInfo {
    let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(width, height), ..ColorInfo::REC709 };
    for c in [vui, colr].into_iter().flatten() {
        apply(&mut color, c);
    }
    color
}

pub fn is_hdr(c: &ColorInfo) -> bool {
    matches!(c.transfer, Transfer::Pq | Transfer::Hlg)
}

/// Decimation factor so the longer side is at most `max_side` (1 = full size).
pub fn decimation(width: u32, height: u32, max_side: Option<u32>) -> usize {
    let long = width.max(height).max(1);
    match max_side {
        Some(m) if m > 0 && long > m => long.div_ceil(m) as usize,
        _ => 1,
    }
}

/// Convert a decoded frame to linear Rec.709 RGB, box-decimated by `n`.
pub fn to_rgb(frame: &VideoFrame, n: usize) -> Rgb32f {
    let color = frame.color;
    // premultiplied linear RGBA in the source primaries; alpha is 1 for camera video
    let (w, h, rgba) = frame.to_linear_f32_decimated(n);
    let src = Gamut::from_primaries(color.primaries);
    let gamut: Option<[[f32; 3]; 3]> = (src != Gamut::Bt709).then(|| {
        let m = filmcraft_color::spaces::gamut_matrix(src, Gamut::Bt709);
        m.map(|row| row.map(|v| v as f32))
    });
    let white = REFERENCE_WHITE_NITS as f32;
    let (kr, kb) = Matrix::Bt2020Ncl.kr_kb();
    let tone = is_hdr(&color).then(|| ToneMap::new(HDR_PEAK_NITS, REFERENCE_WHITE_NITS));
    let mut data = Vec::with_capacity(w.saturating_mul(h));
    for px in rgba.as_chunks::<4>().0 {
        let mut c = [px[0], px[1], px[2]];
        match color.transfer {
            // filmcraft decodes PQ with 1.0 = 100 cd/m²; working units have 1.0 = reference white
            Transfer::Pq => c = c.map(|v| v * 100.0 / white),
            // HLG decodes to scene light; BT.2100 OOTF (γ 1.2 on a 1000 cd/m² display)
            Transfer::Hlg => {
                let ys = (kr * c[0] + (1.0 - kr - kb) * c[1] + kb * c[2]).max(0.0);
                let k = HDR_PEAK_NITS as f32 * ys.powf(0.2) / white;
                c = c.map(|v| v * k);
            }
            _ => {}
        }
        if let Some(m) = &gamut {
            c = filmcraft_color::spaces::apply3(m, c);
        }
        c = c.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
        if let Some(t) = &tone {
            c = t.apply(c);
        }
        data.push(c);
    }
    data.resize(w.saturating_mul(h), [0.0; 3]);
    Rgb32f { width: w, height: h, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colr_wins_then_vui_then_defaults() {
        let d = resolve(640, 480, None, None);
        assert_eq!((d.matrix, d.range), (Matrix::Bt601, Range::Limited));
        let hd = resolve(1920, 1080, None, None);
        assert_eq!(hd.matrix, Matrix::Bt709);
        let vui = Codes { primaries: 9, transfer: 18, matrix: 9, full_range: Some(true) };
        let c = resolve(1920, 1080, Some(vui), None);
        assert_eq!((c.primaries, c.transfer, c.matrix, c.range), (Primaries::Bt2020, Transfer::Hlg, Matrix::Bt2020Ncl, Range::Full));
        let colr = Codes { primaries: 1, transfer: 1, matrix: 1, full_range: None };
        let c = resolve(1920, 1080, Some(vui), Some(colr));
        assert_eq!((c.primaries, c.transfer, c.matrix, c.range), (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709, Range::Full));
        // unspecified (2) and out-of-range codes keep the earlier value
        let junk = Codes { primaries: 2, transfer: 999, matrix: 65535, full_range: None };
        assert_eq!(resolve(1920, 1080, Some(vui), Some(junk)).transfer, Transfer::Hlg);
    }

    #[test]
    fn hdr_white_lands_near_sdr_white() {
        // HLG reference white is 75 % signal; it should map close to 1.0 after the OOTF + tone map
        let mut f = VideoFrame::rgba8(1, 1, vec![191, 191, 191, 255]);
        f.color = ColorInfo { transfer: Transfer::Hlg, primaries: Primaries::Bt2020, matrix: Matrix::Bt2020Ncl, range: Range::Full };
        let img = to_rgb(&f, 1);
        let v = img.data[0];
        assert!(v.iter().all(|c| (0.6..=1.05).contains(c)), "{v:?}");
    }
}
