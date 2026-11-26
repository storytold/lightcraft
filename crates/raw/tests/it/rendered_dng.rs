//! Render to DNG: a linear DNG of a rendered image reads back as the same pixels and colours.

use lightcraft_color::{Mat3, PROPHOTO, REC2020, SRGB};
use lightcraft_raw::{Method, color, write_rendered_dng};

fn image(w: usize, h: usize) -> Vec<f32> {
    (0..w * h).flat_map(|i| [(i % w) as f32 / w as f32, (i / w) as f32 / h as f32, 0.25 + (i % 7) as f32 * 0.25]).collect()
}

fn close(a: &Mat3, b: &Mat3, tol: f64) -> bool {
    a.0.iter().flatten().zip(b.0.iter().flatten()).all(|(x, y)| (x - y).abs() < tol)
}

#[test]
fn rendered_dng_round_trips_pixels_and_colour() {
    let (w, h) = (300usize, 70usize);
    let rgb = image(w, h);
    for space in [SRGB, PROPHOTO] {
        let meta = lightcraft_meta::Metadata { make: Some("Maker".into()), model: Some("Model".into()), ..Default::default() };
        let bytes = write_rendered_dng(w, h, &rgb, &space, &meta, None).unwrap();
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp, raw.cfa.is_none()), (w, h, 3, true));
        assert_eq!(raw.metadata.model.as_deref(), Some("Model"));
        let dev = raw.develop(Method::Ahd).unwrap();
        for (p, q) in rgb.as_chunks::<3>().0.iter().zip(&dev.data) {
            for k in 0..3 {
                // above-white samples (1.25, 1.5, 1.75) are clipped by white level 1 on develop
                let want = p[k].min(1.0);
                assert!((q[k].min(1.0) - want).abs() <= want * 1e-3 + 1e-4, "{space:?} {p:?} {q:?}");
            }
        }
        // the camera transform is the space → Rec. 2020 matrix under a neutral white
        let t = color::camera_transform(&raw, color::as_shot_white_xy(&raw));
        assert!(!t.matrix_is_fallback);
        assert!(t.wb.iter().all(|v| (v - 1.0).abs() < 1e-3), "{:?}", t.wb);
        assert!(close(&t.matrix, &space.to_space(&REC2020), 2e-3), "{}: {:?} vs {:?}", space.name, t.matrix, space.to_space(&REC2020));
    }
}

#[test]
fn rendered_dng_rejects_bad_input_without_panicking() {
    let m = lightcraft_meta::Metadata::default();
    assert!(write_rendered_dng(0, 4, &[], &SRGB, &m, None).is_err());
    assert!(write_rendered_dng(4, 4, &[0.5; 10], &SRGB, &m, None).is_err());
    assert!(write_rendered_dng(usize::MAX, 3, &[0.5; 10], &SRGB, &m, None).is_err());
    let odd = [f32::NAN, f32::INFINITY, -2.0, 70000.0, 0.5, 1.0];
    let raw = lightcraft_raw::decode(&write_rendered_dng(2, 1, &odd, &SRGB, &m, None).unwrap()).unwrap();
    let dev = raw.develop(Method::Bilinear).unwrap();
    assert!(dev.data.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
}
