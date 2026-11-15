//! What devices that sync keep in step, and how two copies of it merge.
//!
//! - [`clock`]: the hybrid logical clock every change is stamped with, and [`clock::Lww`], a
//!   value that keeps the later of two writes.
//! - [`model`]: the sync group (which devices, catalogue or collections only), and each shared
//!   collection: what its owner allows ([`model::Grant`]) and what each other device takes
//!   ([`model::Take`]).
//! - [`replica`]: one catalogue's copy of all that plus its photos' records and collection
//!   membership, the changes still to send to each device, and the merge of what a device sent.
//! - [`pill`]: the two letters and the arrow shown on a photo or a collection.
//! - [`proto`]: the messages two devices exchange.
//!
//! Nothing here opens a socket or a file: the engine reads and writes the catalogue, the disk and
//! the network, and asks this crate what to send and what to keep.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod clock;
pub mod model;
pub mod pill;
pub mod proto;
pub mod replica;

pub use clock::{Clock, Lww, Stamp};
pub use model::{Collection, CollectionKind, DeviceInfo, Grant, Group, Mode, Take};
pub use pill::{Depth, Location, Pill, Status};
pub use replica::{Item, Replica};

/// 64-bit FNV-1a: the same on every build and platform (unlike `std`'s hasher).
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A fingerprint of a JSON value (of its compact text).
pub fn digest(v: &serde_json::Value) -> u64 {
    fnv64(serde_json::to_string(v).unwrap_or_default().as_bytes())
}
