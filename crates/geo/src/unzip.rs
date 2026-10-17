//! Just enough ZIP to read one file out of a GeoNames dump (`cities15000.zip`): the central
//! directory, stored or deflated entries, sizes capped. Own design from the PKWARE APPNOTE
//! (prose spec).

use std::io::Read;

use crate::GeoError;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    let i2 = i.checked_add(1)?;
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i2)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn bad(what: &str) -> GeoError {
    GeoError::Invalid(format!("not a readable ZIP archive ({what})"))
}

/// The contents of the entry whose name ends with `name`, at most `max` bytes.
pub fn extract(zip: &[u8], name: &str, max: u64) -> Result<Vec<u8>, GeoError> {
    // end of central directory: the last "PK\x05\x06" within the final 64 KiB + 22 bytes
    let from = zip.len().saturating_sub(65_557);
    let eocd = (from..zip.len().saturating_sub(21)).rev().find(|&i| zip.get(i..i + 4) == Some(b"PK\x05\x06")).ok_or_else(|| bad("no directory"))?;
    let count = u16_at(zip, eocd + 10).ok_or_else(|| bad("directory"))? as usize;
    let mut p = u32_at(zip, eocd + 16).ok_or_else(|| bad("directory"))? as usize;
    for _ in 0..count {
        if zip.get(p..p.saturating_add(4)) != Some(b"PK\x01\x02") {
            return Err(bad("directory entry"));
        }
        let method = u16_at(zip, p + 10).ok_or_else(|| bad("entry"))?;
        let csize = u32_at(zip, p + 20).ok_or_else(|| bad("entry"))? as usize;
        let usize_ = u32_at(zip, p + 24).ok_or_else(|| bad("entry"))? as u64;
        let nlen = u16_at(zip, p + 28).ok_or_else(|| bad("entry"))? as usize;
        let xlen = u16_at(zip, p + 30).ok_or_else(|| bad("entry"))? as usize;
        let clen = u16_at(zip, p + 32).ok_or_else(|| bad("entry"))? as usize;
        let local = u32_at(zip, p + 42).ok_or_else(|| bad("entry"))? as usize;
        let name_at = p.saturating_add(46);
        let entry = zip.get(name_at..name_at.saturating_add(nlen)).ok_or_else(|| bad("entry name"))?;
        p = name_at.saturating_add(nlen).saturating_add(xlen).saturating_add(clen);
        if !String::from_utf8_lossy(entry).ends_with(name) {
            continue;
        }
        if usize_ > max {
            return Err(GeoError::Invalid(format!("{name} is larger than {max} bytes")));
        }
        if zip.get(local..local.saturating_add(4)) != Some(b"PK\x03\x04") {
            return Err(bad("local header"));
        }
        let lnlen = u16_at(zip, local + 26).ok_or_else(|| bad("local header"))? as usize;
        let lxlen = u16_at(zip, local + 28).ok_or_else(|| bad("local header"))? as usize;
        let start = local.saturating_add(30).saturating_add(lnlen).saturating_add(lxlen);
        let data = zip.get(start..start.saturating_add(csize)).ok_or_else(|| bad("truncated"))?;
        return match method {
            0 => Ok(data.to_vec()),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(data).take(max.saturating_add(1)).read_to_end(&mut out).map_err(|e| bad(&e.to_string()))?;
                if out.len() as u64 > max {
                    return Err(GeoError::Invalid(format!("{name} is larger than {max} bytes")));
                }
                Ok(out)
            }
            m => Err(bad(&format!("compression method {m}"))),
        };
    }
    Err(GeoError::Invalid(format!("{name} is not in the archive")))
}

/// Build a one-file ZIP (deflated) — for tests.
#[cfg(test)]
pub fn make(name: &str, data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    let c = enc.finish().unwrap();
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
        for cut in [10, 30, z.len() - 30, z.len() - 5] {
            let _ = extract(&z[..cut], "cities15000.txt", 100);
        }
    }
}
