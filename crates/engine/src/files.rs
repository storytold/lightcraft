//! Real files: probing (dimensions + metadata for import) and loading (decode → linear Rec.2020,
//! oriented, downscaled to the requested level). Standard formats go through `lcc-codecs`,
//! camera raws through `lcc-raw` (demosaic + DNG colour model).

use std::sync::Arc;

use lcc_catalog::{MediaKind, Meta};
use lcc_color::cct::xy_to_temp_tint;
use lcc_geom::Orientation;
use lcc_pipeline::SourceInfo;
use lcc_raster::Rgb32f;
use lcc_raster::resample::{Filter, fit};

use crate::media::{DenoiseSpec, FileLoader, FileProbe, LoadOptions, PairLoader, PreviewLoader, ProbeInfo, ReducedLoader};

fn meta_of(m: &lcc_meta::Metadata) -> (Meta, Option<String>) {
    // the camera's own wording, to the tenth of a second: `1/250`, `0.4`, `1.3″` (issue #1619: rounding
    // `1/0.4` to `1/2` both showed and exported the wrong time)
    let shutter = m.exposure_time.and_then(lcc_meta::exposure_display_of).unwrap_or_default();
    let camera_make = m.make.clone().unwrap_or_default();
    let camera_model = m.model.clone().unwrap_or_default();
    let meta = Meta {
        camera: lcc_catalog::camera_label(&camera_make, &camera_model),
        camera_make,
        camera_model,
        camera_serial: m.serial_number.as_deref().map(str::trim).unwrap_or_default().chars().take(64).collect(),
        lens_make: m.lens_make.clone().unwrap_or_default(),
        // (a camera that writes no lens name, like Nikon's DSLRs, still says what range the lens covers)
        lens: m.lens_model.clone().or_else(|| m.lens_spec.and_then(lcc_meta::lens_spec_name)).unwrap_or_default(),
        focal_mm: m.focal_length.map(|f| f as f32),
        aperture: m.f_number.map(|f| f as f32),
        shutter,
        iso: m.iso,
        flash: m.flash.as_ref().map(|f| f.fired),
        exposure_bias: m.exposure_bias.map(|v| v as f32).filter(|v| v.is_finite()),
        lens_range: m.lens_spec.map(|s| (s[0] as f32, s[1] as f32)).filter(|(a, b)| a.is_finite() && b.is_finite() && *a > 0.0 && *b >= *a),
        location: m.sublocation.clone().unwrap_or_default(),
        city: m.city.clone().unwrap_or_default(),
        state: m.state.clone().unwrap_or_default(),
        country: m.country.clone().unwrap_or_default(),
        country_code: m.country_code.clone().unwrap_or_default(),
        alt_text: m.alt_text.clone().unwrap_or_default(),
        extended_description: m.extended_description.clone().unwrap_or_default(),
        gps: m.gps.as_ref().map(|g| (g.latitude, g.longitude)),
        gps_removed: None,
        title: m.title.clone().unwrap_or_default(),
        headline: m.headline.clone().unwrap_or_default(),
        caption: m.caption.clone().unwrap_or_default(),
        description_writer: m.description_writer.clone().unwrap_or_default(),
        category: m.category.clone().unwrap_or_default(),
        supplemental_categories: m.supplemental_categories.clone(),
        subject_code: m.subject_code.clone(),
        copyright: m.copyright.clone().unwrap_or_default(),
        copyright_status: lcc_catalog::CopyrightStatus::from_marked(m.copyright_marked),
        usage_terms: m.usage_terms.clone().unwrap_or_default(),
        copyright_url: m.copyright_url.clone().unwrap_or_default(),
        creator: m.artist.clone().unwrap_or_default(),
        contact: m.contact.clone(),
        // paths from `lr:hierarchicalSubject`, names from `dc:subject` that aren't their levels
        keywords: lcc_catalog::keywords::from_file(&m.keywords, &m.hierarchical_keywords),
        regions: m.regions.clone(),
        focus: m.focus.clone(),
    };
    (meta, m.capture_time.as_ref().map(|d| d.to_iso()))
}

/// Lens corrections embedded in a raw's `OpcodeList3` (`WarpRectilinear`, `WarpRectilinear2`, `FixVignetteRadial`: a DNG's own, or the
/// raw reader's equivalent of the camera's correction, e.g. Panasonic / Leica RW2 distortion), re-expressed for the
/// default-cropped, EXIF-oriented image. These are the only "profile" corrections the app applies.
pub fn embedded_lens(raw: &lcc_raw::RawInfo) -> Option<lcc_develop::EmbeddedLens> {
    use lcc_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedWarp, EmbeddedWarpStage, WARP_STAGES};
    use lcc_geom::Point;
    use lcc_raw::Opcode;
    let (aw, ah) = (raw.active_area.width as f64, raw.active_area.height as f64);
    if aw < 2.0 || ah < 2.0 {
        return None;
    }
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let (cx0, cy0, cw, ch) =
        if c.width == 0 || c.height == 0 { (0.0, 0.0, aw, ah) } else { (c.x as f64, c.y as f64, c.width as f64, c.height as f64) };
    let long = cw.max(ch);
    // opcode centres are relative to the (uncropped) active area, in pixel-index units
    let centre = |rel: [f64; 2]| -> (Point, f64) {
        let (px, py) = (rel[0] * (aw - 1.0), rel[1] * (ah - 1.0));
        let m = [(0.0, 0.0), (aw - 1.0, 0.0), (0.0, ah - 1.0), (aw - 1.0, ah - 1.0)]
            .iter()
            .map(|&(x, y)| (x - px).hypot(y - py))
            .fold(0.0, f64::max)
            .max(1e-9);
        (Point::new((px + 0.5 - cx0) / cw, (py + 0.5 - cy0) / ch), m / long)
    };
    let mut lens = EmbeddedLens::default();
    // opcodes in list order, minus the ones the DNG 1.6 skip rule drops (a WarpRectilinear that follows an optional
    // WarpRectilinear2)
    let ops: Vec<&Opcode> = lcc_raw::opcodes::applied_ops(&raw.opcodes.list3).collect();
    // A DNG with a WarpRectilinear2 keeps every warp opcode as a chain of stages (each warps the one before's output);
    // without one the first WarpRectilinear is the lens warp, as it always was.
    let chain = ops.iter().any(|op| matches!(op, Opcode::WarpRectilinear2 { .. }));
    let mut stages = Vec::new();
    for op in ops {
        match op {
            Opcode::WarpRectilinear2 { planes, center, reciprocal, .. } if !planes.is_empty() => {
                let (center, radius) = centre(*center);
                let p = |i: usize| planes[i.min(planes.len() - 1)];
                stages.push(EmbeddedWarpStage { planes: [p(0), p(1), p(2)], center, radius, reciprocal: *reciprocal });
            }
            Opcode::WarpRectilinear { planes, center } if chain && !planes.is_empty() => {
                // WarpRectilinear is the even-power case: kr0 + kr1 r² + kr2 r⁴ + kr3 r⁶, no radius limit
                let (center, radius) = centre(*center);
                let p = |i: usize| {
                    let k = planes[i.min(planes.len() - 1)];
                    let mut s = [0.0; lcc_raw::opcodes::WR2_LEN];
                    s[0] = k[0];
                    s[2] = k[1];
                    s[4] = k[2];
                    s[6] = k[3];
                    s[15] = k[4];
                    s[16] = k[5];
                    s[18] = 1e6;
                    s
                };
                stages.push(EmbeddedWarpStage { planes: [p(0), p(1), p(2)], center, radius, reciprocal: false });
            }
            Opcode::WarpRectilinear { planes, center } if !chain && !planes.is_empty() && lens.warp.is_none() => {
                let (center, radius) = centre(*center);
                let p = |i: usize| planes[i.min(planes.len() - 1)];
                lens.warp = Some(EmbeddedWarp { planes: [p(0), p(1), p(2)], center, radius });
            }
            Opcode::FixVignetteRadial { k, center } if lens.vignette.is_none() => {
                let (center, radius) = centre(*center);
                lens.vignette = Some(EmbeddedVignette { k: *k, center, radius });
            }
            _ => {}
        }
    }
    // (more stages than a lens keeps: the first ones; real files carry two)
    for (slot, st) in lens.stages.iter_mut().zip(stages.into_iter().take(WARP_STAGES)) {
        *slot = Some(st);
    }
    if lens.warp.is_none() && lens.vignette.is_none() && !lens.has_stages() {
        return None;
    }
    Some(lcc_pipeline::optics::reorient_lens(&lens, raw.orientation, cw, ch))
}

/// Probe a video clip on disk (native): the content hash and SHA-1 of the whole file, streamed in
/// 1 MiB chunks (a multi-GB clip is never read into memory; the hash equals [`lcc_preview::hash_bytes`]
/// of its bytes, so duplicate detection and Import → Copy's verification work as for photos), then
/// the container's facts from [`lcc_video::probe`]: display size, duration, frame rate, codec,
/// audio, creation date (normalised like Exif dates, `YYYY-MM-DDThh:mm:ss[±hh:mm]`) and GPS. A clip
/// whose container can't be read still imports (described by its file only, `video: None`).
pub fn probe_video_file(path: &std::path::Path) -> Result<ProbeInfo, String> {
    use std::io::Read;
    let name = path.to_string_lossy().to_string();
    let mut f = std::fs::File::open(path).map_err(|e| format!("{name}: {e}"))?;
    let mut hash = lcc_preview::Hasher128::new();
    let mut sha = lcc_hash::Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let chunk = buf.get(..n).unwrap_or_default();
        hash.update(chunk);
        sha.update(chunk);
        size = size.saturating_add(n as u64);
    }
    let mut info = ProbeInfo {
        width: 1,
        height: 1,
        format: ext_upper(&name),
        kind: MediaKind::Video,
        file_size: size,
        content_hash: Some(hash.finish().to_string()),
        sha1: Some(sha.finish().to_hex()),
        ..Default::default()
    };
    match lcc_video::probe(path) {
        Ok(v) => apply_video_info(&mut info, &v),
        Err(e) => log::debug!("{name}: no video facts ({e})"),
    }
    Ok(info)
}

/// Fill a clip's probe from [`lcc_video::probe`]'s facts.
pub(crate) fn apply_video_info(info: &mut ProbeInfo, v: &lcc_video::VideoInfo) {
    let duration = (v.duration.is_finite() && v.duration >= 0.0).then_some(v.duration);
    if v.width > 0 && v.height > 0 {
        (info.width, info.height) = (v.width, v.height);
    }
    info.duration = duration;
    info.captured = v.created.as_deref().and_then(video_date);
    info.meta.gps = v.gps.filter(|(la, lo)| la.is_finite() && lo.is_finite() && la.abs() <= 90.0 && lo.abs() <= 180.0);
    info.video = Some(lcc_catalog::VideoMeta {
        duration: duration.unwrap_or(0.0),
        fps_num: v.fps_num,
        fps_den: v.fps_den,
        width: v.width,
        height: v.height,
        codec: v.codec.clone(),
        has_audio: v.has_audio,
        trim_in: 0.0,
        trim_out: None,
        poster_time: None,
    });
}

/// A clip's creation date (QuickTime `creationdate` / `©day`, ISO 8601) in the catalog's capture
/// date form ([`lcc_meta::DateTime::to_iso`], as photos' Exif dates). `None` when it isn't a date.
pub(crate) fn video_date(s: &str) -> Option<String> {
    let d = lcc_meta::DateTime::parse_iso(s)?;
    // a bare year (`2026`) or a zero date is no capture time
    (d.year > 1900).then(|| d.to_iso())
}

fn ext_upper(name: &str) -> String {
    std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
}

/// The lens correction for a raw file's bytes with the lens the user `picked` ([`crate::lens_profiles::resolve_picked`])
/// → (the correction, whether it is the file's own). Not a raw: an error.
pub fn lens_for_bytes(bytes: &[u8], picked: Option<&crate::lens_profiles::GivenLens>) -> Result<(Option<lcc_develop::EmbeddedLens>, bool), String> {
    if lcc_raw::probe(bytes).is_none() {
        return Err("not a raw file".into());
    }
    let raw = lcc_raw::probe_info(bytes).map_err(|e| e.to_string())?;
    let mut m = lcc_meta::extract(bytes);
    m.fill_missing(&raw.metadata);
    let window = lcc_raw::sensor_window(bytes, [raw.active_area.width, raw.active_area.height]);
    let resolved = crate::lens_profiles::resolve_picked(&raw, window, || m.clone(), picked);
    let from_file = matches!(resolved, Some((_, crate::lens_profiles::Applied::Embedded)));
    Ok((resolved.map(|(l, _)| l), from_file))
}

/// Probe a file's bytes: kind, dimensions (oriented), metadata. No pixel data is decompressed:
/// raws are described from their headers ([`lcc_raw::probe_info`]), other images too
/// ([`lcc_codecs::read_header`], which refuses truncated files but can't see damage inside
/// compressed data that is all there: such a file imports and shows as unreadable when rendered).
pub fn probe_bytes(name: &str, bytes: &[u8]) -> Result<ProbeInfo, String> {
    let content_hash = Some(lcc_preview::hash_bytes(bytes).to_string());
    let sha1 = Some(lcc_hash::sha1_bytes(bytes).to_hex());
    if crate::import::is_video_name(name) {
        // A video clip: catalogued as MediaKind::Video. Its stream facts (size, duration, codec) come from
        // the video backend's probe, which fills `Photo::video`; until then it is described by its file only.
        return Ok(ProbeInfo {
            width: 1,
            height: 1,
            format: ext_upper(name),
            kind: MediaKind::Video,
            file_size: bytes.len() as u64,
            content_hash,
            sha1,
            ..Default::default()
        });
    }
    let mut m = lcc_meta::extract(bytes);
    let (mut meta, captured) = meta_of(&m);
    let (rating, flag) = match m.rating {
        Some(r) if r < 0 => (0, lcc_catalog::Flag::Reject),
        Some(r) => (r.clamp(0, 5) as u8, lcc_catalog::Flag::None),
        None => (0, lcc_catalog::Flag::None),
    };
    let label = m.label.as_deref().and_then(|l| lcc_catalog::ColorLabel::parse(l.trim()));
    let label_text = m.label.clone();
    if lcc_raw::probe(bytes).is_some() {
        let mut raw = match lcc_raw::probe_info(bytes).map_err(|e| preview_reason(bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // a raw variant we can't decode yet: describe it from its embedded preview
                let (w, h) = embedded_preview_size(bytes).ok_or(format!("unsupported raw ({why}) without an embedded preview"))?;
                return Ok(ProbeInfo {
                    width: w,
                    height: h,
                    format: ext_upper(name),
                    kind: MediaKind::Raw,
                    file_size: bytes.len() as u64,
                    captured,
                    meta,
                    rating,
                    flag,
                    label,
                    label_text,
                    as_shot_wb: None,
                    content_hash,
                    xmp: lcc_meta::embedded(bytes).xmp,
                    orientation: m.orientation,
                    preview_only: Some(why),
                    sha1,
                    ..Default::default()
                });
            }
            Err(Err(e)) => return Err(e),
        };
        let (w, h) = raw.developed_size();
        let (mut w, mut h) = (w.max(1) as u32, h.max(1) as u32);
        if raw.orientation.swaps_axes() {
            std::mem::swap(&mut w, &mut h);
        }
        crate::camera_profiles::apply_dcp(&mut raw.color, &raw.metadata);
        m.fill_missing(&raw.metadata);
        if meta.lens.is_empty()
            && let Some(lens) = m.lens_model.clone().or_else(|| m.lens_spec.and_then(lcc_meta::lens_spec_name))
        {
            meta.lens = lens;
        }
        let scale = WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
        let as_shot_wb = Some(scale.as_shot(lcc_raw::color::as_shot_white_xy_of(&raw)));
        let window = lcc_raw::sensor_window(bytes, [raw.active_area.width, raw.active_area.height]);
        let resolved = crate::lens_profiles::resolve_raw(&raw, window, || m.clone(), None);
        let lens_from_file = matches!(resolved, Some((_, crate::lens_profiles::Applied::Embedded)));
        return Ok(ProbeInfo {
            embedded_lens: resolved.map(|(l, _)| l),
            lens_from_file,
            width: w,
            height: h,
            format: ext_upper(name),
            kind: MediaKind::Raw,
            file_size: bytes.len() as u64,
            captured,
            file_modified: None,
            captured_is_file_date: false,
            meta,
            rating,
            flag,
            label,
            label_text,
            as_shot_wb,
            content_hash,
            xmp: lcc_meta::embedded(bytes).xmp,
            orientation: Some(raw.orientation),
            preview_only: None,
            sha1,
            video: None,
            duration: None,
        });
    }
    let fmt = lcc_codecs::sniff(bytes).ok_or("unrecognized file format")?;
    if let Some(why) = fmt.not_decodable() {
        // e.g. "Heif files can't be opened: HEIC/HEIF support isn't included in this build of the app"
        return Err(format!("{fmt:?} files can't be opened: {why}"));
    }
    // headers only (issue #367: decoding the pixels was nearly all of an import's CPU time)
    let header = lcc_codecs::read_header(bytes).map_err(|e| e.to_string())?;
    let o = Orientation::from_exif(header.orientation);
    let (mut w, mut h) = (header.width, header.height);
    if o.swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    // regions another app drew on the photo as stored: `extract` could only tell by the size the metadata states, which
    // can be missing or wrong; the header's is the size as stored, so the boxes as written are turned by it (#2674)
    if o.swaps_axes()
        && !meta.regions.is_empty()
        && let Some(xmp) = lcc_meta::embedded(bytes).xmp.and_then(|x| lcc_meta::parse_xmp(&x).ok())
        && let Some(drawn_on) = xmp.regions_drawn_on()
        && !xmp.metadata.regions.is_empty()
    {
        meta.regions = xmp.metadata.regions;
        lcc_meta::upright_regions(&mut meta.regions, drawn_on, o, (header.width, header.height));
    }
    let format = match ext_upper(name).as_str() {
        "JPG" | "JPEG" => "JPEG".to_string(),
        "" => format!("{fmt:?}").to_uppercase(),
        e => e.to_string(),
    };
    Ok(ProbeInfo {
        width: w,
        height: h,
        format,
        kind: MediaKind::Image,
        file_size: bytes.len() as u64,
        captured,
        file_modified: None,
        captured_is_file_date: false,
        meta,
        rating,
        flag,
        label,
        label_text,
        as_shot_wb: None,
        content_hash,
        embedded_lens: None,
        lens_from_file: false,
        xmp: crate::sidecar::jpeg_header_xmp(bytes),
        orientation: m.orientation,
        preview_only: None,
        sha1,
        video: None,
        duration: None,
    })
}

/// Why a raw that failed to decode should show its embedded preview instead (`Ok`), or the error
/// to report (`Err`). Variants we can't decode yet always fall back. So does any CR3 error: the CRX
/// decoder is verified on few bodies, and every CR3 opened from its embedded JPEG before it existed.
/// So does any other failure of a recognised raw (damaged or oversized raw data, unreadable TIFF
/// structure): its preview may still be intact. Only a file that is not a raw reports an error.
pub(crate) fn preview_reason(bytes: &[u8], e: lcc_raw::RawError) -> Result<String, String> {
    use lcc_raw::RawError;
    match e {
        RawError::Unsupported(why) => Ok(why),
        e if lcc_raw::probe(bytes) == Some(lcc_raw::RawFormat::Cr3) => Ok(format!("CR3 {e}")),
        RawError::NotRaw => Err(RawError::NotRaw.to_string()),
        e => Ok(e.to_string()),
    }
}

/// Sensor clip level (normalised) for highlight reconstruction.
const HIGHLIGHT_CLIP: f32 = 0.99;

/// How a raw's camera RGB becomes the linear Rec.2020 the pipeline takes.
pub(crate) struct CameraColourModel<'a> {
    /// White-balance multipliers (as shot).
    pub wb: [f32; 3],
    /// White-balanced camera RGB → linear Rec.2020: the camera matrix, or a look fitted to the camera's JPEG on top.
    pub matrix: lcc_color::Mat3,
    /// Baseline exposure, as a gain.
    pub gain: f32,
    /// A DNG profile's hue/saturation map and look table.
    pub tables: Option<&'a lcc_raw::profile::ProfileTables>,
    /// A fitted camera look's hue/saturation table.
    pub hue_sat: Option<&'a crate::camera_preview::HueSat<'a>>,
}

impl CameraColourModel<'_> {
    /// Rebuild sensor-clipped channels; fully clipped highlights become [`clipped_white`] of the matrix, so they
    /// come out of [`Self::apply`] neutral.
    pub fn rebuild_highlights(&self, img: &mut Rgb32f) {
        lcc_raw::highlight::reconstruct_with(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix));
    }

    /// [`Self::rebuild_highlights`] for a binned image whose clipped channels `mask` tells
    /// ([`lcc_raw::RawImage::develop_binned_masked`]).
    pub fn rebuild_highlights_masked(&self, img: &mut Rgb32f, mask: &[u8]) {
        lcc_raw::highlight::reconstruct_masked(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix), mask);
    }

    /// White balance, matrix, tables and baseline exposure, in place.
    pub fn apply(&self, img: &mut Rgb32f) {
        let (wb, m, gain) = (self.wb, self.matrix.to_f32(), self.gain);
        if let (None, Some(hue_sat)) = (self.tables, self.hue_sat) {
            // a fitted camera look: `pixel`, `LANES` pixels at a time so the table step vectorises (same bits)
            const LANES: usize = 16;
            lcc_raster::par_rows(&mut img.data, img.width, |_, row| {
                for chunk in row.chunks_mut(LANES) {
                    let mut rgb = [[0.0f32; LANES]; 3];
                    for (l, p) in chunk.iter().enumerate() {
                        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                        for (i, rgb) in rgb.iter_mut().enumerate() {
                            rgb[l] = (m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]) * gain;
                        }
                    }
                    let out = hue_sat.apply_lanes(rgb);
                    for (l, p) in chunk.iter_mut().enumerate() {
                        *p = finite([out[0][l], out[1][l], out[2][l]]);
                    }
                }
            });
            return;
        }
        img.map_in_place(|p| self.pixel(&m, p));
    }

    /// [`Self::apply`] to one pixel; `m` is the matrix as `f32`.
    fn pixel(&self, m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
        let (wb, gain) = (self.wb, self.gain);
        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        let rgb = [
            m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
            m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
            m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
        ];
        let rgb = match self.tables {
            Some(tables) => tables.apply(rgb, gain),
            None => rgb.map(|v| v * gain),
        };
        finite(self.hue_sat.map_or(rgb, |h| h.apply(rgb)))
    }
}

/// `rgb` (linear Rec.2020) as decoding leaves it for the stages after: negative channels kept, non-finite ones zero.
/// Near black, negative channels are sensor noise through the colour matrix, and keeping them keeps the noise zero-mean
/// until noise reduction and the output's gamut mapping (which moves a colour toward its luminance until it is in
/// range) deal with it. Clamping each channel at zero here kept only the positive half of every channel's noise:
/// through a saturating camera matrix (red row 3.5, −2.4, −0.1 on a NIKON D780 profile) red and blue have the widest
/// noise, so pushed shadows and dark frames came out magenta (a black frame pushed 4 EV rendered at sRGB (64, 28, 65)
/// in its darkest 40%, (2, 4, 4) without the clamp). Moving each such colour toward its luminance instead still left
/// it pink, at (34, 14, 22): any per-pixel rule that treats pixels by the sign of their noise biases the mean.
fn finite(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|v| if v.is_finite() { v } else { 0.0 })
}

/// The white-balanced camera RGB that `model` (white-balanced camera RGB → linear Rec.2020) renders
/// neutral, as bright as it renders `[1, 1, 1]`: what fully clipped highlights are rebuilt to, so they come out
/// neutral whatever the colour model does to the camera's own neutral (a look fitted to the camera's JPEG or a
/// camera profile often tints it by a few per cent, which any darkening of the highlights would show).
/// `[1, 1, 1]` (the camera's own neutral, as before this existed) when the model can't be trusted with it: not
/// invertible or nearly so, an answer far from the camera's neutral (a sane colour model moves white by a few per
/// cent), or one that isn't neutral through the `f32` matrix the decode applies.
pub(crate) fn clipped_white(model: &lcc_color::Mat3) -> [f32; 3] {
    const CAMERA: [f32; 3] = [1.0; 3];
    /// Largest condition number (max row sum norm) of a model trusted for this.
    const CONDITION: f64 = 1e3;
    /// Largest factor between the answer and the camera's neutral, per channel.
    const SPREAD: f64 = 4.0;
    let norm = |m: &lcc_color::Mat3| m.0.iter().map(|row| row.iter().map(|v| v.abs()).sum::<f64>()).fold(0.0, f64::max);
    let Some(inverse) = model.inverse() else { return CAMERA };
    let condition = norm(model) * norm(&inverse);
    if !condition.is_finite() || condition > CONDITION {
        return CAMERA;
    }
    let luma = |v: [f64; 3]| {
        let c = model.apply(v);
        (0..3).map(|i| c[i] * f64::from(lcc_color::LUMA_2020[i])).sum::<f64>()
    };
    let n = inverse.apply([1.0; 3]);
    let (target, own) = (luma([1.0; 3]), luma(n));
    if !(target > 0.0 && own > 0.0 && target.is_finite() && own.is_finite()) {
        return CAMERA;
    }
    let white = n.map(|v| v * target / own);
    if !white.iter().all(|v| v.is_finite() && (1.0 / SPREAD..=SPREAD).contains(v)) {
        return CAMERA;
    }
    let white = white.map(|v| v as f32);
    // neutral through the matrix as the decode applies it
    let m = model.to_f32();
    let out: [f32; 3] = std::array::from_fn(|i| m[i][0] * white[0] + m[i][1] * white[1] + m[i][2] * white[2]);
    let (lo, hi) = (out[0].min(out[1]).min(out[2]), out[0].max(out[1]).max(out[2]));
    if lo > 0.0 && hi.is_finite() && hi - lo <= 1e-3 * hi { white } else { CAMERA }
}

/// The largest block size to bin a raw's mosaic by for a source of at most `max_edge` pixels:
/// the binned image must keep at least 90 % of `max_edge` (a 16 MP sensor still bins 2× for the
/// 2560 px preview). X-Trans can only bin 3× (its 6×6 pattern), and its full demosaic is ~5× the
/// cost of Bayer's, so 3× is accepted down to 75 % (a 24 MP X-Trans preview is then ~2000 px
/// instead of a ~1.2 s full demosaic; zooming in still uses the full-size source).
/// `None` = demosaic at full size.
pub fn bin_factor(raw: &lcc_raw::RawImage, max_edge: usize) -> Option<usize> {
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let long = if c.width > 1 && c.height > 1 { c.width.max(c.height) } else { raw.active_area.width.max(raw.active_area.height) };
    let need = (max_edge.saturating_mul(9) / 10).max(1);
    let need3 = (max_edge.saturating_mul(3) / 4).max(1);
    [8usize, 6, 4, 3, 2].into_iter().find(|&k| raw.can_bin(k) && (long / k >= need || (k == 3 && !raw.can_bin(2) && long / k >= need3)))
}

/// Decode a file into a linear Rec.2020 image no larger than `max_edge`, oriented.
///
/// Runs on a rayon worker: its many short parallel loops then start on the worker's own queue
/// instead of each one waking the pool from outside and waiting for it (which costs more than the
/// loops themselves when the machine is busy).
pub fn load_bytes(bytes: &[u8], max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    load_bytes_with(bytes, max_edge, LoadOptions::default())
}

/// [`load_bytes`] with the photo's settings the decode depends on.
pub fn load_bytes_with(bytes: &[u8], max_edge: usize, options: LoadOptions) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(|_| load_bytes_now(std::borrow::Cow::Borrowed(bytes), max_edge, None, options, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_bytes`] taking the file's bytes: a raw file's bytes are freed as soon as it is decoded
/// (less memory held while it is developed).
pub fn load_vec(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    load_vec_with(bytes, max_edge, LoadOptions::default())
}

/// [`load_vec`] with the photo's settings the decode depends on.
pub fn load_vec_with(bytes: Vec<u8>, max_edge: usize, options: LoadOptions) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, options, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_vec_with`] for a thumbnail: a DNG that keeps reduced-resolution copies of its raw image (Lightroom's HDR and
/// panorama merges) develops the smallest that covers `max_edge` instead of the whole image. The flag says whether
/// it did: such a picture is not the full image, and an export never renders from it.
pub fn load_vec_reduced(bytes: Vec<u8>, max_edge: usize, options: LoadOptions) -> Result<(Rgb32f, SourceInfo, bool), String> {
    rayon::scope(move |_| {
        load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, options, true).map(|(img, _, info, reduced)| (img, info, reduced))
    })
}

/// [`load_vec`] that also develops the photo's cached denoised picture (`spec`) from the same decode: the plain
/// picture, the denoised one (`None` when it cannot be read or does not fit this raw: the plain one is then all
/// there is) and the source facts. Both pictures are the same size.
pub fn load_vec_pair(
    bytes: Vec<u8>,
    max_edge: usize,
    spec: &DenoiseSpec,
    options: LoadOptions,
) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> {
    rayon::scope(move |_| {
        load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, Some(spec), options, false).map(|(img, twin, info, _)| (img, twin, info))
    })
}

/// How a raw is decoded beyond its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawOptions {
    /// The file's name, which names the camera body when the file has no serial number
    /// ([`crate::sensor_maps::body_key`]).
    pub file_name: Option<String>,
    /// Where the file is (remembered with its body: `crate::sensor_maps::body_of_path`).
    pub path: Option<String>,
    /// Correct hot, dead and stuck pixels (the photo's `sensor.pixelFix`): the camera body's known ones and the clear
    /// ones this photo shows, and add what it shows to the body's map.
    pub pixel_fix: bool,
}

impl Default for RawOptions {
    fn default() -> Self {
        RawOptions { file_name: None, path: None, pixel_fix: true }
    }
}

impl RawOptions {
    /// The options a decode with `options` uses (the file named by [`LoadOptions::path`]).
    pub fn of(options: &LoadOptions) -> RawOptions {
        match &options.path {
            Some(p) => RawOptions::for_path(p, options.pixel_fix),
            None => RawOptions { pixel_fix: options.pixel_fix, ..RawOptions::default() },
        }
    }

    /// The options for the file at `path`.
    pub fn for_path(path: &str, pixel_fix: bool) -> RawOptions {
        let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).map(str::to_string);
        RawOptions { file_name: name, path: Some(path.to_string()), pixel_fix }
    }
}

/// What a photo tells the maps: a fingerprint of its samples and capture facts, and those facts.
pub fn shot_of(raw: &lcc_raw::RawImage) -> crate::sensor_maps::Shot {
    let m = &raw.metadata;
    let captured = m.capture_time.map(|t| t.to_iso());
    let mut h = lcc_raw::defects::sample_fingerprint(raw);
    let facts = captured
        .as_deref()
        .unwrap_or("")
        .bytes()
        .chain(m.exposure_time.unwrap_or(0.0).to_bits().to_le_bytes())
        .chain(m.iso.unwrap_or(0).to_le_bytes());
    for b in facts {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    crate::sensor_maps::Shot { fingerprint: h, captured, exposure: m.exposure_time, iso: m.iso, f_number: m.f_number.map(|f| f as f32) }
}

/// Hot, dead and stuck pixels of a decoded raw, before anything reads its samples: look for them (at the learning
/// threshold: those go to the body's map), then correct the body's confirmed ones and the clear ones found here.
pub fn repair_pixels(raw: &mut lcc_raw::RawImage, opts: &RawOptions) -> lcc_pipeline::PixelRepair {
    use lcc_raw::defects::{Fix, Thresholds, correct, detect, refine};
    let m = &raw.metadata;
    let body = crate::sensor_maps::body_key(m.serial_number.as_deref(), opts.file_name.as_deref());
    if let (Some(p), Some(b)) = (&opts.path, &body) {
        crate::sensor_maps::remember_path(p, b);
    }
    let c = raw.develop_crop(raw.active_area.width, raw.active_area.height);
    let base = lcc_pipeline::PixelRepair {
        on: false,
        body: body.clone(),
        orientation: raw.orientation,
        developed: [c.width, c.height],
        sensor: [raw.width, raw.height],
        model: m.model.clone().unwrap_or_default(),
        ..Default::default()
    };
    if !opts.pixel_fix {
        return base;
    }
    let t0 = web_time::Instant::now();
    let (make, model) = (m.make.clone().unwrap_or_default(), m.model.clone().unwrap_or_default());
    let found = detect(raw, &Thresholds::LEARN);
    let strict = refine(raw, &found, &Thresholds::CORRECT);
    let sensor = [raw.width, raw.height];
    let mut fixes: Vec<Fix> = strict.iter().map(Fix::from).collect();
    let mut from_map = 0;
    if let Some(b) = &body {
        if !found.overflow {
            let _ = crate::sensor_maps::record_pixels(b, &make, &model, sensor, &shot_of(raw), &found.defects);
        }
        let known = crate::sensor_maps::fixes_for(b, &model, sensor);
        from_map = known.len();
        fixes.extend(known.iter().copied());
        // a known pixel's cluster mates (they fail the single-photo checks one by one)
        if !known.is_empty() {
            fixes.extend(lcc_raw::defects::grow(raw, &known));
        }
    }
    fixes.sort_by_key(|f| (f.y, f.x, f.s));
    fixes.dedup();
    let n = correct(raw, &fixes);
    if lcc_pipeline::profiling() {
        eprintln!("[profile] pixels: {} found, {from_map} from the map, {n} corrected in {:.1} ms", strict.len(), t0.elapsed().as_secs_f64() * 1e3);
    }
    lcc_pipeline::PixelRepair { on: true, from_map: from_map as u32, found: strict.len() as u32, ..base }
}

/// The denoised camera RGB of `raw` from its cached product, as the plain development would have it: the default
/// crop, and `factor` × `factor` blocks averaged into one pixel when the plain one was binned (plain means: the
/// plain development's clip mask tells highlight reconstruction which of them held clipped samples). `None` when
/// the product is missing, damaged, made from something else or not the size of this raw's active area.
fn denoised_camera_rgb(raw: &lcc_raw::RawImage, spec: &DenoiseSpec, factor: usize) -> Option<Rgb32f> {
    use lcc_denoise::product::{self, Window};
    let a = raw.active_area;
    let header = product::read_header_at(&spec.product).ok()?;
    if header.key != spec.key || (header.width, header.height) != (a.width, a.height) {
        return None;
    }
    if raw.opcodes.list3.is_empty() {
        let c = raw.develop_crop(a.width, a.height);
        let window = Window { x: c.x, y: c.y, width: c.width, height: c.height };
        product::read_window(&spec.product, &spec.key, Some(window), factor, None).ok()
    } else {
        // (the plain development is not binned either when the file has opcodes of this kind: `factor` is 1)
        let whole = product::read(&spec.product, &spec.key, 1).ok()?;
        Some(raw.finish_demosaiced(whole))
    }
}

fn load_bytes_now(
    bytes: std::borrow::Cow<'_, [u8]>,
    max_edge: usize,
    denoise: Option<&DenoiseSpec>,
    options: LoadOptions,
    allow_reduced: bool,
) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo, bool), String> {
    if lcc_raw::probe(&bytes).is_some() {
        // a thumbnail (`allow_reduced`, never an export) of a DNG that keeps smaller copies of its raw image
        // (Lightroom's merges) develops the smallest that covers it; a denoised photo's twin needs the full mosaic
        let reduced =
            (allow_reduced && denoise.is_none() && max_edge < usize::MAX).then(|| lcc_raw::decode_reduced(&bytes, max_edge).ok().flatten()).flatten();
        let used_reduced = reduced.is_some();
        let t_decode = web_time::Instant::now();
        let decoded = match reduced {
            Some(r) => Ok(r),
            None => lcc_raw::decode(&bytes),
        };
        let mut raw = match decoded.map_err(|e| preview_reason(&bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // show the camera's embedded JPEG (rendered, not raw) until the variant is supported
                return load_embedded_preview(&bytes, max_edge)
                    .map(|(img, info)| (img, None, info, false))
                    .ok_or(format!("unsupported raw ({why}) without an embedded preview"));
            }
            Err(Err(e)) => return Err(e),
        };
        let decode_ms = t_decode.elapsed().as_secs_f64() * 1e3;
        // hot, dead and stuck pixels are corrected on the mosaic, before anything reads it
        let pixels = repair_pixels(&mut raw, &RawOptions::of(&options));
        // single outlier photosites, replaced in the mosaic before anything is developed from it (#1345)
        if options.outliers {
            let t = std::time::Instant::now();
            let n = raw.suppress_hot_pixels();
            if lcc_pipeline::profiling() {
                eprintln!("[profile] raw outliers: {n} photosites replaced in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            }
        }
        // the photo's camera profile (`.dcp`), or its camera's default one, replaces the file's colour model
        // (its own look, tone curve and look table, only "As profile": under one of the app's looks its calibration only).
        // A profile without a tone curve of its own (most chart-made ones) keeps the tone the photo has without it,
        // fitted below with the file's own colour (`own_colour`), whichever look sits on top.
        let own_colour = crate::camera_profiles::profile_for(&raw.color, &raw.metadata, &options.camera)
            .filter(|d| crate::camera_preview::keeps_photo_tone(d))
            .map(|_| raw.color.clone());
        let dcp = crate::camera_profiles::apply_choice_with(&mut raw.color, &raw.metadata, &options.camera, options.camera_look).is_some();
        // Embedded lens corrections are applied by the pipeline ("Enable Profile Corrections"), not baked in
        // (removed before the camera look's binned sensor proxy too, which needs an empty `OpcodeList3`).
        let info = raw.info();
        let window = lcc_raw::sensor_window(&bytes, [info.active_area.width, info.active_area.height]);
        // a flat-field map works in full-resolution sensor pixels: a reduced DNG copy has a smaller active area
        // of its own, so the full image's comes from the file's header
        let full_active = if used_reduced {
            lcc_raw::probe_info(&bytes).ok().map(|i| [i.active_area.width, i.active_area.height])
        } else {
            Some([info.active_area.width, info.active_area.height])
        };
        let full_window = if used_reduced { full_active.and_then(|a| lcc_raw::sensor_window(&bytes, a)) } else { window };
        let (lens, lens_from_file) = match crate::lens_profiles::resolve_raw(&info, window, || lcc_meta::extract(&bytes), None) {
            Some((l, applied)) => (Some(l), applied == crate::lens_profiles::Applied::Embedded),
            None => (None, false),
        };
        // (the camera look still pairs the sensor with the camera JPEG through the warp the camera applied to it)
        let lens_geometry = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        // A DNG's preview already has its lens opcodes applied. Keep them for its single tone fit,
        // then remove them again: the rendered source leaves those corrections to the pipeline.
        let dng_lens_ops =
            (raw.format == lcc_raw::RawFormat::Dng && raw.opcodes.list3.iter().any(|op| op.is_lens_correction())).then(|| raw.opcodes.list3.clone());
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        // Which scale white balance is edited on, and the as-shot white on it, from the file's own colour tags or
        // the camera's measured matrices (`Photo::relative_wb` decides the same from the catalog's camera)
        let scale = WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
        // (whether the photo's Profile option can start from measured colour: decided before the colour is chosen)
        let colorimetric = crate::camera_preview::colorimetric_camera(&raw).is_some();
        // (a DNG of rendered RGB: no camera profile describes it, so the Profiles panel offers none)
        let rendered_rgb = lcc_raw::color::is_rendered_rgb(&raw.color);
        let t_colour = web_time::Instant::now();
        // the source's segmentation mattes (DNG semantic masks), read while the starting colour is fitted
        let (((xy, t, camera_look), kept_tone), mattes) = rayon::join(
            || {
                let removed = dng_lens_ops.map(|ops| std::mem::replace(&mut raw.opcodes.list3, ops));
                // With the opcodes restored, the proxy already includes vignetting; do not apply it twice.
                let embedded = if removed.is_some() { None } else { lens.as_ref() };
                let kept_tone = own_colour.and_then(|own| {
                    let mut trace = crate::camera_preview::StartTrace::default();
                    crate::camera_preview::tone_without_profile(&mut raw, own, &bytes, lens_geometry.as_ref(), embedded, options.colour, &mut trace)
                });
                let colour = crate::camera_preview::starting_colour(&mut raw, &bytes, lens_geometry.as_ref(), embedded, options.colour, dcp);
                if let Some(ops) = removed {
                    raw.opcodes.list3 = ops;
                }
                (colour, kept_tone)
            },
            || dng_mattes(&bytes, &info),
        );
        drop(bytes);
        if lcc_pipeline::profiling() {
            eprintln!(
                "[profile] raw load: decode {decode_ms:.1} ms, repair + outliers + profile {:.1} ms, starting colour {:.1} ms",
                (t_colour - t_decode).as_secs_f64() * 1e3 - decode_ms,
                t_colour.elapsed().as_secs_f64() * 1e3
            );
        }
        // Previews and thumbnails bin the mosaic straight to (about) the size they need; only
        // larger levels (exports, 1:1) demosaic the whole sensor.
        let t0 = web_time::Instant::now();
        let bin = bin_factor(&raw, max_edge);
        // (binned: block means, and which colours had a clipped sample in each block)
        let binned = match bin {
            Some(k) => raw.develop_binned_masked(k, HIGHLIGHT_CLIP).map_err(|e| e.to_string())?,
            None => None,
        };
        let factor = if binned.is_some() { bin.unwrap_or(1) } else { 1 };
        let (img, clip_mask) = match binned {
            Some((img, mask)) => (img, Some(mask)),
            None => {
                let method = if max_edge <= 600 { lcc_raw::Method::Bilinear } else { lcc_raw::Method::Ahd };
                (raw.develop(method).map_err(|e| e.to_string())?, None)
            }
        };
        // the same development from the denoised mosaic, when the photo has been denoised
        let twin = denoise.and_then(|spec| denoised_camera_rgb(&raw, spec, factor)).filter(|d| d.width == img.width && d.height == img.height);
        // the samples aren't needed any more (the colour model below reads only the tags)
        raw.data = lcc_raw::RawData::U16(Vec::new());
        let hue_sat = camera_look.as_ref().and_then(|p| p.hue_sat.as_ref()).and_then(crate::camera_preview::HueSat::new);
        // A DNG's own profile look (hue/saturation map, look table), DNG spec chapter 6.
        let tables = lcc_raw::profile::ProfileTables::new(&raw.color.profile, lcc_raw::color::illuminant_weight(&raw.color, xy));
        let colour = CameraColourModel {
            wb: t.wb,
            matrix: camera_look.as_ref().map(|p| p.matrix.mul(&t.matrix)).unwrap_or(t.matrix),
            gain: 2f32.powf(t.baseline_exposure as f32),
            tables: tables.as_ref(),
            hue_sat: hue_sat.as_ref(),
        };
        // camera RGB → what the pipeline takes: clipped highlights rebuilt, white balance and the colour model applied,
        // fitted to `max_edge` and upright (the same for the plain and the denoised picture)
        let finish = |mut img: Rgb32f| {
            let mut stages = vec![("develop", t0.elapsed())];
            stages.push(("transform", t0.elapsed()));
            match &clip_mask {
                Some(mask) => colour.rebuild_highlights_masked(&mut img, mask),
                None => colour.rebuild_highlights(&mut img),
            }
            stages.push(("highlights", t0.elapsed()));
            colour.apply(&mut img);
            stages.push(("colour", t0.elapsed()));
            // a full-size render keeps the picture (fitting it to its own size would copy it, ~390 MB at 33 MP)
            let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Box) } else { img };
            stages.push(("fit", t0.elapsed()));
            let img = img.into_oriented(raw.orientation);
            stages.push(("orient", t0.elapsed()));
            (img, stages)
        };
        let (img, stages) = finish(img);
        if lcc_pipeline::profiling() {
            let mut prev = std::time::Duration::ZERO;
            let parts: Vec<String> = stages
                .iter()
                .map(|(n, t)| {
                    let d = *t - prev;
                    prev = *t;
                    format!("{n} {:.1}", d.as_secs_f64() * 1e3)
                })
                .collect();
            eprintln!("[profile] raw source {}×{} (max {max_edge}, ms after decode): {}", img.width, img.height, parts.join(", "));
        }
        let twin = twin.map(|picture| finish(picture).0);
        // White balance re-evaluates the file's own colour model (when it has one and no file-local look matrix sits
        // on top of it). A model's measured matrices never do: they only give the Kelvin scale its numbers, and the
        // picture is adapted from the as-shot white as every fitted look is (so settings from before that scale
        // existed render exactly as they did, `lcc_pipeline::local::legacy_white`).
        let camera_color = (scale.own_matrix && !t.matrix_is_fallback && (camera_look.is_none() || raw.format == lcc_raw::RawFormat::Dng || dcp))
            .then(|| {
                let tags = lcc_raw::ColorData { profile: Default::default(), ..raw.color.clone() };
                Arc::new(lcc_pipeline::CameraColor { tags, developed_for: xy })
            });
        // a fitted look's tone, else the tone a profile without a curve of its own keeps, else the file's (or profile's) curve
        let camera_tone =
            camera_look.as_ref().map(|p| p.tone).or(kept_tone).or_else(|| raw.color.profile.tone_curve.as_ref().and_then(dng_tone_curve));
        let placement = source_placement(&raw, full_active, full_window);
        let local_tone = local_tone(&raw, lcc_raw::gaintable::SourcePlacement::new(placement.rect, placement.orientation));
        let (temp, tint) = scale.as_shot(xy);
        return Ok((
            img,
            twin,
            SourceInfo {
                raw: true,
                as_shot_temp: temp,
                as_shot_tint: tint,
                lens,
                lens_from_file,
                relative_wb: scale.relative,
                legacy_relative_wb: scale.legacy_relative,
                camera_color,
                camera_tone,
                mattes,
                local_tone,
                filters: None,
                // (the catalog knows the original's size: `media::source_info`)
                native_long: None,
                colorimetric,
                rendered_rgb,
                alpha: None,
                infrared: None,
                generated: None,
                avoid: None,
                pixels: Some(pixels),
                // (the catalog knows which photo this is: `media::source_info`)
                identity: None,
                placement: Some(placement),
                // (the session adds the catalog's maps: `media`, as it adds the generated patches)
                vignetting_maps: None,
            },
            used_reduced,
        ));
    }
    let d = lcc_codecs::decode(&bytes, fit_box(max_edge)).map_err(|e| e.to_string())?;
    drop(bytes);
    // the denoised picture of a finished picture, brought to this decode's size and through the same conversion
    let twin = denoise.and_then(|spec| denoised_finished(&d, spec));
    let img = d.to_working();
    let orientation = Orientation::from_exif(d.orientation);
    // the transparency, fitted and oriented with the pixels, for exports that keep it (issue #2976);
    // a channel that is opaque everywhere is not kept (a decoder's fit brings 1.0 back as 0.99999994:
    // that is not transparency, and the photo must render as an opaque one)
    const OPAQUE: f32 = 1.0 - 1e-4;
    let alpha = d.alpha.filter(|a| (a.width, a.height) == (img.width, img.height) && a.data.iter().any(|&v| v < OPAQUE)).map(|a| {
        let a = if a.width.max(a.height) > max_edge { fit(&a, max_edge, max_edge, Filter::Mitchell) } else { a };
        Arc::new(a.into_oriented(orientation))
    });
    // a film scanner's infrared pass, fitted and oriented with the pixels (Find Film Dust reads it)
    let infrared = d.infrared.filter(|p| (p.width, p.height) == (img.width, img.height)).map(|p| {
        let p = if p.width.max(p.height) > max_edge { fit(&p, max_edge, max_edge, Filter::Mitchell) } else { p };
        Arc::new(p.into_oriented(orientation))
    });
    let finish = |img: Rgb32f| {
        let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
        img.into_oriented(orientation)
    };
    let img = finish(img);
    let twin = twin.map(finish).filter(|t| t.width == img.width && t.height == img.height);
    Ok((img, twin, SourceInfo { alpha, infrared, ..SourceInfo::default() }, false))
}

/// The cached denoised picture of a finished picture (gamma-encoded sRGB at the file's full size, see
/// `crate::denoise`), at the size of `d` (a reduced decode is matched by binning and resampling) and in the working
/// space, as [`lcc_codecs::Decoded::to_working`] has `d`. `None` when the product is missing, damaged, made from
/// something else or not the shape of this picture.
fn denoised_finished(d: &lcc_codecs::Decoded, spec: &DenoiseSpec) -> Option<Rgb32f> {
    use lcc_denoise::product;
    let header = product::read_header_at(&spec.product).ok()?;
    let (w, h) = (d.image.width, d.image.height);
    if header.key != spec.key || w == 0 || h == 0 || header.width < w || header.height < h {
        return None;
    }
    // the same picture: the aspect ratios agree to within the rounding of a reduced decode
    let (fw, fh) = (header.width as f64 / w as f64, header.height as f64 / h as f64);
    if (fw - fh).abs() > 0.02 * fw.max(fh) + 2.0 / (w.min(h) as f64) {
        return None;
    }
    let factor = (header.width / w).min(header.height / h).max(1);
    let encoded = product::read(&spec.product, &spec.key, factor).ok()?;
    let encoded = if (encoded.width, encoded.height) == (w, h) { encoded } else { lcc_raster::resample::resize(&encoded, w, h, Filter::Mitchell) };
    let linear = encoded.map(|p| p.map(lcc_color::transfer::srgb_to_linear));
    let m = lcc_codecs::SourceSpace::named(lcc_codecs::NamedSpace::Srgb, lcc_codecs::SpaceOrigin::Untagged).to_working().to_f32();
    Some(linear.map(|p| {
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
        ]
    }))
}

/// Which white-balance scale a raw is edited on, decided from the file alone, the way the catalog decides it from
/// the photo's format and camera (`lcc_catalog::relative_wb_camera`; a test keeps the two in step).
///
/// A file with colour matrices of its own (DNG) has an absolute scale from its tags. A file without
/// (`camera_preview::file_local_look`) has one when its camera's matrices are measured
/// (`lcc_raw::spectral`), else when its camera's white-balance presets give one
/// (`lcc_raw::kelvin_presets`), else when the camera database has matrices for it (`lcc_camdb`, dcraw's table):
/// each turns its as-shot multipliers into Kelvin and tint, nothing else — the
/// look stays the file's fit. Any other raw is edited relative to its as-shot look (6500 K / 0 means as shot).
pub(crate) struct WbScaleOf {
    own_matrix: bool,
    pub(crate) relative: bool,
    /// Kelvin through the camera's measured matrices or presets: edited on the relative scale before those were used.
    legacy_relative: bool,
    /// The as-shot white through the measured matrices or the presets, when that is where the scale comes from.
    scale_white: Option<lcc_color::Xy>,
    /// The camera's measured matrices, when they give the scale.
    camera: Option<&'static lcc_raw::spectral::Camera>,
    /// The camera database entry whose matrices give the scale, when neither the spectral table nor presets do.
    db: Option<lcc_camdb::Entry>,
}

/// The white balance a DNG of a raw's camera RGB (Photo Merge) declares so that it shows as its source does
/// ([`WbScaleOf::dng_white`]).
pub(crate) enum DngWhite {
    /// Colour tags with the camera's measured matrices: the DNG reads every white through them, as the source does.
    Matrices(Box<lcc_raw::ColorData>),
    /// The white the source's scale shows at As Shot.
    At(lcc_color::Xy),
}

impl WbScaleOf {
    pub(crate) fn new(format: lcc_raw::RawFormat, color: &lcc_raw::ColorData, wb_multipliers: Option<[f32; 3]>, meta: &lcc_meta::Metadata) -> Self {
        let own_matrix = lcc_raw::color::has_matrix(color);
        let fitted = crate::camera_preview::file_local_look(format) && !own_matrix;
        let model = meta.model.as_deref().filter(|_| fitted);
        let camera = model.and_then(|model| lcc_raw::spectral::find(meta.make.as_deref(), model));
        let presets = camera.is_none() && model.is_some_and(|model| lcc_raw::kelvin_presets::covers(meta.make.as_deref(), model));
        // data the camera balanced already (unit multipliers) read As Shot from the levels it balanced them with
        let shot = color.balanced_as_shot.or(wb_multipliers);
        let scale_white = match camera {
            Some(c) => lcc_raw::spectral::as_shot_white_through(c, color, shot),
            None if presets => lcc_raw::kelvin_presets::as_shot_white(color, shot),
            None => None,
        };
        // else the camera database's matrices (dcraw's table, …), the last way to a Kelvin reading
        let db = (camera.is_none() && !presets)
            .then(|| model.and_then(|model| lcc_camdb::find(meta.make.as_deref(), model)))
            .flatten()
            .filter(|e| lcc_raw::color::has_matrix(&e.color_data()));
        let scale_white = scale_white.or_else(|| db.as_ref().and_then(|e| e.as_shot_white_through(color, shot)));
        let covered = camera.is_some() || presets || db.is_some();
        WbScaleOf { own_matrix, relative: fitted && !covered, legacy_relative: fitted && covered, scale_white, camera, db }
    }

    /// What a DNG of this raw's camera RGB declares so that it shows the white balance as this scale does: the
    /// camera's measured matrices when they give the scale, else the white the scale shows at As Shot (the presets'
    /// reading; 6500 K / 0 on the relative scale; else `own`, the white the file's tags give).
    pub(crate) fn dng_white(&self, color: &lcc_raw::ColorData, own: lcc_color::Xy) -> DngWhite {
        if let Some(camera) = self.camera {
            let mut c = lcc_raw::ColorData { profile: Default::default(), kelvin_presets: None, ..color.clone() };
            if camera.fill(&mut c) {
                return DngWhite::Matrices(Box::new(c));
            }
        }
        if let Some(e) = &self.db {
            let mut c = lcc_raw::ColorData { profile: Default::default(), kelvin_presets: None, ..color.clone() };
            if e.fill(&mut c) {
                return DngWhite::Matrices(Box::new(c));
            }
        }
        if self.relative {
            return DngWhite::At(lcc_color::cct::temp_tint_to_xy(6500.0, 0.0));
        }
        DngWhite::At(self.scale_white.unwrap_or(own))
    }
    /// The as-shot temperature and tint on the file's scale: the relative scale's reference, the white through
    /// the measured matrices or the presets, else `own` (the white the file's own tags give). Whole numbers, as the
    /// catalog keeps them. A covered camera whose file names no white (no multipliers, or no presets) falls back to
    /// `own`, D65.
    fn as_shot(&self, own: lcc_color::Xy) -> (f64, f64) {
        if self.relative {
            return (6500.0, 0.0);
        }
        let (t, tint) = xy_to_temp_tint(self.scale_white.unwrap_or(own));
        (t.round(), tint.round())
    }
}

/// The raw's gain table map with where its developed picture (the default crop, oriented) sits in
/// the active area: rendered when the photo's "Camera local tone mapping" option is on.
fn local_tone(raw: &lcc_raw::RawImage, placement: lcc_raw::gaintable::SourcePlacement) -> Option<Arc<lcc_pipeline::local_tone::LocalTone>> {
    let map = raw.color.profile.gain_table_map.clone()?;
    Some(Arc::new(lcc_pipeline::local_tone::LocalTone { map, placement }))
}

/// Where a raw's developed picture (the default crop, oriented) sits in its active area, and that area on the sensor:
/// for the gain table ([`local_tone`]) and a flat-field map (`lcc_pipeline::vignetting_calibration`). `full_active` is the
/// full-resolution active area's size (a reduced DNG copy has a smaller one of its own; `None` when unknown) and
/// `window` where it sits on the sensor (`lcc_raw::sensor_window`).
fn source_placement(
    raw: &lcc_raw::RawImage,
    full_active: Option<[usize; 2]>,
    window: Option<lcc_raw::SensorWindow>,
) -> lcc_raw::gaintable::SourcePlacement {
    placement_from(raw.active_area, raw.crop, raw.orientation, full_active, window)
}

/// [`source_placement`] from the parts of a raw's facts it reads (the decode's and a header read's alike).
fn placement_from(
    a: lcc_raw::Rect,
    crop: lcc_raw::Rect,
    orientation: lcc_geom::Orientation,
    full_active: Option<[usize; 2]>,
    window: Option<lcc_raw::SensorWindow>,
) -> lcc_raw::gaintable::SourcePlacement {
    let (aw, ah) = (a.width.max(1) as f64, a.height.max(1) as f64);
    // (the default crop within the active area, the whole area without one: `RawImage::develop_crop`)
    let c = crop.clipped(a.width, a.height);
    let c = if c.width == 0 || c.height == 0 { lcc_raw::Rect::new(0, 0, a.width, a.height) } else { c };
    let rect = [c.x as f64 / aw, c.y as f64 / ah, c.width as f64 / aw, c.height as f64 / ah];
    let window = window.filter(|w| Some(w.size) == full_active);
    lcc_raw::gaintable::SourcePlacement { active_size: full_active, window, ..lcc_raw::gaintable::SourcePlacement::new(rect, orientation) }
}

/// Where a raw file's developed picture sits on its sensor, from its header (no decode): what a flat-field map is
/// matched against before the photo is rendered. `None`: not a raw this build reads.
pub fn placement_of(bytes: &[u8]) -> Option<lcc_raw::gaintable::SourcePlacement> {
    lcc_raw::probe(bytes)?;
    let info = lcc_raw::probe_info(bytes).ok()?;
    let active = [info.active_area.width, info.active_area.height];
    let window = lcc_raw::sensor_window(bytes, active);
    Some(placement_from(info.active_area, info.crop, info.orientation, Some(active), window))
}

/// The semantic masks of a DNG that AI masks understand, over the developed image (default crop,
/// oriented like it). Person mattes without a confidently selected pixel are left out (the
/// Subject mask then falls back to its heuristic); a sky matte counts even when empty (no sky).
fn dng_mattes(bytes: &[u8], raw: &lcc_raw::RawInfo) -> Option<Arc<lcc_pipeline::masks::Mattes>> {
    use lcc_pipeline::masks::{MatteKind, Mattes};
    if raw.format != lcc_raw::RawFormat::Dng {
        return None;
    }
    let mut mattes = Mattes::default();
    for m in lcc_raw::semantic_masks(bytes) {
        let Some(kind) = matte_kind(&m.name) else { continue };
        let Some(img) = m.developed(raw.active_area, raw.crop) else { continue };
        if kind != MatteKind::Sky && !img.data.iter().any(|&v| v >= 128) {
            continue;
        }
        mattes.push(kind, img.into_oriented(raw.orientation));
    }
    (!mattes.is_empty()).then(|| Arc::new(mattes))
}

/// What a semantic mask selects, by its `SemanticName`: Apple's (iPhone ProRAW) are named after
/// their auxiliary image types, `urn:com:apple:photo:<year>:aux:<type>`. Other names: `None`.
fn matte_kind(name: &str) -> Option<lcc_pipeline::masks::MatteKind> {
    use lcc_pipeline::masks::MatteKind::*;
    let kind = name.strip_prefix("urn:com:apple:photo:")?.rsplit(':').next()?;
    Some(match kind {
        "semanticskymatte" => Sky,
        "semanticskinmatte" => Skin,
        "semantichairmatte" => Hair,
        "semanticteethmatte" => Teeth,
        "semanticglassesmatte" => Glasses,
        "portraiteffectsmatte" => Person,
        _ => return None,
    })
}

/// A DNG `ProfileToneCurve` (linear in, linear out, 1.0 = white after exposure compensation) as the
/// finish stage's camera tone curve: 32 knots, log-spaced over 12 stops below white; above white
/// the camera tone's shoulder continues it.
pub(crate) fn dng_tone_curve(curve: &lcc_raw::profile::ToneCurve) -> Option<lcc_pipeline::tone::CameraTone> {
    let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
        [x, curve.eval(x).min(0.9995)]
    });
    lcc_pipeline::tone::CameraTone::new(knots)
}

/// Orientation for an embedded preview: its own EXIF orientation when it has one, else the raw file's.
fn preview_orientation(raw_bytes: &[u8], jpeg_orientation: u16) -> Orientation {
    if jpeg_orientation > 1 {
        return Orientation::from_exif(jpeg_orientation);
    }
    lcc_meta::extract(raw_bytes).orientation.unwrap_or(Orientation::Normal)
}

/// What stands in for a raw file we can't decode: its largest embedded JPEG (with `covering`, the smallest one that is
/// at least that big, [`lcc_raw::embedded_preview_covering`]), else, for a TIFF-based raw, the file's own first
/// image when the TIFF reader can show it (a reduced RGB copy: the only preview some containers carry). Returns image
/// bytes for [`lcc_codecs::decode`].
fn stand_in_image(bytes: &[u8], covering: Option<usize>) -> Option<std::borrow::Cow<'_, [u8]>> {
    let jpeg = match covering {
        Some(edge) => lcc_raw::embedded_preview_covering(bytes, u32::try_from(edge).unwrap_or(u32::MAX)),
        None => lcc_raw::embedded_preview(bytes),
    };
    if let Some(jpeg) = jpeg {
        return Some(std::borrow::Cow::Owned(jpeg));
    }
    let undecodable = lcc_raw::probe(bytes).is_some_and(|f| !f.is_supported());
    (undecodable && lcc_codecs::sniff(bytes).is_some_and(|f| f == lcc_codecs::Format::Tiff)).then_some(std::borrow::Cow::Borrowed(bytes))
}

/// Shared colour interpretation for quick previews, unsupported RAW fallback and camera-look fitting (`covering`: see
/// [`stand_in_image`]).
pub(crate) fn decode_raw_preview(bytes: &[u8], opts: lcc_codecs::DecodeOptions, covering: Option<usize>) -> Option<lcc_codecs::Decoded> {
    let jpeg = stand_in_image(bytes, covering)?;
    // the colour space is a property of the JPEG; a TIFF stand-in (the file itself, or a Hasselblad RGB preview
    // wrapped as a TIFF) and a JPEG XL preview are read by their own decoders
    let is_jpeg = jpeg.starts_with(&[0xff, 0xd8]);
    match lcc_raw::embedded_preview_color_space(bytes).filter(|_| is_jpeg) {
        Some(space) => {
            let space = match space {
                lcc_raw::PreviewColorSpace::Srgb => lcc_codecs::NamedSpace::Srgb,
                lcc_raw::PreviewColorSpace::AdobeRgb => lcc_codecs::NamedSpace::AdobeRgb,
            };
            lcc_codecs::decode_jpeg_with_fallback(&jpeg, opts, space).ok()
        }
        None => lcc_codecs::decode(&jpeg, opts).ok(),
    }
}

/// Oriented size of the embedded preview of a raw file.
fn embedded_preview_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let jpeg = stand_in_image(bytes, None)?;
    // headers only, as for other images (issue #367): decoding a full-size preview just for its
    // size took about a second per file in an import; a decode remains the fallback
    let (mut w, mut h, orientation) = match lcc_codecs::read_header(&jpeg) {
        Ok(header) => (header.width, header.height, header.orientation),
        Err(_) => {
            let d = lcc_codecs::decode(&jpeg, lcc_codecs::DecodeOptions::fit(64, 64)).ok()?;
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
pub fn embedded_preview_srgb(bytes: &[u8], max_edge: usize) -> Option<lcc_raster::Rgba8> {
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
fn fit_box(max_edge: usize) -> lcc_codecs::DecodeOptions {
    let e = u32::try_from(max_edge).unwrap_or(u32::MAX);
    lcc_codecs::DecodeOptions::fit(e, e)
}

/// Filesystem-backed embedded-preview hook (native).
///
/// The quick path reads the file's header and its embedded preview and nothing else (see
/// [`preview_window`]); a container the header cannot point into is planned from its tags
/// (`lcc_raw::sparse::preview_plan`, the same module the import path probes with), and only a
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
        embedded_preview_srgb(&crate::bounded_read::read_bounded(std::path::Path::new(path), crate::bounded_read::MAX_MEDIA_BYTES).ok()?, max_edge)
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
/// that covers it ([`lcc_raw::embedded_preview_covering`]).
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
    let Some(range) = lcc_raw::bounded_preview(&prefix) else {
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
    if lcc_raw::embedded_preview_from_range(&window, range).is_none() {
        return Ok(None);
    }
    let read = PreviewRead { read: bytes_read, total, header_path: true, planned: false, adobe_rgb: false };
    // On a network share the bytes are the wait, so say what this preview actually cost. A window
    // taken from the prefix cost nothing beyond the prefix itself.
    if lcc_pipeline::profiling() {
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
const PLAN_PREFIX: u64 = 64 * 1024;

/// The preview of a container the header-only path cannot answer for, from the byte ranges
/// [`lcc_raw::sparse::preview_plan`] says [`lcc_raw::embedded_preview`] reads.
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
    use std::io::{Read, Seek, SeekFrom};

    if total > MAX_PREVIEW_PLAN_BYTES {
        return Ok(None);
    }
    let Ok(len) = usize::try_from(total) else { return Ok(None) };
    let mut buf = vec![0u8; len];
    let header = read_prefix_at(file, path, PLAN_PREFIX.min(total))?;
    let Some(head) = buf.get_mut(..header.len()) else { return Ok(None) };
    head.copy_from_slice(&header);
    let mut loaded = lcc_raw::sparse::Ranges::new();
    loaded.insert(0, header.len() as u64);
    for _ in 0..MAX_PREVIEW_PLAN_ROUNDS {
        match lcc_raw::sparse::preview_plan(&buf, &loaded) {
            lcc_raw::sparse::HeaderPlan::Need(ranges) => {
                let wanted = loaded.total().saturating_add(ranges.total());
                if wanted.saturating_mul(10) > total.saturating_mul(MAX_PREVIEW_PLAN_SHARE) {
                    return Ok(None);
                }
                for &(start, end) in ranges.spans() {
                    let (Ok(a), Ok(b)) = (usize::try_from(start), usize::try_from(end)) else { return Ok(None) };
                    let Some(dst) = buf.get_mut(a..b) else { return Ok(None) };
                    file.seek(SeekFrom::Start(start)).map_err(|e| format!("{path}: {e}"))?;
                    file.read_exact(dst).map_err(|e| format!("{path}: {e}"))?;
                    loaded.insert(start, end);
                }
            }
            lcc_raw::sparse::HeaderPlan::Ready => {
                // the plan loads every preview's strips, so the JPEG picked for `max_edge` is the whole file's pick
                let edge = u32::try_from(max_edge).unwrap_or(u32::MAX);
                let Some(window) = lcc_raw::embedded_preview_covering(&buf, edge) else { return Ok(None) };
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
                let adobe_rgb = lcc_raw::embedded_preview_color_space(&buf) == Some(lcc_raw::PreviewColorSpace::AdobeRgb);
                // `extract` copies a TIFF's bytes to hand on its Exif block, so ask the cheap side
                // first: a JPEG that carries its own orientation needs nothing from the container.
                if lcc_meta::extract(&window).orientation.is_none()
                    && lcc_meta::extract(&header).orientation.is_none()
                    && lcc_meta::extract(&buf).orientation.is_some()
                {
                    return Ok(None);
                }
                let read = PreviewRead { read: loaded.total(), total, header_path: true, planned: true, adobe_rgb };
                // A plan-driven preview is the one that used to cost the whole file, so it is worth
                // saying what it cost now too.
                if lcc_pipeline::profiling() {
                    eprintln!("[profile] preview {path}: read {} of {} bytes (plan, window {})", read.read, total, window.len());
                }
                // The planner's buffer is the file's length, so give its pages back the way a decode
                // does (see `fs_pair_loader`).
                drop(buf);
                crate::memory::release();
                return Ok(Some((header, window, read)));
            }
            lcc_raw::sparse::HeaderPlan::Whole(_) => return Ok(None),
        }
    }
    Ok(None)
}

/// The file's first [`lcc_raw::PREVIEW_PREFIX`] bytes (or the whole file when it is shorter),
/// which is what every format [`lcc_raw::bounded_preview`] covers keeps its pointer in.
fn read_prefix(file: &mut std::fs::File, path: &str, total: u64) -> Result<Vec<u8>, String> {
    let len = lcc_raw::PREVIEW_PREFIX.min(usize::try_from(total).unwrap_or(usize::MAX));
    read_prefix_at(file, path, len as u64)
}

/// The file's first `len` bytes, read from the start.
fn read_prefix_at(file: &mut std::fs::File, path: &str, len: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = usize::try_from(len).map_err(|_| format!("{path}: file too large"))?;
    let mut prefix = vec![0u8; len];
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{path}: {e}"))?;
    file.read_exact(&mut prefix).map_err(|e| format!("{path}: {e}"))?;
    Ok(prefix)
}

/// The end of a Fuji RAF's preview block, from the two big-endian u32s every RAF header carries at
/// 84 (offset) and 88 (length) — the same fields `lcc_raw`'s RAF reader uses, read here
/// without the rest of the file so the prefix can be made big enough first.
fn raf_preview_extent(prefix: &[u8]) -> Option<u64> {
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
fn quick_image_srgb(header: &[u8], window: &[u8], adobe_rgb: bool, max_edge: usize) -> Option<lcc_raster::Rgba8> {
    let decoded = if adobe_rgb && window.starts_with(&[0xff, 0xd8]) {
        lcc_codecs::decode_jpeg_with_fallback(window, fit_box(max_edge), lcc_codecs::NamedSpace::AdobeRgb)
    } else {
        lcc_codecs::decode(window, fit_box(max_edge))
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
    Arc::new(|path: &str, max_edge: usize, spec: &DenoiseSpec, options: LoadOptions| {
        let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 4 } else { 10 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = crate::bounded_read::read_bounded(std::path::Path::new(path), crate::bounded_read::MAX_MEDIA_BYTES)
            .map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec_pair(bytes, max_edge, spec, options.at(path));
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
    let loader: FileLoader = Arc::new(|path: &str, max_edge: usize, options: LoadOptions| {
        fs_load(path, max_edge, |bytes| load_vec_with(bytes, max_edge, options.at(path)))
    });
    let probe: FileProbe = Arc::new(|path: &str| {
        // a file still being copied in is left for a later scan (issue #123): its name or an open
        // writer says so before anything is read, its own header (below) after
        if lcc_catalog::incoming::still_being_written(std::path::Path::new(path)) {
            return Err(format!("{path}: {}", lcc_catalog::incoming::STILL_BEING_WRITTEN));
        }
        // a clip is streamed (hash) and its headers read in place: never all of it in memory
        if crate::import::is_video_name(path) {
            let mut info = probe_video_file(std::path::Path::new(path))?;
            info.file_modified = file_modified(path);
            return Ok(info);
        }
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let _permit = crate::memory::work_gate().acquire(len);
        let bytes = crate::bounded_read::read_bounded(std::path::Path::new(path), crate::bounded_read::MAX_MEDIA_BYTES)
            .map_err(|e| format!("{path}: {e}"))?;
        // (a short file that hasn't changed for a while is corrupt: the decode below reports it)
        if lcc_catalog::incoming::declared_end_past(&bytes, bytes.len() as u64)
            && lcc_catalog::incoming::recently_modified(std::path::Path::new(path))
        {
            return Err(format!("{path}: {}", lcc_catalog::incoming::STILL_BEING_WRITTEN));
        }
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = file_modified(path);
        Ok(info)
    });
    (loader, probe)
}

/// The filesystem [`ReducedLoader`]: [`load_vec_reduced`] of the file, gated like [`fs_hooks`]' loader.
pub fn fs_reduced_loader() -> ReducedLoader {
    Arc::new(|path: &str, max_edge: usize, options: LoadOptions| fs_load(path, max_edge, |bytes| load_vec_reduced(bytes, max_edge, options.at(path))))
}

/// Read `path` and `load` it, waiting on [`crate::memory::work_gate`] as [`fs_hooks`] explains.
fn fs_load<T>(path: &str, max_edge: usize, load: impl FnOnce(Vec<u8>) -> Result<T, String>) -> Result<T, String> {
    let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
    let weight = len.saturating_mul(if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 });
    let gate = crate::memory::work_gate();
    let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
    let bytes =
        crate::bounded_read::read_bounded(std::path::Path::new(path), crate::bounded_read::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}"))?;
    let r = load(bytes);
    // the file, the samples and the intermediate images are gone: give their pages back
    crate::memory::release();
    r
}

/// `path`'s modification time in this computer's time zone (see [`ProbeInfo::file_modified`]);
/// `None` when the filesystem doesn't say or the time is outside 1970..=9999.
fn file_modified(path: &str) -> Option<String> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    lcc_catalog::dates::local(secs)
}

/// Whether the file at `path` is as it was when `info` probed it: still a file, the same size and the same
/// modification time (to the second, as the probe records it). A file edited in place keeps its size when only
/// a few bytes change (an embedded XMP packet rewritten at the same length), so the size alone can't tell; for
/// the same reason a file whose modification time isn't known, now or then, counts as changed (upstream #3607).
pub(crate) fn probe_still_valid(path: &str, info: &ProbeInfo) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() != info.file_size {
        return false;
    }
    match (file_modified(path), info.file_modified.as_deref()) {
        (Some(now), Some(then)) => now == then,
        _ => false,
    }
}

impl crate::Session {
    /// Install the filesystem file hooks (desktop, CLI, MCP headless), and on macOS the Apple Photos
    /// runner ([`crate::apple_photos::osascript`]; installed, not run).
    pub fn with_fs(mut self) -> Self {
        // installed `.dcp` camera profiles, before anything asks which camera has a colour model
        crate::camera_profiles::install_dcps();
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
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            self.trash = Some(crate::disk_delete::system_trash());
        }
        self
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// A raw cut short (still being copied) and a temporary name are refused by the file probe as
    /// still being written (issue #123), which the scans and imports skip.
    /// Make and Model are kept separately for exports; the label drops a repeated maker (upstream #1618).
    #[test]
    fn imported_camera_keeps_make_and_model() {
        let m = lcc_meta::Metadata { make: Some("Canon".into()), model: Some("Canon IXUS 185".into()), ..Default::default() };
        let (meta, _) = super::meta_of(&m);
        assert_eq!((meta.camera_make.as_str(), meta.camera_model.as_str()), ("Canon", "Canon IXUS 185"));
        assert_eq!(meta.camera, "Canon IXUS 185");
        let m = lcc_meta::Metadata { make: Some("Sony".into()), model: Some("A7 IV".into()), ..Default::default() };
        assert_eq!(super::meta_of(&m).0.camera, "Sony A7 IV");
    }

    #[test]
    fn files_still_being_copied_are_left_for_a_later_scan() {
        let dir = std::env::temp_dir().join(format!("copy-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // a TIFF header declaring one strip of 4000 bytes at offset 64, with only 100 of them there
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(b"II*\0");
        b[4..8].copy_from_slice(&8u32.to_le_bytes());
        b[8..10].copy_from_slice(&2u16.to_le_bytes());
        for (at, tag, v) in [(10usize, 0x0111u16, 64u32), (22, 0x0117, 4000)] {
            b[at..at + 2].copy_from_slice(&tag.to_le_bytes());
            b[at + 2..at + 4].copy_from_slice(&4u16.to_le_bytes());
            b[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
            b[at + 8..at + 12].copy_from_slice(&v.to_le_bytes());
        }
        b.extend([0u8; 100]);
        let raw = dir.join("IMG_0001.NEF");
        std::fs::write(&raw, &b).unwrap();
        let part = dir.join("IMG_0002.NEF.part");
        std::fs::write(&part, &b).unwrap();
        let (_, probe) = super::fs_hooks();
        for f in [&raw, &part] {
            let e = probe(&f.to_string_lossy()).unwrap_err();
            assert!(lcc_catalog::incoming::is_still_being_written(&e), "{e}");
        }
        // the same short file, unchanged for an hour, is corrupt: reported, not skipped forever
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&raw).unwrap().set_modified(hour_ago).unwrap();
        if let Err(e) = probe(&raw.to_string_lossy()) {
            assert!(!lcc_catalog::incoming::is_still_being_written(&e), "{e}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Keywords from a file keep their hierarchy: the paths of `lr:hierarchicalSubject`, not the
    /// flat names of `dc:subject` that the app's exports (and Lightroom Classic) write besides
    /// them; an exported `travel|Italy|Rome` comes back as itself.
    #[test]
    fn a_lens_without_a_name_is_shown_by_its_range() {
        // a Nikon DSLR raw: no LensModel, the range from the maker note
        let m = lcc_meta::Metadata { lens_spec: Some([18.0, 135.0, 3.5, 5.6]), ..Default::default() };
        assert_eq!(super::meta_of(&m).0.lens, "18.0-135.0 mm f/3.5-5.6");
        let named =
            lcc_meta::Metadata { lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()), lens_spec: Some([24.0, 70.0, 4.0, 4.0]), ..Default::default() };
        assert_eq!(super::meta_of(&named).0.lens, "NIKKOR Z 24-70mm f/4 S");
        assert_eq!(super::meta_of(&lcc_meta::Metadata::default()).0.lens, "");
    }

    #[test]
    fn imported_keywords_keep_their_hierarchy() {
        let m = lcc_meta::Metadata {
            keywords: vec!["Rome".into(), "Italy".into(), "travel".into(), "beach".into()],
            hierarchical_keywords: vec!["travel|Italy|Rome".into()],
            ..Default::default()
        };
        assert_eq!(super::meta_of(&m).0.keywords, ["travel|Italy|Rome", "beach"]);
    }

    /// The third-stop speeds between 1/4 s and 4 s — tripod, night and long-exposure work — read as cameras
    /// write them, not rounded to a whole step (issue #1619: 0.4 s showed and exported as 1/2, 1.3 s as 1″).
    #[test]
    fn imported_shutter_speeds_keep_their_step() {
        let shutter = |t: f64| super::meta_of(&lcc_meta::Metadata { exposure_time: Some(t), ..Default::default() }).0.shutter;
        for (t, want) in [
            (0.3, "0.3"),
            (0.4, "0.4"),
            (0.6, "0.6"),
            (0.8, "0.8"),
            (1.3, "1.3\u{2033}"),
            (1.6, "1.6\u{2033}"),
            (2.5, "2.5\u{2033}"),
            (3.2, "3.2\u{2033}"),
        ] {
            assert_eq!(shutter(t), want, "{t} s");
            // ... and the catalog string reads back as the time the file held: what export writes to EXIF/XMP
            let back = lcc_catalog::parse_shutter_seconds(&shutter(t));
            assert!(back.is_some_and(|s| (s - t).abs() < 0.05), "{t} s → {back:?}");
        }
        // times the old rounding already got right are unchanged
        for (t, want) in [(0.004, "1/250"), (0.5, "0.5"), (2.0, "2\u{2033}"), (30.0, "30\u{2033}")] {
            assert_eq!(shutter(t), want, "{t} s");
        }
        assert_eq!(shutter(f64::NAN), "", "no time, no reading");
        assert_eq!(shutter(-1.0), "", "no time, no reading");
    }

    #[test]
    fn psd_probe_reads_embedded_capture_time_gps_and_title() {
        let capture = lcc_meta::DateTime::parse_iso("2023-05-14T14:40:39").unwrap();
        let metadata = lcc_meta::Metadata {
            capture_time: Some(capture),
            gps: Some(lcc_meta::Gps { latitude: 45.0, longitude: 7.0, altitude: None }),
            title: Some("PSD metadata".into()),
            ..Default::default()
        };
        let bytes = crate::tests_xmp::synthetic_psd_with_metadata(&metadata);
        let probe = probe_bytes("metadata.psd", &bytes).unwrap();
        assert_eq!(probe.captured.as_deref(), Some("2023-05-14T14:40:39"));
        assert_eq!(probe.meta.gps, Some((45.0, 7.0)));
        assert_eq!(probe.meta.title, "PSD metadata");
    }

    use super::*;
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};

    /// Which white-balance scale a raw gets (issue #730): a file with matrices of its own is absolute from them; a
    /// fitted-look file is absolute through its camera's measured matrices, which only read the as-shot
    /// multipliers as Kelvin; any other raw stays relative (6500 / 0 = as shot). The catalog's decision from the
    /// format and camera string agrees.
    #[test]
    fn white_balance_scale_follows_the_files_matrices_then_the_cameras() {
        use lcc_raw::{ColorData, RawFormat};
        let meta = |make: &str, model: &str| lcc_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
        let mul = Some([2.2f32, 1.0, 1.6]);
        let d65 = lcc_color::D65;
        // a covered camera: Kelvin through the spectral matrices, near the daylight these multipliers describe
        let a7m3 = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "ILCE-7M3"));
        assert!(!a7m3.own_matrix && !a7m3.relative && a7m3.legacy_relative && a7m3.scale_white.is_some());
        let (t, tint) = a7m3.as_shot(d65);
        assert!((3500.0..7000.0).contains(&t) && t != 6500.0 && tint.abs() < 30.0 && t.fract() == 0.0 && tint.fract() == 0.0, "{t} {tint}");
        assert!(!lcc_catalog::relative_wb_camera("ARW", "SONY ILCE-7M3"));
        // the same camera without multipliers: the scale is Kelvin still, the white falls back to the tags' (D65)
        let bare = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), None, &meta("SONY", "ILCE-7M3"));
        assert!(bare.legacy_relative && bare.scale_white.is_none());
        assert_eq!(bare.as_shot(d65), {
            let (t, tint) = xy_to_temp_tint(d65);
            (t.round(), tint.round())
        });
        // a camera with neither measured matrices nor presets but a row of dcraw's matrix table (the DSLR-A100):
        // Kelvin through that matrix, edits from before marked legacy, a daylight-ish reading of daylight multipliers
        let a100 = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "DSLR-A100"));
        assert!(!a100.relative && a100.legacy_relative && a100.scale_white.is_some());
        let (t, _) = a100.as_shot(d65);
        assert!((3000.0..9000.0).contains(&t), "{t}");
        assert!(!lcc_catalog::relative_wb_camera("ARW", "SONY DSLR-A100"));
        assert!(matches!(a100.dng_white(&ColorData::default(), d65), DngWhite::Matrices(_)));
        // a camera no table covers: relative, whatever the multipliers
        let none = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("HASSELBLAD", "Lunar"));
        assert!(none.relative && !none.legacy_relative && none.scale_white.is_none());
        assert_eq!(none.as_shot(d65), (6500.0, 0.0));
        assert!(lcc_catalog::relative_wb_camera("ARW", "HASSELBLAD Lunar"));
        // a camera without matrices whose presets give the scale: Kelvin through them, Sony's daylight preset as
        // 5500 K or so; without presets in the file the scale is Kelvin still, the white the tags' (D65)
        let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
        let presets = lcc_raw::kelvin_presets::KelvinPresets::new([
            (2500.0, g(1336.0, 3672.0)),
            (3200.0, g(1720.0, 2620.0)),
            (4500.0, g(2256.0, 1880.0)),
            (6000.0, g(2680.0, 1544.0)),
            (8500.0, g(3136.0, 1300.0)),
        ]);
        let with = ColorData { kelvin_presets: presets, ..Default::default() };
        let a99m2 = WbScaleOf::new(RawFormat::Arw, &with, Some(g(2500.0, 1668.0)), &meta("SONY", "ILCA-99M2"));
        assert!(!a99m2.own_matrix && !a99m2.relative && a99m2.legacy_relative && a99m2.scale_white.is_some());
        let (t, tint) = a99m2.as_shot(d65);
        assert!((5000.0..6000.0).contains(&t) && tint.abs() < 10.0 && t.fract() == 0.0, "{t} {tint}");
        assert!(!lcc_catalog::relative_wb_camera("ARW", "SONY ILCA-99M2"));
        // a Canon body without matrices, through the presets its ColorData lists (PowerShot G1 X Mark III, a
        // Daylight shot with the camera's white balance shifted towards blue: below Daylight's 5651 K)
        let canon = lcc_raw::kelvin_presets::KelvinPresets::from_unordered([
            (5651.0, g(1418.0, 1357.0)),
            (7505.0, g(1653.0, 1171.0)),
            (6594.0, g(1533.0, 1259.0)),
            (3174.0, g(963.0, 2016.0)),
            (8529.0, g(1807.0, 1080.0)),
        ]);
        let with_canon = ColorData { kelvin_presets: canon, ..Default::default() };
        let g1x3 = WbScaleOf::new(RawFormat::Cr2, &with_canon, Some(g(1264.0, 1537.0)), &meta("Canon", "Canon PowerShot G1 X Mark III"));
        assert!(!g1x3.relative && g1x3.scale_white.is_some());
        let (t, tint) = g1x3.as_shot(d65);
        assert!((4300.0..5200.0).contains(&t) && tint.abs() < 10.0, "{t} {tint}");
        assert!(!lcc_catalog::relative_wb_camera("CR2", "Canon Canon PowerShot G1 X Mark III"));
        let bare = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "ILCA-99M2"));
        assert!(bare.legacy_relative && bare.scale_white.is_none());
        // data the camera balanced already (Sony's downsized lossless ARW: unit multipliers) read As Shot from the
        // levels it balanced them with, through the presets and through measured matrices alike; read as unit
        // multipliers, the ILCE-7M3's matrices would put them near 1700 K
        let balanced = ColorData { balanced_as_shot: Some(g(2500.0, 1668.0)), ..with.clone() };
        let ycbcr = WbScaleOf::new(RawFormat::Arw, &balanced, Some([1.0; 3]), &meta("SONY", "ILCA-99M2"));
        assert_eq!(ycbcr.as_shot(d65), a99m2.as_shot(d65));
        let balanced = ColorData { balanced_as_shot: mul, ..Default::default() };
        let ycbcr = WbScaleOf::new(RawFormat::Arw, &balanced, Some([1.0; 3]), &meta("SONY", "ILCE-7M3"));
        assert_eq!(ycbcr.as_shot(d65), a7m3.as_shot(d65));
        let unit = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), Some([1.0; 3]), &meta("SONY", "ILCE-7M3"));
        assert!(unit.as_shot(d65).0 < 2500.0, "{:?}", unit.as_shot(d65));
        // every Sony body but the three without presets is covered (the ILCE-1 has no measured matrices)
        let a1 = WbScaleOf::new(RawFormat::Arw, &with, Some(g(2500.0, 1668.0)), &meta("SONY", "ILCE-1"));
        assert!(!a1.relative && a1.scale_white.is_some() && !lcc_catalog::relative_wb_camera("ARW", "SONY ILCE-1"));
        // presets in a file whose camera isn't named as a Sony body: relative, as the catalog decides from the camera alone
        let other = WbScaleOf::new(RawFormat::Arw, &with, mul, &meta("HASSELBLAD", "Lunar"));
        assert!(other.relative && other.scale_white.is_none());
        let unknown = WbScaleOf::new(RawFormat::Nef, &ColorData::default(), mul, &lcc_meta::Metadata::default());
        assert!(unknown.relative && unknown.as_shot(d65) == (6500.0, 0.0));
        // a file with matrices of its own (DNG): absolute from them, no matter whose camera
        let own = ColorData { illuminant: [21, 0], color_matrix: [Some(lcc_color::Mat3::IDENTITY), None], ..Default::default() };
        let dng = WbScaleOf::new(RawFormat::Dng, &own, mul, &meta("SONY", "ILCE-7M3"));
        assert!(dng.own_matrix && !dng.relative && !dng.legacy_relative && dng.scale_white.is_none());
        let xy = lcc_color::cct::temp_tint_to_xy(4200.0, 12.0);
        assert_eq!(dng.as_shot(xy), (4200.0, 12.0));
        assert!(!lcc_catalog::relative_wb_camera("DNG", "SONY ILCE-7M3"));
    }

    /// A Fujifilm X-Trans III body reads Kelvin through the neutral pair its RAF stores (issue #730); without the pair
    /// in the file, or on a body with matrices (X-T3), nothing changes.
    #[test]
    fn a_fujifilm_x_trans_iii_raf_reads_kelvin_through_its_neutral_pair() {
        use lcc_raw::{ColorData, RawFormat};
        let meta = |make: &str, model: &str| lcc_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
        let a = [366.0f32 / 302.0, 1.0, 866.0 / 302.0];
        let d65 = [646.0f32 / 302.0, 1.0, 498.0 / 302.0];
        let pair = lcc_raw::kelvin_presets::KelvinPresets::from_neutral_pair(a, d65);
        assert!(pair.is_some());
        let with = ColorData { kelvin_presets: pair, ..Default::default() };
        let xt20 = WbScaleOf::new(RawFormat::Raf, &with, Some(d65), &meta("FUJIFILM", "X-T20"));
        assert!(!xt20.relative && xt20.scale_white.is_some());
        let (t, tint) = xt20.as_shot(lcc_color::cct::temp_tint_to_xy(6500.0, 0.0));
        assert!((6000.0..7000.0).contains(&t) && tint.abs() < 25.0, "{t} {tint}");
        assert!(!lcc_catalog::relative_wb_camera("RAF", "FUJIFILM X-T20"));
        let bare = WbScaleOf::new(RawFormat::Raf, &ColorData::default(), Some(d65), &meta("FUJIFILM", "X-T20"));
        assert!(bare.scale_white.is_none());
    }

    /// A merged DNG (Photo Merge) of a raw without colour matrices shows the As Shot temperature and tint its source
    /// shows, on every scale: through the measured matrices (ILCE-7M3), through the presets (ILCA-99M2), the relative
    /// scale's 6500 K / 0 (ILCE-7M2) and a file's own white (an unfitted format). Merges of real ILCE-7M3 brackets
    /// opened at 6113–7307 K / +115 where the ARWs show 3932–5268 K / +7.
    #[test]
    fn a_merged_dng_shows_the_white_balance_its_source_shows() {
        use lcc_raw::{ColorData, RawFormat};
        let meta = |make: &str, model: &str| lcc_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
        let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
        let presets = lcc_raw::kelvin_presets::KelvinPresets::new([
            (2500.0, g(1336.0, 3672.0)),
            (3200.0, g(1720.0, 2620.0)),
            (4500.0, g(2256.0, 1880.0)),
            (6000.0, g(2680.0, 1544.0)),
            (8500.0, g(3136.0, 1300.0)),
        ]);
        let with_presets = ColorData { kelvin_presets: presets, ..Default::default() };
        let cases = [
            (RawFormat::Arw, ColorData::default(), [2.5039f32, 1.0, 1.5078], meta("SONY", "ILCE-7M3")),
            (RawFormat::Arw, with_presets, g(2500.0, 1668.0), meta("SONY", "ILCA-99M2")),
            (RawFormat::Arw, ColorData::default(), [2.2, 1.0, 1.6], meta("SONY", "DSLR-A100")),
            (RawFormat::Rw2, ColorData::default(), [2.0, 1.0, 1.4], meta("Panasonic", "DC-G9")),
        ];
        for (format, color, mul, meta) in cases {
            let own = lcc_raw::color::as_shot_white_of(&color, Some(mul));
            let source = WbScaleOf::new(format, &color, Some(mul), &meta);
            let t = lcc_raw::color::camera_transform_of(&color, own);
            let dng = crate::camera_preview::dng_colour(&color, &source.dng_white(&color, own), &t, None).unwrap();
            let merged = WbScaleOf::new(RawFormat::Dng, &dng, None, &meta);
            let shown = source.as_shot(own);
            assert_eq!(merged.as_shot(lcc_raw::color::as_shot_white_of(&dng, None)), shown, "{:?}", meta.model);
        }
    }

    /// A RAF whose preview pointer is in its header, then a large raw tail the reader must not need.
    fn raf_with_embedded_jpeg(jpeg: &[u8], tail: usize) -> Vec<u8> {
        let head = 160;
        let mut f = vec![0u8; head];
        f.get_mut(..16).expect("head").copy_from_slice(b"FUJIFILMCCD-RAW ");
        f.get_mut(84..88).expect("jpeg offset").copy_from_slice(&(head as u32).to_be_bytes());
        f.get_mut(88..92).expect("jpeg length").copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
        f.extend_from_slice(jpeg);
        f.extend(std::iter::repeat_n(0x33u8, tail));
        f
    }

    /// A raw's preview is read from its header and its JPEG only. Every preview used to cost a
    /// whole-file read, which on a 56 MB RAF over a network share is ten times the bytes and ten
    /// times the wait (measured: 2.5 s local against 25.6 s over SMB, DSCF9492.RAF). The pixels the
    /// quick path produces must equal the whole-file path's, exactly.
    #[test]
    fn preview_loading_reads_only_the_header_and_the_jpeg() {
        let (w, h) = (48u32, 32u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 251) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        let file = raf_with_embedded_jpeg(&jpeg, 200_000);
        let dir = crate::tests_xmp::temp_dir("preview-window");
        let path = dir.join("dscf0001.raf");
        std::fs::write(&path, &file).unwrap();
        let path = path.to_string_lossy().into_owned();
        let max_edge = 512;

        let (header, window, read) = preview_window(&path, max_edge).expect("the file reads").expect("it has a preview");
        assert_eq!(header.len(), lcc_raw::PREVIEW_PREFIX, "the header prefix is bounded");
        assert!(read.header_path, "the header path is what answered");
        assert!(!read.planned, "a RAF pointer in the header needs no planner");
        assert_eq!(read.total, file.len() as u64);
        assert!(read.fraction() < 0.5, "the read is a fraction of the file: {read:?}");
        assert!(window.len() < file.len() / 2, "the raw tail must not be read: {} of {} bytes", window.len(), file.len());

        // The quick path and the whole-file path agree byte for byte.
        let quick = quick_image_srgb(&header, &window, read.adobe_rgb, max_edge).expect("the window holds a decodable preview");
        let whole = embedded_preview_srgb(&file, max_edge).expect("the whole file decodes too");
        assert_eq!((quick.width, quick.height), (whole.width, whole.height), "same preview size");
        assert_eq!(quick.data, whole.data, "byte for byte");

        // A file with no locatable preview declines (the caller then reads it whole), a missing file errors.
        let plain = dir.join("plain.jpg");
        std::fs::write(&plain, &jpeg).unwrap();
        assert!(preview_window(&plain.to_string_lossy(), max_edge).expect("a plain file reads").is_none());
        assert!(preview_window(&dir.join("nope.raf").to_string_lossy(), max_edge).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a fixture's IFD0 says about itself: the tag that makes `probe` call the file a raw, the
    /// file's own orientation, and the EXIF colour space the container declares for its preview.
    #[derive(Clone, Copy, Default)]
    struct Fixture {
        dng: bool,
        sony: bool,
        orientation: Option<u16>,
        /// EXIF `ColorSpace` (0xA001): 1 = sRGB, 0xFFFF with an "R03" InteropIndex = Adobe RGB.
        color_space: Option<u16>,
    }

    /// A TIFF-family raw no header field points into: IFD0 is the JPEG-compressed preview strip (the
    /// shape a DNG's `PreviewImage`, a CR2's or an ARW's preview takes), IFD1 a CFA "sensor" strip of
    /// `tail` bytes. The writer puts each IFD's table after its own chunk, so a `strip` bigger than
    /// the plan's bounded header puts IFD0 (and any orientation tag in it) out of that header's reach.
    fn tiff_with_preview_strip(strip: &[u8], tail: usize, what: Fixture) -> Vec<u8> {
        tiff_with_preview_strips(strip, &[], tail, what)
    }

    /// [`tiff_with_preview_strip`] that also keeps the `smaller` JPEGs in sub-IFDs of IFD0, as an ARW keeps its
    /// 1616 px preview beside the full-size one.
    fn tiff_with_preview_strips(strip: &[u8], smaller: &[&[u8]], tail: usize, what: Fixture) -> Vec<u8> {
        use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut preview = IfdBuilder::new();
        if what.dng {
            preview.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        }
        if what.sony {
            preview.set(t::MAKE, Value::Ascii("SONY".into()));
        }
        if let Some(o) = what.orientation {
            preview.set(t::ORIENTATION, Value::Short(vec![o]));
        }
        if let Some(space) = what.color_space {
            let mut exif = IfdBuilder::new();
            exif.set(0xA001, Value::Short(vec![space]));
            if space == 0xFFFF {
                let mut interop = IfdBuilder::new();
                interop.set(0x0001, Value::Undefined(b"R03".to_vec()));
                exif.set_child(t::INTEROP_IFD, interop);
            }
            preview.set_child(t::EXIF_IFD, exif);
        }
        preview.set(t::COMPRESSION, Value::Short(vec![7]));
        preview.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::RGB]));
        preview.set(t::IMAGE_WIDTH, Value::Long(vec![48]));
        preview.set(t::IMAGE_LENGTH, Value::Long(vec![32]));
        preview.set_image(ImageData::Strips { rows_per_strip: 32, strips: vec![strip.to_vec()] });
        for jpeg in smaller {
            let mut sub = IfdBuilder::new();
            sub.set(t::COMPRESSION, Value::Short(vec![7]));
            sub.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::RGB]));
            sub.set(t::IMAGE_WIDTH, Value::Long(vec![48]));
            sub.set(t::IMAGE_LENGTH, Value::Long(vec![32]));
            sub.set_image(ImageData::Strips { rows_per_strip: 32, strips: vec![jpeg.to_vec()] });
            preview.add_sub_ifd(sub);
        }
        let mut raw = IfdBuilder::new();
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![6000]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![4000]));
        raw.set_image(ImageData::Strips { rows_per_strip: 4000, strips: vec![vec![0x33u8; tail]] });
        TiffWriter::default().write(&[preview, raw]).expect("the fixture writes")
    }

    /// A raw whose preview only its tags locate — a Canon CR3's JPEG track, a TIFF raw whose preview
    /// tag sits past the prefix — is read in part too: `sparse::preview_plan` names the ranges
    /// `embedded_preview` reads, so the preview strip is read and the sensor strip is not. The pixels
    /// must equal the whole-file path's, exactly; a file the planner cannot describe must decline
    /// rather than guess, so the caller keeps reading it whole.
    #[test]
    fn preview_loading_plans_the_ranges_it_needs() {
        let (w, h) = (48u32, 32u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 29 % 241) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        let file = tiff_with_preview_strip(&jpeg, 400_000, Fixture { dng: true, ..Default::default() });
        let dir = crate::tests_xmp::temp_dir("preview-plan");
        let path = dir.join("preview.dng");
        std::fs::write(&path, &file).unwrap();
        let path = path.to_string_lossy().into_owned();
        let max_edge = 512;

        let (header, window, read) = preview_window(&path, max_edge).expect("the file reads").expect("the plan locates its preview");
        assert_eq!(header.len(), PLAN_PREFIX as usize, "the plan's header is bounded");
        assert!(read.planned, "the planner is what answered");
        assert!(read.header_path, "and it is still a bounded read");
        assert_eq!(read.total, file.len() as u64);
        assert!(read.fraction() < 0.5, "the read is a fraction of the file: {read:?} of {} bytes", file.len());
        assert!(window.len() < file.len() / 2, "the sensor strip must not be read: {} of {} bytes", window.len(), file.len());

        // The plan path and the whole-file path agree byte for byte.
        let quick = quick_image_srgb(&header, &window, read.adobe_rgb, max_edge).expect("the window holds a decodable preview");
        let whole = embedded_preview_srgb(&file, max_edge).expect("the whole file decodes too");
        assert_eq!((quick.width, quick.height), (whole.width, whole.height), "same preview size");
        assert_eq!(quick.data, whole.data, "byte for byte");

        // The same structure without the tag that makes it a raw: the planner declines, and the
        // caller's whole-file read is what answers (no guessing at what a file might hold).
        let not_a_raw = tiff_with_preview_strip(&jpeg, 400_000, Fixture::default());
        std::fs::write(&path, &not_a_raw).unwrap();
        assert!(preview_window(&path, max_edge).expect("the file reads").is_none(), "an unknown container falls back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A raw that keeps several JPEGs is shown, on the plan route as on the whole-file path, from the smallest one that
    /// covers the box (`embedded_preview_covering`). The plan route used to hand on the largest whatever the size: an
    /// A7R VI ARW's 9984 × 6656 JPEG for a 512 px thumbnail instead of its 1616 × 1080 one.
    #[test]
    fn preview_loading_plans_the_smallest_covering_jpeg() {
        let jpeg = |w: u32, h: u32| {
            let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 31 % 247) as u8).collect();
            encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&rgb)), 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap()
        };
        let (big, small) = (jpeg(96, 64), jpeg(48, 32));
        assert!(big.len() > small.len(), "the full-size JPEG is the largest");
        let file = tiff_with_preview_strips(&big, &[&small], 400_000, Fixture { dng: true, ..Default::default() });
        let dir = crate::tests_xmp::temp_dir("preview-plan-covering");
        let path = dir.join("two-previews.dng");
        std::fs::write(&path, &file).unwrap();
        let path = path.to_string_lossy().into_owned();

        for (edge, want, which) in [(48, &small, "small"), (49, &big, "full-size"), (usize::MAX, &big, "full-size")] {
            let (header, window, read) = preview_window(&path, edge).expect("the file reads").expect("the plan locates its preview");
            assert!(read.planned, "the planner is what answered");
            assert!(&window == want, "a {edge} px box takes the {which} JPEG");
            let quick = quick_image_srgb(&header, &window, read.adobe_rgb, edge).expect("the window holds a decodable preview");
            let whole = embedded_preview_srgb(&file, edge).expect("the whole file decodes too");
            assert_eq!(
                (quick.width, quick.height, quick.data),
                (whole.width, whole.height, whole.data),
                "{edge} px: the plan and the whole file agree"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file whose preview the quick path could not show the way the whole-file path does is left to
    /// the whole file. Here the orientation tag sits in an IFD0 past the bounded header (its chunk is
    /// bigger than that header) and the preview JPEG carries none of its own, so a window read would
    /// come out unrotated where the whole-file path rotates it — the plan must decline instead of
    /// handing on a wrong picture.
    #[test]
    fn preview_loading_declines_an_orientation_only_the_whole_file_shows() {
        let (w, h) = (48u32, 32u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 17 % 239) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        // the strip (what the IFD's chunk covers) is padded past the plan's header, which is what puts
        // IFD0 out of its reach; `embedded_preview` trims back to the JPEG's end-of-image
        let mut strip = jpeg.clone();
        strip.extend(std::iter::repeat_n(0u8, PLAN_PREFIX as usize));
        let file = tiff_with_preview_strip(&strip, 400_000, Fixture { dng: true, orientation: Some(6), ..Default::default() });
        let dir = crate::tests_xmp::temp_dir("preview-plan-orientation");
        let path = dir.join("rotated.dng");
        std::fs::write(&path, &file).unwrap();
        let path = path.to_string_lossy().into_owned();

        assert_eq!(lcc_meta::extract(&file).orientation, Some(Orientation::from_exif(6)), "the fixture states an orientation");
        assert!(lcc_meta::extract(&file[..PLAN_PREFIX as usize]).orientation.is_none(), "which the bounded header cannot see");
        assert!(lcc_meta::extract(&jpeg).orientation.is_none(), "and the preview JPEG carries none of its own");
        assert!(preview_window(&path, usize::MAX).expect("the file reads").is_none(), "the whole-file path keeps it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A container can declare what its preview JPEG is in, and the whole-file path applies that
    /// declaration (`decode_raw_preview`). The planned path reads it too and the quick decode applies
    /// it, so a container that declares Adobe RGB (a Sony α1's ARW) is read in part like any other and
    /// shows the same pixels as the whole-file path (issue #1440; it used to be read whole) — and one
    /// that declares sRGB (what most bodies write) keeps the fast path as before.
    #[test]
    fn preview_loading_applies_a_container_colour_space() {
        use lcc_raw::PreviewColorSpace;
        let (w, h) = (48u32, 32u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 23 % 233) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        let dir = crate::tests_xmp::temp_dir("preview-plan-space");
        let path = dir.join("space.arw");
        let path = path.to_string_lossy().into_owned();

        let adobe = tiff_with_preview_strip(&jpeg, 400_000, Fixture { sony: true, color_space: Some(0xFFFF), ..Default::default() });
        assert_eq!(lcc_raw::embedded_preview_color_space(&adobe), Some(PreviewColorSpace::AdobeRgb), "the fixture declares Adobe RGB");
        std::fs::write(&path, &adobe).unwrap();
        let (header, window, read) = preview_window(&path, usize::MAX).expect("the file reads").expect("the plan answers for Adobe RGB too");
        assert!(read.planned && read.adobe_rgb, "{read:?}");
        assert!(read.fraction() < 0.5, "the sensor strip is not read: {read:?}");
        let quick = quick_image_srgb(&header, &window, read.adobe_rgb, 512).expect("the window decodes");
        let whole = embedded_preview_srgb(&adobe, 512).expect("the whole file decodes too");
        assert_eq!(quick.data, whole.data, "the space is applied as the whole-file path applies it");
        let plain = quick_image_srgb(&header, &window, false, 512).expect("the window decodes");
        assert_ne!(plain.data, whole.data, "and it changes the pixels (the fixture's colours are saturated enough)");

        let srgb = tiff_with_preview_strip(&jpeg, 400_000, Fixture { sony: true, color_space: Some(1), ..Default::default() });
        assert_eq!(lcc_raw::embedded_preview_color_space(&srgb), Some(PreviewColorSpace::Srgb), "the fixture declares sRGB");
        std::fs::write(&path, &srgb).unwrap();
        let (_, window, read) = preview_window(&path, usize::MAX).expect("the file reads").expect("sRGB is what the JPEG-only decode assumes");
        assert!(read.planned && !read.adobe_rgb, "the plan still answers");
        assert!(window.starts_with(&[0xff, 0xd8]), "and hands on the JPEG");
    }

    /// A scan's probe is reused only for a file as it was: a file rewritten at the same size has another
    /// modification time, and a file with no known time on either side counts as changed (upstream #3607).
    #[test]
    fn a_probe_is_reused_only_for_a_file_with_the_same_size_and_modification_time() {
        let dir = std::env::temp_dir().join(format!("lcc-probe-valid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jpg");
        std::fs::write(&path, [0u8; 10]).unwrap();
        let path = path.to_string_lossy().to_string();
        let now = file_modified(&path);
        assert!(now.is_some());
        let probed = |size: u64, modified: Option<String>| ProbeInfo { file_size: size, file_modified: modified, ..ProbeInfo::default() };
        assert!(probe_still_valid(&path, &probed(10, now.clone())));
        assert!(!probe_still_valid(&path, &probed(9, now.clone())), "another size");
        assert!(!probe_still_valid(&path, &probed(10, None)), "no time when probed");
        let earlier = std::fs::metadata(&path).unwrap().modified().unwrap() - std::time::Duration::from_secs(3600);
        let earlier_secs = earlier.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(!probe_still_valid(&path, &probed(10, lcc_catalog::dates::local(earlier_secs as i64))), "rewritten since");
        assert!(!probe_still_valid(&dir.join("gone.jpg").to_string_lossy(), &probed(10, now)), "gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A JPEG with no date in its metadata (an export saved without it) takes the file's date, as
    /// in Lightroom, instead of grouping under "Unknown Date"; a dated one keeps its capture time.
    #[test]
    fn undated_files_take_the_file_date() {
        let dir = std::env::temp_dir().join(format!("lcc-undated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rgb = vec![128u8; 16 * 16 * 3];
        let img = EncodeImage::new(16, 16, 3, Samples::U8(&rgb));
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        let path = dir.join("export.jpg");
        std::fs::write(&path, &jpeg).unwrap();
        let path = path.to_string_lossy().to_string();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let want = lcc_catalog::dates::local(mtime as i64).unwrap();
        let (_, probe) = fs_hooks();
        let info = probe(&path).unwrap();
        assert_eq!(info.captured, None, "the file itself has no date");
        assert_eq!(info.file_modified.as_deref(), Some(want.as_str()));
        let mut s = crate::Session::new().with_fs();
        let r = crate::import::import(&mut s, &[path], crate::import::ImportMode::Add).unwrap();
        let id = lcc_catalog::PhotoId(r.imported[0]);
        assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some(want.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue #367: probes read headers, not pixels, and report what the decode-based probe did:
    /// oriented dimensions (EXIF orientation 6 swaps them), metadata, the error for a truncated
    /// file or one that isn't an image.
    #[test]
    fn image_probe_reads_headers_like_the_decode_did() {
        let (w, h) = (300u32, 200u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
        for o in [None, Some(1), Some(6), Some(3), Some(8)] {
            let exif = o.map(lcc_codecs::exif::minimal_exif);
            let meta = EncodeMeta { exif: exif.as_deref(), ..Default::default() };
            let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &meta).unwrap();
            let png = lcc_codecs::encode_png(&img, &meta).unwrap();
            for (name, bytes) in [("a.jpg", &jpeg), ("a.png", &png)] {
                let p = probe_bytes(name, bytes).unwrap();
                // what the probe computed from a decode before
                let d = lcc_codecs::decode(bytes, lcc_codecs::DecodeOptions::fit(64, 64)).unwrap();
                let swap = Orientation::from_exif(d.orientation).swaps_axes();
                let want = if swap { (d.source_height, d.source_width) } else { (d.source_width, d.source_height) };
                assert_eq!((p.width, p.height), want, "{name} {o:?}");
                assert_eq!((p.width, p.height), if matches!(o, Some(6 | 8)) { (h, w) } else { (w, h) }, "{name} {o:?}");
                assert_eq!(p.kind, MediaKind::Image);
                assert_eq!(p.format, if name == "a.jpg" { "JPEG" } else { "PNG" });
                assert_eq!(p.file_size, bytes.len() as u64);
                assert!(p.content_hash.is_some());
                assert!(probe_bytes(name, &bytes[..bytes.len() / 2]).is_err(), "{name}: truncated file accepted");
            }
        }
        assert!(probe_bytes("x.jpg", b"not an image").is_err());
        // video clips are catalogued as videos by extension (their streams are probed by the video backend)
        for name in ["a.mp4", "B.MOV", "c.m2ts", "d.webm", "e.MXF"] {
            let p = probe_bytes(name, b"\0\0\0\x18ftypisom").unwrap();
            assert_eq!(p.kind, MediaKind::Video, "{name}");
            assert!(crate::import::is_supported(std::path::Path::new(name)), "{name}");
        }
    }

    /// Issue #1212: a transparent PNG develops on white (what renders, previews, exports and contact sheets
    /// start from), not on the black its transparent pixels store.
    #[test]
    fn transparent_png_develops_on_white() {
        let rgba = [200, 40, 40, 255, 0, 0, 0, 0];
        let png = lcc_codecs::encode_png(&EncodeImage::new(2, 1, 4, Samples::U8(&rgba)), &EncodeMeta::default()).unwrap();
        let (img, _) = load_bytes(&png, 64).unwrap();
        assert!(img.get(0, 0)[0] > 0.3 && img.get(0, 0)[1] < 0.1, "{:?}", img.get(0, 0));
        assert!(img.get(1, 0).iter().all(|&c| (c - 1.0).abs() < 1e-4), "{:?}", img.get(1, 0));
    }

    /// Issue #548: a small preview bins the mosaic by 8; specular points that clip a single green sample
    /// of a block on a grey surface must not become green speckles (the block's green used to take the
    /// clip level, which highlight reconstruction then kept).
    #[test]
    fn coarse_binning_keeps_specular_points_neutral() {
        let (w, h) = (256usize, 192usize);
        // grey after the file's white balance (as-shot neutral 0.5 : 1 : 0.7), brighter to the right
        let neutral = [0.5f32, 1.0, 0.7];
        let cfa = lcc_raw::Cfa::bayer("RGGB").unwrap();
        let mut data: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let v = (0.3 + 0.2 * x as f32 / w as f32) * neutral[cfa.color_at(x, y).min(2) as usize];
                (256.0 + v * (16000.0 - 256.0)).round() as u16
            })
            .collect();
        // one clipped green sample (RGGB: even row, odd column) in every other 8 × 8 block
        for by in 0..h / 8 {
            for bx in (by % 2..w / 8).step_by(2) {
                data[(by * 8 + 4) * w + bx * 8 + 5] = 16000;
            }
        }
        let bytes = crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, Default::default());
        let raw = lcc_raw::decode(&bytes).unwrap();
        assert_eq!(bin_factor(&raw, 32), Some(8));
        let (img, _) = load_bytes(&bytes, 32).unwrap();
        assert_eq!((img.width, img.height), (32, 24));
        let worst = img.data.iter().map(|p| p[1] / ((p[0] + p[2]) / 2.0).max(1e-6)).fold(0.0f32, f32::max);
        assert!(worst < 1.12, "green speckles: green {worst:.2}× the other channels");
    }

    #[test]
    fn cached_denoise_uses_the_same_white_balance_profile_and_orientation_as_plain_raw() {
        use lcc_denoise::product;
        use lcc_raw::{DngWriteOptions, Method, Orientation, profile::HsvTable, write_dng};
        let dir = crate::tests_xmp::temp_dir("denoise-color-finish");
        let fixture = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lcc_raw::decode(&fixture).unwrap();
        raw.orientation = Orientation::Rotate90;
        raw.color.baseline_exposure = 0.5;
        raw.color.profile.look_table =
            Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[23.0, 0.65, 0.8]; 2], srgb_value: false });
        let bytes = write_dng(&raw, &DngWriteOptions::default()).unwrap();
        let decoded = lcc_raw::decode(&bytes).unwrap();
        assert!(decoded.color.profile.look_table.is_some());
        // A cache containing the ordinary demosaic must undergo the identical downstream finish.
        let camera_rgb = decoded.develop(Method::Ahd).unwrap();
        let key = "synthetic-color-finish";
        let path = dir.join("same.lcdn");
        product::write(&path, &camera_rgb, key).unwrap();
        let spec = DenoiseSpec { product: path, key: key.into(), make: None };
        let (ordinary, _) = load_bytes(&bytes, 2048).unwrap();
        let (plain, cached, _) = load_vec_pair(bytes, 2048, &spec, LoadOptions::default()).unwrap();
        assert_eq!(plain.data, ordinary.data, "requesting a twin must not change the ordinary path");
        let cached = cached.unwrap();
        assert_eq!((plain.width, plain.height), (24, 32));
        assert_eq!((cached.width, cached.height), (plain.width, plain.height));
        let worst = plain.data.iter().zip(&cached.data).flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (a - b).abs())).fold(0.0f32, f32::max);
        assert!(worst < 0.002, "color finish differs beyond cache half-float rounding: {worst}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Luminance (from linear RGB) minus its 9 × 9 local mean: tone differences between a render and a camera JPEG
    /// mostly cancel, edges remain.
    fn detail(w: usize, h: usize, lum: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        let l: Vec<f32> = (0..w * h).map(|i| lum(i % w, i / w)).collect();
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (x0, x1, y0, y1) = (x.saturating_sub(4), (x + 5).min(w), y.saturating_sub(4), (y + 5).min(h));
                let m = (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (xx, yy))).map(|(xx, yy)| l[yy * w + xx]).sum::<f32>();
                out[y * w + x] = l[y * w + x] - m / ((x1 - x0) * (y1 - y0)) as f32;
            }
        }
        out
    }

    /// The integer shift (within ±`r` px) of the `p × p` patch of `a` at `(x, y)` that best matches `b`, with its
    /// normalised cross-correlation, or `None` when the patch has too little detail to tell.
    fn patch_shift(a: &[f32], b: &[f32], w: usize, (x, y): (usize, usize), p: usize, r: i64) -> Option<((i64, i64), f32)> {
        let pa: Vec<f32> = (0..p * p).map(|i| a[(y + i / p) * w + x + i % p]).collect();
        let na = pa.iter().map(|v| v * v).sum::<f32>().sqrt();
        if na < 1e-3 * p as f32 {
            return None;
        }
        let mut best = (f32::MIN, (0, 0));
        for dy in -r..=r {
            for dx in -r..=r {
                let (bx, by) = (x as i64 + dx, y as i64 + dy);
                let (mut dot, mut nb) = (0.0f32, 0.0f32);
                for i in 0..p {
                    for j in 0..p {
                        let v = b[(by as usize + i) * w + bx as usize + j];
                        dot += pa[i * p + j] * v;
                        nb += v * v;
                    }
                }
                let c = dot / (na * nb.sqrt()).max(1e-12);
                if c > best.0 {
                    best = (c, (dx, dy));
                }
            }
        }
        Some((best.1, best.0))
    }

    #[test]
    fn corpus_a7cr_warp_keeps_coordinates_after_orientation_and_resizing() {
        use lcc_geom::{Orientation, Point};
        use lcc_pipeline::optics::Warp;
        let root = lcc_brand::env_os("CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
        for name in ["arw-sony-a7cr-compressed.arw", "arw-sony-a7cr-lossless-l.arw"] {
            let Ok(mut bytes) = std::fs::read(root.join("raw").join(name)) else { continue };
            let off = lcc_raw::probe_info(&bytes).unwrap();
            assert!(embedded_lens(&off).is_none());
            let tiff = lcc_tiff::Tiff::parse(&bytes).unwrap();
            let at = tiff.find(0x7036).unwrap().offset as usize;
            let mut enabled = Vec::new();
            tiff.order.put_u16(&mut enabled, 17);
            bytes[at..at + 2].copy_from_slice(&enabled);
            let mut raw = lcc_raw::probe_info(&bytes).unwrap();
            raw.orientation = Orientation::Normal;
            let mut settings = lcc_develop::DevelopSettings::default();
            settings.optics.lens_profile = true;
            let base_lens = embedded_lens(&raw).unwrap();
            let base = Warp::from_settings(9504.0, 6336.0, &settings, Some(&base_lens));
            for orientation in 1..=8 {
                let o = Orientation::from_exif(orientation);
                raw.orientation = o;
                let lens = embedded_lens(&raw).unwrap();
                for edge in [1200.0, 2560.0, 9504.0] {
                    let scale = edge / 9504.0;
                    let (w, h) = if o.swaps_axes() { (6336.0 * scale, edge) } else { (edge, 6336.0 * scale) };
                    let oriented = Warp::from_settings(w, h, &settings, Some(&lens));
                    for (x, y) in [(0.05, 0.1), (0.5, 0.5), (0.9, 0.8)] {
                        let point = Point::new(x * 9504.0, y * 6336.0);
                        let source = base.corrected_to_source(point, 1);
                        let (px, py) = o.map(point.x, point.y, 9504.0, 6336.0);
                        let (sx, sy) = o.map(source.x, source.y, 9504.0, 6336.0);
                        let got = oriented.corrected_to_source(Point::new(px * scale, py * scale), 1);
                        assert!((got.x - sx * scale).hypot(got.y - sy * scale) < 1e-8, "{name} orientation {orientation} edge {edge}");
                    }
                }
            }
        }
    }

    /// Issue #256: public Panasonic / Leica raws (skipped without the corpus) are corrected for distortion the way the
    /// camera corrected its own JPEG (tag 0x0119): at the corners of the frame, the render with "Enable Profile
    /// Corrections" lines up with the embedded JPEG within 2 px at 640 px, while the uncorrected render of the wide
    /// lenses is far off. A file shot with the camera's correction off (DMC-GH1) carries no correction.
    #[test]
    fn corpus_rw2_distortion_matches_the_camera_jpeg() {
        let dir = lcc_brand::env_os("CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        let Ok(gh1) = std::fs::read(dir.join("rw2-panasonic-gh1.rw2")) else {
            lcc_testkit::skip!(Corpus, "{} absent", dir.display());
            return;
        };
        assert_eq!(probe_bytes("gh1.rw2", &gh1).unwrap().embedded_lens, None, "correction off: nothing to apply");
        assert_eq!(load_bytes(&gh1, 320).unwrap().1.lens, None);

        // (file, how far off (px) the uncorrected corners are at the least, if they match within ±R at all)
        let files = [
            ("rw2-panasonic-fz1000m2-4x3.rw2", 8), // 9.1 mm, ~13 % at the corners
            ("rwl-leica-dlux7.rwl", 8),            // 10.9 mm, ~13 %
            ("rw2-panasonic-gx80.rw2", 8),         // 14 mm, ~8 %
            ("rw2-panasonic-gh6.rw2", 3),          // 20 mm, ~1.6 %
            ("rw2-panasonic-s9.rw2", 3),           // 45 mm, 3 % zoom after the correction
            ("rw2-panasonic-g9.rw2", 0),           // 40 mm, ~0.5 %
        ];
        const EDGE: usize = 640;
        const P: usize = 64;
        const R: i64 = 12;
        for (name, off_at_least) in files {
            let bytes = std::fs::read(dir.join(name)).unwrap();
            let probed = probe_bytes(name, &bytes).unwrap();
            assert!(probed.embedded_lens.is_some_and(|l| l.warp.is_some()), "{name}: no distortion correction read");
            let (src, info) = load_bytes(&bytes, 2 * EDGE).unwrap();
            assert_eq!(info.lens, probed.embedded_lens, "{name}: probe and decode disagree");
            // Convert to DNG keeps it: the same correction, as the DNG's own `WarpRectilinear`
            let dng = lcc_raw::write_dng(&lcc_raw::decode(&bytes).unwrap(), &Default::default()).unwrap();
            assert_eq!(probe_bytes("x.dng", &dng).unwrap().embedded_lens, probed.embedded_lens, "{name}: lost in the DNG");
            let mut s = lcc_develop::DevelopSettings::default();
            let render = |s: &lcc_develop::DevelopSettings| lcc_pipeline::render(&src, &info, s, &lcc_pipeline::RenderRequest::fit(EDGE, EDGE)).image;
            let before = render(&s);
            s.optics.lens_profile = true;
            let after = render(&s);
            let (w, h) = (after.width, after.height);
            assert_eq!((before.width, before.height), (w, h));
            // the camera JPEG shows the whole sensor; a render in an in-camera aspect ratio is its centre
            let (cam, _) = load_embedded_preview(&bytes, 4 * EDGE).unwrap();
            let (cw, ch) = (cam.width as f64, cam.height as f64);
            let (sw, sh) = if cw / ch > w as f64 / h as f64 { (ch * w as f64 / h as f64, ch) } else { (cw, cw * h as f64 / w as f64) };
            let cam = cam.crop(((cw - sw) / 2.0).round() as usize, ((ch - sh) / 2.0).round() as usize, sw.round() as usize, sh.round() as usize);
            let cam = lcc_raster::resample::resize(&cam, w, h, Filter::Mitchell);
            let lin = |v: u8| (v as f32 / 255.0).powf(2.2);
            let lum8 = |im: &lcc_raster::Rgba8, x: usize, y: usize| {
                let p = im.data[y * im.width + x];
                0.3 * lin(p[0]) + 0.6 * lin(p[1]) + 0.1 * lin(p[2])
            };
            let reference = detail(w, h, |x, y| {
                let p = cam.data[y * w + x];
                0.3 * p[0] + 0.6 * p[1] + 0.1 * p[2]
            });
            let (db, da) = (detail(w, h, |x, y| lum8(&before, x, y)), detail(w, h, |x, y| lum8(&after, x, y)));
            let m = R as usize + 4;
            let corners = [(m, m), (w - m - P, m), (m, h - m - P), (w - m - P, h - m - P)];
            let mut measured = 0;
            for c in corners {
                let Some(((dx, dy), ca)) = patch_shift(&da, &reference, w, c, P, R) else { continue };
                measured += 1;
                assert!(dx.abs() <= 2 && dy.abs() <= 2 && ca >= 0.5, "{name}: corrected corner {c:?} off by ({dx}, {dy}) px, ncc {ca}");
                if off_at_least > 0
                    && let Some(((bx, by), cb)) = patch_shift(&db, &reference, w, c, P, R)
                {
                    let off = bx.abs().max(by.abs()) >= off_at_least || cb < 0.3;
                    assert!(off, "{name}: uncorrected corner {c:?} already within ({bx}, {by}) px, ncc {cb}");
                }
            }
            assert!(measured >= 2, "{name}: only {measured} corners with detail");
        }
    }

    /// A CR3-shaped file (not decodable yet) whose only content is a `PRVW` preview box.
    pub(crate) fn cr3_with_preview(w: u32, h: u32) -> Vec<u8> {
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        let jpeg = encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let mut b = b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom".to_vec();
        b.extend_from_slice(&((24 + jpeg.len()) as u32).to_be_bytes());
        b.extend_from_slice(b"PRVW\0\0\0\0\0\x01");
        b.extend_from_slice(&(w as u16).to_be_bytes());
        b.extend_from_slice(&(h as u16).to_be_bytes());
        b.extend_from_slice(b"\0\x01");
        b.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
        b.extend_from_slice(&jpeg);
        b
    }

    /// [`cr3_with_preview`] plus a full-size CRX raw track whose `CMP1` coding header is garbage:
    /// the container parses, the raw data doesn't (`RawError::Corrupt`, not `Unsupported`).
    fn cr3_with_corrupt_raw(w: u32, h: u32) -> Vec<u8> {
        let bx = |kind: &[u8; 4], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&(w as u16).to_be_bytes());
        craw[26..28].copy_from_slice(&(h as u16).to_be_bytes());
        craw.extend(bx(b"CMP1", &[0xff; 4]));
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(bx(b"CRAW", &craw));
        let stsz = [0u32, 16, 1].map(u32::to_be_bytes).concat();
        let co64 = [&[0u8, 0, 0, 0, 0, 0, 0, 1][..], &0u64.to_be_bytes()].concat();
        let stbl = [bx(b"stsd", &stsd), bx(b"stsz", &stsz), bx(b"co64", &co64)].concat();
        let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))));
        let preview = cr3_with_preview(w, h);
        [&preview[..24], &bx(b"moov", &trak), &preview[24..]].concat()
    }

    /// A CR3 whose raw data fails to decode, for any reason, still opens from its embedded JPEG:
    /// CR3s did before the CRX decoder, which is verified on few bodies (PR #279 review).
    #[test]
    fn corrupt_cr3_falls_back_to_embedded_preview() {
        let b = cr3_with_corrupt_raw(48, 32);
        let err = lcc_raw::decode(&b).expect_err("the fixture's raw data is corrupt");
        assert!(!matches!(err, lcc_raw::RawError::Unsupported(_)), "{err}");
        let p = probe_bytes("x.cr3", &b).unwrap();
        assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
        assert!(p.preview_only.is_some_and(|why| why.contains("CR3")));
        let (img, src) = load_bytes(&b, 24).unwrap();
        assert_eq!((img.width, img.height), (24, 16));
        assert!(!src.raw);
        // other formats still report a corrupt file as an error
        let mut dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        dng.truncate(dng.len() / 2);
        assert!(load_bytes(&dng, 24).is_err());
    }

    /// Issue #138: a DNG's own profile look (hue/saturation map, look table, tone curve) is
    /// applied when it's developed — Lightroom-converted DNGs rendered flat and muted without it.
    #[test]
    fn dng_profile_look_is_applied() {
        use lcc_raw::profile::{HsvTable, ProfileLook, ToneCurve};
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lcc_raw::decode(&plain).unwrap();
        let sat = |img: &Rgb32f| {
            img.data.iter().map(|p| (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) / p[0].max(p[1]).max(p[2]).max(1e-6)).sum::<f32>()
                / img.data.len() as f32
        };
        let (before, info) = load_bytes(&plain, 64).unwrap();
        assert!(sat(&before) > 0.05, "the synthetic scene is coloured");
        assert!(info.camera_tone.is_none());
        // a map that removes all saturation, and a tone curve
        let grey = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 0.0, 1.0]; 8], srgb_value: false };
        raw.color.profile = ProfileLook {
            hue_sat_map: [Some(grey), None],
            look_table: None,
            tone_curve: ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.3, 1.0, 1.0]),
            gain_table_map: None,
        };
        let with = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        let (after, info) = load_bytes(&with, 64).unwrap();
        assert!(sat(&after) < 1e-3, "saturation {} → {}", sat(&before), sat(&after));
        // a saturation-only map keeps brightness roughly (HSV value is kept in ProPhoto RGB)
        let mean = |img: &Rgb32f| img.data.iter().map(|p| p[0].max(p[1]).max(p[2])).sum::<f32>() / img.data.len() as f32;
        assert!((0.8..1.1).contains(&(mean(&after) / mean(&before))), "{} vs {}", mean(&after), mean(&before));
        let tone = info.camera_tone.expect("the DNG tone curve becomes the camera tone");
        assert!((tone.apply(0.18) - 0.3).abs() < 0.01, "{}", tone.apply(0.18));
        // a look table alone changes the render too (applied after exposure)
        raw.color.profile = ProfileLook {
            look_table: Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 0.5]; 2], srgb_value: false }),
            ..Default::default()
        };
        let (dim, _) = load_bytes(&lcc_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
        assert!((mean(&dim) / mean(&before) - 0.5).abs() < 0.02, "{} vs {}", mean(&dim), mean(&before));
    }

    /// Corrections used while fitting a DNG's preview must not be baked into its editable source.
    /// The pipeline applies them once from SourceInfo, and a file-defined tone stays authoritative.
    #[test]
    fn dng_preview_corrections_leave_the_render_source_uncorrected() {
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lcc_raw::decode(&plain).unwrap();
        raw.color.profile.tone_curve = lcc_raw::profile::ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.4, 1.0, 1.0]);
        let before = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        raw.opcodes.list3 = vec![lcc_raw::Opcode::FixVignetteRadial { k: [1.0, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] }];
        let corrected = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        let (before, expected) = load_bytes(&before, 64).unwrap();
        let (after, info) = load_bytes(&corrected, 64).unwrap();
        assert_eq!(before.data, after.data, "lens correction belongs to the pipeline");
        assert!(info.lens.as_ref().is_some_and(|l| l.vignette.is_some()));
        assert_eq!(info.camera_tone, expected.camera_tone, "the file's own tone is preserved");
    }

    /// A camera JPEG made from the corrected scene teaches one tone to the DNG's existing fit.
    /// The source pixels stay uncorrected, including when the file is rotated.
    #[test]
    fn dng_lens_corrected_preview_teaches_one_tone_without_baking_in_the_lens() {
        use lcc_color::{REC2020, SRGB, luminance_2020, transfer::linear_to_srgb};
        use lcc_raw::{Opcode, Orientation};
        let (w, h) = (384usize, 256usize);
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let v = 0.02 + 0.2 * (0.5 + 0.5 * (x * 0.045).sin() * (y * 0.06).cos()) + 0.1 * x / w as f32;
                let colour = match (i / w % 2, i % w % 2) {
                    (0, 0) => 1.2,
                    (1, 1) => 0.8,
                    _ => 1.0,
                };
                256 + (v * colour * 15744.0) as u16
            })
            .collect();
        let bytes = crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, Default::default());
        let mut raw = lcc_raw::decode(&bytes).unwrap();
        raw.color = lcc_raw::ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(REC2020.from_xyz()), None],
            as_shot_neutral: Some([1.0; 3]),
            ..Default::default()
        };
        let plain = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        raw.opcodes.list3 = vec![
            Opcode::FixVignetteRadial { k: [0.8, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] },
            Opcode::WarpRectilinear { planes: vec![[0.7, 0.2, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] },
        ];
        let t = lcc_raw::color::camera_transform(&raw, lcc_raw::color::as_shot_white_xy(&raw));
        let corrected = raw.develop(lcc_raw::Method::Bilinear).unwrap();
        let to_srgb = REC2020.to_space(&SRGB);
        let pixels: Vec<u8> = corrected
            .data
            .iter()
            .flat_map(|p| {
                let p = t.matrix.apply_f32(std::array::from_fn(|i| p[i] * t.wb[i]));
                let y = luminance_2020(p).max(1e-6);
                let gain = (1.0 - (-3.0 * y).exp()) / y;
                to_srgb.apply_f32(p.map(|v| v * gain)).map(|v| (255.0 * linear_to_srgb(v).clamp(0.0, 1.0)).round() as u8)
            })
            .collect();
        let image = lcc_codecs::EncodeImage::new(w as u32, h as u32, 3, lcc_codecs::Samples::U8(&pixels));
        let jpeg = lcc_codecs::encode_jpeg(&image, 98, lcc_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
        for orientation in [Orientation::Normal, Orientation::Rotate90] {
            raw.orientation = orientation;
            let mut file = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
            // Append a preview IFD to the little-endian DNG written above.
            let root = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
            let count = u16::from_le_bytes(file[root..root + 2].try_into().unwrap()) as usize;
            let next = root + 2 + 12 * count;
            let start = file.len() as u32;
            file[next..next + 4].copy_from_slice(&start.to_le_bytes());
            let jpeg_at = start + 2 + 4 * 12 + 4;
            file.extend(4u16.to_le_bytes());
            for (tag, value) in [(256u16, w as u32), (257, h as u32), (0x201, jpeg_at), (0x202, jpeg.len() as u32)] {
                file.extend(tag.to_le_bytes());
                file.extend(4u16.to_le_bytes());
                file.extend(1u32.to_le_bytes());
                file.extend(value.to_le_bytes());
            }
            file.extend(0u32.to_le_bytes());
            file.extend(&jpeg);
            let (source, info) = load_bytes(&file, 96).unwrap();
            let tone = info.camera_tone.expect("one DNG look fitted to the lens-corrected preview");
            let target = 1.0 - (-3.0f32 * 0.18).exp();
            assert!((tone.apply(0.18) - target).abs() < 0.04, "one tone: {} vs {target}", tone.apply(0.18));
            let expected = load_bytes(&plain, 96).unwrap().0.into_oriented(orientation);
            assert_eq!(source.data, expected.data, "the editable source is not lens-corrected");
        }
    }

    /// A look fitted to a camera's JPEG, as `camera_preview` makes them: its rows don't sum to one, so the camera's
    /// own white-balanced neutral comes out yellow (blue 12 % low).
    fn yellowing_look() -> lcc_color::Mat3 {
        lcc_color::Mat3([[0.42, 0.548, 0.04], [0.246, 0.728, 0.033], [-0.247, 0.679, 0.447]])
    }

    /// A fitted camera look applied in lanes, on rows that aren't a whole number of them, gives the bits of the
    /// pixel-by-pixel path, awkward pixels (black, negative, huge, NaN, infinite) included.
    #[test]
    fn decoding_keeps_negative_channels_and_zeroes_non_finite_ones() {
        assert_eq!(finite([0.6, 0.05, -0.04]), [0.6, 0.05, -0.04]);
        assert_eq!(finite([f32::NAN, -0.1, f32::INFINITY]), [0.0, -0.1, 0.0]);
    }

    /// Dark sensor noise through a saturating camera matrix (a NIKON D780 profile's) averages to a neutral colour: the
    /// per-channel clamp at zero decoding had kept red's and blue's wider positive halves and turned it magenta.
    #[test]
    fn dark_noise_through_a_saturating_matrix_stays_neutral() {
        let matrix = lcc_color::Mat3([[3.48, -2.41, -0.06], [-0.41, 1.57, -0.16], [0.32, -1.86, 2.58]]);
        let colour = CameraColourModel { wb: [1.0; 3], matrix, gain: 1.0, tables: None, hue_sat: None };
        // a grey 0.002 above black with ±0.004 of noise per channel (a deterministic sequence)
        let mut state = 12345u32;
        let mut noise = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32 * 0.008 - 0.004
        };
        let mut img = Rgb32f::new(200, 200);
        for p in img.data.iter_mut() {
            *p = [0.002 + noise(), 0.002 + noise(), 0.002 + noise()];
        }
        colour.apply(&mut img);
        let mut mean = [0.0f64; 3];
        for p in &img.data {
            for (m, v) in mean.iter_mut().zip(p) {
                *m += f64::from(*v);
            }
        }
        let (hi, lo) = (mean[0].max(mean[1]).max(mean[2]), mean[0].min(mean[1]).min(mean[2]));
        assert!(lo > 0.0 && hi / lo < 1.2, "mean {mean:?} is tinted");
    }

    #[test]
    fn camera_look_in_lanes_matches_pixel_by_pixel() {
        let data = (0..72 * 5 * 5).map(|i| [((i * 37) % 61) as f32 - 30.0, 0.6 + ((i * 11) % 13) as f32 / 13.0, 1.0]).collect();
        let table = lcc_raw::profile::HsvTable { hue_divisions: 72, sat_divisions: 5, val_divisions: 5, data, srgb_value: true };
        let hue_sat = crate::camera_preview::HueSat::new(&table).unwrap();
        let colour = CameraColourModel { wb: [2.1, 1.0, 1.5], matrix: yellowing_look(), gain: 1.3, tables: None, hue_sat: Some(&hue_sat) };
        let special = [0.0, -0.0, -0.01, 1e-30, 0.3, 1.0, 5.0, 1e30, f32::INFINITY, f32::NAN];
        let img = Rgb32f::from_fn(37, 5, |x, y| {
            let k = y * 37 + x;
            if k % 5 == 0 {
                [special[k % 10], special[k / 10 % 10], special[k / 100 % 10]]
            } else {
                [(x as f32 * 0.37).sin().abs(), ((x + y) as f32 * 0.11).cos().abs(), y as f32 / 4.0 - 0.05]
            }
        });
        let mut lanes = img.clone();
        colour.apply(&mut lanes);
        let m = colour.matrix.to_f32();
        for (p, got) in img.data.iter().zip(&lanes.data) {
            assert_eq!(got.map(f32::to_bits), colour.pixel(&m, *p).map(f32::to_bits), "{p:?}");
        }
    }

    #[test]
    fn clipped_white_renders_neutral_through_the_colour_model() {
        use lcc_color::{LUMA_2020, Mat3};
        let look = yellowing_look();
        let white = clipped_white(&look);
        let out = look.apply(white.map(f64::from));
        assert!(out.iter().all(|v| (v - out[1]).abs() < 1e-5), "{white:?} renders {out:?}");
        let luma = |c: [f64; 3]| (0..3).map(|i| c[i] * f64::from(LUMA_2020[i])).sum::<f64>();
        assert!((luma(out) - luma(look.apply([1.0; 3]))).abs() < 1e-5, "as bright as the camera's neutral");
        assert_eq!(clipped_white(&Mat3::IDENTITY), [1.0; 3]);
        // a model that can't be inverted, or only just, keeps the camera's neutral
        assert_eq!(clipped_white(&Mat3([[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
        assert_eq!(clipped_white(&Mat3([[1.0, 1.0, 0.0], [1.0, 1.0 + 1e-8, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
        // so does one whose white would overflow the rebuilt highlights (2.5e38 in red here)
        assert_eq!(clipped_white(&Mat3([[3e-39, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
        // or move the camera's neutral implausibly far
        assert_eq!(clipped_white(&Mat3([[0.1, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
        // what it answers renders neutral through the f32 matrix, and fully clipped highlights stay finite
        for model in [look, Mat3([[1.9, -0.7, -0.2], [-0.3, 1.4, -0.1], [0.0, -0.4, 1.4]])] {
            let w = clipped_white(&model);
            let m = model.to_f32();
            let out: [f32; 3] = std::array::from_fn(|i| m[i][0] * w[0] + m[i][1] * w[1] + m[i][2] * w[2]);
            assert!(out.iter().all(|v| (v - out[1]).abs() <= 1e-3 * out[1]), "{w:?} renders {out:?}");
            let colour = CameraColourModel { wb: [2.5, 1.0, 1.6], matrix: model, gain: 1.0, tables: None, hue_sat: None };
            let mut img = Rgb32f::filled(4, 4, [1.0; 3]);
            colour.rebuild_highlights(&mut img);
            colour.apply(&mut img);
            assert!(img.data.iter().flatten().all(|v| v.is_finite()), "{:?}", img.get(0, 0));
        }
    }

    /// Issue #523: fully clipped highlights are neutral in the white-balanced camera RGB, but a colour model that
    /// doesn't map that neutral to neutral tinted them. The tone map's roll-off to white hid the tint at Neutral;
    /// Highlights -100, or the issue's edit (Exposure -0.88, Highlights -75…), showed it. Highlights ease off towards
    /// white (issue #1371), so the clipped light may stay white; what must hold is that it is neutral.
    #[test]
    fn clipped_highlights_stay_neutral_when_darkened() {
        use lcc_pipeline::{RenderRequest, render};
        let wb = [2.4f32, 1.0, 1.6];
        let colour = CameraColourModel { wb, matrix: yellowing_look(), gain: 1.0, tables: None, hue_sat: None };
        // a grey wall (camera neutral) with a fully clipped light and a light clipped in green only
        let mut img = Rgb32f::from_fn(96, 64, |x, y| {
            let d = |cx: f32| ((x as f32 - cx).powi(2) + (y as f32 - 32.0).powi(2)).sqrt();
            if d(28.0) < 14.0 {
                [1.0; 3]
            } else if d(70.0) < 12.0 {
                [0.6 / wb[0], 1.0, 0.7 / wb[2]]
            } else {
                [0.3 / wb[0], 0.3, 0.3 / wb[2]]
            }
        });
        colour.rebuild_highlights(&mut img);
        colour.apply(&mut img);
        let info = SourceInfo { raw: true, ..Default::default() };
        let mut issue = lcc_develop::DevelopSettings::default();
        let l = &mut issue.light;
        (l.exposure, l.contrast, l.highlights, l.shadows, l.whites, l.blacks) = (-0.88, 5.0, -75.0, 52.0, 6.0, -37.0);
        let mut h100 = lcc_develop::DevelopSettings::default();
        h100.light.highlights = -100.0;
        for (name, s) in [("Highlights -100", h100), ("#523's edit", issue)] {
            let out = render(&img, &info, &s, &RenderRequest::fit(96, 64)).image;
            let p = out.get(28, 32);
            let spread = p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
            assert!(spread <= 1, "{name}: clipped light {p:?} is not neutral");
        }
    }

    /// A smooth pink gradient (`[0.9k, k, 0.8k]` in the sensor, k from 0.8 to 1.35, white balance `[2.5, 1, 1.6]`)
    /// through the point where green clips, 1600 px long, in `rows` of a 1600 × `height` picture that is otherwise
    /// near black, rebuilt and rendered at Exposure -2. Returns the rendered colour of the middle row's last
    /// unclipped pixel and the next two, and the largest step between neighbouring pixels along that row.
    fn darkened_ramp(rows: std::ops::Range<usize>, height: usize) -> ([[u8; 4]; 3], u8) {
        use lcc_pipeline::{RenderRequest, render};
        let wb = [2.5f32, 1.0, 1.6];
        let colour = CameraColourModel { wb, matrix: lcc_color::Mat3::IDENTITY, gain: 1.0, tables: None, hue_sat: None };
        let w = 1600usize;
        let k = |x: usize| 0.8 + 0.55 * x as f32 / (w - 1) as f32;
        let sensor =
            Rgb32f::from_fn(w, height, |x, y| if rows.contains(&y) { [0.9 * k(x), k(x), 0.8 * k(x)].map(|v| v.min(1.0)) } else { [0.01; 3] });
        let y = (rows.start + rows.end) / 2;
        let first = (0..w).find(|&x| sensor.get(x, y)[1] >= HIGHLIGHT_CLIP).unwrap();
        let mut img = sensor.clone();
        colour.rebuild_highlights(&mut img);
        colour.apply(&mut img);
        let mut s = lcc_develop::DevelopSettings::default();
        s.light.exposure = -2.0;
        let out = render(&img, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, height)).image;
        let step = |x: usize| {
            let (a, b) = (out.get(x - 1, y), out.get(x, y));
            (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
        };
        ([out.get(first - 1, y), out.get(first, y), out.get(first + 1, y)], (1..w).map(step).max().unwrap_or(0))
    }

    /// The darkened pink gradient carries on across the point where green clips: before the fix the fade towards
    /// the clipped neutral, driven by how close the other channels were to their clip, made a step of 37 levels
    /// there.
    #[test]
    fn rendered_colour_is_continuous_where_the_first_channel_clips() {
        let ([a, b, c], worst) = darkened_ramp(0..16, 16);
        let step = |p: [u8; 4], q: [u8; 4]| (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
        assert!(step(a, b) <= 1 && step(b, c) <= 1, "at the first clip: {a:?} {b:?} {c:?}");
        assert!(worst <= 3, "largest step between neighbouring pixels: {worst} levels");
    }

    /// The same gradient as a thin bright line on near black: every pixel of it is next to a strong edge, so there
    /// is no reliable colour around it. It still carries on across the first clip, however thin the line and however
    /// much dark background there is (a 4-row line on 32 rows used to jump from [225, 126, 153] to [198, 177, 163];
    /// on 512 rows, from [225, 126, 153] to [218, 145, 151]).
    #[test]
    fn rendered_thin_line_is_continuous_where_the_first_channel_clips() {
        let step = |p: [u8; 4], q: [u8; 4]| (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
        for height in [32usize, 128, 512, 1024] {
            for width in [1usize, 2, 4, 16] {
                let top = (height - width) / 2;
                let ([a, b, c], worst) = darkened_ramp(top..top + width, height);
                let case = format!("{width} rows of {height}");
                assert!(step(a, b) <= 1 && step(b, c) <= 1, "{case}, at the first clip: {a:?} {b:?} {c:?}");
                assert!(worst <= 3, "{case}: largest step between neighbouring pixels {worst} levels");
            }
        }
    }

    /// A band clipped in green whose red rises through its own clip from left to right (sensor `[0.985 + 0.01 t, 1,
    /// 0.8]`, t from 0 to 1 across 320 px), `band` rows at the bottom of the picture, below `strip` rows of a pale
    /// surface just under the clip (`[0.4, 0.95, 0.8]`: the band's rim, of another colour) and above those `top` rows
    /// of a strongly coloured reliable surface (`[0.8, 0.35, 0.3]`). Rebuilt and rendered at Exposure -2: the
    /// rendered colour on either side of where red clips on the band's middle row, and the largest step between
    /// neighbouring pixels along that row.
    fn second_clip(top: usize, strip: usize, band: usize) -> ([[u8; 4]; 2], u8) {
        use lcc_pipeline::{RenderRequest, render};
        let wb = [2.5f32, 1.0, 1.6];
        let colour = CameraColourModel { wb, matrix: lcc_color::Mat3::IDENTITY, gain: 1.0, tables: None, hue_sat: None };
        let (w, h) = (320usize, top + strip + band);
        let sensor = Rgb32f::from_fn(w, h, |x, y| {
            if y < top {
                [0.8, 0.35, 0.3]
            } else if y < top + strip {
                [0.4, 0.95, 0.8]
            } else {
                [0.985 + 0.01 * x as f32 / (w - 1) as f32, 1.0, 0.8]
            }
        });
        let y = top + strip + band / 2;
        let at = (0..w).find(|&x| sensor.get(x, y)[0] >= HIGHLIGHT_CLIP).unwrap();
        let mut img = sensor.clone();
        colour.rebuild_highlights(&mut img);
        colour.apply(&mut img);
        let mut s = lcc_develop::DevelopSettings::default();
        s.light.exposure = -2.0;
        let out = render(&img, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, h)).image;
        let step = |x: usize| {
            let (a, b) = (out.get(x - 1, y), out.get(x, y));
            (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
        };
        ([out.get(at - 1, y), out.get(at, y)], (1..w).map(step).max().unwrap_or(0))
    }

    /// Where a second channel clips within reach of the surface's own rim, the rebuilt colour carries on too. The
    /// rim's comparison with the pixel used to drop each clipped channel, so a rim far from the pixel in red suddenly
    /// matched it once red clipped (80 rows of colour, 16 of rim and 32 of band: [237, 124, 153] then
    /// [205, 181, 168]); and a channel about to clip counted as an uncapped floor that was capped once it clipped
    /// (without the coloured surface: [198, 195, 178] then [205, 181, 168]).
    #[test]
    fn rendered_colour_is_continuous_where_a_second_channel_clips() {
        for (top, strip, band) in [(80, 16, 32), (0, 16, 32), (80, 16, 4), (200, 4, 8), (16, 64, 16), (8, 32, 24)] {
            let ([a, b], worst) = second_clip(top, strip, band);
            let case = format!("{top} rows of colour, {strip} of rim, {band} of band");
            let step = (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
            assert!(step <= 1, "{case}: {a:?} then {b:?} where red clips");
            assert!(worst <= 3, "{case}: largest step between neighbouring pixels {worst} levels");
        }
    }

    /// Issue #523 on a public ILCE-7RM4 raw (raw.pixls.us, CC0; skipped without the corpus): a dusk sky clipped in
    /// green behind a poplar. Its rebuilt colour used to come from the tree's foliage and lens fringes, a green halo
    /// on one side of the tree and a magenta one on the other (Highlights -100 or the issue's edit show them).
    #[test]
    fn corpus_clipped_sky_beside_a_tree_has_no_coloured_halo() {
        let dir = lcc_brand::env_os("CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        let Ok(bytes) = std::fs::read(dir.join("arw-sony-a7rm4-14bit-compressed.arw")) else {
            lcc_testkit::skip!(Corpus, "{} absent", dir.display());
            return;
        };
        let (img, _) = load_bytes(&bytes, 1600).unwrap();
        let (w, h) = (img.width, img.height);
        let lum = |p: [f32; 3]| lcc_color::luminance_2020(p);
        let chroma = |p: [f32; 3]| {
            let s = (p[0] + p[1] + p[2]).max(1e-6);
            [p[0] / s, p[2] / s]
        };
        // the poplar stands at about 30 % of the width, its top 25 to 34 % down the frame
        let px = |f: f32, n: usize| (f * n as f32) as usize;
        let (mut near, mut far) = ([[0f64; 2]; 2], [[0f64; 2]; 2]);
        let mut rows = 0;
        for y in px(0.24, h)..px(0.34, h) {
            let row = &img.data[y * w..(y + 1) * w];
            let sky = lum(row[px(0.2, w)]);
            let dark: Vec<usize> = (px(0.25, w)..px(0.35, w)).filter(|&x| lum(row[x]) < 0.5 * sky).collect();
            let (Some(&left), Some(&right)) = (dark.first(), dark.last()) else { continue };
            let k = w as f32 / 6000.0;
            let mean = |a: usize, b: usize| {
                let n = (b - a) as f64;
                (a..b).map(|x| chroma(row[x])).fold([0f64; 2], |s, c| [s[0] + c[0] as f64 / n, s[1] + c[1] as f64 / n])
            };
            let (n0, n1, f0, f1) = (px(8.0 / 6000.0, w).max(2), px(40.0 / 6000.0, w), (200.0 * k) as usize, (320.0 * k) as usize);
            for (side, (n, f)) in
                [(mean(left - n1, left - n0), mean(left - f1, left - f0)), (mean(right + n0, right + n1), mean(right + f0, right + f1))]
                    .into_iter()
                    .enumerate()
            {
                for c in 0..2 {
                    near[side][c] += n[c];
                    far[side][c] += f[c];
                }
            }
            rows += 1;
        }
        assert!(rows > 50, "the tree was not found ({rows} rows)");
        for side in 0..2 {
            let d = (0..2).map(|c| ((near[side][c] - far[side][c]) / rows as f64).abs()).fold(0.0, f64::max);
            assert!(d < 0.004, "{} of the tree the clipped sky's chromaticity is off by {d:.4}", ["left", "right"][side]);
        }
    }

    /// A Lightroom merge (a LinearRaw DNG keeping a reduced copy of its raw image): its grid thumbnail develops the
    /// copy, but an export never does, whatever its size, and not from the thumbnail's source in memory either. The
    /// copy's samples are much brighter than the image's here, so a picture says which one it came from.
    #[test]
    fn exports_never_develop_a_dngs_reduced_copy() {
        use lcc_tiff::tags::{self as t, photometric};
        use lcc_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
        use serde_json::json;
        let linear = |subfile: u32, w: u32, h: u32, sample: u16| {
            let mut ifd = IfdBuilder::new();
            ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![subfile]));
            ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
            ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16, 16, 16]));
            ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
            ifd.set(t::PHOTOMETRIC, Value::Short(vec![photometric::LINEAR_RAW]));
            ifd.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd.set(t::WHITE_LEVEL, Value::Long(vec![32768]));
            ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![sample.to_le_bytes().repeat((w * h * 3) as usize)] });
            ifd
        };
        // (the copy must cover a thumbnail source's 512 px)
        let (w, h) = (1200u32, 600u32);
        let mut main = linear(0, w, h, 1500);
        main.set(t::DEFAULT_CROP_ORIGIN, Value::Long(vec![4, 2]));
        main.set(t::DEFAULT_CROP_SIZE, Value::Long(vec![w - 8, h - 4]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        ifd0.set(t::DNG_VERSION, Value::Byte(vec![1, 7, 0, 0]));
        ifd0.set(t::MAKE, Value::Ascii("DJI".into()));
        ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]));
        ifd0.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![4]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![2]));
        ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd0.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![vec![128; 24]] });
        ifd0.add_sub_ifd(main);
        ifd0.add_sub_ifd(linear(1, w / 2, h / 2, 24000));
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
        let g = |img: &Rgb32f| img.data.iter().map(|p| p[1] as f64).sum::<f64>() / img.data.len().max(1) as f64;
        let mean = |img: &lcc_raster::Rgba8| img.data.iter().map(|p| p[1] as f64).sum::<f64>() / img.data.len().max(1) as f64;

        // the loaders: only the thumbnail's develops the copy, and only when it covers the size
        let (copy, _, used) = load_vec_reduced(bytes.clone(), 512, LoadOptions::default()).unwrap();
        assert!(used, "a thumbnail develops the copy");
        assert!(!load_vec_reduced(bytes.clone(), 1024, LoadOptions::default()).unwrap().2, "no copy covers 1024 px");
        let (full, _) = load_vec(bytes.clone(), 512).unwrap();
        assert!(g(&copy) > 4.0 * g(&full), "the fixture's copy is much brighter: {} vs {}", g(&copy), g(&full));

        let dir = std::env::temp_dir().join(format!("lc-reduced-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("merge-pano.dng");
        std::fs::write(&path, &bytes).unwrap();
        let mut s = crate::Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let (space, depth) = (lcc_pipeline::OutputSpace::Srgb, lcc_pipeline::OutputDepth::U8);
        // an export at a thumbnail's size, before any thumbnail
        assert_eq!(s.export_job(id, 64, 64, space, depth).unwrap().level, crate::media::SourceLevel::Thumb);
        let export = mean(&s.render_export(id, 64, 64, space, depth).unwrap().image);
        // the thumbnail develops the copy and its source stays in memory…
        s.media.clear_sources();
        let thumb = s.thumb_job(id, 64).unwrap().run();
        assert!(thumb.loaded.as_ref().is_some_and(|l| l.reduced));
        let thumb_mean = mean(&thumb.rendered.as_ref().unwrap().image);
        s.accept(&thumb);
        assert!(thumb_mean > export + 20.0, "the thumbnail shows the copy: {thumb_mean} vs {export}");
        // …and an export after it still reads the full image
        let job = s.export_job(id, 64, 64, space, depth).unwrap();
        assert!(matches!(&job.source, crate::media::SourceRef::File { reduced: None, .. }), "the file, not the copy");
        let again = mean(&s.render_export(id, 64, 64, space, depth).unwrap().image);
        assert!((again - export).abs() < 1.0, "{again} vs {export}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// iPhone ProRAW-style semantic masks drive the Sky mask: Apple's sky matte (here the right
    /// half of the sensor, i.e. the bottom of the photo once turned 90° clockwise — where the sky
    /// heuristic would never look) is used; a matte with an unknown name is ignored.
    #[test]
    fn dng_sky_matte_drives_the_sky_mask() {
        use lcc_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
        use lcc_pipeline::{RenderRequest, render};
        use lcc_tiff::tags::{self as t, photometric};
        use lcc_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
        let (w, h) = (16u32, 8u32);
        let image = |ifd: &mut IfdBuilder, w: u32, h: u32, cpp: u16, bits: u16, data: Vec<u8>| {
            ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
            ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
            ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits; cpp as usize]));
            ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![cpp]));
            ifd.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![data] });
        };
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::LINEAR_RAW]));
        image(&mut raw, w, h, 3, 16, 12000u16.to_le_bytes().repeat((w * h * 3) as usize));
        let matte = |name: &str, right: bool| {
            let mut m = IfdBuilder::new();
            m.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![t::SUBFILE_SEMANTIC_MASK]));
            m.set(t::PHOTOMETRIC, Value::Short(vec![photometric::MASK]));
            m.set(t::SEMANTIC_NAME, Value::Ascii(name.into()));
            image(&mut m, w / 2, h / 2, 1, 8, (0..w / 2 * h / 2).map(|i| if (i % (w / 2) >= w / 4) == right { 255 } else { 0 }).collect());
            m
        };
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        ifd0.set(t::DNG_VERSION, Value::Byte(vec![1, 6, 0, 0]));
        ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
        ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]));
        ifd0.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
        image(&mut ifd0, 4, 2, 3, 8, vec![128; 24]);
        ifd0.add_sub_ifd(raw);
        ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticskymatte", true));
        ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticsomethingelse", false));
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

        let (img, info) = load_bytes(&bytes, 64).unwrap();
        assert_eq!((img.width, img.height), (8, 16));
        assert!(info.mattes.is_some());
        let sky = Mask {
            components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Sky }],
            adjust: LocalAdjustments { exposure: -3.0, ..Default::default() },
            ..Default::default()
        };
        let s = DevelopSettings { masks: vec![sky], ..Default::default() };
        let req = RenderRequest::fit(8, 16);
        let green = |s: &DevelopSettings, info: &SourceInfo, y: usize| render(&img, info, s, &req).image.get(4, y)[1] as i32;
        let plain = DevelopSettings::default();
        assert_eq!(green(&s, &info, 2), green(&plain, &info, 2), "no sky at the top");
        assert!(green(&s, &info, 13) + 40 < green(&plain, &info, 13), "the sky at the bottom is darkened");
        // without the matte the photo's own analysis decides (a flat grey frame has no sky
        // edge to find; it never darkens the bottom like the matte)
        let heuristic = SourceInfo { mattes: None, ..info.clone() };
        assert!(green(&s, &heuristic, 13) > green(&s, &info, 13) + 20);
    }

    /// A DNG's white balance is re-evaluated through its own colour model: the decoder hands the
    /// pipeline the file's colour tags (without the profile look, applied at load) and the white
    /// the pixels were developed for, and a custom white balance renders a neutral differently
    /// from the Bradford adaptation it replaces.
    #[test]
    fn dng_white_balance_uses_the_camera_colour_model() {
        use lcc_pipeline::{RenderRequest, render};
        let dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let raw = lcc_raw::decode(&dng).unwrap();
        let (img, info) = load_bytes(&dng, 64).unwrap();
        let cc = info.camera_color.as_ref().expect("a DNG with a colour matrix");
        assert_eq!(cc.developed_for, lcc_raw::color::as_shot_white_xy(&raw));
        assert!(cc.tags.profile.is_empty());
        assert_eq!(cc.tags.color_matrix, raw.color.color_matrix);
        let mut s = lcc_develop::DevelopSettings::default();
        (s.wb.mode, s.wb.temp, s.wb.tint) = (lcc_develop::WbMode::Custom, 3000.0, 20.0);
        let req = RenderRequest::fit(32, 32);
        let camera = render(&img, &info, &s, &req).image;
        let adapted = render(&img, &SourceInfo { camera_color: None, ..info.clone() }, &s, &req).image;
        assert_ne!(camera.data, adapted.data);
    }

    /// Apple ProRAW: like Lightroom Classic, the default render ignores the file's
    /// `ProfileGainTableMap` (Apple's local tone mapping); a map that would double every pixel
    /// changes nothing.
    #[test]
    fn dng_gain_table_map_is_not_rendered() {
        use lcc_raw::gaintable::GainTableMap;
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lcc_raw::decode(&plain).unwrap();
        let (before, _) = load_bytes(&lcc_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
        raw.color.profile.gain_table_map = Some(GainTableMap {
            points_v: 1,
            points_h: 1,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [0.2, 0.2, 0.2, 0.2, 0.2],
            gamma: 1.0,
            gains: vec![2.0],
        });
        let with_map = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        assert!(lcc_raw::decode(&with_map).unwrap().color.profile.gain_table_map.is_some(), "the map is kept");
        let (after, _) = load_bytes(&with_map, 64).unwrap();
        assert_eq!(before.data, after.data);
    }

    /// The Profile option "Camera local tone mapping" renders the map by position (in the raw's
    /// active area, whatever the photo's orientation), and only when it's on.
    #[test]
    fn dng_gain_table_map_renders_when_asked() {
        use lcc_pipeline::{RenderRequest, render};
        use lcc_raw::gaintable::GainTableMap;
        let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lcc_raw::decode(&plain).unwrap();
        // ×1 at the active area's left edge rising to ×4 at its right edge, whatever the colour
        raw.color.profile.gain_table_map = Some(GainTableMap {
            points_v: 1,
            points_h: 2,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0, 0.0],
            gamma: 1.0,
            gains: vec![1.0, 4.0],
        });
        let with_map = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        let (img, info) = load_bytes(&with_map, 64).unwrap();
        let (_, plain_info) = load_bytes(&plain, 64).unwrap();
        assert!(info.local_tone.is_some() && plain_info.local_tone.is_none());
        let req = RenderRequest::fit(64, 64);
        let mut s = lcc_develop::DevelopSettings::default();
        // (darker, so that ×4 never reaches white)
        s.light.exposure = -3.0;
        let off = render(&img, &info, &s, &req).image;
        assert_eq!(off.data, render(&img, &plain_info, &s, &req).image.data, "off: the map changes nothing");
        s.profile.camera_local_tone = true;
        assert!(s.to_json().to_string().contains("camera_local_tone"), "the option is part of the settings (and their hash)");
        let on = render(&img, &info, &s, &req).image;
        // brightness gained, per column: none at the left edge, most at the right
        let column = |im: &lcc_raster::Rgba8, x: usize| (0..im.height).map(|y| im.data[y * im.width + x][1] as f32).sum::<f32>();
        let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
        let last = on.width - 1;
        assert!((gain(0) - 1.0).abs() < 0.08, "left ×{}", gain(0));
        assert!(gain(last) > 1.4 && gain(last) > gain(last / 2) && gain(last / 2) > gain(0), "{} {} {}", gain(0), gain(last / 2), gain(last));
        // the photo turned upside down: the map follows the raw, so the brightened side swaps
        s.orientation = lcc_geom::Orientation::Rotate180;
        s.profile.camera_local_tone = false;
        let off = render(&img, &info, &s, &req).image;
        s.profile.camera_local_tone = true;
        let on = render(&img, &info, &s, &req).image;
        let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
        assert!(gain(0) > 1.4 && (gain(last) - 1.0).abs() < 0.08, "rotated: {} … {}", gain(0), gain(last));
        // a rendered (non-raw) source never gets it
        let jpeg = SourceInfo { raw: false, ..info.clone() };
        assert!(!lcc_pipeline::local_tone::enabled(&jpeg, &s));
    }

    #[test]
    fn unsupported_raw_falls_back_to_embedded_preview() {
        let b = cr3_with_preview(48, 32);
        let p = probe_bytes("x.cr3", &b).unwrap();
        assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
        let (img, src) = load_bytes(&b, 24).unwrap();
        assert_eq!((img.width, img.height), (24, 16));
        assert!(!src.raw);
        // no preview at all: a clear error, not a panic
        assert!(load_bytes(b"\0\0\0\x18ftypcrx \0\0\0\x01", 24).is_err());
    }

    /// Issue #10: a raw variant we can't decode imports as "preview only" with the decoder's
    /// reason, is rendered as the rendered JPEG it is (not as raw), says so to agents, survives a
    /// library reopen, and Reload / Relink clear it once the file decodes (undoably).
    #[test]
    fn unsupported_raw_imports_as_preview_only_and_survives_reopen() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("lc-preview-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (lib, files) = (dir.join("lib"), dir.join("files"));
        std::fs::create_dir_all(&files).unwrap();
        let path = files.join("DSC_0001.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        // the import review already says so
        let scan = s.execute("library.importPreview", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        assert!(scan["candidates"][0]["previewOnly"].as_str().is_some_and(|w| !w.is_empty()), "{scan}");
        let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let p = s.catalog.photo(id).unwrap().clone();
        let why = p.preview_only.clone().expect("marked preview only");
        assert!(why.to_uppercase().contains("CR3"), "the decoder's reason: {why}");
        // stamped with the version that decided it, so a later version rechecks it (#662)
        assert_eq!(p.preview_checked, lcc_catalog::APP_VERSION);
        assert!(!p.preview_only_stale());
        assert_eq!(p.kind, MediaKind::Raw, "still a raw file (filters, Convert to DNG…)");
        assert!(!p.develops_raw());
        // rendered like the JPEG it is: relative white balance, display tone curve, no raw defaults
        assert_eq!(SourceInfo { native_long: None, identity: None, ..crate::media::source_info(&p) }, SourceInfo::default());
        // (new photos start with Constrain Crop on, which is not an edit)
        assert!(p.develop.is_unedited() && p.develop.geometry.constrain_crop, "{:?}", p.develop);
        assert!(s.render_job(id, 48, 48, false, true).unwrap().run().rendered.is_ok());
        // agents see it
        let q = s.execute("catalog.query", &json!({"filter": {}})).unwrap();
        assert_eq!(q["photos"][0]["previewOnly"], json!(why), "{q}");
        let i = s.execute("photo.inspect", &json!({"id": id.0})).unwrap();
        assert_eq!(i["preview_only"], json!(why), "{i}");
        // a library reopen keeps it
        s.persist().unwrap();
        drop(s);
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
        assert_eq!(s.catalog.photo(id).unwrap().preview_checked, lcc_catalog::APP_VERSION);
        // the file decodes now (here: replaced by a DNG): Reload clears it, undo brings it back
        std::fs::write(&path, crate::tests_xmp::synthetic_dng_with(None, lcc_meta::Metadata::default())).unwrap();
        let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(r["reloaded"], json!([id.0]), "{r}");
        assert!(s.catalog.photo(id).unwrap().develops_raw());
        assert_eq!(s.catalog.photo(id).unwrap().preview_checked, "", "the stamp goes with the flag");
        assert!(crate::media::source_info(s.catalog.photo(id).unwrap()).raw);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
        assert_eq!(s.catalog.photo(id).unwrap().preview_checked, lcc_catalog::APP_VERSION);
        // relinking to a file that decodes clears it too
        let dng = files.join("DSC_0001.dng");
        std::fs::write(&dng, crate::tests_xmp::synthetic_dng_with(None, lcc_meta::Metadata::default())).unwrap();
        s.execute("photo.relink", &json!({"id": id.0, "path": dng.to_string_lossy()})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().preview_only, None);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A raw whose TIFF shell holds only a small RGB image (IFD0) next to a private block, as Exif
    /// declares a picture 25x its size.
    fn tiff_shell(w: u32, h: u32, shell: bool) -> Vec<u8> {
        use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![2]));
        ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        ifd0.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![px] });
        ifd0.set_child(
            t::EXIF_IFD,
            IfdBuilder::new().with(t::PIXEL_X_DIMENSION, Value::Long(vec![w * 25])).with(t::PIXEL_Y_DIMENSION, Value::Long(vec![h * 25])),
        );
        let mut b = TiffWriter::default().write(&[ifd0]).unwrap();
        if shell {
            b.extend(std::iter::repeat_n(0x5au8, (w * h * 625 / 8) as usize + 100));
        }
        b
    }

    /// A TIFF raw whose only image is a small thumbnail used to open as a plain image of that size
    /// ("a 150 MP file renders 296x220"), with nothing saying so. Now it is recognised as a raw we can't
    /// decode: preview only, with the reason, still showing that image.
    #[test]
    fn thumbnail_only_tiff_raw_is_preview_only_not_a_plain_image() {
        let b = tiff_shell(24, 16, true);
        let p = probe_bytes("IMG_0001.IIQ", &b).unwrap();
        assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "IIQ", 24, 16));
        let why = p.preview_only.expect("marked preview only");
        assert!(why.contains("600x400") && why.contains("24x16"), "{why}");
        let (img, src) = load_bytes(&b, 12).unwrap();
        assert_eq!((img.width, img.height, src.raw), (12, 8, false));
        assert!(embedded_preview_srgb(&b, 12).is_some());
        // the same pixels without the private block are a small ordinary image
        let plain = probe_bytes("small.tif", &tiff_shell(24, 16, false)).unwrap();
        assert_eq!((plain.kind, plain.preview_only, plain.width, plain.height), (MediaKind::Image, None, 24, 16));
        assert!(lcc_raw::probe(&tiff_shell(24, 16, false)).is_none());

        // TIFFs preserving camera Make tags (SONY, NIKON, etc.) are ordinary images, not raws (#281)
        for make in ["SONY", "NIKON CORPORATION", "PENTAX", "RICOH", "SAMSUNG"] {
            use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![24]));
            ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![16]));
            ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
            ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
            ifd0.set(t::PHOTOMETRIC, Value::Short(vec![2]));
            ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd0.set(t::MAKE, Value::Ascii(make.into()));
            let px: Vec<u8> = (0..24 * 16).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
            ifd0.set_image(ImageData::Strips { rows_per_strip: 16, strips: vec![px] });
            let bytes = TiffWriter::default().write(&[ifd0]).unwrap();
            let p = probe_bytes("photo.tif", &bytes).unwrap();
            assert_eq!((p.kind, p.preview_only, p.width, p.height), (MediaKind::Image, None, 24, 16), "make: {make}");
            let (img, src) = load_bytes(&bytes, 24).unwrap();
            assert_eq!((img.width, img.height, src.raw), (24, 16, false), "make: {make}");
        }
    }

    /// Recognised raw containers we don't decode (Sigma X3F here; the preview's first byte is
    /// overwritten in the file, as Minolta and Epson files do) import as preview only with the JPEG's size, instead of failing.
    #[test]
    fn undecodable_container_with_a_preview_imports_as_preview_only() {
        let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        let mut jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        jpeg[0] = 0x02;
        let mut f = b"FOVb\x02\0\x02\0".to_vec();
        f.extend(std::iter::repeat_n(0x11u8, 300));
        f.extend_from_slice(&jpeg);
        f.extend(std::iter::repeat_n(0x22u8, 300));
        let p = probe_bytes("A.X3F", &f).unwrap();
        assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "X3F", 40, 30));
        assert!(p.preview_only.is_some_and(|w| w.contains("X3f")));
        let (img, src) = load_bytes(&f, 20).unwrap();
        assert_eq!((img.width, img.height, src.raw), (20, 15, false));
        // without a JPEG: a clear error
        let e = probe_bytes("A.X3F", b"FOVb\x02\0\x02\0 nothing here").unwrap_err();
        assert!(e.contains("without an embedded preview"), "{e}");
    }

    /// Issue #1275: an uncompressed Leaf MOS (IFD0 is the 16-bit mosaic, no Make tag; Leaf's data block `0x8606`
    /// holds the preview) imports as a raw shown from its preview, not as a greyscale image of the mosaic.
    #[test]
    fn uncompressed_leaf_mos_imports_as_a_preview_only_raw() {
        use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        let jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let mos = |leaf_data: bool| {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![16]));
            ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![12]));
            ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
            ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
            ifd0.set(t::PHOTOMETRIC, Value::Short(vec![1]));
            ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
            ifd0.set_image(ImageData::Tiles { tile_width: 16, tile_height: 12, tiles: vec![vec![0x30; 16 * 12 * 2]] });
            if leaf_data {
                ifd0.set(0x8606, Value::Undefined([&b"PKTS\0\0\0\x01"[..], &jpeg].concat()));
            }
            TiffWriter::default().write(&[ifd0]).unwrap()
        };
        let p = probe_bytes("aptus22.mos", &mos(true)).unwrap();
        assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "MOS", 40, 30));
        assert!(p.preview_only.is_some_and(|why| !why.is_empty()));
        let (img, src) = load_bytes(&mos(true), 20).unwrap();
        assert_eq!((img.width, img.height, src.raw), (20, 15, false));
        // the same grey image without Leaf's block is an ordinary 16-bit TIFF
        let plain = probe_bytes("grey.tif", &mos(false)).unwrap();
        assert_eq!((plain.kind, plain.preview_only, plain.width, plain.height), (MediaKind::Image, None, 16, 12));
    }

    /// A GoPro GPR (DNG with VC-5 coded data, no preview anywhere) imports, described from its headers, and
    /// develops from its sensor data.
    #[test]
    fn gopro_vc5_dng_imports_and_loads() {
        use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![64]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![48]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![32803]));
        ifd.set(t::COMPRESSION, Value::Short(vec![9]));
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        ifd.set(t::WHITE_LEVEL, Value::Short(vec![16383]));
        let tile = lcc_raw::testdata::vc5_stream(64, 48, [4000, 8192, 8192, 8192], 0);
        ifd.set_image(ImageData::Tiles { tile_width: 64, tile_height: 48, tiles: vec![tile] });
        let f = TiffWriter::default().write(&[ifd]).unwrap();
        assert!(crate::import::EXTENSIONS.contains(&"gpr"), "folder imports find .GPR files");
        let p = probe_bytes("GOPR0001.GPR", &f).unwrap();
        assert_eq!((p.kind, p.preview_only, p.width, p.height), (MediaKind::Raw, None, 64, 48));
        let (img, src) = load_bytes(&f, 64).unwrap();
        assert_eq!((img.width, img.height, src.raw), (64, 48, true));
    }

    /// Any failure of a recognised raw (not only an unsupported variant) can fall back to its preview;
    /// CR3 keeps its own wording.
    #[test]
    fn decode_failures_of_recognised_raws_may_fall_back_to_the_preview() {
        use lcc_raw::RawError;
        let cr3 = b"\0\0\0\x18ftypcrx \0\0\0\x01";
        assert_eq!(preview_reason(b"x", RawError::Unsupported("x".into())), Ok("x".to_string()));
        assert!(preview_reason(b"x", RawError::Corrupt("bad strip".into())).is_ok_and(|w| w.contains("bad strip")));
        assert!(preview_reason(b"x", RawError::Limit("too big")).is_ok());
        assert!(preview_reason(b"x", RawError::Tiff(lcc_tiff::TiffError::MissingTag(256))).is_ok());
        assert!(preview_reason(b"x", RawError::NotRaw).is_err());
        assert!(preview_reason(cr3, RawError::Corrupt("bad".into())).is_ok_and(|w| w.starts_with("CR3 ")));
    }

    #[test]
    fn quick_jobs_show_embedded_previews_then_cached_renders() {
        use crate::media::QuickSource;
        let dir = std::env::temp_dir().join(format!("lc-quick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let mut s = crate::Session::new().with_fs();
        let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        // loupe: an unedited raw opens on its embedded preview…
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Embedded));
        let img = q.rendered.unwrap().image;
        assert_eq!((img.width, img.height), (96, 64));
        // …and once the loupe has rendered it, on that render (any size)
        let full = s.loupe_job(id, 48, 32, true).unwrap().run();
        assert!(full.rendered.is_ok() && full.quick.is_none());
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Cached));
        assert_eq!(q.rendered.unwrap().image.width, 48);
        // grid: embedded first, then the real thumbnail (final, with the thumbnail job's key)
        let job = s.thumb_job(id, 128).unwrap();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!((q.quick, q.key), (Some(QuickSource::Embedded), job.key));
        drop(job.clone().run());
        assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Cached));
        // an edited raw never shows its camera JPEG (unedited, uncropped), nor a render made as
        // shot: the fast draft with its settings…
        s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.3})).unwrap();
        let job = s.thumb_job(id, 128).unwrap();
        assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Draft));
        assert!(!s.embedded_thumb_is_final(s.catalog.photo(id).unwrap()));
        // …and once an edit of it is rendered, that render, with the settings before the next edit
        drop(job.clone().run());
        let full = s.loupe_job(id, 48, 32, true).unwrap().run();
        assert!(full.rendered.is_ok());
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.6})).unwrap();
        let job = s.thumb_job(id, 128).unwrap();
        assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Last));
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Last));
        assert_eq!(q.rendered.unwrap().image.width, 48, "the loupe's last render");
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 3.0})).unwrap();
        let far = s.thumb_job(id, 128).unwrap();
        assert_eq!(s.quick_thumb_job(&far).unwrap().run().quick, Some(QuickSource::Draft), "a look far from the last render");
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.6})).unwrap();
        // a raw edited before anything of it was rendered: a fast draft with its settings from the
        // camera JPEG, in the grid and the loupe (cropped as edited, never the camera JPEG itself)
        s.media.rendered.clear();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!((q.quick, q.key), (Some(QuickSource::Draft), job.key));
        assert!(q.rendered.is_ok());
        let mut d = (*s.catalog.photo(id).unwrap().develop).clone();
        d.crop.geometry.rect = lcc_geom::Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 1.0 };
        s.set_develop(id, d, "Crop").unwrap();
        let q = s.quick_view_job(id, 1600, true).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Draft));
        let img = q.rendered.unwrap().image;
        assert!(img.width * 2 < img.height * 2 && img.width > 0, "the crop applied: {}x{}", img.width, img.height);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue #1448: the grid renders nothing behind an unedited raw's quick thumbnail, so when the
    /// embedded preview can't be read (and nothing is cached) the quick job itself must render the
    /// raw, or the cell stays empty for good ("no quick preview").
    #[test]
    fn a_quick_thumbnail_without_an_embedded_preview_renders_the_raw() {
        use crate::media::QuickSource;
        let dir = std::env::temp_dir().join(format!("lc-quick-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let mut s = crate::Session::new().with_fs();
        let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        s.media.preview_loader = Some(std::sync::Arc::new(|_: &str, _| None));
        let job = s.thumb_job(id, 128).unwrap();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!((q.quick, q.key), (Some(QuickSource::Small), job.key));
        assert!(q.rendered.is_ok(), "{:?}", q.rendered.err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unedited raw's grid thumbnail is its camera JPEG, never rendered: that JPEG is what the photo keeps showing
    /// once its original is missing and an update has changed the thumbnails' keys; with the file back, the camera
    /// JPEG is read from it again.
    #[test]
    fn a_missing_raw_keeps_its_camera_jpeg_thumbnail_through_an_update() {
        use crate::media::QuickSource;
        let dir = std::env::temp_dir().join(format!("lc-kept-raw-{}", std::process::id()));
        let (src, lib) = (dir.join("card"), dir.join("lib"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&src).unwrap();
        let path = src.join("a.cr3");
        std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
        let id = {
            let mut s = crate::Session::new().with_fs();
            s.open_library(&lib, false).unwrap();
            let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
            let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
            let job = s.thumb_job(id, 128).unwrap();
            assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Embedded));
            let t = std::time::Instant::now();
            while s.media.rendered.queued_bytes() > 0 && t.elapsed() < std::time::Duration::from_secs(30) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            id
        };
        // the card is out, and the next start comes after more updates than earlier versions' thumbnails are
        // looked for under: only the thumbnail the photo kept can show
        std::fs::rename(&src, dir.join("card-out")).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        s.media.thumb_version += crate::media::KEPT_VERSIONS + 1;
        let job = s.thumb_job(id, 128).unwrap();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!((q.quick, q.placement), (Some(QuickSource::Kept), None), "the kept thumbnail, claiming no placement");
        let img = q.rendered.expect("the kept camera JPEG").image;
        assert_eq!((img.width, img.height), (96, 64), "the camera JPEG it showed before");
        // the card is back with another picture in the file: its camera JPEG is read again
        std::fs::rename(dir.join("card-out"), &src).unwrap();
        std::fs::write(&path, cr3_with_preview(80, 40)).unwrap();
        let q = s.quick_thumb_job(&job).unwrap().run();
        assert_eq!(q.quick, Some(QuickSource::Embedded));
        assert_eq!(q.rendered.unwrap().image.width, 80, "read from the file, not the kept thumbnail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Sony ARW of `model`: a 64 × 48 uncompressed 14-bit CFA strip in a SubIFD (as `crates/raw/tests/it/vendor_robust.rs`
    /// lays it out) whose IFD carries the full-frame 16–35 mm distortion table (tag 0x7037).
    fn sony_arw(model: &str) -> Vec<u8> {
        use lcc_tiff::tags::{self as t, photometric};
        use lcc_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
        // 3:2, the aspect Sony's lens opcodes are limited to
        let (w, h) = (72u32, 48u32);
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![14]));
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        raw.set(0x7037, Value::SShort(vec![16, 0, 2, 7, 16, 28, 43, 61, 83, 109, 139, 175, 217, 267, 325, 395, 479]));
        raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![2000u16.to_le_bytes().repeat((w * h) as usize)] });
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("SONY".into()));
        ifd0.set(t::MODEL, Value::Ascii(model.into()));
        ifd0.add_sub_ifd(raw);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    /// Sony's embedded distortion correction is read (a verified body's tables) but starts off, as for every raw
    /// outside the built-in formats: the ARW imports with "Enable Profile Corrections" off and counts as unedited;
    /// another body's tables aren't read at all.
    #[test]
    fn arw_with_lens_data_imports_with_profile_corrections_off() {
        let dir = std::env::temp_dir().join(format!("lc-arw-lens-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = crate::Session::new().with_fs();
        for (model, on) in [("ILCE-7RM6", true), ("ILCE-7M4", false)] {
            let path = dir.join(format!("{model}.arw"));
            std::fs::write(&path, sony_arw(model)).unwrap();
            let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
            let p = s.catalog.photo(lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap())).unwrap();
            assert_eq!((p.format.as_str(), p.embedded_lens.is_some(), p.develop.optics.lens_profile), ("ARW", on, false), "{model}");
            assert!(!p.is_edited(), "{model}: as imported");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RW2 and DNG keep their default: the correction is on when the file carries one (RW2 distortion, a DNG's
    /// opcodes), off when it doesn't.
    #[test]
    fn rw2_and_dng_defaults_unchanged() {
        use lcc_catalog::{Photo, PhotoId, Source};
        let s = crate::Session::new();
        let lens = lcc_develop::EmbeddedLens { warp: Some(Default::default()), ..Default::default() };
        for format in ["RW2", "DNG"] {
            for embedded in [None, Some(lens)] {
                let name = format!("x.{format}");
                let mut p = Photo::new(PhotoId(1), Source::File { path: format!("/{name}") }, &name, format, 64, 48, "");
                p.kind = MediaKind::Raw;
                p.embedded_lens = embedded.map(Box::new);
                crate::import::apply_import_defaults(&s, &mut p);
                assert_eq!(p.develop.optics.lens_profile, embedded.is_some(), "{format} {embedded:?}");
            }
        }
    }

    /// A Canon CR3 whose lens the database matches (or any raw outside the built-in formats with lens data) starts
    /// with "Enable Profile Corrections" off and unedited, so browsing and culling never warp it; its own XMP
    /// with `LensProfileEnable=1` turns the correction on, and with the switch absent leaves it off.
    #[test]
    fn database_matched_raw_starts_with_profile_corrections_off() {
        use lcc_catalog::{Photo, PhotoId, Source};
        let s = crate::Session::new();
        let lens = lcc_develop::EmbeddedLens { warp: Some(Default::default()), ..Default::default() };
        let xmp = |switch: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
                   crs:ProcessVersion="15.4" crs:Exposure2012="+0.50" {switch}/></rdf:RDF></x:xmpmeta>"#
            )
        };
        for format in ["CR3", "CR2", "NEF", "ARW", "ORF", "PEF"] {
            let new = || {
                let name = format!("x.{format}");
                let mut p = Photo::new(PhotoId(1), Source::File { path: format!("/{name}") }, &name, format, 64, 48, "");
                p.kind = MediaKind::Raw;
                p.embedded_lens = Some(Box::new(lens));
                crate::import::apply_import_defaults(&s, &mut p);
                p
            };
            let p = new();
            assert!(!p.develop.optics.lens_profile, "{format}: a new photo starts with the correction off");
            assert!(!p.is_edited() && *p.develop == p.camera_defaults(), "{format}");
            for (switch, on) in [(r#"crs:LensProfileEnable="1""#, true), ("", false), (r#"crs:LensProfileEnable="0""#, false)] {
                let mut p = new();
                let sc = crate::sidecar::parse_sidecar(&xmp(switch), crate::crs::Target::RawAbsolute).unwrap();
                crate::sidecar::merge_into(&mut p, &sc, "now");
                assert_eq!(p.develop.optics.lens_profile, on, "{format} {switch:?}");
            }
        }
    }

    /// A photo catalogued when import still turned every raw's lens correction on (a database match too) and never
    /// edited since counts as unedited; one with a history keeps counting its settings as edits.
    #[test]
    fn a_photo_imported_with_the_old_default_is_still_unedited() {
        use lcc_catalog::{HistoryStep, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::File { path: "/x.CR3".into() }, "x.CR3", "CR3", 64, 48, "");
        p.kind = MediaKind::Raw;
        p.embedded_lens = Some(Box::new(lcc_develop::EmbeddedLens { warp: Some(Default::default()), ..Default::default() }));
        let mut d = p.camera_defaults();
        assert!(!d.optics.lens_profile);
        d.optics.lens_profile = true;
        p.develop = std::sync::Arc::new(d.clone());
        assert!(!p.is_edited(), "as an older build imported it");
        p.history.push(HistoryStep { label: "Lens Corrections".into(), settings: std::sync::Arc::new(d), batch: None });
        assert!(p.is_edited(), "turned on by the user");
    }

    /// Stored settings stay as they are when the library opens again: an ARW catalogued before its lens data was read
    /// (none, correction off) keeps the correction off and counts as unedited, and one whose user switched the
    /// correction off keeps it off.
    #[test]
    fn existing_photo_settings_are_not_rewritten() {
        use lcc_catalog::{Op, PhotoId};
        let dir = std::env::temp_dir().join(format!("lc-arw-lens-existing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (lib, files) = (dir.join("lib"), dir.join("files"));
        std::fs::create_dir_all(&files).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let import = |s: &mut crate::Session, name: &str| {
            let path = files.join(name);
            // trailing bytes of its own: not a duplicate of the other file
            std::fs::write(&path, [sony_arw("ILCE-7RM6"), name.as_bytes().to_vec()].concat()).unwrap();
            let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
            PhotoId(r["imported"][0].as_u64().unwrap())
        };
        let old = import(&mut s, "old.arw");
        // as a release that didn't read the Sony tables catalogued it
        let mut d = (*s.catalog.photo(old).unwrap().develop).clone();
        d.optics.lens_profile = false;
        let ops = vec![
            Op::SetEmbeddedLens { id: old, lens: None },
            Op::SetDevelop { id: old, settings: std::sync::Arc::new(d), label: "x".into(), edited: None },
        ];
        s.commit("x", Op::Batch { ops }).unwrap();
        let off = import(&mut s, "off.arw");
        s.execute("develop.merge", &serde_json::json!({"settings": {"optics": {"lens_profile": false}}})).unwrap();
        let before: Vec<_> = [old, off].iter().map(|id| s.catalog.photo(*id).unwrap().clone()).collect();
        assert!(before.iter().all(|p| !p.develop.optics.lens_profile));
        s.persist().unwrap();
        drop(s);
        let mut s = crate::Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        for b in &before {
            let p = s.catalog.photo(b.id).unwrap();
            assert_eq!((&p.develop, &p.embedded_lens), (&b.develop, &b.embedded_lens), "{}", p.file_name);
        }
        assert!(!s.catalog.photo(old).unwrap().is_edited(), "still as imported");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Upstream #3783: a DNG's `ProfileToneCurve` is the finish stage's camera curve, so process V8
    /// extends it past its last point (the sensor's white, scene 1.0) with the stop roll-off as well. A
    /// curve that reaches white there renders as under V7 up to it and white past it; one that ends
    /// below white (here 0.9) keeps what Exposure lifts past the sensor's white apart instead of
    /// reaching 8-bit 255 within a stop.
    #[test]
    fn v8_extends_a_dng_tone_curve_past_white_in_stops() {
        use lcc_color::transfer::linear_to_srgb;
        use lcc_develop::{DevelopSettings, Process};
        use lcc_pipeline::SourceInfo;
        let code = |curve: &[f64], process: Process, y: f32| {
            let tone = lcc_raw::profile::ToneCurve::from_tag(curve).and_then(|c| super::dng_tone_curve(&c));
            let info = SourceInfo { raw: true, camera_tone: Some(tone.expect("a valid curve")), ..SourceInfo::default() };
            let s = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
            let o = lcc_pipeline::finish::base_tone(&s, &info, 1.0).apply(y);
            (linear_to_srgb(o.clamp(0.0, 1.0)) * 255.0).round()
        };
        let to_white = [0.0, 0.0, 0.03, 0.09, 0.18, 0.4, 0.5, 0.78, 1.0, 1.0];
        for y in [0.01, 0.05, 0.18, 0.5, 0.9] {
            assert_eq!(code(&to_white, Process::V8, y), code(&to_white, Process::V7, y), "{y}");
        }
        for y in [1.0, 2.0, 4.0] {
            assert_eq!((code(&to_white, Process::V7, y), code(&to_white, Process::V8, y)), (255.0, 255.0), "{y}");
        }
        let below_white = [0.0, 0.0, 0.03, 0.09, 0.18, 0.4, 0.5, 0.72, 1.0, 0.9];
        let at = |process, ev: f32| code(&below_white, process, ev.exp2());
        assert_eq!(at(Process::V8, 0.0), at(Process::V7, 0.0));
        assert_eq!(at(Process::V7, 1.0), 255.0, "V7 is frozen");
        let (white, beyond) = (at(Process::V8, 0.0), at(Process::V8, 1.0));
        assert!(beyond < 254.0 && beyond - white >= 3.0, "{white} then {beyond} a stop past the sensor's white");
    }

    /// An ILCE-7CR frame at ISO 50 (the sensor at ISO 100 with a stop more light) develops at half the level of the
    /// same sensor data at ISO 100: the decoder's baseline exposure reaches the render.
    #[test]
    fn a7cr_iso_50_develops_a_stop_darker_than_iso_100() {
        use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let arw = |iso: u16| {
            let (w, h) = (64u32, 48u32);
            let mut raw = IfdBuilder::new();
            raw.set(t::MAKE, Value::Ascii("SONY".into()));
            raw.set(t::MODEL, Value::Ascii("ILCE-7CR".into()));
            raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
            raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
            raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
            raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
            raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
            raw.set(t::COMPRESSION, Value::Short(vec![1]));
            raw.set(t::WHITE_LEVEL, Value::Long(vec![16383]));
            // a mid grey, far from clipping at either exposure
            let samples: Vec<u8> = (0..w * h).flat_map(|_| 3000u16.to_le_bytes()).collect();
            raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![samples] });
            let mut exif = IfdBuilder::new();
            exif.set(t::ISO_SPEED, Value::Short(vec![iso]));
            raw.set_child(t::EXIF_IFD, exif);
            TiffWriter::default().write(&[raw]).expect("the fixture writes")
        };
        let mean = |iso| {
            let (img, _) = load_bytes(&arw(iso), 64).expect("the fixture develops");
            let sum: f64 = img.data.iter().map(|p| p.iter().map(|v| f64::from(*v)).sum::<f64>()).sum();
            sum / (img.data.len() * 3) as f64
        };
        let (at_50, at_100) = (mean(50), mean(100));
        assert!(at_100 > 0.01, "the fixture develops to a visible level: {at_100}");
        let ratio = at_50 / at_100;
        assert!((ratio - 0.5).abs() < 0.01, "ISO 50 / ISO 100 = {ratio}");
    }
}
