//! The view follows a small edit from the list it had ([`Session::visible`]): whatever the edit, the result has to
//! be the list worked out from every photo.

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;

/// A small generator of numbers that is the same on every run.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: usize) -> usize {
        self.0 = lightcraft_catalog::mix64(self.0);
        (self.0 % sides.max(1) as u64) as usize
    }
}

/// A library of `n` photos in three folders, with ratings, flags, keywords and dates spread over them, an album
/// holding every third photo, a smart album of the photos without keywords, and a stack.
fn library(n: usize) -> (Session, Vec<PhotoId>, u64, u64) {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..n {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{:04}.jpg", (i * 37) % n);
        let mut p = Photo::new(
            id,
            Source::File { path: format!("/pics/{}/{name}", i % 3) },
            &name,
            "JPEG",
            60,
            40,
            &format!("2026-01-{:02}T10:00:00", 1 + i % 9),
        );
        p.captured = Some(format!("2025-{:02}-{:02}T10:00:{:02}", 1 + i % 12, 1 + i % 28, i % 60));
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let pick = |s: &mut Session, ids: Vec<u64>, command: &str, params: Value| {
        s.execute("library.select", &json!({"ids": ids})).unwrap();
        s.execute(command, &params).unwrap();
    };
    let every = |k: usize, of: usize| ids.iter().enumerate().filter(|(i, _)| i % of == k).map(|(_, id)| id.0).collect::<Vec<u64>>();
    pick(&mut s, every(0, 4), "photo.rate", json!({"rating": 3}));
    pick(&mut s, every(1, 4), "photo.rate", json!({"rating": 5}));
    pick(&mut s, every(0, 5), "photo.flag", json!({"flag": "pick"}));
    pick(&mut s, every(0, 2), "photo.setMeta", json!({"addKeywords": ["travel"]}));
    pick(&mut s, every(0, 7), "photo.setMeta", json!({"addKeywords": ["weddings"]}));
    let album = s.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    s.execute("album.addPhotos", &json!({"id": album, "ids": every(0, 3)})).unwrap();
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "keywordCount", "op": "is", "value": 0}]}});
    let smart = s.execute("album.createSmart", &json!({"name": "Unkeyworded", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    pick(&mut s, vec![ids[10].0, ids[11].0, ids[12].0], "stack.group", json!({}));
    (s, ids, album, smart)
}

/// One edit at random: most touch a photo or a few (what it is rated, flagged, keyworded, developed; when it was
/// taken; where its file is; whether it is in the library at all), some change an album, a stack or the library's
/// past, and now and then a photo is added or removed for good.
fn edit(s: &mut Session, dice: &mut Dice, ids: &mut Vec<PhotoId>, album: u64) {
    let chosen: Vec<u64> = (0..1 + dice.roll(3)).map(|_| ids[dice.roll(ids.len())].0).collect();
    let one = PhotoId(chosen[0]);
    let n = dice.roll(1000);
    // (ops a command would refuse here, or on a photo that is gone, are edits that were not made: also a case)
    match dice.roll(9) {
        0 => {
            let _ =
                s.commit("Capture time", Op::SetCaptured { id: one, captured: Some(format!("2025-{:02}-{:02}T08:00:00", 1 + n % 12, 1 + n % 28)) });
            return;
        }
        1 => {
            let name = format!("REN_{n:04}.jpg");
            let source = Source::File { path: format!("/pics/{}/{name}", n % 3) };
            let _ = s.commit("Locate", Op::Relink { id: one, file_name: name, source, format: None });
            return;
        }
        2 if n.is_multiple_of(4) => {
            let id = s.catalog.alloc_photo_id();
            let name = format!("NEW_{n:04}.jpg");
            let mut p = Photo::new(id, Source::File { path: format!("/pics/{}/{name}", n % 3) }, &name, "JPEG", 60, 40, "2026-02-01T10:00:00");
            p.captured = Some(format!("2025-{:02}-11T09:00:00", 1 + n % 12));
            if s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).is_ok() {
                ids.push(id);
            }
            return;
        }
        2 if n % 4 == 1 => {
            let _ = s.commit("Remove", Op::RemovePhoto { id: one });
            return;
        }
        2 => {
            let _ = s.commit("Local", Op::SetLocal { id: one, local: n.is_multiple_of(2) });
            return;
        }
        _ => {}
    }
    // (a photo in Recently Deleted can't be chosen: the edit is then not made, which is an edit too)
    let _ = s.execute("library.select", &json!({"ids": chosen}));
    let flag = ["pick", "reject", "none"][dice.roll(3)];
    let keyword = ["travel", "weddings", "pets"][dice.roll(3)];
    let _ = match dice.roll(12) {
        0 | 1 => s.execute("photo.rate", &json!({"rating": dice.roll(6)})),
        2 => s.execute("photo.flag", &json!({"flag": flag})),
        3 | 4 => s.execute("photo.setMeta", &json!({"addKeywords": [keyword]})),
        5 | 6 => s.execute("photo.setMeta", &json!({"keywords": []})),
        7 if n.is_multiple_of(2) => s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.1 * (n % 9) as f64})),
        7 => s.execute("stack.group", &json!({})),
        8 => s.execute("photo.delete", &json!({})),
        9 => s.execute("album.addPhotos", &json!({"id": album, "ids": chosen})),
        10 => s.execute("album.removePhotos", &json!({"id": album, "ids": chosen})),
        _ => s.undo_step().map(|_| Value::Null),
    };
}

#[test]
fn following_an_edit_gives_the_list_worked_out_from_every_photo() {
    let (mut s, mut ids, album, smart) = library(240);
    let mut dice = Dice(7);
    let sources = [
        json!({"kind": "all"}),
        json!({"kind": "picks"}),
        json!({"kind": "album", "id": smart}),
        json!({"kind": "album", "id": album}),
        json!({"kind": "libraryFolder", "path": "/pics/1"}),
        json!({"kind": "recentlyDeleted"}),
    ];
    let sorts = [
        json!({"key": "random", "seed": 11}),
        json!({"key": "captureDate", "ascending": false}),
        json!({"key": "rating", "ascending": true}),
        json!({"key": "fileName", "ascending": true}),
        json!({"key": "editDate", "ascending": false}),
        json!({"key": "importDate", "ascending": true}),
    ];
    let filters = [json!({"rating": 0, "text": ""}), json!({"rating": 3, "text": ""}), json!({"rating": 0, "text": "travel"})];
    let mut checked = 0;
    for (i, source) in sources.iter().enumerate() {
        for (j, sort) in sorts.iter().enumerate() {
            let filter = &filters[(i + j) % filters.len()];
            s.execute("library.source", source).unwrap();
            s.execute("library.sort", sort).unwrap();
            s.execute("library.filter", filter).unwrap();
            let _ = s.visible_cloned();
            // (every other view holds its photos until refreshed: what stays then has to be what asking every photo
            // would keep)
            s.execute("library.holdView", &json!({"on": (i + j) % 2 == 1})).unwrap();
            let _ = s.visible_cloned();
            for step in 0..40 {
                edit(&mut s, &mut dice, &mut ids, album);
                let followed = s.visible_cloned();
                let whole = s.visible_in_full();
                assert_eq!(followed, whole, "source {source}, sort {sort}, filter {filter}, hold {}, edit {step}", s.hold_view);
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 1440);
    // and it was following that was checked: most edits touch a photo or two, and the view kept up with those
    // without asking every photo again
    assert!(s.view_followed > 400, "followed {} times, worked out {} times", s.view_followed, s.view_worked_out);
}

#[test]
fn a_view_that_asks_about_the_last_days_follows_the_clock_too() {
    let (mut s, ids, _, _) = library(60);
    // Given a smart album of the photos taken in the last 30 days, a day after the latest of them
    let now = std::sync::Arc::new(std::sync::Mutex::new("2026-01-01T12:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "captureDate", "op": "inLast", "value": 30}]}});
    let recent = s.execute("album.createSmart", &json!({"name": "Recent", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": recent})).unwrap();
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    let shown = s.visible_cloned();
    assert!(!shown.is_empty() && shown.len() < ids.len(), "{} of {}", shown.len(), ids.len());
    // When a photo is rated two months later
    *now.lock().unwrap() = "2026-03-05T12:00:00".to_string();
    let followed = s.view_followed;
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    // Then the photos nobody touched have left with the days: the view was asked whole, not followed
    assert!(s.visible_cloned().is_empty(), "{:?}", s.visible_cloned());
    assert_eq!(s.visible_cloned(), s.visible_in_full());
    assert_eq!(s.view_followed, followed);
    // And the same through a view that only looks into that album
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "album", "op": "is", "value": recent}]}});
    if let Ok(r) = s.execute("album.createSmart", &json!({"name": "In Recent", "rules": rules})) {
        s.execute("library.source", &json!({"kind": "album", "id": r["id"]})).unwrap();
        let _ = s.visible_cloned();
        s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
        s.execute("photo.rate", &json!({"rating": 3})).unwrap();
        assert_eq!(s.visible_cloned(), s.visible_in_full());
        assert_eq!(s.view_followed, followed, "a view that looks into a clocked album is asked whole too");
    }
}

#[test]
fn an_edit_to_many_photos_or_to_more_than_photos_is_worked_out_whole() {
    let (mut s, ids, album, _) = library(600);
    s.execute("library.sort", &json!({"key": "random", "seed": 3})).unwrap();
    let _ = s.visible_cloned();
    let (followed, whole) = (s.view_followed, s.view_worked_out);
    // one photo rated: followed
    s.execute("library.select", &json!({"ids": [ids[5].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    let _ = s.visible_cloned();
    assert_eq!((s.view_followed, s.view_worked_out), (followed + 1, whole));
    // five hundred photos rated at once: worked out from every photo
    s.execute("library.select", &json!({"ids": ids.iter().take(500).map(|id| id.0).collect::<Vec<_>>()})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let _ = s.visible_cloned();
    assert_eq!((s.view_followed, s.view_worked_out), (followed + 1, whole + 1));
    // an album's photos changed: what that does to which view is not asked photo by photo
    s.execute("album.addPhotos", &json!({"id": album, "ids": [ids[1].0]})).unwrap();
    let _ = s.visible_cloned();
    assert_eq!((s.view_followed, s.view_worked_out), (followed + 1, whole + 2));
    // nothing changed: neither
    let _ = s.visible_cloned();
    assert_eq!((s.view_followed, s.view_worked_out), (followed + 1, whole + 2));
}

#[test]
fn the_albums_counts_followed_through_edits_are_the_counts_made_from_every_photo() {
    use lightcraft_catalog::tally::AlbumTally;
    let (mut s, mut ids, album, smart) = library(240);
    // more smart albums: picks, well rated or keyworded ones, one that looks into an album of chosen photos
    for rules in [
        json!({"ruleSet": {"match": "all", "rules": [{"field": "flag", "op": "is", "value": "pick"}]}}),
        json!({"ruleSet": {"match": "any", "rules": [{"field": "rating", "op": "gte", "value": 4}, {"field": "keywords", "op": "contains", "value": "pets"}]}}),
        json!({"album": album, "rating": 3}),
        // what the edits below change besides: a label, a slider, when a photo was taken, what its file is called
        json!({"label": "red"}),
        json!({"edited": true}),
        json!({"dateFrom": "2025-06-01"}),
        json!({"text": "REN"}),
    ] {
        let made = s.execute("album.createSmart", &json!({"name": "More", "rules": rules}));
        assert!(made.is_ok(), "{rules}: {made:?}");
    }
    assert!(s.catalog.album(lightcraft_catalog::AlbumId(smart)).is_some_and(|a| a.is_smart()));
    let fresh = |s: &Session| -> Vec<(u64, usize)> {
        let mut v: Vec<(u64, usize)> = s.catalog.albums().map(|a| (a.id.0, s.catalog.album_count(a.id))).collect();
        v.sort_unstable();
        v
    };
    let shown = |tally: &AlbumTally| -> Vec<(u64, usize)> {
        let mut v: Vec<(u64, usize)> = tally.counts().iter().map(|(id, n)| (id.0, *n)).collect();
        v.sort_unstable();
        v
    };
    let mut tally = AlbumTally::of(&s.catalog);
    assert_eq!(shown(&tally), fresh(&s));
    let mut dice = Dice(23);
    let (mut followed, mut whole) = (0, 0);
    for step in 0..900 {
        edit(&mut s, &mut dice, &mut ids, album);
        // (not every frame draws the albums: sometimes several edits pass between two looks)
        if dice.roll(4) == 0 {
            continue;
        }
        let moved = tally.revision() != s.catalog.revision;
        match (moved, tally.update(&s.catalog)) {
            (true, true) => followed += 1,
            (true, false) => whole += 1,
            // (an edit that was refused changed nothing: nothing to follow)
            (false, _) => {}
        }
        assert_eq!(shown(&tally), fresh(&s), "after edit {step}");
    }
    assert!(followed > 300 && whole > 20, "followed {followed} times, counted whole {whole} times");
    // a photo deleted: the album of chosen photos it is in counts one less, without every photo being asked
    let in_album = s
        .catalog
        .album(lightcraft_catalog::AlbumId(album))
        .and_then(|a| a.photos.iter().copied().find(|id| s.catalog.photo(*id).is_some_and(|p| !p.deleted)));
    if let Some(id) = in_album {
        let before = tally.counts()[&lightcraft_catalog::AlbumId(album)];
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("photo.delete", &json!({})).unwrap();
        assert!(tally.update(&s.catalog), "followed");
        assert_eq!(tally.counts()[&lightcraft_catalog::AlbumId(album)], before - 1);
    }
    // three hundred photos rated at once: counted from every photo, and right
    let many: Vec<u64> = s.visible_cloned().iter().take(300).map(|id| id.0).collect();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    s.execute("library.filter", &json!({"rating": 0, "text": ""})).unwrap();
    let many: Vec<u64> = if many.len() >= 300 { many } else { s.visible_cloned().iter().take(300).map(|id| id.0).collect() };
    if many.len() > 256 {
        s.execute("library.select", &json!({"ids": many})).unwrap();
        s.execute("photo.rate", &json!({"rating": 5})).unwrap();
        assert!(!tally.update(&s.catalog), "more photos than are followed one by one");
        assert_eq!(shown(&tally), fresh(&s));
    }
}
