//! Pure-Rust chart workflow; ordinary raw/JPEG pairs cannot be passed to the matrix fitter.
use lightcraft_color::{
    camera::CameraCalibration,
    chart::{self, Dataset},
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const HELP: &str = "chart sample RAW LAYOUT.json OUT.json\nchart fit OUT.profile.json WARM.json DAYLIGHT.json [MORE.json…]\nchart validate PROFILE.json OUT.report.json HOLDOUT.json [MORE.json…]\nchart inspect RAW\nSee docs/nikon-colour-calibration.md. Outputs must not already exist.";

fn read<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut b = Vec::new();
    f.take(8 * 1024 * 1024 + 1).read_to_end(&mut b).map_err(|e| e.to_string())?;
    if b.len() > 8 * 1024 * 1024 {
        return Err("chart JSON exceeds 8 MiB".into());
    }
    serde_json::from_slice(&b).map_err(|e| format!("{path}: {e}"))
}
fn read_raw(path: &str) -> Result<Vec<u8>, String> {
    const MAX_RAW_BYTES: u64 = 256 * 1024 * 1024;
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("{path}: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX_RAW_BYTES {
        return Err("chart RAW input must be a regular file up to 256 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RAW_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > MAX_RAW_BYTES {
        return Err("chart RAW input grew beyond 256 MiB".into());
    }
    Ok(bytes)
}

fn write<T: serde::Serialize>(path: &str, value: &T) -> Result<(), String> {
    let b = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("{path}: {e}"))?;
    if let Err(e) = f.write_all(&b).and_then(|_| f.sync_all()) {
        let _ = std::fs::remove_file(path);
        return Err(format!("{path}: {e}"));
    }
    Ok(())
}
fn datasets(paths: &[String]) -> Result<Dataset, String> {
    let first = paths.first().ok_or(HELP)?;
    let mut d: Dataset = read(first)?;
    for path in paths.iter().skip(1) {
        let next: Dataset = read(path)?;
        if next.version != d.version || next.make != d.make || next.model != d.model || next.provenance != d.provenance {
            return Err("dataset identity/provenance differs; record a common calibration project source".into());
        }
        d.captures.extend(next.captures);
        if d.captures.len() > 256 {
            return Err("too many chart captures".into());
        }
    }
    d.validate()?;
    Ok(d)
}
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("sample") if args.len() == 4 => {
            let path = args.get(1).ok_or(HELP)?;
            let bytes = read_raw(path)?;
            let hash = format!("{:x}", Sha256::digest(&bytes));
            let raw = lightcraft_raw::decode_base(&bytes).map_err(|e| e.to_string())?;
            let layout = read(args.get(2).ok_or(HELP)?)?;
            write(args.get(3).ok_or(HELP)?, &lightcraft_raw::chart::sample(&raw, hash, &layout)?)?;
            println!("Sampled unbalanced sensor patches; no camera calibration claimed.");
        }
        Some("fit") if args.len() >= 4 => {
            let profile = chart::fit(&datasets(args.get(2..).ok_or(HELP)?)?)?;
            write(args.get(1).ok_or(HELP)?, &profile)?;
            println!("Fitted warm/daylight matrices. Independent validation has NOT been performed.");
        }
        Some("validate") if args.len() >= 4 => {
            let profile: CameraCalibration = read(args.get(1).ok_or(HELP)?)?;
            let report = chart::validate(&profile, &datasets(args.get(3..).ok_or(HELP)?)?)?;
            write(args.get(2).ok_or(HELP)?, &report)?;
            println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
        }
        Some("inspect") if args.len() == 2 => {
            let bytes = read_raw(args.get(1).ok_or(HELP)?)?;
            let raw = lightcraft_raw::decode_base(&bytes).map_err(|e| e.to_string())?;
            let report = serde_json::json!({"format":raw.format,"make":raw.metadata.make,"model":raw.metadata.model,"sensor":[raw.width,raw.height],
                "bits":raw.bits,"black":raw.black,"white":raw.white,"activeArea":raw.active_area,"crop":raw.crop,"cameraWB":raw.wb_multipliers,
                "embeddedCalibration":lightcraft_engine::raw_color::embedded_model(&raw),
                "note":"Fixed code-range white is an estimate when no explicit saturation tag/curve exists; no measured camera calibration is bundled."});
            println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
        }
        _ => return Err(HELP.into()),
    }
    Ok(())
}
