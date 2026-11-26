//! Content-aware hole filling (inpainting) for the Remove tool, backed by `craft_develop::inpaint`.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub use craft_develop::inpaint::{Cancelled, CompleteParams, complete, complete_with, membrane, membrane_fill, patchmatch};
