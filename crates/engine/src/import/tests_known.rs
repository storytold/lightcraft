//! What a scan or an import knows of the library: worked out for the files it meets, and not on
//! the session's thread. A library can hold millions of photos; looking at one folder must not
//! cost a table of them all before the first file is read.
use super::*;
use lightcraft_catalog::{Catalog, Op};

/// A library of `folders` folders of three photos each, the third a copy (by content) of a
/// photo in the folder before.
fn library(folders: usize) -> Catalog {
    let mut c = Catalog::new();
    for folder in 0..folders {
        for (n, name) in ["a.jpg", "b.jpg", "c.jpg"].into_iter().enumerate() {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::File { path: format!("/pics/{folder:03}/{name}") }, name, "JPEG", 6, 4, "2026-01-01");
            p.content_hash = Some(format!("{:032x}", if n == 2 { folder.saturating_sub(1) * 3 } else { folder * 3 + n }));
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
    }
    c
}

/// Given a library of many folders, when a scan meets the files of one of them (and one the
/// library does not have), then what it knows of the library is those files' photos, each as the
/// library has it, and nothing of the other folders.
#[test]
fn a_scan_knows_the_library_photos_of_the_files_it_found() {
    let c = library(100);
    let files: Vec<String> = ["/pics/007/a.jpg", "/pics/007/b.jpg", "/pics/007/new.jpg", "/pics/007//c.jpg"].map(String::from).to_vec();
    let known = library_files(&c, &files);
    assert_eq!(known.len(), 3, "the three the library has (a path is compared as a path, not as text)");
    let (id, as_found) = known.get(Path::new("/pics/007/b.jpg")).expect("b");
    let p = c.photo(*id).unwrap();
    assert_eq!((as_found.path.as_str(), as_found.existing, as_found.duplicate.as_deref()), ("/pics/007/b.jpg", Some(id.0), Some("path")));
    assert_eq!((as_found.format.as_str(), as_found.width, as_found.height, as_found.file_size), (p.format.as_str(), p.width, p.height, p.file_size));
    assert!(library_files(&c, &[]).is_empty());
    // two photos of one file (a virtual copy): the later one is the one named, as a table of the
    // whole library named it
    let mut c = c;
    let copy = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(copy, Source::File { path: "/pics/007/a.jpg".into() }, "a.jpg", "JPEG", 6, 4, "2026-01-02")) })
        .unwrap();
    assert_eq!(library_files(&c, &files).get(Path::new("/pics/007/a.jpg")).map(|(id, _)| *id), Some(copy));
}

/// Given a library and a folder on disk that holds a file the library has by its path, one it
/// has by its content under another name, and a new one, when the folder is scanned (the whole
/// scan, as the import review runs it), then each is found for what it is.
#[test]
fn a_scan_finds_what_the_library_has_by_path_and_by_content() {
    let dir = std::env::temp_dir().join(format!("lc-known-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let at = |name: &str| dir.join(name).to_string_lossy().to_string();
    for name in ["here.jpg", "copy.jpg", "new.jpg"] {
        std::fs::write(dir.join(name), name.as_bytes()).unwrap();
    }
    let mut s = Session::new();
    s.catalog = library(20);
    let here = s.catalog.alloc_photo_id();
    s.catalog
        .apply(Op::AddPhoto { photo: Box::new(Photo::new(here, Source::File { path: at("here.jpg") }, "here.jpg", "JPEG", 6, 4, "2026-01-01")) })
        .unwrap();
    // (the probe: every file is a picture whose content is its name; `copy.jpg` has the content of a library photo)
    let had = s.catalog.photos().find(|p| p.file_name == "b.jpg").map(|p| (p.id, p.content_hash.clone().unwrap())).unwrap();
    let shared = had.1.clone();
    s.media.file_probe = Some(std::sync::Arc::new(move |path: &str| {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let content_hash = Some(if name == "copy.jpg" { shared.clone() } else { format!("hash-of-{name}") });
        Ok(ProbeInfo { format: "JPEG".into(), width: 6, height: 4, file_size: 8, content_hash, ..Default::default() })
    }));
    let found = scan(&mut s, &[dir.to_string_lossy().to_string()]);
    let of = |name: &str| found.iter().find(|c| c.name == name).map(|c| (c.duplicate.as_deref(), c.existing)).unwrap();
    assert_eq!(of("here.jpg"), (Some("path"), Some(here.0)));
    assert_eq!(of("copy.jpg"), (Some("content"), Some(had.0.0)));
    assert_eq!(of("new.jpg"), (None, None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the hashes of the files a scan probed, when the library is asked who has them, then it
/// answers for those hashes only; and where two photos have one content, the same one of them
/// as a table of the whole library would name (the later one).
#[test]
fn a_scan_knows_who_has_the_contents_it_probed() {
    let c = library(100);
    // (the content of /pics/006/a.jpg, which /pics/007/c.jpg has too; and one nobody has)
    let shared = format!("{:032x}", 6 * 3);
    let nobody = format!("{:032x}", 999_999);
    let known = library_hashes(&c, [shared.as_str(), nobody.as_str()].into_iter());
    assert_eq!(known.len(), 1);
    let whole: HashMap<String, PhotoId> = c.photos().filter_map(|p| p.content_hash.clone().map(|h| (h, p.id))).collect();
    assert_eq!(known.get(&shared), whole.get(&shared));
    assert!(matches!(&c.photo(known[&shared]).unwrap().source, Source::File { path } if path == "/pics/007/c.jpg"));
}

/// Given a session with a library, when an import is set up, then no table of the library is
/// made on the spot (that is the session's thread); it is made when the first files are
/// prepared, and then finds a file the library has by its path and one it has by its content.
#[test]
fn an_import_looks_the_library_up_when_it_prepares_files() {
    let mut s = Session::new();
    s.catalog = library(50);
    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
    assert!(job.by_path.is_empty() && job.by_hash.is_empty() && job.library.is_some(), "nothing is looked up yet");
    let out = job.prepare_files(vec!["/pics/007/a.jpg".into()], &std::sync::atomic::AtomicBool::new(false));
    assert!(job.library.is_none(), "looked up once");
    assert_eq!((job.by_path.len(), job.by_hash.len()), (150, 100));
    assert!(matches!(out.items.as_slice(), [PreparedItem::Duplicate { reason: "path", existing: Some(_), .. }]), "found by its path");
    // what the options said when the import was set up decides, whatever is made of them after
    let id = s.catalog.photos().next().unwrap().id;
    s.catalog.apply(Op::SetDeleted { id, deleted: true }).unwrap();
    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, on_deleted: OnDeleted::Fresh, ..ImportOptions::default() }).unwrap();
    job.opts.on_deleted = OnDeleted::Skip;
    job.prepare_files(Vec::new(), &std::sync::atomic::AtomicBool::new(false));
    assert_eq!(job.by_path.len(), 149, "the trashed photo was passed over, as set up");
}

/// Given a photo in Recently Deleted and an import that replaces such photos with a fresh
/// import, when the library is looked up, then that photo is no duplicate (as before the lookup
/// moved to the first files).
#[test]
fn a_trashed_photo_is_not_looked_up_when_imports_replace_it() {
    let mut s = Session::new();
    s.catalog = library(2);
    // (a photo whose content is its own: nobody else in this library has it)
    let id = s.catalog.photos().find(|p| p.file_name == "b.jpg").unwrap().id;
    s.catalog.apply(Op::SetDeleted { id, deleted: true }).unwrap();
    for (on_deleted, expected) in [(OnDeleted::Fresh, 5), (OnDeleted::Skip, 6)] {
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, on_deleted, ..ImportOptions::default() }).unwrap();
        job.prepare_files(Vec::new(), &std::sync::atomic::AtomicBool::new(false));
        assert_eq!(job.by_path.len(), expected, "{on_deleted:?}");
        let its_content = s.catalog.photo(id).and_then(|p| p.content_hash.clone()).unwrap();
        assert_eq!(job.by_hash.contains_key(&its_content), on_deleted == OnDeleted::Skip, "{on_deleted:?}");
    }
}
