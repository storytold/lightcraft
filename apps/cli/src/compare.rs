//! `<cli> compare REFERENCE TEST [--size N] [--no-align] [--diff OUT.png] [--json]`: how far one render of a photo is
//! from another (from this app, Lightroom, Capture One, darktable or a camera), in CIEDE2000 by tone and hue
//! ([`lcc_colourfit::compare`]).

use std::path::Path;

use lcc_colourfit::compare::{self, CompareOptions};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut opts = CompareOptions::default();
    let mut files = Vec::new();
    let mut diff = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args.get(i).map_or("", String::as_str) {
            "--size" => {
                i += 1;
                let v = args.get(i).ok_or("--size needs a number of pixels")?;
                opts.long_edge = v.parse().map_err(|_| format!("--size: `{v}` is not a number of pixels"))?;
            }
            "--no-align" => opts.align = false,
            "--diff" => {
                i += 1;
                diff = Some(args.get(i).ok_or("--diff needs an output .png path")?.clone());
            }
            "--json" => json = true,
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    let [reference, test] = files.as_slice() else {
        return Err("compare needs two image files: REFERENCE TEST".into());
    };
    let report = compare::compare(Path::new(reference), Path::new(test), &opts).map_err(|e| e.to_string())?;
    if let Some(out) = &diff {
        let rgb = compare::map_rgb8(&report);
        let (w, h) = (u32::try_from(report.width).map_err(|_| "map too wide")?, u32::try_from(report.height).map_err(|_| "map too tall")?);
        let img = lcc_codecs::EncodeImage::new(w, h, 3, lcc_codecs::Samples::U8(&rgb));
        let png = lcc_codecs::encode_png(&img, &lcc_codecs::EncodeMeta::default()).map_err(|e| format!("{out}: {e}"))?;
        std::fs::write(out, png).map_err(|e| format!("{out}: {e}"))?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    } else {
        print!("{}", compare::text(&report));
        if let Some(out) = &diff {
            println!("ΔE00 map: {out} (black 0 · blue 1 · green 2 · yellow 5 · red 10+ · grey not compared)");
        }
    }
    Ok(())
}
