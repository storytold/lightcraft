//! Lossy DNG (compression 34892) round trip: write, decode, develop, compare with the source.

use lightcraft_raw::{
    BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, Mat3, Method, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect,
};

/// A smooth Bayer mosaic (12-bit, black 64) of `w × h` with a colour matrix and a white balance.
fn mosaic(w: usize, h: usize) -> RawImage {
    let data: Vec<u16> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let c = (x % 2) + (y % 2);
            (64.0 + 3000.0 * (x as f32 / w as f32) * (0.5 + c as f32 * 0.25) + 500.0 * (y as f32 / h as f32)) as u16
        })
        .collect();
    let color = ColorData {
        illuminant: [21, 0],
        color_matrix: [Some(Mat3([[0.7, -0.1, -0.05], [-0.4, 1.2, 0.2], [-0.1, 0.2, 0.6]])), None],
        as_shot_neutral: Some([0.5, 1.0, 0.7]),
        ..Default::default()
    };
    RawImage {
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
        crop: Rect::new(2, 2, w - 4, h - 4),
        orientation: Orientation::Rotate90,
        color,
        wb_multipliers: None,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
        pixel_aspect: 1.0,
    }
}

fn lossy(raw: &RawImage, long_edge: Option<u32>) -> Vec<u8> {
    let opts = DngWriteOptions { compression: DngCompression::LossyJpeg { tile: 32, quality: 95, long_edge }, ..Default::default() };
    lightcraft_raw::write_dng(raw, &opts).unwrap()
}

#[test]
fn lossy_dng_round_trips_close_to_the_source() {
    let raw = mosaic(80, 56);
    let bytes = lossy(&raw, None);
    let back = lightcraft_raw::decode(&bytes).unwrap();
    assert_eq!((back.width, back.height, back.cpp, back.cfa.is_none()), (76, 52, 3, true), "demosaiced, default crop applied");
    assert!(back.linearized);
    assert_eq!(back.orientation, raw.orientation);
    assert_eq!(back.color.color_matrix, raw.color.color_matrix);
    assert_eq!(back.color.as_shot_neutral, raw.color.as_shot_neutral);
    let a = raw.develop(Method::Ahd).unwrap();
    let b = back.develop(Method::Ahd).unwrap();
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut worst) = (0f64, 0f32);
    for (p, q) in a.data.iter().zip(&b.data) {
        for k in 0..3 {
            let d = (p[k].clamp(0.0, 1.0) - q[k]).abs();
            sum += d as f64;
            worst = worst.max(d);
        }
    }
    let mean = sum / (a.data.len() * 3) as f64;
    assert!(mean < 0.01 && worst < 0.08, "mean {mean} worst {worst}");
}

#[test]
fn lossy_dng_scales_to_the_long_edge_and_is_smaller() {
    let raw = mosaic(160, 96);
    let small = lossy(&raw, Some(78));
    let back = lightcraft_raw::decode(&small).unwrap();
    assert_eq!(back.width.max(back.height), 78);
    assert_eq!(back.width, 78);
    assert!(back.height == 46 || back.height == 47, "{}", back.height);
    let full = lightcraft_raw::write_dng(&raw, &DngWriteOptions { compression: DngCompression::Uncompressed, ..Default::default() }).unwrap();
    assert!(small.len() < full.len() / 2, "{} vs {}", small.len(), full.len());
}

#[test]
fn lossy_dng_of_linear_data_and_extreme_values() {
    // a 3-sample linear raw with values above white, negative and non-finite samples: no panic, clipped
    let (w, h) = (20usize, 12usize);
    let data: Vec<f32> = (0..w * h * 3).map(|i| [2.0, -1.0, f32::NAN, 0.5, f32::INFINITY][i % 5]).collect();
    let raw = RawImage { cpp: 3, cfa: None, data: RawData::F32(data), white: vec![1.0], black: BlackLevel::uniform(0.0), ..mosaic(w, h) };
    let back = lightcraft_raw::decode(&lossy(&raw, Some(4))).unwrap();
    assert_eq!(back.width.max(back.height), 4);
    let rgb = back.develop(Method::Bilinear).unwrap();
    assert!(rgb.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
}
