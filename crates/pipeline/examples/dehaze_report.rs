//! Measure Dehaze (issue #1237): for every process this build knows and Dehaze −100, −50, 0, +25,
//! +50, +100, print the median luminance shift, how many pixels clip to black, the width of the
//! dark halo at a bright/dark edge, the hue of a neutral grey patch and what a flat grey image does.
//!
//! `cargo run --release -p lightcraft-pipeline --example dehaze_report -- [--size 1200] [--out DIR] [image…]`
//!
//! `--out DIR` also writes each input's renders as PNGs (`<input>-v<process>-<amount>.png`) and a
//! 3×2 contact sheet per process (`<input>-v<process>-sheet.png`: −100, −50, 0 / +25, +50, +100).
//!
//! Inputs: the procedural hazy valley (`lightcraft_scenes::hazy_valley`, which also has the edge
//! and grey patch), two misty demo scenes, and any sRGB PNG/JPEG paths given (e.g. CC0 corpus
//! photos); edge, grey and flat-grey figures need the valley's known layout, so they are only
//! printed for it.
use lightcraft_color::{REC2020, SRGB};
use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_pipeline::{RenderRequest, SourceInfo, render};
use lightcraft_raster::{Rgb32f, Rgba8};
use lightcraft_scenes::haze::{black_pct, grey_lch, halo_widths, hue_diff, median_luma};
use lightcraft_scenes::{Kind, demo_library, hazy_valley};

const AMOUNTS: [f64; 6] = [-100.0, -50.0, 0.0, 25.0, 50.0, 100.0];

fn main() {
    let mut size = 1200usize;
    let mut paths = Vec::new();
    let mut out: Option<std::path::PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--out" {
            out = args.next().map(Into::into);
        } else if a == "--size" {
            size = args.next().and_then(|v| v.parse().ok()).unwrap_or(size).max(64);
        } else {
            paths.push(a);
        }
    }

    let (w, h) = (size, size * 2 / 3);
    let mut inputs: Vec<(String, Rgb32f, bool)> = vec![("hazy valley".into(), hazy_valley(w, h), true)];
    for s in demo_library().into_iter().filter(|s| s.kind == Kind::MistyForest) {
        inputs.push((format!("demo: {}", s.name), s.render_fit(size), false));
    }
    for p in &paths {
        match load_srgb(p, size) {
            Ok(img) => inputs.push((p.clone(), img, false)),
            Err(e) => eprintln!("skipping {p}: {e}"),
        }
    }

    for (name, src, layout) in &inputs {
        println!("\n== {name} ({}×{}) ==", src.width, src.height);
        for process in Process::ALL {
            println!("-- process V{} --", process.version().0);
            println!(
                "{:>7} {:>9} {:>8} {:>9} {:>10} {:>9} {:>9} {:>7} {:>6} {:>7} {:>9}",
                "amount", "med luma", "EV", "black %", "Δblack %", "halo dk", "halo br", "grey L", "C*", "h°", "Δh vs+25"
            );
            let renders: Vec<(f64, Rgba8)> = AMOUNTS.iter().map(|&a| (a, develop(src, process, a))).collect();
            if let Some(dir) = &out {
                save(dir, name, process, &renders);
            }
            let Some((_, base)) = renders.iter().find(|(a, _)| *a == 0.0) else { continue };
            let base_med = median_luma(base);
            let base_black = black_pct(base);
            let hue25 = renders.iter().find(|(a, _)| *a == 25.0).map(|(_, r)| grey_lch(r)[2]);
            for (amount, r) in &renders {
                let med = median_luma(r);
                let ev = (med.max(1e-4) / base_med.max(1e-4)).log2();
                let black = black_pct(r);
                let mut line = format!("{:>+7.0} {:>9.1} {:>+8.2} {:>9.1} {:>+10.1}", amount, encode(med) * 255.0, ev, black, black - base_black);
                if *layout {
                    let (dk, br) = halo_widths(r);
                    let [l, c, hue] = grey_lch(r);
                    let dh = hue25.map_or(f32::NAN, |h25| hue_diff(hue, h25));
                    line += &format!(" {dk:>9.1} {br:>9.1} {l:>7.1} {c:>6.2} {hue:>7.1} {dh:>+9.1}");
                }
                println!("{line}");
            }
        }
    }

    println!("\n== flat grey (0.18), max |Δ| vs Dehaze 0, 8-bit ==");
    let flat = Rgb32f::filled(300, 200, [0.18; 3]);
    for process in Process::ALL {
        let base = develop(&flat, process, 0.0);
        let diffs: Vec<String> = AMOUNTS
            .iter()
            .map(|&a| {
                let r = develop(&flat, process, a);
                let d = r.data.iter().zip(&base.data).map(|(p, q)| (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0)).max().unwrap_or(0);
                format!("{a:+.0}: {d}")
            })
            .collect();
        println!("V{}  {}", process.version().0, diffs.join("  "));
    }
}

/// Writes every render and a 3×2 sheet of them (half size) into `dir`.
fn save(dir: &std::path::Path, name: &str, process: Process, renders: &[(f64, Rgba8)]) {
    let stem: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let stem = format!("{}-v{}", stem.trim_matches('-'), process.version().0);
    let to_image = |r: &Rgba8| image::RgbaImage::from_fn(r.width as u32, r.height as u32, |x, y| image::Rgba(r.get(x as usize, y as usize)));
    let Some((_, first)) = renders.first() else { return };
    let (tw, th) = ((first.width / 2) as u32, (first.height / 2) as u32);
    let mut sheet = image::RgbaImage::from_pixel(tw * 3, th * 2, image::Rgba([0, 0, 0, 255]));
    for (i, (amount, r)) in renders.iter().enumerate() {
        let img = to_image(r);
        if let Err(e) = img.save(dir.join(format!("{stem}-{amount:+.0}.png"))) {
            eprintln!("cannot save: {e}");
        }
        let small = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
        image::imageops::replace(&mut sheet, &small, i64::from((i as u32 % 3) * tw), i64::from((i as u32 / 3) * th));
    }
    if let Err(e) = sheet.save(dir.join(format!("{stem}-sheet.png"))) {
        eprintln!("cannot save the sheet: {e}");
    }
}

fn develop(src: &Rgb32f, process: Process, amount: f64) -> Rgba8 {
    let mut s = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
    s.effects.dehaze = amount;
    render(src, &SourceInfo::default(), &s, &RenderRequest::fit(src.width, src.height)).image
}

fn encode(v: f32) -> f32 {
    lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0))
}

/// An sRGB PNG/JPEG as linear Rec.2020, fitted within `size` on the long edge.
fn load_srgb(path: &str, size: usize) -> Result<Rgb32f, image::ImageError> {
    let img = image::open(path)?;
    let img = img.resize(size as u32, size as u32, image::imageops::FilterType::Triangle).to_rgb32f();
    let to2020 = SRGB.to_space(&REC2020);
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data = img
        .pixels()
        .map(|p| {
            let c = p.0.map(lightcraft_color::transfer::srgb_to_linear);
            to2020.apply_f32(c).map(|v| v.max(0.0))
        })
        .collect();
    Ok(Rgb32f { width: w, height: h, data })
}
