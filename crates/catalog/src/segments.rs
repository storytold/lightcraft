//! Where the segment files of a snapshot in several files will be: in a folder of the library's
//! own, named `segments/<first id>-<seq>.json`. The stores ([`crate::store`]) know these names:
//! they make the folder, list its files and sync it. Nothing writes or reads segments yet: what
//! they hold, and how they are written and read, come with the catalog format that uses them.

/// The folder in the library that holds the segment files.
pub const SEGMENT_DIR: &str = "segments";
/// Segment files are named `segments/<first id>-<seq>.json`: every one starts with this, and
/// ends with [`SEGMENT_SUFFIX`]. (A file an interrupted write left is the same name plus `.tmp`,
/// and is listed under this prefix too.)
pub const SEGMENT_PREFIX: &str = "segments/";
/// Segments are JSON, and say so.
pub const SEGMENT_SUFFIX: &str = ".json";

/// The name of the segment file `<stem>.json` in the segments folder (`stem`: `<first id>-<seq>`).
pub fn segment_file(stem: &str) -> String {
    format!("{SEGMENT_PREFIX}{stem}{SEGMENT_SUFFIX}")
}
