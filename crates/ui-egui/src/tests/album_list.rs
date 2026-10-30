//! `albums.list` answers at once from the counts the sidebar keeps: it never counts a smart album's photos on the UI
//! thread (in a large library that is seconds in which the window doesn't answer). A count that isn't of the catalog
//! as it is now is listed as pending (`countPending`), and the ones the sidebar follows through small edits are current
//! at once.

use std::time::{Duration, Instant};

use lightcraft_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::control::{ControlRequest, Outcome};
use crate::{LightcraftApp, Services};

/// A library of `photos` photos rated 0 to 5 in turn, and no frame drawn: nothing has counted its albums yet.
fn app(photos: u64) -> LightcraftApp {
    let mut session = lightcraft_engine::Session::new();
    for i in 1..=photos {
        let mut p = Photo::new(PhotoId(i), Source::Demo { scene: 0 }, &format!("p{i}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.rating = (i % 6) as u8;
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    LightcraftApp::new(session, Services { png: None, ..Default::default() })
}

/// `albums.list` as the control channel runs it.
fn list(app: &mut LightcraftApp, ctx: &egui::Context) -> Vec<Value> {
    let (req, _) = ControlRequest::new("engine.execute", json!({"command": "albums.list"}));
    let Outcome::Done(reply) = crate::control::handle(app, ctx, &req) else { panic!("expected a reply") };
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"].as_array().unwrap().clone()
}

fn run(app: &mut LightcraftApp, command: &str, params: Value) -> Value {
    app.run(command, params).unwrap_or_else(|e| panic!("{command}: {e}"))
}

fn album(listed: &[Value], id: u64) -> &Value {
    listed.iter().find(|a| a["id"] == id).unwrap_or_else(|| panic!("no album {id} in {listed:?}"))
}

/// Ask again until the smart album `id` is no longer pending (its count of every photo is done and taken).
fn settled(app: &mut LightcraftApp, ctx: &egui::Context, id: u64) -> Vec<Value> {
    let t0 = Instant::now();
    loop {
        let listed = list(app, ctx);
        if album(&listed, id)["countPending"] == false {
            return listed;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "the count of every photo ends");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn smart(app: &mut LightcraftApp, name: &str, rules: Value) -> u64 {
    run(app, "album.createSmart", json!({"name": name, "rules": rules}))["id"].as_u64().unwrap()
}

fn count_of(app: &LightcraftApp, id: u64) -> u64 {
    app.session.catalog.album_count(AlbumId(id)) as u64
}

/// The first answer lists every album without counting the smart albums' photos (they are pending, and counted on a
/// worker); an album of chosen photos is exact at once; the next answers are current, and no photo is counted again.
#[test]
fn the_albums_are_listed_at_once_with_the_smart_albums_counts_pending_until_counted() {
    let mut app = app(3000);
    let ctx = egui::Context::default();
    let four = smart(&mut app, "Four stars or more", json!({"rating": 4}));
    let five = smart(&mut app, "Five stars", json!({"rating": 5}));
    let picked = run(&mut app, "album.create", json!({"name": "Picked", "addSelected": false}))["id"].as_u64().unwrap();
    run(&mut app, "library.select", json!({"ids": [1, 2, 3]}));
    run(&mut app, "album.addPhotos", json!({"id": picked, "ids": [1, 2, 3]}));
    assert_eq!(app.caches.album_count_scans, 0, "nothing counted yet");

    let first = list(&mut app, &ctx);
    for id in [four, five] {
        assert_eq!(album(&first, id)["countPending"], true, "a smart album's count isn't known at the first answer: {first:?}");
        assert_eq!(album(&first, id)["count"], 0, "no count of an earlier catalog to show");
    }
    assert_eq!(album(&first, picked)["count"], 3, "an album of chosen photos is counted from its list");
    assert!(album(&first, picked).get("countPending").is_none(), "it has nothing to wait for");
    assert_eq!(app.caches.album_count_scans, 1, "every photo is being counted, once, elsewhere");

    let settled = settled(&mut app, &ctx, four);
    assert_eq!(album(&settled, four)["count"], count_of(&app, four));
    assert_eq!(album(&settled, five)["count"], count_of(&app, five));
    assert_eq!(album(&settled, five)["countPending"], false);
    assert_eq!(app.caches.album_count_scans, 1, "the answers since counted nothing");
}

/// An edit to a photo is followed from the counts kept, so the answer is current at once; new rules count every photo
/// again on the worker, and until that is done the answer lists the count it had, pending.
#[test]
fn an_edit_to_a_photo_is_followed_and_new_rules_leave_the_old_count_pending() {
    let mut app = app(3000);
    let ctx = egui::Context::default();
    let four = smart(&mut app, "Four stars or more", json!({"rating": 4}));
    list(&mut app, &ctx);
    let before = album(&settled(&mut app, &ctx, four), four)["count"].as_u64().unwrap();
    assert_eq!(before, count_of(&app, four));

    // a photo rated 5: one more
    run(&mut app, "library.select", json!({"ids": [2]}));
    run(&mut app, "photo.rate", json!({"ids": [2], "rating": 5}));
    let after = list(&mut app, &ctx);
    assert_eq!(album(&after, four)["countPending"], false, "a small edit is followed at once");
    assert_eq!(album(&after, four)["count"].as_u64().unwrap(), before + 1);
    assert_eq!(app.caches.album_count_scans, 1, "without counting every photo again");

    // new rules: the count shown stays the one before until the new one is made
    run(&mut app, "album.setRules", json!({"id": four, "rules": {"rating": 5}}));
    let changed = list(&mut app, &ctx);
    assert_eq!(album(&changed, four)["countPending"], true, "{changed:?}");
    assert_eq!(album(&changed, four)["count"].as_u64().unwrap(), before + 1, "the count of the rules before");
    let done = settled(&mut app, &ctx, four);
    assert_eq!(album(&done, four)["count"], count_of(&app, four));
    assert_eq!(app.caches.album_count_scans, 2, "every photo was counted again, for the new rules only");
}

/// A new minute puts only the albums whose rules ask about "the last N days" behind (the catalog didn't change, so the
/// other counts are exact), and they are counted again on the worker like any other count behind.
#[test]
fn a_new_minute_leaves_only_the_albums_asking_about_the_last_hour_pending() {
    let mut app = app(300);
    let ctx = egui::Context::default();
    let clock_at = |app: &mut LightcraftApp, now: &'static str| app.session.clock = Box::new(move || now.to_string());
    clock_at(&mut app, "2026-09-30T12:00:00");
    let four = smart(&mut app, "Four stars or more", json!({"rating": 4}));
    let rules = json!({"ruleSet": {"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}});
    let recent = smart(&mut app, "Recent", rules);
    list(&mut app, &ctx);
    let first = settled(&mut app, &ctx, recent);
    assert_eq!((album(&first, four)["countPending"].as_bool(), album(&first, recent)["countPending"].as_bool()), (Some(false), Some(false)));
    assert_eq!(app.caches.album_count_scans, 1);

    clock_at(&mut app, "2026-09-30T12:01:10");
    let later = list(&mut app, &ctx);
    assert_eq!(album(&later, four)["countPending"], false, "no rule of it asks about the clock: {later:?}");
    assert_eq!(album(&later, recent)["countPending"], true, "its count is of the minute before: {later:?}");
    let done = settled(&mut app, &ctx, recent);
    assert_eq!(album(&done, recent)["count"], count_of(&app, recent));
    assert_eq!(app.caches.album_count_scans, 2, "every photo was counted again for the new minute");
    lightcraft_catalog::rules::set_now(None);
}

/// In a large library with many smart albums the answer comes in a fraction of the time it takes to count the albums
/// once (what it did, on the UI thread, before).
#[test]
fn listing_the_albums_of_a_large_library_takes_a_fraction_of_counting_them() {
    let mut app = app(60_000);
    let ctx = egui::Context::default();
    for i in 0..12 {
        smart(&mut app, &format!("Rated {i}"), json!({"rating": i % 5 + 1}));
    }
    let counted = Instant::now();
    let tally = lightcraft_catalog::tally::AlbumTally::of(&app.session.catalog);
    let counting = counted.elapsed();
    assert_eq!(tally.counts().len(), 12);

    let listed = Instant::now();
    let first = list(&mut app, &ctx);
    let listing = listed.elapsed();
    assert!(first.iter().all(|a| a["countPending"] == true), "{first:?}");
    assert!(listing < counting / 2, "listing took {listing:?}, counting every album once takes {counting:?}");
}
