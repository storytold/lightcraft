//! An import asks the disk for a photo's sidecar only where the folder's listing leaves room for
//! one: on a network share every name asked about is a round trip, found or not.
use super::*;
use crate::sidecar::SidecarNaming;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-listed-sidecars-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Make `dir` and the folders in it an hour old, so the walk vouches for their times
/// (`walk::TIME_GRAIN`, two seconds) without the test waiting.
fn settle(dir: &Path) {
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            settle(&e.path());
        }
    }
    set_folder_time(dir, hour_ago);
}

/// Set a folder's modification time (Unix: a plain handle; Windows: one opened for attributes,
/// with backup semantics, which is how a folder is opened there).
fn set_folder_time(dir: &Path, at: std::time::SystemTime) {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS
        std::fs::OpenOptions::new().write(true).access_mode(0x100).custom_flags(0x0200_0000).open(dir)
    };
    #[cfg(not(windows))]
    let handle = std::fs::File::open(dir);
    handle.unwrap().set_modified(at).unwrap();
}

fn modified(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

fn xmp(rating: u8) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/></rdf:RDF></x:xmpmeta>"#
    )
}

/// An import of files that are not read (the probe says "a JPEG") with the naming `naming`.
fn job(naming: SidecarNaming) -> ImportJob {
    let mut s = Session::new();
    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
    job.naming = naming;
    job.probe = Some(std::sync::Arc::new(|_: &str| Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })));
    job
}

/// (file name → the rating its sidecar gave, `None`: no sidecar) in the order imported.
fn ratings(p: Prepared) -> Vec<(String, Option<u8>)> {
    p.items
        .into_iter()
        .map(|i| match i {
            PreparedItem::Ready(r) => (Path::new(&r.path).file_name().unwrap().to_string_lossy().to_string(), r.sidecar.and_then(|sc| sc.rating)),
            other => panic!("not ready: {other:?}"),
        })
        .collect()
}

/// Given folders listed completely that hold no `.xmp`, when their files are imported, then no
/// sidecar name is asked of the disk (it was nine names for every file); and files named one by
/// one, whose folders nobody listed, are asked about as before.
#[test]
fn a_listed_folder_without_sidecars_is_not_asked_about_them() {
    let root = scratch("none");
    let mut named = Vec::new();
    for folder in 0..50 {
        let dir = root.join(format!("f{folder:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..20 {
            let f = dir.join(format!("IMG_{n:04}.jpg"));
            std::fs::write(&f, b"x").unwrap();
            named.push(f.to_string_lossy().to_string());
        }
    }
    settle(&root);
    let cancel = AtomicBool::new(false);

    let mut listed = job(SidecarNaming::Stem);
    let p = listed.prepare(&[root.to_string_lossy().to_string()], &cancel);
    assert_eq!(p.items.len(), 1000);
    assert_eq!(listed.listed.asked.load(Relaxed), 0, "1,000 files in 50 listed folders: no disk probe");

    let mut by_name = job(SidecarNaming::Stem);
    let p = by_name.prepare(&named, &cancel);
    assert_eq!(p.items.len(), 1000);
    assert_eq!(by_name.listed.asked.load(Relaxed), 1000, "files named directly keep their disk probes");
    let _ = std::fs::remove_dir_all(&root);
}

/// Given sidecars in every case and naming, when the folder is imported, then each is read, and
/// only the photo without one is not asked about.
#[test]
fn sidecars_in_the_listing_are_read_in_any_case() {
    for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
        let root = scratch("cases");
        let write = |name: &str, rating: u8| std::fs::write(root.join(name), xmp(rating)).unwrap();
        for p in ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg", "f.jpg"] {
            std::fs::write(root.join(p), b"x").unwrap();
        }
        write("a.xmp", 1);
        write("b.XMP", 2);
        write("c.Xmp", 3);
        write("d.jpg.xmp", 4);
        write("e.JPG.XMP", 5);
        settle(&root);
        let mut job = job(naming);
        let found = ratings(job.prepare(&[root.to_string_lossy().to_string()], &AtomicBool::new(false)));
        let expect: Vec<(String, Option<u8>)> =
            [("a.jpg", Some(1)), ("b.jpg", Some(2)), ("c.jpg", Some(3)), ("d.jpg", Some(4)), ("e.jpg", Some(5)), ("f.jpg", None)]
                .into_iter()
                .map(|(n, r)| (n.to_string(), r))
                .collect();
        let mut found = found;
        found.sort();
        assert_eq!(found, expect, "{naming:?}");
        assert_eq!(job.listed.asked.load(Relaxed), 5, "{naming:?}: the five with a sidecar are asked about, f.jpg is not");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A folder the walk did not list completely, or whose names the listing can't compare as the
/// disk does (not ASCII), is not trusted: its photos are asked about as before.
#[test]
fn a_listing_that_cannot_say_leaves_the_disk_to_decide() {
    let root = scratch("cannot-say");
    let (whole, partial, accents) = (root.join("whole"), root.join("partial"), root.join("accents"));
    for d in [&whole, &partial, &accents] {
        std::fs::create_dir_all(d).unwrap();
    }
    let in_ = |d: &Path, n: &str| d.join(n).to_string_lossy().to_string();
    let mut l = listed_folders::ListedFolders::default();
    let file = |n: &str| (std::ffi::OsString::from(n), crate::walk::EntryKind::File);
    l.note(&partial, vec![file("a.jpg")], false, modified(&partial));
    l.note(&accents, vec![file("é.jpg"), file("a.jpg")], true, modified(&accents));
    l.note(&whole, vec![file("a.jpg"), file("b.xmp")], true, modified(&whole));
    let naming = SidecarNaming::Stem;
    // (a folder that is not on disk any more has no time to compare: asked about, too)
    l.note(Path::new("/p/gone"), vec![file("a.jpg")], true, Some(std::time::SystemTime::UNIX_EPOCH));
    // (a folder too recent to vouch for, as the walk reports it: no time)
    let recent = root.join("recent");
    std::fs::create_dir_all(&recent).unwrap();
    l.note(&recent, vec![file("a.jpg")], true, None);
    let cases = [
        in_(&partial, "a.jpg"),
        in_(&accents, "a.jpg"),
        in_(&root.join("unlisted"), "a.jpg"),
        in_(&whole, "b.jpg"),
        in_(&whole, "a~1.jpg"),
        in_(&whole, "nul.jpg"),
        "/p/gone/a.jpg".to_string(),
        in_(&recent, "a.jpg"),
    ];
    for original in &cases {
        let before = l.asked.load(Relaxed);
        assert!(l.find(original, naming).is_none());
        assert_eq!(l.asked.load(Relaxed), before + 1, "{original}: the disk is asked");
    }
    let before = l.asked.load(Relaxed);
    assert!(l.find(&in_(&whole, "a.jpg"), naming).is_none());
    assert_eq!(l.asked.load(Relaxed), before, "a listed folder with no a.xmp: the disk is not asked");
    let _ = std::fs::remove_dir_all(&root);
}

/// Given a review (the scan before the Add Photos dialog) of folders without sidecars, and an
/// import of the files it listed, when the files are handled, then neither a sidecar name nor
/// "is it a folder" is asked of the disk for them; and the sidecar the review's listing shows is
/// read.
#[test]
fn an_import_after_a_review_trusts_the_folders_the_review_listed() {
    let root = scratch("review");
    for folder in 0..20 {
        let dir = root.join(format!("f{folder:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..10 {
            std::fs::write(dir.join(format!("IMG_{n:04}.jpg")), b"x").unwrap();
        }
    }
    std::fs::write(root.join("f07").join("IMG_0003.xmp"), xmp(4)).unwrap();
    settle(&root);
    let mut s = Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|_: &str| Ok(ProbeInfo { format: "JPEG".into(), file_size: 1, ..ProbeInfo::default() })));
    let (input, paths) = ScanInput::new(&mut s, &[root.to_string_lossy().to_string()]);
    let out = scan_with(input, &paths, &ScanProgress::default());
    let selected: Vec<String> = out.candidates.iter().map(|c| c.path.clone()).collect();
    assert_eq!(selected.len(), 200);
    // (what the app does with the scan's output)
    s.import_probes = out.probes;
    let listed = out.listed;

    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap().with_listed(listed);
    let p = job.prepare(&selected, &AtomicBool::new(false));
    assert_eq!(job.listed.asked.load(Relaxed), 1, "only the photo with a sidecar in the listing is asked about");
    let found = ratings(p);
    assert_eq!(found.len(), 200);
    assert_eq!(found.iter().filter(|(_, r)| r.is_some()).collect::<Vec<_>>(), [&("IMG_0003.jpg".to_string(), Some(4))]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A path the listing calls a file is not asked about (here the disk holds a folder by that
/// name, which only a look at the disk would find) while its folder is as it was listed; one it
/// does not list still is.
#[test]
fn a_file_the_listing_does_not_know_is_still_checked() {
    let root = scratch("known");
    for name in ["known.jpg", "unknown.jpg"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("inner.jpg"), b"x").unwrap();
    }
    settle(&root);
    let mut job = job(SidecarNaming::Stem);
    // (the folder is as listed: its time is the one the listing holds)
    job.listed.note(&root, vec![("known.jpg".into(), crate::walk::EntryKind::File)], true, modified(&root));
    let (known, unknown) = (root.join("known.jpg"), root.join("unknown.jpg"));
    let files = job.expand(&[known.to_string_lossy().to_string(), unknown.to_string_lossy().to_string()]);
    assert_eq!(files, [known.to_string_lossy().to_string(), unknown.join("inner.jpg").to_string_lossy().to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Given a review whose listing was kept, when a sidecar appears in a folder afterwards (the
/// review was left open, or cancelled, and something else added it), then the import that gets
/// the listing still reads it: a folder that changed since it was listed is probed.
#[test]
fn a_folder_that_changed_since_it_was_listed_is_probed() {
    let root = scratch("stale");
    for n in 0..3 {
        std::fs::write(root.join(format!("a{n}.jpg")), b"x").unwrap();
    }
    settle(&root);
    let mut walked = ListedFolders::default();
    let (files, _) = expand_knowing(
        &[root.to_string_lossy().to_string()],
        None,
        crate::walk::Limits::default(),
        Some(&mut |dir, names, complete, modified| {
            walked.note(dir, names, complete, modified);
            true
        }),
        true,
        &|_| false,
    );
    assert_eq!(files.len(), 3);
    // a sidecar arrives, more than a clock tick after the listing
    std::fs::write(root.join("a1.xmp"), xmp(5)).unwrap();
    let mut job = job(SidecarNaming::Stem).with_listed(walked);
    let found = ratings(job.prepare_files(files, &AtomicBool::new(false)));
    assert_eq!(found.iter().filter(|(_, r)| *r == Some(5)).count(), 1, "{found:?}");
    assert_eq!(job.listed.asked.load(Relaxed), 3, "the folder is asked about, photo by photo");
    let _ = std::fs::remove_dir_all(&root);
}

/// A batch looks at the time of each folder its files are in, once: not once per file, and not
/// once per folder that was listed.
#[test]
fn a_batch_looks_at_each_of_its_folders_once() {
    let root = scratch("looks");
    for folder in 0..50 {
        let dir = root.join(format!("f{folder:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..20 {
            std::fs::write(dir.join(format!("IMG_{n:04}.jpg")), b"x").unwrap();
        }
    }
    settle(&root);
    let mut job = job(SidecarNaming::Stem);
    let files = job.expand(&[root.to_string_lossy().to_string()]);
    assert_eq!(files.len(), 1000);
    let in_folders = |names: &[&str]| -> Vec<String> {
        files.iter().filter(|f| Path::new(f).parent().and_then(Path::file_name).is_some_and(|d| names.iter().any(|n| d == *n))).cloned().collect()
    };
    let cancel = AtomicBool::new(false);
    let first = job.prepare_files(in_folders(&["f00", "f01"]), &cancel);
    assert_eq!(first.items.len(), 40);
    assert_eq!(job.listed.looked.load(Relaxed), 2, "40 files in 2 of the 50 listed folders");
    job.prepare_files(in_folders(&["f01", "f02", "f03"]), &cancel);
    assert_eq!(job.listed.looked.load(Relaxed), 2 + 3, "the next batch looks again, at its own 3");
    assert_eq!(job.listed.asked.load(Relaxed), 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// Given a folder listed in the same clock tick as its files were made, when a sidecar is
/// written into it right after the listing and the folder's time reads as it did before (a
/// volume that keeps folder times to the second or two), then the import still reads it: a
/// time too recent to vouch for is not trusted. (The test puts the folder's time back.)
#[test]
fn a_sidecar_written_in_the_tick_of_the_listing_is_read() {
    let root = scratch("racy");
    for n in 0..3 {
        std::fs::write(root.join(format!("a{n}.jpg")), b"x").unwrap();
    }
    let at_listing = modified(&root).unwrap();
    let mut walked = ListedFolders::default();
    let (files, _) = expand_knowing(
        &[root.to_string_lossy().to_string()],
        None,
        crate::walk::Limits::default(),
        Some(&mut |dir, names, complete, modified| {
            walked.note(dir, names, complete, modified);
            true
        }),
        true,
        &|_| false,
    );
    std::fs::write(root.join("a1.xmp"), xmp(5)).unwrap();
    set_folder_time(&root, at_listing);
    let mut job = job(SidecarNaming::Stem).with_listed(walked);
    let found = ratings(job.prepare_files(files, &AtomicBool::new(false)));
    assert_eq!(found.iter().filter(|(_, r)| *r == Some(5)).count(), 1, "{found:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Given a review whose listing was kept, when a listed file is replaced by a folder holding a
/// photo afterwards, then the import's expansion walks that folder: a name in a folder that
/// changed since it was listed is not trusted to still be a file.
#[test]
fn a_listed_file_replaced_by_a_folder_is_expanded() {
    let root = scratch("replaced");
    for n in 0..3 {
        std::fs::write(root.join(format!("a{n}.jpg")), b"x").unwrap();
    }
    settle(&root);
    let mut walked = ListedFolders::default();
    let (files, _) = expand_knowing(
        &[root.to_string_lossy().to_string()],
        None,
        crate::walk::Limits::default(),
        Some(&mut |dir, names, complete, modified| {
            walked.note(dir, names, complete, modified);
            true
        }),
        true,
        &|_| false,
    );
    assert_eq!(files.len(), 3);
    std::fs::remove_file(root.join("a1.jpg")).unwrap();
    std::fs::create_dir(root.join("a1.jpg")).unwrap();
    std::fs::write(root.join("a1.jpg").join("inside.jpg"), b"x").unwrap();
    let mut job = job(SidecarNaming::Stem).with_listed(walked);
    // (the paths handed to the import are the files the review listed)
    let found = job.expand(&files);
    assert!(found.iter().any(|f| f.ends_with("inside.jpg")), "{found:?}");
    assert_eq!(found.len(), 3, "{found:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder noted with no time (too recent to vouch for, or the volume keeps none) is not
/// trusted, and finding that out costs no look at the disk.
#[test]
fn a_folder_without_a_listed_time_costs_no_look() {
    let root = scratch("no-time");
    std::fs::write(root.join("a.jpg"), b"x").unwrap();
    let mut l = ListedFolders::default();
    l.note(&root, vec![("a.jpg".into(), crate::walk::EntryKind::File)], true, None);
    assert!(!l.is_known_file(&root.join("a.jpg")));
    assert_eq!(l.looked.load(Relaxed), 0);
    let _ = std::fs::remove_dir_all(&root);
}
