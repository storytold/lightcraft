//! Fujifilm maker-note values a render needs. Tag numbers follow the public ExifTool FujiFilm table.

use craft_tiff::{Tiff, makernote, tags};

/// `DynamicRangeSetting`: 0 Auto, 1 Manual (then [`DEVELOPMENT_DYNAMIC_RANGE`] holds the percentage).
const DYNAMIC_RANGE_SETTING: u16 = 0x1402;
/// `DevelopmentDynamicRange`: the percentage set by hand.
const DEVELOPMENT_DYNAMIC_RANGE: u16 = 0x1403;
/// `AutoDynamicRange`: the percentage Auto DR chose.
const AUTO_DYNAMIC_RANGE: u16 = 0x140b;

/// The dynamic range (100, 200 or 400 %) a Fujifilm raw was developed at, from the maker note of the Exif it carries
/// (a RAF's is in its embedded JPEG, [`crate::embedded`]): the percentage set by hand, else the one Auto DR chose. At
/// DR200 and DR400 the camera exposes the sensor one or two stops darker and brightens its rendering back. `None` for
/// other files, or when the note doesn't say.
pub fn fujifilm_dynamic_range(bytes: &[u8]) -> Option<u32> {
    let exif = crate::embedded(bytes).exif?;
    let data = crate::strip_exif_header(&exif);
    let tiff = Tiff::parse(data).ok()?;
    let make = tiff.ifds.first()?.string(tags::MAKE)?;
    let entry = tiff.exif()?.get(tags::MAKER_NOTE)?;
    let note = makernote::parse_makernote(data, entry.offset, u64::try_from(entry.count()).ok()?, tiff.order, &make)?;
    let tag = if note.ifd.u16(DYNAMIC_RANGE_SETTING) == Some(1) { DEVELOPMENT_DYNAMIC_RANGE } else { AUTO_DYNAMIC_RANGE };
    note.ifd.u16(tag).map(u32::from).filter(|dr| matches!(dr, 100 | 200 | 400))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian IFD entry with its value (or the value's offset) in the last four bytes.
    fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
        [tag.to_le_bytes().as_slice(), &kind.to_le_bytes(), &count.to_le_bytes(), &value.to_le_bytes()].concat()
    }

    /// A RAF whose embedded JPEG's Exif carries a Fujifilm maker note with `values` as SHORT tags.
    fn raf(values: &[(u16, u16)]) -> Vec<u8> {
        // the note: `FUJIFILM`, its IFD's offset from the note's start, then the IFD (always little-endian)
        let mut note = b"FUJIFILM".to_vec();
        note.extend(12u32.to_le_bytes());
        note.extend(u16::try_from(values.len()).unwrap().to_le_bytes());
        for &(tag, value) in values {
            note.extend(entry(tag, 3, 1, u32::from(value)));
        }
        note.extend(0u32.to_le_bytes());
        // the Exif TIFF: IFD0 (Make and the Exif IFD), the Exif IFD (the maker note), then their data
        let make = b"FUJIFILM\0";
        let (ifd0, exif_ifd) = (8u32, 8 + 30);
        let make_at = exif_ifd + 18;
        let note_at = make_at + u32::try_from(make.len()).unwrap();
        let mut tiff = b"II*\0".to_vec();
        tiff.extend(ifd0.to_le_bytes());
        tiff.extend(2u16.to_le_bytes());
        tiff.extend(entry(tags::MAKE, 2, u32::try_from(make.len()).unwrap(), make_at));
        tiff.extend(entry(0x8769, 4, 1, exif_ifd));
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(1u16.to_le_bytes());
        tiff.extend(entry(tags::MAKER_NOTE, 7, u32::try_from(note.len()).unwrap(), note_at));
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(make);
        tiff.extend(&note);
        // the JPEG: SOI, the Exif APP1 segment, EOI
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(u16::try_from(tiff.len() + 8).unwrap().to_be_bytes());
        jpeg.extend(b"Exif\0\0");
        jpeg.extend(&tiff);
        jpeg.extend([0xff, 0xd9]);
        // the RAF: its magic, and the JPEG's offset and length (big-endian) at byte 84
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(84, 0);
        raf.extend(100u32.to_be_bytes());
        raf.extend(u32::try_from(jpeg.len()).unwrap().to_be_bytes());
        raf.resize(100, 0);
        raf.extend(jpeg);
        raf
    }

    /// Auto DR gives the range it chose; a range set by hand gives that one; anything else is unknown.
    #[test]
    fn the_dynamic_range_comes_from_the_maker_note() {
        assert_eq!(fujifilm_dynamic_range(&raf(&[(DYNAMIC_RANGE_SETTING, 0), (AUTO_DYNAMIC_RANGE, 200)])), Some(200));
        assert_eq!(fujifilm_dynamic_range(&raf(&[(AUTO_DYNAMIC_RANGE, 100)])), Some(100), "no setting tag: Auto");
        let manual = [(DYNAMIC_RANGE_SETTING, 1), (DEVELOPMENT_DYNAMIC_RANGE, 400), (AUTO_DYNAMIC_RANGE, 200)];
        assert_eq!(fujifilm_dynamic_range(&raf(&manual)), Some(400), "set by hand");
        assert_eq!(fujifilm_dynamic_range(&raf(&[(DYNAMIC_RANGE_SETTING, 0)])), None, "Auto without its choice");
        assert_eq!(fujifilm_dynamic_range(&raf(&[(AUTO_DYNAMIC_RANGE, 230)])), None, "not a camera setting");
        assert_eq!(fujifilm_dynamic_range(b"not a raw"), None);
    }

    /// A RAF cut short anywhere is no dynamic range, never a panic.
    #[test]
    fn a_truncated_raf_gives_none() {
        let whole = raf(&[(AUTO_DYNAMIC_RANGE, 200)]);
        for end in 0..whole.len() {
            let _ = fujifilm_dynamic_range(&whole[..end]);
        }
        assert_eq!(fujifilm_dynamic_range(&whole[..100]), None, "the header without its JPEG");
    }
}
