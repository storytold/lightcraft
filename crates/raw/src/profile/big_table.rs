//! Bounded runtime decoder for a user-supplied CRS type-0 HSV BigTable.
//!
//! Original implementation of the public DNG SDK's documented binary layout (BigTable
//! serialization, versions 1/2). No profile data or SDK code is included. This parses the
//! table and verifies its canonical MD5; it does not select a rendering stage or Amount law.

use std::{fmt, str::FromStr};

use md5::{Digest, Md5};
use miniz_oxide::{
    DataFormat, MZFlush, MZStatus,
    inflate::stream::{InflateState, inflate},
};

use super::HsvTable;

pub const MAX_ENCODED_BYTES: usize = 1 << 20;
const MAX_PACKED_BYTES: usize = 1 << 18;
const MAX_ENTRIES: usize = 36 * 32 * 16;
const MAX_PLAIN_BYTES: usize = 20 + 12 * MAX_ENTRIES + 4 + 16 + 4;
const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";

/// A CRS table's format fingerprint, not an authentication claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct LookFingerprint([u8; 16]);

impl FromStr for LookFingerprint {
    type Err = TableError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() != 32 {
            return Err(TableError::Fingerprint);
        }
        let mut bytes = [0; 16];
        for (out, [hi, lo]) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                b'A'..=b'F' => Ok(b - b'A' + 10),
                _ => Err(TableError::Fingerprint),
            };
            *out = digit(*hi)? * 16 + digit(*lo)?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for LookFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookEncoding {
    Linear,
    Srgb,
}

/// Fully checked table. Its samples are immutable through this API and never serialized.
#[derive(Clone, PartialEq)]
pub struct DecodedLookTable {
    fingerprint: LookFingerprint,
    hsv: HsvTable,
    encoding: LookEncoding,
    amount_range: [f64; 2],
    flags: u32,
}

impl fmt::Debug for DecodedLookTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedLookTable")
            .field("fingerprint", &self.fingerprint)
            .field("dimensions", &[self.hsv.hue_divisions, self.hsv.sat_divisions, self.hsv.val_divisions])
            .field("encoding", &self.encoding)
            .field("amount_range", &self.amount_range)
            .field("flags", &self.flags)
            .finish()
    }
}

impl DecodedLookTable {
    pub fn fingerprint(&self) -> LookFingerprint {
        self.fingerprint
    }
    pub fn hsv(&self) -> &HsvTable {
        &self.hsv
    }
    pub fn encoding(&self) -> LookEncoding {
        self.encoding
    }
    /// Declared metadata only; this decoder does not infer a creative-profile Amount law.
    pub fn amount_range(&self) -> [f64; 2] {
        self.amount_range
    }
    pub fn embed_never(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// Bounded reasons only: errors never echo encoded payloads or XML content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TableError {
    #[error("invalid table fingerprint")]
    Fingerprint,
    #[error("table exceeds {0} bound")]
    Limit(&'static str),
    #[error("invalid table Base85 encoding")]
    Base85,
    #[error("invalid or non-exact table zlib stream")]
    Compression,
    #[error("truncated table stream")]
    Truncated,
    #[error("unsupported table type {0}")]
    Type(u32),
    #[error("unsupported HSV table version {0}")]
    Version(u32),
    #[error("unsupported HSV table dimensions")]
    Dimensions,
    #[error("invalid HSV table sample")]
    Sample,
    #[error("unsupported HSV encoding {0}")]
    Encoding(u32),
    #[error("invalid table Amount bounds")]
    Amount,
    #[error("unsupported table flags {0}")]
    Flags(u32),
    #[error("unexpected trailing table bytes")]
    Trailing,
    #[error("table canonical fingerprint mismatch")]
    Mismatch,
}

fn decode85(text: &str) -> Result<Vec<u8>, TableError> {
    if text.len() > MAX_ENCODED_BYTES {
        return Err(TableError::Limit("encoded text"));
    }
    let mut out = Vec::new();
    let (mut value, mut power, mut count) = (0u64, 1u64, 0usize);
    for byte in text.bytes() {
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            continue;
        }
        let digit = ALPHABET.iter().position(|&b| b == byte).ok_or(TableError::Base85)? as u64;
        value += digit * power; // At most five base-85 digits: bounded well below U64.
        count += 1;
        if count == 5 {
            let word = u32::try_from(value).map_err(|_| TableError::Base85)?;
            if out.len() > MAX_PACKED_BYTES - 4 {
                return Err(TableError::Limit("compressed data"));
            }
            out.extend_from_slice(&word.to_le_bytes());
            (value, power, count) = (0, 1, 0);
        } else {
            power *= 85;
        }
    }
    if count == 1 {
        return Err(TableError::Base85);
    }
    if count > 1 {
        let bytes = count - 1;
        if value >= 1u64 << (bytes * 8) {
            return Err(TableError::Base85);
        }
        if out.len() > MAX_PACKED_BYTES - bytes {
            return Err(TableError::Limit("compressed data"));
        }
        out.extend_from_slice(value.to_le_bytes().get(..bytes).ok_or(TableError::Base85)?);
    }
    Ok(out)
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], TableError> {
        let (head, tail) = self.0.split_at_checked(n).ok_or(TableError::Truncated)?;
        self.0 = tail;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32, TableError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| TableError::Truncated)?))
    }
    fn f32(&mut self) -> Result<f32, TableError> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Result<f64, TableError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().map_err(|_| TableError::Truncated)?))
    }
}

/// Decode one type-0 payload and require its canonical digest to equal the requested reference.
pub fn decode_type0(encoded: &str, expected: LookFingerprint) -> Result<DecodedLookTable, TableError> {
    let packed = decode85(encoded)?;
    // Image-table envelopes carry their uncompressed type/version instead of a length.
    if packed.starts_with(&[2, 0, 0, 0, 1, 0, 0, 0]) {
        return Err(TableError::Type(2));
    }
    let mut envelope = Cursor(&packed);
    let declared = usize::try_from(envelope.u32()?).map_err(|_| TableError::Limit("decoded data"))?;
    if !(24..=MAX_PLAIN_BYTES).contains(&declared) {
        return Err(TableError::Limit("decoded data"));
    }
    let mut plain = vec![0; declared + 1];
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let result = inflate(&mut state, envelope.0, &mut plain, MZFlush::Finish);
    if result.status != Ok(MZStatus::StreamEnd) || result.bytes_written != declared || result.bytes_consumed != envelope.0.len() {
        return Err(TableError::Compression);
    }
    plain.truncate(declared);
    parse_type0(&plain, expected)
}

fn parse_type0(plain: &[u8], expected: LookFingerprint) -> Result<DecodedLookTable, TableError> {
    let mut c = Cursor(plain);
    let kind = c.u32()?;
    if kind != 0 {
        return Err(TableError::Type(kind));
    }
    let version = c.u32()?;
    if !matches!(version, 1 | 2) {
        return Err(TableError::Version(version));
    }
    let (h, s, v) = (c.u32()?, c.u32()?, c.u32()?);
    if !(1..=360).contains(&h) || !(2..=256).contains(&s) || !(1..=256).contains(&v) {
        return Err(TableError::Dimensions);
    }
    let count = h
        .checked_mul(s)
        .and_then(|n| n.checked_mul(v))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n <= MAX_ENTRIES)
        .ok_or(TableError::Dimensions)?;
    let sample_bytes = c.take(count.checked_mul(12).ok_or(TableError::Dimensions)?)?;
    let mut sample_cursor = Cursor(sample_bytes);
    let mut data = Vec::with_capacity(count);
    for _ in 0..count {
        let triple = [sample_cursor.f32()?, sample_cursor.f32()?, sample_cursor.f32()?];
        if triple.iter().any(|x| !x.is_finite() || x.abs() > f32::MAX / 4.0) || triple.get(1..).is_none_or(|scales| scales.iter().any(|&x| x < 0.0)) {
            return Err(TableError::Sample);
        }
        data.push(triple);
    }
    let encoding_word = c.u32()?;
    let encoding = match encoding_word {
        0 => LookEncoding::Linear,
        1 => LookEncoding::Srgb,
        other => return Err(TableError::Encoding(other)),
    };
    let amount_range = if version == 2 { [c.f64()?, c.f64()?] } else { [1.0, 1.0] };
    let [min, max] = amount_range;
    if !min.is_finite() || !max.is_finite() || !(0.0..=1.0).contains(&min) || max < 1.0 {
        return Err(TableError::Amount);
    }
    let flags = match c.0.len() {
        0 => 0,
        4 => c.u32()?,
        _ => return Err(TableError::Trailing),
    };
    if flags & !1 != 0 {
        return Err(TableError::Flags(flags));
    }
    let canonical_version = if amount_range == [1.0, 1.0] { 1u32 } else { 2u32 };
    let mut digest = Md5::new();
    for word in [0, canonical_version, h, s, v] {
        digest.update(word.to_le_bytes());
    }
    digest.update(sample_bytes);
    digest.update(encoding_word.to_le_bytes());
    if canonical_version == 2 {
        for amount in amount_range {
            digest.update(amount.to_le_bytes());
        }
    }
    if flags != 0 {
        digest.update(flags.to_le_bytes());
    }
    let fingerprint = LookFingerprint(digest.finalize().into());
    if fingerprint != expected {
        return Err(TableError::Mismatch);
    }
    let hsv = HsvTable {
        hue_divisions: h as usize,
        sat_divisions: s as usize,
        val_divisions: v as usize,
        data,
        srgb_value: encoding == LookEncoding::Srgb && v > 1,
    };
    Ok(DecodedLookTable { fingerprint, hsv, encoding, amount_range, flags })
}

#[cfg(test)]
#[path = "big_table_tests.rs"]
mod tests;
