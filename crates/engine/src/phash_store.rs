//! A file's perceptual hash ([`crate::phash`]) kept on disk in the preview cache, so a picture
//! looked at once is never looked at again: not after a restart, not by the next duplicate scan.
//!
//! **Key** ([`file_key`]): the file itself — its canonical path, size and modified time — with
//! [`crate::phash::PHASH_VERSION`] and this format's version, like the eye finder's records
//! ([`crate::eye_store`]). It needs no catalog entry, and a file that changed is hashed afresh.
//!
//! **Format**: a `.lcdata` record of [`lcc_preview::DiskCache`] (counted in the cache's size, let
//! go with the thumbnails when it is full): `LCPH`, the format version (u32 LE), the hash (u64 LE).
//! A record that doesn't read is dropped and the picture hashed again; it never stops anything.

use lcc_preview::{Hash128, Hasher128, PreviewCache};

use crate::phash::PHASH_VERSION;

const MAGIC: &[u8; 4] = b"LCPH";
/// The record's layout (bump when it changes; the key holds it too).
pub const FORMAT: u32 = 1;

/// The record's key for the file at `path`: `None` when the file can't be looked up.
pub fn file_key(path: &str) -> Option<Hash128> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
    let m = std::fs::metadata(&canon).ok().filter(|m| m.is_file())?;
    let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
    Some(
        Hasher128::new()
            .str("phash-file")
            .str(&canon.to_string_lossy())
            .u64(m.len())
            .u64(modified)
            .u64(PHASH_VERSION)
            .u64(u64::from(FORMAT))
            .finish(),
    )
}

/// The hash kept for `key`; a record that doesn't read is dropped (`None`, hash again).
pub fn read(cache: &PreviewCache, key: Hash128) -> Option<u64> {
    let bytes = cache.get_data(key)?;
    let record = if bytes.len() == 16 && bytes.get(..4) == Some(MAGIC) && bytes.get(4..8) == Some(&FORMAT.to_le_bytes()) {
        bytes.get(8..16).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
    } else {
        None
    };
    if record.is_none() {
        log::warn!("duplicates: dropping an unreadable phash record {key} in the preview cache");
        cache.remove_data(key);
    }
    record
}

/// Keep `hash` under `key`.
pub fn write(cache: &PreviewCache, key: Hash128, hash: u64) {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&hash.to_le_bytes());
    cache.put_data(key, &out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder that goes away with the test, however it ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_key_follows_the_file_and_a_record_round_trips() {
        let dir = Scratch(std::env::temp_dir().join(format!("lc-phash-store-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir.0);
        std::fs::create_dir_all(&dir.0).unwrap();
        let f = dir.0.join("a.jpg");
        std::fs::write(&f, b"pixels").unwrap();
        let key = file_key(f.to_str().unwrap()).unwrap();
        // a changed file (here: a different size) is another key, so it is hashed afresh
        std::fs::write(&f, b"more pixels").unwrap();
        assert_ne!(key, file_key(f.to_str().unwrap()).unwrap());
        assert_eq!(file_key(dir.0.join("nope").to_str().unwrap()), None);

        let cache_dir = dir.0.join("cache");
        let cache = lcc_preview::PreviewCache::with_disk(1 << 20, &cache_dir, 8 << 20);
        assert_eq!(read(&cache, key), None);
        write(&cache, key, 0x0123_4567_89ab_cdef);
        assert_eq!(read(&cache, key), Some(0x0123_4567_89ab_cdef));
        // a record of another shape is dropped, not returned
        cache.put_data(key, b"LCXX");
        assert_eq!(read(&cache, key), None);
    }
}
