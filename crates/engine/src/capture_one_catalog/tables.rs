//! The catalog's tables as rows with named columns, and the check that the reader's columns exist.

use std::collections::HashMap;
use std::path::Path;

use super::{ReadError, VersionInfo};
use crate::lightroom_sqlite::{Database, FileSource, LiveTable, Record, Value};

/// The tables and columns without which no photo can be read. Everything else is optional: a
/// missing column reads as empty, a missing table as no rows.
const REQUIRED: &[(&str, &[&str])] = &[
    ("ZPATHLOCATION", &["Z_PK", "ZISRELATIVE", "ZRELATIVEPATH", "ZMACROOT", "ZWINROOT"]),
    ("ZIMAGE", &["Z_PK", "ZIMAGELOCATION", "ZIMAGEFILENAME"]),
    ("ZVARIANT", &["Z_PK", "ZIMAGE", "ZCOMBINEDSETTINGS"]),
    ("ZVARIANTMETADATA", &["ZLAYER"]),
];
/// Most `ZVERSIONINFO` rows kept (a catalog gains one or two per Capture One release).
const MAX_VERSIONS: usize = 1_000;
/// Numbers beyond this are not exact in a float, so a float that large is not an id or a count.
const MAX_EXACT: f64 = 9.0e15;

pub(super) struct Db {
    db: Database,
    tables: Vec<LiveTable>,
}

/// A table's rows. Column names are upper case, as Capture One writes them.
pub(super) struct Table {
    columns: HashMap<String, usize>,
    rows: Vec<Record>,
}

#[derive(Clone, Copy)]
pub(super) struct Row<'a> {
    columns: &'a HashMap<String, usize>,
    values: &'a [Value],
}

/// The page reader reports a file that changed under it as text; keep that apart from damage.
fn sqlite_error(e: String) -> ReadError {
    if e.contains("changed during reading") { ReadError::Changed } else { ReadError::Invalid(e) }
}

impl Db {
    pub(super) fn open(database: &Path) -> Result<Self, ReadError> {
        let source = FileSource::open(database).map_err(sqlite_error)?;
        // only once the file is taken as it is: a transaction Capture One began before would
        // otherwise be read as the catalog, and one it begins after changes the file, which
        // `ensure_unchanged` sees
        super::locate::refuse_while_written(database)?;
        let db = Database::open_file_with_wal(source, &[]).map_err(sqlite_error)?;
        let tables = db.live_tables().map_err(sqlite_error)?;
        Ok(Self { db, tables })
    }

    pub(super) fn ensure_unchanged(&self) -> Result<(), ReadError> {
        self.db.ensure_unchanged().map_err(|_| ReadError::Changed)
    }

    fn live(&self, name: &str) -> Option<&LiveTable> {
        self.tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// The rows of `name`; no rows when the catalog has no such table.
    pub(super) fn table(&self, name: &str) -> Result<Table, ReadError> {
        let empty = || Table { columns: HashMap::new(), rows: Vec::new() };
        let Some(live) = self.live(name) else { return Ok(empty()) };
        let Some(names) = live.column_names.as_ref() else { return Ok(empty()) };
        let rows = self.db.read_table(live.rootpage, names.len()).map_err(|e| sqlite_error(format!("{name}: {e}")))?;
        let columns = names.iter().enumerate().map(|(i, n)| (n.to_ascii_uppercase(), i)).collect();
        Ok(Table { columns, rows })
    }

    /// [`Db::table`] for a table the import can do without: a table that can't be read is
    /// reported and read as empty.
    pub(super) fn optional(&self, name: &str, warnings: &mut Vec<String>) -> Result<Table, ReadError> {
        match self.table(name) {
            Err(ReadError::Invalid(why)) => {
                warnings.push(format!("skipping Capture One table {why}"));
                Ok(Table { columns: HashMap::new(), rows: Vec::new() })
            }
            other => other,
        }
    }

    /// Every required table and column the catalog lacks, as an error naming them and the
    /// program that last wrote the catalog.
    pub(super) fn check_schema(&self) -> Result<(), ReadError> {
        let mut missing = Vec::new();
        for (table, columns) in REQUIRED {
            let Some(names) = self.live(table).and_then(|t| t.column_names.as_ref()) else {
                missing.push((*table).to_string());
                continue;
            };
            missing.extend(columns.iter().filter(|c| !names.iter().any(|n| n.eq_ignore_ascii_case(c))).map(|c| format!("{table}.{c}")));
        }
        if missing.is_empty() { Ok(()) } else { Err(ReadError::Unsupported { written_by: self.versions().pop(), missing }) }
    }

    /// `ZVERSIONINFO`, oldest first; empty when it can't be read.
    pub(super) fn versions(&self) -> Vec<VersionInfo> {
        let Ok(table) = self.table("ZVERSIONINFO") else { return Vec::new() };
        let mut rows: Vec<_> = table.rows().map(|r| (r.int("Z_PK").unwrap_or(0), r)).collect();
        rows.sort_by_key(|(pk, _)| *pk);
        rows.into_iter()
            .take(MAX_VERSIONS)
            .map(|(_, r)| VersionInfo {
                app: r.text("ZAUTHOR").trim().to_string(),
                schema: r.int("ZVERSION").unwrap_or(0),
                compatible: r.int("ZCOMPATIBLEVERSION").unwrap_or(0),
            })
            .collect()
    }

    /// `ZDOCUMENTCONTENT.ZDOCUMENTUUID`, when it looks like an id.
    pub(super) fn document_uuid(&self) -> Option<String> {
        let table = self.table("ZDOCUMENTCONTENT").ok()?;
        let id = table.rows().next()?.text("ZDOCUMENTUUID").trim().to_string();
        (!id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(id)
    }
}

impl Table {
    pub(super) fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(|r| Row { columns: &self.columns, values: &r.values })
    }

    /// The names of the table's columns.
    pub(super) fn columns(&self) -> impl Iterator<Item = &str> {
        self.columns.keys().map(String::as_str)
    }
}

impl<'a> Row<'a> {
    fn value(&self, column: &str) -> Option<&'a Value> {
        self.values.get(*self.columns.get(column)?)
    }

    /// A whole number; a float only when it is a whole number a float holds exactly (`1.0`, not
    /// `1.5`, which would otherwise point at row 1).
    pub(super) fn int(&self, column: &str) -> Option<i64> {
        match self.value(column)? {
            Value::Integer(n) => Some(*n),
            Value::Real(f) if f.is_finite() && f.abs() <= MAX_EXACT && f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    /// A reference to another row: Capture One writes `-1` or NULL for none.
    pub(super) fn id(&self, column: &str) -> Option<i64> {
        self.int(column).filter(|n| *n > 0)
    }

    /// A finite number, also when stored as text (`ZGPSLATITUDE` is a `VARCHAR`).
    pub(super) fn real(&self, column: &str) -> Option<f64> {
        let n = match self.value(column)? {
            Value::Integer(n) => *n as f64,
            Value::Real(f) => *f,
            Value::Text(s) => s.trim().parse().ok()?,
            _ => return None,
        };
        n.is_finite().then_some(n)
    }

    /// Text as stored; empty for anything that isn't text.
    pub(super) fn text(&self, column: &str) -> &'a str {
        match self.value(column) {
            Some(Value::Text(s)) => s,
            _ => "",
        }
    }

    pub(super) fn flag(&self, column: &str) -> bool {
        self.int(column).is_some_and(|n| n != 0)
    }
}
