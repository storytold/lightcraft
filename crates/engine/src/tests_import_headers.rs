use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::Session;

fn big_dng(seed: u32) -> Vec<u8> {
    let (w, h) = (2000usize, 1500usize);
    let mut x = 0x9e37_79b9u32 ^ seed;
    let data: Vec<u16> = (0..w * h)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            256 + (x % 15000) as u16
        })
        .collect();
    let meta = lightcraft_meta::Metadata { make: Some("Example".into()), model: Some("Camera".into()), ..Default::default() };
    crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, meta)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

struct Counted {
    whole: Arc<AtomicUsize>,
    headers: Arc<AtomicUsize>,
}

fn counted_session() -> (Session, Counted) {
    let mut s = Session::new().with_fs();
    let (whole, headers) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (file_probe, header_probe) = (s.media.file_probe.clone().unwrap(), s.media.header_probe.clone().unwrap());
    let w = whole.clone();
    s.media.file_probe = Some(Arc::new(move |path: &str| {
        w.fetch_add(1, Ordering::Relaxed);
        file_probe(path)
    }));
    let h = headers.clone();
    s.media.header_probe = Some(Arc::new(move |path: &str| {
        h.fetch_add(1, Ordering::Relaxed);
        header_probe(path)
    }));
    (s, Counted { whole, headers })
}

fn reads(c: &Counted) -> (usize, usize) {
    (c.headers.swap(0, Ordering::Relaxed), c.whole.swap(0, Ordering::Relaxed))
}

fn import(s: &mut Session, paths: &[&Path], mode: &str, dest: &Path) -> Value {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": paths, "mode": mode, "destination": dest.to_string_lossy(), "organize": "flat"})).unwrap()
}

fn hash_of(s: &Session, id: &Value) -> Option<String> {
    s.catalog.photo(lightcraft_catalog::PhotoId(id.as_u64().unwrap())).and_then(|p| p.content_hash.clone())
}

#[test]
fn library_import_add_copy_move_read_raws_from_headers() {
    let base = crate::tests_xmp::temp_dir("import-headers-modes");
    let (s_dir, dest) = (base.join("card"), base.join("out"));
    let (mut s, counted) = counted_session();
    let added = write(&s_dir, "added.dng", &big_dng(1));
    let r = import(&mut s, &[&added], "add", &dest);
    assert_eq!(reads(&counted), (1, 0), "Add reads the raw's headers: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), None, "a raw described from its headers has no hash");
    let copied_bytes = big_dng(2);
    let copied = write(&s_dir, "copied.dng", &copied_bytes);
    let r = import(&mut s, &[&copied], "copy", &dest);
    assert_eq!(reads(&counted), (1, 0), "Copy too: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), Some(lightcraft_preview::hash_bytes(&copied_bytes).to_string()), "the copy hashed it");
    let moved = write(&s_dir, "moved.dng", &big_dng(3));
    let r = import(&mut s, &[&moved], "move", &dest);
    assert_eq!(reads(&counted), (1, 0), "Move too: {r}");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!moved.exists() && dest.join("moved.dng").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn import_preview_marks_a_hashless_copy_as_a_duplicate() {
    let base = crate::tests_xmp::temp_dir("import-headers-preview");
    let bytes = big_dng(4);
    let orig = write(&base.join("first"), "orig.dng", &bytes);
    let copy = write(&base.join("second"), "copy.dng", &bytes);
    let (mut s, counted) = counted_session();
    let r = import(&mut s, &[&orig], "add", &base.join("out"));
    let id = r["imported"][0].as_u64();
    reads(&counted);
    let preview = s.execute("library.importPreview", &json!({"paths": [copy.to_string_lossy()]})).unwrap();
    assert_eq!(reads(&counted), (1, 0), "the review reads the raw's headers: {preview}");
    let c = &preview["candidates"][0];
    assert_eq!((c["duplicate"].as_str(), c["existing"].as_u64()), (Some("content"), id), "{preview}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_copied_raw_imported_from_headers_still_has_a_full_hash() {
    let base = crate::tests_xmp::temp_dir("import-headers-copy");
    let bytes = big_dng(5);
    let card = write(&base.join("card"), "a.dng", &bytes);
    let (mut s, counted) = counted_session();
    let preview = s.execute("library.importPreview", &json!({"paths": [card.to_string_lossy()]})).unwrap();
    assert_eq!(preview["scanned"], 1, "{preview}");
    let r = import(&mut s, &[&card], "copy", &base.join("out"));
    assert_eq!(reads(&counted), (1, 0), "probed once, by the review, from its headers: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    let _ = std::fs::remove_dir_all(&base);
}
