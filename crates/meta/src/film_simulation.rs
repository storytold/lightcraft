//! The film simulation a Fujifilm camera recorded in its maker note (`FUJIFILM` header, little-endian IFD, offsets
//! relative to the note): colour looks in FilmMode (0x1401), black-and-white ones in Saturation (0x1003), which
//! then takes FilmMode's place. Tag identities and value meanings come from the public ExifTool FujiFilm tag table;
//! the names are the ones the cameras' menus show. Unknown codes give no name rather than a guess.

use lightcraft_tiff::{ByteOrder, Ifd, makernote, tags as t};

const SATURATION: u16 = 0x1003;
const FILM_MODE: u16 = 0x1401;

/// The film simulation recorded in a Fujifilm Exif IFD's maker note, or `None` for another make, a note without
/// one, or a code this table doesn't know.
pub(crate) fn read(exif: Option<&Ifd>, make: Option<&str>) -> Option<String> {
    let make = make?;
    if !make.trim().to_ascii_uppercase().starts_with("FUJIFILM") {
        return None;
    }
    let note = exif?.bytes(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(note, 0, note.len() as u64, ByteOrder::Little, make)?;
    if mn.kind != makernote::MakerNoteKind::Fujifilm {
        return None;
    }
    let monochrome = mn.ifd.u16(SATURATION).and_then(monochrome_name);
    monochrome.or_else(|| mn.ifd.u16(FILM_MODE).and_then(colour_name)).map(str::to_string)
}

/// A FilmMode code's name.
fn colour_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x000 => "Provia/Standard",
        0x100 => "Studio Portrait",
        0x110 => "Studio Portrait Enhanced Saturation",
        0x120 => "Astia/Soft",
        0x130 => "Studio Portrait Increased Sharpness",
        // ExifTool "F2/Fujichrome (Velvia)": what X-S10 RAFs carry for Velvia; older bodies wrote 0x400 "F4/Velvia"
        0x200 => "Velvia/Vivid",
        0x300 => "Studio Portrait Ex",
        0x400 => "Velvia/Vivid",
        0x500 => "Pro Neg. Std",
        0x501 => "Pro Neg. Hi",
        0x600 => "Classic Chrome",
        0x700 => "Eterna/Cinema",
        0x800 => "Classic Neg.",
        0x900 => "Eterna Bleach Bypass",
        0xa00 => "Nostalgic Neg.",
        0xb00 => "Reala Ace",
        _ => return None,
    })
}

/// A Saturation code's name when it is a black-and-white simulation (its other codes are colour saturation levels).
fn monochrome_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x300 => "Monochrome",
        0x301 => "Monochrome + R Filter",
        0x302 => "Monochrome + Ye Filter",
        0x303 => "Monochrome + G Filter",
        0x310 => "Sepia",
        0x500 => "Acros",
        0x501 => "Acros + R Filter",
        0x502 => "Acros + Ye Filter",
        0x503 => "Acros + G Filter",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::read_exif;
    use lightcraft_tiff::{IfdBuilder, TiffWriter, Value};

    /// A Fujifilm maker note holding the given SHORT tags (values inline, so no offsets to lay out).
    fn fuji_note(tags: &[(u16, u16)]) -> Vec<u8> {
        let order = ByteOrder::Little;
        let mut note = b"FUJIFILM".to_vec();
        order.put_u32(&mut note, 12);
        order.put_u16(&mut note, tags.len() as u16);
        for &(tag, value) in tags {
            order.put_u16(&mut note, tag);
            order.put_u16(&mut note, 3);
            order.put_u32(&mut note, 1);
            order.put_u16(&mut note, value);
            order.put_u16(&mut note, 0);
        }
        order.put_u32(&mut note, 0);
        note
    }

    /// An Exif block as a Fujifilm camera writes it into a RAF's embedded JPEG (the enclosing TIFF is big-endian
    /// here to show the note keeps its own byte order).
    pub(crate) fn fuji_exif(make: &str, tags: &[(u16, u16)]) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(fuji_note(tags)));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii(make.into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Big, false).write(&[ifd0]).unwrap()
    }

    fn film(make: &str, tags: &[(u16, u16)]) -> Option<String> {
        read_exif(&fuji_exif(make, tags)).film_simulation
    }

    #[test]
    fn colour_simulations_come_from_film_mode() {
        assert_eq!(film("FUJIFILM", &[(SATURATION, 0x8000), (FILM_MODE, 0x600)]).as_deref(), Some("Classic Chrome"));
        assert_eq!(film("FUJIFILM", &[(FILM_MODE, 0x000)]).as_deref(), Some("Provia/Standard"));
        assert_eq!(film("FUJIFILM", &[(FILM_MODE, 0x200)]).as_deref(), Some("Velvia/Vivid"));
        assert_eq!(film("FUJIFILM", &[(SATURATION, 0x100), (FILM_MODE, 0xb00)]).as_deref(), Some("Reala Ace"));
    }

    #[test]
    fn black_and_white_simulations_come_from_saturation() {
        assert_eq!(film("FUJIFILM", &[(SATURATION, 0x501)]).as_deref(), Some("Acros + R Filter"));
        // a monochrome Saturation code wins over a FilmMode left in the note
        assert_eq!(film("FUJIFILM", &[(SATURATION, 0x310), (FILM_MODE, 0x000)]).as_deref(), Some("Sepia"));
    }

    #[test]
    fn unknown_codes_other_makes_and_missing_tags_give_nothing() {
        assert_eq!(film("FUJIFILM", &[(FILM_MODE, 0xc00)]), None);
        assert_eq!(film("FUJIFILM", &[(SATURATION, 0x8000)]), None);
        assert_eq!(film("FUJIFILM", &[]), None);
        assert_eq!(film("Canon", &[(FILM_MODE, 0x600)]), None);
    }

    #[test]
    fn a_truncated_note_gives_nothing() {
        let mut note = fuji_note(&[(FILM_MODE, 0x600)]);
        note.truncate(14);
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(note));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("FUJIFILM".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        assert_eq!(read_exif(&TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()).film_simulation, None);
    }
}
