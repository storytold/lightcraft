//! The free-text search and smart albums' Text rules read a photo's lower-cased text from a cache
//! ([`crate::index`]) rather than lower-casing every field of every photo on every keystroke. The
//! cache must give the matches the fields give, and follow every edit of a field it holds.

use lightcraft_meta::{Rect, Region, RegionKind};
use serde_json::json;

use super::*;
use crate::index::PhotoText;

fn photo(id: u64, file: &str) -> Photo {
    Photo::new(PhotoId(id), Source::Demo { scene: 1 }, file, "JPEG", 60, 40, "2026-01-01T00:00:00")
}

fn face(name: &str) -> Region {
    Region { rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 }, kind: RegionKind::Face, name: Some(name.into()), description: None }
}

/// Photos whose text is hard on a lower-casing shortcut: Unicode that changes length or depends on
/// its neighbours, a word that is only there across two fields, a colon in a plain word.
fn library() -> Catalog {
    let mut photos = Vec::new();
    let mut p = photo(1, "IMG_0001.JPG");
    p.rating = 3;
    p.meta.camera = "Canon EOS R5".into();
    p.meta.lens = "RF24-70mm F2.8 L".into();
    p.meta.title = "Trip to Ōsaka".into();
    p.meta.caption = "Straße am Fluss".into();
    p.meta.location = "Kyōto".into();
    p.meta.keywords = vec!["Travel|Italy".into(), "Ὀδυσσεύς".into(), "Kyōto Japan".into()];
    photos.push(p);
    let mut p = photo(2, "İstanbul.jpg");
    p.meta.title = "ΣΊΣΥΦΟΣ".into();
    p.meta.caption = "Re: 10:30 meeting".into();
    p.meta.city = "Zürich".into();
    p.meta.regions = vec![face("Ana Lima")];
    photos.push(p);
    let mut p = photo(3, "plain.png");
    p.format = "PNG".into();
    photos.push(p);
    let mut p = photo(4, "abc");
    p.meta.camera = "def".into();
    p.meta.title = "ǅ ǆ".into();
    photos.push(p);
    let mut p = photo(5, "name:colon.jpg");
    p.meta.lens = "title:trip".into();
    p.copy_name = Some("Black and White".into());
    photos.push(p);
    let mut c = Catalog::new();
    for p in photos {
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    c
}

/// How the search read a word before the cache: each field lower-cased alone, for every photo.
fn before_text_matches(p: &Photo, t: &str) -> bool {
    let hay = [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format];
    hay.iter().any(|h| h.to_lowercase().contains(t))
        || p.meta.keywords.iter().any(|k| k.to_lowercase().contains(t))
        || p.people().iter().any(|n| n.to_lowercase().contains(t))
}

/// [`before_text_matches`] for the words the tests use: plain ones and the fields the cache serves.
fn before_matches(p: &Photo, text: &str) -> bool {
    text.split_whitespace().all(|tok| {
        let t = tok.to_lowercase();
        match t.split_once(':') {
            Some(("camera", val)) => p.meta.camera.to_lowercase().contains(val),
            Some(("lens", val)) => p.meta.lens.to_lowercase().contains(val),
            Some(("name" | "file", val)) => p.file_name.to_lowercase().contains(val),
            Some(("rating", val)) => p.rating.to_string() == val,
            Some(("kw" | "keyword", val)) => p.meta.keywords.iter().any(|k| crate::keywords::is_under(k, val)),
            _ => before_text_matches(p, &t),
        }
    })
}

const QUERIES: &[&str] = &[
    "img",
    "IMG_0001",
    "IMG_0001.JPG",
    "canon r5",
    "CANON eos",
    "ōsaka",
    "ŌSAKA",
    "osaka",
    "straße",
    "STRASSE",
    "i̇stanbul",
    "İSTANBUL",
    "istanbul",
    "σίσυφος",
    "ΣΊΣΥΦΟΣ",
    "ς",
    "σ",
    "ὀδυσσεύς",
    "travel",
    "italy",
    "travel|italy",
    "kyōto japan",
    "kyōto",
    "10:30",
    "re:",
    "name:img",
    "NAME:IMG_0001",
    "file:plain",
    "name:",
    "camera:canon",
    "camera:r5",
    "lens:rf24",
    "lens:",
    "title:trip",
    "abcdef",
    "abc def",
    "ǆ",
    "rating:3 trip",
    "kw:travel ōsaka",
    "kw:travel osaka",
    "png jpeg",
    "",
    "   ",
    "zzz",
];

fn shown(c: &Catalog, text: &str) -> Vec<u64> {
    let f = Filter { text: text.into(), ..Default::default() };
    let mut ids: Vec<u64> = c.query(&f, &Sort::default()).into_iter().map(|id| id.0).collect();
    ids.sort_unstable();
    ids
}

fn expected(c: &Catalog, text: &str) -> Vec<u64> {
    c.photos().filter(|p| !p.deleted && before_matches(p, text)).map(|p| p.id.0).collect()
}

/// A search finds what the fields hold, whether it asks for one photo at a time, as a pass over the
/// library that makes the cache, or one photo after that.
#[test]
fn search_matches_as_the_fields_do() {
    let c = library();
    let mut one_by_one = Vec::new();
    for q in QUERIES.iter().copied() {
        let f = Filter { text: q.into(), ..Default::default() };
        one_by_one.push(c.photos().filter(|p| f.matches(p, &c)).map(|p| p.id.0).collect::<Vec<_>>());
    }
    assert!(c.photos().all(|p| c.search_text(p, false).is_none()), "testing one photo makes no cache for the library");
    for (q, got) in QUERIES.iter().zip(&one_by_one) {
        assert_eq!(*got, expected(&c, q), "{q:?}, one photo at a time");
    }
    for q in QUERIES.iter().copied() {
        assert_eq!(shown(&c, q), expected(&c, q), "{q:?}, a pass over the library");
    }
    assert!(c.photos().all(|p| c.search_text(p, false).is_some()), "a pass made the cache");
    for q in QUERIES.iter().copied() {
        let f = Filter { text: q.into(), ..Default::default() };
        let got: Vec<u64> = c.photos().filter(|p| f.matches(p, &c)).map(|p| p.id.0).collect();
        assert_eq!(got, expected(&c, q), "{q:?}, one photo at a time, with the cache");
    }
}

/// A photo that isn't the catalog's own (an edit not made yet, a candidate being tried) is read
/// from its own fields, not from what the catalog kept for the photo with its id.
#[test]
fn a_copy_of_a_photo_is_read_as_it_is() {
    let c = library();
    let f = Filter { text: "ōsaka".into(), ..Default::default() };
    assert_eq!(c.count(&f), 1);
    let mut p = (**c.photo(PhotoId(1)).unwrap()).clone();
    p.meta.title = "Trip to Nara".into();
    p.meta.keywords.clear();
    p.meta.location.clear();
    assert!(!f.matches(&p, &c), "the catalog's own text says Ōsaka, this copy doesn't");
    let nara = Filter { text: "nara".into(), ..Default::default() };
    assert!(nara.matches(&p, &c));
    assert!(!nara.matches(c.photo(PhotoId(1)).unwrap(), &c));
    let rule = Filter { rule_set: Some(rules(json!({"rules": [{"field": "text", "op": "contains", "value": "nara"}]}))), ..Default::default() };
    assert_eq!(c.count(&rule), 0);
    assert!(rule.matches(&p, &c));
}

fn rules(v: serde_json::Value) -> RuleSet {
    serde_json::from_value(v).unwrap()
}

fn text_rule(op: &str, value: &str) -> Filter {
    Filter { rule_set: Some(rules(json!({"rules": [{"field": "text", "op": op, "value": value}]}))), ..Default::default() }
}

/// What the cache holds is what a scan of the photos would give it, and every photo's is used.
fn assert_current(c: &Catalog) {
    for p in c.photos() {
        let kept = c.search_text(p, false).unwrap_or_else(|| panic!("photo {}: no text kept, or kept for another place", p.id.0));
        let now = PhotoText::of(p);
        assert_eq!(kept.text(), now.text(), "photo {}", p.id.0);
        assert_eq!(c.rule_text_of(p), Some(crate::rules::rule_text(p).as_str()), "photo {}: the Text rule's text", p.id.0);
    }
}

/// Make the caches, as the first search and the first smart album with a Text rule do.
fn warm(c: &Catalog) {
    c.count(&Filter { text: "x".into(), ..Default::default() });
    c.count(&text_rule("contains", "x"));
    assert_current(c);
}

fn found(c: &Catalog, text: &str) -> usize {
    c.count(&Filter { text: text.into(), ..Default::default() })
}

fn found_by_rule(c: &Catalog, text: &str) -> usize {
    c.count(&text_rule("contains", text))
}

/// An edit of each field that is searched puts the new text in the cache and takes the old one out,
/// through the op, its inverse and a batch that fails and is taken back.
#[test]
fn the_cache_follows_every_field_that_is_searched() {
    let mut c = library();
    warm(&c);
    let id = PhotoId(3);
    let edit = |c: &mut Catalog, set: &dyn Fn(&mut Meta)| {
        let mut meta = c.photo(id).unwrap().meta.clone();
        set(&mut meta);
        c.apply(Op::SetMeta { id, meta: Box::new(meta) }).unwrap()
    };
    type Set = fn(&mut Meta, &str);
    let fields: [(&str, Set); 6] = [
        ("title", |m, v| m.title = v.into()),
        ("caption", |m, v| m.caption = v.into()),
        ("camera", |m, v| m.camera = v.into()),
        ("lens", |m, v| m.lens = v.into()),
        ("location", |m, v| m.location = v.into()),
        ("keywords", |m, v| m.keywords = vec![v.into()]),
    ];
    for (n, (field, set)) in fields.into_iter().enumerate() {
        let word = format!("Zq{n}x");
        let before = found(&c, &word);
        let undo = edit(&mut c, &|m| set(m, &word));
        assert_eq!((before, found(&c, &word)), (0, 1), "{field}: found after the edit");
        assert_eq!(found_by_rule(&c, &word.to_lowercase()), 1, "{field}: the Text rule");
        assert_current(&c);
        c.apply(undo).unwrap();
        assert_eq!(found(&c, &word), 0, "{field}: gone after the undo");
        assert_current(&c);
    }
    // the fields only Text rules read
    let rule_fields: [(&str, Set); 3] =
        [("city", |m, v| m.city = v.into()), ("state", |m, v| m.state = v.into()), ("country", |m, v| m.country = v.into())];
    for (n, (field, set)) in rule_fields.into_iter().enumerate() {
        let word = format!("zr{n}y");
        let undo = edit(&mut c, &|m| set(m, &word));
        assert_eq!((found_by_rule(&c, &word), found(&c, &word)), (1, 0), "{field}");
        assert_current(&c);
        c.apply(undo).unwrap();
        assert_eq!(found_by_rule(&c, &word), 0, "{field}: gone after the undo");
    }
    let undo = edit(&mut c, &|m| m.alt_text = "zalt".into());
    assert_eq!(found_by_rule(&c, "zalt"), 1);
    c.apply(undo).unwrap();
    assert_eq!(found_by_rule(&c, "zalt"), 0);
    let undo = edit(&mut c, &|m| m.regions = vec![face("Zed Person")]);
    assert_eq!(found_by_rule(&c, "zed person"), 1, "a person named on a face");
    c.apply(undo).unwrap();
    assert_eq!(found_by_rule(&c, "zed person"), 0);
    assert_current(&c);
}

#[test]
fn the_cache_follows_a_file_name_and_a_format() {
    let mut c = library();
    warm(&c);
    let id = PhotoId(3);
    let source = Source::Demo { scene: 1 };
    let undo = c.apply(Op::SetFile { id, file_name: "Renamed_0042.PNG".into(), source: source.clone() }).unwrap();
    assert_eq!((found(&c, "renamed_0042"), found(&c, "plain"), found(&c, "name:renamed")), (1, 0, 1));
    assert_eq!((found_by_rule(&c, "renamed_0042"), found_by_rule(&c, "plain")), (1, 0));
    assert_current(&c);
    c.apply(undo).unwrap();
    assert_eq!((found(&c, "renamed_0042"), found(&c, "plain")), (0, 1));
    assert_current(&c);

    let undo = c.apply(Op::Relink { id, file_name: "Moved.TIF".into(), source: source.clone(), format: Some("TIFF".into()) }).unwrap();
    assert_eq!((found(&c, "moved"), found(&c, "tiff"), found(&c, "png")), (1, 1, 0));
    assert_eq!((found_by_rule(&c, "moved"), found_by_rule(&c, "tiff"), found_by_rule(&c, "png")), (1, 1, 0));
    assert_current(&c);
    c.apply(undo).unwrap();
    assert_eq!((found(&c, "moved"), found(&c, "tiff"), found(&c, "png")), (0, 0, 1));
    assert_current(&c);
}

/// A photo that comes or goes (an import, a virtual copy, a permanent delete and its undo) does
/// too, and a virtual copy's name is read by a Text rule.
#[test]
fn the_cache_follows_photos_that_come_and_go() {
    let mut c = library();
    warm(&c);
    let mut copy = (**c.photo(PhotoId(1)).unwrap()).clone();
    copy.id = PhotoId(9);
    copy.copy_of = Some(PhotoId(1));
    copy.meta = Default::default();
    copy.copy_name = Some("Zcopy Name".into());
    copy.file_name = "Zfresh.jpg".into();
    let undo = c.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    assert_eq!((found(&c, "zfresh"), found_by_rule(&c, "zcopy"), found(&c, "zcopy")), (1, 1, 0));
    assert_current(&c);
    let redo = c.apply(undo).unwrap();
    assert_eq!((found(&c, "zfresh"), found_by_rule(&c, "zcopy")), (0, 0));
    assert_current(&c);
    c.apply(redo).unwrap();
    assert_eq!(found(&c, "zfresh"), 1);
    assert_current(&c);
    let removed = c.apply(Op::RemovePhoto { id: PhotoId(1) }).unwrap();
    assert_eq!(found(&c, "ōsaka"), 0);
    assert_current(&c);
    c.apply(removed).unwrap();
    assert_eq!(found(&c, "ōsaka"), 1);
    assert_current(&c);
}

/// A batch that fails half way is taken back, and the cache with it.
#[test]
fn the_cache_follows_a_batch_that_fails() {
    let mut c = library();
    warm(&c);
    let mut meta = c.photo(PhotoId(3)).unwrap().meta.clone();
    meta.title = "Zbatch title".into();
    let batch = Op::Batch {
        ops: vec![
            Op::SetMeta { id: PhotoId(3), meta: Box::new(meta) },
            Op::SetFile { id: PhotoId(3), file_name: "Zbatch.png".into(), source: Source::Demo { scene: 1 } },
            Op::SetFile { id: PhotoId(404), file_name: "gone.png".into(), source: Source::Demo { scene: 1 } },
        ],
    };
    assert!(c.apply(batch).is_err());
    assert_eq!((found(&c, "zbatch"), found_by_rule(&c, "zbatch")), (0, 0));
    assert_eq!(found(&c, "plain"), 1);
    assert_current(&c);
}

/// An edit of a photo that something else holds (a thumbnail list, an undo step) puts it at another
/// place in memory. The cache follows it, so a search isn't left reading every such photo from its fields.
#[test]
fn the_cache_follows_a_photo_that_others_hold() {
    let mut c = library();
    warm(&c);
    let held: Vec<_> = c.photos().cloned().collect();
    for p in &held {
        c.apply(Op::SetRating { id: p.id, rating: 5 }).unwrap();
    }
    assert!(held.iter().all(|old| !std::ptr::eq(&**old, &**c.photo(old.id).unwrap())), "the edits copied the photos");
    assert_current(&c);
    assert_eq!(found(&c, "ōsaka"), 1);
    c.apply(Op::SetFile { id: PhotoId(2), file_name: "after.jpg".into(), source: Source::Demo { scene: 1 } }).unwrap();
    assert_current(&c);
    assert_eq!((found(&c, "after"), found(&c, "istanbul")), (1, 0));
}

/// A catalog read from a snapshot or cloned starts with no cache and builds its own.
#[test]
fn a_clone_builds_its_own() {
    let mut c = library();
    warm(&c);
    let mut copy = c.clone();
    assert!(copy.photos().all(|p| copy.search_text(p, false).is_none()));
    assert_eq!(found(&copy, "ōsaka"), 1);
    warm(&copy);
    let mut meta = copy.photo(PhotoId(1)).unwrap().meta.clone();
    meta.title = "Elsewhere".into();
    copy.apply(Op::SetMeta { id: PhotoId(1), meta: Box::new(meta) }).unwrap();
    assert_eq!((found(&copy, "elsewhere"), found(&c, "elsewhere")), (1, 0));
    c.apply(Op::SetRating { id: PhotoId(1), rating: 1 }).unwrap();
    assert_current(&c);
}

/// The words of Text rules (not the search) read the same with the cache as without.
#[test]
fn text_rules_match_as_the_fields_do() {
    let c = library();
    let every_op = ["contains", "notContains", "is", "isNot", "startsWith", "endsWith", "isEmpty", "isNotEmpty"];
    let values =
        ["", "ōsaka", "TRIP TO", "kyōto japan", "ana lima", "zürich", "img_0001.jpg", "black and white", "ς", "i̇stanbul", "png", "re: 10:30 meeting"];
    let before: Vec<Vec<usize>> =
        every_op.iter().map(|op| values.iter().map(|v| c.photos().filter(|p| text_rule(op, v).matches(p, &c)).count()).collect()).collect();
    let after: Vec<Vec<usize>> = every_op.iter().map(|op| values.iter().map(|v| c.count(&text_rule(op, v))).collect()).collect();
    assert_eq!(before, after);
    assert!(c.photos().all(|p| c.rule_text_of(p).is_some()), "the first rule asked made the text");
    let again: Vec<Vec<usize>> =
        every_op.iter().map(|op| values.iter().map(|v| c.photos().filter(|p| text_rule(op, v).matches(p, &c)).count()).collect()).collect();
    assert_eq!(before, again);
}

/// A smart album's count through the tally path (`album_contains`, one photo at a time) reads the
/// kept text: it is made there, and an edit shows in the next count.
#[test]
fn a_smart_album_count_reads_the_kept_text() {
    let mut c = library();
    let mut album = Album::new(AlbumId(1), "Osaka");
    album.smart = Some(Box::new(text_rule("contains", "ōsaka")));
    c.apply(Op::AddAlbum { album }).unwrap();
    let one_by_one = |c: &Catalog| c.photos().filter(|p| c.album_contains(AlbumId(1), p)).count();
    assert_eq!(one_by_one(&c), 1);
    assert!(c.photos().all(|p| c.rule_text_of(p).is_some()), "counting made the text");
    let mut meta = c.photo(PhotoId(3)).unwrap().meta.clone();
    meta.title = "Ōsaka again".into();
    c.apply(Op::SetMeta { id: PhotoId(3), meta: Box::new(meta) }).unwrap();
    assert_eq!(one_by_one(&c), 2);
}

/// The kept text holds the names of the people on a photo's faces, and a quoted phrase is read as
/// one word that never runs from one field into the next.
#[test]
fn the_kept_text_holds_the_people_and_a_phrase_stays_in_one_field() {
    let mut c = library();
    warm(&c);
    assert_eq!(found(&c, "ana"), 1, "a person's name is searched");
    assert_eq!(found(&c, "\"ana lima\""), 1, "a quoted name is one phrase");
    assert_eq!(found(&c, "\"lima ana\""), 0);
    assert_eq!(found(&c, "\"ōsaka straße\""), 0, "a phrase doesn't run from the title into the caption");
    assert_eq!(found(&c, "\"trip to ōsaka\""), 1, "…but fits inside one field");
    assert_eq!(found(&c, "\"Kyōto Japan\""), 1, "…or one keyword");
    // (the photo that has Ana Lima's face)
    let mut meta = c.photo(PhotoId(2)).unwrap().meta.clone();
    meta.regions = vec![face("Zed Person")];
    c.apply(Op::SetMeta { id: PhotoId(2), meta: Box::new(meta) }).unwrap();
    assert_eq!(found(&c, "\"zed person\""), 1, "a renamed person is found at once");
    assert_eq!(found(&c, "ana"), 0, "and the old name is gone");
    assert_current(&c);
}
