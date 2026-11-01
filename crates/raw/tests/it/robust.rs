//! Malformed-input robustness: decoding random or corrupted files must never panic.

use lightcraft_raw::{
    BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, Method, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect,
};
use proptest::prelude::*;

fn sample(comp: DngCompression) -> Vec<u8> {
    let (w, h) = (24usize, 16usize);
    let data: Vec<u16> = (0..w * h).map(|i| (i * 97 % 4000) as u16).collect();
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 12,
        black: BlackLevel::uniform(64.0),
        white: vec![4095.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    lightcraft_raw::write_dng(&raw, &DngWriteOptions { compression: comp, ..Default::default() }).unwrap()
}

fn exercise(bytes: &[u8]) {
    let _ = lightcraft_raw::probe(bytes);
    let _ = lightcraft_raw::embedded_preview(bytes);
    if let Ok(img) = lightcraft_raw::decode(bytes)
        && img.width * img.height <= 1 << 16
    {
        let _ = img.develop(Method::Ahd);
        let xy = lightcraft_raw::color::as_shot_white_xy(&img);
        let _ = lightcraft_raw::color::camera_transform(&img, xy);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, .. ProptestConfig::default() })]

    #[test]
    fn mutated_dngs_never_panic(flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..24), cut in any::<usize>(), lj in any::<bool>()) {
        let mut data = sample(if lj { DngCompression::Lj92 { tile: 16 } } else { DngCompression::Uncompressed });
        for (i, v) in flips {
            let n = data.len();
            data[i % n] = v;
        }
        let keep = if cut % 3 == 0 { cut % data.len() } else { data.len() };
        exercise(&data[..keep]);
    }

    #[test]
    fn random_tiff_like_bytes_never_panic(mut data in proptest::collection::vec(any::<u8>(), 8..700), kind in 0usize..4) {
        let heads: [&[u8]; 4] = [b"II*\0\x08\0\0\0", b"MM\0*\0\0\0\x08", b"II*\0\x10\0\0\0CR\x02\0", b"II+\0\x08\0\0\0"];
        let h = heads[kind];
        let n = h.len().min(data.len());
        data[..n].copy_from_slice(&h[..n]);
        exercise(&data);
    }

    #[test]
    fn random_lj92_never_panics(data in proptest::collection::vec(any::<u8>(), 0..400)) {
        let mut s = vec![0xff, 0xd8, 0xff, 0xc3, 0, 11, 12, 0, 8, 0, 8, 1, 1, 0x11, 0];
        s.extend_from_slice(&data);
        let _ = lightcraft_raw::ljpeg::decode(&s, 1 << 20);
    }
}

#[test]
fn truncation_at_every_length() {
    for comp in
        [DngCompression::Uncompressed, DngCompression::Lj92 { tile: 16 }, DngCompression::LossyJpeg { tile: 16, quality: 80, long_edge: None }]
    {
        let d = sample(comp);
        for n in (0..d.len()).step_by(7) {
            exercise(&d[..n]);
        }
    }
}

#[test]
fn hostile_dimensions_are_rejected_quickly() {
    // a DNG claiming a 60000 × 60000 16-bit image with 10 bytes of data
    use lightcraft_tiff::tags as t;
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
    let mut ifd = IfdBuilder::new();
    ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![60000]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![60000]));
    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![32803]));
    ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    ifd.set(t::COMPRESSION, Value::Short(vec![7]));
    ifd.set_image(ImageData::Tiles { tile_width: 60000, tile_height: 60000, tiles: vec![vec![0xff, 0xd8, 0xff, 0xd9]] });
    let bytes = TiffWriter::default().write(&[ifd]).unwrap();
    let t0 = std::time::Instant::now();
    assert!(lightcraft_raw::decode(&bytes).is_err());
    assert!(t0.elapsed().as_secs_f64() < 2.0);
}

/// A Bayer DNG whose `BlackLevel` is a FLOAT holding NaN (or infinity), and a float CFA DNG with
/// NaN samples: every normalised sample came out NaN, and AHD's green estimate clamped between two
/// NaN neighbours, which panics (the 1:1 view and every export of such a file). Non-finite levels
/// read as 0 and non-finite samples as black: the photo develops to finite numbers.
#[test]
fn non_finite_black_levels_and_samples_develop() {
    use lightcraft_tiff::tags as t;
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
    let (w, h) = (32u32, 16u32);
    for black in [Value::Float(vec![f32::NAN]), Value::Double(vec![f64::INFINITY]), Value::Float(vec![64.0, f32::NAN, 64.0, 64.0])] {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
        ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        ifd.set(t::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![2, 2]));
        ifd.set(t::BLACK_LEVEL, black.clone());
        ifd.set(t::WHITE_LEVEL, Value::Long(vec![4095]));
        let strip: Vec<u8> = (0..w * h).flat_map(|i| (100 + (i * 37 % 3000) as u16).to_le_bytes()).collect();
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
        let bytes = TiffWriter::default().write(&[ifd]).unwrap();
        let img = lightcraft_raw::decode(&bytes).unwrap_or_else(|e| panic!("{black:?}: {e}"));
        let rgb = img.develop(Method::Ahd).unwrap();
        assert!(rgb.data.iter().flatten().all(|v| v.is_finite()), "{black:?}: finite output");
        let _ = img.develop(Method::Ppg);
    }
    // float samples: a block of NaN and infinities in the middle of the mosaic
    let (w, h) = (24usize, 16usize);
    let data: Vec<f32> = (0..w * h)
        .map(|i| match (i % w, i / w) {
            (8..=13, 5..=10) => f32::NAN,
            (2, _) => f32::INFINITY,
            _ => (i * 97 % 1000) as f32 / 1000.0,
        })
        .collect();
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::F32(data),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 32,
        black: BlackLevel::uniform(0.0),
        white: vec![1.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
        pixel_aspect: 1.0,
    };
    for img in [raw.clone(), lightcraft_raw::decode(&lightcraft_raw::write_dng(&raw, &DngWriteOptions::default()).unwrap()).unwrap()] {
        let rgb = img.develop(Method::Ahd).unwrap();
        assert!(rgb.data.iter().flatten().all(|v| v.is_finite()), "float NaN samples: finite output");
    }
}

// --- CR3 containers (Canon CRX): structurally valid files whose every byte may be mutated ---

/// A CR3 with a single CRAW track carrying a CMP1 descriptor (and optional CDI1/IAD1) plus raw samples.
mod cr3 {
    pub(crate) fn box_bytes(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn track(entry: &[u8], offset: u64, size: u32) -> Vec<u8> {
        let mut stsd = vec![0u8; 4];
        stsd.extend(1u32.to_be_bytes());
        stsd.extend_from_slice(entry);
        let mut stsz = vec![0u8; 4];
        stsz.extend(size.to_be_bytes());
        stsz.extend(1u32.to_be_bytes());
        let mut co64 = vec![0u8; 4];
        co64.extend(1u32.to_be_bytes());
        co64.extend(offset.to_be_bytes());
        let tables = [box_bytes(b"stsd", &stsd), box_bytes(b"stsz", &stsz), box_bytes(b"co64", &co64)].concat();
        box_bytes(b"trak", &box_bytes(b"mdia", &box_bytes(b"minf", &box_bytes(b"stbl", &tables))))
    }

    /// `CMP1` coding descriptor: 52 bytes, four planes, 14 bits, RGGB, `levels` wavelet levels.
    pub(crate) fn cmp1(width: u32, height: u32, tile_width: u32, tile_height: u32, levels: u8, header_size: u32) -> Vec<u8> {
        let mut coding = vec![0u8; 52];
        coding[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
        coding[2..4].copy_from_slice(&48u16.to_be_bytes());
        coding[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        coding[8..12].copy_from_slice(&width.to_be_bytes());
        coding[12..16].copy_from_slice(&height.to_be_bytes());
        coding[16..20].copy_from_slice(&tile_width.to_be_bytes());
        coding[20..24].copy_from_slice(&tile_height.to_be_bytes());
        coding[24] = 14;
        coding[25] = 0x40;
        coding[26] = levels;
        coding[28..32].copy_from_slice(&header_size.to_be_bytes());
        coding
    }

    pub(crate) fn file(coding: &[u8], width: u16, height: u16, sample: &[u8]) -> Vec<u8> {
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&width.to_be_bytes());
        craw[26..28].copy_from_slice(&height.to_be_bytes());
        craw.extend(box_bytes(b"CMP1", coding));
        let entry = box_bytes(b"CRAW", &craw);
        let ftyp = box_bytes(b"ftyp", b"crx \0\0\0\x01crx isom");
        let track_len = track(&entry, 0, sample.len() as u32).len();
        let offset = (ftyp.len() + 8 + track_len + 7) & !7;
        let mut out = ftyp;
        out.extend(box_bytes(b"moov", &track(&entry, offset as u64, sample.len() as u32)));
        out.resize(offset, 0);
        out.extend_from_slice(sample);
        out
    }

    fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
        out.extend(code.to_be_bytes());
        out.extend(8u16.to_be_bytes());
        out.extend(size.to_be_bytes());
        out.extend(flags.to_be_bytes());
    }

    /// A 4×4 lossless CRX stream: four 2×2 planes, each sample value 1 (sensor level 8193).
    pub(crate) fn lossless_sample() -> Vec<u8> {
        let mut sample = Vec::new();
        marker(&mut sample, 0xff01, 4, 0);
        for p in 0..4u32 {
            marker(&mut sample, 0xff02, 1, (p << 28) | 0x0800_0000);
            marker(&mut sample, 0xff03, 1, 4 << 19);
        }
        sample.extend([0x1e; 4]);
        sample
    }

    pub(crate) fn lossless_file() -> Vec<u8> {
        file(&cmp1(4, 4, 4, 4, 0, 108), 4, 4, &lossless_sample())
    }

    /// A 16×16 three-level C-RAW CRX stream of flat zero subbands (sensor level 8192).
    pub(crate) fn craw_sample() -> Vec<u8> {
        let mut sample = Vec::new();
        marker(&mut sample, 0xff01, 52, 0);
        for plane in 0..4u32 {
            marker(&mut sample, 0xff02, 13, (plane << 28) | 0x0800_0000);
            for band in 0..10u32 {
                marker(&mut sample, 0xff03, if band >= 7 { 2 } else { 1 }, (band << 28) | (4 << 19));
            }
        }
        for _ in 0..4 {
            sample.extend([0x80; 4]);
            sample.extend([0xf0; 3]);
            for _ in 0..3 {
                sample.extend([0xff, 0xf8]);
            }
        }
        sample
    }

    pub(crate) fn craw_file() -> Vec<u8> {
        file(&cmp1(16, 16, 16, 16, 3, 540), 16, 16, &craw_sample())
    }
}

/// Both fixtures decode as built, so the mutations below hit live decode paths.
#[test]
fn cr3_fixtures_decode() {
    let lossless = lightcraft_raw::decode(&cr3::lossless_file()).unwrap();
    assert_eq!((lossless.width, lossless.height, lossless.bits), (4, 4, 14));
    assert_eq!(lossless.data.get(0), 8193.0);
    let craw = lightcraft_raw::decode(&cr3::craw_file()).unwrap();
    assert_eq!((craw.width, craw.height, craw.bits), (16, 16, 14));
    assert_eq!(craw.data.get(0), 8192.0);
    assert_eq!(lightcraft_raw::probe_info(&cr3::craw_file()).unwrap(), craw.info());
}

proptest! {
    /// Mutating or truncating structurally valid CR3 containers exercises the CRX marker walker and
    /// entropy readers far deeper than random TIFF bytes: errors or clean decodes, never a panic.
    #[test]
    fn mutated_cr3_containers_never_panic(
        kind in 0usize..2,
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..24),
        cut in any::<usize>(),
    ) {
        let mut data = if kind == 0 { cr3::lossless_file() } else { cr3::craw_file() };
        for (i, v) in flips {
            let n = data.len();
            data[i % n] = v;
        }
        let keep = if cut % 3 == 0 { cut % data.len() } else { data.len() };
        exercise(&data[..keep]);
        // The header-only description path (used when importing) must hold up against the same mutations.
        let _ = lightcraft_raw::probe_info(&data[..keep]);
    }
}
