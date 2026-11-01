//! The store's side of a snapshot in several files (`crate::segments`): the segments folder is
//! made with its first file, files are written under names that don't exist yet, listed and
//! synced. Nothing in the catalog writes or reads them yet.

use crate::segments::segment_file;
use crate::*;

/// The plain write of the store (a whole file given at once) puts a segment in its folder too.
#[test]
fn every_way_of_writing_a_segment_makes_its_folder() {
    let dir = std::env::temp_dir().join(format!("lc-segments-plain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    store.write_atomic(&segment_file("1-1"), b"{}").unwrap();
    assert_eq!(std::fs::read(dir.join("segments/1-1.json")).unwrap(), b"{}");
    assert_eq!(store.list().unwrap(), ["segments/1-1.json"]);
    store.remove(&segment_file("1-1")).unwrap();
    assert!(store.list().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file written with `write_new_with` is durable in itself, leaves no staging file, is
/// listed by the name the catalog gives it, and `sync` has nothing to say about a folder that can
/// be synced.
#[test]
fn a_file_written_for_a_later_write_to_name_is_listed_and_synced() {
    let dir = std::env::temp_dir().join(format!("lc-segments-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    assert!(store.segmented());
    let n = store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"[1]")).unwrap();
    assert_eq!(n, 3);
    store.write_new_with(&segment_file("2-1"), &mut |w| w.write_all(b"[2]")).unwrap();
    store.sync().unwrap();
    assert_eq!(std::fs::read(dir.join("segments/1-1.json")).unwrap(), b"[1]");
    let mut listed = store.list().unwrap();
    listed.sort();
    assert_eq!(listed, ["segments/1-1.json", "segments/2-1.json"]);
    assert!(!dir.join("segments/1-1.json.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A write that fails leaves nothing of the file behind, and no folder entry names it.
#[test]
fn a_write_that_fails_leaves_no_file() {
    let dir = std::env::temp_dir().join(format!("lc-segments-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    let e = store.write_new_with(&segment_file("1-1"), &mut |_| Err(std::io::Error::other("no room"))).unwrap_err();
    assert!(e.to_string().contains("no room"), "{e}");
    assert!(store.list().unwrap().is_empty());
    assert!(!dir.join("segments/1-1.json.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The segments folder is the library's own: a file of that name is an error that says so, for
/// writing and for listing (what is listed may be removed).
#[test]
fn a_file_where_the_segments_folder_goes_is_refused() {
    let dir = std::env::temp_dir().join(format!("lc-segments-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    std::fs::write(dir.join("segments"), b"not a folder").unwrap();
    let e = store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"[]")).unwrap_err();
    assert!(e.to_string().contains("is not a folder"), "{e}");
    let e = store.write_atomic(&segment_file("1-1"), b"[]").unwrap_err();
    assert!(e.to_string().contains("is not a folder"), "{e}");
    let e = store.list().unwrap_err();
    assert!(e.to_string().contains("is not a folder"), "{e}");
    assert_eq!(std::fs::read(dir.join("segments")).unwrap(), b"not a folder");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A link to a folder elsewhere is not the library's own folder either: nothing is written
/// through it and nothing in it is listed.
#[cfg(unix)]
#[test]
fn a_link_where_the_segments_folder_goes_is_refused() {
    let base = std::env::temp_dir().join(format!("lc-segments-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (dir, elsewhere) = (base.join("library"), base.join("elsewhere"));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut store = FsStore::open(&dir).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("segments")).unwrap();
    let e = store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"[]")).unwrap_err();
    assert!(e.to_string().contains("is not a folder"), "{e}");
    assert!(store.list().is_err());
    std::fs::write(elsewhere.join("1-1.json"), b"theirs").unwrap();
    let e = store.remove(&segment_file("1-1")).unwrap_err();
    assert!(e.to_string().contains("is not a folder"), "{e}");
    assert_eq!(std::fs::read(elsewhere.join("1-1.json")).unwrap(), b"theirs", "nothing was removed through the link");
    std::fs::remove_file(elsewhere.join("1-1.json")).unwrap();
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing went through the link");
    let _ = std::fs::remove_dir_all(&base);
}

/// What exists today is unchanged: a plain file is written whole, in the library folder, and
/// `write_atomic_with` reports its length, as before (it does not make the segments folder).
#[test]
fn the_streamed_write_of_a_plain_file_is_what_it_was() {
    let dir = std::env::temp_dir().join(format!("lc-segments-plain-with-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    let n = store.write_atomic_with("catalog.snap", &mut |w| w.write_all(b"{}\n")).unwrap();
    assert_eq!(n, 3);
    assert_eq!(std::fs::read(dir.join("catalog.snap")).unwrap(), b"{}\n");
    assert!(!dir.join("segments").exists());
    assert_eq!(store.list().unwrap(), ["catalog.snap"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The in-memory store lists what it holds, and holds segments unless it is made to keep one file.
#[test]
fn the_memory_store_lists_its_files() {
    let mut store = MemStore::new();
    assert!(store.segmented());
    store.write_atomic("catalog.snap", b"{}").unwrap();
    assert_eq!(store.list().unwrap(), ["catalog.snap"]);
    store.remove("catalog.snap").unwrap();
    assert!(store.list().unwrap().is_empty());
}

/// A name that is taken is an error and its file keeps what it had: a second `write_new_with`
/// never replaces (a plain rename would), and leaves no staging file.
#[test]
fn writing_a_new_file_never_replaces_one_that_exists() {
    let dir = std::env::temp_dir().join(format!("lc-segments-exists-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"first")).unwrap();
    let e = store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"second")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(std::fs::read(dir.join("segments/1-1.json")).unwrap(), b"first");
    assert!(!dir.join("segments/1-1.json.tmp").exists());
    // (a file of the library folder too, and a file that appeared meanwhile)
    std::fs::write(dir.join("catalog.snap"), b"theirs").unwrap();
    let e = store.write_new_with("catalog.snap", &mut |w| w.write_all(b"ours")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(std::fs::read(dir.join("catalog.snap")).unwrap(), b"theirs");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The memory store holds to the same contract, and a store that doesn't override the method
/// refuses rather than replace.
#[test]
fn the_memory_store_does_not_replace_a_new_file_either() {
    let mut store = MemStore::new();
    assert_eq!(store.write_new_with("segments/1-1.json", &mut |w| w.write_all(b"first")).unwrap(), 5);
    let e = store.write_new_with("segments/1-1.json", &mut |w| w.write_all(b"second")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(store.get("segments/1-1.json").unwrap(), b"first");

    struct Plain(MemStore);
    impl Store for Plain {
        fn read(&mut self, n: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.0.read(n)
        }
        fn write_atomic(&mut self, n: &str, d: &[u8]) -> std::io::Result<()> {
            self.0.write_atomic(n, d)
        }
        fn append(&mut self, n: &str, d: &[u8]) -> std::io::Result<()> {
            self.0.append(n, d)
        }
        fn truncate(&mut self, n: &str, l: u64) -> std::io::Result<()> {
            self.0.truncate(n, l)
        }
        fn describe(&self) -> String {
            String::new()
        }
    }
    let mut plain = Plain(MemStore::new());
    plain.write_atomic("a", b"kept").unwrap();
    let e = plain.write_new_with("a", &mut |w| w.write_all(b"x")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(plain.read("a").unwrap().unwrap(), b"kept");
}

/// Names that leave the library's folder are refused before the file system is touched, by
/// every operation that takes a name; the names the catalog uses still work.
#[test]
fn names_that_leave_the_library_are_refused() {
    let base = std::env::temp_dir().join(format!("lc-segments-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("library");
    let mut store = FsStore::open(&dir).unwrap();
    std::fs::write(base.join("outside"), b"keep").unwrap();
    let outside = base.join("outside").display().to_string();
    for name in [
        "../outside",
        "segments/../../outside",
        "a/b",
        "segments/a/b",
        "segments/",
        "segments",
        "",
        ".",
        "..",
        "segments/..",
        "/etc/passwd",
        outside.as_str(),
        "other/x.json",
        "a\\b",
    ] {
        let bad = |e: std::io::Error| assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{name:?}: {e}");
        bad(store.write_new_with(name, &mut |w| w.write_all(b"x")).unwrap_err());
        bad(store.write_atomic(name, b"x").unwrap_err());
        bad(store.write_atomic_with(name, &mut |w| w.write_all(b"x")).unwrap_err());
        bad(store.read(name).unwrap_err());
        bad(store.append(name, b"x").unwrap_err());
        bad(store.truncate(name, 0).unwrap_err());
        bad(store.remove(name).unwrap_err());
    }
    assert_eq!(std::fs::read(base.join("outside")).unwrap(), b"keep");
    assert!(std::fs::read_dir(&base).unwrap().count() == 2, "nothing was made beside the library");
    store.write_atomic("catalog.snap", b"{}").unwrap();
    store.write_new_with(&segment_file("1-1"), &mut |w| w.write_all(b"[]")).unwrap();
    store.write_atomic("catalog.log.damaged-3", b"x").unwrap();
    assert_eq!(store.read("catalog.snap").unwrap().unwrap(), b"{}");
    let _ = std::fs::remove_dir_all(&base);
}
