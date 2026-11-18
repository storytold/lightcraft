//! Reduced-size development straight from the mosaic ("superpixel" binning).
//!
//! When the caller needs an image at most half (or a third, a quarter…) of the sensor size — a
//! 2560 px loupe preview or a grid thumbnail of a 24 MP raw — demosaicing the full mosaic and then
//! downscaling throws most of the work away. Instead every `k × k` block of CFA samples becomes one
//! output pixel whose channels are the means of the block's samples of that colour. Every block must
//! contain all three colours (Bayer: `k` even; X-Trans: `k` a multiple of 3). There is no
//! interpolation, so there are no demosaicing artefacts; the colour planes are offset by less than
//! one output pixel, which is invisible at these scales.
//!
//! Clipping is decided per sample, before averaging. [`RawImage::develop_binned_masked`] keeps every
//! channel the mean of its samples and says, per output pixel, which colours had a sample at or above
//! `clip` (for [`crate::highlight::reconstruct_masked`], which rebuilds them with the mean as a lower
//! bound). A specular point that clips one green sample of the 32 in an 8 × 8 block thus counts as
//! partly clipped without the block's green jumping to the clip level, which would read as a green
//! speckle once reconstructed. [`RawImage::develop_binned`] instead gives such a channel the block's
//! maximum sample of the colour, so that plain value thresholds see it as clipped.

use crate::{Normalized, RawData, RawImage, Result, Rgb32f};
use rayon::prelude::*;

impl RawImage {
    /// Can `k × k` blocks be binned (single-plane CFA data, every `k × k` window containing R, G
    /// and B; Bayer: `k` even, so that every block has the same layout)?
    pub fn can_bin(&self, k: usize) -> bool {
        if self.merged_rgb() {
            // block means of a picture with no mosaic keep no phase: any block size, but only plain levels
            return k >= 2 && (self.pixel_aspect - 1.0).abs() < 0.01 && self.opcodes.list1.is_empty() && self.opcodes.list2.is_empty();
        }
        let Some(cfa) = &self.cfa else { return false };
        if k < 2 || self.cpp != 1 || (self.pixel_aspect - 1.0).abs() >= 0.01 || !cfa.valid() || (cfa.is_bayer() && !k.is_multiple_of(2)) {
            return false;
        }
        // every window position relative to the pattern
        (0..cfa.height).all(|j| (0..cfa.width).all(|i| block_layout(cfa, i, j, k).1.iter().all(|&n| n > 0)))
    }

    /// Like [`RawImage::develop`] at `1/k` of the size (camera RGB, white = 1.0, `OpcodeList3` and the default crop
    /// applied, not oriented). `None` when the data cannot be binned ([`RawImage::can_bin`]) or its `OpcodeList3`
    /// holds an operation that needs every pixel ([`crate::opcodes::Opcode::binned`]); callers then demosaic.
    pub fn develop_binned(&self, k: usize, clip: f32) -> Result<Option<Rgb32f>> {
        Ok(self.bin(k, clip, true)?.map(|(img, _)| img))
    }

    /// Like [`RawImage::develop_binned`], with every channel the mean of its block's samples, and the
    /// clip mask: per output pixel, bits [`crate::highlight::CLIPPED_R`] / `_G` / `_B` for the colours
    /// with a sample at or above `clip` in the block. Hand both to [`crate::highlight::reconstruct_masked`].
    pub fn develop_binned_masked(&self, k: usize, clip: f32) -> Result<Option<(Rgb32f, Vec<u8>)>> {
        self.bin(k, clip, false)
    }

    /// The binned image and its clip mask; `keep_max`: a channel with a clipped sample takes the
    /// block's maximum sample of the colour instead of the mean.
    fn bin(&self, k: usize, clip: f32, keep_max: bool) -> Result<Option<(Rgb32f, Vec<u8>)>> {
        self.validate()?;
        if !self.can_bin(k) {
            return Ok(None);
        }
        if self.merged_rgb() {
            return self.bin_merged_rgb(k, clip, keep_max);
        }
        // `OpcodeList3` (lens shading as a gain map, lens corrections) works on the demosaiced active area: its
        // operations re-expressed for the binned image, or a full-size develop when one needs every pixel
        let Some(list3) = self.opcodes.list3.iter().map(|op| op.binned(k)).collect::<Option<Vec<_>>>() else { return Ok(None) };
        let Some(cfa) = &self.cfa else { return Ok(None) };
        let a = self.active_area;
        let whole = crate::Rect::new(0, 0, a.width, a.height);
        let crop = self.crop.clipped(a.width, a.height);
        let crop = if crop.width < k || crop.height < k { whole } else { crop };
        // bin the default crop (relative to the active area) only, or all of it for `OpcodeList3` (cropped after)
        let c = if list3.is_empty() { crop } else { whole };
        let (bw, bh) = (c.width / k, c.height / k);
        if bw == 0 || bh == 0 {
            return Ok(None);
        }
        // Opcode lists 1/2 need the normalised copy; plain files are normalised row by row.
        let norm: Option<Normalized> = if self.opcodes.list1.is_empty() && self.opcodes.list2.is_empty() { None } else { Some(self.normalized()?) };
        let cfa = cfa.shifted(a.x, a.y);
        let range = self.white_at(0) - self.black.mean();
        let scale = if range > 0.0 { 1.0 / range } else { 1.0 };
        let uniform_black =
            self.black.delta_h.is_empty() && self.black.delta_v.is_empty() && self.black.values.iter().all(|&v| v == self.black.values[0]);
        let black0 = self.black.at(0, 0, 0, 1);
        // colour of every sample of a block, per pattern phase of the block's corner
        let (pw, ph) = (cfa.width, cfa.height);
        let layouts: Vec<(Vec<u8>, [f32; 3])> = (0..ph)
            .flat_map(|j| (0..pw).map(move |i| (i, j)))
            .map(|(i, j)| {
                let (l, n) = block_layout(&cfa, i, j, k);
                (l, n.map(|n| 1.0 / n.max(1) as f32))
            })
            .collect();
        let wlen = bw * k;
        let mut out = Rgb32f::new(bw, bh);
        let mut mask = vec![0u8; bw * bh];
        out.data.par_chunks_mut(bw).zip(mask.par_chunks_mut(bw)).enumerate().for_each(|(by, (row, mrow))| {
            // the block row's k sample rows, normalised
            let mut rows = vec![0f32; k * wlen];
            for dy in 0..k {
                let y = c.y + by * k + dy;
                let dst = &mut rows[dy * wlen..(dy + 1) * wlen];
                match (&norm, &self.data) {
                    (Some(nm), _) => dst.copy_from_slice(&nm.data[y * nm.width + c.x..][..wlen]),
                    (None, RawData::U16(d)) if uniform_black => {
                        let src = &d[(a.y + y) * self.width + a.x + c.x..][..wlen];
                        for (o, &v) in dst.iter_mut().zip(src) {
                            *o = (v as f32 - black0) * scale;
                        }
                    }
                    (None, data) => {
                        let base = (a.y + y) * self.width + a.x + c.x;
                        for (x, o) in dst.iter_mut().enumerate() {
                            *o = (data.get(base + x) - self.black.at(c.x + x, y, 0, 1)) * scale;
                        }
                    }
                }
            }
            let py = (c.y + by * k) % ph;
            for (bx, (px, m)) in row.iter_mut().zip(mrow.iter_mut()).enumerate() {
                let (layout, inv) = &layouts[py * pw + (c.x + bx * k) % pw];
                let (mut sum, mut max) = ([0f32; 3], [f32::MIN; 3]);
                for dy in 0..k {
                    let src = &rows[dy * wlen + bx * k..][..k];
                    for (&v, &col) in src.iter().zip(&layout[dy * k..(dy + 1) * k]) {
                        let col = col as usize;
                        sum[col] += v;
                        max[col] = max[col].max(v);
                    }
                }
                for ch in 0..3 {
                    let clipped = max[ch] >= clip;
                    px[ch] = if clipped && keep_max { max[ch] } else { sum[ch] * inv[ch] };
                    *m |= u8::from(clipped) << ch;
                }
            }
        });
        Ok(Some(finish_binned(out, mask, &list3, crop, k)))
    }

    /// Three samples per pixel and no mosaic, from the merged pixel-shift decoder (a NEFX): binned by plain block
    /// means. A LinearRaw DNG keeps its own path (it has reduced copies of its own and has always been developed
    /// whole), so only this format is taken.
    fn merged_rgb(&self) -> bool {
        self.format == crate::RawFormat::Nef && self.cpp == 3 && self.cfa.is_none()
    }

    /// [`Self::bin`] for [`Self::merged_rgb`] data: every channel of an output pixel is the mean of the block's samples
    /// of that channel, and a channel is clipped when one of them reaches `clip` (as for a mosaic).
    fn bin_merged_rgb(&self, k: usize, clip: f32, keep_max: bool) -> Result<Option<(Rgb32f, Vec<u8>)>> {
        let RawData::U16(data) = &self.data else { return Ok(None) };
        // `OpcodeList3` as for a mosaic: re-expressed for the binned image, or a full-size develop
        let Some(list3) = self.opcodes.list3.iter().map(|op| op.binned(k)).collect::<Option<Vec<_>>>() else { return Ok(None) };
        // a black level that varies by row, column or position needs every sample's own place
        let black = &self.black;
        if !black.delta_h.is_empty() || !black.delta_v.is_empty() || black.repeat_rows > 1 || black.repeat_cols > 1 {
            return Ok(None);
        }
        let a = self.active_area;
        let whole = crate::Rect::new(0, 0, a.width, a.height);
        let crop = self.crop.clipped(a.width, a.height);
        let crop = if crop.width < k || crop.height < k { whole } else { crop };
        let c = if list3.is_empty() { crop } else { whole };
        let (bw, bh) = (c.width / k, c.height / k);
        if bw == 0 || bh == 0 {
            return Ok(None);
        }
        // each channel against its own black and white level, as `normalized` does
        let low: [f32; 3] = std::array::from_fn(|s| black.at(0, 0, s, 3));
        let scale: [f32; 3] = std::array::from_fn(|s| {
            let range = self.white_at(s) - low[s];
            if range > 0.0 { 1.0 / range } else { 1.0 }
        });
        let inv_n = 1.0 / (k * k) as f32;
        let mut out = Rgb32f::new(bw, bh);
        let mut mask = vec![0u8; bw * bh];
        out.data.par_chunks_mut(bw).zip(mask.par_chunks_mut(bw)).enumerate().for_each(|(by, (row, mrow))| {
            // the block row's `k` source rows (a row the data lacks leaves its blocks black: the size was validated)
            let rows: Vec<&[u16]> = (0..k)
                .map(|dy| {
                    let start = ((a.y + c.y + by * k + dy) * self.width + a.x + c.x) * 3;
                    data.get(start..start + bw * k * 3).unwrap_or_default()
                })
                .collect();
            for (bx, (px, m)) in row.iter_mut().zip(mrow.iter_mut()).enumerate() {
                let (mut sum, mut max) = ([0f32; 3], [0u16; 3]);
                for line in &rows {
                    for pixel in line.get(bx * k * 3..(bx + 1) * k * 3).unwrap_or_default().as_chunks::<3>().0 {
                        for (s, &v) in pixel.iter().enumerate() {
                            sum[s] += f32::from(v);
                            max[s] = max[s].max(v);
                        }
                    }
                }
                for ch in 0..3 {
                    let top = (f32::from(max[ch]) - low[ch]) * scale[ch];
                    let clipped = top >= clip;
                    px[ch] = if clipped && keep_max { top } else { (sum[ch] * inv_n - low[ch]) * scale[ch] };
                    *m |= u8::from(clipped) << ch;
                }
            }
        });
        Ok(Some(finish_binned(out, mask, &list3, crop, k)))
    }
}

/// The binned image's last steps: `list3` (already re-expressed for the binned scale), then the default crop at that
/// scale when `list3` made the whole active area be binned.
fn finish_binned(mut out: Rgb32f, mask: Vec<u8>, list3: &[crate::opcodes::Opcode], crop: crate::Rect, k: usize) -> (Rgb32f, Vec<u8>) {
    if list3.is_empty() {
        return (out, mask);
    }
    let (bw, bh) = (out.width, out.height);
    crate::opcodes::apply_list3(list3, &mut out);
    let (x0, y0) = ((crop.x / k).min(bw - 1), (crop.y / k).min(bh - 1));
    let (cw, ch) = ((crop.width / k).clamp(1, bw - x0), (crop.height / k).clamp(1, bh - y0));
    if (x0, y0, cw, ch) == (0, 0, bw, bh) {
        return (out, mask);
    }
    let mask = (y0..y0 + ch).flat_map(|y| mask[y * bw + x0..y * bw + x0 + cw].iter().copied()).collect();
    (out.into_crop(x0, y0, cw, ch), mask)
}

/// Colours of the `k × k` samples of a block whose corner sits at pattern position `(i, j)`
/// (row-major) and the count of each colour.
fn block_layout(cfa: &crate::Cfa, i: usize, j: usize, k: usize) -> (Vec<u8>, [u32; 3]) {
    let mut n = [0u32; 3];
    let l: Vec<u8> = (0..k * k)
        .map(|t| {
            let c = cfa.color_at(i + t % k, j + t / k).min(2);
            n[c as usize] += 1;
            c
        })
        .collect();
    (l, n)
}

#[cfg(test)]
mod tests {
    use crate::demosaic::mosaic_from_rgb;
    use crate::*;

    fn raw_from(n: &Normalized, black: f32, white: f32) -> RawImage {
        let data = n.data.iter().map(|&v| (v * (white - black) + black).round() as u16).collect();
        RawImage {
            format: RawFormat::Dng,
            width: n.width,
            height: n.height,
            cpp: 1,
            data: RawData::U16(data),
            cfa: n.cfa.clone(),
            bits: 14,
            black: BlackLevel::uniform(black),
            white: vec![white],
            active_area: Rect::new(0, 0, n.width, n.height),
            crop: Rect::new(0, 0, n.width, n.height),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Metadata::default(),
            lens_metadata: Default::default(),
        }
    }

    #[test]
    fn binning_matches_block_means_and_keeps_clipping() {
        let img = Rgb32f::from_fn(24, 18, |x, y| [0.1 + x as f32 * 0.01, 0.2 + y as f32 * 0.01, 0.3]);
        let mut raw = raw_from(&mosaic_from_rgb(&img, &Cfa::bayer("GRBG").unwrap()), 512.0, 16383.0);
        assert!(raw.can_bin(2) && raw.can_bin(4) && !raw.can_bin(3));
        let b = raw.develop_binned(2, 0.99).unwrap().unwrap();
        assert_eq!((b.width, b.height), (12, 9));
        let p = b.get(3, 2);
        // red sample of block (3,2) under GRBG is at (7,4); blue at (6,5); greens average (6,4)+(7,5)
        assert!((p[0] - img.get(7, 4)[0]).abs() < 1e-3, "{p:?}");
        assert!((p[2] - 0.3).abs() < 1e-3);
        assert!((p[1] - (img.get(6, 4)[1] + img.get(7, 5)[1]) / 2.0).abs() < 1e-3);
        // a single clipped green sample keeps the block's green clipped
        if let RawData::U16(d) = &mut raw.data {
            d[4 * 24 + 6] = 16383;
        }
        assert!(raw.develop_binned(2, 0.99).unwrap().unwrap().get(3, 2)[1] >= 0.99);
        // default crop is honoured at the binned scale
        raw.crop = Rect::new(2, 2, 20, 12);
        let c = raw.develop_binned(2, 0.99).unwrap().unwrap();
        assert_eq!((c.width, c.height), (10, 6));
        // X-Trans: 3×3 blocks hold all colours, 2×2 don't
        let x = raw_from(&mosaic_from_rgb(&img, &Cfa::xtrans()), 0.0, 1000.0);
        assert!(x.can_bin(3) && x.can_bin(6) && !x.can_bin(2));
        let xb = x.develop_binned(3, 0.99).unwrap().unwrap();
        assert_eq!((xb.width, xb.height), (8, 6));
        assert!((xb.get(4, 3)[2] - 0.3).abs() < 2e-3);
    }

    /// Issue #548: specular points that clip one green sample in some 8 × 8 blocks of a grey surface.
    /// Taking the block's maximum makes those blocks' green the clip level, which reconstruction then
    /// keeps (a green speckle); the masked binning keeps the mean and marks the green as clipped.
    #[test]
    fn clipping_is_decided_before_averaging() {
        let img = Rgb32f::filled(64, 64, [0.3, 0.3, 0.3]);
        let mut raw = raw_from(&mosaic_from_rgb(&img, &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        if let RawData::U16(d) = &mut raw.data {
            // one green sample (RGGB: odd column of an even row) in every other block
            for by in 0..8 {
                for bx in (by % 2..8).step_by(2) {
                    d[(by * 8 + 2) * 64 + bx * 8 + 3] = 4095;
                }
            }
        }
        let wb = [1.0f32; 3];
        let (mut masked, mask) = raw.develop_binned_masked(8, 0.99).unwrap().unwrap();
        assert_eq!(mask.len(), 64);
        assert_eq!(mask.iter().filter(|&&m| m == highlight::CLIPPED_G).count(), 32);
        assert!(mask.iter().all(|&m| m & (highlight::CLIPPED_R | highlight::CLIPPED_B) == 0));
        // the clipped block's green is its mean: (31 · 0.3 + 1) / 32
        assert!((masked.get(0, 0)[1] - (31.0 * 0.3 + 1.0) / 32.0).abs() < 1e-3, "{:?}", masked.get(0, 0));
        assert_eq!(highlight::reconstruct_masked(&mut masked, wb, 0.99, [1.0; 3], &mask), 32);
        let green = |img: &Rgb32f| img.data.iter().map(|p| p[1] / p[0]).fold(0.0f32, f32::max);
        // (no more than the mean itself: the clipped sample counts at the clip level)
        assert!(green(&masked) < 1.08, "green excess {}", green(&masked));
        // the maximum rule: a green speckle at the clip level
        let mut max = raw.develop_binned(8, 0.99).unwrap().unwrap();
        assert!(max.get(0, 0)[1] >= 0.99);
        highlight::reconstruct(&mut max, wb, 0.99);
        assert!(green(&max) > 3.0, "{}", green(&max));
        // without clipped samples both are the same plain means, and the mask is empty
        let clean = raw_from(&mosaic_from_rgb(&img, &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let (a, m) = clean.develop_binned_masked(8, 0.99).unwrap().unwrap();
        assert!(m.iter().all(|&v| v == 0));
        assert!(a.data == clean.develop_binned(8, 0.99).unwrap().unwrap().data);
    }

    #[test]
    fn binning_matches_demosaic_downscaled_on_smooth_images() {
        let img = Rgb32f::from_fn(64, 48, |x, y| [0.2 + 0.3 * (x as f32 / 64.0), 0.4 - 0.2 * (y as f32 / 48.0), 0.25]);
        let raw = raw_from(&mosaic_from_rgb(&img, &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let b = raw.develop_binned(2, 0.99).unwrap().unwrap();
        let full = lcc_raster::resample::half(&raw.develop(Method::Ahd).unwrap());
        for y in 2..b.height - 2 {
            for x in 2..b.width - 2 {
                for c in 0..3 {
                    assert!((b.get(x, y)[c] - full.get(x, y)[c]).abs() < 0.01, "({x},{y})");
                }
            }
        }
    }
}

#[cfg(test)]
mod list3_tests {
    use crate::demosaic::mosaic_from_rgb;
    use crate::opcodes::{Area, Opcode};
    use crate::*;

    fn raw(list3: Vec<Opcode>) -> RawImage {
        let img = Rgb32f::from_fn(32, 24, |x, y| [0.1 + x as f32 * 0.005, 0.2 + y as f32 * 0.005, 0.25]);
        let n = mosaic_from_rgb(&img, &Cfa::bayer("RGGB").unwrap());
        RawImage {
            format: RawFormat::Dng,
            width: n.width,
            height: n.height,
            cpp: 1,
            data: RawData::U16(n.data.iter().map(|&v| (v * 16383.0).round() as u16).collect()),
            cfa: n.cfa.clone(),
            bits: 14,
            black: BlackLevel::uniform(0.0),
            white: vec![16383.0],
            active_area: Rect::new(0, 0, n.width, n.height),
            crop: Rect::new(2, 2, 28, 20),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists { list3, ..Default::default() },
            metadata: Metadata::default(),
            lens_metadata: Default::default(),
        }
    }

    fn area() -> Area {
        Area { top: 0, left: 0, bottom: 24, right: 32, plane: 0, planes: 3, row_pitch: 1, col_pitch: 1 }
    }

    /// A lens-shading gain map in `OpcodeList3` (as DJI DNGs carry) no longer forces a full-size develop: the binned
    /// image gets it at its own scale, and the default crop after it.
    #[test]
    fn a_gain_map_applies_to_the_binned_image() {
        let gain =
            Opcode::GainMap { area: area(), points_v: 2, points_h: 2, spacing: [1.0, 1.0], origin: [0.0, 0.0], map_planes: 1, gains: vec![2.0; 4] };
        let plain = raw(Vec::new()).develop_binned(2, 0.99).unwrap().unwrap();
        let shaded = raw(vec![gain]).develop_binned(2, 0.99).unwrap().expect("binned despite OpcodeList3");
        assert_eq!((shaded.width, shaded.height), (plain.width, plain.height));
        assert_eq!((plain.width, plain.height), (14, 10), "the default crop at the binned scale");
        for (a, b) in plain.data.iter().zip(&shaded.data) {
            for c in 0..3 {
                assert!((b[c] - 2.0 * a[c]).abs() < 1e-4, "{a:?} {b:?}");
            }
        }
    }

    /// An operation that needs every pixel (per-row deltas) still refuses binning: the caller demosaics.
    #[test]
    fn a_per_row_opcode_refuses_binning() {
        let rows = Opcode::DeltaPerRow { area: area(), deltas: vec![0.01; 24] };
        assert!(raw(vec![rows]).develop_binned(2, 0.99).unwrap().is_none());
        assert!(
            Opcode::GainMap {
                area: Area { row_pitch: 2, ..area() },
                points_v: 1,
                points_h: 1,
                spacing: [1.0, 1.0],
                origin: [0.0, 0.0],
                map_planes: 1,
                gains: vec![1.0]
            }
            .binned(2)
            .is_none()
        );
    }

    /// A three-sample raw (no mosaic) of `format`, `f(x, y, channel)` in sensor units.
    fn rgb_raw(format: RawFormat, w: usize, h: usize, f: impl Fn(usize, usize, usize) -> u16) -> RawImage {
        let data = (0..w * h * 3).map(|i| f(i / 3 % w, i / 3 / w, i % 3)).collect();
        RawImage {
            format,
            width: w,
            height: h,
            cpp: 3,
            data: RawData::U16(data),
            cfa: None,
            bits: 14,
            black: BlackLevel { repeat_rows: 1, repeat_cols: 1, values: vec![1008.0, 1000.0, 1016.0], ..Default::default() },
            white: vec![16383.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(0, 0, w, h),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Metadata::default(),
            lens_metadata: Default::default(),
        }
    }

    fn smooth(x: usize, y: usize, c: usize) -> u16 {
        (1100 + x * 37 + y * 53 + c * 400 + (x * y) % 17) as u16
    }

    /// A LinearRaw DNG (Lightroom's HDR and panorama merges) is not binned and develops as it always has: each
    /// sample minus its channel's black over the range to the white level, the default crop applied. A reference
    /// worked out here pins that, so the merged-RGB binning below can't change it.
    #[test]
    fn a_linear_raw_dng_develops_as_before_and_is_never_binned() {
        let mut raw = rgb_raw(RawFormat::Dng, 24, 18, smooth);
        raw.crop = Rect::new(3, 2, 18, 12);
        for k in 2..=8 {
            assert!(!raw.can_bin(k), "k = {k}");
            assert!(raw.develop_binned_masked(k, 0.99).unwrap().is_none());
        }
        let img = raw.develop(Method::Bilinear).unwrap();
        assert_eq!((img.width, img.height), (18, 12));
        let blacks = [1008.0f32, 1000.0, 1016.0];
        for (y, x) in [(0, 0), (5, 7), (11, 17)] {
            for c in 0..3 {
                let want = (f32::from(smooth(x + 3, y + 2, c)) - blacks[c]) / (16383.0 - blacks[c]);
                assert!((img.get(x, y)[c] - want).abs() < 1e-6, "({x},{y}) {c}: {} vs {want}", img.get(x, y)[c]);
            }
        }
    }

    /// Merged pixel-shift RGB (a NEFX) bins to block means per channel, with clipping decided per sample.
    #[test]
    fn merged_rgb_bins_to_block_means_and_marks_clipped_samples() {
        let mut raw = rgb_raw(RawFormat::Nef, 24, 18, smooth);
        // any block size works: there is no mosaic phase to keep
        for k in [2, 3, 4, 5, 6, 8] {
            assert!(raw.can_bin(k), "k = {k}");
        }
        assert!(!raw.can_bin(1));
        let blacks = [1008.0f32, 1000.0, 1016.0];
        let full = raw.develop(Method::Bilinear).unwrap();
        let (b, mask) = raw.develop_binned_masked(3, 0.99).unwrap().unwrap();
        assert_eq!((b.width, b.height, mask.len()), (8, 6, 48));
        assert!(mask.iter().all(|&m| m == 0));
        // the same image as averaging the full development over each block
        for (bx, by) in [(0, 0), (3, 2), (7, 5)] {
            for c in 0..3 {
                let mean = (0..9).map(|i| full.get(bx * 3 + i % 3, by * 3 + i / 3)[c]).sum::<f32>() / 9.0;
                assert!((b.get(bx, by)[c] - mean).abs() < 1e-5, "({bx},{by}) {c}");
            }
        }
        // one clipped green sample marks that block's green, and only it; the mean stays the mean
        if let RawData::U16(d) = &mut raw.data {
            d[(4 * 24 + 7) * 3 + 1] = 16383;
        }
        let (m, mask) = raw.develop_binned_masked(3, 0.99).unwrap().unwrap();
        assert_eq!(mask.iter().filter(|&&v| v != 0).count(), 1);
        assert_eq!(mask[8 + 2], highlight::CLIPPED_G, "block (2, 1) of 8 across");
        assert!(m.get(2, 1)[1] < 0.5, "the mean of the block, not its clip level");
        // the plain variant gives the clipped channel the block's maximum
        assert!(raw.develop_binned(3, 0.99).unwrap().unwrap().get(2, 1)[1] >= 0.99);
        // the default crop is binned from its own corner
        raw.crop = Rect::new(3, 3, 18, 12);
        let c = raw.develop_binned(3, 0.99).unwrap().unwrap();
        assert_eq!((c.width, c.height), (6, 4));
        let want = (0..9).map(|i| f32::from(smooth(3 + i % 3, 3 + i / 3, 2)) - blacks[2]).sum::<f32>() / 9.0 / (16383.0 - blacks[2]);
        assert!((c.get(0, 0)[2] - want).abs() < 1e-5);
    }

    /// Operations that need every pixel, files with opcode lists on the raw values, and the header-only image still
    /// decline: the caller then develops at full size.
    #[test]
    fn merged_rgb_declines_what_it_cannot_bin_exactly() {
        let mut raw = rgb_raw(RawFormat::Nef, 24, 18, smooth);
        raw.opcodes.list3.push(Opcode::DeltaPerRow {
            area: Area { top: 0, left: 0, bottom: 18, right: 24, plane: 0, planes: 3, row_pitch: 1, col_pitch: 1 },
            deltas: vec![0.0; 18],
        });
        assert!(raw.develop_binned(2, 0.99).unwrap().is_none());
        let mut raw = rgb_raw(RawFormat::Nef, 24, 18, smooth);
        raw.opcodes.list2.push(Opcode::Unknown { id: 99, flags: 0, params: vec![] });
        assert!(!raw.can_bin(2));
        let mut raw = rgb_raw(RawFormat::Nef, 24, 18, smooth);
        raw.black.delta_v = vec![0.0; 18];
        assert!(raw.develop_binned(2, 0.99).unwrap().is_none(), "a per-row black level needs every row");
    }
}
