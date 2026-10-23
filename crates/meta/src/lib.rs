//! Photo metadata for LightCraft: EXIF (TIFF/raw files, JPEG APP1, PNG `eXIf`, WebP `EXIF`), XMP packets
//! (read + write, standard namespaces plus our own `lc:` namespace) and basic IPTC-IIM.
//!
//! Entry points:
//! - [`read_exif`] — a TIFF-structured Exif block → [`Metadata`]; [`from_tiff`] for an already parsed stream.
//! - [`embedded`] / [`jpeg_segments`] / [`png_chunks`] / [`webp_chunks`] — locate Exif / XMP / ICC / IPTC blocks.
//! - [`parse_xmp`] / [`write_xmp`] — XMP packets (interchange fields + the opaque `lc:settings` JSON).
//! - [`parse_iptc`] — IPTC-IIM record 2 datasets.
//! - [`parse_gpx`] — GPS track logs (GPX) for geotagging by capture time.
//! - [`extract`] — all of the above for a whole file (JPEG, PNG, WebP, TIFF/DNG/raw, Canon CR3).
//! - [`cr3`] — the Canon CR3 container (metadata blocks, XMP, the JPEG and raw tracks).
#![forbid(unsafe_code)]

mod container;
pub mod cr3;
mod datetime;
mod exif;
pub mod focus;
mod focus_read;
mod gpx;
mod iptc;
pub mod mrw;
pub mod tags;
mod xmp;
mod xmp_merge;

pub use container::{Embedded, embedded, jpeg_segments, png_chunks, webp_chunks};
pub use datetime::DateTime;
pub use exif::{from_tiff, read_exif, strip_exif_header, try_read_exif, write_exif};
pub use focus::FocusPoint;
pub use gpx::{GpxError, Match, TrackPoint, Tracklog, parse_gpx};
pub use iptc::parse_iptc;
pub use lightcraft_geom::{Orientation, Rect};
pub use tags::{TagRow, file_tag_rows, tag_rows};
pub use xmp::{CRS_NS, LC_NS, XmpData, XmpError, XmpValue, parse_xmp, write_xmp, write_xmp_lc};
pub use xmp_merge::{MergeRules, merge_xmp};

fn embedded_xmp_error(standard: Option<XmpData>, message: impl Into<String>) -> EmbeddedXmpError {
    EmbeddedXmpError { standard: standard.map(Box::new), message: message.into() }
}

fn standard_without_extended_reference(data: &XmpData) -> XmpData {
    let mut data = data.clone();
    data.properties.remove("xmpNote:HasExtendedXMP");
    data.values.remove("xmpNote:HasExtendedXMP");
    data
}

fn compose_jpeg_xmp(embedded: &Embedded, packets: &container::JpegXmpPackets) -> Result<(Option<XmpData>, Option<String>), EmbeddedXmpError> {
    let parsed_standard = embedded.xmp.as_deref().and_then(|packet| parse_xmp(packet).ok());
    let standard = parsed_standard.as_ref().map(standard_without_extended_reference);
    let Some(standard_data) = parsed_standard.as_ref() else {
        if packets.saw_extension {
            return Err(embedded_xmp_error(None, "Extended XMP is present but its standard packet cannot be parsed"));
        }
        return Ok((None, None));
    };
    let reference = xmp::extended_xmp_reference(standard_data).map_err(|error| embedded_xmp_error(standard.clone(), error))?;
    let Some(guid) = reference else {
        if packets.saw_extension {
            return Err(embedded_xmp_error(standard, "Extended XMP chunks have no namespace-qualified standard-packet reference"));
        }
        return Ok((standard, None));
    };
    let extended = packets.assemble(&guid).map_err(|error| embedded_xmp_error(standard.clone(), error))?;
    let standard_packet = embedded.xmp.as_deref().unwrap_or_default();
    let composed = xmp::parse_xmp_pair(standard_packet, &extended)
        .map_err(|error| embedded_xmp_error(standard.clone(), format!("referenced Extended XMP is invalid: {error}")))?;
    Ok((Some(composed), Some(extended)))
}

/// The JPEG container's checked XMP composition result, paired with the other embedded blocks.
pub(crate) fn jpeg_embedded(bytes: &[u8]) -> (Embedded, Result<Option<XmpData>, EmbeddedXmpError>) {
    let (mut embedded, packets) = container::jpeg_segments_with_packets(bytes);
    let parsed = compose_jpeg_xmp(&embedded, &packets);
    let result = match parsed {
        Ok((data, extension)) => {
            embedded.xmp_extended = extension;
            Ok(data)
        }
        Err(error) => Err(error),
    };
    (embedded, result)
}

fn compose_non_jpeg_xmp(embedded: &Embedded) -> Result<Option<XmpData>, EmbeddedXmpError> {
    let parsed_standard = embedded.xmp.as_deref().and_then(|packet| parse_xmp(packet).ok());
    let standard = parsed_standard.as_ref().map(standard_without_extended_reference);
    let Some(standard_data) = parsed_standard.as_ref() else {
        if embedded.xmp_extended.is_some() {
            return Err(embedded_xmp_error(None, "Extended XMP is present but its standard packet cannot be parsed"));
        }
        return Ok(None);
    };
    let reference = xmp::extended_xmp_reference(standard_data).map_err(|error| embedded_xmp_error(standard.clone(), error))?;
    match (reference, embedded.xmp_extended.as_deref()) {
        (Some(_), Some(extended)) => xmp::parse_xmp_pair(embedded.xmp.as_deref().unwrap_or_default(), extended)
            .map(Some)
            .map_err(|error| embedded_xmp_error(standard, format!("referenced Extended XMP is invalid: {error}"))),
        (Some(_), None) => Err(embedded_xmp_error(standard, "referenced Extended XMP is unavailable")),
        (None, Some(_)) => Err(embedded_xmp_error(standard, "Extended XMP has no namespace-qualified standard-packet reference")),
        (None, None) => Ok(standard),
    }
}

fn embedded_with_xmp(bytes: &[u8]) -> (Embedded, Result<Option<XmpData>, EmbeddedXmpError>) {
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg_embedded(bytes);
    }
    let embedded = embedded(bytes);
    let parsed = compose_non_jpeg_xmp(&embedded);
    (embedded, parsed)
}

/// Parse XMP embedded in a file, composing a referenced JPEG Extended XMP packet before projecting metadata.
/// On an invalid reference or extension, the error retains the parsed standard packet for best-effort reads.
pub fn parse_embedded_xmp(bytes: &[u8]) -> Result<Option<XmpData>, EmbeddedXmpError> {
    embedded_with_xmp(bytes).1
}

use serde::{Deserialize, Serialize};

/// An embedded Extended XMP reference or packet could not be inspected completely.
/// The standard packet remains available for best-effort reads.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedXmpError {
    standard: Option<Box<XmpData>>,
    message: String,
}

impl EmbeddedXmpError {
    /// Best-effort standard-packet properties parsed before the extension error.
    pub fn standard(&self) -> Option<&XmpData> {
        self.standard.as_deref()
    }
}

impl std::fmt::Display for EmbeddedXmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EmbeddedXmpError {}

/// GPS position (WGS 84). Latitude/longitude in signed decimal degrees (north / east positive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gps {
    pub latitude: f64,
    pub longitude: f64,
    /// Metres above sea level (negative below).
    pub altitude: Option<f64>,
}

/// Exif `Flash` value decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flash {
    pub fired: bool,
    /// The raw Exif `Flash` bit field.
    pub raw: u16,
}

/// A named region of interest on a photo (MWG Region Guidelines, `mwg-rs:Regions`): most often a face,
/// drawn by Lightroom, digiKam, Picasa or similar tools. Read-only for now — LightCraft does not write
/// regions yet (see `docs/xmp-interop.md`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// Normalized to the image's full, oriented frame (`Metadata::width`/`height`), y-down, 0..1 on
    /// both axes — MWG's own convention, and what `AppliedToDimensions` areas are converted to.
    pub rect: Rect,
    pub kind: RegionKind,
    /// `mwg-rs:Name` (e.g. the person's name).
    pub name: Option<String>,
    pub description: Option<String>,
}

/// `mwg-rs:Type`. Unrecognised values are kept verbatim rather than dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    Face,
    Pet,
    Focus,
    BarCode,
    /// `mwg-rs:Type` was missing, empty, or a value we don't recognise.
    Other(String),
}

/// Everything LightCraft shows or searches about a photo. All fields are optional; unknown = `None`/empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    // camera
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub software: Option<String>,
    // lens
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub lens_serial_number: Option<String>,
    /// Min/max focal length (mm) and min/max f-number at those, as in Exif `LensSpecification`.
    pub lens_spec: Option<[f64; 4]>,
    // capture
    pub capture_time: Option<DateTime>,
    /// Seconds.
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<f64>,
    pub flash: Option<Flash>,
    /// EV.
    pub exposure_bias: Option<f64>,
    /// Exif `ExposureProgram` code.
    pub exposure_program: Option<u16>,
    /// Exif `MeteringMode` code.
    pub metering_mode: Option<u16>,
    /// Exif `WhiteBalance` (0 auto, 1 manual).
    pub white_balance: Option<u16>,
    pub orientation: Option<Orientation>,
    pub gps: Option<Gps>,
    /// Pixel dimensions as recorded (not oriented).
    pub width: Option<u32>,
    pub height: Option<u32>,
    // descriptive / user
    pub artist: Option<String>,
    pub copyright: Option<String>,
    /// Copyright status (`xmpRights:Marked`): `true` = copyrighted, `false` = public domain, `None` = unknown.
    pub copyright_marked: Option<bool>,
    /// Rights usage terms (`xmpRights:UsageTerms`).
    pub usage_terms: Option<String>,
    /// Copyright info URL (`xmpRights:WebStatement`).
    pub copyright_url: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    /// Accessibility text (`Iptc4xmpCore:AltTextAccessibility`).
    pub alt_text: Option<String>,
    /// Long accessibility description (`Iptc4xmpCore:ExtDescrAccessibility`).
    pub extended_description: Option<String>,
    /// Place within the city (`Iptc4xmpCore:Location`).
    pub sublocation: Option<String>,
    /// `photoshop:City`, `photoshop:State`, `photoshop:Country`.
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub keywords: Vec<String>,
    /// Hierarchical keywords, `|`-separated paths (`lr:hierarchicalSubject` convention).
    pub hierarchical_keywords: Vec<String>,
    /// -1 = rejected, 0 = unrated, 1..=5 stars.
    pub rating: Option<i8>,
    /// Colour label name (e.g. "Red").
    pub label: Option<String>,
    /// Face/pet/focus/barcode regions (`mwg-rs:Regions`), read from XMP only.
    pub regions: Vec<Region>,
    /// Where the camera focused, from its maker notes ([`focus`]; Sony only for now), on the upright photo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<FocusPoint>,
}

macro_rules! fill {
    ($self:ident, $o:ident, $($f:ident),*) => { $( if $self.$f.is_none() { $self.$f = $o.$f.clone(); } )* };
}
macro_rules! overlay {
    ($self:ident, $o:ident, $($f:ident),*) => { $( if $o.$f.is_some() { $self.$f = $o.$f.clone(); } )* };
}

impl Metadata {
    /// Fill every empty field of `self` from `other`.
    pub fn fill_missing(&mut self, other: &Metadata) {
        fill!(
            self,
            other,
            make,
            model,
            serial_number,
            software,
            lens_make,
            lens_model,
            lens_serial_number,
            lens_spec,
            capture_time,
            exposure_time,
            f_number,
            iso,
            focal_length,
            focal_length_35mm,
            flash,
            exposure_bias,
            exposure_program,
            metering_mode,
            white_balance,
            orientation,
            gps,
            width,
            height,
            artist,
            copyright,
            copyright_marked,
            usage_terms,
            copyright_url,
            title,
            caption,
            alt_text,
            extended_description,
            sublocation,
            city,
            state,
            country,
            rating,
            label
        );
        if self.keywords.is_empty() {
            self.keywords = other.keywords.clone();
        }
        if self.hierarchical_keywords.is_empty() {
            self.hierarchical_keywords = other.hierarchical_keywords.clone();
        }
        if self.regions.is_empty() {
            self.regions = other.regions.clone();
        }
        if self.focus.is_none() {
            self.focus = other.focus.clone();
        }
    }

    /// Replace the user-editable fields (rating, label, title, caption, artist, copyright, keywords, GPS) with
    /// `other`'s where `other` has them — the XMP-over-EXIF precedence rule.
    pub fn overlay_user_fields(&mut self, other: &Metadata) {
        overlay!(self, other, rating, label, title, caption, artist, copyright, copyright_marked, usage_terms, copyright_url, gps);
        if !other.keywords.is_empty() {
            self.keywords = other.keywords.clone();
        }
        if !other.hierarchical_keywords.is_empty() {
            self.hierarchical_keywords = other.hierarchical_keywords.clone();
        }
        if !other.regions.is_empty() {
            self.regions = other.regions.clone();
        }
    }

    /// Exposure time formatted like cameras show it: `1/250`, `0.5`, `2″`.
    pub fn exposure_display(&self) -> Option<String> {
        let t = self.exposure_time?;
        if t.is_nan() || t <= 0.0 {
            return None;
        }
        Some(if t < 0.3 {
            format!("1/{}", (1.0 / t).round())
        } else if t < 1.0 {
            format!("{}", (t * 10.0).round() / 10.0)
        } else {
            format!("{}″", (t * 10.0).round() / 10.0)
        })
    }

    /// Human name of the metering mode.
    pub fn metering_name(&self) -> Option<&'static str> {
        Some(match self.metering_mode? {
            1 => "Average",
            2 => "Center-weighted average",
            3 => "Spot",
            4 => "Multi-spot",
            5 => "Pattern",
            6 => "Partial",
            255 => "Other",
            _ => "Unknown",
        })
    }

    /// Human name of the exposure program.
    pub fn exposure_program_name(&self) -> Option<&'static str> {
        Some(match self.exposure_program? {
            1 => "Manual",
            2 => "Program AE",
            3 => "Aperture-priority AE",
            4 => "Shutter speed priority AE",
            5 => "Creative (slow speed)",
            6 => "Action (high speed)",
            7 => "Portrait",
            8 => "Landscape",
            9 => "Bulb",
            _ => "Not defined",
        })
    }
}

/// Read everything we understand from a whole file (JPEG, PNG, WebP, TIFF-based incl. DNG/raw).
/// Precedence: EXIF, then IPTC fills gaps, then XMP overrides user fields and fills the rest.
pub fn extract(bytes: &[u8]) -> Metadata {
    let (e, parsed_xmp) = embedded_with_xmp(bytes);
    let mut m = e.exif.as_deref().map(read_exif).unwrap_or_default();
    if let Some(i) = e.iptc.as_deref() {
        m.fill_missing(&parse_iptc(i));
    }
    let xmp = match parsed_xmp {
        Ok(data) => data,
        Err(error) => error.standard().cloned(),
    };
    if let Some(x) = xmp {
        m.overlay_user_fields(&x.metadata);
        m.fill_missing(&x.metadata);
    }
    m
}

/// A lens name from its focal and aperture range (Exif LensSpecification, or a Nikon maker note's Lens), written the
/// way Lightroom shows a lens it has no name for: `50.0 mm f/1.8`, `18.0-135.0 mm f/3.5-5.6`.
pub fn lens_spec_name(spec: [f64; 4]) -> Option<String> {
    let [short, long, wide, narrow] = spec;
    if ![short, long, wide, narrow].iter().all(|v| v.is_finite() && *v > 0.0) || long < short {
        return None;
    }
    let focal = if (long - short).abs() < 0.05 { format!("{short:.1}") } else { format!("{short:.1}-{long:.1}") };
    let aperture = if (narrow - wide).abs() < 0.05 { format!("{wide:.1}") } else { format!("{wide:.1}-{narrow:.1}") };
    Some(format!("{focal} mm f/{aperture}"))
}

/// Lenient number parsing used by XMP/IPTC: `"28/10"`, `"2.8"`, `"+0.7"`.
pub(crate) fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some((n, d)) = s.split_once('/') {
        let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
        return (d != 0.0).then_some(n / d).filter(|v| v.is_finite());
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG_XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    const JPEG_XMP_EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";

    fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let len = u16::try_from(payload.len() + 2).expect("test APP1 payload fits u16");
        let mut segment = vec![0xff, marker];
        segment.extend_from_slice(&len.to_be_bytes());
        segment.extend_from_slice(payload);
        segment
    }

    fn xmp_jpeg(standard: &str, guid: &str, extension: &str, chunks: &[(u32, &[u8])]) -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8];
        let mut packet = JPEG_XMP_SIG.to_vec();
        packet.extend_from_slice(standard.as_bytes());
        jpeg.extend(jpeg_segment(0xe1, &packet));
        for (offset, data) in chunks {
            let mut payload = JPEG_XMP_EXT_SIG.to_vec();
            payload.extend_from_slice(guid.as_bytes());
            payload.extend_from_slice(&(extension.len() as u32).to_be_bytes());
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(data);
            jpeg.extend(jpeg_segment(0xe1, &payload));
        }
        jpeg
    }

    fn xmp_document(body: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" {body}></rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    fn referenced_standard(guid: &str, extra: &str) -> String {
        xmp_document(&format!(r#"xmlns:reference="http://ns.adobe.com/xmp/note/" reference:HasExtendedXMP="{guid}" {extra}"#))
    }

    fn split_chunks(packet: &str) -> [(u32, &[u8]); 2] {
        let split = packet.len() / 2;
        let (first, second) = packet.as_bytes().split_at(split);
        [(split as u32, second), (0, first)]
    }

    #[test]
    fn lens_names_from_a_spec_read_like_lightroom() {
        assert_eq!(lens_spec_name([50.0, 50.0, 1.8, 1.8]).as_deref(), Some("50.0 mm f/1.8"));
        assert_eq!(lens_spec_name([18.0, 135.0, 3.5, 5.6]).as_deref(), Some("18.0-135.0 mm f/3.5-5.6"));
        assert_eq!(lens_spec_name([24.0, 70.0, 2.8, 2.8]).as_deref(), Some("24.0-70.0 mm f/2.8"));
        assert_eq!(lens_spec_name([70.0, 210.0, 4.0, 5.6]).as_deref(), Some("70.0-210.0 mm f/4.0-5.6"));
        assert_eq!(lens_spec_name([0.0, 0.0, 0.0, 0.0]), None);
        assert_eq!(lens_spec_name([200.0, 80.0, 2.8, 2.8]), None);
        assert_eq!(lens_spec_name([f64::NAN, 50.0, 1.8, 1.8]), None);
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number("28/10"), Some(2.8));
        assert_eq!(parse_number(" +0.7 "), Some(0.7));
        assert_eq!(parse_number("1/0"), None);
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("inf"), None);
    }

    #[test]
    fn exposure_display() {
        let mut m = Metadata { exposure_time: Some(1.0 / 250.0), ..Default::default() };
        assert_eq!(m.exposure_display().as_deref(), Some("1/250"));
        m.exposure_time = Some(0.5);
        assert_eq!(m.exposure_display().as_deref(), Some("0.5"));
        m.exposure_time = Some(2.0);
        assert_eq!(m.exposure_display().as_deref(), Some("2″"));
        m.exposure_time = Some(0.0);
        assert_eq!(m.exposure_display(), None);
    }

    #[test]
    fn merge_rules() {
        let mut a = Metadata { make: Some("A".into()), rating: Some(1), ..Default::default() };
        let b = Metadata { make: Some("B".into()), model: Some("M".into()), rating: Some(4), keywords: vec!["k".into()], ..Default::default() };
        a.fill_missing(&b);
        assert_eq!(a.make.as_deref(), Some("A"));
        assert_eq!(a.model.as_deref(), Some("M"));
        assert_eq!(a.rating, Some(1));
        a.overlay_user_fields(&b);
        assert_eq!(a.rating, Some(4));
        assert_eq!(a.make.as_deref(), Some("A"));
        assert_eq!(a.keywords, vec!["k".to_string()]);
    }

    #[test]
    fn names() {
        let m = Metadata { metering_mode: Some(5), exposure_program: Some(3), ..Default::default() };
        assert_eq!(m.metering_name(), Some("Pattern"));
        assert_eq!(m.exposure_program_name(), Some("Aperture-priority AE"));
    }

    #[test]
    fn extended_xmp_gps_reaches_extractor() {
        let guid = "A1234567890123456789012345678901";
        let extension = xmp_document(r#"xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:GPSLatitude="45,0N" exif:GPSLongitude="7,0E""#);
        let standard = referenced_standard(guid, "");
        let jpeg = xmp_jpeg(&standard, guid, &extension, &split_chunks(&extension));
        let gps = extract(&jpeg).gps.expect("the extension GPS reaches extraction");
        assert_eq!((gps.latitude, gps.longitude), (45.0, 7.0));
    }

    #[test]
    fn extended_xmp_caption_reaches_extractor() {
        let guid = "B1234567890123456789012345678901";
        let caption = "multi-chunk caption ".repeat(160);
        let extension = xmp_document(&format!(r#"xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:description>{caption}</dc:description>"#));
        let standard = referenced_standard(guid, "");
        let jpeg = xmp_jpeg(&standard, guid, &extension, &split_chunks(&extension));
        assert_eq!(extract(&jpeg).caption.as_deref(), Some(caption.trim()));
    }

    #[test]
    fn extended_xmp_split_gps_properties_are_combined() {
        let guid = "C1234567890123456789012345678901";
        let standard = xmp_document(&format!(
            r#"xmlns:note="http://ns.adobe.com/xmp/note/" xmlns:exif="http://ns.adobe.com/exif/1.0/" note:HasExtendedXMP="{guid}" exif:GPSLatitude="45,0N""#
        ));
        let extension = xmp_document(r#"xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:GPSLongitude="7,0E""#);
        let jpeg = xmp_jpeg(&standard, guid, &extension, &split_chunks(&extension));
        let gps = extract(&jpeg).gps.expect("GPS properties across packets compose");
        assert_eq!((gps.latitude, gps.longitude), (45.0, 7.0));
    }

    #[test]
    fn extended_xmp_structured_values_are_composed() {
        let guid = "F1234567890123456789012345678901";
        let standard = xmp_document(&format!(
            r#"xmlns:note="http://ns.adobe.com/xmp/note/" xmlns:exif="http://ns.adobe.com/exif/1.0/" note:HasExtendedXMP="{guid}"><exif:Flash rdf:parseType="Resource"><exif:Fired>True</exif:Fired></exif:Flash>"#
        ));
        let extension =
            xmp_document(r#"xmlns:exif="http://ns.adobe.com/exif/1.0/"><exif:Flash rdf:parseType="Resource"><exif:Mode>2</exif:Mode></exif:Flash>"#);
        let jpeg = xmp_jpeg(&standard, guid, &extension, &split_chunks(&extension));
        assert_eq!(extract(&jpeg).flash.map(|flash| flash.raw), Some(17));
    }

    #[test]
    fn extended_xmp_rejects_mismatched_guid_and_bad_coverage() {
        let guid = "D1234567890123456789012345678901";
        let extension = xmp_document(r#"xmlns:dc="http://purl.org/dc/elements/1.1/" dc:title="Extension title""#);
        let standard = referenced_standard(guid, r#"xmlns:dc="http://purl.org/dc/elements/1.1/" dc:title="Standard title""#);
        let bytes = extension.as_bytes();
        let middle = bytes.len() / 2;
        let inconsistent_lengths = {
            let mut jpeg = vec![0xff, 0xd8];
            let mut standard_payload = JPEG_XMP_SIG.to_vec();
            standard_payload.extend_from_slice(standard.as_bytes());
            jpeg.extend(jpeg_segment(0xe1, &standard_payload));
            for (offset, declared, part) in
                [(0u32, bytes.len() as u32, &bytes[..middle]), (middle as u32, (bytes.len() + 1) as u32, &bytes[middle..])]
            {
                let mut payload = JPEG_XMP_EXT_SIG.to_vec();
                payload.extend_from_slice(guid.as_bytes());
                payload.extend_from_slice(&declared.to_be_bytes());
                payload.extend_from_slice(&offset.to_be_bytes());
                payload.extend_from_slice(part);
                jpeg.extend(jpeg_segment(0xe1, &payload));
            }
            jpeg
        };
        let oversized_length = {
            let mut jpeg = vec![0xff, 0xd8];
            let mut standard_payload = JPEG_XMP_SIG.to_vec();
            standard_payload.extend_from_slice(standard.as_bytes());
            jpeg.extend(jpeg_segment(0xe1, &standard_payload));
            let mut payload = JPEG_XMP_EXT_SIG.to_vec();
            payload.extend_from_slice(guid.as_bytes());
            payload.extend_from_slice(&(16u32 * 1024 * 1024 + 1).to_be_bytes());
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(bytes);
            jpeg.extend(jpeg_segment(0xe1, &payload));
            jpeg
        };
        let foreign_namespace_reference = {
            let foreign_standard = xmp_document(&format!(
                r#"xmlns:note="urn:example:foreign-note" note:HasExtendedXMP="{guid}" xmlns:dc="http://purl.org/dc/elements/1.1/" dc:title="Standard title""#
            ));
            xmp_jpeg(&foreign_standard, guid, &extension, &split_chunks(&extension))
        };
        let mut truncated_segment = xmp_jpeg(&standard, guid, &extension, &split_chunks(&extension));
        truncated_segment.pop();

        let cases = [
            xmp_jpeg(&standard, "E1234567890123456789012345678901", &extension, &split_chunks(&extension)),
            xmp_jpeg(&standard, guid, &extension, &[(0, &bytes[..middle]), ((middle + 1) as u32, &bytes[middle..])]),
            xmp_jpeg(&standard, guid, &extension, &[(0, &bytes[..middle]), ((middle - 1) as u32, &bytes[middle..])]),
            xmp_jpeg(&standard, guid, &extension, &[(0, bytes), (0, bytes)]),
            xmp_jpeg(&standard, guid, &extension, &[(0, &bytes[..middle])]),
            inconsistent_lengths,
            oversized_length,
            truncated_segment,
            foreign_namespace_reference,
        ];
        for (index, jpeg) in cases.iter().enumerate() {
            assert!(embedded(jpeg).xmp_extended.is_none(), "malformed extension case {index} was exposed");
            assert_eq!(extract(jpeg).title.as_deref(), Some("Standard title"), "standard XMP remains readable");
            let error = parse_embedded_xmp(jpeg).expect_err("invalid referenced extension is reported");
            assert_eq!(error.standard().and_then(|xmp| xmp.metadata.title.as_deref()), Some("Standard title"));
        }
    }

    #[test]
    fn standard_only_jpeg_xmp_still_extracts() {
        let standard = xmp_document(r#"xmlns:dc="http://purl.org/dc/elements/1.1/" dc:title="Standard-only title""#);
        let mut jpeg = vec![0xff, 0xd8];
        let mut packet = JPEG_XMP_SIG.to_vec();
        packet.extend_from_slice(standard.as_bytes());
        jpeg.extend(jpeg_segment(0xe1, &packet));
        assert_eq!(extract(&jpeg).title.as_deref(), Some("Standard-only title"));
    }
}
