use super::load::{load_vec, load_vec_pair, load_vec_reduced};
use super::preview::{MAX_PREVIEW_PLAN_BYTES, fit_box, fs_preview_loader, preview_orientation};
use super::probe::{describe, probe_bytes};
use super::{Arc, DenoiseSpec, FileLoader, FileProbe, Filter, PairLoader, ProbeInfo, ReducedLoader, fit};
use crate::media::RawDecode;

/// The file's first [`lightcraft_raw::PREVIEW_PREFIX`] bytes (or the whole file when it is shorter),
/// which is what every format [`lightcraft_raw::bounded_preview`] covers keeps its pointer in.
pub(super) fn read_prefix(file: &mut std::fs::File, path: &str, total: u64) -> Result<Vec<u8>, String> {
    let len = lightcraft_raw::PREVIEW_PREFIX.min(usize::try_from(total).unwrap_or(usize::MAX));
    read_prefix_at(file, path, len as u64)
}

/// The file's first `len` bytes, read from the start.
pub(super) fn read_prefix_at(file: &mut std::fs::File, path: &str, len: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = usize::try_from(len).map_err(|_| format!("{path}: file too large"))?;
    let mut prefix = vec![0u8; len];
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{path}: {e}"))?;
    file.read_exact(&mut prefix).map_err(|e| format!("{path}: {e}"))?;
    Ok(prefix)
}

/// The end of a Fuji RAF's preview block, from the two big-endian u32s every RAF header carries at
/// 84 (offset) and 88 (length) — the same fields `lightcraft_raw`'s RAF reader uses, read here
/// without the rest of the file so the prefix can be made big enough first.
pub(super) fn raf_preview_extent(prefix: &[u8]) -> Option<u64> {
    if !prefix.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let u32_at = |at: usize| prefix.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])).map(u64::from);
    let (offset, len) = (u32_at(84)?, u32_at(88)?);
    (offset > 0 && len > 0).then(|| offset.checked_add(len)).flatten()
}

/// The display-ready preview of a raw whose embedded JPEG was already located: the counterpart of
/// [`embedded_preview_srgb`] for [`preview_window`], which does not re-read the container.
/// `adobe_rgb`: the container declares the preview Adobe RGB ([`PreviewRead::adobe_rgb`]), applied
/// as [`decode_raw_preview`] applies it. `None` when the bytes are not a decodable image.
pub(super) fn quick_image_srgb(header: &[u8], window: &[u8], adobe_rgb: bool, max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    // a plain sRGB camera JPEG (nearly all of them) straight to 8-bit (PERF-CULL-3: 45 MP preview
    // of an EOS R5 C fitted to 2560 px, 194 → ~110 ms); anything else takes the general decode
    let edge = u32::try_from(max_edge).unwrap_or(u32::MAX);
    if !adobe_rgb && let Ok(Some((image, orientation))) = lightcraft_codecs::decode_jpeg_srgb8_fit(window, edge, edge) {
        return Some(image.oriented(preview_orientation(header, orientation)));
    }
    let decoded = if adobe_rgb && window.starts_with(&[0xff, 0xd8]) {
        lightcraft_codecs::decode_jpeg_with_fallback(window, fit_box(max_edge), lightcraft_codecs::NamedSpace::AdobeRgb)
    } else {
        lightcraft_codecs::decode(window, fit_box(max_edge))
    };
    let mut d = decoded.ok()?;
    if d.image.width.max(d.image.height) > max_edge {
        d.image = fit(&d.image, max_edge, max_edge, Filter::Box);
        d.alpha = None;
    }
    // The JPEG's own EXIF orientation, else the raw's (from its header), as the whole-file path does
    Some(d.to_srgb8().oriented(preview_orientation(header, d.orientation)))
}

/// The filesystem-backed [`PairLoader`]: the file is read and decoded once, and developed as usual and from the photo's
/// denoised picture. It holds two pictures at once, so it counts for more at the memory gate than a plain load.
pub fn fs_pair_loader() -> PairLoader {
    Arc::new(|path: &str, max_edge: usize, spec: &DenoiseSpec, with: RawDecode<'_>| {
        let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 4 } else { 10 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = crate::read_file::read_regular(path, crate::read_file::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec_pair(bytes, max_edge, spec, with);
        crate::memory::release();
        r
    })
}

/// Filesystem-backed hooks (native). On the web the host installs bytes-based hooks instead.
///
/// Their working memory is bounded by [`crate::memory::work_gate`]: background loads (grid
/// thumbnails, neighbour prefetch: [`crate::memory::in_background`]) and import probes wait while
/// too much is in flight, each counting its file's size times the expansion of decoding it;
/// interactive loads (the loupe, exports) never wait — they are counted, so background work
/// yields to them.
pub fn fs_hooks() -> (FileLoader, FileProbe) {
    let loader: FileLoader =
        Arc::new(|path: &str, max_edge: usize, with: RawDecode<'_>| fs_load(path, max_edge, |bytes| load_vec(bytes, max_edge, with)));
    let probe: FileProbe = Arc::new(|path: &str| {
        // one open, then everything from the open file: its size and modification time are not
        // round trips of their own (on a network share each of them is one)
        let file = crate::read_file::open_for_read(path).map_err(|e| format!("{path}: {e}"))?;
        let meta = file.metadata().ok();
        let len = meta.as_ref().map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX));
        let _permit = crate::memory::work_gate().acquire(len);
        let bytes = crate::read_file::read_open(file, crate::read_file::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}"))?;
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = meta.as_ref().and_then(modified_iso);
        Ok(info)
    });
    (loader, probe)
}

/// The filesystem [`ReducedLoader`]: [`load_vec_reduced`] of the file, gated like [`fs_hooks`]' loader.
pub fn fs_reduced_loader() -> ReducedLoader {
    Arc::new(|path: &str, max_edge: usize, with: RawDecode<'_>| fs_load(path, max_edge, |bytes| load_vec_reduced(bytes, max_edge, with)))
}

/// Read `path` and `load` it, waiting on [`crate::memory::work_gate`] as [`fs_hooks`] explains.
fn fs_load<T>(path: &str, max_edge: usize, load: impl FnOnce(Vec<u8>) -> Result<T, String>) -> Result<T, String> {
    let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
    let weight = len.saturating_mul(if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 });
    let gate = crate::memory::work_gate();
    let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
    let bytes = crate::read_file::read_regular(path, crate::read_file::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}"))?;
    let r = load(bytes);
    // the file, the samples and the intermediate images are gone: give their pages back
    crate::memory::release();
    r
}

/// The modification time in `meta` as ISO 8601 (see [`ProbeInfo::file_modified`]); `None` when the
/// filesystem doesn't say or the time is before 1970.
pub(super) fn modified_iso(meta: &std::fs::Metadata) -> Option<String> {
    let t = meta.modified().ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(crate::import::civil(secs))
}

/// The first bytes of a raw read before asking what else describing it needs: most formats' headers
/// are all in here, so one read usually does.
const RAW_PREFIX: u64 = 512 * 1024;
/// Rounds of reads [`read_raw_headers`] makes before giving up on reading a file in part.
const RAW_ROUNDS: usize = 32;
/// Tenths of a raw [`read_raw_headers`] may read before it gives up and the file is read whole:
/// reading most of it in parts and then all of it again costs more than one whole read.
const RAW_MAX_SHARE: u64 = 6;
/// The largest raw [`read_raw_headers`] builds a buffer of the file's length for (as
/// [`MAX_PREVIEW_PLAN_BYTES`]); a larger file is read whole.
const RAW_MAX_BYTES: u64 = MAX_PREVIEW_PLAN_BYTES;

/// The raw `file` (`len` bytes) read only where describing it looks
/// ([`lightcraft_raw::sparse::header_plan`]): a buffer of the file's length holding those ranges, the
/// rest zeros. `None` when it has to be read whole (not a raw laid out for this, a format whose
/// header probe reads samples, a file smaller than the first read, or a read failing).
fn read_raw_headers(file: &mut std::fs::File, len: u64) -> Option<Vec<u8>> {
    use lightcraft_raw::sparse::{HeaderPlan, Ranges, header_plan};
    use std::io::{Read, Seek, SeekFrom};
    if len <= RAW_PREFIX || len > RAW_MAX_BYTES {
        return None;
    }
    let mut buf = vec![0u8; usize::try_from(len).ok()?];
    let mut read = |buf: &mut [u8], start: u64, end: u64| -> Option<()> {
        let part = buf.get_mut(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(part).ok()
    };
    let mut loaded = Ranges::new();
    read(&mut buf, 0, RAW_PREFIX)?;
    loaded.insert(0, RAW_PREFIX);
    let mut wanted = RAW_PREFIX;
    for _ in 0..RAW_ROUNDS {
        match header_plan(&buf, &loaded) {
            HeaderPlan::Ready => return Some(buf),
            HeaderPlan::Whole(_) => return None,
            HeaderPlan::Need(more) => {
                wanted = more.spans().iter().fold(wanted, |w, &(a, b)| w.saturating_add(b.saturating_sub(a)));
                if wanted.saturating_mul(10) > len.saturating_mul(RAW_MAX_SHARE) {
                    return None;
                }
                for &(a, b) in more.spans() {
                    if loaded.overlaps(a, b) {
                        return None; // asked again: don't loop
                    }
                    read(&mut buf, a, b)?;
                    loaded.insert(a, b);
                }
            }
        }
    }
    None
}

/// The probe for browsing a folder (Local), where a folder on a network share can hold thousands of
/// raws: a raw is described from the parts of it its headers are in (a few percent of a DNG or CR3)
/// and gets no content hash, which needs every byte (a Local photo is found again by its path; the
/// hash is worked out when it joins the library). Whatever can't be described that way — other
/// formats, a raw whose sensor data can't be decoded (described from its embedded preview), any
/// failure — is read whole, as [`fs_hooks`]' probe reads it.
pub(crate) fn light_probe(path: &str) -> Result<ProbeInfo, String> {
    use std::io::{Seek, SeekFrom};
    // one open, as [`fs_hooks`]' probe: only a regular file is read in part, anything else is refused by the whole read
    let mut file = crate::read_file::open_for_read(path).map_err(|e| format!("{path}: {e}"))?;
    let meta = file.metadata().ok();
    let modified = meta.as_ref().and_then(modified_iso);
    let len = meta.as_ref().filter(|m| m.is_file()).map_or(0, |m| m.len());
    let _permit = crate::memory::work_gate().acquire(usize::try_from(len).unwrap_or(usize::MAX));
    if let Some(buf) = read_raw_headers(&mut file, len)
        && let Ok(mut info) = describe(path, &buf, None)
        && info.preview_only.is_none()
    {
        info.file_modified = modified;
        return Ok(info);
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{path}: {e}"))?;
    let bytes = crate::read_file::read_open(file, crate::read_file::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}"))?;
    let mut info = probe_bytes(path, &bytes)?;
    info.file_modified = modified;
    Ok(info)
}

impl crate::Session {
    /// Install the filesystem file hooks (desktop, CLI, MCP headless), and on macOS the Apple Photos
    /// runner ([`crate::apple_photos::osascript`]; installed, not run).
    pub fn with_fs(mut self) -> Self {
        let (l, p) = fs_hooks();
        self.media.file_loader = Some(l);
        self.media.reduced_loader = Some(fs_reduced_loader());
        self.media.file_probe = Some(p);
        self.media.file_probe_light = Some(Arc::new(light_probe));
        self.media.preview_loader = Some(fs_preview_loader());
        self.media.denoise.loader = Some(fs_pair_loader());
        #[cfg(target_os = "macos")]
        {
            self.apple_photos = Some(crate::apple_photos::osascript());
        }
        self
    }
}
