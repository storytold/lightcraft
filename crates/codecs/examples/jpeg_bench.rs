//! Compare the parallel JPEG encoder with `jpeg-encoder` (size, time, PSNR):
//! `cargo run --release -p lightcraft-codecs --example jpeg_bench [image]` (default: 6000×4000 synthetic).
use lightcraft_codecs::ChromaSubsampling;
use std::time::Instant;

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}

fn best<T>(n: usize, mut f: impl FnMut() -> T) -> (T, f64) {
    let mut out = None;
    let mut t = f64::MAX;
    for _ in 0..n {
        let s = Instant::now();
        let r = f();
        t = t.min(s.elapsed().as_secs_f64() * 1e3);
        out = Some(r);
    }
    (out.unwrap(), t)
}

fn main() {
    let (w, h, rgb) = match std::env::args().nth(1) {
        Some(p) => {
            let bytes = std::fs::read(p).expect("read");
            let img = lightcraft_codecs::decode(&bytes, lightcraft_codecs::DecodeOptions::default()).expect("open").to_srgb8();
            let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            (img.width, img.height, rgb)
        }
        None => {
            let (w, h) = (6000, 4000);
            let mut v = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                for x in 0..w {
                    let t = (((x as f32 / 37.0).sin() * (y as f32 / 23.0).cos()) * 0.5 + 0.5) * 255.0;
                    v.extend_from_slice(&[(x * 255 / w) as u8, t as u8, ((x ^ y) & 0xff) as u8]);
                }
            }
            (w, h, v)
        }
    };
    for (sub, cs) in [(ChromaSubsampling::S444, false), (ChromaSubsampling::S420, true)] {
        let (ours, t_ours) = best(5, || lightcraft_codecs::jpeg_par::encode(&rgb, w, h, 3, 90, sub, &[]));
        let (theirs, t_theirs) = best(3, || {
            let ic_img = craft_image_codecs::Image::from_u8(w as u32, h as u32, craft_image_codecs::ChannelLayout::Rgb, rgb.clone()).unwrap();
            let opts = craft_image_codecs::EncodeOptions { jpeg_quality: 90, jpeg_chroma_subsampling: cs, ..Default::default() };
            craft_image_codecs::encode(&ic_img, craft_image_codecs::Format::Jpeg, &opts).unwrap()
        });
        let dec = |b: &[u8]| {
            craft_image_codecs::decode_as_with(craft_image_codecs::Format::Jpeg, b, &craft_image_codecs::DecodeOptions::default())
                .unwrap()
                .as_u8()
                .unwrap()
                .to_vec()
        };
        println!(
            "{w}x{h} {sub:?} q90: parallel {t_ours:.0} ms, {} KB, PSNR {:.2} dB | craft-image-codecs {t_theirs:.0} ms, {} KB, PSNR {:.2} dB",
            ours.len() / 1024,
            psnr(&rgb, &dec(&ours)),
            theirs.len() / 1024,
            psnr(&rgb, &dec(&theirs))
        );
    }
}
