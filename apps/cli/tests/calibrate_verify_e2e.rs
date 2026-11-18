//! `calibrate-lightroom verify` end to end, synthetic: edited raws (an XMP sidecar with `crs:` settings) checked
//! against stand-in "edited exports" — the app's own renders of the same raws, with the edit and without it. The
//! matching set must come out close, the unedited set far and darker, and bad arguments must fail with a message.
//! No Adobe data is involved.

use std::path::Path;
use std::process::Command;

use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, Mat3, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");
const PHOTOS: usize = 2;
const SIDECAR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4" crs:HasSettings="True" crs:WhiteBalance="As Shot" crs:Exposure2012="+1.00"/></rdf:RDF></x:xmpmeta>"#;

/// The packet Lightroom embeds in an export: the same edit, flagged as already applied to the export's pixels.
const EXPORT_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4" crs:HasSettings="True" crs:AlreadyApplied="True" crs:WhiteBalance="As Shot" crs:Exposure2012="+1.00"/></rdf:RDF></x:xmpmeta>"#;

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-cli-verify-e2e-{name}"))
}

fn rng(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

/// Photo `k`: a chart of colour patches with fine texture, mid-grey exposure so +1 EV does not clip it all.
fn photo(k: usize) -> RawImage {
    let (w, h, cell) = (768usize, 512usize, 48usize);
    let (black, white) = (512.0f64, 16000.0f64);
    let mut seed = 0x2545_f491_4f6c_dd1du64 ^ (k as u64 * 104_729);
    let (cols, rows) = (w.div_ceil(cell), h.div_ceil(cell));
    let patches: Vec<[f64; 3]> = (0..cols * rows)
        .map(|_| {
            let level = 0.03 + 0.2 * rng(&mut seed);
            let hue = rng(&mut seed) * std::f64::consts::TAU;
            let sat = 0.6 * rng(&mut seed);
            [0.0, 2.1, 4.2].map(|o: f64| level * (1.0 + sat * (hue + o).cos()).max(0.05))
        })
        .collect();
    let cfa = Cfa::bayer("RGGB").unwrap();
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let p = patches[(y / cell) * cols + x / cell];
            let tex = 1.0 + 0.15 * (rng(&mut seed) - 0.5);
            let v = (p[cfa.color_at(x, y) as usize] * tex).clamp(0.0, 1.0);
            data.push((black + v * (white - black)).round() as u16);
        }
    }
    let color = ColorData {
        illuminant: [17, 21],
        color_matrix: [
            Some(Mat3([[1.0, -0.35, -0.02], [-0.42, 1.30, 0.10], [-0.06, 0.20, 0.66]])),
            Some(Mat3([[0.90, -0.30, -0.02], [-0.40, 1.25, 0.14], [-0.04, 0.16, 0.78]])),
        ],
        as_shot_neutral: Some([0.5, 1.0, 0.7]),
        ..Default::default()
    };
    let mut raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(black as f32),
        white: vec![white as f32],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some("Synthica".into());
    raw.metadata.model = Some("SX-1".into());
    raw
}

fn run(cmd: &mut Command) -> String {
    let o = cmd.output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{cmd:?} failed:\n{out}\n{err}");
    assert!(!err.contains("panicked"), "{err}");
    out.into_owned()
}

/// The sRGB transfer curve (IEC 61966-2-1).
fn srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn render(raw: &Path, out: &Path) {
    run(Command::new(BIN).args(["render", s(raw), "-o", s(out), "--opt", "bitDepth=16", "--opt", "colorSpace=proPhoto", "--opt", "metadata=none"]));
}

fn verify_json(raws: &Path, exports: &Path) -> serde_json::Value {
    let out = run(Command::new(BIN).args(["calibrate-lightroom", "verify", "--raws", s(raws), "--exports", s(exports), "--json"]));
    serde_json::from_str(&out).unwrap()
}

#[test]
fn verify_measures_edited_renders_against_their_exports() {
    let root = scratch("main");
    let (raws, plain, edited, unedited, report) =
        (root.join("raws"), root.join("plain"), root.join("edited"), root.join("unedited"), root.join("report"));
    for d in [&raws, &plain, &edited, &unedited] {
        std::fs::create_dir_all(d).unwrap();
    }
    for k in 0..PHOTOS {
        let bytes = lcc_raw::write_dng(&photo(k), &DngWriteOptions::default()).unwrap();
        let name = format!("SX1_{k:04}");
        // the edited raw, with its sidecar
        let dng = raws.join(format!("{name}.dng"));
        std::fs::write(&dng, &bytes).unwrap();
        std::fs::write(raws.join(format!("{name}.xmp")), SIDECAR).unwrap();
        // the same raw without the edit, for an unedited "export"
        let bare = plain.join(format!("{name}.dng"));
        std::fs::write(&bare, &bytes).unwrap();
        render(&dng, &edited.join(format!("{name}.tif")));
        render(&bare, &unedited.join(format!("{name}.tif")));
    }

    // the text report and the report folder; its verify.json is what `--json` prints
    let text = run(Command::new(BIN).args(["calibrate-lightroom", "verify", "--raws", s(&raws), "--exports", s(&edited), "--report", s(&report)]));
    assert!(text.contains("all photos"), "{text}");
    assert!(report.join("SX1_0000.de00.png").is_file());
    let close: serde_json::Value = serde_json::from_slice(&std::fs::read(report.join("verify.json")).unwrap()).unwrap();
    let photos = close["photos"].as_array().unwrap();
    assert_eq!(photos.len(), PHOTOS, "{close}");
    for p in photos {
        assert_eq!(p["settings_from"], "sidecar", "{p}");
        assert!(p["groups"].as_array().unwrap().iter().any(|g| g == "light"), "{p}");
    }
    let close_median = close["overall"]["median"].as_f64().unwrap();
    assert!(close_median < 1.0, "the app's render of the edit matches its own export: {close}");

    let far = verify_json(&raws, &unedited);
    let far_median = far["overall"]["median"].as_f64().unwrap();
    assert!(far_median > close_median + 2.0, "a missing edit is measured: {far}");
    assert!(far["overall"]["lightness_bias"].as_f64().unwrap() > 1.0, "the app's +1 EV render reads lighter: {far}");
    let groups = far["groups"].as_array().unwrap();
    assert!(groups.iter().any(|g| g["name"] == "light"), "{far}");

    // no sidecar, the edit carried by the export itself (as Lightroom writes with Include: All Metadata, marked
    // crs:AlreadyApplied, which must not stop verify from applying it to the raw)
    let embedded = root.join("embedded");
    std::fs::create_dir_all(&embedded).unwrap();
    for k in 0..PHOTOS {
        let name = format!("SX1_{k:04}");
        let srgb_tif = root.join(format!("{name}.srgb.tif"));
        run(Command::new(BIN).args(["render", s(&raws.join(format!("{name}.dng"))), "-o", s(&srgb_tif), "--opt", "metadata=none"]));
        let d = lcc_codecs::decode(&std::fs::read(&srgb_tif).unwrap(), lcc_codecs::DecodeOptions::default()).unwrap();
        let px: Vec<u8> = d.image.data.iter().flat_map(|p| p.map(|v| (srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8)).collect();
        let img = lcc_codecs::EncodeImage::new(d.image.width as u32, d.image.height as u32, 3, lcc_codecs::Samples::U8(&px));
        let meta = lcc_codecs::EncodeMeta { xmp: Some(EXPORT_XMP), ..Default::default() };
        let tif = lcc_codecs::encode_tiff(&img, lcc_codecs::TiffCompression::default(), &meta).unwrap();
        std::fs::write(embedded.join(format!("{name}.tif")), tif).unwrap();
    }
    let from_export = verify_json(&plain, &embedded);
    for p in from_export["photos"].as_array().unwrap() {
        assert_eq!(p["settings_from"], "export", "{p}");
    }
    assert!(from_export["overall"]["median"].as_f64().unwrap() < 1.0, "the export's own settings are applied: {from_export}");
}

#[test]
fn verify_rejects_bad_arguments_with_a_message() {
    let root = scratch("bad");
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let e = s(&empty);
    for args in [
        vec!["calibrate-lightroom", "verify"],
        vec!["calibrate-lightroom", "verify", "--raws", e],
        vec!["calibrate-lightroom", "verify", "--raws", e, "--exports", e],
        vec!["calibrate-lightroom", "verify", "--raws", "/nonexistent", "--exports", "/nonexistent"],
        vec!["calibrate-lightroom", "verify", e],
        vec!["calibrate-lightroom", "verify", "--raws", e, "--exports", e, "--size", "big"],
        vec!["calibrate-lightroom", "verify", "--raws", e, "--exports", e, "--bogus"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked") && !err.trim().is_empty(), "{args:?}: {err}");
    }
}
