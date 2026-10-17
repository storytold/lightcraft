//! Small, read-only SQLite reader used by Lightroom catalog import.
//!
//! This is a clean-room implementation based on SQLite's public file-format
//! description: <https://www.sqlite.org/fileformat.html> (sections 1.6, 1.7,
//! 2.1, 2.2, 2.3, 2.4 and 2.5). It is deliberately a page reader, rather
//! than a SQL engine. It never writes its input and has no SQLite runtime
//! dependency.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_PAGE_ID: u32 = 16_777_216;
const MAX_COLUMNS: usize = 16_384;
const MAX_ROWS: usize = 1_000_000;
const MAX_CELLS: usize = 4_000_000;
pub(crate) const MAX_WAL_BYTES: u64 = 1 << 30;
const MAX_VISITED_BTREE_PAGES: usize = MAX_CELLS;
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
    main: MainPages,
    wal: Option<WalPages>,
    page_size: usize,
    reserved: usize,
    encoding: TextEncoding,
    page_count: u32,
    tables: Vec<LiveTable>,
    rowid_aliases: HashMap<u32, usize>,
}

enum MainPages {
    File(FilePages),
    #[cfg(test)]
    Bytes(Vec<u8>),
}

struct FilePages {
    file: Mutex<File>,
    len: u64,
    source_failure: Arc<AtomicBool>,
}

impl FilePages {
    fn read_page(&self, page_id: u32, page_size: usize) -> Result<Vec<u8>, String> {
        let page_number = u64::from(page_id.checked_sub(1).ok_or("page number is zero")?);
        let page_size_u64 = u64::try_from(page_size).map_err(|_| "SQLite page size does not fit file offsets")?;
        let start = page_number.checked_mul(page_size_u64).ok_or("page offset overflow")?;
        let end = start.checked_add(page_size_u64).ok_or("page offset overflow")?;
        if end > self.len {
            return Err(format!("page {page_id} is truncated"));
        }
        let mut page = vec![0u8; page_size];
        let mut file = self.file.lock().map_err(|_| "SQLite file reader lock is poisoned")?;
        if let Err(error) = file.seek(SeekFrom::Start(start)) {
            self.source_failure.store(true, Ordering::Relaxed);
            return Err(format!("seek SQLite page {page_id}: {error}"));
        }
        if let Err(error) = file.read_exact(&mut page) {
            self.source_failure.store(true, Ordering::Relaxed);
            return Err(format!("read SQLite page {page_id}: {error}"));
        }
        Ok(page)
    }
}

enum WalPages {
    File(WalFileIndex),
    #[cfg(test)]
    Bytes(HashMap<u32, Vec<u8>>),
}

struct WalFileIndex {
    file: Mutex<File>,
    len: u64,
    page_size: usize,
    frame_stride: u64,
    committed_frames: u32,
    page_count: Option<u32>,
    entries: Vec<(u32, u32)>,
    salts: (u32, u32),
    source_failure: Arc<AtomicBool>,
}

impl WalPages {
    fn page_with_cancel(&self, page_id: u32, cancel: &std::sync::atomic::AtomicBool) -> Result<Option<Vec<u8>>, String> {
        check_cancelled(cancel)?;
        match self {
            Self::File(index) => index.page_with_cancel(page_id, cancel),
            #[cfg(test)]
            Self::Bytes(pages) => Ok(pages.get(&page_id).cloned()),
        }
    }
}

impl WalFileIndex {
    fn scan(
        mut file: File,
        len: u64,
        page_size: usize,
        cancel: &std::sync::atomic::AtomicBool,
        source_failure: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        check_cancelled(cancel)?;
        if len > MAX_WAL_BYTES {
            return Err("WAL exceeds 1 GiB scan limit".into());
        }
        let frame_stride =
            u64::try_from(page_size).map_err(|_| "WAL page size does not fit file offsets")?.checked_add(24).ok_or("WAL frame size overflow")?;
        if len == 0 {
            return Ok(Self {
                file: Mutex::new(file),
                len,
                page_size,
                frame_stride,
                committed_frames: 0,
                page_count: None,
                entries: Vec::new(),
                salts: (0, 0),
                source_failure,
            });
        }
        if len < 32 {
            return Err("truncated WAL header".into());
        }
        let mut header = [0u8; 32];
        read_exact_at(&mut file, 0, &mut header, "WAL header")?;
        check_cancelled(cancel)?;
        let magic = be_u32(&header, 0)?;
        if !matches!(magic, 0x377f0682 | 0x377f0683) || be_u32(&header, 4)? != 3_007_000 {
            return Err("invalid WAL header".into());
        }
        if be_u32(&header, 8)? != u32::try_from(page_size).map_err(|_| "SQLite page size overflow")? {
            return Err("WAL page size mismatch".into());
        }
        let big = magic == 0x377f0683;
        let mut checksum = wal_checksum(&header[..24], big, (0, 0));
        if checksum != (be_u32(&header, 24)?, be_u32(&header, 28)?) {
            return Err("invalid WAL header checksum".into());
        }
        let salts = (be_u32(&header, 16)?, be_u32(&header, 20)?);
        let complete_frames = usize::try_from((len - 32) / frame_stride).map_err(|_| "WAL frame count does not fit host")?;
        if complete_frames > MAX_CELLS {
            return Err("WAL contains too many frames".into());
        }
        let mut page = vec![0u8; page_size];
        let mut frame_header = [0u8; 24];
        let mut committed_frames = 0u32;
        let mut page_count = None;
        for frame_index in 0..complete_frames {
            check_cancelled(cancel)?;
            let offset = wal_frame_offset(frame_index, frame_stride)?;
            read_exact_at(&mut file, offset, &mut frame_header, "WAL frame header")?;
            if be_u32(&frame_header, 0)? == 0 || salts != (be_u32(&frame_header, 8)?, be_u32(&frame_header, 12)?) {
                break;
            }
            let page_offset = offset.checked_add(24).ok_or("WAL page offset overflow")?;
            read_exact_at(&mut file, page_offset, &mut page, "WAL frame page")?;
            #[cfg(test)]
            notify_wal_frame_scanned_for_test();
            check_cancelled(cancel)?;
            checksum = wal_checksum(&frame_header[..8], big, checksum);
            checksum = wal_checksum(&page, big, checksum);
            if checksum != (be_u32(&frame_header, 16)?, be_u32(&frame_header, 20)?) {
                break;
            }
            let commit_size = be_u32(&frame_header, 4)?;
            if commit_size != 0 {
                committed_frames = u32::try_from(frame_index + 1).map_err(|_| "WAL frame index overflow")?;
                page_count = Some(commit_size);
            }
        }

        let mut entries = Vec::new();
        if committed_frames != 0 {
            let count = usize::try_from(committed_frames).map_err(|_| "WAL frame count does not fit host")?;
            entries.try_reserve_exact(count).map_err(|_| "unable to reserve bounded WAL page index")?;
            for frame_index in 0..committed_frames {
                check_cancelled(cancel)?;
                let offset = wal_frame_offset(frame_index as usize, frame_stride)?;
                read_exact_at(&mut file, offset, &mut frame_header, "committed WAL frame header")?;
                let page_id = be_u32(&frame_header, 0)?;
                let commit_size = be_u32(&frame_header, 4)?;
                if page_id == 0 || page_id > MAX_PAGE_ID {
                    return Err("WAL frame page number is outside database".into());
                }
                if commit_size > MAX_PAGE_ID {
                    return Err("WAL commit database size is too large".into());
                }
                entries.push((page_id, frame_index));
            }
            entries.sort_unstable();
            let mut unique = 0usize;
            for read in 0..entries.len() {
                let entry = entries[read];
                if unique > 0 && entries[unique - 1].0 == entry.0 {
                    entries[unique - 1] = entry;
                } else {
                    entries[unique] = entry;
                    unique += 1;
                }
            }
            entries.truncate(unique);
        }
        Ok(Self { file: Mutex::new(file), len, page_size, frame_stride, committed_frames, page_count, entries, salts, source_failure })
    }

    #[cfg(test)]
    fn page(&self, page_id: u32) -> Result<Option<Vec<u8>>, String> {
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        self.page_with_cancel(page_id, &never_cancel)
    }

    fn page_with_cancel(&self, page_id: u32, cancel: &std::sync::atomic::AtomicBool) -> Result<Option<Vec<u8>>, String> {
        check_cancelled(cancel)?;
        let Ok(index) = self.entries.binary_search_by_key(&page_id, |(candidate, _)| *candidate) else { return Ok(None) };
        let (indexed_page, frame_index) = self.entries.get(index).copied().ok_or("WAL page index changed")?;
        if indexed_page != page_id || frame_index >= self.committed_frames {
            return Err("WAL page index is invalid".into());
        }
        let frame_offset = wal_frame_offset(frame_index as usize, self.frame_stride)?;
        let page_offset = frame_offset.checked_add(24).ok_or("WAL page offset overflow")?;
        let page_size = u64::try_from(self.page_size).map_err(|_| "WAL page size does not fit file offsets")?;
        if page_offset.checked_add(page_size).ok_or("WAL page offset overflow")? > self.len {
            return Err("indexed WAL page is truncated".into());
        }
        let mut file = self.file.lock().map_err(|_| "WAL file reader lock is poisoned")?;
        let mut frame_header = [0u8; 24];
        if let Err(error) = read_exact_at(&mut file, frame_offset, &mut frame_header, "indexed WAL frame header") {
            self.source_failure.store(true, Ordering::Relaxed);
            return Err(error);
        }
        check_cancelled(cancel)?;
        if be_u32(&frame_header, 0)? != page_id || (be_u32(&frame_header, 8)?, be_u32(&frame_header, 12)?) != self.salts {
            self.source_failure.store(true, Ordering::Relaxed);
            return Err("indexed WAL frame changed during reading".into());
        }
        let mut page = vec![0u8; self.page_size];
        if let Err(error) = read_exact_at(&mut file, page_offset, &mut page, "indexed WAL page") {
            self.source_failure.store(true, Ordering::Relaxed);
            return Err(error);
        }
        check_cancelled(cancel)?;
        Ok(Some(page))
    }
}

/// Collect one row of a table that is kept whole, at most `MAX_ROWS` of them.
fn keep_row(out: &mut Vec<Record>, record: Record) -> Result<(), String> {
    if out.len() >= MAX_ROWS {
        return Err("SQLite table contains more than one million records".into());
    }
    out.try_reserve(1).map_err(|_| "unable to reserve bounded SQLite rows")?;
    out.push(record);
    Ok(())
}

fn wal_frame_offset(frame_index: usize, frame_stride: u64) -> Result<u64, String> {
    let frame_index = u64::try_from(frame_index).map_err(|_| "WAL frame index does not fit file offsets")?;
    32u64.checked_add(frame_index.checked_mul(frame_stride).ok_or("WAL frame offset overflow")?).ok_or("WAL frame offset overflow".into())
}

fn read_exact_at(file: &mut File, offset: u64, bytes: &mut [u8], what: &str) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek {what}: {error}"))?;
    file.read_exact(bytes).map_err(|error| format!("read {what}: {error}"))
}

fn check_cancelled(cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) { Err("Lightroom import cancelled".into()) } else { Ok(()) }
}

#[cfg(test)]
std::thread_local! {
    static PAGE_READ_HOOK: std::cell::Cell<Option<fn(u32)>> = const { std::cell::Cell::new(None) };
    static WAL_SCAN_HOOK: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_page_read_hook_for_test(hook: Option<fn(u32)>) {
    PAGE_READ_HOOK.with(|slot| slot.set(hook));
}

#[cfg(test)]
pub(crate) fn set_wal_scan_hook_for_test(hook: Option<fn()>) {
    WAL_SCAN_HOOK.with(|slot| slot.set(hook));
}

#[cfg(test)]
fn notify_page_read_for_test(page_id: u32) {
    PAGE_READ_HOOK.with(|slot| {
        if let Some(hook) = slot.get() {
            hook(page_id);
        }
    });
}

#[cfg(test)]
fn notify_wal_frame_scanned_for_test() {
    WAL_SCAN_HOOK.with(|slot| {
        if let Some(hook) = slot.get() {
            hook();
        }
    });
}

#[derive(Clone, Copy)]
enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Walk state: pages visited (cycle guard and visited-page budget), the payload decoded so far, and the consumer
/// each row is handed to as it is decoded, so a table's memory is what the consumer keeps, not the table's size.
/// `payload_budget` caps the total payload for walks whose consumer keeps every row; streaming walks pass `None`.
struct WalkState<'f> {
    seen: HashSet<u32>,
    payload_total: usize,
    payload_budget: Option<usize>,
    sink: &'f mut dyn FnMut(Record) -> Result<(), String>,
}

#[cfg(test)]
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
        let main_header = main.get(..100).ok_or("truncated SQLite header")?.to_vec();
        if main_header.get(..16) != Some(b"SQLite format 3\0") {
            return Err("invalid SQLite header signature".into());
        }
        let page_size = sqlite_page_size(be_u16(&main_header, 16)?)?;
        validate_header_fields(&main_header, page_size)?;
        if main.len() < page_size || !main.len().is_multiple_of(page_size) {
            return Err("SQLite image is not a whole number of pages".into());
        }
        let main_pages = u32::try_from(main.len() / page_size).map_err(|_| "SQLite image is too large")?;
        if main_pages == 0 || main_pages > MAX_PAGE_ID {
            return Err("invalid SQLite image page count".into());
        }

        let (wal, wal_page_count) = if committed_wal.is_empty() {
            (None, None)
        } else {
            let parsed = parse_wal(committed_wal, page_size)?;
            if parsed.page_size != page_size {
                return Err("WAL page size mismatch".into());
            }
            if parsed.frame_count > MAX_CELLS {
                return Err("WAL contains too many frames".into());
            }
            (Some(WalPages::Bytes(parsed.pages)), parsed.page_count)
        };
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        Self::open_parts(MainPages::Bytes(main), main_header, main_pages, wal, wal_page_count, &never_cancel)
    }

    /// Open a read-only main database file and an already checksum-validated WAL prefix.
    pub(crate) fn open_file_with_wal(
        file: File,
        file_len: u64,
        wal_file: Option<(File, u64)>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, String> {
        if file_len < 100 {
            return Err("truncated SQLite header".into());
        }
        let mut file = file;
        let mut main_header = vec![0u8; 100];
        file.seek(SeekFrom::Start(0)).map_err(|error| format!("seek SQLite header: {error}"))?;
        file.read_exact(&mut main_header).map_err(|error| format!("read SQLite header: {error}"))?;
        if main_header.get(..16) != Some(b"SQLite format 3\0") {
            return Err("invalid SQLite header signature".into());
        }
        let page_size = sqlite_page_size(be_u16(&main_header, 16)?)?;
        validate_header_fields(&main_header, page_size)?;
        let page_size_u64 = u64::try_from(page_size).map_err(|_| "SQLite page size does not fit file offsets")?;
        if file_len < page_size_u64 || !file_len.is_multiple_of(page_size_u64) {
            return Err("SQLite image is not a whole number of pages".into());
        }
        let main_pages = u32::try_from(file_len / page_size_u64).map_err(|_| "SQLite image is too large")?;
        if main_pages == 0 || main_pages > MAX_PAGE_ID {
            return Err("invalid SQLite image page count".into());
        }
        let source_failure = Arc::new(AtomicBool::new(false));
        let (wal, wal_page_count) = match wal_file {
            Some((file, len)) => {
                let index = WalFileIndex::scan(file, len, page_size, cancel, Arc::clone(&source_failure))?;
                let page_count = index.page_count;
                (Some(WalPages::File(index)), page_count)
            }
            None => (None, None),
        };
        let main = MainPages::File(FilePages { file: Mutex::new(file), len: file_len, source_failure });
        Self::open_parts(main, main_header, main_pages, wal, wal_page_count, cancel)
    }

    fn open_parts(
        main: MainPages,
        main_header: Vec<u8>,
        main_pages: u32,
        wal: Option<WalPages>,
        wal_page_count: Option<u32>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, String> {
        if main_header.get(..16) != Some(b"SQLite format 3\0") {
            return Err("invalid SQLite header signature".into());
        }
        let page_size = sqlite_page_size(be_u16(&main_header, 16)?)?;
        validate_header_fields(&main_header, page_size)?;

        check_cancelled(cancel)?;
        let page_one = wal.as_ref().map(|wal| wal.page_with_cancel(1, cancel)).transpose()?.flatten();
        let effective_header = page_one.as_deref().unwrap_or(main_header.as_slice());
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

        let mut db = Self { main, wal, page_size, reserved, encoding, page_count, tables: Vec::new(), rowid_aliases: HashMap::new() };
        let schema = db.parse_schema(cancel)?;
        db.tables = schema.tables;
        db.rowid_aliases = schema.aliases;
        Ok(db)
    }

    /// Return tables from `sqlite_schema` in schema order.
    #[cfg(test)]
    pub fn live_tables(&self) -> Result<Vec<LiveTable>, String> {
        Ok(self.tables.clone())
    }

    pub(crate) fn live_table(&self, name: &str, cancel: &std::sync::atomic::AtomicBool) -> Result<Option<&LiveTable>, String> {
        check_cancelled(cancel)?;
        for table in &self.tables {
            check_cancelled(cancel)?;
            if table.name == name {
                return Ok(Some(table));
            }
        }
        Ok(None)
    }

    pub(crate) fn has_observed_source_failure(&self) -> bool {
        let main_failed = match &self.main {
            MainPages::File(source) => source.source_failure.load(Ordering::Relaxed),
            #[cfg(test)]
            MainPages::Bytes(_) => false,
        };
        let wal_failed = match &self.wal {
            Some(WalPages::File(index)) => index.source_failure.load(Ordering::Relaxed),
            #[cfg(test)]
            Some(WalPages::Bytes(_)) | None => false,
            #[cfg(not(test))]
            None => false,
        };
        main_failed || wal_failed
    }

    /// Read a whole table B-tree into memory, decoding up to `ncols` values per record (at most a million rows and
    /// `MAX_TABLE_PAYLOAD` bytes). Large tables go through [`Database::for_each_row_with_cancel`] instead.
    #[cfg(test)]
    pub fn read_table(&self, rootpage: u32, ncols: usize) -> Result<Vec<Record>, String> {
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        self.read_table_with_cancel(rootpage, ncols, &never_cancel)
    }

    #[cfg(test)]
    pub(crate) fn read_table_with_cancel(&self, rootpage: u32, ncols: usize, cancel: &std::sync::atomic::AtomicBool) -> Result<Vec<Record>, String> {
        self.read_table_with_page_budget(rootpage, ncols, cancel, MAX_VISITED_BTREE_PAGES)
    }

    #[cfg(test)]
    fn read_table_with_page_budget(
        &self,
        rootpage: u32,
        ncols: usize,
        cancel: &std::sync::atomic::AtomicBool,
        visited_page_budget: usize,
    ) -> Result<Vec<Record>, String> {
        let mut out = Vec::new();
        self.walk_rows(rootpage, ncols, cancel, visited_page_budget, Some(MAX_TABLE_PAYLOAD), &mut |record| keep_row(&mut out, record))?;
        Ok(out)
    }

    /// Stream a table B-tree's rows to `sink` in rowid order, decoding up to `ncols` values per record. Only one
    /// record (bounded by `MAX_PAYLOAD`) is held at a time, so a table of any size costs what `sink` keeps; pages are
    /// read from the file one at a time. An error from `sink` (or cancellation) stops the walk and is returned.
    pub(crate) fn for_each_row_with_cancel(
        &self,
        rootpage: u32,
        ncols: usize,
        cancel: &std::sync::atomic::AtomicBool,
        sink: &mut dyn FnMut(Record) -> Result<(), String>,
    ) -> Result<(), String> {
        self.walk_rows(rootpage, ncols, cancel, MAX_VISITED_BTREE_PAGES, None, sink)
    }

    /// [`Database::for_each_row_with_cancel`] for consumers that keep every row: the walk stops past a million rows
    /// or `MAX_TABLE_PAYLOAD` bytes of payload.
    pub(crate) fn for_each_kept_row_with_cancel(
        &self,
        rootpage: u32,
        ncols: usize,
        cancel: &std::sync::atomic::AtomicBool,
        sink: &mut dyn FnMut(Record) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut rows = 0usize;
        self.walk_rows(rootpage, ncols, cancel, MAX_VISITED_BTREE_PAGES, Some(MAX_TABLE_PAYLOAD), &mut |record| {
            if rows >= MAX_ROWS {
                return Err("SQLite table contains more than one million records".into());
            }
            rows += 1;
            sink(record)
        })
    }

    fn walk_rows(
        &self,
        rootpage: u32,
        ncols: usize,
        cancel: &std::sync::atomic::AtomicBool,
        visited_page_budget: usize,
        payload_budget: Option<usize>,
        sink: &mut dyn FnMut(Record) -> Result<(), String>,
    ) -> Result<(), String> {
        check_cancelled(cancel)?;
        if !(1..=MAX_PAGE_ID).contains(&rootpage) || rootpage > self.page_count {
            return Err("table root page is outside database".into());
        }
        if ncols > MAX_COLUMNS {
            return Err("table has too many columns".into());
        }
        let alias = self.rowid_aliases.get(&rootpage).copied();
        let mut state = WalkState { seen: HashSet::new(), payload_total: 0, payload_budget, sink };
        self.visit_table(rootpage, ncols, alias, 0, cancel, visited_page_budget, &mut state)?;
        check_cancelled(cancel)
    }

    fn page(&self, page_id: u32, cancel: &std::sync::atomic::AtomicBool) -> Result<Vec<u8>, String> {
        check_cancelled(cancel)?;
        if page_id == 0 || page_id > self.page_count {
            return Err(format!("page {page_id} is outside database"));
        }
        let page = if let Some(wal) = &self.wal
            && let Some(page) = wal.page_with_cancel(page_id, cancel)?
        {
            page
        } else {
            match &self.main {
                MainPages::File(source) => source.read_page(page_id, self.page_size)?,
                #[cfg(test)]
                MainPages::Bytes(main) => {
                    let id = usize::try_from(page_id).map_err(|_| "page number does not fit host")?;
                    let start = id.checked_sub(1).and_then(|number| number.checked_mul(self.page_size)).ok_or("page offset overflow")?;
                    let end = start.checked_add(self.page_size).ok_or("page offset overflow")?;
                    main.get(start..end).map(<[u8]>::to_vec).ok_or_else(|| format!("page {page_id} is truncated"))?
                }
            }
        };
        #[cfg(test)]
        notify_page_read_for_test(page_id);
        check_cancelled(cancel)?;
        Ok(page)
    }

    fn visit_table(
        &self,
        page_id: u32,
        ncols: usize,
        alias: Option<usize>,
        depth: usize,
        cancel: &std::sync::atomic::AtomicBool,
        visited_page_budget: usize,
        state: &mut WalkState<'_>,
    ) -> Result<(), String> {
        check_cancelled(cancel)?;
        if depth > MAX_BTREE_DEPTH {
            return Err("SQLite B-tree is too deep".into());
        }
        if state.seen.contains(&page_id) {
            return Err("SQLite B-tree contains a cycle or shared page".into());
        }
        if state.seen.len() >= visited_page_budget {
            return Err("SQLite B-tree exceeds visited-page budget".into());
        }
        state.seen.try_reserve(1).map_err(|_| "unable to reserve bounded SQLite visited-page set")?;
        state.seen.insert(page_id);
        let page = self.page(page_id, cancel)?;
        let usable = self.page_size.checked_sub(self.reserved).ok_or("invalid usable page size")?;
        let base = if page_id == 1 { 100 } else { 0 };
        let content = page.get(..usable).ok_or("truncated usable SQLite page")?;
        let kind = *content.get(base).ok_or("truncated B-tree header")?;
        let cells = usize::from(be_u16_at(content, base + 3)?);
        if cells > MAX_CELLS {
            return Err("SQLite B-tree contains too many cells".into());
        }
        match kind {
            0x0d => {
                let ptr_start = base.checked_add(8).ok_or("B-tree header overflow")?;
                let ptr_bytes = cells.checked_mul(2).ok_or("B-tree pointer-array overflow")?;
                let ptr_end = ptr_start.checked_add(ptr_bytes).ok_or("B-tree pointer-array overflow")?;
                if ptr_end > usable {
                    return Err("truncated B-tree pointer array".into());
                }
                for i in 0..cells {
                    check_cancelled(cancel)?;
                    let p = ptr_start.checked_add(i.checked_mul(2).ok_or("cell offset overflow")?).ok_or("cell offset overflow")?;
                    let cell = usize::from(be_u16_at(content, p)?);
                    if cell < ptr_end || cell >= usable {
                        return Err("invalid B-tree cell pointer".into());
                    }
                    let mut at = cell;
                    let payload_len = usize_from_u64(read_varint(content, &mut at)?, "payload length")?;
                    if payload_len > MAX_PAYLOAD {
                        return Err("SQLite record payload is too large".into());
                    }
                    let rowid = i64::from_be_bytes(read_varint(content, &mut at)?.to_be_bytes());
                    if let Some(budget) = state.payload_budget {
                        state.payload_total = state.payload_total.checked_add(payload_len).ok_or("SQLite table payload total overflow")?;
                        if state.payload_total > budget {
                            return Err("SQLite table payload exceeds bounded import size".into());
                        }
                    }
                    let payload = self.payload(&page, at, payload_len, cancel)?;
                    let mut values = decode_record(&payload, ncols, self.encoding)?;
                    check_cancelled(cancel)?;
                    if let Some(index) = alias {
                        if index >= values.len() {
                            return Err("INTEGER PRIMARY KEY column is missing from record".into());
                        }
                        if let Some(value @ Value::Null) = values.get_mut(index) {
                            *value = Value::Integer(rowid);
                        }
                    }
                    (state.sink)(Record { values })?;
                }
                Ok(())
            }
            0x05 => {
                let ptr_start = base.checked_add(12).ok_or("B-tree header overflow")?;
                let ptr_bytes = cells.checked_mul(2).ok_or("B-tree pointer-array overflow")?;
                let ptr_end = ptr_start.checked_add(ptr_bytes).ok_or("B-tree pointer-array overflow")?;
                if ptr_end > usable {
                    return Err("truncated interior B-tree pointer array".into());
                }
                for i in 0..cells {
                    check_cancelled(cancel)?;
                    let p = ptr_start.checked_add(i.checked_mul(2).ok_or("cell offset overflow")?).ok_or("cell offset overflow")?;
                    let cell = usize::from(be_u16_at(content, p)?);
                    if cell < ptr_end || cell >= usable {
                        return Err("invalid interior B-tree cell pointer".into());
                    }
                    let child = be_u32_at(content, cell)?;
                    if child == 0 {
                        return Err("interior B-tree cell has null child".into());
                    }
                    let mut key_at = cell.checked_add(4).ok_or("interior cell overflow")?;
                    let _ = read_varint(content, &mut key_at)?;
                    self.visit_table(child, ncols, alias, depth + 1, cancel, visited_page_budget, state)?;
                }
                check_cancelled(cancel)?;
                let right = be_u32_at(content, base + 8)?;
                if right == 0 {
                    return Err("interior B-tree has null right child".into());
                }
                self.visit_table(right, ncols, alias, depth + 1, cancel, visited_page_budget, state)
            }
            0x02 | 0x0a => Err("SQLite index B-trees are unsupported".into()),
            other => Err(format!("unsupported SQLite B-tree page type 0x{other:02x}")),
        }
    }

    fn payload(&self, page: &[u8], cell_at: usize, payload_len: usize, cancel: &std::sync::atomic::AtomicBool) -> Result<Vec<u8>, String> {
        check_cancelled(cancel)?;
        let usable = self.page_size.checked_sub(self.reserved).ok_or("invalid usable page size")?;
        if usable < 8 {
            return Err("SQLite usable page size is too small".into());
        }
        let max_local = usable.checked_sub(35).ok_or("SQLite usable page size is too small")?;
        let min_local = usable
            .checked_sub(12)
            .ok_or("SQLite usable page size is too small")?
            .saturating_mul(32)
            .checked_div(255)
            .ok_or("SQLite local payload calculation failed")?
            .saturating_sub(23);
        let local = if payload_len <= max_local {
            payload_len
        } else {
            let span = usable.checked_sub(4).ok_or("SQLite usable page size is too small")?;
            let candidate = min_local.saturating_add(payload_len.checked_sub(min_local).ok_or("SQLite payload length is too small")? % span);
            if candidate > max_local { min_local } else { candidate }
        };
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
        let mut out = Vec::new();
        out.try_reserve_exact(capacity).map_err(|_| "unable to reserve bounded SQLite payload")?;
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
            check_cancelled(cancel)?;
            if next == 0 || seen.contains(&next) || seen.len() >= MAX_OVERFLOW_PAGES {
                return Err("invalid or cyclic SQLite overflow chain".into());
            }
            seen.try_reserve(1).map_err(|_| "unable to reserve bounded SQLite overflow-page set")?;
            seen.insert(next);
            let overflow = self.page(next, cancel)?;
            let take = remaining.min(usable.checked_sub(4).ok_or("SQLite usable page size is too small")?);
            let end = 4usize.checked_add(take).ok_or("overflow payload offset")?;
            out.extend_from_slice(overflow.get(4..end).ok_or("truncated SQLite overflow page")?);
            remaining -= take;
            check_cancelled(cancel)?;
            if remaining > 0 {
                next = be_u32_at(&overflow, 0)?;
            }
        }
        Ok(out)
    }

    fn parse_schema(&self, cancel: &std::sync::atomic::AtomicBool) -> Result<SchemaInfo, String> {
        check_cancelled(cancel)?;
        let mut tables = Vec::new();
        let mut aliases = HashMap::new();
        let mut rows = Vec::new();
        let mut state = WalkState {
            seen: HashSet::new(),
            payload_total: 0,
            payload_budget: Some(MAX_TABLE_PAYLOAD),
            sink: &mut |record| keep_row(&mut rows, record),
        };
        self.visit_table(1, 5, None, 0, cancel, MAX_VISITED_BTREE_PAGES, &mut state)?;
        for record in rows {
            check_cancelled(cancel)?;
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
                aliases.try_reserve(1).map_err(|_| "unable to reserve bounded SQLite rowid aliases")?;
                aliases.insert(rootpage, index);
            }
            tables.try_reserve(1).map_err(|_| "unable to reserve bounded SQLite schema tables")?;
            tables.push(LiveTable { name, rootpage, column_names: Some(column_names) });
        }
        check_cancelled(cancel)?;
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
    let reserved = usize::from(*header.get(20).ok_or("truncated SQLite header")?);
    let usable = page_size.checked_sub(reserved).ok_or("invalid SQLite reserved-byte count")?;
    if usable < 480 {
        return Err("SQLite usable page size is below 480 bytes".into());
    }
    // 0 = no schema yet (file format §1.3.10): a catalog whose tables are all still in its WAL
    // has that in the main file, and the WAL's own page 1 is checked again with the overlay
    let schema_format = be_u32(header, 44)?;
    if !(0..=4).contains(&schema_format) {
        return Err("unsupported SQLite schema format".into());
    }
    Ok(reserved)
}

pub(crate) fn wal_checksum(bytes: &[u8], big: bool, mut sum: (u32, u32)) -> (u32, u32) {
    for pair in bytes.as_chunks::<8>().0 {
        let first = [pair[0], pair[1], pair[2], pair[3]];
        let second = [pair[4], pair[5], pair[6], pair[7]];
        let (first, second) =
            if big { (u32::from_be_bytes(first), u32::from_be_bytes(second)) } else { (u32::from_le_bytes(first), u32::from_le_bytes(second)) };
        sum.0 = sum.0.wrapping_add(first).wrapping_add(sum.1);
        sum.1 = sum.1.wrapping_add(second).wrapping_add(sum.0);
    }
    sum
}

#[cfg(test)]
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
    let mut at = 0;
    let header_len = usize_from_u64(read_varint(payload, &mut at)?, "record header length")?;
    if header_len < at || header_len > payload.len() {
        return Err("invalid SQLite record header length".into());
    }
    let mut serials = Vec::new();
    while at < header_len {
        if serials.len() >= ncols {
            return Err("SQLite record has more columns than table schema".into());
        }
        serials.push(read_varint(payload, &mut at)?);
    }
    if at != header_len {
        return Err("SQLite record header is malformed".into());
    }
    let mut body = header_len;
    let mut values = Vec::with_capacity(ncols);
    for serial in serials {
        let (value, size) = serial_value(payload, &mut body, serial, encoding)?;
        values.push(value);
        if values.len() > MAX_COLUMNS {
            return Err("SQLite record has too many columns".into());
        }
        let _ = size;
    }
    if body > payload.len() {
        return Err("SQLite record body is truncated".into());
    }
    values.resize(ncols, Value::Null);
    Ok(values)
}

fn serial_value(payload: &[u8], body: &mut usize, serial: u64, encoding: TextEncoding) -> Result<(Value, usize), String> {
    let (kind, len) = match serial {
        0 => (0, 0),
        1 => (1, 1),
        2 => (2, 2),
        3 => (3, 3),
        4 => (4, 4),
        5 => (5, 6),
        6 => (6, 8),
        7 => (7, 8),
        8 => (8, 0),
        9 => (9, 0),
        10 | 11 => return Err("reserved SQLite serial type".into()),
        n if n >= 12 => {
            let len = usize_from_u64((n - if n % 2 == 0 { 12 } else { 13 }) / 2, "SQLite field length")?;
            (if n % 2 == 0 { 12 } else { 13 }, len)
        }
        _ => return Err("invalid SQLite serial type".into()),
    };
    let end = body.checked_add(len).ok_or("SQLite record body offset overflow")?;
    let bytes = payload.get(*body..end).ok_or("SQLite record body is truncated")?;
    let value = match kind {
        0 => Value::Null,
        1 => Value::Integer(i64::from(i8::from_be_bytes([*bytes.first().ok_or("truncated integer")?]))),
        2 => Value::Integer(i64::from(i16::from_be_bytes(bytes.try_into().map_err(|_| "truncated integer")?))),
        3 => {
            let first = *bytes.first().ok_or("truncated integer")?;
            let second = *bytes.get(1).ok_or("truncated integer")?;
            let third = *bytes.get(2).ok_or("truncated integer")?;
            let n = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
            let signed = if n & 0x80_0000 != 0 { n | 0xff00_0000 } else { n };
            Value::Integer(i64::from(i32::from_be_bytes(signed.to_be_bytes())))
        }
        4 => Value::Integer(i64::from(i32::from_be_bytes(bytes.try_into().map_err(|_| "truncated integer")?))),
        5 => {
            let n = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
            let signed = if n & (1 << 47) != 0 { n | 0xffff_0000_0000_0000 } else { n };
            Value::Integer(i64::from_be_bytes(signed.to_be_bytes()))
        }
        6 => Value::Integer(i64::from_be_bytes(bytes.try_into().map_err(|_| "truncated integer")?)),
        7 => {
            let n = f64::from_be_bytes(bytes.try_into().map_err(|_| "truncated real")?);
            if !n.is_finite() {
                return Err("non-finite SQLite real value".into());
            }
            Value::Real(n)
        }
        8 => Value::Integer(0),
        9 => Value::Integer(1),
        12 => Value::Blob(bytes.to_vec()),
        13 => Value::Text(decode_text(bytes, encoding)?),
        _ => return Err("invalid SQLite field type".into()),
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
    let size = if value == 1 { 65_536 } else { usize::from(value) };
    if !(512..=65_536).contains(&size) || !size.is_power_of_two() {
        return Err("invalid SQLite page size".into());
    }
    Ok(size)
}

fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes.get(*at).ok_or("truncated SQLite varint")?;
        *at = (*at).checked_add(1).ok_or("SQLite varint offset overflow")?;
        if i == 8 {
            value = (value << 8) | u64::from(byte);
            return Ok(value);
        }
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("invalid SQLite varint".into())
}

fn usize_from_u64(value: u64, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("{label} exceeds host size"))
}

fn be_u16(bytes: &[u8], at: usize) -> Result<u16, String> {
    let end = at.checked_add(2).ok_or("SQLite integer offset overflow")?;
    bytes.get(at..end).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_be_bytes).ok_or("truncated SQLite integer".into())
}

fn be_u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    be_u16(bytes, at)
}

fn be_u32(bytes: &[u8], at: usize) -> Result<u32, String> {
    let end = at.checked_add(4).ok_or("SQLite integer offset overflow")?;
    bytes.get(at..end).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated SQLite integer".into())
}

fn be_u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    be_u32(bytes, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    static CANCEL_AFTER_SCHEMA_ROOT_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static SCHEMA_PAGE_READS_TEST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn cancel_after_schema_root(page_id: u32) {
        use std::sync::atomic::Ordering;
        SCHEMA_PAGE_READS_TEST.fetch_add(1, Ordering::Relaxed);
        if page_id == 1 {
            CANCEL_AFTER_SCHEMA_ROOT_TEST.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn cancellation_stops_schema_btree_after_current_page() {
        use std::sync::atomic::Ordering;

        const PAGE_SIZE: usize = 512;
        let mut main = vec![0u8; 3 * PAGE_SIZE];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21] = 64;
        main[22] = 32;
        main[23] = 32;
        main[24..28].copy_from_slice(&1u32.to_be_bytes());
        main[28..32].copy_from_slice(&3u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        main[92..96].copy_from_slice(&1u32.to_be_bytes());
        main[100] = 0x05;
        main[103..105].copy_from_slice(&1u16.to_be_bytes());
        main[108..112].copy_from_slice(&3u32.to_be_bytes());
        main[112..114].copy_from_slice(&500u16.to_be_bytes());
        main[500..504].copy_from_slice(&2u32.to_be_bytes());
        main[504] = 1;

        CANCEL_AFTER_SCHEMA_ROOT_TEST.store(false, Ordering::Relaxed);
        SCHEMA_PAGE_READS_TEST.store(0, Ordering::Relaxed);
        set_page_read_hook_for_test(Some(cancel_after_schema_root));
        let header = main[..100].to_vec();
        let result = Database::open_parts(MainPages::Bytes(main), header, 3, None, None, &CANCEL_AFTER_SCHEMA_ROOT_TEST);
        set_page_read_hook_for_test(None);

        let error = match result {
            Ok(_) => panic!("schema traversal must stop when cancellation is signalled after the root page"),
            Err(error) => error,
        };
        assert!(error.contains("cancelled"), "unexpected schema cancellation error: {error}");
        assert_eq!(SCHEMA_PAGE_READS_TEST.load(Ordering::Relaxed), 1, "schema traversal must not read either child page");
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = vec![u8::try_from(value & 0x7f).unwrap_or(0)];
        value >>= 7;
        while value != 0 {
            out.push(u8::try_from(value & 0x7f).unwrap_or(0) | 0x80);
            value >>= 7;
        }
        out.reverse();
        out
    }

    fn leaf(page: &mut [u8], base: usize, rowid: u64, payload: &[u8]) {
        page[base] = 0x0d;
        page[base + 3..base + 5].copy_from_slice(&1u16.to_be_bytes());
        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(rowid));
        cell.extend(payload);
        let start = page.len() - cell.len();
        page[start..].copy_from_slice(&cell);
        page[base + 8..base + 10].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        page[base + 5..base + 7].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
    }

    fn leaf_rows(page: &mut [u8], base: usize, rows: &[(u64, &[u8])]) {
        page[base] = 0x0d;
        page[base + 3..base + 5].copy_from_slice(&u16::try_from(rows.len()).unwrap_or(0).to_be_bytes());
        let mut end = page.len();
        for (index, (rowid, payload)) in rows.iter().enumerate() {
            let mut cell = varint(payload.len() as u64);
            cell.extend(varint(*rowid));
            cell.extend(*payload);
            end -= cell.len();
            page[end..end + cell.len()].copy_from_slice(&cell);
            let pointer = base + 8 + index * 2;
            page[pointer..pointer + 2].copy_from_slice(&u16::try_from(end).unwrap_or(0).to_be_bytes());
        }
        page[base + 5..base + 7].copy_from_slice(&u16::try_from(end).unwrap_or(0).to_be_bytes());
    }

    fn schema_record(name: &str, root: u8, sql: &str) -> Vec<u8> {
        let mut header = vec![23];
        let text_serial = |text: &str| 13 + 2 * text.len() as u64;
        header.extend(varint(text_serial(name)));
        header.extend(varint(text_serial(name)));
        header.push(1);
        header.extend(varint(text_serial(sql)));
        let mut body = b"table".to_vec();
        body.extend(name.as_bytes());
        body.extend(name.as_bytes());
        body.push(root);
        body.extend(sql.as_bytes());
        let mut record = varint((1 + header.len()) as u64);
        record.extend(header);
        record.extend(body);
        record
    }

    fn sqlite_main(page_count: u32) -> Vec<u8> {
        let mut main = vec![0u8; page_count as usize * 512];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21..24].copy_from_slice(&[64, 32, 32]);
        main[24..28].copy_from_slice(&1u32.to_be_bytes());
        main[28..32].copy_from_slice(&page_count.to_be_bytes());
        main[40..44].copy_from_slice(&1u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        main[92..96].copy_from_slice(&1u32.to_be_bytes());
        main[100] = 0x0d;
        main
    }

    fn wal_fixture(page_size: usize, big: bool, frames: &[(u32, u32, u8)]) -> Vec<u8> {
        let pages: Vec<_> = frames.iter().map(|&(page_id, commit_size, fill)| (page_id, commit_size, vec![fill; page_size])).collect();
        let frame_pages: Vec<_> = pages.iter().map(|(page_id, commit_size, page)| (*page_id, *commit_size, page.as_slice())).collect();
        wal_fixture_pages(page_size, big, &frame_pages)
    }

    fn wal_fixture_pages(page_size: usize, big: bool, frames: &[(u32, u32, &[u8])]) -> Vec<u8> {
        let mut wal = vec![0; 32];
        wal[..4].copy_from_slice(&(if big { 0x377f0683u32 } else { 0x377f0682u32 }).to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&u32::try_from(page_size).unwrap_or(0).to_be_bytes());
        wal[16..20].copy_from_slice(&123u32.to_be_bytes());
        wal[20..24].copy_from_slice(&456u32.to_be_bytes());
        let mut checksum = wal_checksum(&wal[..24], big, (0, 0));
        wal[24..28].copy_from_slice(&checksum.0.to_be_bytes());
        wal[28..32].copy_from_slice(&checksum.1.to_be_bytes());
        for &(page_id, commit_size, page) in frames {
            assert_eq!(page.len(), page_size);
            let mut header = [0u8; 24];
            header[..4].copy_from_slice(&page_id.to_be_bytes());
            header[4..8].copy_from_slice(&commit_size.to_be_bytes());
            header[8..12].copy_from_slice(&123u32.to_be_bytes());
            header[12..16].copy_from_slice(&456u32.to_be_bytes());
            checksum = wal_checksum(&header[..8], big, checksum);
            checksum = wal_checksum(page, big, checksum);
            header[16..20].copy_from_slice(&checksum.0.to_be_bytes());
            header[20..24].copy_from_slice(&checksum.1.to_be_bytes());
            wal.extend(header);
            wal.extend(page);
        }
        wal
    }

    fn refresh_wal_checksums(wal: &mut [u8], page_size: usize, big: bool) {
        let mut checksum = wal_checksum(&wal[..24], big, (0, 0));
        let frame_stride = 24 + page_size;
        let mut offset = 32;
        while offset + frame_stride <= wal.len() {
            let header_end = offset + 24;
            let page_end = header_end + page_size;
            checksum = wal_checksum(&wal[offset..offset + 8], big, checksum);
            checksum = wal_checksum(&wal[header_end..page_end], big, checksum);
            wal[offset + 16..offset + 20].copy_from_slice(&checksum.0.to_be_bytes());
            wal[offset + 20..offset + 24].copy_from_slice(&checksum.1.to_be_bytes());
            offset += frame_stride;
        }
    }

    fn scratch_wal(bytes: &[u8]) -> (File, std::path::PathBuf) {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = lcc_brand::env_os("TEST_SCRATCH").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let directory = root.join("app-wal-index-fixtures");
        std::fs::create_dir_all(&directory).expect("create WAL fixture scratch directory");
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("wal-{}-{sequence}.wal", std::process::id()));
        let mut writer = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).expect("create isolated WAL fixture");
        writer.write_all(bytes).expect("write synthetic WAL fixture");
        writer.sync_all().expect("flush synthetic WAL fixture");
        drop(writer);
        (File::open(&path).expect("open synthetic WAL read-only"), path)
    }

    fn scratch_sqlite(bytes: &[u8]) -> (File, std::path::PathBuf) {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = lcc_brand::env_os("TEST_SCRATCH").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let directory = root.join("app-main-file-fixtures");
        std::fs::create_dir_all(&directory).expect("create SQLite fixture scratch directory");
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("main-{}-{sequence}.sqlite", std::process::id()));
        let mut writer = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).expect("create isolated SQLite fixture");
        writer.write_all(bytes).expect("write synthetic SQLite fixture");
        writer.sync_all().expect("flush synthetic SQLite fixture");
        drop(writer);
        (File::open(&path).expect("open synthetic SQLite database read-only"), path)
    }

    #[test]
    fn schema_names_and_integer_primary_key_alias_are_decoded() {
        let sql = r#"CREATE TABLE "t" ("id_local" INTEGER PRIMARY KEY, "odd,name" TEXT, PRIMARY KEY ("id_local"))"#;
        let (names, alias) = parse_create_table(sql).unwrap();
        assert_eq!(names, vec!["id_local", "odd,name"]);
        assert_eq!(alias, Some(0));
        let (_, no_alias) = parse_create_table("CREATE TABLE t (id INTEGER PRIMARY KEY DESC, value TEXT)").unwrap();
        assert_eq!(no_alias, None);
        let (table_names, table_alias) = parse_create_table("CREATE TABLE t (id INTEGER, value TEXT, PRIMARY KEY(id))").unwrap();
        assert_eq!(table_names, vec!["id", "value"]);
        assert_eq!(table_alias, Some(0));
    }

    /// A database file with no schema yet has schema format 0 in its header (file format
    /// §1.3.10): a catalog created in WAL mode whose tables are all still in its `-wal` file (one
    /// Lightroom has open and hasn't checkpointed) was refused as "unsupported SQLite schema
    /// format" before the WAL was looked at. It opens, with no tables; other formats stay refused.
    #[test]
    fn schema_format_zero_is_an_empty_database() {
        let header = |format: u32| {
            let mut main = vec![0u8; 512];
            main[..16].copy_from_slice(b"SQLite format 3\0");
            main[16..18].copy_from_slice(&512u16.to_be_bytes());
            main[18] = 2;
            main[19] = 2;
            main[21] = 64;
            main[22] = 32;
            main[23] = 32;
            main[28..32].copy_from_slice(&1u32.to_be_bytes());
            main[44..48].copy_from_slice(&format.to_be_bytes());
            main[56..60].copy_from_slice(&1u32.to_be_bytes());
            main[100] = 0x0d;
            main[105..107].copy_from_slice(&512u16.to_be_bytes());
            main
        };
        let db = Database::open_with_wal(header(0), &[]).unwrap();
        assert!(db.live_tables().unwrap().is_empty());
        assert!(Database::open_with_wal(header(4), &[]).is_ok());
        assert_eq!(Database::open_with_wal(header(5), &[]).err().as_deref(), Some("unsupported SQLite schema format"));
    }

    #[test]
    fn unsupported_unrelated_schema_entries_are_skipped() {
        let mut main = vec![0u8; 2 * 512];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21] = 64;
        main[22] = 32;
        main[23] = 32;
        main[28..32].copy_from_slice(&2u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        let virtual_sql = "CREATE VIRTUAL TABLE unrelated USING fts5(text)";
        let supported_sql = "CREATE TABLE supported (id INTEGER PRIMARY KEY)";
        let virtual_record = schema_record("unrelated", 2, virtual_sql);
        let supported_record = schema_record("supported", 2, supported_sql);
        leaf_rows(&mut main[..512], 100, &[(1, &virtual_record), (2, &supported_record)]);
        let db = Database::open_with_wal(main, &[]).unwrap();
        let tables = db.live_tables().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "supported");
    }

    #[test]
    fn synthetic_btree_substitutes_rowid_and_reads_schema() {
        let mut main = vec![0u8; 2 * 512];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21] = 64;
        main[22] = 32;
        main[23] = 32;
        main[28..32].copy_from_slice(&2u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        let sql = "CREATE TABLE \"t\" (\"id_local\" INTEGER PRIMARY KEY, \"name\" TEXT)";
        // rootpage is an integer field, so replace fourth text value with a compact record.
        let mut header = vec![23, 15, 15, 1];
        header.extend(varint((13 + 2 * sql.len()) as u64));
        let mut body = b"tablett".to_vec();
        body.push(2);
        body.extend(sql.as_bytes());
        let mut record = varint((1 + header.len()) as u64);
        record.append(&mut header);
        record.extend(body);
        let schema = record;
        leaf(&mut main[512..], 0, 1, &[3, 0, 15, b'x']);
        leaf(&mut main[..512], 100, 1, &schema);
        let db = Database::open_with_wal(main, &[]).unwrap();
        let tables = db.live_tables().unwrap();
        assert!(tables.iter().any(|table| table.name == "t"));
        let rows = db.read_table(2, 2).unwrap();
        assert_eq!(rows.first().and_then(|row| row.values.first()), Some(&Value::Integer(1)));
    }

    #[test]
    fn reads_overflow_payload_from_file_backed_main_pages_and_rejects_truncation() {
        const PAGE_SIZE: usize = 512;
        const LOCAL_PAYLOAD: usize = 95;

        let text = "z".repeat(600);
        let mut payload = varint(3);
        payload.extend(varint(13 + 2 * text.len() as u64));
        payload.extend(text.as_bytes());
        assert_eq!(payload.len(), 603);
        assert_eq!(payload.len() - LOCAL_PAYLOAD, PAGE_SIZE - 4);

        let mut main = vec![0u8; 3 * PAGE_SIZE];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21..24].copy_from_slice(&[64, 32, 32]);
        main[24..28].copy_from_slice(&1u32.to_be_bytes());
        main[28..32].copy_from_slice(&3u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        main[92..96].copy_from_slice(&1u32.to_be_bytes());

        let schema = schema_record("t", 2, "CREATE TABLE t (value TEXT)");
        leaf_rows(&mut main[..PAGE_SIZE], 100, &[(1, &schema)]);

        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(1));
        cell.extend(&payload[..LOCAL_PAYLOAD]);
        cell.extend(3u32.to_be_bytes());
        let cell_start = PAGE_SIZE - cell.len();
        let table_page = PAGE_SIZE;
        main[table_page] = 0x0d;
        main[table_page + 3..table_page + 5].copy_from_slice(&1u16.to_be_bytes());
        main[table_page + 5..table_page + 7].copy_from_slice(&(cell_start as u16).to_be_bytes());
        main[table_page + 8..table_page + 10].copy_from_slice(&(cell_start as u16).to_be_bytes());
        main[table_page + cell_start..table_page + PAGE_SIZE].copy_from_slice(&cell);
        let overflow_page = 2 * PAGE_SIZE;
        main[overflow_page + 4..3 * PAGE_SIZE].copy_from_slice(&payload[LOCAL_PAYLOAD..]);

        let (file, path) = scratch_sqlite(&main);
        let (truncated_file, truncated_path) = scratch_sqlite(&main[..2 * PAGE_SIZE]);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let db = Database::open_file_with_wal(file, main.len() as u64, None, &cancel).unwrap();
        let table = db.live_table("t", &cancel).unwrap().unwrap();
        assert_eq!(table.rootpage, 2);
        let rows = db.read_table_with_cancel(table.rootpage, 1, &cancel).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values, vec![Value::Text(text)]);

        let truncated_error = match Database::open_file_with_wal(truncated_file, (2 * PAGE_SIZE) as u64, None, &cancel) {
            Ok(_) => panic!("a truncated file must not satisfy its SQLite header page count"),
            Err(error) => error,
        };
        assert!(truncated_error.contains("header page count exceeds image"), "unexpected truncation error: {truncated_error}");
        drop(db);
        std::fs::remove_file(path).expect("remove synthetic file-backed overflow fixture");
        std::fs::remove_file(truncated_path).expect("remove synthetic truncated fixture");
    }

    static CANCEL_AFTER_OVERFLOW_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static OVERFLOW_PAGE_READS_TEST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn cancel_after_first_overflow_page(page_id: u32) {
        use std::sync::atomic::Ordering;
        if (3..=5).contains(&page_id) {
            OVERFLOW_PAGE_READS_TEST.fetch_add(1, Ordering::Relaxed);
            if page_id == 3 {
                CANCEL_AFTER_OVERFLOW_TEST.store(true, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn cancellation_stops_overflow_chain_after_current_page() {
        use std::sync::atomic::Ordering;

        const PAGE_SIZE: usize = 512;
        const LOCAL_PAYLOAD: usize = 39;
        let mut main = vec![0u8; 5 * PAGE_SIZE];
        let text = "x".repeat(1494);
        let mut payload = varint(3);
        payload.extend(varint(13 + 2 * text.len() as u64));
        payload.extend(text.as_bytes());
        assert_eq!(payload.len(), 1497);

        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(1));
        cell.extend(&payload[..LOCAL_PAYLOAD]);
        cell.extend(3u32.to_be_bytes());
        let cell_start = PAGE_SIZE - cell.len();
        main[PAGE_SIZE] = 0x0d;
        main[PAGE_SIZE + 3..PAGE_SIZE + 5].copy_from_slice(&1u16.to_be_bytes());
        main[PAGE_SIZE + 5..PAGE_SIZE + 7].copy_from_slice(&(cell_start as u16).to_be_bytes());
        main[PAGE_SIZE + 8..PAGE_SIZE + 10].copy_from_slice(&(cell_start as u16).to_be_bytes());
        main[PAGE_SIZE + cell_start..2 * PAGE_SIZE].copy_from_slice(&cell);

        let mut overflow = vec![0u8; PAGE_SIZE];
        overflow[..4].copy_from_slice(&4u32.to_be_bytes());
        overflow[4..].copy_from_slice(&payload[LOCAL_PAYLOAD..LOCAL_PAYLOAD + PAGE_SIZE - 4]);
        main[2 * PAGE_SIZE..3 * PAGE_SIZE].copy_from_slice(&overflow);
        overflow.fill(0);
        overflow[..4].copy_from_slice(&5u32.to_be_bytes());
        overflow[4..].copy_from_slice(&payload[LOCAL_PAYLOAD + PAGE_SIZE - 4..LOCAL_PAYLOAD + 2 * (PAGE_SIZE - 4)]);
        main[3 * PAGE_SIZE..4 * PAGE_SIZE].copy_from_slice(&overflow);
        overflow.fill(0);
        overflow[4..4 + payload.len() - LOCAL_PAYLOAD - 2 * (PAGE_SIZE - 4)].copy_from_slice(&payload[LOCAL_PAYLOAD + 2 * (PAGE_SIZE - 4)..]);
        main[4 * PAGE_SIZE..5 * PAGE_SIZE].copy_from_slice(&overflow);

        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: PAGE_SIZE,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 5,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        CANCEL_AFTER_OVERFLOW_TEST.store(false, Ordering::Relaxed);
        OVERFLOW_PAGE_READS_TEST.store(0, Ordering::Relaxed);
        set_page_read_hook_for_test(Some(cancel_after_first_overflow_page));
        let result = db.read_table_with_cancel(2, 1, &CANCEL_AFTER_OVERFLOW_TEST);
        set_page_read_hook_for_test(None);

        let error = match result {
            Ok(_) => panic!("overflow traversal must stop after cancellation on its first page"),
            Err(error) => error,
        };
        assert!(error.contains("cancelled"), "unexpected overflow cancellation error: {error}");
        assert_eq!(OVERFLOW_PAGE_READS_TEST.load(Ordering::Relaxed), 1, "must not read the rest of the overflow chain after cancellation");
    }

    #[test]
    fn interior_table_pointer_array_uses_cell_offsets() {
        let mut main = vec![0u8; 4 * 512];
        let mut interior = vec![0u8; 512];
        interior[0] = 0x05;
        interior[3..5].copy_from_slice(&1u16.to_be_bytes());
        interior[8..12].copy_from_slice(&4u32.to_be_bytes());
        interior[12..14].copy_from_slice(&500u16.to_be_bytes());
        interior[500..504].copy_from_slice(&3u32.to_be_bytes());
        interior[504] = 1;
        main[512..1024].copy_from_slice(&interior);
        leaf(&mut main[1024..1536], 0, 1, &[2, 0]);
        leaf(&mut main[1536..], 0, 2, &[2, 0]);
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 4,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        assert_eq!(db.read_table(2, 1).unwrap().len(), 2);
    }

    #[test]
    fn table_walk_accepts_a_valid_five_page_tree_under_the_normal_reader_budget() {
        fn interior(page: &mut [u8], base: usize, child: u32, right_child: u32) {
            page[base] = 0x05;
            page[base + 3..base + 5].copy_from_slice(&1u16.to_be_bytes());
            page[base + 8..base + 12].copy_from_slice(&right_child.to_be_bytes());
            page[base + 12..base + 14].copy_from_slice(&500u16.to_be_bytes());
            page[500..504].copy_from_slice(&child.to_be_bytes());
            page[504] = 1;
        }

        let mut main = vec![0u8; 7 * 512];
        interior(&mut main[..512], 100, 2, 3);
        interior(&mut main[512..1024], 0, 4, 5);
        leaf(&mut main[2 * 512..3 * 512], 0, 3, &[2, 0]);
        leaf(&mut main[3 * 512..4 * 512], 0, 1, &[2, 0]);
        leaf(&mut main[4 * 512..5 * 512], 0, 2, &[2, 0]);
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 7,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        let rows = db.read_table(1, 1).expect("valid five-page tree must use the production page budget in unit tests");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn table_walk_rejects_trees_exceeding_the_visited_page_budget() {
        fn interior(page: &mut [u8], base: usize, child: u32, right_child: u32) {
            page[base] = 0x05;
            page[base + 3..base + 5].copy_from_slice(&1u16.to_be_bytes());
            page[base + 8..base + 12].copy_from_slice(&right_child.to_be_bytes());
            page[base + 12..base + 14].copy_from_slice(&500u16.to_be_bytes());
            page[500..504].copy_from_slice(&child.to_be_bytes());
            page[504] = 1;
        }

        let mut main = vec![0u8; 7 * 512];
        interior(&mut main[..512], 100, 2, 3);
        interior(&mut main[512..1024], 0, 4, 5);
        leaf(&mut main[2 * 512..3 * 512], 0, 3, &[2, 0]);
        leaf(&mut main[3 * 512..4 * 512], 0, 1, &[2, 0]);
        leaf(&mut main[4 * 512..5 * 512], 0, 2, &[2, 0]);
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 7,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        let error = db.read_table_with_page_budget(1, 1, &never_cancel, 3).expect_err("a five-page traversal must exceed its injected test budget");
        assert!(error.contains("visited-page budget"), "unexpected error: {error}");
    }

    #[test]
    fn streamed_wal_index_keeps_last_commit_and_ignores_bad_or_uncommitted_tail() {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        for big in [false, true] {
            let stride = 24 + 512;
            let mut bytes = wal_fixture(512, big, &[(5, 6, 0x11), (5, 6, 0x22), (5, 0, 0x33), (6, 6, 0x44)]);
            let bad_frame_checksum = 32 + 3 * stride + 16;
            bytes[bad_frame_checksum] ^= 1;
            let len = u64::try_from(bytes.len()).expect("synthetic WAL length");
            let (file, path) = scratch_wal(&bytes);
            let index = WalFileIndex::scan(file, len, 512, &cancelled, Arc::new(AtomicBool::new(false))).expect("scan valid committed prefix");
            assert_eq!(index.committed_frames, 2);
            assert_eq!(index.page_count, Some(6));
            assert_eq!(index.page(5).unwrap().unwrap(), vec![0x22; 512]);
            assert!(index.page(6).unwrap().is_none(), "bad-tail frame must not be indexed");
            drop(index);
            std::fs::remove_file(path).expect("remove isolated WAL fixture");
        }
    }

    #[test]
    fn streamed_wal_index_ignores_uncommitted_only_and_rejects_truncation() {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let bytes = wal_fixture(512, false, &[(5, 0, 0x55)]);
        let len = u64::try_from(bytes.len()).expect("synthetic WAL length");
        let (file, path) = scratch_wal(&bytes);
        let index = WalFileIndex::scan(file, len, 512, &cancelled, Arc::new(AtomicBool::new(false))).expect("scan valid uncommitted WAL");
        assert_eq!(index.committed_frames, 0);
        assert_eq!(index.page_count, None);
        assert!(index.page(5).unwrap().is_none());
        drop(index);
        std::fs::remove_file(&path).expect("remove isolated WAL fixture");

        let bytes = wal_fixture(512, false, &[(5, 6, 0x66)]);
        let len = u64::try_from(bytes.len()).expect("synthetic WAL length");
        let (file, path) = scratch_wal(&bytes);
        let index = WalFileIndex::scan(file, len, 512, &cancelled, Arc::new(AtomicBool::new(false))).expect("scan committed WAL");
        let writer = std::fs::OpenOptions::new().write(true).open(&path).expect("open WAL fixture for truncation");
        writer.set_len(len - 1).expect("truncate synthetic WAL after indexing");
        assert!(index.page(5).is_err(), "truncated indexed frame must not fall back to main");
        drop(index);
        drop(writer);
        std::fs::remove_file(path).expect("remove isolated WAL fixture");
    }

    #[test]
    fn streamed_wal_index_rejects_invalid_header_and_keeps_the_wal_scan_cap() {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        for offset in [0usize, 24] {
            let mut bytes = wal_fixture(512, false, &[]);
            bytes[offset] ^= 1;
            let len = u64::try_from(bytes.len()).expect("synthetic WAL length");
            let (file, path) = scratch_wal(&bytes);
            let result = WalFileIndex::scan(file, len, 512, &cancelled, Arc::new(AtomicBool::new(false)));
            assert!(result.is_err(), "invalid WAL header byte {offset} must fail");
            drop(result);
            std::fs::remove_file(path).expect("remove isolated WAL fixture");
        }
        let (file, path) = scratch_wal(&wal_fixture(512, false, &[]));
        let result = WalFileIndex::scan(file, MAX_WAL_BYTES + 1, 512, &cancelled, Arc::new(AtomicBool::new(false)));
        assert!(result.is_err(), "scan budget must reject an oversized WAL length before reading");
        drop(result);
        std::fs::remove_file(path).expect("remove isolated WAL fixture");
    }

    #[test]
    fn streamed_wal_rejects_unsupported_version_and_page_size() {
        let cancelled = AtomicBool::new(false);
        let mut invalid_version = wal_fixture(512, false, &[]);
        invalid_version[4..8].copy_from_slice(&3_007_001u32.to_be_bytes());
        refresh_wal_checksums(&mut invalid_version, 512, false);
        let (file, path) = scratch_wal(&invalid_version);
        let version_result = WalFileIndex::scan(file, invalid_version.len() as u64, 512, &cancelled, Arc::new(AtomicBool::new(false)));
        let version_error = match version_result {
            Err(error) => error,
            Ok(_) => panic!("unsupported WAL version must fail"),
        };
        assert!(version_error.contains("invalid WAL header"), "unexpected version result: {version_error}");
        std::fs::remove_file(path).expect("remove invalid-version WAL fixture");

        let mut invalid_page_size = wal_fixture(512, false, &[]);
        invalid_page_size[8..12].copy_from_slice(&1024u32.to_be_bytes());
        refresh_wal_checksums(&mut invalid_page_size, 512, false);
        let (file, path) = scratch_wal(&invalid_page_size);
        let page_size_result = WalFileIndex::scan(file, invalid_page_size.len() as u64, 512, &cancelled, Arc::new(AtomicBool::new(false)));
        let page_size_error = match page_size_result {
            Err(error) => error,
            Ok(_) => panic!("mismatched WAL page size must fail"),
        };
        assert!(page_size_error.contains("WAL page size mismatch"), "unexpected page-size result: {page_size_error}");
        std::fs::remove_file(path).expect("remove invalid-page-size WAL fixture");
    }

    #[test]
    fn streamed_wal_stops_at_salt_mismatch_and_ignores_partial_tail() {
        let cancelled = AtomicBool::new(false);
        let stride = 24 + 512;
        let mut mismatched_salt = wal_fixture(512, false, &[(5, 6, 0x11), (6, 6, 0x22)]);
        mismatched_salt[32 + stride + 8] ^= 1;
        mismatched_salt.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        let (file, path) = scratch_wal(&mismatched_salt);
        let index = WalFileIndex::scan(file, mismatched_salt.len() as u64, 512, &cancelled, Arc::new(AtomicBool::new(false)))
            .expect("retain committed prefix before mismatched salt");
        assert_eq!(index.committed_frames, 1);
        assert_eq!(index.page_count, Some(6));
        assert_eq!(index.page(5).unwrap().unwrap(), vec![0x11; 512]);
        assert!(index.page(6).unwrap().is_none());
        drop(index);
        std::fs::remove_file(path).expect("remove mismatched-salt WAL fixture");

        let mut partial_tail = wal_fixture(512, false, &[(5, 6, 0x33)]);
        partial_tail.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let (file, path) = scratch_wal(&partial_tail);
        let index = WalFileIndex::scan(file, partial_tail.len() as u64, 512, &cancelled, Arc::new(AtomicBool::new(false)))
            .expect("ignore incomplete trailing frame");
        assert_eq!(index.committed_frames, 1);
        assert_eq!(index.page(5).unwrap().unwrap(), vec![0x33; 512]);
        drop(index);
        std::fs::remove_file(path).expect("remove partial-tail WAL fixture");
    }

    #[test]
    fn streamed_wal_rejects_invalid_page_id_and_commit_size_in_a_valid_frame() {
        let cancelled = AtomicBool::new(false);
        for (offset, invalid_value, expected) in [
            (32usize, MAX_PAGE_ID + 1, "WAL frame page number is outside database"),
            (36usize, MAX_PAGE_ID + 1, "WAL commit database size is too large"),
        ] {
            let mut bytes = wal_fixture(512, false, &[(5, 6, 0x44)]);
            bytes[offset..offset + 4].copy_from_slice(&invalid_value.to_be_bytes());
            refresh_wal_checksums(&mut bytes, 512, false);
            let (file, path) = scratch_wal(&bytes);
            let result = WalFileIndex::scan(file, bytes.len() as u64, 512, &cancelled, Arc::new(AtomicBool::new(false)));
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("invalid committed WAL frame field must fail"),
            };
            assert!(error.contains(expected), "unexpected invalid committed-frame result: {error}");
            std::fs::remove_file(path).expect("remove invalid-frame WAL fixture");
        }
    }

    #[test]
    fn file_wal_page_one_invalid_signature_is_rejected_over_valid_main() {
        let cancel = AtomicBool::new(false);
        let main = sqlite_main(1);
        let mut invalid_wal_page_one = main[..512].to_vec();
        invalid_wal_page_one[0] = b'X';
        let wal = wal_fixture_pages(512, false, &[(1, 1, &invalid_wal_page_one)]);
        let (main_file, main_path) = scratch_sqlite(&main);
        let (wal_file, wal_path) = scratch_wal(&wal);
        let result = Database::open_file_with_wal(main_file, main.len() as u64, Some((wal_file, wal.len() as u64)), &cancel);
        let error = result.err();
        std::fs::remove_file(main_path).expect("remove valid MAIN fixture");
        std::fs::remove_file(wal_path).expect("remove invalid-signature WAL fixture");
        assert_eq!(error.as_deref(), Some("invalid SQLite header page in WAL"));
    }

    #[test]
    fn file_wal_page_one_can_grow_visible_database_and_supply_high_page() {
        let cancel = AtomicBool::new(false);
        let mut main = sqlite_main(6);
        let schema = schema_record("t", 7, "CREATE TABLE t (value TEXT)");
        leaf_rows(&mut main[..512], 100, &[(1, &schema)]);
        let mut effective_page_one = main[..512].to_vec();
        effective_page_one[24..28].copy_from_slice(&2u32.to_be_bytes());
        effective_page_one[28..32].copy_from_slice(&7u32.to_be_bytes());
        effective_page_one[92..96].copy_from_slice(&2u32.to_be_bytes());
        let mut page_seven = vec![0u8; 512];
        leaf(&mut page_seven, 0, 1, &[2, 17, b'o', b'k']);
        let wal = wal_fixture_pages(512, false, &[(7, 0, &page_seven), (1, 7, &effective_page_one)]);
        let (main_file, main_path) = scratch_sqlite(&main);
        let (wal_file, wal_path) = scratch_wal(&wal);
        let db = Database::open_file_with_wal(main_file, main.len() as u64, Some((wal_file, wal.len() as u64)), &cancel)
            .expect("open database grown by committed WAL");
        assert_eq!(db.page_count, 7);
        let table = db.live_table("t", &cancel).unwrap().expect("schema table supplied by effective page one");
        assert_eq!(table.rootpage, 7);
        let rows = db.read_table_with_cancel(table.rootpage, 1, &cancel).expect("read page supplied by committed WAL");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values, vec![Value::Text("ok".into())]);
        drop(db);
        std::fs::remove_file(main_path).expect("remove growing-main fixture");
        std::fs::remove_file(wal_path).expect("remove growing-WAL fixture");
    }

    #[test]
    fn file_wal_shrink_hides_pages_beyond_final_commit_size() {
        let cancel = AtomicBool::new(false);
        let main = sqlite_main(6);
        let page_six = vec![0x66; 512];
        let wal = wal_fixture_pages(512, false, &[(6, 4, &page_six)]);
        let (main_file, main_path) = scratch_sqlite(&main);
        let (wal_file, wal_path) = scratch_wal(&wal);
        let db = Database::open_file_with_wal(main_file, main.len() as u64, Some((wal_file, wal.len() as u64)), &cancel)
            .expect("open database shrunk by final WAL commit");
        assert_eq!(db.page_count, 4);
        assert!(db.page(5, &cancel).unwrap_err().contains("outside database"), "MAIN pages beyond the final commit must be inaccessible");
        drop(db);
        std::fs::remove_file(main_path).expect("remove shrinking-main fixture");
        std::fs::remove_file(wal_path).expect("remove shrinking-WAL fixture");
    }

    static CANCEL_DURING_WAL_SCAN_TEST: AtomicBool = AtomicBool::new(false);

    fn cancel_during_wal_scan_test() {
        CANCEL_DURING_WAL_SCAN_TEST.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn cancellation_interrupts_file_wal_frame_scan() {
        CANCEL_DURING_WAL_SCAN_TEST.store(false, std::sync::atomic::Ordering::Relaxed);
        let bytes = wal_fixture(512, false, &[(5, 6, 0x77), (6, 6, 0x88)]);
        let (file, path) = scratch_wal(&bytes);
        set_wal_scan_hook_for_test(Some(cancel_during_wal_scan_test));
        let result = WalFileIndex::scan(file, bytes.len() as u64, 512, &CANCEL_DURING_WAL_SCAN_TEST, Arc::new(AtomicBool::new(false)));
        set_wal_scan_hook_for_test(None);
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("cancellation during WAL scan must stop indexing"),
        };
        assert!(error.contains("cancelled"), "unexpected WAL scan cancellation: {error}");
        std::fs::remove_file(path).expect("remove cancelled WAL fixture");
    }

    #[test]
    fn file_page_offset_checks_zero_and_overflow_and_source_reads_stick_fatal_state() {
        let (file, offset_path) = scratch_sqlite(&sqlite_main(2));
        let source_failure = Arc::new(AtomicBool::new(false));
        let pages = FilePages { file: Mutex::new(file), len: u64::MAX, source_failure: source_failure.clone() };
        assert!(pages.read_page(0, 512).unwrap_err().contains("page number is zero"));
        #[cfg(target_pointer_width = "64")]
        assert!(pages.read_page(2, usize::MAX).unwrap_err().contains("page offset overflow"));
        assert!(wal_frame_offset(usize::MAX, u64::MAX).is_err());
        assert!(!source_failure.load(std::sync::atomic::Ordering::Relaxed), "invalid offsets are not source-I/O failures");
        drop(pages);
        std::fs::remove_file(offset_path).expect("remove offset-boundary fixture");

        let main = sqlite_main(2);
        let (file, path) = scratch_sqlite(&main);
        let cancel = AtomicBool::new(false);
        let db = Database::open_file_with_wal(file, main.len() as u64, None, &cancel).expect("open synthetic main before truncation");
        let truncate = std::fs::OpenOptions::new().write(true).open(&path).expect("open main fixture for injected read failure");
        truncate.set_len(512).expect("truncate main fixture after initialization");
        let read_error = db.page(2, &cancel).unwrap_err();
        assert!(read_error.contains("read SQLite page 2"), "unexpected main-page I/O error: {read_error}");
        assert!(db.has_observed_source_failure(), "the observed main-source read failure must remain sticky");
        drop(db);
        drop(truncate);
        std::fs::remove_file(path).expect("remove read-failure main fixture");
    }

    #[test]
    fn negative_rowid_varint_is_signed() {
        let mut main = vec![0u8; 512];
        let page = &mut main[..];
        page[100] = 0x0d;
        page[103..105].copy_from_slice(&1u16.to_be_bytes());
        let start = 500usize;
        page[start] = 2;
        for offset in 1..10 {
            if let Some(byte) = page.get_mut(start + offset) {
                *byte = 0xff;
            }
        }
        page[start + 10..start + 12].copy_from_slice(&[2, 0]);
        page[108..110].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        page[105..107].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        let mut aliases = HashMap::new();
        aliases.insert(1, 0);
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 1,
            tables: Vec::new(),
            rowid_aliases: aliases,
        };
        let rows = db.read_table(1, 1).unwrap();
        assert_eq!(rows.first().and_then(|row| row.values.first()), Some(&Value::Integer(-1)));
    }

    #[test]
    fn reserved_page_trailer_cannot_supply_cells() {
        let mut main = vec![0u8; 512];
        main[100] = 0x0d;
        main[103..105].copy_from_slice(&1u16.to_be_bytes());
        main[108..110].copy_from_slice(&490u16.to_be_bytes());
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: 512,
            reserved: 32,
            encoding: TextEncoding::Utf8,
            page_count: 1,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        assert!(db.read_table(1, 1).is_err());
    }

    #[test]
    fn utf16_and_overflow_payloads_are_supported() {
        let mut payload = vec![2, 17, b'A', 0];
        assert_eq!(decode_record(&payload, 1, TextEncoding::Utf16Le).unwrap(), vec![Value::Text("A".into())]);
        payload.clear();
        payload.extend([2, 17, 0, b'A']);
        assert_eq!(decode_record(&payload, 1, TextEncoding::Utf16Be).unwrap(), vec![Value::Text("A".into())]);

        let mut main = vec![0u8; 2 * 512];
        let db = Database {
            main: MainPages::Bytes(std::mem::take(&mut main)),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 2,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        let mut cell_page = vec![b'x'; 512];
        // For a 512-byte table leaf & 600-byte payload, SQLite stores 92
        // payload bytes locally, followed by the overflow page pointer.
        cell_page[92..96].copy_from_slice(&2u32.to_be_bytes());
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        let inline_page = vec![b'i'; 512];
        assert_eq!(db.payload(&inline_page, 0, 110, &never_cancel).unwrap(), vec![b'i'; 110]);
        let mut overflow_page = vec![b'y'; 512];
        overflow_page[..4].copy_from_slice(&0u32.to_be_bytes());
        let mut db = db;
        db.wal = Some(WalPages::Bytes(HashMap::from([(2, overflow_page)])));
        let bytes = db.payload(&cell_page, 0, 600, &never_cancel).unwrap();
        assert_eq!(bytes.len(), 600);
        assert_eq!(&bytes[..92], &[b'x'; 92]);
        assert_eq!(&bytes[92..], &[b'y'; 508]);
    }

    #[test]
    fn malformed_varint_and_wal_cycle_fail_cleanly() {
        let mut at = 0;
        assert!(read_varint(&[0x80; 8], &mut at).is_err());
        let mut wal = vec![0u8; 32 + 24 + 512];
        wal[..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&512u32.to_be_bytes());
        wal[16..20].copy_from_slice(&7u32.to_be_bytes());
        wal[20..24].copy_from_slice(&8u32.to_be_bytes());
        wal[32..36].copy_from_slice(&1u32.to_be_bytes());
        wal[36..40].copy_from_slice(&1u32.to_be_bytes());
        wal[40..44].copy_from_slice(&7u32.to_be_bytes());
        wal[44..48].copy_from_slice(&8u32.to_be_bytes());
        let parsed = parse_wal(&wal, 512).unwrap();
        assert_eq!(parsed.frame_count, 1);
        assert_eq!(parsed.page_count, Some(1));
        assert_eq!(parsed.pages.get(&1).map(Vec::len), Some(512));
        let mut page = vec![0u8; 512];
        page[16..18].copy_from_slice(&512u16.to_be_bytes());
        let db = Database {
            main: MainPages::Bytes(vec![0; 512]),
            wal: None,
            page_size: 512,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: 1,
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        let never_cancel = std::sync::atomic::AtomicBool::new(false);
        assert!(db.payload(&page, 0, 600, &never_cancel).is_err());
    }

    /// A table larger than any in-memory cap streams through `for_each_row_with_cancel` one record at a time:
    /// 1.2 million rows (more than a collecting read keeps) under one interior page; the sink only counts.
    #[test]
    fn large_tables_stream_past_the_collect_cap() {
        const PAGE: usize = 32 * 1024;
        const ROWS: u64 = 1_200_000;
        let per_leaf = 3_000u64;
        let leaves = ROWS.div_ceil(per_leaf) as usize;
        let mut main = vec![0u8; PAGE * (2 + leaves)];
        let mut rowid = 0u64;
        for leaf_index in 0..leaves {
            let page = &mut main[PAGE * (2 + leaf_index)..PAGE * (3 + leaf_index)];
            let n = per_leaf.min(ROWS - rowid);
            let rows: Vec<(u64, &[u8])> = (0..n).map(|i| (rowid + i + 1, &[2u8, 1, 7][..])).collect();
            leaf_rows(page, 0, &rows);
            rowid += n;
        }
        // Page 2: the interior root, one cell per leaf but the last (the right child).
        let interior = &mut main[PAGE..2 * PAGE];
        interior[0] = 0x05;
        interior[3..5].copy_from_slice(&u16::try_from(leaves - 1).unwrap().to_be_bytes());
        interior[8..12].copy_from_slice(&u32::try_from(2 + leaves).unwrap().to_be_bytes());
        let mut end = PAGE;
        for i in 0..leaves - 1 {
            let mut cell = u32::try_from(3 + i).unwrap().to_be_bytes().to_vec();
            cell.extend(varint((i as u64 + 1) * per_leaf));
            end -= cell.len();
            interior[end..end + cell.len()].copy_from_slice(&cell);
            interior[12 + i * 2..14 + i * 2].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
        }
        let db = Database {
            main: MainPages::Bytes(main),
            wal: None,
            page_size: PAGE,
            reserved: 0,
            encoding: TextEncoding::Utf8,
            page_count: u32::try_from(2 + leaves).unwrap(),
            tables: Vec::new(),
            rowid_aliases: HashMap::new(),
        };
        let never_cancel = AtomicBool::new(false);
        let (mut count, mut sum) = (0u64, 0i64);
        db.for_each_row_with_cancel(2, 1, &never_cancel, &mut |r| {
            count += 1;
            if let Some(Value::Integer(n)) = r.values.first() {
                sum += n;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!((count, sum), (ROWS, 7 * ROWS as i64));
        assert!(db.read_table(2, 1).is_err(), "collecting a whole table stays capped");
        assert!(db.for_each_kept_row_with_cancel(2, 1, &never_cancel, &mut |_| Ok(())).is_err(), "a kept walk stays capped");
        // A sink error stops the walk and is returned.
        let mut seen = 0;
        let stopped = db.for_each_row_with_cancel(2, 1, &never_cancel, &mut |_| {
            seen += 1;
            if seen == 10 { Err("enough".into()) } else { Ok(()) }
        });
        assert_eq!((stopped, seen), (Err("enough".to_string()), 10));
    }
}
