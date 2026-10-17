//! Sandboxed WebAssembly pixel filters: a host for **PhotoCraft plug-in ABI v1** filter modules.
//!
//! A filter is a WebAssembly module exporting `memory`, `pc_abi_version`, `pc_manifest`,
//! `pc_alloc` and `pc_filter` (the ABI is documented in the crate README). It runs in [`wasmi`], a
//! pure-Rust interpreter, with:
//!
//! - **no host imports**: no WASI, file system, network, clock or randomness. A module that
//!   imports anything is rejected;
//! - an **instruction budget** (fuel) per call, metered in slices so a wall-clock deadline
//!   (native builds) and the caller's cancel flag are checked while it runs;
//! - a **linear-memory cap**, a **recursion-depth cap** and wasmi's strict module limits;
//! - a **fresh instance per band**, so no state leaks between calls.
//!
//! Every failure (malformed module, trap, exhausted budget, bad output) is an [`Error`]; nothing
//! a module does can panic or hang the host.
//!
//! [`Filter::apply_rgb`] runs a filter over an interleaved linear RGB `f32` buffer in bands.
//! [`Registry`] holds installed filters. Ported from the PhotoCraft plug-in host (same authors,
//! MIT/Apache-2.0).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod apply;
pub mod manifest;
mod registry;
mod runtime;
mod sniff;

pub use apply::FrameInfo;
pub use manifest::{Area, Kind, Manifest, ParamSpec, Stage};
pub use registry::{FolderReport, Registry};
pub use runtime::{ABI_VERSION, Filter, Limits};
pub use sniff::is_filter_module;

/// A filter error. Every way a module can misbehave ends here, never in a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a valid filter module: {0}")]
    Module(String),
    #[error("filter ABI: {0}")]
    Abi(String),
    #[error("filter manifest: {0}")]
    Manifest(String),
    #[error("filter trapped: {0}")]
    Trap(String),
    #[error("filter exceeded its {0}")]
    Limit(String),
    #[error("filter parameters: {0}")]
    Params(String),
    #[error("filter failed: {0}")]
    Failed(String),
    #[error("filter run cancelled")]
    Cancelled,
    #[error("no filter {0:?} is installed")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;
