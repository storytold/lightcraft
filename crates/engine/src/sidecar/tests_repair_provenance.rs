//! Native files and genuine Session entry points: packet ownership survives partial renames.
use super::*;
use crate::rename::{MoveFs, RealFs};
use crate::tests_xmp::same_path_or_file;
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
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "injected provenance partial move"))
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
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:fixture="urn:lightcraft:provenance-test" xmp:Rating="5" fixture:Marker="{marker}"/></rdf:RDF></x:xmpmeta>"#
    )
}
fn path(photo: &Path, style: SidecarNaming, upper: bool) -> PathBuf {
    let p = sidecar_path(&photo.to_string_lossy(), style);
    if upper { p.with_extension("XMP") } else { p }
}
fn queued(s: &Session) -> Vec<crate::rename::SidecarMove> {
    s.pending_sidecar_repairs.iter().chain(s.undo.iter().chain(&s.redo).flat_map(|e| &e.retry_sidecars)).cloned().collect()
}
fn session(photo: &Path) -> Session {
    let mut s = Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: photo.to_string_lossy().into_owned() }, "a.jpg", "JPEG", 4, 3, "");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.selection = crate::Selection::single(PhotoId(1));
    s
}
fn save(s: &mut Session) -> serde_json::Value {
    s.execute("photo.saveMetadataToFile", &json!({})).unwrap()
}
struct Partial {
    s: Session,
    dir: PathBuf,
    a: PathBuf,
    b: PathBuf,
    companion: PathBuf,
    source: PathBuf,
    target: PathBuf,
}
fn partial(style: SidecarNaming, upper_stem: bool, upper_full: bool, shared: bool) -> Partial {
    // one folder per fixture: tests run in parallel, and each removes its folder at the end
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = crate::tests_xmp::temp_dir(&format!("sidecar-provenance-{n}"));
    let a = dir.join("a.jpg");
    let b = dir.join("Trip.jpg");
    std::fs::write(&a, b"tiny physical photo; rename does not decode").unwrap();
    let stem_a = path(&a, SidecarNaming::Stem, upper_stem);
    let full_a = path(&a, SidecarNaming::Full, upper_full);
    let stem_b = path(&b, SidecarNaming::Stem, upper_stem);
    let full_b = path(&b, SidecarNaming::Full, upper_full);
    std::fs::write(&stem_a, packet("observed-stem")).unwrap();
    std::fs::write(&full_a, packet("observed-full")).unwrap();
    if shared {
        std::fs::write(dir.join("a.CR3"), b"shared physical sibling").unwrap();
    }
    let mut s = session(&a);
    s.xmp.naming = style;
    let rollback = style == SidecarNaming::Full;
    let faults = Faults(vec![full_a.clone(), if rollback { stem_b.clone() } else { b.clone() }]);
    let plans = s.plan_rename_with(&faults, &[PhotoId(1)], "Trip", 1);
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&faults, &plans)?}))).is_err());
    assert_eq!(queued(&s).len(), 1);
    let (companion, source, target) = if rollback { (full_a, stem_b, stem_a) } else { (stem_b, full_a, full_b) };
    Partial { s, dir, a, b, companion, source, target }
}
fn replace(p: &Path, held: &Path, in_place: bool) -> (Vec<u8>, Vec<u8>) {
    let original = std::fs::read(p).unwrap();
    #[cfg(unix)]
    let before = {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(p).unwrap();
        (m.dev(), m.ino())
    };
    if in_place {
        std::fs::write(held, &original).unwrap();
    } else {
        std::fs::rename(p, held).unwrap();
    }
    let foreign = packet(&"f".repeat(original.len() - packet("").len())).into_bytes();
    assert_eq!(original.len(), foreign.len(), "content changes must be detected even at the same length");
    assert_ne!(original, foreign);
    assert!(parse_sidecar(std::str::from_utf8(&foreign).unwrap(), crate::crs::Target::Rendered).is_ok());
    std::fs::write(p, &foreign).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(p).unwrap();
        assert_eq!(before == (m.dev(), m.ino()), in_place, "exercise replacement and unchanged-inode edits separately");
    }
    (original, foreign)
}

#[test]
fn replaced_and_in_place_companions_block_save_autowrite_retry_and_file_history() {
    for style in [SidecarNaming::Stem, SidecarNaming::Full] {
        for upper_stem in [false, true] {
            for upper_full in [false, true] {
                for in_place in [false, true] {
                    let mut f = partial(style, upper_stem, upper_full, false);
                    let held = f.dir.join("original-held.xmp");
                    let (original, foreign) = replace(&f.companion, &held, in_place);
                    let queue = queued(&f.s);
                    let source = std::fs::read(&f.source).unwrap();
                    let before = f.s.catalog.photo(PhotoId(1)).unwrap().source.clone();
                    f.s.xmp.auto_write = true;
                    f.s.execute("photo.setMeta", &json!({"title": "Fresh", "addKeywords": ["café"]})).unwrap();
                    assert_eq!(queued(&f.s), queue, "AutoWrite must retain the exact repair");
                    assert_eq!(std::fs::read(&f.companion).unwrap(), foreign);
                    let saved = save(&mut f.s);
                    assert!(saved["written"].as_array().unwrap().is_empty());
                    assert!(saved["failed"][0]["error"].as_str().unwrap().contains("changed since"));
                    assert_eq!(queued(&f.s), queue);
                    assert_eq!(std::fs::read(&f.source).unwrap(), source);
                    assert!(!f.target.exists());
                    assert!(f.s.repair_sidecars_with(&RealFs).is_err());
                    let plans = f.s.plan_rename(&[PhotoId(1)], if style == SidecarNaming::Stem { "Trip" } else { "a" }, 1);
                    assert!(f.s.apply_rename(&plans).is_err());
                    // Metadata history remains available while physical ownership is unresolved.
                    f.s.execute("edit.undo", &json!({})).unwrap();
                    assert_eq!(std::fs::read(&f.companion).unwrap(), foreign);
                    assert!(f.s.undo_step().is_err(), "the real rename Undo must reject the foreign companion");
                    assert_eq!(queued(&f.s), queue);
                    assert_eq!(f.s.catalog.photo(PhotoId(1)).unwrap().source, before);
                    assert_eq!(std::fs::read(&f.companion).unwrap(), foreign);
                    assert_eq!(std::fs::read(&held).unwrap(), original);
                    // Restoring the observed content resolves ownership and keeps the positive path.
                    std::fs::write(&f.companion, &original).unwrap();
                    assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
                    assert!(queued(&f.s).is_empty());
                    assert_eq!(std::fs::read(&f.target).unwrap(), source);
                    drop(f.s);
                    std::fs::remove_dir_all(f.dir).unwrap();
                }
            }
        }
    }
}

#[test]
fn failed_redo_and_discarded_or_pruned_history_keep_companion_provenance() {
    for owner in ["redo", "discarded", "pruned"] {
        let mut f = partial(SidecarNaming::Stem, false, false, false);
        if owner == "pruned" {
            for _ in 0..1000 {
                f.s.commit("Unrelated retention step", Op::Batch { ops: Vec::new() }).unwrap();
            }
            assert_eq!(f.s.pending_sidecar_repairs.len(), 1);
        } else {
            f.s.undo_step().unwrap();
            let full_a = path(&f.a, SidecarNaming::Full, false);
            assert!(f.s.redo_step_with(&Faults(vec![full_a, f.b.clone()])).is_err());
            assert_eq!(f.s.redo.iter().map(|e| e.retry_sidecars.len()).sum::<usize>(), 1);
            if owner == "discarded" {
                f.s.commit("Discard failed redo", Op::SetRating { id: PhotoId(1), rating: 3 }).unwrap();
                assert_eq!(f.s.pending_sidecar_repairs.len(), 1);
                assert!(f.s.redo.is_empty());
            }
        }
        let held = f.dir.join("original-held.xmp");
        let (original, foreign) = replace(&f.companion, &held, false);
        let queue = queued(&f.s);
        assert!(!save(&mut f.s)["failed"].as_array().unwrap().is_empty());
        assert!(f.s.repair_sidecars_with(&RealFs).is_err());
        if owner == "redo" {
            assert!(f.s.redo_step().is_err());
        }
        assert_eq!(queued(&f.s), queue);
        assert_eq!(std::fs::read(&f.companion).unwrap(), foreign);
        std::fs::write(&f.companion, &original).unwrap();
        assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
        assert!(queued(&f.s).is_empty());
        drop(f.s);
        std::fs::remove_dir_all(f.dir).unwrap();
    }
}

fn two_repairs() -> Option<Partial> {
    let mut f = partial(SidecarNaming::Stem, false, false, false);
    // Resolve the initial fixture, then create a new genuine partial rename with two packets.
    f.s.undo_step().unwrap();
    let upper_a = path(&f.a, SidecarNaming::Full, true);
    std::fs::write(&upper_a, packet("observed-upper-full")).unwrap();
    let full_a = path(&f.a, SidecarNaming::Full, false);
    if RealFs.same_file(&upper_a, &full_a) {
        drop(f.s);
        std::fs::remove_dir_all(f.dir).unwrap();
        return None; // This case requires two physically distinct case spellings.
    }
    let fs = Faults(vec![upper_a, full_a.clone(), f.b.clone()]);
    let plans = f.s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    assert!(f.s.apply_rename_with(&fs, &plans).is_err());
    assert_eq!(queued(&f.s).len(), 2);
    f.source = full_a;
    Some(f)
}

#[test]
fn absent_peer_destinations_are_not_owned_and_retirement_records_only_completed_packets() {
    let Some(mut f) = two_repairs() else { return };
    let future = path(&f.b, SidecarNaming::Full, false);
    let upper = path(&f.b, SidecarNaming::Full, true);
    let foreign = packet("foreign-at-never-created-destination").into_bytes();
    std::fs::write(&future, &foreign).unwrap();
    let queue = queued(&f.s);
    let original_companion = std::fs::read(&f.companion).unwrap();
    assert!(!save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(f.s.repair_sidecars_with(&RealFs).is_err());
    assert_eq!(queued(&f.s), queue);
    assert!(!upper.exists(), "a foreign peer must be rejected before the first repair retires");
    assert_eq!(std::fs::read(&future).unwrap(), foreign);
    assert_eq!(std::fs::read(&f.companion).unwrap(), original_companion);
    std::fs::remove_file(&future).unwrap();
    assert!(f.s.repair_sidecars_with(&Faults(vec![f.source.clone()])).is_err());
    assert_eq!(queued(&f.s).len(), 1);
    let held = f.dir.join("retired-original-held.xmp");
    let (observed_upper, replaced) = replace(&upper, &held, false);
    let queue = queued(&f.s);
    assert!(!save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(f.s.repair_sidecars_with(&RealFs).is_err());
    assert_eq!(queued(&f.s), queue);
    assert_eq!(std::fs::read(&upper).unwrap(), replaced);
    assert!(!future.exists());
    std::fs::write(&upper, &observed_upper).unwrap();
    assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(queued(&f.s).is_empty());
    assert_eq!(std::fs::read(&upper).unwrap(), observed_upper);
    drop(f.s);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn own_atomic_shared_packet_writes_advance_retained_versions_and_failures_do_not() {
    let mut f = partial(SidecarNaming::Stem, false, false, true);
    let shared = path(&f.a, SidecarNaming::Stem, false);
    let shared_before = std::fs::read(&shared).unwrap();
    let copied_before = std::fs::read(&f.companion).unwrap();
    let raw = f.dir.join("a.CR3");
    let mut photo = Photo::new(PhotoId(2), Source::File { path: raw.to_string_lossy().into_owned() }, "a.CR3", "CR3", 4, 3, "");
    photo.kind = MediaKind::Raw;
    photo.meta.title = "Legitimate shared packet write".into();
    f.s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    // A missing repair for the moved JPEG must not block a healthy save by its raw sibling.
    let held = f.dir.join("unavailable-source.xmp");
    std::fs::rename(&f.source, &held).unwrap();
    let before = queued(&f.s);
    {
        let _fault = lightcraft_catalog::safe_file::fail_writes_after(0);
        assert!(f.s.save_sidecar(PhotoId(2)).is_err());
    }
    assert_eq!(queued(&f.s), before, "failed atomic writes must not advance provenance");
    assert_eq!(std::fs::read(&shared).unwrap(), shared_before);
    f.s.save_sidecar(PhotoId(2)).unwrap();
    assert_ne!(std::fs::read(&shared).unwrap(), shared_before);
    assert_eq!(std::fs::read(&f.companion).unwrap(), copied_before);
    assert_eq!(queued(&f.s).len(), 1);
    // External edits are still refused after the application's own atomic replacement.
    let shared_written = std::fs::read(&shared).unwrap();
    std::fs::write(&shared, packet("foreign-after-our-write")).unwrap();
    assert!(f.s.save_sidecar(PhotoId(2)).is_err());
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), packet("foreign-after-our-write"));
    std::fs::write(&shared, &shared_written).unwrap();
    f.s.save_sidecar(PhotoId(2)).unwrap();
    std::fs::rename(&held, &f.source).unwrap();
    assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(queued(&f.s).is_empty(), "own shared writes must not stale the retained repair");
    assert_eq!(std::fs::read(&shared).unwrap(), shared_written);
    drop(f.s);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn replaced_copy_keeps_its_shared_original_and_repair_source_byte_identical() {
    let mut f = partial(SidecarNaming::Stem, true, true, true);
    let shared = path(&f.a, SidecarNaming::Stem, true);
    let shared_before = std::fs::read(&shared).unwrap();
    let source = std::fs::read(&f.source).unwrap();
    let held = f.dir.join("copied-original-held.xmp");
    let (original, foreign) = replace(&f.companion, &held, false);
    let queue = queued(&f.s);
    f.s.xmp.auto_write = true;
    f.s.execute("photo.setMeta", &json!({"title": "Fresh"})).unwrap();
    assert!(!save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(f.s.repair_sidecars_with(&RealFs).is_err());
    assert_eq!(queued(&f.s), queue);
    assert_eq!(std::fs::read(&f.companion).unwrap(), foreign);
    assert_eq!(std::fs::read(&shared).unwrap(), shared_before);
    assert_eq!(std::fs::read(&f.source).unwrap(), source);
    std::fs::write(&f.companion, &original).unwrap();
    assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
    assert!(queued(&f.s).is_empty());
    assert_eq!(std::fs::read(&shared).unwrap(), shared_before);
    drop(f.s);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn changed_separated_sources_are_not_moved_or_retired() {
    for style in [SidecarNaming::Stem, SidecarNaming::Full] {
        let mut f = partial(style, false, true, false);
        let held = f.dir.join("source-original-held.xmp");
        let (original, foreign) = replace(&f.source, &held, true);
        let queue = queued(&f.s);
        let companion = std::fs::read(&f.companion).unwrap();
        assert!(!save(&mut f.s)["failed"].as_array().unwrap().is_empty());
        assert!(f.s.repair_sidecars_with(&RealFs).is_err());
        assert_eq!(queued(&f.s), queue);
        assert_eq!(std::fs::read(&f.source).unwrap(), foreign);
        assert_eq!(std::fs::read(&f.companion).unwrap(), companion);
        assert!(!f.target.exists());
        std::fs::write(&f.source, &original).unwrap();
        assert!(save(&mut f.s)["failed"].as_array().unwrap().is_empty());
        assert!(queued(&f.s).is_empty());
        drop(f.s);
        std::fs::remove_dir_all(f.dir).unwrap();
    }
}

#[test]
fn detached_history_entry_is_checked_before_unrelated_global_retirement() {
    // A private-method contract, not a UI reproduction: application reachability of two
    // simultaneous repair owners is unproven. Native move failures and follow_stuck seed
    // both records; the second owner's normal apply_rename preflight is deliberately bypassed.
    struct NativePartial {
        id: PhotoId,
        original: PathBuf,
        moved: PathBuf,
        stem: PathBuf,
        full: PathBuf,
        companion: PathBuf,
        target: PathBuf,
    }
    let root = crate::tests_xmp::temp_dir("detached-history-preflight-contract");
    let image = lightcraft_raster::Rgba8::from_fn(4, 3, |_, _| [40, 80, 120, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
    let mut s = Session::new();
    s.xmp.naming = SidecarNaming::Stem;
    let mut native = Vec::new();
    for (i, name) in ["first", "second"].into_iter().enumerate() {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("a.png");
        let moved = dir.join("Trip.png");
        std::fs::write(&original, &png).unwrap();
        let stem = path(&original, SidecarNaming::Stem, false);
        let full = path(&original, SidecarNaming::Full, true);
        for (p, marker) in [(&stem, format!("{name}-stem")), (&full, format!("{name}-full"))] {
            std::fs::write(p, packet(&marker)).unwrap();
            assert_eq!(parse_sidecar(&std::fs::read_to_string(p).unwrap(), crate::crs::Target::Rendered).unwrap().rating, Some(5));
        }
        let id = PhotoId(i as u64 + 1);
        let photo = Photo::new(id, Source::File { path: original.to_string_lossy().into_owned() }, "a.png", "PNG", 4, 3, "");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let fs = Faults(vec![full.clone(), moved.clone()]);
        if i == 0 {
            let plans = s.plan_rename_with(&fs, &[id], "Trip", 1);
            assert!(s.apply_rename_with(&fs, &plans).is_err());
        } else {
            // Register the genuine second partial move without draining the first repair.
            let moves = vec![(original.to_string_lossy().into_owned(), moved.to_string_lossy().into_owned())];
            let err = crate::rename::move_all(&fs, &moves).unwrap_err();
            assert_eq!(err.stuck, moves);
            assert_eq!(err.pending_sidecars.len(), 1);
            let op = Op::SetFile { id, source: Source::File { path: moved.to_string_lossy().into_owned() }, file_name: "Trip.png".into() };
            s.follow_stuck(&op, err, true);
        }
        assert_eq!(s.undo.len(), i + 1);
        assert_eq!(queued(&s).len(), i + 1);
        assert_eq!(s.catalog.photo(id).unwrap().source, Source::File { path: moved.to_string_lossy().into_owned() });
        let decoded = lightcraft_codecs::decode(&std::fs::read(&moved).unwrap(), Default::default()).unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 3));
        native.push(NativePartial {
            id,
            original,
            companion: path(&moved, SidecarNaming::Stem, false),
            target: path(&moved, SidecarNaming::Full, true),
            moved,
            stem,
            full,
        });
    }
    let first = &native[0];
    let second = &native[1];
    let first_stem = std::fs::read(&first.companion).unwrap();
    let full_packets: Vec<_> = native.iter().map(|f| std::fs::read(&f.full).unwrap()).collect();
    let held = root.join("observed-second-companion.xmp");
    let (observed, foreign) = replace(&second.companion, &held, false);
    let mut entry = s.undo.pop().unwrap(); // Undo/Redo call this method with their entry detached.
    let retained = queued(&s);
    let detached = entry.retry_sidecars.clone();
    assert_eq!((retained.len(), detached.len()), (1, 1));
    let sources: Vec<_> = native.iter().map(|f| s.catalog.photo(f.id).unwrap().source.clone()).collect();
    let log_len = s.pending_log.len();
    let mut packets: Vec<_> = native.iter().flat_map(|f| [&f.moved, &f.full, &f.companion]).map(|p| (p.clone(), std::fs::read(p).unwrap())).collect();
    packets.push((held.clone(), std::fs::read(&held).unwrap()));
    assert!(!first.target.exists() && !second.target.exists());

    let err = s.apply_with_files(&mut entry, &RealFs).unwrap_err();
    assert!(err.to_string().contains("changed since"), "{err}");
    assert_eq!(std::fs::read(&second.companion).unwrap(), foreign);
    println!(
        "detached history preflight contract: retained_before={}, retained_after={}, detached_before={}, detached_after={}, first_target_exists={}",
        retained.len(),
        queued(&s).len(),
        detached.len(),
        entry.retry_sidecars.len(),
        first.target.exists()
    );
    assert_eq!(queued(&s), retained, "reject the detached owner before any unrelated global repair retires");
    assert_eq!(entry.retry_sidecars, detached);
    assert_eq!(s.pending_log.len(), log_len);
    for (f, source) in native.iter().zip(&sources) {
        assert_eq!(&s.catalog.photo(f.id).unwrap().source, source);
        assert!(!f.original.exists() && !f.target.exists());
    }
    for (p, bytes) in &packets {
        assert_eq!(std::fs::read(p).unwrap(), *bytes, "rejection must preserve {} byte for byte", p.display());
    }

    // Resolving the changed content permits the same real method to repair and undo the move.
    std::fs::write(&second.companion, &observed).unwrap();
    let inverse = s.apply_with_files(&mut entry, &RealFs).unwrap();
    assert!(queued(&s).is_empty() && entry.retry_sidecars.is_empty());
    assert_eq!(s.catalog.photo(first.id).unwrap().source, sources[0]);
    assert_eq!(s.catalog.photo(second.id).unwrap().source, Source::File { path: second.original.to_string_lossy().into_owned() });
    assert_eq!(s.file_moves(&inverse), vec![(second.original.to_string_lossy().into_owned(), second.moved.to_string_lossy().into_owned())]);
    assert_eq!(std::fs::read(&first.moved).unwrap(), png);
    assert_eq!(std::fs::read(&second.original).unwrap(), png);
    assert_eq!(std::fs::read(&first.companion).unwrap(), first_stem);
    assert_eq!(std::fs::read(&first.target).unwrap(), full_packets[0]);
    assert_eq!(std::fs::read(&second.stem).unwrap(), observed);
    assert_eq!(std::fs::read(&second.full).unwrap(), full_packets[1]);
    assert_eq!(std::fs::read(&held).unwrap(), observed);
    assert!(!first.original.exists() && !first.full.exists());
    assert!(!second.moved.exists() && !second.companion.exists() && !second.target.exists());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
