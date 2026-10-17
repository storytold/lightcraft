//! Spike S1: decode the frame at a time and write it as PNG.
//! `cargo run --release -p lcc-video --example frame_png -- clip.mp4 1.5 out.png [max_side]`

#![cfg_attr(target_arch = "wasm32", allow(unused))]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let (Some(input), Some(t), Some(out)) = (a.get(1), a.get(2), a.get(3)) else {
        return Err("usage: frame_png <clip> <seconds> <out.png> [max_side]".into());
    };
    let max_side = a.get(4).map(|s| s.parse()).transpose()?;
    let info = lcc_video::probe(input.as_ref())?;
    eprintln!("{info:?}");
    let mut r = lcc_video::FrameReader::open(input.as_ref())?;
    let start = std::time::Instant::now();
    let f = r.frame_at(t.parse()?, max_side)?;
    eprintln!("frame at {:.3}s, {}x{}, {:?}", f.time, f.image.width, f.image.height, start.elapsed());
    let px = f.image.to_srgb8();
    let bytes: Vec<u8> = px.data.iter().flatten().copied().collect();
    let img = image::RgbaImage::from_raw(px.width as u32, px.height as u32, bytes).ok_or("image size")?;
    img.save(out)?;
    Ok(())
}
