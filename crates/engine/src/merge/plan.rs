//! A merge planned against its memory before any photo is decoded (issue #3708: the photos used
//! to be decoded first, and that could run out of memory before the merge was sized or refused).
//!
//! The plan works from each photo's size and file size ([`PhotoSize`]: the catalog's, else the
//! file's header) and follows the merge's phases in the order they run, with the bytes a pixel
//! each holds (see the constants): the decode (photos one at a time, each beside the frames
//! decoded before it), an HDR's merge (each bracket's, for an HDR panorama) and the stitch
//! ([`super::fit_in_memory`], which also sizes the panorama after decoding, from the real frames).

use super::{MIN_PANORAMA_MEGAPIXELS, MergeKind, PANO_BLEND_BYTES, PANO_TILE_BYTES, PREVIEW_EDGE, fit_in_memory, mb};

/// Bytes per pixel of a decoded photo held as a frame (32-bit float RGB).
pub(crate) const FRAME_BYTES: f64 = 12.0;
/// What decoding a photo holds per pixel beside its frame while it decodes (photos are decoded at
/// full size, then made smaller): a raw's samples and normalised samples (2 + 4 bytes for a mosaic,
/// up to 12 + 12 for a 3-sample float DNG), or the copy its crop or turn makes (12). The file's
/// bytes come on top ([`PhotoSize::bytes`]).
const DECODE_BYTES: f64 = 24.0;
/// What making a decoded photo smaller holds per pixel of the full-size photo (the resampler's
/// intermediate), when it is made smaller.
const RESAMPLE_BYTES: f64 = 12.0;
/// An HDR merge holds per pixel of each frame: the frame (12), how near saturation it is (4) and,
/// aligned, which of its pixels are valid (1); and a frame's size of radiance (12) besides.
const HDR_FRAME_BYTES: f64 = 17.0;
/// Deghosting holds each frame at most this long edge, in RGB with a weight (16 bytes) and a map (4).
const DEGHOST_EDGE: f64 = 2048.0;
const DEGHOST_BYTES: f64 = 20.0;

/// The smallest long edge [`plan_memory`] decodes a panorama's photos to (a preview's frames, whose
/// features are still found); a merge that doesn't fit with them is refused.
pub(crate) const MIN_PLANNED_EDGE: usize = PREVIEW_EDGE;

/// A photo's size, known before it is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhotoSize {
    pub width: u32,
    pub height: u32,
    /// The file's bytes (held while it decodes).
    pub bytes: u64,
}

impl PhotoSize {
    /// `None` for a size the catalog doesn't know (a pixel or less, or no file size).
    pub fn new(width: u32, height: u32, bytes: u64) -> Option<PhotoSize> {
        (u64::from(width) * u64::from(height) > 1 && bytes > 0).then_some(PhotoSize { width, height, bytes })
    }

    /// From the file's header, as an import reads it, without decoding pixels: `None` when it
    /// can't be read, or the format has no header reader (its "header" would be a decode).
    pub(crate) fn from_header(bytes: &[u8]) -> Option<PhotoSize> {
        use lightcraft_codecs::Format;
        let (w, h) = if lightcraft_raw::probe(bytes).is_some() {
            let (w, h) = lightcraft_raw::probe_info(bytes).ok()?.developed_size();
            (u32::try_from(w).ok()?, u32::try_from(h).ok()?)
        } else {
            match lightcraft_codecs::sniff(bytes)? {
                Format::Jpeg | Format::Png | Format::Tiff | Format::WebP | Format::Psd | Format::Heif | Format::Gif | Format::Bmp => {}
                _ => return None,
            }
            let h = lightcraft_codecs::read_header(bytes).ok()?;
            (h.width, h.height)
        };
        PhotoSize::new(w, h, bytes.len() as u64)
    }

    fn px(self) -> f64 {
        f64::from(self.width) * f64::from(self.height)
    }

    fn long(self) -> usize {
        self.width.max(self.height) as usize
    }

    fn reduced(self, edge: Option<usize>) -> bool {
        edge.is_some_and(|e| e < self.long())
    }

    /// The pixels of its frame decoded at most `edge` px on the long side (rounded as the
    /// resampler, `lightcraft_raster::resample::fit`, rounds them).
    pub(crate) fn decoded(self, edge: Option<usize>) -> f64 {
        let Some(e) = edge.filter(|_| self.reduced(edge)) else { return self.px() };
        let s = e as f64 / self.long() as f64;
        let side = |v: u32| (f64::from(v) * s).round().max(1.0);
        side(self.width) * side(self.height)
    }

    /// The bytes it holds at the peak of its decode, its frame included.
    fn decoding(self, edge: Option<usize>) -> f64 {
        let resample = if self.reduced(edge) { RESAMPLE_BYTES } else { 0.0 };
        self.px() * (FRAME_BYTES + DECODE_BYTES + resample) + self.bytes as f64
    }
}

/// The peak of decoding `sizes` one at a time, in order, each beside the frames before it.
fn decode_peak(sizes: &[PhotoSize], edge: Option<usize>) -> f64 {
    let (mut held, mut peak) = (0.0f64, 0.0f64);
    for s in sizes {
        peak = peak.max(held + s.decoding(edge));
        held += s.decoded(edge) * FRAME_BYTES;
    }
    peak.max(held)
}

/// What an HDR merge of frames of `px` pixels holds beyond the frames themselves.
fn hdr_extra(px: &[f64], deghost: bool) -> f64 {
    let frames: f64 = px.iter().sum();
    let largest = px.iter().copied().fold(0.0, f64::max);
    let ghost = if deghost { px.iter().map(|&p| p.min(DEGHOST_EDGE * DEGHOST_EDGE)).sum::<f64>() * DEGHOST_BYTES } else { 0.0 };
    frames * (HDR_FRAME_BYTES - FRAME_BYTES) + largest * FRAME_BYTES + ghost
}

/// The brackets an HDR panorama of `n` photos may be merged in: the one asked for, else (from
/// EXIF, after decoding) every size that makes at least two brackets of at least two photos.
fn brackets(asked: usize, n: usize) -> Vec<usize> {
    if asked >= 2 {
        return vec![asked];
    }
    let all: Vec<usize> = (2..=n / 2).filter(|b| n.is_multiple_of(*b)).collect();
    if all.is_empty() { vec![2] } else { all }
}

/// The merge's peak (bytes) with its photos decoded at most `edge`, and the pixels its stitch
/// gets (panoramas).
fn need(kind: &MergeKind, sizes: &[PhotoSize], edge: Option<usize>) -> (f64, Option<f64>) {
    let decode = decode_peak(sizes, edge);
    let px: Vec<f64> = sizes.iter().map(|s| s.decoded(edge)).collect();
    match kind {
        MergeKind::Hdr { hdr } => {
            let frames: f64 = px.iter().sum::<f64>() * FRAME_BYTES;
            (decode.max(frames + hdr_extra(&px, hdr.deghost != lightcraft_merge::Deghost::None)), None)
        }
        MergeKind::Panorama { .. } => (decode, Some(px.iter().sum())),
        MergeKind::HdrPanorama { hdr, bracket, .. } => {
            let (mut peak, mut stitched) = (decode, 0.0f64);
            for b in brackets(*bracket, px.len()) {
                // each bracket is merged beside the photos not merged yet and the merges before it
                let (mut held, mut merged) = (px.iter().sum::<f64>() * FRAME_BYTES, 0.0);
                for set in px.chunks(b) {
                    peak = peak.max(held + merged + hdr_extra(set, hdr.deghost != lightcraft_merge::Deghost::None));
                    held -= set.iter().sum::<f64>() * FRAME_BYTES;
                    merged += set.iter().copied().fold(0.0, f64::max) * FRAME_BYTES;
                }
                stitched = stitched.max(merged / FRAME_BYTES);
            }
            (peak, Some(stitched))
        }
    }
}

/// Plan a full-size merge of photos of `sizes` against `room` bytes (the merge's memory,
/// [`super::merge_memory`]) before any is decoded. `Ok(None)`: it fits at full size, and runs as it
/// always did. `Ok(Some(edge))`: a panorama's (or HDR panorama's) photos are decoded at most `edge`
/// px on their long side, as large as fits beside a panorama as large as they are (the result says
/// `limit: "memory"`); an HDR keeps its photos' resolution or is refused. Refused when not even
/// [`MIN_PLANNED_EDGE`] fits, naming the memory needed and allowed.
pub(crate) fn plan_memory(kind: &MergeKind, sizes: &[PhotoSize], room: f64) -> Result<Option<usize>, String> {
    let pano = match kind {
        MergeKind::Hdr { .. } => None,
        MergeKind::Panorama { pano } | MergeKind::HdrPanorama { pano, .. } => Some(pano),
    };
    let fits = |edge: Option<usize>| {
        let (bytes, stitch) = need(kind, sizes, edge);
        bytes <= room && stitch.zip(pano).is_none_or(|(px, pano)| fit_in_memory(pano, px, room).is_ok())
    };
    if fits(None) {
        return Ok(None);
    }
    let what = match kind {
        MergeKind::Hdr { .. } => "HDR merge",
        MergeKind::Panorama { .. } => "panorama",
        MergeKind::HdrPanorama { .. } => "HDR panorama",
    };
    let Some(pano) = pano else {
        // an HDR is its photos' resolution: not made smaller than them
        return Err(not_enough(what, &format!("with its {} photos at full size,", sizes.len()), need(kind, sizes, None).0, room));
    };
    let long = sizes.iter().map(|s| s.long()).max().unwrap_or(0);
    let least = MIN_PLANNED_EDGE.min(long);
    if least >= long || !fits(Some(least)) {
        // at the smallest frames: the photos' decode and merges, or the stitch at its least
        let (bytes, stitch) = need(kind, sizes, Some(least));
        let at_least = MIN_PANORAMA_MEGAPIXELS.min(pano.max_megapixels).max(0.1) * 1e6;
        let stitch = stitch.map_or(0.0, |px| (px * FRAME_BYTES + at_least * PANO_TILE_BYTES).max(at_least * PANO_BLEND_BYTES));
        let how = if least < long { format!("even with its photos made {least} px on the long side,") } else { "even at its least,".to_owned() };
        return Err(not_enough(what, &how, bytes.max(stitch), room));
    }
    // the largest edge whose frames fit beside a panorama as large as they are, else the smallest
    let stitched = |edge: usize| need(kind, sizes, Some(edge)).1.unwrap_or(0.0);
    let (mut lo, mut hi) = (least, long);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(Some(mid)) && stitched(mid) * PANO_BLEND_BYTES <= room {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(Some(lo))
}

/// Plan a preview's decode (its frames are [`PREVIEW_EDGE`] px, but each photo is decoded at full
/// size first): `Ok(true)` decodes `threads` photos at once, `Ok(false)` one at a time; refused
/// when not even one photo's decode fits.
pub(crate) fn plan_preview(sizes: &[PhotoSize], room: f64, threads: usize) -> Result<bool, String> {
    let edge = Some(PREVIEW_EDGE);
    let mut peaks: Vec<f64> = sizes.iter().map(|s| s.decoding(edge)).collect();
    peaks.sort_by(|a, b| b.total_cmp(a));
    let small: f64 = sizes.iter().map(|s| s.decoded(edge) * FRAME_BYTES).sum();
    let at_once: f64 = peaks.iter().take(threads.max(1)).sum::<f64>() + small;
    if at_once <= room {
        return Ok(true);
    }
    let one = peaks.first().copied().unwrap_or(0.0) + small;
    if one <= room {
        return Ok(false);
    }
    Err(not_enough("merge preview", "even decoding one photo at a time,", one, room))
}

/// "not enough memory for this `what`: `how` it needs about N MB, and the merge may use M MB; …"
pub(crate) fn not_enough(what: &str, how: &str, need: f64, room: f64) -> String {
    format!(
        "not enough memory for this {what}: {how} it needs about {:.0} MB, and the merge may use {:.0} MB; merge fewer or smaller photos, or close other apps",
        mb(need).ceil(),
        mb(room.max(0.0)).floor()
    )
}

/// `sizes` with each unknown one taken as the largest known (a merge's photos come from one
/// camera, as a rule); `None` when none is known: the merge then runs unplanned, as it did before
/// it was planned.
pub(crate) fn fill_unknown(sizes: &[Option<PhotoSize>]) -> Option<Vec<PhotoSize>> {
    let largest = sizes.iter().flatten().copied().max_by(|a, b| a.px().total_cmp(&b.px()))?;
    let bytes = sizes.iter().flatten().map(|s| s.bytes).max().unwrap_or(largest.bytes);
    Some(sizes.iter().map(|s| s.unwrap_or(PhotoSize { bytes, ..largest })).collect())
}
