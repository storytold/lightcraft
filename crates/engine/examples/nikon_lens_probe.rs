//! Measurement only: dump an uncorrected quarter-size camera-RGB proxy and render the current source.
//!
//! `cargo run --release -p lightcraft-engine --example nikon_lens_probe -- IN OUT_DIR [OPCODES.json]`
//! Optional JSON is an array of experimental `OpcodeList3` lens opcodes. It replaces the lens description
//! after source loading, without changing the decoder, saved settings, or camera-look fit. Candidate renders
//! therefore isolate optics; they do not predict a future camera-look refit. See docs/raw/nikon_lens_probe.py.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::{Read, Write};
use std::path::Path;

use lightcraft_develop::DevelopSettings;
use lightcraft_engine::files;
use lightcraft_pipeline::{RenderRequest, SourceInfo};
use lightcraft_raster::Rgb32f;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let input = args.first().ok_or("usage: nikon_lens_probe IN OUT_DIR [OPCODES.json]")?;
    let dir = Path::new(args.get(1).ok_or("missing OUT_DIR")?);
    std::fs::create_dir_all(dir)?;
    let bytes = std::fs::read(input)?;
    let mut raw = lightcraft_raw::decode(&bytes)?;
    let mut header = raw.info();
    std::fs::write(dir.join("raw.json"), serde_json::to_vec_pretty(&header)?)?;
    raw.opcodes.list3.clear();
    let proxy = raw.develop_binned(4, 1.0)?.ok_or("cannot bin this raw")?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(dir.join("camera-rgb.f32"))?);
    out.write_all(&u32::try_from(proxy.width)?.to_le_bytes())?;
    out.write_all(&u32::try_from(proxy.height)?.to_le_bytes())?;
    for pixel in &proxy.data {
        for value in pixel {
            out.write_all(&value.to_le_bytes())?;
        }
    }
    out.flush()?;
    drop(raw);
    let (source, mut info) = files::load_bytes(&bytes, 1200)?;
    let mut settings = DevelopSettings::default();
    settings.wb.temp = info.as_shot_temp;
    settings.wb.tint = info.as_shot_tint;
    settings.optics.lens_profile = info.lens.is_some();
    render(&source, &info, &settings, &dir.join("baseline.png"))?;
    if let Some(path) = args.get(2) {
        let data = read_candidates(std::fs::File::open(path)?)?;
        header.opcodes.list3 = serde_json::from_slice(&data)?;
        validate_candidates(&header.opcodes.list3)?;
        info.lens = files::embedded_lens(&header);
        settings.optics.lens_profile = info.lens.is_some();
        render(&source, &info, &settings, &dir.join("candidate.png"))?;
    }
    Ok(())
}

/// Read one byte beyond the limit to distinguish a complete input from an oversized one.
fn read_candidates(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(65_537).read_to_end(&mut data)?;
    if data.len() > 65_536 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "candidate opcodes exceed 64 KiB"));
    }
    Ok(data)
}

/// Diagnostic inputs have a deliberately small range; reject arbitrary operations and invalid gains.
fn validate_candidates(ops: &[lightcraft_raw::Opcode]) -> Result<(), &'static str> {
    use lightcraft_raw::Opcode;
    if ops.is_empty() || ops.len() > 2 {
        return Err("provide one or two lens opcodes");
    }
    if ops.iter().filter(|op| matches!(op, Opcode::WarpRectilinear { .. })).count() > 1
        || ops.iter().filter(|op| matches!(op, Opcode::FixVignetteRadial { .. })).count() > 1
    {
        return Err("provide at most one warp and one radial vignette");
    }
    for op in ops {
        let center = match op {
            Opcode::WarpRectilinear { planes, center } if planes.len() == 1 && planes.iter().flatten().all(|v| v.is_finite() && v.abs() <= 16.0) => {
                center
            }
            Opcode::FixVignetteRadial { k, center } if k.iter().all(|v| v.is_finite() && v.abs() <= 16.0) => {
                for i in 0..=256 {
                    let r2 = (f64::from(i) / 256.0).powi(2);
                    let gain = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                    if !gain.is_finite() || !(0.01..=16.0).contains(&gain) {
                        return Err("candidate vignette gain is outside 0.01 to 16");
                    }
                }
                center
            }
            _ => return Err("candidate must be a bounded single-plane warp or radial vignette"),
        };
        if !center.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err("candidate center must be inside the active area");
        }
    }
    Ok(())
}

/// Use the same CPU renderer and PNG encoder as the app, with no catalog or filesystem hooks.
fn render(source: &Rgb32f, info: &SourceInfo, settings: &DevelopSettings, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let rendered = lightcraft_pipeline::render(source, info, settings, &RenderRequest::fit(1200, 1200));
    let image = lightcraft_codecs::encode::EncodeImage::rgba8(&rendered.image);
    std::fs::write(path, lightcraft_codecs::encode::encode_png(&image, &Default::default())?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_reject_duplicate_kinds_and_accept_one_of_each() {
        use lightcraft_raw::Opcode;
        let warp = Opcode::WarpRectilinear { planes: vec![[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] };
        let vignette = Opcode::FixVignetteRadial { k: [0.0; 5], center: [0.5, 0.5] };
        assert!(validate_candidates(std::slice::from_ref(&warp)).is_ok());
        assert!(validate_candidates(std::slice::from_ref(&vignette)).is_ok());
        assert!(validate_candidates(&[warp.clone(), vignette.clone()]).is_ok());
        assert!(validate_candidates(&[vignette.clone(), warp.clone()]).is_ok());
        assert_eq!(validate_candidates(&[warp.clone(), warp]), Err("provide at most one warp and one radial vignette"));
        assert_eq!(validate_candidates(&[vignette.clone(), vignette]), Err("provide at most one warp and one radial vignette"));
    }

    /// A hostile stream must be rejected after at most limit + 1 bytes, without reading to EOF.
    #[test]
    fn candidate_reader_bounds_the_read_and_accepts_the_exact_limit() {
        let mut endless = std::io::repeat(b' ');
        let mut counted = (&mut endless).take(1_000_000);
        let error = read_candidates(&mut counted).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(counted.limit(), 1_000_000 - 65_537);
        assert_eq!(read_candidates(std::io::repeat(b' ').take(65_536)).unwrap().len(), 65_536);
    }
}
