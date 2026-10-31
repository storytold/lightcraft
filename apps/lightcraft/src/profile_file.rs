//! Reading a display's ICC profile file for the screen-profile services (`colord` on Linux,
//! `win_screen_profile` on Windows).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The largest profile file read (real display profiles are a few KiB to a few MiB).
pub const MAX_PROFILE: u64 = 16 << 20;

/// The bytes of profile file `path`, `None` when it can't be read or is implausibly large.
pub fn load(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE + 1).read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty() && bytes.len() as u64 <= MAX_PROFILE).then_some(bytes)
}

/// A profile file read again only when it changes: the services are asked about once a second, and
/// calibration software rewrites its profile under the same name.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Default)]
pub struct Cached {
    /// The file last read: its path, length and modification time.
    stamp: Option<(PathBuf, u64, Option<SystemTime>)>,
    bytes: Option<Vec<u8>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Cached {
    /// The bytes of profile file `path`, read again when it isn't the file read last time or has
    /// changed since (see [`load`]).
    pub fn read(&mut self, path: &Path) -> Option<Vec<u8>> {
        let stamp = std::fs::metadata(path).ok().map(|m| (path.to_path_buf(), m.len(), m.modified().ok()));
        if stamp.is_none() || stamp != self.stamp {
            self.bytes = stamp.as_ref().and_then(|_| load(path));
            self.stamp = stamp;
        }
        self.bytes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_files_are_read_with_a_size_cap() {
        let dir = std::env::temp_dir().join(format!("profile-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.icc");
        std::fs::write(&small, b"acsp").unwrap();
        assert_eq!(load(&small).as_deref(), Some(&b"acsp"[..]));
        let empty = dir.join("empty.icc");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(load(&empty), None);
        assert_eq!(load(&dir.join("missing.icc")), None);
        let big = dir.join("big.icc");
        std::fs::File::create(&big).unwrap().set_len(MAX_PROFILE + 1).unwrap();
        assert_eq!(load(&big), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cached_profile_is_read_again_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("profile-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.icc"), dir.join("b.icc"));
        std::fs::write(&a, b"first").unwrap();
        std::fs::write(&b, b"other").unwrap();
        let mut cache = Cached::default();
        assert_eq!(cache.read(&a).as_deref(), Some(&b"first"[..]));
        assert_eq!(cache.read(&a).as_deref(), Some(&b"first"[..]));
        // another monitor's profile
        assert_eq!(cache.read(&b).as_deref(), Some(&b"other"[..]));
        // rewritten under the same name (a different length, so the change shows whatever the
        // file system's time resolution)
        std::fs::write(&b, b"calibrated").unwrap();
        assert_eq!(cache.read(&b).as_deref(), Some(&b"calibrated"[..]));
        // gone: nothing, then read again once it is back
        std::fs::remove_file(&b).unwrap();
        assert_eq!(cache.read(&b), None);
        std::fs::write(&b, b"back").unwrap();
        assert_eq!(cache.read(&b).as_deref(), Some(&b"back"[..]));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
