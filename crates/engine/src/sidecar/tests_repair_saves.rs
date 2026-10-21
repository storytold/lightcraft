//! Real Session/serializer regressions for saves that overlap retained physical repairs.
use super::*;
use crate::rename::{MoveFs, RealFs};
use crate::tests_xmp::same_path_or_file;
use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

struct PartialRename {
    sidecar: PathBuf,
    photo: PathBuf,
}
impl MoveFs for PartialRename {
    fn exists(&self, path: &Path) -> bool {
        RealFs.exists(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        RealFs.is_file(path)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if same_path_or_file(a, &self.sidecar) || same_path_or_file(a, &self.photo) {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "transient rename fault"))
        } else {
            RealFs.rename_no_replace(a, b)
        }
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if same_path_or_file(a, &self.sidecar) {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "transient copy fault"))
        } else {
            RealFs.copy_no_replace(a, b)
        }
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        RealFs.remove_file(path)
    }
    fn stem_sibling(&self, path: &Path) -> Option<String> {
        RealFs.stem_sibling(path)
    }
}

const FOREIGN: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:fixture="urn:lightcraft:repair-test" xmp:Rating="2" fixture:Marker="keep-me"/></rdf:RDF></x:xmpmeta>"#;

fn png(path: &Path) {
    let image = lightcraft_raster::Rgba8::from_fn(4, 3, |_, _| [40, 80, 120, 255]);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn queued(s: &Session) -> Vec<crate::rename::SidecarMove> {
    s.pending_sidecar_repairs.iter().chain(s.undo.iter().chain(&s.redo).flat_map(|entry| entry.retry_sidecars.iter())).cloned().collect()
}

#[test]
fn autowrite_repairs_partial_rename_before_merging_foreign_xmp_across_library_switch() {
    for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
        for save_naming in [SidecarNaming::Stem, SidecarNaming::Full] {
            for upper in [false, true] {
                for switch in [false, true] {
                    let root = crate::tests_xmp::temp_dir(&format!("repair-save-{naming:?}-{save_naming:?}-{upper}-{switch}"));
                    let a = root.join("a.png");
                    let b = root.join("Trip.png");
                    png(&a);
                    let mut old = sidecar_path(&a.to_string_lossy(), naming);
                    if upper {
                        old = old.with_extension("XMP");
                    }
                    std::fs::write(&old, FOREIGN).unwrap();
                    let mut s = Session::new().with_fs();
                    s.open_library(root.join("library-a"), false).unwrap();
                    s.execute("library.import", &json!({"paths": [a]})).unwrap();
                    let mut id = s.catalog.photos().next().unwrap().id;
                    s.selection = crate::Selection::single(id);
                    s.undo.clear();
                    let fs = PartialRename { sidecar: old.clone(), photo: b.clone() };
                    let plans = s.plan_rename_with(&fs, &[id], "Trip", 1);
                    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
                    assert_eq!(queued(&s).len(), 1);
                    assert_eq!(s.catalog.photo(id).unwrap().source, Source::File { path: b.to_string_lossy().into_owned() });
                    let pending = queued(&s);
                    let held = root.join("temporarily-unavailable.xmp");
                    std::fs::rename(&old, &held).unwrap();
                    if switch {
                        s.close_library().unwrap();
                        s.open_library(root.join("library-b"), false).unwrap();
                        assert_eq!(s.pending_sidecar_repairs, pending);
                        s.execute("library.import", &json!({"paths": [b]})).unwrap();
                        id = s.catalog.photos().next().unwrap().id;
                        s.selection = crate::Selection::single(id);
                    }
                    s.xmp.naming = save_naming;
                    s.xmp.auto_write = true;
                    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
                    let preferred = sidecar_path(&b.to_string_lossy(), save_naming);
                    assert!(!preferred.exists() && !preferred.with_extension("XMP").exists());
                    assert!(!pending[0].to.exists());
                    assert_eq!(queued(&s), pending, "unavailable repair stays queued after AutoWrite");
                    let saved = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
                    assert!(saved["written"].as_array().unwrap().is_empty());
                    assert!(saved["failed"][0]["error"].as_str().unwrap().contains("unfinished XMP repair"));
                    assert_eq!(std::fs::read(&held).unwrap(), FOREIGN);
                    assert_eq!(queued(&s), pending);

                    // A missing sidecar for another photo does not prevent this healthy save.
                    let healthy = root.join("healthy.png");
                    png(&healthy);
                    let healthy_id = PhotoId(999);
                    s.commit(
                        "Healthy Photo",
                        Op::AddPhoto {
                            photo: Box::new(Photo::new(
                                healthy_id,
                                Source::File { path: healthy.to_string_lossy().into_owned() },
                                "healthy.png",
                                "PNG",
                                4,
                                3,
                                "",
                            )),
                        },
                    )
                    .unwrap();
                    s.execute("photo.setMeta", &json!({"ids": [healthy_id.0], "title": "Healthy save"})).unwrap();
                    let packet = std::fs::read_to_string(sidecar_path(&healthy.to_string_lossy(), save_naming)).unwrap();
                    assert_eq!(parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap().title.as_deref(), Some("Healthy save"));
                    assert_eq!(queued(&s), pending);

                    // An existing destination is never overwritten and the original packet remains.
                    std::fs::rename(&held, &old).unwrap();
                    let destination = pending[0].to.clone();
                    let collision = String::from_utf8(FOREIGN.to_vec()).unwrap().replace("Rating=\"2\"", "Rating=\"5\"").into_bytes();
                    let collision_path = if upper { preferred.clone() } else { destination.clone() };
                    std::fs::write(&collision_path, &collision).unwrap();
                    s.execute("photo.setMeta", &json!({"ids": [id.0], "title": "Fresh title", "addKeywords": ["café"]})).unwrap();
                    assert_eq!(std::fs::read(&old).unwrap(), FOREIGN);
                    assert_eq!(std::fs::read(&collision_path).unwrap(), collision);
                    assert_eq!(queued(&s), pending);
                    s.selection = crate::Selection::single(id);
                    assert_eq!(s.execute("photo.saveMetadataToFile", &json!({})).unwrap()["failed"].as_array().unwrap().len(), 1);
                    assert_eq!(std::fs::read(&collision_path).unwrap(), collision);
                    std::fs::remove_file(&collision_path).unwrap(); // remove only this test's collision

                    // The next successful AutoWrite runs the real repair and production XMP merge.
                    s.execute("photo.setMeta", &json!({"title": "Final title"})).unwrap();
                    assert!(queued(&s).is_empty());
                    assert!(!old.exists());
                    let packet = std::fs::read_to_string(&destination).unwrap();
                    assert!(packet.contains("fixture:Marker=\"keep-me\""));
                    let parsed = parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap();
                    assert_eq!(parsed.rating, Some(4));
                    assert_eq!(parsed.title.as_deref(), Some("Final title"));
                    assert!(parsed.keywords.as_ref().unwrap().contains(&"café".to_string()));
                    if upper {
                        // by listing: on a case-insensitive volume `preferred` answers for the upper-case packet
                        let listed = std::fs::read_dir(&root).unwrap().any(|e| Some(e.unwrap().file_name().as_os_str()) == preferred.file_name());
                        assert!(!listed, "merge uses the repaired uppercase packet");
                    }
                    for (step, title) in [("edit.undo", "Fresh title"), ("edit.redo", "Final title")] {
                        s.execute(step, &json!({})).unwrap();
                        let packet = std::fs::read_to_string(&destination).unwrap();
                        assert!(packet.contains("fixture:Marker=\"keep-me\""));
                        assert_eq!(parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap().title.as_deref(), Some(title));
                    }
                    drop(s);
                    std::fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }
}

#[test]
fn unowned_shared_stem_repair_does_not_block_healthy_jpeg_full_sidecar_save() {
    let root = crate::tests_xmp::temp_dir("repair-save-shared-stem");
    let raw = root.join("old.CR3");
    let moved = root.join("Trip.CR3");
    let jpeg = root.join("Trip.jpg");
    std::fs::write(&raw, b"physical fixture; catalogued without decoding").unwrap();
    png(&jpeg);
    let old = sidecar_path(&raw.to_string_lossy(), SidecarNaming::Stem);
    std::fs::write(&old, FOREIGN).unwrap();
    let mut s = Session::new();
    let raw_id = PhotoId(1);
    let jpeg_id = PhotoId(2);
    let mut photo = Photo::new(raw_id, Source::File { path: raw.to_string_lossy().into_owned() }, "old.CR3", "CR3", 4, 3, "");
    photo.kind = MediaKind::Raw;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.catalog
        .apply(Op::AddPhoto {
            photo: Box::new(Photo::new(jpeg_id, Source::File { path: jpeg.to_string_lossy().into_owned() }, "Trip.jpg", "JPEG", 4, 3, "")),
        })
        .unwrap();
    let fs = PartialRename { sidecar: old.clone(), photo: moved };
    let plans = s.plan_rename_with(&fs, &[raw_id], "Trip", 1);
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
    let pending = queued(&s);
    assert_eq!(pending.len(), 1);
    let held = root.join("temporarily-unavailable.xmp");
    std::fs::rename(&old, &held).unwrap();
    assert_eq!(s.sidecar_naming(jpeg_id), SidecarNaming::Full);
    s.selection = crate::Selection::single(jpeg_id);
    s.xmp.auto_write = true;
    s.execute("photo.setMeta", &json!({"title": "Healthy JPEG"})).unwrap();
    let full = sidecar_path(&jpeg.to_string_lossy(), SidecarNaming::Full);
    let packet = std::fs::read_to_string(&full).unwrap();
    assert_eq!(parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap().title.as_deref(), Some("Healthy JPEG"));
    assert!(!pending[0].to.exists());
    assert_eq!(queued(&s), pending);
    assert_eq!(std::fs::read(&held).unwrap(), FOREIGN);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
