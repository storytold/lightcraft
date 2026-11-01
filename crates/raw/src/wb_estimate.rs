//! As-shot white balance for raws that record none we can read: measured from the file's own embedded preview.
//!
//! Sources: own design. The camera rendered its preview with the as-shot white balance, so pixels the preview shows
//! as neutral grey were neutral for that white balance. The mosaic is averaged per colour over the preview's pixel
//! grid (inside the default crop); over the preview pixels whose channels lie within a few codes of each other
//! (not near black, not near clipping, no clipped sensor sample under them) the median green-over-red and
//! green-over-blue ratios are the gains. A colour matrix maps neutral to neutral, so its absence doesn't bias the
//! result. Without enough neutral pixels the mean of the unclipped mosaic is used instead (grey world).
use crate::{BlackLevel, Cfa, Rect};

/// Preview pixels whose largest and smallest channel differ by less than this (8-bit codes) count as neutral.
const NEUTRAL_SPREAD: u8 = 12;
/// At least this many neutral preview pixels are needed.
const MIN_NEUTRAL: usize = 64;
/// Previews larger than this (pixels) are not decoded for the estimate.
const MAX_PREVIEW_PIXELS: usize = 1 << 24;

/// The raw frame the estimate looks at.
pub(crate) struct Frame<'a> {
    pub data: &'a [u16],
    pub width: usize,
    pub crop: Rect,
    pub cfa: &'a Cfa,
    pub black: &'a BlackLevel,
    pub white: f32,
}

/// Gains `[R/G, 1, B/G]`: from the neutral pixels of the embedded preview, else grey world. `None` for an empty or
/// degenerate frame.
pub(crate) fn as_shot(bytes: &[u8], f: &Frame) -> Option<[f32; 3]> {
    from_preview(bytes, f).or_else(|| grey_world(f))
}

/// Per preview-grid cell means of R, G, B (black subtracted) and whether a clipped sample lies in the cell.
fn cell_means(f: &Frame, pw: usize, ph: usize) -> Option<Vec<([f64; 3], bool)>> {
    let c = f.crop;
    if pw == 0 || ph == 0 || c.width < pw || c.height < ph || f.width == 0 {
        return None;
    }
    let mut cells = vec![([0f64; 3], [0u32; 3], false); pw.checked_mul(ph)?];
    for y in c.y..c.y.saturating_add(c.height) {
        let row = f.data.get(y.checked_mul(f.width)?..y.checked_mul(f.width)?.checked_add(f.width)?)?;
        let cy = (y - c.y) * ph / c.height;
        for x in c.x..c.x.saturating_add(c.width).min(f.width) {
            let v = f32::from(*row.get(x)?);
            let cx = (x - c.x) * pw / c.width;
            let col = usize::from(f.cfa.color_at(x, y)).min(2);
            let cell = cells.get_mut(cy * pw + cx)?;
            if v >= f.white * 0.98 {
                cell.2 = true;
            }
            cell.0[col] += f64::from(v - f.black.at(x, y, 0, 1));
            cell.1[col] += 1;
        }
    }
    Some(cells.into_iter().map(|(s, n, clip)| ([0, 1, 2].map(|k| if n[k] > 0 { s[k] / f64::from(n[k]) } else { 0.0 }), clip)).collect())
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

fn from_preview(bytes: &[u8], f: &Frame) -> Option<[f32; 3]> {
    let jpeg = crate::preview::embedded_preview(bytes)?;
    let (px, pw, ph, ch) = crate::tiffraw::lossy_jpeg(&jpeg, 3, crate::tiffraw::MAX_JPEG_SAMPLES).ok()?;
    if ch != 3 {
        return None;
    }
    from_rgb(&px, pw, ph, f)
}

/// Gains from an 8-bit RGB preview `pw` x `ph` (rows of RGB triplets) showing the crop: its neutral pixels, else
/// grey world. For raws whose preview is an uncompressed RGB image rather than a JPEG.
pub(crate) fn as_shot_rgb(px: &[u8], pw: usize, ph: usize, f: &Frame) -> Option<[f32; 3]> {
    from_rgb(px, pw, ph, f).or_else(|| grey_world(f))
}

fn from_rgb(px: &[u8], pw: usize, ph: usize, f: &Frame) -> Option<[f32; 3]> {
    if pw.checked_mul(ph)? > MAX_PREVIEW_PIXELS || px.len() < pw * ph * 3 {
        return None;
    }
    // the preview must show the crop: aspect ratios within 3%
    let (ra, pa) = (f.crop.width as f64 / f.crop.height.max(1) as f64, pw as f64 / ph as f64);
    if (ra / pa - 1.0).abs() > 0.03 {
        return None;
    }
    let cells = cell_means(f, pw, ph)?;
    let (mut gr, mut gb) = (Vec::new(), Vec::new());
    for (i, (m, clip)) in cells.iter().enumerate() {
        let p = px.get(i * 3..i * 3 + 3)?;
        let (lo, hi) = (p.iter().min().copied().unwrap_or(0), p.iter().max().copied().unwrap_or(255));
        if *clip || lo < 30 || hi > 240 || hi - lo >= NEUTRAL_SPREAD || m.iter().any(|v| *v <= 1.0) {
            continue;
        }
        gr.push(m[1] / m[0]);
        gb.push(m[1] / m[2]);
    }
    if gr.len() < MIN_NEUTRAL {
        return None;
    }
    gains(median(&mut gr)?, median(&mut gb)?)
}

fn gains(gr: f64, gb: f64) -> Option<[f32; 3]> {
    let ok = |g: f64| g.is_finite() && (0.1..=10.0).contains(&g);
    (ok(gr) && ok(gb)).then_some([gr as f32, 1.0, gb as f32])
}

fn grey_world(f: &Frame) -> Option<[f32; 3]> {
    let c = f.crop;
    let (mut s, mut n) = ([0f64; 3], [0u64; 3]);
    for y in c.y..c.y.saturating_add(c.height) {
        for x in c.x..c.x.saturating_add(c.width).min(f.width) {
            let Some(&v) = f.data.get(y.checked_mul(f.width)?.checked_add(x)?) else { continue };
            let v = f32::from(v);
            if v >= f.white * 0.98 {
                continue;
            }
            let k = usize::from(f.cfa.color_at(x, y)).min(2);
            s[k] += f64::from((v - f.black.at(x, y, 0, 1)).max(0.0));
            n[k] += 1;
        }
    }
    if n.contains(&0) {
        return None;
    }
    let m = [0, 1, 2].map(|k| s[k] / n[k] as f64);
    if m.iter().any(|v| *v <= 0.0) {
        return None;
    }
    gains(m[1] / m[0], m[1] / m[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_world_balances_a_tinted_flat_frame() {
        // RGGB, R at 400, G at 800, B at 200 above a black of 100
        let (w, h) = (8, 8);
        let d: Vec<u16> = (0..w * h).map(|i| [500, 900, 900, 300][(i / w % 2) * 2 + i % 2]).collect();
        let cfa = Cfa::bayer_static("RGGB");
        let black = BlackLevel::uniform(100.0);
        let f = Frame { data: &d, width: w, crop: Rect::new(0, 0, w, h), cfa: &cfa, black: &black, white: 4095.0 };
        assert_eq!(as_shot(b"not a raw", &f), Some([2.0, 1.0, 4.0]));
        // degenerate: everything clipped
        let d = vec![4095u16; w * h];
        let f = Frame { data: &d, ..f };
        assert_eq!(as_shot(b"", &f), None);
        // a crop outside the data never panics
        let f = Frame { crop: Rect::new(4, 4, 100, 100), ..f };
        let _ = as_shot(b"", &f);
    }
}
