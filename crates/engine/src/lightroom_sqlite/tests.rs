use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lightcraft-page-source-{}-{n}.lrcat", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn file_backed(mut db: Database) -> (Scratch, Database) {
    let scratch = Scratch::new(&db.main);
    db.file = Some(FileSource::open(&scratch.0).unwrap());
    db.main.truncate(100);
    (scratch, db)
}

#[test]
fn file_source_bounds_and_mutation_detection() {
    let scratch = Scratch::new(&[0; 512]);
    let source = FileSource::open(&scratch.0).unwrap();
    assert_eq!(source.page(1, 512).unwrap().len(), 512);
    assert!(source.page(2, 512).unwrap_err().contains("truncated"));
    File::options().write(true).open(&scratch.0).unwrap().set_len(100).unwrap();
    assert!(source.page(1, 512).is_err());
    assert!(source.ensure_unchanged().unwrap_err().contains("changed during reading"));
    drop(source);
    // Unix set_len creates a hole. Do not reserve a 16 GiB normal file on Windows.
    #[cfg(unix)]
    {
        File::options().write(true).open(&scratch.0).unwrap().set_len(MAX_DATABASE_BYTES + 1).unwrap();
        assert_eq!(FileSource::open(&scratch.0).err().unwrap(), "catalog exceeds 16 GiB import limit");
    }
}

#[cfg(unix)]
#[test]
fn file_source_detects_same_sized_path_replacement() {
    let original = Scratch::new(&[0; 512]);
    let replacement = Scratch::new(&[0; 512]);
    let source = FileSource::open(&original.0).unwrap();
    std::fs::rename(&replacement.0, &original.0).unwrap();
    assert!(source.ensure_unchanged().unwrap_err().contains("changed during reading"));
}

#[test]
fn file_source_detects_same_length_content_changes() {
    let scratch = Scratch::new(&[0; 512]);
    let source = FileSource::open(&scratch.0).unwrap();
    std::fs::write(&scratch.0, [1; 512]).unwrap();
    // Make the timestamp change deterministic even on filesystems with coarse timestamps.
    File::options()
        .write(true)
        .open(&scratch.0)
        .unwrap()
        .set_modified(source.before.modified().unwrap() + std::time::Duration::from_secs(2))
        .unwrap();
    assert!(source.ensure_unchanged().unwrap_err().contains("changed during reading"));
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
    let scratch = Scratch::new(&db.main);
    let file_db = Database::open_file_with_wal(FileSource::open(&scratch.0).unwrap(), &[]).unwrap();
    assert_eq!(file_db.live_tables().unwrap(), tables);
    assert_eq!(file_db.read_table(2, 2).unwrap(), rows);
    assert_eq!(file_db.main.len(), 100);
    file_db.ensure_unchanged().unwrap();
    // The same committed WAL page overrides the lazily read main page.
    let mut wal = vec![0; 32 + 24 + 512];
    wal[..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
    wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
    wal[8..12].copy_from_slice(&512u32.to_be_bytes());
    wal[32..36].copy_from_slice(&2u32.to_be_bytes());
    wal[36..40].copy_from_slice(&2u32.to_be_bytes());
    leaf(&mut wal[56..], 0, 2, &[3, 0, 15, b'y']);
    let memory_wal = Database::open_with_wal(db.main.clone(), &wal).unwrap();
    let file_wal = Database::open_file_with_wal(FileSource::open(&scratch.0).unwrap(), &wal).unwrap();
    assert_eq!(memory_wal.read_table(2, 2).unwrap(), file_wal.read_table(2, 2).unwrap());
    assert_eq!(file_wal.read_table(2, 2).unwrap()[0].values[0], Value::Integer(2));
    let mut malformed = db.main.clone();
    malformed.push(0);
    let partial = Scratch::new(&malformed);
    assert_eq!(
        Database::open_file_with_wal(FileSource::open(&partial.0).unwrap(), &[]).err().unwrap(),
        "SQLite image is not a whole number of pages"
    );
    let mut malformed = db.main.clone();
    malformed[24..28].copy_from_slice(&1u32.to_be_bytes());
    malformed[92..96].copy_from_slice(&1u32.to_be_bytes());
    malformed[28..32].copy_from_slice(&3u32.to_be_bytes());
    let oversized = Scratch::new(&malformed);
    assert_eq!(Database::open_file_with_wal(FileSource::open(&oversized.0).unwrap(), &[]).err().unwrap(), "SQLite header page count exceeds image");
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
        file: None,
        main,
        overlay: HashMap::new(),
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
        file: None,
        main,
        overlay: HashMap::new(),
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
        file: None,
        main,
        overlay: HashMap::new(),
        page_size: 512,
        reserved: 32,
        encoding: TextEncoding::Utf8,
        page_count: 1,
        tables: Vec::new(),
        rowid_aliases: HashMap::new(),
    };
    assert!(db.read_table(1, 1).is_err());
    let (_scratch, db) = file_backed(db);
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
        file: None,
        main: std::mem::take(&mut main),
        overlay: HashMap::new(),
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
    let inline_page = vec![b'i'; 512];
    assert_eq!(db.payload(&inline_page, 0, 110).unwrap(), vec![b'i'; 110]);
    let mut overflow_page = vec![b'y'; 512];
    overflow_page[..4].copy_from_slice(&0u32.to_be_bytes());
    let mut db = db;
    db.main[512..].copy_from_slice(&overflow_page);
    let mut with_overflow = db.overlay;
    with_overflow.insert(2, overflow_page);
    db.overlay = with_overflow;
    let bytes = db.payload(&cell_page, 0, 600).unwrap();
    assert_eq!(bytes.len(), 600);
    assert_eq!(&bytes[..92], &[b'x'; 92]);
    assert_eq!(&bytes[92..], &[b'y'; 508]);
    let (_scratch, mut db) = file_backed(db);
    db.overlay.clear();
    let bytes = db.payload(&cell_page, 0, 600).unwrap();
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
        file: None,
        main: vec![0; 512],
        overlay: HashMap::new(),
        page_size: 512,
        reserved: 0,
        encoding: TextEncoding::Utf8,
        page_count: 1,
        tables: Vec::new(),
        rowid_aliases: HashMap::new(),
    };
    assert!(db.payload(&page, 0, 600).is_err());
}
