use super::*;

/// A little-endian TIFF with one IFD: a single strip of `len` bytes at offset 64.
fn tiff(len: u32) -> Vec<u8> {
    let mut b = vec![0u8; 64];
    b[0..2].copy_from_slice(b"II");
    b[2..4].copy_from_slice(&42u16.to_le_bytes());
    b[4..8].copy_from_slice(&8u32.to_le_bytes());
    b[8..10].copy_from_slice(&2u16.to_le_bytes());
    let entry = |b: &mut Vec<u8>, at: usize, tag: u16, v: u32| {
        b[at..at + 2].copy_from_slice(&tag.to_le_bytes());
        b[at + 2..at + 4].copy_from_slice(&4u16.to_le_bytes());
        b[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
        b[at + 8..at + 12].copy_from_slice(&v.to_le_bytes());
    };
    entry(&mut b, 10, 0x0111, 64);
    entry(&mut b, 22, 0x0117, len);
    b.extend(std::iter::repeat_n(7u8, len as usize));
    b
}

#[test]
fn a_complete_raw_is_not_flagged_and_a_truncated_one_is() {
    let b = tiff(1000);
    assert!(declared_end(&b).is_some());
    assert!(!declared_end_past(&b, b.len() as u64));
    let head = &b[..500];
    assert!(declared_end_past(head, head.len() as u64), "a truncated raw is still being written");
}

/// A complete Panasonic RW2 that gives its strip offset as 0xFFFFFFFF and the real one in tag 0x0118 is not taken for a
/// file still being copied (a copy of one made in the last ten minutes was refused: "not a readable photo"); a short
/// one still is.
#[test]
fn a_panasonic_raw_with_a_placeholder_strip_offset_is_complete() {
    let mut b = vec![0u8; 64];
    b[0..2].copy_from_slice(b"II");
    b[2..4].copy_from_slice(&0x55u16.to_le_bytes());
    b[4..8].copy_from_slice(&8u32.to_le_bytes());
    b[8..10].copy_from_slice(&3u16.to_le_bytes());
    let entry = |b: &mut Vec<u8>, at: usize, tag: u16, v: u32| {
        b[at..at + 2].copy_from_slice(&tag.to_le_bytes());
        b[at + 2..at + 4].copy_from_slice(&4u16.to_le_bytes());
        b[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
        b[at + 8..at + 12].copy_from_slice(&v.to_le_bytes());
    };
    entry(&mut b, 10, 0x0111, u32::MAX);
    entry(&mut b, 22, 0x0117, 1000);
    entry(&mut b, 34, 0x0118, 64);
    b.extend(std::iter::repeat_n(7u8, 1000));
    assert_eq!(declared_end(&b), Some(1064));
    assert!(!declared_end_past(&b, b.len() as u64));
    assert!(declared_end_past(&b[..500], 500));
    // in a plain TIFF, 0x0118 is MinSampleValue: a placeholder offset there gives no end, and 0x0118 is not an offset
    b[2..4].copy_from_slice(&42u16.to_le_bytes());
    assert_eq!(declared_end(&b), None);
}

/// Whatever the format, a strip that would end past what a TIFF's 32-bit offsets can address (a maker's placeholder
/// offset or count) says nothing about the file being complete: no file is taken for one still being copied by it.
#[test]
fn an_impossible_strip_end_never_marks_a_file_as_being_copied() {
    for (offset, count) in [(u32::MAX, 1000), (0xFFFF_FF00, 0x1000), (64, u32::MAX)] {
        let mut b = tiff(100);
        b[18..22].copy_from_slice(&offset.to_le_bytes());
        b[30..34].copy_from_slice(&count.to_le_bytes());
        assert_eq!(declared_end(&b), None, "offset {offset:#x}, count {count:#x}");
        assert!(!declared_end_past(&b, b.len() as u64));
    }
}

#[test]
fn hostile_headers_never_panic() {
    let base = tiff(16);
    let mut cases: Vec<Vec<u8>> = vec![vec![], b"II".to_vec(), b"MM\0*".to_vec(), b"II*\0\xff\xff\xff\xff".to_vec()];
    // every single-byte corruption and every truncation of a valid header
    for i in 0..base.len() {
        for v in [0u8, 0xff, 0x80] {
            let mut c = base.clone();
            c[i] = v;
            cases.push(c);
        }
        cases.push(base[..i].to_vec());
    }
    // an IFD whose next-IFD link points at itself, and a huge value count
    let mut looped = base.clone();
    looped[34..38].copy_from_slice(&8u32.to_le_bytes());
    cases.push(looped);
    let mut huge = base.clone();
    huge[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(huge);
    for c in &cases {
        let _ = declared_end_past(c, c.len() as u64);
    }
}

#[test]
fn temporary_names_are_skipped() {
    for n in [".IMG_0001.NEF.a1B2c3", "IMG_0001.NEF.part", "x.crdownload", "~$a.tif"] {
        assert!(temp_name(Path::new(n)), "{n}");
        assert!(still_being_written(Path::new(n)), "{n}");
    }
    assert!(!temp_name(Path::new("IMG_0001.NEF")));
}

#[test]
fn a_closed_file_is_not_being_written() {
    let dir = std::env::temp_dir().join(format!("incoming-closed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.nef");
    std::fs::write(&f, b"x").unwrap();
    assert!(!still_being_written(&f));
    assert!(!still_being_written(&dir.join("missing.nef")));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Windows sees an open writer on a local disk too; macOS / BSD only on network shares, which a
/// test can't count on.
#[cfg(windows)]
#[test]
fn an_open_writer_is_seen() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("incoming-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.nef");
    let mut w = std::fs::File::create(&f).unwrap();
    w.write_all(b"partial").unwrap();
    assert!(writer_active(&f));
    drop(w);
    assert!(!writer_active(&f));
    std::fs::remove_dir_all(&dir).unwrap();
}
