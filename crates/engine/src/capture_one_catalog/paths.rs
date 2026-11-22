//! Where an original is: its folder's `ZPATHLOCATION` row plus the file name.

use std::path::Path;

use super::Original;
use super::tables::Row;
use crate::doc_path::{self, NotFollowed};

/// An original's path and whether it may be looked at.
pub(super) struct Located {
    pub path: String,
    pub original: Original,
    pub managed: bool,
}

/// Whether a path part names a place of its own: `Path::join` would put it in place of the
/// folder it is meant to be below.
fn is_absolute_part(part: &str) -> bool {
    let bytes = part.as_bytes();
    part.starts_with(['/', '\\'])
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':'))
        || Path::new(part).is_absolute()
}

/// A path relative to the catalog with `/` between its parts; `.` parts and doubled separators
/// are dropped, `..` parts kept. Catalogs made on Windows record `\`, also when macOS reads them.
fn relative_parts(stored: &str) -> Vec<&str> {
    stored.split(['/', '\\']).filter(|part| !part.is_empty() && *part != ".").collect()
}

fn refused(path: String, why: &str) -> Located {
    Located { path, original: Original::Refused(why.into()), managed: false }
}

fn resolved(stored: String, database: &Path, managed: bool) -> Located {
    match doc_path::resolve(&stored, Some(database)) {
        Ok(found) => Located { path: found.to_string_lossy().into_owned(), original: Original::Local, managed },
        Err(NotFollowed::NotHere) => Located { path: stored, original: Original::NotHere, managed },
        Err(NotFollowed::Refused(why)) => Located { path: stored, original: Original::Refused(why.to_string()), managed: false },
    }
}

/// The original `name` in the folder `location` names, resolved with the catalog's database as
/// the document. `location` is `None` when the image's `ZPATHLOCATION` row is gone.
pub(super) fn original(location: Option<Row<'_>>, name: &str, database: &Path) -> Located {
    let Some(location) = location else { return refused(name.to_string(), super::notice::NO_FOLDER) };
    if name.is_empty() || is_absolute_part(name) {
        return refused(name.to_string(), super::notice::BAD_FILE_NAME);
    }
    let below = location.text("ZRELATIVEPATH");
    if location.flag("ZISRELATIVE") {
        return in_catalog(below, name, database);
    }
    let (here, elsewhere) =
        if cfg!(windows) { (location.text("ZWINROOT"), location.text("ZMACROOT")) } else { (location.text("ZMACROOT"), location.text("ZWINROOT")) };
    if !here.is_empty() {
        return resolved(under_root(here, below, name, cfg!(windows)), database, false);
    }
    if !elsewhere.is_empty() {
        // recorded on the other system only: kept as an offline record, never looked at. Written
        // with `/`, which both systems read, so the record's file name is the file's here too.
        let path = under_root(&elsewhere.replace('\\', "/"), below, name, false);
        return Located { path, original: Original::NotHere, managed: false };
    }
    refused(name.to_string(), super::notice::NO_FOLDER_RECORDED)
}

/// A file in a folder recorded relative to the catalog. It is managed when the folder is inside
/// the catalog's (no `..`), as `./Originals` is.
fn in_catalog(below: &str, name: &str, database: &Path) -> Located {
    if is_absolute_part(below) {
        return refused(name.to_string(), super::notice::ABSOLUTE_FOLDER);
    }
    let mut parts = relative_parts(below);
    let managed = !parts.contains(&"..");
    parts.push(name);
    resolved(parts.join("/"), database, managed)
}

/// `root` + `below` + `name` with the separator of the system the root is from. A root keeps its
/// spaces: `/Volumes/NAME   ` is a real mount point.
fn under_root(root: &str, below: &str, name: &str, windows: bool) -> String {
    let separator = if windows { '\\' } else { '/' };
    let mut path = root.trim_end_matches(['/', '\\']).to_string();
    for part in relative_parts(below).into_iter().chain([name]) {
        path.push(separator);
        path.push_str(part);
    }
    path
}
