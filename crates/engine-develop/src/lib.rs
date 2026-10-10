//! Engine develop: presets and profiles (built-in looks, `.lcpreset` files), preset import
//! (`.lrtemplate`, XMP, Luminar looks) and the `crs:` XMP develop-settings mapping. Re-exported by the
//! `dac-engine` façade, where the develop commands run against the session.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod crs;
pub mod crs_masks;
pub mod preset_import;
pub mod preset_luminar;
pub mod presets;

pub use dac_engine_core::{legacy, walk};
