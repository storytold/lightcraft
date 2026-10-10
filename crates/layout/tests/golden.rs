//! Golden-image tests: every built-in template, filled with procedural photos, rendered at a
//! preview resolution and reduced to a 16×16 grid of block means, compared with
//! `tests/golden/<template>.txt` (±3 per channel). Text signatures, not images, so no media is
//! committed. `UPDATE_GOLDEN=1 cargo test -p dac-layout --test golden` rewrites them; look at the
//! diff before committing.

use dac_layout::render::{PhotoSource, PlaceholderText, RenderOptions, render_document};
use dac_layout::tokens::PhotoInfo;
use dac_layout::{builtin_templates, render::Rendered};
use dac_raster::Rgba8;

/// Photos: a hue from the id, a horizontal ramp, a dark diagonal band; 3:2 landscape, every
/// third one portrait.
struct Procedural;

impl PhotoSource for Procedural {
    fn image(&self, photo: &str, _long: usize) -> Result<Rgba8, String> {
        let n: usize = photo.parse().map_err(|_| "bad id")?;
        let (w, h) = if n % 3 == 2 { (60, 90) } else { (90, 60) };
        let base = [(n * 70 % 256) as f32, (n * 130 % 256) as f32, (n * 200 % 256) as f32];
        Ok(Rgba8::from_fn(w, h, |x, y| {
            let t = x as f32 / w as f32;
            let band = if (x + y) % 30 < 6 { 0.3 } else { 1.0 };
            let c = |i: usize| ((base[i] * (0.4 + 0.6 * t) + 40.0) * band).min(255.0) as u8;
            [c(0), c(1), c(2), 255]
        }))
    }
    fn info(&self, photo: &str) -> PhotoInfo {
        PhotoInfo {
            filename: format!("IMG_{photo:0>4}.JPG"),
            title: format!("Photo {photo}"),
            caption: "A caption".into(),
            shutter: Some(0.004),
            aperture: Some(8.0),
            date: "2024-05-01".into(),
            ..PhotoInfo::default()
        }
    }
}

const GRID: usize = 16;

fn signature(r: &Rendered) -> Vec<[u8; 3]> {
    let img = &r.image;
    let mut out = Vec::with_capacity(GRID * GRID);
    for by in 0..GRID {
        for bx in 0..GRID {
            let (x0, x1) = (bx * img.width / GRID, ((bx + 1) * img.width / GRID).max(bx * img.width / GRID + 1));
            let (y0, y1) = (by * img.height / GRID, ((by + 1) * img.height / GRID).max(by * img.height / GRID + 1));
            let mut sum = [0u64; 3];
            let mut n = 0u64;
            for y in y0..y1.min(img.height) {
                for x in x0..x1.min(img.width) {
                    let p = img.get(x, y);
                    for i in 0..3 {
                        sum[i] += p[i] as u64;
                    }
                    n += 1;
                }
            }
            out.push(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    out
}

fn to_text(sig: &[[u8; 3]]) -> String {
    sig.chunks(GRID)
        .map(|row| row.iter().map(|p| format!("{:02x}{:02x}{:02x}", p[0], p[1], p[2])).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn from_text(s: &str) -> Vec<[u8; 3]> {
    s.split_whitespace().map(|h| [0, 2, 4].map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())).collect()
}

#[test]
fn builtin_templates_match_golden_signatures() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let photos: Vec<String> = (0..24).map(|i| i.to_string()).collect();
    let mut failures = Vec::new();
    for t in builtin_templates() {
        let doc = t.instantiate(&photos, false).unwrap();
        let pages = render_document(
            &doc,
            &Procedural,
            &PlaceholderText,
            &RenderOptions {
                dpi: std::env::var("LAYOUT_DPI").ok().and_then(|v| v.parse().ok()).unwrap_or(24.0),
                include_bleed: true,
                ..RenderOptions::default()
            },
        )
        .unwrap();
        assert!(pages.iter().all(|p| p.warnings.is_empty()), "{}", t.name);
        let file: String = t.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        let path = dir.join(format!("{file}.txt"));
        if let Some(dump) = std::env::var_os("LAYOUT_DUMP") {
            // binary PPM for eyeballing (convert with any image tool); never committed
            let img = &pages[0].image;
            let mut bytes = format!("P6 {} {} 255\n", img.width, img.height).into_bytes();
            bytes.extend(img.data.iter().flat_map(|p| [p[0], p[1], p[2]]));
            std::fs::write(std::path::Path::new(&dump).join(format!("{file}.ppm")), bytes).unwrap();
        }
        let got = signature(&pages[0]);
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, to_text(&got)).unwrap();
            continue;
        }
        let want = from_text(&std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {}; run with UPDATE_GOLDEN=1", path.display())));
        let worst = got.iter().zip(&want).flat_map(|(a, b)| (0..3).map(move |i| a[i].abs_diff(b[i]))).max().unwrap_or(255);
        if want.len() != got.len() || worst > 3 {
            failures.push(format!("{}: differs by up to {worst}", t.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
