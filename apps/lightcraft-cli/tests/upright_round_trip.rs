//! `lightcraft-cli run` with a script that sends the settings `develop.get` returned back whole after
//! each change: the photo's Upright correction stays in its frame (issue #205; the second send used
//! to put it back in the frame the settings were read in).

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_lightcraft-cli");

const AUTO: &str = "1.010477639,-0.008540717,-0.006124940,0.014938050,1.011645918,-0.015359712,-0.007190889,-0.003396748,1.000000000";

/// A 60 × 40 JPEG stored on its side (EXIF 6) with a Lightroom sidecar holding a saved Auto matrix.
fn fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let (w, h) = (60usize, 40usize);
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data: (0..w * h).map(|i| [(i % w * 4) as u8, (i / w * 6) as u8, 57, 255]).collect() };
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

/// `lightcraft-cli run` with `args` and the JSON-lines `script` on stdin: each command's result.
fn run(args: &[&str], script: &str) -> Vec<Value> {
    let mut child = Command::new(BIN).arg("run").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .inspect(|l| assert_eq!(l["ok"], true, "{l}"))
        .map(|l| l["result"].clone())
        .collect()
}

fn line(command: &str, params: Value) -> String {
    format!("{}\n", json!({"command": command, "params": params}))
}

#[test]
fn a_script_sending_the_settings_back_whole_after_each_change_keeps_the_correction() {
    let dir = std::env::temp_dir().join(format!("lightcraft-cli-upright-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let photo = fixture(&dir);
    let lib = dir.join("library");
    let args = ["--library", lib.to_str().unwrap(), "--import", photo.to_str().unwrap(), "--script", "-"];
    // the original correction, then Rotate Right and the settings read once
    let out = run(&args, &(line("develop.get", json!({})) + &line("photo.rotateRight", json!({})) + &line("develop.get", json!({}))));
    let original = out[0]["geometry"]["upright_transform"].clone();
    assert!(original.is_array(), "{original}");
    let mut local = out[2].clone();
    assert_ne!(local["geometry"]["upright_transform"], original);
    // sent back whole: the rotation set back to Normal, then also Exposure
    local["orientation"] = json!("Normal");
    let mut script = line("develop.merge", json!({"settings": local}));
    local["light"]["exposure"] = json!(0.5);
    script += &(line("develop.merge", json!({"settings": local})) + &line("develop.get", json!({})));
    let out = run(&args, &script);
    let now = &out[2];
    assert_eq!((&now["orientation"], &now["light"]["exposure"]), (&json!("Normal"), &json!(0.5)));
    assert_eq!(now["geometry"]["upright_transform"], original);
    let _ = std::fs::remove_dir_all(&dir);
}
