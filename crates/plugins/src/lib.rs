//! Sandboxed WebAssembly plug-ins (Phase 4.3).
//!
//! A plug-in is a WebAssembly module implementing the plug-in ABI (v1, documented in
//! `docs/plugins.md`). It runs in [`wasmi`], a pure-Rust interpreter, under:
//!
//! - an **instruction budget** (fuel) per call, metered in slices so a wall-clock deadline is also
//!   enforced on native builds; a **linear-memory cap**, a **recursion cap** and wasmi's strict
//!   module limits; a **fresh instance per call**, so no state leaks between calls;
//! - **one narrow door to the host**: the only imports a module may have are `host.call`,
//!   `host.response` and `host.log`. Every request through `host.call` is a JSON message that the
//!   host checks against the plug-in's **granted capabilities** ([`Permissions`]: catalog read,
//!   standard-metadata write, network hosts, filesystem roots) before doing anything.
//!
//! What a plug-in can contribute (declared in its [`Manifest`]): menu commands with an optional
//! declarative dialog, export post-process hooks, metadata/keyword providers and publish services
//! (through the [`publish::PublishProvider`] trait boundary). The [`Manager`] keeps the installed
//! plug-ins, their grants (given on install, revocable), enabled state and logs.
//!
//! Every failure (malformed module, trap, exhausted budget, denied capability, bad output) is an
//! [`Error`]; nothing a module does can panic or hang the host.
//!
//! The app reaches its catalog through [`HostApi`], which the engine implements; this crate
//! depends on no app crate, so it stays at the bottom of the layer stack.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod context;
pub mod manager;
pub mod manifest;
pub mod permissions;
pub mod publish;
mod runtime;
mod store;

pub use context::{HostApi, NoHost};
pub use manager::{Installed, Manager};
pub use manifest::{CommandDecl, Field, FieldKind, Hooks, Manifest, PublishDecl};
pub use permissions::{FsRoot, Permissions};
pub use runtime::{ABI_VERSION, Limits, Plugin};
pub use store::NamespaceStore;

/// A plug-in error. Every way a module can misbehave ends here, never in a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a valid plug-in module: {0}")]
    Module(String),
    #[error("plug-in ABI: {0}")]
    Abi(String),
    #[error("plug-in manifest: {0}")]
    Manifest(String),
    #[error("plug-in trapped: {0}")]
    Trap(String),
    #[error("plug-in exceeded its {0}")]
    Limit(String),
    #[error("plug-in arguments: {0}")]
    Params(String),
    #[error("plug-in failed: {0}")]
    Failed(String),
    #[error("no plug-in {0:?} is installed")]
    NotFound(String),
    #[error("plug-in {0:?} is disabled")]
    Disabled(String),
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;
