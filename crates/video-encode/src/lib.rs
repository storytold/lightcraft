//! Silent movies from frames the caller makes (a timelapse of stills, issue #497).
//!
//! [`SequenceWriter`] takes frames of one fixed size at a fixed rate and writes an **H.264 MP4**
//! (High, 8-bit 4:2:0) or a **ProRes 422 MOV** (10-bit; HQ at quality ≥ 80, Standard ≥ 50, else
//! LT), with no audio. RGB → Y'CbCr uses BT.709 coefficients (`filmcraft-color`), limited range.
//! Encoders: `filmcraft-h264enc`, `filmcraft-prores`; muxer: `filmcraft-isobmff`'s `Mp4Writer`
//! (the roadmap's M14 builds video on FilmCraft's crates).
//!
//! The file is written to `<out>.partial` and renamed by [`SequenceWriter::finish`]; dropping the
//! writer unfinished (an error, a cancel) removes the partial file, so a failed export never
//! leaves a truncated movie behind.
//!
//! Native only: on wasm32 the crate is empty.
//!
//! Sources: own design over FilmCraft's public crate APIs (same organisation).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-util")))]
pub mod probe;
