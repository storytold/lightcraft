use super::*;
use std::cell::RefCell;

/// Windows has no file id in std: two spellings of one file are the same file whichever is
/// asked about first (a save after a partial rename saw `Trip.XMP` as a collision with the
/// known `Trip.xmp`), and only the spelling on disk is listed.
#[cfg(windows)]
#[test]
fn same_file_spellings_are_symmetric_on_windows() {
    let dir = std::env::temp_dir().join(format!("lc-same-file-spelling-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (real, alias, other) = (dir.join("Trip.xmp"), dir.join("Trip.XMP"), dir.join("other.xmp"));
    std::fs::write(&real, b"packet").unwrap();
    std::fs::write(&other, b"packet").unwrap();
    assert!(RealFs.same_file(&real, &alias) && RealFs.same_file(&alias, &real));
    assert!(!RealFs.same_file(&real, &other) && !RealFs.same_file(&other, &alias));
    assert!(RealFs.listed(&real) && !RealFs.listed(&alias));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The cross-volume move's check compares files chunk by chunk: equal files across several
/// chunks, a difference in the last chunk, different lengths, a missing file.
#[test]
fn same_contents_compares_in_chunks() {
    let dir = std::env::temp_dir().join(format!("lc-same-contents-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let big: Vec<u8> = (0..(2usize << 20) + 123).map(|i| (i % 251) as u8).collect();
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::write(&a, &big).unwrap();
    std::fs::write(&b, &big).unwrap();
    assert!(same_contents(&a, &b).unwrap());
    let mut other = big.clone();
    *other.last_mut().unwrap() ^= 1;
    std::fs::write(&b, &other).unwrap();
    assert!(!same_contents(&a, &b).unwrap());
    std::fs::write(&b, &big[..big.len() - 1]).unwrap();
    assert!(!same_contents(&a, &b).unwrap());
    std::fs::write(&a, b"").unwrap();
    std::fs::write(&b, b"").unwrap();
    assert!(same_contents(&a, &b).unwrap());
    assert!(same_contents(&a, &dir.join("missing")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model volume: files by path with contents; case-insensitive (default APFS/NTFS) or
/// case-sensitive (Linux, case-sensitive APFS). `fail` makes renames from a matching source fail.
pub(crate) struct FakeFs {
    pub ci: bool,
    pub files: RefCell<Vec<(String, Vec<u8>)>>,
    pub fail: RefCell<Vec<String>>,
    /// Renames made so far, and the numbers (from 0) of the ones that fail: a failure at a given moment, which a
    /// source path alone cannot name (a file moved back is renamed from the path it was first moved from).
    pub renames: std::cell::Cell<usize>,
    pub fail_renames: RefCell<Vec<usize>>,
}

impl FakeFs {
    pub fn new(ci: bool, files: &[(&str, &str)]) -> FakeFs {
        FakeFs {
            ci,
            files: RefCell::new(files.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect()),
            fail: RefCell::new(Vec::new()),
            renames: std::cell::Cell::new(0),
            fail_renames: RefCell::new(Vec::new()),
        }
    }
    /// The model volume spells paths with `/`; the code under test joins them with the host's separator.
    fn norm(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }
    fn idx(&self, p: &Path) -> Option<usize> {
        let p = Self::norm(p);
        self.files.borrow().iter().position(|(f, _)| {
            let f = f.replace('\\', "/");
            if self.ci { folded_path(&f) == folded_path(&p) } else { f == p }
        })
    }
    /// The listing: (exact path, contents), sorted.
    pub fn listing(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = self.files.borrow().iter().map(|(p, c)| (p.replace('\\', "/"), String::from_utf8_lossy(c).to_string())).collect();
        v.sort();
        v
    }
}

impl MoveFs for FakeFs {
    fn packet_version(&self, path: &Path) -> std::io::Result<PacketVersion> {
        let i = self.idx(path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such packet"))?;
        self.files.borrow().get(i).map(|(_, bytes)| PacketVersion::of(bytes)).ok_or_else(|| std::io::Error::other("packet disappeared"))
    }
    fn exists(&self, p: &Path) -> bool {
        self.idx(p).is_some()
    }
    fn is_file(&self, p: &Path) -> bool {
        self.exists(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        self.idx(a).is_some() && self.idx(a) == self.idx(b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        let nth = self.renames.get();
        self.renames.set(nth + 1);
        if self.fail_renames.borrow().contains(&nth) || self.fail.borrow().iter().any(|f| Self::norm(a).contains(&f.replace('\\', "/"))) {
            return Err(std::io::Error::other("injected failure"));
        }
        if self.exists(b) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists"));
        }
        let i = self.idx(a).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        self.files.borrow_mut()[i].0 = Self::norm(b);
        Ok(())
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if self.fail.borrow().iter().any(|f| Self::norm(a).contains(&f.replace('\\', "/"))) {
            return Err(std::io::Error::other("injected failure"));
        }
        if self.exists(b) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists"));
        }
        let i = self.idx(a).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        let c = self.files.borrow()[i].1.clone();
        self.files.borrow_mut().push((Self::norm(b), c));
        Ok(())
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        let i = self.idx(p).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        self.files.borrow_mut().remove(i);
        Ok(())
    }
    fn listed(&self, p: &Path) -> bool {
        let p = Self::norm(p);
        self.files.borrow().iter().any(|(f, _)| f.replace('\\', "/") == p)
    }
    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        let stem = Self::norm(&sidecar.with_extension(""));
        self.files.borrow().iter().map(|(f, _)| f.clone()).find(|f| {
            let p = Path::new(f);
            Self::norm(&p.with_extension("")) == stem && !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
        })
    }
}

fn file_photo(id: u64, path: &str) -> Photo {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    // the host's separator, so `dir.join(name)` in the planner spells the same path the same way
    let path = path.replace('/', std::path::MAIN_SEPARATOR_STR);
    Photo::new(PhotoId(id), Source::File { path }, &name, "JPG", 1, 1, "2026-01-01T00:00:00")
}

/// Compare the unfinished operation and every observed packet after partial progress.
/// Companion order is incidental; paths and recorded content versions are exact.
pub(crate) fn assert_repair_packets(actual: &SidecarMove, unfinished: &SidecarMove, packets: &[(PathBuf, &[u8])]) {
    let mut expected = unfinished.clone();
    expected.companions =
        packets.iter().map(|(path, bytes)| RetainedPacket { path: path.clone(), version: Some(PacketVersion::of(bytes)) }).collect();
    let mut actual = actual.clone();
    actual.companions.sort_by(|a, b| a.path.cmp(&b.path));
    expected.companions.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(actual, expected);
}

#[test]
fn folder_moves_repair_retained_sidecars_before_changing_paths() {
    for command in ["folder.rename", "folder.move"] {
        for queue in ["pending", "undo", "redo"] {
            for (naming, copied) in [(SidecarNaming::Stem, false), (SidecarNaming::Full, false), (SidecarNaming::Stem, true)] {
                let root = crate::tests_xmp::temp_dir("folder-sidecar-repair");
                let from = root.join("Trip");
                std::fs::create_dir_all(&from).unwrap();
                let photo = from.join("b.jpg");
                std::fs::write(&photo, b"Photo bytes").unwrap();
                let old_sidecar = crate::sidecar::sidecar_path(&from.join("a.jpg").to_string_lossy(), naming);
                let sidecar = crate::sidecar::sidecar_path(&photo.to_string_lossy(), naming);
                std::fs::write(&old_sidecar, b"Retained edits").unwrap();
                // An ordinary occupied destination must block repair without replacing its bytes.
                std::fs::write(&sidecar, b"Other edits").unwrap();
                let mut s = Session::new();
                s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, &photo.to_string_lossy())) }).unwrap();
                if copied {
                    let sibling = from.join("a.raw");
                    std::fs::write(&sibling, b"Shared original").unwrap();
                    s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(2, &sibling.to_string_lossy())) }).unwrap();
                }
                s.commit("Rating", Op::SetRating { id: PhotoId(1), rating: 3 }).unwrap();
                if queue == "redo" {
                    s.undo_step().unwrap();
                }
                let repair = SidecarMove {
                    from: old_sidecar.clone(),
                    to: sidecar.clone(),
                    copied,
                    companions: vec![RetainedPacket { path: old_sidecar.clone(), version: Some(RealFs.packet_version(&old_sidecar).unwrap()) }],
                };
                match queue {
                    "undo" => s.undo.last_mut().unwrap().retry_sidecars.push(repair),
                    "redo" => s.redo.last_mut().unwrap().retry_sidecars.push(repair),
                    _ => s.pending_sidecar_repairs.push(repair),
                }
                let params = if command == "folder.rename" {
                    serde_json::json!({"path": from, "name": "Holiday"})
                } else {
                    serde_json::json!({"path": from, "into": root.join("Archive")})
                };
                let to = if command == "folder.rename" { root.join("Holiday") } else { root.join("Archive").join("Trip") };
                let before = s.catalog.to_snapshot();
                let history = (s.undo.len(), s.redo.len());
                assert!(s.execute(command, &params).is_err());
                assert!(from.is_dir() && !to.exists());
                assert_eq!(s.catalog.to_snapshot(), before);
                assert_eq!((s.undo.len(), s.redo.len()), history);
                assert_eq!(std::fs::read(&old_sidecar).unwrap(), b"Retained edits");
                assert_eq!(std::fs::read(&sidecar).unwrap(), b"Other edits");
                std::fs::remove_file(&sidecar).unwrap();
                s.execute(command, &params).unwrap();
                assert!(s.pending_sidecar_repairs.is_empty());
                assert!(s.undo.iter().chain(&s.redo).all(|e| e.retry_sidecars.is_empty()));
                let moved_sidecar = to.join(sidecar.file_name().unwrap());
                assert_eq!(std::fs::read(&moved_sidecar).unwrap(), b"Retained edits");
                assert_eq!(to.join(old_sidecar.file_name().unwrap()).exists(), copied);
                for (step, current) in [("edit.undo", &from), ("edit.redo", &to)] {
                    s.execute(step, &serde_json::json!({})).unwrap();
                    assert_eq!(
                        s.catalog.photo(PhotoId(1)).unwrap().source,
                        Source::File { path: current.join("b.jpg").to_string_lossy().into_owned() }
                    );
                    assert_eq!(std::fs::read(current.join(sidecar.file_name().unwrap())).unwrap(), b"Retained edits");
                    assert_eq!(current.join(old_sidecar.file_name().unwrap()).exists(), copied);
                }
                // Explicit IDs choose targets after the command's selection gate.
                s.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
                s.execute("photo.rename", &serde_json::json!({"ids": [1], "template": "Latest"})).unwrap();
                s.execute("edit.undo", &serde_json::json!({})).unwrap();
                assert!(to.join("b.jpg").is_file());
                assert_eq!(std::fs::read(&moved_sidecar).unwrap(), b"Retained edits");
                s.execute("edit.redo", &serde_json::json!({})).unwrap();
                assert!(to.join("Latest.jpg").is_file());
                let final_sidecar = crate::sidecar::sidecar_path(&to.join("Latest.jpg").to_string_lossy(), naming);
                assert_eq!(std::fs::read(final_sidecar).unwrap(), b"Retained edits");
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn mixed_separator_noop_rename_preserves_files_catalog_and_undo() {
    let fs = FakeFs::new(true, &[("N:/photos/IMG.jpg", "image"), ("N:/photos/IMG.xmp", "edits"), ("N:/photos/other.jpg", "other")]);
    // Any attempt to move the unchanged photo (including rollback) must fail.
    fs.fail.borrow_mut().push("IMG".into());
    let mut s = Session::new();
    // spelled with `/` on purpose (not `file_photo`, which uses the host's separator)
    for (id, path) in [(1, "N:/photos/IMG.jpg"), (2, "N:/photos/other.jpg")] {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let photo = Photo::new(PhotoId(id), Source::File { path: path.into() }, &name, "JPG", 1, 1, "2026-01-01T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let before = fs.listing();
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "{name}", 1);
    assert_ne!(plans[0].from_path, plans[0].to_path, "fixture exercises different separator spelling");
    assert_eq!(s.apply_rename_with(&fs, &plans).unwrap(), 0);
    assert_eq!(fs.listing(), before);
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: "N:/photos/IMG.jpg".into() });
    assert!(s.undo.is_empty());
    move_file_with(&fs, "N:/photos/IMG.jpg", "N:/photos\\IMG.jpg").unwrap();
    let separator_only = Op::SetFile { id: PhotoId(1), file_name: "IMG.jpg".into(), source: Source::File { path: "N:/photos\\IMG.jpg".into() } };
    assert!(s.file_moves(&separator_only).is_empty());

    // A batch may combine that no-op with a genuine rename, without touching IMG or its sidecar.
    let mut batch = plans;
    batch.extend(s.plan_rename_with(&fs, &[PhotoId(2)], "renamed", 1));
    assert_eq!(s.apply_rename_with(&fs, &batch).unwrap(), 1);
    assert_eq!(
        fs.listing(),
        vec![
            ("N:/photos/IMG.jpg".into(), "image".into()),
            ("N:/photos/IMG.xmp".into(), "edits".into()),
            ("N:/photos/renamed.jpg".into(), "other".into())
        ]
    );
    let undo = &s.undo.last().unwrap().op;
    let moves = s.file_moves(undo);
    assert_eq!(moves.len(), 1);
    assert_eq!(Path::new(&moves[0].1), Path::new("N:/photos/other.jpg"));
    move_all(&fs, &moves).unwrap();
    s.catalog.apply(undo.clone()).unwrap();
    assert_eq!(fs.listing(), before);
}

/// Issue #95: a case-only rename is a real change on a case-insensitive volume and never
/// replaces a different file that differs only by case on a case-sensitive one.
#[test]
fn case_only_renames_never_replace_another_file() {
    // case-sensitive: IMG_1.JPG and img_1.JPG are two photos
    let fs = FakeFs::new(false, &[("/p/IMG_1.JPG", "first"), ("/p/img_1.JPG", "second"), ("/p/IMG_1.xmp", "sc1"), ("/p/img_1.xmp", "sc2")]);
    assert!(move_file_with(&fs, "/p/IMG_1.JPG", "/p/img_1.JPG").unwrap_err().message.contains("already exists"));
    assert_eq!(fs.listing().len(), 4);
    assert!(fs.listing().contains(&("/p/img_1.JPG".into(), "second".into())));
    // the planner picks a free name instead
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, "/p/IMG_1.JPG")) }).unwrap();
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "img_1", 1);
    assert_eq!(plans[0].to, "img_1-1.JPG");
    move_file_with(&fs, "/p/IMG_1.JPG", plans[0].to_path.as_deref().unwrap()).unwrap();
    assert_eq!(
        fs.listing(),
        vec![
            ("/p/img_1-1.JPG".into(), "first".into()),
            ("/p/img_1-1.xmp".into(), "sc1".into()),
            ("/p/img_1.JPG".into(), "second".into()),
            ("/p/img_1.xmp".into(), "sc2".into())
        ]
    );
    // case-sensitive, target free: a plain rename
    let fs = FakeFs::new(false, &[("/p/IMG_2.JPG", "x")]);
    move_file_with(&fs, "/p/IMG_2.JPG", "/p/img_2.JPG").unwrap();
    assert_eq!(fs.listing(), vec![("/p/img_2.JPG".into(), "x".into())]);

    // case-insensitive: the "existing" target is the photo itself → renamed via a temp name
    let fs = FakeFs::new(true, &[("/p/IMG_1.JPG", "first"), ("/p/IMG_1.xmp", "sc1")]);
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, "/p/IMG_1.JPG")) }).unwrap();
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "img_1", 1);
    assert_eq!(plans[0].to, "img_1.JPG", "not a collision with itself");
    move_file_with(&fs, "/p/IMG_1.JPG", "/p/img_1.JPG").unwrap();
    assert_eq!(fs.listing(), vec![("/p/img_1.JPG".into(), "first".into()), ("/p/img_1.xmp".into(), "sc1".into())]);
    // case-insensitive, another file under another name: still a collision
    let fs = FakeFs::new(true, &[("/p/A.JPG", "a"), ("/p/b.JPG", "b")]);
    assert!(move_file_with(&fs, "/p/A.JPG", "/p/B.JPG").is_err());
}

/// On a case-insensitive volume (Windows, macOS) `a.XMP` answers for `a.xmp`: a renamed
/// photo's sidecars keep the spelling they have on disk, and a failure on the real spelling
/// is the one reported and queued.
#[test]
fn case_insensitive_renames_keep_each_sidecars_spelling() {
    let fs = FakeFs::new(true, &[("/p/a.jpg", "A"), ("/p/a.xmp", "stem"), ("/p/a.jpg.XMP", "full"), ("/p/b.jpg", "B"), ("/p/b.Xmp", "mixed")]);
    move_file_with(&fs, "/p/a.jpg", "/p/c.jpg").unwrap();
    move_file_with(&fs, "/p/b.jpg", "/p/d.jpg").unwrap();
    assert_eq!(
        fs.listing(),
        vec![
            ("/p/c.jpg".into(), "A".into()),
            ("/p/c.jpg.XMP".into(), "full".into()),
            ("/p/c.xmp".into(), "stem".into()),
            ("/p/d.jpg".into(), "B".into()),
            ("/p/d.xmp".into(), "mixed".into())
        ]
    );
    // the move of the sidecar's real spelling is the one attempted (and here fails): the photo
    // goes back and nothing changed
    let before = fs.listing();
    *fs.fail.borrow_mut() = vec!["/p/c.xmp".into()];
    let e = move_file_with(&fs, "/p/c.jpg", "/p/a.jpg").unwrap_err();
    assert!(e.message.replace('\\', "/").contains("rename /p/c.xmp: injected failure"), "{e}");
    assert!(!e.moved && e.pending_sidecars.is_empty());
    assert_eq!(fs.listing(), before);
}

/// Issue #105: a rollback that fails is reported, and the library follows the files that kept
/// their new names (an undoable partial rename), so nothing goes missing.
#[test]
fn failed_rollbacks_are_reported_and_followed() {
    let fs = FakeFs::new(false, &[("/p/a.jpg", "A"), ("/p/a.xmp", "As"), ("/p/b.jpg", "B"), ("/p/c.jpg", "C")]);
    let mut s = Session::new();
    for (i, f) in ["/p/a.jpg", "/p/b.jpg", "/p/c.jpg"].iter().enumerate() {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(i as u64 + 1, f)) }).unwrap();
    }
    let ids = [PhotoId(1), PhotoId(2), PhotoId(3)];
    let plans = s.plan_rename_with(&fs, &ids, "Trip-{seq}", 1);
    // c can't be renamed (the share went away), and neither can Trip-1 be moved back
    fs.fail.borrow_mut().extend(["c.jpg".to_string(), "Trip-1".to_string()]);
    let err = s.apply_rename_with(&fs, &plans).unwrap_err().to_string().replace('\\', "/");
    assert!(err.contains("rename /p/c.jpg"), "{err}");
    assert!(err.contains("/p/Trip-1.jpg could not be moved back"), "{err}");
    assert!(err.contains("/p/a.jpg → /p/Trip-1.jpg"), "lists what stayed renamed: {err}");
    assert_eq!(
        fs.listing(),
        vec![
            ("/p/Trip-1.jpg".into(), "A".into()),
            ("/p/Trip-1.xmp".into(), "As".into()),
            ("/p/b.jpg".into(), "B".into()),
            ("/p/c.jpg".into(), "C".into())
        ]
    );
    // the catalog matches the disk
    let path = |s: &Session, id: u64| match &s.catalog.photo(PhotoId(id)).unwrap().source {
        Source::File { path } => path.replace('\\', "/"),
        Source::Demo { .. } => String::new(),
    };
    assert_eq!(path(&s, 1), "/p/Trip-1.jpg");
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().file_name, "Trip-1.jpg");
    assert_eq!((path(&s, 2), path(&s, 3)), ("/p/b.jpg".into(), "/p/c.jpg".into()));
    assert_eq!(s.undo.last().map(|e| e.label.as_str()), Some("Rename 1 Photo (partly)"));

    // a sidecar that won't move, and a file that won't go back: the file keeps its new name
    // with the sidecars that did move, and says so
    let fs = FakeFs::new(false, &[("/p/d.jpg", "D"), ("/p/d.xmp", "Ds"), ("/p/d.jpg.xmp", "Df")]);
    fs.fail.borrow_mut().extend(["d.jpg.xmp".to_string(), "/p/e.jpg".to_string()]);
    let e = move_file_with(&fs, "/p/d.jpg", "/p/e.jpg").unwrap_err();
    assert!(e.moved, "{e}");
    assert!(e.message.replace('\\', "/").contains("it is now /p/e.jpg with its sidecar /p/e.xmp"), "{e}");
    assert_eq!(fs.listing(), vec![("/p/d.jpg.xmp".into(), "Df".into()), ("/p/e.jpg".into(), "D".into()), ("/p/e.xmp".into(), "Ds".into())]);
    // … and when the file does go back, its sidecars follow it
    let fs = FakeFs::new(false, &[("/p/d.jpg", "D"), ("/p/d.xmp", "Ds"), ("/p/d.jpg.xmp", "Df")]);
    fs.fail.borrow_mut().push("d.jpg.xmp".to_string());
    let e = move_file_with(&fs, "/p/d.jpg", "/p/e.jpg").unwrap_err();
    assert!(!e.moved);
    assert_eq!(fs.listing().len(), 3);
    assert!(fs.listing().iter().all(|(p, _)| p.starts_with("/p/d.")));
}

#[test]
fn pending_sidecars_preserve_shared_copies_and_do_not_repeat_case_aliases() {
    // These are synthetic volume semantics; no host driver or OS runtime is involved.
    for ci in [false, true] {
        let fs = FakeFs::new(ci, &[("/p/a.CR3", "Raw"), ("/p/a.JPG", "Jpeg"), ("/p/a.XMP", "Shared edits"), ("/p/a.CR3.XMP", "Raw edits")]);
        fs.fail.borrow_mut().extend(["/p/a.XMP".into(), "/p/b.CR3".into()]);
        let mut error = move_file_with(&fs, "/p/a.CR3", "/p/b.CR3").unwrap_err();
        assert!(error.moved);
        assert_eq!(error.pending_sidecars.len(), 2);
        assert!(error.pending_sidecars[0].copied);
        assert!(!error.pending_sidecars[1].copied);
        fs.fail.borrow_mut().clear();
        retry_sidecars(&fs, &mut error.pending_sidecars).unwrap();
        assert!(error.pending_sidecars.is_empty());
        assert_eq!(
            fs.listing(),
            vec![
                ("/p/a.JPG".into(), "Jpeg".into()),
                ("/p/a.XMP".into(), "Shared edits".into()),
                ("/p/b.CR3".into(), "Raw".into()),
                ("/p/b.CR3.XMP".into(), "Raw edits".into()),
                ("/p/b.XMP".into(), "Shared edits".into()),
            ]
        );
    }
}

/// Issue #92 (comment): renaming one of `IMG_1.CR3` + `IMG_1.JPG` copies their shared
/// `IMG_1.xmp` instead of taking it away from the other; the last one renamed takes it.
#[test]
fn shared_stem_sidecar_is_copied_not_taken() {
    let fs = FakeFs::new(false, &[("/p/IMG_1.CR3", "raw"), ("/p/IMG_1.JPG", "jpg"), ("/p/IMG_1.xmp", "shared"), ("/p/IMG_1.CR3.xmp", "own")]);
    move_file_with(&fs, "/p/IMG_1.CR3", "/p/Trip_1.CR3").unwrap();
    assert_eq!(
        fs.listing(),
        vec![
            ("/p/IMG_1.JPG".into(), "jpg".into()),
            ("/p/IMG_1.xmp".into(), "shared".into()),
            ("/p/Trip_1.CR3".into(), "raw".into()),
            ("/p/Trip_1.CR3.xmp".into(), "own".into()),
            ("/p/Trip_1.xmp".into(), "shared".into()),
        ]
    );
    // the JPEG is the last one using it: it moves
    move_file_with(&fs, "/p/IMG_1.JPG", "/p/Trip_2.JPG").unwrap();
    assert!(fs.listing().contains(&("/p/Trip_2.xmp".into(), "shared".into())));
    assert!(!fs.exists(Path::new("/p/IMG_1.xmp")));
    // a failure after the copy removes the copy and keeps the original
    let fs = FakeFs::new(false, &[("/p/A.CR3", "raw"), ("/p/A.JPG", "jpg"), ("/p/A.xmp", "shared"), ("/p/A.CR3.xmp", "own")]);
    fs.fail.borrow_mut().push("A.CR3.xmp".into());
    assert!(move_file_with(&fs, "/p/A.CR3", "/p/B.CR3").is_err());
    assert_eq!(fs.listing().len(), 4);
    assert!(fs.listing().contains(&("/p/A.xmp".into(), "shared".into())) && !fs.exists(Path::new("/p/B.xmp")));
}

/// Issue #95 on the real disk, whichever kind of volume the temp folder is on.
#[test]
fn case_only_rename_on_disk() {
    let dir = std::env::temp_dir().join(format!("lc-rename-case-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (upper, lower) = (dir.join("IMG_1.JPG"), dir.join("img_1.JPG"));
    std::fs::write(&upper, b"first").unwrap();
    let case_sensitive = !lower.exists();
    let p = |q: &Path| q.to_string_lossy().to_string();
    if case_sensitive {
        std::fs::write(&lower, b"second").unwrap();
        assert!(move_file(&p(&upper), &p(&lower)).is_err());
        assert_eq!(std::fs::read(&lower).unwrap(), b"second");
        assert_eq!(std::fs::read(&upper).unwrap(), b"first");
    } else {
        move_file(&p(&upper), &p(&lower)).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["img_1.JPG".to_string()]);
        assert_eq!(std::fs::read(&lower).unwrap(), b"first");
    }
    // a plain rename never replaces
    std::fs::write(dir.join("other.JPG"), b"other").unwrap();
    assert!(move_file(&p(&dir.join("other.JPG")), &p(&lower)).is_err());
    assert_eq!(std::fs::read(dir.join("other.JPG")).unwrap(), b"other");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_templates_stay_inside_the_destination() {
    let mut p = sample_photo();
    assert_eq!(expand_folder("{date:%Y}/{date:%Y%m%d}", &p, 1), vec!["2026", "20260114"]);
    assert_eq!(expand_folder("{date:%Y}\\{date:%Y-%m}", &p, 1), vec!["2026", "2026-01"], "backslash separates too");
    assert_eq!(expand_folder("{date:%Y}/{date:%B}", &p, 1), vec!["2026", "January"]);
    assert_eq!(expand_folder("{date:%Y}/{date:%m %b}", &p, 1), vec!["2026", "01 Jan"]);
    // metadata can't add levels or climb out
    p.meta.camera = "../../etc/x".into();
    p.meta.title = "..".into();
    assert_eq!(expand_folder("{camera}/{title}/{date:%Y}", &p, 1), vec!["-..-etc-x", "unknown", "2026"]);
    p.meta.camera = "C:\\Windows".into();
    assert_eq!(expand_folder("{camera}", &p, 1), vec!["C--Windows"]);
    // missing metadata → `unknown`; missing capture time → the import time
    p.meta.camera.clear();
    p.captured = None;
    assert_eq!(expand_folder("{camera}//{date:%Y%m%d}", &p, 1), vec!["unknown", "20260120"]);
    for bad in ["", "  ", "/abs/{date}", "\\\\server\\x", "C:/x", "c:x", "~/x", "../{date}", "{date}/../x", "a/./b", "{date:%Y}/.."] {
        assert!(folder_template_error(bad).is_some(), "{bad:?} should be rejected");
    }
    for ok in ["{date:%Y}/{date:%Y%m%d}", "Trips/{camera}", "{date}", "a//b"] {
        assert_eq!(folder_template_error(ok), None, "{ok:?}");
    }
    // even unchecked, rejected levels are skipped rather than followed
    assert_eq!(expand_folder("/../{date:%Y}/./x", &p, 1), vec!["2026", "x"]);
}

/// The help lists exactly what `expand_tokens` understands: every listed tag (and alias) is
/// replaced, never left literal, and the help's examples match the documented behaviour.
#[test]
fn token_help_matches_the_implementation() {
    let p = sample_photo();
    for t in TOKENS {
        for tag in std::iter::once(&t.tag).chain(t.aliases) {
            let v = expand_tokens(tag, &p, 1, 1);
            assert!(!v.contains('{'), "{tag} is not a known token (expanded to {v:?})");
            assert!(!v.is_empty(), "{tag}: the sample photo should give an example");
        }
    }
    assert_eq!(token_example("{seq:3}"), "001");
    assert_eq!(token_example("{date}"), "20260114");
    assert_eq!(token_example("{date:%Y%m%d_%H%M%S}"), "20260114_055848");
    assert_eq!(token_example("{ext}"), "CR3");
    assert_eq!(expand("{date:%Y%m%d_%H%M%S}_{seq:3}", &p, 1), "20260114_055848_001.CR3");
    for (d, _) in DATE_DIRECTIVES {
        let v = format_date("2026-01-14T05:58:48", d);
        assert!(!v.contains('%') || *d == "%%", "{d} is not a known directive");
    }
    assert_eq!(format_date("2026-09-30T14:05:00", "%B %b"), "September Sep");
    assert_eq!(format_date("2026-12-01T00:00:00", "%d %B %Y"), "01 December 2026");
    // a month that isn't 01–12 gives no name instead of panicking
    for bad in ["2026-00-01", "2026-13-01", "2026-x", "", "2026-é1-01"] {
        assert_eq!(format_date(bad, "%B|%b"), "|", "{bad:?}");
    }
    assert_eq!(unknown_tokens("{date}_{camra}-{seq:2}{x}{camra}"), vec!["{camra}".to_string(), "{x}".to_string()]);
    assert!(unknown_tokens("{name}{ext}{date:%Y}").is_empty());
    let json = token_help_json();
    assert_eq!(json["tokens"].as_array().unwrap().len(), TOKENS.len());
    assert_eq!(json["tokens"][0]["example"], "IMG_0042");
    // the command descriptions that list the tokens list all of them
    for id in ["photo.rename", "photo.renamePreview", "library.import"] {
        let spec = crate::cmd::command_specs().iter().find(|c| c.id == id).unwrap();
        for t in TOKENS.iter().filter(|t| !t.tag.contains(':')) {
            assert!(spec.params.contains(t.tag), "{id} does not mention {}", t.tag);
        }
    }
}

#[test]
fn templates() {
    let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 1 }, "IMG_0042.CR2", "CR2", 1, 1, "2026-01-01T00:00:00");
    p.captured = Some("2026-09-30T14:05:09".into());
    p.meta.camera = "Model X/2".into();
    p.meta.title = "Sunset: beach".into();
    assert_eq!(expand("{name}", &p, 1), "IMG_0042.CR2");
    assert_eq!(expand("Trip-{seq:3}", &p, 7), "Trip-007.CR2");
    assert_eq!(expand("{date}_{name}", &p, 1), "20260930_IMG_0042.CR2");
    assert_eq!(expand("{date:%Y-%m-%d %H.%M.%S}", &p, 1), "2026-09-30 14.05.09.CR2");
    assert_eq!(expand("{camera} {title}", &p, 1), "Model X-2 Sunset- beach.CR2");
    assert_eq!(expand("{unknown}-{seq}", &p, 12), "{unknown}-12.CR2");
    assert_eq!(expand("  ", &p, 1), "IMG_0042.CR2", "empty template keeps the name");
    assert_eq!(expand("../{name}", &p, 1), "-IMG_0042.CR2", "no path components");
}

#[test]
fn number_folder_and_metadata_tokens() {
    let path = std::path::Path::new("shoots").join("2026-09 Coast").join("DSC_0815.NEF");
    let mut p = Photo::new(PhotoId(1), Source::File { path: path.to_string_lossy().into() }, "DSC_0815.NEF", "NEF", 1, 1, "2026-01-01T00:00:00");
    p.meta.lens = "24-70mm F2.8".into();
    p.meta.iso = Some(400);
    p.meta.creator = "A. Person".into();
    p.rating = 4;
    assert_eq!(expand("{folder}_{num}", &p, 1), "2026-09 Coast_0815.NEF");
    assert_eq!(expand("{rating}star-{iso}-{lens}-{creator}", &p, 1), "4star-400-24-70mm F2.8-A. Person.NEF");
    // no number at the end of the name / no folder: empty
    p.file_name = "beach.NEF".into();
    p.source = Source::Demo { scene: 0 };
    assert_eq!(expand("{folder}{name}{num}", &p, 1), "beach.NEF");
    // a bare {seq} is padded to the caller's width
    assert_eq!(expand_tokens("{seq}|{seq:2}", &p, 7, 3), "007|07");
}
