//! Paths stored in a file LightCraft reads but the user did not choose path by path, such as the
//! originals a Lightroom catalog lists.
//!
//! Such a path reaches the file system only through [`resolve`]: a relative path is taken from the
//! document's folder, and only local files, or files on the network share the document itself is
//! on, are looked at (see `craft_docpath`). A path that is not followed is handled like a missing
//! file.

use std::fmt;
use std::path::{Path, PathBuf};

pub use craft_docpath::Refused;

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

#[cfg(test)]
mod tests {
    use super::*;

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
        let gvfs = Path::new("/run/user/1000/gvfs/smb-share:server=tokyo,share=photos/catalog.lrcat");
        #[cfg(not(windows))]
        assert_eq!(resolve("a.jpg", Some(gvfs)), Ok(gvfs.with_file_name("a.jpg")));
    }
}
