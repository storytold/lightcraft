//! `mask.export`: the matte of a mask, as a file and as numbers.

use serde_json::json;

use crate::Session;

fn path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lc-matte-{tag}-{}.png", std::process::id()))
}

#[test]
fn a_matte_is_a_grey_png_of_the_mask() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "radial", "center": [0.25, 0.5], "rx": 0.1, "ry": 0.1, "feather": 0})).unwrap();
    let file = path("radial");
    let v = s.execute("mask.export", &json!({"path": file, "longEdge": 256})).unwrap();

    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&file).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut grey = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut grey).unwrap();
    assert_eq!(info.color_type, png::ColorType::Grayscale);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(info.width.into()), Some(info.height.into())));
    assert_eq!(info.width.max(info.height), 256);

    let (w, h) = (info.width as usize, info.height as usize);
    assert!(grey[h / 2 * w + w / 4] > 200, "the middle of the mask is selected");
    assert_eq!(grey[h / 2 * w + w * 3 / 4], 0, "the far side is not");

    let coverage = v["coverage"].as_f64().unwrap();
    assert!(coverage > 0.01 && coverage < 0.5, "{coverage}");
    let bounds: Vec<f64> = v["bounds"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect();
    assert!(bounds[0] < 0.25 && bounds[2] > 0.25 && bounds[2] < 0.5, "{bounds:?}");
    let _ = std::fs::remove_file(file);
}

#[test]
fn a_matte_needs_a_path_and_a_mask() {
    let mut s = Session::with_demo();
    assert!(s.execute("mask.export", &json!({"path": path("none")})).is_err());
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    assert!(s.execute("mask.export", &json!({})).is_err());
    assert!(s.execute("mask.export", &json!({"id": 9999, "path": path("none")})).is_err());
}
