//! A merge planned against its memory before any photo is decoded (issue #3708: the photos used
//! to be decoded first, and that could run out of memory before the merge was sized or refused).
//!
//! The plan works from each photo's size and file size ([`PhotoSize`]: the catalog's, else the
//! file's header) and follows the merge's phases in the order they run, with the bytes a pixel
//! each holds (see the constants): the decode (photos one at a time, each beside the frames
//! decoded before it; a preview's several at once when they fit), an HDR's merge (each bracket's,
//! for an HDR panorama) and the stitch ([`super::fit_with_floor`], which also sizes the panorama
//! after decoding, from the real frames). Just before each photo is decoded its header is read
//! again and the plan checked with it ([`holds`]).

use super::{
    MIN_PREVIEW_MEGAPIXELS, MergeKind, PANO_BLEND_BYTES, PANO_TILE_BYTES, PREVIEW_EDGE, fit_with_floor, mb, merge_budget, panorama_floor,
    round_megapixels,
};

/// Bytes per pixel of a decoded photo held as a frame (32-bit float RGB).
pub(crate) const FRAME_BYTES: f64 = 12.0;
/// What decoding a photo holds per pixel beside its frame while it decodes (photos are decoded at
/// full size, then made smaller): a raw's samples and normalised samples (2 + 4 bytes for a mosaic,
/// up to 12 + 12 for a 3-sample float DNG), or the copy its crop or turn makes (12). The file's
/// bytes come on top ([`PhotoSize::bytes`]), and a raw whose sensor has many more samples than its
/// picture has pixels holds [`SAMPLE_BYTES`] of each instead when that is more.
const DECODE_BYTES: f64 = 24.0;
/// What a raw holds per sensor sample while it decodes: the sample and its normalised copy (at most
/// a 4-byte float each).
const SAMPLE_BYTES: f64 = 8.0;
/// What making a decoded photo smaller holds per pixel of the full-size photo (the resampler's
/// intermediate), when it is made smaller.
const RESAMPLE_BYTES: f64 = 12.0;
/// An HDR merge holds per pixel of each frame: the frame (12), how near saturation it is (4) and,
/// aligned, which of its pixels are valid (1); and a frame's size of radiance (12) besides.
const HDR_FRAME_BYTES: f64 = 17.0;
/// While an HDR merge measures how near saturation each frame is (every frame at once), each holds
/// two maps of 4 bytes a pixel beside its frame (`lightcraft_merge::hdr::saturation`).
const SATURATION_BYTES: f64 = 8.0;
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
    /// The samples its decode reads: a raw's sensor (masked borders and every sample of a pixel
    /// included), else its pixels. The catalog doesn't keep a raw's: its pixels until the header
    /// is read just before the decode.
    pub samples: u64,
    /// The pixels of a raw's active area (its sensor without the masked borders): a decode may
    /// demosaic all of them, however small its default crop ([`PhotoSize::whole`]). Its pixels for
    /// anything else, and until the header is read for a raw the catalog has.
    pub active: u64,
    /// A full-size decode demosaics the whole active area, then crops it (an `OpcodeList3` beyond
    /// lens corrections, or a mosaic that isn't Bayer or several samples a pixel): only a Bayer
    /// mosaic without one demosaics just its default crop. A preview's decode always does.
    pub whole: bool,
}

impl PhotoSize {
    /// `None` for a size the catalog doesn't know (a pixel or less, or no file size).
    pub fn new(width: u32, height: u32, bytes: u64) -> Option<PhotoSize> {
        let px = u64::from(width) * u64::from(height);
        (px > 1 && bytes > 0).then_some(PhotoSize { width, height, bytes, samples: px, active: px, whole: false })
    }

    /// From the file's header, as an import reads it, without decoding pixels: `None` when it
    /// can't be read, or the format has no header reader (its "header" would be a decode).
    pub(crate) fn from_header(bytes: &[u8]) -> Option<PhotoSize> {
        use lightcraft_codecs::Format;
        let file = bytes.len() as u64;
        if lightcraft_raw::probe(bytes).is_some() {
            let info = lightcraft_raw::probe_info(bytes).ok()?;
            let (w, h) = info.developed_size();
            let size = PhotoSize::new(u32::try_from(w).ok()?, u32::try_from(h).ok()?, file)?;
            let sensor = (info.width as u64).saturating_mul(info.height as u64).saturating_mul(info.cpp.max(1) as u64);
            let active = (info.active_area.width as u64).saturating_mul(info.active_area.height as u64);
            // as `lightcraft_raw::RawImage::develop` decides (the merge drops lens corrections first)
            let (aw, ah) = (info.active_area.width, info.active_area.height);
            let windowed = lightcraft_raw::demosaic::ahd_windowable(info.cfa.as_ref(), info.cpp, aw, ah)
                && info.opcodes.list3.iter().all(lightcraft_raw::opcodes::Opcode::is_lens_correction);
            return Some(PhotoSize { samples: size.samples.max(sensor), active: size.active.max(active), whole: !windowed, ..size });
        }
        match lightcraft_codecs::sniff(bytes)? {
            Format::Jpeg | Format::Png | Format::Tiff | Format::WebP | Format::Psd | Format::Heif | Format::Gif | Format::Bmp | Format::Jxl => {}
            _ => return None,
        }
        let (w, h) = lightcraft_codecs::read_size(bytes).ok()?;
        PhotoSize::new(w, h, file)
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

    /// The bytes it holds at the peak of its decode, its frame included (`preview`: a preview's
    /// decode, which demosaics a raw's whole active area). A JPEG a preview decodes at a reduced
    /// DCT scale is still counted at full size: a deliberate margin, since the scale depends on details
    /// the catalog doesn't keep (the format, whether it's progressive).
    pub(crate) fn decoding(self, edge: Option<usize>, preview: bool) -> f64 {
        let resample = if self.reduced(edge) { RESAMPLE_BYTES } else { 0.0 };
        // a raw demosaicing more than its picture holds the demosaiced active area beside its
        // samples, and then beside the crop (its frame) made from it
        let demosaic = if (preview || self.whole) && self.active as f64 > self.px() { self.active as f64 * FRAME_BYTES } else { 0.0 };
        let decode = (self.px() * DECODE_BYTES).max(self.samples as f64 * SAMPLE_BYTES + demosaic);
        self.px() * (FRAME_BYTES + resample) + decode + self.bytes as f64
    }
}

/// The peak of decoding `sizes` one at a time, in order, each beside the frames before it.
fn decode_peak(sizes: &[PhotoSize], edge: Option<usize>) -> f64 {
    let (mut held, mut peak) = (0.0f64, 0.0f64);
    for s in sizes {
        peak = peak.max(held + s.decoding(edge, false));
        held += s.decoded(edge) * FRAME_BYTES;
    }
    peak.max(held)
}

/// What an HDR merge of frames of `px` pixels holds beyond the frames themselves, at the larger of
/// its two peaks: measuring saturation (every frame at once), and merging.
fn hdr_extra(px: &[f64], deghost: bool) -> f64 {
    let frames: f64 = px.iter().sum();
    let largest = px.iter().copied().fold(0.0, f64::max);
    let ghost = if deghost { px.iter().map(|&p| p.min(DEGHOST_EDGE * DEGHOST_EDGE)).sum::<f64>() * DEGHOST_BYTES } else { 0.0 };
    let merging = frames * (HDR_FRAME_BYTES - FRAME_BYTES) + largest * FRAME_BYTES + ghost;
    merging.max(frames * SATURATION_BYTES)
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

/// The fewest pixels a panorama's stitch gets from its photos decoded at most `edge`: an HDR
/// panorama's bracket left to EXIF could be any of [`brackets`], each making its own frames.
fn fewest_stitched(kind: &MergeKind, sizes: &[PhotoSize], edge: Option<usize>) -> f64 {
    let px: Vec<f64> = sizes.iter().map(|s| s.decoded(edge)).collect();
    match kind {
        MergeKind::Hdr { .. } => 0.0,
        MergeKind::Panorama { .. } => px.iter().sum(),
        MergeKind::HdrPanorama { bracket, .. } => brackets(*bracket, px.len())
            .into_iter()
            .map(|b| px.chunks(b).map(|set| set.iter().copied().fold(0.0, f64::max)).sum::<f64>())
            .fold(f64::INFINITY, f64::min),
    }
}

/// The merge's peak (bytes) after its photos are decoded at most `edge` (`decode`: the decode's
/// own peak), and the pixels its stitch gets (panoramas).
fn need_after(kind: &MergeKind, sizes: &[PhotoSize], edge: Option<usize>, decode: f64) -> (f64, Option<f64>) {
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

/// The merge's peak (bytes) with its photos decoded one at a time at most `edge`, and the pixels
/// its stitch gets (panoramas).
fn need(kind: &MergeKind, sizes: &[PhotoSize], edge: Option<usize>) -> (f64, Option<f64>) {
    need_after(kind, sizes, edge, decode_peak(sizes, edge))
}

/// The peak of a preview's decode: `threads` photos at once (the largest), else one at a time,
/// beside every preview frame.
fn preview_decode(sizes: &[PhotoSize], threads: usize) -> f64 {
    let edge = Some(PREVIEW_EDGE);
    let mut peaks: Vec<f64> = sizes.iter().map(|s| s.decoding(edge, true)).collect();
    peaks.sort_by(|a, b| b.total_cmp(a));
    let small: f64 = sizes.iter().map(|s| s.decoded(edge) * FRAME_BYTES).sum();
    peaks.iter().take(threads.max(1)).sum::<f64>() + small
}

fn pano_of(kind: &MergeKind) -> Option<&lightcraft_merge::PanoOptions> {
    match kind {
        MergeKind::Hdr { .. } => None,
        MergeKind::Panorama { pano } | MergeKind::HdrPanorama { pano, .. } => Some(pano),
    }
}

/// The share of its frames' pixels a panorama is taken to keep (their overlap aside) when the plan
/// decodes them smaller: they keep at least their floor's pixels over this (or the photos' own),
/// so the panorama of them still reaches its floor ([`super::reaches_floor`] checks it after the
/// stitch, when the overlap is known).
const PANORAMA_SHARE: f64 = 0.5;

/// The panorama's floor (megapixels, [`panorama_floor`]) when its stitch gets `photos` pixels at
/// the photos' own size (a preview's: at its size); `None` for an HDR.
fn floor_of(kind: &MergeKind, photos: Option<f64>) -> Option<f64> {
    pano_of(kind).map(|pano| panorama_floor(pano.max_megapixels, photos.unwrap_or(0.0)))
}

/// Whether a merge with its peak at `bytes` and a stitch of `stitch` pixels fits in `room`, its
/// panorama no smaller than `floor` ([`floor_of`]: the photos' own size decides it, not the frames
/// the plan made smaller).
fn fits_in(kind: &MergeKind, (bytes, stitch): (f64, Option<f64>), floor: Option<f64>, room: f64) -> bool {
    bytes <= room && stitch.zip(pano_of(kind)).zip(floor).is_none_or(|((px, pano), floor)| fit_with_floor(pano, px, floor, room).is_ok())
}

/// The bytes a merge with its peak at `bytes` and a stitch of `stitch` pixels needs at the least:
/// its peak, or its stitch at its floor ([`fit_with_floor`]'s refusal).
fn least_need((bytes, stitch): (f64, Option<f64>), floor: Option<f64>) -> f64 {
    let Some((px, floor)) = stitch.zip(floor) else { return bytes };
    let at_least = floor * 1e6;
    bytes.max((px * FRAME_BYTES + at_least * PANO_TILE_BYTES).max(at_least * PANO_BLEND_BYTES))
}

fn what(kind: &MergeKind, preview: bool) -> &'static str {
    match (kind, preview) {
        (_, true) => "merge preview",
        (MergeKind::Hdr { .. }, _) => "HDR merge",
        (MergeKind::Panorama { .. }, _) => "panorama",
        (MergeKind::HdrPanorama { .. }, _) => "HDR panorama",
    }
}

/// Plan a full-size merge of photos of `sizes` against `room` bytes (the merge's memory,
/// [`super::merge_memory`]) before any is decoded. `Ok(None)`: it fits at full size, and runs as it
/// always did. `Ok(Some(edge))`: a panorama's (or HDR panorama's) photos are decoded at most `edge`
/// px on their long side, as large as fits and still leaves the panorama as large as memory allows
/// (the result says `limit: "memory"`), and only while the panorama still reaches its floor
/// ([`panorama_floor`] of the photos' own size); an HDR keeps its photos' resolution or is refused.
/// Refused when not even [`MIN_PLANNED_EDGE`] fits, or the panorama can't reach its floor, naming
/// the memory needed and allowed.
pub(crate) fn plan_memory(kind: &MergeKind, sizes: &[PhotoSize], room: f64) -> Result<Option<usize>, String> {
    let native = need(kind, sizes, None).1;
    let floor = floor_of(kind, native);
    let fits = |edge: Option<usize>| fits_in(kind, need(kind, sizes, edge), floor, room);
    if fits(None) {
        return Ok(None);
    }
    let (Some(pano), Some(floor)) = (pano_of(kind), floor) else {
        // an HDR is its photos' resolution: not made smaller than them
        let how = format!("with its {} photos at full size,", sizes.len());
        return Err(not_enough(what(kind, false), &how, need(kind, sizes, None).0, room));
    };
    let long = sizes.iter().map(|s| s.long()).max().unwrap_or(0);
    // the smallest edge whose frames keep enough pixels for a panorama at its floor (memory bounds
    // the frames' size from above, below; this bounds it from below)
    // (for every bracket an HDR panorama's photos could be merged in)
    let enough = (floor * 1e6 / PANORAMA_SHARE).min(fewest_stitched(kind, sizes, None));
    let (mut below, mut keeps) = (0, long);
    while keeps - below > 1 {
        let mid = below + (keeps - below) / 2;
        if fewest_stitched(kind, sizes, Some(mid)) >= enough {
            keeps = mid;
        } else {
            below = mid;
        }
    }
    let least = MIN_PLANNED_EDGE.min(long).max(keeps);
    if least >= long || !fits(Some(least)) {
        // at the smallest frames: the photos' decode and merges, or the stitch at its floor
        let at = round_megapixels(floor);
        let how = if least < long {
            format!("even with its photos made {least} px on the long side and the panorama {at} MP, the smallest it is made,")
        } else {
            format!("even with the panorama {at} MP, the smallest it is made,")
        };
        return Err(not_enough(what(kind, false), &how, least_need(need(kind, sizes, Some(least)), Some(floor)), room));
    }
    // the largest edge whose frames fit and leave the stitch as large as it can be (what memory
    // allows for the blend, or the frames' own pixels, or what was asked): beyond it the frames
    // leave less room for the projected tiles ([`fit_with_floor`]), and the panorama comes out
    // smaller than with smaller frames
    let full = |edge: usize| {
        let px = need(kind, sizes, Some(edge)).1.unwrap_or(0.0);
        let largest = (room / PANO_BLEND_BYTES).min(px).min(pano.max_megapixels * 1e6);
        (room - px * FRAME_BYTES) / PANO_TILE_BYTES >= largest
    };
    let (mut lo, mut hi) = (least, long);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(Some(mid)) && full(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(Some(lo))
}

/// Plan a preview (its frames are [`PREVIEW_EDGE`] px, but each photo is decoded at full size
/// first, then merged or stitched): `Ok(true)` decodes `threads` photos at once, `Ok(false)` one at
/// a time; refused when not even that, or the merge after it, fits.
pub(crate) fn plan_preview(kind: &MergeKind, sizes: &[PhotoSize], room: f64, threads: usize) -> Result<bool, String> {
    holds(kind, sizes, room, Some(PREVIEW_EDGE), Some(threads))
        .map(|()| true)
        .or_else(|_| holds(kind, sizes, room, Some(PREVIEW_EDGE), Some(1)).map(|()| false))
}

/// Whether a merge of photos of `sizes` fits in `room` decoded at most `edge` px long, `threads`
/// at once for a preview (`None`: a full-size merge, one at a time): the plan, checked again just
/// before each photo is decoded with its header's size. Refused with the memory needed and allowed.
pub(crate) fn holds(kind: &MergeKind, sizes: &[PhotoSize], room: f64, edge: Option<usize>, threads: Option<usize>) -> Result<(), String> {
    let (n, floor) = match threads {
        // a preview's floor follows its own (small) frames, and is at most a preview's
        Some(t) => {
            let n = need_after(kind, sizes, edge, preview_decode(sizes, t));
            (n, floor_of(kind, n.1).map(|f| f.min(MIN_PREVIEW_MEGAPIXELS)))
        }
        None => (need(kind, sizes, edge), floor_of(kind, need(kind, sizes, None).1)),
    };
    if fits_in(kind, n, floor, room) {
        return Ok(());
    }
    let how = match (threads, edge) {
        (Some(1), _) => "even decoding one photo at a time,".to_owned(),
        (Some(t), _) => format!("decoding {t} photos at once,"),
        (None, Some(e)) if sizes.iter().any(|s| s.reduced(Some(e))) => format!("with its photos made {e} px on the long side,"),
        (None, _) => format!("with its {} photos at full size,", sizes.len()),
    };
    Err(not_enough(what(kind, threads.is_some()), &how, least_need(n, floor), room))
}

/// "not enough memory for this `what`: `how` it needs about N MB, but the merge's budget is M MB
/// (…); merge fewer or smaller photos" ([`merge_budget`]).
pub(crate) fn not_enough(what: &str, how: &str, need: f64, room: f64) -> String {
    format!(
        "not enough memory for this {what}: {how} it needs about {:.0} MB, {}; merge fewer or smaller photos",
        mb(need).ceil(),
        merge_budget(mb(room.max(0.0)).floor())
    )
}

/// `sizes` with each unknown one taken as the largest known (a merge's photos come from one
/// camera, as a rule); `None` when none is known: the merge then runs unplanned, as it did before
/// it was planned.
pub(crate) fn fill_unknown(sizes: &[Option<PhotoSize>]) -> Option<Vec<PhotoSize>> {
    let largest = sizes.iter().flatten().copied().max_by(|a, b| a.px().total_cmp(&b.px()))?;
    let bytes = sizes.iter().flatten().map(|s| s.bytes).max().unwrap_or(largest.bytes);
    let samples = sizes.iter().flatten().map(|s| s.samples).max().unwrap_or(largest.samples);
    let active = sizes.iter().flatten().map(|s| s.active).max().unwrap_or(largest.active);
    let whole = sizes.iter().flatten().any(|s| s.whole);
    Some(sizes.iter().map(|s| s.unwrap_or(PhotoSize { bytes, samples, active, whole, ..largest })).collect())
}
