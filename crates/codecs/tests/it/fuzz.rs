//! Decoders must never panic on malformed input: random bytes behind every magic number, and
//! mutated/truncated valid files. Uses `decode_unguarded` so panics in our own code are not masked
//! by the public entry point's panic guard.

use lightcraft_codecs::exif::minimal_exif;
use lightcraft_codecs::icc::write_named;
use lightcraft_codecs::*;
use lightcraft_raster::Rgba8;
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
    let _ = lightcraft_codecs::icc::parse(bytes);
    let _ = lightcraft_codecs::gainmap::read_jpeg(bytes);
    let _ = lightcraft_codecs::gainmap::is_gain_map_jpeg(bytes);
}

fn gain_map_jpeg(meta: &EncodeMeta) -> Vec<u8> {
    use lightcraft_codecs::gainmap::{GainMapOptions, compute, encode_jpeg};
    let hdr: Vec<[f32; 3]> = (0..24 * 16).map(|i| [i as f32 * 0.02; 3]).collect();
    let sdr: Vec<[f32; 3]> = hdr.iter().map(|p| p.map(|v| v / (1.0 + v))).collect();
    let (g, m) = compute(&sdr, &hdr, 24, 16, [0.2126, 0.7152, 0.0722], &GainMapOptions::default()).unwrap();
    let base: Vec<u8> = sdr.iter().flat_map(|p| p.map(|v| (v * 255.0) as u8)).collect();
    encode_jpeg(&EncodeImage::new(24, 16, 3, Samples::U8(&base)), &g, &m, 80, ChromaSubsampling::S420, meta).unwrap()
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

/// A GIF of a few dozen bytes: a `w` × `h` logical screen and one empty frame of that size.
fn tiny_gif(w: u16, h: u16) -> Vec<u8> {
    let mut b = b"GIF89a".to_vec();
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend([0, 0, 0, b',', 0, 0, 0, 0]);
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend([0, 2, 0, b';']);
    b
}

/// A 54-byte uncompressed 24-bit BMP header declaring `w` × `h` and no pixel data.
fn tiny_bmp(w: i32, h: i32) -> Vec<u8> {
    let mut b = b"BM".to_vec();
    b.extend(54u32.to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    b
}

/// Issue #2553: the import probe had no header reader for GIF or BMP and decoded them in full, so
/// a tiny file declaring a canvas just under the 1-gigapixel limit allocated ~4.3 GB (an abort, not
/// an error, when it failed). Both the probe and a decode now refuse a canvas the file's bytes
/// can't fill, before the decoder allocates it.
#[test]
fn tiny_gif_and_bmp_declaring_a_gigapixel_canvas_are_refused_before_allocating() {
    for (name, bytes) in [("gif", tiny_gif(32_767, 32_767)), ("bmp", tiny_bmp(32_000, 32_000))] {
        assert!(bytes.len() < 64, "{name}: {} bytes", bytes.len());
        let header = read_header(&bytes);
        assert!(matches!(&header, Err(Error::Malformed(_, m)) if m.contains("can hold")), "{name}: {header:?}");
        let decoded = decode(&bytes, DecodeOptions::default());
        assert!(matches!(&decoded, Err(Error::Malformed(_, m)) if m.contains("can hold")), "{name}: {:?}", decoded.err());
    }
}

/// Above 4 MP the probe reads GIF and BMP sizes from the header without decoding; the size agrees
/// with a full decode.
#[test]
fn large_gif_and_bmp_sizes_come_from_the_header() {
    let (w, h) = (2_100u32, 2_000u32);
    let rgb = vec![90u8; (w * h * 3) as usize];
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp).encode(&rgb, w, h, image::ExtendedColorType::Rgb8).unwrap();
    let mut gif = Vec::new();
    let frame = image::RgbaImage::from_pixel(w, h, image::Rgba([90, 90, 90, 255]));
    image::codecs::gif::GifEncoder::new(&mut gif).encode_frame(image::Frame::new(frame)).unwrap();
    for (name, bytes) in [("bmp", bmp), ("gif", gif)] {
        let header = read_header(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((header.width, header.height, header.orientation), (w, h, 1), "{name}");
        let d = decode(&bytes, DecodeOptions::fit(64, 64)).unwrap();
        assert_eq!((d.source_width, d.source_height), (w, h), "{name}");
    }
}

/// A 64-byte RLE8 BMP declaring `w` × `h` whose pixels are one end-of-bitmap code: all blank.
fn rle_bmp_ending_at_once(w: i32, h: i32) -> Vec<u8> {
    let mut b = b"BM".to_vec();
    b.extend(64u32.to_le_bytes()); // file size
    b.extend(0u32.to_le_bytes());
    b.extend(62u32.to_le_bytes()); // pixel data offset: 14 + 40 + 2 palette entries
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(8u16.to_le_bytes());
    b.extend(1u32.to_le_bytes()); // BI_RLE8
    b.extend(2u32.to_le_bytes()); // pixel data size
    b.extend([0u8; 8]);
    b.extend(2u32.to_le_bytes()); // colours used
    b.extend(0u32.to_le_bytes());
    b.extend([0, 0, 0, 0, 255, 255, 255, 0]);
    b.extend([0, 1]); // end of bitmap
    assert_eq!(b.len(), 64);
    b
}

/// An RLE8 BMP may end its pixels with an end-of-bitmap code that leaves the rest blank, so a file
/// of a few dozen bytes is a valid picture: up to 4 MP it opens, at its declared size.
#[test]
fn an_rle_bmp_that_ends_early_still_opens() {
    let (w, h) = (2_000i32, 2_000i32);
    let b = rle_bmp_ending_at_once(w, h);
    let header = read_header(&b).unwrap();
    assert_eq!((header.width, header.height), (w as u32, h as u32));
    let d = decode(&b, DecodeOptions::fit(64, 64)).unwrap();
    assert_eq!((d.source_width, d.source_height), (w as u32, h as u32));
}

/// A mostly blank canvas still costs its full size to decode, so a tiny RLE BMP can't claim a
/// gigapixel one: canvases the file's runs can't cover are limited to 4 MP, refused before the
/// decoder allocates them.
#[test]
fn a_tiny_rle_bmp_declaring_a_gigapixel_canvas_is_refused_before_allocating() {
    for (w, h) in [(2_001, 2_000), (32_000, 32_000)] {
        let b = rle_bmp_ending_at_once(w, h);
        let header = read_header(&b);
        assert!(matches!(&header, Err(Error::Malformed(_, m)) if m.contains("mostly blank")), "{w} × {h}: {header:?}");
        let decoded = decode(&b, DecodeOptions::default());
        assert!(matches!(&decoded, Err(Error::Malformed(_, m)) if m.contains("mostly blank")), "{w} × {h}: {:?}", decoded.err());
    }
}

/// A `sw` × `sh` GIF screen whose first frame is one pixel (black, from a 2-colour palette).
fn gif_with_one_pixel_frame(sw: u16, sh: u16) -> Vec<u8> {
    let mut b = b"GIF89a".to_vec();
    b.extend(sw.to_le_bytes());
    b.extend(sh.to_le_bytes());
    b.extend([0x80, 0, 0]); // global colour table of 2 entries
    b.extend([0, 0, 0, 255, 255, 255]);
    b.extend([b',', 0, 0, 0, 0, 1, 0, 1, 0, 0]); // a 1 × 1 frame at the origin
    b.extend([2, 2, 0x44, 0x01, 0]); // LZW: clear, index 0, end
    b.push(b';');
    b
}

/// Density is judged on the first frame's own size, not the screen's: a valid GIF with a small first
/// frame on a larger screen opens, at the screen's size (up to 4 MP, as for other mostly blank
/// canvases).
#[test]
fn a_gif_with_a_small_first_frame_on_a_larger_screen_opens() {
    let b = gif_with_one_pixel_frame(2_000, 2_000);
    let header = read_header(&b).unwrap();
    assert_eq!((header.width, header.height), (2_000, 2_000));
    let d = decode(&b, DecodeOptions::fit(64, 64)).unwrap();
    assert_eq!((d.source_width, d.source_height), (2_000, 2_000));
}

#[test]
fn a_gif_whose_one_pixel_frame_sits_on_a_gigapixel_screen_is_refused_before_allocating() {
    let b = gif_with_one_pixel_frame(32_767, 32_767);
    let header = read_header(&b);
    assert!(matches!(&header, Err(Error::Malformed(_, m)) if m.contains("mostly blank")), "{header:?}");
    let decoded = decode(&b, DecodeOptions::default());
    assert!(matches!(&decoded, Err(Error::Malformed(_, m)) if m.contains("mostly blank")), "{:?}", decoded.err());
}

/// A GIF's comments and other extensions describe no pixels, so padding a tiny GIF with 100 KB of
/// comment doesn't let it claim a gigapixel canvas: only the frame's LZW data counts.
#[test]
fn a_gif_padded_with_comments_still_cannot_claim_a_gigapixel_canvas() {
    let mut bytes = tiny_gif(32_767, 32_767);
    let mut comment = vec![0x21, 0xFE];
    for _ in 0..400 {
        comment.push(255);
        comment.extend([b'x'; 255]);
    }
    comment.push(0);
    bytes.splice(13..13, comment);
    assert!(bytes.len() > 100_000);
    let header = read_header(&bytes);
    assert!(matches!(&header, Err(Error::Malformed(_, m)) if m.contains("can hold")), "{header:?}");
    let decoded = decode(&bytes, DecodeOptions::default());
    assert!(matches!(&decoded, Err(Error::Malformed(_, m)) if m.contains("can hold")), "{:?}", decoded.err());
}

/// Padding is not pixel data: coverage comes from what the pixel data draws, not from the file's
/// length. Each file here is padded past what the old length-based bounds (8 pixels per byte for
/// uncompressed BMP, 128 for RLE, 2731 per LZW byte for a GIF frame) needed to accept its 9 MP
/// canvas, and each draws far less, so each is refused before the decoder allocates the canvas.
#[test]
fn files_padded_with_junk_cannot_claim_a_canvas_their_pixel_data_doesnt_draw() {
    let refused = |name: &str, bytes: &[u8], why: &str| {
        let header = read_header(bytes);
        assert!(matches!(&header, Err(Error::Malformed(_, m)) if m.contains(why)), "{name}: {header:?}");
        let decoded = decode(bytes, DecodeOptions::default());
        assert!(matches!(&decoded, Err(Error::Malformed(_, m)) if m.contains(why)), "{name}: {:?}", decoded.err());
    };
    // a 24-bit BMP needs 3 bytes a pixel (27 MB here); 1.2 MB of zeros used to pass as 8 pixels a byte
    let mut bmp = tiny_bmp(3_000, 3_000);
    bmp.resize(bmp.len() + 1_200_000, 0);
    refused("uncompressed bmp", &bmp, "can hold");
    // an RLE BMP's picture ends at its end-of-bitmap code; 100 KB of runs after it draw nothing
    let mut rle = rle_bmp_ending_at_once(3_000, 3_000);
    rle.extend([255, 1].repeat(50_000));
    refused("rle bmp padded after its end", &rle, "mostly blank");
    // runs that never end their row are clipped to it: 100 KB of them draw one 3000-pixel row
    let mut rle = rle_bmp_ending_at_once(3_000, 3_000);
    rle.truncate(rle.len() - 2);
    rle.extend([255, 1].repeat(50_000));
    refused("rle bmp padded within one row", &rle, "mostly blank");
    // a GIF frame's picture ends at its LZW end code; 4 KB of data sub-blocks after it draw nothing
    let mut gif = b"GIF89a".to_vec();
    gif.extend(3_000u16.to_le_bytes());
    gif.extend(3_000u16.to_le_bytes());
    gif.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]); // 2-colour global table
    gif.extend([b',', 0, 0, 0, 0]);
    gif.extend(3_000u16.to_le_bytes());
    gif.extend(3_000u16.to_le_bytes());
    gif.extend([0, 2]); // no local table; LZW minimum code size 2
    gif.extend([255, 0x2C]); // a full sub-block: 3-bit clear (4) then end (5), then padding
    gif.extend([0xFF; 254]);
    for _ in 0..15 {
        gif.push(255);
        gif.extend([0xFF; 255]);
    }
    gif.extend([0, b';']);
    refused("gif padded after its end code", &gif, "mostly blank");
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
            gain_map_jpeg(&meta),
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
    fn mutated_valid_files(which in 0usize..7, flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..12), cut in any::<prop::sample::Index>()) {
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
