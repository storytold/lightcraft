//! Immich integration (L3): a typed client for the endpoints the app uses, written from Immich's
//! public API documentation (plan/immich.md; no Immich code or generated SDK), plus the pure parts
//! of the integration that need the catalog:
//!
//! - [`client`]: connect (version check against [`types::MIN_VERSION`], user, key permissions),
//!   paged metadata search, thumbnails, original downloads, albums, people, external libraries;
//!   retries for idempotent calls; errors ([`ImmichError`]) a user can act on.
//! - [`link`]: match assets to catalog photos by SHA-1, else by name + capture time + size
//!   ("probable"), and the remote-link ops to write.
//! - [`mapping`]: Immich metadata → catalog metadata on import (one way).
//! - [`sync`]: two-way metadata sync (IMM-SYNC): snapshots, the three-way merge, conflicts.
//! - [`people`]: Immich faces → face regions, names back to Immich people (IMM-PEOPLE).
//! - [`extlib`]: path mapping between Immich's container paths and local folders.
//! - [`accounts`]: the connected accounts as kept in settings (never the API key).
//!
//! The client is native only; the pure modules build everywhere.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod accounts;
pub mod extlib;
pub mod link;
pub mod mapping;
pub mod people;
pub mod sync;
pub mod types;

#[cfg(not(target_arch = "wasm32"))]
pub mod client;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
pub mod publish;

pub use accounts::{Account, Accounts};
#[cfg(not(target_arch = "wasm32"))]
pub use client::{Client, ServerOptions, ServerStatus};
#[cfg(not(target_arch = "wasm32"))]
pub use error::ImmichError;
pub use types::{Asset, MIN_VERSION, ServerVersion};

#[cfg(test)]
mod tests;
