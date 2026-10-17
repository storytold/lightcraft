//! The auto updater: find a newer release of the app on GitHub, download the file for this
//! machine, and stage installing it — the shape Tauri's updater has, with no extra service behind
//! it.
//!
//! The pieces, in the order a session uses them:
//!
//! 1. [`check`] reads the release feed — the static `latest.json` the release pipeline publishes,
//!    GitHub's Releases API, or a feed a Rust caller names ([`Options::manifest_url`]: a test or a
//!    mirror) — compares the newest release's version with this build ([`version`]), and picks the
//!    asset that belongs to this platform ([`target`]). Only files under this repository's release
//!    downloads are offered, and only over https.
//! 2. [`download`] fetches that asset into the cache folder, resuming an interrupted transfer,
//!    capped, and checked against the size the feed states and the release's `SHA256SUMS.txt`
//!    ([`checksum`]) before it is renamed into place. A file that fails its check is deleted.
//! 3. [`install`] says what installing will do ([`install::Plan`]) and, when asked, does it: the
//!    Windows installer runs, a macOS disk image is mounted and its app checked and swapped in for
//!    the running one, an AppImage replaces itself, and anything else is laid out where the user can unpack
//!    it. Every path ends with the app running again, or with a sentence saying what to do.
//!
//! Never a panic: no network, a rate-limited API, a release without a file for this machine, a
//! truncated download, a full disk, an installer that refuses — all are [`UpdateError`] values a
//! dialog can show, and every one of them leaves the current installation untouched.
//!
//! Native only: on wasm32 the crate is empty (the web build updates by reloading the page).
//!
//! Sources: own design, from the documented GitHub Releases API and the Web Linking/CDN layout of
//! release assets; version precedence follows Semantic Versioning 2.0.0.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod checksum;
mod error;
mod release;
mod version;

mod check;
mod download;
mod install;
mod target;

pub use check::{
    Cache, Checked, MAX_DOWNLOAD_BYTES, Options, REUSE_AFTER, Update, api_url, check, from_release, manifest_url, release_download_prefix,
};
pub use download::{Downloaded, client, download, part_path, stage_dir};
pub use error::UpdateError;
pub use install::{Plan, Restart, clean_up_after_update, install, install_with, plan, reveal, runs_from_appimage};
pub use release::{Release, newest, version_of};
pub use target::{Asset, Kind, Os, Target};
pub use version::Version;

/// The `User-Agent` GitHub's API requires: the brand's binary name and version.
pub fn user_agent() -> String {
    lcc_net::user_agent()
}
