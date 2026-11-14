//! Taking one file out of a `.zip` (a `.dtmodel` is one), the little of the format a model download needs: stored and
//! deflated entries, found by name, checked against their CRC-32 and a size limit, written next to where they are
//! going and moved into place. It is a stranger's file, so every length and offset in it is checked, a name is only
//! ever compared (never used as a path), and zip64, encrypted and multi-disk archives are refused.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// How far from the end the end-of-central-directory record can start (a 22-byte record and a 64 KiB comment).
const EOCD_SEARCH: u64 = 22 + 65535;
/// Largest central directory we read.
const MAX_DIRECTORY: u64 = 1 << 20;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ArchiveError {
    #[error("the archive could not be read: {0}")]
    Io(String),
    #[error("not a usable archive: {0}")]
    Format(String),
    #[error("the archive has no file called `{0}`")]
    Missing(String),
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e.to_string())
    }
}

fn format<T>(why: &str) -> Result<T, ArchiveError> {
    Err(ArchiveError::Format(why.into()))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// CRC-32 (IEEE) of `bytes`.
fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    !bytes.iter().fold(!0u32, |c, &b| table[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8))
}

struct Entry {
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local_header: u64,
}

/// Find the entry called `name` (exactly, or as the last part of a longer name: `folder/name`) in the central directory.
fn find(f: &mut (impl Read + Seek), name: &str) -> Result<Entry, ArchiveError> {
    let len = f.seek(SeekFrom::End(0))?;
    let tail_len = len.min(EOCD_SEARCH);
    f.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21)).rev().find(|&i| u32_at(&tail, i) == Some(0x0605_4b50));
    let Some(e) = eocd else { return format("this is not a zip file") };
    let (entries, size, offset) = (u16_at(&tail, e + 10), u32_at(&tail, e + 12), u32_at(&tail, e + 16));
    let (Some(entries), Some(size), Some(offset)) = (entries, size, offset) else { return format("the end of the archive is cut short") };
    if entries == 0xffff || size == u32::MAX || offset == u32::MAX {
        return format("zip64 archives are not supported");
    }
    if u64::from(size) > MAX_DIRECTORY || u64::from(offset) + u64::from(size) > len {
        return format("the list of files is not where it says");
    }
    f.seek(SeekFrom::Start(u64::from(offset)))?;
    let mut dir = vec![0u8; size as usize];
    f.read_exact(&mut dir)?;
    let mut at = 0usize;
    for _ in 0..entries {
        if u32_at(&dir, at) != Some(0x0201_4b50) {
            return format("the list of files is damaged");
        }
        let get = |o: usize| at.checked_add(o);
        let (flags, method, crc) = (u16_at(&dir, at + 8), u16_at(&dir, at + 10), u32_at(&dir, at + 16));
        let (compressed, uncompressed) = (u32_at(&dir, at + 20), u32_at(&dir, at + 24));
        let (n, x, c) = (u16_at(&dir, at + 28), u16_at(&dir, at + 30), u16_at(&dir, at + 32));
        let local = u32_at(&dir, at + 42);
        let (Some(flags), Some(method), Some(crc), Some(compressed), Some(uncompressed), Some(n), Some(x), Some(c), Some(local), Some(start)) =
            (flags, method, crc, compressed, uncompressed, n, x, c, local, get(46))
        else {
            return format("the list of files is cut short");
        };
        let Some(entry_name) = dir.get(start..start + usize::from(n)) else { return format("a file name is cut short") };
        let matches = entry_name == name.as_bytes() || entry_name.strip_suffix(name.as_bytes()).is_some_and(|p| p.ends_with(b"/"));
        if matches {
            if flags & 1 != 0 {
                return format("encrypted files are not supported");
            }
            if compressed == u32::MAX || uncompressed == u32::MAX {
                return format("zip64 files are not supported");
            }
            return Ok(Entry { method, crc, compressed: u64::from(compressed), size: u64::from(uncompressed), local_header: u64::from(local) });
        }
        at = start + usize::from(n) + usize::from(x) + usize::from(c);
    }
    Err(ArchiveError::Missing(name.into()))
}

/// Take the file called `name` out of the archive at `archive` and write it to `dest` (through a temporary file next
/// to it); `max_bytes` bounds its size. Returns the number of bytes written.
pub fn extract(archive: &Path, name: &str, dest: &Path, max_bytes: u64) -> Result<u64, ArchiveError> {
    let mut f = std::io::BufReader::new(std::fs::File::open(archive)?);
    let e = find(&mut f, name)?;
    if e.size > max_bytes || e.compressed > max_bytes {
        return format("the file in the archive is larger than allowed");
    }
    f.seek(SeekFrom::Start(e.local_header))?;
    let mut head = [0u8; 30];
    f.read_exact(&mut head)?;
    if u32_at(&head, 0) != Some(0x0403_4b50) {
        return format("the file's header is not where it says");
    }
    let skip = u64::from(u16_at(&head, 26).unwrap_or(0)) + u64::from(u16_at(&head, 28).unwrap_or(0));
    f.seek_relative(i64::try_from(skip).map_err(|_| ArchiveError::Format("a header is too long".into()))?)?;
    let len = usize::try_from(e.compressed)
        .ok()
        .filter(|&n| n <= 256 * 1024 * 1024)
        .ok_or_else(|| ArchiveError::Format("the compressed model is too large".into()))?;
    if e.size > 256 * 1024 * 1024 {
        return format("the extracted model is too large");
    }
    let mut packed = Vec::new();
    packed.try_reserve_exact(len).map_err(|_| ArchiveError::Format("not enough memory for the model archive".into()))?;
    packed.resize(len, 0u8);
    f.read_exact(&mut packed)?;
    let data = match e.method {
        0 => packed,
        8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, max_bytes.min(256 * 1024 * 1024) as usize)
            .map_err(|err| ArchiveError::Format(format!("{err:?}")))?,
        m => return format(&format!("compression method {m} is not supported")),
    };
    if data.len() as u64 != e.size || crc32(&data) != e.crc {
        return format("the file in the archive is damaged");
    }
    let tmp = dest.with_extension("part");
    let written = (|| -> std::io::Result<()> {
        let mut out = std::fs::File::create(&tmp)?;
        out.write_all(&data)?;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp, dest)
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(data.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip with the given (name, bytes, deflate) entries, written by hand.
    pub(crate) fn make_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data, deflate) in files {
            let packed = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
            let offset = out.len() as u32;
            let method: u16 = if *deflate { 8 } else { 0 };
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&crc32(data).to_le_bytes());
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            dir.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            dir.extend_from_slice(&method.to_le_bytes());
            dir.extend_from_slice(&[0, 0, 0, 0]);
            dir.extend_from_slice(&crc32(data).to_le_bytes());
            dir.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&offset.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lc-denoise-zip-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_stored_and_a_deflated_file_come_out_whole() {
        let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let zip = make_zip(&[("m/config.json", b"{}", false), ("m/model.onnx", &big, true), ("m/other.onnx", b"zz", true)]);
        let (a, out) = (temp("a.zip"), temp("a.onnx"));
        std::fs::write(&a, &zip).unwrap();
        assert_eq!(extract(&a, "model.onnx", &out, 1 << 20).unwrap(), big.len() as u64);
        assert_eq!(std::fs::read(&out).unwrap(), big);
        assert_eq!(extract(&a, "m/config.json", &out, 1 << 20).unwrap(), 2);
        assert_eq!(std::fs::read(&out).unwrap(), b"{}");
        // a name must match whole path parts: `del.onnx` is not `model.onnx`
        assert!(matches!(extract(&a, "del.onnx", &out, 1 << 20), Err(ArchiveError::Missing(_))));
        assert!(matches!(extract(&a, "nothing", &out, 1 << 20), Err(ArchiveError::Missing(_))));
        // the size limit applies
        assert!(matches!(extract(&a, "model.onnx", &out, 1000), Err(ArchiveError::Format(_))));
        for p in [a, out] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Opt-in: `LC_DENOISE_DTMODEL=<rawdenoise-nind.dtmodel> cargo test -p lightcraft-denoise -- --ignored archive`
    #[test]
    #[ignore = "needs the real download: set LC_DENOISE_DTMODEL"]
    fn the_real_download_gives_the_model_it_should() {
        let Some(path) = std::env::var_os("LC_DENOISE_DTMODEL") else { return };
        let known = crate::known::find("rawnind-bayer").unwrap();
        let (d, entry) = (known.download.unwrap(), known.entry.unwrap());
        assert_eq!(crate::hash::sha256_file(Path::new(&path)).unwrap(), d.sha256);
        let out = temp("real.onnx");
        let n = extract(Path::new(&path), entry, &out, crate::manifest::MAX_MODEL_BYTES).unwrap();
        assert_eq!(Some(n), known.manifest.size_bytes);
        assert_eq!(Some(crate::hash::sha256_file(&out).unwrap()), known.manifest.sha256);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn damaged_and_hostile_archives_are_errors_not_panics() {
        let zip = make_zip(&[("m/model.onnx", &vec![9u8; 5000], true)]);
        let (a, out) = (temp("b.zip"), temp("b.onnx"));
        // cut anywhere
        for n in (0..zip.len()).step_by(3) {
            std::fs::write(&a, &zip[..n]).unwrap();
            assert!(extract(&a, "model.onnx", &out, 1 << 20).is_err(), "cut at {n}");
        }
        // one byte changed anywhere: an error, or (when it falls in an unused field) the right file, never a panic
        for at in 0..zip.len() {
            let mut bad = zip.clone();
            bad[at] ^= 0x5a;
            std::fs::write(&a, &bad).unwrap();
            if extract(&a, "model.onnx", &out, 1 << 20).is_ok() {
                assert_eq!(std::fs::read(&out).unwrap(), vec![9u8; 5000], "a changed byte at {at} gave another file");
            }
        }
        // not a zip at all, and an empty file
        for junk in [Vec::new(), b"PK".to_vec(), vec![0u8; 100_000]] {
            std::fs::write(&a, junk).unwrap();
            assert!(extract(&a, "model.onnx", &out, 1 << 20).is_err());
        }
        assert!(extract(&temp("missing.zip"), "model.onnx", &out, 1 << 20).is_err());
        for p in [a, out] {
            let _ = std::fs::remove_file(p);
        }
    }
}
