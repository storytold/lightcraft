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
//!   standard-metadata write, network hosts, filesystem roots, programs) before doing anything.
//!
//! What a plug-in can contribute (declared in its [`Manifest`]): menu commands with an optional
//! declarative dialog (a command may be a job, run on a worker that reports its progress through
//! a [`JobReporter`] and continues with `{continue: state}`), export post-process hooks, metadata/keyword providers and publish services
//! (through the [`publish::PublishProvider`] trait boundary), settings kept for the whole app
//! ([`SettingsStore`]), and programs installed on the computer that it starts ([`programs`]: only
//! the user grants one). The [`Manager`] keeps the installed plug-ins, their grants (given on
//! install, revocable), enabled state, settings and logs.
//!
//! Every failure (malformed module, trap, exhausted budget, denied capability, bad output) is an
//! [`Error`]; nothing a module does can panic or hang the host.
//!
//! The app reaches its catalog through [`HostApi`], which the engine implements; this crate
//! depends on no app crate, so it stays at the bottom of the layer stack.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod about;
mod b64;
mod context;
pub mod manager;
pub mod manifest;
pub mod permissions;
pub mod programs;
pub mod publish;
mod runtime;
mod safe_io;
pub mod settings;
mod store;

pub use about::About;
pub use context::{HostApi, JobReporter, JobUnit, NoHost, PIXELS_DEFAULT_EDGE, PIXELS_MAX_EDGE, PIXELS_MIN_EDGE, Unwatched};
pub use manager::{Begun, Denial, Failed, Installed, Manager, PendingCall, Prepared, RequestState, SettingsView, duplicate_error, read_module};
pub use manifest::{
    CommandDecl, DestinationDecl, Field, FieldKind, HOST_FEATURES, Header, Hooks, MAX_PICTURE_BYTES, MAX_QUICK_ACTIONS, Manifest, MetadataField,
    PublishDecl,
};
pub use permissions::{FsRoot, Group, Permissions, Request, refused_root};
pub use programs::{EndStatus, Exit, Found, ProgramDecl, Reservation, RunDir, Running, StartRequest};
pub use runtime::{ABI_VERSION, Limits, Outcome, Plugin};
pub use settings::SettingsStore;
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
    /// The plug-in requires host features this version of the app doesn't have.
    #[error("{}", missing_message(.0))]
    Unsupported(Vec<String>),
    /// An older version than the installed one, not confirmed.
    #[error("version {new} is older than the installed version {installed}; confirm to install it anyway (allowDowngrade)")]
    Downgrade { installed: String, new: String },
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
    /// Plug-in `.0` is busy: `.1` says with what (an earlier call, a job).
    #[error("plug-in {0:?} is busy: {1}")]
    Busy(String, String),
    /// A job stopped because it was cancelled.
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// "needs `a` and `b`, which this version of the app doesn't have".
fn missing_message(missing: &[String]) -> String {
    let names: Vec<String> = missing.iter().map(|m| format!("`{m}`")).collect();
    let list = match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("the plug-in needs {list}, which this version of the app doesn't have")
}
