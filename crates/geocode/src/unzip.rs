//! Just enough ZIP to read one file out of a GeoNames dump (`cities15000.zip`): the central
//! directory, stored or deflated entries, sizes capped.
//!
//! Sources: PKWARE APPNOTE (prose spec); own design.

use crate::GeocodeError;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    let s = b.get(i..i.checked_add(2)?)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i.checked_add(4)?)?;
    Some(u32::from_le_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
}

fn bad(what: &str) -> GeocodeError {
    GeocodeError::Invalid(format!("not a readable ZIP archive ({what})"))
}

/// The contents of the entry whose name ends with `name`, at most `max` bytes.
pub fn extract(zip: &[u8], name: &str, max: usize) -> Result<Vec<u8>, GeocodeError> {
    // end of central directory: the last "PK\x05\x06" within the final 64 KiB + 22 bytes
    let from = zip.len().saturating_sub(65_557);
    let eocd = (from..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip.get(i..i.saturating_add(4)) == Some(b"PK\x05\x06"))
        .ok_or_else(|| bad("no directory"))?;
    let field16 = |at: usize, what: &str| u16_at(zip, at).map(usize::from).ok_or_else(|| bad(what));
    let field32 = |at: usize, what: &str| u32_at(zip, at).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| bad(what));
    let count = field16(eocd.saturating_add(10), "directory")?;
    let mut p = field32(eocd.saturating_add(16), "directory")?;
    for _ in 0..count {
        if zip.get(p..p.saturating_add(4)) != Some(b"PK\x01\x02") {
            return Err(bad("directory entry"));
        }
        let method = field16(p.saturating_add(10), "entry")?;
        let csize = field32(p.saturating_add(20), "entry")?;
        let usize_ = field32(p.saturating_add(24), "entry")?;
        let nlen = field16(p.saturating_add(28), "entry")?;
        let xlen = field16(p.saturating_add(30), "entry")?;
        let clen = field16(p.saturating_add(32), "entry")?;
        let local = field32(p.saturating_add(42), "entry")?;
        let name_at = p.saturating_add(46);
        let entry = zip.get(name_at..name_at.saturating_add(nlen)).ok_or_else(|| bad("entry name"))?;
        p = name_at.saturating_add(nlen).saturating_add(xlen).saturating_add(clen);
        if !String::from_utf8_lossy(entry).ends_with(name) {
            continue;
        }
        if usize_ > max {
            return Err(GeocodeError::Invalid(format!("{name} is larger than {max} bytes")));
        }
        if zip.get(local..local.saturating_add(4)) != Some(b"PK\x03\x04") {
            return Err(bad("local header"));
        }
        let lnlen = field16(local.saturating_add(26), "local header")?;
        let lxlen = field16(local.saturating_add(28), "local header")?;
        let start = local.saturating_add(30).saturating_add(lnlen).saturating_add(lxlen);
        let data = zip.get(start..start.saturating_add(csize)).ok_or_else(|| bad("truncated"))?;
        return match method {
            0 if data.len() <= max => Ok(data.to_vec()),
            0 => Err(GeocodeError::Invalid(format!("{name} is larger than {max} bytes"))),
            8 => {
                let decoded = miniz_oxide::inflate::decompress_to_vec_with_limit(data, max).map_err(|e| bad(&format!("deflate: {e:?}")))?;
                if decoded.len() != usize_ {
                    return Err(bad("uncompressed size"));
                }
                Ok(decoded)
            }
            m => Err(bad(&format!("compression method {m}"))),
        };
    }
    Err(GeocodeError::Invalid(format!("{name} is not in the archive")))
}

/// A one-file deflated ZIP (test fixtures: the engine's download test serves one).
#[doc(hidden)]
pub fn make(name: &str, data: &[u8]) -> Vec<u8> {
    let c = miniz_oxide::deflate::compress_to_vec(data, 6);
    let mut z = Vec::new();
    z.extend_from_slice(b"PK\x03\x04");
    z.extend_from_slice(&[20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&(c.len() as u32).to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z.extend_from_slice(name.as_bytes());
    z.extend_from_slice(&c);
    let cd = z.len();
    z.extend_from_slice(b"PK\x01\x02");
    z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&(c.len() as u32).to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0; 12]);
    z.extend_from_slice(&0u32.to_le_bytes());
    z.extend_from_slice(name.as_bytes());
    let cdlen = z.len() - cd;
    z.extend_from_slice(b"PK\x05\x06");
    z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend_from_slice(&(cdlen as u32).to_le_bytes());
    z.extend_from_slice(&(cd as u32).to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_junk() {
        let z = make("cities15000.txt", b"hello world");
        assert_eq!(extract(&z, "cities15000.txt", 100).unwrap(), b"hello world");
        assert!(extract(&z, "cities15000.txt", 5).is_err());
        assert!(extract(&z, "other.txt", 100).is_err());
        assert!(extract(b"PK", "x", 10).is_err());
        assert!(extract(b"", "x", 10).is_err());
        for cut in 0..z.len() {
            let _ = extract(&z[..cut], "cities15000.txt", 100);
        }
        for i in 0..z.len() {
            let mut bad = z.clone();
            bad[i] ^= 0xff;
            let _ = extract(&bad, "cities15000.txt", 100);
        }
    }

    #[test]
    fn rejects_deflated_entry_with_inconsistent_directory_size() {
        let mut z = make("cities15000.txt", b"hello world");
        let eocd = z.len() - 22;
        let cd = u32::from_le_bytes([z[eocd + 16], z[eocd + 17], z[eocd + 18], z[eocd + 19]]) as usize;
        let declared_size = cd + 24;
        z[declared_size..declared_size + 4].copy_from_slice(&2u32.to_le_bytes());

        assert!(extract(&z, "cities15000.txt", 100).is_err());
    }
}
