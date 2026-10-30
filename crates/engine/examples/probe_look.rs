//! Write, per raw, the CIELAB colour each pixel hands a measured look, as the app renders the photo: the input of a
//! fitted look (`tools/fit_look.py` pairs it with the camera's conversion of the same raw).
//!
//! The look `ID` (a built-in look of the raws' camera, e.g. `look:x-t5/provia`) is swapped, in this process only, for
//! a probe whose nodes hold their own CIELAB coordinates, scaled to 0..1: trilinear interpolation reproduces a linear
//! function exactly, so a 32-bit linear Adobe RGB export of the photo with that profile holds, per pixel,
//! `((L/100, (a-min)/(max-min), (b-min)/(max-min)))^(563/256)`, clamped to the grid as the look clamps it.
//!
//! usage: cargo run --release -p lightcraft-engine --example probe_look -- ID LONG_EDGE OUT_DIR RAF…
//! Writes OUT_DIR/<raf stem>.tif (float32 RGB); inputs are only read.
use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{ExportOptions, export_photo};
use lightcraft_pipeline::looks::{self, Axis, Look};
use serde_json::json;

const LIGHTNESS: Axis = Axis { min: 0.0, max: 100.0, n: 17 };
const AB: Axis = Axis { min: -168.0, max: 168.0, n: 29 };

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [id, edge, out, rafs @ ..] = args.as_slice() else {
        return Err("usage: probe_look ID LONG_EDGE OUT_DIR RAF…".into());
    };
    let edge: u64 = edge.parse().map_err(|_| "LONG_EDGE is a number")?;
    let real = lightcraft_engine::camera_looks::find(id).ok_or(format!("{id}: not a built-in look"))?;
    let at = |a: Axis, i: usize| a.min + (a.max - a.min) * i as f32 / (a.n - 1) as f32;
    let mut nodes = Vec::new();
    for l in 0..LIGHTNESS.n {
        for a in 0..AB.n {
            for b in 0..AB.n {
                let unit = |x: Axis, i: usize| (at(x, i) - x.min) / (x.max - x.min);
                nodes.push([unit(LIGHTNESS, l), unit(AB, a), unit(AB, b)]);
            }
        }
    }
    // the look file's camera colour (the input every look of the model shares)
    let file = lightcraft_engine::camera_looks::BUNDLED.iter().find(|(m, _)| real.fits(m)).ok_or("no look file")?.1;
    let v: serde_json::Value = serde_json::from_str(file).map_err(|e| e.to_string())?;
    let forward: [[f64; 3]; 3] = serde_json::from_value(v["input"]["forward_matrix"].clone()).map_err(|e| e.to_string())?;
    let white: [f64; 3] = serde_json::from_value(v["input"]["white"].clone()).map_err(|e| e.to_string())?;
    let probe = Look::new(id, &real.model, &real.name, &real.group, "probe", forward, white, LIGHTNESS, AB, nodes)?;
    looks::register(probe);
    std::fs::create_dir_all(out).map_err(|e| format!("{out}: {e}"))?;
    let o =
        ExportOptions::from_params(&json!({"format": "tiff", "bitDepth": 32, "colorSpace": "adobeRgb", "longEdge": edge, "tiffCompression": "none"}))
            .map_err(|e| e.to_string())?;
    // one probe per file name: two raws of the same name (another folder, a camera's numbering wrapping round) would
    // overwrite each other's probe, and the fit would pair one raw's probe with the other's conversion
    let stem_of = |raf: &str| std::path::Path::new(raf).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut seen = std::collections::HashMap::new();
    for raf in rafs {
        if let Some(first) = seen.insert(stem_of(raf), raf) {
            return Err(format!("{first} and {raf} have the same name: rename one, or probe them into different folders"));
        }
    }
    for raf in rafs {
        let path = std::fs::canonicalize(raf).map_err(|e| format!("{raf}: {e}"))?.to_string_lossy().to_string();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [path]})).map_err(|e| e.to_string())?;
        let pid = r["imported"][0].as_u64().ok_or(format!("{raf}: not imported"))?;
        s.execute("library.select", &json!({"ids": [pid], "active": pid})).map_err(|e| e.to_string())?;
        s.execute("develop.merge", &json!({"settings": {"profile": {"id": id, "amount": 100}}})).map_err(|e| e.to_string())?;
        let e = export_photo(&mut s, PhotoId(pid), &o, 1)?;
        let stem = stem_of(raf);
        let dst = std::path::Path::new(out).join(format!("{stem}.tif"));
        std::fs::write(&dst, &e.bytes).map_err(|err| format!("{}: {err}", dst.display()))?;
        eprintln!("{stem}: {}x{}", e.width, e.height);
    }
    Ok(())
}
