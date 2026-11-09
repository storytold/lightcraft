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
    s.execute("crop.set", &json!({"rect": [0.5, 0.2, 0.9, 0.8]})).unwrap();
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

    assert_eq!(v["path"], json!(std::fs::canonicalize(&file).unwrap()));
    let expected_coverage = grey.iter().map(|&v| f64::from(v)).sum::<f64>() / (255.0 * (w * h) as f64);
    assert_eq!(v["coverage"].as_f64().unwrap(), expected_coverage);
    let selected: Vec<_> = grey.iter().enumerate().filter(|(_, v)| **v >= 128).map(|(i, _)| (i % w, i / w)).collect();
    let expected_bounds = [
        selected.iter().map(|p| p.0).min().unwrap() as f64 / w as f64,
        selected.iter().map(|p| p.1).min().unwrap() as f64 / h as f64,
        (selected.iter().map(|p| p.0).max().unwrap() + 1) as f64 / w as f64,
        (selected.iter().map(|p| p.1).max().unwrap() + 1) as f64 / h as f64,
    ];
    assert_eq!(v["bounds"], json!(expected_bounds));
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

#[test]
fn matte_rejects_invalid_sizes_and_existing_files() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let file = path("validation");
    for edge in [json!(-1), json!(15), json!(3073), json!(16384), json!(16.5), json!("256"), json!(null)] {
        assert!(s.execute("mask.export", &json!({"path": file, "longEdge": edge})).is_err());
        assert!(!file.exists());
    }
    std::fs::write(&file, b"keep").unwrap();
    assert!(s.execute("mask.export", &json!({"path": file, "longEdge": 16})).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    std::fs::remove_file(file).unwrap();
}

#[test]
fn matte_protects_originals_and_missing_sidecars() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let original = path("original");
    std::fs::write(&original, b"original").unwrap();
    let photo =
        Photo::new(PhotoId(9999), Source::File { path: original.to_string_lossy().into_owned() }, "original.png", "PNG", 10, 10, "2026-01-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    for file in [original.clone(), original.with_extension("xmp")] {
        let error = s.execute("mask.export", &json!({"path": file, "longEdge": 16})).unwrap_err();
        assert!(error.to_string().contains("original") || error.to_string().contains("sidecar"), "{error}");
    }
    assert_eq!(std::fs::read(&original).unwrap(), b"original");
    assert!(!original.with_extension("xmp").exists());
    std::fs::remove_file(original).unwrap();
}

#[test]
fn explicit_mask_with_subtraction_can_export_an_empty_selection() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    let id = s.active_mask.unwrap();
    s.execute("mask.addComponent", &json!({"kind": "radial", "op": "subtract", "rx": 2.0, "ry": 2.0, "feather": 0})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let file = path("empty");
    let v = s.execute("mask.export", &json!({"id": id, "path": file, "longEdge": 16})).unwrap();
    assert_eq!(v["coverage"], 0.0);
    assert!(v["bounds"].is_null());
    std::fs::remove_file(file).unwrap();
}

#[test]
fn matte_uses_full_source_above_preview_size() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let file = path("full");
    let v = s.execute("mask.export", &json!({"path": file, "longEdge": 2561})).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&file).unwrap())).read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.width.max(info.height), 2561);
    assert_eq!(info.color_type, png::ColorType::Grayscale);
    assert_eq!(v["width"], info.width);
    assert_eq!(v["height"], info.height);
    std::fs::remove_file(file).unwrap();
}

#[test]
fn matte_returns_to_photo_coordinates_after_transform_and_flip() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "radial", "center": [0.25, 0.5], "rx": 0.05, "ry": 0.05, "feather": 0})).unwrap();
    let id = s.active().unwrap();
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.geometry.offset_x = 15.0;
    d.crop.flip_h = true;
    s.set_develop(id, d, "Transform").unwrap();
    let file = path("geometry");
    let v = s.execute("mask.export", &json!({"path": file, "longEdge": 256})).unwrap();
    let b = v["bounds"].as_array().unwrap();
    assert!((b[0].as_f64().unwrap() + b[2].as_f64().unwrap() - 0.5).abs() < 0.03, "{v}");
    std::fs::remove_file(file).unwrap();
}
