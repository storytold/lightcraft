//! Import: files and folders (recursive) → photos.
//!
//! 1. Expand folders recursively into supported files (hidden entries and the library's own
//!    folder are skipped), sorted for a stable order.
//! 2. Skip paths already in the catalog.
//! 3. Probe the rest — dimensions, metadata, and the **content hash** of the bytes — in parallel
//!    on native targets.
//! 4. Skip **duplicates by content** (same bytes already in the library, or twice in this batch).
//!    A duplicate (by path or content) of a photo in Recently Deleted is restored instead, with
//!    its edits, unless the import says otherwise ([`OnDeleted`]).
//! 5. *Add* in place (the photo points at the original file), *copy* into the library's
//!    `Originals/YYYY/YYYY-MM-DD/` folder (names made unique) and point at the copy, or *move*
//!    there: placed like a copy (with its XMP sidecars), and the original removed only once its
//!    catalog record is committed and saved (see [`crate::import_move`] for the safety rules).
//! 6. Apply each photo's XMP sidecar (or a raw/DNG file's embedded XMP): the sidecar wins for
//!    metadata and develop settings are restored (see [`crate::sidecar`]). Its capture time fills
//!    in when the file has none (read before step 5, so it also files and names the copy).
//! 7. Optionally apply a preset and add keywords (the import dialog's options).
//! 8. Commit all new photos as one undoable op. (Undoing a move removes the catalog records;
//!    the files stay at the destination — undo never moves files back over a card.)
//! 9. Copy and Move rename sequences use checked platform-sized numbers: the last number is usable once, and any
//!    further renamed files are reported as failed without losing earlier completed transfers.
//!
//! The import dialog first calls [`scan`] (`library.importPreview`): the same expansion, probing
//! and duplicate detection without adding anything, so the user can review the candidates. The
//! probes are kept and reused by the import that follows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde::{Deserialize, Serialize};

use crate::Session;
use crate::media::ProbeInfo;

mod commit;
mod defaults;
mod files;
mod job;
mod scan;
mod types;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use commit::workers;
pub use commit::{batch_size, civil, commit_prepared, system_clock};
pub use defaults::{CameraDefault, ImportDefaults, apply_import_defaults, has_import_look, import_defaults};
pub use files::{
    EXTENSIONS, LAUNCH_PROBE_LIMIT, LaunchPath, accept_list, expand, expand_within, is_supported, launch_deadline, launch_path, launch_path_until,
};
pub(crate) use files::{expand_noting, probe_all, probe_paths, probe_with};
#[cfg(test)]
use job::PreparedItem;
pub use job::{ImportJob, Prepared, candidate_thumb_key, import, import_with};
pub(crate) use scan::scan_files;
pub use scan::{ScanInput, ScanOutput, ScanProgress, scan, scan_with};
#[cfg(test)]
use scan::{library_files, library_hashes};
pub use types::{
    Duplicate, FileFacts, ImportCandidate, ImportMode, ImportOptions, ImportReport, ImportUprightMode, ImportWarning, ImportWarningCode, Kept, Moved,
    OnDeleted, Organize,
};

#[cfg(test)]
#[path = "import/tests_known.rs"]
mod tests_known;
