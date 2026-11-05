//! What library queries cost at 100k photos (ignored:
//! `cargo test --release -p lightcraft-catalog --lib scale_100k -- --ignored --nocapture`).

use std::time::Instant;

use serde_json::json;

use super::*;

/// 100k photos, 10k two-photo stacks, a 20k-photo album, keywords and flags.
fn library(n: u64) -> (Catalog, AlbumId) {
    let mut c = Catalog::new();
    let mut ops: Vec<Op> = (0..n)
        .map(|i| {
            let mut p = Photo::new(
                PhotoId(i + 1),
                Source::Demo { scene: (i % 20) as u32 },
                &format!("IMG_{:06}.jpg", mix64(i) % 1_000_000),
                "JPEG",
                6000,
                4000,
                "2026-01-01T00:00:00",
            );
            p.captured = Some(format!("20{:02}-{:02}-{:02}T{:02}:00:00", 10 + i % 16, 1 + i % 12, 1 + i % 28, i % 24));
            p.rating = (i % 6) as u8;
            p.flag = if i % 7 == 0 { Flag::Pick } else { Flag::None };
            p.meta.keywords = vec![format!("kw{}", i % 500), "travel|italy".into()];
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    ops.extend(
        (0..n / 10)
            .map(|s| Op::AddStack { stack: Stack { id: StackId(s + 1), photos: vec![PhotoId(s * 10 + 1), PhotoId(s * 10 + 2)], collapsed: false } }),
    );
    let album = AlbumId(1);
    let mut a = Album::new(album, "Big");
    a.photos = (0..n).step_by(5).map(|i| PhotoId(i + 1)).collect();
    ops.push(Op::AddAlbum { album: a });
    let t = Instant::now();
    c.apply(Op::Batch { ops }).unwrap();
    eprintln!("build {n} photos, {} stacks: {:.0} ms", n / 10, t.elapsed().as_secs_f64() * 1e3);
    (c, album)
}

fn smart(c: &mut Catalog, id: u64, rules: serde_json::Value) -> AlbumId {
    let mut a = Album::new(AlbumId(id), format!("Smart {id}"));
    a.smart = Some(Box::new(Filter { rule_set: Some(serde_json::from_value(rules).unwrap()), ..Default::default() }));
    c.apply(Op::AddAlbum { album: a }).unwrap();
    AlbumId(id)
}

/// Best of five runs (the machine may be busy with other work).
fn time<T>(what: &str, f: impl Fn() -> T) -> T {
    let mut best = f64::MAX;
    let mut r = None;
    for _ in 0..5 {
        let t = Instant::now();
        r = Some(f());
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("{what}: {best:.1} ms");
    r.unwrap()
}

#[test]
#[ignore]
fn scale_100k() {
    let (mut c, album) = library(100_000);
    let all = Filter::default();
    time("query, capture date", || c.query(&all, &Sort::default()));
    time("query, file name", || c.query(&all, &Sort { key: SortKey::FileName, ..Default::default() }));
    time("query, text", || c.query(&Filter { text: "kw42".into(), ..Default::default() }, &Sort::default()));
    time("query, album of 20k", || c.query(&Filter { album: Some(album), ..Default::default() }, &Sort::default()));
    let only: Vec<PhotoId> = (0..5000).map(|i| PhotoId(i * 7 + 1)).collect();
    time("query, only 5k ids", || c.query(&Filter { only: only.clone(), ..Default::default() }, &Sort::default()));
    let stacked = smart(&mut c, 10, json!({"match": "all", "rules": [{"field": "stacked", "op": "is", "value": true}]}));
    let flagged = smart(
        &mut c,
        11,
        json!({"match": "all", "rules": [{"field": "flag", "op": "is", "value": "pick"}, {"field": "kind", "op": "is", "value": "image"}]}),
    );
    let keyword = smart(&mut c, 12, json!({"match": "all", "rules": [{"field": "keywords", "op": "contains", "value": "kw42"}]}));
    let in_album = smart(&mut c, 13, json!({"match": "all", "rules": [{"field": "album", "op": "is", "value": album.0}]}));
    let recent = smart(&mut c, 14, json!({"match": "all", "rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 2, "unit": "years"}}]}));
    for (what, a) in [("stacked", stacked), ("flag + kind", flagged), ("keyword", keyword), ("in album", in_album), ("in the last 2 years", recent)] {
        let n = time(&format!("smart album count, {what}"), || c.album_count(a));
        eprintln!("  = {n}");
    }
    time("albums_of", || c.albums_of(PhotoId(6)));
    time("arrange stacks", || c.arrange_stacks(&c.query(&all, &Sort::default())));
    time("stack_of × 1000", || (0..1000u64).filter(|i| c.stack_of(PhotoId(i * 97 + 1)).is_some()).count());
}
