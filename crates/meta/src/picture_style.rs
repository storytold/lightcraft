//! Picture style / Film Simulation tags used to select a camera colour profile.
//!
//! Today: Fujifilm FilmMode (maker-note `0x1401`). Values follow the public ExifTool FujiFilm tag
//! list; `0xA00` is Nostalgic Neg. on current X bodies (measured on X-E5).

use crate::{embedded, strip_exif_header};
use craft_tiff::{Tiff, makernote, tags as t};

/// Fujifilm maker-note FilmMode.
const FUJI_FILM_MODE: u16 = 0x1401;

/// Fujifilm FilmMode (or other picture-style) key for `bytes` (e.g. `PROVIA`, `VELVIA`), or `None`
/// when the tag is absent / unknown.
pub fn picture_look(bytes: &[u8]) -> Option<String> {
    fuji_film_look(bytes)
}

fn fuji_film_look(bytes: &[u8]) -> Option<String> {
    let emb = embedded(bytes);
    let exif = emb.exif.as_deref()?;
    let data = strip_exif_header(exif);
    let tiff = Tiff::parse(data).ok()?;
    let make = tiff.ifds.first().and_then(|i| i.string(t::MAKE)).unwrap_or_default();
    if !make.to_ascii_uppercase().contains("FUJI") {
        return None;
    }
    let exif_ifd = tiff.exif()?;
    let e = exif_ifd.get(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(data, e.offset, e.count() as u64, tiff.order, &make)?;
    let mode = mn.ifd.u16(FUJI_FILM_MODE)?;
    map_fuji_film_mode(mode)
}

fn map_fuji_film_mode(mode: u16) -> Option<String> {
    let key = match mode {
        0x0000 => "PROVIA", // F0 Standard
        0x0100 | 0x0110 | 0x0130 => "PORTRAIT",
        0x0120 => "ASTIA",
        0x0200 | 0x0400 => "VELVIA",
        0x0500 => "PRO_NEG_STD",
        0x0501 => "PRO_NEG_HI",
        0x0600 => "CLASSIC_CHROME",
        0x0700 => "ETERNA",
        0x0800 => "CLASSIC_NEG",
        0x0900 | 0x0A00 => "NOSTALGIC",
        0x0B00 => "REALA_ACE",
        _ => return None,
    };
    Some(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    fn jpeg_with_fuji_film_mode(mode: u16) -> Vec<u8> {
        // Fujifilm maker note: "FUJIFILM" + LE u32 IFD offset from note start (12 = after the header).
        let mut note = b"FUJIFILM".to_vec();
        ByteOrder::Little.put_u32(&mut note, 12);
        ByteOrder::Little.put_u16(&mut note, 1); // one entry
        ByteOrder::Little.put_u16(&mut note, FUJI_FILM_MODE);
        ByteOrder::Little.put_u16(&mut note, 3); // SHORT
        ByteOrder::Little.put_u32(&mut note, 1);
        ByteOrder::Little.put_u32(&mut note, u32::from(mode));
        ByteOrder::Little.put_u32(&mut note, 0); // next IFD

        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(note));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("FUJIFILM".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        let tiff = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn film_mode_values_map() {
        assert_eq!(map_fuji_film_mode(0).as_deref(), Some("PROVIA"));
        assert_eq!(map_fuji_film_mode(0x200).as_deref(), Some("VELVIA"));
        assert_eq!(map_fuji_film_mode(0xA00).as_deref(), Some("NOSTALGIC"));
        assert_eq!(map_fuji_film_mode(0x120).as_deref(), Some("ASTIA"));
        assert_eq!(map_fuji_film_mode(0x800).as_deref(), Some("CLASSIC_NEG"));
    }

    #[test]
    fn picture_look_reads_fuji_film_mode_from_jpeg() {
        assert_eq!(picture_look(&jpeg_with_fuji_film_mode(0)).as_deref(), Some("PROVIA"));
        assert_eq!(picture_look(&jpeg_with_fuji_film_mode(0x200)).as_deref(), Some("VELVIA"));
        assert_eq!(picture_look(&jpeg_with_fuji_film_mode(0xA00)).as_deref(), Some("NOSTALGIC"));
    }
}
