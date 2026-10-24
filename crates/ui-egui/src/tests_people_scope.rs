//! The People view lists the people and unnamed faces of the photos the view shows: the sidebar's source (an album,
//! Picks, a folder…) and the filter bar (issue #2516). All Photos, Missing Photos and Recently Deleted list everyone.

use std::time::Duration;

use lightcraft_catalog::{Flag, Op, PhotoId};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn face(x: f64, name: Option<&str>) -> lightcraft_meta::Region {
    lightcraft_meta::Region {
        rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 },
        kind: lightcraft_meta::RegionKind::Face,
        name: name.map(str::to_string),
        description: None,
    }
}

/// `id`'s face regions become `regions` (whatever it had before).
fn set_faces(h: &mut Headless, id: PhotoId, regions: Vec<lightcraft_meta::Region>) {
    let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
    meta.regions = regions;
    h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command} {params}: {r}");
    h.settle(SETTLE);
    h.step();
    r["result"].clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

fn shows(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({}), T).to_string().contains(&format!("\"{id}\""))
}

/// In an album only its people and unnamed faces are listed, under its name, and a card's Show photos fills the grid.
/// Picks and the filter bar narrow the list too; All Photos, Missing Photos and Recently Deleted list everyone.
#[test]
fn the_people_view_follows_the_source_and_the_filter_bar() {
    let mut h = demo();
    let ids: Vec<PhotoId> = h.app.session.catalog.photos().map(|p| p.id).take(3).collect();
    let (jane, john, stranger) = (ids[0], ids[1], ids[2]);
    set_faces(&mut h, jane, vec![face(0.1, Some("Jane Doe"))]);
    set_faces(&mut h, john, vec![face(0.3, Some("John Roe"))]);
    set_faces(&mut h, stranger, vec![face(0.3, None)]);
    // (the demo photos come with flags and ratings of their own)
    for id in [jane, john, stranger] {
        h.app.session.commit("setup", Op::SetFlag { id, flag: Flag::None }).unwrap();
        h.app.session.commit("setup", Op::SetRating { id, rating: 0 }).unwrap();
    }
    let unnamed = format!("unnamed-face:{}:0", stranger.0);
    let album = exec(&mut h, "album.create", json!({"name": "Shoot"}))["id"].clone();
    exec(&mut h, "album.addPhotos", json!({"id": album, "ids": [jane.0, stranger.0]}));
    exec(&mut h, "library.source", json!({"kind": "album", "id": album}));
    let r = exec(&mut h, "view.people", json!({}));
    assert_eq!(r, json!({"people": 1, "source": "Shoot"}), "the album's people, and where they come from");
    assert!(shows(&mut h, "person:Jane Doe") && shows(&mut h, &unnamed), "the album's named and unnamed faces");
    assert!(!shows(&mut h, "person:John Roe"), "someone who is not in the album is not listed");
    assert!(shows(&mut h, "people:source"), "the title says where the people come from");
    // their photos in the grid: the album's photos of them, never an empty grid
    click(&mut h, "person:Jane Doe");
    click(&mut h, "person:photos");
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    assert_eq!(h.app.session.visible_cloned(), vec![jane]);

    // everyone again under All Photos (the person filter Show photos set does not narrow the cards)
    exec(&mut h, "library.source", json!({"kind": "all"}));
    let r = exec(&mut h, "view.people", json!({}));
    assert_eq!(r, json!({"people": 2, "source": null}));
    assert!(shows(&mut h, "person:Jane Doe") && shows(&mut h, "person:John Roe") && shows(&mut h, &unnamed));
    assert!(!shows(&mut h, "people:source"));
    exec(&mut h, "library.filter", json!({"person": null}));

    // Picks: only the picked photos' people
    h.app.session.commit("pick", Op::SetFlag { id: john, flag: Flag::Pick }).unwrap();
    exec(&mut h, "library.source", json!({"kind": "picks"}));
    exec(&mut h, "view.people", json!({}));
    assert!(shows(&mut h, "person:John Roe") && !shows(&mut h, "person:Jane Doe") && !shows(&mut h, &unnamed), "Picks");

    // the filter bar narrows the named and the unnamed faces alike
    exec(&mut h, "library.source", json!({"kind": "all"}));
    h.app.session.commit("rate", Op::SetRating { id: jane, rating: 5 }).unwrap();
    exec(&mut h, "library.filter", json!({"rating": 5}));
    exec(&mut h, "view.people", json!({}));
    assert!(shows(&mut h, "person:Jane Doe") && !shows(&mut h, "person:John Roe") && !shows(&mut h, &unnamed), "the filter bar");
    exec(&mut h, "library.filter", json!({"rating": 0}));

    // the views that are not a part of the library list everyone
    for kind in ["recentlyDeleted", "missing"] {
        exec(&mut h, "library.source", json!({"kind": kind}));
        let r = exec(&mut h, "view.people", json!({}));
        assert_eq!(r, json!({"people": 2, "source": null}), "{kind}");
        assert!(shows(&mut h, "person:Jane Doe") && shows(&mut h, "person:John Roe") && shows(&mut h, &unnamed), "{kind}");
    }
}

/// A folder on disk: the people of its photos that are in the library, not of the library's other photos. A file only
/// browsed there is no one's yet (their page, and Rename Person, look at the library), so a card always opens on their
/// faces.
#[test]
fn a_browsed_folder_lists_the_people_of_its_library_photos() {
    let mut h = demo();
    let elsewhere = h.app.session.catalog.photos().map(|p| p.id).next().unwrap();
    set_faces(&mut h, elsewhere, vec![face(0.1, Some("Cid")), face(0.5, None)]);
    let dir = std::env::temp_dir().join(format!("lc-ui-people-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    for i in 0..2u8 {
        let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 90 * i, 255]);
        std::fs::write(dir.join(format!("p{i}.png")), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let vis = h.app.session.visible_cloned();
    assert_eq!(vis.len(), 2);
    let (imported, browsed) = (vis[0], vis[1]);
    exec(&mut h, "photo.addToLibrary", json!({"ids": [imported.0]}));
    set_faces(&mut h, imported, vec![face(0.1, Some("Ann")), face(0.5, None)]);
    set_faces(&mut h, browsed, vec![face(0.1, Some("Bea")), face(0.5, None)]);
    let r = exec(&mut h, "view.people", json!({}));
    assert_eq!(r["people"], 1, "{r}");
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(r["source"].as_str().is_some_and(|s| s.ends_with(&name)), "the folder's name, not \"Folder\": {r}");
    assert!(shows(&mut h, "person:Ann") && shows(&mut h, &format!("unnamed-face:{}:1", imported.0)));
    assert!(!shows(&mut h, "person:Bea") && !shows(&mut h, &format!("unnamed-face:{}:1", browsed.0)), "a browsed file is no one's");
    assert!(!shows(&mut h, "person:Cid") && !shows(&mut h, &format!("unnamed-face:{}:1", elsewhere.0)), "nor is a photo elsewhere");
    click(&mut h, "person:Ann");
    assert!(shows(&mut h, &format!("person-face:{}:0", imported.0)), "the card opens on their face");
    let _ = std::fs::remove_dir_all(&dir);
}
