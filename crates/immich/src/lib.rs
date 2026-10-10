//! Talking to somebody's Immich: browsing, importing from and publishing to a self-hosted
//! [Immich](https://immich.app) server — any server the user names, with a key they created.
//!
//! Nothing here knows about a particular instance: the base URL and the API key are the whole
//! configuration ([`Client::new`]), so a LAN address, a Tailscale name and a reverse-proxied
//! subpath are all just inputs. Requests run on whatever thread calls them (the engine uses a
//! background thread; the UI thread never waits for the network), every call returns
//! [`Result`], cancellation is a flag, and downloads and uploads are size-capped before they are
//! bounded by the peer.
//!
//! Auth is an `x-api-key` header. The key is never formatted into an error message and the
//! [`Client`] `Debug` impl redacts it, so a logged command cannot leak it.
//!
//! The HTTP client is the pure-Rust one from `lightcraft-fetch` (rustls with the RustCrypto
//! provider, webpki roots only), extended with request bodies and multipart uploads: see
//! [`http`] and [`multipart`]. Trusting only the built-in roots means a self-signed certificate
//! fails with [`Error::Http`] naming what to do about it; per-server trust anchors are a
//! follow-up.
//!
//! Native only: on wasm32 this crate is empty.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod api;
pub mod error;
pub mod http;
pub mod multipart;

pub use api::{Album, Asset, Client, Limits, Paged, SearchQuery, ServerVersion};
pub use error::Error;

#[cfg(test)]
mod tests;
