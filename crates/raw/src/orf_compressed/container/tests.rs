use super::*;
use craft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter};

/// Exif `CFAPattern` of an RGGB sensor.
const RGGB: &[u8] = &[0, 2, 0, 2, 0, 1, 1, 2];

/// A minimal ORF with one strip in IFD0, an Exif `CFAPattern` and, optionally, a maker note blob.
fn orf_with_note(w: u32, h: u32, bits: u16, strip: Vec<u8>, cfa_pattern: Option<&[u8]>, note: Option<Vec<u8>>) -> Vec<u8> {
    let mut ifd = IfdBuilder::new();
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits]));
    ifd.set(t::COMPRESSION, Value::Short(vec![1]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![1]));
    ifd.set(t::MAKE, Value::Ascii("OLYMPUS IMAGING CORP.".into()));
    ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
    if cfa_pattern.is_some() || note.is_some() {
        let mut exif = IfdBuilder::new();
        if let Some(p) = cfa_pattern {
            exif.set(EXIF_CFA_PATTERN, Value::Undefined(p.to_vec()));
        }
        if let Some(n) = note {
            exif.set(t::MAKER_NOTE, Value::Undefined(n));
        }
        ifd.set_child(t::EXIF_IFD, exif);
    }
    let mut b = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
    b[2] = b'R';
    b[3] = b'O';
    b
}

/// An old-style Olympus maker note (`OLYMP\0`, offsets relative to the file) at file offset `at`, with `entries`
/// of (tag, TIFF type, value bytes).
fn olymp_note(at: usize, entries: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let ifd_end = 8 + 2 + 12 * entries.len() + 4;
    let (mut b, mut extra) = (b"OLYMP\0\x01\0".to_vec(), Vec::new());
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, value) in entries {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&typ.to_le_bytes());
        b.extend_from_slice(&(if *typ == 3 { value.len() / 2 } else { value.len() } as u32).to_le_bytes());
        if value.len() <= 4 {
            let mut v = value.clone();
            v.resize(4, 0);
            b.extend_from_slice(&v);
        } else {
            b.extend_from_slice(&((at + ifd_end + extra.len()) as u32).to_le_bytes());
            extra.extend_from_slice(value);
        }
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&extra);
    b
}

/// SHORT values, little-endian.
fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// An inline ImageProcessing sub-directory of SHORT entries (tag, value).
fn image_processing(entries: &[(u16, u16)]) -> (u16, u16, Vec<u8>) {
    let mut ifd = (entries.len() as u16).to_le_bytes().to_vec();
    for &(tag, value) in entries {
        ifd.extend_from_slice(&tag.to_le_bytes());
        ifd.extend_from_slice(&[3, 0, 1, 0, 0, 0]);
        ifd.extend_from_slice(&value.to_le_bytes());
        ifd.extend_from_slice(&[0, 0]);
    }
    ifd.extend_from_slice(&0u32.to_le_bytes());
    (IMAGE_PROCESSING, 7, ifd)
}

/// A 4 × 2 ORF holding `strip`, its maker note made of `entries`, placed where the note's own offsets point.
fn compressed_orf(strip: &[u8], entries: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let file = |at| orf_with_note(4, 2, 16, strip.to_vec(), Some(RGGB), Some(olymp_note(at, entries)));
    let at = file(0).windows(6).position(|b| b == b"OLYMP\0").unwrap();
    let bytes = file(at);
    assert_eq!(bytes.windows(6).position(|b| b == b"OLYMP\0"), Some(at));
    bytes
}

/// Stream parameters: a 4-byte header prefix (tag `0x0653`), plus `extra` entries.
fn params(extra: &[(u16, u16)]) -> (u16, u16, Vec<u8>) {
    image_processing(&[&[(0x0653, 4)], extra].concat())
}

#[test]
fn compressed_orf_synthetic_fixture_decodes() {
    // a 4-byte prefix equal to 1 and [0, 0], then 8 symbols of 0x10 (sign 0, sub 0, q 0, rem 0 with k = 4), padding
    let strip = [0, 0, 0, 1, 0, 0, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0, 0];
    let bytes = compressed_orf(&strip, &[params(&[]), (MN_VALID_BITS, 3, shorts(&[12]))]);
    let raw = crate::decode(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits), (4, 2, 12));
    assert_eq!(raw.data, RawData::U16(vec![0; 8]));
    assert_eq!(raw.cfa.as_ref().map(Cfa::name).as_deref(), Some("RGGB"));
    assert_eq!(crate::probe_info(&bytes).unwrap(), raw.info());
}

#[test]
fn compressed_orf_synthetic_negative_residual_and_unary_escape() {
    // 1. negative residual: symbol 0 is 0x91 (sign 1, sub 0, q 0, rem 1: delta -2, adjusted -8); with a boundary
    // baseline of 100 (tag 0x0642), sample 0 reconstructs to 100 - 8 = 92
    let strip_neg = [0, 0, 0, 1, 0, 0, 0x91, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0, 0];
    let raw = crate::decode(&compressed_orf(&strip_neg, &[params(&[(0x0642, 100)]), (MN_VALID_BITS, 3, shorts(&[12]))])).unwrap();
    assert_eq!(raw.data, RawData::U16(vec![92, 100, 88, 100, 100, 100, 100, 100]));

    // 2. unary escape: 12 zero bits, an 11-bit escape of 1, the boundary bit and a 4-bit remainder of 0: delta 16,
    // adjusted 64; with the baseline of 100, sample 0 reconstructs to 164
    let strip_esc = [0, 0, 0, 1, 0, 0, 0, 0, 0, 0x60, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0];
    let raw = crate::decode(&compressed_orf(&strip_esc, &[params(&[(0x0642, 100)]), (MN_VALID_BITS, 3, shorts(&[12]))])).unwrap();
    assert_eq!(raw.data, RawData::U16(vec![164, 164, 168, 168, 100, 100, 100, 100]));

    // 3. without ValidBits the depth comes from the samples, the same in a header probe as in a full decode: a
    // baseline of 5000 puts them above 4096, so 14 bits
    let bytes = compressed_orf(&strip_neg, &[params(&[(0x0642, 5000)])]);
    assert_eq!(crate::decode(&bytes).unwrap().bits, 14);
    assert_eq!(crate::probe_info(&bytes).unwrap().bits, 14);
    // a baseline of 4100: only sample 0 (4092) is below 4096, and it is the one a sparse scan would read
    let bytes = compressed_orf(&strip_neg, &[params(&[(0x0642, 4100)])]);
    let raw = crate::decode(&bytes).unwrap();
    assert_eq!(raw.data, RawData::U16(vec![4092, 4100, 4088, 4100, 4100, 4100, 4100, 4100]));
    assert_eq!((raw.bits, crate::probe_info(&bytes).unwrap().bits), (14, 14));
}

/// The white level sees every CFA site: a 2000 × 2000 mosaic whose odd columns alone saturate (at 4095) and whose
/// even columns sit at 3000 has its plateau at the top, not at 3000.
#[test]
fn white_level_samples_every_cfa_site() {
    let (w, h) = (2000usize, 2000usize);
    let data: Vec<u16> = (0..w * h).map(|i| if i % w % 2 == 1 { 4095 } else { 3000 }).collect();
    let (white, source) = white_from_data(&data, 12);
    assert!(white > 4000.0, "white {white}");
    assert_eq!(source, LevelSource::Inferred);
}

#[test]
fn damaged_compressed_orf_returns_corrupt_error_instead_of_word16() {
    // 16 bytes for 8 pixels would pass for 16-bit words, but the stated stream's prefix is 2, not 1
    let strip = [0, 0, 0, 2, 0, 0, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0, 0];
    let bytes = compressed_orf(&strip, &[params(&[]), (MN_VALID_BITS, 3, shorts(&[12]))]);
    assert!(matches!(crate::decode(&bytes), Err(RawError::Corrupt(_))));
    assert!(matches!(crate::probe_info(&bytes), Err(RawError::Corrupt(_))));
}

/// Stated stream parameters out of range are an error, not a reason to hand the strip to craft-raw, which would read
/// these 16 bytes as eight 16-bit words.
#[test]
fn out_of_range_stream_parameters_are_corrupt_not_uncompressed() {
    let strip = [0, 0, 0, 1, 0, 0, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0, 0];
    for bad in [(0x0653, 9), (0x0651, 99), (0x0644, 0), (0x0643, 7), (0x0646, 40)] {
        let ip = if bad.0 == 0x0653 { image_processing(&[bad]) } else { params(&[bad]) };
        let bytes = compressed_orf(&strip, &[ip, (MN_VALID_BITS, 3, shorts(&[12]))]);
        assert!(matches!(decode(&bytes, Mode::Full), Err(RawError::Corrupt(_))), "{bad:?}");
        assert!(matches!(crate::decode(&bytes), Err(RawError::Corrupt(_))), "{bad:?}");
        assert!(matches!(crate::probe_info(&bytes), Err(RawError::Corrupt(_))), "{bad:?}");
    }
}

/// An uncompressed file may state every stream parameter as 0 (the CC0 E-M5 Mark II sample does): a header length of
/// 0 is no compressed stream, so craft-raw reads the file.
#[test]
fn all_zero_stream_parameters_are_left_to_craft_raw() {
    let words: Vec<u8> = (0..8u16).flat_map(|i| (200 + i * 37).to_le_bytes()).collect();
    let zeros: Vec<(u16, u16)> = (0x0640..=0x0653).map(|tag| (tag, 0)).collect();
    let bytes = compressed_orf(&words, &[image_processing(&zeros)]);
    assert!(matches!(decode(&bytes, Mode::Full), Ok(None)));
    assert_eq!(crate::decode(&bytes).unwrap().data, RawData::U16((0..8u16).map(|i| 200 + i * 37).collect()));
}

/// A file without stream parameters is not recognised here: craft-raw reads it as before.
#[test]
fn uncompressed_orf_is_left_to_craft_raw() {
    let words: Vec<u8> = (0..8u16).flat_map(|i| (200 + i * 37).to_le_bytes()).collect();
    let bytes = compressed_orf(&words, &[image_processing(&[(0x0600, 0)]), (MN_VALID_BITS, 3, shorts(&[12]))]);
    assert!(matches!(decode(&bytes, Mode::Full), Ok(None)));
    let raw = crate::decode(&bytes).unwrap();
    assert_eq!(raw.data, RawData::U16((0..8u16).map(|i| 200 + i * 37).collect()));
}

/// Hostile stream parameters and truncated files are errors or left to craft-raw, never a panic.
#[test]
fn hostile_compressed_orfs_never_panic() {
    let strip = [0, 0, 0, 1, 0, 0, 0xff, 0x00, 0xaa, 0x55, 0x10, 0x10, 0x10, 0x10, 0, 0];
    for extra in [vec![(0x0651, 99)], vec![(0x0644, 0)], vec![(0x0643, 7)], vec![(0x0642, 0xffff)], vec![(0x0646, 40)]] {
        let bytes = compressed_orf(&strip, &[params(&extra), (MN_VALID_BITS, 3, shorts(&[12]))]);
        let _ = crate::decode(&bytes);
        let _ = crate::probe_info(&bytes);
    }
    let bytes = compressed_orf(&strip, &[params(&[]), (MN_VALID_BITS, 3, shorts(&[12]))]);
    for cut in 0..bytes.len() {
        let _ = crate::decode(&bytes[..cut]);
        let _ = crate::probe_info(&bytes[..cut]);
    }
}
