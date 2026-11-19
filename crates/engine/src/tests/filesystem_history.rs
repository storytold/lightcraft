//! Filesystem history and retained XMP repair regressions.

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Selection, Session};

fn select(s: &mut Session, ids: &[u64], active: u64) {
    s.execute("library.select", &json!({"ids": ids, "active": active})).unwrap();
}

fn memory_library(store: &lightcraft_catalog::MemStore) -> Session {
    let mut s = Session::new();
    s.open_library_in(
        crate::library::LibraryStores {
            dir: "history-regression".into(),
            catalog: Box::new(store.clone()),
            files: Box::new(lightcraft_catalog::MemStore::new()),
            on_disk: false,
        },
        false,
    )
    .unwrap();
    s
}

fn file_history_step(s: &mut Session, fs: &dyn crate::rename::MoveFs, redo: bool) -> crate::Result<serde_json::Value> {
    s.execute_fn(if redo { "edit.redo" } else { "edit.undo" }, |s| {
        let label = if redo { s.redo_step_with(fs)? } else { s.undo_step_with(fs)? };
        Ok(json!({"label": label}))
    })
}

/// The model volume spells paths with `/`; the code under test joins them with the host's separator.
fn slash(path: &str) -> String {
    path.replace('\\', "/")
}

fn file_source(s: &Session, id: u64) -> String {
    let Source::File { path } = &s.catalog.photo(PhotoId(id)).unwrap().source else { panic!("synthetic file fixture") };
    slash(path)
}

fn assert_file_catalog_and_journal(s: &Session, fs: &crate::rename::tests::FakeFs, store: &lightcraft_catalog::MemStore) {
    let files = fs.listing();
    for photo in s.catalog.photos() {
        let Source::File { path } = &photo.source else { panic!("synthetic file fixture") };
        assert!(files.iter().any(|(file, _)| *file == slash(path)), "photo {} points to missing {path}: {files:?}", photo.id.0);
        assert_eq!(std::path::Path::new(path).file_name().unwrap().to_string_lossy(), photo.file_name.as_str());
    }
    let replayed = memory_library(store);
    assert_eq!(replayed.catalog.to_snapshot(), s.catalog.to_snapshot());
}

fn sidecar_history_fixture(redo: bool) -> (Session, crate::rename::tests::FakeFs, lightcraft_catalog::MemStore) {
    let store = lightcraft_catalog::MemStore::new();
    let mut s = memory_library(&store);
    let fs = crate::rename::tests::FakeFs::new(
        false,
        &[
            ("/p/a.jpg", "Photo"),
            ("/p/a.XMP", "Upper stem edits"),
            ("/p/a.xmp", "Lower stem edits"),
            ("/p/a.jpg.XMP", "Upper full-name edits"),
            ("/p/a.jpg.xmp", "Lower full-name edits"),
            ("/p/other.jpg", "Other"),
        ],
    );
    for (i, path) in ["/p/a.jpg", "/p/other.jpg"].iter().enumerate() {
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        let photo = Photo::new(PhotoId(i as u64 + 1), Source::File { path: (*path).into() }, &name, "JPEG", 16, 16, "");
        s.commit("Synthetic File", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    s.persist().unwrap();
    s.undo.clear();
    select(&mut s, &[1], 1);
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).unwrap();
    if redo {
        file_history_step(&mut s, &fs, false).unwrap();
    }
    select(&mut s, &[2], 2);
    (s, fs, store)
}

fn sidecar_history_entry(s: &Session, redo: bool) -> &crate::UndoEntry {
    if redo { s.redo.last().unwrap() } else { s.undo.last().unwrap() }
}

#[test]
fn merged_renames_coalesce_physical_moves_and_keep_retry_inverses_and_journal() {
    use crate::rename::tests::FakeFs;
    use crate::sidecar::{SidecarNaming, sidecar_path};

    let store = lightcraft_catalog::MemStore::new();
    let mut s = memory_library(&store);
    let fs = FakeFs::new(false, &[("/p/a.jpg", "Photo"), ("/p/a.xmp", "Edits"), ("/p/other.jpg", "Other")]);
    for (id, path) in [(1, "/p/a.jpg"), (2, "/p/other.jpg")] {
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        let photo = Photo::new(PhotoId(id), Source::File { path: path.into() }, &name, "JPEG", 16, 16, "");
        s.commit("Synthetic File", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    select(&mut s, &[1], 1);
    let copy = s.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap();
    s.undo.clear();
    let before = fs.listing();
    for name in ["middle", "final"] {
        let plans = s.plan_rename_with(&fs, &[PhotoId(1)], name, 1);
        s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).unwrap();
        assert_file_catalog_and_journal(&s, &fs, &store);
    }
    let renamed = fs.listing();
    s.merge_undo(2, "Rename Twice");
    assert_eq!(s.undo.len(), 1);
    let moves: Vec<_> = s.file_moves(&s.undo.last().unwrap().op).iter().map(|(from, to)| (slash(from), slash(to))).collect();
    assert_eq!(moves, vec![("/p/final.jpg".to_string(), "/p/a.jpg".to_string())]);
    // First exercise the ordinary merged step, before any corrective paths exist.
    for redo in [false, true] {
        file_history_step(&mut s, &fs, redo).unwrap();
        assert_eq!(fs.listing(), if redo { renamed.clone() } else { before.clone() });
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, s.catalog.photo(PhotoId(copy)).unwrap().source);
        assert_file_catalog_and_journal(&s, &fs, &store);
    }
    for redo in [false, true, false] {
        select(&mut s, &[2], 2);
        let entry = if redo { s.redo.last().unwrap() } else { s.undo.last().unwrap() };
        let moves = s.file_moves(&entry.op);
        assert_eq!(moves.len(), 1, "virtual copies must share the final physical move");
        let (from, to) = &moves[0];
        let sidecar = sidecar_path(from, SidecarNaming::Stem).to_string_lossy().into_owned();
        *fs.fail.borrow_mut() = vec![sidecar, to.clone()];
        let history = (s.undo.len(), s.redo.len());
        assert!(file_history_step(&mut s, &fs, redo).is_err());
        assert_eq!((s.undo.len(), s.redo.len()), history);
        assert_eq!(s.selection, Selection::single(PhotoId(2)));
        assert_file_catalog_and_journal(&s, &fs, &store);
        fs.fail.borrow_mut().clear();
        file_history_step(&mut s, &fs, redo).unwrap();
        assert_eq!(fs.listing(), if redo { renamed.clone() } else { before.clone() });
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, s.catalog.photo(PhotoId(copy)).unwrap().source);
        assert_file_catalog_and_journal(&s, &fs, &store);
    }
}

#[test]
fn partial_file_history_failures_retry_without_losing_either_inverse_or_journal() {
    use crate::rename::tests::FakeFs;

    for redo in [false, true] {
        let store = lightcraft_catalog::MemStore::new();
        let mut s = memory_library(&store);
        let fs = FakeFs::new(
            false,
            &[
                ("/p/a.jpg", "A"),
                ("/p/a.xmp", "A edits"),
                ("/p/b.jpg", "B"),
                ("/p/b.xmp", "B edits"),
                ("/p/c.jpg", "C"),
                ("/p/c.xmp", "C edits"),
                ("/p/other.jpg", "Other"),
            ],
        );
        let ops = ["/p/a.jpg", "/p/b.jpg", "/p/c.jpg", "/p/other.jpg"]
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
                let photo = Photo::new(PhotoId(i as u64 + 1), Source::File { path: (*path).into() }, &name, "JPEG", 16, 16, "");
                Op::AddPhoto { photo: Box::new(photo) }
            })
            .collect();
        s.commit("Synthetic Files", Op::Batch { ops }).unwrap();
        // A virtual copy shares B's physical file and must follow every partial correction too.
        let mut copy = (**s.catalog.photo(PhotoId(2)).unwrap()).clone();
        copy.id = PhotoId(5);
        copy.copy_of = Some(PhotoId(2));
        s.commit("Synthetic Copy", Op::AddPhoto { photo: Box::new(copy) }).unwrap();
        s.persist().unwrap();
        s.undo.clear();
        select(&mut s, &[1, 2, 3], 1);
        let before = fs.listing();
        let plans = s.plan_rename_with(&fs, &[PhotoId(1), PhotoId(2), PhotoId(3)], "Trip-{seq}", 1);
        s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).unwrap();
        let renamed = fs.listing();
        if redo {
            file_history_step(&mut s, &fs, false).unwrap();
        }
        select(&mut s, &[4], 4);
        // Two bounded failures leave a different file moved on each attempt. A subsequent
        // failure with no moves must keep those corrections; a final retry round-trips them.
        for attempt in 0..2 {
            let entry = if redo { s.redo.last().unwrap() } else { s.undo.last().unwrap() };
            let moves = s.file_moves(&entry.op);
            assert_eq!(moves.len(), 3 - attempt);
            *fs.fail.borrow_mut() = vec![moves[1].0.clone(), moves[0].1.clone()];
            let history = (s.undo.len(), s.redo.len());
            let error = file_history_step(&mut s, &fs, redo).unwrap_err().to_string();
            assert!(error.contains("could not be moved back"), "{error}");
            assert_eq!((s.undo.len(), s.redo.len()), history);
            assert_eq!(s.selection, Selection::single(PhotoId(4)));
            assert_file_catalog_and_journal(&s, &fs, &store);
        }
        let entry = if redo { s.redo.last().unwrap() } else { s.undo.last().unwrap() };
        let moves = s.file_moves(&entry.op);
        assert_eq!(moves.len(), 1);
        *fs.fail.borrow_mut() = vec![moves[0].0.clone()];
        let unchanged = fs.listing();
        let history = (s.undo.len(), s.redo.len());
        assert!(file_history_step(&mut s, &fs, redo).is_err());
        assert_eq!(fs.listing(), unchanged);
        assert_eq!((s.undo.len(), s.redo.len()), history);
        assert_eq!(s.selection, Selection::single(PhotoId(4)));
        assert_file_catalog_and_journal(&s, &fs, &store);
        fs.fail.borrow_mut().clear();
        file_history_step(&mut s, &fs, redo).unwrap();
        assert_eq!(fs.listing(), if redo { renamed.clone() } else { before.clone() });
        assert_file_catalog_and_journal(&s, &fs, &store);
        file_history_step(&mut s, &fs, !redo).unwrap();
        assert_eq!(fs.listing(), if redo { before.clone() } else { renamed.clone() });
        assert_file_catalog_and_journal(&s, &fs, &store);
        file_history_step(&mut s, &fs, redo).unwrap();
        assert_eq!(fs.listing(), if redo { renamed } else { before });
        assert_file_catalog_and_journal(&s, &fs, &store);
    }
}

#[test]
fn history_retries_both_sidecar_styles_after_following_a_photo_with_failed_rollback() {
    use crate::rename::MoveFs;
    use crate::sidecar::{SidecarNaming, sidecar_path};

    for redo in [false, true] {
        for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
            let (mut s, fs, store) = sidecar_history_fixture(redo);
            let moves = s.file_moves(&sidecar_history_entry(&s, redo).op);
            assert_eq!(moves.len(), 1);
            let (from, to) = &moves[0];
            let from = from.clone();
            let to = to.clone();
            let before = fs.listing();
            let failing = sidecar_path(&from, naming).with_extension("XMP");
            *fs.fail.borrow_mut() = vec![failing.to_string_lossy().into_owned(), to.clone()];
            let history = (s.undo.len(), s.redo.len());
            let error = file_history_step(&mut s, &fs, redo).unwrap_err().to_string();
            assert!(error.contains("could not be moved back"), "{error}");
            assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: to.clone() });
            assert!(s.file_moves(&sidecar_history_entry(&s, redo).op).is_empty(), "the photo already followed its destination");
            let expected_pending = if naming == SidecarNaming::Stem { 4 } else { 2 };
            assert_eq!(sidecar_history_entry(&s, redo).retry_sidecars.len(), expected_pending);
            assert_file_catalog_and_journal(&s, &fs, &store);

            // Repeating the same XMP failure must not turn the empty photo move list into success.
            *fs.fail.borrow_mut() = vec![failing.to_string_lossy().into_owned()];
            let partial = fs.listing();
            let error = file_history_step(&mut s, &fs, redo).unwrap_err().to_string();
            assert!(error.contains("injected failure"), "{error}");
            assert_eq!(fs.listing(), partial);
            assert_eq!((s.undo.len(), s.redo.len()), history);
            assert_eq!(s.selection, Selection::single(PhotoId(2)));
            assert_eq!(sidecar_history_entry(&s, redo).retry_sidecars.len(), expected_pending);
            assert_file_catalog_and_journal(&s, &fs, &store);

            // Make progress, then fail at the other full-name spelling. Completed sidecars
            // must not be retried (their old names no longer exist).
            let last = sidecar_history_entry(&s, redo).retry_sidecars.last().unwrap().clone();
            *fs.fail.borrow_mut() = vec![last.from.to_string_lossy().into_owned()];
            assert!(file_history_step(&mut s, &fs, redo).is_err());
            assert_eq!(sidecar_history_entry(&s, redo).retry_sidecars.len(), 1);
            assert_eq!((s.undo.len(), s.redo.len()), history);
            assert_eq!(s.selection, Selection::single(PhotoId(2)));
            assert_file_catalog_and_journal(&s, &fs, &store);

            // A new destination collision stays an error and never replaces unrelated bytes.
            fs.fail.borrow_mut().clear();
            fs.files.borrow_mut().push((last.to.to_string_lossy().into_owned(), b"Unrelated edits".to_vec()));
            let occupied = fs.listing();
            assert!(file_history_step(&mut s, &fs, redo).is_err());
            assert_eq!(fs.listing(), occupied);
            assert_eq!((s.undo.len(), s.redo.len()), history);
            assert_eq!(sidecar_history_entry(&s, redo).retry_sidecars.len(), 1);
            assert_file_catalog_and_journal(&s, &fs, &store);
            fs.remove_file(&last.to).unwrap();

            file_history_step(&mut s, &fs, redo).unwrap();
            let mut after = before
                .iter()
                .map(|(path, bytes)| (path.replace(slash(&from).trim_end_matches(".jpg"), slash(&to).trim_end_matches(".jpg")), bytes.clone()))
                .collect::<Vec<_>>();
            after.sort();
            assert_eq!(fs.listing(), after);
            assert_file_catalog_and_journal(&s, &fs, &store);
            file_history_step(&mut s, &fs, !redo).unwrap();
            assert_eq!(fs.listing(), before);
            assert_file_catalog_and_journal(&s, &fs, &store);
            file_history_step(&mut s, &fs, redo).unwrap();
            assert_eq!(fs.listing(), after);
            assert_file_catalog_and_journal(&s, &fs, &store);
        }
    }
}

#[test]
fn history_repairs_a_sidecar_whose_rollback_failed_before_retrying_the_photo() {
    use crate::sidecar::{SidecarNaming, sidecar_path};

    for redo in [false, true] {
        let (mut s, fs, store) = sidecar_history_fixture(redo);
        let moves = s.file_moves(&sidecar_history_entry(&s, redo).op);
        let (from, to) = &moves[0];
        let from = from.clone();
        let to = to.clone();
        let before = fs.listing();
        // The stem sidecars move first; full-name failure rolls the photo back, but the
        // first stem sidecar cannot return. The catalog has not changed this time.
        let failed_full = sidecar_path(&from, SidecarNaming::Full).with_extension("XMP");
        let stranded = sidecar_path(&to, SidecarNaming::Stem).with_extension("XMP");
        *fs.fail.borrow_mut() = vec![failed_full.to_string_lossy().into_owned(), stranded.to_string_lossy().into_owned()];
        let history = (s.undo.len(), s.redo.len());
        let error = file_history_step(&mut s, &fs, redo).unwrap_err().to_string();
        assert!(error.contains("sidecar") && error.contains("could not be moved back"), "{error}");
        let entry = sidecar_history_entry(&s, redo);
        assert_eq!(entry.retry_sidecars.len(), 1);
        assert!(entry.retry_inverse.is_empty());
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: from });
        assert_file_catalog_and_journal(&s, &fs, &store);
        let partial = fs.listing();
        *fs.fail.borrow_mut() = vec![stranded.to_string_lossy().into_owned()];
        assert!(file_history_step(&mut s, &fs, redo).is_err());
        assert_eq!(fs.listing(), partial);
        assert_eq!((s.undo.len(), s.redo.len()), history);
        assert_eq!(s.selection, Selection::single(PhotoId(2)));
        fs.fail.borrow_mut().clear();
        file_history_step(&mut s, &fs, redo).unwrap();
        assert_file_catalog_and_journal(&s, &fs, &store);
        file_history_step(&mut s, &fs, !redo).unwrap();
        assert_eq!(fs.listing(), before);
        assert_file_catalog_and_journal(&s, &fs, &store);
    }
}

#[test]
fn initial_partial_rename_keeps_sidecar_repairs_for_noop_retry_and_history() {
    use crate::rename::tests::FakeFs;
    let store = lightcraft_catalog::MemStore::new();
    let mut s = memory_library(&store);
    let fs = FakeFs::new(false, &[("/p/a.jpg", "Photo"), ("/p/a.xmp", "Stem edits"), ("/p/a.jpg.xmp", "Full edits")]);
    let photo = Photo::new(PhotoId(1), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 16, 16, "");
    s.commit("Synthetic File", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.persist().unwrap();
    s.undo.clear();
    select(&mut s, &[1], 1);
    let before = fs.listing();
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    *fs.fail.borrow_mut() = vec!["/p/a.xmp".into(), "/p/Trip.jpg".into()];
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
    assert_eq!(file_source(&s, 1), "/p/Trip.jpg");
    assert_eq!(s.undo.len(), 1);
    assert_eq!(s.undo.last().unwrap().retry_sidecars.len(), 2);
    assert_file_catalog_and_journal(&s, &fs, &store);
    let retry = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    assert_eq!(retry[0].from_path, retry[0].to_path, "the physical photo already has the requested name");
    *fs.fail.borrow_mut() = vec!["/p/a.xmp".into()];
    let partial = fs.listing();
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &retry)?}))).is_err());
    assert_eq!(fs.listing(), partial);
    assert_eq!(s.undo.last().unwrap().retry_sidecars.len(), 2, "a failed no-op retry must keep its repair queue");
    fs.fail.borrow_mut().clear();
    assert_eq!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &retry)?}))).unwrap()["renamed"], 0);
    assert!(s.undo.last().unwrap().retry_sidecars.is_empty());
    assert_eq!(s.undo.len(), 1, "a repaired no-op retry adds no empty rename step");
    let after = fs.listing();
    assert_eq!(
        after,
        vec![("/p/Trip.jpg".into(), "Photo".into()), ("/p/Trip.jpg.xmp".into(), "Full edits".into()), ("/p/Trip.xmp".into(), "Stem edits".into())]
    );
    assert_file_catalog_and_journal(&s, &fs, &store);
    file_history_step(&mut s, &fs, false).unwrap();
    assert_eq!(fs.listing(), before);
    file_history_step(&mut s, &fs, true).unwrap();
    assert_eq!(fs.listing(), after);
    assert_file_catalog_and_journal(&s, &fs, &store);
}

#[test]
fn initial_sidecar_rollback_failure_keeps_a_repair_only_history_step() {
    use crate::rename::tests::FakeFs;
    let store = lightcraft_catalog::MemStore::new();
    let mut s = memory_library(&store);
    let fs = FakeFs::new(false, &[("/p/a.jpg", "Photo"), ("/p/a.XMP", "Stem edits"), ("/p/a.jpg.XMP", "Full edits")]);
    let photo = Photo::new(PhotoId(1), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 16, 16, "");
    s.commit("Synthetic File", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.persist().unwrap();
    s.undo.clear();
    select(&mut s, &[1], 1);
    let before = fs.listing();
    let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Trip", 1);
    *fs.fail.borrow_mut() = vec!["/p/a.jpg.XMP".into(), "/p/Trip.XMP".into()];
    assert!(s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).is_err());
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: "/p/a.jpg".into() });
    assert_eq!(s.undo.len(), 1);
    assert_eq!(s.undo.last().unwrap().retry_sidecars.len(), 1);
    assert_file_catalog_and_journal(&s, &fs, &store);
    let partial = fs.listing();
    *fs.fail.borrow_mut() = vec!["/p/Trip.XMP".into()];
    assert!(file_history_step(&mut s, &fs, false).is_err());
    assert_eq!(fs.listing(), partial);
    assert_eq!(s.undo.last().unwrap().retry_sidecars.len(), 1);
    fs.fail.borrow_mut().clear();
    file_history_step(&mut s, &fs, false).unwrap();
    assert_eq!(fs.listing(), before);
    assert_file_catalog_and_journal(&s, &fs, &store);
    file_history_step(&mut s, &fs, true).unwrap();
    assert_eq!(fs.listing(), before, "redo of the repair-only step must not rename healthy files");
    assert_file_catalog_and_journal(&s, &fs, &store);
}

#[test]
fn discarded_file_history_keeps_sidecar_repairs_without_stale_catalog_inverses() {
    use crate::rename::MoveFs;
    use crate::sidecar::{SidecarNaming, sidecar_path};

    for redo in [false, true] {
        for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
            let (mut s, fs, store) = sidecar_history_fixture(redo);
            let moves = s.file_moves(&sidecar_history_entry(&s, redo).op);
            let (from, to) = moves[0].clone();
            let failing = sidecar_path(&from, naming).with_extension("XMP");
            *fs.fail.borrow_mut() = vec![failing.to_string_lossy().into_owned(), to.clone()];
            assert!(file_history_step(&mut s, &fs, redo).is_err());
            let pending = sidecar_history_entry(&s, redo).retry_sidecars.clone();
            assert!(!sidecar_history_entry(&s, redo).retry_inverse.is_empty());
            // A new edit discards Redo. Enough edits also evict an unresolved Undo entry.
            for i in 0..if redo { 1 } else { 1000 } {
                s.execute("photo.rate", &json!({"ids": [2], "rating": i % 5 + 1})).unwrap();
            }
            assert!(s.redo.is_empty());
            assert_eq!(s.pending_sidecar_repairs, pending);
            assert!(s.undo.iter().all(|e| e.retry_inverse.is_empty() && e.retry_sidecars.is_empty()));
            let partial = fs.listing();
            // Metadata history remains usable without write access to the separated XMP.
            file_history_step(&mut s, &fs, false).unwrap();
            file_history_step(&mut s, &fs, true).unwrap();
            assert_eq!(s.pending_sidecar_repairs, pending);
            assert_eq!(fs.listing(), partial);
            assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: to.clone() });
            assert_file_catalog_and_journal(&s, &fs, &store);

            let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "Next", 1);
            let history = (s.undo.len(), s.redo.len());
            assert!(s.apply_rename_with(&fs, &plans).is_err());
            assert_eq!(fs.listing(), partial);
            assert_eq!((s.undo.len(), s.redo.len()), history);
            assert_eq!(s.pending_sidecar_repairs, pending);
            // Retain only unfinished work after partial progress; never overwrite a collision.
            let last = pending.last().unwrap().clone();
            *fs.fail.borrow_mut() = vec![last.from.to_string_lossy().into_owned()];
            assert!(s.apply_rename_with(&fs, &plans).is_err());
            assert_eq!(s.pending_sidecar_repairs.len(), 1);
            crate::rename::tests::assert_repair_packets(
                &s.pending_sidecar_repairs[0],
                &last,
                &[
                    (sidecar_path(&to, SidecarNaming::Stem).with_extension("XMP"), b"Upper stem edits"),
                    (sidecar_path(&to, SidecarNaming::Stem), b"Lower stem edits"),
                    (sidecar_path(&to, SidecarNaming::Full).with_extension("XMP"), b"Upper full-name edits"),
                    (last.from.clone(), b"Lower full-name edits"),
                ],
            );
            // Successful repairs advance companion paths while retaining their content versions.
            let last = s.pending_sidecar_repairs[0].clone();
            fs.fail.borrow_mut().clear();
            fs.files.borrow_mut().push((last.to.to_string_lossy().into_owned(), b"Other edits".to_vec()));
            let occupied = fs.listing();
            assert!(s.apply_rename_with(&fs, &plans).is_err());
            assert_eq!(fs.listing(), occupied);
            assert_eq!(s.pending_sidecar_repairs, vec![last.clone()]);
            fs.remove_file(&last.to).unwrap();

            // A no-op Rename still repairs the detached sidecars, without a new undo step.
            let noop = s.plan_rename_with(&fs, &[PhotoId(1)], "{name}", 1);
            assert_eq!(s.apply_rename_with(&fs, &noop).unwrap(), 0);
            assert!(s.pending_sidecar_repairs.is_empty());
            assert_eq!((s.undo.len(), s.redo.len()), history);
            let repaired = fs.listing();
            assert_file_catalog_and_journal(&s, &fs, &store);
            s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).unwrap();
            let renamed = fs.listing();
            file_history_step(&mut s, &fs, false).unwrap();
            assert_eq!(fs.listing(), repaired, "new history restores the current name, not the discarded inverse");
            file_history_step(&mut s, &fs, true).unwrap();
            assert_eq!(fs.listing(), renamed);
            assert_file_catalog_and_journal(&s, &fs, &store);
        }
    }
}

#[test]
fn merged_rename_failure_keeps_rolled_back_photo_identity() {
    use crate::rename::tests::FakeFs;

    let store = lightcraft_catalog::MemStore::new();
    let mut s = memory_library(&store);
    let fs = FakeFs::new(false, &[("/p/a.jpg", "Photo 1"), ("/p/a.xmp", "Photo 1 edits"), ("/p/b.jpg", "Photo 2")]);
    for (id, path) in [(1, "/p/a.jpg"), (2, "/p/b.jpg")] {
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        let photo = Photo::new(PhotoId(id), Source::File { path: path.into() }, &name, "JPEG", 16, 16, "");
        s.commit("Synthetic File", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    s.persist().unwrap();
    s.undo.clear();
    for (id, name) in [(1, "c"), (2, "a"), (2, "d")] {
        select(&mut s, &[id], id);
        let plans = s.plan_rename_with(&fs, &[PhotoId(id)], name, 1);
        s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &plans)?}))).unwrap();
    }
    assert_eq!(
        fs.listing(),
        vec![
            ("/p/c.jpg".to_string(), "Photo 1".to_string()),
            ("/p/c.xmp".to_string(), "Photo 1 edits".to_string()),
            ("/p/d.jpg".to_string(), "Photo 2".to_string()),
        ]
    );
    s.merge_undo(3, "Rename Three Times");
    assert_eq!((s.undo.len(), s.redo.len()), (1, 0));
    let moves: Vec<_> = s.file_moves(&s.undo.last().unwrap().op).iter().map(|(from, to)| (slash(from), slash(to))).collect();
    assert_eq!(moves, vec![("/p/d.jpg".to_string(), "/p/b.jpg".to_string()), ("/p/c.jpg".to_string(), "/p/a.jpg".to_string())]);
    // The second photo move sticks; the preceding d -> b move must roll back to d.
    *fs.fail.borrow_mut() = vec!["/p/c.xmp".into(), "/p/a.jpg".into()];
    let error = slash(&file_history_step(&mut s, &fs, false).unwrap_err().to_string());
    assert!(error.contains("rename /p/c.xmp: injected failure"), "{error}");
    assert!(error.contains("/p/c.jpg could not be moved back"), "{error}");
    let files = fs.listing();
    assert_eq!(
        files,
        vec![
            ("/p/a.jpg".to_string(), "Photo 1".to_string()),
            ("/p/c.xmp".to_string(), "Photo 1 edits".to_string()),
            ("/p/d.jpg".to_string(), "Photo 2".to_string()),
        ]
    );
    assert_eq!((s.undo.len(), s.redo.len()), (1, 0));
    assert_eq!(file_source(&s, 1), "/p/a.jpg");
    let path = file_source(&s, 2);
    assert_eq!(path, "/p/d.jpg", "photo 2 rolled back physically and must still identify d.jpg");
    assert_eq!(files.iter().find(|(file, _)| *file == path).map(|(_, bytes)| bytes.as_str()), Some("Photo 2"));
}
