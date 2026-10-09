//! Developer-only ORF validation. Reads files locally; optional sensor dumps use create-new files.
//! `cargo run -p lightcraft-raw --example orf_audit -- [--sensor-dir DIR] FILE...`
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use lightcraft_raw::{RawData, RawError, RawFormat, decode, probe, probe_info};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut output = None;
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--sensor-dir" {
            output = Some(PathBuf::from(args.next().ok_or("--sensor-dir requires a directory")?));
        } else {
            paths.push(PathBuf::from(arg));
        }
    }
    if paths.is_empty() {
        return Err("usage: orf_audit [--sensor-dir DIR] FILE...".into());
    }
    if let Some(dir) = &output {
        std::fs::create_dir_all(dir)?;
    }
    for path in paths {
        let mut bytes = Vec::new();
        File::open(&path)?.take(512 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 512 * 1024 * 1024 {
            return Err("ORF audit input exceeds 512 MiB".into());
        }
        if probe(&bytes) != Some(RawFormat::Orf) {
            return Err(format!("{} is not an ORF", path.display()).into());
        }
        let start = Instant::now();
        let header = probe_info(&bytes);
        let header_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        match decode(&bytes) {
            Ok(raw) => {
                let full_ms = start.elapsed().as_secs_f64() * 1000.0;
                if header? != raw.info() {
                    return Err(format!("{}: header differs from full decode", path.display()).into());
                }
                println!(
                    "{}: {}x{} bits={} cfa={} active={:?} probe={header_ms:.2}ms decode={full_ms:.2}ms",
                    path.display(),
                    raw.width,
                    raw.height,
                    raw.bits,
                    raw.cfa.as_ref().map(|c| c.name()).unwrap_or_default(),
                    raw.active_area,
                );
                if let Some(dir) = &output {
                    let name = path.file_name().ok_or("input has no filename")?;
                    let mut name = name.to_os_string();
                    name.push(".u16le");
                    let target = dir.join(name);
                    let mut writer = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(&target)?);
                    let RawData::U16(samples) = raw.data else { return Err("ORF audit expected integer sensor samples".into()) };
                    for value in samples {
                        writer.write_all(&value.to_le_bytes())?;
                    }
                    writer.flush()?;
                    println!("  sensor dump: {}", target.display());
                }
            }
            Err(RawError::Unsupported(reason)) => {
                if !matches!(header, Err(RawError::Unsupported(_))) {
                    return Err(format!("{}: header accepts an unsupported variant", path.display()).into());
                }
                println!("{}: unsupported ({reason}), probe={header_ms:.2}ms", path.display());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
