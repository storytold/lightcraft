//! Runs BiRefNet on photos and writes red-tinted mask overlays and the alpha mattes:
//! `cargo run -p lightcraft-segment --release --example birefnet_run -- <model dir> <out dir> <tag> <photo.jpg>…`
//! (`<out dir>/<stem>_<tag>.jpg` and `<stem>_<tag>_alpha.png`).

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: birefnet_run <model dir> <out dir> <tag> <photo>…";
    let dir = PathBuf::from(args.next().ok_or(usage)?);
    let out = PathBuf::from(args.next().ok_or(usage)?);
    let tag = args.next().ok_or(usage)?;
    let t = std::time::Instant::now();
    let model = lightcraft_segment::BiRefNet::load(&dir)?;
    eprintln!("{}: load {:?}", model.backbone_name(), t.elapsed());
    for photo in args {
        let img = image::open(&photo)?.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let t = std::time::Instant::now();
        let logits = model.segment(img.as_raw(), w, h)?;
        eprintln!("{photo}: {:?}", t.elapsed());
        let alpha = lightcraft_segment::mask::alpha(&logits, lightcraft_segment::BIREFNET_SIDE, w, h);
        let mut o = img.clone();
        for (px, a) in o.pixels_mut().zip(&alpha) {
            let m = 0.55 * a;
            for (c, tint) in px.0.iter_mut().zip([255.0, 40.0, 40.0]) {
                *c = (f32::from(*c) * (1.0 - m) + tint * m).round().clamp(0.0, 255.0) as u8;
            }
        }
        let stem = PathBuf::from(&photo).file_stem().and_then(|s| s.to_str()).unwrap_or("photo").to_string();
        o.save(out.join(format!("{stem}_{tag}.jpg")))?;
        let a: Vec<u8> = alpha.iter().map(|v| (v * 255.0).round() as u8).collect();
        image::GrayImage::from_raw(w as u32, h as u32, a).ok_or("alpha size")?.save(out.join(format!("{stem}_{tag}_alpha.png")))?;
    }
    Ok(())
}
