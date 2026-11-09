//! The import scan reads only what the review needs (#3786): a bounded part of each raw, and the whole of a file only
//! where a library photo or another file of the scan has its size. The content hash is made as the import copies the
//! file (or by a full read, for a file added where it is), and is the one the whole-file probe made.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use serde_json::{Value, json};

use crate::Session;
use crate::files::header_probe::tests::{counting_opener, padded_dng};
use crate::files::header_probe::{HEAD, full_probe, header_probe};
use crate::import::{ScanInput, ScanOutput, ScanProgress, scan_with};
use crate::import_move::{Check, Fault, copy_new, inject};

const PAD: usize = 4 << 20;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-headers-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A session whose probes count the bytes they read.
fn counting_session() -> (Session, Arc<AtomicU64>) {
    let count = Arc::new(AtomicU64::new(0));
    let mut s = Session::new().with_fs();
    let full = full_probe(counting_opener(count.clone()));
    s.media.file_probe = Some(full.clone());
    s.media.file_probe_headers = Some((full, header_probe(counting_opener(count.clone()))));
    (s, count)
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn scan(s: &mut Session, dir: &Path) -> ScanOutput {
    let (input, paths) = ScanInput::new(s, &[dir.to_string_lossy().to_string()]);
    scan_with(input, &paths, &ScanProgress::default())
}

fn hash(bytes: &[u8]) -> String {
    lightcraft_preview::hash_bytes(bytes).to_string()
}

fn import(s: &mut Session, dir: &Path, mode: &str) -> Value {
    s.execute("library.import", &json!({"paths": [dir.to_string_lossy()], "mode": mode})).unwrap()
}

fn candidate<'a>(out: &'a ScanOutput, name: &str) -> &'a crate::import::ImportCandidate {
    out.candidates.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn a_scan_of_new_files_reads_a_bounded_prefix_of_each() {
    let src = temp_dir("bounded");
    let files: Vec<Vec<u8>> = (1..=4).map(|i| padded_dng(i, PAD)).collect();
    for (i, f) in files.iter().enumerate() {
        put(&src, &format!("{i}.dng"), f);
    }
    let (mut s, count) = counting_session();
    let out = scan(&mut s, &src);
    assert_eq!(out.candidates.len(), 4);
    assert!(out.candidates.iter().all(|c| c.error.is_none() && c.duplicate.is_none()), "{:?}", out.candidates);
    assert!(out.candidates.iter().zip(&files).all(|(c, f)| c.file_size == f.len() as u64));
    let total = files.iter().map(Vec::len).sum::<usize>() as u64;
    // (the first HEAD bytes, and a few KB more where the headers point beyond them)
    assert!(count.load(Relaxed) <= 4 * (HEAD + 64 * 1024), "read {} bytes of {total}", count.load(Relaxed));
    assert!(out.probes.values().all(|p| p.content_hash.is_none()), "no content hash without a full read");
    let _ = std::fs::remove_dir_all(&src);
}

/// A library with one photo (`a.dng`), and the folder `other` with a byte-for-byte copy of it, a file of its size
/// with other contents (the same headers), and a file of a size nothing in the library has.
struct Fixture {
    s: Session,
    count: Arc<AtomicU64>,
    other: PathBuf,
    a: Vec<u8>,
    b: Vec<u8>,
    c: Vec<u8>,
    lib: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = temp_dir(tag);
    let (src, other, lib) = (base.join("src"), base.join("other"), base.join("lib"));
    let (a, b, c) = (padded_dng(1, PAD), padded_dng(2, PAD), padded_dng(3, PAD + 2048));
    put(&src, "a.dng", &a);
    let (mut s, count) = counting_session();
    s.open_library(&lib, false).unwrap();
    let r = import(&mut s, &src, "copy");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    put(&other, "a-copy.dng", &a);
    put(&other, "b.dng", &b);
    put(&other, "c.dng", &c);
    count.store(0, Relaxed);
    Fixture { s, count, other, a, b, c, lib }
}

#[test]
fn a_file_of_a_library_photos_size_gets_an_exact_verdict_and_the_others_are_not_read_in_full() {
    let mut f = fixture("verdict");
    let id = f.s.catalog.photos().next().unwrap().id.0;
    let out = scan(&mut f.s, &f.other);
    let (copy, b, c) = (candidate(&out, "a-copy.dng"), candidate(&out, "b.dng"), candidate(&out, "c.dng"));
    assert_eq!((copy.duplicate.as_deref(), copy.existing), (Some("content"), Some(id)), "the same bytes");
    assert_eq!(b.duplicate, None, "the same size and headers, other contents: not a duplicate");
    assert_eq!(c.duplicate, None);
    let (read, whole) = (f.count.load(Relaxed), (f.a.len() + f.b.len()) as u64);
    assert!(read >= whole, "the two candidates are read in full: {read}");
    assert!(read < whole + f.c.len() as u64 / 2, "the file of another size is not: {read} bytes read, {} in the folder", whole + f.c.len() as u64);
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn a_photo_recorded_without_a_size_makes_every_file_a_candidate() {
    let mut f = fixture("nosize");
    let p = f.s.catalog.photos().next().unwrap().clone();
    f.s.catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: 0,
            content_hash: p.content_hash.clone(),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
    let out = scan(&mut f.s, &f.other);
    assert_eq!(candidate(&out, "a-copy.dng").duplicate.as_deref(), Some("content"));
    assert_eq!(candidate(&out, "c.dng").duplicate, None);
    assert!(f.count.load(Relaxed) >= (f.a.len() + f.b.len() + f.c.len()) as u64, "every file was read in full");
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn a_photo_with_no_hash_and_no_size_rules_nothing_out() {
    let mut f = fixture("nohash");
    let p = f.s.catalog.photos().next().unwrap().clone();
    f.s.catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: 0,
            content_hash: None,
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
    let out = scan(&mut f.s, &f.other);
    assert!(out.candidates.iter().all(|c| c.duplicate.is_none()), "a photo with no hash is no duplicate by content");
    let whole = (f.a.len() + f.b.len() + f.c.len()) as u64;
    assert!(f.count.load(Relaxed) < whole / 2, "no file was a candidate: {} bytes read of {whole}", f.count.load(Relaxed));
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn the_scans_progress_counts_the_files_it_reads_in_full() {
    let mut f = fixture("progress");
    let (input, paths) = ScanInput::new(&mut f.s, &[f.other.to_string_lossy().to_string()]);
    let progress = ScanProgress::default();
    let out = scan_with(input, &paths, &progress);
    assert_eq!(out.candidates.len(), 3);
    // (three files read for their headers, then the two of the library photo's size read in full)
    assert_eq!((progress.total.load(Relaxed), progress.done.load(Relaxed)), (5, 5));
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn two_new_files_with_one_content_are_found_by_the_scan() {
    let src = temp_dir("twins");
    let a = padded_dng(5, PAD);
    put(&src, "one.dng", &a);
    put(&src, "two.dng", &a);
    put(&src, "three.dng", &padded_dng(6, PAD + 2048));
    let (mut s, _) = counting_session();
    let out = scan(&mut s, &src);
    let dups = out.candidates.iter().filter(|c| c.duplicate.as_deref() == Some("content")).count();
    assert_eq!(dups, 1, "{:?}", out.candidates);
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn the_hash_recorded_by_a_copy_is_the_whole_files_and_the_copy_is_read_once() {
    let src = temp_dir("copyhash");
    let lib = temp_dir("copyhash-lib");
    let files: Vec<Vec<u8>> = (1..=3).map(|i| padded_dng(10 + i, PAD)).collect();
    for (i, f) in files.iter().enumerate() {
        put(&src, &format!("{i}.dng"), f);
    }
    let (mut s, count) = counting_session();
    s.open_library(&lib, false).unwrap();
    let r = import(&mut s, &src, "copy");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(3), "{r}");
    assert!(count.load(Relaxed) <= 3 * (HEAD + 64 * 1024), "the probes read {} bytes: the copies hash the files", count.load(Relaxed));
    let mut recorded: Vec<(u64, Option<String>)> = s.catalog.photos().map(|p| (p.file_size, p.content_hash.clone())).collect();
    recorded.sort();
    let mut expected: Vec<(u64, Option<String>)> = files.iter().map(|f| (f.len() as u64, Some(hash(f)))).collect();
    expected.sort();
    assert_eq!(recorded, expected);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_file_added_where_it_is_records_the_whole_files_hash() {
    let src = temp_dir("inplace");
    let lib = temp_dir("inplace-lib");
    let a = padded_dng(21, PAD);
    put(&src, "a.dng", &a);
    let (mut s, _) = counting_session();
    s.open_library(&lib, false).unwrap();
    let r = import(&mut s, &src, "add");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.catalog.photos().next().unwrap().content_hash, Some(hash(&a)));
    // (the same file again, from another place, is found as the duplicate it is)
    let again = temp_dir("inplace-again");
    put(&again, "same.dng", &a);
    let r = import(&mut s, &again, "add");
    assert_eq!(r["duplicates"][0]["reason"], "content", "{r}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
    let _ = std::fs::remove_dir_all(&again);
}

#[test]
fn a_copy_that_fails_its_verification_still_fails() {
    let src = temp_dir("corrupt");
    let lib = temp_dir("corrupt-lib");
    put(&src, "a.dng", &padded_dng(31, PAD));
    let (mut s, _) = counting_session();
    s.open_library(&lib, false).unwrap();
    inject(Fault::Corrupt);
    let r = import(&mut s, &src, "copy");
    inject(Fault::None);
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.catalog.len(), 0);
    assert_eq!(files_under(&lib.join("Originals")), 0, "no bad copy is left");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_file_that_changed_size_since_it_was_listed_is_refused_not_copied() {
    let dir = temp_dir("resized");
    let src = put(&dir, "a.dng", &padded_dng(41, PAD));
    let len = std::fs::metadata(&src).unwrap().len();
    for listed in [len + 1, len - 1, 0] {
        let r = copy_new(&src, &dir.join("out"), "a.dng", Check::Read { len: listed });
        assert!(r.is_err(), "listed as {listed} bytes");
        assert!(std::fs::read_dir(dir.join("out")).map_or(true, |mut d| d.next().is_none()), "the refused copy is removed");
    }
    let (_, h) = copy_new(&src, &dir.join("out"), "a.dng", Check::Read { len }).unwrap();
    assert_eq!(h.map(|h| h.to_string()), Some(hash(&std::fs::read(&src).unwrap())));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_files_never_crash_a_scan_or_an_import() {
    let src = temp_dir("hostile");
    let lib = temp_dir("hostile-lib");
    let tiny = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    put(&src, "empty.dng", &[]);
    put(&src, "tiny.dng", &tiny);
    put(&src, "junk.dng", &vec![0xAB; 3 * HEAD as usize]);
    let cut = padded_dng(51, PAD);
    put(&src, "cut.dng", cut.get(..cut.len() / 3).unwrap());
    put(&src, "ok.dng", &padded_dng(52, PAD));
    let (mut s, _) = counting_session();
    s.open_library(&lib, false).unwrap();
    let out = scan(&mut s, &src);
    assert_eq!(out.candidates.len(), 5);
    assert!(candidate(&out, "empty.dng").error.is_some());
    assert!(candidate(&out, "junk.dng").error.is_some());
    assert!(candidate(&out, "ok.dng").error.is_none());
    let r = import(&mut s, &src, "copy");
    assert!(r["imported"].as_array().is_some_and(|v| v.len() >= 2), "{r}");
    let tiny_photo = s.catalog.photos().find(|p| p.file_size == tiny.len() as u64).unwrap();
    assert_eq!(tiny_photo.content_hash, Some(hash(&tiny)), "a file under the prefix is hashed whole");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// How many files lie under `dir` (0 when it isn't there).
fn files_under(dir: &Path) -> usize {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| if e.path().is_dir() { files_under(&e.path()) } else { 1 }).sum()
}

#[test]
fn a_header_probe_stands_only_beside_the_file_probe_it_was_made_with() {
    let mut s = Session::new().with_fs();
    assert!(s.media.header_probe().is_some());
    s.media.file_probe = Some(Arc::new(|_| Err("stub".into())));
    assert!(s.media.header_probe().is_none(), "a probe installed since probes every file whole");
    assert!(Session::new().media.header_probe().is_none());
}

/// The path the import dialog takes: a review (the scan's probes are kept), then the import that reuses them.
#[test]
fn an_import_after_a_review_records_the_whole_files_hash_in_every_mode() {
    for mode in ["copy", "add"] {
        let (src, lib) = (temp_dir(&format!("review-{mode}")), temp_dir(&format!("review-{mode}-lib")));
        let files: Vec<Vec<u8>> = (61..=63).map(|i| padded_dng(i, PAD)).collect();
        for (i, f) in files.iter().enumerate() {
            put(&src, &format!("{i}.dng"), f);
        }
        let (mut s, count) = counting_session();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(r["scanned"], 3, "{r}");
        assert!(count.load(Relaxed) <= 3 * (HEAD + 64 * 1024), "the review read {} bytes", count.load(Relaxed));
        let r = import(&mut s, &src, mode);
        assert_eq!(r["imported"].as_array().map(Vec::len), Some(3), "{mode}: {r}");
        let mut recorded: Vec<Option<String>> = s.catalog.photos().map(|p| p.content_hash.clone()).collect();
        recorded.sort();
        let mut expected: Vec<Option<String>> = files.iter().map(|f| Some(hash(f))).collect();
        expected.sort();
        assert_eq!(recorded, expected, "{mode}");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&lib);
    }
}

fn imported_names(r: &Value) -> Vec<String> {
    let mut names: Vec<String> = r["imported"].as_array().map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default();
    names.sort();
    names
}

/// An import made with no review before it (`library.import`, an auto-import, a drop) has no scan's verdicts: the
/// copies are hashed as they are read, and a file that might be a duplicate is read in full first to be told.
#[test]
fn a_copy_import_without_a_review_skips_a_duplicate_of_a_library_photo_and_keeps_the_same_size_original() {
    let mut f = fixture("noreview-size");
    let r = import(&mut f.s, &f.other, "copy");
    assert_eq!(r["duplicates"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["duplicates"][0]["reason"], "content", "{r}");
    assert_eq!(imported_names(&r).len(), 2, "b (the same size, other contents) and c are new: {r}");
    assert_eq!(f.s.catalog.photos().count(), 3);
    let mut hashes: Vec<Option<String>> = f.s.catalog.photos().map(|p| p.content_hash.clone()).collect();
    hashes.sort();
    let mut expected = vec![Some(hash(&f.a)), Some(hash(&f.b)), Some(hash(&f.c))];
    expected.sort();
    assert_eq!(hashes, expected);
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn a_copy_import_without_a_review_skips_a_duplicate_of_a_photo_recorded_without_a_size() {
    let mut f = fixture("noreview-nosize");
    let p = f.s.catalog.photos().next().unwrap().clone();
    f.s.catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: 0,
            content_hash: p.content_hash.clone(),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
    let other = f.other.join("only-copy");
    put(&other, "a-copy.dng", &f.a);
    let r = import(&mut f.s, &other, "copy");
    assert_eq!(r["duplicates"][0]["reason"], "content", "{r}");
    assert_eq!(f.s.catalog.photos().count(), 1);
    let _ = std::fs::remove_dir_all(f.lib.parent().unwrap());
}

#[test]
fn a_copy_import_without_a_review_skips_a_file_that_repeats_one_of_its_own_batch() {
    let (src, lib) = (temp_dir("noreview-twins"), temp_dir("noreview-twins-lib"));
    put(&src, "twin-a.dng", &padded_dng(32, PAD + 1));
    put(&src, "twin-b.dng", &padded_dng(32, PAD + 1));
    put(&src, "other.dng", &padded_dng(33, PAD + 2048));
    let (mut s, _) = counting_session();
    s.open_library(&lib, false).unwrap();
    let r = import(&mut s, &src, "copy");
    assert_eq!(r["duplicates"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.catalog.photos().count(), 2, "{r}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// The app prepares an import a batch at a time and adds each batch to the catalog before the next is prepared.
#[test]
fn a_copy_import_without_a_review_skips_a_file_that_repeats_one_of_an_earlier_batch() {
    use crate::import::{ImportJob, ImportMode, ImportOptions, commit_prepared};
    let (src, lib) = (temp_dir("noreview-batches"), temp_dir("noreview-batches-lib"));
    let bytes = padded_dng(31, PAD);
    let first = put(&src, "a.dng", &bytes);
    let later = put(&src.join("later"), "b.dng", &bytes);
    let (mut s, _) = counting_session();
    s.open_library(&lib, false).unwrap();
    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Copy, ..Default::default() }).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let (mut imported, mut duplicates) = (0, 0);
    for file in [first, later] {
        let batch = job.prepare_files(vec![file.to_string_lossy().to_string()], &cancel);
        let r = commit_prepared(&mut s, &job.opts, job.now(), batch).unwrap();
        (imported, duplicates) = (imported + r.imported.len(), duplicates + r.duplicates.len());
    }
    assert_eq!((imported, duplicates), (1, 1));
    assert_eq!(s.catalog.photos().count(), 1);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}
