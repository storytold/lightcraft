//! Sequential decode cost: `cargo run --release -p lcc-video --example seq_bench -- clip [frames] [max_side]`

#![cfg_attr(target_arch = "wasm32", allow(unused))]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let input = a.get(1).ok_or("usage: seq_bench <clip> [frames] [max_side]")?;
    let n: usize = a.get(2).map(|s| s.parse()).transpose()?.unwrap_or(120);
    let max_side: Option<u32> = a.get(3).map(|s| s.parse()).transpose()?;
    let mut r = lcc_video::FrameReader::open(input.as_ref())?;
    r.seek(0.0);
    let start = std::time::Instant::now();
    let mut got = 0;
    let mut size = (0, 0);
    while got < n {
        let Some(f) = r.next_frame(max_side)? else { break };
        size = (f.image.width, f.image.height);
        got += 1;
    }
    let el = start.elapsed().as_secs_f64();
    println!("{input}: {got} frames {}x{} in {el:.2}s = {:.1} ms/frame, {:.1} fps", size.0, size.1, el * 1000.0 / got.max(1) as f64, got as f64 / el);
    Ok(())
}
