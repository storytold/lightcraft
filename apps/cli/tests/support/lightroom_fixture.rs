use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_COUNT: u32 = 262_160;
pub const LOCK_BYTE_PAGE: u32 = 262_145;
pub const PHOTO_PAGE: u32 = PAGE_COUNT;

#[derive(Clone)]
enum Field {
    Null,
    Integer(i64),
    Text(String),
}

pub fn write_large_sparse_catalog(directory: &Path) -> PathBuf {
    write_sparse_catalog(directory, true)
}

pub fn write_small_catalog(directory: &Path) -> PathBuf {
    write_sparse_catalog(directory, false)
}

fn write_sparse_catalog(directory: &Path, large: bool) -> PathBuf {
    let page_count = if large { PAGE_COUNT } else { 5 };
    let photo_page = if large { PHOTO_PAGE } else { 5 };
    let path = directory.join(if large { "synthetic-large.lrcat" } else { "synthetic-small.lrcat" });
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).expect("create new sparse synthetic catalog");
    drop(file);
    if cfg!(windows) {
        let set = std::process::Command::new("C:/Windows/System32/fsutil.exe")
            .args(["sparse", "setflag"])
            .arg(&path)
            .output()
            .expect("set sparse flag on synthetic catalog only");
        assert!(set.status.success(), "fsutil sparse setflag: {}", String::from_utf8_lossy(&set.stderr));
        let query = std::process::Command::new("C:/Windows/System32/fsutil.exe")
            .args(["sparse", "queryflag"])
            .arg(&path)
            .output()
            .expect("verify sparse flag on synthetic catalog only");
        let text = format!("{}{}", String::from_utf8_lossy(&query.stdout), String::from_utf8_lossy(&query.stderr));
        assert!(query.status.success() && text.to_ascii_lowercase().contains("sparse"), "fsutil sparse queryflag: {text}");
    }
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open sparse synthetic catalog for page writes");
    file.set_len(u64::from(page_count) * PAGE_SIZE as u64).expect("sparsely extend synthetic catalog");

    let originals = directory.join("synthetic-originals");
    let tables = [
        (
            "AgLibraryRootFolder",
            "id_local INTEGER PRIMARY KEY, absolutePath",
            vec![Field::Null, Field::Text(originals.to_string_lossy().into_owned())],
        ),
        (
            "AgLibraryFolder",
            "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot",
            vec![Field::Null, Field::Integer(1), Field::Text("Photos".into())],
        ),
        ("AgLibraryFile", "id_local INTEGER PRIMARY KEY, folder, idx_filename", vec![Field::Null, Field::Integer(1), Field::Text("test.ARW".into())]),
        (
            "Adobe_images",
            "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
            vec![
                Field::Null,
                Field::Text("synthetic-large-catalog-uuid".into()),
                Field::Integer(1),
                Field::Text("ARW".into()),
                Field::Integer(6000),
                Field::Integer(4000),
                Field::Integer(5),
                Field::Integer(1),
            ],
        ),
    ];
    let mut schema_rows = Vec::new();
    for (index, (name, columns, row)) in tables.iter().enumerate() {
        let root_page = if index == 3 { photo_page } else { index as u32 + 2 };
        schema_rows.push(vec![
            Field::Text("table".into()),
            Field::Text((*name).into()),
            Field::Text((*name).into()),
            Field::Integer(i64::from(root_page)),
            Field::Text(format!("CREATE TABLE {name} ({columns})")),
        ]);
        write_page(&mut file, root_page, &leaf(std::slice::from_ref(row), 0), PAGE_SIZE as u64);
    }

    let mut free_count = 0u32;
    for page_id in 5..photo_page {
        if page_id != LOCK_BYTE_PAGE {
            free_count = free_count.checked_add(1).expect("synthetic freelist page count");
        }
    }
    let mut free_pages = (5..photo_page).filter(|page_id| *page_id != LOCK_BYTE_PAGE);
    let first_trunk = free_pages.next().unwrap_or(0);
    let mut trunk = first_trunk;
    while trunk != 0 {
        let mut leaves = Vec::with_capacity(1018);
        for _ in 0..1018 {
            let Some(page_id) = free_pages.next() else { break };
            leaves.push(page_id);
        }
        let next_trunk = free_pages.next().unwrap_or(0);
        let mut page = vec![0u8; PAGE_SIZE];
        page[..4].copy_from_slice(&next_trunk.to_be_bytes());
        page[4..8].copy_from_slice(&u32::try_from(leaves.len()).expect("freelist leaf count").to_be_bytes());
        for (index, page_id) in leaves.iter().enumerate() {
            let offset = 8 + index * 4;
            page[offset..offset + 4].copy_from_slice(&page_id.to_be_bytes());
        }
        write_page(&mut file, trunk, &page, PAGE_SIZE as u64);
        trunk = next_trunk;
    }

    let mut header = vec![0u8; PAGE_SIZE];
    header[..16].copy_from_slice(b"SQLite format 3\0");
    header[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
    header[18] = 1;
    header[19] = 1;
    header[21] = 64;
    header[22] = 32;
    header[23] = 32;
    header[24..28].copy_from_slice(&1u32.to_be_bytes());
    header[28..32].copy_from_slice(&page_count.to_be_bytes());
    header[32..36].copy_from_slice(&first_trunk.to_be_bytes());
    header[36..40].copy_from_slice(&free_count.to_be_bytes());
    header[40..44].copy_from_slice(&1u32.to_be_bytes());
    header[44..48].copy_from_slice(&4u32.to_be_bytes());
    header[56..60].copy_from_slice(&1u32.to_be_bytes());
    header[92..96].copy_from_slice(&1u32.to_be_bytes());
    leaf_into(&mut header, 100, &schema_rows);
    write_page(&mut file, 1, &header, PAGE_SIZE as u64);
    file.sync_all().expect("flush synthetic SQLite fixture");
    path
}

fn write_page(file: &mut std::fs::File, page_id: u32, page: &[u8], page_size: u64) {
    let offset = u64::from(page_id - 1).checked_mul(page_size).expect("fixture page offset");
    file.seek(SeekFrom::Start(offset)).expect("seek synthetic page");
    file.write_all(page).expect("write synthetic page");
}

fn leaf(rows: &[Vec<Field>], base: usize) -> Vec<u8> {
    let mut page = vec![0u8; PAGE_SIZE];
    leaf_into(&mut page, base, rows);
    page
}

fn leaf_into(page: &mut [u8], base: usize, rows: &[Vec<Field>]) {
    page[base] = 0x0d;
    page[base + 3..base + 5].copy_from_slice(&u16::try_from(rows.len()).expect("synthetic leaf row count").to_be_bytes());
    let mut end = page.len();
    for (index, values) in rows.iter().enumerate() {
        let payload = record(values);
        let mut cell = varint(u64::try_from(payload.len()).expect("synthetic payload length"));
        cell.extend(varint(u64::try_from(index + 1).expect("synthetic rowid")));
        cell.extend(payload);
        end -= cell.len();
        page[end..end + cell.len()].copy_from_slice(&cell);
        let pointer = base + 8 + index * 2;
        page[pointer..pointer + 2].copy_from_slice(&u16::try_from(end).expect("synthetic cell offset").to_be_bytes());
    }
    page[base + 5..base + 7].copy_from_slice(&u16::try_from(end).expect("synthetic content offset").to_be_bytes());
}

fn record(values: &[Field]) -> Vec<u8> {
    let mut serials = Vec::new();
    let mut body = Vec::new();
    for value in values {
        let serial = match value {
            Field::Null => 0,
            Field::Integer(number) => {
                body.extend(number.to_be_bytes());
                6
            }
            Field::Text(text) => {
                body.extend(text.as_bytes());
                13 + 2 * u64::try_from(text.len()).expect("synthetic text length")
            }
        };
        serials.extend(varint(serial));
    }
    let header_size = 1 + serials.len();
    let mut bytes = varint(u64::try_from(header_size).expect("synthetic record header length"));
    bytes.extend(serials);
    bytes.extend(body);
    bytes
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = vec![u8::try_from(value & 0x7f).unwrap_or(0)];
    value >>= 7;
    while value != 0 {
        bytes.push(u8::try_from(value & 0x7f).unwrap_or(0) | 0x80);
        value >>= 7;
    }
    bytes.reverse();
    bytes
}
