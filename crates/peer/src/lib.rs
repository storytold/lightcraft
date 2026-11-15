//! Finding, pairing with and talking to the app on other devices on the local network.
//!
//! - [`beacon`]: the multicast DNS service discovery advert (RFC 6762 / RFC 6763, Bonjour) that
//!   says "the app runs here" (and whether it is looking for a device to pair with).
//! - [`dns`]: the bounds-checked DNS message encoder / decoder it uses.
//! - [`identity`]: this device (id, name, Ed25519 key) and the devices it is paired with.
//! - [`secure`]: the encrypted, mutually authenticated channel, and the pairing code.
//! - [`service`]: the threads that tie these together, and what Settings ▸ Sync shows.
//!
//! The catalog exchange between paired devices is built on top of this (in the engine).
//!
//! # The handshake (protocol `lcc-peer-v2`)
//!
//! Every connection starts with a commit-then-reveal handshake ([`secure`] has the details):
//!
//! 1. The side that connected sends a **commitment**: SHA-256 over its hello, which holds its
//!    device id, Ed25519 key, a fresh X25519 key and a fresh 32-byte nonce.
//! 2. The side that accepted answers with its own **hello**.
//! 3. The first side **reveals** its hello; the other checks it against the commitment.
//! 4. Both sign the transcript (commitment and both hellos) and derive the session keys and the
//!    six-digit pairing code from it and the X25519 secret.
//!
//! Neither side can choose its hello after seeing the other's, so a device relaying between two
//! others can't try key after key until both screens show the same code: it gets one blind guess
//! per connection it completes, and a device that sees many unfinished connections refuses
//! pairing requests for a while. Hellos with fields this version doesn't know are refused.
//!
//! Version 1 (hellos first, no commitment) is not spoken any more. A device on the old version is
//! told apart by its advert (no `proto` entry) and by its first message; both give
//! [`Error::Version`]: both devices need the same version of the app.
//!
//! After the handshake a connection says what it is for: a pairing request, a paired device's
//! hello (the catalog exchange follows) or a paired device's check that it is still paired. A
//! device that isn't paired (any more) is answered with "not paired" ([`Error::NotPaired`]).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod beacon;
#[cfg(not(target_arch = "wasm32"))]
pub mod dns;
#[cfg(not(target_arch = "wasm32"))]
pub mod identity;
#[cfg(not(target_arch = "wasm32"))]
pub mod secure;
#[cfg(not(target_arch = "wasm32"))]
pub mod service;

#[cfg(not(target_arch = "wasm32"))]
pub use identity::{Identity, Peer, Store};
#[cfg(not(target_arch = "wasm32"))]
pub use secure::{Channel, Frame};
#[cfg(not(target_arch = "wasm32"))]
pub use service::{HelloHandler, Service, Stage, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(String),
    /// A stored file that can't be used.
    Corrupt(String),
    /// The other side said or sent something unusable.
    Protocol(String),
    /// A pairing is already in progress.
    Busy,
    /// The two devices aren't paired with each other (any more): this one doesn't know the other,
    /// or the other answered that it doesn't know this one (it was unpaired there). Pair again.
    NotPaired,
    /// The other device speaks another version of the protocol: both need the same app version.
    Version,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(m) | Error::Corrupt(m) | Error::Protocol(m) => f.write_str(m),
            Error::Busy => f.write_str("a pairing is already in progress"),
            Error::NotPaired => f.write_str("the two devices aren't paired with each other (any more); pair them again"),
            Error::Version => f.write_str("the two devices run different versions of the app; both devices need the same app version"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
