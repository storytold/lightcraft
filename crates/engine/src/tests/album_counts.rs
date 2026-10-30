//! `albums.list` and the smart albums' counts: a window offers the counts it keeps and the command counts nothing
//! (listing them pending when they aren't of the catalog as it is now); a session with no window counts every photo
//! once and follows edits to a few.

use std::sync::{Arc, Mutex};

use lightcraft_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::{CountsAge, Session};

fn photo(id: u64, captured: &str, rating: u8) -> Op {
    let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("p{id}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
    p.captured = Some(captured.into());
    p.rating = rating;
    Op::AddPhoto { photo: Box::new(p) }
}

fn library(ratings: &[u8]) -> Session {
    let mut s = Session::new();
    for (i, r) in ratings.iter().enumerate() {
        s.catalog.apply(photo(i as u64 + 1, "2026-09-30T11:59:30", *r)).unwrap();
    }
    s
}

fn listed(s: &mut Session, id: u64) -> Value {
    let list = s.execute("albums.list", &json!({})).unwrap();
    list.as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap()
}

fn smart(s: &mut Session, name: &str, rules: Value) -> u64 {
    s.execute("album.createSmart", &json!({"name": name, "rules": rules})).unwrap()["id"].as_u64().unwrap()
}

/// Without a window the command counts every photo once, keeps the counts, and follows an edit to a photo from them.
#[test]
fn without_a_window_the_albums_are_counted_once_and_edits_to_a_photo_are_followed() {
    let mut s = library(&[5, 1, 1, 4]);
    let good = smart(&mut s, "Good", json!({"rating": 4}));
    let picked = s.execute("album.create", &json!({"name": "Picked", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.select", &json!({"ids": [1, 2]})).unwrap();
    s.execute("album.addPhotos", &json!({"id": picked, "ids": [1, 2]})).unwrap();
    assert_eq!(s.album_counts.scans, 0);

    let a = listed(&mut s, good);
    assert_eq!((a["count"].as_u64(), a["countPending"].as_bool()), (Some(2), Some(false)), "{a}");
    assert_eq!(listed(&mut s, picked)["count"], 2);
    assert_eq!(s.album_counts.scans, 1);
    listed(&mut s, good);
    assert_eq!(s.album_counts.scans, 1, "listed again: nothing changed, nothing counted");

    s.execute("library.select", &json!({"ids": [2]})).unwrap();
    s.execute("photo.rate", &json!({"ids": [2], "rating": 5})).unwrap();
    assert_eq!(listed(&mut s, good)["count"], 3, "the edit is in the count");
    assert_eq!(s.album_counts.scans, 1, "followed from the photo it touched");

    s.execute("album.setRules", &json!({"id": good, "rules": {"rating": 5}})).unwrap();
    let a = listed(&mut s, good);
    assert_eq!((a["count"].as_u64(), a["countPending"].as_bool()), (Some(2), Some(false)), "{a}");
    assert_eq!(s.album_counts.scans, 2, "new rules ask every photo again");
    assert_eq!(a["count"].as_u64().unwrap(), s.catalog.album_count(AlbumId(good)) as u64);
}

/// The counts a window offers are used as they are, once, with nothing counted here; a smart album is pending when the
/// window says they aren't of the catalog as it is now, or (for an album asking about "the last N days") of this minute.
#[test]
fn counts_a_window_offers_are_listed_with_nothing_counted_here() {
    let mut s = library(&[5, 1, 1, 4]);
    s.clock = Box::new(|| "2026-09-30T12:00:00".to_string());
    let good = smart(&mut s, "Good", json!({"rating": 4}));
    let recent_rules = json!({"ruleSet": {"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}});
    let recent = smart(&mut s, "Recent", recent_rules);
    let offered = Arc::new(std::collections::HashMap::from([(AlbumId(good), 7), (AlbumId(recent), 8)]));
    let counts_and_flags = |s: &mut Session| -> Vec<(u64, bool)> {
        let list = s.execute("albums.list", &json!({})).unwrap();
        list.as_array().unwrap().iter().map(|a| (a["count"].as_u64().unwrap(), a["countPending"].as_bool().unwrap())).collect()
    };

    s.offer_album_counts(offered.clone(), CountsAge::Behind);
    assert_eq!(counts_and_flags(&mut s), [(7, true), (8, true)]);
    assert_eq!(s.album_counts.scans, 0, "nothing is counted for an offer");

    s.offer_album_counts(offered.clone(), CountsAge::Current);
    assert_eq!(counts_and_flags(&mut s), [(7, false), (8, false)]);
    assert_eq!(s.album_counts.scans, 0);

    // a count of an earlier minute is behind only for the album whose rules ask about the clock
    s.offer_album_counts(offered, CountsAge::ClockBehind);
    assert_eq!(counts_and_flags(&mut s), [(7, false), (8, true)]);
    assert_eq!(s.album_counts.scans, 0);

    // used once: the next answer is the session's own
    assert_eq!(counts_and_flags(&mut s), [(2, false), (4, false)]);
    assert_eq!(s.album_counts.scans, 1);

    // and an offer not used is taken back
    s.offer_album_counts(Arc::new(Default::default()), CountsAge::Behind);
    s.withdraw_album_counts();
    assert_eq!(counts_and_flags(&mut s), [(2, false), (4, false)]);
}

/// "In the last hour" counts back from the session clock: the counts kept are of its minute, so an hour later they
/// are counted again though the catalog didn't change.
#[test]
fn counts_of_albums_asking_about_the_last_hour_follow_the_session_clock() {
    let mut s = library(&[1]);
    let now = Arc::new(Mutex::new("2026-09-30T12:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    let rules = json!({"ruleSet": {"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}});
    let recent = smart(&mut s, "Recent", rules);
    assert_eq!(listed(&mut s, recent)["count"], 1);
    assert_eq!(listed(&mut s, recent)["count"], 1);
    assert_eq!(s.album_counts.scans, 1, "the same minute");
    *now.lock().unwrap() = "2026-09-30T13:00:10".into();
    assert_eq!(listed(&mut s, recent)["count"], 0, "an hour later the photo is out of it");
    assert_eq!(s.album_counts.scans, 2);
    lightcraft_catalog::rules::set_now(None);
}
