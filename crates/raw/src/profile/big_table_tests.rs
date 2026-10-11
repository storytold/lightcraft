use super::*;
use miniz_oxide::deflate::compress_to_vec_zlib;

// Authored two-entry identity table, independently hashed with Python's hashlib.
const IDENTITY: &str = "E96CB8AF6484F8402BB0E6ECFC8AD0FC";
const IDENTITY_ENCODED: &str = "M0000KZk$u}gwKGjMAj4lLuC58PZ00Y}2T0";

fn fingerprint(bytes: &[u8]) -> LookFingerprint {
    LookFingerprint(Md5::digest(bytes).into())
}

fn encode85(bytes: &[u8]) -> String {
    let mut text = String::new();
    for chunk in bytes.chunks(4) {
        let mut padded = [0; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        let mut value = u32::from_le_bytes(padded);
        for _ in 0..=chunk.len() {
            text.push(ALPHABET[(value % 85) as usize] as char);
            value /= 85;
        }
    }
    text
}

fn packed(plain: &[u8]) -> Vec<u8> {
    let mut bytes = (plain.len() as u32).to_le_bytes().to_vec();
    bytes.extend(compress_to_vec_zlib(plain, 6));
    bytes
}

fn wire(version: u32, dims: [u32; 3], samples: &[[f32; 3]], encoding: u32, amounts: [f64; 2], flags: Option<u32>) -> Vec<u8> {
    let mut out = Vec::new();
    for word in [0, version, dims[0], dims[1], dims[2]] {
        out.extend(word.to_le_bytes());
    }
    for triple in samples {
        for sample in triple {
            out.extend(sample.to_le_bytes());
        }
    }
    out.extend(encoding.to_le_bytes());
    if version == 2 {
        for amount in amounts {
            out.extend(amount.to_le_bytes());
        }
    }
    if let Some(flags) = flags {
        out.extend(flags.to_le_bytes());
    }
    out
}

fn identity() -> Vec<u8> {
    wire(1, [1, 2, 1], &[[0.0, 1.0, 1.0]; 2], 0, [1.0; 2], None)
}

fn decode(plain: &[u8], expected: LookFingerprint) -> Result<DecodedLookTable, TableError> {
    decode_type0(&encode85(&packed(plain)), expected)
}

#[test]
fn canonical_identity_and_version_flags_are_exact() {
    let expected: LookFingerprint = IDENTITY.parse().unwrap();
    assert_eq!(fingerprint(&identity()), expected);
    assert_eq!(format!("{expected}"), IDENTITY);
    assert_eq!(IDENTITY.to_ascii_lowercase().parse::<LookFingerprint>().unwrap(), expected);
    assert_eq!(format!("{:x}", Md5::digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    let golden = decode_type0(IDENTITY_ENCODED, expected).unwrap();
    for version in [1, 2] {
        for flags in [None, Some(0)] {
            let table = decode(&wire(version, [1, 2, 1], &[[0.0, 1.0, 1.0]; 2], 0, [1.0; 2], flags), expected).unwrap();
            assert_eq!(table, golden);
            assert_eq!(table.hsv().lookup(25.0, 0.5, 0.3), [0.0, 1.0, 1.0]);
        }
    }
    let flags = wire(1, [1, 2, 1], &[[0.0, 1.0, 1.0]; 2], 0, [1.0; 2], Some(1));
    assert!(decode(&flags, fingerprint(&flags)).unwrap().embed_never());
    assert_eq!(decode(&flags, expected), Err(TableError::Mismatch));
    for invalid in ["", "0", "E96CB8AF6484F8402BB0E6ECFC8AD0F_", "é96CB8AF6484F8402BB0E6ECFC8AD0FC"] {
        assert!(invalid.parse::<LookFingerprint>().is_err());
    }
}

#[test]
fn ordering_encoding_amount_and_signed_float_bits_survive() {
    let samples: Vec<[f32; 3]> = (0..8).map(|i| [i as f32 * 10.0, 0.5 + i as f32 / 10.0, 1.0]).collect();
    let bytes = wire(2, [2, 2, 2], &samples, 1, [0.25, 2.0], Some(1));
    let table = decode(&bytes, fingerprint(&bytes)).unwrap();
    assert_eq!(table.hsv().data, samples);
    assert_eq!(table.hsv().lookup(180.0, 0.0, 1.0), samples[6]);
    assert_eq!(table.encoding(), LookEncoding::Srgb);
    assert!(table.hsv().srgb_value);
    assert_eq!(table.amount_range(), [0.25, 2.0]);
    assert!(!format!("{table:?}").contains("data:"));

    let signed = wire(2, [1, 2, 1], &[[-0.0, -0.0, 1.0]; 2], 1, [-0.0, 2.0], None);
    let table = decode(&signed, fingerprint(&signed)).unwrap();
    assert_eq!(table.hsv().data[0][0].to_bits(), (-0.0f32).to_bits());
    assert_eq!(table.hsv().data[0][1].to_bits(), (-0.0f32).to_bits());
    assert_eq!(table.amount_range()[0].to_bits(), (-0.0f64).to_bits());
    assert_eq!(table.encoding(), LookEncoding::Srgb);
    assert!(!table.hsv().srgb_value); // One value slice has no encoded lookup axis.
}

#[test]
fn base85_tails_and_bounds_reject_ambiguous_bytes() {
    for len in 0..64 {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 67 + 11) as u8).collect();
        let encoded = encode85(&bytes);
        assert_eq!(decode85(&format!(" \n{encoded}\t\r")).unwrap(), bytes);
    }
    for bad in ["0", "##", "###", "####", "#####", "_", "&", "é"] {
        assert_eq!(decode85(bad), Err(TableError::Base85));
    }
    assert!(matches!(decode85(&" ".repeat(MAX_ENCODED_BYTES + 1)), Err(TableError::Limit(_))));
    assert!(matches!(decode85(&"00000".repeat(MAX_PACKED_BYTES / 4 + 1)), Err(TableError::Limit(_))));
}

#[test]
fn compressed_length_checksum_and_end_are_required() {
    let plain = identity();
    let expected = fingerprint(&plain);
    let original = packed(&plain);
    for cut in 0..original.len() {
        assert!(decode_type0(&encode85(&original[..cut]), expected).is_err());
    }
    let mut bad = original.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(decode_type0(&encode85(&bad), expected), Err(TableError::Compression));
    for declared in [47u32, 49] {
        let mut bad = original.clone();
        bad[..4].copy_from_slice(&declared.to_le_bytes());
        assert_eq!(decode_type0(&encode85(&bad), expected), Err(TableError::Compression));
    }
    for extra in [vec![0], compress_to_vec_zlib(&plain, 6)] {
        let mut bad = original.clone();
        bad.extend(extra);
        assert_eq!(decode_type0(&encode85(&bad), expected), Err(TableError::Compression));
    }
    assert!(matches!(decode_type0(&encode85(&u32::MAX.to_le_bytes()), expected), Err(TableError::Limit(_))));
    assert_eq!(decode_type0(&encode85(&[2, 0, 0, 0, 1, 0, 0, 0]), expected), Err(TableError::Type(2)));
}

#[test]
fn hostile_headers_samples_and_trailing_fields_are_errors() {
    let good = identity();
    let expected = fingerprint(&good);
    for (offset, word, error) in [(0, 1u32, TableError::Type(1)), (4, 3, TableError::Version(3)), (44, 2, TableError::Encoding(2))] {
        let mut bad = good.clone();
        bad[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        assert_eq!(decode(&bad, expected), Err(error));
    }
    for dims in [[0, 2, 1], [361, 2, 1], [1, 1, 1], [1, 257, 1], [1, 2, 257], [360, 256, 256], [u32::MAX; 3]] {
        assert_eq!(decode(&wire(1, dims, &[], 0, [1.0; 2], None), expected), Err(TableError::Dimensions));
    }
    for bad in [f32::NAN, f32::INFINITY, -1.0, f32::MAX] {
        let mut samples = [[0.0, 1.0, 1.0]; 2];
        samples[0][1] = bad;
        assert_eq!(decode(&wire(1, [1, 2, 1], &samples, 0, [1.0; 2], None), expected), Err(TableError::Sample));
    }
    for amounts in [[f64::NAN, 2.0], [0.0, f64::INFINITY], [-0.1, 2.0], [1.1, 2.0], [0.5, 0.9]] {
        assert_eq!(decode(&wire(2, [1, 2, 1], &[[0.0, 1.0, 1.0]; 2], 0, amounts, None), expected), Err(TableError::Amount));
    }
    assert_eq!(decode(&wire(1, [1, 2, 1], &[[0.0, 1.0, 1.0]; 2], 0, [1.0; 2], Some(2)), expected), Err(TableError::Flags(2)));
    for tail in [vec![0], vec![0; 5], vec![0; 8]] {
        let mut bad = good.clone();
        bad.extend(tail);
        assert_eq!(decode(&bad, expected), Err(TableError::Trailing));
    }
    assert_eq!(decode(&good[..good.len() - 4], expected), Err(TableError::Truncated));
}
