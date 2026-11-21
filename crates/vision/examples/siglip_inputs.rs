//! Writes the golden test's images as PNGs for `tools/siglip_reference.py`:
//! `cargo run -p lightcraft-vision --example siglip_inputs -- <out dir>`.

#[path = "../tests/inputs.rs"]
mod inputs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: siglip_inputs <out dir>")?);
    std::fs::create_dir_all(&dir)?;
    for (name, img) in inputs::images() {
        let path = dir.join(format!("{name}.png"));
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), img.width as u32, img.height as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&img.data.iter().flatten().copied().collect::<Vec<u8>>())?;
        w.finish()?;
        println!("{}", path.display());
    }
    std::fs::write(dir.join("texts.json"), serde_json::to_string(&inputs::TEXTS)?)?;
    std::fs::write(dir.join("token_cases.json"), serde_json::to_string(&inputs::token_cases())?)?;
    Ok(())
}
