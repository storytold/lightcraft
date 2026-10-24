//! Synthetic Lightroom catalogs for tests: a small SQLite file written page by page, so the catalog reader is tested
//! on the bytes it reads in the field (text and BLOB cells, Lightroom's compressed encoding) with no SQLite library.
//! Each table is one leaf page; rows must fit in it without overflow pages.

use std::path::{Path, PathBuf};

const PAGE_SIZE: usize = 4096;
/// The largest record a 4096-byte table leaf holds without an overflow page (SQLite file format §1.6: U - 35).
const MAX_LOCAL: usize = PAGE_SIZE - 35;

#[derive(Clone, Debug)]
pub(crate) enum Field {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

pub(crate) fn text(s: &str) -> Field {
    Field::Text(s.into())
}

pub(crate) struct Table {
    pub name: &'static str,
    /// The column list of its `CREATE TABLE`, as Lightroom writes it.
    pub columns: &'static str,
    pub rows: Vec<Vec<Field>>,
}

/// Lightroom Classic's compressed text (as in `Adobe_AdditionalMetadata.xmp`, and history steps and snapshots since
/// Lightroom Classic 13): the decoded length as 4 big-endian bytes, then the zlib stream.
pub(crate) fn compressed(text: &str) -> Vec<u8> {
    compressed_bytes(text.as_bytes())
}

/// [`compressed`] for any bytes, including ones that aren't UTF-8.
pub(crate) fn compressed_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
    out.extend(miniz_oxide::deflate::compress_to_vec_zlib(bytes, 6));
    out
}

pub(crate) const HISTORY_COLUMNS: &str = "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, dateCreated, digest, \
     hasBigData INTEGER NOT NULL DEFAULT 0, hasDevelopAdjustments, image INTEGER, name, relValueString, text, valueString";
pub(crate) const SNAPSHOT_COLUMNS: &str = "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, digest, \
     hasBigData INTEGER NOT NULL DEFAULT 0, hasDevelopAdjustments, image INTEGER, locked, name, snapshotID, text";

/// A history step row for image `image` (columns as in [`HISTORY_COLUMNS`]); `settings` is a text or BLOB field.
pub(crate) fn history_step(id: i64, image: i64, date: f64, name: &str, settings: Field) -> Vec<Field> {
    vec![
        Field::Integer(id),
        text(&format!("history-{id}")),
        Field::Real(date),
        Field::Null,
        Field::Integer(0),
        Field::Integer(1),
        Field::Integer(image),
        text(name),
        Field::Null,
        settings,
        Field::Null,
    ]
}

/// A snapshot row for image `image` (columns as in [`SNAPSHOT_COLUMNS`]); `settings` is a text or BLOB field.
pub(crate) fn snapshot(id: i64, image: i64, name: &str, settings: Field) -> Vec<Field> {
    vec![
        Field::Integer(id),
        text(&format!("snapshot-{id}")),
        Field::Null,
        Field::Integer(0),
        Field::Integer(1),
        Field::Integer(image),
        Field::Null,
        text(name),
        Field::Null,
        settings,
    ]
}

/// The tables of a catalog with one photo, image 1 (`<originals>/Photos/test.ARW`, not created), with the develop
/// settings `settings`. Add history, snapshot or other tables and pass them all to [`write`].
pub(crate) fn one_photo(originals: &Path, settings: &str) -> Vec<Table> {
    let mut root = originals.to_string_lossy().into_owned();
    if !root.ends_with('/') {
        root.push('/');
    }
    vec![
        Table {
            name: "AgLibraryRootFolder",
            columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, absolutePath UNIQUE NOT NULL DEFAULT '', name NOT NULL DEFAULT '', relativePathFromCatalog",
            rows: vec![vec![Field::Integer(1), text("root-1"), Field::Text(root), text("originals"), Field::Null]],
        },
        Table {
            name: "AgLibraryFolder",
            columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, pathFromRoot NOT NULL DEFAULT '', rootFolder INTEGER NOT NULL DEFAULT 0",
            rows: vec![vec![Field::Integer(1), text("folder-1"), text("Photos/"), Field::Integer(1)]],
        },
        Table {
            name: "AgLibraryFile",
            columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, baseName NOT NULL DEFAULT '', extension NOT NULL DEFAULT '', folder INTEGER NOT NULL DEFAULT 0, idx_filename NOT NULL DEFAULT ''",
            rows: vec![vec![Field::Integer(1), text("file-1"), text("test"), text("ARW"), Field::Integer(1), text("test.ARW")]],
        },
        Table {
            name: "Adobe_images",
            columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, fileFormat NOT NULL DEFAULT 'unset', fileHeight, fileWidth, masterImage INTEGER, pick NOT NULL DEFAULT 0, rating, rootFile INTEGER NOT NULL DEFAULT 0",
            rows: vec![vec![
                Field::Integer(1),
                text("image-1"),
                text("RAW"),
                Field::Integer(10),
                Field::Integer(20),
                Field::Null,
                Field::Integer(0),
                Field::Null,
                Field::Integer(1),
            ]],
        },
        Table {
            name: "Adobe_imageDevelopSettings",
            columns: "id_local INTEGER PRIMARY KEY, image INTEGER, text",
            rows: vec![vec![Field::Integer(1), Field::Integer(1), text(settings)]],
        },
    ]
}

/// The format header an import archive starts with (`lightroom_archive`'s), before its zlib-compressed JSON.
pub(crate) const ARCHIVE_HEADER: &[u8] = b"LC-LRARCH\0\x01Z";

/// The JSON the import archive at `path` holds, for tests that check what an import kept. The JSON itself is capped
/// at `MAX_ARCHIVE_BYTES` too, so that is the most it can inflate to.
pub(crate) fn read_archive(path: &Path) -> serde_json::Value {
    let bytes = std::fs::read(path).unwrap();
    let json = bytes.strip_prefix(ARCHIVE_HEADER).unwrap_or_else(|| panic!("{} is not an import archive", path.display()));
    serde_json::from_slice(
        &miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(json, crate::lightroom_archive::MAX_ARCHIVE_BYTES as usize).unwrap(),
    )
    .unwrap()
}

/// A new, empty folder for one test's catalog.
pub(crate) fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-lrcat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write `tables` as a SQLite database at `path`: page 1 holds the schema, table `i` page `i + 2`.
pub(crate) fn write(path: &Path, tables: &[Table]) {
    let mut schema = Vec::new();
    let mut pages = Vec::new();
    for (i, table) in tables.iter().enumerate() {
        let root = i64::try_from(i + 2).unwrap();
        schema.push(vec![
            text("table"),
            text(table.name),
            text(table.name),
            Field::Integer(root),
            Field::Text(format!("CREATE TABLE {}({})", table.name, table.columns)),
        ]);
        pages.push(leaf(0, &table.rows, table.name));
    }
    let mut first = leaf(100, &schema, "sqlite_master");
    first[..16].copy_from_slice(b"SQLite format 3\0");
    first[16..18].copy_from_slice(&u16::try_from(PAGE_SIZE).unwrap().to_be_bytes());
    first[18] = 1; // legacy journal mode: no WAL file is read unless it exists
    first[19] = 1;
    first[21] = 64;
    first[22] = 32;
    first[23] = 32;
    first[24..28].copy_from_slice(&1u32.to_be_bytes());
    first[28..32].copy_from_slice(&u32::try_from(pages.len() + 1).unwrap().to_be_bytes());
    first[40..44].copy_from_slice(&1u32.to_be_bytes());
    first[44..48].copy_from_slice(&4u32.to_be_bytes());
    first[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
    first[92..96].copy_from_slice(&1u32.to_be_bytes());
    first[96..100].copy_from_slice(&3_045_000u32.to_be_bytes());
    let mut bytes = first;
    for page in pages {
        bytes.extend(page);
    }
    std::fs::write(path, bytes).unwrap();
}

fn leaf(base: usize, rows: &[Vec<Field>], table: &str) -> Vec<u8> {
    let mut page = vec![0u8; PAGE_SIZE];
    page[base] = 0x0d;
    page[base + 3..base + 5].copy_from_slice(&u16::try_from(rows.len()).unwrap().to_be_bytes());
    let mut end = PAGE_SIZE;
    for (i, values) in rows.iter().enumerate() {
        let payload = record(values);
        assert!(payload.len() <= MAX_LOCAL, "{table} row {i}: {} bytes needs an overflow page, which the fixture doesn't write", payload.len());
        let rowid = match values.first() {
            Some(Field::Integer(id)) => u64::try_from(*id).unwrap(),
            _ => u64::try_from(i + 1).unwrap(),
        };
        let mut cell = varint(u64::try_from(payload.len()).unwrap());
        cell.extend(varint(rowid));
        cell.extend(payload);
        let pointers_end = base + 8 + (i + 1) * 2;
        end = end.checked_sub(cell.len()).filter(|e| *e >= pointers_end).unwrap_or_else(|| panic!("{table}: rows don't fit in one page"));
        page[end..end + cell.len()].copy_from_slice(&cell);
        page[pointers_end - 2..pointers_end].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
    }
    page[base + 5..base + 7].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
    page
}

/// A record (SQLite file format §2.1). An `INTEGER PRIMARY KEY` column's value is the rowid, stored as NULL.
fn record(values: &[Field]) -> Vec<u8> {
    let mut serials = Vec::new();
    let mut body = Vec::new();
    for (i, value) in values.iter().enumerate() {
        let serial = match value {
            Field::Integer(_) if i == 0 => 0,
            Field::Null => 0,
            Field::Integer(n) => {
                body.extend(n.to_be_bytes());
                6
            }
            Field::Real(n) => {
                body.extend(n.to_be_bytes());
                7
            }
            Field::Text(s) => {
                body.extend(s.as_bytes());
                13 + 2 * u64::try_from(s.len()).unwrap()
            }
            Field::Blob(b) => {
                body.extend(b);
                12 + 2 * u64::try_from(b.len()).unwrap()
            }
        };
        serials.extend(varint(serial));
    }
    // the header length counts its own varint
    let mut header_len = serials.len() + 1;
    if varint(u64::try_from(header_len).unwrap()).len() > 1 {
        header_len += 1;
    }
    let mut out = varint(u64::try_from(header_len).unwrap());
    out.extend(serials);
    out.extend(body);
    out
}

/// SQLite's big-endian varint (values below 2^56, which is all a fixture needs).
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = vec![(value & 0x7f) as u8];
    value >>= 7;
    while value != 0 {
        out.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    out.reverse();
    out
}
