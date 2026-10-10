//! HTTP(S) for everything that talks to a server: Immich, publish services, map tiles and
//! geocoding, model downloads. Pure Rust: `std::net` sockets and rustls with the RustCrypto
//! provider (no `ring`/`aws-lc-rs`, nothing compiled from C), Mozilla roots from `webpki-roots`,
//! gzip through `flate2`'s Rust backend.
//!
//! - [`Client`] with [`ClientConfig`]: connect and stall timeouts, redirects (credentials never
//!   follow a redirect to another origin, and https never downgrades to http), an optional HTTP
//!   proxy (`CONNECT` for https), and per-connection [`Trust`]: extra CA certificates, pinned
//!   fingerprints, and trust-on-first-use with a confirmation hook for self-signed home servers.
//! - [`Request`]: GET/HEAD/POST/PUT/PATCH/DELETE, headers ([`Request::secret_header`] /
//!   [`Request::bearer`] for credentials), JSON or raw bodies, streaming [`Multipart`] uploads
//!   with progress, cancellation through an `AtomicBool`.
//! - [`Response`]: status, headers, the body as a [`std::io::Read`] stream (gzip decoded), or
//!   capped [`Response::bytes`] / [`Response::text`] / [`Response::json`].
//! - [`Client::download`]: resumable, size-capped, SHA-256-verified downloads to a file.
//!
//! Blocking by design: callers run requests on worker threads. Logging never includes header
//! values, bodies or query strings (they carry API keys and share keys).
//!
//! Native only: on wasm32 this crate is empty.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod client;
mod conn;
mod download;
mod error;
pub mod http;
mod multipart;
mod tls;
mod url;

pub use client::{Client, ClientConfig, Method, Progress, Request, Response};
pub use download::{DownloadOptions, Downloaded};
pub use error::NetError;
pub use multipart::Multipart;
pub use tls::{CertInfo, Fingerprint, TofuHook, Trust};
pub use url::Url;

/// `User-Agent`: the brand's binary name and the version.
pub fn user_agent() -> String {
    format!("{}/{}", dac_brand::BINARY, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_robust;
