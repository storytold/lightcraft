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
            _ => Tiff::parse_with(bytes, &craft_tiff::ParseOptions { max_ifds: 256, ..Default::default() })
                .ok()
                .as_ref()
                .map(lightcraft_meta::from_tiff)
                .unwrap_or_default(),
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

fn color(c: craft_raw::ColorData) -> ColorData {
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
        kelvin_presets: c.kelvin_presets.and_then(|k| crate::kelvin_presets::KelvinPresets::new(k.points().iter().copied())),
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
