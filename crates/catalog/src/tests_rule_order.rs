//! A rule set is tested cheapest rule first ([`RuleSet::by_cost`]), once per pass; what matches
//! is what testing the rules in the order they were written gives.
//!
//! Scenarios:
//! - Given random libraries (Unicode and Windows paths, nested groups, album rules that test
//!   smart albums and missing albums), then every album, query and count gives what the written
//!   order gives, for all, any and none.
//! - Given a library where smart albums test themselves (the app refuses to make one; a saved or
//!   damaged catalog can hold it, and there the answer can depend on which album is asked first),
//!   then the rules are tested as written, so the answers are the written order's, as before.
//! - Given `all[album is <smart album>, <a rule no photo meets>]`, then the smart album is never
//!   asked about a photo: the cheap rule ends it first (written first, it ran for every photo).
//! - Given rules in any order, then the saved rules and the order the editor shows are untouched.

use serde_json::json;

use crate::rules::{EVALUATED, WRITTEN_ORDER};
use crate::*;

/// A small generator: the same sequence every run.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
    fn pick<'a, T>(&mut self, of: &'a [T]) -> &'a T {
        &of[self.next(of.len() as u64) as usize]
    }
}

const PATHS: &[&str] = &[
    "/Volumes/share/photos/Asie/Japon/Été/IMG_0001.jpg",
    "/Volumes/share/photos/ASIE/japon/IMG_0002.JPG",
    "C:\\Users\\me\\Pictures\\Ünïcode\\Tokyo\\IMG_0003.jpg",
    "/Volumes/share/photos/Straße/İstanbul/IMG_0004.cr2",
    "/Volumes/share/photos/東京/渋谷/IMG_0005.jpg",
    "/Volumes/share/photos/Asie/Chine/clip.mp4",
    "relative/dir/IMG_0007.jpg",
];

const WORDS: &[&str] =
    &["asie", "Japon", "été", "ÜNÏCODE", "tokyo", "straße", "i̇stanbul", "東京", "photos\\asie", "photos/Asie/", " ", "", "img_000"];

fn library(rng: &mut Rng) -> (Catalog, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let ids: Vec<PhotoId> = (0..24)
        .map(|i| {
            let id = c.alloc_photo_id();
            let path = if i == 5 { None } else { Some(rng.pick(PATHS).to_string()) };
            let source = path.map_or(Source::Demo { scene: 1 }, |path| Source::File { path });
            let name = format!("IMG_{i:04}.{}", ["jpg", "cr2", "mp4"][rng.next(3) as usize]);
            let mut p = Photo::new(id, source, &name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
            p.rating = rng.next(6) as u8;
            p.flag = [Flag::None, Flag::Pick, Flag::Reject][rng.next(3) as usize];
            p.label = [None, Some(ColorLabel::Red), Some(ColorLabel::Blue)][rng.next(3) as usize];
            p.meta.keywords = (0..rng.next(4)).map(|k| format!("kw{}", k + rng.next(2))).collect();
            p.captured = Some(format!("20{}-0{}-1{}T10:00:00", 10 + rng.next(16), 1 + rng.next(9), rng.next(9)));
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id
        })
        .collect();
    (c, ids)
}

fn leaf(rng: &mut Rng, albums: &[AlbumId]) -> serde_json::Value {
    let field = |f: &str, op: &str, v: serde_json::Value| json!({"field": f, "op": op, "value": v});
    match rng.next(14) {
        0 => field("rating", ["gte", "lte", "is", "gt", "lt"][rng.next(5) as usize], json!(rng.next(6))),
        1 => field("flag", ["is", "isNot"][rng.next(2) as usize], json!(["pick", "reject", "none"][rng.next(3) as usize])),
        2 => field("label", ["is", "isNot"][rng.next(2) as usize], json!(["red", "blue", "none"][rng.next(3) as usize])),
        3 => field("kind", ["is", "isNot"][rng.next(2) as usize], json!(["image", "raw", "video"][rng.next(3) as usize])),
        4 => field("keywordCount", ["lte", "gte"][rng.next(2) as usize], json!(rng.next(4))),
        5 => field("keywords", ["contains", "notContains", "isEmpty"][rng.next(3) as usize], json!(format!("kw{}", rng.next(3)))),
        6 => field("captureDate", ["before", "after"][rng.next(2) as usize], json!(format!("20{}", 10 + rng.next(16)))),
        7 | 8 => field("filePath", ["contains", "notContains", "startsWith", "is", "isEmpty"][rng.next(5) as usize], json!(*rng.pick(WORDS))),
        9 => field("fileName", "contains", json!(["img_000", ".cr2", "mp4"][rng.next(3) as usize])),
        10 => field("text", ["contains", "notContains"][rng.next(2) as usize], json!(*rng.pick(WORDS))),
        // an album that may not exist (99) or may be on a loop
        _ => {
            let album = if rng.next(8) == 0 { AlbumId(99) } else { *rng.pick(albums) };
            field("album", ["is", "isNot"][rng.next(2) as usize], json!(album.0))
        }
    }
}

fn rule_set(rng: &mut Rng, albums: &[AlbumId], depth: u32) -> serde_json::Value {
    let rules: Vec<serde_json::Value> = (0..rng.next(5))
        .map(|_| if depth < 2 && rng.next(4) == 0 { json!({"group": rule_set(rng, albums, depth + 1)}) } else { leaf(rng, albums) })
        .collect();
    let mode = ["all", "any", "none"][rng.next(3) as usize];
    json!({"match": mode, "rules": rules})
}

fn filter(rules: serde_json::Value) -> Filter {
    Filter { rule_set: Some(serde_json::from_value(rules).unwrap()), ..Default::default() }
}

/// What a catalog answers for every album, photo and a few queries.
fn answers(c: &Catalog, albums: &[AlbumId], photos: &[PhotoId], queries: &[Filter]) -> String {
    let mut out = String::new();
    for a in albums {
        let held: Vec<bool> = photos.iter().map(|p| c.album_contains(*a, c.photo(*p).unwrap())).collect();
        out += &format!("{a:?} {held:?} {:?} {}\n", c.album_photos(*a), c.album_count(*a));
    }
    for p in photos {
        out += &format!("{p:?} {:?}\n", c.albums_of(*p));
    }
    for q in queries {
        out += &format!("{:?} {}\n", c.query(q, &Sort::default()), c.count(q));
    }
    out
}

#[test]
fn the_cheapest_first_order_matches_what_the_written_order_matches() {
    let chained = same_answers_in_random_libraries(Rng(11), false);
    assert!(chained > 20, "the generator makes smart albums that test smart albums: {chained}");
}

#[test]
fn a_library_with_loops_answers_as_written_and_does_not_hang() {
    let looping = same_answers_in_random_libraries(Rng(5), true);
    assert!(looping > 20, "the generator makes loops: {looping}");
}

/// Albums of random libraries and queries answer the same cheapest first and as written; returns
/// how many smart albums tested a smart album (`loops`: one that leads back to itself).
fn same_answers_in_random_libraries(mut rng: Rng, loops: bool) -> usize {
    let mut chained = 0;
    for round in 0..150 {
        let (mut c, photos) = library(&mut rng);
        let plain = c.alloc_album_id();
        c.apply(Op::AddAlbum { album: Album { photos: photos.iter().copied().step_by(3).collect(), ..Album::new(plain, "Plain") } }).unwrap();
        let folder = c.alloc_album_id();
        c.apply(Op::AddAlbum { album: Album { folder: true, ..Album::new(folder, "F") } }).unwrap();
        let smarts: Vec<AlbumId> = (0..6)
            .map(|i| {
                let id = c.alloc_album_id();
                let parent = (i % 3 == 0).then_some(folder);
                c.apply(Op::AddAlbum { album: Album { parent, smart: Some(Box::default()), ..Album::new(id, format!("S{i}")) } }).unwrap();
                id
            })
            .collect();
        let albums: Vec<AlbumId> = [plain, folder].into_iter().chain(smarts.iter().copied()).collect();
        for (i, s) in smarts.iter().enumerate() {
            // (without loops a smart album tests only the albums made before it)
            let testable = if loops { &albums[..] } else { &albums[..2 + i] };
            let _ = c.apply(Op::SetAlbumRules { id: *s, rules: Box::new(filter(rule_set(&mut rng, testable, 0))) });
        }
        let queries: Vec<Filter> = (0..4).map(|_| filter(rule_set(&mut rng, &albums, 0))).collect();
        // (the copy starts without what the catalog worked out, so each is worked out under its own order)
        let written = c.clone();
        let ordered_answers = answers(&c, &albums, &photos, &queries);
        WRITTEN_ORDER.set(true);
        let written_answers = answers(&written, &albums, &photos, &queries);
        WRITTEN_ORDER.set(false);
        assert_eq!(ordered_answers, written_answers, "round {round}: {}", c.to_snapshot());
        let reaching = |s: &AlbumId| {
            smarts.iter().any(|t| if loops { c.album_reaches(*t, *s) && c.album_reaches(*s, *t) } else { t != s && c.album_reaches(*s, *t) })
        };
        chained += smarts.iter().filter(|s| reaching(s)).count();
        assert_eq!(c.keeps_written_order(), smarts.iter().any(|s| c.album_reaches(*s, *s)));
        assert!(loops || !c.keeps_written_order(), "round {round}: {}", c.to_snapshot());
    }
    chained
}

#[test]
fn a_cheap_rule_that_fails_ends_the_test_before_a_smart_album_runs() {
    let mut c = Catalog::new();
    let photos: Vec<PhotoId> = (0..20)
        .map(|i| {
            let id = c.alloc_photo_id();
            let p = Photo::new(id, Source::File { path: format!("/a/b/{i}.jpg") }, &format!("{i}.jpg"), "JPEG", 6000, 4000, "2026-09-30T10:00:00");
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id
        })
        .collect();
    let inner = c.alloc_album_id();
    c.apply(Op::AddAlbum {
        album: Album {
            smart: Some(Box::new(filter(json!({"rules": [{"field": "filePath", "op": "contains", "value": "/a/"}]})))),
            ..Album::new(inner, "Inner")
        },
    })
    .unwrap();
    let rules = |first: serde_json::Value, second: serde_json::Value| json!({"match": "all", "rules": [first, second]});
    let album = json!({"field": "album", "op": "is", "value": inner.0});
    // no photo is rated 6 stars (a rating is at most 5)
    let never = json!({"field": "rating", "op": "gte", "value": 6});
    let outer = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(filter(rules(album.clone(), never.clone())))), ..Album::new(outer, "Outer") } })
        .unwrap();
    EVALUATED.set(0);
    assert_eq!(c.album_count(outer), 0);
    // (the outer album's own rules count as one run for each photo; its inner album must run for none)
    assert_eq!(EVALUATED.get(), photos.len(), "only the outer album ran, once per photo");
    // the same photos whichever way round the rules are written
    let swapped = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(filter(rules(never, album)))), ..Album::new(swapped, "Swapped") } }).unwrap();
    assert_eq!((c.album_count(swapped), c.album_photos(swapped)), (0, vec![]));
}

#[test]
fn rules_are_ordered_by_cost_without_touching_the_saved_ones() {
    let mut c = Catalog::new();
    let plain = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album::new(plain, "Plain") }).unwrap();
    let smart = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::default()), ..Album::new(smart, "Smart") } }).unwrap();
    let written: RuleSet = serde_json::from_value(json!({"match": "all", "rules": [
        {"field": "album", "op": "is", "value": smart.0},
        {"field": "filePath", "op": "contains", "value": "x"},
        {"group": {"match": "any", "rules": [
            {"field": "album", "op": "isNot", "value": smart.0},
            {"field": "rating", "op": "gte", "value": 3}]}},
        {"field": "keywords", "op": "contains", "value": "k"},
        {"field": "album", "op": "is", "value": plain.0},
        {"field": "rating", "op": "gte", "value": 3},
        {"field": "album", "op": "is", "value": 99},
        {"field": "flag", "op": "is", "value": "pick"}]}))
    .unwrap();
    let before = written.clone();
    let ordered = written.by_cost(&c);
    assert_eq!(written, before, "the saved rules stay as the user wrote them");
    let names: Vec<String> = ordered
        .rules
        .iter()
        .map(|r| match r {
            Rule::Group { .. } => "group".to_string(),
            Rule::Field { field, op, .. } => format!("{field} {op}"),
        })
        .collect();
    // scalars (a missing album is false at once), then lists and a plain album, then text, then
    // smart albums and the group that holds one; equals keep the written order
    assert_eq!(names, ["rating gte", "album is", "flag is", "keywords contains", "album is", "filePath contains", "album is", "group"]);
    let inner: Vec<&str> = match ordered.rules.get(7) {
        Some(Rule::Group { group }) => group.rules.iter().map(|r| if let Rule::Field { field, .. } = r { field.as_str() } else { "group" }).collect(),
        _ => Vec::new(),
    };
    assert_eq!(inner, ["rating", "album"], "nested groups are ordered too");
}

/// B = all[album is C], C = all[album isNot B], A = all[any[album is C, flag is pick], album is B]:
/// asked in the order the rules are written, a picked photo is not in A; asked cheapest first
/// (the flag, then the albums) it was. With a loop the order stays as written.
#[test]
fn smart_albums_that_test_each_other_keep_the_written_order() {
    let mut c = Catalog::new();
    let mut p = Photo::new(c.alloc_photo_id(), Source::File { path: "/a/1.jpg".into() }, "1.jpg", "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.flag = Flag::Pick;
    let photo = p.id;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let ids: Vec<AlbumId> = ["A", "B", "C"]
        .iter()
        .map(|name| {
            let id = c.alloc_album_id();
            c.apply(Op::AddAlbum { album: Album { smart: Some(Box::default()), ..Album::new(id, *name) } }).unwrap();
            id
        })
        .collect();
    let (a, b, cc) = (ids[0], ids[1], ids[2]);
    let album = |op: &str, id: AlbumId| json!({"field": "album", "op": op, "value": id.0});
    let set = |c: &mut Catalog, id: AlbumId, rules: serde_json::Value| c.apply(Op::SetAlbumRules { id, rules: Box::new(filter(rules)) }).unwrap();
    set(&mut c, b, json!({"match": "all", "rules": [album("is", cc)]}));
    set(&mut c, cc, json!({"match": "all", "rules": [album("isNot", b)]}));
    set(
        &mut c,
        a,
        json!({"match": "all", "rules": [
            {"group": {"match": "any", "rules": [album("is", cc), {"field": "flag", "op": "is", "value": "pick"}]}},
            album("is", b)]}),
    );
    let written = c.clone();
    let ask = |c: &Catalog| {
        ids.iter().map(|id| (c.album_contains(*id, c.photo(photo).unwrap()), c.album_photos(*id), c.album_count(*id))).collect::<Vec<_>>()
    };
    WRITTEN_ORDER.set(true);
    let as_written = ask(&written);
    WRITTEN_ORDER.set(false);
    assert!(c.keeps_written_order());
    assert_eq!(ask(&c), as_written);
}

/// A = all[any[album is D1, flag is pick], album is W], D1 tests D2 … D30, D30 tests W, W tests Z,
/// Z = rating ≥ 0: asked from A, the chain is cut by the depth limit, and asked cheapest first (the
/// flag, then the albums) the cut falls elsewhere. Chains that long keep the written order.
#[test]
fn a_chain_as_deep_as_the_limit_keeps_the_written_order() {
    let mut c = Catalog::new();
    let mut p = Photo::new(c.alloc_photo_id(), Source::File { path: "/a/1.jpg".into() }, "1.jpg", "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.flag = Flag::Pick;
    p.rating = 0; // (Z, rating at least 0, holds for it)
    let photo = p.id;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let new_smart = |c: &mut Catalog, name: &str| {
        let id = c.alloc_album_id();
        c.apply(Op::AddAlbum { album: Album { smart: Some(Box::default()), ..Album::new(id, name) } }).unwrap();
        id
    };
    let a = new_smart(&mut c, "A");
    let chain: Vec<AlbumId> = (1..=30).map(|i| new_smart(&mut c, &format!("D{i}"))).collect();
    let (w, z) = (new_smart(&mut c, "W"), new_smart(&mut c, "Z"));
    let album = |id: AlbumId| json!({"field": "album", "op": "is", "value": id.0});
    let set = |c: &mut Catalog, id: AlbumId, rules: serde_json::Value| c.apply(Op::SetAlbumRules { id, rules: Box::new(filter(rules)) }).unwrap();
    set(
        &mut c,
        a,
        json!({"match": "all", "rules": [{"group": {"match": "any", "rules": [album(chain[0]), {"field": "flag", "op": "is", "value": "pick"}]}}, album(w)]}),
    );
    for pair in chain.windows(2) {
        set(&mut c, pair[0], json!({"match": "all", "rules": [album(pair[1])]}));
    }
    set(&mut c, chain[29], json!({"match": "all", "rules": [album(w)]}));
    set(&mut c, w, json!({"match": "all", "rules": [album(z)]}));
    set(&mut c, z, json!({"match": "all", "rules": [{"field": "rating", "op": "gte", "value": 0}]}));
    let written = c.clone();
    let ask = |c: &Catalog| [a, w, z, chain[0]].map(|id| (c.album_contains(id, c.photo(photo).unwrap()), c.album_photos(id), c.album_count(id)));
    WRITTEN_ORDER.set(true);
    let as_written = ask(&written);
    // (the expected answers, written down: the same evaluator gives both sides below, so a
    // regression that lets the 33rd album in would otherwise change both alike)
    assert_eq!(as_written.each_ref().map(|answer| answer.0), [false, true, true, true], "the depth limit cuts the chain only while A is asked");
    WRITTEN_ORDER.set(false);
    assert!(c.keeps_written_order());
    assert_eq!(ask(&c), as_written);
}

/// What was worked out from the albums (whether rules may be tested cheapest first) is worked out
/// again after an album is added or removed: 31 smart albums in a chain are fine, a 32nd on top of
/// it reaches the depth limit (the written order from then on), and taking it away lifts that.
#[test]
fn adding_or_removing_an_album_forgets_whether_the_rules_may_be_reordered() {
    let mut c = Catalog::new();
    let mut p = Photo::new(c.alloc_photo_id(), Source::File { path: "/a/1.jpg".into() }, "1.jpg", "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.flag = Flag::Pick;
    let photo = p.id;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let album = |id: AlbumId| json!({"field": "album", "op": "is", "value": id.0});
    let smart = |c: &mut Catalog, name: &str, rules: serde_json::Value| {
        let id = c.alloc_album_id();
        c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(filter(rules))), ..Album::new(id, name) } }).unwrap();
        id
    };
    // S1 tests S2 … S30 tests S31, which tests nothing: 31 albums
    let mut chain = vec![smart(&mut c, "S31", json!({"match": "all", "rules": []}))];
    for i in (1..=30).rev() {
        let next = chain[chain.len() - 1];
        chain.push(smart(&mut c, &format!("S{i}"), json!({"match": "all", "rules": [album(next)]})));
    }
    let bottom = chain[0];
    let top = chain[30];
    let ask = |c: &Catalog| [top, bottom].map(|id| (c.album_contains(id, c.photo(photo).unwrap()), c.album_photos(id), c.album_count(id)));
    assert!(!c.keeps_written_order(), "31 albums in a chain are under the limit (worked out, and remembered)");
    let before = ask(&c);
    let head = smart(&mut c, "S0", json!({"match": "all", "rules": [album(top)]}));
    assert!(c.keeps_written_order(), "a 32nd album on top of the chain reaches the depth limit");
    assert_eq!(c.clone().keeps_written_order(), c.keeps_written_order(), "as a catalog that worked it out afresh");
    assert_eq!(ask(&c), ask(&c.clone()));
    c.apply(Op::RemoveAlbum { id: head }).unwrap();
    assert!(!c.keeps_written_order(), "without it the chain is under the limit again");
    assert_eq!(c.clone().keeps_written_order(), c.keeps_written_order());
    assert_eq!(ask(&c), before);
}
