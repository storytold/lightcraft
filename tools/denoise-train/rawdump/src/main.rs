//! Offline helper for the AI Denoise training tools (`tools/denoise-train`). Not part of LightCraft.
//!
//! It reads a raw file with LightCraft's own decoder and normalisation (black subtracted, white at 1, not white
//! balanced), exactly what AI Denoise feeds its model, and writes one of:
//!
//! - `bin <raw> <out-prefix>`: a clean training picture. Every 2 × 2 Bayer cell (aligned to RGGB) becomes one camera
//!   RGB pixel (R, mean of the greens, B), as 16-bit planes `<out>.u16` (3 × h × w, little endian, 65535 = white) and
//!   `<out>.json` (size, ISO, camera). A cell with a clipped sample is written as 65535 in all three planes, so the
//!   trainer can leave it out.
//! - `noise <raw>`: noise statistics only (no picture data): for each of the four Bayer planes, 8 × 8 blocks are
//!   measured (mean, and the variance of a high-pass residual), and per brightness bin the low percentiles of the
//!   variance are printed as JSON. Flat areas give the lowest variances, so the lower envelope is the sensor's
//!   noise curve.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let r = match args.get(1).map(String::as_str) {
        Some("bin") if args.len() == 4 => bin(&args[2], &args[3]),
        Some("noise") if args.len() == 3 => noise(&args[2]),
        Some("selftest") if args.len() == 4 => selftest(&args[2], &args[3]),
        Some("noisy") if args.len() == 7 => noisy(&args[2], &args[3], &args[4], &args[5], &args[6]),
        _ => Err("usage: rawdump bin <raw> <out-prefix> | rawdump noise <raw> | rawdump selftest <onnx> <manifest> | rawdump noisy <raw> <out.dng> <a> <b> <k>".into()),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

struct Mosaic {
    w: usize,
    h: usize,
    data: Vec<f32>,
    /// first RGGB cell
    ox: usize,
    oy: usize,
    iso: Option<u32>,
    make: String,
    model: String,
}

fn load(path: &str) -> Result<Mosaic, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let raw = lightcraft_raw::decode(&bytes).map_err(|e| e.to_string())?;
    if raw.cpp != 1 || !raw.cfa.as_ref().is_some_and(|c| c.is_bayer()) {
        return Err("not a Bayer raw".into());
    }
    let n = raw.normalized().map_err(|e| e.to_string())?;
    let cfa = n.cfa.clone().ok_or("no CFA")?;
    let (ox, oy) = [(0, 0), (1, 0), (0, 1), (1, 1)]
        .into_iter()
        .find(|&(x, y)| cfa.color_at(x, y) == 0 && cfa.color_at(x + 1, y + 1) == 2)
        .ok_or("no RGGB alignment")?;
    Ok(Mosaic {
        w: n.width,
        h: n.height,
        data: n.data,
        ox,
        oy,
        iso: raw.metadata.iso,
        make: raw.metadata.make.clone().unwrap_or_default(),
        model: raw.metadata.model.clone().unwrap_or_default(),
    })
}

const CLIP: f32 = 0.97;

fn bin(path: &str, out: &str) -> Result<(), String> {
    let m = load(path)?;
    let (cw, ch) = ((m.w - m.ox) / 2, (m.h - m.oy) / 2);
    let at = |x: usize, y: usize| m.data[y * m.w + x];
    let mut planes = vec![0u16; 3 * cw * ch];
    let mut clipped = 0usize;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65534.0).round() as u16;
    for cy in 0..ch {
        for cx in 0..cw {
            let (x, y) = (m.ox + 2 * cx, m.oy + 2 * cy);
            let (r, g1, g2, b) = (at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1));
            let i = cy * cw + cx;
            if r.max(g1).max(g2).max(b) >= CLIP {
                clipped += 1;
                planes[i] = 65535;
                planes[cw * ch + i] = 65535;
                planes[2 * cw * ch + i] = 65535;
            } else {
                planes[i] = q(r);
                planes[cw * ch + i] = q(0.5 * (g1 + g2));
                planes[2 * cw * ch + i] = q(b);
            }
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(format!("{out}.u16")).map_err(|e| e.to_string())?);
    for v in &planes {
        f.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    f.flush().map_err(|e| e.to_string())?;
    let meta = serde_json::json!({
        "width": cw, "height": ch, "iso": m.iso, "make": m.make, "model": m.model,
        "clippedFraction": clipped as f64 / (cw * ch).max(1) as f64,
    });
    std::fs::write(format!("{out}.json"), meta.to_string()).map_err(|e| e.to_string())?;
    println!("{}", meta);
    Ok(())
}

fn noise(path: &str) -> Result<(), String> {
    let m = load(path)?;
    let (cw, ch) = ((m.w - m.ox) / 2, (m.h - m.oy) / 2);
    const BINS: usize = 40;
    let (lo, hi) = (1e-4f64.ln(), 1.0f64.ln());
    let mut planes_out = Vec::new();
    for (name, dx, dy) in [("R", 0, 0), ("G1", 1, 0), ("G2", 0, 1), ("B", 1, 1)] {
        let p = |cx: usize, cy: usize| m.data[(m.oy + 2 * cy + dy) * m.w + m.ox + 2 * cx + dx] as f64;
        let mut bins: Vec<Vec<f64>> = vec![Vec::new(); BINS];
        let b = 8;
        let mut by = 1;
        while by + b < ch - 1 {
            let mut bx = 1;
            while bx + b < cw - 1 {
                let (mut s, mut s2, mut mx, mut n) = (0.0, 0.0, 0.0f64, 0.0);
                let mut mean = 0.0;
                for y in by..by + b {
                    for x in bx..bx + b {
                        let v = p(x, y);
                        mean += v;
                        mx = mx.max(p(x - 1, y)).max(p(x + 1, y)).max(p(x, y - 1)).max(p(x, y + 1)).max(v);
                        let r = v - 0.25 * (p(x - 1, y) + p(x + 1, y) + p(x, y - 1) + p(x, y + 1));
                        s += r;
                        s2 += r * r;
                        n += 1.0;
                    }
                }
                mean /= n;
                if mx < CLIP as f64 && mean > 1e-4 {
                    let var = (s2 / n - (s / n).powi(2)) / 1.25;
                    let k = (((mean.ln() - lo) / (hi - lo)) * BINS as f64) as usize;
                    if k < BINS {
                        bins[k].push(var);
                    }
                }
                bx += b;
            }
            by += b;
        }
        let rows: Vec<_> = bins
            .iter_mut()
            .enumerate()
            .filter(|(_, v)| v.len() >= 20)
            .map(|(k, v)| {
                v.sort_by(|a, b| a.total_cmp(b));
                let pct = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
                let centre = (lo + (k as f64 + 0.5) / BINS as f64 * (hi - lo)).exp();
                serde_json::json!({"mean": centre, "n": v.len(), "p05": pct(0.05), "p10": pct(0.10), "p25": pct(0.25), "p50": pct(0.5)})
            })
            .collect();
        planes_out.push(serde_json::json!({"plane": name, "bins": rows}));
    }
    let out = serde_json::json!({"iso": m.iso, "make": m.make, "model": m.model, "width": m.w, "height": m.h, "planes": planes_out});
    println!("{out}");
    Ok(())
}

/// Load a model with LightCraft's own ONNX reader and CPU runner and run the installation self-test.
fn selftest(onnx: &str, manifest: &str) -> Result<(), String> {
    let bytes = std::fs::read(manifest).map_err(|e| format!("{manifest}: {e}"))?;
    let m = lightcraft_denoise::manifest::parse(&bytes).map_err(|e| e.to_string())?;
    let r = lightcraft_denoise::runtime::CpuRunner::load(std::path::Path::new(onnx), &m).map_err(|e| e.to_string())?;
    let t = r.self_test();
    println!("ok={} load_ms={:.0} tile_ms={:.0} scale={:.4}", t.ok, t.load_ms, t.tile_ms, t.scale);
    for (text, ok) in &t.checks {
        println!("  [{}] {text}", if *ok { "pass" } else { "FAIL" });
    }
    if t.ok { Ok(()) } else { Err("self-test failed".into()) }
}

/// A synthetic test pair for comparing noise reduction: the raw darkened by `k` (exposure) with Poisson-Gaussian
/// noise of `variance = a * x + b` added (normalised units, white = 1), written as a DNG. With a = b = 0 it is the
/// clean reference at the same exposure. The noise is deterministic (fixed seed).
fn noisy(path: &str, out: &str, a: &str, b: &str, k: &str) -> Result<(), String> {
    let num = |s: &str| s.parse::<f32>().map_err(|e| format!("{s}: {e}"));
    let (a, b, k) = (num(a)?, num(b)?, num(k)?);
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut raw = lightcraft_raw::decode(&bytes).map_err(|e| e.to_string())?;
    if raw.cpp != 1 {
        return Err("not a mosaic".into());
    }
    let (w, area) = (raw.width, raw.active_area);
    let range = raw.white_at(0) - raw.black.mean();
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut uniform = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    let mut gauss = move || {
        let (u, v) = (uniform(), uniform());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    };
    let black: Vec<f32> = (0..area.height * area.width).map(|i| raw.black.at(i % area.width, i / area.width, 0, 1)).collect();
    let lightcraft_raw::RawData::U16(data) = &mut raw.data else { return Err("float raws are not supported".into()) };
    for y in 0..area.height {
        for x in 0..area.width {
            let i = (area.y + y) * w + area.x + x;
            let bl = black[y * area.width + x];
            let v = (data[i] as f32 - bl) / range * k;
            let var = (a * v.max(0.0) + b).max(0.0);
            let n = v + var.sqrt() * gauss();
            data[i] = (bl + n.min(1.0) * range).round().clamp(0.0, 65535.0) as u16;
        }
    }
    let dng = lightcraft_raw::write_dng(&raw, &lightcraft_raw::DngWriteOptions::default()).map_err(|e| e.to_string())?;
    std::fs::write(out, dng).map_err(|e| e.to_string())?;
    Ok(())
}
