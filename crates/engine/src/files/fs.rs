use super::load::{load_vec, load_vec_pair, load_vec_reduced};
use super::preview::{fit_box, fs_preview_loader, preview_orientation};
use super::probe::{ext_upper, meta_of, probe_bytes};
use super::*;

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

/// Read metadata boxes (ftyp, moov, meta) of an ISO-BMFF container into a bounded buffer
/// without reading large media payloads (mdat). Capped at 64 MB.
fn read_video_metadata_boxes(path: &str, file_len: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut out = Vec::new();
    let mut at = 0u64;
    let mut budget = 200usize;
    const MAX_BOX_SIZE: u64 = 64 * 1024 * 1024;

    while at.saturating_add(8) <= file_len && budget > 0 {
        budget -= 1;
        f.seek(SeekFrom::Start(at)).map_err(|e| format!("{path}: {e}"))?;
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr).map_err(|e| format!("{path}: {e}"))?;
        let size_u32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];

        let (total_len, hdr_len) = match size_u32 {
            0 => (file_len.saturating_sub(at), 8u64),
            1 => {
                let mut large = [0u8; 8];
                f.read_exact(&mut large).map_err(|e| format!("{path}: {e}"))?;
                (u64::from_be_bytes(large), 16u64)
            }
            n => (n, 8u64),
        };

        if total_len < hdr_len {
            break;
        }

        let end = match at.checked_add(total_len) {
            Some(e) if e <= file_len => e,
            _ => break,
        };

        if &kind == b"moov" || &kind == b"ftyp" || &kind == b"meta" {
            if total_len > MAX_BOX_SIZE {
                return Err(format!("{path}: video metadata box exceeds 64MB limit"));
            }
            f.seek(SeekFrom::Start(at)).map_err(|e| format!("{path}: {e}"))?;
            let mut box_data = vec![0u8; total_len as usize];
            f.read_exact(&mut box_data).map_err(|e| format!("{path}: {e}"))?;
            out.extend_from_slice(&box_data);
        }

        at = end;
    }

    Ok(out)
}

/// Compute the content hash of `path` incrementally in 64 KB chunks, matching `lightcraft_preview::hash_bytes`.
fn hash_file_stream(path: &str) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut hasher = lightcraft_preview::Hasher128::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or(&[]));
    }
    Ok(hasher.finish().to_string())
}

/// Probe a video file using bounded box reads and incremental hashing.
fn probe_video_file(path: &str, file_len: u64) -> Result<ProbeInfo, String> {
    let meta_bytes = read_video_metadata_boxes(path, file_len)?;
    if meta_bytes.is_empty() {
        return Err("no metadata boxes found".into());
    }
    let v = lightcraft_meta::probe_video(&meta_bytes).ok_or("unreadable video: no movie header or video track")?;
    let (mut w, mut h) = (v.width, v.height);
    if let Some(o) = v.orientation
        && o.swaps_axes()
    {
        std::mem::swap(&mut w, &mut h);
    }
    let (v_meta, v_captured) = meta_of(&v.metadata);
    let captured = v_captured.or(v.captured);
    let mut meta = Meta::default();
    if !v_meta.camera.is_empty() {
        meta.camera = v_meta.camera;
    }
    if v_meta.gps.is_some() {
        meta.gps = v_meta.gps;
    }
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let format = match ext_upper(&name).as_str() {
        "MP4" | "MOV" | "M4V" => ext_upper(&name),
        _ => "MP4".to_string(),
    };
    let content_hash = Some(hash_file_stream(path)?);
    Ok(ProbeInfo {
        width: w,
        height: h,
        format,
        kind: MediaKind::Video,
        file_size: file_len,
        captured,
        file_modified: None,
        meta,
        as_shot_wb: None,
        content_hash,
        xmp: lightcraft_meta::embedded(&meta_bytes).xmp,
        preview_only: None,
        duration: v.duration,
        embedded_lens: None,
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
        use std::io::Read;
        // one open, then everything from the open file: its size and modification time are not
        // round trips of their own (on a network share each of them is one)
        let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let meta = file.metadata().ok();
        let len = meta.as_ref().map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX));
        // A video container is read box by box, capped at 64 MB, instead of whole; one that can't
        // be read that way falls back to probing its bytes.
        let is_video = matches!(ext_upper(path).as_str(), "MP4" | "MOV" | "M4V");
        let weight = if is_video { len.min(64 * 1024 * 1024) } else { len };
        let _permit = crate::memory::work_gate().acquire(weight);
        let mut info = if is_video {
            match probe_video_file(path, u64::try_from(len).unwrap_or(u64::MAX)) {
                Ok(info) => info,
                Err(_) => {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
                    probe_bytes(path, &bytes)?
                }
            }
        } else {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
            probe_bytes(path, &bytes)?
        };
        info.file_modified = meta.as_ref().and_then(modified_iso);
        Ok(info)
    });
    (loader, probe)
}

/// The filesystem [`ReducedLoader`]: [`load_vec_reduced`] of the file, gated like [`fs_hooks`]' loader.
pub fn fs_reduced_loader() -> ReducedLoader {
    Arc::new(|path: &str, max_edge: usize| fs_load(path, max_edge, |bytes| load_vec_reduced(bytes, max_edge)))
}

/// Read `path` and `load` it, waiting on [`crate::memory::work_gate`] as [`fs_hooks`] explains.
fn fs_load<T>(path: &str, max_edge: usize, load: impl FnOnce(Vec<u8>) -> Result<T, String>) -> Result<T, String> {
    let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
    let weight = len.saturating_mul(if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 });
    let gate = crate::memory::work_gate();
    let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let r = load(bytes);
    // the file, the samples and the intermediate images are gone: give their pages back
    crate::memory::release();
    r
}

/// The modification time in `meta` as ISO 8601 (see [`ProbeInfo::file_modified`]); `None` when the
/// filesystem doesn't say or the time is before 1970.
fn modified_iso(meta: &std::fs::Metadata) -> Option<String> {
    let t = meta.modified().ok()?;
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
