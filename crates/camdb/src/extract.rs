//! A database entry from a camera-native DNG: a DNG the camera wrote itself (Leica, Pentax, Ricoh, Hasselblad,
//! phones…), whose `ColorMatrix1/2` and `ForwardMatrix1/2` are the maker's own calibration.
//!
//! DNGs written by a converter are refused: Adobe's (DNG Converter, Lightroom, Camera Raw, Photoshop), whose matrices
//! are Adobe's profiles; darktable, RawTherapee, dnglab, LibRaw, dcraw, DxO and Capture One, whose matrices come
//! from their own or third-party (often GPL) tables; and this app's own DNG export, whose matrices came from this database or a per-photo fit.
//! The decision rests on the `Software` tag; a DNG without one is refused too, as its origin can't be told. Per-unit
//! `CameraCalibration1/2` and the as-shot white are not copied: they describe one camera body and one photo.
use crate::{Entry, Error, Provenance, Source};

/// Words in a `Software` tag that mark a converter's DNG (compared without case).
const CONVERTERS: &[&str] = &["adobe", "lightroom", "camera raw", "dng converter", "photoshop"];

/// Other raw converters that write DNGs, as (word in the `Software` tag, name to report), compared without case
/// and ignoring spaces, hyphens and underscores. Their matrices come from their own or third-party (often GPL)
/// tables, not from the camera maker, so they must never enter the database as camera-native.
const THIRD_PARTY: &[(&str, &str)] = &[
    ("darktable", "darktable"),
    ("rawtherapee", "RawTherapee"),
    ("dnglab", "dnglab"),
    ("libraw", "LibRaw"),
    ("dcraw", "dcraw"),
    ("pureraw", "DxO PureRAW"),
    ("photolab", "DxO PhotoLab"),
    ("dxo", "DxO"),
    ("captureone", "Capture One"),
    ("phaseone", "Capture One"),
];

/// Why `software` marks a DNG written by a converter, if it does.
pub fn converter(software: &str) -> Option<String> {
    let lower = software.to_lowercase();
    if let Some(word) = CONVERTERS.iter().find(|w| lower.contains(*w)) {
        return Some(format!("written by Adobe software (\"{software}\" contains \"{word}\"); Adobe-converted DNGs are never a source"));
    }
    let squashed: String = lower.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).collect();
    if let Some((_, name)) = THIRD_PARTY.iter().find(|(w, _)| squashed.contains(*w)) {
        return Some(format!(
            "written by {name} (\"{software}\"); a converter's DNG carries the converter's matrices, not the maker's, \
             so use a DNG the camera wrote itself"
        ));
    }
    let own = lcc_brand::DISPLAY_NAME.to_lowercase();
    if !own.is_empty() && lower.contains(&own) {
        return Some(format!("written by this app's DNG export (\"{software}\"); its matrices are not a measurement"));
    }
    None
}

/// The entry a camera-native DNG supports, from its decoded header.
pub fn from_info(info: &lcc_raw::RawInfo) -> Result<Entry, Error> {
    if info.format != lcc_raw::RawFormat::Dng {
        return Err(Error(format!("not a DNG ({:?})", info.format)));
    }
    let software = info.metadata.software.as_deref().map(str::trim).unwrap_or_default();
    if software.is_empty() {
        return Err(Error("the DNG has no Software tag, so it can't be told from a converted DNG".into()));
    }
    if let Some(why) = converter(software) {
        return Err(Error(why));
    }
    let make = info.metadata.make.as_deref().map(str::trim).unwrap_or_default();
    let model = info.metadata.model.as_deref().map(str::trim).unwrap_or_default();
    let color = &info.color;
    let entry = Entry {
        make: make.into(),
        model: model.into(),
        aliases: vec![],
        illuminants: color.illuminant,
        color_matrix: color.color_matrix,
        forward_matrix: color.forward_matrix,
        wb_presets: vec![],
        black: None,
        white: None,
        provenance: Provenance { source: Source::NativeDng, detail: format!("Software: {software}"), contributor: None },
    };
    entry.validate()?;
    Ok(entry)
}

/// The entry a camera-native DNG file supports.
pub fn from_dng(bytes: &[u8]) -> Result<Entry, Error> {
    let info = lcc_raw::probe_info(bytes).map_err(|e| Error(format!("can't read the DNG: {e}")))?;
    from_info(&info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_raw::Mat3;

    fn info(software: Option<&str>) -> lcc_raw::RawInfo {
        let mut info = lcc_raw::probe_info(&dng()).unwrap();
        info.metadata.software = software.map(Into::into);
        info
    }

    /// A small DNG with dual-illuminant matrices, written by our own DNG writer.
    fn dng() -> Vec<u8> {
        let (w, h) = (8usize, 8usize);
        let raw = lcc_raw::RawImage {
            format: lcc_raw::RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: lcc_raw::RawData::U16((0..w * h).map(|i| (200 + i * 13) as u16).collect()),
            cfa: lcc_raw::Cfa::bayer("RGGB"),
            bits: 12,
            black: lcc_raw::BlackLevel::uniform(64.0),
            white: vec![4095.0],
            active_area: lcc_raw::Rect::new(0, 0, w, h),
            crop: lcc_raw::Rect::new(0, 0, w, h),
            orientation: lcc_raw::Orientation::Normal,
            color: lcc_raw::ColorData {
                illuminant: [17, 21],
                color_matrix: [
                    Some(Mat3([[1.2, -0.3, -0.1], [-0.4, 1.3, 0.1], [-0.05, 0.2, 0.6]])),
                    Some(Mat3([[0.8, -0.2, -0.05], [-0.4, 1.2, 0.2], [-0.1, 0.25, 0.55]])),
                ],
                ..Default::default()
            },
            wb_multipliers: None,
            linearized: false,
            opcodes: lcc_raw::OpcodeLists::default(),
            metadata: lcc_raw::Metadata { make: Some("Example".into()), model: Some("Example X1".into()), ..Default::default() },
            lens_metadata: Default::default(),
            pixel_aspect: 1.0,
        };
        lcc_raw::write_dng(&raw, &lcc_raw::DngWriteOptions::default()).unwrap()
    }

    #[test]
    fn a_camera_native_dng_gives_a_native_entry() {
        let e = from_info(&info(Some("Example X1 Ver.1.02"))).unwrap();
        assert_eq!((e.make.as_str(), e.model.as_str()), ("Example", "Example X1"));
        assert_eq!(e.illuminants, [17, 21]);
        assert!(e.color_matrix.iter().all(Option::is_some));
        assert_eq!(e.provenance.source, Source::NativeDng);
        assert_eq!(e.provenance.detail, "Software: Example X1 Ver.1.02");
        // and it survives the maker-file format
        let text = crate::to_toml(std::slice::from_ref(&e)).unwrap();
        assert_eq!(crate::parse_toml(&text).unwrap(), vec![e]);
    }

    #[test]
    fn converted_and_unlabelled_dngs_are_refused() {
        for sw in ["Adobe DNG Converter 16.5", "Adobe Lightroom Classic 13.0 (Macintosh)", "Camera Raw 16.0", "Adobe Photoshop 25.0"] {
            let err = from_info(&info(Some(sw))).unwrap_err();
            assert!(err.0.contains("Adobe"), "{sw}: {err}");
        }
        assert!(from_info(&info(None)).is_err());
        assert!(from_info(&info(Some("  "))).is_err());
        // our own DNG export writes the app's name: refused on the bytes path
        assert!(from_dng(&dng()).unwrap_err().0.contains("this app"));
    }

    #[test]
    fn third_party_converter_dngs_are_refused() {
        for (sw, name) in [
            ("darktable 4.6.1", "darktable"),
            ("RawTherapee 5.10", "RawTherapee"),
            ("Raw-Therapee", "RawTherapee"),
            ("dnglab 0.6.0", "dnglab"),
            ("DNGLab", "dnglab"),
            ("LibRaw 0.21.2", "LibRaw"),
            ("dcraw v9.28", "dcraw"),
            ("DxO PhotoLab 7", "DxO PhotoLab"),
            ("DxO PureRAW 4", "DxO PureRAW"),
            ("DxO", "DxO"),
            ("Capture One 23 Macintosh", "Capture One"),
            ("CaptureOne", "Capture One"),
            ("capture_one", "Capture One"),
            ("Phase One", "Capture One"),
        ] {
            let err = from_info(&info(Some(sw))).unwrap_err();
            assert!(err.0.contains(name), "{sw}: {err}");
        }
        // a camera's own firmware string is not mistaken for a converter
        assert!(converter("Ver.1.02").is_none());
        assert!(converter("LEICA Q3 1.2.0").is_none());
    }

    #[test]
    fn non_dngs_and_garbage_are_errors() {
        let mut i = info(Some("Example X1"));
        i.format = lcc_raw::RawFormat::Nef;
        assert!(from_info(&i).is_err());
        let mut i = info(Some("Example X1"));
        i.color.color_matrix = [None, None];
        assert!(from_info(&i).is_err());
        assert!(from_dng(b"not a dng").is_err());
        assert!(from_dng(&[]).is_err());
    }
}
