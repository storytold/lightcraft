use super::*;

fn manifest_json(sha: &str) -> String {
    format!(
        r#"{{"id": "test-defringe", "name": "Test defringe", "version": "1", "licence": "MIT", "file": "model.onnx", "sha256": "{sha}", "contract": "{CONTRACT}"}}"#
    )
}

#[test]
fn a_manifest_is_checked() {
    let sha = "ab".repeat(32);
    let m = Manifest::parse(&manifest_json(&sha)).unwrap();
    assert_eq!((m.id.as_str(), m.file.as_str()), ("test-defringe", "model.onnx"));
    assert!(Manifest::parse(&manifest_json("abc")).is_err(), "short hash");
    assert!(Manifest::parse(&manifest_json(&sha).replace(CONTRACT, "bayerToRgb")).is_err(), "a denoiser's contract");
    assert!(Manifest::parse(&manifest_json(&sha).replace("model.onnx", "../model.onnx")).is_err(), "a path");
    assert!(Manifest::parse(&manifest_json(&sha).replace("model.onnx", "model.bin")).is_err(), "not ONNX");
    assert!(Manifest::parse(&manifest_json(&sha).replace("test-defringe", "a b")).is_err(), "an id with a space");
    assert!(Manifest::parse(&manifest_json(&sha).replace("\"version\"", "\"extra\": 1, \"version\"")).is_err(), "unknown fields");
}

fn correction(w: usize, h: usize, at: &[(usize, [f32; 3])]) -> Correction {
    Correction {
        key: 7,
        w,
        h,
        index: at.iter().map(|a| a.0 as u32).collect(),
        gain: at.iter().map(|a| a.1.map(f16_bits)).collect(),
        memo: Mutex::new(None),
    }
}

#[test]
fn a_correction_survives_the_disk_and_a_damaged_one_is_refused() {
    let dir = std::env::temp_dir().join(format!("lc-adf-{}", std::process::id()));
    let path = dir.join("c.lcadf");
    let c = correction(10, 4, &[(3, [0.5, -0.25, 0.0]), (17, [0.0, 0.125, -0.5])]);
    c.write(&path).unwrap();
    let back = Correction::read(&path, 7).unwrap();
    assert_eq!((back.w, back.h, &back.index, &back.gain), (10, 4, &c.index, &c.gain));
    assert!(Correction::read(&path, 8).is_none(), "another key");
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    assert!(Correction::read(&path, 7).is_none(), "damaged");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_correction_multiplies_the_source_and_a_smaller_source_gets_the_mean() {
    let src = Arc::new(Rgb32f { width: 4, height: 2, data: vec![[0.5, 0.5, 0.5]; 8] });
    let c = correction(4, 2, &[(1, [0.5, -0.5, 0.0])]);
    let full = c.apply(&src, 1.0);
    assert_eq!(full.data[1], [0.75, 0.25, 0.5]);
    assert_eq!(full.data[0], [0.5; 3]);
    let half = c.apply(&src, 0.5);
    assert_eq!(half.data[1], [0.625, 0.375, 0.5]);
    assert!(Arc::ptr_eq(&c.apply(&src, 0.0), &src), "no amount, no copy");
    // a 2 × 1 source: its first pixel covers correction pixels 0, 1, 4 and 5, one of which has the gain
    let small = Arc::new(Rgb32f { width: 2, height: 1, data: vec![[0.5, 0.5, 0.5]; 2] });
    let s = c.apply(&small, 1.0);
    assert_eq!(s.data[0], [0.5 * (1.0 + 0.5 / 4.0), 0.5 * (1.0 - 0.5 / 4.0), 0.5]);
    assert_eq!(s.data[1], [0.5; 3]);
}

#[test]
fn half_floats_round_trip() {
    for v in [0.0f32, 0.5, -0.25, 1.0, 4.0, -0.8, 1e-3, 3.140625] {
        assert!((f16_value(f16_bits(v)) - v).abs() <= v.abs() * 1e-3 + 1e-7, "{v}");
    }
    assert_eq!(f16_value(f16_bits(1e9)), 65504.0);
}
