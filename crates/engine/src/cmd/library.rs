//! Library commands: view source, filter/sort, selection, ratings/flags/labels, rotate, delete,
//! metadata, albums, import.

use lightcraft_catalog::{Album, AlbumId, ColorLabel, CopyrightStatus, Filter, Flag, GroupBy, Op, PhotoId, Sort, SortKey};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, has_active, has_library_selection, has_selection, ok, str_param};
use crate::{LibrarySource, Result, Selection, Session};

mod filters;
mod gps;
mod import;
mod selection;
mod specs;

pub(crate) use filters::covers_other_disks;
pub use import::{ImportRequest, import_batch_done, import_params, list_auto_import_folder};
pub use specs::specs;

#[cfg(test)]
#[path = "library/tests_relative_ratings.rs"]
mod relative_rating_tests;

#[cfg(test)]
#[path = "library/tests_flag_cycle.rs"]
mod flag_cycle_tests;

#[cfg(test)]
#[path = "library/tests_rated_out.rs"]
mod rated_out_tests;

#[cfg(test)]
#[path = "library/tests_enable_filters.rs"]
mod enable_filters_tests;

#[cfg(test)]
#[path = "library/tests_source_album_filter.rs"]
mod source_album_filter_tests;
