//! Pixel-shift merge: a series of raw frames shot with the sensor moved between them becomes one image with
//! full colour at every position, merged **before** demosaicing, on the raw mosaics.
//!
//! The camera moves the sensor by one pixel between the four frames of a *set* and by half a pixel between sets.
//! One set therefore sees every scene point through a red, two green and a blue filter, and the merge just
//! collects them: no interpolation, no registration. The geometry was measured from a 16-shot series of a Z 8 and
//! from Nikon's own merges of it (sets of 4 and the full 16): the merge here reproduces those merges' red and green
//! samples bit for bit.
//!
//! Placement: a sample of frame `k` at raw site `(x, y)` lands on output node `(scale·x + dx_k, scale·y + dy_k)`
//! of a grid `scale` times as fine as the sensor (`scale` 1 for one set of four, 2 for 16 shots). The table of
//! `(dx, dy)` per frame index is [`Pattern`]; it is fixed (the maker-note positions of the frames are not used
//! to place anything: they are good to about 0.1 px, not enough). Only the 16-shot table and the set of four
//! are verified; the 4-, 8- and 32-shot tables are added when series of those have been measured.
//!
//! Every output node gets its red from one sample, its blue from one and its green as the mean (rounded down) of
//! the two green samples. Values stay on the camera's scale: black and white are the frames' own and nothing is
//! rescaled. The result covers the frames' whole active area; the default crop (`crop`) is the frames' crop
//! scaled, so the margin the camera crops stays in the data.
//!
//! Frames are added one at a time and dropped by the caller: the working set is the result plus one frame.
//!
//! Known difference from NX Studio's merge (measured on two 16-shot series of a Z 8): red and green are bit-identical,
//! and so is blue except at blue sites of every twelfth sensor row (rows ≡ 5 mod 12), where about half of the merged
//! values differ from ours by about the noise. The camera already stores smoothed values in those rows (their noise is a
//! third lower than the other blue rows'), and NX Studio's output differs there from the stored samples; we keep the
//! stored samples. No pattern shows in the tests on real images.

use lcc_geom::Orientation;
use lcc_meta::Metadata;
use lcc_raw::merge_provenance::{self, Provenance};
use lcc_raw::{BlackLevel, ColorData, DngCompression, DngWriteOptions, Opcode, OpcodeLists, RawData, RawFormat, RawImage, Rect};
use rayon::prelude::*;

use crate::{MergeError, Result};

/// Where each frame of a series puts its samples.
#[derive(Debug, PartialEq, Eq)]
pub struct Pattern {
    /// Output pixels per sensor pixel (per side).
    pub scale: usize,
    /// Per frame index (0-based): the offset `(dx, dy)` in output pixels.
    pub shifts: &'static [(usize, usize)],
}

/// One set of four frames at native resolution: the image moves left, left and up, up between frames.
pub const SET: Pattern = Pattern { scale: 1, shifts: &[(0, 0), (1, 0), (1, 1), (0, 1)] };

/// Sixteen shots: four sets of four, the sets half a sensor pixel apart (output at twice the sensor resolution).
pub const SHOT_16: Pattern = Pattern {
    scale: 2,
    shifts: &[(0, 0), (2, 0), (2, 2), (0, 2), (1, 0), (3, 0), (3, 2), (1, 2), (1, 1), (3, 1), (3, 3), (1, 3), (0, 1), (2, 1), (2, 3), (0, 3)],
};

/// The table for a series of `count` shots, `None` for a count that has not been verified.
pub fn pattern_for(count: u16) -> Option<&'static Pattern> {
    match count {
        16 => Some(&SHOT_16),
        _ => None,
    }
}

/// The frame indexes (0-based) of set `set` (0..4) of a 16-shot series, to merge alone with [`SET`].
pub fn set_frames(set: usize) -> Option<std::ops::Range<usize>> {
    (set < 4).then(|| set * 4..set * 4 + 4)
}

/// The merged image.
pub struct Merged {
    pub width: usize,
    pub height: usize,
    /// R, G, B per node, row-major, on the frames' own scale (black not subtracted).
    pub data: Vec<u16>,
    pub black: f32,
    pub white: f32,
    pub scale: usize,
    /// The frames' default crop in output pixels: (x, y, width, height).
    pub crop: [usize; 4],
    /// Nodes that lacked a sample of some colour (the outermost nodes) and were filled from a neighbour.
    pub filled: usize,
}

const HAVE_R: u8 = 1;
const HAVE_B: u8 = 2;
const HAVE_G1: u8 = 4;
const HAVE_G2: u8 = 8;
const COMPLETE: u8 = HAVE_R | HAVE_B | HAVE_G1 | HAVE_G2;

/// Collects the frames of one merge.
pub struct Merger {
    pattern: &'static Pattern,
    raw_w: usize,
    raw_h: usize,
    active: [usize; 4],
    crop: [usize; 4],
    width: usize,
    height: usize,
    black: f32,
    white: f32,
    data: Vec<u16>,
    state: Vec<u8>,
    added: Vec<bool>,
}

fn bad(m: impl Into<String>) -> MergeError {
    MergeError::Mismatch(m.into())
}

impl Merger {
    /// A merger for frames like `first` (size, active area, crop, levels); `first` itself is not added.
    pub fn new(pattern: &'static Pattern, first: &RawImage) -> Result<Merger> {
        let a = first.active_area;
        if first.cpp != 1 || first.cfa.as_ref().is_none_or(|c| !c.is_bayer()) || !matches!(first.data, RawData::U16(_)) {
            return Err(bad("pixel shift needs raw frames with a Bayer mosaic of 16-bit samples"));
        }
        if a.width == 0 || a.height == 0 || a.x.saturating_add(a.width) > first.width || a.y.saturating_add(a.height) > first.height {
            return Err(bad("the raw frames have no valid active area"));
        }
        let b = &first.black;
        if !b.delta_h.is_empty() || !b.delta_v.is_empty() || b.values.windows(2).any(|v| v[0] != v[1]) {
            return Err(bad("the black level of these frames varies by position; not supported"));
        }
        let (w, h) = (a.width.checked_mul(pattern.scale), a.height.checked_mul(pattern.scale));
        let (Some(width), Some(height)) = (w, h) else { return Err(bad("image too large")) };
        let n = width.checked_mul(height).and_then(|v| v.checked_mul(3)).ok_or_else(|| bad("image too large"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(n).map_err(|_| MergeError::Output("not enough memory for the merged image".into()))?;
        data.resize(n, 0);
        let mut state = Vec::new();
        state.try_reserve_exact(width * height).map_err(|_| MergeError::Output("not enough memory for the merged image".into()))?;
        state.resize(width * height, 0);
        let c = first.crop.clipped(a.width, a.height);
        let crop = if c.width == 0 || c.height == 0 { [0, 0, a.width, a.height] } else { [c.x, c.y, c.width, c.height] };
        Ok(Merger {
            pattern,
            raw_w: first.width,
            raw_h: first.height,
            active: [a.x, a.y, a.width, a.height],
            crop,
            width,
            height,
            black: b.mean(),
            white: first.white.first().copied().unwrap_or(16383.0),
            data,
            state,
            added: vec![false; pattern.shifts.len()],
        })
    }

    /// Put frame `index` (0-based) into the image.
    pub fn add_frame(&mut self, index: usize, frame: &RawImage) -> Result<()> {
        let Some(&(dx, dy)) = self.pattern.shifts.get(index) else { return Err(bad(format!("frame {} is not part of this pattern", index + 1))) };
        if std::mem::replace(&mut self.added[index], true) {
            return Err(bad(format!("frame {} was added twice", index + 1)));
        }
        let a = frame.active_area;
        if (frame.width, frame.height) != (self.raw_w, self.raw_h) || [a.x, a.y, a.width, a.height] != self.active {
            return Err(bad(format!("frame {} has a different size from the first", index + 1)));
        }
        if frame.black.mean() != self.black || frame.white.first().copied().unwrap_or(0.0) != self.white {
            return Err(bad(format!("frame {} has different black or white levels", index + 1)));
        }
        let (RawData::U16(src), Some(cfa)) = (&frame.data, &frame.cfa) else { return Err(bad("pixel shift needs 16-bit mosaic frames")) };
        if frame.cpp != 1 || !cfa.is_bayer() || src.len() < frame.width.saturating_mul(frame.height) {
            return Err(bad(format!("frame {} is not a Bayer mosaic", index + 1)));
        }
        let (scale, width) = (self.pattern.scale, self.width);
        let [ax, ay, aw, ah] = self.active;
        let stride = frame.width;
        self.data
            .par_chunks_mut(width * 3)
            .zip(self.state.par_chunks_mut(width))
            .enumerate()
            .filter(|(y, _)| *y >= dy && (*y - dy) % scale == 0 && (*y - dy) / scale < ah)
            .for_each(|(y, (row, st))| {
                let sy = (y - dy) / scale;
                let src_row = &src[(ay + sy) * stride..(ay + sy) * stride + stride];
                for sx in 0..aw {
                    let x = scale * sx + dx;
                    if x >= width {
                        break;
                    }
                    let v = src_row[ax + sx];
                    let s = &mut st[x];
                    let px = &mut row[x * 3..x * 3 + 3];
                    match cfa.color_at(ax + sx, ay + sy) {
                        0 => {
                            px[0] = v;
                            *s |= HAVE_R;
                        }
                        2 => {
                            px[2] = v;
                            *s |= HAVE_B;
                        }
                        _ => {
                            if *s & HAVE_G1 == 0 {
                                px[1] = v;
                                *s |= HAVE_G1;
                            } else {
                                px[1] = ((u32::from(px[1]) + u32::from(v)) / 2) as u16;
                                *s |= HAVE_G2;
                            }
                        }
                    }
                }
            });
        Ok(())
    }

    /// The finished image. Frames not added are an error.
    pub fn finish(mut self) -> Result<Merged> {
        if let Some(i) = self.added.iter().position(|a| !a) {
            return Err(bad(format!("frame {} was not added", i + 1)));
        }
        let (w, h) = (self.width, self.height);
        let mut filled = 0usize;
        // The outermost nodes lack a sample of some colour (a frame's site would lie outside the sensor):
        // take it from the nearest node that has one.
        let incomplete: Vec<usize> = (0..w * h).filter(|&i| self.state[i] != COMPLETE).collect();
        for &i in &incomplete {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            let mut touched = false;
            for (bit, c) in [(HAVE_R, 0usize), (HAVE_B, 2), (HAVE_G1, 1)] {
                if self.state[i] & bit != 0 {
                    continue;
                }
                'search: for r in 1..=8i64 {
                    for dy in -r..=r {
                        for dx in -r..=r {
                            let (nx, ny) = (x + dx, y + dy);
                            if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                                continue;
                            }
                            let j = ny as usize * w + nx as usize;
                            if self.state[j] & bit != 0 {
                                self.data[i * 3 + c] = self.data[j * 3 + c];
                                touched = true;
                                break 'search;
                            }
                        }
                    }
                }
            }
            filled += usize::from(touched);
        }
        let s = self.pattern.scale;
        let [cx, cy, cw, ch] = self.crop;
        Ok(Merged {
            width: w,
            height: h,
            data: self.data,
            black: self.black,
            white: self.white,
            scale: s,
            crop: [cx * s, cy * s, cw * s, ch * s],
            filled,
        })
    }
}

/// What a merged image becomes as a file: the frame-01 facts the DNG carries.
pub struct DngSource {
    /// Frame 01's metadata (camera, lens, exposure, capture time…).
    pub metadata: Metadata,
    /// Frame 01's colour as DNG tags (white balance, matrices, look): the merged data is still its camera RGB.
    pub color: ColorData,
    pub orientation: Orientation,
    /// Frame 01's lens correction (`OpcodeList3`). The warp is normalised to the active area and its optical-centre
    /// offset scales with the image, so the frame's own opcodes fit the merged image as they are.
    pub lens: Vec<Opcode>,
    pub provenance: Provenance,
    /// Frame 01's JPEG, kept as the merged file's preview: the colour look is fitted to it as it is for the camera's raw.
    pub preview: Option<lcc_raw::DngPreview>,
}

/// Compression of the merged DNG's 16-bit samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DngPacking {
    /// Tiles of Deflate with horizontal prediction.
    Deflate,
    /// Tiles of lossless JPEG.
    LosslessJpeg,
    Uncompressed,
}

/// The merged image as a LinearRaw DNG: three 16-bit samples per pixel on the camera's own scale (black and white as
/// the frames had them, nothing rescaled), the whole merged area with the default crop set to the frames' crop,
/// `source`'s colour, lens data and metadata, and the provenance block in the XMP. Consumes the image.
pub fn write_dng(merged: Merged, source: DngSource, packing: DngPacking) -> Result<Vec<u8>> {
    let (w, h) = (merged.width, merged.height);
    if w == 0 || h == 0 {
        return Err(MergeError::Output("empty image".into()));
    }
    let [cx, cy, cw, ch] = merged.crop;
    let mut metadata = source.metadata;
    metadata.width = u32::try_from(w).ok();
    metadata.height = u32::try_from(h).ok();
    metadata.orientation = Some(source.orientation);
    metadata.software = Some(merge_provenance::software());
    let packet = source.provenance.add_to_packet(&lcc_meta::write_xmp(&metadata, None));
    let source_preview = source.preview;
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 3,
        data: RawData::U16(merged.data),
        cfa: None,
        bits: 14,
        black: BlackLevel::uniform(merged.black),
        white: vec![merged.white],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(cx, cy, cw, ch),
        orientation: source.orientation,
        color: source.color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists { list3: source.lens, ..Default::default() },
        metadata,
        lens_metadata: Default::default(),
    };
    let compression = match packing {
        DngPacking::Deflate => DngCompression::Deflate { tile: 256, half: false },
        DngPacking::LosslessJpeg => DngCompression::Lj92 { tile: 256 },
        DngPacking::Uncompressed => DngCompression::Uncompressed,
    };
    lcc_raw::write_dng(&raw, &DngWriteOptions { compression, xmp: Some(packet), exif: true, preview: source_preview, ..Default::default() })
        .map_err(|e| MergeError::Output(e.to_string()))
}

/// Merge a series, loading one frame at a time with `load(index)` (0-based); each frame is dropped before the next
/// is loaded. `progress(fraction)` returns false to cancel.
pub fn merge_series(
    pattern: &'static Pattern,
    frames: std::ops::Range<usize>,
    load: &mut dyn FnMut(usize) -> std::result::Result<RawImage, String>,
    progress: &dyn Fn(f32) -> bool,
) -> Result<Merged> {
    let n = frames.len().max(1);
    let mut merger: Option<Merger> = None;
    for (k, i) in frames.enumerate() {
        if !progress(k as f32 / n as f32) {
            return Err(MergeError::Cancelled);
        }
        let frame = load(i).map_err(MergeError::Decode)?;
        if merger.is_none() {
            merger = Some(Merger::new(pattern, &frame)?);
        }
        if let Some(m) = merger.as_mut() {
            // frames of a set are numbered from 0 within the pattern
            m.add_frame(k, &frame)?;
        }
    }
    progress(1.0);
    merger.ok_or(MergeError::TooFew(0, 1))?.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_geom::Orientation;
    use lcc_raw::{BlackLevel, Cfa, ColorData, RawFormat, Rect};

    /// Deterministic "scene": distinct values for every node and colour (so a wrong placement can't match).
    fn truth(x: usize, y: usize, c: usize) -> u16 {
        let h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (c as u64 + 1).wrapping_mul(0x1656_67B1_9E37_79F9);
        let h = h ^ (h >> 29);
        1008 + (h % 15000) as u16
    }

    fn frame(pattern: &Pattern, k: usize, w: usize, h: usize, cfa: &str, active: Rect, crop: Rect) -> RawImage {
        let (dx, dy) = pattern.shifts[k];
        let cfa = Cfa::bayer(cfa).unwrap();
        // the node of a site is relative to the active area, as in the merge
        let mut d = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                let c = cfa.color_at(x, y) as usize;
                let (sx, sy) = (x.wrapping_sub(active.x), y.wrapping_sub(active.y));
                d[y * w + x] =
                    if sx < active.width && sy < active.height { truth(pattern.scale * sx + dx, pattern.scale * sy + dy, c) } else { 1008 };
            }
        }
        RawImage {
            format: RawFormat::Nef,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16(d),
            cfa: Some(cfa),
            bits: 14,
            black: BlackLevel::uniform(1008.0),
            white: vec![16383.0],
            active_area: active,
            crop,
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: Default::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
        }
    }

    /// The truth at node (X, Y) of the merge: red and blue as sampled, green as the node's own value.
    pub(super) fn check(m: &Merged, border: usize) -> usize {
        let mut checked = 0;
        for y in border..m.height - border {
            for x in border..m.width - border {
                for c in 0..3 {
                    assert_eq!(m.data[(y * m.width + x) * 3 + c], truth(x, y, c), "node ({x},{y}) channel {c}");
                }
                checked += 1;
            }
        }
        checked
    }

    pub(super) fn run(pattern: &'static Pattern, n: usize, w: usize, h: usize, cfa: &str, active: Rect, crop: Rect) -> Merged {
        let mut load = |i: usize| Ok(frame(pattern, i, w, h, cfa, active, crop));
        merge_series(pattern, 0..n, &mut load, &|_| true).unwrap()
    }

    #[test]
    fn four_frames_merge_back_exactly_at_native_resolution() {
        let full = Rect::new(0, 0, 40, 30);
        for cfa in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let m = run(&SET, 4, 40, 30, cfa, full, Rect::new(2, 2, 36, 26));
            assert_eq!((m.width, m.height, m.scale), (40, 30, 1));
            // nodes at the first row/column lack some samples; everything inside is exact
            assert_eq!(check(&m, 2), 36 * 26);
            assert_eq!(m.crop, [2, 2, 36, 26]);
            assert!(m.filled <= 2 * 40 + 2 * 30, "{}", m.filled);
        }
    }

    #[test]
    fn sixteen_frames_merge_back_exactly_at_twice_the_resolution() {
        let full = Rect::new(0, 0, 36, 24);
        for cfa in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let m = run(&SHOT_16, 16, 36, 24, cfa, full, Rect::new(4, 4, 28, 16));
            assert_eq!((m.width, m.height, m.scale), (72, 48, 2));
            assert_eq!(m.crop, [8, 8, 56, 32]);
            // the outermost rows and columns lack samples (frames of the last sets reach 3 nodes further)
            assert_eq!(check(&m, 4), (72 - 8) * (48 - 8), "{cfa}");
            assert_eq!((m.black, m.white), (1008.0, 16383.0));
        }
    }

    #[test]
    fn a_1_to_1_image_area_and_an_active_area_offset_merge_exactly() {
        // square frames whose active area starts inside the stored data (not the full-frame constants), and an
        // odd active-area origin, which changes which colour sits on the first site
        for (active, crop) in [(Rect::new(2, 2, 24, 24), Rect::new(4, 4, 16, 16)), (Rect::new(3, 1, 22, 26), Rect::new(2, 3, 14, 18))] {
            for cfa in ["RGGB", "BGGR", "GRBG", "GBRG"] {
                let m = run(&SHOT_16, 16, 30, 30, cfa, active, crop);
                assert_eq!((m.width, m.height), (active.width * 2, active.height * 2), "{cfa}");
                assert_eq!(m.crop, [crop.x * 2, crop.y * 2, crop.width * 2, crop.height * 2]);
                assert_eq!(check(&m, 4), (m.width - 8) * (m.height - 8), "{cfa}");
            }
        }
    }
    #[test]
    fn green_is_the_rounded_down_mean_of_two_samples() {
        let full = Rect::new(0, 0, 8, 8);
        let mut f: Vec<RawImage> = (0..4).map(|k| frame(&SET, k, 8, 8, "RGGB", full, full)).collect();
        // node (4,3): frame 1 reads site (4,3) (green) and frame 3 site (3,2) (green); frames 2 and 4 give blue and red
        if let RawData::U16(v) = &mut f[0].data {
            v[3 * 8 + 4] = 5001;
        }
        if let RawData::U16(v) = &mut f[2].data {
            v[2 * 8 + 3] = 5000;
        }
        let mut m = Merger::new(&SET, &f[0]).unwrap();
        for (i, fr) in f.iter().enumerate() {
            m.add_frame(i, fr).unwrap();
        }
        let out = m.finish().unwrap();
        let px = |x: usize, y: usize| [out.data[(y * 8 + x) * 3], out.data[(y * 8 + x) * 3 + 1], out.data[(y * 8 + x) * 3 + 2]];
        assert_eq!(px(4, 3), [truth(4, 3, 0), 5000, truth(4, 3, 2)]);
        // a neighbour is untouched
        assert_eq!(px(5, 3), [truth(5, 3, 0), truth(5, 3, 1), truth(5, 3, 2)]);
    }

    #[test]
    fn bad_input_is_refused() {
        let full = Rect::new(0, 0, 16, 16);
        let a = frame(&SET, 0, 16, 16, "RGGB", full, full);
        let mut m = Merger::new(&SET, &a).unwrap();
        m.add_frame(0, &a).unwrap();
        // twice, out of the pattern, wrong size, wrong levels, missing frames
        assert!(m.add_frame(0, &a).is_err());
        assert!(m.add_frame(4, &a).is_err());
        let small = frame(&SET, 1, 12, 16, "RGGB", Rect::new(0, 0, 12, 16), Rect::new(0, 0, 12, 16));
        assert!(m.add_frame(1, &small).is_err());
        let mut levels = frame(&SET, 1, 16, 16, "RGGB", full, full);
        levels.black = BlackLevel::uniform(1000.0);
        assert!(m.add_frame(1, &levels).is_err());
        assert!(m.finish().is_err());
        // a non-mosaic frame
        let mut rgb = frame(&SET, 0, 16, 16, "RGGB", full, full);
        rgb.cfa = None;
        assert!(Merger::new(&SET, &rgb).is_err());
        // a repeating black pattern with equal values is the same level; unequal values are refused
        let mut rep = frame(&SET, 0, 16, 16, "RGGB", full, full);
        rep.black = BlackLevel { repeat_rows: 2, repeat_cols: 2, values: vec![1008.0; 4], delta_h: vec![], delta_v: vec![] };
        assert!(Merger::new(&SET, &rep).is_ok());
        rep.black.values[3] = 1009.0;
        assert!(Merger::new(&SET, &rep).is_err());
        assert!(pattern_for(4).is_none() && pattern_for(32).is_none() && pattern_for(16).is_some());
        assert_eq!(set_frames(3), Some(12..16));
        assert_eq!(set_frames(4), None);
    }

    #[test]
    fn progress_can_cancel() {
        let full = Rect::new(0, 0, 8, 8);
        let mut load = |i: usize| Ok(frame(&SET, i, 8, 8, "RGGB", full, full));
        let r = merge_series(&SET, 0..4, &mut load, &|f| f < 0.4);
        assert!(matches!(r, Err(MergeError::Cancelled)));
    }
}

#[cfg(test)]
mod dng_tests_support {
    use super::tests::{check, run};
    use super::*;
    use lcc_raw::merge_provenance::{ENGINE, SourceFrame, Window};

    pub(super) fn source() -> DngSource {
        let metadata = Metadata { make: Some("TESTMAKE".into()), model: Some("TESTMODEL".into()), iso: Some(64), ..Default::default() };
        let color = ColorData { as_shot_neutral: Some([0.54, 1.0, 0.7]), ..Default::default() };
        let provenance = Provenance {
            engine: ENGINE,
            shot_count: 16,
            reference: 1,
            scale: 2,
            window: Some(Window { sensor: [40, 24], origin: [2, 0], size: [36, 24] }),
            frames: (1..=16)
                .map(|i| SourceFrame { index: i, file_name: format!("synthetic {i}.nef"), captured: String::new(), counter: 100 + u32::from(i) })
                .collect(),
        };
        let lens = vec![Opcode::WarpRectilinear { planes: vec![[1.0, 0.01, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] }];
        DngSource { metadata, color, orientation: Orientation::Normal, lens, provenance, preview: None }
    }

    /// The written DNG decodes to the merged samples bit for bit, with the camera's levels, the crop, the lens data
    /// and the provenance, whatever the packing; and it is recognised as a merge (binned previews).
    #[test]
    fn a_merged_dng_round_trips_bit_exactly() {
        let full = Rect::new(0, 0, 36, 24);
        for packing in [DngPacking::Deflate, DngPacking::LosslessJpeg, DngPacking::Uncompressed] {
            let m = run(&SHOT_16, 16, 36, 24, "RGGB", full, Rect::new(4, 4, 28, 16));
            assert_eq!(check(&m, 4), (72 - 8) * (48 - 8));
            let (data, crop) = (m.data.clone(), m.crop);
            let bytes = write_dng(m, source(), packing).unwrap();
            let raw = lcc_raw::decode(&bytes).unwrap();
            assert_eq!((raw.width, raw.height, raw.cpp), (72, 48, 3), "{packing:?}");
            assert_eq!(raw.format, RawFormat::Nef, "a merge reads like the camera's raw ({packing:?})");
            assert!(!lcc_raw::color::has_matrix(&raw.color), "its colour tags are for other converters ({packing:?})");
            assert!(matches!(&raw.data, RawData::U16(d) if *d == data), "samples differ: {packing:?}");
            assert_eq!(raw.black.mean(), 1008.0, "{packing:?}");
            assert!(raw.white.iter().all(|w| *w == 16383.0), "{packing:?}");
            assert_eq!(raw.active_area, Rect::new(0, 0, 72, 48));
            assert_eq!([raw.crop.x, raw.crop.y, raw.crop.width, raw.crop.height], crop);
            assert_eq!(raw.opcodes.list3, source().lens);
            assert_eq!(raw.metadata.model.as_deref(), Some("TESTMODEL"));
            assert_eq!(raw.metadata.software, Some(merge_provenance::software()));
            assert_eq!(merge_provenance::from_dng(&bytes), Some(source().provenance));
            // previews: three-sample binning applies to a merge, and gives block means of the samples
            assert!(raw.can_bin(2) && raw.can_bin(4));
            let b = raw.develop_binned(4, 0.99).unwrap().unwrap();
            assert!(b.width > 0 && b.height > 0);
        }
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::dng_tests_support::source;
    use super::*;
    use lcc_meta::{DateTime, Gps};

    /// The merged file carries frame 01's capture time with its sub-seconds and time-zone offset in its Exif and in its
    /// XMP, and the descriptive fields that were set; a reader that looks at either finds them.
    #[test]
    fn a_merged_dng_carries_the_capture_time_and_descriptive_metadata() {
        let full = Rect::new(0, 0, 36, 24);
        for time in ["2025-08-07T11:22:27.150+02:00", "2025-08-07T11:22:27", "2025-12-31T23:59:59.007-04:30"] {
            let want = DateTime::parse_iso(time).unwrap();
            let m = super::tests::run(&SHOT_16, 16, 36, 24, "RGGB", full, Rect::new(4, 4, 28, 16));
            let mut s = source();
            s.metadata.capture_time = Some(want);
            s.metadata.keywords = vec!["travel|Italy|Rome".into(), "macro".into()];
            s.metadata.hierarchical_keywords = vec!["travel|Italy|Rome".into(), "macro".into()];
            s.metadata.artist = Some("Test Creator".into());
            s.metadata.copyright = Some("Test Copyright".into());
            s.metadata.title = Some("Test Title".into());
            s.metadata.caption = Some("Test Caption".into());
            s.metadata.city = Some("Testville".into());
            s.metadata.gps = Some(Gps { latitude: 45.5, longitude: 9.25, altitude: None });
            let bytes = write_dng(m, s, DngPacking::Deflate).unwrap();
            // the Exif IFD alone (what the raw decoder reads from the TIFF)
            let raw = lcc_raw::decode(&bytes).unwrap();
            assert_eq!(raw.metadata.capture_time, Some(want), "{time}: Exif");
            // Exif and XMP together, as the library reads a file
            let read = lcc_meta::extract(&bytes);
            assert_eq!(read.capture_time, Some(want), "{time}: extract");
            assert_eq!(read.capture_time.map(|d| d.to_iso()), Some(time.to_string()));
            assert_eq!(read.keywords, vec!["travel|Italy|Rome".to_string(), "macro".to_string()]);
            assert_eq!(read.artist.as_deref(), Some("Test Creator"));
            assert_eq!(read.copyright.as_deref(), Some("Test Copyright"));
            assert_eq!(read.title.as_deref(), Some("Test Title"));
            assert_eq!(read.caption.as_deref(), Some("Test Caption"));
            assert_eq!(read.city.as_deref(), Some("Testville"));
            assert_eq!(read.gps.map(|g| (g.latitude, g.longitude)), Some((45.5, 9.25)));
            assert_eq!(read.rating, None);
        }
    }
}
