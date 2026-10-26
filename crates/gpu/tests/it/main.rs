//! The integration tests, one module per file, linked into one test binary.
//! Binaries of their own: `tests/shutdown.rs` and `tests/shutdown_first.rs` close the GPU for the whole
//! process, `tests/memory.rs` checks the process's device-memory counters, which renders on other test
//! threads would change, and `tests/texture.rs` and `tests/texture_lost.rs` lend a window device before
//! anything creates the render device (issue #1228).

mod equivalence;
mod fallback;
#[cfg(not(target_arch = "wasm32"))]
mod gpu_probe;
