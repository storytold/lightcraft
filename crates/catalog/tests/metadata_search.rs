use lightcraft_catalog::{Catalog, Filter, Journal, MemStore, Op, Photo, PhotoId, Sort, Source};
use lightcraft_meta::SearchMetadata;
use std::sync::Arc;

fn metadata() -> SearchMetadata {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let data = format!("prompt\0{}", include_str!("../../meta/tests/fixtures/comfy-api.json"));
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(data.as_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(b"tEXt");
    crc.update(data.as_bytes());
    png.extend_from_slice(&crc.finalize().to_be_bytes());
    lightcraft_meta::search_metadata(&png)
}

fn photo(id: u64) -> Photo {
    Photo::new(PhotoId(id), Source::File { path: "/offline/never-opened.png".into() }, "city.png", "PNG", 1024, 768, "2026-10-18")
}

fn query(c: &Catalog, text: &str) -> Vec<PhotoId> {
    c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default())
}

#[test]
fn derived_metadata_tracks_incremental_queries_and_loaded_revisions() {
    use lightcraft_catalog::Aspects;

    let mut c = Catalog::new();
    c.apply(Op::AddPhoto { photo: Box::new(photo(1)) }).unwrap();
    c.apply(Op::AddPhoto { photo: Box::new(photo(2)) }).unwrap();
    let filter = Filter { text: "model:flux".into(), ..Default::default() };
    let sort = Sort::default();
    let was = c.query(&filter, &sort);
    let before = c.revision;
    let index = Some(Arc::new(metadata()));
    c.set_metadata_search(PhotoId(1), index.clone()).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![PhotoId(1)]));
    assert_eq!(c.aspects_since(before), Some(Aspects::META));
    assert_eq!(c.follow(&was, &c.touched_since(before).unwrap(), &filter, &sort, None), vec![PhotoId(1)]);
    c.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![PhotoId(1), PhotoId(2)]));
    assert_eq!(c.aspects_since(before), Some(Aspects::META.with(Aspects::RATING)));

    // Library loading assigns a fresh revision range; old query caches must stay invalid.
    let old = c.revision;
    c.revision = 7 << 32;
    let loaded = c.revision;
    c.set_metadata_search(PhotoId(1), None).unwrap();
    assert_eq!(c.touched_since(old), None);
    assert_eq!(c.aspects_since(old), None);
    assert_eq!(c.touched_since(loaded), Some(vec![PhotoId(1)]));
    assert_eq!(c.aspects_since(loaded), Some(Aspects::META));
    assert!(c.query(&filter, &sort).is_empty());
    let unchanged = c.revision;
    c.set_metadata_search(PhotoId(1), None).unwrap();
    assert_eq!(c.revision, unchanged);
}

#[test]
fn comfy_free_text_fields_phrases_and_combined_filters() {
    let mut c = Catalog::new();
    let mut p = photo(1);
    p.metadata_search = Some(Arc::new(metadata()));
    p.rating = 4;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    for text in [
        "CYBERPUNK",
        "FLUX",
        "detail",
        "MyCustomNode",
        "stormy",
        "strength_model",
        "0.7",
        "café",
        "comfy:true",
        "COMFY:YES",
        "prompt:cyber",
        "prompt:\"neon city\"",
        "positive:cyberpunk",
        "negative:blurry",
        "model:FLUX",
        "model:\"flux/flux-dev\"",
        "lora:detail lora:cinema",
        "sampler:eul",
        "scheduler:kar",
        "vae:ae",
        "node:custom",
        "seed:123456",
        "seed:18446744073709551614",
        "steps:>20",
        "steps:30",
        "steps:<31",
        "cfg:>5",
        "cfg:7.5",
        "denoise:<1",
        "width:1024 height:768",
        "comfy:true prompt:\"NEON CITY\" model:flux sampler:euler steps:>20 cfg:>5 rating:4 type:image",
        "\"neon city\" stormy",
    ] {
        assert_eq!(query(&c, text), vec![PhotoId(1)], "{text}");
    }
    for text in [
        "comfy:false",
        "comfy:invalid",
        "prompt:stormy",
        "prompt:class_type",
        "positive:blurry",
        "negative:cyberpunk",
        "seed:123",
        "seed:18446744073709551615",
        "seed:18446744073709551613",
        "seed:1e5",
        "seed:bogus",
        "steps:>30",
        "steps:garbage",
        "cfg:<5",
        "cfg:NaN",
        "model:missing",
        "lora:missing",
        "prompt:\"city neon\"",
    ] {
        assert!(query(&c, text).is_empty(), "{text}");
    }
}

#[test]
fn derived_metadata_never_enters_catalog_snapshot_or_journal() {
    let store = MemStore::new();
    let (mut journal, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
    let mut p = photo(1);
    p.metadata_search = Some(Arc::new(metadata()));
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(!encoded.contains("metadata_search"));
    assert!(!encoded.contains("comfy"));
    let op = Op::AddPhoto { photo: Box::new(p) };
    c.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    journal.snapshot(&c).unwrap();
    drop(journal);
    let (_, reopened, report) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(lightcraft_catalog::journal::VERSION, 9);
    assert_eq!(report.upgraded_from, None);
    assert!(reopened.photo(PhotoId(1)).unwrap().metadata_search.is_none());
    assert!(query(&reopened, "comfy:true").is_empty());
}

#[test]
fn comfy_non_generated_images_keep_existing_search_behavior() {
    let mut c = Catalog::new();
    let mut p = photo(1);
    p.file_name = "Summer trip.png".into();
    p.meta.title = "A neon city".into();
    p.meta.caption = "Meeting at 10:30, say \"hello\" in C:\\photos".into();
    p.meta.camera = "Maker X2".into();
    p.meta.iso = Some(1600);
    p.meta.city = "Berlin".into();
    p.meta.creator = "Alice".into();
    p.meta.alt_text = "Church in a forest".into();
    p.meta.keywords = vec!["Travel|Japan".into()];
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    for text in [
        "summer",
        "berlin alice church",
        "meta:city=berlin meta:creator=alice",
        "meta:\"alt text=church in a forest\"",
        "neon japan",
        "camera:x2 iso:>800",
        "10:30",
        "keyword:travel",
        "comfy:false",
        "\"neon city\"",
        "name:\"summer trip\"",
        "\"say \\\"hello\\\"\"",
        "\"in C:\\\\photos\"",
    ] {
        assert_eq!(query(&c, text), vec![PhotoId(1)], "{text}");
    }
    for text in ["comfy:true", "prompt:neon", "seed:123", "cfg:>1"] {
        assert!(query(&c, text).is_empty(), "{text}");
    }
}

#[test]
fn comfy_searches_twenty_thousand_offline_records_without_opening_files() {
    let mut c = Catalog::new();
    let meta = Arc::new(metadata());
    for id in 1..=20_000 {
        let mut p = photo(id);
        p.metadata_search = Some(meta.clone());
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let started = std::time::Instant::now();
    assert_eq!(query(&c, "comfy:true prompt:\"neon city\" model:flux steps:>20").len(), 20_000);
    eprintln!("ComfyUI query over 20,000 offline records: {:?}", started.elapsed());
}
