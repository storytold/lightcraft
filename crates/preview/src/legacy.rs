//! On-disk names from the project's previous name. Thumbnail caches made by those builds carry them,
//! so they stay as written: renaming them would make the cache look through its folder again.
//! This is the only preview file allowed to spell the previous name.

/// The marker file saying a thumbnail folder has been looked through for thumbnails under the
/// name of before (see `disk::DiskCache::take_over_old`).
pub const TIDIED: &str = ".lightcraft-thumbs";
