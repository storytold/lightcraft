use crate::convert::{from_craft, to_craft};
use crate::{Metadata, Orientation};
use craft_tiff::{ParseOptions, Tiff, TiffError, tags as t};

pub fn strip_exif_header(b: &[u8]) -> &[u8] {
    craft_metadata::strip_exif_header(b)
}

pub fn read_exif(bytes: &[u8]) -> Metadata {
    from_craft(craft_metadata::read_exif(bytes))
}

pub fn try_read_exif(bytes: &[u8]) -> Result<Metadata, TiffError> {
    craft_metadata::try_read_exif(bytes).map(from_craft)
}

pub fn try_read_exif_with(bytes: &[u8], opts: &ParseOptions) -> Result<Metadata, TiffError> {
    craft_metadata::try_read_exif_with(bytes, opts).map(from_craft)
}

pub fn from_tiff(tiff: &Tiff) -> Metadata {
    from_craft(craft_metadata::from_tiff(tiff))
}

pub fn write_exif(meta: &Metadata) -> Vec<u8> {
    craft_metadata::write_exif(&to_craft(meta))
}

pub(crate) fn ifd0_orientation(bytes: &[u8]) -> Option<Orientation> {
    let tiff = Tiff::parse(strip_exif_header(bytes)).ok()?;
    tiff.ifds.first()?.u16(t::ORIENTATION).map(Orientation::from_exif)
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

    #[test]
    fn written_shutter_speeds_show_as_the_same_reciprocal() {
        // #1619 / #2542: the exact time is what export writes, and it reads back as the text shown
        for (t, want) in [(1.0 / 3.0, "1/3"), (1.0 / 250.0, "1/250"), (1.0 / 12_800.0, "1/12800"), (0.3, "0.3"), (1.0 / 2.8, "0.3571")] {
            let back = read_exif(&write_exif(&Metadata { exposure_time: Some(t), ..Default::default() }));
            let got = back.exposure_time.unwrap();
            assert_eq!(crate::format_exposure_time(got).as_deref(), Some(want), "{t} came back as {got}");
        }
    }
}
