//! Just enough ZIP reading to take one file out of a Logi profile package (`.lp5`). Every offset
//! and size comes from the file and is checked; inflated data is capped.

/// Largest archive read.
pub const MAX_ARCHIVE: usize = 16 * 1024 * 1024;
/// Largest member inflated.
pub const MAX_MEMBER: usize = 8 * 1024 * 1024;

/// `len` bytes at `at`, if they are all there.
fn bytes(b: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    b.get(at..at.checked_add(len)?)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = bytes(b, at, 2)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = bytes(b, at, 4)?;
    Some(u32::from_le_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
}

/// The bytes of the member called `name` (exact match, `/` separators).
pub fn read_member(archive: &[u8], name: &str) -> Result<Vec<u8>, String> {
    if archive.len() > MAX_ARCHIVE {
        return Err("the file is too large to be a Logi profile".into());
    }
    // End of central directory: the last "PK\x05\x06" within the final 64 KiB + 22 bytes.
    let min_start = archive.len().saturating_sub(22 + 65_535);
    let eocd =
        (min_start..archive.len().saturating_sub(21)).rev().find(|&i| bytes(archive, i, 4) == Some(b"PK\x05\x06")).ok_or("not a ZIP archive")?;
    let off = |base: usize, n: usize| base.checked_add(n).ok_or("damaged ZIP");
    let count = usize::from(u16_at(archive, off(eocd, 10)?).ok_or("damaged ZIP")?);
    let cd = usize::try_from(u32_at(archive, off(eocd, 16)?).ok_or("damaged ZIP")?).map_err(|_| "damaged ZIP")?;
    let mut at = cd;
    for _ in 0..count {
        if bytes(archive, at, 4) != Some(b"PK\x01\x02") {
            return Err("damaged ZIP directory".into());
        }
        let method = u16_at(archive, off(at, 10)?).ok_or("damaged ZIP")?;
        let csize = usize::try_from(u32_at(archive, off(at, 20)?).ok_or("damaged ZIP")?).map_err(|_| "damaged ZIP")?;
        let usize_ = usize::try_from(u32_at(archive, off(at, 24)?).ok_or("damaged ZIP")?).map_err(|_| "damaged ZIP")?;
        let nlen = usize::from(u16_at(archive, off(at, 28)?).ok_or("damaged ZIP")?);
        let elen = usize::from(u16_at(archive, off(at, 30)?).ok_or("damaged ZIP")?);
        let clen = usize::from(u16_at(archive, off(at, 32)?).ok_or("damaged ZIP")?);
        let local = usize::try_from(u32_at(archive, off(at, 42)?).ok_or("damaged ZIP")?).map_err(|_| "damaged ZIP")?;
        let entry_name = bytes(archive, off(at, 46)?, nlen).ok_or("damaged ZIP")?;
        if entry_name == name.as_bytes() {
            if bytes(archive, local, 4) != Some(b"PK\x03\x04") {
                return Err("damaged ZIP entry".into());
            }
            let lnlen = usize::from(u16_at(archive, off(local, 26)?).ok_or("damaged ZIP")?);
            let lelen = usize::from(u16_at(archive, off(local, 28)?).ok_or("damaged ZIP")?);
            let start = off(off(local, 30)?, lnlen + lelen)?;
            let data = bytes(archive, start, csize).ok_or("damaged ZIP entry")?;
            if usize_ > MAX_MEMBER {
                return Err(format!("{name} is too large"));
            }
            return match method {
                0 => Ok(data.to_vec()),
                8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_MEMBER).map_err(|_| format!("{name} can't be unpacked")),
                m => Err(format!("{name} uses an unsupported compression ({m})")),
            };
        }
        at = off(at, 46 + nlen + elen + clen)?;
    }
    Err(format!("{name} is not in the file"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ZIP with stored (method 0) and deflated (method 8) members.
    pub(crate) fn make_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data, deflate) in files {
            let packed = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
            let off = out.len() as u32;
            let method: u16 = if *deflate { 8 } else { 0 };
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0; 8]);
            cd.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0; 12]); // extra, comment, disk, int attr, ext attr (2+2+2+2+4)
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn reads_stored_and_deflated_members() {
        let z = make_zip(&[("a.txt", b"hello", false), ("dir/ProfileInfo.json", b"{\"x\": 1}", true)]);
        assert_eq!(read_member(&z, "a.txt").unwrap(), b"hello");
        assert_eq!(read_member(&z, "dir/ProfileInfo.json").unwrap(), b"{\"x\": 1}");
        assert!(read_member(&z, "missing").unwrap_err().contains("not in the file"));
    }

    #[test]
    fn junk_and_truncation_are_errors_not_panics() {
        let z = make_zip(&[("ProfileInfo.json", b"{}", true)]);
        assert!(read_member(b"", "x").is_err());
        assert!(read_member(b"PK\x05\x06", "x").is_err());
        for cut in 0..z.len() {
            let _ = read_member(&z[..cut], "ProfileInfo.json");
        }
        let mut bad = z.clone();
        let n = bad.len();
        bad[n - 6..n - 2].copy_from_slice(&u32::MAX.to_le_bytes()); // directory offset past the end
        assert!(read_member(&bad, "ProfileInfo.json").is_err());
        let mut flip = z;
        for i in 0..flip.len() {
            flip[i] ^= 0xA5;
            let _ = read_member(&flip, "ProfileInfo.json");
            flip[i] ^= 0xA5;
        }
    }
}
