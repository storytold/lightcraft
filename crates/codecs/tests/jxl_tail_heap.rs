#![cfg(all(feature = "jxl", not(target_arch = "wasm32")))]

use lightcraft_codecs::*;
use lightcraft_raster::Rgba8;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn codestream(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(b"\xff\x0a") {
        return bytes.to_vec();
    }
    let mut raw = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let end = if len == 0 { bytes.len() } else { at + len };
        match &bytes[at + 4..at + 8] {
            b"jxlc" => raw.extend_from_slice(&bytes[at + 8..end]),
            b"jxlp" => raw.extend_from_slice(&bytes[at + 12..end]),
            _ => {}
        }
        at = end;
    }
    assert!(raw.starts_with(b"\xff\x0a"));
    raw
}

#[test]
fn tiny_jxl_tail_does_not_allocate_a_tail_sized_buffer() {
    let image = Rgba8::from_fn(8, 8, |x, y| [(x * 20) as u8, (y * 20) as u8, 80, 255]);
    let encoded = encode_jxl(&EncodeImage::rgba8(&image), &JxlOptions { lossless: true, ..Default::default() }, &EncodeMeta::default()).unwrap();
    let mut raw = codestream(&encoded);
    let opts = DecodeOptions { max_pixels: 64, ..Default::default() };
    decode(&raw, opts.clone()).unwrap();
    raw.resize(raw.len() + (1 << 20), 0);
    let _profiler = dhat::Profiler::builder().testing().build();
    let decoded = decode(&raw, opts).unwrap();
    assert_eq!((decoded.width, decoded.height), (8, 8));
    let peak = dhat::HeapStats::get().max_bytes;
    eprintln!("peak heap bytes: {peak}");
    assert!(peak < (128 << 10), "tiny image used {peak} heap bytes");
}
