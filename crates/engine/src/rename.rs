//! Batch rename: a file-name template applied to the selected photos, renaming the files on
//! disk (with their XMP sidecars) safely.
//!
//! Template tokens (shared with import renaming and export file naming): `{name}` (current name
//! without extension), `{num}` (the number at the end of the name: `IMG_0042` → `0042`), `{seq}` /
//! `{seq:3}` (sequence number, zero-padded to 3 digits), `{date}` / `{date:%Y%m%d}` (capture date;
//! `%Y %y %m %B %b %d %H %M %S`), `{folder}` (the original's folder name), `{camera}`, `{lens}`, `{iso}`,
//! `{rating}`, `{title}`, `{creator}`, `{ext}` (extension, without the dot). Unknown tokens stay as
//! typed. Characters that aren't allowed in file names become `-`. The original extension is always
//! kept.
//!
//! Safety:
//! - a target that exists on disk, or that another photo in the batch gets, receives a `-1`, `-2`…
//!   suffix — no file is ever overwritten. Whether an existing target is the photo itself (a
//!   change of letter case on a case-insensitive volume) is decided by file identity, not by
//!   comparing names, so on a case-sensitive volume `img_1.jpg` next to `IMG_1.JPG` is a collision
//!   (issue #95); moves never replace a file even if one appears after the check (hard link +
//!   unlink where the volume supports it);
//! - the moves happen before the catalog changes; if one fails, the ones already done are moved
//!   back and nothing is committed;
//! - the catalog op ([`Op::SetFile`]) is undoable: undo/redo move the files back and forth (see
//!   [`crate::Session::undo_step`]);
//! - virtual copies follow their master's file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lightcraft_catalog::dates::MONTHS;
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde::Serialize;

use crate::sidecar::SidecarNaming;
use crate::{EngineError, Result, Session};

mod fs;
mod moves;
mod plan;
mod sidecar;
mod tokens;

pub(crate) use fs::{MoveFs, RealFs};
#[cfg(test)]
use fs::{folded_path, same_contents};
pub(crate) use moves::move_all;
#[cfg(test)]
pub(crate) use moves::move_file_with;
pub use moves::{MoveError, move_file};
pub use plan::{plan_rename_photos, rename_photos};
#[cfg(test)]
pub(crate) use sidecar::PacketVersion;
#[cfg(test)]
use sidecar::RetainedPacket;
pub(crate) use sidecar::{SidecarMove, retry_sidecars, validate_sidecars};
#[cfg(test)]
use tokens::format_date;
pub use tokens::{
    DATE_DIRECTIVES, TEMPLATE_NOTES, TOKENS, TokenHelp, expand, expand_folder, expand_tokens, expand_tokens_with, folder_template_error,
    sample_photo, token_example, token_help_json, token_summary, unknown_tokens,
};

/// One planned rename.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenamePlan {
    pub id: u64,
    pub from: String,
    pub to: String,
    /// File sources: the old and new paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_path: Option<String>,
}

#[cfg(test)]
#[path = "rename/tests_listed_sources.rs"]
mod tests_listed_sources;

#[cfg(test)]
pub(crate) mod tests;
