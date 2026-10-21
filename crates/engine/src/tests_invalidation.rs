//! What a develop edit invalidates: a slider tick changes one photo's develop settings, so a view that reads no
//! develop state keeps its list (same generation, nothing derived from it worked out again), and the views that do
//! read it follow the edit (see `lightcraft_catalog::reads`).

use lightcraft_catalog::{Album, AlbumId, Filter, Op, Photo, PhotoId, RuleSet, Sort, SortKey, Source};
use serde_json::json;

use crate::{LibrarySource, Session};

/// A library of `n` photos with capture dates and a keyword each.
fn library(n: u64) -> Session {
    let mut s = Session::new();
    let ops = (0..n)
        .map(|i| {
            let mut p = Photo::new(
                PhotoId(i + 1),
                Source::Demo { scene: (i % 20) as u32 },
                &format!("IMG_{i:06}.jpg"),
                "JPEG",
                6000,
                4000,
                "2026-01-01T00:00:00",
            );
            p.captured = Some(format!("20{:02}-{:02}-{:02}T10:00:00", 10 + i % 16, 1 + i % 12, 1 + i % 28));
            p.meta.keywords = vec![format!("kw{}", i % 300)];
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    s.commit("Add", Op::Batch { ops }).unwrap();
    s
}

/// Change photo `id`'s exposure (a committed edit, or a slider tick inside an interaction).
fn expose(s: &mut Session, id: PhotoId, ev: f64) {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.light.exposure = ev;
    s.set_develop(id, d, "Exposure").unwrap();
}

fn crop(s: &mut Session, id: PhotoId, rect: lightcraft_geom::Rect) {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.crop.geometry.rect = rect;
    s.set_develop(id, d, "Crop").unwrap();
}

fn smart(s: &mut Session, id: u64, rules: serde_json::Value) {
    let mut a = Album::new(AlbumId(id), format!("Smart {id}"));
    let rules: RuleSet = serde_json::from_value(rules).unwrap();
    a.smart = Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() }));
    s.commit("New Smart Album", Op::AddAlbum { album: a }).unwrap();
}

/// Slider ticks and committed develop edits leave the grid's list as it is (same generation, the very same list)
/// when nothing in view reads develop state; a library change still redoes it.
#[test]
fn develop_edits_keep_the_view() {
    let mut s = library(50);
    let (generation, list) = s.visible_shared();
    let total = s.source_total();
    let id = s.visible()[7];
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    for i in 0..10 {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 10.0})).unwrap();
        assert_eq!(s.visible_shared().0, generation, "tick {i}");
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    expose(&mut s, id, 2.0);
    s.execute("edit.undo", &json!({})).unwrap();
    let (kept, same) = s.visible_shared();
    assert_eq!(kept, generation, "a committed edit and its undo");
    assert!(std::sync::Arc::ptr_eq(&list, &same));
    assert_eq!(s.source_total(), total);
    // (ten ticks, then the edit and its undo before the view was asked again)
    assert_eq!(s.view_kept, 11);
    // a library change does
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    assert_ne!(s.visible_shared().0, generation);
}

/// Filters, sorts and smart albums that read develop state follow every develop edit, slider ticks included.
#[test]
fn views_that_read_develop_state_follow_develop_edits() {
    let mut s = library(20);
    let ids = s.visible().to_vec();
    let (a, b) = (ids[3], ids[11]);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();

    // the filter bar's Edited, during a drag
    s.filter = Filter { edited: Some(true), ..Default::default() };
    assert!(s.visible().is_empty());
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert_eq!(s.visible(), [a], "a slider tick makes the photo edited");
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.0})).unwrap();
    assert!(s.visible().is_empty(), "and back");
    s.execute("develop.endInteraction", &json!({})).unwrap();
    // a search token
    s.filter = Filter { text: "edited:yes".into(), ..Default::default() };
    expose(&mut s, b, 1.0);
    assert_eq!(s.visible(), [b]);

    // Edit Date: the photo edited last comes first
    s.filter = Filter::default();
    s.sort = Sort { key: SortKey::EditDate, ascending: false, ..Default::default() };
    s.clock = Box::new(|| "2100-05-01T10:00:00".to_string());
    expose(&mut s, a, 0.3);
    assert_eq!(s.visible()[0], a);
    s.clock = Box::new(|| "2100-05-01T11:00:00".to_string());
    expose(&mut s, b, 0.7);
    assert_eq!(s.visible()[0], b);

    // a smart album testing Cropped, and one testing it through an Album rule
    s.sort = Sort::default();
    smart(&mut s, 100, json!({"rules": [{"field": "cropped", "op": "is", "value": true}]}));
    smart(&mut s, 101, json!({"rules": [{"field": "album", "op": "is", "value": 100}]}));
    for album in [100, 101] {
        s.source = LibrarySource::Album(AlbumId(album));
        assert!(s.visible().is_empty());
        assert_eq!(s.source_total(), Some(0));
    }
    crop(&mut s, a, lightcraft_geom::Rect::new(0.1, 0.1, 0.9, 0.9));
    for album in [100, 101] {
        s.source = LibrarySource::Album(AlbumId(album));
        assert_eq!(s.visible(), [a], "album {album}");
        assert_eq!(s.source_total(), Some(1), "album {album}");
    }
    // and the size rules (a crop changes the shown shape)
    s.source = LibrarySource::All;
    let square: RuleSet = serde_json::from_value(json!({"rules": [{"field": "aspect", "op": "is", "value": "square"}]})).unwrap();
    s.filter = Filter { rule_set: Some(square), ..Default::default() };
    assert!(s.visible().is_empty());
    crop(&mut s, b, lightcraft_geom::Rect::new(0.0, 0.0, 4000.0 / 6000.0, 1.0));
    assert_eq!(s.visible(), [b]);
}

/// Random slider ticks, committed exposures and crops, ratings and undos, in views that read develop state and
/// views that don't: what each shows (and its source's count) is what asking every photo gives, and the views that
/// read no develop state kept their list through the develop edits.
#[test]
fn kept_and_followed_views_are_what_a_full_query_gives() {
    let mut s = library(120);
    smart(&mut s, 100, json!({"rules": [{"field": "edited", "op": "is", "value": true}]}));
    smart(&mut s, 101, json!({"rules": [{"field": "cropped", "op": "is", "value": true}]}));
    smart(&mut s, 102, json!({"rules": [{"field": "album", "op": "is", "value": 101}]}));
    smart(&mut s, 103, json!({"rules": [{"field": "keywords", "op": "contains", "value": "kw1"}]}));
    let ids = s.visible().to_vec();
    let sources = [
        LibrarySource::All,
        LibrarySource::Album(AlbumId(100)),
        LibrarySource::Album(AlbumId(101)),
        LibrarySource::Album(AlbumId(102)),
        LibrarySource::Album(AlbumId(103)),
    ];
    let filters = [
        Filter::default(),
        Filter { edited: Some(false), ..Default::default() },
        Filter { text: "edited:yes".into(), ..Default::default() },
        Filter { keyword: Some("kw2".into()), ..Default::default() },
        Filter { album: Some(AlbumId(101)), ..Default::default() },
    ];
    let sorts = [SortKey::CaptureDate, SortKey::EditDate, SortKey::FileName];
    let mut n = 0u64;
    let mut seconds = 0u64;
    for (i, source) in sources.iter().enumerate() {
        for (j, filter) in filters.iter().enumerate() {
            s.source = *source;
            s.filter = filter.clone();
            s.sort = Sort { key: sorts[(i + j) % sorts.len()], ..Default::default() };
            let _ = s.visible();
            for step in 0..24 {
                n += 1;
                seconds += 1;
                let at = format!("2100-01-01T00:{:02}:{:02}", (seconds / 60) % 60, seconds % 60);
                s.clock = Box::new(move || at.clone());
                let id = ids[(n * 37 % ids.len() as u64) as usize];
                s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
                match n % 8 {
                    0..=3 => {
                        s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
                        s.execute("develop.set", &json!({"control": "light.exposure", "value": (n % 5) as f64 / 4.0})).unwrap();
                        s.execute("develop.endInteraction", &json!({})).unwrap();
                    }
                    4 => expose(&mut s, id, 0.0),
                    5 => crop(&mut s, id, lightcraft_geom::Rect::new(0.0, 0.0, if n.is_multiple_of(3) { 1.0 } else { 0.8 }, 1.0)),
                    6 => s.execute("photo.rate", &json!({"rating": n % 6})).map(|_| ()).unwrap(),
                    _ => {
                        let _ = s.undo_step();
                    }
                }
                let shown = s.visible_cloned();
                let whole = s.visible_in_full();
                assert_eq!(shown, whole, "source {source:?}, filter {filter:?}, sort {:?}, edit {step}", s.sort.key);
                let counted = s.catalog.count(&s.source.to_filter(&Filter::default(), &s.catalog));
                assert_eq!(s.source_total(), Some(counted), "source {source:?}, edit {step}");
            }
        }
    }
    // both were checked: most of these views read develop state and follow each edit; the others kept their list
    // through the develop edits
    let (kept, followed) = (s.view_kept, s.view_followed);
    assert!(kept >= 30 && followed > 300, "kept {kept} times, followed {followed}, worked out {}", s.view_worked_out);
}

/// A view that asks about "the last N days" changes with the clock as well as with edits: a develop edit asks it
/// again, as every edit did before.
#[test]
fn a_view_that_follows_the_clock_is_asked_again_after_a_develop_edit() {
    let mut s = library(10);
    smart(&mut s, 100, json!({"rules": [{"field": "captureDate", "op": "inLast", "value": 30}]}));
    s.source = LibrarySource::Album(AlbumId(100));
    let (generation, _) = s.visible_shared();
    let id = s.catalog.photos().next().unwrap().id;
    expose(&mut s, id, 1.0);
    assert_ne!(s.visible_shared().0, generation);
}

/// Per-tick cost of a slider drag on a big library (the develop op plus the grid query the filmstrip asks for every
/// frame): `cargo test --release -p lightcraft-engine -- --ignored drag_tick_100k --nocapture`.
#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn drag_tick_100k() {
    let mut s = library(100_000);
    let id = s.visible()[50_000];
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    let n = 60;
    let tick = |s: &mut Session, name: &str| {
        s.visible();
        s.source_total();
        let t = std::time::Instant::now();
        for i in 0..n {
            s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 100.0})).unwrap();
            s.visible();
            s.source_total();
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        eprintln!("slider tick at 100k photos, {name}: {ms:.3} ms (develop.set + visible + source_total)");
    };
    tick(&mut s, "warm-up");
    tick(&mut s, "by capture date");
    s.sort = Sort { key: SortKey::FileName, ..Default::default() };
    tick(&mut s, "by file name");
    s.sort = Sort::default();
    s.filter = Filter { keyword: Some("kw7".into()), ..Default::default() };
    tick(&mut s, "keyword filter");
    // a view that reads develop state follows each tick from the photo it touched
    s.filter = Filter { edited: Some(false), ..Default::default() };
    tick(&mut s, "Unedited filter");
    s.execute("develop.endInteraction", &json!({})).unwrap();
}
