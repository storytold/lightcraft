//! Malformed-input robustness: decoding random or corrupted files must never panic.

use lcc_raw::{BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, Method, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};
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
    lcc_raw::write_dng(&raw, &DngWriteOptions { compression: comp, ..Default::default() }).unwrap()
}

fn exercise(bytes: &[u8]) {
    let _ = lcc_raw::probe(bytes);
    let _ = lcc_raw::embedded_preview(bytes);
    if let Ok(img) = lcc_raw::decode(bytes)
        && img.width * img.height <= 1 << 16
    {
        let _ = img.develop(Method::Ahd);
        let xy = lcc_raw::color::as_shot_white_xy(&img);
        let _ = lcc_raw::color::camera_transform(&img, xy);
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
        let _ = lcc_raw::ljpeg::decode(&s, 1 << 20);
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
    use lcc_tiff::tags as t;
    use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
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
    // rejected, not decoded: a decode that tried to allocate the claimed image would hang or die,
    // so completion within a generous deadline (not a stopwatch) is the proof
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(lcc_raw::decode(&bytes).is_err());
    });
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(120)), Ok(true), "hostile dimensions are rejected without hanging");
}

/// A Bayer DNG whose `BlackLevel` is a FLOAT holding NaN (or infinity), and a float CFA DNG with
/// NaN samples: every normalised sample came out NaN, and AHD's green estimate clamped between two
/// NaN neighbours, which panics (the 1:1 view and every export of such a file). Non-finite levels
/// read as 0 and non-finite samples as black: the photo develops to finite numbers.
#[test]
fn non_finite_black_levels_and_samples_develop() {
    use lcc_tiff::tags as t;
    use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
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
        let img = lcc_raw::decode(&bytes).unwrap_or_else(|e| panic!("{black:?}: {e}"));
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
    for img in [raw.clone(), lcc_raw::decode(&lcc_raw::write_dng(&raw, &DngWriteOptions::default()).unwrap()).unwrap()] {
        let rgb = img.develop(Method::Ahd).unwrap();
        assert!(rgb.data.iter().flatten().all(|v| v.is_finite()), "float NaN samples: finite output");
    }
}

// CR3, ARW6, VC-5, RAF and NEF HE containers: the per-container table in `src/vendor/container_robust.rs`
// (it builds its seeds from the crate's own test encoders).
