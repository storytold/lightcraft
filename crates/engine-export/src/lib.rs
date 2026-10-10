//! Engine export: output options and their validation, encoding (JPEG, PNG, TIFF, AVIF, DNG
//! options), output sharpening, text and graphic watermarks, the embedded craft fonts and the
//! file-name templates. Re-exported by the `dac-engine` façade, which prepares and runs export
//! batches against the session.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod export;
pub mod fonts;
pub mod naming;

pub use fonts::{CRAFT_FONTS, CraftFont};
