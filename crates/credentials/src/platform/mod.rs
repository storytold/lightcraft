//! Platform keychains, one module per backend. Everything platform-specific stays in here.

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod secret_service;

use crate::{CredError, SecretStore};

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
pub(crate) fn open() -> Result<Box<dyn SecretStore>, CredError> {
    Ok(Box::new(secret_service::SecretService::connect()?))
}

/// macOS Keychain and Windows Credential Manager are only reachable through FFI, which product
/// crates may not use (AGENTS.md). Until a safe binding exists, callers use the file store.
#[cfg(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
pub(crate) fn open() -> Result<Box<dyn SecretStore>, CredError> {
    Err(CredError::Unsupported("this platform's keychain needs native code; use an encrypted file"))
}
