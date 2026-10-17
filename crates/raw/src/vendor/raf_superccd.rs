//! Fujifilm RAF from the FinePix bodies before the X series (SuperCCD HR / SR / SR Pro), whose raw block is a
//! bare sample array instead of a TIFF with a raw IFD.
//!
//! Sources: the ExifTool FujiFilm tag-name documentation (RAF header records `0x0100` RawImageFullSize, `0x0121`
//! RawImageSize, `0x0130` FujiLayout, `0x2ff0` WB_GRGBLevels) and our own black-box analysis of the CC0 samples on
//! raw.pixls.us (E550, E900, F700, S2Pro, S3Pro, S5Pro, S5000, S5200, S5600, S6000fd, S6500fd, S7000, S9500,
//! S9600); the layout was cross-read in prose from published decoder descriptions, no code copied (fork-only per the
//! raw-decoder reference policy):
//!
//! - The strip is exactly `RawImageFullSize` (rows × columns) 16-bit words: little-endian 14-bit samples, except
//!   the S2Pro (big-endian 12-bit). The byte order is chosen from the data (the reading with the smaller median).
//! - SuperCCD photosites sit on a lattice turned 45°. `FujiLayout` byte 0 bit 7 picks one of two storage orders;
//!   byte 1 bit 3 clear means the data is on that rotated lattice (set: an ordinary upright Bayer array).
//!   - Layout 0: each stored row holds `2 × fw` sites (`fw` = RawImageSize width / 2); site (row, col) lies at
//!     (r, c) = (fw − 1 + row − col/2, row + (col + 1)/2) of a square grid.
//!   - Layout 1: stored rows come in pairs that interleave along one diagonal; each holds `fw` sites and
//!     (row, col) lies at (r, c) = (fw − 1 − col + row/2, col + (row + 1)/2); RawImageSize height counts row pairs.
//!   - The used area is centred in the stored one, its offsets rounded down to even (keeps the CFA phase).
//! - On that grid the colours form a Bayer mosaic, GBRG at (0, 0) (`fw` is even on every sample), found by
//!   correlating each phase's chromaticity with the camera JPEG. The S2Pro stores a used area of 2880 × 2144 in
//!   layout 0 turned a quarter, so its image is rotated 90° clockwise for display.
//! - The grid is demosaiced and resampled on the 45° diagonals (bilinear, √½ pixel steps) to an upright camera-RGB
//!   image of `fw·√2 × (grid − fw)·√2` pixels.
//! - S3Pro / S5Pro (SR Pro) store the low-sensitivity "R" photosites as a second frame after the first; the first
//!   (the "S" photosites) is the image. SR bodies that interleave S and R rows in one frame (F700, S20Pro) are refused.

use super::raf::Header;
use super::white_from_data;
use crate::demosaic::{Method, demosaic};
use crate::{BlackLevel, Cfa, ColorData, Mode, Normalized, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lcc_geom::Orientation;
use rayon::prelude::*;

const FULL_SIZE: u16 = 0x0100;
const IMAGE_SIZE: u16 = 0x0121;
const LAYOUT: u16 = 0x0130;
const WB_GRGB: u16 = 0x2ff0;

fn pair(h: &Header, tag: u16) -> Option<(usize, usize)> {
    let d = h.records.iter().find(|r| r.0 == tag).map(|r| r.1).filter(|d| d.len() >= 4)?;
    Some((u16::from_be_bytes([d[0], d[1]]) as usize, u16::from_be_bytes([d[2], d[3]]) as usize))
}

/// Whether `h` describes a pre-X SuperCCD file (sizes and layout records, no raw IFD).
pub(crate) fn applies(h: &Header) -> bool {
    pair(h, FULL_SIZE).is_some() && h.records.iter().any(|r| r.0 == LAYOUT)
}

/// The storage geometry of the used area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    layout1: bool,
    /// Sites per diagonal of the grid (half the RawImageSize width).
    fw: usize,
    rows: usize,
    cols: usize,
    top: usize,
    left: usize,
    /// Side of the square grid holding every site.
    grid: usize,
}

impl Geometry {
    fn new(layout1: bool, full: (usize, usize), image: (usize, usize)) -> Result<Geometry> {
        let (rh, rw) = full;
        let (ih, iw) = image;
        let fw = iw / 2;
        let (rows, cols) = if layout1 { (ih.saturating_mul(2), fw) } else { (ih, iw) };
        if fw < 2 || rows < 2 || rows > rh || cols > rw {
            return Err(RawError::Corrupt(format!("SuperCCD RAF: {iw}x{ih} image in a {rw}x{rh} strip")));
        }
        let (top, left) = (((rh - rows) >> 2) << 1, ((rw - cols) >> 2) << 1);
        // r ≤ fw − 1 + (rows − 1) and c ≤ (rows − 1) + fw (layout 0); r, c ≤ fw + rows / 2 (layout 1)
        let grid = if layout1 { fw + rows / 2 + 1 } else { fw + rows };
        Ok(Geometry { layout1, fw, rows, cols, top, left, grid })
    }

    /// Grid position of stored site (row, col) of the used area.
    #[inline]
    fn site(&self, row: usize, col: usize) -> (usize, usize) {
        if self.layout1 { (self.fw - 1 - col + (row >> 1), col + ((row + 1) >> 1)) } else { (self.fw - 1 + row - (col >> 1), row + ((col + 1) >> 1)) }
    }

    /// Output size: the diamond's inscribed upright rectangle.
    fn output(&self) -> (usize, usize) {
        let s = std::f64::consts::SQRT_2;
        ((self.fw as f64 * s) as usize, ((self.grid - 1).saturating_sub(self.fw) as f64 * s) as usize)
    }
}

/// Read `n` 16-bit samples in the byte order whose middle samples have the smaller median.
fn samples(src: &[u8]) -> Vec<u16> {
    let words = src.as_chunks::<2>().0;
    let median = |le: bool| {
        let mid = words.len() / 2;
        let mut v: Vec<u16> = words
            .iter()
            .skip(mid.saturating_sub(32_768))
            .take(65_536)
            .map(|w| if le { u16::from_le_bytes(*w) } else { u16::from_be_bytes(*w) })
            .collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied().unwrap_or(0)
    };
    let le = median(true) <= median(false);
    words.par_iter().map(|w| if le { u16::from_le_bytes(*w) } else { u16::from_be_bytes(*w) }).collect()
}

pub(crate) fn decode(h: &Header, raw: &[u8], camera: &str, mode: Mode) -> Result<RawImage> {
    let full = pair(h, FULL_SIZE).ok_or_else(|| RawError::Corrupt("RAF without RawImageFullSize".into()))?;
    let layout = h.records.iter().find(|r| r.0 == LAYOUT).map(|r| r.1).unwrap_or_default();
    let (b0, b1) = (layout.first().copied().unwrap_or(0), layout.get(1).copied().unwrap_or(0));
    if b1 & 8 != 0 {
        return Err(RawError::Unsupported("Fujifilm RAF: upright pre-X layout without a raw IFD".into()));
    }
    let s2pro = camera.replace(' ', "").eq_ignore_ascii_case("FinePixS2Pro");
    // the S2Pro's RawImageSize describes the output, not the stored area
    let image = if s2pro { (2144, 2880) } else { pair(h, IMAGE_SIZE).ok_or_else(|| RawError::Corrupt("RAF without RawImageSize".into()))? };
    let g = Geometry::new(b0 & 0x80 != 0, full, image)?;
    let n = full.0.checked_mul(full.1).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let cells = g.grid.checked_mul(g.grid).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let src =
        raw.get(..n * 2).ok_or_else(|| RawError::Unsupported(format!("Fujifilm RAF: strip of {} bytes for {}x{}", raw.len(), full.1, full.0)))?;
    let px = samples(src);
    let used = |row: usize, col: usize| px.get((row + g.top) * full.1 + col + g.left).copied().unwrap_or(0);
    let dual = dual_rows(&g, &used);
    let (ow, oh) = if dual.is_some() { (g.cols, g.rows / 2) } else { g.output() };
    if ow < 8 || oh < 8 {
        return Err(RawError::Corrupt("SuperCCD RAF: empty image".into()));
    }

    let mut data = Vec::new();
    // 14-bit samples on every body but the S2Pro (12-bit)
    let bits = if (0..g.rows).any(|r| (0..g.cols).any(|c| used(r, c) > 4095)) { 14 } else { 12 };
    if let (Mode::Full, Some(s_row)) = (mode, dual) {
        let s_px: Vec<u16> = (0..oh).flat_map(|k| (0..ow).map(move |j| (k, j))).map(|(k, j)| used(2 * k + s_row, j)).collect();
        let white = white_from_data(&s_px, bits).max(1.0);
        data = sr_upright(&s_px, ow, oh, white);
    } else if mode == Mode::Full {
        let used_px: Vec<u16> = (0..g.rows).flat_map(|r| (0..g.cols).map(move |c| (r, c))).map(|(r, c)| used(r, c)).collect();
        let white = white_from_data(&used_px, bits).max(1.0);
        let mut grid = vec![0f32; cells];
        for row in 0..g.rows {
            for col in 0..g.cols {
                let (r, c) = g.site(row, col);
                if let (Some(cell), Some(&v)) = (grid.get_mut(r * g.grid + c), used_px.get(row * g.cols + col)) {
                    *cell = f32::from(v) / white;
                }
            }
        }
        drop(used_px);
        let mosaic = Normalized {
            width: g.grid,
            height: g.grid,
            cpp: 1,
            data: grid,
            cfa: Some(Cfa::bayer_static(if g.fw % 2 == 0 { "GBRG" } else { "RGGB" })),
        };
        let rgb = demosaic(&mosaic, Method::Ppg);
        drop(mosaic);
        data = rotate(&rgb, g.fw as f64, ow, oh);
    }

    let wb = pair(h, WB_GRGB).and_then(|(gl, r)| {
        let d = h.records.iter().find(|x| x.0 == WB_GRGB).map(|x| x.1)?;
        let b = u16::from_be_bytes([*d.get(6)?, *d.get(7)?]);
        (gl > 0 && r > 0 && b > 0).then(|| [r as f32 / gl as f32, 1.0, f32::from(b) / gl as f32])
    });
    let mut metadata = h.jpeg.map(lcc_meta::extract).unwrap_or_default();
    // keep the full output as the image; trim the resampled edge where the diamond's corners blur
    let active = Rect::new(2, 2, ow - 4, oh - 4);
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let orientation = if s2pro { Orientation::from_exif(6) } else { metadata.orientation.unwrap_or(Orientation::Normal) };
    let img = RawImage {
        format: RawFormat::Raf,
        width: ow,
        height: oh,
        cpp: 3,
        data: RawData::U16(data),
        cfa: None,
        bits,
        black: BlackLevel::uniform(0.0),
        white: vec![65535.0],
        active_area: active,
        crop: Rect::new(0, 0, active.width, active.height),
        orientation,
        color: ColorData::default(),
        wb_multipliers: wb,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
        pixel_aspect: 1.0,
    };
    img.validate_for(mode)?;
    Ok(img)
}

/// SR bodies interleave sensitive (S) and small (R) photosites by stored row, the R rows several stops lower: the
/// parity of the S rows, or `None` for a single-sensitivity lattice.
fn dual_rows(g: &Geometry, used: &impl Fn(usize, usize) -> u16) -> Option<usize> {
    let (mut even, mut odd) = (0u64, 0u64);
    for row in (g.rows / 4..3 * g.rows / 4).step_by(2) {
        for col in (g.cols / 4..3 * g.cols / 4).step_by(3) {
            even += u64::from(used(row, col));
            odd += u64::from(used(row + 1, col));
        }
    }
    if even > 4 * odd.max(1) {
        Some(0)
    } else if odd > 4 * even.max(1) {
        Some(1)
    } else {
        None
    }
}

/// Colour of S photosite (row k, column j) of an SR body: every other S row is green, the rows between alternate
/// red and blue with the phase flipping every second one (measured against the camera JPEG on the F700).
fn sr_colour(k: usize, j: usize) -> usize {
    if k & 1 == 1 {
        1
    } else if ((k >> 1) ^ j) & 1 == 0 {
        0
    } else {
        2
    }
}

/// The S photosites of an SR body form an upright lattice (each S row is one image row): fill the two missing colours
/// of each with the mean of that colour in its 3 × 3 neighbourhood, scaled so `white` maps to 65535.
fn sr_upright(s: &[u16], w: usize, h: usize, white: f32) -> Vec<u16> {
    let mut out = vec![0u16; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(k, line)| {
        for (j, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let (mut sum, mut n) = ([0f32; 3], [0f32; 3]);
            for kk in k.saturating_sub(1)..(k + 2).min(h) {
                for jj in j.saturating_sub(1)..(j + 2).min(w) {
                    let c = sr_colour(kk, jj);
                    sum[c] += f32::from(s.get(kk * w + jj).copied().unwrap_or(0));
                    n[c] += 1.0;
                }
            }
            let own = sr_colour(k, j);
            for c in 0..3 {
                let v = if c == own { f32::from(s.get(k * w + j).copied().unwrap_or(0)) } else { sum[c] / n[c].max(1.0) };
                px[c] = (v / white).clamp(0.0, 1.0).mul_add(65535.0, 0.5) as u16;
            }
        }
    });
    out
}

/// Resample the demosaiced grid on its diagonals: output (x, y) reads grid (r, c) = (fw + (y − x)·√½, (y + x)·√½).
fn rotate(rgb: &crate::Rgb32f, fw: f64, ow: usize, oh: usize) -> Vec<u16> {
    let step = std::f64::consts::FRAC_1_SQRT_2;
    let (gw, gh) = (rgb.width, rgb.height);
    let at = |r: usize, c: usize| rgb.data.get(r * gw + c).copied().unwrap_or([0.0; 3]);
    let mut out = vec![0u16; ow * oh * 3];
    out.par_chunks_mut(ow * 3).enumerate().for_each(|(y, line)| {
        for (x, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let r = fw + (y as f64 - x as f64) * step;
            let c = (y as f64 + x as f64) * step;
            let (r0, c0) = (r.floor().clamp(0.0, (gh.saturating_sub(2)) as f64), c.floor().clamp(0.0, (gw.saturating_sub(2)) as f64));
            let (fr, fc) = ((r - r0).clamp(0.0, 1.0) as f32, (c - c0).clamp(0.0, 1.0) as f32);
            let (r0, c0) = (r0 as usize, c0 as usize);
            let (a, b, d, e) = (at(r0, c0), at(r0, c0 + 1), at(r0 + 1, c0), at(r0 + 1, c0 + 1));
            for k in 0..3 {
                let top = a[k] + (b[k] - a[k]) * fc;
                let bottom = d[k] + (e[k] - d[k]) * fc;
                let v = top + (bottom - top) * fr;
                px[k] = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_fill_the_grid_once() {
        for layout1 in [false, true] {
            let image = if layout1 { (6, 16) } else { (12, 16) };
            let full = if layout1 { (12, 8) } else { (12, 16) };
            let g = Geometry::new(layout1, full, image).unwrap();
            let mut seen = vec![0u8; g.grid * g.grid];
            for row in 0..g.rows {
                for col in 0..g.cols {
                    let (r, c) = g.site(row, col);
                    assert!(r < g.grid && c < g.grid, "{layout1}: ({row}, {col}) -> ({r}, {c}) outside {}", g.grid);
                    seen[r * g.grid + c] += 1;
                }
            }
            assert!(seen.iter().all(|&n| n <= 1), "{layout1}: a grid cell holds two sites");
            assert_eq!(seen.iter().filter(|&&n| n == 1).count(), g.rows * g.cols);
        }
    }

    #[test]
    fn hostile_sizes_are_errors() {
        assert!(Geometry::new(false, (10, 10), (20, 20)).is_err());
        assert!(Geometry::new(true, (0, 0), (0, 0)).is_err());
        assert!(Geometry::new(true, (4, 1), (2, 2)).is_err());
    }

    fn header<'a>(records: Vec<(u16, &'a [u8])>, raw: &'a [u8]) -> Header<'a> {
        Header { jpeg: None, records, raw: Some(raw) }
    }

    /// A flat grey lattice in either layout turns into a flat upright RGB image; short strips, missing records and
    /// absurd sizes are errors, never panics.
    #[test]
    fn synthetic_lattices_decode_flat_and_hostile_ones_fail() {
        let wb: &[u8] = &[0, 80, 0, 160, 0, 80, 0, 120];
        for (layout, full, image) in [([0x01u8, 0x02], [0u8, 64, 0, 72], [0u8, 64, 0, 64]), ([0x80, 0x81], [0, 128, 0, 36], [0, 64, 0, 64])] {
            let n = usize::from(full[1]) * usize::from(full[3]);
            let strip: Vec<u8> = (0..n).flat_map(|_| 1000u16.to_le_bytes()).collect();
            let h = header(vec![(FULL_SIZE, &full), (IMAGE_SIZE, &image), (LAYOUT, &layout), (WB_GRGB, wb)], &strip);
            let img = decode(&h, &strip, "FinePix Test", Mode::Full).unwrap();
            assert_eq!((img.cpp, img.wb_multipliers), (3, Some([2.0, 1.0, 1.5])));
            let RawData::U16(d) = &img.data else { panic!() };
            let a = img.active_area;
            let mid = ((a.y + a.height / 2) * img.width + a.x + a.width / 2) * 3;
            let px = &d[mid..mid + 3];
            assert!(px.iter().all(|&v| v.abs_diff(px[0]) <= 1 && v > 0), "{layout:?}: {px:?}");
            assert_eq!(decode(&h, &strip, "", Mode::Header).unwrap().info(), img.info(), "{layout:?}");
            assert!(decode(&h, &strip[..strip.len() / 2], "", Mode::Full).is_err());
            let none = header(vec![(FULL_SIZE, &full), (LAYOUT, &layout)], &strip);
            assert!(decode(&none, &strip, "", Mode::Full).is_err());
        }
        let huge: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        let h = header(vec![(FULL_SIZE, huge), (IMAGE_SIZE, huge), (LAYOUT, &[0, 0])], &[]);
        assert!(decode(&h, &[], "", Mode::Full).is_err());
    }
}
