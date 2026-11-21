//! Files a cloud sync client keeps online only: a placeholder with the file's name and size, and
//! no bytes on this computer. Reading one makes the client download it (Dropbox, iCloud Drive,
//! OneDrive, Google Drive, Box on macOS and Windows), which for a library of thousands of photos
//! is hours of downloading. Asking whether a file is a placeholder reads only its metadata, which
//! the system answers without downloading anything.
//!
//! - macOS: File Provider placeholders carry `SF_DATALESS` in their `st_flags` (`ls -lO` shows
//!   `dataless`).
//! - Windows: Cloud Files placeholders carry `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` or
//!   `FILE_ATTRIBUTE_RECALL_ON_OPEN`; older clients set `FILE_ATTRIBUTE_OFFLINE`.
//! - Elsewhere there is no such mark, so nothing is a placeholder.

use std::path::Path;

/// `SF_DATALESS` (`<sys/stat.h>`): the file's contents are not on this computer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAC_DATALESS: u32 = 0x4000_0000;

/// `FILE_ATTRIBUTE_OFFLINE`, `FILE_ATTRIBUTE_RECALL_ON_OPEN` and
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` (`winnt.h`).
#[cfg_attr(not(windows), allow(dead_code))]
const WINDOWS_NOT_LOCAL: u32 = 0x0000_1000 | 0x0004_0000 | 0x0040_0000;

/// Whether macOS `st_flags` mark a placeholder.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_flags_are_placeholder(flags: u32) -> bool {
    flags & MAC_DATALESS != 0
}

/// Whether Windows file attributes mark a placeholder.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_attributes_are_placeholder(attributes: u32) -> bool {
    attributes & WINDOWS_NOT_LOCAL != 0
}

/// Whether `meta` describes a regular file whose contents are online only.
pub fn is_placeholder(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        mac_flags_are_placeholder(meta.st_flags())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        windows_attributes_are_placeholder(meta.file_attributes())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

/// Whether `path` is a file whose contents are online only (`false` when it can't be looked at).
pub fn path_is_placeholder(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| is_placeholder(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_dataless_flag_marks_a_placeholder() {
        assert!(mac_flags_are_placeholder(0x4000_0000));
        // `compressed,dataless`, as `ls -lO` shows a Dropbox online-only file
        assert!(mac_flags_are_placeholder(0x4000_0020));
        assert!(!mac_flags_are_placeholder(0));
        // hidden, compressed, user-immutable: all downloaded
        assert!(!mac_flags_are_placeholder(0x8000 | 0x20 | 0x2));
    }

    #[test]
    fn windows_recall_attributes_mark_a_placeholder() {
        assert!(windows_attributes_are_placeholder(0x0040_0000), "recall on data access (OneDrive, Dropbox)");
        assert!(windows_attributes_are_placeholder(0x0004_0000), "recall on open");
        assert!(windows_attributes_are_placeholder(0x0000_1000), "offline");
        // archive, read-only, pinned (0x80000), unpinned (0x100000) alone: the bytes are here
        assert!(!windows_attributes_are_placeholder(0x20 | 0x1 | 0x0008_0000));
        assert!(!windows_attributes_are_placeholder(0x20 | 0x0010_0000));
    }

    #[test]
    fn an_ordinary_file_or_a_folder_is_not_a_placeholder() {
        struct Removed(std::path::PathBuf);
        impl Drop for Removed {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("lightcraft-cloud-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // (removed when the test ends, after a failed assertion as well)
        let _removed = Removed(dir.clone());
        let file = dir.join("photo.jpg");
        std::fs::write(&file, b"bytes").unwrap();
        assert!(!path_is_placeholder(&file));
        assert!(!path_is_placeholder(&dir));
        assert!(!path_is_placeholder(&dir.join("not-there.jpg")));
    }
}
