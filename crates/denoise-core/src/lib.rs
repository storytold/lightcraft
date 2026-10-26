//! Model metadata, bounded archive/cache formats and Bayer tiling, independent of inference.
//! Used by featureless engine and web builds; no neural-network or matrix backend is linked.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
pub mod archive;
pub mod bayer;
pub mod bm3d;
pub mod builtin;
pub mod hash;
pub mod known;
pub mod licence;
pub mod manifest;
pub mod nlm;
pub mod product;
pub mod run;
pub mod tiles;

pub use manifest::{DenoiserManifest, Domain, Gain, ManifestError};
pub use run::{Control, Error, Params, TileRunner, denoise_bayer};
