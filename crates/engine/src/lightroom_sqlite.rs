//! Small, read-only SQLite reader used by Lightroom catalog import.
//!
//! This is a clean-room implementation based on SQLite's public file-format
//! description: <https://www.sqlite.org/fileformat.html> (sections 1.6, 1.7,
//! 2.1, 2.2, 2.3, 2.4 and 2.5). It is deliberately a page reader, rather
//! than a SQL engine. It never writes its input and has no SQLite runtime
//! dependency.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_DATABASE_BYTES: u64 = 16 << 30;

/// A bounded, read-only page source. Only referenced pages are loaded, never the whole file.
pub struct FileSource {
    file: Mutex<File>,
    path: PathBuf,
    before: Metadata,
    header: Vec<u8>,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !std::fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
            return Err("SQLite catalog must be a regular file".into());
        }
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let before = file.metadata().map_err(|e| e.to_string())?;
        if !before.is_file() {
            return Err("SQLite catalog must be a regular file".into());
        }
        if before.len() > MAX_DATABASE_BYTES {
            return Err("catalog exceeds 16 GiB import limit".into());
        }
        let mut header = vec![0; 100];
        file.read_exact(&mut header).map_err(|e| format!("cannot read SQLite header: {e}"))?;
        let source = Self { file: Mutex::new(file), path: path.into(), before, header };
        source.ensure_unchanged()?;
        Ok(source)
    }

    pub fn header(&self) -> &[u8] {
        &self.header
    }

    pub fn ensure_unchanged(&self) -> Result<(), String> {
        let file = self.file.lock().map_err(|_| "SQLite page source lock poisoned")?;
        let handle = file.metadata().map_err(|e| e.to_string())?;
        let path = std::fs::metadata(&self.path).map_err(|_| "catalog changed during reading; close Lightroom and retry")?;
        if !same_metadata(&self.before, &handle) || !same_metadata(&self.before, &path) {
            return Err("catalog changed during reading; close Lightroom and retry".into());
        }
        Ok(())
    }

    fn page(&self, page_id: u32, size: usize) -> Result<Vec<u8>, String> {
        let start = u64::from(page_id).checked_sub(1).and_then(|n| n.checked_mul(size as u64)).ok_or("page offset overflow")?;
        if start.checked_add(size as u64).is_none_or(|end| end > self.before.len()) {
            return Err(format!("page {page_id} is truncated"));
        }
        let mut out = vec![0; size];
        let mut file = self.file.lock().map_err(|_| "SQLite page source lock poisoned")?;
        file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut out)).map_err(|e| format!("cannot read SQLite page {page_id}: {e}"))?;
        Ok(out)
    }
}

fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return false;
        }
    }
    a.len() == b.len() && a.modified().ok() == b.modified().ok() && a.created().ok() == b.created().ok()
}

const MAX_PAGE_ID: u32 = 16_777_216;
const MAX_COLUMNS: usize = 16_384;
const MAX_ROWS: usize = 1_000_000;
const MAX_CELLS: usize = 4_000_000;
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
const MAX_TABLE_PAYLOAD: usize = 256 * 1024 * 1024;
const MAX_OVERFLOW_PAGES: usize = 1_048_576;
const MAX_BTREE_DEPTH: usize = 64;

/// Values supported by SQLite table records.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A row returned by [`Database::read_table`].
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub values: Vec<Value>,
}

/// A table discovered in `sqlite_schema`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTable {
    pub name: String,
    pub rootpage: u32,
    pub column_names: Option<Vec<String>>,
}

/// Read-only SQLite database image, with a committed WAL overlay if supplied.
pub struct Database {
    main: Vec<u8>,
    file: Option<FileSource>,
    overlay: HashMap<u32, Vec<u8>>,
    page_size: usize,
    reserved: usize,
    encoding: TextEncoding,
    page_count: u32,
    tables: Vec<LiveTable>,
    rowid_aliases: HashMap<u32, usize>,
}

#[derive(Clone, Copy)]
enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

struct WalkState {
    seen: HashSet<u32>,
    out: Vec<Record>,
    payload_total: usize,
}

struct WalOverlay {
    page_size: usize,
    frame_count: usize,
    pages: HashMap<u32, Vec<u8>>,
    page_count: Option<u32>,
}

struct SchemaInfo {
    tables: Vec<LiveTable>,
    aliases: HashMap<u32, usize>,
}

impl Database {
    /// Open a main database image and an already checksum-validated WAL prefix.
    ///
    /// The WAL argument must include its 32-byte header and only complete
    /// frames through a commit. Frames are overlaid by page number, with the
    /// final commit's database-size field defining the visible page count.
    #[cfg(test)]
    pub fn open_with_wal(main: Vec<u8>, committed_wal: &[u8]) -> Result<Self, String> {
        Self::open(main, None, committed_wal)
    }

    pub fn open_file_with_wal(source: FileSource, committed_wal: &[u8]) -> Result<Self, String> {
        Self::open(source.header().to_vec(), Some(source), committed_wal)
    }

    pub fn ensure_unchanged(&self) -> Result<(), String> {
        if let Some(file) = &self.file {
            file.ensure_unchanged()?;
        }
        Ok(())
    }

    fn open(main: Vec<u8>, file: Option<FileSource>, committed_wal: &[u8]) -> Result<Self, String> {
        let main_header = main.get(..100).ok_or("truncated SQLite header")?;
        if main_header.get(..16) != Some(b"SQLite format 3\0") {
            return Err("invalid SQLite header signature".into());
        }
        let page_size = sqlite_page_size(be_u16(main_header, 16)?)?;
        validate_header_fields(main_header, page_size)?;
        let main_len = file.as_ref().map_or(main.len() as u64, |source| source.before.len());
        if main_len < page_size as u64 || !main_len.is_multiple_of(page_size as u64) {
            return Err("SQLite image is not a whole number of pages".into());
        }
        let main_pages = u32::try_from(main_len / page_size as u64).map_err(|_| "SQLite image is too large")?;
        if main_pages == 0 || main_pages > MAX_PAGE_ID {
            return Err("invalid SQLite image page count".into());
        }

        let mut overlay = HashMap::new();
        let mut wal_page_count = None;
        if !committed_wal.is_empty() {
            let parsed = parse_wal(committed_wal, page_size)?;
            if parsed.page_size != page_size {
                return Err("WAL page size mismatch".into());
            }
            if parsed.frame_count > MAX_CELLS {
                return Err("WAL contains too many frames".into());
            }
            overlay = parsed.pages;
            wal_page_count = parsed.page_count;
        }
        let effective_header = overlay.get(&1).map(Vec::as_slice).unwrap_or(main_header);
        if effective_header.get(..16) != Some(b"SQLite format 3\0") {
            return Err("invalid SQLite header page in WAL".into());
        }
        let effective_size = sqlite_page_size(be_u16(effective_header, 16)?)?;
        if effective_size != page_size {
            return Err("WAL page 1 changes SQLite page size".into());
        }
        let reserved = validate_header_fields(effective_header, page_size)?;
        let header_pages = be_u32(effective_header, 28)?;
        let header_valid = be_u32(effective_header, 24)? != 0 && be_u32(effective_header, 24)? == be_u32(effective_header, 92)?;
        if header_valid && header_pages > main_pages && wal_page_count.is_none() {
            return Err("SQLite header page count exceeds image".into());
        }
        let mut page_count = if header_valid && header_pages != 0 { header_pages } else { main_pages };
        if let Some(count) = wal_page_count {
            page_count = count;
        }
        if page_count == 0 || page_count > MAX_PAGE_ID {
            return Err("invalid visible SQLite page count".into());
        }
        let encoding = match be_u32(effective_header, 56)? {
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            _ => return Err("unsupported SQLite text encoding".into()),
        };

        let mut db = Self { main, file, overlay, page_size, reserved, encoding, page_count, tables: Vec::new(), rowid_aliases: HashMap::new() };
        let schema = db.parse_schema()?;
        db.tables = schema.tables;
        db.rowid_aliases = schema.aliases;
        Ok(db)
    }

    /// Return tables from `sqlite_schema` in schema order.
    pub fn live_tables(&self) -> Result<Vec<LiveTable>, String> {
        Ok(self.tables.clone())
    }

    /// Read a table B-tree, decoding up to `ncols` values per record.
    pub fn read_table(&self, rootpage: u32, ncols: usize) -> Result<Vec<Record>, String> {
        if !(1..=MAX_PAGE_ID).contains(&rootpage) || rootpage > self.page_count {
            return Err("table root page is outside database".into());
        }
        if ncols > MAX_COLUMNS {
            return Err("table has too many columns".into());
        }
        let alias = self.rowid_aliases.get(&rootpage).copied();
        let mut state = WalkState { seen: HashSet::new(), out: Vec::new(), payload_total: 0 };
        self.visit_table(rootpage, ncols, alias, 0, &mut state)?;
        Ok(state.out)
    }

    fn page(&self, page_id: u32) -> Result<Cow<'_, [u8]>, String> {
        if page_id == 0 || page_id > self.page_count {
            return Err(format!("page {page_id} is outside database"));
        }
        if let Some(page) = self.overlay.get(&page_id) {
            return Ok(Cow::Borrowed(page));
        }
        if let Some(file) = &self.file {
            return file.page(page_id, self.page_size).map(Cow::Owned);
        }
        let id = usize::try_from(page_id).map_err(|_| "page number does not fit host")?;
        let start = id.checked_sub(1).and_then(|n| n.checked_mul(self.page_size)).ok_or("page offset overflow")?;
        let end = start.checked_add(self.page_size).ok_or("page offset overflow")?;
        self.main.get(start..end).map(Cow::Borrowed).ok_or_else(|| format!("page {page_id} is truncated"))
    }

    fn visit_table(&self, page_id: u32, ncols: usize, alias: Option<usize>, depth: usize, state: &mut WalkState) -> Result<(), String> {
        if depth > MAX_BTREE_DEPTH {
            return Err("SQLite B-tree is too deep".into());
        }
        if !state.seen.insert(page_id) {
            return Err("SQLite B-tree contains a cycle or shared page".into());
        }
        let page = self.page(page_id)?;
        let usable = self.page_size.checked_sub(self.reserved).ok_or("invalid usable page size")?;
        let content = page.get(..usable).ok_or("truncated usable SQLite page")?;
        let hdr = craft_sqlite::parse_btree_page_header(content, page_id == 1).map_err(|err| match err {
            craft_sqlite::Error::UnsupportedBtreePageType(other) => format!("unsupported SQLite B-tree page type 0x{other:02x}"),
            _ => "truncated B-tree header".into(),
        })?;
        if matches!(hdr.kind, 0x02 | 0x0a) {
            return Err("SQLite index B-trees are unsupported".into());
        }
        let cells = hdr.cells;
        if cells > MAX_CELLS || state.out.len().saturating_add(cells) > MAX_ROWS.saturating_add(MAX_CELLS) {
            return Err("SQLite B-tree contains too many cells".into());
        }
        match hdr.kind {
            0x0d => {
                for i in 0..cells {
                    let cell = craft_sqlite::btree_cell_offset(content, &hdr, i).map_err(|_| "invalid B-tree cell pointer")?;
                    let mut at = cell;
                    let payload_len = usize_from_u64(read_varint(content, &mut at)?, "payload length")?;
                    if payload_len > MAX_PAYLOAD {
                        return Err("SQLite record payload is too large".into());
                    }
                    let rowid = i64::from_be_bytes(read_varint(content, &mut at)?.to_be_bytes());
                    state.payload_total = state.payload_total.checked_add(payload_len).ok_or("SQLite table payload total overflow")?;
                    if state.payload_total > MAX_TABLE_PAYLOAD {
                        return Err("SQLite table payload exceeds bounded import size".into());
                    }
                    let payload = self.payload(&page, at, payload_len)?;
                    let mut values = decode_record(&payload, ncols, self.encoding)?;
                    if let Some(index) = alias {
                        if index >= values.len() {
                            return Err("INTEGER PRIMARY KEY column is missing from record".into());
                        }
                        if let Some(value @ Value::Null) = values.get_mut(index) {
                            *value = Value::Integer(rowid);
                        }
                    }
                    if state.out.len() >= MAX_ROWS {
                        return Err("SQLite table contains more than one million records".into());
                    }
                    state.out.push(Record { values });
                }
                Ok(())
            }
            0x05 => {
                for i in 0..cells {
                    let cell = craft_sqlite::btree_cell_offset(content, &hdr, i).map_err(|_| "invalid interior B-tree cell pointer")?;
                    let child = be_u32_at(content, cell)?;
                    if child == 0 {
                        return Err("interior B-tree cell has null child".into());
                    }
                    let mut key_at = cell.checked_add(4).ok_or("interior cell overflow")?;
                    let _ = read_varint(content, &mut key_at)?;
                    self.visit_table(child, ncols, alias, depth + 1, state)?;
                }
                let right = be_u32_at(content, hdr.header_offset + 8)?;
                if right == 0 {
                    return Err("interior B-tree has null right child".into());
                }
                self.visit_table(right, ncols, alias, depth + 1, state)
            }
            _ => Err(format!("unsupported SQLite B-tree page type 0x{:02x}", hdr.kind)),
        }
    }

    fn payload(&self, page: &[u8], cell_at: usize, payload_len: usize) -> Result<Vec<u8>, String> {
        let usable = self.page_size.checked_sub(self.reserved).ok_or("invalid usable page size")?;
        if usable < 8 {
            return Err("SQLite usable page size is too small".into());
        }
        let usable_u32 = u32::try_from(usable).map_err(|_| "SQLite usable page size is too small")?;
        let local = craft_sqlite::local_payload_len(usable_u32, payload_len as u64, craft_sqlite::PayloadKind::TableLeaf)
            .map(|len| len as usize)
            .ok_or("SQLite usable page size is too small")?;
        let local_end = cell_at.checked_add(local).ok_or("SQLite cell payload overflow")?;
        if local_end > usable {
            return Err("truncated SQLite cell payload".into());
        }
        let overflow_bytes = self
            .page_count
            .saturating_sub(1)
            .try_into()
            .unwrap_or(usize::MAX)
            .min(MAX_OVERFLOW_PAGES)
            .checked_mul(usable.checked_sub(4).ok_or("SQLite usable page size is too small")?)
            .ok_or("SQLite overflow capacity overflow")?;
        let capacity = local.checked_add(overflow_bytes).ok_or("SQLite payload capacity overflow")?.min(payload_len);
        let mut out = Vec::with_capacity(capacity);
        out.extend_from_slice(page.get(cell_at..local_end).ok_or("truncated SQLite cell payload")?);
        let mut remaining = payload_len.checked_sub(local).ok_or("SQLite payload length is too small")?;
        if remaining == 0 {
            return Ok(out);
        }
        let overflow_at = local_end;
        match overflow_at.checked_add(4) {
            Some(end) if end <= usable => {}
            _ => return Err("SQLite overflow pointer is in reserved page bytes".into()),
        }
        let first = be_u32_at(page, overflow_at)?;
        let mut next = first;
        let mut seen = HashSet::new();
        while remaining > 0 {
            if next == 0 || !seen.insert(next) || seen.len() > MAX_OVERFLOW_PAGES {
                return Err("invalid or cyclic SQLite overflow chain".into());
            }
            let overflow = self.page(next)?;
            let take = remaining.min(usable.checked_sub(4).ok_or("SQLite usable page size is too small")?);
            let end = 4usize.checked_add(take).ok_or("overflow payload offset")?;
            out.extend_from_slice(overflow.get(4..end).ok_or("truncated SQLite overflow page")?);
            remaining -= take;
            if remaining > 0 {
                next = be_u32_at(&overflow, 0)?;
            }
        }
        Ok(out)
    }

    fn parse_schema(&self) -> Result<SchemaInfo, String> {
        let mut tables = Vec::new();
        let mut aliases = HashMap::new();
        let mut state = WalkState { seen: HashSet::new(), out: Vec::new(), payload_total: 0 };
        self.visit_table(1, 5, None, 0, &mut state)?;
        for record in state.out {
            if record.values.len() < 5 {
                return Err("sqlite_schema row has too few columns".into());
            }
            let kind = match record.values.first() {
                Some(Value::Text(s)) => s.as_str(),
                _ => return Err("sqlite_schema row has invalid type".into()),
            };
            let name = match record.values.get(1) {
                Some(Value::Text(s)) => s.clone(),
                _ => return Err("sqlite_schema row has invalid name".into()),
            };
            if kind.eq_ignore_ascii_case("virtual") {
                continue;
            }
            if !kind.eq_ignore_ascii_case("table") {
                continue;
            }
            let rootpage = match record.values.get(3) {
                Some(Value::Integer(n)) if *n > 0 => u32::try_from(*n).map_err(|_| "sqlite_schema root page is invalid")?,
                _ => continue,
            };
            let sql = match record.values.get(4) {
                Some(Value::Text(s)) => s,
                _ => continue,
            };
            let Ok((column_names, alias)) = parse_create_table(sql) else { continue };
            if column_names.len() > MAX_COLUMNS {
                continue;
            }
            if tables.iter().any(|table: &LiveTable| table.rootpage == rootpage) {
                continue;
            }
            if let Some(index) = alias {
                aliases.insert(rootpage, index);
            }
            tables.push(LiveTable { name, rootpage, column_names: Some(column_names) });
        }
        Ok(SchemaInfo { tables, aliases })
    }
}

fn validate_header_fields(header: &[u8], page_size: usize) -> Result<usize, String> {
    let write_version = *header.get(18).ok_or("truncated SQLite header")?;
    let read_version = *header.get(19).ok_or("truncated SQLite header")?;
    if !matches!(write_version, 1 | 2) || !matches!(read_version, 1 | 2) {
        return Err("unsupported SQLite file format version".into());
    }
    if *header.get(21).ok_or("truncated SQLite header")? != 64
        || *header.get(22).ok_or("truncated SQLite header")? != 32
        || *header.get(23).ok_or("truncated SQLite header")? != 32
    {
        return Err("invalid SQLite payload fractions".into());
    }
    let reserved = *header.get(20).ok_or("truncated SQLite header")?;
    let page_size_u32 = u32::try_from(page_size).map_err(|_| "invalid SQLite reserved-byte count")?;
    let usable = craft_sqlite::usable_page_size(page_size_u32, reserved).map_err(|err| match err {
        craft_sqlite::Error::UsablePageSizeTooSmall(_) => "SQLite usable page size is below 480 bytes",
        _ => "invalid SQLite reserved-byte count",
    })?;
    let _ = usable;
    let reserved = usize::from(reserved);
    // 0 = no schema yet (file format §1.3.10): a catalog whose tables are all still in its WAL
    // has that in the main file, and the WAL's own page 1 is checked again with the overlay
    let schema_format = be_u32(header, 44)?;
    if !(0..=4).contains(&schema_format) {
        return Err("unsupported SQLite schema format".into());
    }
    Ok(reserved)
}

fn parse_wal(wal: &[u8], page_size: usize) -> Result<WalOverlay, String> {
    let header = wal.get(..32).ok_or("truncated WAL header")?;
    let magic = be_u32(header, 0)?;
    if magic != 0x377f0682 && magic != 0x377f0683 {
        return Err("invalid WAL magic".into());
    }
    if be_u32(header, 4)? != 3_007_000 || be_u32(header, 8)? != u32::try_from(page_size).map_err(|_| "page size overflow")? {
        return Err("invalid WAL page size or format version".into());
    }
    let stride = page_size.checked_add(24).ok_or("WAL frame size overflow")?;
    let body = wal.get(32..).ok_or("truncated WAL body")?;
    if body.len() % stride != 0 {
        return Err("WAL ends with a partial frame".into());
    }
    let salt = (be_u32(header, 16)?, be_u32(header, 20)?);
    let frame_count = body.len() / stride;
    if frame_count > MAX_CELLS {
        return Err("WAL has too many frames".into());
    }
    let mut overlay = HashMap::new();
    let mut page_count = None;
    for frame_no in 0..frame_count {
        let start = frame_no.checked_mul(stride).ok_or("WAL frame offset overflow")?;
        let end = start.checked_add(stride).ok_or("WAL frame offset overflow")?;
        let frame = body.get(start..end).ok_or("truncated WAL frame")?;
        let page_id = be_u32(frame, 0)?;
        if page_id == 0 || page_id > MAX_PAGE_ID || salt != (be_u32(frame, 8)?, be_u32(frame, 12)?) {
            return Err("invalid WAL frame page or salt".into());
        }
        let commit = be_u32(frame, 4)?;
        if commit != 0 {
            if commit > MAX_PAGE_ID {
                return Err("WAL commit database size is too large".into());
            }
            page_count = Some(commit);
        }
        overlay.insert(page_id, frame.get(24..).ok_or("truncated WAL page")?.to_vec());
    }
    Ok(WalOverlay { page_size, frame_count, pages: overlay, page_count })
}

fn decode_record(payload: &[u8], ncols: usize, encoding: TextEncoding) -> Result<Vec<Value>, String> {
    let (header, data) = craft_sqlite::split_record(payload).map_err(|err| match err {
        craft_sqlite::Error::InvalidRecordHeaderLength(_) => "invalid SQLite record header length".into(),
        other => other.to_string(),
    })?;
    let mut serials = Vec::new();
    let mut at = 0;
    while at < header.len() {
        if serials.len() >= ncols {
            return Err("SQLite record has more columns than table schema".into());
        }
        serials.push(read_varint(header, &mut at).map_err(|_| "SQLite record header is malformed")?);
    }
    let mut body = 0;
    let mut values = Vec::with_capacity(ncols);
    for serial in serials {
        let (value, _) = serial_value(data, &mut body, serial, encoding)?;
        values.push(value);
        if values.len() > MAX_COLUMNS {
            return Err("SQLite record has too many columns".into());
        }
    }
    values.resize(ncols, Value::Null);
    Ok(values)
}

fn serial_value(payload: &[u8], body: &mut usize, serial: u64, encoding: TextEncoding) -> Result<(Value, usize), String> {
    let len_u64 = craft_sqlite::serial_type_len(serial).map_err(|_| "reserved SQLite serial type")?;
    let len = usize_from_u64(len_u64, "SQLite field length")?;
    let end = body.checked_add(len).ok_or("SQLite record body offset overflow")?;
    let bytes = payload.get(*body..end).ok_or("SQLite record body is truncated")?;
    let value = match serial {
        0 => Value::Null,
        1..=6 => Value::Integer(craft_sqlite::decode_signed_be_int(bytes).map_err(|_| "truncated integer")?),
        7 => {
            let n = f64::from_be_bytes(bytes.try_into().map_err(|_| "truncated real")?);
            if !n.is_finite() {
                return Err("non-finite SQLite real value".into());
            }
            Value::Real(n)
        }
        8 => Value::Integer(0),
        9 => Value::Integer(1),
        10 | 11 => return Err("reserved SQLite serial type".into()),
        n if n % 2 == 0 => Value::Blob(bytes.to_vec()),
        _ => Value::Text(decode_text(bytes, encoding)?),
    };
    *body = end;
    Ok((value, len))
}

fn decode_text(bytes: &[u8], encoding: TextEncoding) -> Result<String, String> {
    match encoding {
        TextEncoding::Utf8 => String::from_utf8(bytes.to_vec()).map_err(|_| "invalid UTF-8 SQLite text".into()),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return Err("odd-length UTF-16 SQLite text".into());
            }
            let units: Result<Vec<u16>, String> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let first = *pair.first().ok_or("truncated UTF-16 SQLite text")?;
                    let second = *pair.get(1).ok_or("truncated UTF-16 SQLite text")?;
                    Ok(match encoding {
                        TextEncoding::Utf16Le => u16::from_le_bytes([first, second]),
                        TextEncoding::Utf16Be => u16::from_be_bytes([first, second]),
                        TextEncoding::Utf8 => return Err("invalid UTF-16 encoding state".into()),
                    })
                })
                .collect();
            String::from_utf16(&units?).map_err(|_| "invalid UTF-16 SQLite text".into())
        }
    }
}

fn parse_create_table(sql: &str) -> Result<(Vec<String>, Option<usize>), String> {
    let tokens = tokenize_sql(sql)?;
    if !tokens.first().is_some_and(|t| t.word_is("CREATE")) {
        return Err("malformed CREATE TABLE SQL".into());
    }
    let mut kind_at = 1usize;
    if tokens.get(kind_at).is_some_and(|t| t.word_is("TEMPORARY") || t.word_is("TEMP")) {
        kind_at += 1;
    }
    if tokens.get(kind_at).is_some_and(|t| t.word_is("VIRTUAL")) {
        return Err("virtual tables are unsupported".into());
    }
    if !tokens.get(kind_at).is_some_and(|t| t.word_is("TABLE")) {
        return Err("malformed CREATE TABLE SQL".into());
    }
    let open = tokens.iter().position(|t| t.is_symbol(b'(')).ok_or("CREATE TABLE has no column list")?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if token.is_symbol(b'(') {
            depth += 1;
        } else if token.is_symbol(b')') {
            depth = depth.checked_sub(1).ok_or("unbalanced CREATE TABLE parentheses")?;
            if depth == 0 {
                close = Some(i);
                break;
            }
        }
    }
    let close = close.ok_or("unterminated CREATE TABLE column list")?;
    if tokens
        .get(close + 1..)
        .ok_or("invalid CREATE TABLE suffix")?
        .windows(2)
        .any(|w| w.first().is_some_and(|t| t.word_is("WITHOUT")) && w.get(1).is_some_and(|t| t.word_is("ROWID")))
    {
        return Err("WITHOUT ROWID tables are unsupported".into());
    }
    let body = tokens.get(open + 1..close).ok_or("invalid CREATE TABLE column list")?;
    let mut columns = Vec::new();
    let mut alias = None;
    let mut integer_columns = HashSet::new();
    let mut table_primary = None;
    let mut start = 0usize;
    let mut nested = 0usize;
    for i in 0..=body.len() {
        let comma = i == body.len() || (nested == 0 && body.get(i).is_some_and(|t| t.is_symbol(b',')));
        if comma {
            let part = body.get(start..i).ok_or("invalid CREATE TABLE column range")?;
            if part.is_empty() {
                return Err("empty CREATE TABLE column definition".into());
            }
            if is_table_constraint(part) {
                if table_primary.is_none() {
                    table_primary = table_primary_column(part);
                }
            } else {
                let name = part.first().and_then(Token::identifier).ok_or("column name is not an identifier")?;
                if columns.iter().any(|old| old == name) {
                    return Err("duplicate SQLite column name".into());
                }
                let index = columns.len();
                if exact_integer_decl(part) {
                    integer_columns.insert(name.to_ascii_lowercase());
                }
                if integer_primary_key_alias(part) {
                    alias = Some(index);
                }
                columns.push(name.to_string());
            }
            start = i + 1;
        } else if body.get(i).is_some_and(|token| token.is_symbol(b'(')) {
            nested += 1;
        } else if body.get(i).is_some_and(|token| token.is_symbol(b')')) {
            nested = nested.checked_sub(1).ok_or("unbalanced CREATE TABLE column definition")?;
        }
    }
    if columns.is_empty() {
        return Err("CREATE TABLE has no columns".into());
    }
    if alias.is_none()
        && let Some(primary) = table_primary
    {
        let key = primary.to_ascii_lowercase();
        if integer_columns.contains(&key) {
            alias = columns.iter().position(|column| column.eq_ignore_ascii_case(&primary));
        }
    }
    Ok((columns, alias))
}

fn is_table_constraint(part: &[Token]) -> bool {
    part.first().is_some_and(|token| ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"].iter().any(|word| token.word_is(word)))
}

fn exact_integer_decl(part: &[Token]) -> bool {
    if !part.get(1).is_some_and(|token| token.word_is("INTEGER")) {
        return false;
    }
    match part.get(2) {
        None => true,
        Some(token) if token.is_symbol(b'(') => false,
        Some(token) => ["PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "CONSTRAINT", "GENERATED"]
            .iter()
            .any(|word| token.word_is(word)),
    }
}

fn table_primary_column(part: &[Token]) -> Option<String> {
    let primary = part.iter().position(|token| token.word_is("PRIMARY"))?;
    if !part.get(primary + 1).is_some_and(|token| token.word_is("KEY")) {
        return None;
    }
    let open = primary + 2;
    if !part.get(open).is_some_and(|token| token.is_symbol(b'(')) {
        return None;
    }
    let close = (open + 1..part.len()).find(|index| part.get(*index).is_some_and(|token| token.is_symbol(b')')))?;
    let terms = part.get(open + 1..close)?;
    if terms.len() == 1 || (terms.len() == 2 && terms.get(1).is_some_and(|token| token.word_is("ASC") || token.word_is("DESC"))) {
        return terms.first().and_then(Token::identifier).map(str::to_owned);
    }
    None
}

fn integer_primary_key_alias(part: &[Token]) -> bool {
    if !exact_integer_decl(part) {
        return false;
    }
    let mut primary = None;
    let mut depth = 0usize;
    for i in 2..part.len() {
        if part.get(i).is_some_and(|token| token.is_symbol(b'(')) {
            depth += 1;
        } else if part.get(i).is_some_and(|token| token.is_symbol(b')')) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && part.get(i).is_some_and(|token| token.word_is("PRIMARY")) && part.get(i + 1).is_some_and(|t| t.word_is("KEY")) {
            primary = Some(i);
            break;
        }
    }
    let Some(i) = primary else { return false };
    !part.get(i + 2).is_some_and(|token| token.word_is("DESC")) && !part.get(i + 2).is_some_and(|token| token.is_symbol(b'('))
}

#[derive(Clone, Debug)]
enum Token {
    Word(String),
    Quoted(String),
    StringLiteral,
    Symbol(u8),
}

impl Token {
    fn is_symbol(&self, symbol: u8) -> bool {
        matches!(self, Token::Symbol(value) if *value == symbol)
    }

    fn word_is(&self, wanted: &str) -> bool {
        matches!(self, Token::Word(word) if word.eq_ignore_ascii_case(wanted))
    }

    fn identifier(&self) -> Option<&str> {
        match self {
            Token::Word(word) | Token::Quoted(word) => Some(word),
            _ => None,
        }
    }
}

fn tokenize_sql(sql: &str) -> Result<Vec<Token>, String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        } else if bytes.get(at) == Some(&b'-') && bytes.get(at + 1) == Some(&b'-') {
            at += 2;
            while at < bytes.len() && bytes.get(at) != Some(&b'\n') {
                at += 1;
            }
        } else if bytes.get(at) == Some(&b'/') && bytes.get(at + 1) == Some(&b'*') {
            at += 2;
            let mut closed = false;
            while at + 1 < bytes.len() {
                if bytes.get(at) == Some(&b'*') && bytes.get(at + 1) == Some(&b'/') {
                    at += 2;
                    closed = true;
                    break;
                }
                at += 1;
            }
            if !closed {
                return Err("unterminated SQL comment".into());
            }
        } else if matches!(bytes.get(at), Some(b'(' | b')' | b',' | b'.' | b';')) {
            out.push(Token::Symbol(*bytes.get(at).ok_or("truncated SQL punctuation")?));
            at += 1;
        } else if matches!(bytes.get(at), Some(b'"' | b'`' | b'[')) {
            let quote = *bytes.get(at).ok_or("truncated SQL quote")?;
            let end_quote = if quote == b'[' { b']' } else { quote };
            at += 1;
            let mut value = Vec::new();
            let mut closed = false;
            while at < bytes.len() {
                if bytes.get(at) == Some(&end_quote) {
                    if bytes.get(at + 1) == Some(&end_quote) {
                        value.push(end_quote);
                        at += 2;
                    } else {
                        at += 1;
                        closed = true;
                        break;
                    }
                } else {
                    value.push(*bytes.get(at).ok_or("truncated SQL identifier")?);
                    at += 1;
                }
            }
            if !closed {
                return Err("unterminated quoted SQL identifier".into());
            }
            out.push(Token::Quoted(String::from_utf8(value).map_err(|_| "invalid SQL identifier encoding")?));
        } else if bytes.get(at) == Some(&b'\'') {
            at += 1;
            let mut closed = false;
            while at < bytes.len() {
                if bytes.get(at) == Some(&b'\'') {
                    if bytes.get(at + 1) == Some(&b'\'') {
                        at += 2;
                    } else {
                        at += 1;
                        closed = true;
                        break;
                    }
                } else {
                    at += 1;
                }
            }
            if !closed {
                return Err("unterminated SQL string".into());
            }
            out.push(Token::StringLiteral);
        } else {
            let start = at;
            while at < bytes.len()
                && !bytes.get(at).is_some_and(u8::is_ascii_whitespace)
                && !matches!(bytes.get(at), Some(b'(' | b')' | b',' | b'.' | b';' | b'\'' | b'"' | b'`' | b'['))
            {
                at += 1;
            }
            if start == at {
                return Err("unsupported SQL punctuation".into());
            }
            out.push(Token::Word(sql.get(start..at).ok_or("invalid SQL token boundary")?.to_string()));
        }
    }
    Ok(out)
}

fn sqlite_page_size(value: u16) -> Result<usize, String> {
    let size = craft_sqlite::page_size_from_raw(value).map_err(|_| "invalid SQLite page size")?;
    usize::try_from(size).map_err(|_| "invalid SQLite page size".into())
}

fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64, String> {
    let rest = bytes.get(*at..).ok_or("truncated SQLite varint")?;
    let (value, len) = craft_sqlite::read_varint(rest).map_err(|_| "truncated SQLite varint")?;
    *at = (*at).checked_add(len).ok_or("SQLite varint offset overflow")?;
    Ok(value)
}

fn usize_from_u64(value: u64, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("{label} exceeds host size"))
}

fn be_u16(bytes: &[u8], at: usize) -> Result<u16, String> {
    let end = at.checked_add(2).ok_or("SQLite integer offset overflow")?;
    bytes.get(at..end).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_be_bytes).ok_or("truncated SQLite integer".into())
}

fn be_u32(bytes: &[u8], at: usize) -> Result<u32, String> {
    let end = at.checked_add(4).ok_or("SQLite integer offset overflow")?;
    bytes.get(at..end).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated SQLite integer".into())
}

fn be_u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    be_u32(bytes, at)
}

#[cfg(test)]
mod tests;
