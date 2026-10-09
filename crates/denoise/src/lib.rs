//! AI denoise for LightCraft, as a step between the sensor and the demosaic that the rest of the develop
//! pipeline never sees: the raw file stays untouched, the denoised picture is cached data that can always be
//! made again, and the amount is an ordinary develop setting.
//!
//! - [`manifest`]: what a denoise model takes and gives, and who may use it (untrusted JSON, validated).
//! - [`known`] and [`archive`]: the models LightCraft can fetch, and taking the model file out of the `.zip` it comes in.
//! - [`bayer`]: Bayer layouts and the packing of a mosaic into the four planes a model takes.
//! - [`tiles`] and [`run`]: cutting a picture into overlapping tiles for a fixed-size model, bringing each tile's
//!   output scale back to the input's, keeping clipped highlights as they were, and blending the tiles together.
//!   The model itself is a [`run::TileRunner`], so all of this is tested without one.
//! - [`product`]: the cache file the denoised picture is kept in (half floats, compressed in strips) and the
//!   blend with the plain demosaic that the Amount slider controls.
//! - [`cpu`] and [`runtime`]: a single-thread pure-Rust CPU runner and its installation self-test.
//! - [`net`] and [`onnx`]: checked plain network data and bounded ONNX decoding, shared by CPU and GPU.
//!
//! Everything here treats model files, manifests and cache files as hostile input: sizes are capped, numbers must
//! be finite, and a malformed file is an error, never a panic.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(feature = "runtime")]
pub mod cpu;

pub mod net;
pub mod onnx;
mod onnx_proto;

pub mod reference;

#[cfg(feature = "runtime")]
pub mod runtime;
#[doc(hidden)]
pub mod synthetic;

pub use dac_denoise_core::*;

pub mod synthetic_proto;
