//! Writing DNG files from a [`RawImage`] (uncompressed or lossless-JPEG tiles), used for DNG export and for
//! bit-exact decoder round-trip tests. The raw image is stored in IFD0 (allowed by DNG 1.7 §"File Structure").

use crate::{RawData, RawError, RawImage, Result, ljpeg, opcodes};
use lightcraft_color::Mat3;
use lightcraft_tiff::tags::{self as t, compression, photometric};
use lightcraft_tiff::writer::{rational, srational};
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DngCompression {
    Uncompressed,
    /// Lossless JPEG tiles of `tile × tile` pixels (`tile` even). CFA tiles are encoded as two interleaved
    /// components of half the tile width (the common DNG layout), linear data with one component per sample.
    Lj92 {
        tile: u32,
    },
    /// Deflate (zlib) tiles of `tile × tile` pixels. Floating-point data is stored as 16-bit (`half`) or
    /// 32-bit IEEE floats with the floating-point predictor (TIFF predictor 3, Adobe TIFF Technical Note 3);
    /// integer data with horizontal differencing (predictor 2). 16-bit float is what HDR merges usually use.
    Deflate {
        tile: u32,
        half: bool,
    },
    /// Lossy DNG (DNG 1.4 compression 34892): the raw data demosaiced to linear camera RGB (with `OpcodeList3`
    /// and the default crop applied), scaled down to at most `long_edge` pixels on the long side when given,
    /// then stored as 8-bit baseline JPEG tiles of `tile × tile` pixels at JPEG `quality` (1–100). The 8-bit
    /// codes follow a 1/2.2 power curve that a `LinearizationTable` maps back to linear 16-bit values, so
    /// shadows keep their precision. The colour tags (matrices, white balance, profile look) are kept.
    LossyJpeg {
        tile: u32,
        quality: u8,
        long_edge: Option<u32>,
    },
}

/// Gamma of the 8-bit codes in a lossy DNG (own choice: perceptually close to even steps, like sRGB).
const LOSSY_GAMMA: f64 = 2.2;

/// The 256-entry `LinearizationTable` of a lossy DNG: code → linear 16-bit value.
fn lossy_linearization() -> Vec<u16> {
    (0..256u32).map(|i| ((i as f64 / 255.0).powf(LOSSY_GAMMA) * 65535.0).round() as u16).collect()
}

/// The image a lossy DNG stores: `raw` developed to linear camera RGB (white = 1), scaled to fit
/// `long_edge`, as 8-bit codes (in `u16` samples) on the [`LOSSY_GAMMA`] curve, with the levels,
/// geometry and opcode lists that this data needs (black 0, white 65535 after linearization).
fn lossy_source(raw: &RawImage, long_edge: Option<u32>) -> Result<RawImage> {
    let mut rgb = raw.develop(crate::Method::Ahd)?;
    if let Some(edge) = long_edge.map(|e| e.max(1) as usize) {
        let long = rgb.width.max(rgb.height);
        if long > edge {
            let s = edge as f64 / long as f64;
            let w = ((rgb.width as f64 * s).round() as usize).max(1);
            let h = ((rgb.height as f64 * s).round() as usize).max(1);
            rgb = lightcraft_raster::resample::resize(&rgb, w, h, lightcraft_raster::resample::Filter::Lanczos3);
        }
    }
    let (w, h) = (rgb.width, rgb.height);
    if w == 0 || h == 0 {
        return Err(RawError::Corrupt("empty image".into()));
    }
    let inv = 1.0 / LOSSY_GAMMA as f32;
    let data: Vec<u16> = rgb
        .data
        .par_iter()
        .flat_map_iter(|px| px.map(|v| if v.is_finite() { (v.clamp(0.0, 1.0).powf(inv) * 255.0).round() as u16 } else { 0 }))
        .collect();
    let full = crate::Rect::new(0, 0, w, h);
    Ok(RawImage {
        width: w,
        height: h,
        cpp: 3,
        data: RawData::U16(data),
        cfa: None,
        bits: 8,
        black: crate::BlackLevel::default(),
        white: vec![65535.0],
        active_area: full,
        crop: full,
        // list 1 and 2 work on the undemosaiced data and list 3 is applied by `develop`
        opcodes: crate::OpcodeLists::default(),
        linearized: true,
        ..raw.clone()
    })
}

/// Software tag of the DNGs this writer makes. A bare "LightCraft" marks older files, whose CFAPattern is anchored at
/// the sensor origin instead of the ActiveArea (the reader keeps their original reading).
pub const SOFTWARE: &str = "LightCraft DNG 2";

/// zlib level of Deflate tiles. On a 67 MP focus-stack merge (half floats) level 6 took 7× as long as level 1
/// (3.9 s vs 0.55 s on 4 cores) to save 3 % of the file (266 vs 275 MB); 5–12 % on raw and rendered DNGs.
const DEFLATE_LEVEL: u8 = 1;

/// Largest Deflate tile edge the writer pads to (`tile` is caller-supplied in [`DngCompression::Deflate`]):
/// square-tile buffers grow with the square, so an unclamped edge lets a request pad a tiny valid image into
/// a multi-gigabyte buffer and billions of edge-replicate iterations. 2048² samples (≤ 24 MB of f32) is far
/// above the 256 default and every real tile this writer makes.
const MAX_DEFLATE_TILE: u32 = 2048;

/// `tile` aligned down to a multiple of 16 and clamped to [`MAX_DEFLATE_TILE`].
fn deflate_tile(tile: u32) -> usize {
    (tile.max(16) & !15).min(MAX_DEFLATE_TILE) as usize
}

/// IEEE 754 binary32 → binary16 (round to nearest even; overflow → ±inf, NaN kept).
pub fn f32_to_f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // subnormal half
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let r = if rem > mid || (rem == mid && half & 1 == 1) { half + 1 } else { half };
        return sign | r as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    let r = if rem > 0x1000 || (rem == 0x1000 && half & 1 == 1) { half + 1 } else { half };
    sign | r as u16
}

/// Apply the floating-point predictor (inverse of `unpack::undo_float_predictor`) to one row of
/// `n` big-endian samples of `bytes_per` bytes: split into byte planes (most significant first), then
/// byte-difference with `stride` samples.
fn float_predict(row: &[u8], n: usize, bytes_per: usize, stride: usize) -> Vec<u8> {
    let mut planes = vec![0u8; row.len()];
    for i in 0..n {
        for b in 0..bytes_per {
            planes[b * n + i] = row[i * bytes_per + b];
        }
    }
    for i in (stride..planes.len()).rev() {
        planes[i] = planes[i].wrapping_sub(planes[i - stride]);
    }
    planes
}

#[derive(Clone, Debug, PartialEq)]
pub struct DngWriteOptions {
    pub compression: DngCompression,
    pub order: ByteOrder,
    /// XMP packet to embed (tag 700); defaults to one generated from the image metadata.
    pub xmp: Option<String>,
}

impl Default for DngWriteOptions {
    fn default() -> Self {
        Self { compression: DngCompression::Lj92 { tile: 256 }, order: ByteOrder::Little, xmp: None }
    }
}

fn srat_mat(m: &Mat3) -> Value {
    Value::SRational(m.0.iter().flatten().map(|&v| srational(v)).collect())
}

fn rat_vec(v: &[f64]) -> Value {
    Value::Rational(v.iter().map(|&x| rational(x)).collect())
}

/// Serialise `raw` as a DNG.
pub fn write_dng(raw: &RawImage, opts: &DngWriteOptions) -> Result<Vec<u8>> {
    raw.validate()?;
    let lossy;
    let raw = match opts.compression {
        DngCompression::LossyJpeg { long_edge, .. } => {
            lossy = lossy_source(raw, long_edge)?;
            &lossy
        }
        _ => raw,
    };
    let (w, h, cpp) = (raw.width, raw.height, raw.cpp);
    let mut ifd = IfdBuilder::new();
    ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w as u32]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h as u32]));
    ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![cpp as u16]));
    ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![if raw.cfa.is_some() { photometric::CFA } else { photometric::LINEAR_RAW }]));
    ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
    ifd.set(t::DNG_BACKWARD_VERSION, Value::Byte(vec![1, 3, 0, 0]));
    let make = raw.metadata.make.as_deref().unwrap_or_default();
    let model = raw.metadata.model.as_deref().unwrap_or_default();
    if !make.is_empty() {
        ifd.set(t::MAKE, Value::Ascii(make.to_owned()));
    }
    if !model.is_empty() {
        ifd.set(t::MODEL, Value::Ascii(model.to_owned()));
    }
    let model_includes_make = !make.is_empty()
        && model.get(..make.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(make))
        && model.get(make.len()..).is_some_and(|suffix| suffix.chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_'));
    let unique = if model_includes_make || make.is_empty() {
        model.to_owned()
    } else if model.is_empty() {
        make.to_owned()
    } else {
        format!("{make} {model}")
    };
    ifd.set(t::UNIQUE_CAMERA_MODEL, Value::Ascii(if unique.is_empty() { "LightCraft".into() } else { unique }));
    ifd.set(t::ORIENTATION, Value::Short(vec![raw.orientation.to_exif()]));
    ifd.set(t::SOFTWARE, Value::Ascii(SOFTWARE.into()));
    let xmp = opts.xmp.clone().unwrap_or_else(|| lightcraft_meta::write_xmp(&raw.metadata, None));
    ifd.set(t::XMP, Value::Byte(xmp.into_bytes()));

    if let Some(cfa) = &raw.cfa {
        ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![cfa.height as u16, cfa.width as u16]));
        // the tag is anchored at the ActiveArea's top-left, `raw.cfa` at the sensor origin
        let a = raw.active_area;
        ifd.set(t::CFA_PATTERN_EP, Value::Byte(cfa.shifted(a.x, a.y).pattern));
        ifd.set(t::CFA_PLANE_COLOR, Value::Byte(vec![0, 1, 2]));
        ifd.set(t::CFA_LAYOUT, Value::Short(vec![1]));
    }
    let b = &raw.black;
    ifd.set(t::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![b.repeat_rows as u16, b.repeat_cols as u16]));
    let mut bl: Vec<f64> = b.values.iter().map(|&v| v as f64).collect();
    if bl.len() == 1 && cpp > 1 {
        bl = vec![bl[0]; cpp];
    }
    ifd.set(t::BLACK_LEVEL, rat_vec(&bl));
    if !b.delta_h.is_empty() {
        ifd.set(t::BLACK_LEVEL_DELTA_H, Value::SRational(b.delta_h.iter().map(|&v| srational(v as f64)).collect()));
    }
    if !b.delta_v.is_empty() {
        ifd.set(t::BLACK_LEVEL_DELTA_V, Value::SRational(b.delta_v.iter().map(|&v| srational(v as f64)).collect()));
    }
    let white: Vec<u32> = (0..cpp).map(|s| raw.white_at(s).round().clamp(0.0, u32::MAX as f32) as u32).collect();
    if matches!(raw.data, RawData::F32(_)) {
        ifd.set(t::WHITE_LEVEL, rat_vec(&(0..cpp).map(|s| raw.white_at(s) as f64).collect::<Vec<_>>()));
    } else {
        ifd.set(t::WHITE_LEVEL, Value::Long(white));
    }
    let a = raw.active_area;
    ifd.set(t::ACTIVE_AREA, Value::Long(vec![a.y as u32, a.x as u32, (a.y + a.height) as u32, (a.x + a.width) as u32]));
    ifd.set(t::DEFAULT_CROP_ORIGIN, Value::Long(vec![raw.crop.x as u32, raw.crop.y as u32]));
    ifd.set(t::DEFAULT_CROP_SIZE, Value::Long(vec![raw.crop.width as u32, raw.crop.height as u32]));

    let c = &raw.color;
    for (i, tag) in [t::CALIBRATION_ILLUMINANT_1, t::CALIBRATION_ILLUMINANT_2].into_iter().enumerate() {
        if c.color_matrix[i].is_some() {
            ifd.set(tag, Value::Short(vec![c.illuminant[i]]));
        }
    }
    for (pair, tags) in [
        (&c.color_matrix, [t::COLOR_MATRIX_1, t::COLOR_MATRIX_2]),
        (&c.forward_matrix, [t::FORWARD_MATRIX_1, t::FORWARD_MATRIX_2]),
        (&c.camera_calibration, [t::CAMERA_CALIBRATION_1, t::CAMERA_CALIBRATION_2]),
    ] {
        for i in 0..2 {
            if let Some(m) = &pair[i] {
                ifd.set(tags[i], srat_mat(m));
            }
        }
    }
    if let Some(v) = c.analog_balance {
        ifd.set(t::ANALOG_BALANCE, rat_vec(&v));
    }
    if let Some(v) = c.as_shot_neutral {
        ifd.set(t::AS_SHOT_NEUTRAL, rat_vec(&v));
    } else if let Some(xy) = c.as_shot_white_xy {
        ifd.set(t::AS_SHOT_WHITE_XY, rat_vec(&[xy.x, xy.y]));
    } else if let Some(m) = raw.wb_multipliers.filter(|m| m.iter().all(|v| *v > 0.0 && v.is_finite())) {
        // vendor raws (NEF, ARW…) carry white balance as multipliers: the DNG neutral is their reciprocal
        ifd.set(t::AS_SHOT_NEUTRAL, rat_vec(&m.map(|v| 1.0 / v as f64)));
    }
    ifd.set(t::BASELINE_EXPOSURE, Value::SRational(vec![srational(c.baseline_exposure)]));
    if let Some(v) = c.baseline_sharpness {
        ifd.set(t::BASELINE_SHARPNESS, rat_vec(&[v]));
    }
    // the source's own profile look travels with its data (conversions, smart previews, merges)
    let p = &c.profile;
    let table_tags = |ifd: &mut IfdBuilder, table: &crate::profile::HsvTable, dims: u16, enc: u16| {
        ifd.set(dims, Value::Long(vec![table.hue_divisions as u32, table.sat_divisions as u32, table.val_divisions as u32]));
        if table.srgb_value {
            ifd.set(enc, Value::Long(vec![1]));
        }
    };
    let flat = |table: &crate::profile::HsvTable| Value::Float(table.data.iter().flatten().copied().collect());
    match &p.hue_sat_map {
        [Some(a), b] => {
            table_tags(&mut ifd, a, t::PROFILE_HUE_SAT_MAP_DIMS, t::PROFILE_HUE_SAT_MAP_ENCODING);
            ifd.set(t::PROFILE_HUE_SAT_MAP_DATA_1, flat(a));
            if let Some(b) =
                b.as_ref().filter(|b| (b.hue_divisions, b.sat_divisions, b.val_divisions) == (a.hue_divisions, a.sat_divisions, a.val_divisions))
            {
                ifd.set(t::PROFILE_HUE_SAT_MAP_DATA_2, flat(b));
            }
        }
        [None, Some(b)] => {
            table_tags(&mut ifd, b, t::PROFILE_HUE_SAT_MAP_DIMS, t::PROFILE_HUE_SAT_MAP_ENCODING);
            ifd.set(t::PROFILE_HUE_SAT_MAP_DATA_1, flat(b));
        }
        [None, None] => {}
    }
    if let Some(l) = &p.look_table {
        table_tags(&mut ifd, l, t::PROFILE_LOOK_TABLE_DIMS, t::PROFILE_LOOK_TABLE_ENCODING);
        ifd.set(t::PROFILE_LOOK_TABLE_DATA, flat(l));
    }
    if let Some(curve) = &p.tone_curve {
        ifd.set(t::PROFILE_TONE_CURVE, Value::Float(curve.points.iter().flatten().copied().collect()));
    }
    if let Some(map) = &p.gain_table_map {
        // the raw IFD is IFD 0 here, where both tag versions are valid
        let (version2, bytes) = map.to_bytes(opts.order);
        ifd.set(if version2 { t::PROFILE_GAIN_TABLE_MAP_2 } else { t::PROFILE_GAIN_TABLE_MAP }, Value::Undefined(bytes));
    }
    for (list, tag) in [(&raw.opcodes.list1, t::OPCODE_LIST_1), (&raw.opcodes.list2, t::OPCODE_LIST_2), (&raw.opcodes.list3, t::OPCODE_LIST_3)] {
        if !list.is_empty() {
            ifd.set(tag, Value::Undefined(opcodes::write_list(list)));
        }
    }

    match (&raw.data, opts.compression) {
        (RawData::F32(v), DngCompression::Deflate { tile, half }) => {
            let tile = deflate_tile(tile);
            let bp = if half { 2 } else { 4 };
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![(bp * 8) as u16; cpp]));
            ifd.set(t::SAMPLE_FORMAT, Value::Short(vec![3; cpp]));
            ifd.set(t::COMPRESSION, Value::Short(vec![compression::ADOBE_DEFLATE]));
            ifd.set(t::PREDICTOR, Value::Short(vec![3]));
            let (ta, td) = (w.div_ceil(tile), h.div_ceil(tile));
            let tiles: Vec<Vec<u8>> = (0..ta * td)
                .into_par_iter()
                .map(|i| {
                    let (tx, ty) = (i % ta * tile, i / ta * tile);
                    let mut buf = Vec::with_capacity(tile * tile * cpp * bp);
                    let mut row = Vec::with_capacity(tile * cpp * bp);
                    for y in 0..tile {
                        let sy = (ty + y).min(h - 1);
                        row.clear();
                        for x in 0..tile {
                            let sx = (tx + x).min(w - 1);
                            for s in 0..cpp {
                                let f = v[(sy * w + sx) * cpp + s];
                                if half {
                                    row.extend_from_slice(&f32_to_f16(f).to_be_bytes());
                                } else {
                                    row.extend_from_slice(&f.to_bits().to_be_bytes());
                                }
                            }
                        }
                        buf.extend_from_slice(&float_predict(&row, tile * cpp, bp, cpp));
                    }
                    miniz_oxide::deflate::compress_to_vec_zlib(&buf, DEFLATE_LEVEL)
                })
                .collect();
            ifd.set_image(ImageData::Tiles { tile_width: tile as u32, tile_height: tile as u32, tiles });
        }
        (RawData::F32(v), _) => {
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![32; cpp]));
            ifd.set(t::SAMPLE_FORMAT, Value::Short(vec![3; cpp]));
            ifd.set(t::COMPRESSION, Value::Short(vec![compression::NONE]));
            let mut bytes = Vec::with_capacity(v.len() * 4);
            for &x in v {
                let mut tmp = Vec::with_capacity(4);
                opts.order.put_u32(&mut tmp, x.to_bits());
                bytes.extend_from_slice(&tmp);
            }
            ifd.set_image(ImageData::Strips { rows_per_strip: h as u32, strips: vec![bytes] });
        }
        (RawData::U16(v), DngCompression::LossyJpeg { tile, quality, .. }) => {
            // `lossy_source` made 3-sample 8-bit codes
            let tile = (tile.clamp(16, 4096) & !15) as usize;
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![8; cpp]));
            ifd.set(t::COMPRESSION, Value::Short(vec![compression::LOSSY_JPEG]));
            ifd.set(t::LINEARIZATION_TABLE, Value::Short(lossy_linearization()));
            let quality = quality.clamp(1, 100);
            let (ta, td) = (w.div_ceil(tile), h.div_ceil(tile));
            let tiles: Vec<Vec<u8>> = (0..ta * td)
                .into_par_iter()
                .map(|i| {
                    let (tx, ty) = (i % ta * tile, i / ta * tile);
                    let mut buf = Vec::with_capacity(tile * tile * 3);
                    for y in 0..tile {
                        let sy = (ty + y).min(h - 1);
                        for x in 0..tile {
                            let sx = (tx + x).min(w - 1);
                            for s in 0..3 {
                                buf.push(v.get((sy * w + sx) * cpp + s.min(cpp - 1)).map_or(0, |&c| c.min(255) as u8));
                            }
                        }
                    }
                    let mut out = Vec::new();
                    jpeg_encoder::Encoder::new(&mut out, quality)
                        .encode(&buf, tile as u16, tile as u16, jpeg_encoder::ColorType::Rgb)
                        .map(|()| out)
                        .map_err(|e| RawError::Corrupt(format!("lossy JPEG tile: {e}")))
                })
                .collect::<Result<_>>()?;
            ifd.set_image(ImageData::Tiles { tile_width: tile as u32, tile_height: tile as u32, tiles });
        }
        (RawData::U16(v), _) => {
            let maxv = v.iter().copied().max().unwrap_or(0);
            let bits = (16 - maxv.leading_zeros()).max(raw.bits.min(16)).max(2) as u8;
            ifd.set(
                t::BITS_PER_SAMPLE,
                Value::Short(vec![if matches!(opts.compression, DngCompression::Uncompressed) { 16 } else { bits as u16 }; cpp]),
            );
            match opts.compression {
                DngCompression::Uncompressed => {
                    ifd.set(t::COMPRESSION, Value::Short(vec![compression::NONE]));
                    let rows = (65536 / (w * cpp * 2)).clamp(1, h);
                    let strips = v
                        .chunks(rows * w * cpp)
                        .map(|s| {
                            let mut b = Vec::with_capacity(s.len() * 2);
                            s.iter().for_each(|&x| opts.order.put_u16(&mut b, x));
                            b
                        })
                        .collect();
                    ifd.set_image(ImageData::Strips { rows_per_strip: rows as u32, strips });
                }
                DngCompression::Deflate { tile, .. } => {
                    let tile = deflate_tile(tile);
                    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16; cpp]));
                    ifd.set(t::COMPRESSION, Value::Short(vec![compression::ADOBE_DEFLATE]));
                    ifd.set(t::PREDICTOR, Value::Short(vec![2]));
                    let (ta, td) = (w.div_ceil(tile), h.div_ceil(tile));
                    let tiles: Vec<Vec<u8>> = (0..ta * td)
                        .into_par_iter()
                        .map(|i| {
                            let (tx, ty) = (i % ta * tile, i / ta * tile);
                            let mut buf = Vec::with_capacity(tile * tile * cpp * 2);
                            let mut row = vec![0u16; tile * cpp];
                            for y in 0..tile {
                                let sy = (ty + y).min(h - 1);
                                for x in 0..tile {
                                    let sx = (tx + x).min(w - 1);
                                    for s in 0..cpp {
                                        row[x * cpp + s] = v[(sy * w + sx) * cpp + s];
                                    }
                                }
                                for i in (cpp..row.len()).rev() {
                                    row[i] = row[i].wrapping_sub(row[i - cpp]);
                                }
                                row.iter().for_each(|&x| opts.order.put_u16(&mut buf, x));
                            }
                            miniz_oxide::deflate::compress_to_vec_zlib(&buf, DEFLATE_LEVEL)
                        })
                        .collect();
                    ifd.set_image(ImageData::Tiles { tile_width: tile as u32, tile_height: tile as u32, tiles });
                }
                // handled above (the data is converted first)
                DngCompression::LossyJpeg { .. } => return Err(RawError::Unsupported("lossy DNG of this data".into())),
                DngCompression::Lj92 { tile } => {
                    let tile = (tile.max(2) & !1) as usize;
                    if tile > 65534 {
                        return Err(RawError::Limit("tile too large"));
                    }
                    ifd.set(t::COMPRESSION, Value::Short(vec![compression::JPEG]));
                    let (ta, td) = (w.div_ceil(tile), h.div_ceil(tile));
                    let tiles: Vec<Vec<u8>> = (0..ta * td)
                        .into_par_iter()
                        .map(|i| {
                            let (tx, ty) = (i % ta * tile, i / ta * tile);
                            let mut buf = Vec::with_capacity(tile * tile * cpp);
                            for y in 0..tile {
                                let sy = (ty + y).min(h - 1);
                                for x in 0..tile {
                                    let sx = (tx + x).min(w - 1);
                                    for s in 0..cpp {
                                        buf.push(v[(sy * w + sx) * cpp + s]);
                                    }
                                }
                            }
                            if cpp == 1 {
                                ljpeg::encode(&buf, tile / 2, tile, 2, bits, 1, 0)
                            } else {
                                ljpeg::encode(&buf, tile, tile, cpp, bits, 1, 0)
                            }
                        })
                        .collect();
                    ifd.set_image(ImageData::Tiles { tile_width: tile as u32, tile_height: tile as u32, tiles });
                }
            }
        }
    }
    Ok(TiffWriter::new(opts.order, false).write(&[ifd])?)
}

/// Exif `LightSource` code of a white point the rendered DNG writer knows (D65 = 21, D50 = 23).
fn illuminant_of(white: lightcraft_color::Xy) -> u16 {
    if (white.x - lightcraft_color::D50.x).abs() < 1e-3 && (white.y - lightcraft_color::D50.y).abs() < 1e-3 { 23 } else { 21 }
}

/// Render to DNG: a linear DNG (`LinearRaw`, 16-bit float Deflate tiles) of an already rendered,
/// oriented image. `rgb` is `width × height` interleaved linear RGB in `space` (white = 1; values
/// above 1 are kept up to the half-float maximum, negative and non-finite ones become 0). The colour tags describe `space`
/// exactly — `ColorMatrix1` = XYZ → space, `ForwardMatrix1` = space → XYZ D50, a neutral
/// `AsShotNeutral` — and an identity `ProfileToneCurve` keeps raw converters from adding their
/// default tone curve on top of the baked-in edit.
pub fn write_rendered_dng(
    width: usize,
    height: usize,
    rgb: &[f32],
    space: &lightcraft_color::RgbSpace,
    metadata: &lightcraft_meta::Metadata,
    xmp: Option<String>,
) -> Result<Vec<u8>> {
    let n = width.checked_mul(height).and_then(|p| p.checked_mul(3)).ok_or(RawError::Limit("image too large"))?;
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
        return Err(RawError::Corrupt(format!("cannot write a {width}×{height} DNG")));
    }
    let src = rgb.get(..n).ok_or_else(|| RawError::Corrupt(format!("{} samples for a {width}×{height} RGB image", rgb.len())))?;
    let data: Vec<f32> = src.iter().map(|&v| if v.is_finite() { v.clamp(0.0, 65504.0) } else { 0.0 }).collect();
    let full = crate::Rect::new(0, 0, width, height);
    let forward = lightcraft_color::bradford(space.white, lightcraft_color::D50).mul(&space.to_xyz());
    let color = crate::ColorData {
        illuminant: [illuminant_of(space.white), 0],
        color_matrix: [Some(space.from_xyz()), None],
        forward_matrix: [Some(forward), None],
        as_shot_neutral: Some([1.0; 3]),
        profile: crate::profile::ProfileLook {
            tone_curve: Some(crate::profile::ToneCurve { points: vec![[0.0, 0.0], [1.0, 1.0]] }),
            ..Default::default()
        },
        ..Default::default()
    };
    let raw = RawImage {
        format: crate::RawFormat::Dng,
        width,
        height,
        cpp: 3,
        data: RawData::F32(data),
        cfa: None,
        bits: 16,
        black: crate::BlackLevel::uniform(0.0),
        white: vec![1.0],
        active_area: full,
        crop: full,
        orientation: lightcraft_geom::Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: crate::OpcodeLists::default(),
        metadata: metadata.clone(),
        lens_metadata: Default::default(),
    };
    let order = ByteOrder::Little;
    write_dng(&raw, &DngWriteOptions { compression: DngCompression::Deflate { tile: 256, half: true }, order, xmp })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlackLevel, OpcodeLists, RawData, RawFormat, RawImage, Rect};

    /// Deflate tiles of `tile × tile` pad into a square buffer that grows with the square: a
    /// caller-supplied edge of `u32::MAX` would make the writer pad an 8 × 8 image into billions of
    /// edge-replicate iterations and a multi-gigabyte buffer. The clamp keeps the bounded shape — same
    /// file as requesting the cap exactly, and a DNG either way.
    #[test]
    fn a_hostile_deflate_tile_edge_is_clamped() {
        let (w, h) = (8usize, 8usize);
        let full = Rect::new(0, 0, w, h);
        let raw = RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16((0..64).map(|i| i as u16 * 129).collect()),
            cfa: None,
            bits: 12,
            black: BlackLevel::uniform(0.0),
            white: vec![4095.0],
            active_area: full,
            crop: full,
            orientation: lightcraft_geom::Orientation::Normal,
            color: crate::ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
        };
        let opts = |tile: u32| DngWriteOptions { compression: DngCompression::Deflate { tile, half: false }, ..DngWriteOptions::default() };
        let hostile = write_dng(&raw, &opts(u32::MAX)).expect("the clamped tile writes");
        assert_eq!(hostile, write_dng(&raw, &opts(2048)).expect("the cap itself writes"), "u32::MAX is clamped to the cap");
        assert_ne!(hostile, write_dng(&raw, &opts(256)).expect("a normal tile writes"), "the clamp is not a no-op");
        assert_eq!(crate::probe(&hostile), Some(RawFormat::Dng), "the clamped output is still a DNG");
    }
}
