//! A `filePath` rule over a non-ASCII path or text compares in place, with the answers the
//! allocating comparison ([`file_path_matches_slow`]) always gave. That nothing is allocated per
//! photo is checked in `tests/path_fold_heap.rs` (dhat's profiler is global, so it has its own binary).

use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;
use crate::Source;

const OPS: [&str; 9] = ["contains", "notContains", "is", "isNot", "startsWith", "endsWith", "isEmpty", "isNotEmpty", "bogus"];

fn file(path: &str) -> Source {
    Source::File { path: path.to_string() }
}

fn fast(op: &str, path: &str, want: &str) -> bool {
    file_path_matches(op, &file(path), &Value::String(want.to_string()))
}

fn old(op: &str, path: &str, want: &str) -> bool {
    file_path_matches_slow(op, path, &Value::String(want.to_string()))
}

#[test]
fn pinned_unicode_cases_keep_their_old_answers() {
    // (path, text) pairs where lower-casing changes the length, folds across scripts or looks at its neighbors
    let pairs = [
        ("İstanbul/Photo", "i̇stanbul"), // İ lower-cases to i + U+0307
        ("a/İ", "\u{307}"),             // a match that starts inside an expansion
        ("STRASSE", "straße"),          // ß never equals ss, either way
        ("Straße", "STRASSE"),
        ("ẞ/x", "ß"),     // capital sharp s lower-cases to ß
        ("ΟΔΟΣ", "οδος"), // final sigma: Σ → ς at the end of a word
        ("ΟΔΟΣ", "οδοσ"),
        ("ΣΑΣ/ΟΔΟΣ", "σας"),
        ("\u{212A}elvin", "kelvin"), // Kelvin sign → k
        ("ǅ/x", "ǆ"),                // a title-case digraph
        ("D:\\Фото\\Ё", "d:/фото/ё"),
        ("  Ünï  ", "ünï"), // whitespace around the path is trimmed for is / startsWith / endsWith
        ("Ünï", "  ünï\t"),
        ("\u{85}Ü\u{2003}", "ü"),
        ("", "é"),
        ("é", ""),
    ];
    for (path, want) in pairs {
        for op in OPS {
            assert_eq!(fast(op, path, want), old(op, path, want), "{op} {path:?} {want:?}");
        }
    }
}

#[test]
fn a_path_or_text_longer_than_the_buffers_still_answers_as_before() {
    // İ grows by half when lower-cased, so these cross FOLDED_PATH_BYTES and FOLDED_TEXT_BYTES on either side of the limit
    for len in [FOLDED_PATH_BYTES / 3, FOLDED_PATH_BYTES / 2 - 1, FOLDED_PATH_BYTES / 2, FOLDED_PATH_BYTES, FOLDED_PATH_BYTES * 2] {
        let path = format!("/Фото/{}/IMG.JPG", "İ".repeat(len / 2));
        for want_len in [3, FOLDED_TEXT_BYTES / 3, FOLDED_TEXT_BYTES / 2, FOLDED_TEXT_BYTES, FOLDED_TEXT_BYTES * 2] {
            let tail = "i̇".repeat(want_len / 3);
            for want in [tail.clone(), format!("{tail}/img.jpg"), format!("/фото/{tail}")] {
                for op in OPS {
                    assert_eq!(fast(op, &path, &want), old(op, &path, &want), "{op} {len} {want_len}");
                }
            }
        }
    }
}

#[test]
fn a_text_that_is_not_a_string_still_answers_as_before() {
    for value in [json!(7), json!(null), json!(true), json!(["é"])] {
        for op in OPS {
            let src = file("/Фото/7.jpg");
            assert_eq!(file_path_matches(op, &src, &value), file_path_matches_slow(op, "/Фото/7.jpg", &value), "{op} {value}");
        }
    }
}

/// Letters whose lower-casing is odd, plus plain ones, separators and whitespace.
fn glyph() -> impl Strategy<Value = char> {
    prop::sample::select(vec![
        'a', 'A', 's', 'S', 'ß', 'ẞ', 'İ', 'i', 'I', '\u{307}', 'é', 'É', 'ü', 'Ü', 'Σ', 'σ', 'ς', 'ǅ', 'ǆ', 'Ǆ', '\u{212A}', 'k', 'Ω', 'ω', 'Ё',
        'ё', 'Ф', 'ф', '日', '本', '/', '\\', ' ', '\t', '\u{85}', '\u{2003}', '.', '1',
    ])
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(glyph(), 0..14).prop_map(|v| v.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn the_in_place_comparison_equals_the_allocating_one(path in text(), want in text(), op in prop::sample::select(OPS.to_vec())) {
        prop_assert_eq!(fast(op, &path, &want), old(op, &path, &want), "{} {:?} {:?}", op, path, want);
    }

    #[test]
    fn a_slice_of_the_path_is_found_whatever_its_case(path in text(), from in 0usize..14, len in 0usize..14, upper in any::<bool>(), op in prop::sample::select(OPS.to_vec())) {
        let chars: Vec<char> = path.chars().collect();
        let from = from.min(chars.len());
        let to = (from + len).min(chars.len());
        let slice: String = chars[from..to].iter().collect();
        let want = if upper { slice.to_uppercase() } else { slice.to_lowercase() };
        prop_assert_eq!(fast(op, &path, &want), old(op, &path, &want), "{} {:?} {:?}", op, path, want);
    }

    #[test]
    fn any_unicode_text_agrees(path in "\\PC{0,12}", want in "\\PC{0,6}", op in prop::sample::select(OPS.to_vec())) {
        prop_assert_eq!(fast(op, &path, &want), old(op, &path, &want), "{} {:?} {:?}", op, path, want);
    }
}

/// A timed loop over a library's worth of non-ASCII paths: run it with
/// `just test catalog path_fold_timing -- --ignored --nocapture` (release numbers: `--release`).
#[test]
#[ignore = "a measurement, not a check"]
fn path_fold_timing() {
    const N: usize = 844_425;
    let folders = ["Фото", "Ünïcode", "日本語", "Straße", "Éclair", "İstanbul"];
    let paths: Vec<Source> = (0..N).map(|i| file(&format!("/Volumes/share/{}/{}/IMG_{i:07}.JPG", folders[i % 6], folders[(i / 6) % 6]))).collect();
    let wants = [("contains", "ÉCLAIR/img_000"), ("contains", "tokyo"), ("startsWith", "/volumes/share/фото"), ("endsWith", "99.jpg")];
    for (op, want) in wants {
        let value = Value::String(want.to_string());
        let t = std::time::Instant::now();
        let new_hits = paths.iter().filter(|p| file_path_matches(op, p, &value)).count();
        let new_time = t.elapsed();
        let t = std::time::Instant::now();
        let old_hits = paths.iter().filter(|p| matches!(p, Source::File { path } if file_path_matches_slow(op, path, &value))).count();
        let old_time = t.elapsed();
        assert_eq!(new_hits, old_hits);
        println!("{op:<11} {want:<22} hits {new_hits:>7}  in place {new_time:>10.2?}  allocating {old_time:>10.2?}");
    }
}
