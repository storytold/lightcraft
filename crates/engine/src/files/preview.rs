use super::fs::{Planned, quick_image_srgb, raf_preview_extent, read_planned, read_prefix, read_prefix_at};
use super::{Arc, Filter, Orientation, PreviewLoader, Rgb32f, SourceInfo, fit};

/// Orientation for an embedded preview: its own EXIF orientation when it has one, else the raw file's.
pub(super) fn preview_orientation(raw_bytes: &[u8], jpeg_orientation: u16) -> Orientation {
    if jpeg_orientation > 1 {
        return Orientation::from_exif(jpeg_orientation);
    }
    lightcraft_meta::extract(raw_bytes).orientation.unwrap_or(Orientation::Normal)
}

/// What stands in for a raw file we can't decode: its largest embedded JPEG (with `covering`, the smallest one that is
/// at least that big, [`lightcraft_raw::embedded_preview_covering`]), else, for a TIFF-based raw, the file's own first
/// image when the TIFF reader can show it (a reduced RGB copy: the only preview some containers carry). Returns image
/// bytes for [`lightcraft_codecs::decode`].
fn stand_in_image(bytes: &[u8], covering: Option<usize>) -> Option<std::borrow::Cow<'_, [u8]>> {
    let jpeg = match covering {
        Some(edge) => lightcraft_raw::embedded_preview_covering(bytes, u32::try_from(edge).unwrap_or(u32::MAX)),
        None => lightcraft_raw::embedded_preview(bytes),
    };
    if let Some(jpeg) = jpeg {
        return Some(std::borrow::Cow::Owned(jpeg));
    }
    let undecodable = lightcraft_raw::probe(bytes).is_some_and(|f| !f.is_supported());
    (undecodable && lightcraft_codecs::sniff(bytes).is_some_and(|f| f == lightcraft_codecs::Format::Tiff))
        .then_some(std::borrow::Cow::Borrowed(bytes))
}

/// Shared colour interpretation for quick previews, unsupported RAW fallback and camera-look fitting (`covering`: see
/// [`stand_in_image`]).
pub(crate) fn decode_raw_preview(
    bytes: &[u8],
    opts: lightcraft_codecs::DecodeOptions,
    covering: Option<usize>,
) -> Option<lightcraft_codecs::Decoded> {
    let jpeg = stand_in_image(bytes, covering)?;
    // the colour space is a property of the JPEG; a TIFF stand-in (the file itself, or a Hasselblad RGB preview
    // wrapped as a TIFF) and a JPEG XL preview are read by their own decoders
    let is_jpeg = jpeg.starts_with(&[0xff, 0xd8]);
    match lightcraft_raw::embedded_preview_color_space(bytes).filter(|_| is_jpeg) {
        Some(space) => {
            let space = match space {
                lightcraft_raw::PreviewColorSpace::Srgb => lightcraft_codecs::NamedSpace::Srgb,
                lightcraft_raw::PreviewColorSpace::AdobeRgb => lightcraft_codecs::NamedSpace::AdobeRgb,
            };
            lightcraft_codecs::decode_jpeg_with_fallback(&jpeg, opts, space).ok()
        }
        None => lightcraft_codecs::decode(&jpeg, opts).ok(),
    }
}

/// Oriented size of the embedded preview of a raw file.
pub(super) fn embedded_preview_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let jpeg = stand_in_image(bytes, None)?;
    // headers only, as for other images (issue #367): decoding a full-size preview just for its
    // size took about a second per file in an import; a decode remains the fallback
    let (mut w, mut h, orientation) = match lightcraft_codecs::read_header(&jpeg) {
        Ok(header) => (header.width, header.height, header.orientation),
        Err(_) => {
            let d = lightcraft_codecs::decode(&jpeg, lightcraft_codecs::DecodeOptions::fit(64, 64)).ok()?;
            (d.source_width, d.source_height, d.orientation)
        }
    };
    if preview_orientation(bytes, orientation).swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    Some((w, h))
}

/// The embedded preview of a raw file as a working-space image no larger than `max_edge`, oriented.
pub fn load_embedded_preview(bytes: &[u8], max_edge: usize) -> Option<(Rgb32f, SourceInfo)> {
    // always the largest JPEG: a source's geometry must not depend on its size level (previews match exports)
    let d = decode_raw_preview(bytes, fit_box(max_edge), None)?;
    let img = d.to_working();
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Some((img.oriented(preview_orientation(bytes, d.orientation)), SourceInfo::default()))
}

/// The embedded preview of a raw file for display (sRGB, oriented, no larger than `max_edge`): the
/// loupe and grid show it until the raw itself has been developed ([`crate::media::QuickJob`]). The smallest
/// embedded JPEG that covers `max_edge` is decoded, not the largest.
pub fn embedded_preview_srgb(bytes: &[u8], max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    let mut d = decode_raw_preview(bytes, fit_box(max_edge), Some(max_edge))?;
    if d.image.width.max(d.image.height) > max_edge {
        d.image = fit(&d.image, max_edge, max_edge, Filter::Box);
        d.alpha = None;
    }
    let o = preview_orientation(bytes, d.orientation);
    Some(d.to_srgb8().oriented(o))
}

/// Decode options fitting into a `max_edge` square; `usize::MAX` (a full-size load) or any edge past
/// `u32::MAX` asks for the largest box rather than wrapping to a small one.
pub(super) fn fit_box(max_edge: usize) -> lightcraft_codecs::DecodeOptions {
    let e = u32::try_from(max_edge).unwrap_or(u32::MAX);
    lightcraft_codecs::DecodeOptions::fit(e, e)
}

/// Filesystem-backed embedded-preview hook (native).
///
/// The quick path reads the file's header and its embedded preview and nothing else (see
/// [`preview_window`]); a container the header cannot point into is planned from its tags
/// (`lightcraft_raw::sparse::preview_plan`, the same module the import path probes with), and only a
/// file the planner cannot describe falls back to the whole-file path. The memory gate still counts
/// the whole file, which is what the decode of one can hold.
pub fn fs_preview_loader() -> PreviewLoader {
    Arc::new(|path: &str, max_edge: usize| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(len.saturating_mul(2)) } else { gate.acquire_urgent(len.saturating_mul(2)) };
        if let Some(image) = preview_window(path, max_edge)
            .ok()
            .flatten()
            .and_then(|(header, window, read)| quick_image_srgb(&header, &window, read.adobe_rgb, max_edge))
        {
            return Some(image);
        }
        embedded_preview_srgb(&std::fs::read(path).ok()?, max_edge)
    })
}

/// What [`preview_window`] read from a file, so a caller can see what a preview cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewRead {
    /// Bytes read.
    pub read: u64,
    /// The file's size.
    pub total: u64,
    /// Whether the preview came from a bounded read (`false`: the whole file was read, which is what
    /// every preview cost before the header path existed).
    pub header_path: bool,
    /// Whether the sparse planner named the ranges, rather than a format's own header fields: the
    /// path a Canon CR3 and the TIFF raws whose preview tag sits past the prefix take (`false` on
    /// the header path, and on the whole-file fallback).
    pub planned: bool,
    /// Adobe RGB when the container says the preview is in it and the JPEG doesn't say otherwise
    /// (`decode_raw_preview` applies the same declaration): the quick decode applies it too.
    pub adobe_rgb: bool,
}

impl PreviewRead {
    /// The share of the file that was read, `0.0`..=`1.0`.
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.read as f64 / self.total as f64).clamp(0.0, 1.0)
    }
}

/// The file's header and its embedded preview, read without the rest of the file: `(header bytes,
/// preview bytes, what was read)`. `None` when the header points at no preview this build can find,
/// which is the caller's cue to read the file whole. `max_edge` is the size the preview is for: where a file keeps
/// several JPEGs (the plan route), the window is the one [`embedded_preview_srgb`] decodes for that size, the smallest
/// that covers it ([`lightcraft_raw::embedded_preview_covering`]).
///
/// Why it exists: a 56 MB Fuji RAF keeps its JPEG in the first 5.6 MB (measured: `DSCF9492.RAF`),
/// so reading the file whole to extract it costs ten times the bytes — and on a network share ten
/// times the wait (measured on that file: 2.5 s local against 25.6 s over SMB). `embedded_preview`
/// still needs the whole container, so the header comes back with the window; for a preview taken
/// out of a container that carries no preview colour space of its own (Nikon's maker note) the
/// whole-file fallback stays in charge.
pub fn preview_window(path: &str, max_edge: usize) -> Result<Option<(Vec<u8>, Vec<u8>, PreviewRead)>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let total = file.metadata().map_err(|e| format!("{path}: {e}"))?.len();
    let mut prefix = read_prefix(&mut file, path, total)?;

    // What the header itself declares, before the container reader is asked: it clamps a slice to
    // the buffer it was given, so a prefix that stops inside the declared preview yields a
    // self-consistently short range that looks trustworthy (measured on a real 56 MB DSCF9492.RAF:
    // a 4 KiB prefix reports 3948 bytes where the file holds 5.6 MB). Reading the declared extent
    // first costs nothing — it lives in the first bytes of every format covered here.
    if let Some(extent) = raf_preview_extent(&prefix)
        && extent > prefix.len() as u64
    {
        prefix = read_prefix_at(&mut file, path, extent.min(total))?;
    }
    let Some(range) = lightcraft_raw::bounded_preview(&prefix) else {
        // The header carries no pointer this path reads — a Canon CR3's boxes, a TIFF raw whose
        // preview tag sits past the prefix, the containers whose preview only a scan finds: ask the
        // sparse planner which ranges `embedded_preview` reads before handing the caller the
        // whole-file fallback.
        return planned_preview(&mut file, path, total, max_edge);
    };
    // A preview at the file's end may have been padded past it: clamp rather than fail.
    let end = range.offset.saturating_add(range.window_len).min(total);
    let len = usize::try_from(end.saturating_sub(range.offset)).map_err(|_| format!("{path}: preview too large"))?;
    // When the prefix already covers the range (the usual case: the prefix was grown to the declared
    // extent), take the window out of it. Reading those bytes twice would double a 5.6 MB preview.
    let prefix_len = prefix.len() as u64;
    let from_prefix = usize::try_from(range.offset).ok().and_then(|start| prefix.get(start..start.checked_add(len)?)).map(<[u8]>::to_vec);
    let (window, bytes_read) = match from_prefix {
        Some(window) => (window, prefix_len),
        None => {
            let mut window = vec![0u8; len];
            file.seek(SeekFrom::Start(range.offset)).map_err(|e| format!("{path}: {e}"))?;
            file.read_exact(&mut window).map_err(|e| format!("{path}: {e}"))?;
            (window, prefix_len + len as u64)
        }
    };

    // A lying pointer must not cost the user their preview: hand the caller the whole-file fallback.
    if lightcraft_raw::embedded_preview_from_range(&window, range).is_none() {
        return Ok(None);
    }
    let read = PreviewRead { read: bytes_read, total, header_path: true, planned: false, adobe_rgb: false };
    // On a network share the bytes are the wait, so say what this preview actually cost. A window
    // taken from the prefix cost nothing beyond the prefix itself.
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] preview {path}: read {} of {} bytes (header {}, window {})", read.read, total, prefix_len, window.len());
    }
    Ok(Some((prefix, window, read)))
}

/// The most times the sparse planner may name ranges before the preview gives up: the plan is
/// monotone (it never asks for a range twice), so a real file needs a handful.
const MAX_PREVIEW_PLAN_ROUNDS: usize = 8;

/// Tenths of a file the planner may have read before the preview gives up and the whole-file path
/// takes over. The import path bounds its own loop the same way; a "preview" that wants most of the
/// file has stopped being one.
const MAX_PREVIEW_PLAN_SHARE: u64 = 6;

/// The largest file the planner path builds its buffer for. The raws it serves are far below this;
/// a file past it is read whole as before rather than allocating its length up front.
const MAX_PREVIEW_PLAN_BYTES: u64 = 1 << 30;

/// The header the quick path is handed on the planner route. The header route grows its prefix to
/// the preview's declared extent, so `quick_image_srgb` can read the file's own EXIF orientation out
/// of it when the preview JPEG carries none; this route reads a bounded prefix instead, big enough
/// for IFD0 (which is where that tag lives) and for a CR3's metadata boxes.
pub(super) const PLAN_PREFIX: u64 = 64 * 1024;

/// The preview of a container the header-only path cannot answer for, from the byte ranges
/// [`lightcraft_raw::sparse::preview_plan`] says [`lightcraft_raw::embedded_preview`] reads.
///
/// The planner is asked about a buffer of the file's whole length in which only the ranges named so
/// far hold bytes, and only those ranges are read: the sensor data in the bulk ranges is not. The
/// buffer is the file's length, which the loader's memory gate already counts, and its untouched
/// pages stay zero and are never written — though on macOS a freed buffer of that size may stay in
/// the allocator's cache rather than going back to the OS (measured on a 38k-photo import, #832), so
/// this path's cost is the bytes read, not a promise about RSS.
///
/// `None` — the caller's cue to read the file whole, exactly as when the header declines — when the
/// planner says it cannot describe the file (NEF, ORF and PEF probe from samples; a structure it
/// does not trust), when the ranges it names do not fit the buffer or want most of the file, or when
/// the quick path could not reproduce what the whole-file path shows (the two container facts
/// below). The planner's contract is the one its corpus test asserts over real files (what it loaded
/// gives what the whole file gives), so a window that still fails to decode ends in the same
/// whole-file fallback in [`fs_preview_loader`], never in a wrong picture.
fn planned_preview(file: &mut std::fs::File, path: &str, total: u64, max_edge: usize) -> Result<Option<(Vec<u8>, Vec<u8>, PreviewRead)>, String> {
    if total > MAX_PREVIEW_PLAN_BYTES {
        return Ok(None);
    }
    let Ok(len) = usize::try_from(total) else { return Ok(None) };
    let mut buf = vec![0u8; len];
    let header = read_prefix_at(file, path, PLAN_PREFIX.min(total))?;
    let Some(head) = buf.get_mut(..header.len()) else { return Ok(None) };
    head.copy_from_slice(&header);
    let mut loaded = lightcraft_raw::sparse::Ranges::new();
    loaded.insert(0, header.len() as u64);
    let planned = read_planned(file, &mut buf, &mut loaded, MAX_PREVIEW_PLAN_ROUNDS, MAX_PREVIEW_PLAN_SHARE, lightcraft_raw::sparse::preview_plan)
        .map_err(|e| format!("{path}: {e}"))?;
    if planned != Planned::Ready {
        return Ok(None);
    }
    // the plan loads every preview's strips, so the JPEG picked for `max_edge` is the whole file's pick
    let edge = u32::try_from(max_edge).unwrap_or(u32::MAX);
    let Some(window) = lightcraft_raw::embedded_preview_covering(&buf, edge) else { return Ok(None) };
    // The quick path hands the JPEG to the decoder alone, so what the *container* says
    // about the preview has to be something the bytes it is handed can show. Two container
    // facts change the pixels and are not in the JPEG: the preview's colour space (which
    // the whole-file path applies through `decode_raw_preview`) and, when the JPEG carries
    // no orientation of its own, the file's EXIF orientation. The colour space is read
    // here and handed on ([`PreviewRead::adobe_rgb`]: a Sony α1 writes Adobe RGB previews,
    // which used to send each of them to the whole-file path, issue #1440); a container
    // that says sRGB agrees with the JPEG-only decode (its own declaration, else sRGB).
    // Where the orientation is present and the header cannot answer for it, the
    // whole-file path keeps the preview.
    let adobe_rgb = lightcraft_raw::embedded_preview_color_space(&buf) == Some(lightcraft_raw::PreviewColorSpace::AdobeRgb);
    // `extract` copies a TIFF's bytes to hand on its Exif block, so ask the cheap side
    // first: a JPEG that carries its own orientation needs nothing from the container.
    if lightcraft_meta::extract(&window).orientation.is_none()
        && lightcraft_meta::extract(&header).orientation.is_none()
        && lightcraft_meta::extract(&buf).orientation.is_some()
    {
        return Ok(None);
    }
    let read = PreviewRead { read: loaded.total(), total, header_path: true, planned: true, adobe_rgb };
    // A plan-driven preview is the one that used to cost the whole file, so it is worth
    // saying what it cost now too.
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] preview {path}: read {} of {} bytes (plan, window {})", read.read, total, window.len());
    }
    // The planner's buffer is the file's length, so give its pages back the way a decode
    // does (see `fs_pair_loader`).
    drop(buf);
    crate::memory::release();
    Ok(Some((header, window, read)))
}
