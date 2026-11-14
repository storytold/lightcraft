//! Serves SAM 3 on this machine's GPU (Metal, or CUDA in a `cuda` build) to one LightCraft editor
//! over a loopback socket (an SSH tunnel); see docs/ai-masks.md → Remote GPU inference.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Start the selected GPU backend and serve the supplied model directory.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or("usage: lightcraft-sam3-worker MODEL_DIR [127.0.0.1:8793]")?;
    let address = args.next().unwrap_or_else(|| "127.0.0.1:8793".into()).parse()?;
    if !cfg!(feature = "cuda") && !cfg!(target_os = "macos") {
        return Err("SAM worker requires CUDA on this platform; rebuild with: cargo build --release --locked -p lightcraft-segment --features cuda --bin lightcraft-sam3-worker".into());
    }
    let device = if cfg!(feature = "cuda") {
        candle_core::Device::new_cuda(0).map_err(|e| format!("Cannot initialize CUDA device 0: {e}; check the driver, runtime libraries and CUDA_VISIBLE_DEVICES"))?
    } else {
        candle_core::Device::new_metal(0).map_err(|e| format!("Cannot initialize Metal device 0: {e}"))?
    };
    lightcraft_segment::remote::serve(address, std::path::Path::new(&dir), device)?;
    Ok(())
}

/// The worker serves a local GPU over a socket; there is nothing to serve in a browser.
#[cfg(target_arch = "wasm32")]
fn main() {}
