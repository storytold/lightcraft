//! A tiny, bounded reader of uncompressed ("stored") zip entries, for MediaPipe `.task` bundles
//! (a zip of `.tflite` files written without compression).
//!
//! Written from the public ZIP format description (PKWARE APPNOTE.TXT, sections 4.3.7 local file
//! header, 4.3.12 central directory header and 4.3.16 end of central directory record). Only
//! stored entries without ZIP64 are read; anything else, or a malformed archive, is `None`.

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The end of central directory record: the last signature within the final 64 KiB + 22 bytes.
fn eocd(zip: &[u8]) -> Option<usize> {
    let last = zip.len().checked_sub(22)?;
    let first = last.saturating_sub(u16::MAX as usize);
    (first..=last).rev().find(|&p| u32_at(zip, p) == Some(EOCD_SIG))
}

/// The bytes of the stored entry `name` (exact path inside the archive), if there is one.
pub fn entry<'a>(zip: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let e = eocd(zip)?;
    let count = u16_at(zip, e + 10)?;
    let mut p = u32_at(zip, e + 16)? as usize;
    for _ in 0..count {
        if u32_at(zip, p)? != CENTRAL_SIG {
            return None;
        }
        let method = u16_at(zip, p + 10)?;
        let size = u32_at(zip, p + 20)?;
        let name_len = u16_at(zip, p + 28)?;
        let extra_len = u16_at(zip, p + 30)?;
        let comment_len = u16_at(zip, p + 32)?;
        let local = u32_at(zip, p + 42)? as usize;
        let n = zip.get(p.checked_add(46)?..p.checked_add(46 + name_len)?)?;
        if n == name.as_bytes() {
            if method != 0 || size == u32::MAX {
                return None;
            }
            if u32_at(zip, local)? != LOCAL_SIG {
                return None;
            }
            let start = local.checked_add(30 + u16_at(zip, local + 26)? + u16_at(zip, local + 28)?)?;
            return zip.get(start..start.checked_add(size as usize)?);
        }
        p = p.checked_add(46 + name_len + extra_len + comment_len)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored zip with the given entries, as `zip -0` writes it (no data descriptors).
    fn build(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let off = out.len() as u32;
            out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, method, time, date, crc
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&3u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&[1, 2, 3]);
            out.extend_from_slice(data);
            central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn finds_stored_entries() {
        let z = build(&[("a.tflite", b"hello"), ("dir/b.bin", b"world!")]);
        assert_eq!(entry(&z, "a.tflite"), Some(&b"hello"[..]));
        assert_eq!(entry(&z, "dir/b.bin"), Some(&b"world!"[..]));
        assert_eq!(entry(&z, "missing"), None);
    }

    #[test]
    fn malformed_archives_are_none() {
        let z = build(&[("a.tflite", b"hello"), ("b", b"x")]);
        for n in 0..z.len() {
            let _ = entry(&z[..n], "a.tflite");
            let _ = entry(&z[n..], "b");
        }
        let mut bad = z.clone();
        let len = bad.len();
        bad[len - 6..len - 2].copy_from_slice(&u32::MAX.to_le_bytes()); // central directory offset
        assert_eq!(entry(&bad, "a.tflite"), None);
        // a deflated entry is not returned
        let mut deflated = z.clone();
        let cd = u32_at(&z, len - 6).unwrap() as usize;
        deflated[cd + 10] = 8;
        assert_eq!(entry(&deflated, "a.tflite"), None);
        assert_eq!(entry(&[], "a"), None);
    }
}
