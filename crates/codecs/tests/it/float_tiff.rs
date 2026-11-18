//! Non-finite samples in procedural 32-bit float TIFFs (issue #1952, item 6).

use lightcraft_codecs::{icc, *};

fn tiff(samples: &[f32], channels: u8, profile: Option<&[u8]>) -> Vec<u8> {
    encode_tiff(
        &EncodeImage::new((samples.len() / channels as usize) as u32, 1, channels, Samples::F32(samples)),
        TiffCompression::None,
        &EncodeMeta { icc: profile, ..Default::default() },
    )
    .unwrap()
}

#[test]
fn float_tiff_nonfinite_rgb_clips_without_losing_finite_hdr() {
    let samples =
        [[f32::INFINITY; 3], [f32::NAN; 3], [f32::NEG_INFINITY; 3], [-0.25; 3], [0.25, 0.5, 0.75], [2.0, 4.0, 8.0], [f32::INFINITY, 0.5, f32::NAN]];
    let bytes = tiff(samples.as_flattened(), 3, None);
    let decoded = decode(&bytes, DecodeOptions::default()).unwrap();
    assert!(decoded.float);
    assert_eq!(decoded.bit_depth, 32);
    assert_eq!(decoded.image.data, [[1.0; 3], [0.0; 3], [0.0; 3], [-0.25; 3], [0.25, 0.5, 0.75], [2.0, 4.0, 8.0], [1.0, 0.5, 0.0]]);
    let display = decoded.to_srgb8();
    assert_eq!(display.data[0], [255; 4]);
    for pixel in &display.data[1..4] {
        assert_eq!(*pixel, [0, 0, 0, 255]);
    }
    assert_eq!(display.data[6], [255, 188, 0, 255]);
    assert!(decoded.to_working().data.iter().flatten().all(|v| v.is_finite()));

    // A box-filtered pair must average clipped white with its ordinary neighbor.
    let pair = tiff(&[f32::INFINITY, f32::INFINITY, f32::INFINITY, 0.25, 0.25, 0.25], 3, None);
    let small = decode(&pair, DecodeOptions::fit(1, 1)).unwrap();
    assert_eq!(small.image.data, [[0.625; 3]]);
}

#[test]
fn float_tiff_nonfinite_gray_and_icc_curves_match_clipped_samples() {
    let samples = [f32::INFINITY, f32::NAN, f32::NEG_INFINITY, 0.25, 1.0, 2.0, -0.25];
    let clipped = [1.0, 0.0, 0.0, 0.25, 1.0, 2.0, -0.25];
    let profiles = [
        None,
        Some(icc::write_matrix_trc(&lightcraft_color::SRGB, &Trc::Linear)),
        Some(icc::write_named(NamedSpace::Srgb)),
        Some(icc::write_matrix_trc(&lightcraft_color::SRGB, &Trc::Gamma(2.0))),
        Some(icc::write_matrix_trc(&lightcraft_color::SRGB, &Trc::Table(vec![0, 16384, 65535]))),
        Some({
            let mut p = craft_cms::Builtin::GrayGamma22.profile().clone();
            p.gray_trc = Some(craft_cms::Curve::Gamma(2.0));
            p.with_encoded_bytes().to_bytes().to_vec()
        }),
        Some(b"invalid ICC profile".to_vec()),
    ];
    for channels in [1, 3] {
        let samples: Vec<_> = samples.iter().flat_map(|v| std::iter::repeat_n(*v, channels as usize)).collect();
        let clipped: Vec<_> = clipped.iter().flat_map(|v| std::iter::repeat_n(*v, channels as usize)).collect();
        for profile in &profiles {
            let profile = profile.as_deref();
            let decoded = decode(&tiff(&samples, channels, profile), DecodeOptions::default()).unwrap();
            let reference = decode(&tiff(&clipped, channels, profile), DecodeOptions::default()).unwrap();
            assert_eq!(decoded.grayscale, channels == 1);
            assert_eq!(decoded.image, reference.image, "channels={channels}, space={:?}", decoded.space);
            assert_eq!(decoded.to_srgb8(), reference.to_srgb8());
            assert!(decoded.image.data[5].iter().all(|v| *v > 1.0), "finite HDR survives the curve");
            assert!(decoded.image.data[6].iter().all(|v| *v < 0.0), "finite negative samples survive the curve");
        }
    }
}

#[test]
fn float_tiff_nonfinite_alpha_is_finite_and_bounded() {
    let samples = [
        [f32::INFINITY, f32::INFINITY, f32::INFINITY, f32::INFINITY],
        [f32::NAN, f32::NEG_INFINITY, 0.5, f32::NAN],
        [0.25, 0.5, 2.0, f32::NEG_INFINITY],
        [2.0, 3.0, 4.0, 0.5],
    ];
    let decoded = decode(&tiff(samples.as_flattened(), 4, None), DecodeOptions::default()).unwrap();
    assert_eq!(decoded.alpha.as_ref().unwrap().data, [1.0, 0.0, 0.0, 0.5]);
    assert_eq!(decoded.image.data, [[1.0; 3], [0.0, 0.0, 0.5], [0.25, 0.5, 2.0], [2.0, 3.0, 4.0]]);
    assert!(decoded.to_working().data.iter().flatten().all(|v| v.is_finite()));

    // Associated alpha takes the same path before unpremultiplication.
    let mut strip = Vec::new();
    for s in [f32::INFINITY, f32::NAN, f32::NEG_INFINITY, 0.5, 1.0, 2.0, 3.0, 0.5] {
        strip.extend_from_slice(&s.to_le_bytes());
    }
    let mut ifd = craft_tiff::IfdBuilder::new()
        .with(craft_tiff::tags::IMAGE_WIDTH, craft_tiff::Value::Long(vec![2]))
        .with(craft_tiff::tags::IMAGE_LENGTH, craft_tiff::Value::Long(vec![1]))
        .with(craft_tiff::tags::BITS_PER_SAMPLE, craft_tiff::Value::Short(vec![32, 32, 32, 32]))
        .with(craft_tiff::tags::COMPRESSION, craft_tiff::Value::Short(vec![1]))
        .with(craft_tiff::tags::PHOTOMETRIC, craft_tiff::Value::Short(vec![2]))
        .with(craft_tiff::tags::SAMPLES_PER_PIXEL, craft_tiff::Value::Short(vec![4]))
        .with(craft_tiff::tags::PLANAR_CONFIGURATION, craft_tiff::Value::Short(vec![1]))
        .with(craft_tiff::tags::SAMPLE_FORMAT, craft_tiff::Value::Short(vec![3, 3, 3, 3]))
        .with(craft_tiff::tags::EXTRA_SAMPLES, craft_tiff::Value::Short(vec![1]));
    ifd.set_image(craft_tiff::ImageData::Strips { rows_per_strip: 1, strips: vec![strip] });
    let tif = craft_tiff::TiffWriter::new(craft_tiff::ByteOrder::Little, false).write(&[ifd]).unwrap();
    let decoded = decode(&tif, DecodeOptions::default()).unwrap();
    assert_eq!(decoded.alpha.as_ref().unwrap().data, [0.5, 0.5]);
    assert_eq!(decoded.image.data, [[1.0, 0.0, 0.0], [2.0, 4.0, 6.0]]);
}

fn lut_profile() -> Vec<u8> {
    use craft_cms::pipeline::Stage;
    use craft_cms::profile::{Lut, LutKind};
    let mut profile = craft_cms::Builtin::Srgb.profile().clone();
    profile.matrix = None;
    profile.trc = None;
    let m: Vec<f64> = NamedSpace::Srgb.to_xyz_d50().0.iter().flatten().map(|&v| v * 32768.0 / 65535.0).collect();
    let ident = vec![craft_cms::Curve::Identity; 3];
    // A procedural mAB profile without a CLUT: linear RGB to D50 XYZ, then identity curves.
    profile.a2b[1] = Some(Lut {
        kind: LutKind::Ab,
        inputs: 3,
        outputs: 3,
        stages: vec![Stage::Curves(ident.clone()), Stage::Matrix { rows: 3, cols: 3, m, offset: vec![0.0; 3] }, Stage::Curves(ident)],
    });
    profile.with_encoded_bytes().to_bytes().to_vec()
}

#[test]
fn float_tiff_nonfinite_cms_samples_match_clipped_samples() {
    let profile = lut_profile();
    let samples = [f32::INFINITY, f32::NAN, f32::NEG_INFINITY, 0.25, 0.5, 0.75];
    let clipped = [1.0, 0.0, 0.0, 0.25, 0.5, 0.75];
    let decoded = decode(&tiff(&samples, 3, Some(&profile)), DecodeOptions::default()).unwrap();
    let reference = decode(&tiff(&clipped, 3, Some(&profile)), DecodeOptions::default()).unwrap();
    assert_eq!(decoded.space.origin, SpaceOrigin::IccCms);
    assert_eq!(decoded.image, reference.image);
    assert!(decoded.to_working().data.iter().flatten().all(|v| v.is_finite()));
}
