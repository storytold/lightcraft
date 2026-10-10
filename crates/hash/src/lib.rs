//! Streaming SHA-1 of a file's bytes, for matching originals with remote services that identify
//! assets by it (Immich stores the SHA-1 of every original). SHA-1 is used here as a content
//! identifier agreed with those services, not for security.
//!
//! Made to ride along an existing read pass: wrap the reader the import already uses in a
//! [`HashingReader`] (or feed the chunks it reads to a [`Sha1`]) and take the digest at the end,
//! so a file is read from disk once. [`sha1_reader`] and [`sha1_file`] cover a standalone pass
//! (back-filling old catalogs).
//!
//! Pure computation; builds everywhere, wasm32 included.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::Read;
use std::path::Path;

use sha1::Digest;

/// A SHA-1 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha1Digest(pub [u8; 20]);

impl Sha1Digest {
    /// Lower-case hex (40 characters).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Parse 40 hex characters (any case).
    pub fn from_hex(s: &str) -> Option<Sha1Digest> {
        let s = s.trim();
        if s.len() != 40 {
            return None;
        }
        let mut out = [0u8; 20];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Sha1Digest(out))
    }

    /// Standard base64, the form Immich's API uses for `checksum`.
    pub fn to_base64(&self) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(28);
        for chunk in self.0.chunks(3) {
            let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(A.get(((n >> (18 - 6 * i)) & 63) as usize).copied().unwrap_or(b'=')));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Parse standard base64 (28 characters with padding, or 27 without), or 40 hex characters:
    /// Immich answers base64 and accepts both.
    pub fn parse(s: &str) -> Option<Sha1Digest> {
        let s = s.trim();
        if s.len() == 40 {
            return Sha1Digest::from_hex(s);
        }
        let s = s.trim_end_matches('=');
        if s.len() != 27 {
            return None;
        }
        let val = |c: u8| -> Option<u32> {
            Some(u32::from(match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                _ => return None,
            }))
        };
        let mut bits: u32 = 0;
        let mut nbits = 0u32;
        let mut out = Vec::with_capacity(20);
        for c in s.bytes() {
            bits = (bits << 6) | val(c)?;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                out.push((bits >> nbits) as u8);
                bits &= (1 << nbits) - 1;
            }
        }
        <[u8; 20]>::try_from(out.as_slice()).ok().map(Sha1Digest)
    }
}

impl std::fmt::Display for Sha1Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl std::fmt::Debug for Sha1Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha1Digest({})", self.to_hex())
    }
}

/// An incremental SHA-1.
#[derive(Clone, Default)]
pub struct Sha1(sha1::Sha1);

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> Sha1Digest {
        Sha1Digest(self.0.finalize().into())
    }
}

/// Hashes everything read through it.
pub struct HashingReader<R> {
    inner: R,
    hasher: Sha1,
    bytes: u64,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        HashingReader { inner, hasher: Sha1::new(), bytes: 0 }
    }

    /// Bytes read so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// The digest of everything read so far, and the reader back. Read to the end first for the
    /// whole file's SHA-1.
    pub fn finish(self) -> (R, Sha1Digest) {
        (self.inner, self.hasher.finish())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(buf.get(..n).unwrap_or_default());
        self.bytes = self.bytes.saturating_add(n as u64);
        Ok(n)
    }
}

/// SHA-1 of everything `r` yields.
pub fn sha1_reader(r: impl Read) -> std::io::Result<Sha1Digest> {
    let mut h = HashingReader::new(r);
    std::io::copy(&mut h, &mut std::io::sink())?;
    Ok(h.finish().1)
}

/// SHA-1 of a byte slice.
pub fn sha1_bytes(bytes: &[u8]) -> Sha1Digest {
    let mut h = Sha1::new();
    h.update(bytes);
    h.finish()
}

/// SHA-1 of a file's bytes.
pub fn sha1_file(path: &Path) -> std::io::Result<Sha1Digest> {
    sha1_reader(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_base64_and_hex() {
        let d = sha1_bytes(b"abc");
        assert_eq!(Sha1Digest::parse(&d.to_base64()), Some(d));
        assert_eq!(Sha1Digest::parse(&d.to_hex().to_uppercase()), Some(d));
        assert_eq!(Sha1Digest::parse("qZk+NkcGgWq6PiVxeFDCbJzQ2J0="), Some(d));
        assert_eq!(Sha1Digest::parse("not base64 at all, no!!!!!!"), None);
        assert_eq!(Sha1Digest::parse(""), None);
    }

    #[test]
    fn known_vectors() {
        assert_eq!(sha1_reader(&b""[..]).unwrap().to_hex(), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        let abc = sha1_reader(&b"abc"[..]).unwrap();
        assert_eq!(abc.to_hex(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(abc.to_base64(), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
        assert_eq!(Sha1Digest::from_hex(&abc.to_hex().to_uppercase()), Some(abc));
        assert_eq!(Sha1Digest::from_hex("xyz"), None);
        assert_eq!(Sha1Digest::from_hex(&"é".repeat(20)), None);
    }

    #[test]
    fn hashing_reader_rides_along() {
        let data: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        let mut r = HashingReader::new(&data[..]);
        let mut copy = Vec::new();
        r.read_to_end(&mut copy).unwrap();
        assert_eq!(r.bytes_read(), 100_000);
        let (_, d) = r.finish();
        let mut h = Sha1::new();
        for c in data.chunks(777) {
            h.update(c);
        }
        assert_eq!(h.finish(), d);
        assert_eq!(copy, data);
    }
}
