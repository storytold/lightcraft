//! Fujifilm in-camera film recipe, read from the MakerNote.
//!
//! Tag numbers and value names follow the public FujiFilm table (ExifTool tag names,
//! `MakerNotes` / FujiFilm). No third-party metadata code was consulted. This does not
//! reproduce the camera JPEG or recover a custom recipe name: the file does not store one.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lcc_tiff::makernote::{MakerNoteKind, parse_makernote};
use lcc_tiff::{Ifd, Tiff};
use serde::{Deserialize, Serialize};

use crate::strip_exif_header;

/// One labelled recipe row, in display order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FujiRow {
    /// MakerNote tag this row came from (the first, when a row joins two tags).
    pub tag: u16,
    pub label: String,
    pub value: String,
}

/// Settings recovered from a Fujifilm MakerNote. Empty means the note had none of these tags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FujiRecipe {
    pub rows: Vec<FujiRow>,
}

impl FujiRecipe {
    /// Plain text for the clipboard, one `Label: value` line per row.
    pub fn text(&self) -> String {
        self.rows.iter().map(|r| format!("{}: {}", r.label, r.value)).collect::<Vec<_>>().join("\n")
    }

    fn push(&mut self, tag: u16, label: &str, value: String) {
        self.rows.push(FujiRow { tag, label: label.to_string(), value });
    }
}

/// Recipe in a file. A RAF contributes only its embedded JPEG (the header's offset and length);
/// a JPEG is read whole. `Ok(None)` when the file has no Fujifilm recipe.
pub fn read_recipe_file(path: &Path) -> Result<Option<FujiRecipe>, String> {
    let bytes = read_for_recipe(path)?;
    Ok(recipe_from_bytes(&bytes))
}

/// Recipe in a JPEG, RAF, or TIFF-structured EXIF block already in memory.
pub fn recipe_from_bytes(bytes: &[u8]) -> Option<FujiRecipe> {
    let exif = crate::embedded(bytes).exif?;
    recipe_from_exif(&exif)
}

/// Recipe in a TIFF-structured EXIF block (with or without an `Exif\0\0` prefix).
pub fn recipe_from_exif(bytes: &[u8]) -> Option<FujiRecipe> {
    let bytes = strip_exif_header(bytes);
    let tiff = Tiff::parse(bytes).ok()?;
    let ifd0 = tiff.ifds.first()?;
    let exif = ifd0.exif.as_ref()?;
    let entry = exif.get(0x927c)?;
    let make = ifd0.string(0x010f).unwrap_or_default();
    let note = parse_makernote(bytes, entry.offset, entry.count() as u64, tiff.order, &make)?;
    if note.kind != MakerNoteKind::Fujifilm {
        return None;
    }
    let recipe = decode(&note.ifd);
    (!recipe.rows.is_empty()).then_some(recipe)
}

fn read_for_recipe(path: &Path) -> Result<Vec<u8>, String> {
    let shown = path.display().to_string();
    let mut file = File::open(path).map_err(|e| format!("{shown}: {e}"))?;
    let mut head = [0u8; 92];
    let n = file.read(&mut head).map_err(|e| format!("{shown}: {e}"))?;
    if head.get(..n.min(15)).is_some_and(|h| h.starts_with(b"FUJIFILMCCD-RAW")) {
        if n < 92 {
            return Err(format!("{shown}: truncated RAF header"));
        }
        let off = u32::from_be_bytes([head[84], head[85], head[86], head[87]]) as u64;
        let len = u32::from_be_bytes([head[88], head[89], head[90], head[91]]) as u64;
        if off == 0 || len == 0 {
            return Ok(Vec::new());
        }
        const MAX_JPEG: u64 = 64 * 1024 * 1024;
        if len > MAX_JPEG {
            return Err(format!("{shown}: embedded JPEG is too large"));
        }
        let len = usize::try_from(len).map_err(|_| format!("{shown}: embedded JPEG is too large"))?;
        file.seek(SeekFrom::Start(off)).map_err(|e| format!("{shown}: {e}"))?;
        let mut jpeg = vec![0u8; len];
        file.read_exact(&mut jpeg).map_err(|e| format!("{shown}: {e}"))?;
        return Ok(jpeg);
    }
    drop(file);
    std::fs::read(path).map_err(|e| format!("{shown}: {e}"))
}

fn decode(ifd: &Ifd) -> FujiRecipe {
    let mut out = FujiRecipe::default();
    let film = ifd.u16(0x1401).map(film_mode);
    let filter = ifd.u16(0x1003).and_then(bw_filter);
    match (film, filter) {
        (Some(film), Some(filter)) => out.push(0x1401, "Film simulation", format!("{film}, {filter}")),
        (Some(film), None) => out.push(0x1401, "Film simulation", film),
        (None, Some(filter)) => out.push(0x1003, "Film simulation", filter.to_string()),
        (None, None) => {}
    }
    if let Some(dr) = ifd.u16(0x1402).map(dynamic_range_setting).or_else(|| ifd.u16(0x1400).map(dynamic_range_legacy)) {
        let tag = if ifd.contains(0x1402) { 0x1402 } else { 0x1400 };
        out.push(tag, "Dynamic range", dr);
    }
    if let Some(code) = ifd.u16(0x1002) {
        out.push(0x1002, "White balance", white_balance(code, ifd.i64(0x1005)));
    }
    if let Some(v) = ifd.value(0x100a)
        && let (Some(red), Some(blue)) = (v.get_i64(0), v.get_i64(1))
    {
        let (red, blue) = scale_shift(red, blue);
        out.push(0x100a, "White balance shift", format!("R {} / B {}", signed(red), signed(blue)));
    }
    let grain = match (ifd.i64(0x1047).map(grain_roughness), ifd.u16(0x104c).map(grain_size)) {
        (Some(rough), Some(size)) => Some((0x1047_u16, format!("{rough} / {size}"))),
        (Some(rough), None) => Some((0x1047, rough)),
        (None, Some(size)) => Some((0x104c, size)),
        (None, None) => None,
    };
    if let Some((tag, value)) = grain {
        out.push(tag, "Grain", value);
    }
    match (ifd.i64(0x1041).map(tone_dial), ifd.i64(0x1040).map(tone_dial)) {
        (Some(high), Some(shadow)) => out.push(0x1041, "Highlight / Shadow", format!("{high} / {shadow}")),
        (Some(high), None) => out.push(0x1041, "Highlight", high),
        (None, Some(shadow)) => out.push(0x1040, "Shadow", shadow),
        (None, None) => {}
    }
    if let Some(v) = ifd.i64(0x1048) {
        out.push(0x1048, "Color Chrome", chrome(v));
    }
    if let Some(v) = ifd.i64(0x104e) {
        out.push(0x104e, "Color Chrome FX Blue", chrome(v));
    }
    if let Some(v) = ifd.u16(0x1001) {
        out.push(0x1001, "Sharpness", sharpness(v));
    }
    if let Some(v) = ifd.u16(0x1003).filter(|v| bw_filter(*v).is_none()) {
        out.push(0x1003, "Color", color_step(v));
    }
    if let Some(v) = ifd.u16(0x100e).map(noise_reduction).or_else(|| ifd.u16(0x100b).map(noise_reduction_legacy)) {
        let tag = if ifd.contains(0x100e) { 0x100e } else { 0x100b };
        out.push(tag, "Noise reduction", v);
    }
    if let Some(v) = ifd.i64(0x100f) {
        out.push(0x100f, "Clarity", clarity(v));
    }
    out
}

fn named(v: u16, table: &[(u16, &str)]) -> String {
    table.iter().find(|(k, _)| *k == v).map(|(_, n)| (*n).to_string()).unwrap_or_else(|| format!("Unknown ({v})"))
}

fn film_mode(v: u16) -> String {
    named(
        v,
        &[
            (0x000, "F0/Standard (Provia)"),
            (0x100, "F1/Studio Portrait"),
            (0x110, "F1a/Studio Portrait Enhanced Saturation"),
            (0x120, "F1b/Studio Portrait Smooth Skin Tone (Astia)"),
            (0x130, "F1c/Studio Portrait Increased Sharpness"),
            (0x200, "F2/Fujichrome (Velvia)"),
            (0x300, "F3/Studio Portrait Ex"),
            (0x400, "F4/Velvia"),
            (0x500, "Pro Neg. Std"),
            (0x501, "Pro Neg. Hi"),
            (0x600, "Classic Chrome"),
            (0x700, "Eterna"),
            (0x800, "Classic Negative"),
            (0x900, "Bleach Bypass"),
            (0xa00, "Nostalgic Neg"),
            (0xb00, "Reala ACE"),
        ],
    )
}

fn bw_filter(v: u16) -> Option<&'static str> {
    Some(match v {
        0x300 => "B&W",
        0x301 => "B&W Red Filter",
        0x302 => "B&W Yellow Filter",
        0x303 => "B&W Green Filter",
        0x310 => "B&W Sepia",
        0x500 => "Acros",
        0x501 => "Acros Red Filter",
        0x502 => "Acros Yellow Filter",
        0x503 => "Acros Green Filter",
        _ => return None,
    })
}

fn dynamic_range_setting(v: u16) -> String {
    if v == 0x8000 {
        return "As film simulation".to_string();
    }
    named(v, &[(0x0, "Auto"), (0x1, "Manual"), (0x100, "DR100"), (0x200, "DR200"), (0x201, "DR400")])
}

fn dynamic_range_legacy(v: u16) -> String {
    named(v, &[(1, "DR100"), (3, "Wide")])
}

fn white_balance(code: u16, kelvin: Option<i64>) -> String {
    if code == 0xff0 {
        return match kelvin.filter(|k| *k > 0) {
            Some(k) => format!("Kelvin ({k} K)"),
            None => "Kelvin".to_string(),
        };
    }
    named(
        code,
        &[
            (0x0, "Auto"),
            (0x1, "Auto (white priority)"),
            (0x2, "Auto (ambiance priority)"),
            (0x100, "Daylight"),
            (0x200, "Cloudy"),
            (0x300, "Daylight Fluorescent"),
            (0x301, "Day White Fluorescent"),
            (0x302, "White Fluorescent"),
            (0x303, "Warm White Fluorescent"),
            (0x304, "Living Room Warm White Fluorescent"),
            (0x400, "Incandescent"),
            (0x500, "Flash"),
            (0x600, "Underwater"),
            (0xf00, "Custom"),
            (0xf01, "Custom 2"),
            (0xf02, "Custom 3"),
            (0xf03, "Custom 4"),
            (0xf04, "Custom 5"),
        ],
    )
}

/// Newer cameras store the shift times 20. Older cameras store about `-9..=9`. Divide only when
/// the pair is on the newer scale and both components are multiples of 20; otherwise keep the
/// stored pair so a mixed value is not shown as a wrong shift.
fn scale_shift(red: i64, blue: i64) -> (i64, i64) {
    let wide = red.unsigned_abs() > 9 || blue.unsigned_abs() > 9;
    if wide && red % 20 == 0 && blue % 20 == 0 { (red / 20, blue / 20) } else { (red, blue) }
}

/// Highlight and shadow use 16 stored units per dial step, with the opposite sign
/// (stored −16 is +1, stored 16 is −1). Half steps are multiples of 8, so stored 24 is −1.5.
/// Values that are not on that grid, or outside the camera's −4..=+4 dial, stay unknown.
fn tone_dial(v: i64) -> String {
    if v % 8 != 0 {
        return format!("Unknown ({v})");
    }
    let Some(half_steps) = v.checked_div(8).and_then(i64::checked_neg) else {
        return format!("Unknown ({v})");
    };
    if half_steps.unsigned_abs() > 8 {
        return format!("Unknown ({v})");
    }
    if half_steps % 2 == 0 {
        return signed(half_steps / 2);
    }
    let sign = if half_steps > 0 { "+" } else { "−" };
    format!("{sign}{}.5", half_steps.unsigned_abs() / 2)
}

fn grain_roughness(v: i64) -> String {
    match v {
        0 => "Off".to_string(),
        32 => "Weak".to_string(),
        64 => "Strong".to_string(),
        other => format!("Unknown ({other})"),
    }
}

fn grain_size(v: u16) -> String {
    match v {
        0 => "Off".to_string(),
        16 => "Small".to_string(),
        32 => "Large".to_string(),
        other => format!("Unknown ({other})"),
    }
}

fn chrome(v: i64) -> String {
    match v {
        0 => "Off".to_string(),
        32 => "Weak".to_string(),
        64 => "Strong".to_string(),
        other => format!("Unknown ({other})"),
    }
}

fn sharpness(v: u16) -> String {
    if v == 0x8000 {
        return "As film simulation".to_string();
    }
    match v {
        0x0 => signed(-4),
        0x1 => signed(-3),
        0x2 => signed(-2),
        0x3 => "0".to_string(),
        0x4 => "+2".to_string(),
        0x5 => "+3".to_string(),
        0x6 => "+4".to_string(),
        0x82 => signed(-1),
        0x84 => "+1".to_string(),
        0xffff => "n/a".to_string(),
        other => format!("Unknown ({other})"),
    }
}

fn color_step(v: u16) -> String {
    if v == 0x8000 {
        return "As film simulation".to_string();
    }
    match v {
        0x0 => "0".to_string(),
        0x80 => "+1".to_string(),
        0xc0 => "+3".to_string(),
        0xe0 => "+4".to_string(),
        0x100 => "+2".to_string(),
        0x180 => signed(-1),
        0x200 => "Low".to_string(),
        0x400 => signed(-2),
        0x4c0 => signed(-3),
        0x4e0 => signed(-4),
        other => format!("Unknown ({other})"),
    }
}

fn noise_reduction(v: u16) -> String {
    match v {
        0x0 => "0".to_string(),
        0x100 => "+2".to_string(),
        0x180 => "+1".to_string(),
        0x1c0 => "+3".to_string(),
        0x1e0 => "+4".to_string(),
        0x200 => signed(-2),
        0x280 => signed(-1),
        0x2c0 => signed(-3),
        0x2e0 => signed(-4),
        other => format!("Unknown ({other})"),
    }
}

fn noise_reduction_legacy(v: u16) -> String {
    named(v, &[(0x40, "Low"), (0x80, "Normal"), (0x100, "n/a")])
}

fn clarity(v: i64) -> String {
    if v % 1000 == 0 && (-5000..=5000).contains(&v) { signed(v / 1000) } else { format!("Unknown ({v})") }
}

fn signed(v: i64) -> String {
    if v > 0 {
        format!("+{v}")
    } else if v < 0 {
        format!("−{}", v.unsigned_abs())
    } else {
        "0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_tiff::tags;
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    fn inline(tag: u16, ty: u16, count: u32, val: [u8; 4]) -> [u8; 12] {
        let mut e = [0u8; 12];
        e[0..2].copy_from_slice(&tag.to_le_bytes());
        e[2..4].copy_from_slice(&ty.to_le_bytes());
        e[4..8].copy_from_slice(&count.to_le_bytes());
        e[8..12].copy_from_slice(&val);
        e
    }

    fn i32_inline(tag: u16, v: i32) -> [u8; 12] {
        inline(tag, 9, 1, v.to_le_bytes())
    }

    fn u16_inline(tag: u16, v: u16) -> [u8; 12] {
        let mut val = [0u8; 4];
        val[..2].copy_from_slice(&v.to_le_bytes());
        inline(tag, 3, 1, val)
    }

    /// A Fujifilm note: little-endian IFD at byte 12, the white-balance shift stored out of line.
    fn fuji_note(film: u16, shift: [i32; 2]) -> Vec<u8> {
        let mut entries = vec![
            inline(0x100a, 9, 2, [0, 0, 0, 0]),
            i32_inline(0x1040, -32),
            i32_inline(0x1041, -16),
            i32_inline(0x1047, 32),
            i32_inline(0x1048, 64),
            u16_inline(0x104c, 16),
            u16_inline(0x1401, film),
            u16_inline(0x1402, 0x200),
        ];
        let ifd_len = 2 + entries.len() * 12 + 4;
        let data_at = 12 + ifd_len;
        entries[0][8..12].copy_from_slice(&(data_at as u32).to_le_bytes());
        let mut note = b"FUJIFILM".to_vec();
        note.extend_from_slice(&12u32.to_le_bytes());
        note.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for e in &entries {
            note.extend_from_slice(e);
        }
        note.extend_from_slice(&0u32.to_le_bytes());
        note.extend_from_slice(&shift[0].to_le_bytes());
        note.extend_from_slice(&shift[1].to_le_bytes());
        note
    }

    fn exif_with_note(note: Vec<u8>) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(tags::MAKER_NOTE, Value::Undefined(note));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(tags::MAKE, Value::Ascii("FUJIFILM".into()));
        ifd0.set(tags::MODEL, Value::Ascii("X-S10".into()));
        ifd0.set_child(tags::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    fn jpeg_with_exif(exif: &[u8]) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(exif);
        let len = (payload.len() + 2) as u16;
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(&payload);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    fn sample_jpeg() -> Vec<u8> {
        jpeg_with_exif(&exif_with_note(fuji_note(0x600, [40, -80])))
    }

    fn sample_raf(tail: usize) -> Vec<u8> {
        let jpeg = sample_jpeg();
        let mut raf = b"FUJIFILMCCD-RAW ".to_vec();
        raf.resize(120, 0);
        raf[84..88].copy_from_slice(&120u32.to_be_bytes());
        raf[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
        raf.extend_from_slice(&jpeg);
        raf.extend(std::iter::repeat_n(0x33, tail));
        raf
    }

    fn value<'a>(recipe: &'a FujiRecipe, label: &str) -> &'a str {
        recipe.rows.iter().find(|r| r.label == label).map(|r| r.value.as_str()).unwrap_or("")
    }

    #[test]
    fn tone_half_steps() {
        assert_eq!(tone_dial(-16), "+1");
        assert_eq!(tone_dial(-32), "+2");
        assert_eq!(tone_dial(16), "−1");
        assert_eq!(tone_dial(24), "−1.5");
        assert_eq!(tone_dial(-8), "+0.5");
        assert_eq!(tone_dial(0), "0");
        assert_eq!(tone_dial(7), "Unknown (7)");
    }

    #[test]
    fn classic_chrome_recipe_from_jpeg_and_raf() {
        for bytes in [sample_jpeg(), sample_raf(64)] {
            let recipe = recipe_from_bytes(&bytes).unwrap();
            assert_eq!(value(&recipe, "Film simulation"), "Classic Chrome");
            assert_eq!(value(&recipe, "Dynamic range"), "DR200");
            // Public table: stored −16 is dial +1, stored −32 is dial +2.
            assert_eq!(value(&recipe, "Highlight / Shadow"), "+1 / +2");
            assert_eq!(value(&recipe, "Grain"), "Weak / Small");
            assert_eq!(value(&recipe, "Color Chrome"), "Strong");
            assert_eq!(value(&recipe, "White balance shift"), "R +2 / B −4");
            let text = recipe.text();
            assert!(text.contains("Film simulation: Classic Chrome"), "{text}");
            assert!(crate::file_tag_rows(&bytes).iter().any(|r| r.group == "Fujifilm" && r.value == "Classic Chrome"));
            assert!(crate::file_tag_rows(&bytes).iter().any(|r| r.name == "Maker Note"));
        }
        assert_eq!(crate::extract(&sample_jpeg()).fuji_recipe.as_ref().map(|r| value(r, "Dynamic range")), Some("DR200"));
    }

    #[test]
    fn old_shift_scale_is_not_divided() {
        let recipe = recipe_from_exif(&exif_with_note(fuji_note(0x600, [2, -4]))).unwrap();
        assert_eq!(value(&recipe, "White balance shift"), "R +2 / B −4");
    }

    #[test]
    fn unknown_film_code_is_labelled() {
        let recipe = recipe_from_exif(&exif_with_note(fuji_note(0x1234, [0, 0]))).unwrap();
        assert_eq!(value(&recipe, "Film simulation"), "Unknown (4660)");
    }

    #[test]
    fn acros_filter_is_appended() {
        let only = {
            let entry = u16_inline(0x1003, 0x501);
            let mut n = b"FUJIFILM".to_vec();
            n.extend_from_slice(&12u32.to_le_bytes());
            n.extend_from_slice(&1u16.to_le_bytes());
            n.extend_from_slice(&entry);
            n.extend_from_slice(&0u32.to_le_bytes());
            n
        };
        let recipe = recipe_from_exif(&exif_with_note(only)).unwrap();
        assert_eq!(value(&recipe, "Film simulation"), "Acros Red Filter");
    }

    #[test]
    fn truncated_and_non_fuji_notes_are_absent() {
        assert!(recipe_from_exif(&exif_with_note(b"FUJIFILM".to_vec())).is_none());
        let mut plain = Vec::new();
        ByteOrder::Little.put_u16(&mut plain, 1);
        ByteOrder::Little.put_u16(&mut plain, 1);
        ByteOrder::Little.put_u16(&mut plain, 3);
        ByteOrder::Little.put_u32(&mut plain, 1);
        ByteOrder::Little.put_u16(&mut plain, 7);
        ByteOrder::Little.put_u16(&mut plain, 0);
        ByteOrder::Little.put_u32(&mut plain, 0);
        assert!(recipe_from_exif(&exif_with_note(plain)).is_none());
        assert!(recipe_from_bytes(b"not an image").is_none());
    }

    #[test]
    fn read_recipe_file_uses_the_raf_jpeg_window() {
        let dir = std::env::temp_dir().join(format!("lc-fuji-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raf = dir.join("dscf0001.raf");
        let jpeg = dir.join("dscf0001.jpg");
        std::fs::write(&raf, sample_raf(200_000)).unwrap();
        std::fs::write(&jpeg, sample_jpeg()).unwrap();
        let from_raf = read_recipe_file(&raf).unwrap().unwrap();
        let from_jpeg = read_recipe_file(&jpeg).unwrap().unwrap();
        assert_eq!(from_raf, from_jpeg);
        assert_eq!(value(&from_raf, "Film simulation"), "Classic Chrome");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
