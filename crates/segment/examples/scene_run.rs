//! Run the scene (EoMT) and depth (Depth Anything V2) models on JPEG photos and write overlays:
//! sky (blue tint), person (red tint) and depth (grayscale).
//!
//! `cargo run -p lightcraft-segment --release --example scene_run -- <eomt dir> <depth dir> <out dir> <photo.jpg>…`

use std::path::{Path, PathBuf};
use std::time::Instant;

use lightcraft_segment::depth::DepthModel;
use lightcraft_segment::scene::{self, SceneModel};

/// Bilinear sample of a `gw × gh` grid (covering the image) at pixel `(x, y)` of a `w × h` image.
fn sample(g: &[f32], gw: usize, gh: usize, x: usize, y: usize, w: usize, h: usize) -> f32 {
    let fx = ((x as f32 + 0.5) * gw as f32 / w as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
    let fy = ((y as f32 + 0.5) * gh as f32 / h as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |x: usize, y: usize| g[y * gw + x];
    (at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx) * (1.0 - ty) + (at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx) * ty
}

/// `rgb` (`w × h`) tinted `color` where the class map `g` (`gw × gh`) is.
fn tint(rgb: &[u8], w: usize, h: usize, (g, gw, gh): (&[f32], usize, usize), color: [f32; 3], out: &Path) {
    let mut o = rgb.to_vec();
    for y in 0..h {
        for x in 0..w {
            let m = 0.55 * sample(g, gw, gh, x, y, w, h).clamp(0.0, 1.0);
            for c in 0..3 {
                let i = (y * w + x) * 3 + c;
                o[i] = (rgb[i] as f32 * (1.0 - m) + color[c] * m).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    image::RgbImage::from_raw(w as u32, h as u32, o).unwrap().save(out).unwrap();
}

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [eomt, depth, out, photos @ ..] = args.as_slice() else {
        eprintln!("usage: scene_run <eomt dir> <depth dir> <out dir> <photo.jpg>…");
        std::process::exit(2);
    };
    let t = Instant::now();
    let sm = SceneModel::load(eomt).unwrap();
    let dm = DepthModel::load(depth).unwrap();
    eprintln!("load both: {:?}", t.elapsed());
    let sky = scene::class_ids(|l| l.contains("sky"));
    let person = scene::class_ids(|l| l == "person");
    for p in photos {
        let img = image::open(p).unwrap().to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let rgb = img.into_raw();
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        // SCENE_BENCH=n: best of n runs
        let runs: usize = std::env::var("SCENE_BENCH").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
        let (mut ts, mut td) = (std::time::Duration::MAX, std::time::Duration::MAX);
        let (mut s, mut d) = (None, None);
        for _ in 0..runs {
            let t = Instant::now();
            s = Some(sm.segment(&rgb, w, h).unwrap());
            ts = ts.min(t.elapsed());
            let t = Instant::now();
            d = Some(dm.estimate(&rgb, w, h).unwrap());
            td = td.min(t.elapsed());
        }
        let (s, d) = (s.unwrap(), d.unwrap());
        let people = s.instances.iter().filter(|i| i.class == 0).count();
        let labels: Vec<String> = s.instances.iter().map(|i| format!("{} {:.2}", scene::COCO_PANOPTIC_LABELS[i.class], i.score)).collect();
        println!("{stem}: scene {ts:?} ({}x{}), depth {td:?} ({}x{}), {people} people; {labels:?}", s.width, s.height, d.width, d.height);
        tint(&rgb, w, h, (&s.class_probability(&sky), s.width, s.height), [40.0, 120.0, 255.0], &out.join(format!("{stem}_sky.jpg")));
        tint(&rgb, w, h, (&s.class_probability(&person), s.width, s.height), [255.0, 40.0, 40.0], &out.join(format!("{stem}_person.jpg")));
        // each person instance in its own colour
        let mut o = rgb.clone();
        let colors = [[255.0, 40.0, 40.0], [40.0, 220.0, 40.0], [40.0, 120.0, 255.0], [255.0, 200.0, 0.0], [220.0, 0.0, 220.0], [0.0, 220.0, 220.0]];
        for (k, inst) in s.instances.iter().filter(|i| i.class == 0).enumerate() {
            let c = colors[k % colors.len()];
            for y in 0..h {
                for x in 0..w {
                    let m = 0.55 * sample(&inst.mask, s.width, s.height, x, y, w, h).clamp(0.0, 1.0);
                    for ch in 0..3 {
                        let i = (y * w + x) * 3 + ch;
                        o[i] = (o[i] as f32 * (1.0 - m) + c[ch] * m).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        image::RgbImage::from_raw(w as u32, h as u32, o).unwrap().save(out.join(format!("{stem}_people.jpg"))).unwrap();
        let gray: Vec<u8> = (0..w * h).map(|i| (sample(&d.data, d.width, d.height, i % w, i / w, w, h) * 255.0).round() as u8).collect();
        image::GrayImage::from_raw(w as u32, h as u32, gray).unwrap().save(out.join(format!("{stem}_depth.png"))).unwrap();
    }
}
