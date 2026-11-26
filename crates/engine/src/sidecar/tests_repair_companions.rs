//! Real Session partial moves, sidecar serialization/merging, AutoWrite and history.
use super::*;
use crate::rename::{MoveFs, RealFs};
use crate::tests_xmp::{same_path_for_volume, same_path_or_file};
use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

struct Faults(Vec<PathBuf>);
impl MoveFs for Faults {
    fn exists(&self, p: &Path) -> bool {
        RealFs.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        RealFs.is_file(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if self.0.iter().any(|p| same_path_or_file(p, a)) {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "injected partial move"))
        } else {
            RealFs.rename_no_replace(a, b)
        }
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        RealFs.copy_no_replace(a, b)
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        RealFs.remove_file(p)
    }
    fn stem_sibling(&self, p: &Path) -> Option<String> {
        RealFs.stem_sibling(p)
    }
}
fn packet(marker: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:fixture="urn:lightcraft:companion-test" xmp:Rating="2" fixture:Marker="{marker}"/></rdf:RDF></x:xmpmeta>"#
    )
}
fn queues(s: &Session) -> Vec<crate::rename::SidecarMove> {
    s.pending_sidecar_repairs.iter().chain(s.undo.iter().chain(&s.redo).flat_map(|e| &e.retry_sidecars)).cloned().collect()
}
fn session(path: &Path) -> Session {
    let mut s = Session::new();
    let photo = Photo::new(
        PhotoId(1),
        Source::File { path: path.to_string_lossy().into_owned() },
        path.file_name().unwrap().to_str().unwrap(),
        "JPEG",
        4,
        3,
        "",
    );
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.selection = crate::Selection::single(PhotoId(1));
    s
}
fn save(s: &mut Session) -> serde_json::Value {
    s.execute("photo.saveMetadataToFile", &json!({})).unwrap()
}
fn assert_fresh(path: &Path) {
    let xml = std::fs::read_to_string(path).unwrap();
    assert!(xml.contains("fixture:Marker="), "foreign property must survive real serialization/merge");
    let data = parse_sidecar(&xml, crate::crs::Target::Rendered).unwrap();
    assert_eq!(data.title.as_deref(), Some("Fresh"));
    assert!(data.keywords.unwrap().contains(&"café".to_string()));
}

#[test]
fn partial_move_companions_allow_save_and_autowrite_without_weakening_collisions() {
    for upper_stem in [false, true] {
        for upper_full in [false, true] {
            for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
                for auto in [false, true] {
                    check_companion_scenario(upper_stem, upper_full, naming, auto);
                }
            }
        }
    }
}

fn check_companion_scenario(upper_stem: bool, upper_full: bool, naming: SidecarNaming, auto: bool) {
    let dir = crate::tests_xmp::temp_dir(&format!("r3-companions-{upper_stem}-{upper_full}-{naming:?}-{auto}"));
    let a = dir.join("a.jpg");
    let b = dir.join("Trip.jpg");
    std::fs::write(&a, b"physical fixture; Session rename does not decode").unwrap();
    let path = |photo: &Path, style, upper| {
        let path = sidecar_path(&photo.to_string_lossy(), style);
        if upper { path.with_extension("XMP") } else { path }
    };
    let stem_a = path(&a, SidecarNaming::Stem, upper_stem);
    let full_a = path(&a, SidecarNaming::Full, upper_full);
    let stem_b = path(&b, SidecarNaming::Stem, upper_stem);
    let full_b = path(&b, SidecarNaming::Full, upper_full);
    let stem_bytes = packet("stem-keep");
    let full_bytes = packet("full-keep");
    std::fs::write(&stem_a, &stem_bytes).unwrap();
    std::fs::write(&full_a, &full_bytes).unwrap();
    let mut s = session(&a);
    s.xmp.naming = naming;
    s.xmp.auto_write = auto;
    let fs = Faults(vec![full_a.clone(), b.clone()]);
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: b.to_string_lossy().into_owned() });
    let pending = queues(&s);
    assert_eq!(pending.len(), 1);
    assert_eq!(std::fs::read_to_string(&stem_b).unwrap(), stem_bytes);

    // Missing packet and an unrelated packet at either case spelling remain protected.
    let held = dir.join("held.xmp");
    std::fs::rename(&full_a, &held).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Fresh", "addKeywords": ["café"]})).unwrap();
    assert_eq!(save(&mut s)["failed"].as_array().unwrap().len(), 1);
    assert_eq!(queues(&s), pending);
    assert_eq!(std::fs::read_to_string(&stem_b).unwrap(), stem_bytes);
    std::fs::rename(&held, &full_a).unwrap();
    for collision in [full_b.clone(), full_b.with_extension(if upper_full { "xmp" } else { "XMP" })] {
        std::fs::write(&collision, packet("unrelated-packet")).unwrap();
        let occupied = std::fs::read(&collision).unwrap();
        assert_eq!(save(&mut s)["failed"].as_array().unwrap().len(), 1);
        assert_eq!(queues(&s), pending);
        assert_eq!(std::fs::read(&collision).unwrap(), occupied);
        assert_eq!(std::fs::read_to_string(&full_a).unwrap(), full_bytes);
        assert_eq!(std::fs::read_to_string(&stem_b).unwrap(), stem_bytes);
        std::fs::remove_file(collision).unwrap();
    }
    // AutoWrite is the first successful repair when enabled; explicit save otherwise.
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    if !auto {
        assert_eq!(save(&mut s)["failed"].as_array().unwrap().len(), 0);
    }
    assert!(queues(&s).is_empty());
    assert!(!full_a.exists());
    let (written, untouched, expected) =
        if naming == SidecarNaming::Stem { (&stem_b, &full_b, &full_bytes) } else { (&full_b, &stem_b, &stem_bytes) };
    assert_fresh(written);
    assert_eq!(std::fs::read_to_string(untouched).unwrap(), *expected);
    assert_eq!(parse_sidecar(&std::fs::read_to_string(written).unwrap(), crate::crs::Target::Rendered).unwrap().rating, Some(4));
    for (command, rating) in [("edit.undo", 0), ("edit.redo", 4)] {
        s.execute(command, &json!({})).unwrap();
        if !auto {
            save(&mut s);
        }
        assert_fresh(written);
        assert_eq!(parse_sidecar(&std::fs::read_to_string(written).unwrap(), crate::crs::Target::Rendered).unwrap().rating, Some(rating));
        assert_eq!(std::fs::read_to_string(untouched).unwrap(), *expected);
    }
    assert_eq!(s.xmp.naming, naming);
    assert_eq!(s.xmp.auto_write, auto);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn rollback_repair_and_shared_copy_keep_known_companions() {
    for rollback in [false, true] {
        for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
            let dir = crate::tests_xmp::temp_dir(&format!("r3-companion-rollback-{rollback}-{naming:?}"));
            let a = dir.join("a.jpg");
            let b = dir.join("Trip.jpg");
            std::fs::write(&a, b"physical fixture").unwrap();
            let stem_a = sidecar_path(&a.to_string_lossy(), SidecarNaming::Stem);
            let full_a = sidecar_path(&a.to_string_lossy(), SidecarNaming::Full);
            let stem_b = sidecar_path(&b.to_string_lossy(), SidecarNaming::Stem);
            std::fs::write(&stem_a, packet("stem-keep")).unwrap();
            std::fs::write(&full_a, packet("full-keep")).unwrap();
            // When the photo cannot roll back, the stem is copied for the physical sibling.
            if !rollback {
                std::fs::write(dir.join("a.CR3"), b"physical stem sibling").unwrap();
            }
            let mut s = session(&a);
            let fs = Faults(vec![full_a.clone(), if rollback { stem_b.clone() } else { b.clone() }]);
            let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
            assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
            assert_eq!(queues(&s).len(), 1);
            let actual = if rollback { &a } else { &b };
            assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: actual.to_string_lossy().into_owned() });
            s.xmp.naming = naming;
            s.execute("photo.setMeta", &json!({"title": "Fresh", "addKeywords": ["café"]})).unwrap();
            let result = save(&mut s);
            assert!(result["failed"].as_array().unwrap().is_empty(), "{result}");
            assert!(queues(&s).is_empty());
            assert_fresh(&sidecar_path(&actual.to_string_lossy(), naming));
            if !rollback {
                assert_eq!(std::fs::read_to_string(&stem_a).unwrap(), packet("stem-keep"));
            }
            drop(s);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn a_later_save_recognizes_companions_after_an_earlier_repair_retires() {
    let dir = crate::tests_xmp::temp_dir("r3-successive-companions");
    let a = dir.join("a.jpg");
    let b = dir.join("Trip.jpg");
    let stem_a = sidecar_path(&a.to_string_lossy(), SidecarNaming::Stem);
    let full_a = sidecar_path(&a.to_string_lossy(), SidecarNaming::Full);
    let upper_a = full_a.with_extension("XMP");
    std::fs::write(&a, b"physical fixture").unwrap();
    std::fs::write(&stem_a, packet("stem-keep")).unwrap();
    std::fs::write(&full_a, packet("lower-keep")).unwrap();
    std::fs::write(&upper_a, packet("upper-keep")).unwrap();
    if RealFs.same_file(&full_a, &upper_a) {
        std::fs::remove_dir_all(dir).unwrap();
        return; // This case requires two physically distinct full packets.
    }
    let mut s = session(&a);
    s.xmp.naming = SidecarNaming::Full;
    let fs = Faults(vec![full_a.clone(), upper_a.clone(), b.clone()]);
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
    assert_eq!(queues(&s).len(), 2);
    assert!(s.repair_sidecars_with(&Faults(vec![full_a.clone()])).is_err());
    assert_eq!(queues(&s).len(), 1);
    let upper_b = sidecar_path(&b.to_string_lossy(), SidecarNaming::Full).with_extension("XMP");
    assert_eq!(std::fs::read_to_string(&upper_b).unwrap(), packet("upper-keep"));
    s.execute("photo.setMeta", &json!({"title": "Fresh", "addKeywords": ["café"]})).unwrap();
    assert!(save(&mut s)["failed"].as_array().unwrap().is_empty());
    assert!(queues(&s).is_empty());
    assert_fresh(&sidecar_path(&b.to_string_lossy(), SidecarNaming::Full));
    assert_eq!(std::fs::read_to_string(&upper_b).unwrap(), packet("upper-keep"));
    assert_eq!(std::fs::read_to_string(sidecar_path(&b.to_string_lossy(), SidecarNaming::Stem)).unwrap(), packet("stem-keep"));
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn queued_stem_retry_preserves_shared_packet_after_batch_rollback() {
    let dir = crate::tests_xmp::temp_dir("queued-stem-batch-rollback");
    let a = dir.join("a.jpg");
    let b = dir.join("a.png");
    let first = dir.join("Trip_1.jpg");
    let second = dir.join("Trip_2.png");
    let shared = dir.join("a.xmp");
    let first_packet = dir.join("Trip_1.xmp");
    let second_packet = dir.join("Trip_2.xmp");
    let jpeg = b"first physical photo; rename does not decode".to_vec();
    let png = b"second physical photo; rename does not decode".to_vec();
    let bytes = packet("shared-after-batch-rollback").into_bytes();
    std::fs::write(&a, &jpeg).unwrap();
    std::fs::write(&b, &png).unwrap();
    std::fs::write(&shared, &bytes).unwrap();
    let mut s = session(&a);
    let photo = Photo::new(PhotoId(2), Source::File { path: b.to_string_lossy().into_owned() }, "a.png", "PNG", 4, 3, "");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.xmp.auto_write = false;

    // Existing Faults refuses renames only: the first shared-packet copy must succeed.
    let faults = Faults(vec![shared.clone(), second.clone()]);
    let plans = s.plan_rename_with(&faults, &[PhotoId(1), PhotoId(2)], "Trip_{seq}", 1);
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&faults, &plans)?}))).is_err());
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: a.to_string_lossy().into_owned() });
    assert_eq!(s.catalog.photo(PhotoId(2)).unwrap().source, Source::File { path: second.to_string_lossy().into_owned() });
    assert_eq!(std::fs::read(&a).unwrap(), jpeg);
    assert_eq!(std::fs::read(&second).unwrap(), png);
    assert!(!b.exists() && !first.exists());
    assert_eq!(std::fs::read(&shared).unwrap(), bytes);
    assert_eq!(std::fs::read(&first_packet).unwrap(), bytes);
    assert!(!second_packet.exists());
    let pending = queues(&s);
    assert_eq!(pending.len(), 1);
    assert!(same_path_for_volume(&pending[0].from, &shared), "{:?}", pending[0].from);
    assert!(same_path_for_volume(&pending[0].to, &second_packet), "{:?}", pending[0].to);
    let catalog_before = s.catalog.to_snapshot();
    let history_before = (s.undo.len(), s.redo.len(), s.commits());

    // Public command, real filesystem: its no-op path drains the naturally retained repair.
    let retry = s.execute("photo.rename", &json!({"ids": [2], "template": "{name}"}));
    let catalog_after = s.catalog.to_snapshot();
    let history_after = (s.undo.len(), s.redo.len(), s.commits());
    let repaired = queues(&s).is_empty();
    let old_paths_absent = !b.exists() && !first.exists();
    let files = [&a, &second, &first_packet, &second_packet].map(|p| std::fs::read(p).map_err(|e| e.kind()));
    let retained = std::fs::read(&shared).map_err(|e| e.kind());
    drop(s);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!dir.exists(), "fixture cleanup must precede the qualifying assertion");

    assert_eq!(retry.unwrap()["renamed"], 0);
    assert_eq!(catalog_after, catalog_before, "repair must preserve both catalog sources");
    assert_eq!(history_after, history_before);
    assert!(repaired && old_paths_absent);
    assert_eq!(files, [Ok(jpeg), Ok(png), Ok(bytes.clone()), Ok(bytes.clone())]);
    assert_eq!(retained, Ok(bytes), "queued repair took shared XMP from the photo restored by batch rollback");
}
