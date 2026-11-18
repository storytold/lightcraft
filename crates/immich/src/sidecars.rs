//! IMM-SIDECARS: XMP sidecars as Immich stores them (plan/immich.md → Sidecars).
//!
//! Immich keeps an `.xmp` sidecar next to every uploaded asset (its library copy) and, for an
//! external library, beside the original on disk. Its public API (v3.2.0) lists an asset's files
//! (`GET /asset-files?assetId=…&type=sidecar`) and serves one (`GET /asset-files/{id}/download`),
//! so the app can read back the XMP it sent with an upload — including its own `lc:settings`
//! develop settings — and apply it to another catalog photo. That is what makes an Immich server
//! a bridge between two libraries: one instance publishes originals with their sidecars, another
//! imports or links the assets and pulls the sidecars back.
//!
//! Writing a *managed* asset's sidecar is not exposed by the API: a sidecar travels only with an
//! upload, and afterwards Immich merges the fields it supports (description, rating, capture time,
//! location, tags) into the sidecar when they are updated. For an external library the sidecar is
//! the file on disk, which the app writes itself. [`crate::publish`] sends a photo's whole XMP with
//! its original.
//!
//! Sources: own design; Immich's public API documentation and its XMP sidecar documentation
//! (plan/immich.md → Sidecars).

use lcc_catalog::XmpStatus;

use crate::types::{MIN_ASSET_FILES_VERSION, ServerVersion};

/// The most bytes one sidecar may be. A sidecar holds metadata, not pixels; a larger answer is
/// refused rather than read into memory (a hostile or broken server must not exhaust the app).
pub const MAX_SIDECAR_BYTES: u64 = 16 * 1024 * 1024;

/// The most sidecars one run pulls (network round trips, not a library scan).
pub const MAX_PULL_ITEMS: usize = 5000;

/// The most sidecar bytes one run keeps in memory.
pub const MAX_PULL_BYTES: u64 = 256 * 1024 * 1024;

/// Whether `v`'s public API lists and serves an asset's sidecar files.
pub fn supports_asset_files(v: ServerVersion) -> bool {
    v >= MIN_ASSET_FILES_VERSION
}

/// Why pulling a sidecar onto a photo is skipped without `overwrite`: the local copy would lose
/// work. `status` is the photo's XMP state, `local_edited` its last edit time and `asset_updated`
/// the asset's `updatedAt` (ISO 8601). A photo edited here after the asset last changed stays as
/// it is; the caller reports the reason.
pub fn pull_skip_reason(status: XmpStatus, local_edited: Option<&str>, asset_updated: Option<&str>) -> Option<&'static str> {
    match status {
        XmpStatus::ChangedInCatalog => return Some("changedInCatalog"),
        XmpStatus::Conflict => return Some("conflict"),
        XmpStatus::Unknown | XmpStatus::InSync | XmpStatus::ChangedOnDisk => {}
    }
    let edited = local_edited.and_then(crate::sync::instant);
    let remote = asset_updated.and_then(crate::sync::instant);
    (edited.zip(remote).is_some_and(|(local, remote)| local > remote)).then_some("editedLocally")
}

/// The sidecar file name Immich prefers for an original: `IMG_0001.jpg` → `IMG_0001.jpg.xmp`
/// (Immich also accepts `IMG_0001.xmp`, and prefers the full name when both exist).
pub fn sidecar_file_name(original: &str) -> String {
    format!("{original}.xmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_pull_keeps_local_work() {
        // a clean photo is pulled
        assert_eq!(pull_skip_reason(XmpStatus::InSync, None, None), None);
        assert_eq!(pull_skip_reason(XmpStatus::Unknown, Some("2026-01-02T00:00:00"), Some("2026-01-01T00:00:00Z")), Some("editedLocally"));
        // the catalog changed since its sidecar was last in step
        assert_eq!(pull_skip_reason(XmpStatus::ChangedInCatalog, None, None), Some("changedInCatalog"));
        assert_eq!(pull_skip_reason(XmpStatus::Conflict, None, None), Some("conflict"));
        // a sidecar changed on disk is no reason to refuse (the app reads the server's copy)
        assert_eq!(pull_skip_reason(XmpStatus::ChangedOnDisk, None, None), None);
        // the asset is newer than the last local edit
        assert_eq!(pull_skip_reason(XmpStatus::InSync, Some("2026-01-01T00:00:00"), Some("2026-01-02T00:00:00Z")), None);
        // the local edit is newer
        assert_eq!(pull_skip_reason(XmpStatus::InSync, Some("2026-01-03T00:00:00Z"), Some("2026-01-02T00:00:00Z")), Some("editedLocally"));
        // a missing or unparseable time changes nothing
        assert_eq!(pull_skip_reason(XmpStatus::InSync, Some("not a time"), Some("2026-01-02T00:00:00Z")), None);
    }
}
