//! Where account secrets (API keys, tokens, passwords) live: never in the catalog, settings,
//! logs, MCP output or crash reports.
//!
//! - [`os_store`]: the platform keychain, where it is reachable in pure Rust without `unsafe`.
//!   Linux and the BSDs use the freedesktop Secret Service (GNOME Keyring, KWallet) over D-Bus
//!   with `zbus`. macOS Keychain and Windows Credential Manager need FFI, which this workspace
//!   keeps out of product crates (see AGENTS.md: `unsafe` lives only in `crates/sysmem`), so there
//!   [`os_store`] answers [`CredError::Unsupported`] and callers use the file store. The platform
//!   code is isolated in `platform/`, so a safe backend can drop in later.
//! - [`FileStore`]: a file encrypted with XChaCha20-Poly1305 under a key derived from the user's
//!   passphrase with Argon2id. Wrong passphrases and tampering are detected, never "decrypted"
//!   to garbage; writes are atomic.
//!
//! [`Secret`] holds a secret in memory: its `Debug`/`Display` never show the value and the memory
//! is zeroed on drop. Read it with [`Secret::expose`] only where it is sent.
//!
//! Native only: on wasm32 this crate is empty.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod file;
mod platform;

pub use file::FileStore;

use zeroize::Zeroizing;

/// A secret value. Never printed: `{:?}` and `{}` show `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(Zeroizing::new(value.into()))
    }

    /// The value, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Which secret: a service (`"immich"`) and an account within it (e.g. `server URL + user`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub service: String,
    pub account: String,
}

impl Key {
    pub fn new(service: &str, account: &str) -> Key {
        Key { service: service.to_string(), account: account.to_string() }
    }
}

/// Why a secret could not be read or written. Messages never contain secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredError {
    /// No keychain backend on this platform: use [`FileStore`].
    Unsupported(&'static str),
    /// The keychain exists but can't be reached (no session bus, no Secret Service running…).
    Unavailable(String),
    /// The keychain is locked and unlocking needs a prompt.
    Locked,
    /// The passphrase does not open the file (or the file was altered).
    WrongPassphrase,
    /// The file is not a credentials file of a known format.
    Corrupt(String),
    Io(String),
}

impl std::fmt::Display for CredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredError::Unsupported(e) => write!(f, "no system keychain available: {e}"),
            CredError::Unavailable(e) => write!(f, "the system keychain can't be reached: {e}"),
            CredError::Locked => write!(f, "the system keychain is locked; unlock it and try again"),
            CredError::WrongPassphrase => write!(f, "wrong passphrase, or the credentials file was altered"),
            CredError::Corrupt(e) => write!(f, "unreadable credentials file: {e}"),
            CredError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CredError {}

/// A place to keep secrets.
pub trait SecretStore: Send + Sync {
    /// Short description for settings ("Secret Service", "encrypted file").
    fn name(&self) -> &'static str;
    fn get(&self, key: &Key) -> Result<Option<Secret>, CredError>;
    /// Store or replace.
    fn set(&self, key: &Key, secret: &Secret) -> Result<(), CredError>;
    /// `true` when something was deleted.
    fn delete(&self, key: &Key) -> Result<bool, CredError>;
}

/// The platform keychain, or why there is none.
pub fn os_store() -> Result<Box<dyn SecretStore>, CredError> {
    platform::open()
}

#[cfg(test)]
mod tests;
