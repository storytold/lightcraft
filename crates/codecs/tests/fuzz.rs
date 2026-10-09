//! Decoders must never panic on malformed input: random bytes behind every magic number, and
//! mutated/truncated valid files. Uses `decode_unguarded` so panics in our own code are not masked
//! by the public entry point's panic guard.

use dac_codecs::exif::minimal_exif;
use dac_codecs::icc::write_named;
use dac_codecs::*;
use dac_raster::Rgba8;
use proptest::prelude::*;
use std::sync::OnceLock;

const MAGICS: &[&[u8]] = &[
    b"\xFF\xD8\xFF",
    b"\x89PNG\r\n\x1a\n",
    b"II*\0",
    b"MM\0*",
    b"RIFF\0\0\0\0WEBP",
    b"GIF89a",
    b"BM\0\0\0\0\0\0\0\0\0\0\0\0\x28\0\0\0",
    b"8BPS\0\x01",
    b"\xFF\x0A",
    b"\0\0\0\x0CJXL \x0D\x0A\x87\x0A",
];

fn small_opts() -> DecodeOptions {
    DecodeOptions { max_size: None, max_pixels: 1 << 22 }
}

fn try_all(bytes: &[u8]) {
    if let Some(f) = sniff(bytes) {
        let _ = decode_unguarded(bytes, f, &small_opts());
        let _ = decode_unguarded(bytes, f, &DecodeOptions { max_size: Some((8, 8)), max_pixels: 1 << 22 });
        let _ = read_header_unguarded(bytes, f);
    }
    let _ = decode(bytes, small_opts());
    let _ = read_header(bytes);
    let _ = decode_thumbnail_with(bytes, &ThumbnailOptions { max_pixels: small_opts().max_pixels, ..ThumbnailOptions::new(16) });
    let _ = dac_codecs::icc::parse(bytes);
}

#[test]
fn webp_truncated_metadata_cannot_allocate_declared_chunk_size() {
    let mut bytes = seeds()[5].clone();
    let tag = bytes.windows(4).rposition(|w| w == b"XMP ").unwrap();
    bytes[tag + 4..tag + 8].copy_from_slice(&1_946_157_060u32.to_le_bytes());
    let decoded = decode(&bytes, small_opts()).unwrap();
    assert_eq!((decoded.image.width, decoded.image.height), (24, 16));
    assert!(decoded.xmp.is_none(), "invalid optional metadata is ignored");
}

#[test]
fn thumbnail_rejects_huge_gif_before_allocating_source_pixels() {
    // A logical screen header without a global palette; previously this reached a
    // multi-gigabyte output allocation before discovering that no frame follows.
    let bytes = [
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x8f, 0xa4, 0xb0, 0x3f, 0x57, 0x14, 0xe8, 0x8a, 0xf8, 0xe0, 0xc7, 0x41, 0x9d, 0x0c, 0x10, 0xde, 0x54,
        0x24, 0xcb, 0xed, 0xff, 0x4a, 0xe7, 0xa0, 0x82, 0x10, 0xa0, 0xbc, 0xac, 0xe5, 0x33, 0x3f, 0xc9, 0x8d, 0x69, 0xf4, 0x24, 0xce, 0x99, 0x4f,
        0x0d, 0x47, 0xa0, 0xb0, 0xf4, 0xa8, 0x59, 0xad, 0x29, 0xbd, 0x09, 0x48, 0x63, 0xea, 0x59, 0xb7, 0xb7, 0xf2, 0xa1, 0x90, 0xeb, 0x92, 0xc5,
        0x9c, 0xde, 0xcc, 0x96, 0x67, 0xc4, 0xe5, 0x8c, 0x03, 0x50, 0xb2, 0x30, 0xf8, 0xf4, 0x69, 0x1d, 0xeb, 0xb9, 0xbd, 0x4e, 0xfb, 0xbd, 0xb6,
        0x02,
    ];
    let result = decode_thumbnail(&bytes, 16);
    assert!(matches!(result, Err(Error::TooLarge(42_127, 16_304))), "{result:?}");
}

#[test]
fn thumbnail_respects_configured_source_pixel_limit() {
    let image = Rgba8::from_fn(3, 2, |_, _| [40, 80, 120, 255]);
    let bytes = encode_png(&EncodeImage::rgba8(&image), &EncodeMeta::default()).unwrap();
    let mut opts = ThumbnailOptions::new(1);
    opts.max_pixels = 5;
    assert!(matches!(decode_thumbnail_with(&bytes, &opts), Err(Error::TooLarge(3, 2))));
    opts.max_pixels = 6;
    assert_eq!(decode_thumbnail_with(&bytes, &opts).unwrap().image.width, 1);
}

fn seeds() -> &'static Vec<Vec<u8>> {
    static S: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    S.get_or_init(|| {
        let img = Rgba8::from_fn(24, 16, |x, y| [(x * 10) as u8, (y * 15) as u8, ((x ^ y) * 8) as u8, (x * y) as u8]);
        let icc = write_named(NamedSpace::DisplayP3);
        let exif = minimal_exif(6);
        let meta = EncodeMeta { icc: Some(&icc), exif: Some(&exif), xmp: Some("<x/>"), ..Default::default() };
        let e = EncodeImage::rgba8(&img);
        let u16s: Vec<u16> = (0..24 * 16 * 3).map(|i| (i * 331) as u16).collect();
        let f32s: Vec<f32> = (0..24 * 16 * 3).map(|i| i as f32 * 0.01).collect();
        vec![
            encode_jpeg(&e, 80, ChromaSubsampling::S420, &meta).unwrap(),
            encode_png(&e, &meta).unwrap(),
            encode_png(&EncodeImage::new(24, 16, 3, Samples::U16(&u16s)), &meta).unwrap(),
            encode_tiff(&e, TiffCompression::Lzw, &meta).unwrap(),
            encode_tiff(&EncodeImage::new(24, 16, 3, Samples::F32(&f32s)), TiffCompression::Deflate, &meta).unwrap(),
            encode_webp_lossless(&e, &meta).unwrap(),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn random_after_magic(m in 0usize..MAGICS.len(), tail in proptest::collection::vec(any::<u8>(), 0..600)) {
        let mut b = MAGICS[m].to_vec();
        b.extend_from_slice(&tail);
        try_all(&b);
    }

    #[test]
    fn mutated_valid_files(which in 0usize..6, flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..12), cut in any::<prop::sample::Index>()) {
        let mut b = seeds()[which].clone();
        for (i, v) in flips {
            let i = i.index(b.len());
            b[i] = v;
        }
        try_all(&b);
        let n = cut.index(b.len());
        try_all(&b[..n]);
    }
}
