//! The makers `craft-raw` decodes (DNG, Hasselblad, CRW, CR2, NEF / NRW, PEF, SRW, MRW, ERF, MEF, Kodak DCS), mapped
//! onto this crate's [`RawImage`]: the colour tags into LightCraft's colour types, and what the format layer leaves
//! to the developer added here:
//!
//! - [`Metadata`] from the file's Exif (a CRW, which has none, from its own records), with the developed size;
//! - DNG and Hasselblad samples stuck below the stated white level raised to it ([`RawImage::lift_clipped_samples`]);
//! - Epson ERF and Mamiya MEF, which record no white balance, get one estimated from their preview
//!   ([`crate::wb_estimate`]); their header probe decodes the samples for it, as before;
//! - Nikon bodies whose files record no white-balance presets get LightCraft's built-in ones
//!   ([`crate::kelvin_builtin`]).

use craft_tiff::{Tiff, tags as t};
use lightcraft_color::Xy;
use lightcraft_meta::{DateTime, Metadata};

use crate::{ColorData, Mat3, Mode, Orientation, RawFormat, RawImage, Rect, Result};

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let format = crate::probe(bytes);
    // the white balance of these two is measured from the samples, so the probe decodes them too
    let estimate_wb = matches!(format, Some(RawFormat::Erf | RawFormat::Mef));
    let mut img = if mode == Mode::Full || estimate_wb {
        let raw = craft_raw::decode(bytes)?;
        let metadata = metadata(bytes, raw.format, &raw.camera, raw.info().developed_size());
        image(raw, metadata)
    } else {
        let info = craft_raw::probe_info(bytes)?;
        let metadata = metadata(bytes, info.format, &info.camera, info.developed_size());
        header_image(info, metadata)
    };
    if estimate_wb && img.wb_multipliers.is_none() {
        img.wb_multipliers = estimated_wb(bytes, &img);
    }
    if mode == Mode::Header {
        img.data = craft_raw::RawData::U16(Vec::new());
    }
    finish(&mut img);
    Ok(img)
}

pub(crate) fn decode_reduced(bytes: &[u8], min_long: usize) -> Result<Option<RawImage>> {
    let Some(raw) = craft_raw::decode_reduced(bytes, min_long, &craft_raw::Limits::default())? else { return Ok(None) };
    // the developed size of the whole picture, which the metadata reports whatever copy the samples come from
    let full = craft_raw::probe_info(bytes)?;
    let metadata = metadata(bytes, full.format, &full.camera, full.developed_size());
    let mut img = image(raw, metadata);
    finish(&mut img);
    Ok(Some(img))
}

/// What LightCraft adds to every decoded file of these makers.
fn finish(img: &mut RawImage) {
    // DNG writers state a white level the sensor may never reach; samples stuck at the real saturation point are
    // raised to the white level so they count as clipped
    if matches!(img.format, RawFormat::Dng | RawFormat::CfaTiff) {
        img.lift_clipped_samples();
    }
    // a CRW's rotation is its ImageInfo record's, not an Exif tag's
    if img.format == RawFormat::Crw {
        img.metadata.orientation = Some(img.orientation);
    }
    if matches!(img.format, RawFormat::Nef | RawFormat::Nrw) && img.color.kelvin_presets.is_none() {
        img.color.kelvin_presets = img.metadata.model.as_deref().and_then(|model| crate::kelvin_builtin::find(img.metadata.make.as_deref(), model));
    }
}

/// The file's Exif as [`Metadata`], with `size` (the developed size) as its width and height. A CRW has no Exif:
/// its make, model and capture time come from its own records.
fn metadata(bytes: &[u8], format: RawFormat, camera: &craft_raw::Camera, size: (usize, usize)) -> Metadata {
    let mut m = if format == RawFormat::Crw {
        Metadata {
            make: camera.make.clone(),
            model: camera.model.clone(),
            capture_time: camera.capture_unix.and_then(datetime_from_unix),
            ..Metadata::default()
        }
    } else {
        // CR3 (ISO BMFF) and RAF (Fuji container) do not start with a TIFF header; lightcraft_meta::extract handles them.
        // An MRW keeps its Exif TIFF in the `TTW` block.
        match format {
            RawFormat::Cr3 | RawFormat::Raf => lightcraft_meta::extract(bytes),
            RawFormat::Mrw => lightcraft_meta::mrw::parse(bytes)
                .and_then(|m| m.block(b"TTW"))
                .and_then(|b| Tiff::parse(b).ok())
                .as_ref()
                .map(lightcraft_meta::from_tiff)
                .unwrap_or_default(),
            // the full Exif reader, not `from_tiff` alone: it also takes what the TIFF's IFDs leave out from the
            // file's own records (a RW2's JpgFromRaw Exif and the ISO in its IFD0, a maker-note lens name)
            _ => lightcraft_meta::try_read_exif_with(bytes, &craft_tiff::ParseOptions { max_ifds: 256, ..Default::default() }).unwrap_or_default(),
        }
    };
    m.width = Some(size.0 as u32);
    m.height = Some(size.1 as u32);
    m
}

/// Civil date-time from a CRW TimeStamp record's Unix seconds (the camera's local clock, no UTC offset; inverse of
/// [`DateTime::unix_seconds`]). Before 1970 or after 2200 is a garbage clock, not a capture time.
fn datetime_from_unix(t: i64) -> Option<DateTime> {
    use chrono::{Datelike, Timelike};
    let c = chrono::DateTime::from_timestamp(t, 0)?.naive_utc();
    let dt = DateTime {
        year: u16::try_from(c.year()).ok()?,
        month: u8::try_from(c.month()).ok()?,
        day: u8::try_from(c.day()).ok()?,
        hour: u8::try_from(c.hour()).ok()?,
        minute: u8::try_from(c.minute()).ok()?,
        second: u8::try_from(c.second()).ok()?,
        millis: 0,
        offset_minutes: None,
        precision: Default::default(),
    };
    ((1970..=2200).contains(&dt.year)).then_some(dt)
}

fn orientation(o: craft_raw::Orientation) -> Orientation {
    Orientation::from_exif(o.to_exif())
}

pub(crate) fn color(c: craft_raw::ColorData) -> ColorData {
    let m = |p: [Option<craft_raw::Matrix3>; 2]| p.map(|m| m.map(Mat3));
    ColorData {
        illuminant: c.illuminant,
        color_matrix: m(c.color_matrix),
        forward_matrix: m(c.forward_matrix),
        camera_calibration: m(c.camera_calibration),
        analog_balance: c.analog_balance,
        as_shot_neutral: c.as_shot_neutral,
        as_shot_white_xy: c.as_shot_white_xy.map(|[x, y]| Xy::new(x, y)),
        baseline_exposure: c.baseline_exposure,
        baseline_sharpness: c.baseline_sharpness,
        profile: c.profile,
        kelvin_presets: c.kelvin_presets.and_then(|k| kelvin_presets(&k)),
    }
}

/// `craft-raw`'s presets as LightCraft's Kelvin scale. A file that records only its neutral multipliers for
/// Illuminant A and D65 (Fujifilm RAF) comes with no points: the points between them are worked out here.
fn kelvin_presets(k: &craft_raw::kelvin::KelvinPresets) -> Option<crate::kelvin_presets::KelvinPresets> {
    match k.neutral_pair() {
        Some([a, d65]) => crate::kelvin_presets::KelvinPresets::from_neutral_pair(a, d65),
        None => crate::kelvin_presets::KelvinPresets::new(k.points().iter().copied()),
    }
}

fn image(r: craft_raw::RawImage, metadata: Metadata) -> RawImage {
    RawImage {
        format: r.format,
        width: r.width,
        height: r.height,
        cpp: r.cpp,
        data: r.data,
        cfa: r.cfa,
        bits: r.bits,
        black: r.black,
        white: r.white,
        active_area: r.active_area,
        crop: r.crop,
        orientation: orientation(r.orientation),
        color: color(r.color),
        wb_multipliers: r.wb_multipliers,
        linearized: r.linearized,
        pixel_aspect: r.pixel_aspect,
        opcodes: r.opcodes,
        metadata,
        lens_metadata: r.lens_metadata,
    }
}

/// An image without samples from a header probe; only [`RawImage::into_info`] looks at it.
fn header_image(i: craft_raw::RawInfo, metadata: Metadata) -> RawImage {
    RawImage {
        format: i.format,
        width: i.width,
        height: i.height,
        cpp: i.cpp,
        data: craft_raw::RawData::U16(Vec::new()),
        cfa: i.cfa,
        bits: i.bits,
        black: Default::default(),
        white: vec![65535.0],
        active_area: i.active_area,
        crop: i.crop,
        orientation: orientation(i.orientation),
        color: color(i.color),
        wb_multipliers: i.wb_multipliers,
        linearized: false,
        pixel_aspect: i.pixel_aspect,
        opcodes: i.opcodes,
        metadata,
        lens_metadata: i.lens_metadata,
    }
}

/// The as-shot white balance of an ERF or MEF, from its preview's neutral pixels (an MEF's uncompressed RGB
/// preview, an ERF's JPEG), else grey world.
fn estimated_wb(bytes: &[u8], img: &RawImage) -> Option<[f32; 3]> {
    let craft_raw::RawData::U16(data) = &img.data else { return None };
    let (a, c) = (img.active_area, img.crop);
    let crop = Rect::new(a.x.checked_add(c.x)?, a.y.checked_add(c.y)?, c.width, c.height);
    let cfa = img.cfa.as_ref()?;
    let frame = crate::wb_estimate::Frame { data, width: img.width, crop, cfa, black: &img.black, white: img.white_at(0) };
    match (img.format == RawFormat::Mef).then(|| rgb_preview(bytes)).flatten() {
        Some((px, pw, ph)) => crate::wb_estimate::as_shot_rgb(px, pw, ph, &frame),
        None => crate::wb_estimate::as_shot(bytes, &frame),
    }
}

/// The largest uncompressed 8-bit RGB sub-IFD of a TIFF (an MEF's preview): its pixels, width and height.
fn rgb_preview(bytes: &[u8]) -> Option<(&[u8], usize, usize)> {
    let tiff = Tiff::parse(bytes).ok()?;
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| {
            let info = i.image().ok()?;
            let ok = i.u16(t::PHOTOMETRIC) == Some(2)
                && i.u16(t::COMPRESSION).unwrap_or(1) == 1
                && i.u16(t::SAMPLES_PER_PIXEL) == Some(3)
                && i.u64s(t::BITS_PER_SAMPLE).is_some_and(|b| b.iter().all(|&v| v == 8));
            let (w, h) = (info.width as usize, info.height as usize);
            let chunks = info.chunks(bytes.len() as u64);
            let [c] = chunks.as_slice() else { return None };
            let px = craft_tiff::image::chunk_bytes(bytes, c)?;
            (ok && px.len() >= w.checked_mul(h)?.checked_mul(3)?).then_some((px, w, h))
        })
        .max_by_key(|(_, w, h)| w * h)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Illuminant A and D65 neutral multipliers of a Fujifilm GFX 100 RAF (its 0xf00c record), green = 1.
    const A: [f32; 3] = [1.1125828, 1.0, 2.7880795];
    const D65: [f32; 3] = [1.9437087, 1.0, 1.6357616];

    #[test]
    fn a_neutral_pair_keeps_its_kelvin_scale() {
        let pair = craft_raw::kelvin::KelvinPresets::from_neutral_pair(A, D65);
        assert!(pair.as_ref().is_some_and(|k| k.points().is_empty()), "craft-raw keeps a pair without points");
        let c = color(craft_raw::ColorData { kelvin_presets: pair, ..Default::default() });
        assert_eq!(c.kelvin_presets, crate::kelvin_presets::KelvinPresets::from_neutral_pair(A, D65));
        assert!(c.kelvin_presets.is_some());
    }

    #[test]
    fn listed_presets_keep_their_points() {
        let points = [(2500.0, [1.0, 1.0, 3.0]), (5000.0, [1.6, 1.0, 2.0]), (7500.0, [2.0, 1.0, 1.5])];
        let c = color(craft_raw::ColorData { kelvin_presets: craft_raw::kelvin::KelvinPresets::new(points), ..Default::default() });
        assert_eq!(c.kelvin_presets, crate::kelvin_presets::KelvinPresets::new(points));
        assert!(c.kelvin_presets.is_some());
    }

    use craft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    /// A RW2 header: Panasonic IFD0 ISO 160 (tag 0x0017), an Exif IFD without ISOSpeedRatings, and optionally a
    /// JpgFromRaw (tag 0x002E) whose APP1 Exif states `preview_iso`.
    fn rw2(standard_iso: Option<u16>, preview_iso: Option<u16>) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(t::EXPOSURE_TIME, Value::Rational(vec![(1, 100)]));
        if let Some(iso) = standard_iso {
            exif.set(t::ISO_SPEED, Value::Short(vec![iso]));
        }
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Panasonic".into()));
        ifd0.set(t::MODEL, Value::Ascii("DMC-GF6".into()));
        ifd0.set(0x0017, Value::Short(vec![160]));
        if let Some(iso) = preview_iso {
            let mut inner_exif = IfdBuilder::new();
            inner_exif.set(t::ISO_SPEED, Value::Short(vec![iso]));
            let mut inner = IfdBuilder::new();
            inner.set_child(t::EXIF_IFD, inner_exif);
            let mut app1 = b"Exif\0\0".to_vec();
            app1.extend(TiffWriter::new(ByteOrder::Little, false).write(&[inner]).unwrap());
            let mut jpg = vec![0xff, 0xd8, 0xff, 0xe1];
            jpg.extend(((app1.len() + 2) as u16).to_be_bytes());
            jpg.extend(app1);
            jpg.extend([0xff, 0xda, 0, 2, 0xff, 0xd9]);
            ifd0.set(0x002e, Value::Undefined(jpg));
        }
        ifd0.set_child(t::EXIF_IFD, exif);
        let mut bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
        bytes[2] = b'U'; // IIU\0, the RW2 signature
        bytes
    }

    /// storytold/lightcraft#3508: a RW2's short Exif IFD has no ISO; the decoder's metadata takes it from the JpgFromRaw
    /// Exif, else the Panasonic IFD0 tag, while an ISO the raw's own Exif states keeps precedence.
    #[test]
    fn rw2_metadata_iso_falls_back_to_the_file_records() {
        let iso = |b: &[u8]| metadata(b, RawFormat::Rw2, &craft_raw::Camera::default(), (8, 8)).iso;
        assert_eq!(iso(&rw2(None, None)), Some(160));
        assert_eq!(iso(&rw2(None, Some(320))), Some(320));
        assert_eq!(iso(&rw2(Some(400), Some(320))), Some(400));
    }
}
