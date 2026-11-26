//! A whole count of every album (`AlbumTally::of`) keeps each smart album's photos for the pass, so an album testing
//! another doesn't work its rules out again. The counts must be the ones each album gets counted alone.

use serde_json::json;

use super::*;
use crate::tally::AlbumTally;

fn smart(id: u64, rules: serde_json::Value) -> Album {
    let rule_set = Some(serde_json::from_value(rules).unwrap());
    Album { smart: Some(Box::new(Filter { rule_set, ..Default::default() })), ..Album::new(AlbumId(id), format!("Smart {id}")) }
}

fn photos(n: u64) -> Vec<Op> {
    (0..n)
        .map(|i| {
            // folders with capitals, accents and a Windows path, so the text rules see every spelling
            let dir = ["Asian/Japanese", "Été/Ünï", "C:\\Users\\Me\\Pictures", "plain", "[SITES]/x"][(i % 5) as usize];
            let mut p = Photo::new(
                PhotoId(i + 1),
                Source::File { path: format!("/Volumes/share/photos/{dir}/name{}/IMG_{i:04}.jpg", i % 9) },
                &format!("IMG_{i:04}.jpg"),
                "JPEG",
                6000,
                4000,
                "2026-01-01T00:00:00",
            );
            p.rating = (i % 6) as u8;
            p.flag = [Flag::None, Flag::Pick, Flag::Reject][(i % 3) as usize];
            p.label = (i % 4 == 0).then_some(ColorLabel::Red);
            p.meta.keywords = (0..i % 4).map(|k| format!("kw{k}")).collect();
            p.deleted = i % 37 == 0;
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect()
}

fn is(album: u64) -> serde_json::Value {
    json!({"field": "album", "op": "is", "value": album})
}

fn is_not(album: u64) -> serde_json::Value {
    json!({"field": "album", "op": "isNot", "value": album})
}

fn path(op: &str, value: &str) -> serde_json::Value {
    json!({"field": "filePath", "op": op, "value": value})
}

/// Every album's count in `c`, each album counted alone (no pass, rules worked out per photo).
fn counted_alone(c: &Catalog) -> Vec<(AlbumId, usize)> {
    let mut v: Vec<_> = c.albums().map(|a| (a.id, c.album_count(a.id))).collect();
    v.sort_unstable();
    v
}

fn counted_together(c: &Catalog) -> Vec<(AlbumId, usize)> {
    let tally = AlbumTally::of(c);
    let mut v: Vec<_> = tally.counts().iter().map(|(id, n)| (*id, *n)).collect();
    v.sort_unstable();
    v
}

#[test]
fn a_whole_count_equals_each_album_counted_alone() {
    let mut c = Catalog::new();
    let mut ops = photos(240);
    let mut plain = Album::new(AlbumId(1), "Picks");
    plain.photos = (0..240).step_by(4).map(|i| PhotoId(i + 1)).collect();
    let folder = Album { folder: true, ..Album::new(AlbumId(2), "Trips") };
    let inside = Album { parent: Some(AlbumId(2)), photos: (0..240).step_by(3).map(|i| PhotoId(i + 1)).collect(), ..Album::new(AlbumId(3), "In") };
    ops.extend([plain, folder, inside].map(|album| Op::AddAlbum { album }));
    // the inner albums: #2-like, #14-like, then albums chained on it, the other way round, in groups and in different orders
    ops.push(Op::AddAlbum {
        album: smart(
            10,
            json!({"match": "any", "rules": [{"field": "flag", "op": "is", "value": "reject"}, {"field": "label", "op": "is", "value": "red"}]}),
        ),
    });
    ops.push(Op::AddAlbum {
        album: smart(11, json!({"match": "all", "rules": [is_not(10), path("notContains", "Été"), path("notContains", "plain")]})),
    });
    for k in 0..9u64 {
        let tail = path("contains", &format!("NAME{k}"));
        let rules = match k % 3 {
            0 => json!({"match": "all", "rules": [is(11), tail]}),
            1 => json!({"match": "all", "rules": [json!({"field": "keywordCount", "op": "lte", "value": 2}), is(11), tail]}),
            _ => json!({"match": "all", "rules": [tail, is(11)]}),
        };
        ops.push(Op::AddAlbum { album: smart(12 + k, rules) });
    }
    ops.push(Op::AddAlbum {
        album: smart(
            21,
            json!({"match": "all", "rules": [{"group": {"match": "any", "rules": [is(11), is(1)]}}, {"field": "rating", "op": "gte", "value": 2}]}),
        ),
    });
    ops.push(Op::AddAlbum { album: smart(22, json!({"match": "all", "rules": [is_not(21), is(3)]})) });
    ops.push(Op::AddAlbum { album: smart(23, json!({"match": "any", "rules": [is(2), is(999), is(1)]})) });
    // a loop saved anyway (30 ↔ 31), albums that test it from outside (32, 33), and one that tests both worlds
    ops.push(Op::AddAlbum { album: smart(30, json!({"rules": []})) });
    ops.push(Op::AddAlbum { album: smart(31, json!({"rules": [is_not(30)]})) });
    ops.push(Op::AddAlbum { album: smart(32, json!({"rules": [is(31), {"field": "rating", "op": "gte", "value": 1}]})) });
    ops.push(Op::AddAlbum { album: smart(33, json!({"rules": [is_not(32), is(11)]})) });
    ops.push(Op::AddAlbum { album: smart(34, json!({"rules": [is(34)]})) });
    // a chain deeper than the limit on how deep albums may test albums
    ops.push(Op::AddAlbum { album: smart(100, json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})) });
    for k in 101..=140u64 {
        ops.push(Op::AddAlbum { album: smart(k, json!({"rules": [is(k - 1)]})) });
    }
    ops.push(Op::AddAlbum { album: smart(141, json!({"rules": [is(140), is_not(11)]})) });
    // an album past the depth limit that also tests a memoized album directly: where the shared album is read from the
    // pass near the top, the chain below must still find it answered as it always did (200 ← 231 … 201, height 32)
    ops.push(Op::AddAlbum { album: smart(200, json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})) });
    for k in 201..=230u64 {
        ops.push(Op::AddAlbum { album: smart(k, json!({"rules": [is(k + 1)]})) });
    }
    ops.push(Op::AddAlbum { album: smart(231, json!({"rules": [is(200)]})) });
    ops.push(Op::AddAlbum { album: smart(250, json!({"match": "all", "rules": [is(200), is(201)]})) });
    c.apply(Op::Batch { ops }).unwrap();
    c.apply(Op::SetAlbumRules {
        id: AlbumId(30),
        rules: Box::new(Filter { rule_set: Some(serde_json::from_value(json!({"rules": [is_not(31)]})).unwrap()), ..Default::default() }),
    })
    .unwrap();

    let alone = counted_alone(&c);
    let count = |id: u64| alone.iter().find(|(a, _)| a.0 == id).map(|(_, n)| *n);
    assert!(count(11) > Some(0) && count(131) > Some(0), "{alone:?}");
    assert_eq!(count(132), Some(0), "the chain is too deep from here: holds nothing, as it did");
    assert!(count(250) > Some(0), "200 is answered near the top, so the chain finds it answered");
    assert_eq!(counted_together(&c), alone);
    // the pass leaves nothing behind: the same answers whichever album asks first, and again after an edit
    c.apply(Op::SetAlbumRules {
        id: AlbumId(10),
        rules: Box::new(Filter {
            rule_set: Some(serde_json::from_value(json!({"rules": [{"field": "rating", "op": "is", "value": 5}]})).unwrap()),
            ..Default::default()
        }),
    })
    .unwrap();
    let after = counted_alone(&c);
    assert_ne!(after, alone);
    assert_eq!(counted_together(&c), after);
}

/// The reason for the pass: with an album tested by many, its rules are worked out once per photo, not once per
/// photo for each album that tests it.
#[test]
fn an_album_tested_by_many_is_worked_out_once_a_photo() {
    let mut c = Catalog::new();
    let mut ops: Vec<Op> = (0..60u64)
        .map(|i| {
            let mut p = Photo::new(PhotoId(i + 1), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00");
            p.rating = (i % 6) as u8;
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    ops.push(Op::AddAlbum { album: smart(1, json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})) });
    for k in 0..10u64 {
        ops.push(Op::AddAlbum { album: smart(2 + k, json!({"rules": [is(1), {"field": "rating", "op": "lte", "value": k % 6}]})) });
    }
    c.apply(Op::Batch { ops }).unwrap();
    let alone = counted_alone(&c);
    crate::rules::EVALUATED.with(|n| n.set(0));
    let tally = AlbumTally::of(&c);
    let worked = crate::rules::EVALUATED.with(std::cell::Cell::get);
    assert_eq!(worked, 11 * 60, "each of the 11 smart albums asks each of the 60 photos once");
    let mut together: Vec<_> = tally.counts().iter().map(|(id, n)| (*id, *n)).collect();
    together.sort_unstable();
    assert_eq!(together, alone);
}
