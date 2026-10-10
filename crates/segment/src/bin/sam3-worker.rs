//! Serves SAM 3 on this Mac's Metal GPU to one editor over a loopback socket (an SSH
//! tunnel); see docs/ai-masks.md → Remote Metal inference.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or("usage: sam3-worker MODEL_DIR [127.0.0.1:8793]")?;
    let address = args.next().unwrap_or_else(|| "127.0.0.1:8793".into()).parse()?;
    let device = candle_core::Device::new_metal(0)?;
    dac_segment::remote::serve(address, std::path::Path::new(&dir), device)?;
    Ok(())
}

/// The worker serves a local GPU over a socket; there is nothing to serve in a browser.
#[cfg(target_arch = "wasm32")]
fn main() {}
