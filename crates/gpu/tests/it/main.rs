//! The integration tests, one module per file, linked into one test binary.
//! Binaries of their own: `tests/shutdown.rs` and `tests/shutdown_first.rs` close the GPU for the whole
//! process, `tests/memory.rs` checks the process's device-memory counters, which renders on other test
//! threads would change, and `tests/display.rs` and `tests/window_pending.rs` adopt (or wait for) the
//! window's device, once per process.

mod equivalence;
mod fallback;
#[cfg(not(target_arch = "wasm32"))]
mod gpu_probe;
mod thumbs_bench;
