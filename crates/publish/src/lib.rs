//! Publish services (L3), Lightroom Classic's model (plan: PLAN_phase_4.md §4.2):
//!
//! - a **publish service** ([`ServiceConfig`]: a kind, a name, the kind's settings and the
//!   export settings it renders with) holds **published collections**;
//! - a published collection is a regular catalog album (inside a collection set named after the
//!   service), so adding, removing and showing its photos reuse the album commands and views;
//! - every photo of a published collection is *new*, *modified* (changed since it was
//!   published), *published*, or, once taken out of the collection, *to remove*
//!   ([`state::status`]);
//! - what was published where is a remote link in the catalog's `remote_identity` table
//!   (service [`SERVICE`], account `<service id>/<album id>`, remote id = the service's id for the
//!   published file), whose checksum is the photo's [`state::fingerprint`] at publish time;
//! - services implement [`PublishService`]: the built-in Hard Drive ([`hard_drive`]) now, Immich
//!   and plugin services later.
//!
//! The configuration lives in `publish.json` in the library folder ([`PublishConfig`]).
//! Rendering is the engine's job: a service receives the encoded file.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod config;
pub mod hard_drive;
pub mod service;
pub mod state;

#[cfg(test)]
mod tests;

pub use config::{CollectionConfig, PublishConfig, ServiceConfig};
pub use service::{Capabilities, Comment, PublishError, PublishService, Published, Upload, open_service};
pub use state::{PhotoState, Status, account_id, fingerprint, status};

/// The `service` of every publish link in `remote_identity`.
pub const SERVICE: &str = "publish";
/// The configuration file in the library folder.
pub const FILE: &str = "publish.json";
/// The built-in Hard Drive service's kind.
pub const KIND_HARD_DRIVE: &str = "hardDrive";
/// Service kinds this build can publish with.
pub const KINDS: &[&str] = &[KIND_HARD_DRIVE];
