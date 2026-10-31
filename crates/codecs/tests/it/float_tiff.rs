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
        Some(moxcms::ColorProfile::new_gray_with_gamma(2.0).encode().unwrap()),
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
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        use tiff::encoder::{TiffEncoder, colortype};
        let mut encoder = TiffEncoder::new(&mut cursor).unwrap();
        let mut image = encoder.new_image::<colortype::RGBA32Float>(2, 1).unwrap();
        image.encoder().write_tag(tiff::tags::Tag::ExtraSamples, &[1u16][..]).unwrap();
        image.write_data(&[f32::INFINITY, f32::NAN, f32::NEG_INFINITY, 0.5, 1.0, 2.0, 3.0, 0.5]).unwrap();
    }
    let decoded = decode(&cursor.into_inner(), DecodeOptions::default()).unwrap();
    assert_eq!(decoded.alpha.as_ref().unwrap().data, [0.5, 0.5]);
    assert_eq!(decoded.image.data, [[2.0, 0.0, 0.0], [2.0, 4.0, 6.0]]);
}

fn lut_profile() -> Vec<u8> {
    use moxcms::{ColorProfile, LutMultidimensionalType, LutWarehouse, Matrix3d, ToneReprCurve, Vector3d, Xyzd};
    let mut profile = ColorProfile::new_srgb();
    profile.cicp = None;
    profile.red_colorant = Xyzd::default();
    profile.green_colorant = Xyzd::default();
    profile.blue_colorant = Xyzd::default();
    profile.red_trc = None;
    profile.green_trc = None;
    profile.blue_trc = None;
    // A procedural mAB profile without a CLUT: linear RGB to D50 XYZ, then identity curves.
    profile.lut_a_to_b_colorimetric = Some(LutWarehouse::Multidimensional(LutMultidimensionalType {
        num_input_channels: 3,
        num_output_channels: 3,
        grid_points: [0; 16],
        clut: None,
        a_curves: vec![],
        m_curves: vec![ToneReprCurve::Parametric(vec![1.0]); 3],
        matrix: Matrix3d { v: NamedSpace::Srgb.to_xyz_d50().0.map(|r| r.map(|v| v * 32768.0 / 65535.0)) },
        b_curves: vec![ToneReprCurve::Parametric(vec![1.0]); 3],
        bias: Vector3d::default(),
    }));
    profile.encode().unwrap()
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
