//! Moving photos to a folder on a NAS or network share (`photo.moveToFolder`, which dropping them
//! on a Folders row runs), where the share can go away between the move and its undo:
//!
//! * The share offline when Undo runs: Undo is refused with the reason and keeps the step;
//!   nothing is deleted, the library still points at the files where they are, and once the share
//!   is back the same Undo moves them home.
//! * The share read-only when Undo runs: the same.
//! * The connection lost partway through a move: the files already moved go back, the library is
//!   unchanged and no history step is left.
//! * One file the share can't give back during Undo: the files already brought back go out again,
//!   so every photo points at its file and the step can run once the share answers.
//! * Across devices (a share is another volume): the file is copied, checked and only then
//!   removed; the copy goes to a hidden name and takes the photo's name only once complete.

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("local")).unwrap();
        std::fs::create_dir_all(dir.join("share")).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).components().collect::<std::path::PathBuf>().to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn add_file(s: &mut Session, path: &str, body: &str) -> PhotoId {
    std::fs::write(path, body).unwrap();
    let id = s.catalog.alloc_photo_id();
    let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let p = Photo::new(id, Source::File { path: path.into() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn file_of(s: &Session, id: PhotoId) -> String {
    match &s.catalog.photo(id).unwrap().source {
        Source::File { path } => path.clone(),
        _ => panic!("not a file"),
    }
}

/// Two photos on the local disk moved to the share.
fn moved_to_share(dir: &Scratch) -> (Session, PhotoId, PhotoId) {
    let mut s = Session::new();
    let a = add_file(&mut s, &dir.path("local/a.jpg"), "A");
    std::fs::write(dir.path("local/a.xmp"), "<edits/>").unwrap();
    let b = add_file(&mut s, &dir.path("local/b.jpg"), "B");
    let r = s.execute("photo.moveToFolder", &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap();
    assert_eq!(r["moved"], 2);
    (s, a, b)
}

#[test]
fn undo_with_the_share_offline_is_refused_keeps_the_step_and_works_once_it_is_back() {
    let dir = Scratch::new("offline");
    let (mut s, a, b) = moved_to_share(&dir);
    let steps = s.undo.len();
    // the share goes offline: its folder is no longer there
    let away = dir.0.join("share-unmounted");
    std::fs::rename(dir.0.join("share"), &away).unwrap();
    let e = s.execute("edit.undo", &json!({})).unwrap_err().to_string();
    assert!(e.contains("can't move the files back"), "says why: {e}");
    assert_eq!(s.undo.len(), steps, "the step stays, to undo once the share is back");
    // nothing deleted, nothing half-moved, and the library still points at the share
    assert!(away.join("a.jpg").is_file() && away.join("a.xmp").is_file() && away.join("b.jpg").is_file());
    assert!(!dir.0.join("local/a.jpg").exists() && !dir.0.join("local/b.jpg").exists());
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    assert_eq!(file_of(&s, b), dir.path("share/b.jpg"));
    // the share is back: the same Undo moves them home, sidecar and all
    std::fs::rename(&away, dir.0.join("share")).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(std::fs::read_to_string(dir.path("local/a.jpg")).unwrap(), "A");
    assert!(dir.0.join("local/a.xmp").is_file() && dir.0.join("local/b.jpg").is_file());
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    assert_eq!(file_of(&s, b), dir.path("local/b.jpg"));
}

#[cfg(unix)]
#[test]
fn undo_from_a_read_only_share_is_refused_and_deletes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("readonly");
    let (mut s, a, _) = moved_to_share(&dir);
    let steps = s.undo.len();
    std::fs::set_permissions(dir.0.join("share"), std::fs::Permissions::from_mode(0o555)).unwrap();
    // running as root ignores permissions: nothing to check there
    let root = std::fs::write(dir.0.join("share/probe"), "x").is_ok();
    if !root {
        assert!(s.execute("edit.undo", &json!({})).is_err());
        assert_eq!(s.undo.len(), steps, "the step stays");
        assert!(dir.0.join("share/a.jpg").is_file() && dir.0.join("share/b.jpg").is_file(), "nothing deleted");
        assert!(!dir.0.join("local/a.jpg").exists(), "no second copy left behind");
        assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    }
    std::fs::set_permissions(dir.0.join("share"), std::fs::Permissions::from_mode(0o755)).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dir.0.join("local/a.jpg").is_file());
}

/// The model volume spells paths with `/`; the code under test joins them with the host's separator.
fn host(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

fn model_session(fs_files: &[&str]) -> (Session, Vec<PhotoId>) {
    let mut s = Session::new();
    let ids = fs_files
        .iter()
        .map(|path| {
            let id = s.catalog.alloc_photo_id();
            let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_string();
            let p = Photo::new(id, Source::File { path: host(path) }, &name, "JPEG", 1, 1, "2026-01-01T00:00:00");
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id
        })
        .collect();
    (s, ids)
}

fn plans(s: &Session, ids: &[PhotoId], into: &str) -> Vec<crate::rename::RenamePlan> {
    ids.iter()
        .map(|id| {
            let from = file_of(s, *id);
            let name = std::path::Path::new(&from).file_name().unwrap().to_string_lossy().to_string();
            crate::rename::RenamePlan {
                id: id.0,
                from: name.clone(),
                to: name.clone(),
                from_path: Some(from),
                to_path: Some(host(&format!("{into}/{name}"))),
            }
        })
        .collect()
}

/// Every photo points at a file the model volume holds (or, for `offline`, one it held).
fn assert_linked(s: &Session, fs: &crate::rename::tests::FakeFs, offline: &[&str]) {
    let files = fs.listing();
    for p in s.catalog.photos() {
        let Source::File { path } = &p.source else { continue };
        let path = path.replace('\\', "/");
        assert!(
            files.iter().any(|(f, _)| *f == path) || offline.contains(&path.as_str()),
            "photo {} points at {path}, which isn't there: {files:?}",
            p.id.0
        );
    }
}

#[test]
fn a_connection_lost_partway_through_a_move_puts_back_what_moved() {
    let fs = crate::rename::tests::FakeFs::new(false, &[("/local/a.jpg", "A"), ("/local/b.jpg", "B"), ("/local/c.jpg", "C")]);
    let (mut s, ids) = model_session(&["/local/a.jpg", "/local/b.jpg", "/local/c.jpg"]);
    let plans = plans(&s, &ids, "/share");
    // the share stops answering when the second file goes
    fs.fail.borrow_mut().push("/local/b.jpg".into());
    let steps = s.undo.len();
    assert!(s.apply_file_plans(&fs, &plans, &|_| "Move to Folder".into()).is_err());
    assert_eq!(
        fs.listing(),
        vec![("/local/a.jpg".into(), "A".into()), ("/local/b.jpg".into(), "B".into()), ("/local/c.jpg".into(), "C".into())],
        "all back home"
    );
    assert_eq!(s.undo.len(), steps, "no step for a move that didn't happen");
    assert_linked(&s, &fs, &[]);
}

#[test]
fn a_file_the_share_cannot_give_back_rolls_the_whole_undo_back() {
    let fs = crate::rename::tests::FakeFs::new(false, &[("/local/a.jpg", "A"), ("/local/b.jpg", "B")]);
    let (mut s, ids) = model_session(&["/local/a.jpg", "/local/b.jpg"]);
    let plans = plans(&s, &ids, "/share");
    s.apply_file_plans(&fs, &plans, &|_| "Move to Folder".into()).unwrap();
    assert_eq!(fs.listing(), vec![("/share/a.jpg".into(), "A".into()), ("/share/b.jpg".into(), "B".into())]);
    // b can't be read back: the share answers for a but not for b
    fs.fail.borrow_mut().push("/share/b.jpg".into());
    let e = s.undo_step_with(&fs).unwrap_err().to_string();
    assert!(e.contains("can't move the files back"), "{e}");
    // a came back before b failed: it went out again, so the step is whole and every photo
    // points at its file, none lost
    assert_linked(&s, &fs, &[]);
    let contents: Vec<String> = fs.listing().into_iter().map(|(_, c)| c).collect();
    assert_eq!(contents, vec!["A".to_string(), "B".to_string()], "both files still exist once: {:?}", fs.listing());
    // the share answers again: undoing whatever is left brings both home
    fs.fail.borrow_mut().clear();
    while s.undo_step_with(&fs).is_ok() && fs.listing().iter().any(|(p, _)| p.starts_with("/share")) {}
    assert_eq!(fs.listing(), vec![("/local/a.jpg".into(), "A".into()), ("/local/b.jpg".into(), "B".into())]);
    assert_linked(&s, &fs, &[]);
}

/// Where this machine has a second file system to write to (Linux's `/dev/shm`), a real move
/// across devices: copied, checked, the original removed, and Undo copies it back. Skipped where
/// there is none (a network share can't be mounted in a test).
#[test]
fn a_move_across_devices_copies_checks_and_removes_and_undoes_the_same_way() {
    let dir = Scratch::new("devices");
    let Some(other) = other_device(&dir.0) else { return };
    let share = other.join(format!("lc-share-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&share);
    std::fs::create_dir_all(&share).unwrap();
    let mut s = Session::new();
    let body: String = (0..300_000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let a = add_file(&mut s, &dir.path("local/a.jpg"), &body);
    let r = s.execute("photo.moveToFolder", &json!({"ids": [a.0], "folder": share.to_string_lossy()}));
    let moved =
        r.is_ok() && std::fs::read_to_string(share.join("a.jpg")).ok().as_deref() == Some(body.as_str()) && !dir.0.join("local/a.jpg").exists();
    let undone = moved
        && s.execute("edit.undo", &json!({})).is_ok()
        && std::fs::read_to_string(dir.0.join("local/a.jpg")).ok().as_deref() == Some(body.as_str());
    let gone_from_share = !share.join("a.jpg").exists();
    let _ = std::fs::remove_dir_all(&share);
    assert!(moved, "moved across devices: {r:?}");
    assert!(undone, "and back");
    assert!(gone_from_share, "no copy left on the other device");
}

/// A writable folder on another device than `here`, if there is one.
fn other_device(here: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(here).ok()?.dev();
        ["/dev/shm", "/run/shm"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| {
                std::fs::metadata(p).is_ok_and(|m| m.dev() != dev)
                    && std::fs::write(p.join(format!(".lc-probe-{}", std::process::id())), b"x").is_ok()
            })
            .inspect(|p| {
                let _ = std::fs::remove_file(p.join(format!(".lc-probe-{}", std::process::id())));
            })
    }
    #[cfg(not(unix))]
    {
        let _ = here;
        None
    }
}

#[test]
fn the_copy_across_volumes_is_whole_or_absent_and_never_replaces() {
    use crate::rename::MoveFs;
    let dir = Scratch::new("copy");
    let body: Vec<u8> = (0..(3usize << 20) + 17).map(|i| (i % 251) as u8).collect();
    let (from, to) = (dir.0.join("local/big.raw"), dir.0.join("share/big.raw"));
    std::fs::write(&from, &body).unwrap();
    crate::rename::RealFs.copy_no_replace(&from, &to).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), body);
    // nothing left under a hidden name beside it
    let left: Vec<_> = std::fs::read_dir(dir.0.join("share")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("big.raw")], "{left:?}");
    // a partial left by a copy the share cut off doesn't block the next copy
    let stale = dir.0.join(format!("share/.again.raw.lc-partial-{}-0", std::process::id()));
    std::fs::write(&stale, b"cut off").unwrap();
    crate::rename::RealFs.copy_no_replace(&from, &dir.0.join("share/again.raw")).unwrap();
    assert_eq!(std::fs::read(dir.0.join("share/again.raw")).unwrap(), body);
    std::fs::remove_file(&stale).unwrap();
    std::fs::remove_file(dir.0.join("share/again.raw")).unwrap();
    // a copy that can't start (its source unreadable) leaves nothing at all under the photo's name
    let gone = dir.0.join("local/missing.raw");
    assert!(crate::rename::RealFs.copy_no_replace(&gone, &dir.0.join("share/missing.raw")).is_err());
    assert!(!dir.0.join("share/missing.raw").exists());
    let left: Vec<_> = std::fs::read_dir(dir.0.join("share")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left.len(), 1, "no partial file left: {left:?}");
    // a name taken is never replaced, and what is there stays
    std::fs::write(&to, b"other").unwrap();
    assert!(crate::rename::RealFs.copy_no_replace(&from, &to).is_err());
    assert_eq!(std::fs::read(&to).unwrap(), b"other");
}
