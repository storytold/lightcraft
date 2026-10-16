//! `photo.fujiRecipe` reads a Fujifilm MakerNote from a JPEG, or from a RAF's embedded JPEG only.

use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_tiff::tags;
use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};
use serde_json::json;

use crate::Session;

fn inline(tag: u16, ty: u16, count: u32, val: [u8; 4]) -> [u8; 12] {
    let mut e = [0u8; 12];
    e[0..2].copy_from_slice(&tag.to_le_bytes());
    e[2..4].copy_from_slice(&ty.to_le_bytes());
    e[4..8].copy_from_slice(&count.to_le_bytes());
    e[8..12].copy_from_slice(&val);
    e
}

fn u16_inline(tag: u16, v: u16) -> [u8; 12] {
    let mut val = [0u8; 4];
    val[..2].copy_from_slice(&v.to_le_bytes());
    inline(tag, 3, 1, val)
}

fn i32_inline(tag: u16, v: i32) -> [u8; 12] {
    inline(tag, 9, 1, v.to_le_bytes())
}

fn sample_raf() -> Vec<u8> {
    let mut entries = vec![
        inline(0x100a, 9, 2, [0, 0, 0, 0]),
        i32_inline(0x1040, -32),
        i32_inline(0x1041, -16),
        i32_inline(0x1047, 32),
        i32_inline(0x1048, 64),
        u16_inline(0x104c, 16),
        u16_inline(0x1401, 0x600),
        u16_inline(0x1402, 0x200),
    ];
    let data_at = 12 + 2 + entries.len() * 12 + 4;
    entries[0][8..12].copy_from_slice(&(data_at as u32).to_le_bytes());
    let mut note = b"FUJIFILM".to_vec();
    note.extend_from_slice(&12u32.to_le_bytes());
    note.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in &entries {
        note.extend_from_slice(e);
    }
    note.extend_from_slice(&0u32.to_le_bytes());
    note.extend_from_slice(&40i32.to_le_bytes());
    note.extend_from_slice(&(-80i32).to_le_bytes());

    let mut exif = IfdBuilder::new();
    exif.set(tags::MAKER_NOTE, Value::Undefined(note));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(tags::MAKE, Value::Ascii("FUJIFILM".into()));
    ifd0.set_child(tags::EXIF_IFD, exif);
    let tiff = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let len = (payload.len() + 2) as u16;
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
    jpeg.extend_from_slice(&len.to_be_bytes());
    jpeg.extend_from_slice(&payload);
    jpeg.extend_from_slice(&[0xff, 0xd9]);

    let mut raf = b"FUJIFILMCCD-RAW ".to_vec();
    raf.resize(120, 0);
    raf[84..88].copy_from_slice(&120u32.to_be_bytes());
    raf[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
    raf.extend_from_slice(&jpeg);
    raf.extend(std::iter::repeat_n(0x33, 200_000));
    raf
}

fn add(s: &mut Session, path: &str, name: &str) -> u64 {
    let id = s.catalog.alloc_photo_id();
    s.catalog
        .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: path.to_string() }, name, "RAF", 100, 100, "2026-10-12")) })
        .unwrap();
    id.0
}

#[test]
fn fuji_recipe_reads_a_raf_without_importing_the_sensor() {
    let dir = std::env::temp_dir().join(format!("lc-fuji-cmd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let raf = dir.join("dscf0001.raf");
    std::fs::write(&raf, sample_raf()).unwrap();
    let plain = dir.join("note.txt");
    std::fs::write(&plain, b"hello").unwrap();

    let mut s = Session::new().with_fs();
    let id = add(&mut s, &raf.to_string_lossy(), "dscf0001.raf");
    let r = s.execute("photo.fujiRecipe", &json!({"id": id})).unwrap();
    assert_eq!(r["present"], true, "{r}");
    assert_eq!(r["text"].as_str().unwrap().lines().next(), Some("Film simulation: Classic Chrome"));
    assert!(r["text"].as_str().unwrap().contains("Dynamic range: DR200"), "{r}");
    assert!(r["text"].as_str().unwrap().contains("Highlight / Shadow: +1 / +2"), "{r}");
    assert!(r["text"].as_str().unwrap().contains("White balance shift: R +2 / B −4"), "{r}");

    let other = add(&mut s, &plain.to_string_lossy(), "note.txt");
    assert_eq!(s.execute("photo.fujiRecipe", &json!({"id": other})).unwrap()["present"], false);

    let demo = s.catalog.alloc_photo_id();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(Photo::new(demo, Source::Demo { scene: 1 }, "demo", "JPEG", 1, 1, "t")) }).unwrap();
    assert_eq!(s.execute("photo.fujiRecipe", &json!({"id": demo.0})).unwrap()["present"], false);

    let _ = std::fs::remove_dir_all(&dir);
}
