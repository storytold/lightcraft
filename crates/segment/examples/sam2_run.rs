//! Runs SAM 2.1 on photos with clicks and writes red-tinted mask overlays, timing each step:
//! `cargo run -p lightcraft-segment --release --example sam2_run -- <model dir> <photo dir> <clicks.json> <out dir>`.
//!
//! `clicks.json` maps a photo's file stem to positive clicks `[[x, y], …]` normalized to the
//! image (0..1); photos without an entry are skipped.

use std::path::PathBuf;
use std::time::Instant;

use lightcraft_segment::{Click, SAM2_MASK_SIDE, Sam2, mask};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, photos, clicks, out] = &args[..] else {
        return Err("usage: sam2_run <model dir> <photo dir> <clicks.json> <out dir>".into());
    };
    let (photos, out) = (PathBuf::from(photos), PathBuf::from(out));
    std::fs::create_dir_all(&out)?;
    let clicks: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(clicks)?)?;
    let t = Instant::now();
    let model = Sam2::load(&PathBuf::from(dir))?;
    eprintln!("load: {:?} on {:?}", t.elapsed(), model.device());
    let mut stems: Vec<&String> = clicks.keys().collect();
    stems.sort();
    for stem in stems {
        let pts: Vec<Click> = clicks[stem]
            .as_array()
            .ok_or("clicks: expected arrays")?
            .iter()
            .filter_map(|p| Some(Click { x: p.get(0)?.as_f64()? as f32, y: p.get(1)?.as_f64()? as f32, positive: true }))
            .collect();
        let img = image::open(photos.join(format!("{stem}.jpg")))?.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let t = Instant::now();
        let enc = model.encode(img.as_raw(), w, h)?;
        let te = t.elapsed();
        let t = Instant::now();
        let p = model.segment_clicks(&enc, &pts)?;
        let td = t.elapsed();
        // a second click on the same encoding (the interactive case)
        let t = Instant::now();
        let _ = model.segment_clicks(&enc, &pts)?;
        let td2 = t.elapsed();
        eprintln!("{stem}: encode {te:?}, decode {td:?} / {td2:?}, score {:.3}", p.score);
        let alpha = mask::alpha(&p.logits, SAM2_MASK_SIDE, w, h);
        let mut o = img.clone();
        for (px, a) in o.pixels_mut().zip(&alpha) {
            if *a > 0.5 {
                for (c, tint) in px.0.iter_mut().zip([255.0f32, 40.0, 40.0]) {
                    *c = (f32::from(*c) * 0.45 + tint * 0.55).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        for c in &pts {
            let (cx, cy) = ((c.x * w as f32) as i64, (c.y * h as f32) as i64);
            for dy in -6i64..=6 {
                for dx in -6i64..=6 {
                    if dx * dx + dy * dy <= 36
                        && let Some(px) = o.get_pixel_mut_checked((cx + dx) as u32, (cy + dy) as u32)
                    {
                        px.0 = [40, 230, 80];
                    }
                }
            }
        }
        o.save(out.join(format!("{stem}_sam2.jpg")))?;
    }
    Ok(())
}
