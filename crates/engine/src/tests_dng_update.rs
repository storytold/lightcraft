//! Metadata ▸ Update DNG Preview & Metadata (`photo.updateDngPreview`): the file is rewritten with a
//! preview and the photo's XMP and the raw stays what it was; files that must not be touched are left
//! byte for byte as they were.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;
use crate::tests_xmp::{synthetic_dng_with, temp_dir};

/// A session holding the one DNG `bytes` written to a scratch folder.
fn with_dng(tag: &str, bytes: &[u8]) -> (Session, lcc_testkit::TempDir, PathBuf) {
    let dir = temp_dir(tag);
    let file = dir.join("shot.dng");
    std::fs::write(&file, bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    (s, dir, file)
}

fn pixels(file: &[u8]) -> Vec<u16> {
    match lcc_raw::decode(file).unwrap().data {
        lcc_raw::RawData::U16(v) => v,
        other => panic!("unexpected data {other:?}"),
    }
}

fn xmp_of(file: &[u8]) -> String {
    String::from_utf8(lcc_raw::dngupdate::embedded_xmp(file).unwrap().expect("an embedded XMP packet")).unwrap()
}

#[test]
fn the_dng_gets_a_preview_and_the_current_xmp_merged_and_keeps_its_raw() {
    // (another writer's XMP is already in the file)
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:foo="http://example.com/foo/" foo:Keep="precious"/></rdf:RDF></x:xmpmeta>"#;
    let original = synthetic_dng_with(Some(xmp), lcc_meta::Metadata::default());
    let (mut s, dir, file) = with_dng("dng-update", &original);
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    // bad parameters are reported, and change nothing
    for bad in [json!({"longEdge": 10}), json!({"longEdge": "big"}), json!({"quality": 0}), json!({"quality": 101}), json!({"ids": ["x"]})] {
        assert!(s.execute("photo.updateDngPreview", &bad).is_err(), "{bad}");
    }
    assert_eq!(std::fs::read(&file).unwrap(), original, "a refused update leaves the file alone");
    let r = s.execute("photo.updateDngPreview", &json!({})).unwrap();
    assert_eq!(r["updated"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["updated"][0]["replacedPreviews"], 0);
    let after = std::fs::read(&file).unwrap();
    assert_eq!(pixels(&after), pixels(&original), "the raw decodes the same");
    assert!(xmp_of(&after).contains("Rating"), "the rating is in the embedded XMP");
    assert!(xmp_of(&after).contains("precious"), "the other writer's XMP in the file is kept: {}", xmp_of(&after));
    let tiff = lcc_tiff::Tiff::parse(&after).unwrap();
    let preview = tiff.ifds[0].sub_ifds.last().expect("a preview IFD");
    let (at, len) = (preview.u32(lcc_tiff::tags::STRIP_OFFSETS).unwrap() as usize, preview.u32(lcc_tiff::tags::STRIP_BYTE_COUNTS).unwrap() as usize);
    assert_eq!(after.get(at..at + 2), Some(&[0xFF, 0xD8][..]), "a JPEG preview");
    assert!(len > 100);
    // the update leaves no activity row behind, and a second one replaces the preview
    assert!(s.activity.list().is_empty());
    let again = s.execute("photo.updateDngPreview", &json!({"longEdge": 512})).unwrap();
    assert_eq!(again["updated"][0]["replacedPreviews"], 1, "{again}");
    let twice = std::fs::read(&file).unwrap();
    assert_eq!(lcc_tiff::Tiff::parse(&twice).unwrap().ifds[0].sub_ifds.len(), 1);
    assert_eq!(pixels(&twice), pixels(&original));
    let r: Value = s.execute("photo.updateDngPreview", &json!({"quality": 60})).unwrap();
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_it_must_not_touch_are_left_byte_for_byte() {
    // a photo that is not a DNG
    let mut demo = Session::with_demo();
    let e = demo.execute("photo.updateDngPreview", &json!({})).unwrap_err().to_string();
    assert!(e.contains("DNG"), "{e}");
    // Written by Adobe software
    let good = synthetic_dng_with(None, lcc_meta::Metadata::default());
    let sw = lcc_tiff::Tiff::parse(&good).unwrap().ifds[0].get(lcc_tiff::tags::SOFTWARE).unwrap().offset as usize;
    let mut adobe = good.clone();
    adobe[sw..sw + 5].copy_from_slice(b"Adobe");
    let (mut s, dir, file) = with_dng("dng-update-adobe", &adobe);
    let e = s.execute("photo.updateDngPreview", &json!({})).unwrap_err().to_string();
    assert!(e.contains("Adobe"), "{e}");
    assert_eq!(std::fs::read(&file).unwrap(), adobe);
    assert!(s.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    // an embedded XMP packet that cannot be read
    let (mut s, dir, file) = with_dng("dng-update-badxmp", &synthetic_dng_with(Some("<x:xmpmeta><unclosed"), lcc_meta::Metadata::default()));
    let before = std::fs::read(&file).unwrap();
    let e = s.execute("photo.updateDngPreview", &json!({})).unwrap_err().to_string();
    assert!(e.contains("XMP"), "{e}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);

    // a damaged file: an error, no panic, no change
    let mut cut = good.clone();
    cut.truncate(good.len() / 2);
    let (mut s, dir, file) = with_dng("dng-update-cut", &good);
    std::fs::write(&file, &cut).unwrap();
    assert!(s.execute("photo.updateDngPreview", &json!({})).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), cut);
    let _ = std::fs::remove_dir_all(&dir);
}
