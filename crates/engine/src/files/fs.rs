use super::load::{load_vec, load_vec_pair, load_vec_reduced};
use super::preview::{fit_box, fs_preview_loader, preview_orientation};
use super::probe::probe_bytes;
use super::{Arc, DenoiseSpec, FileLoader, FileProbe, Filter, PairLoader, ReducedLoader, fit};

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
/// as [`decode_raw_preview`] applies it. `None` when the bytes are not a decodable image, and
/// when they are a JPEG that stops before its end (a pointer that lies about the preview's length,
/// or a window whose last range wasn't read): the caller's whole-file fallback decodes the real one.
pub(super) fn quick_image_srgb(header: &[u8], window: &[u8], adobe_rgb: bool, max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    if lightcraft_codecs::read_header(window).is_ok_and(|h| h.truncated) {
        return None;
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
    Arc::new(|path: &str, max_edge: usize, spec: &DenoiseSpec| {
        let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 4 } else { 10 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec_pair(bytes, max_edge, spec);
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
    let loader: FileLoader = Arc::new(|path: &str, max_edge: usize| fs_load(path, max_edge, |bytes| load_vec(bytes, max_edge)));
    let probe: FileProbe = Arc::new(|path: &str| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let _permit = crate::memory::work_gate().acquire(len);
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = file_modified(path);
        Ok(info)
    });
    (loader, probe)
}

/// The filesystem [`ReducedLoader`]: [`load_vec_reduced`] of the file, gated like [`fs_hooks`]' loader.
pub fn fs_reduced_loader() -> ReducedLoader {
    Arc::new(|path: &str, max_edge: usize| fs_load(path, max_edge, |bytes| load_vec_reduced(bytes, max_edge)))
}

/// How much of the start of a JPEG is read first to find its frame size (markers up to the frame
/// header: EXIF, ICC and the like come first); doubled until it is found or the file is all read.
pub(super) const FRAME_PROBE: usize = 1 << 20;

/// What a decode of a JPEG cut short holds beyond its file: the decoder reads the whole frame
/// (3 bytes a pixel; with the float image of a full-size load, 15), where the file may be a small
/// prefix of a large photo. 0 for a file that ends in an end-of-image marker (a whole file; one
/// with data after it, such as a motion photo's video, is charged its frame too: more than it
/// holds, never less) or isn't a JPEG. Reads two bytes at the end of the file and its start, up to
/// the frame header. The caller's permit for the file's length covers what is read here: the
/// probe never exceeds the file.
pub(super) fn cut_jpeg_frame_weight(path: &str, len: usize, max_edge: usize) -> usize {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return 0 };
    let mut tail = [0u8; 2];
    let mut soi = [0u8; 2];
    // the first two bytes settle a non-JPEG (most files) before anything larger is read
    if len < 4
        || file.read_exact(&mut soi).is_err()
        || soi != [0xFF, 0xD8]
        || file.seek(SeekFrom::End(-2)).and_then(|_| file.read_exact(&mut tail)).is_err()
        || tail == [0xFF, 0xD9]
    {
        return 0;
    }
    let per_pixel = if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 15 };
    let mut probe = len.min(FRAME_PROBE);
    loop {
        let mut head = vec![0u8; probe];
        if file.seek(SeekFrom::Start(0)).and_then(|_| file.read_exact(&mut head)).is_err() || !head.starts_with(&[0xFF, 0xD8]) {
            return 0;
        }
        if let Some((w, h)) = lightcraft_codecs::jpeg_frame_size(&head) {
            return (w as usize).saturating_mul(h as usize).saturating_mul(per_pixel);
        }
        if probe >= len {
            return 0;
        }
        probe = probe.saturating_mul(2).min(len);
    }
}

/// Read `path` and `load` it, waiting on [`crate::memory::work_gate`] as [`fs_hooks`] explains.
fn fs_load<T>(path: &str, max_edge: usize, load: impl FnOnce(Vec<u8>) -> Result<T, String>) -> Result<T, String> {
    load_gated(crate::memory::work_gate(), crate::memory::is_background(), path, max_edge, load)
}

/// [`fs_load`] on `gate`. Everything held is counted, and nothing waits while holding a permit:
/// the permit for the file's length covers the few bytes read to see whether a JPEG is cut short
/// (they are freed before the next wait); a cut one then waits once for its file plus its whole
/// frame, before the file is read.
pub(super) fn load_gated<T>(
    gate: &crate::memory::WorkGate,
    background: bool,
    path: &str,
    max_edge: usize,
    load: impl FnOnce(Vec<u8>) -> Result<T, String>,
) -> Result<T, String> {
    let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
    let weight = len.saturating_mul(if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 });
    let acquire = |bytes: usize| if background { gate.acquire(bytes) } else { gate.acquire_urgent(bytes) };
    let mut permit = acquire(weight);
    let frame = cut_jpeg_frame_weight(path, len, max_edge);
    if len.saturating_add(frame) > weight {
        drop(permit);
        permit = acquire(weight.max(len.saturating_add(frame)));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let r = load(bytes);
    drop(permit);
    // the file, the samples and the intermediate images are gone: give their pages back
    crate::memory::release();
    r
}

/// `path`'s modification time as ISO 8601 (see [`ProbeInfo::file_modified`]); `None` when the
/// filesystem doesn't say or the time is before 1970.
fn file_modified(path: &str) -> Option<String> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(crate::import::civil(secs))
}

impl crate::Session {
    /// Install the filesystem file hooks (desktop, CLI, MCP headless), and on macOS the Apple Photos
    /// runner ([`crate::apple_photos::osascript`]; installed, not run).
    pub fn with_fs(mut self) -> Self {
        let (l, p) = fs_hooks();
        self.media.file_loader = Some(l);
        self.media.reduced_loader = Some(fs_reduced_loader());
        self.media.file_probe = Some(p);
        self.media.preview_loader = Some(fs_preview_loader());
        self.media.denoise.loader = Some(fs_pair_loader());
        #[cfg(target_os = "macos")]
        {
            self.apple_photos = Some(crate::apple_photos::osascript());
        }
        self
    }
}
