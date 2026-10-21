//! The side panels' tallies of the library (folders, keywords, dates, people, counts) are kept through an edit that
//! cannot have changed them, and are never kept through one that did.

use std::sync::Arc;

use serde_json::json;

use crate::{LightcraftApp, Services};

/// A library of photos that are files in folders, taken on different days (the demo's photos are in no folder),
/// some of them keyworded.
fn app() -> LightcraftApp {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut s = lightcraft_engine::Session::new();
    let mut ids = Vec::new();
    for i in 0..60usize {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i:03}.jpg");
        let mut p = Photo::new(id, Source::File { path: format!("/pics/{}/{name}", i % 4) }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
        p.captured = Some(format!("2025-{:02}-{:02}T10:00:00", 1 + i % 12, 1 + i % 28));
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id.0);
    }
    s.execute("library.select", &json!({"ids": ids.iter().step_by(2).collect::<Vec<_>>()})).unwrap();
    s.execute("photo.setMeta", &json!({"addKeywords": ["travel"]})).unwrap();
    LightcraftApp::new(s, Services { png: None, ..Default::default() })
}

/// The tallies as the panels would show them now.
fn shown(a: &mut LightcraftApp) -> String {
    let cat = &a.session.catalog;
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        a.caches.folder_tree(cat),
        a.caches.keyword_tree(cat),
        a.caches.date_groups(cat),
        a.caches.person_names(cat),
        a.caches.counts(cat)
    )
}

/// The same tallies made from every photo.
fn fresh(a: &LightcraftApp) -> String {
    let cat = &a.session.catalog;
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        cat.folder_tree(),
        cat.keyword_tree(),
        cat.date_groups(),
        cat.people().into_iter().map(|p| p.name).collect::<Vec<_>>(),
        crate::Caches::default().counts(cat)
    )
}

#[test]
fn a_tally_kept_through_an_edit_is_the_tally_made_from_every_photo() {
    let mut a = app();
    let ids: Vec<u64> = a.session.visible_cloned().iter().map(|p| p.0).collect();
    let mut dice = 5u64;
    let mut roll = |sides: usize| {
        dice = lightcraft_catalog::mix64(dice);
        (dice % sides as u64) as usize
    };
    assert_eq!(shown(&mut a), fresh(&a));
    assert!(!a.caches.folder_tree(&a.session.catalog).is_empty() && !a.caches.date_groups(&a.session.catalog).is_empty());
    for step in 0..600 {
        let id = ids[roll(ids.len())];
        // (a photo in Recently Deleted can't be chosen: the edit is then not made, which is an edit too)
        let _ = a.session.execute("library.select", &json!({"ids": [id]}));
        let keyword = ["travel", "weddings", "pets|dogs"][roll(3)];
        let flag = ["pick", "reject", "none"][roll(3)];
        // where a file is, when a photo was taken, a photo that comes or goes for good
        {
            use lightcraft_catalog::{Op, Photo, PhotoId, Source};
            let n = roll(1000);
            let moved = match roll(8) {
                0 => {
                    let name = format!("MOVED_{n:03}.jpg");
                    let source = Source::File { path: format!("/elsewhere/{}/{name}", n % 3) };
                    Some(a.session.commit("Locate", Op::Relink { id: PhotoId(id), file_name: name, source, format: None }))
                }
                1 => Some(
                    a.session
                        .commit("Capture time", Op::SetCaptured { id: PhotoId(id), captured: Some(format!("2024-{:02}-03T10:00:00", 1 + n % 12)) }),
                ),
                2 if n.is_multiple_of(3) => Some(a.session.commit("Remove", Op::RemovePhoto { id: PhotoId(id) })),
                2 if n % 3 == 1 => {
                    let new = a.session.catalog.alloc_photo_id();
                    let name = format!("NEW_{n:03}.jpg");
                    let p = Photo::new(new, Source::File { path: format!("/new/{name}") }, &name, "JPEG", 60, 40, "2026-02-01T10:00:00");
                    Some(a.session.commit("Add", Op::AddPhoto { photo: Box::new(p) }))
                }
                _ => None,
            };
            if moved.is_some() {
                if roll(3) > 0 {
                    assert_eq!(shown(&mut a), fresh(&a), "after edit {step}");
                }
                continue;
            }
        }
        let _ = match roll(11) {
            0 | 1 => a.session.execute("photo.rate", &json!({"rating": roll(6)})),
            2 => a.session.execute("photo.flag", &json!({"flag": flag})),
            3 | 4 => a.session.execute("photo.setMeta", &json!({"addKeywords": [keyword]})),
            5 => a.session.execute("photo.setMeta", &json!({"keywords": []})),
            6 => a.session.execute("photo.setMeta", &json!({"title": format!("Title {}", roll(4))})),
            7 => a.session.execute("develop.set", &json!({"control": "light.exposure", "value": 0.1 * roll(9) as f64})),
            8 => a.session.execute("photo.delete", &json!({})),
            9 => a.session.execute("photo.label", &json!({"label": "red"})),
            _ => a.session.undo_step().map(|_| serde_json::Value::Null),
        };
        // (not every frame asks for every tally: sometimes several edits pass between two looks)
        if roll(3) > 0 {
            assert_eq!(shown(&mut a), fresh(&a), "after edit {step}");
        }
    }
}

#[test]
fn a_rating_or_a_slider_keeps_every_tally_and_a_keyword_keeps_the_folders_and_dates() {
    let mut a = app();
    let id = a.session.visible_cloned()[0].0;
    a.session.execute("library.select", &json!({"ids": [id]})).unwrap();
    let look = |a: &mut LightcraftApp| {
        let cat = &a.session.catalog;
        (a.caches.folder_tree(cat), a.caches.keyword_tree(cat), a.caches.date_groups(cat), a.caches.person_names(cat))
    };
    let was = look(&mut a);
    // a rating, then a slider: nothing is tallied again
    a.session.execute("photo.rate", &json!({"rating": 2})).unwrap();
    a.session.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    let now = look(&mut a);
    assert!(Arc::ptr_eq(&was.0, &now.0) && Arc::ptr_eq(&was.1, &now.1) && Arc::ptr_eq(&was.2, &now.2) && Arc::ptr_eq(&was.3, &now.3));
    // a keyword: the keyword tree is made again, the folders and the dates are not
    a.session.execute("photo.setMeta", &json!({"addKeywords": ["travel"]})).unwrap();
    let now = look(&mut a);
    assert!(Arc::ptr_eq(&was.0, &now.0) && Arc::ptr_eq(&was.2, &now.2), "folders and dates kept");
    assert!(!Arc::ptr_eq(&was.1, &now.1), "keywords tallied again");
    // a photo deleted: every tally is made again
    a.session.execute("photo.delete", &json!({})).unwrap();
    let gone = look(&mut a);
    assert!(!Arc::ptr_eq(&now.0, &gone.0) && !Arc::ptr_eq(&now.1, &gone.1) && !Arc::ptr_eq(&now.2, &gone.2));
    // an album made (more than a photo's own change): nothing is assumed
    let before = look(&mut a);
    a.session.execute("album.create", &json!({"name": "Trip"})).unwrap();
    let after = look(&mut a);
    assert!(!Arc::ptr_eq(&before.0, &after.0));
}
