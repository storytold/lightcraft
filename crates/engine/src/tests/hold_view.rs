//! A view holds the photos it shows until it is refreshed (`library.holdView`): an edit that makes a photo stop
//! matching a smart album's rules, Picks or the filter bar leaves it where it is, and the others where they are.

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{LibrarySource, Session};

fn relative_clock_session(captures: &[&str]) -> (Session, std::sync::Arc<std::sync::Mutex<String>>, Vec<PhotoId>) {
    let mut s = Session::new();
    let now = std::sync::Arc::new(std::sync::Mutex::new("2026-10-24T12:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    let mut ids = Vec::new();
    for (index, captured) in captures.iter().enumerate() {
        let id = PhotoId(index as u64 + 1);
        let name = format!("synthetic-{index}.jpg");
        let mut photo = Photo::new(id, Source::Demo { scene: 0 }, &name, "JPEG", 2, 2, "2026-10-01T00:00:00");
        photo.captured = Some((*captured).to_string());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        ids.push(id);
    }
    (s, now, ids)
}

/// The demo library with no photo keyworded, showing a smart album of the photos without keywords, at random.
fn unkeyworded() -> Session {
    let mut s = Session::with_demo();
    let all: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    s.execute("photo.setMeta", &json!({"ids": all, "keywords": []})).unwrap();
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "keywordCount", "op": "is", "value": 0}]}});
    let album = s.execute("album.createSmart", &json!({"name": "Unkeyworded", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.execute("library.sort", &json!({"key": "random"})).unwrap();
    s
}

fn hold(s: &mut Session) {
    assert_eq!(s.execute("library.holdView", &json!({"on": true})).unwrap()["hold"], true);
}

fn keyword(s: &mut Session, id: PhotoId) {
    s.execute("photo.setMeta", &json!({"ids": [id.0], "addKeywords": ["travel"]})).unwrap();
}

#[test]
fn a_keyword_given_in_a_held_view_leaves_the_photo_and_the_order_where_they_are() {
    // Given a smart album of the photos without keywords, at random, held
    let mut s = unkeyworded();
    hold(&mut s);
    let (before, seed) = (s.visible_cloned(), s.sort.seed);
    assert!(before.len() > 10);
    // When photos in it are given a keyword, one after the other
    for at in [3, 7, 0] {
        keyword(&mut s, before[at]);
        // Then nothing moves: the same photos in the same places, under the same shuffle
        assert_eq!(s.visible_cloned(), before, "after a keyword on the photo at {at}");
        assert_eq!(s.sort.seed, seed);
    }
    // When the view is refreshed, Then they are gone and the others keep their order
    s.execute("library.refresh", &json!({})).unwrap();
    let after = s.visible_cloned();
    let expected: Vec<PhotoId> = before.iter().copied().filter(|id| ![before[3], before[7], before[0]].contains(id)).collect();
    assert_eq!(after, expected);
    assert_eq!(s.sort.seed, seed, "a refresh is not a reshuffle");
}

#[test]
fn a_view_that_is_not_held_follows_every_edit_as_before() {
    let mut s = unkeyworded();
    assert!(!s.hold_view, "off unless asked for");
    let before = s.visible_cloned();
    keyword(&mut s, before[3]);
    let after = s.visible_cloned();
    assert!(!after.contains(&before[3]), "the photo leaves at once");
    assert_eq!(after, before.iter().copied().filter(|id| *id != before[3]).collect::<Vec<_>>(), "the others keep their order");
}

#[test]
fn a_held_view_still_takes_in_what_starts_matching_and_lets_go_of_what_is_deleted() {
    let mut s = unkeyworded();
    let all = s.visible_cloned();
    // one photo has a keyword before the view is held: it is not in it
    keyword(&mut s, all[5]);
    hold(&mut s);
    let before = s.visible_cloned();
    assert!(!before.contains(&all[5]));
    // its keyword taken off again: it matches, and comes in at its place in the shuffle
    s.execute("photo.setMeta", &json!({"ids": [all[5].0], "keywords": []})).unwrap();
    assert_eq!(s.visible_cloned(), all, "back where the shuffle puts it");
    // a photo that was kept after it stopped matching, then deleted, is not kept in the view
    keyword(&mut s, all[2]);
    assert!(s.visible_cloned().contains(&all[2]));
    s.execute("library.select", &json!({"ids": [all[2].0]})).unwrap();
    s.execute("photo.delete", &json!({})).unwrap();
    assert!(!s.visible_cloned().contains(&all[2]), "Recently Deleted is somewhere else");
}

#[test]
fn another_source_filter_sort_or_shuffle_is_another_view() {
    let mut s = unkeyworded();
    hold(&mut s);
    let before = s.visible_cloned();
    // Reshuffle: a new order of what matches now
    keyword(&mut s, before[1]);
    assert!(s.visible_cloned().contains(&before[1]));
    s.execute("library.shuffle", &json!({})).unwrap();
    assert!(!s.visible_cloned().contains(&before[1]), "a reshuffle refreshes");
    // another sort
    let now = s.visible_cloned();
    keyword(&mut s, now[1]);
    s.execute("library.sort", &json!({"key": "fileName"})).unwrap();
    assert!(!s.visible_cloned().contains(&now[1]), "another sort refreshes");
    // the filter bar
    let now = s.visible_cloned();
    keyword(&mut s, now[1]);
    s.execute("library.filter", &json!({"rating": 0, "text": "a"})).unwrap();
    assert!(!s.visible_cloned().contains(&now[1]), "another filter refreshes");
    s.execute("library.filter", &json!({"text": ""})).unwrap();
    // away and back
    let now = s.visible_cloned();
    let source = s.source;
    keyword(&mut s, now[1]);
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    let _ = s.visible_cloned();
    s.source = source;
    assert!(!s.visible_cloned().contains(&now[1]), "coming back to it refreshes");
}

#[test]
fn picks_and_the_filter_bar_hold_too_and_an_album_lets_go_of_what_is_taken_out_of_it() {
    let mut s = Session::with_demo();
    let all = s.visible_cloned();
    // Picks
    s.execute("library.select", &json!({"ids": [all[0].0, all[1].0, all[2].0]})).unwrap();
    s.execute("photo.flag", &json!({"flag": "pick"})).unwrap();
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    hold(&mut s);
    let picks = s.visible_cloned();
    assert!(picks.len() >= 3, "{picks:?}");
    s.execute("library.select", &json!({"ids": [picks[1].0]})).unwrap();
    s.execute("photo.flag", &json!({"flag": "none"})).unwrap();
    assert_eq!(s.visible_cloned(), picks, "a pick unflagged stays until the view is refreshed");
    s.execute("library.refresh", &json!({})).unwrap();
    assert_eq!(s.visible_cloned(), picks.iter().copied().filter(|id| *id != picks[1]).collect::<Vec<_>>());
    // the filter bar over an album: a rating under the filter's stays, a photo taken out of the album goes
    let album = s.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    let ids: Vec<u64> = all.iter().take(5).map(|p| p.0).collect();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    s.execute("album.addPhotos", &json!({"id": album, "ids": ids})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.execute("library.filter", &json!({"rating": 3})).unwrap();
    let shown = s.visible_cloned();
    assert_eq!(shown.len(), 5);
    s.execute("library.select", &json!({"ids": [shown[0].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    assert_eq!(s.visible_cloned().len(), 5, "rated under the filter: held");
    s.execute("album.removePhotos", &json!({"id": album, "ids": [shown[0].0, shown[1].0]})).unwrap();
    let left = s.visible_cloned();
    assert!(!left.contains(&shown[0]) && !left.contains(&shown[1]), "out of the album is out of the view: {left:?}");
    assert_eq!(left.len(), 3);
    assert_eq!(s.source, LibrarySource::Album(lightcraft_catalog::AlbumId(album)));
}

#[test]
fn turning_the_hold_off_lets_go_at_once() {
    let mut s = unkeyworded();
    hold(&mut s);
    let before = s.visible_cloned();
    keyword(&mut s, before[4]);
    assert!(s.visible_cloned().contains(&before[4]));
    assert_eq!(s.execute("library.holdView", &json!({})).unwrap()["hold"], false, "toggles without `on`");
    assert!(!s.visible_cloned().contains(&before[4]));
}

#[test]
fn changing_the_rules_of_the_smart_album_shown_is_another_view() {
    let mut s = unkeyworded();
    hold(&mut s);
    let before = s.visible_cloned();
    let LibrarySource::Album(album) = s.source else { panic!("a smart album is shown") };
    // the album now asks for photos with a keyword: none has one, and none is kept from what it asked before
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "keywordCount", "op": "gt", "value": 0}]}});
    s.execute("album.setRules", &json!({"id": album.0, "rules": rules})).unwrap();
    assert!(s.visible_cloned().is_empty(), "the old rules' photos are not held under the new ones");
    // and undoing the change brings the first question back, with what matches it
    s.undo_step().unwrap();
    assert_eq!(s.visible_cloned(), before);
}

#[test]
fn a_source_with_no_filter_over_it_holds_nothing() {
    // All Photos, an album or a folder with no filter: nothing an edit does can make a photo stop matching, so
    // nothing is kept (and nothing is paid for the hold)
    let mut s = Session::with_demo();
    hold(&mut s);
    let all = s.visible_cloned();
    keyword(&mut s, all[0]);
    assert_eq!(s.visible_cloned(), all);
    // a photo deleted from All Photos leaves it, held or not
    s.execute("library.select", &json!({"ids": [all[0].0]})).unwrap();
    s.execute("photo.delete", &json!({})).unwrap();
    assert!(!s.visible_cloned().contains(&all[0]));
}

#[test]
fn asking_for_the_state_it_has_changes_nothing_and_a_word_is_not_a_bool() {
    let mut s = unkeyworded();
    hold(&mut s);
    let before = s.visible_cloned();
    keyword(&mut s, before[2]);
    hold(&mut s);
    assert!(s.visible_cloned().contains(&before[2]), "on, asked for again: the held photos stay");
    assert!(s.execute("library.holdView", &json!({"on": "false"})).is_err());
    assert!(s.hold_view, "a refused call changes nothing");
}

#[test]
fn relative_clock_held_view_keeps_expired_photos_admits_new_matches_and_refreshes() {
    let (mut s, now, ids) = relative_clock_session(&["2026-10-23T12:00:30", "2026-10-22T12:00:30"]);
    let rules = json!({
        "ruleSet": {
            "match": "any",
            "rules": [
                {"field": "captureDate", "op": "inLast", "value": 1},
                {"field": "captureDate", "op": "notInLast", "value": 2}
            ]
        }
    });
    s.execute("library.filter", &rules).unwrap();
    s.execute("library.holdView", &json!({"on": true})).unwrap();
    assert_eq!(s.visible_cloned(), vec![ids[0]], "only the soon-to-expire photo matches initially");

    *now.lock().unwrap() = "2026-10-24T12:01:00".to_string();
    let held = s.visible_cloned();
    assert_eq!(held.len(), 2, "the expired photo stays held while the newly matching photo enters: {held:?}");
    assert!(held.contains(&ids[0]), "the expired photo remains held");
    assert!(held.contains(&ids[1]), "the photo crossing the second relative boundary enters");

    s.execute("library.refresh", &json!({})).unwrap();
    assert_eq!(s.visible_cloned(), vec![ids[1]], "explicit refresh releases the expired photo");
}
