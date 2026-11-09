//! The import scan's probe: a raw is described from the parts of it its headers are in.
//!
//! A raw is tens of megabytes, nearly all of it sensor data that describing it never looks at, and an import's scan
//! over a card or a share waits for every one of those bytes (about an hour for 2,000 CR2s from an SD card, #3786).
//! [`header_probe`] reads the byte ranges [`lightcraft_raw::sparse::header_plan`] names (a few percent of a DNG, CR2 or
//! CR3) and describes the file from them, as describing the whole file would: the planner's contract is that a buffer
//! of the file's length holding those ranges probes like the file. That buffer's bytes are not the file's, so the
//! description carries no content hash ([`ProbeInfo::content_hash`] is `None`): the hash needs every byte, and is made
//! where the file is read in full anyway (the import's copy) or where the review needs it
//! ([`crate::import::scan_with`] reads the files whose size a library photo shares).
//!
//! Everything the planner can't describe from part of it (a JPEG or other image, a NEF, ORF or PEF, whose header probes
//! read samples, a raw shown from its embedded preview, a file under [`HEAD`] bytes, any failing read) is read whole
//! and described by [`probe_bytes`], hash included, as the full probe ([`full_probe`]) does.

use std::io::{Read, Seek, SeekFrom};

use lightcraft_raw::sparse::{HeaderPlan, Ranges, header_plan};

use super::probe::describe;
use super::{Arc, FileProbe, probe_bytes};

/// Bytes read from the start of a raw before the planner is asked what else its headers need: most formats' headers are
/// all in here, so one read usually does. A file no longer than this is read whole.
pub(crate) const HEAD: u64 = 512 * 1024;
/// Rounds of reads [`read_headers`] makes before it gives up on reading a file in part: a real file needs a handful.
const ROUNDS: usize = 32;
/// Tenths of a file [`read_headers`] may read before it gives up: reading most of it in parts and then all of it again
/// costs more than one whole read. (The preview planner bounds itself the same way.)
const MAX_SHARE: u64 = 6;
/// The largest file [`read_headers`] builds a buffer of the file's length for; a larger one is read whole.
const MAX_BYTES: u64 = 1 << 30;

/// Where a probe reads a file from: the file system ([`os_opener`]), or a stand-in that counts what is read (tests).
pub(crate) trait FileSource: Read + Seek + Send {
    /// The file's size when it was opened (0 when the file system doesn't say).
    fn size(&self) -> u64;
    /// The file's modification time ([`ProbeInfo::file_modified`]).
    fn modified(&self) -> Option<String>;
}

/// Opens the file at a path for a probe.
pub(crate) type Opener = Arc<dyn Fn(&str) -> std::io::Result<Box<dyn FileSource>> + Send + Sync>;

struct OsFile {
    file: std::fs::File,
    len: u64,
    modified: Option<String>,
}

impl Read for OsFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for OsFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(to)
    }
}

impl FileSource for OsFile {
    fn size(&self) -> u64 {
        self.len
    }

    fn modified(&self) -> Option<String> {
        self.modified.clone()
    }
}

/// The file system's [`Opener`]: one open, then everything from the open file (its size and modification time are not
/// round trips of their own: on a network share each of them is one).
pub(crate) fn os_opener() -> Opener {
    Arc::new(|path: &str| {
        let file = std::fs::File::open(path)?;
        let meta = file.metadata().ok();
        let len = meta.as_ref().map_or(0, std::fs::Metadata::len);
        let modified = meta.as_ref().and_then(modified_iso);
        Ok(Box::new(OsFile { file, len, modified }) as Box<dyn FileSource>)
    })
}

/// The modification time in `meta` as ISO 8601 (see [`ProbeInfo::file_modified`]); `None` when the
/// filesystem doesn't say or the time is before 1970.
fn modified_iso(meta: &std::fs::Metadata) -> Option<String> {
    let t = meta.modified().ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(crate::import::civil(secs))
}

/// The probe that reads a file in full and describes it with its content hash ([`probe_bytes`]), waiting on the memory
/// gate for the file's size.
pub(crate) fn full_probe(open: Opener) -> FileProbe {
    Arc::new(move |path: &str| {
        let mut src = open(path).map_err(|e| format!("{path}: {e}"))?;
        let _permit = crate::memory::work_gate().acquire(usize::try_from(src.size()).unwrap_or(usize::MAX));
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = src.modified();
        Ok(info)
    })
}

/// The probe that describes a raw from the parts of it its headers are in, without a content hash; any other file, and a
/// raw that can't be described that way, as [`full_probe`] does (see the module docs).
pub(crate) fn header_probe(open: Opener) -> FileProbe {
    Arc::new(move |path: &str| {
        let mut src = open(path).map_err(|e| format!("{path}: {e}"))?;
        let len = src.size();
        let _permit = crate::memory::work_gate().acquire(usize::try_from(len).unwrap_or(usize::MAX));
        let mut bytes = match read_headers(&mut *src, len) {
            Ok(buf) => match describe(path, &buf, None) {
                Ok(mut info) if info.preview_only.is_none() => {
                    info.file_modified = src.modified();
                    // (the buffer is the file's length, most of it never written: give its pages back the way a decode does)
                    drop(buf);
                    crate::memory::release();
                    return Ok(info);
                }
                // (what was read is the file's start and the ranges the headers are in: read the file again, whole)
                _ => Vec::new(),
            },
            Err(prefix) => prefix,
        };
        // the rest of the file, after what is already read from its start
        src.seek(SeekFrom::Start(bytes.len() as u64)).map_err(|e| format!("{path}: {e}"))?;
        src.read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = src.modified();
        Ok(info)
    })
}

/// The file of `len` bytes read only where describing it looks: a buffer of the file's length holding the ranges
/// [`header_plan`] names, the rest zeros. `Err` when it has to be read whole (a file no longer than [`HEAD`], a layout
/// or format the planner doesn't describe from part of it, a plan wanting most of the file, a read failing: a file
/// that shrank since it was listed), with the start of the file as far as it was read, which the whole read goes on from.
fn read_headers(src: &mut dyn FileSource, len: u64) -> Result<Vec<u8>, Vec<u8>> {
    if len <= HEAD || len > MAX_BYTES {
        return Err(Vec::new());
    }
    let Ok(size) = usize::try_from(len) else { return Err(Vec::new()) };
    let mut buf = vec![0u8; size];
    let mut read = |buf: &mut [u8], start: u64, end: u64| -> Option<()> {
        let part = buf.get_mut(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
        src.seek(SeekFrom::Start(start)).ok()?;
        src.read_exact(part).ok()
    };
    let mut loaded = Ranges::new();
    if read(&mut buf, 0, HEAD).is_none() {
        return Err(Vec::new());
    }
    loaded.insert(0, HEAD);
    let give_up = |mut buf: Vec<u8>| {
        buf.truncate(usize::try_from(HEAD).unwrap_or(0));
        Err(buf)
    };
    for _ in 0..ROUNDS {
        match header_plan(&buf, &loaded) {
            HeaderPlan::Ready => return Ok(buf),
            HeaderPlan::Whole(_) => return give_up(buf),
            HeaderPlan::Need(more) => {
                if loaded.total().saturating_add(more.total()).saturating_mul(10) > len.saturating_mul(MAX_SHARE) {
                    return give_up(buf);
                }
                for &(start, end) in more.spans() {
                    // (the plan names what isn't loaded yet: a range asked again is a plan that doesn't move)
                    if loaded.overlaps(start, end) || read(&mut buf, start, end).is_none() {
                        return give_up(buf);
                    }
                    loaded.insert(start, end);
                }
            }
        }
    }
    give_up(buf)
}

#[cfg(test)]
pub(crate) mod tests;
