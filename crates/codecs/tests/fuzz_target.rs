//! Fuzz target `codecs` (`cargo xtask fuzz codecs`): mutated JPEG/PNG/TIFF/WebP files and corpus images
//! through sniffing, decoding (unguarded, so our own panics aren't masked), headers, thumbnails, ICC and
//! gain maps. A short run in `cargo test`; time-boxed long runs under `LCC_FUZZ_SECS` (see
//! `lcc-fuzzkit`). Crash inputs go into `tests/it/fuzz.rs`.

use lcc_codecs::exif::minimal_exif;
use lcc_codecs::icc::write_named;
use lcc_codecs::*;
use lcc_raster::Rgba8;

fn seeds() -> Vec<Vec<u8>> {
    let img = Rgba8::from_fn(24, 16, |x, y| [(x * 10) as u8, (y * 15) as u8, ((x ^ y) * 8) as u8, (x * y) as u8]);
    let icc = write_named(NamedSpace::DisplayP3);
    let exif = minimal_exif(6);
    let meta = EncodeMeta { icc: Some(&icc), exif: Some(&exif), xmp: Some("<x/>"), ..Default::default() };
    let e = EncodeImage::rgba8(&img);
    let u16s: Vec<u16> = (0..24 * 16 * 3).map(|i| (i * 331) as u16).collect();
    vec![
        encode_jpeg(&e, 80, ChromaSubsampling::S420, &meta).unwrap(),
        encode_png(&e, &meta).unwrap(),
        encode_png(&EncodeImage::new(24, 16, 3, Samples::U16(&u16s)), &meta).unwrap(),
        encode_tiff(&e, TiffCompression::Lzw, &meta).unwrap(),
        encode_webp_lossless(&e, &meta).unwrap(),
    ]
}

#[test]
fn fuzz_target() {
    let opts = DecodeOptions { max_size: None, max_pixels: 1 << 22, ..Default::default() };
    let exts = ["jpg", "jpeg", "png", "tif", "tiff", "webp", "gif", "bmp", "psd", "jxl", "heic", "heif", "avif", "jp2"];
    lcc_fuzzkit::run_target("codecs", &seeds(), &exts, 300, |bytes| {
        if let Some(f) = sniff(bytes) {
            let _ = decode_unguarded(bytes, f, &opts);
            let _ = read_header_unguarded(bytes, f);
        }
        let _ = read_header(bytes);
        let _ = decode_thumbnail_with(bytes, &ThumbnailOptions { max_pixels: opts.max_pixels, ..ThumbnailOptions::new(16) });
        let _ = icc::parse(bytes);
        let _ = gainmap::read_jpeg(bytes);
    });
}
