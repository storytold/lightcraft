//! Exif / TIFF tags → [`Metadata`] (CIPA DC-008 Exif 2.32, TIFF 6.0, DNG 1.7 tag semantics).

use crate::{DateTime, Flash, Gps, Metadata, Orientation};
use lightcraft_tiff::{Ifd, ParseOptions, Tiff, TiffError, tags as t};

pub(crate) const MAX_GPS_NESTING: usize = 4;

/// Strip an `Exif\0\0` (or `Exif\0\xff`) prefix as found in JPEG APP1 and some WebP `EXIF` chunks.
pub fn strip_exif_header(b: &[u8]) -> &[u8] {
    if b.len() >= 6 && &b[..4] == b"Exif" && b[4] == 0 { &b[6..] } else { b }
}

/// Read metadata from a TIFF-structured Exif block (a whole TIFF/DNG/raw file, or the payload of a JPEG APP1
/// Exif segment with or without the `Exif\0\0` prefix). Malformed input yields whatever could be read.
pub fn read_exif(bytes: &[u8]) -> Metadata {
    try_read_exif(bytes).unwrap_or_default()
}

/// Like [`read_exif`] but reports a structurally invalid TIFF header / IFD0.
pub fn try_read_exif(bytes: &[u8]) -> Result<Metadata, TiffError> {
    try_read_exif_with(bytes, &ParseOptions::default())
}

/// [`try_read_exif`] with the caller's parser options (its value allocation budget in particular).
/// A decoder working from a sparse buffer — only selected ranges of the logical file are actually
/// loaded, the rest is zeros — must not size its allocations by the logical file length, which
/// unrelated bytes of the file can inflate at no cost to the attacker.
pub fn try_read_exif_with(bytes: &[u8], opts: &ParseOptions) -> Result<Metadata, TiffError> {
    let data = strip_exif_header(bytes);
    let tiff = Tiff::parse_with(data, opts)?;
    let mut m = from_tiff(&tiff);
    // Panasonic (and some Canons) write no LensModel, only the name in their maker note
    if m.lens_model.is_none() {
        m.lens_model = panasonic_maker_note_lens(data, &tiff).or_else(|| canon_maker_note_lens(data, &tiff));
    }
    // the maker note's values are offsets into the stream, which `from_tiff` doesn't have
    m.focus = crate::focus::read(data, &tiff, m.make.as_deref(), m.model.as_deref(), m.orientation);
    Ok(m)
}

/// Whether a parsed EXIF block contains GPS location-bearing tags, even if their values cannot
/// be projected into a valid [`Gps`] position. Used by Original export's location-removal guard.
pub fn exif_embeds_gps_location(bytes: &[u8]) -> bool {
    exif_gps_tag_presence(bytes).unwrap_or(false)
}

/// Whether a source file contains location-bearing EXIF tags or an embedded EXIF block that cannot
/// be parsed. JPEG APP1 packets, raw CR3 CMT4 GPS data, and all PNG/WebP EXIF chunks are inspected;
/// when a container has no separate EXIF block, the whole source is checked as a possible TIFF-structured EXIF stream.
pub fn source_embeds_exif_gps_location(bytes: &[u8]) -> bool {
    let mut budget = crate::container::gps_scan_budget(bytes.len());
    if is_jpeg_xl(bytes) {
        // The codec can decode JPEG XL, but this metadata crate cannot inspect its EXIF/XMP boxes.
        // Do not let an unchanged Original copy bypass location removal.
        return true;
    }
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        let Some(jpeg) = crate::container::raf_embedded_jpeg(bytes) else { return true };
        if crate::container::jpeg_embeds_location_in_preview(jpeg, 0, &mut budget) {
            return true;
        }
    }
    if let Some(has_location) = crate::container::png_webp_embeds_exif_gps_location(bytes, &mut budget) {
        return has_location;
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return crate::container::jpeg_embeds_location_at_depth(bytes, 0, &mut budget);
    }
    if bytes.starts_with(b"\0MRM") {
        let Some((mrw, complete)) = crate::mrw::parse_with_status(bytes) else { return true };
        if !complete {
            return true;
        }
        for (_, block) in mrw.blocks.iter().filter(|(name, _)| name == b"TTW") {
            if exif_gps_tag_presence_with_budget(block, 0, &mut budget) != Some(false) {
                return true;
            }
        }
        return false;
    }
    if let Some(cr3) = crate::cr3::parse_cr3(bytes) {
        if !crate::cr3::source_boxes_are_complete(bytes, cr3.tracks.len()) {
            return true;
        }
        let Some(cmt1_blocks) = crate::cr3::cmt1_blocks(bytes) else { return true };
        for cmt1 in cmt1_blocks {
            if exif_gps_tag_presence_with_budget(cmt1, 0, &mut budget) != Some(false) {
                return true;
            }
        }
        let Some(cmt2_blocks) = crate::cr3::cmt2_blocks(bytes) else { return true };
        for cmt2 in cmt2_blocks {
            if exif_gps_tag_presence_with_budget(cmt2, 0, &mut budget) != Some(false) {
                return true;
            }
        }
        let Some(cmt4s) = crate::cr3::cmt4_blocks(bytes) else { return true };
        if cmt4s.into_iter().any(|cmt4| cr3_cmt4_embeds_gps_location_with_budget(cmt4, &mut budget) != Some(false)) {
            return true;
        }
        let Some(thumbnails) = crate::cr3::thumbnail_blocks(bytes) else { return true };
        for thumbnail in thumbnails {
            let Some(jpeg) = crate::cr3::thumbnail_jpeg(thumbnail) else { return true };
            if crate::container::jpeg_embeds_location_in_preview(jpeg, 1, &mut budget) {
                return true;
            }
        }
        for track in &cr3.tracks {
            if track.kind != crate::cr3::Cr3TrackKind::Jpeg {
                continue;
            }
            let Some((at, len)) = track.data else { return true };
            let Some(end) = at.checked_add(len) else { return true };
            let Some(jpeg) = bytes.get(at..end) else { return true };
            if !jpeg.starts_with(&[0xff, 0xd8]) {
                return true;
            }
            if crate::container::jpeg_embeds_location_in_preview(jpeg, 1, &mut budget) {
                return true;
            }
        }
        let Some(xmp_packets) = crate::cr3::xmp_packets(bytes) else { return true };
        for packet in xmp_packets {
            if !crate::container::consume_gps_scan_work(&mut budget, packet.len()) || crate::container::xmp_packet_has_location(packet) != Some(false)
            {
                return true;
            }
        }
        let Some(timed_exif) = cr3.timed_exif_with_status(bytes) else { return true };
        for part in timed_exif.into_iter().filter(|part| part.tag == 0x8769) {
            if exif_gps_tag_presence_with_budget(part.data, 1, &mut budget) != Some(false) {
                return true;
            }
        }
        return false;
    }
    if crate::heif::is_heif(bytes) {
        let Some((exifs, xmp_packets, jpeg_items)) = crate::heif::privacy_items(bytes, &mut budget) else { return true };
        for exif in exifs {
            if exif_gps_tag_presence_with_budget(&exif, 0, &mut budget) != Some(false) {
                return true;
            }
        }
        for packet in xmp_packets {
            if !crate::container::consume_gps_scan_work(&mut budget, packet.len())
                || crate::container::xmp_packet_has_location(&packet) != Some(false)
            {
                return true;
            }
        }
        for jpeg in jpeg_items {
            if !jpeg.starts_with(&[0xff, 0xd8])
                || !crate::container::consume_gps_scan_work(&mut budget, jpeg.len())
                || crate::container::jpeg_embeds_location_in_preview(&jpeg, 1, &mut budget)
            {
                return true;
            }
        }
        return false;
    }
    if bytes.starts_with(b"8BPS") {
        return crate::psd::embeds_location(bytes, &mut budget) != Some(false);
    }
    match crate::container::embedded(bytes).exif.as_deref() {
        Some(exif) => exif_gps_tag_presence_with_budget(exif, 0, &mut budget) != Some(false),
        None => match exif_gps_tag_presence_with_budget(bytes, 0, &mut budget) {
            Some(found) => found,
            None => lightcraft_tiff::Tiff::sniff(strip_exif_header(bytes)).is_some(),
        },
    }
}

fn is_jpeg_xl(bytes: &[u8]) -> bool {
    const JXL_SIGNATURE_BOX: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";
    if bytes.starts_with(&[0xff, 0x0a]) || bytes.starts_with(JXL_SIGNATURE_BOX) {
        return true;
    }
    if bytes.len() < 12 || bytes.get(4..8) != Some(b"ftyp") {
        return false;
    }
    let Some(size_bytes) = bytes.get(..4).and_then(|value| <[u8; 4]>::try_from(value).ok()) else { return false };
    let size = u32::from_be_bytes(size_bytes) as usize;
    let end = size.clamp(12, bytes.len().min(4096));
    if bytes.get(8..12) == Some(b"jxl ") {
        return true;
    }
    let mut at = 16usize;
    while let Some(next) = at.checked_add(4).filter(|next| *next <= end) {
        if bytes.get(at..next) == Some(b"jxl ") {
            return true;
        }
        at = next;
    }
    false
}

/// CMT4 is already the GPS IFD, unlike a normal EXIF block which points to a GPS IFD.
/// Treat an unreadable CMT4 as location-bearing because an Original copy preserves its bytes.
fn cr3_cmt4_embeds_gps_location_with_budget(bytes: &[u8], budget: &mut usize) -> Option<bool> {
    let Some(remaining) = budget.checked_sub(bytes.len()) else {
        *budget = 0;
        return None;
    };
    *budget = remaining;
    let (tiff, complete) = Tiff::parse_with_status(strip_exif_header(bytes), &ParseOptions::default()).ok()?;
    let all_ifds = tiff.all_ifds();
    let has_location = tiff.ifds.iter().any(|ifd| ifd.contains(t::GPS_IFD) || gps_coordinate_tags_present(ifd));
    if has_location {
        return Some(true);
    }
    for ifd in all_ifds {
        if ifd.contains(t::XMP) {
            let packet = ifd.bytes(t::XMP)?;
            if !crate::container::consume_gps_scan_work(budget, packet.len()) || crate::container::xmp_packet_has_location(packet) != Some(false) {
                return Some(true);
            }
        }
        if ifd.contains(t::IPTC_NAA) && ifd_iptc_contains_place_name(ifd, tiff.order, budget) != Some(false) {
            return Some(true);
        }
    }
    complete.then_some(false)
}

fn gps_coordinate_tags_present(ifd: &Ifd) -> bool {
    [t::GPS_LATITUDE, t::GPS_LONGITUDE, t::GPS_ALTITUDE, t::GPS_DEST_LATITUDE, t::GPS_DEST_LONGITUDE].into_iter().any(|tag| ifd.contains(tag))
}

fn gps_subifd_location_present(ifd: &Ifd) -> bool {
    if ifd.gps.as_deref().is_some_and(gps_coordinate_tags_present) {
        return true;
    }
    if ifd.sub_ifds.iter().any(gps_subifd_location_present) {
        return true;
    }
    if ifd.exif.as_deref().is_some_and(gps_subifd_location_present) || ifd.interop.as_deref().is_some_and(gps_subifd_location_present) {
        return true;
    }
    false
}

/// Scan the IPTC tag's supported byte representation, including the widespread TIFF-incorrect LONG form.
fn ifd_iptc_contains_place_name(ifd: &Ifd, order: lightcraft_tiff::ByteOrder, budget: &mut usize) -> Option<bool> {
    let value = ifd.value(t::IPTC_NAA)?;
    if let lightcraft_tiff::Value::Long(words) = value {
        let byte_len = words.len().checked_mul(4)?;
        if !crate::container::consume_gps_scan_work(budget, byte_len) {
            return None;
        }
        let mut bytes = Vec::with_capacity(byte_len);
        for word in words {
            bytes.extend_from_slice(&match order {
                lightcraft_tiff::ByteOrder::Little => word.to_le_bytes(),
                lightcraft_tiff::ByteOrder::Big => word.to_be_bytes(),
            });
        }
        let end = bytes.iter().rposition(|byte| *byte != 0).map_or(0, |index| index.saturating_add(1));
        let bytes = bytes.get(..end)?;
        return crate::iptc::contains_place_name(bytes);
    }
    let iim = ifd.bytes(t::IPTC_NAA)?;
    if !crate::container::consume_gps_scan_work(budget, iim.len()) {
        return None;
    }
    crate::iptc::contains_place_name(iim)
}

/// Return tag presence separately from parse failure so Original-copy privacy checks can fail closed.
pub(crate) fn exif_gps_tag_presence(bytes: &[u8]) -> Option<bool> {
    let mut budget = crate::container::gps_scan_budget(bytes.len());
    exif_gps_tag_presence_with_budget(bytes, 0, &mut budget)
}

/// Inspect EXIF and bounded embedded JPEG thumbnails for location-bearing tags.
pub(crate) fn exif_gps_tag_presence_with_budget(bytes: &[u8], depth: usize, budget: &mut usize) -> Option<bool> {
    if depth >= MAX_GPS_NESTING {
        return None;
    }
    let data = strip_exif_header(bytes);
    let Some(remaining) = budget.checked_sub(data.len()) else {
        *budget = 0;
        return None;
    };
    *budget = remaining;
    let (tiff, complete) = Tiff::parse_with_status(data, &ParseOptions::default()).ok()?;
    let all_ifds = tiff.all_ifds();
    if all_ifds.iter().any(|ifd| ifd.contains(t::GPS_IFD)) || tiff.ifds.iter().any(gps_subifd_location_present) {
        return Some(true);
    }
    for ifd in &all_ifds {
        if ifd.contains(t::XMP) {
            let packet = ifd.bytes(t::XMP)?;
            if !crate::container::consume_gps_scan_work(budget, packet.len()) || crate::container::xmp_packet_has_location(packet) != Some(false) {
                return Some(true);
            }
        }
        if ifd.contains(t::IPTC_NAA) && ifd_iptc_contains_place_name(ifd, tiff.order, budget) != Some(false) {
            return Some(true);
        }
        if ifd.contains(t::PHOTOSHOP_IMAGE_RESOURCES) {
            let resources = ifd.bytes(t::PHOTOSHOP_IMAGE_RESOURCES)?;
            if !crate::container::consume_gps_scan_work(budget, resources.len())
                || crate::psd::resources_embeds_location(resources, depth, budget) != Some(false)
            {
                return Some(true);
            }
        }
    }
    if !complete {
        return None;
    }
    for ifd in tiff.all_ifds() {
        if ifd.contains(PANASONIC_JPG_FROM_RAW) {
            let jpg = ifd.bytes(PANASONIC_JPG_FROM_RAW)?;
            if !jpg.starts_with(&[0xff, 0xd8]) {
                return None;
            }
            if crate::container::jpeg_embeds_location_in_preview(jpg, depth.saturating_add(1), budget) {
                return Some(true);
            }
        }
        let has_offset = ifd.contains(t::JPEG_INTERCHANGE_FORMAT);
        let has_length = ifd.contains(t::JPEG_INTERCHANGE_FORMAT_LENGTH);
        if !has_offset && !has_length {
            continue;
        }
        if !has_offset || !has_length {
            return None;
        }
        let offset = usize::try_from(ifd.u64(t::JPEG_INTERCHANGE_FORMAT)?).ok()?;
        let length = usize::try_from(ifd.u64(t::JPEG_INTERCHANGE_FORMAT_LENGTH)?).ok()?;
        if offset == 0 || length == 0 {
            return None;
        }
        let end = offset.checked_add(length)?;
        let thumbnail = data.get(offset..end)?;
        if !thumbnail.starts_with(&[0xff, 0xd8]) {
            return None;
        }
        if crate::container::jpeg_embeds_location_in_preview(thumbnail, depth.saturating_add(1), budget) {
            return Some(true);
        }
    }
    Some(false)
}

fn text(ifd: Option<&Ifd>, tag: u16) -> Option<String> {
    let s = ifd?.string(tag)?;
    let s = s.trim().to_string();
    (!s.is_empty() && !s.bytes().all(|b| b == b' ' || b == 0)).then_some(s)
}

fn pos(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x > 0.0)
}

/// The orientation the Exif's own first IFD states. [`from_tiff`] also reports one that only the XMP packet in
/// the Exif's tag 700 states; LightCraft displays a photo by the Exif's alone, so that one can't tell how a photo
/// is stored.
pub(crate) fn ifd0_orientation(bytes: &[u8]) -> Option<Orientation> {
    let tiff = Tiff::parse_with(strip_exif_header(bytes), &ParseOptions::default()).ok()?;
    tiff.ifds.first().and_then(|i| i.u16(t::ORIENTATION)).filter(|v| (1..=8).contains(v)).map(Orientation::from_exif)
}

/// Build [`Metadata`] from a parsed TIFF stream (IFD0 + Exif + GPS; DNG tags as fallbacks).
pub fn from_tiff(tiff: &Tiff) -> Metadata {
    let ifd0 = tiff.ifds.first();
    let exif = tiff.exif();
    let gps = tiff.gps();
    let either = |tag: u16| exif.and_then(|e| e.value(tag)).or_else(|| ifd0.and_then(|i| i.value(tag)));
    let mut m = Metadata {
        make: text(ifd0, t::MAKE),
        model: text(ifd0, t::MODEL).or_else(|| text(ifd0, t::UNIQUE_CAMERA_MODEL)),
        software: text(ifd0, t::SOFTWARE),
        artist: text(ifd0, t::ARTIST),
        copyright: text(ifd0, t::COPYRIGHT),
        caption: text(ifd0, t::IMAGE_DESCRIPTION),
        serial_number: text(exif, t::BODY_SERIAL_NUMBER).or_else(|| text(ifd0, t::CAMERA_SERIAL_NUMBER)),
        lens_make: text(exif, t::LENS_MAKE),
        lens_model: text(exif, t::LENS_MODEL),
        lens_serial_number: text(exif, t::LENS_SERIAL_NUMBER),
        ..Default::default()
    };
    // Exif requires LensSpecification/LensInfo as 4 rationals.
    let spec = exif.and_then(|e| e.f64s(t::LENS_SPECIFICATION)).or_else(|| ifd0.and_then(|i| i.f64s(t::LENS_INFO)));
    if let Some(s) = spec.filter(|s| s.len() == 4 && s[0] > 0.0) {
        m.lens_spec = Some([s[0], s[1], s[2], s[3]]);
    }
    // Nikon DSLRs write neither LensModel nor LensSpecification, only the same four values in their maker note
    if m.lens_spec.is_none() {
        m.lens_spec = nikon_lens_spec(exif, m.make.as_deref(), tiff.order);
    }
    m.orientation = ifd0.and_then(|i| i.u16(t::ORIENTATION)).filter(|v| (1..=8).contains(v)).map(Orientation::from_exif);
    m.rating = ifd0.and_then(|i| i.i64(t::RATING)).map(|r| r.clamp(-1, 5) as i8);

    m.exposure_time = pos(either(t::EXPOSURE_TIME).and_then(|v| v.get_f64(0)))
        .or_else(|| exif.and_then(|e| e.f64(t::SHUTTER_SPEED_VALUE)).filter(|v| v.abs() < 64.0).map(|tv| 2f64.powf(-tv)));
    m.f_number = pos(either(t::F_NUMBER).and_then(|v| v.get_f64(0)))
        .or_else(|| exif.and_then(|e| e.f64(t::APERTURE_VALUE)).filter(|v| v.abs() < 64.0).map(|av| 2f64.powf(av / 2.0)));
    let iso = either(t::ISO_SPEED).and_then(|v| v.get_u64(0)).filter(|&v| v > 0);
    let iso_ext = exif.and_then(|e| e.u64(t::RECOMMENDED_EXPOSURE_INDEX).or_else(|| e.u64(t::ISO_SPEED_RATINGS_EXT))).filter(|&v| v > 0);
    m.iso = match (iso, iso_ext) {
        (Some(65535), Some(x)) | (None, Some(x)) => Some(x),
        (Some(v), _) => Some(v),
        _ => None,
    }
    .map(|v| v.min(u32::MAX as u64) as u32);
    m.focal_length = pos(either(t::FOCAL_LENGTH).and_then(|v| v.get_f64(0)));
    m.focal_length_35mm = pos(exif.and_then(|e| e.f64(t::FOCAL_LENGTH_35MM)));
    m.exposure_bias = exif.and_then(|e| e.f64(t::EXPOSURE_BIAS)).filter(|v| v.is_finite() && v.abs() < 100.0);
    m.exposure_program = exif.and_then(|e| e.u16(t::EXPOSURE_PROGRAM));
    m.metering_mode = exif.and_then(|e| e.u16(t::METERING_MODE));
    m.white_balance = exif.and_then(|e| e.u16(t::WHITE_BALANCE));
    m.flash = exif.and_then(|e| e.u16(t::FLASH)).map(|raw| Flash { fired: raw & 1 != 0, raw });

    // capture time: DateTimeOriginal, then DateTimeDigitized, then IFD0 DateTime
    let candidates = [
        (exif, t::DATE_TIME_ORIGINAL, t::OFFSET_TIME_ORIGINAL, t::SUBSEC_TIME_ORIGINAL),
        (exif, t::DATE_TIME_DIGITIZED, t::OFFSET_TIME_DIGITIZED, t::SUBSEC_TIME_DIGITIZED),
        (ifd0, t::DATE_TIME, t::OFFSET_TIME, t::SUBSEC_TIME),
    ];
    for (ifd, dt_tag, off_tag, sub_tag) in candidates {
        if let Some(mut dt) = text(ifd, dt_tag).and_then(|s| DateTime::parse_exif(&s)) {
            if let Some(s) = text(exif, sub_tag) {
                dt = dt.with_subsec(&s);
            }
            dt.offset_minutes = text(exif, off_tag).and_then(|s| DateTime::parse_offset(&s));
            m.capture_time = Some(dt);
            break;
        }
    }

    m.width = exif.and_then(|e| e.u32(t::PIXEL_X_DIMENSION)).filter(|&v| v > 0).or_else(|| ifd0.and_then(|i| i.u32(t::IMAGE_WIDTH)));
    m.height = exif.and_then(|e| e.u32(t::PIXEL_Y_DIMENSION)).filter(|&v| v > 0).or_else(|| ifd0.and_then(|i| i.u32(t::IMAGE_LENGTH)));

    m.gps = gps.and_then(read_gps);
    if let Some(iptc) = ifd0.and_then(|i| i.value(t::IPTC_NAA)) {
        // IPTC-NAA is often typed LONG; re-serialise to bytes in file order.
        let bytes = match iptc {
            lightcraft_tiff::Value::Byte(b) | lightcraft_tiff::Value::Undefined(b) => b.clone(),
            other => lightcraft_tiff::writer::encode(tiff.order, other),
        };
        m.fill_missing(&crate::parse_iptc(&bytes));
    }
    if let Some(x) = ifd0.and_then(|i| i.bytes(t::XMP)).map(String::from_utf8_lossy)
        && let Ok(x) = crate::parse_xmp(&x)
    {
        // (the size stays the one Exif states: `extract` turns regions by it, and a size only the XMP states may be the upright one)
        let size = (m.width, m.height);
        m.overlay_user_fields(&x.metadata);
        m.fill_missing(&x.metadata);
        (m.width, m.height) = size;
    }
    m
}

/// Degrees/minutes/seconds triplet as decimal degrees. A component that is present but not a number (a rational
/// with a zero denominator, as in the placeholder GPS block phones write without a fix) makes the whole value unusable.
fn dms(ifd: &Ifd, tag: u16) -> Option<f64> {
    let v = ifd.value(tag)?;
    let part = |i: usize| if i < v.count() { v.get_f64(i) } else { Some(0.0) };
    let d = v.get_f64(0)?;
    let m = part(1)?;
    let s = part(2)?;
    if !d.is_finite() || d < 0.0 || !m.is_finite() || !(0.0..60.0).contains(&m) || !s.is_finite() || !(0.0..60.0).contains(&s) {
        return None;
    }
    let deg = d + m / 60.0 + s / 3600.0;
    deg.is_finite().then_some(deg)
}

/// `Some(true)` for the negative hemisphere (`neg`), `Some(false)` for `pos`, `None` when the ref is missing or invalid.
fn hemisphere(g: &Ifd, tag: u16, pos: char, neg: char) -> Option<bool> {
    match g.string(tag)?.trim_start().chars().next()?.to_ascii_uppercase() {
        c if c == neg => Some(true),
        c if c == pos => Some(false),
        _ => None,
    }
}

fn read_gps(g: &Ifd) -> Option<Gps> {
    let mut lat = dms(g, t::GPS_LATITUDE)?;
    let mut lon = dms(g, t::GPS_LONGITUDE)?;
    if hemisphere(g, t::GPS_LATITUDE_REF, 'N', 'S')? {
        lat = -lat;
    }
    if hemisphere(g, t::GPS_LONGITUDE_REF, 'E', 'W')? {
        lon = -lon;
    }
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let altitude = g.f64(t::GPS_ALTITUDE).filter(|a| a.is_finite()).map(|a| if g.u32(t::GPS_ALTITUDE_REF) == Some(1) { -a } else { a });
    Some(Gps { latitude: lat, longitude: lon, altitude })
}

/// Panasonic maker note tag `0x0051` (LensType in the ExifTool Panasonic tag names): the lens name, ASCII.
const PANASONIC_LENS_TYPE: u16 = 0x0051;
/// RW2/RWL IFD0 tag `0x002E` (JpgFromRaw): a JPEG whose APP1 holds the full Exif, maker note included.
const PANASONIC_JPG_FROM_RAW: u16 = 0x002E;

/// The lens name in the Panasonic maker note (tag `0x0051`) of this Exif or RW2 stream (`data` starts at its
/// TIFF header): the note of its own Exif IFD, else, for a raw, the note in its JpgFromRaw's Exif.
pub(crate) fn panasonic_maker_note_lens(data: &[u8], tiff: &Tiff) -> Option<String> {
    panasonic_lens(data, tiff).or_else(|| panasonic_jpg_from_raw_lens(tiff))
}

/// The lens name in a `Panasonic\0` maker note of this Exif stream (`data` starts at its TIFF header).
fn panasonic_lens(data: &[u8], tiff: &Tiff) -> Option<String> {
    let exif = tiff.exif()?;
    if !exif.bytes(t::MAKER_NOTE)?.starts_with(b"Panasonic\0") {
        return None;
    }
    let e = exif.get(t::MAKER_NOTE)?;
    let make = tiff.ifds.first().and_then(|i| i.string(t::MAKE)).unwrap_or_default();
    let mn = lightcraft_tiff::makernote::parse_makernote(data, e.offset, e.count() as u64, tiff.order, &make)?;
    text(Some(&mn.ifd), PANASONIC_LENS_TYPE)
}

/// The lens name from the Exif of a RW2's (or Leica RWL's) embedded JpgFromRaw.
fn panasonic_jpg_from_raw_lens(tiff: &Tiff) -> Option<String> {
    let jpg = tiff.ifds.first()?.bytes(PANASONIC_JPG_FROM_RAW)?;
    let exif = crate::jpeg_segments(jpg).exif?;
    let data = strip_exif_header(&exif);
    panasonic_lens(data, &Tiff::parse(data).ok()?)
}

/// Canon maker note tag `0x0095` (LensModel in the ExifTool Canon tag names): the lens name as ASCII. Older
/// bodies instead record a numeric LensType (CameraSettings, tag `0x0001`, index 22), which is no name to read.
const CANON_LENS_MODEL: u16 = 0x0095;

/// The lens name in a Canon maker note of this Exif stream (`data` starts at its TIFF header): the note of
/// its own Exif IFD. Only consulted when Make is Canon, so other vendors' plain notes are never read as lenses.
fn canon_maker_note_lens(data: &[u8], tiff: &Tiff) -> Option<String> {
    let make = tiff.ifds.first().and_then(|i| i.string(t::MAKE)).unwrap_or_default();
    if !make.trim().to_ascii_uppercase().starts_with("CANON") {
        return None;
    }
    let exif = tiff.exif()?;
    let e = exif.get(t::MAKER_NOTE)?;
    let mn = lightcraft_tiff::makernote::parse_makernote(data, e.offset, e.count() as u64, tiff.order, &make)?;
    text(Some(&mn.ifd), CANON_LENS_MODEL)
}

/// Nikon maker note tag `0x0084` (Lens in the ExifTool Nikon tag names): shortest and longest focal length, then the
/// widest aperture at each, as rationals, like Exif LensSpecification. Zero for a lens that doesn't report itself.
const NIKON_LENS: u16 = 0x0084;

/// The lens a Nikon camera records in its maker note, when it's plausible.
fn nikon_lens_spec(exif: Option<&Ifd>, make: Option<&str>, order: lightcraft_tiff::ByteOrder) -> Option<[f64; 4]> {
    let make = make?;
    if !make.trim().to_ascii_uppercase().starts_with("NIKON") {
        return None;
    }
    let note = exif?.bytes(t::MAKER_NOTE)?;
    let mn = lightcraft_tiff::makernote::parse_makernote(note, 0, note.len() as u64, order, make)?;
    let s = mn.ifd.f64s(NIKON_LENS)?;
    let [short, long, wide, narrow] = s.as_slice() else { return None };
    let focal = |f: f64| f.is_finite() && f > 0.0 && f < 10_000.0;
    let aperture = |a: f64| a.is_finite() && a > 0.0 && a < 100.0;
    (focal(*short) && focal(*long) && long >= short && aperture(*wide) && aperture(*narrow)).then_some([*short, *long, *wide, *narrow])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    fn capture_time_tiff(original: Option<&str>, digitized: Option<&str>, ifd0_time: Option<&str>, xmp: Option<&str>) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        if let Some(value) = original {
            exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii(value.into()));
        }
        if let Some(value) = digitized {
            exif.set(t::DATE_TIME_DIGITIZED, Value::Ascii(value.into()));
        }
        let mut ifd0 = IfdBuilder::new();
        if let Some(value) = ifd0_time {
            ifd0.set(t::DATE_TIME, Value::Ascii(value.into()));
        }
        ifd0.set_child(t::EXIF_IFD, exif);
        if let Some(packet) = xmp {
            ifd0.set(t::XMP, Value::Byte(packet.as_bytes().to_vec()));
        }
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn exif_clock_invalid_original_uses_the_next_valid_exif_time() {
        let invalid = "2026:01:01 12:34:56:99";
        let digitized = capture_time_tiff(Some(invalid), Some("2026:01:02 10:00:00"), Some("2026:01:03 11:00:00"), None);
        assert_eq!(read_exif(&digitized).capture_time.map(|date| date.to_iso()), Some("2026-01-02T10:00:00".into()));

        let ifd0 = capture_time_tiff(Some(invalid), Some("2026:01:02 10:00:00:"), Some("2026:01:03 11:00:00"), None);
        assert_eq!(read_exif(&ifd0).capture_time.map(|date| date.to_iso()), Some("2026-01-03T11:00:00".into()));

        let none = capture_time_tiff(Some(invalid), Some("2026:01:02 10:00:00:junk"), Some("2026:01:03 11:00:00:"), None);
        assert_eq!(read_exif(&none).capture_time, None);

        let original = capture_time_tiff(Some("2026:01:04 12:00:00"), Some("2026:01:02 10:00:00"), Some("2026:01:03 11:00:00"), None);
        assert_eq!(read_exif(&original).capture_time.map(|date| date.to_iso()), Some("2026-01-04T12:00:00".into()));
    }

    #[test]
    fn exif_clock_invalid_original_allows_xmp_fallback_and_keeps_raw_tag() {
        let capture = DateTime::parse_iso("2026-01-02T10:00:00Z");
        let packet = crate::write_xmp(&Metadata { capture_time: capture, ..Default::default() }, None);
        let bytes = capture_time_tiff(Some("2026:01:01 12:34:56:99"), None, None, Some(&packet));
        assert_eq!(crate::extract(&bytes).capture_time.map(|date| date.to_iso()), Some("2026-01-02T10:00:00+00:00".into()));

        let without_fallback = capture_time_tiff(Some("2026:01:01 12:34:56:junk"), None, None, None);
        assert_eq!(crate::extract(&without_fallback).capture_time, None);
        assert!(crate::embedded(&without_fallback).exif.is_some(), "rejecting the projection must retain the embedded EXIF block");
    }

    /// An Exif block from a Panasonic body: no LensModel, the lens name in maker note tag 0x0051, whose value
    /// lies outside the note's IFD at an offset relative to the Exif TIFF header (as Panasonic writes it).
    pub(crate) fn panasonic_exif(order: ByteOrder, lens: Option<&str>, lens_model: Option<&str>) -> Vec<u8> {
        let note = |at: u32| {
            let mut v = b"Panasonic\0\0\0".to_vec();
            order.put_u16(&mut v, 1);
            order.put_u16(&mut v, if lens.is_some() { PANASONIC_LENS_TYPE } else { 0x0001 });
            order.put_u16(&mut v, 2);
            let name = format!("{:<34}\0", lens.unwrap_or("0210"));
            order.put_u32(&mut v, name.len() as u32);
            order.put_u32(&mut v, at + 30);
            order.put_u32(&mut v, 0);
            v.extend_from_slice(name.as_bytes());
            v
        };
        let build = |at: u32| {
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(note(at)));
            exif.set(t::FOCAL_LENGTH, Value::Rational(vec![(25, 1)]));
            if let Some(l) = lens_model {
                exif.set(t::LENS_MODEL, Value::Ascii(l.into()));
            }
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Panasonic".into()));
            ifd0.set_child(t::EXIF_IFD, exif);
            TiffWriter::new(order, false).write(&[ifd0]).unwrap()
        };
        // the note's own offset is only known once it is laid out; its length doesn't change
        let first = build(0);
        let at = Tiff::parse(&first).unwrap().exif().unwrap().get(t::MAKER_NOTE).unwrap().offset as u32;
        build(at)
    }

    #[test]
    fn panasonic_maker_note_names_the_lens() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let m = read_exif(&panasonic_exif(order, Some("LUMIX G 25/F1.7"), None));
            assert_eq!(m.lens_model.as_deref(), Some("LUMIX G 25/F1.7"));
            assert_eq!(m.focal_length, Some(25.0));
        }
        // Exif's own LensModel wins, and a note without 0x0051 leaves the lens empty
        let m = read_exif(&panasonic_exif(ByteOrder::Little, Some("LUMIX G 25/F1.7"), Some("Other 25mm")));
        assert_eq!(m.lens_model.as_deref(), Some("Other 25mm"));
        assert_eq!(read_exif(&panasonic_exif(ByteOrder::Little, None, None)).lens_model, None);
    }

    /// A RW2 has the full Exif only in the APP1 of its JpgFromRaw (IFD0 0x002E), not in its own Exif IFD.
    pub(crate) fn panasonic_rw2(lens: &str) -> Vec<u8> {
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xe1];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(panasonic_exif(ByteOrder::Little, Some(lens), None));
        jpg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        jpg.extend(app1);
        jpg.extend_from_slice(&[0xff, 0xda, 0, 2, 0xff, 0xd9]);
        let mut exif = IfdBuilder::new();
        exif.set(t::FOCAL_LENGTH, Value::Rational(vec![(85, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Panasonic".into()));
        ifd0.set(PANASONIC_JPG_FROM_RAW, Value::Undefined(jpg));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn rw2_reads_the_lens_from_its_jpg_from_raw() {
        let rw2 = panasonic_rw2("LUMIX S 85/F1.8");
        let m = read_exif(&rw2);
        assert_eq!(m.lens_model.as_deref(), Some("LUMIX S 85/F1.8"));
        assert_eq!(m.focal_length, Some(85.0));
        assert_eq!(crate::extract(&rw2).lens_model.as_deref(), Some("LUMIX S 85/F1.8"));
    }

    /// Canon maker note tags: `0x0095` holds the lens name as ASCII; `0x0001` is CameraSettings, a table of
    /// int16s whose index 22 is the numeric LensType.
    const CANON_LENS_MODEL_TAG: u16 = 0x0095;
    const CANON_CAMERA_SETTINGS: u16 = 0x0001;

    /// An Exif block from a Canon EOS 5D: `Canon` / `Canon EOS 5D` in IFD0, no LensModel tag, and a plain maker
    /// note whose IFD (value offsets relative to the enclosing TIFF header, as Canon writes it) records
    /// CameraSettings with the numeric LensType at index 22 and, when `lens` is given, the name at `0x0095`.
    fn canon_exif(order: ByteOrder, lens: Option<&str>) -> Vec<u8> {
        let note = |at: u32| {
            // The plain IFD: entry count, one or two 12-byte entries and the next-IFD pointer, then the
            // out-of-line values (CameraSettings, then the lens name when present).
            // pointer, then the out-of-line values (CameraSettings, then the lens name when present).
            let settings_at = at + 2 + 12 * (1 + lens.is_some() as u32) + 4;
            let name_at = settings_at + 60;
            let mut v = Vec::new();
            let entry = |v: &mut Vec<u8>, tag: u16, kind: u16, count: u32, off: u32| {
                order.put_u16(v, tag);
                order.put_u16(v, kind);
                order.put_u32(v, count);
                order.put_u32(v, off);
            };
            order.put_u16(&mut v, 1 + lens.is_some() as u16);
            entry(&mut v, CANON_CAMERA_SETTINGS, 3, 30, settings_at);
            if let Some(name) = lens {
                entry(&mut v, CANON_LENS_MODEL_TAG, 2, name.len() as u32, name_at);
            }
            order.put_u32(&mut v, 0);
            for i in 0..30u16 {
                order.put_u16(&mut v, if i == 22 { 178 } else { 0 }); // 22: numeric LensType, no name
            }
            if let Some(name) = lens {
                v.extend_from_slice(name.as_bytes());
            }
            v
        };
        let build = |at: u32| {
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(note(at)));
            exif.set(t::FOCAL_LENGTH, Value::Rational(vec![(50, 1)]));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Canon".into()));
            ifd0.set(t::MODEL, Value::Ascii("Canon EOS 5D".into()));
            ifd0.set_child(t::EXIF_IFD, exif);
            TiffWriter::new(order, false).write(&[ifd0]).unwrap()
        };
        // the note's own offset is only known once it is laid out; its length doesn't change
        let first = build(0);
        let at = Tiff::parse(&first).unwrap().exif().unwrap().get(t::MAKER_NOTE).unwrap().offset as u32;
        build(at)
    }

    #[test]
    fn canon_maker_note_lens_fills_lens_model() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let m = read_exif(&canon_exif(order, Some("Canon EF 50mm f/1.8 II")));
            assert_eq!(m.lens_model.as_deref(), Some("Canon EF 50mm f/1.8 II"));
            // a note that only records the numeric LensType has no name to fill in
            assert_eq!(read_exif(&canon_exif(order, None)).lens_model, None);
        }
    }

    /// An Exif block from a Nikon body whose maker note records the lens (tag 0x0084) and no LensModel or
    /// LensSpecification, as the D700, D750, D80 and D5300 write it.
    fn nikon_exif(make: &str, lens: Option<[(u32, u32); 4]>, spec: Option<[(u32, u32); 4]>) -> Vec<u8> {
        let mut note = IfdBuilder::new();
        note.set(0x0001, Value::Undefined(b"0210".to_vec()));
        if let Some(l) = lens {
            note.set(NIKON_LENS, Value::Rational(l.to_vec()));
        }
        let mut blob = b"Nikon\0\x02\x10\0\0".to_vec();
        blob.extend(TiffWriter::new(ByteOrder::Little, false).write(&[note]).unwrap());
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(blob));
        if let Some(sp) = spec {
            exif.set(t::LENS_SPECIFICATION, Value::Rational(sp.to_vec()));
        }
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii(make.into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Big, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn nikon_maker_note_lens_fills_the_lens_spec() {
        let zoom = [(18, 1), (135, 1), (35, 10), (56, 10)];
        assert_eq!(read_exif(&nikon_exif("NIKON CORPORATION", Some(zoom), None)).lens_spec, Some([18.0, 135.0, 3.5, 5.6]));
        // Exif's own LensSpecification wins, another make's note is not read, and a lens that reports nothing is left out
        let prime = [(50, 1), (50, 1), (18, 10), (18, 10)];
        assert_eq!(read_exif(&nikon_exif("NIKON CORPORATION", Some(zoom), Some(prime))).lens_spec, Some([50.0, 50.0, 1.8, 1.8]));
        assert_eq!(read_exif(&nikon_exif("Canon", Some(zoom), None)).lens_spec, None);
        assert_eq!(read_exif(&nikon_exif("NIKON CORPORATION", Some([(0, 1), (0, 1), (0, 1), (0, 1)]), None)).lens_spec, None);
        assert_eq!(read_exif(&nikon_exif("NIKON CORPORATION", Some([(200, 1), (80, 1), (28, 10), (28, 10)]), None)).lens_spec, None);
        assert_eq!(read_exif(&nikon_exif("NIKON CORPORATION", None, None)).lens_spec, None);
    }

    pub(crate) fn sample_exif(order: ByteOrder) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(t::EXPOSURE_TIME, Value::Rational(vec![(1, 250)]));
        exif.set(t::F_NUMBER, Value::Rational(vec![(28, 10)]));
        exif.set(t::ISO_SPEED, Value::Short(vec![400]));
        exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii("2023:07:14 18:30:05".into()));
        exif.set(t::OFFSET_TIME_ORIGINAL, Value::Ascii("+02:00".into()));
        exif.set(t::SUBSEC_TIME_ORIGINAL, Value::Ascii("42".into()));
        exif.set(t::EXPOSURE_BIAS, Value::SRational(vec![(-2, 3)]));
        exif.set(t::METERING_MODE, Value::Short(vec![5]));
        exif.set(t::FLASH, Value::Short(vec![0x19]));
        exif.set(t::FOCAL_LENGTH, Value::Rational(vec![(50, 1)]));
        exif.set(t::FOCAL_LENGTH_35MM, Value::Short(vec![75]));
        exif.set(t::WHITE_BALANCE, Value::Short(vec![0]));
        exif.set(t::EXPOSURE_PROGRAM, Value::Short(vec![3]));
        exif.set(t::LENS_MAKE, Value::Ascii("LensCo".into()));
        exif.set(t::LENS_MODEL, Value::Ascii("50mm F1.8".into()));
        exif.set(t::BODY_SERIAL_NUMBER, Value::Ascii("12345".into()));
        exif.set(t::LENS_SPECIFICATION, Value::Rational(vec![(50, 1), (50, 1), (18, 10), (18, 10)]));
        exif.set(t::PIXEL_X_DIMENSION, Value::Long(vec![6000]));
        exif.set(t::PIXEL_Y_DIMENSION, Value::Long(vec![4000]));
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE_REF, Value::Ascii("S".into()));
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(33, 1), (51, 1), (3564, 100)]));
        gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
        gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(151, 1), (12, 1), (4032, 100)]));
        gps.set(t::GPS_ALTITUDE_REF, Value::Byte(vec![1]));
        gps.set(t::GPS_ALTITUDE, Value::Rational(vec![(125, 10)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Maker Inc.".into()));
        ifd0.set(t::MODEL, Value::Ascii("Model X ".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
        ifd0.set(t::ARTIST, Value::Ascii("Jane".into()));
        ifd0.set(t::COPYRIGHT, Value::Ascii("(c) Jane".into()));
        ifd0.set(t::RATING, Value::Short(vec![3]));
        ifd0.set_child(t::EXIF_IFD, exif);
        ifd0.set_child(t::GPS_IFD, gps);
        TiffWriter::new(order, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn reads_all_fields() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let m = read_exif(&sample_exif(order));
            assert_eq!(m.make.as_deref(), Some("Maker Inc."));
            assert_eq!(m.model.as_deref(), Some("Model X"));
            assert_eq!(m.exposure_time, Some(0.004));
            assert_eq!(m.f_number, Some(2.8));
            assert_eq!(m.iso, Some(400));
            let dt = m.capture_time.unwrap();
            assert_eq!(dt.to_iso(), "2023-07-14T18:30:05.420+02:00");
            assert!((m.exposure_bias.unwrap() + 0.6667).abs() < 1e-3);
            assert_eq!(m.metering_mode, Some(5));
            assert_eq!(m.flash, Some(Flash { fired: true, raw: 0x19 }));
            assert_eq!(m.focal_length, Some(50.0));
            assert_eq!(m.focal_length_35mm, Some(75.0));
            assert_eq!(m.orientation, Some(Orientation::Rotate90));
            assert_eq!(m.lens_model.as_deref(), Some("50mm F1.8"));
            assert_eq!(m.lens_make.as_deref(), Some("LensCo"));
            assert_eq!(m.serial_number.as_deref(), Some("12345"));
            assert_eq!(m.lens_spec, Some([50.0, 50.0, 1.8, 1.8]));
            assert_eq!((m.width, m.height), (Some(6000), Some(4000)));
            assert_eq!(m.artist.as_deref(), Some("Jane"));
            assert_eq!(m.rating, Some(3));
            let g = m.gps.unwrap();
            assert!((g.latitude + 33.8599).abs() < 1e-4, "{g:?}");
            assert!((g.longitude - 151.2112).abs() < 1e-4);
            assert_eq!(g.altitude, Some(-12.5));
        }
    }

    #[test]
    fn placeholder_gps_block_is_no_location() {
        // Android phones without a fix write NUL refs and 0/0 rationals (#1617).
        let read = |lat_ref: &str, lat: Vec<(u32, u32)>| {
            let mut gps = IfdBuilder::new();
            gps.set(t::GPS_LATITUDE_REF, Value::Ascii(lat_ref.into()));
            gps.set(t::GPS_LATITUDE, Value::Rational(lat));
            gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
            gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(151, 1), (12, 1), (4032, 100)]));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set_child(t::GPS_IFD, gps);
            read_exif(&TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()).gps
        };
        let valid = vec![(33, 1), (51, 1), (3564, 100)];
        assert!(read("N", valid.clone()).is_some());
        assert_eq!(read("N", vec![(0, 0), (0, 0), (0, 0)]), None, "0/0 rationals");
        assert_eq!(read("N", vec![(33, 1), (0, 0), (0, 0)]), None, "0/0 minutes");
        assert_eq!(read("\0", valid.clone()), None, "NUL ref");
        assert_eq!(read("", valid.clone()), None, "empty ref");
        assert_eq!(read("X", valid), None, "invalid ref");
    }

    fn gps_from_components_with_refs(lat: Vec<(u32, u32)>, lon: Vec<(u32, u32)>, lat_ref: &str, lon_ref: &str) -> Option<Gps> {
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE_REF, Value::Ascii(lat_ref.into()));
        gps.set(t::GPS_LATITUDE, Value::Rational(lat));
        gps.set(t::GPS_LONGITUDE_REF, Value::Ascii(lon_ref.into()));
        gps.set(t::GPS_LONGITUDE, Value::Rational(lon));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        read_exif(&TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()).gps
    }

    fn gps_from_components(lat: Vec<(u32, u32)>, lon: Vec<(u32, u32)>) -> Option<Gps> {
        gps_from_components_with_refs(lat, lon, "N", "E")
    }

    /// Destination coordinates count as location data even without the primary position tags.
    #[test]
    fn exif_gps_destination_tags_are_location_bearing() {
        for tag in [t::GPS_DEST_LATITUDE, t::GPS_DEST_LONGITUDE] {
            let mut gps = IfdBuilder::new();
            gps.set(tag, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set_child(t::GPS_IFD, gps);
            let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
            assert!(exif_embeds_gps_location(&bytes), "destination tag {tag}");
        }
    }

    /// Location values in TIFF XMP and IPTC prevent an unchanged Original copy.
    #[test]
    fn source_tiff_guard_checks_xmp_and_iptc_location() {
        let xmp = crate::write_xmp(&Metadata { city: Some("Cusco".into()), ..Default::default() }, None);
        let xmp_tiff = TiffWriter::default().write(&[IfdBuilder::new().with(t::XMP, Value::Byte(xmp.into_bytes()))]).unwrap();

        let iim = [0x1c, 2, 90, 0, 5, b'C', b'u', b's', b'c', b'o'];
        let iptc_tiff = TiffWriter::default().write(&[IfdBuilder::new().with(t::IPTC_NAA, Value::Undefined(iim.to_vec()))]).unwrap();

        assert!(source_embeds_exif_gps_location(&xmp_tiff), "TIFF tag 700 can contain place names");
        assert!(source_embeds_exif_gps_location(&iptc_tiff), "TIFF IPTC can contain place names");
    }

    #[test]
    fn source_tiff_guard_checks_photoshop_resources_and_long_iptc() {
        let city_iim = [0x1c, 2, 90, 0, 5, b'C', b'u', b's', b'c', b'o'];
        let mut resource = b"8BIM".to_vec();
        resource.extend_from_slice(&0x0404u16.to_be_bytes());
        resource.extend_from_slice(&[0, 0]);
        resource.extend_from_slice(&u32::try_from(city_iim.len()).unwrap().to_be_bytes());
        resource.extend_from_slice(&city_iim);
        assert_eq!(resource.len() % 2, 0);
        let photoshop_tiff =
            TiffWriter::default().write(&[IfdBuilder::new().with(t::PHOTOSHOP_IMAGE_RESOURCES, Value::Undefined(resource))]).unwrap();
        assert!(source_embeds_exif_gps_location(&photoshop_tiff), "TIFF Photoshop resources may contain IPTC place names");

        let clean_iim = [0x1c, 2, 25, 0, 4, b't', b'e', b's', b't'];
        let padded: Vec<_> = clean_iim
            .chunks(4)
            .map(|chunk| {
                let mut bytes = [0u8; 4];
                bytes[..chunk.len()].copy_from_slice(chunk);
                u32::from_le_bytes(bytes)
            })
            .collect();
        let long_iptc_tiff = TiffWriter::new(ByteOrder::Little, false).write(&[IfdBuilder::new().with(t::IPTC_NAA, Value::Long(padded))]).unwrap();
        assert!(!source_embeds_exif_gps_location(&long_iptc_tiff), "commonly mis-typed LONG IPTC without place names remains safe");
    }

    #[test]
    fn gps_value_tags_in_non_gps_ifds_are_not_location() {
        let mut interop = IfdBuilder::new();
        interop.set(t::GPS_LATITUDE, Value::Undefined(vec![b'R', b'9', b'8', b'\0']));
        let mut exif = IfdBuilder::new();
        exif.set_child(t::INTEROP_IFD, interop);
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::EXIF_IFD, exif);
        let bytes = TiffWriter::default().write(&[ifd0]).unwrap();
        assert!(!exif_embeds_gps_location(&bytes), "InteroperabilityVersion reuses the GPS latitude tag number");
    }

    /// An unreadable GPS child still counts when its parent contains the GPS pointer tag.
    #[test]
    fn exif_gps_unreadable_child_is_location_bearing() {
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (0, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        let mut bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

        let tiff = Tiff::parse(&bytes).unwrap();
        assert!(tiff.gps().is_some());
        let pointer = usize::try_from(tiff.ifds.first().and_then(|ifd| ifd.get(t::GPS_IFD)).unwrap().offset).unwrap();
        let pointer_end = pointer.checked_add(4).unwrap();
        bytes.get_mut(pointer..pointer_end).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());

        let unreadable = Tiff::parse(&bytes).unwrap();
        assert!(unreadable.gps().is_none(), "the out-of-range GPS child is not resolved");
        assert!(unreadable.ifds.first().is_some_and(|ifd| ifd.contains(t::GPS_IFD)), "the parent GPS pointer remains parsed");
        assert!(exif_embeds_gps_location(&bytes));
    }

    /// TIFF JPEG thumbnails are checked for embedded GPS and fail closed when their range is unreadable.
    #[test]
    fn exif_gps_guard_checks_jpeg_thumbnail_metadata() {
        let exif_with_gps = {
            let mut gps = IfdBuilder::new();
            gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
            gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
            gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
            gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(10, 1), (30, 1), (0, 1)]));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set_child(t::GPS_IFD, gps);
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
        };
        let exif_without_gps = {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Procedural camera".into()));
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
        };
        let tiff_with_thumbnail = |thumbnail_exif: &[u8]| {
            let mut jpeg = vec![0xff, 0xd8];
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend_from_slice(thumbnail_exif);
            let length = u16::try_from(payload.len().saturating_add(2)).unwrap();
            jpeg.extend_from_slice(&[0xff, 0xe1]);
            jpeg.extend_from_slice(&length.to_be_bytes());
            jpeg.extend_from_slice(&payload);
            jpeg.extend_from_slice(&[0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9]);

            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Outer camera".into()));
            let mut ifd1 = IfdBuilder::new();
            ifd1.set(t::JPEG_INTERCHANGE_FORMAT, Value::Long(vec![0]));
            ifd1.set(t::JPEG_INTERCHANGE_FORMAT_LENGTH, Value::Long(vec![u32::try_from(jpeg.len()).unwrap()]));
            let mut bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0, ifd1]).unwrap();
            let parsed = Tiff::parse(&bytes).unwrap();
            let value_at = usize::try_from(parsed.ifds.get(1).unwrap().get(t::JPEG_INTERCHANGE_FORMAT).unwrap().offset).unwrap();
            let thumbnail_at = u32::try_from(bytes.len()).unwrap();
            bytes.extend_from_slice(&jpeg);
            bytes.get_mut(value_at..value_at + 4).unwrap().copy_from_slice(&thumbnail_at.to_le_bytes());
            bytes
        };

        let with_gps = tiff_with_thumbnail(&exif_with_gps);
        let without_gps = tiff_with_thumbnail(&exif_without_gps);
        assert!(exif_embeds_gps_location(&with_gps), "GPS in the thumbnail must count as location-bearing");
        assert!(!exif_embeds_gps_location(&without_gps), "GPS-free thumbnails remain eligible");

        let mut unreadable_thumbnail = with_gps;
        let parsed = Tiff::parse(&unreadable_thumbnail).unwrap();
        let value_at = usize::try_from(parsed.ifds.get(1).unwrap().get(t::JPEG_INTERCHANGE_FORMAT).unwrap().offset).unwrap();
        unreadable_thumbnail.get_mut(value_at..value_at + 4).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(source_embeds_exif_gps_location(&unreadable_thumbnail), "an unreadable thumbnail cannot authorize Original export");
    }

    /// XMP location metadata in a TIFF JPEG thumbnail is retained by an unchanged Original copy.
    #[test]
    fn exif_gps_guard_checks_xmp_in_jpeg_thumbnail() {
        let xmp = crate::write_xmp(&Metadata { city: Some("Cusco".into()), ..Default::default() }, None);
        let packet = [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), xmp.as_bytes()].concat();
        let segment_length = u16::try_from(packet.len().saturating_add(2)).unwrap();
        let jpeg = [vec![0xff, 0xd8, 0xff, 0xe1], segment_length.to_be_bytes().to_vec(), packet, vec![0xff, 0xd9]].concat();

        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Outer camera".into()));
        let mut ifd1 = IfdBuilder::new();
        ifd1.set(t::JPEG_INTERCHANGE_FORMAT, Value::Long(vec![0]));
        ifd1.set(t::JPEG_INTERCHANGE_FORMAT_LENGTH, Value::Long(vec![u32::try_from(jpeg.len()).unwrap()]));
        let mut bytes = TiffWriter::default().write(&[ifd0, ifd1]).unwrap();
        let parsed = Tiff::parse(&bytes).unwrap();
        let value_at = usize::try_from(parsed.ifds.get(1).unwrap().get(t::JPEG_INTERCHANGE_FORMAT).unwrap().offset).unwrap();
        let thumbnail_at = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&jpeg);
        bytes.get_mut(value_at..value_at + 4).unwrap().copy_from_slice(&thumbnail_at.to_le_bytes());

        assert!(source_embeds_exif_gps_location(&bytes), "XMP place names in the thumbnail must block the copy");
    }

    /// Repeated IFD references cannot restart the byte-work allowance for the same JPEG thumbnail.
    #[test]
    fn exif_gps_guard_bounds_repeated_thumbnail_scans() {
        let thumbnail_payload = vec![0; 60_000];
        let segment_length = u16::try_from(thumbnail_payload.len().saturating_add(2)).unwrap();
        let thumbnail = [vec![0xff, 0xd8, 0xff, 0xe0], segment_length.to_be_bytes().to_vec(), thumbnail_payload, vec![0xff, 0xd9]].concat();

        let mut ifds = Vec::new();
        for _ in 0..24 {
            let mut ifd = IfdBuilder::new();
            ifd.set(t::JPEG_INTERCHANGE_FORMAT, Value::Long(vec![0]));
            ifd.set(t::JPEG_INTERCHANGE_FORMAT_LENGTH, Value::Long(vec![u32::try_from(thumbnail.len()).unwrap()]));
            ifds.push(ifd);
        }
        let mut bytes = TiffWriter::default().write(&ifds).unwrap();
        let parsed = Tiff::parse(&bytes).unwrap();
        let thumbnail_at = u32::try_from(bytes.len()).unwrap();
        for ifd in &parsed.ifds {
            let value_at = usize::try_from(ifd.get(t::JPEG_INTERCHANGE_FORMAT).unwrap().offset).unwrap();
            bytes.get_mut(value_at..value_at + 4).unwrap().copy_from_slice(&thumbnail_at.to_le_bytes());
        }
        bytes.extend_from_slice(&thumbnail);

        let mut budget = crate::container::gps_scan_budget(bytes.len());
        assert_eq!(exif_gps_tag_presence_with_budget(&bytes, 0, &mut budget), Some(true));
        assert_eq!(budget, 0, "the shared work budget should exhaust before all references can be rescanned");
        assert!(source_embeds_exif_gps_location(&bytes));
    }

    /// Panasonic `JpgFromRaw` is an embedded JPEG, not TIFF's generic thumbnail offset/length pair.
    #[test]
    fn exif_gps_guard_checks_panasonic_jpg_from_raw() {
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut gps_ifd0 = IfdBuilder::new();
        gps_ifd0.set_child(t::GPS_IFD, gps);
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend(TiffWriter::default().write(&[gps_ifd0]).unwrap());
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend_from_slice(&u16::try_from(exif_payload.len() + 2).unwrap().to_be_bytes());
        jpeg.extend_from_slice(&exif_payload);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        let ifd0 = IfdBuilder::new().with(PANASONIC_JPG_FROM_RAW, Value::Undefined(jpeg));
        let rw2 = TiffWriter::default().write(&[ifd0]).unwrap();

        assert!(source_embeds_exif_gps_location(&rw2), "GPS in Panasonic JpgFromRaw must block an unchanged copy");
    }

    /// Recognized TIFF headers with an unreadable IFD cannot establish that GPS is absent.
    #[test]
    fn source_tiff_gps_guard_fails_closed_on_unreadable_ifd() {
        let malformed = b"II*\0\xff\xff\xff\xff";
        assert!(source_embeds_exif_gps_location(malformed));
    }

    /// JPEG XL sources are not inspected for EXIF/XMP here, so Original export must fail closed.
    #[test]
    fn source_jxl_gps_guard_fails_closed_when_metadata_is_unverified() {
        let bare_codestream = [0xff, 0x0a, 0x00, 0x00];
        let signature_box = b"\0\0\0\x0cJXL \r\n\x87\n";
        let ftyp_box = b"\0\0\0\x10ftypjxl \0\0\0\0";

        for (name, bytes) in [
            ("bare codestream", bare_codestream.as_slice()),
            ("container signature", signature_box.as_slice()),
            ("ISO-BMFF jxl brand", ftyp_box.as_slice()),
        ] {
            assert!(source_embeds_exif_gps_location(bytes), "{name} cannot authorize Original export");
        }
    }

    /// Invalid EXIF GPS subdivisions are rejected for latitude and longitude.
    #[test]
    fn exif_gps_rejects_invalid_subdivisions() {
        let valid_axis = vec![(10, 1), (0, 1), (0, 1)];
        for invalid in
            [vec![(45, 1), (60, 1), (0, 1)], vec![(45, 1), (90, 1), (0, 1)], vec![(45, 1), (0, 1), (60, 1)], vec![(45, 1), (0, 1), (90, 1)]]
        {
            assert!(gps_from_components(invalid.clone(), valid_axis.clone()).is_none(), "latitude {invalid:?}");
            assert!(gps_from_components(valid_axis.clone(), invalid.clone()).is_none(), "longitude {invalid:?}");
        }
    }

    /// Signed negative minutes and seconds are not valid DMS subdivisions on either axis.
    #[test]
    fn exif_gps_rejects_negative_subdivisions() {
        let signed_gps = |lat: Vec<(i32, i32)>, lon: Vec<(i32, i32)>| {
            let mut gps = IfdBuilder::new();
            gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
            gps.set(t::GPS_LATITUDE, Value::SRational(lat));
            gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
            gps.set(t::GPS_LONGITUDE, Value::SRational(lon));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set_child(t::GPS_IFD, gps);
            read_exif(&TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()).gps
        };
        let valid = vec![(10, 1), (0, 1), (0, 1)];
        for invalid in [vec![(45, 1), (-1, 1), (0, 1)], vec![(45, 1), (0, 1), (-1, 1)]] {
            assert!(signed_gps(invalid.clone(), valid.clone()).is_none(), "latitude {invalid:?}");
            assert!(signed_gps(valid.clone(), invalid.clone()).is_none(), "longitude {invalid:?}");
        }
    }

    #[test]
    fn exif_gps_preserves_valid_subdivisions() {
        let gps =
            gps_from_components(vec![(45, 1), (5999, 100), (5999, 100)], vec![(10, 1), (30, 1), (0, 1)]).expect("fractional components below 60");
        assert!((gps.latitude - (45.0 + 59.99 / 60.0 + 59.99 / 3600.0)).abs() < 1e-10);
        assert_eq!(gps.longitude, 10.5);
        assert_eq!(
            gps_from_components_with_refs(vec![(45, 1), (30, 1), (0, 1)], vec![(10, 1), (30, 1), (0, 1)], "S", "W")
                .map(|g| (g.latitude, g.longitude)),
            Some((-45.5, -10.5))
        );
        assert_eq!(
            gps_from_components(vec![(45, 1)], vec![(10, 1), (30, 1)]).map(|g| (g.latitude, g.longitude)),
            Some((45.0, 10.5)),
            "missing minutes / seconds retain their zero defaults"
        );
        assert_eq!(
            gps_from_components(vec![(0, 1), (0, 1), (0, 1)], vec![(0, 1), (0, 1), (0, 1)]).map(|g| (g.latitude, g.longitude)),
            Some((0.0, 0.0))
        );
        assert_eq!(
            gps_from_components(vec![(90, 1), (0, 1), (0, 1)], vec![(180, 1), (0, 1), (0, 1)]).map(|g| (g.latitude, g.longitude)),
            Some((90.0, 180.0))
        );
    }

    /// Writer rounding carries through minute and degree boundaries without emitting a 60 component.
    #[test]
    fn exif_gps_writer_rounds_before_splitting_dms() {
        let minute_carry = 12.0 + 59.9996 / 3600.0;
        let degree_carry = 45.0 + 59.0 / 60.0 + 59.9996 / 3600.0;
        let bytes =
            write_exif(&Metadata { gps: Some(Gps { latitude: degree_carry, longitude: minute_carry, altitude: None }), ..Default::default() });

        let tiff = Tiff::parse(&bytes).expect("writer emits a valid TIFF stream");
        let gps = tiff.gps().expect("valid coordinates are written");
        let Value::Rational(latitude) = gps.value(t::GPS_LATITUDE).expect("latitude is present") else { panic!("latitude must be rational") };
        let Value::Rational(longitude) = gps.value(t::GPS_LONGITUDE).expect("longitude is present") else { panic!("longitude must be rational") };
        assert_eq!(latitude, &[(46, 1), (0, 1), (0, 1000)]);
        assert_eq!(longitude, &[(12, 1), (1, 1), (0, 1000)]);
        for components in [latitude, longitude] {
            assert!(components.get(1).is_some_and(|(minutes, _)| *minutes < 60));
            assert!(components.get(2).is_some_and(|(seconds, denominator)| *seconds < 60 * denominator));
        }
        let round_trip = read_exif(&bytes).gps.expect("rounded output remains readable");
        assert_eq!(round_trip.latitude, 46.0);
        assert!((round_trip.longitude - (12.0 + 1.0 / 60.0)).abs() < 1e-10);
    }

    /// Invalid or non-finite coordinates do not produce a GPS IFD.
    #[test]
    fn exif_gps_writer_rejects_invalid_coordinates() {
        for (latitude, longitude) in [
            (91.0, 0.0),
            (-91.0, 0.0),
            (0.0, 181.0),
            (0.0, -181.0),
            (f64::NAN, 0.0),
            (0.0, f64::NAN),
            (f64::INFINITY, 0.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.0),
            (0.0, f64::NEG_INFINITY),
        ] {
            let bytes = write_exif(&Metadata { gps: Some(Gps { latitude, longitude, altitude: None }), ..Default::default() });
            let tiff = Tiff::parse(&bytes).expect("writer emits a valid TIFF stream");
            assert!(!tiff.ifds.first().is_some_and(|ifd| ifd.contains(t::GPS_IFD)), "invalid GPS {latitude}, {longitude}");
        }
    }

    /// Direct TIFF sources without a container EXIF block use their own GPS IFD for the privacy check.
    #[test]
    fn source_exif_gps_guard_checks_whole_file_tiff() {
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Procedural camera".into()));
        let without_gps = TiffWriter::default().write(&[ifd0]).unwrap();

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (0, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        let with_gps = TiffWriter::default().write(&[ifd0]).unwrap();

        assert!(!source_embeds_exif_gps_location(&without_gps));
        assert!(source_embeds_exif_gps_location(&with_gps));
    }

    #[test]
    fn exif_prefix_and_garbage() {
        let mut b = b"Exif\0\0".to_vec();
        b.extend(sample_exif(ByteOrder::Little));
        assert_eq!(read_exif(&b).iso, Some(400));
        assert_eq!(read_exif(b"garbage"), Metadata::default());
        assert!(try_read_exif(b"garbage").is_err());
    }

    #[test]
    fn apex_fallbacks_and_extended_iso() {
        let mut exif = IfdBuilder::new();
        exif.set(t::SHUTTER_SPEED_VALUE, Value::SRational(vec![(8, 1)]));
        exif.set(t::APERTURE_VALUE, Value::Rational(vec![(4, 1)]));
        exif.set(t::ISO_SPEED, Value::Short(vec![65535]));
        exif.set(t::RECOMMENDED_EXPOSURE_INDEX, Value::Long(vec![102400]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::DATE_TIME, Value::Ascii("2020:01:02 03:04:05".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        let m = read_exif(&TiffWriter::default().write(&[ifd0]).unwrap());
        assert_eq!(m.exposure_time, Some(1.0 / 256.0));
        assert_eq!(m.f_number, Some(4.0));
        assert_eq!(m.iso, Some(102400));
        assert_eq!(m.capture_time.unwrap().to_exif(), "2020:01:02 03:04:05");
    }

    #[test]
    fn xmp_in_ifd0_overrides_user_fields() {
        let mut meta = Metadata { rating: Some(5), title: Some("From XMP".into()), ..Default::default() };
        meta.keywords = vec!["a".into()];
        let x = crate::write_xmp(&meta, None);
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::RATING, Value::Short(vec![1]));
        ifd0.set(t::XMP, Value::Byte(x.into_bytes()));
        let m = read_exif(&TiffWriter::default().write(&[ifd0]).unwrap());
        assert_eq!(m.rating, Some(5));
        assert_eq!(m.title.as_deref(), Some("From XMP"));
    }
}

/// An exact `1/N` rational for a sub-second value whose reciprocal is a whole number (shutter speeds such as
/// 1/12800 s, which [`lightcraft_tiff::writer::rational`]'s decimal denominators cannot represent).
fn reciprocal(v: Option<f64>) -> Option<lightcraft_tiff::Value> {
    let v = v.filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)?;
    let inv = 1.0 / v;
    ((inv - inv.round()).abs() < 1e-6 && inv.round() <= u32::MAX as f64).then(|| lightcraft_tiff::Value::Rational(vec![(1, inv.round() as u32)]))
}

/// Convert a valid decimal coordinate to DMS, rounding total milliseconds before splitting so
/// rounding carries cannot emit 60 seconds or minutes.
fn gps_dms(value: f64, max_degrees: u32) -> Option<lightcraft_tiff::Value> {
    let magnitude = value.abs();
    if !magnitude.is_finite() || magnitude > f64::from(max_degrees) {
        return None;
    }
    let total_millis = (magnitude * 3_600_000.0).round() as u64;
    let degrees = (total_millis / 3_600_000) as u32;
    let minutes = ((total_millis % 3_600_000) / 60_000) as u32;
    let seconds_millis = (total_millis % 60_000) as u32;
    Some(lightcraft_tiff::Value::Rational(vec![(degrees, 1), (minutes, 1), (seconds_millis, 1000)]))
}

/// Serialise the interchange subset of `meta` as a TIFF-structured Exif block (little-endian, no
/// `Exif\0\0` prefix): IFD0 (make, model, software, artist, copyright, description, orientation,
/// date), the Exif IFD (capture settings, lens) and, when present, the GPS IFD.
pub fn write_exif(meta: &Metadata) -> Vec<u8> {
    use lightcraft_tiff::writer::{rational, srational};
    use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};
    let ascii = |s: &Option<String>| s.as_ref().filter(|v| !v.is_empty()).map(|v| Value::Ascii(v.clone()));
    let mut ifd0 = IfdBuilder::new();
    for (tag, v) in [
        (t::MAKE, ascii(&meta.make)),
        (t::MODEL, ascii(&meta.model)),
        (t::SOFTWARE, ascii(&meta.software)),
        (t::ARTIST, ascii(&meta.artist)),
        (t::COPYRIGHT, ascii(&meta.copyright)),
        (t::IMAGE_DESCRIPTION, ascii(&meta.caption)),
        (t::DATE_TIME, meta.capture_time.map(|d| Value::Ascii(d.to_exif()))),
        (t::ORIENTATION, meta.orientation.map(|o| Value::Short(vec![o.to_exif()]))),
    ] {
        if let Some(v) = v {
            ifd0.set(tag, v);
        }
    }
    let mut ex = IfdBuilder::new();
    ex.set(t::EXIF_VERSION, Value::Undefined(b"0232".to_vec()));
    let r = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0).map(|v| Value::Rational(vec![rational(v)]));
    for (tag, v) in [
        (t::DATE_TIME_ORIGINAL, meta.capture_time.map(|d| Value::Ascii(d.to_exif()))),
        (t::EXPOSURE_TIME, reciprocal(meta.exposure_time).or_else(|| r(meta.exposure_time))),
        (t::F_NUMBER, r(meta.f_number)),
        (t::FOCAL_LENGTH, r(meta.focal_length)),
        (t::ISO_SPEED, meta.iso.map(|i| Value::Short(vec![i.min(65_535) as u16]))),
        (t::ISO_SPEED_RATINGS_EXT, meta.iso.filter(|&i| i > 65_534).map(|i| Value::Long(vec![i]))),
        (t::LENS_MAKE, ascii(&meta.lens_make)),
        (t::LENS_MODEL, ascii(&meta.lens_model)),
        (t::EXPOSURE_BIAS, meta.exposure_bias.map(|v| Value::SRational(vec![srational(v)]))),
    ] {
        if let Some(v) = v {
            ex.set(tag, v);
        }
    }
    if let Some(d) = meta.capture_time {
        // an offset past 18 h is not a real one (a deserialized value can be anything): write none
        let offset = d.offset_minutes.filter(|o| o.unsigned_abs() <= 18 * 60).map(|o| {
            let sign = if o < 0 { '-' } else { '+' };
            format!("{sign}{:02}:{:02}", o.unsigned_abs() / 60, o.unsigned_abs() % 60)
        });
        let subsec = (d.millis != 0).then(|| format!("{:03}", d.millis));
        for (tag, v) in [(t::OFFSET_TIME, &offset), (t::OFFSET_TIME_ORIGINAL, &offset), (t::SUBSEC_TIME, &subsec), (t::SUBSEC_TIME_ORIGINAL, &subsec)]
        {
            if let Some(v) = v {
                ex.set(tag, Value::Ascii(v.clone()));
            }
        }
    }
    ifd0.set_child(t::EXIF_IFD, ex);
    if let Some(g) = meta.gps
        && let (Some(latitude), Some(longitude)) = (gps_dms(g.latitude, 90), gps_dms(g.longitude, 180))
    {
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_VERSION_ID, Value::Byte(vec![2, 3, 0, 0]));
        gps.set(t::GPS_LATITUDE_REF, Value::Ascii(if g.latitude < 0.0 { "S" } else { "N" }.into()));
        gps.set(t::GPS_LATITUDE, latitude);
        gps.set(t::GPS_LONGITUDE_REF, Value::Ascii(if g.longitude < 0.0 { "W" } else { "E" }.into()));
        gps.set(t::GPS_LONGITUDE, longitude);
        if let Some(a) = g.altitude {
            gps.set(t::GPS_ALTITUDE_REF, Value::Byte(vec![u8::from(a < 0.0)]));
            gps.set(t::GPS_ALTITUDE, Value::Rational(vec![rational(a.abs())]));
        }
        ifd0.set_child(t::GPS_IFD, gps);
    }
    TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap_or_default()
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn capture_time_offset_and_subsec_round_trip() {
        for iso in ["2026-09-20T05:40:00.250+02:00", "2026-09-20T05:40:00.005-03:30", "2026-09-20T05:40:00"] {
            let m = Metadata { capture_time: DateTime::parse_iso(iso), ..Default::default() };
            assert_eq!(read_exif(&write_exif(&m)).capture_time, m.capture_time, "{iso}");
        }
    }

    /// A deserialized offset can be any i16, `i16::MIN` included: writing must not overflow.
    #[test]
    fn extreme_capture_time_offsets_do_not_panic() {
        for o in [i16::MIN, i16::MAX, -1081, 1081] {
            let mut dt = DateTime::parse_iso("2026-09-20T05:40:00").unwrap();
            dt.offset_minutes = Some(o);
            let m = Metadata { capture_time: Some(dt), ..Default::default() };
            assert_eq!(read_exif(&write_exif(&m)).capture_time.map(|d| d.offset_minutes), Some(None), "{o}");
        }
    }

    #[test]
    fn exif_round_trips() {
        let m = Metadata {
            make: Some("Synthetic".into()),
            model: Some("X2".into()),
            copyright: Some("© 2026 A. Person".into()),
            artist: Some("A. Person".into()),
            capture_time: DateTime::parse_iso("2026-09-20T05:40:00"),
            exposure_time: Some(1.0 / 250.0),
            f_number: Some(2.8),
            iso: Some(400),
            focal_length: Some(35.0),
            lens_model: Some("35mm F2".into()),
            gps: Some(Gps { latitude: 43.7904, longitude: -110.6818, altitude: Some(2000.0) }),
            ..Default::default()
        };
        let back = read_exif(&write_exif(&m));
        assert_eq!(back.make, m.make);
        assert_eq!(back.model, m.model);
        assert_eq!(back.copyright, m.copyright);
        assert_eq!(back.artist, m.artist);
        assert_eq!(back.capture_time.map(|d| d.to_exif()), m.capture_time.map(|d| d.to_exif()));
        assert_eq!(back.iso, Some(400));
        assert!((back.f_number.unwrap() - 2.8).abs() < 1e-6);
        assert!((back.exposure_time.unwrap() - 0.004).abs() < 1e-9);
        assert_eq!(back.lens_model, m.lens_model);
        let g = back.gps.unwrap();
        assert!((g.latitude - 43.7904).abs() < 1e-5 && (g.longitude + 110.6818).abs() < 1e-5, "{g:?}");
    }

    #[test]
    fn iso_above_65534_round_trips() {
        for iso in [65_534, 65_535, 102_400, 409_600] {
            let back = read_exif(&write_exif(&Metadata { iso: Some(iso), ..Default::default() }));
            assert_eq!(back.iso, Some(iso));
        }
    }

    #[test]
    fn fast_shutter_speeds_round_trip_exactly() {
        for den in [8000.0, 12_800.0, 16_000.0, 32_000.0] {
            let back = read_exif(&write_exif(&Metadata { exposure_time: Some(1.0 / den), ..Default::default() }));
            let got = back.exposure_time.unwrap();
            assert!((1.0 / got - den).abs() < 0.5, "1/{den} s came back as 1/{}", 1.0 / got);
        }
    }
}
