//! Finding a catalog's database and telling whether Capture One is using it.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::ReadError;

/// Most entries of a folder that are looked at while searching for the database.
const MAX_ENTRIES: usize = 10_000;
/// The first eight bytes of a SQLite rollback journal that holds a transaction
/// (<https://www.sqlite.org/fileformat.html>, section 3).
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

pub(super) struct Located {
    pub database: PathBuf,
    /// A `writelock` file is next to the database.
    pub in_use: bool,
}

pub(super) fn is_database_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".cocatalogdb") || lower.ends_with(".cocatalogdb.backup")
}

/// The database for `path`: the file itself, or the one in the folder. A folder's database is
/// the one named after the folder (`Name.cocatalog/Name.cocatalogdb`), else its only
/// `.cocatalogdb`; upgrade leftovers (`.cocatalogdb.backup`) are never picked from a folder.
pub(super) fn locate(path: &Path) -> Result<Located, ReadError> {
    let not_found = || ReadError::NotFound(path.to_string_lossy().into_owned());
    let database = if path.is_dir() {
        in_folder(path)?.ok_or_else(not_found)?
    } else if path.is_file() && path.file_name().is_some_and(|n| is_database_name(&n.to_string_lossy())) {
        path.to_path_buf()
    } else {
        return Err(not_found());
    };
    let in_use = database.parent().is_some_and(|folder| folder.join("writelock").is_file());
    Ok(Located { database, in_use })
}

fn in_folder(folder: &Path) -> Result<Option<PathBuf>, ReadError> {
    if let Some(stem) = folder.file_stem() {
        let named = folder.join(format!("{}.cocatalogdb", stem.to_string_lossy()));
        if named.is_file() {
            return Ok(Some(named));
        }
    }
    let Ok(mut entries) = std::fs::read_dir(folder) else { return Ok(None) };
    let mut found: Vec<PathBuf> = entries
        .by_ref()
        .take(MAX_ENTRIES)
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(".cocatalogdb")) && p.is_file())
        .collect();
    // a database past the entries looked at would make the pick depend on the folder's order
    if entries.next().is_some() {
        return Err(ReadError::FolderTooLarge(folder.to_string_lossy().into_owned()));
    }
    found.sort();
    if found.len() > 1 {
        return Err(ReadError::Ambiguous {
            folder: folder.to_string_lossy().into_owned(),
            databases: found.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect(),
        });
    }
    Ok(found.pop())
}

/// The file SQLite keeps next to `database` (`-journal`, `-wal`), from the path's own bytes: a
/// name that isn't UTF-8 still names that file.
pub(super) fn beside(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Refuse a database with a rollback journal that holds a transaction, or with a write-ahead
/// log: its pages may be mid-write, and only SQLite itself can roll that back.
/// A journal or log that is there but can't be read is not taken as absent: it may hold a
/// transaction.
pub(super) fn refuse_while_written(database: &Path) -> Result<(), ReadError> {
    use std::io::ErrorKind;
    let unknown = |what: &str, e: std::io::Error| ReadError::WriteStateUnknown(format!("its {what} can't be read: {e}"));
    let mut start = [0u8; 8];
    let hot = match std::fs::File::open(beside(database, "-journal")) {
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(unknown("journal", e)),
        Ok(mut journal) => match journal.read_exact(&mut start) {
            Ok(()) => start == JOURNAL_MAGIC,
            // shorter than a header: an emptied journal, which holds no transaction
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => false,
            Err(e) => return Err(unknown("journal", e)),
        },
    };
    let wal = match std::fs::metadata(beside(database, "-wal")) {
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(unknown("write-ahead log", e)),
        Ok(m) => m.len() > 0,
    };
    if hot || wal { Err(ReadError::Busy) } else { Ok(()) }
}
