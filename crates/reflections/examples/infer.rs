//! Developer equivalence check: cargo run -p lightcraft-reflections --example infer -- MODEL INPUT.rgb W H OUTPUT.f32 [cpu]
use std::io::Write;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |n| args.get(n).ok_or("MODEL INPUT.rgb W H OUTPUT.f32 [cpu]");
    let (w, h) = (get(2)?.parse::<usize>()?, get(3)?.parse::<usize>()?);
    let input = std::fs::read(get(1)?)?;
    let net = lightcraft_reflections::Net::load(std::path::Path::new(get(0)?), args.get(5).is_some_and(|x| x == "cpu"))?;
    let result = net.infer(&input, w, h)?;
    let mut f = std::io::BufWriter::new(std::fs::File::create(get(4)?)?);
    for pixel in result {
        for channel in pixel {
            f.write_all(&channel.to_le_bytes())?;
        }
    }
    f.flush()?;
    Ok(())
}
