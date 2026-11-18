//! Peak heap of a `read_window` that must reject a product's strips (`crates/denoise-core/src/product.rs`), measured
//! with dhat's testing mode. The result is allocated only after the wanted strips' checksums are verified, so a cache
//! whose compressed bytes are damaged or absent fails without first reserving the (multi-megabyte) output. One test
//! per binary (dhat's profiler is global).

use lightcraft_denoise_core::product::read_window;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lc-denoise-{name}-{}.lcdn", std::process::id()))
}

/// A 2000 × 1024 product of key `k` (a 24 MB result): its 8 strips each claim 8 bytes that do not match their
/// checksum, and the file is exactly the length its header and table declare. A 24 MB peak means the output was
/// reserved; the cap here bounds the header, the table and the strips' own bytes.
fn damaged_product() -> Vec<u8> {
    let header = br#"{"width":2000,"height":1024,"strip":128,"key":"k"}"#;
    const STRIP_LEN: usize = 8;
    let mut bytes = Vec::from(b"LCDN".as_slice());
    bytes.extend_from_slice(&[1, 0, 0, 0]);
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(header);
    let expected_len = 12 + header.len() + 8 * 8 + 8 * STRIP_LEN;
    for _ in 0..8 {
        // 8 claimed bytes whose recorded checksum does not match theirs (0xAA bytes do not have this CRC)
        bytes.extend_from_slice(&(STRIP_LEN as u32).to_le_bytes());
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    }
    for _ in 0..8 {
        bytes.extend_from_slice(&[0xAAu8; STRIP_LEN]);
    }
    assert_eq!(bytes.len(), expected_len, "the crafted file lies about its length");
    bytes
}

#[test]
fn a_damaged_product_is_rejected_before_its_output_is_allocated() {
    let bytes = damaged_product();
    let p = temp("damaged-product-heap");
    std::fs::write(&p, &bytes).unwrap();
    let _profiler = dhat::Profiler::builder().testing().build();
    let why = read_window(&p, "k", None, 1, None).err().unwrap().to_string();
    let peak = dhat::HeapStats::get().max_bytes;
    let _ = std::fs::remove_file(p);
    assert!(why.contains("damaged"), "{why}");
    eprintln!("peak heap {peak} bytes");
    assert!(peak < 4 << 20, "peak heap {peak} bytes: the output was reserved before the strips were checked");
}
