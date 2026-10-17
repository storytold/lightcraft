//! MCP `set_develop` with the settings `get_develop` returned, the rotation changed: the photo's
//! Upright correction is turned with it, as Rotate does (issue #205). The settings state the
//! unchanged correction; it used to be kept in the old frame.

use lightcraft_engine::Session;
use lightcraft_mcp::{Headless, call_tool};
use serde_json::{Value, json};

const AUTO: &str = "1.010477639,-0.008540717,-0.006124940,0.014938050,1.011645918,-0.015359712,-0.007190889,-0.003396748,1.000000000";

/// A 60 × 40 JPEG stored on its side (EXIF 6) with a Lightroom sidecar holding a saved Auto matrix,
/// in its own folder `name`.
fn fixture(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-mcp-upright-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (60usize, 40usize);
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data: (0..w * h).map(|i| [(i % w * 4) as u8, (i / w * 6) as u8, 91, 255]).collect() };
    let exif = lightcraft_codecs::exif::minimal_exif(6);
    let meta = lightcraft_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    let bytes =
        lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, lightcraft_codecs::ChromaSubsampling::S444, &meta).unwrap();
    std::fs::write(dir.join("a.jpg"), bytes).unwrap();
    let xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:PerspectiveUpright="1" crs:UprightVersion="151388160" crs:UprightTransformCount="6" crs:UprightTransform_1="{AUTO}"/></rdf:RDF></x:xmpmeta>"#
    );
    std::fs::write(dir.join("a.xmp"), xmp).unwrap();
    dir.join("a.jpg")
}

fn correction(b: &Headless) -> Value {
    let p = b.session.catalog.photos().next().unwrap();
    serde_json::to_value(p.develop.geometry.upright_transform).unwrap()
}

#[test]
fn set_develop_with_get_develop_settings_turns_the_correction() {
    let path = fixture("once");
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("library.select", &json!({"ids": [id]})).unwrap();
    let mut b = Headless::new(s);
    let original = correction(&b);
    assert!(!original.is_null());
    assert!(!call_tool(&mut b, "command_run", &json!({"id": "photo.rotateRight", "params": {}})).is_error);
    assert_ne!(correction(&b), original);
    let got = call_tool(&mut b, "get_develop", &json!({"id": id}));
    let mut whole = got.structured.clone().unwrap();
    whole["orientation"] = json!("Normal");
    let r = call_tool(&mut b, "set_develop", &json!({"id": id, "settings": whole}));
    assert!(!r.is_error, "{:?}", r.structured);
    assert_eq!(correction(&b), original);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// An agent's local copy of `get_develop`, read once and sent back whole after each change (the
/// rotation, then Exposure, then the same copy with its matrix written to 9 decimals): the
/// correction stays the photo's own in its frame (the second send used to put it back in the old
/// one).
#[test]
fn set_develop_of_a_local_copy_after_each_change_keeps_the_correction() {
    let path = fixture("local");
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("library.select", &json!({"ids": [id]})).unwrap();
    let mut b = Headless::new(s);
    let original = correction(&b);
    assert!(!call_tool(&mut b, "command_run", &json!({"id": "photo.rotateRight", "params": {}})).is_error);
    let mut local = call_tool(&mut b, "get_develop", &json!({"id": id})).structured.unwrap();
    local["orientation"] = json!("Normal");
    for change in ["rotation", "exposure", "9 decimals"] {
        match change {
            "exposure" => local["light"]["exposure"] = json!(0.5),
            "9 decimals" => {
                for x in local["geometry"]["upright_transform"].as_array_mut().unwrap() {
                    *x = json!((x.as_f64().unwrap() * 1e9).round() / 1e9);
                }
            }
            _ => {}
        }
        let r = call_tool(&mut b, "set_develop", &json!({"id": id, "settings": local}));
        assert!(!r.is_error, "{:?}", r.structured);
        assert_eq!(correction(&b), original, "after the {change} send");
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
