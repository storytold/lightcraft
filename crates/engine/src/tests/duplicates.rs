//! Duplicates: byte-identical files, the same picture saved again, a different picture that must not group, the
//! keep / reject action, and malformed input.

use std::path::Path;

use lightcraft_catalog::{Flag, Op, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-dupes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A picture with some structure, drawn at any size: `seed` picks which picture (the same seed at another size is the
/// same picture resized).
fn picture(seed: u32, w: usize, h: usize) -> lightcraft_raster::Rgba8 {
    let data = (0..w * h)
        .map(|i| {
            let (u, v) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let (fx, fy) = ((1 + seed % 3) as f32, (1 + seed / 3) as f32);
            let l = 0.5 + 0.35 * (std::f32::consts::TAU * u * fx + seed as f32).sin() * (std::f32::consts::TAU * v * fy).cos();
            let c = |k: f32| (l * k * 255.0).clamp(0.0, 255.0) as u8;
            [c(1.0), c(0.85), c(0.6), 255]
        })
        .collect();
    lightcraft_raster::Rgba8 { width: w, height: h, data }
}

fn write_png(path: &Path, seed: u32, w: usize, h: usize) {
    let img = picture(seed, w, h);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn write_jpeg(path: &Path, seed: u32, w: usize, h: usize, quality: u8) {
    let img = picture(seed, w, h);
    let bytes = lightcraft_codecs::encode_jpeg(
        &lightcraft_codecs::EncodeImage::rgba8(&img),
        quality,
        lightcraft_codecs::ChromaSubsampling::S420,
        &Default::default(),
    )
    .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library of: `a.png` (picture 1, 96 x 64), `a-resaved.jpg` (the same picture re-encoded), `a-small.png` (resized to
/// 48 x 32), and `b.png` (another picture, same size).
fn library(tag: &str) -> Session {
    let dir = temp_dir(tag);
    write_png(&dir.join("a.png"), 1, 96, 64);
    write_jpeg(&dir.join("a-resaved.jpg"), 1, 96, 64, 80);
    write_png(&dir.join("a-small.png"), 1, 48, 32);
    write_png(&dir.join("b.png"), 5, 96, 64);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(4), "{r}");
    s
}

fn id_of(s: &Session, name: &str) -> PhotoId {
    s.catalog.photos().find(|p| p.file_name == name).unwrap_or_else(|| panic!("no {name}")).id
}

fn ids_of(group: &Value) -> Vec<u64> {
    let mut v: Vec<u64> = group["photos"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect();
    v.sort_unstable();
    v
}

/// A second record of the same file (the same bytes and content hash), as a library holds after an import that kept both.
fn add_copy_of(s: &mut Session, name: &str, rating: u8) -> PhotoId {
    let mut copy = (**s.catalog.photo(id_of(s, name)).unwrap()).clone();
    copy.id = s.catalog.alloc_photo_id();
    copy.file_name = format!("copy-of-{name}");
    copy.rating = rating;
    let id = copy.id;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    id
}

#[test]
fn byte_identical_files_form_an_exact_group_with_the_better_copy_to_keep() {
    let mut s = library("exact");
    let a = id_of(&s, "a.png");
    let copy = add_copy_of(&mut s, "a.png", 3);
    let r = s.execute("library.findDuplicates", &json!({"level": "exact"})).unwrap();
    let groups = r["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{r}");
    assert_eq!(groups[0]["level"], "exact");
    assert_eq!(ids_of(&groups[0]), {
        let mut v = vec![a.0, copy.0];
        v.sort_unstable();
        v
    });
    assert_eq!(groups[0]["keep"], copy.0, "the rated copy is the one to keep");
    assert_eq!(groups[0]["reasons"], json!(["rated"]));
    assert!(groups[0]["photos"].as_array().unwrap().iter().all(|p| p["exact"] == true && p["similarity"] == 1.0));
    // deciding an exact group checks it by content hash: no photo is decoded for a signature
    s.execute("library.resolveDuplicates", &json!({"ids": [a.0, copy.0], "keep": [copy.0]})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().flag, Flag::Reject);
    assert!(s.signatures.is_empty(), "an exact group is checked without reading signatures");
}

#[test]
fn the_same_picture_saved_again_forms_a_near_group_and_a_different_picture_does_not() {
    let mut s = library("near");
    let (a, jpg, small, b) = (id_of(&s, "a.png"), id_of(&s, "a-resaved.jpg"), id_of(&s, "a-small.png"), id_of(&s, "b.png"));
    // exact finds nothing: no two files are byte-identical
    let exact = s.execute("library.findDuplicates", &json!({"level": "exact"})).unwrap();
    assert_eq!(exact["groups"], json!([]));
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    let groups = r["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{r}");
    assert_eq!(groups[0]["level"], "near");
    let mut want = vec![a.0, jpg.0, small.0];
    want.sort_unstable();
    assert_eq!(ids_of(&groups[0]), want, "the re-encoded and the resized copy are grouped; {b:?} (another picture) is not");
    assert!(!ids_of(&groups[0]).contains(&b.0));
    // the larger of the equal-size files is kept; the resized copy says why it is the smaller
    assert!(groups[0]["keep"] == a.0 || groups[0]["keep"] == jpg.0, "{r}");
    assert!(groups[0]["reasons"].as_array().unwrap().contains(&json!("larger")), "{r}");
    let small_row = groups[0]["photos"].as_array().unwrap().iter().find(|p| p["id"] == small.0).unwrap();
    assert_eq!(small_row["exact"], false);
    let sim = small_row["similarity"].as_f64().unwrap();
    assert!((0.985..=1.0).contains(&sim), "{sim}");
    // a stricter cut-off than the copies reach keeps them apart; the allowed range is checked
    let strict = s.execute("library.findDuplicates", &json!({"similarity": 1.0})).unwrap();
    assert!(strict["groups"].as_array().unwrap().iter().all(|g| g["photos"].as_array().unwrap().len() < 3), "{strict}");
}

#[test]
fn exact_copies_join_the_near_group_of_their_picture() {
    let mut s = library("both");
    let copy = add_copy_of(&mut s, "a-resaved.jpg", 0);
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    let groups = r["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "one picture, one group: {r}");
    assert_eq!(groups[0]["photos"].as_array().unwrap().len(), 4);
    assert!(ids_of(&groups[0]).contains(&copy.0));
}

#[test]
fn keeping_one_rejects_the_others_in_one_undo_step_and_deletes_nothing() {
    let mut s = library("resolve");
    let (a, jpg, small) = (id_of(&s, "a.png"), id_of(&s, "a-resaved.jpg"), id_of(&s, "a-small.png"));
    let total = s.catalog.photos().count();
    let r = s.execute("library.resolveDuplicates", &json!({"ids": [a.0, jpg.0, small.0], "keep": [a.0]})).unwrap();
    let mut rejected: Vec<u64> = r["rejected"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    rejected.sort_unstable();
    let mut want = vec![jpg.0, small.0];
    want.sort_unstable();
    assert_eq!((rejected, r["kept"].clone()), (want, json!([a.0])));
    let flag = |s: &Session, id: PhotoId| s.catalog.photo(id).unwrap().flag;
    assert_eq!((flag(&s, a), flag(&s, jpg), flag(&s, small)), (Flag::None, Flag::Reject, Flag::Reject));
    assert_eq!(s.catalog.photos().count(), total, "nothing is deleted");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((flag(&s, a), flag(&s, jpg), flag(&s, small)), (Flag::None, Flag::None, Flag::None), "one undo step");
    // keeping several keeps them all
    let r = s.execute("library.resolveDuplicates", &json!({"ids": [a.0, jpg.0, small.0], "keep": [a.0, jpg.0]})).unwrap();
    assert_eq!(r["rejected"], json!([small.0]));
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let mut s = library("errors");
    let (a, jpg, b) = (id_of(&s, "a.png"), id_of(&s, "a-resaved.jpg"), id_of(&s, "b.png"));
    let before: Vec<Flag> = s.catalog.photos().map(|p| p.flag).collect();
    for params in [
        json!({"level": "similar"}),
        json!({"level": 3}),
        json!({"similarity": 0.5}),
        json!({"similarity": "high"}),
        json!({"similarity": 1.5}),
        json!({"ids": 3}),
        json!({"ids": ["3"]}),
        json!({"ids": [9_999_999]}),
        json!({"colour": "red"}),
    ] {
        assert!(s.execute("library.findDuplicates", &params).is_err(), "{params}");
    }
    for params in [
        json!({}),
        json!({"ids": [a.0, jpg.0]}),
        json!({"ids": [a.0, jpg.0], "keep": []}),
        json!({"ids": [a.0, jpg.0], "keep": "all"}),
        json!({"ids": [a.0, jpg.0], "keep": ["x"]}),
        json!({"ids": [a.0, jpg.0], "keep": [9_999_999]}),
        json!({"ids": [a.0, 9_999_999], "keep": [a.0]}),
        json!({"ids": [a.0, a.0], "keep": [a.0]}),
        json!({"ids": [a.0], "keep": [a.0]}),
        json!({"ids": 7, "keep": [a.0]}),
        json!({"ids": [a.0, jpg.0], "keep": [a.0], "delete": true}),
        // a different picture is not a duplicate, whoever names it
        json!({"ids": [a.0, b.0], "keep": [a.0]}),
        json!({"ids": [a.0, jpg.0, b.0], "keep": [a.0]}),
    ] {
        assert!(s.execute("library.resolveDuplicates", &params).is_err(), "{params}");
    }
    assert_eq!(s.catalog.photos().map(|p| p.flag).collect::<Vec<_>>(), before, "a refused call changes nothing");
}

#[test]
fn a_photo_whose_file_cannot_be_read_is_reported_not_fatal() {
    let mut s = library("unreadable");
    let mut gone = (**s.catalog.photo(id_of(&s, "a.png")).unwrap()).clone();
    gone.id = s.catalog.alloc_photo_id();
    gone.file_name = "gone.png".into();
    gone.content_hash = Some("not-a-real-hash".into());
    gone.source = Source::File { path: "Z:/no/such/dir/gone.png".into() };
    let gone_id = gone.id;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(gone) }).unwrap();
    let r = s.execute("library.findDuplicates", &json!({})).unwrap();
    assert_eq!(r["unreadable"], json!([gone_id.0]), "{r}");
    assert_eq!(r["groups"].as_array().map(Vec::len), Some(1), "the readable copies are still grouped");
}

/// The scan the dialog runs a few milliseconds per frame gives the command's answer, and with no time to spare a step
/// reads one picture (so a frame never stalls on more than one thumbnail) and, once every picture is read, joins one
/// unit with its look-alikes (the join is budgeted too: `finish` only assembles the answer).
#[test]
fn a_scan_in_steps_gives_the_commands_answer_and_reads_or_joins_one_unit_per_step_without_a_budget() {
    use crate::cmd::duplicates::Scan;
    let mut s = library("steps");
    let want = s.execute("library.findDuplicates", &json!({})).unwrap();
    let mut scan = Scan::begin(&s, &json!({})).unwrap();
    let total = scan.progress().1;
    assert_eq!(total, 4, "four files, none byte-identical");
    assert_eq!(scan.progress().0, 0);
    let (mut calls, mut join_calls) = (0, 0);
    while !scan.step(&mut s, Some(std::time::Duration::ZERO)) {
        calls += 1;
        assert_eq!(scan.progress(), (calls.min(total), total), "one picture per step");
        if calls > total {
            join_calls += 1;
        }
    }
    assert!(calls >= total, "every picture is read before the scan can end: {calls} steps");
    assert!(join_calls >= 2, "the look-alike join needs several steps, not one unbudgeted pass in `finish`: {join_calls}");
    assert_eq!(scan.finish(&s), want);
    // a bad parameter is refused before anything is read
    assert!(Scan::begin(&s, &json!({"level": "close"})).is_err());
}
