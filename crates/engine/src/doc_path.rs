//! Paths stored in a file LightCraft reads but the user did not choose path by path, such as the
//! originals a Lightroom catalog lists.
//!
//! Such a path reaches the file system only through [`resolve`]: a relative path is taken from the
//! document's folder, and only local files, or files on the network share the document itself is
//! on, are looked at (see `craft_docpath`). An operation may also use [`NetworkShares`] for UNC
//! shares explicitly chosen by the user. A path that is not followed is handled like a missing
//! file.

use std::fmt;
use std::path::{Path, PathBuf};

pub use craft_docpath::Refused;

/// Maximum number of shares explicitly granted by one import; discovery has no such cap.
pub const MAX_NETWORK_SHARES: usize = 32;

/// Network shares explicitly selected by the caller for this operation. This is not saved in
/// documents or global settings. Construction and resolution are lexical and perform no I/O.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkShares {
    paths: Vec<String>,
    explicit: bool,
}

impl NetworkShares {
    /// A folder on each allowed UNC share. Access is granted to the whole server/share pair,
    /// using craft-docpath's normalisation and comparison, never a string-prefix comparison.
    pub fn new(paths: Vec<String>) -> Result<Self, String> {
        if paths.len() > MAX_NETWORK_SHARES {
            return Err("networkShares: at most 32 network shares may be selected".into());
        }
        for path in &paths {
            if path.len() > 32_768
                || craft_docpath::classify(path) != craft_docpath::PathKind::Network
                || craft_docpath::resolve(path, Some(Path::new(path))).is_err()
            {
                return Err("networkShares: choose a UNC network folder such as \\\\server\\share\\Photos".into());
            }
        }
        Ok(Self { paths, explicit: true })
    }

    /// Parse the explicit command option before reading any catalog or original.
    pub fn from_params(params: &serde_json::Value) -> Result<Self, String> {
        match params.get("networkShares") {
            None | Some(serde_json::Value::Null) => Ok(Self::default()),
            Some(serde_json::Value::Array(values)) if values.len() > MAX_NETWORK_SHARES => {
                Err("networkShares: at most 32 network shares may be selected".into())
            }
            Some(serde_json::Value::Array(values)) => Self::new(
                values
                    .iter()
                    .map(|v| v.as_str().map(str::to_string).ok_or_else(|| "networkShares must be an array of UNC folder paths".to_string()))
                    .collect::<Result<_, _>>()?,
            ),
            Some(_) => Err("networkShares must be an array of UNC folder paths".into()),
        }
    }

    /// UNC folders supplied for this operation; no implicit document grant is listed here.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Preserve the distinction between the default document policy and an explicit empty selection.
    pub fn param_value(&self) -> serde_json::Value {
        if self.explicit { serde_json::json!(self.paths) } else { serde_json::Value::Null }
    }

    /// Resolve against the real document first. Only a network refusal can be retried using
    /// an explicitly chosen share; device, URL and invalid-path refusals remain refusals.
    pub fn resolve(&self, stored: &str, document: Option<&Path>) -> Result<PathBuf, NotFollowed> {
        // Resolve relative originals against the real catalog too: a relative path inside a
        // network catalog must not bypass an unchecked share via the document's implicit grant.
        let network_path = self
            .explicit
            .then(|| craft_docpath::resolve(stored, document).ok())
            .flatten()
            .filter(|p| p.to_str().is_some_and(|s| craft_docpath::classify(s) == craft_docpath::PathKind::Network));
        let stored = network_path.as_deref().and_then(Path::to_str).unwrap_or(stored);
        let result = if network_path.is_some() { Err(NotFollowed::Refused(Refused::Network)) } else { resolve(stored, document) };
        if result != Err(NotFollowed::Refused(Refused::Network)) {
            return result;
        }
        for share in &self.paths {
            match resolve(stored, Some(Path::new(share))) {
                Err(NotFollowed::Refused(Refused::Network)) => {}
                allowed => return allowed,
            }
        }
        result
    }
}

/// Canonical UNC server/share of a stored path, without accessing that server.
/// Validation uses the same parser as import; remote URLs and device paths are not offers.
pub fn network_share(path: &str) -> Option<String> {
    if path.len() > 32_768 || craft_docpath::classify(path) != craft_docpath::PathKind::Network {
        return None;
    }
    let normalized = craft_docpath::resolve(path, Some(Path::new(path))).ok()?;
    let normalized = normalized.to_str()?;
    let tail = normalized.strip_prefix(r"\\?\UNC\").or_else(|| normalized.strip_prefix(r"\\"))?;
    let mut parts = tail.split('\\');
    let host = parts.next()?;
    let share = parts.next()?;
    Some(format!(r"\\{host}\{share}"))
}

/// Why a stored path is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFollowed {
    /// The path is on another computer (other than the document's share), names a device, is a
    /// URL, or is not a usable path.
    Refused(Refused),
    /// The path is not an absolute path on this computer, such as a Windows drive path read on
    /// macOS or Linux.
    NotHere,
}

impl fmt::Display for NotFollowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => r.fmt(f),
            Self::NotHere => f.write_str("the path is not a path on this computer"),
        }
    }
}

impl std::error::Error for NotFollowed {}

/// The file to look at for `stored`, a path or `file:` URL stored in the document at `document`,
/// or why it is not followed. A relative `document` is taken from the working directory, where it
/// was opened from.
pub fn resolve(stored: &str, document: Option<&Path>) -> Result<PathBuf, NotFollowed> {
    let document = document.map(|d| if d.is_relative() { std::path::absolute(d).unwrap_or_else(|_| d.to_path_buf()) } else { d.to_path_buf() });
    match craft_docpath::resolve_absolute(stored, document.as_deref()) {
        Ok(path) => Ok(path),
        // A usable path that is not absolute here: a Windows path on macOS or Linux, or a
        // relative path with no document folder to take it from.
        Err(Refused::Invalid) if craft_docpath::resolve(stored, document.as_deref()).is_ok() => Err(NotFollowed::NotHere),
        Err(r) => Err(NotFollowed::Refused(r)),
    }
}

/// Distinguishes a refused redirect from an unavailable original that can stay offline.
#[derive(Debug)]
struct NetworkRedirect;
impl fmt::Display for NetworkRedirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("network originals through reparse points are not imported")
    }
}
impl std::error::Error for NetworkRedirect {}

/// Recognize a refused redirect so it cannot be retained as an ordinary offline original.
pub(crate) fn is_network_redirect(error: &std::io::Error) -> bool {
    error.get_ref().is_some_and(|e| e.is::<NetworkRedirect>())
}

#[cfg(windows)]
pub(crate) mod network;

/// Open a catalog original for reading. UNC paths are opened component by component relative
/// to directory handles, refusing reparse points; the returned handle is the one to read.
/// Local paths retain their usual filesystem semantics. Share authorization precedes this call.
pub(crate) fn open_original(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    let path = path.as_ref();
    #[cfg(windows)]
    if network::is_unc(path) {
        return network::open(path, true);
    }
    std::fs::File::open(path)
}

/// Query an original through the same no-follow UNC handle walk used for reading.
/// A later read must call `open_original` again, never open the previously checked path unchecked.
pub(crate) fn original_metadata(path: &Path) -> std::io::Result<std::fs::Metadata> {
    #[cfg(windows)]
    if network::is_unc(path) {
        return network::open(path, false)?.metadata();
    }
    std::fs::metadata(path)
}

/// Read an original or sidecar using the validated handle, without reopening its path.
pub(crate) fn read_original(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = open_original(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Read a UTF-8 sidecar through a no-follow handle, preserving the usual UTF-8 error behavior.
pub(crate) fn read_original_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    String::from_utf8(read_original(path)?).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Explicit grants match complete server/share pairs and never authorize devices, URLs or traversal.
    fn selected_network_shares_allow_only_the_chosen_server_and_share() {
        let catalog = std::env::temp_dir().join("local.lrcat");
        let selected = NetworkShares::new(vec![r"\\nas\other\folder".into(), r"\\NAS\Photos\chosen-folder".into()]).unwrap();
        let allowed = [r"\\nas\photos\trip\a.jpg", "//nas/photos/trip/a.jpg", r"\\?\UNC\nas\photos\trip\a.jpg"];
        for path in allowed {
            assert_eq!(NetworkShares::default().resolve(path, Some(&catalog)), Err(NotFollowed::Refused(Refused::Network)));
            let found = selected.resolve(path, Some(&catalog));
            #[cfg(windows)]
            assert!(found.is_ok(), "{path}: {found:?}");
            #[cfg(not(windows))]
            assert_eq!(found, Err(NotFollowed::NotHere), "UNC paths are not native Unix paths");
        }
        for path in [r"\\other\photos\a.jpg", r"\\nas\photos-private\a.jpg", "file://nas/private/a.jpg", "file://nas/photos/trip/a.jpg"] {
            assert_eq!(selected.resolve(path, Some(&catalog)), Err(NotFollowed::Refused(Refused::Network)), "{path}");
        }
        for path in
            [r"\\.\COM1", r"\\nas\photos\NUL", "https://nas/photos/a.jpg", r"\\nas\photos\..\private\a.jpg", "file://nas/photos/%2e%2e/private/a.jpg"]
        {
            assert!(matches!(selected.resolve(path, Some(&catalog)), Err(NotFollowed::Refused(_))), "{path}");
        }
        assert_eq!(
            selected.resolve("local.jpg", Some(&catalog)),
            resolve("local.jpg", Some(&catalog)),
            "local paths stay relative to the real catalog"
        );
    }

    #[test]
    /// Discovery canonicalizes share names while an empty explicit grant overrides the implicit catalog share.
    fn network_sources_are_canonical_and_an_explicit_empty_selection_denies_the_document_share() {
        for path in [r"\\nas\photos\Trip", "//nas/photos/Trip", r"\\?\UNC\nas\photos\Trip"] {
            assert_eq!(network_share(path).as_deref(), Some(r"\\nas\photos"));
        }
        for path in ["C:/Photos", r"\\.\COM1", "file://nas/photos/a.jpg", r"\\nas\photos\..\other"] {
            assert_eq!(network_share(path), None, "{path}");
        }
        let document = Path::new(r"\\nas\photos\catalog.lrcat");
        let empty = NetworkShares::new(Vec::new()).unwrap();
        assert_eq!(empty.resolve(r"\\nas\photos\a.jpg", Some(document)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(empty.resolve("a.jpg", Some(document)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(NetworkShares::default().resolve(r"\\nas\photos\a.jpg", Some(document)), resolve(r"\\nas\photos\a.jpg", Some(document)));
    }

    #[test]
    /// Reject malformed or excessive grants before any filesystem access.
    fn network_share_options_are_explicit_bounded_and_validated() {
        use serde_json::json;
        assert_eq!(NetworkShares::from_params(&json!({})).unwrap(), NetworkShares::default());
        for option in [
            json!(true),
            json!("\\\\nas\\photos"),
            json!([1]),
            json!(["C:/Photos"]),
            json!(["https://nas/photos"]),
            json!([r"\\.\COM1"]),
            json!(["file://nas/photos"]),
            json!([r"\\nas\photos\..\other"]),
            json!(vec![r"\\nas\photos"; 33]),
        ] {
            assert!(NetworkShares::from_params(&json!({"networkShares": option})).is_err(), "{option}");
        }
        let paths = vec![r"\\nas\photos".to_string()];
        assert_eq!(NetworkShares::from_params(&json!({"networkShares": paths})).unwrap().paths(), paths);
    }

    #[test]
    fn relative_paths_are_taken_from_the_document_folder_and_other_computers_are_refused() {
        let doc = std::env::temp_dir().join("lrcat").join("catalog.lrcat");
        assert_eq!(resolve("Photos/a.jpg", Some(&doc)), Ok(std::env::temp_dir().join("lrcat").join("Photos").join("a.jpg")));
        assert_eq!(resolve("a.jpg", None), Err(NotFollowed::NotHere));
        assert_eq!(resolve(r"\\192.0.2.1\share\x.jpg", Some(&doc)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(resolve("file://192.0.2.1/s/x.jpg", Some(&doc)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(resolve(r"\\.\COM1", Some(&doc)), Err(NotFollowed::Refused(Refused::Device)));
        #[cfg(windows)]
        assert_eq!(resolve("Photos/2024:05/a.jpg", Some(&doc)), Err(NotFollowed::Refused(Refused::Invalid)));
        #[cfg(not(windows))]
        assert_eq!(resolve(r"C:\Photos\a.jpg", Some(&doc)), Err(NotFollowed::NotHere));
        #[cfg(not(windows))]
        let gvfs = Path::new("/run/user/1000/gvfs/smb-share:server=tokyo,share=photos/catalog.lrcat");
        #[cfg(not(windows))]
        assert_eq!(resolve("a.jpg", Some(gvfs)), Ok(gvfs.with_file_name("a.jpg")));
    }
}
