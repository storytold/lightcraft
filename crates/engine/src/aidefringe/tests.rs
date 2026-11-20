use super::*;
use std::path::PathBuf;

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

/// A folder of this test's own, removed when it ends (also when an assertion fails).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        TempDir(crate::tests_xmp::temp_dir(tag))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn publish() -> Publish {
    Publish { epoch: Arc::new(Mutex::new(0)), at: 0 }
}

#[test]
fn a_correction_survives_the_disk_and_a_damaged_one_is_refused() {
    let dir = TempDir::new("adf-file");
    let path = dir.0.join("c.lcadf");
    let c = correction(10, 4, &[(3, [0.5, -0.25, 0.0]), (17, [0.0, 0.125, -0.5])]);
    c.write(&path, &publish()).unwrap();
    // a second writer of the same correction (the background job and an export) succeeds too
    c.write(&path, &publish()).unwrap();
    let back = Correction::read(&path, 7).unwrap();
    assert_eq!((back.w, back.h, &back.index, &back.gain), (10, 4, &c.index, &c.gain));
    assert!(Correction::read(&path, 8).is_none(), "another key");
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    assert!(Correction::read(&path, 7).is_none(), "damaged");
    // after the cache was cleared, work started before it is not published
    let stale = Publish { epoch: Arc::new(Mutex::new(1)), at: 0 };
    let other = dir.0.join("d.lcadf");
    assert!(c.write(&other, &stale).is_err());
    assert!(!other.exists());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1, "no temporary file left behind");
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

#[test]
fn a_smaller_source_whose_size_does_not_divide_keeps_a_constant_gain() {
    // 3 × 1 with the same gain everywhere, rendered 2 × 1: two pixels of it map to the first, one to the second
    let c = correction(3, 1, &[(0, [0.5; 3]), (1, [0.5; 3]), (2, [0.5; 3])]);
    let src = Arc::new(Rgb32f { width: 2, height: 1, data: vec![[0.4; 3]; 2] });
    let out = c.apply(&src, 1.0);
    for p in &out.data {
        assert!(p.iter().all(|v| (v - 0.6).abs() < 1e-6), "{:?}", out.data);
    }
}

#[test]
fn the_memo_keeps_nothing_alive_and_never_mistakes_a_new_picture_for_an_old_one() {
    let c = correction(2, 1, &[(0, [1.0, 0.0, 0.0])]);
    let a = Arc::new(Rgb32f { width: 2, height: 1, data: vec![[0.5; 3]; 2] });
    let first = c.apply(&a, 1.0);
    assert!(Arc::ptr_eq(&first, &c.apply(&a, 1.0)), "the same view reuses its picture");
    assert_eq!(Arc::strong_count(&first), 1, "the memo holds no strong reference");
    drop(first);
    drop(a);
    // a new picture, maybe at the old one's address: corrected afresh
    let b = Arc::new(Rgb32f { width: 2, height: 1, data: vec![[0.25; 3]; 2] });
    assert_eq!(c.apply(&b, 1.0).data[0], [0.5, 0.25, 0.25]);
}

#[test]
fn gains_go_back_to_where_lateral_ca_correction_read_each_plane_from() {
    let (w, h) = (101, 101);
    let mut frame = lightcraft_pipeline::geometry::Frame::new(w, h, &DevelopSettings::default(), false);
    // red read 1 % further out from the centre (50.5, 50.5), blue 1 % further in
    frame.add_lateral_ca([0.01, -0.01]);
    let map = frame.source_map(w, h, w, h).unwrap();
    assert!(!map.is_identity());
    let (index, gain) = to_source([((50 * w + 90) as u32, [0.5f32, 0.25, 0.5])].into_iter(), &map, w, h);
    let at = |x: usize, c: usize| index.iter().position(|&i| i as usize == 50 * w + x).map_or(0.0, |k| f16_value(gain[k][c]));
    // red was read at x 90.9: 60 % from pixel 90, 40 % from 91; blue at 90.1: 40 % from 89, 60 % from 90
    let near = |a: f32, b: f32| (a - b).abs() < 1e-2;
    assert!(near(at(90, 0), 0.3) && near(at(91, 0), 0.2), "red {} {}", at(90, 0), at(91, 0));
    assert!(near(at(89, 2), 0.2) && near(at(90, 2), 0.3), "blue {} {}", at(89, 2), at(90, 2));
    assert!(near(at(90, 1), 0.25) && at(89, 1) == 0.0 && at(91, 1) == 0.0, "green stays put");
    assert_eq!(at(92, 0) + at(88, 2), 0.0, "nothing further away");
    // the same gain over an area stays the same inside it
    let all = (20..80).flat_map(|y| (20..80).map(move |x| ((y * w + x) as u32, [0.5f32; 3])));
    let (index, gain) = to_source(all, &map, w, h);
    let k = index.iter().position(|&i| i as usize == 50 * w + 60).unwrap();
    assert!(gain[k].iter().all(|&g| near(f16_value(g), 0.5)), "{:?}", gain[k].map(f16_value));
}

#[test]
fn without_lens_corrections_the_gains_stay_where_they_were_made() {
    let frame = lightcraft_pipeline::geometry::Frame::new(8, 4, &DevelopSettings::default(), false);
    let map = frame.source_map(8, 4, 8, 4).unwrap();
    assert!(map.is_identity());
    let (index, gain) = to_source([(5u32, [0.5f32, 0.0, -0.25])].into_iter(), &map, 8, 4);
    assert_eq!((index, gain), (vec![5], vec![[f16_bits(0.5), 0, f16_bits(-0.25)]]));
}

#[test]
fn a_gain_goes_back_to_the_source_pixel_its_plane_was_read_from() {
    // one bright red source pixel; sampled with lateral CA correction, it shows up elsewhere in the red plane
    let (w, h) = (101, 101);
    let mut src = Rgb32f { width: w, height: h, data: vec![[0.1; 3]; w * h] };
    src.data[30 * w + 95][0] = 1.0;
    let mut frame = lightcraft_pipeline::geometry::Frame::new(w, h, &DevelopSettings::default(), false);
    frame.add_lateral_ca([0.03, 0.0]);
    let tap = frame.sample(&src, w, h);
    let shown = (0..w * h).max_by(|&a, &b| tap.data[a][0].total_cmp(&tap.data[b][0])).unwrap();
    assert_ne!(shown, 30 * w + 95, "the test needs the red plane to move");
    let map = frame.source_map(w, h, w, h).unwrap();
    let (index, gain) = to_source([(shown as u32, [0.5, 0.0, 0.0])].into_iter(), &map, w, h);
    let top = (0..index.len()).max_by(|&a, &b| f16_value(gain[a][0]).total_cmp(&f16_value(gain[b][0]))).unwrap();
    assert_eq!(index[top] as usize, 30 * w + 95);
}
