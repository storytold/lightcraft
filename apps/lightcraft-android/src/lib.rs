//! Android host for the shared LightCraft engine and desktop UI.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "android")]
mod app;

// Reuse the desktop's bounded, loopback-only transport for emulator tests.
#[cfg(all(target_os = "android", debug_assertions))]
#[path = "../../lightcraft/src/control_server.rs"]
mod control_server;
pub mod gestures;
#[cfg(target_os = "android")]
mod platform;
pub mod sources;
pub mod storage;
