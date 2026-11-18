//! Library ▸ Validate DNG Files (`lcc_raw::dngdigest`): a DNG we wrote checks as intact, damage to its raw data is
//! found, and hostile files (truncated, offsets pointing nowhere, garbage) are reported rather than unwound.

use lcc_raw::dngdigest::{DigestKind, DngCheck, check_dng, raw_image_digest};
use lcc_raw::{BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect, write_dng};

fn image(w: usize, h: usize) -> RawImage {
    let data: Vec<u16> = (0..w * h).map(|i| (300 + (i * 37 + i / w * 11) % 3000) as u16).collect();
    RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Cfa::bayer("RGGB"),
        bits: 14,
        black: BlackLevel::uniform(256.0),
        white: vec![16000.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: lcc_meta::Metadata::default(),
        lens_metadata: Default::default(),
    }
}

fn dng(compression: DngCompression) -> Vec<u8> {
    write_dng(&image(64, 48), &DngWriteOptions { compression, ..Default::default() }).unwrap()
}

#[test]
fn a_written_dng_is_intact_in_every_storage() {
    for c in [DngCompression::Uncompressed, DngCompression::Lj92 { tile: 32 }, DngCompression::Deflate { tile: 32, half: false }] {
        // (deflate with integer data writes 16-bit integers)
        let bytes = dng(c);
        assert_eq!(check_dng(&bytes), DngCheck::Intact(DigestKind::RawImage), "{c:?}");
    }
}

#[test]
fn a_flipped_sample_is_damage() {
    let bytes = dng(DngCompression::Uncompressed);
    // the pixel bytes are the bulk of the file: flip one well inside them
    let mut bad = bytes.clone();
    let at = bad.len() / 2;
    bad[at] ^= 0x10;
    assert!(matches!(check_dng(&bad), DngCheck::Damaged(_)), "{:?}", check_dng(&bad));
}

#[test]
fn the_digest_is_of_the_samples_not_the_compressed_bytes() {
    let a = dng(DngCompression::Uncompressed);
    let b = dng(DngCompression::Lj92 { tile: 32 });
    assert_ne!(a.len(), b.len());
    assert_eq!(check_dng(&a), check_dng(&b));
    // 16-bit little-endian samples in row order: the spec's RawImageDigest
    let img = image(2, 2);
    let mut bytes = Vec::new();
    if let RawData::U16(v) = &img.data {
        v.iter().for_each(|s| bytes.extend_from_slice(&s.to_le_bytes()));
    }
    assert_eq!(raw_image_digest(&img.data), lcc_hash::md5_bytes(&bytes));
}

#[test]
fn the_new_digest_is_the_md5_of_the_tiles_md5s_clipped_at_the_edges() {
    let (w, h, cpp) = (5usize, 3usize, 1usize);
    let samples: Vec<u16> = (0..(w * h * cpp) as u16).map(|v| v * 257 + 1).collect();
    let bytes = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| -> Vec<u8> {
        ys.flat_map(|y| xs.clone().map(move |x| (x, y))).flat_map(|(x, y)| samples[y * w + x].to_le_bytes()).collect()
    };
    // 2 x 2 tiles over 5 x 3: three tiles across, two down, the last column and row short
    let mut outer = Vec::new();
    for (ys, xs) in [(0..2, 0..2), (0..2, 2..4), (0..2, 4..5), (2..3, 0..2), (2..3, 2..4), (2..3, 4..5)] {
        outer.extend_from_slice(&lcc_hash::md5_bytes(&bytes(xs, ys)).0);
    }
    assert_eq!(lcc_raw::dngdigest::new_raw_image_digest(&samples, w, h, cpp, 2, 2), Some(lcc_hash::md5_bytes(&outer)));
    // a tile as big as the image is one tile: the digest of its digest
    let whole = lcc_hash::md5_bytes(&bytes(0..5, 0..3));
    assert_eq!(lcc_raw::dngdigest::new_raw_image_digest(&samples, w, h, cpp, 256, 256), Some(lcc_hash::md5_bytes(&whole.0)));
    // sizes that do not fit the samples are refused, not read past
    assert_eq!(lcc_raw::dngdigest::new_raw_image_digest(&samples, w + 1, h, cpp, 2, 2), None);
    assert_eq!(lcc_raw::dngdigest::new_raw_image_digest(&samples, w, h, cpp, 0, 2), None);
    assert_eq!(lcc_raw::dngdigest::new_raw_image_digest(&samples, usize::MAX, usize::MAX, 2, 2, 2), None);
}

#[test]
fn truncated_and_hostile_files_are_reported_never_panic() {
    let whole = dng(DngCompression::Lj92 { tile: 32 });
    // every prefix class: header only, mid-IFD, mid-pixel-data, one byte short
    for cut in [0, 1, 4, 8, 20, 100, whole.len() / 4, whole.len() / 2] {
        let r = check_dng(&whole[..cut.min(whole.len())]);
        assert!(!matches!(r, DngCheck::Intact(_)), "a file cut at {cut} of {} must not pass", whole.len());
    }
    // one byte short may only lose slack, but it must be a verdict either way
    let _ = check_dng(&whole[..whole.len() - 1]);
    // garbage and a non-DNG TIFF
    assert!(matches!(check_dng(b"not a tiff at all"), DngCheck::Damaged(_)));
    assert!(matches!(check_dng(&[]), DngCheck::Damaged(_)));
    // wrong tile offsets are damage not a panic
    {
        let whole = dng(DngCompression::Lj92 { tile: 32 });
        let (at, n) = tile_offsets_at(&whole);
        assert!(n > 1);
        // every tile pointing past the end, then every tile pointing into the file's header, then each one swapped
        for value in [u32::MAX, whole.len() as u32 + 1, 0, 8, (whole.len() / 2) as u32] {
            let mut bad = whole.clone();
            for i in 0..n {
                bad[at + 4 * i..at + 4 * i + 4].copy_from_slice(&value.to_le_bytes());
            }
            assert!(matches!(check_dng(&bad), DngCheck::Damaged(_)), "offsets all {value}: {:?}", check_dng(&bad));
        }
        let mut swapped = whole.clone();
        let (a, b) = (swapped[at..at + 4].to_vec(), swapped[at + 4..at + 8].to_vec());
        swapped[at..at + 4].copy_from_slice(&b);
        swapped[at + 4..at + 8].copy_from_slice(&a);
        assert!(matches!(check_dng(&swapped), DngCheck::Damaged(_)), "two tiles swapped: {:?}", check_dng(&swapped));
    }
}

/// The stored tile offsets, found in the file's bytes.
fn tile_offsets_at(bytes: &[u8]) -> (usize, usize) {
    let tiff = lcc_tiff::Tiff::parse(bytes).unwrap();
    let offsets = tiff.ifds[0].u64s(lcc_tiff::tags::TILE_OFFSETS).unwrap();
    let pattern: Vec<u8> = offsets.iter().flat_map(|o| (*o as u32).to_le_bytes()).collect();
    let at = bytes.windows(pattern.len()).position(|w| w == pattern.as_slice()).unwrap();
    (at, offsets.len())
}

#[test]
fn no_four_bytes_of_the_header_region_make_it_panic() {
    let whole = dng(DngCompression::Lj92 { tile: 32 });
    let mut bad = whole.clone();
    for i in (0..bad.len().min(4096)).step_by(4) {
        let saved = bad[i..i + 4].to_vec();
        bad[i..i + 4].copy_from_slice(&[0xFF; 4]);
        let _ = check_dng(&bad);
        bad[i..i + 4].copy_from_slice(&saved);
    }
}

#[test]
fn a_file_without_a_digest_is_unverifiable_not_damaged() {
    // lossy DNGs are decoded, not stored: the writer leaves the digest out
    let opts = DngWriteOptions { compression: DngCompression::LossyJpeg { tile: 32, quality: 90, long_edge: None }, ..Default::default() };
    let bytes = write_dng(&image(64, 48), &opts).unwrap();
    assert!(matches!(check_dng(&bytes), DngCheck::Unverifiable(_)), "{:?}", check_dng(&bytes));
}
