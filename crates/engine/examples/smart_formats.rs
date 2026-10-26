//! Compare the existing codecs on the decoded smart-preview source, without opening a library.
use std::time::Instant;

use lightcraft_codecs::{EncodeImage, Samples, TiffCompression};
use lightcraft_color::transfer::linear_to_srgb;
use lightcraft_engine::{files, smart};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for path in std::env::args().skip(1) {
        let start = Instant::now();
        let raw = std::fs::read(&path)?;
        let probe = files::probe_bytes(&path, &raw)?;
        let (img, info) = files::load_vec(raw, 2560)?;
        println!(
            "{}",
            serde_json::json!({"source": path, "width": img.width, "height": img.height,
            "native_width": probe.width, "native_height": probe.height, "load_ms": start.elapsed().as_secs_f64() * 1000.0})
        );
        let low = img.data.iter().flatten().copied().fold(0.0f32, f32::min);
        let high = img.data.iter().flatten().copied().fold(1.0f32, f32::max);
        let span = high - low;
        let integer: Vec<u16> = img.data.iter().flatten().map(|v| (linear_to_srgb((v - low) / span) * 65535.0).round() as u16).collect();

        for format in ["jpeg92", "png16", "tiff16", "jxl16", "tiff32"] {
            let start = Instant::now();
            let encoded = EncodeImage::new(img.width as u32, img.height as u32, 3, Samples::U16(&integer));
            let bytes = match format {
                "jpeg92" => smart::encode(&img, Some(&info))?,
                "png16" => lightcraft_codecs::encode_png(&encoded, &Default::default())?,
                "tiff16" => lightcraft_codecs::encode_tiff(&encoded, TiffCompression::Deflate, &Default::default())?,
                "jxl16" => lightcraft_codecs::encode_jxl(
                    &encoded,
                    &lightcraft_codecs::JxlOptions { lossless: true, ..Default::default() },
                    &Default::default(),
                )?,
                _ => smart::encode_format(&img, Some(&info), smart::Format::Lossless)?,
            };
            let encode_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let decoded = if matches!(format, "jpeg92" | "tiff32") { smart::decode(&bytes)?.0 } else { decode_integer(&bytes, low, span)? };
            let decode_ms = start.elapsed().as_secs_f64() * 1000.0;
            let max_error = img.data.iter().flatten().zip(decoded.data.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            let mut fidelity = Vec::new();
            if matches!(format, "jpeg92" | "tiff32") {
                for (exposure, highlights) in [(0.0, 0.0), (-2.0, -100.0), (2.0, 0.0)] {
                    let mut settings = lightcraft_develop::DevelopSettings::default();
                    settings.light.exposure = exposure;
                    settings.light.highlights = highlights;
                    let reference = render(&img, &info, &settings)?;
                    let proxy = render(&decoded, &info, &settings)?;
                    let mut errors: Vec<f64> = reference
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .zip(proxy.as_chunks::<3>().0.iter())
                        .map(|(a, b)| {
                            let (a, b) = (lab(a), lab(b));
                            a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt()
                        })
                        .collect();
                    errors.sort_by(f64::total_cmp);
                    fidelity.push(serde_json::json!({"exposure": exposure, "highlights": highlights,
                        "mean_delta_e76": errors.iter().sum::<f64>() / errors.len() as f64,
                        "p99_delta_e76": errors[errors.len()*99/100]}));
                }
            }
            println!(
                "{}",
                serde_json::json!({"format":format, "bytes":bytes.len(), "encode_ms":encode_ms,
                "decode_ms":decode_ms, "fidelity":fidelity, "max_source_error":max_error, "low":low, "high":high})
            );
        }
    }
    Ok(())
}

/// The integer codecs receive identical sRGB-companded channel values. Their decoders
/// linearize them, but JXL also changes primaries. Undo that change before restoring the
/// stored range so every error is measured in the original scene-linear Rec.2020 samples.
fn decode_integer(bytes: &[u8], low: f32, span: f32) -> Result<lightcraft_raster::Rgb32f, lightcraft_codecs::Error> {
    let decoded = lightcraft_codecs::decode(bytes, Default::default())?;
    let matrix = decoded.space.to_space(&lightcraft_color::SRGB).to_f32();
    let mut image = decoded.image;
    for pixel in &mut image.data {
        let [r, g, b] = *pixel;
        *pixel = matrix.map(|row| (row[0] * r + row[1] * g + row[2] * b) * span + low);
    }
    Ok(image)
}

#[test]
fn integer_codecs_compare_in_the_same_sample_space() {
    let samples: Vec<u16> = (0..96 * 64 * 3).map(|i| ((i * 977 + 123) % 65536) as u16).collect();
    let encoded = EncodeImage::new(96, 64, 3, Samples::U16(&samples));
    let jxl = lightcraft_codecs::encode_jxl(&encoded, &lightcraft_codecs::JxlOptions { lossless: true, ..Default::default() }, &Default::default())
        .unwrap();
    let direct = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(&jxl)).unwrap();
    let frame = direct.render_frame(0).unwrap();
    let fb = frame.image_all_channels();
    let codes: Vec<u16> = fb.buf().iter().map(|v| (v * 65535.0).round() as u16).collect();
    assert_eq!(codes, samples, "lossless JXL preserves all integer samples");
    let (low, span) = (-0.1, 4.0);
    let reference: Vec<f32> = samples.iter().map(|v| lightcraft_color::transfer::srgb_to_linear(*v as f32 / 65535.0) * span + low).collect();
    for (name, bytes) in [
        ("png16", lightcraft_codecs::encode_png(&encoded, &Default::default()).unwrap()),
        ("tiff16", lightcraft_codecs::encode_tiff(&encoded, TiffCompression::Deflate, &Default::default()).unwrap()),
        ("jxl16", jxl),
    ] {
        let decoded = decode_integer(&bytes, low, span).unwrap();
        let error = reference.iter().zip(decoded.data.as_flattened()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("{name}: normalized colour conversion error {}", error / span);
        // Colour conversion is approximate. Allow two normalized 16-bit steps across the
        // stored range; the encoded JXL integers were checked exactly above.
        assert!(error < span * 2.0 / 65535.0, "{name}: colour conversion error {error}");
    }
}

fn render(
    img: &lightcraft_raster::Rgb32f,
    info: &lightcraft_pipeline::SourceInfo,
    settings: &lightcraft_develop::DevelopSettings,
) -> Result<Vec<f32>, String> {
    let req = lightcraft_pipeline::RenderRequest {
        depth: lightcraft_pipeline::output::OutputDepth::F32Linear,
        ..lightcraft_pipeline::RenderRequest::fit(img.width, img.height)
    };
    match lightcraft_pipeline::render(img, info, settings, &req).deep.map(|d| d.samples) {
        Some(lightcraft_pipeline::output::DeepSamples::F32(v)) => Ok(v),
        _ => Err("expected float render".into()),
    }
}

fn lab(rgb: &[f32]) -> [f64; 3] {
    let [r, g, b] = [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64];
    let xyz = [
        (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047,
        0.2126729 * r + 0.7151522 * g + 0.0721750 * b,
        (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883,
    ];
    let [x, y, z] = xyz.map(|v| if v > 216.0 / 24389.0 { v.cbrt() } else { v * 841.0 / 108.0 + 4.0 / 29.0 });
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}
