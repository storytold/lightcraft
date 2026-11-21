//! AppKit's Open Documents event, without replacing winit's application delegate.
//!
//! Finder, Open With and Dock drops send `aevt/odoc`. AppKit normally translates this into
//! `application:openURLs:` or `application:openFiles:`, neither exposed by winit 0.30. We handle
//! that event directly. Install after creating NSApplication and before running its event loop.
//! All Objective-C code is isolated here; callers receive owned paths through a safe API.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::OpenDocuments;
