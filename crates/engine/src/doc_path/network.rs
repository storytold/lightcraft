//! Windows UNC opens. Only the explicitly addressed share root is opened with ambient
//! authority. Every descendant is a single component opened relative to an existing handle;
//! no absolute-path retry, canonicalization-and-reopen, or symlink-target traversal is used.
//!
//! The SMB server is the authority for the contents of its share. Server-side mounts and
//! transport referrals belong to that server's namespace; this is not a sandbox of the server.

use std::fs::{File, Metadata};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt as _};
use std::path::{Component, Path};

use cap_primitives::fs::{OpenOptions, OpenOptionsExt as _};

const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const READ_ATTRIBUTES: u32 = 0x80;
const TRAVERSE: u32 = 0x20;
const SHARE_READ_WRITE: u32 = 3;
const REPARSE_POINT: u32 = 0x400;

/// Recognize native UNC prefixes even when a name contains non-Unicode Windows code units.
/// Such names must fail validation, not fall back to an unchecked filesystem open.
pub(super) fn is_unc(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..)))
}

/// Normalize and validate the entire UNC path before opening its server/share root.
pub(super) fn open(path: &Path, read: bool) -> io::Result<File> {
    let stored = path.to_str().ok_or_else(invalid_path)?;
    let root = super::network_share(stored).ok_or_else(invalid_path)?;
    let normalized = craft_docpath::resolve(stored, Some(path)).map_err(|_| invalid_path())?;
    let mut parts = normalized.components();
    if !matches!(parts.next(), Some(Component::Prefix(_))) || !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(invalid_path());
    }
    let names: Vec<_> = parts.take(257).collect();
    if names.len() > 256 || names.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(invalid_path());
    }
    // No catalog-supplied descendant is part of this ambient open. In particular, do not
    // open the catalog's recorded root folder here: it may itself contain a redirect.
    let root = std::fs::OpenOptions::new()
        .access_mode(READ_ATTRIBUTES | TRAVERSE)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
        .open(root)?;
    check_entry(&root, true)?;
    descend(root, &names, read, |_, _| {})
}

/// Keep ancestors open while resolving each normal component, then return the checked leaf.
/// `observed` lets regression tests attempt a replacement at the exact check/open boundary.
fn descend(root: File, names: &[Component<'_>], read: bool, mut observed: impl FnMut(usize, &File)) -> io::Result<File> {
    let mut ancestors = Vec::new();
    let mut current = root;
    for (index, name) in names.iter().enumerate() {
        let leaf = index + 1 == names.len();
        let mut options = OpenOptions::new();
        options.custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS).share_mode(SHARE_READ_WRITE);
        if leaf && read {
            options.read(true);
        } else {
            options.access_mode(READ_ATTRIBUTES | if leaf { 0 } else { TRAVERSE });
        }
        // cap-primitives uses NtCreateFile relative to `current`. The one-component name
        // cannot re-root or trigger its absolute-path fallback. A remote symlink error is
        // returned to the caller, not retried through Win32's path-based CreateFile API.
        let child = cap_primitives::fs::open(&current, Path::new(name.as_os_str()), &options).map_err(|error| {
            // ERROR_STOPPED_ON_SYMLINK / ERROR_SYMLINK_CLASS_DISABLED: native SMB opens
            // return these instead of evaluating a remote link through another share.
            if matches!(error.raw_os_error(), Some(681 | 1463)) {
                io::Error::new(io::ErrorKind::PermissionDenied, super::NetworkRedirect)
            } else {
                error
            }
        })?;
        let metadata = check_entry(&child, !leaf)?;
        if leaf && read && !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "original is not a regular file"));
        }
        observed(index, &child);
        ancestors.push(current);
        current = child;
    }
    if read && names.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "original is a share root"));
    }
    Ok(current)
}

/// Inspect the opened object itself, rejecting every reparse tag before any descendant or data read.
fn check_entry(file: &File, directory: bool) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    if metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, super::NetworkRedirect));
    }
    if directory && !metadata.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "original path component is not a directory"));
    }
    Ok(metadata)
}

/// Reject malformed input without issuing any network request.
fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid UNC original path")
}

#[cfg(test)]
mod tests;
