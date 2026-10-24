//! Fujifilm RAF without a raw IFD: the FinePix bodies from before the X series, most of them SuperCCD.
//!
//! Sources: the ExifTool FujiFilm RAF tag-name documentation (header records `0x0100` RawImageFullSize, `0x0110`
//! RawImageCropTopLeft, `0x0111` RawImageCroppedSize, `0x0121` RawImageSize, `0x0130` FujiLayout, `0x2ff0`
//! WB_GRGBLevels) and our own black-box analysis of CC0 samples from raw.pixls.us (FinePix S2Pro, S3Pro, S5Pro,
//! S5000, S5200, S5500, S6000fd, S6500fd, S7000, S9500, S9600, S100FS, S200EXR), each checked against the camera's
//! own embedded JPEG:
//!
//! - The raw block is exactly `0x0100` (rows, columns) unpacked 16-bit samples: 14-bit little-endian, or 12-bit
//!   big-endian in the oldest body (S2Pro). Read in the wrong order nearly every sample lands at or above 2^14, so the
//!   order that keeps them below decides (the body, when a black or flat frame can't).
//! - Record `0x4000` (eight `u16`) matches the dark floor where the camera left the black level in (S100FS ≈ 514,
//!   S200EXR ≈ 517, S2Pro ≈ 122) and is zero, or a few counts (S5000: 0, 1, 0, 10), where the floor is zero.
//! - The S3Pro and S5Pro files hold a second block of the raw block's size right after it, much darker (median
//!   60–110, at most 8192): most likely their second, low-sensitivity photodiodes. It isn't read: these bodies are
//!   decoded from their primary photodiodes alone.
//! - The first FujiLayout byte tells the layout. 8–11: an ordinary Bayer, RGGB at (0, 0), its crop records in
//!   samples (S5500, S200EXR). Bit 7 set or 0–2: a SuperCCD, whose photosites lie on a square grid turned 45°, so
//!   they fill every other point of a picture grid at half their pitch (a quincunx). `0x0121` and the crop records
//!   count that grid in lines of two rows and in columns.
//! - [`SuperCcd`] says how the stored grid maps onto the picture grid. Each mapping was chosen over its mirror
//!   images by the embedded JPEG, and over the one with the opposite row shift by colour fringes at edges (the wrong
//!   shift puts red and blue half a pitch off green).
//! - Turned back by 45°, the sites form an ordinary Bayer. It is demosaiced there with AHD, and every picture point
//!   between four sites takes their mean. The result is linear camera RGB on the half-pitch grid, the size of the
//!   camera's own JPEGs.

use super::raf::{Header, camera_name, pair, record};
use super::white_from_data;
use crate::{BlackLevel, Cfa, ColorData, Method, Mode, Normalized, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_geom::Orientation;
use rayon::prelude::*;

const FULL_SIZE: u16 = 0x0100;
const CROP_TOP_LEFT: u16 = 0x0110;
const CROPPED_SIZE: u16 = 0x0111;
const RAW_SIZE: u16 = 0x0121;
const LAYOUT: u16 = 0x0130;
const WB_GRGB: u16 = 0x2ff0;
const BLACK: u16 = 0x4000;

/// Bodies in this container whose sensor data none of the layouts here reads: the DBP for GX680 (its CC0 sample
/// matches no SuperCCD mapping) and the first SuperCCD SR bodies, which pair each site with a second, low-sensitivity
/// photodiode and of which no CC0 sample shows how the file stores them. They stay refused rather than misread.
/// Names compare as [`same_body`] does.
const UNDECODED: &[&str] = &["DBP for GX680", "FinePix F700", "FinePix F710", "FinePix S20Pro"];

/// Whether the header's camera name is `body`, whatever the spacing and case: the S2Pro's header (and its Exif
/// `Model`) says `FinePixS2Pro`, the later bodies' `FinePix S3Pro`.
fn same_body(model: &str, body: &str) -> bool {
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    squeeze(model).eq_ignore_ascii_case(&squeeze(body))
}

/// How a SuperCCD's stored grid (row `r`, sample `k`) maps onto the picture's half-pitch grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuperCcd {
    /// One stored row per picture row, its sites two columns apart, odd rows one column to the right (FujiLayout
    /// bit 7: S5000, S5200, S7000, S9500, S9600). Odd rows are green; even rows alternate red and blue, starting
    /// with red on rows 0 mod 4 and blue on rows 2 mod 4.
    Rows,
    /// Each stored row zig-zags down two picture rows, its odd samples one row lower (FujiLayout 0–2: S3Pro, S5Pro,
    /// S100FS, S6000fd, S6500fd). Odd samples are green; even ones alternate red and blue, starting with red on even
    /// rows and blue on odd rows.
    Lines,
    /// [`Self::Lines`] turned a quarter: each stored row zig-zags down two picture columns, the rows running right to
    /// left (FujiLayout 0–2 with rows too short for lines: S2Pro). Colours as in [`Self::Lines`].
    Columns,
}

impl SuperCcd {
    /// Width × height of the picture grid of a `rows × cols` stored grid.
    fn picture_size(self, rows: usize, cols: usize) -> Option<(usize, usize)> {
        match self {
            SuperCcd::Rows => Some((cols.checked_mul(2)?, rows)),
            SuperCcd::Lines => Some((cols, rows.checked_mul(2)?)),
            SuperCcd::Columns => Some((rows.checked_mul(2)?, cols)),
        }
    }

    /// Picture point (x, y) of stored sample (r, k) on a picture `width` wide.
    fn place(self, r: usize, k: usize, width: usize) -> (usize, usize) {
        match self {
            SuperCcd::Rows => (2 * k + (r & 1), r),
            SuperCcd::Lines => (k, 2 * r + (k & 1)),
            SuperCcd::Columns => (width.saturating_sub(1 + 2 * r + (k & 1)), k),
        }
    }

    /// Colour (0 = R, 1 = G, 2 = B) of stored sample (r, k).
    fn colour(self, r: usize, k: usize) -> u8 {
        let red = match self {
            SuperCcd::Rows if r % 2 == 1 => return 1,
            SuperCcd::Rows => r.is_multiple_of(4) == k.is_multiple_of(2),
            SuperCcd::Lines | SuperCcd::Columns if k % 2 == 1 => return 1,
            SuperCcd::Lines | SuperCcd::Columns => r.is_multiple_of(2) == k.is_multiple_of(4),
        };
        if red { 0 } else { 2 }
    }
}

/// The sites of a picture grid turned 45°: picture point (x, y) with `x + y ≡ parity (mod 2)` lands on point
/// (u, v) = ((x + y − parity) / 2, (x − y − parity) / 2 + offset) of a square grid `width × height`.
struct Turned {
    parity: usize,
    offset: usize,
    width: usize,
    height: usize,
}

impl Turned {
    /// The turned grid of a `picture_width × picture_height` picture whose sites have `x + y ≡ parity (mod 2)`.
    fn new(picture_width: usize, picture_height: usize, parity: usize) -> Turned {
        let offset = picture_height.div_ceil(2);
        Turned { parity, offset, width: (picture_width + picture_height) / 2 + 1, height: (picture_width + 2 * offset) / 2 + 1 }
    }

    /// The cell (row-major) of site (x, y) of the picture.
    fn index(&self, x: usize, y: usize) -> usize {
        let u = (x + y).saturating_sub(self.parity) / 2;
        let v = (x + 2 * self.offset).saturating_sub(y + self.parity) / 2;
        v * self.width + u
    }
}

/// A SuperCCD picture: its size, its sites turned 45° and the Bayer they form there.
struct Geometry {
    s: SuperCcd,
    width: usize,
    height: usize,
    turned: Turned,
    cfa: Cfa,
}

impl SuperCcd {
    /// The geometry of a `rows × cols` stored grid, checked in every mode, so a probe refuses what a decode would.
    fn geometry(self, rows: usize, cols: usize) -> Result<Geometry> {
        if rows < 4 || cols < 4 {
            return Err(RawError::Corrupt(format!("Fujifilm SuperCCD RAF of {cols}x{rows} samples")));
        }
        let (width, height) = self.picture_size(rows, cols).ok_or(RawError::Limit("image too large"))?;
        width.checked_mul(height).and_then(|p| p.checked_mul(3)).filter(|p| *p <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
        let (x0, y0) = self.place(0, 0, width);
        let turned = Turned::new(width, height, (x0 + y0) % 2);
        // the turned grid holds about twice the sites of a 3:2 picture (4× at 5.8:1); a thin one would make it huge
        let sites = rows.saturating_mul(cols);
        turned
            .width
            .checked_mul(turned.height)
            .filter(|cells| *cells <= sites.saturating_mul(4) && *cells <= crate::MAX_SAMPLES)
            .ok_or(RawError::Limit("Fujifilm SuperCCD picture too narrow for its size"))?;
        // the Bayer the turned sites form, read off the first 4 × 4 samples
        let mut pattern = [None::<u8>; 4];
        for r in 0..4 {
            for k in 0..4 {
                let (x, y) = self.place(r, k, width);
                let i = turned.index(x, y);
                let phase = (i / turned.width % 2) * 2 + i % turned.width % 2;
                let c = self.colour(r, k);
                match pattern.get_mut(phase) {
                    Some(p @ None) => *p = Some(c),
                    Some(Some(p)) if *p == c => {}
                    _ => return Err(RawError::Unsupported("Fujifilm SuperCCD colour layout".into())),
                }
            }
        }
        let [Some(c0), Some(c1), Some(c2), Some(c3)] = pattern else {
            return Err(RawError::Unsupported("Fujifilm SuperCCD colour layout".into()));
        };
        let cfa = Cfa { width: 2, height: 2, pattern: vec![c0, c1, c2, c3] };
        if !cfa.is_bayer() {
            return Err(RawError::Unsupported(format!("Fujifilm SuperCCD colour layout {}", cfa.name())));
        }
        Ok(Geometry { s: self, width, height, turned, cfa })
    }
}

/// The geometry the FujiLayout record and the sizes describe.
enum Layout {
    Bayer,
    SuperCcd(SuperCcd),
}

/// The arrangement the FujiLayout record gives a `rows × cols` stored grid: 8–11 a Bayer, bit 7 SuperCCD rows, 0–2
/// SuperCCD lines when RawImageSize's lines fit the stored rows, or columns when they fit them turned a quarter.
fn layout(h: &Header, rows: usize, cols: usize) -> Result<Layout> {
    let first = record(h, LAYOUT).and_then(|l| l.first().copied());
    let lines = || pair(h, RAW_SIZE).filter(|&(l, w)| l > 0 && w > 0);
    match first {
        Some(8..=11) => Ok(Layout::Bayer),
        Some(b) if b & 0x80 != 0 => Ok(Layout::SuperCcd(SuperCcd::Rows)),
        Some(0..=2) => match lines() {
            Some((l, w)) if cols >= w && rows >= l => Ok(Layout::SuperCcd(SuperCcd::Lines)),
            Some((l, w)) if rows * 2 >= w && cols >= l * 2 => Ok(Layout::SuperCcd(SuperCcd::Columns)),
            _ => Err(RawError::Unsupported(format!("Fujifilm SuperCCD RAF of {cols}x{rows} samples in an unknown arrangement"))),
        },
        Some(b) => Err(RawError::Unsupported(format!("Fujifilm RAF sensor layout {b:#04x}"))),
        None => Err(RawError::Unsupported("RAF without a raw IFD or a sensor layout record (older FinePix layout)".into())),
    }
}

/// Whether the samples are big-endian: the reading that leaves fewer of them at or above 2^14. When both leave as
/// many (every sample fits read either way, as in a black or flat frame), the body decides: the S2Pro, the one
/// big-endian body known, or little-endian for the rest.
fn big_endian(src: &[u8], model: &str) -> bool {
    let words = src.as_chunks::<2>().0;
    let step = (words.len() / 65_536).max(1);
    let (mut le, mut be) = (0usize, 0usize);
    for w in words.iter().step_by(step) {
        le += usize::from(u16::from_le_bytes(*w) >= 1 << 14);
        be += usize::from(u16::from_be_bytes(*w) >= 1 << 14);
    }
    if be == le { same_body(model, "FinePix S2Pro") } else { be < le }
}

/// The mean of a record of big-endian `u16`s.
fn mean_u16(d: &[u8]) -> Option<f32> {
    let v = d.as_chunks::<2>().0;
    (!v.is_empty()).then(|| v.iter().map(|w| f32::from(u16::from_be_bytes(*w))).sum::<f32>() / v.len() as f32)
}

/// Decode a pre-X FinePix RAF from its header `h` and its raw block `raw` of bare 16-bit samples: a Bayer as it is
/// stored, a SuperCCD as linear camera RGB on its picture grid ([`develop`]). The bodies in [`UNDECODED`] and raw
/// blocks that aren't exactly the recorded size are refused.
pub(super) fn decode(bytes: &[u8], h: &Header, raw: &[u8], mode: Mode) -> Result<RawImage> {
    let model = camera_name(bytes);
    if UNDECODED.iter().any(|body| same_body(&model, body)) {
        return Err(RawError::Unsupported(format!("Fujifilm {model} RAF (its sensor layout is not decoded yet)")));
    }
    // every body with bare samples records RawImageSize; the TIFF-wrapped ones don't, so a damaged one of those
    // stays refused instead of being read as samples
    if record(h, RAW_SIZE).is_none() {
        return Err(RawError::Unsupported("RAF without a raw IFD (older FinePix layout)".into()));
    }
    let (rows, cols) = pair(h, FULL_SIZE)
        .filter(|&(r, c)| r > 0 && c > 0)
        .ok_or_else(|| RawError::Unsupported("RAF without a raw IFD or a sensor size record (older FinePix layout)".into()))?;
    let n = rows.checked_mul(cols).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let need = n.checked_mul(2).ok_or(RawError::Limit("image too large"))?;
    let Some(src) = raw.get(..need) else {
        return Err(RawError::Unsupported(format!("Fujifilm RAF packing ({} bytes for {cols}x{rows} 16-bit samples)", raw.len())));
    };
    // every sample's raw block is exactly its size; a longer one holds data no sample shows (the S3Pro / S5Pro second
    // block comes after the raw block, not in it), and decoding just its first part would drop the rest silently
    if raw.len() > need {
        return Err(RawError::Unsupported(format!(
            "Fujifilm RAF whose raw block is longer than its size records ({} bytes for {cols}x{rows} 16-bit samples)",
            raw.len()
        )));
    }
    let layout = layout(h, rows, cols)?;
    let big = big_endian(src, &model);
    let bits = if big { 12 } else { 14 };
    let black = record(h, BLACK).and_then(mean_u16).unwrap_or(0.0);
    let wb = record(h, WB_GRGB).and_then(|d| {
        let &[g0, g1, r0, r1, _, _, b0, b1, ..] = d else { return None };
        let [g, r, b] = [[g0, g1], [r0, r1], [b0, b1]].map(|w| f32::from(u16::from_be_bytes(w)));
        (g > 0.0 && r > 0.0 && b > 0.0).then(|| [r / g, 1.0, b / g])
    });
    let unpack = || -> Vec<u16> {
        let words = src.as_chunks::<2>().0;
        if big { words.par_iter().map(|w| u16::from_be_bytes(*w)).collect() } else { words.par_iter().map(|w| u16::from_le_bytes(*w)).collect() }
    };
    let (top, left) = pair(h, CROP_TOP_LEFT).unwrap_or((0, 0));
    let cropped = pair(h, CROPPED_SIZE).filter(|&(ch, cw)| ch > 0 && cw > 0);

    let mut metadata = h.jpeg.map(lightcraft_meta::extract).unwrap_or_default();
    let orientation = metadata.orientation.unwrap_or(Orientation::Normal);
    let (width, height, cpp, cfa, data, white, active, black) = match layout {
        Layout::Bayer => {
            let data = if mode == Mode::Full { unpack() } else { Vec::new() };
            let white = white_from_data(&data, bits);
            let active = cropped.map_or(Rect::new(0, 0, cols, rows), |(ch, cw)| Rect::new(left, top, cw, ch).clipped(cols, rows));
            (cols, rows, 1, Some(Cfa::bayer_static("RGGB")), RawData::U16(data), white, active, BlackLevel::uniform(black))
        }
        Layout::SuperCcd(s) => {
            let g = s.geometry(rows, cols)?;
            let (pw, ph) = (g.width, g.height);
            let data = if mode == Mode::Full {
                let samples = unpack();
                let white = white_from_data(&samples, bits);
                develop(g, &samples, cols, black, white)
            } else {
                Vec::new()
            };
            // the crop records count lines of two picture rows
            let active = cropped.map_or(Rect::new(0, 0, pw, ph), |(ch, cw)| Rect::new(left, 2 * top, cw, 2 * ch).clipped(pw, ph));
            (pw, ph, 3, None, RawData::F32(data), 1.0, active, BlackLevel::uniform(0.0))
        }
    };
    let active = if active.width == 0 || active.height == 0 { Rect::new(0, 0, width, height) } else { active };
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let img = RawImage {
        format: RawFormat::Raf,
        width,
        height,
        cpp,
        data,
        cfa,
        bits,
        black,
        white: vec![white],
        active_area: active,
        crop: Rect::new(0, 0, active.width, active.height),
        orientation,
        color: ColorData::default(),
        wb_multipliers: wb,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
    };
    img.validate_for(mode)?;
    Ok(img)
}

/// SuperCCD samples (`cols` to a stored row) → linear camera RGB on the picture's half-pitch grid (row-major, 3 values
/// a point, white 1.0): the sites turned 45° into a Bayer, demosaiced, and turned back; a point between four sites is
/// their mean.
fn develop(g: Geometry, samples: &[u16], cols: usize, black: f32, white: f32) -> Vec<f32> {
    let Geometry { s, width: pw, height: ph, turned, cfa } = g;
    let range = white - black;
    let scale = if range > 0.0 { 1.0 / range } else { 1.0 };
    let mut mosaic = vec![0f32; turned.width * turned.height];
    // the sites of each turned row are one run of cells; the cells either side of it lie outside the picture
    let (mut first, mut last) = (vec![usize::MAX; turned.height], vec![0usize; turned.height]);
    for (r, row) in samples.chunks(cols).enumerate() {
        for (k, &v) in row.iter().enumerate() {
            let (x, y) = s.place(r, k, pw);
            let i = turned.index(x, y);
            if let Some(m) = mosaic.get_mut(i) {
                *m = (f32::from(v) - black) * scale;
                let (tv, u) = (i / turned.width, i % turned.width);
                if let (Some(a), Some(b)) = (first.get_mut(tv), last.get_mut(tv)) {
                    (*a, *b) = ((*a).min(u), (*b).max(u));
                }
            }
        }
    }
    // repeat each run's end sites outwards (keeping the Bayer phase), so the demosaic doesn't darken the picture's
    // edges by reading the empty corners of the turned grid as black
    mosaic.par_chunks_mut(turned.width).zip(first.par_iter().zip(&last)).for_each(|(row, (&a, &b))| {
        if a >= b {
            return;
        }
        for u in 0..row.len() {
            let from = if u < a {
                a + ((a - u) & 1)
            } else if u > b {
                b - ((u - b) & 1)
            } else {
                continue;
            };
            if let Some(v) = row.get(from).copied()
                && let Some(cell) = row.get_mut(u)
            {
                *cell = v;
            }
        }
    });
    let rgb = crate::demosaic(&Normalized { width: turned.width, height: turned.height, cpp: 1, data: mosaic, cfa: Some(cfa) }, Method::Ahd);
    let at = |x: usize, y: usize| rgb.data.get(turned.index(x, y)).copied();
    let mut out = vec![0f32; pw * ph * 3];
    out.par_chunks_mut(pw * 3).enumerate().for_each(|(y, line)| {
        for (x, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let v = if (x + y) % 2 == turned.parity {
                at(x, y).unwrap_or_default()
            } else {
                let near = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
                let (sum, n) = near
                    .iter()
                    .filter(|&&(nx, ny)| nx < pw && ny < ph)
                    .filter_map(|&(nx, ny)| at(nx, ny))
                    .fold(([0f32; 3], 0f32), |(s, n), c| ([s[0] + c[0], s[1] + c[1], s[2] + c[2]], n + 1.0));
                if n > 0.0 { sum.map(|c| c / n) } else { [0.0; 3] }
            };
            *px = v;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pre-X FinePix RAF: header, records (big-endian, as the cameras write them), bare 16-bit samples.
    fn legacy(model: &str, samples: &[u16], big: bool, records: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut dir = (records.len() as u32).to_be_bytes().to_vec();
        for (tag, d) in records {
            dir.extend_from_slice(&tag.to_be_bytes());
            dir.extend_from_slice(&(d.len() as u16).to_be_bytes());
            dir.extend_from_slice(d);
        }
        let block: Vec<u8> = samples.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
        let jpeg = b"\xff\xd8\xff\xd9";
        let mut out = b"FUJIFILMCCD-RAW 0201FF000000".to_vec();
        out.extend_from_slice(model.as_bytes());
        out.resize(60, 0);
        out.extend_from_slice(b"0100");
        out.resize(160, 0);
        let (jpeg_off, dir_off) = (160u32, 160 + jpeg.len() as u32);
        let raw_off = dir_off + dir.len() as u32;
        for (i, v) in [jpeg_off, jpeg.len() as u32, dir_off, dir.len() as u32, raw_off, block.len() as u32].iter().enumerate() {
            out[84 + i * 4..88 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(jpeg);
        out.extend_from_slice(&dir);
        out.extend_from_slice(&block);
        out
    }

    /// `v` as big-endian bytes, as the records hold them.
    fn u16s(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// The records of a `rows × cols` file with `layout` and the given crop (top, left, height, width) and black.
    fn records(rows: usize, cols: usize, layout: u8, size: (u16, u16), crop: [u16; 4], black: u16) -> Vec<(u16, Vec<u8>)> {
        vec![
            (FULL_SIZE, u16s(&[rows as u16, cols as u16])),
            (CROP_TOP_LEFT, u16s(&crop[..2])),
            (CROPPED_SIZE, u16s(&crop[2..])),
            (RAW_SIZE, u16s(&[size.0, size.1])),
            (LAYOUT, vec![layout, layout, layout, layout]),
            (WB_GRGB, u16s(&[80, 160, 80, 120])),
            (BLACK, u16s(&[black; 8])),
        ]
    }

    /// A Bayer body's bare samples come back as they were stored, in either byte order, with the crop, black level and
    /// white balance its records give.
    #[test]
    fn bayer_layout_reads_bare_samples_in_either_byte_order() {
        let (rows, cols) = (8usize, 12usize);
        let px: Vec<u16> = (0..rows * cols).map(|i| (i * 37 % 3900 + 100) as u16).collect();
        let recs = records(rows, cols, 0x0a, (8, 12), [2, 2, 4, 8], 100);
        for (big, bits) in [(false, 14), (true, 12)] {
            let bytes = legacy("FinePix S5500", &px, big, &recs);
            assert_eq!(crate::probe(&bytes), Some(RawFormat::Raf));
            let r = crate::decode(&bytes).unwrap();
            assert_eq!(crate::probe_info(&bytes).unwrap(), r.info());
            assert_eq!(r.data, RawData::U16(px.clone()), "big-endian: {big}");
            assert_eq!((r.cpp, r.bits, r.cfa.as_ref().map(Cfa::name)), (1, bits, Some("RGGB".into())));
            assert_eq!(r.active_area, Rect::new(2, 2, 8, 4));
            assert_eq!(r.black.mean(), 100.0);
            assert_eq!(r.wb_multipliers, Some([2.0, 1.0, 1.5]));
        }
    }

    /// Samples that fit in 14 bits read either way (a flat frame of 0x0020: 32 big-endian, 8192 little-endian) can't
    /// tell their byte order: the S2Pro's is big-endian, every other body's little-endian. The S2Pro's header (and its
    /// Exif `Model`) says `FinePixS2Pro`, without the space the later bodies have.
    #[test]
    fn samples_that_cannot_tell_their_byte_order_take_the_bodys() {
        let px = vec![32u16; 8 * 12];
        let recs = records(8, 12, 0x0a, (8, 12), [0, 0, 8, 12], 0);
        for name in ["FinePixS2Pro", "FinePix S2Pro"] {
            let r = crate::decode(&legacy(name, &px, true, &recs)).unwrap();
            assert_eq!((&r.data, r.bits), (&RawData::U16(px.clone()), 12), "{name}");
        }
        let r = crate::decode(&legacy("FinePix S5500", &px, true, &recs)).unwrap();
        assert_eq!((r.data, r.bits), (RawData::U16(vec![0x2000; 8 * 12]), 14));
        // when the samples can tell, they decide, whatever the body
        let px: Vec<u16> = (0..96).map(|i| (i * 37 % 3900 + 100) as u16).collect();
        assert_eq!(crate::decode(&legacy("FinePix S2Pro", &px, false, &recs)).unwrap().data, RawData::U16(px));
    }

    /// A SuperCCD file whose sites sample `f` at their picture points, with each colour scaled by `gain`.
    fn superccd(s: SuperCcd, rows: usize, cols: usize, f: impl Fn(usize, usize) -> f32, gain: [f32; 3]) -> Vec<u8> {
        let (pw, _) = s.picture_size(rows, cols).unwrap();
        let (black, white) = (64.0, 16383.0);
        let px: Vec<u16> = (0..rows * cols)
            .map(|i| {
                let (r, k) = (i / cols, i % cols);
                let (x, y) = s.place(r, k, pw);
                (black + f(x, y) * gain[s.colour(r, k) as usize] * (white - black)).round() as u16
            })
            .collect();
        let (layout, size) = match s {
            SuperCcd::Rows => (0x80, ((rows / 2) as u16, (cols * 2) as u16)),
            SuperCcd::Lines => (0x01, (rows as u16, cols as u16)),
            SuperCcd::Columns => (0x00, ((cols / 2) as u16, (rows * 2) as u16)),
        };
        legacy("FinePix TEST", &px, false, &records(rows, cols, layout, size, [2, 4, size.0 - 4, size.1 - 8], 64))
    }

    /// (arrangement, stored rows, stored columns); the columns' rows are too short to be lines, as in the S2Pro.
    const ARRANGEMENTS: [(SuperCcd, usize, usize); 3] = [(SuperCcd::Rows, 40, 20), (SuperCcd::Lines, 20, 40), (SuperCcd::Columns, 24, 36)];

    /// Every picture point away from the edges takes the value its site (or the mean of its four neighbours) was
    /// given: a grey ramp, which every demosaic reproduces, pins where each stored sample lands.
    #[test]
    fn superccd_samples_land_on_their_picture_points() {
        let f = |x: usize, y: usize| 0.05 + x as f32 * 0.004 + y as f32 * 0.009;
        for (s, rows, cols) in ARRANGEMENTS {
            let bytes = superccd(s, rows, cols, f, [1.0; 3]);
            let r = crate::decode(&bytes).unwrap_or_else(|e| panic!("{s:?}: {e}"));
            assert_eq!(crate::probe_info(&bytes).unwrap(), r.info(), "{s:?}");
            let (pw, ph) = s.picture_size(rows, cols).unwrap();
            assert_eq!((r.width, r.height, r.cpp, r.cfa.is_none()), (pw, ph, 3, true), "{s:?}");
            // two lines and four columns off the top left, as many off the bottom right
            assert_eq!(r.active_area, Rect::new(4, 4, pw - 8, ph - 8), "{s:?}");
            let RawData::F32(v) = &r.data else { panic!("{s:?}: float samples expected") };
            for y in 6..ph - 6 {
                for x in 6..pw - 6 {
                    for c in 0..3 {
                        let got = v[(y * pw + x) * 3 + c];
                        assert!((got - f(x, y)).abs() < 2e-4, "{s:?} ({x}, {y}) channel {c}: {got} vs {}", f(x, y));
                    }
                }
            }
        }
    }

    /// Each stored sample's colour is the one the demosaic is told: a flat colour comes back as itself (its level
    /// scaled, as its brightest channel reads as the white level), up to the picture's edges, where the demosaic reads
    /// the turned grid's empty corners (only the four corners themselves, the tips of that grid, fall short).
    #[test]
    fn superccd_colours_reach_their_channels() {
        let gain = [0.2, 0.5, 0.8];
        for (s, rows, cols) in ARRANGEMENTS {
            let r = crate::decode(&superccd(s, rows, cols, |_, _| 1.0, gain)).unwrap();
            let (pw, ph) = (r.width, r.height);
            let RawData::F32(v) = &r.data else { panic!("float samples expected") };
            for y in 0..ph {
                for x in 0..pw {
                    if (x < 4 || x >= pw - 4) && (y < 4 || y >= ph - 4) {
                        continue;
                    }
                    let px = &v[(y * pw + x) * 3..][..3];
                    let ratios = [px[0] / px[1], px[2] / px[1]];
                    assert!((ratios[0] - 0.4).abs() < 1e-3 && (ratios[1] - 1.6).abs() < 1e-3, "{s:?} ({x}, {y}): {px:?}");
                }
            }
        }
    }

    /// The mappings the stored grids use: the 4 × 4 corner of each, as picture points.
    #[test]
    fn superccd_mappings_put_sites_on_a_quincunx() {
        for (s, rows, cols) in ARRANGEMENTS {
            let (pw, ph) = s.picture_size(rows, cols).unwrap();
            let mut seen = vec![false; pw * ph];
            for r in 0..rows {
                for k in 0..cols {
                    let (x, y) = s.place(r, k, pw);
                    assert!(x < pw && y < ph, "{s:?} ({r}, {k}) → ({x}, {y})");
                    assert!(!std::mem::replace(&mut seen[y * pw + x], true), "{s:?}: two samples at ({x}, {y})");
                }
            }
            // half the points are sites, and every other point of each row
            assert_eq!(seen.iter().filter(|&&b| b).count(), pw * ph / 2, "{s:?}");
            assert!(seen.chunks(pw).all(|row| row.windows(2).all(|w| w[0] != w[1])), "{s:?}");
        }
        assert_eq!(SuperCcd::Columns.place(0, 0, 40), (39, 0));
        assert_eq!(SuperCcd::Columns.place(0, 1, 40), (38, 1));
        assert_eq!(SuperCcd::Rows.place(1, 0, 40), (1, 1));
        assert_eq!(SuperCcd::Lines.place(0, 1, 40), (1, 1));
        // the colours the CC0 samples showed (0 = R, 1 = G, 2 = B): the first stored rows, samples 0..4
        let row = |s: SuperCcd, r: usize| (0..4).map(|k| s.colour(r, k)).collect::<Vec<_>>();
        assert_eq!([row(SuperCcd::Rows, 0), row(SuperCcd::Rows, 1), row(SuperCcd::Rows, 2)], [[0, 2, 0, 2], [1; 4], [2, 0, 2, 0]]);
        for s in [SuperCcd::Lines, SuperCcd::Columns] {
            assert_eq!([row(s, 0), row(s, 1)], [[0, 1, 2, 1], [2, 1, 0, 1]], "{s:?}");
        }
    }

    /// Damaged, inconsistent and refused files are errors in a probe as in a decode: never a panic, never a misread.
    #[test]
    fn malformed_legacy_rafs_are_errors() {
        let px = vec![1000u16; 8 * 12];
        let good = records(8, 12, 0x0a, (8, 12), [0, 0, 8, 12], 0);
        let with = |f: &dyn Fn(&mut Vec<(u16, Vec<u8>)>)| {
            let mut r = good.clone();
            f(&mut r);
            r
        };
        let set = |tag: u16, d: Vec<u8>| move |r: &mut Vec<(u16, Vec<u8>)>| r.iter_mut().filter(|e| e.0 == tag).for_each(|e| e.1 = d.clone());
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("no size", legacy("FinePix X", &px, false, &with(&|r| r.retain(|e| e.0 != FULL_SIZE)))),
            ("zero size", legacy("FinePix X", &px, false, &with(&set(FULL_SIZE, u16s(&[0, 12]))))),
            ("size past the data", legacy("FinePix X", &px, false, &with(&set(FULL_SIZE, u16s(&[9, 12]))))),
            ("huge size", legacy("FinePix X", &px, false, &with(&set(FULL_SIZE, u16s(&[0xffff, 0xffff]))))),
            ("twice the data", legacy("FinePix X", &[px.clone(), px.clone()].concat(), false, &good)),
            ("a sample more than its size", legacy("FinePix X", &[px.clone(), vec![1000]].concat(), false, &good)),
            ("no layout", legacy("FinePix X", &px, false, &with(&|r| r.retain(|e| e.0 != LAYOUT)))),
            ("unknown layout", legacy("FinePix X", &px, false, &with(&set(LAYOUT, vec![0x40; 4])))),
            // what a TIFF-wrapped RAF whose TIFF is damaged looks like here
            ("no RawImageSize", legacy("FinePix X", &px, false, &with(&|r| r.retain(|e| e.0 != RAW_SIZE)))),
            (
                "lines of no size",
                legacy(
                    "FinePix X",
                    &px,
                    false,
                    &with(&|r| {
                        set(LAYOUT, vec![1; 4])(r);
                        set(RAW_SIZE, u16s(&[0, 0]))(r);
                    }),
                ),
            ),
            // 4 × 32000 samples (256 kB) would turn into a 32003² grid (4 GB of floats, 12 GB demosaiced)
            (
                "a thin SuperCCD",
                legacy(
                    "FinePix X",
                    &vec![1000u16; 4 * 32_000],
                    false,
                    &with(&|r| {
                        set(LAYOUT, vec![0x80; 4])(r);
                        set(FULL_SIZE, u16s(&[4, 32_000]))(r);
                        set(RAW_SIZE, u16s(&[2, 64_000]))(r);
                    }),
                ),
            ),
            (
                "lines that fit no arrangement",
                legacy(
                    "FinePix X",
                    &px,
                    false,
                    &with(&|r| {
                        set(LAYOUT, vec![1; 4])(r);
                        set(RAW_SIZE, u16s(&[100, 100]))(r);
                    }),
                ),
            ),
            (
                "tiny SuperCCD",
                legacy(
                    "FinePix X",
                    &px[..6],
                    false,
                    &with(&|r| {
                        set(LAYOUT, vec![0x80; 4])(r);
                        set(FULL_SIZE, u16s(&[2, 3]))(r);
                    }),
                ),
            ),
            ("an undecoded body", legacy("DBP for GX680", &px, false, &good)),
            // a header may spell the name with or without the space before the model (the S2Pro's has none)
            ("an undecoded body, spaced", legacy("FinePix S20Pro", &px, false, &good)),
            ("an undecoded body, unspaced", legacy("FinePixS20Pro", &px, false, &good)),
            ("an undecoded body, unspaced F700", legacy("FinePixF700", &px, false, &good)),
        ];
        for (what, bytes) in cases {
            assert!(crate::decode(&bytes).is_err(), "{what}");
            assert!(crate::probe_info(&bytes).is_err(), "{what}");
        }
        // the records themselves may be cut short or out of range: no record makes a decode panic
        let bytes = legacy("FinePix X", &px, false, &with(&set(LAYOUT, vec![0x80; 4])));
        let dir = be(&bytes, 92);
        for at in dir..dir + 4 * 14 {
            for v in [0u8, 0x7f, 0xff] {
                let mut b = bytes.clone();
                b[at] = v;
                let _ = crate::decode(&b);
                let _ = crate::probe_info(&b);
            }
        }
        // a crop reaching past the picture is clipped to it
        let crop = with(&|r| {
            set(LAYOUT, vec![0x80; 4])(r);
            set(CROPPED_SIZE, u16s(&[0xffff, 0xffff]))(r);
        });
        let r = crate::probe_info(&legacy("FinePix X", &px, false, &crop)).unwrap();
        assert_eq!(r.active_area, Rect::new(0, 0, 24, 8));
    }

    /// The big-endian `u32` at `at`: a header pointer.
    fn be(b: &[u8], at: usize) -> usize {
        u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
    }
}
