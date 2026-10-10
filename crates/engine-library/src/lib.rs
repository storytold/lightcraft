//! Engine library: XMP sidecars (read, write, merge), removable devices and the read-only SQLite
//! reader behind Lightroom Classic catalog import. Re-exported by the `dac-engine` façade, where the
//! library commands (import, folders, collections, keywords, metadata) run against the session.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod devices;
pub mod lightroom_sqlite;
pub mod sidecar;
