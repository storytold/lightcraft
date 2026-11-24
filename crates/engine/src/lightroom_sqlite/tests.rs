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

#[test]
fn record_serial_types_must_end_in_the_header() {
    assert_eq!(decode_record(&[2, 1, 7], 2, TextEncoding::Utf8).unwrap(), [Value::Integer(7), Value::Null]);
    for payload in [&[2, 0x80][..], &[2, 0x80, 9][..]] {
        assert_eq!(decode_record(payload, 2, TextEncoding::Utf8).unwrap_err(), "SQLite record header is malformed");
    }
}

#[test]
fn record_values_decode_all_serial_types_and_reject_reserved() {
    let mut header = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17];
    let header_len = 1 + header.len();
    let mut payload = varint(header_len as u64);
    payload.append(&mut header);
    payload.push((-5i8).to_be_bytes()[0]);
    payload.extend((-300i16).to_be_bytes());
    payload.extend([0xff, 0xff, 0xfe]); // 24-bit -2
    payload.extend((-100_000i32).to_be_bytes());
    payload.extend([0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]); // 48-bit -2
    payload.extend(i64::MIN.to_be_bytes());
    payload.extend(3.5f64.to_be_bytes());
    payload.extend([0xaa, 0xbb]);
    payload.extend(b"hi");

    let values = decode_record(&payload, 13, TextEncoding::Utf8).unwrap();
    assert_eq!(
        values,
        vec![
            Value::Null,
            Value::Integer(-5),
            Value::Integer(-300),
            Value::Integer(-2),
            Value::Integer(-100_000),
            Value::Integer(-2),
            Value::Integer(i64::MIN),
            Value::Real(3.5),
            Value::Integer(0),
            Value::Integer(1),
            Value::Blob(vec![0xaa, 0xbb]),
            Value::Text("hi".into()),
            Value::Null,
        ]
    );

    assert_eq!(decode_record(&[2, 10], 1, TextEncoding::Utf8).unwrap_err(), "reserved SQLite serial type");
    assert_eq!(decode_record(&[2, 11], 1, TextEncoding::Utf8).unwrap_err(), "reserved SQLite serial type");

    let mut nan_payload = vec![2, 7];
    nan_payload.extend(f64::NAN.to_be_bytes());
    assert_eq!(decode_record(&nan_payload, 1, TextEncoding::Utf8).unwrap_err(), "non-finite SQLite real value");
}

#[test]
fn committed_wal_overrides_page_one_schema_and_page_count() {
    let make_page1 = |schema_format: u32, page_count: u32| {
        let mut page = vec![0u8; 512];
        page[..16].copy_from_slice(b"SQLite format 3\0");
        page[16..18].copy_from_slice(&512u16.to_be_bytes());
        page[18] = 2;
        page[19] = 2;
        page[21] = 64;
        page[22] = 32;
        page[23] = 32;
        page[28..32].copy_from_slice(&page_count.to_be_bytes());
        page[44..48].copy_from_slice(&schema_format.to_be_bytes());
        page[56..60].copy_from_slice(&1u32.to_be_bytes());
        page[100] = 0x0d;
        page[105..107].copy_from_slice(&512u16.to_be_bytes());
        page
    };

    let main = make_page1(0, 1);
    let mut wal_page1 = make_page1(4, 2);
    let schema = schema_record("t", 2, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
    leaf(&mut wal_page1, 100, 1, &schema);

    let mut wal_page2 = vec![0u8; 512];
    leaf(&mut wal_page2, 0, 42, &[3, 0, 17, b'o', b'k']);

    let mut wal = vec![0u8; 32 + 2 * (24 + 512)];
    wal[..4].copy_from_slice(&0x377f0683u32.to_be_bytes());
    wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
    wal[8..12].copy_from_slice(&512u32.to_be_bytes());
    wal[16..20].copy_from_slice(&11u32.to_be_bytes());
    wal[20..24].copy_from_slice(&22u32.to_be_bytes());

    // Frame 1: page 2 (uncommitted)
    let f1 = 32;
    wal[f1..f1 + 4].copy_from_slice(&2u32.to_be_bytes());
    wal[f1 + 8..f1 + 12].copy_from_slice(&11u32.to_be_bytes());
    wal[f1 + 12..f1 + 16].copy_from_slice(&22u32.to_be_bytes());
    wal[f1 + 24..f1 + 24 + 512].copy_from_slice(&wal_page2);

    // Frame 2: page 1 (commit with page_count = 2)
    let f2 = 32 + 24 + 512;
    wal[f2..f2 + 4].copy_from_slice(&1u32.to_be_bytes());
    wal[f2 + 4..f2 + 8].copy_from_slice(&2u32.to_be_bytes());
    wal[f2 + 8..f2 + 12].copy_from_slice(&11u32.to_be_bytes());
    wal[f2 + 12..f2 + 16].copy_from_slice(&22u32.to_be_bytes());
    wal[f2 + 24..f2 + 24 + 512].copy_from_slice(&wal_page1);

    let db = Database::open_with_wal(main.clone(), &wal).unwrap();
    let tables = db.live_tables().unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "t");
    let rows = db.read_table(2, 2).unwrap();
    assert_eq!(rows, vec![Record { values: vec![Value::Integer(42), Value::Text("ok".into())] }]);

    // Salt mismatch in WAL frame is rejected.
    wal[f1 + 8..f1 + 12].copy_from_slice(&99u32.to_be_bytes());
    assert_eq!(Database::open_with_wal(main, &wal).err().as_deref(), Some("invalid WAL frame page or salt"));
}

#[test]
fn reader_enforces_column_page_and_cycle_limits() {
    let mut main = vec![0u8; 512];
    main[100] = 0x05;
    main[103..105].copy_from_slice(&0u16.to_be_bytes());
    main[108..112].copy_from_slice(&1u32.to_be_bytes()); // self-cycle
    let db = Database {
        file: None,
        main,
        overlay: HashMap::new(),
        page_size: 512,
        reserved: 0,
        encoding: TextEncoding::Utf8,
        page_count: 1,
        tables: Vec::new(),
        rowid_aliases: HashMap::new(),
    };
    assert_eq!(db.read_table(0, 1).unwrap_err(), "table root page is outside database");
    assert_eq!(db.read_table(2, 1).unwrap_err(), "table root page is outside database");
    assert_eq!(db.read_table(1, MAX_COLUMNS + 1).unwrap_err(), "table has too many columns");
    assert_eq!(db.read_table(1, 1).unwrap_err(), "SQLite B-tree contains a cycle or shared page");
    assert_eq!(decode_record(&[3, 8, 9], 1, TextEncoding::Utf8).unwrap_err(), "SQLite record has more columns than table schema");
}

#[test]
fn committed_wal_multi_commit_growth_and_shrink_parity() {
    let make_page1 = |schema_format: u32, page_count: u32| {
        let mut page = vec![0u8; 512];
        page[..16].copy_from_slice(b"SQLite format 3\0");
        page[16..18].copy_from_slice(&512u16.to_be_bytes());
        page[18] = 2;
        page[19] = 2;
        page[21] = 64;
        page[22] = 32;
        page[23] = 32;
        page[28..32].copy_from_slice(&page_count.to_be_bytes());
        page[44..48].copy_from_slice(&schema_format.to_be_bytes());
        page[56..60].copy_from_slice(&1u32.to_be_bytes());
        page[100] = 0x0d;
        page[105..107].copy_from_slice(&512u16.to_be_bytes());
        page
    };

    let main = make_page1(0, 1);
    let mut wal_page1 = make_page1(4, 3);
    let schema = schema_record("t", 2, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
    leaf(&mut wal_page1, 100, 1, &schema);

    let mut stale_page2 = vec![0u8; 512];
    leaf(&mut stale_page2, 0, 1, &[3, 0, 23, b's', b't', b'a', b'l', b'e']);
    let mut fresh_page2 = vec![0u8; 512];
    leaf(&mut fresh_page2, 0, 2, &[3, 0, 23, b'f', b'r', b'e', b's', b'h']);

    let mut grown_page3 = vec![0u8; 512];
    leaf(&mut grown_page3, 0, 3, &[2, 1, 3]);

    let frame = |page_id: u32, commit: u32, page_bytes: &[u8]| {
        let mut out = vec![0u8; 24 + 512];
        out[..4].copy_from_slice(&page_id.to_be_bytes());
        out[4..8].copy_from_slice(&commit.to_be_bytes());
        out[8..12].copy_from_slice(&11u32.to_be_bytes());
        out[12..16].copy_from_slice(&22u32.to_be_bytes());
        out[24..].copy_from_slice(page_bytes);
        out
    };

    let mut wal = vec![0u8; 32];
    wal[..4].copy_from_slice(&0x377f0683u32.to_be_bytes());
    wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
    wal[8..12].copy_from_slice(&512u32.to_be_bytes());
    wal[16..20].copy_from_slice(&11u32.to_be_bytes());
    wal[20..24].copy_from_slice(&22u32.to_be_bytes());
    wal.extend(frame(2, 0, &stale_page2));
    wal.extend(frame(3, 0, &grown_page3));
    wal.extend(frame(1, 3, &wal_page1));
    wal.extend(frame(2, 3, &fresh_page2));

    let scratch = Scratch::new(&main);
    let mem_db = Database::open_with_wal(main.clone(), &wal).unwrap();
    let file_db = Database::open_file_with_wal(FileSource::open(&scratch.0).unwrap(), &wal).unwrap();
    assert_eq!(mem_db.live_tables().unwrap(), file_db.live_tables().unwrap());
    assert_eq!(mem_db.read_table(2, 2).unwrap(), file_db.read_table(2, 2).unwrap());
    assert_eq!(mem_db.read_table(2, 2).unwrap(), vec![Record { values: vec![Value::Integer(2), Value::Text("fresh".into())] }]);

    let grown_rows = vec![Record { values: vec![Value::Integer(3)] }];
    assert_eq!(mem_db.read_table(3, 1).unwrap(), grown_rows);
    assert_eq!(file_db.read_table(3, 1).unwrap(), grown_rows);

    // Append a shrink commit to the very same WAL: page 3 is still present as an old frame.
    let mut shrunk_page1 = make_page1(4, 2);
    leaf(&mut shrunk_page1, 100, 1, &schema);
    wal.extend(frame(1, 2, &shrunk_page1));
    let mem_shrunk = Database::open_with_wal(main, &wal).unwrap();
    let file_shrunk = Database::open_file_with_wal(FileSource::open(&scratch.0).unwrap(), &wal).unwrap();
    assert_eq!(mem_shrunk.read_table(2, 2).unwrap(), mem_db.read_table(2, 2).unwrap());
    assert_eq!(file_shrunk.read_table(2, 2).unwrap(), file_db.read_table(2, 2).unwrap());
    assert_eq!(mem_shrunk.read_table(3, 1).unwrap_err(), "table root page is outside database");
    assert_eq!(file_shrunk.read_table(3, 1).unwrap_err(), "table root page is outside database");

    // Also check shrink against a main file that physically contains the page beyond the boundary.
    let mut three_page_main = make_page1(4, 3);
    three_page_main.resize(3 * 512, 0);
    leaf(&mut three_page_main[..512], 100, 1, &schema);
    let mut shrink_wal = wal[..32].to_vec();
    shrink_wal.extend(frame(2, 2, &fresh_page2));
    let shrink_scratch = Scratch::new(&three_page_main);
    let mem_shrunk = Database::open_with_wal(three_page_main, &shrink_wal).unwrap();
    let file_shrunk = Database::open_file_with_wal(FileSource::open(&shrink_scratch.0).unwrap(), &shrink_wal).unwrap();
    assert_eq!(mem_shrunk.read_table(3, 1).unwrap_err(), "table root page is outside database");
    assert_eq!(file_shrunk.read_table(3, 1).unwrap_err(), "table root page is outside database");
}

#[test]
fn cross_app_parity_text_blob_null_overflow_and_ordering() {
    let mut page1 = vec![0u8; 512];
    page1[..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&512u16.to_be_bytes());
    page1[18] = 2;
    page1[19] = 2;
    page1[21] = 64;
    page1[22] = 32;
    page1[23] = 32;
    page1[28..32].copy_from_slice(&5u32.to_be_bytes());
    page1[44..48].copy_from_slice(&4u32.to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());
    let schema = schema_record(
        "samples",
        2,
        "CREATE TABLE samples(id INTEGER PRIMARY KEY, n INTEGER, r REAL, missing, label TEXT, overflow TEXT, binary BLOB)",
    );
    leaf(&mut page1, 100, 1, &schema);

    // Page 2: interior table page pointing to leaf page 3 (rowid <= 1) and right leaf page 4.
    let mut page2 = vec![0u8; 512];
    page2[0] = 0x05;
    page2[3..5].copy_from_slice(&1u16.to_be_bytes());
    page2[8..12].copy_from_slice(&4u32.to_be_bytes());
    let mut interior_cell = 3u32.to_be_bytes().to_vec();
    interior_cell.extend(varint(1));
    let icell_off = 512 - interior_cell.len();
    page2[icell_off..].copy_from_slice(&interior_cell);
    page2[5..7].copy_from_slice(&u16::try_from(icell_off).unwrap().to_be_bytes());
    page2[12..14].copy_from_slice(&u16::try_from(icell_off).unwrap().to_be_bytes());

    // Row 1 on page 3 with 600-byte overflow text spanning into page 5.
    let overflow_text = "x".repeat(600);
    let mut row1 = Vec::new();
    let mut hdr1 = vec![0u8, 3, 7, 0, 23, 0, 16]; // placeholder header len
    let text_serial = u64::try_from(overflow_text.len() * 2 + 13).unwrap();
    let mut hdr1_body = vec![0u8, 3, 7, 0];
    hdr1_body.extend(varint(u64::try_from("Päris".len() * 2 + 13).unwrap()));
    hdr1_body.extend(varint(text_serial));
    hdr1_body.push(16); // 2-byte blob
    let hlen1 = varint(u64::try_from(hdr1_body.len() + 1).unwrap());
    hdr1.clear();
    hdr1.extend(hlen1);
    hdr1.extend(hdr1_body);
    row1.extend(&hdr1);
    row1.extend(&(-8_388_607i32).to_be_bytes()[1..4]); // 3-byte signed int
    row1.extend(1.25f64.to_be_bytes());
    row1.extend("Päris".as_bytes());
    row1.extend(overflow_text.as_bytes());
    row1.extend([0x00, 0xff]);

    let local = craft_sqlite::local_payload_len(512, row1.len() as u64, craft_sqlite::PayloadKind::TableLeaf).unwrap() as usize;
    let mut page3 = vec![0u8; 512];
    page3[0] = 0x0d;
    page3[3..5].copy_from_slice(&1u16.to_be_bytes());
    let mut cell1 = varint(u64::try_from(row1.len()).unwrap());
    cell1.extend(varint(1));
    cell1.extend(&row1[..local]);
    cell1.extend(5u32.to_be_bytes()); // overflow page 5
    let c1_off = 512 - cell1.len();
    page3[c1_off..].copy_from_slice(&cell1);
    page3[5..7].copy_from_slice(&u16::try_from(c1_off).unwrap().to_be_bytes());
    page3[8..10].copy_from_slice(&u16::try_from(c1_off).unwrap().to_be_bytes());

    let mut page5 = vec![0u8; 512];
    let rem = row1.len() - local;
    page5[4..4 + rem].copy_from_slice(&row1[local..]);

    // Row 2 on page 4: NULLs, 0, 1, empty blob.
    let mut page4 = vec![0u8; 512];
    leaf(&mut page4, 0, 2, &[8, 0, 8, 9, 0, 13, 13, 12]);

    let mut main = Vec::with_capacity(5 * 512);
    main.extend(page1);
    main.extend(page2);
    main.extend(page3);
    main.extend(page4);
    main.extend(page5);

    let scratch = Scratch::new(&main);
    let mem_db = Database::open_with_wal(main.clone(), &[]).unwrap();
    let file_db = Database::open_file_with_wal(FileSource::open(&scratch.0).unwrap(), &[]).unwrap();
    assert_eq!(mem_db.read_table(2, 7).unwrap(), file_db.read_table(2, 7).unwrap());
    assert_eq!(
        mem_db.read_table(2, 7).unwrap(),
        vec![
            Record {
                values: vec![
                    Value::Integer(1),
                    Value::Integer(-8_388_607),
                    Value::Real(1.25),
                    Value::Null,
                    Value::Text("Päris".into()),
                    Value::Text(overflow_text),
                    Value::Blob(vec![0x00, 0xff]),
                ],
            },
            Record {
                values: vec![
                    Value::Integer(2),
                    Value::Integer(0),
                    Value::Integer(1),
                    Value::Null,
                    Value::Text(String::new()),
                    Value::Text(String::new()),
                    Value::Blob(Vec::new()),
                ],
            },
        ]
    );

    // Cyclic overflow chain is rejected in both memory and file readers.
    let mut cyclic = main.clone();
    cyclic[4 * 512..4 * 512 + 4].copy_from_slice(&5u32.to_be_bytes());
    let cyclic_local = craft_sqlite::local_payload_len(512, 1200, craft_sqlite::PayloadKind::TableLeaf).unwrap() as usize;
    let mut cyclic_cell = varint(1200);
    cyclic_cell.extend(varint(1));
    cyclic_cell.extend(vec![b'x'; cyclic_local]);
    cyclic_cell.extend(5u32.to_be_bytes());
    let cc_off = 512 - cyclic_cell.len();
    cyclic[2 * 512 + cc_off..3 * 512].copy_from_slice(&cyclic_cell);
    cyclic[2 * 512 + 5..2 * 512 + 7].copy_from_slice(&u16::try_from(cc_off).unwrap().to_be_bytes());
    cyclic[2 * 512 + 8..2 * 512 + 10].copy_from_slice(&u16::try_from(cc_off).unwrap().to_be_bytes());
    let cyclic_scratch = Scratch::new(&cyclic);
    let cyclic_mem = Database::open_with_wal(cyclic, &[]).unwrap();
    let cyclic_file = Database::open_file_with_wal(FileSource::open(&cyclic_scratch.0).unwrap(), &[]).unwrap();
    assert_eq!(cyclic_mem.read_table(2, 7).unwrap_err(), "invalid or cyclic SQLite overflow chain");
    assert_eq!(cyclic_file.read_table(2, 7).unwrap_err(), "invalid or cyclic SQLite overflow chain");

    // Truncation and corruption errors match across both readers.
    assert_eq!(Database::open_with_wal(main[..50].to_vec(), &[]).err().as_deref(), Some("truncated SQLite header"));
    assert_eq!(Database::open_with_wal(main[..600].to_vec(), &[]).err().as_deref(), Some("SQLite image is not a whole number of pages"));
}
