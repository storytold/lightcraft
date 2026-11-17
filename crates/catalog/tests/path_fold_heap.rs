//! Heap blocks a smart album's `filePath` rules allocate per photo, measured with dhat's testing mode. A rule over
//! a non-ASCII path or text (`crates/catalog/src/rules.rs`, `file_path_matches`) used to build two lower-cased
//! strings for every photo it was asked about; a large library asks it for every photo of every smart album, so
//! it compares in place. One test per binary (dhat's profiler is global).

use lightcraft_catalog::{Catalog, Photo, PhotoId, RuleSet, Source};
use serde_json::json;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn photo(id: u64, path: &str) -> Photo {
    Photo::new(PhotoId(id), Source::File { path: path.to_string() }, "IMG_0001.JPG", "JPG", 6000, 4000, "2026-09-20T10:00:00")
}

fn rule_set(op: &str, value: &str) -> RuleSet {
    serde_json::from_value(json!({"rules": [{"field": "filePath", "op": op, "value": value}]})).unwrap()
}

#[test]
fn a_file_path_rule_allocates_nothing_per_photo() {
    let cat = Catalog::new();
    let photos: Vec<Photo> = ["/Volumes/tokyo/Фото/Éclair/Straße/ÜBER_2026.JPG", "D:\\日本語\\İstanbul\\IMG_0001.jpg", "/srv/plain/ascii/path.jpg"]
        .iter()
        .zip(1u64..)
        .map(|(p, i)| photo(i, p))
        .collect();
    let rules: Vec<RuleSet> = [
        ("contains", "éCLAIR"),
        ("contains", "tokyo"),
        ("notContains", "ÜBER"),
        ("is", "/srv/plain/ascii/path.jpg"),
        ("isNot", "straße"),
        ("startsWith", "\\Volumes\\TOKYO\\ФОТО"),
        ("endsWith", ".JPG"),
        ("isEmpty", ""),
        ("isNotEmpty", ""),
    ]
    .iter()
    .map(|(op, v)| rule_set(op, v))
    .collect();
    // warm anything lazily built (the clock, thread locals) before counting
    let expected: Vec<Vec<bool>> = rules.iter().map(|r| photos.iter().map(|p| r.matches(p, &cat)).collect()).collect();
    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get().total_blocks;
    let mut hits = 0usize;
    for _ in 0..100 {
        for (r, want) in rules.iter().zip(&expected) {
            for (p, w) in photos.iter().zip(want) {
                let got = r.matches(p, &cat);
                assert_eq!(got, *w);
                hits += usize::from(got);
            }
        }
    }
    let blocks = dhat::HeapStats::get().total_blocks - before;
    assert!(hits > 0, "the rules matched nothing: the test measures nothing");
    assert_eq!(blocks, 0, "{blocks} heap blocks for {} rule tests", 100 * rules.len() * photos.len());
}
