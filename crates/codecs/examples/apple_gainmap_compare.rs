//! Compare our helper with the optional macOS probe's independent Core Image rendering.
//! See docs/apple-gain-maps.md. No media or reference buffers are bundled with this example.
use lightcraft_codecs::gainmap::{GainImage, apple};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Reference {
    width: usize,
    height: usize,
    map_width: usize,
    map_height: usize,
    headroom: Option<f64>,
    maker33: Option<f64>,
    maker48: Option<f64>,
    coreimage_headroom: f64,
}

fn read_rgb(path: &Path, pixels: usize) -> Result<Vec<[f32; 3]>, Box<dyn std::error::Error>> {
    let expected_bytes = pixels.checked_mul(12).ok_or("RGB byte count overflow")?;
    if std::fs::metadata(path)?.len() != expected_bytes as u64 {
        return Err("RGB byte count mismatch".into());
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() != expected_bytes {
        return Err("RGB byte count mismatch".into());
    }
    let mut rgb = Vec::with_capacity(pixels);
    for pixel in bytes.chunks_exact(12) {
        let mut channels = [0.0; 3];
        for (channel, chunk) in channels.iter_mut().zip(pixel.chunks_exact(4)) {
            *channel = f32::from_le_bytes(chunk.try_into()?);
            if !channel.is_finite() {
                return Err("non-finite reference sample".into());
            }
        }
        rgb.push(channels);
    }
    Ok(rgb)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("usage: apple_gainmap_compare REFERENCE_DIRECTORY".into());
    }
    let directory = Path::new(args.get(1).ok_or("missing directory")?);
    if std::fs::metadata(directory.join("reference.json"))?.len() > 65536 {
        return Err("reference manifest too large".into());
    }
    let info: Reference = serde_json::from_slice(&std::fs::read(directory.join("reference.json"))?)?;
    let pixels = info.width.checked_mul(info.height).filter(|n| *n > 0 && *n <= 32_000_000).ok_or("unsupported image dimensions")?;
    let mut base = read_rgb(&directory.join("base.rgbf32le"), pixels)?;
    let reference = read_rgb(&directory.join("hdr.rgbf32le"), pixels)?;
    let map_pixels = info.map_width.checked_mul(info.map_height).filter(|n| *n > 0 && *n <= 32_000_000).ok_or("unsupported map dimensions")?;
    if std::fs::metadata(directory.join("gain.gray8"))?.len() != map_pixels as u64 {
        return Err("map byte count mismatch".into());
    }
    let gain = GainImage { width: info.map_width, height: info.map_height, channels: 1, data: std::fs::read(directory.join("gain.gray8"))? };
    let metadata = apple::Metadata { headroom: info.headroom, maker33: info.maker33, maker48: info.maker48 };
    apple::apply(&mut base, info.width, info.height, &gain, &metadata)?;
    // Absolute error relative to SDR white, and relative error with a dark-pixel floor.
    let mut relative = Vec::with_capacity(pixels * 3);
    let mut sum = 0.0_f64;
    let mut maximum = 0.0_f32;
    for (ours, expected) in base.iter().flatten().zip(reference.iter().flatten()) {
        let difference = (ours - expected).abs();
        sum += f64::from(difference);
        maximum = maximum.max(difference);
        relative.push(difference / expected.abs().max(0.01));
    }
    relative.sort_by(f32::total_cmp);
    let p99 = relative.get(relative.len().saturating_sub(1) * 99 / 100).copied().ok_or("empty reference")?;
    println!("resolved headroom: {}; Core Image: {}", metadata.headroom()?, info.coreimage_headroom);
    println!(
        "{} pixels; mean absolute error: {:.9}; max: {:.9}; p99 relative error (floor 0.01): {:.7}%",
        pixels,
        sum / (pixels * 3) as f64,
        maximum,
        p99 * 100.0
    );
    // A measurement, not a universal pass/fail threshold: inspect errors and corpus coverage.
    Ok(())
}
