//! In an album, the Delete key takes the selected photos out of the album only (issue #1805): they
//! stay in the library and in their folder, as in Lightroom. Photo ▸ Delete Photo still moves them
//! to Recently Deleted, from everywhere.

use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::headless::Headless;
use crate::menubar::MenuNode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

/// The Photo menu's items: (id, shortcut).
fn photo_menu(h: &Headless) -> Vec<(String, Option<String>)> {
    let bar = crate::menubar::menu_bar(&h.app);
    let photo = &bar.iter().find(|(t, _)| t == "Photo").expect("Photo menu").1;
    photo
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { id, shortcut, .. } => Some((id.clone(), shortcut.clone())),
            _ => None,
        })
        .collect()
}

fn garden(h: &Headless) -> u64 {
    h.app.session.catalog.albums().find(|a| a.name == "Garden").map(|a| a.id.0).expect("the demo's Garden album")
}

fn in_album(h: &Headless, album: u64, photo: PhotoId) -> bool {
    h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).is_some_and(|a| a.photos.contains(&photo))
}

/// Given an album, when a photo is selected in it and Delete is pressed, then the photo leaves the
/// album and nothing else: it is not deleted, nothing asks, and All Photos still shows it.
#[test]
fn the_delete_key_in_an_album_removes_from_the_album_only() {
    let mut h = demo();
    assert!(h.app.ui.settings.confirm_delete, "the delete confirmation is on by default");
    let album = garden(&h);
    exec(&mut h, "library.source", json!({"kind": "album", "id": album}));
    let photo = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [photo.0]}));

    let r = h.request("ui.key", json!({"key": "delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.dialog, None, "removing from an album asks nothing (it is undoable and keeps the photo)");
    assert!(!in_album(&h, album, photo), "the photo left the album");
    assert!(!h.app.session.catalog.photo(photo).unwrap().deleted, "the photo is not deleted from the library");
    exec(&mut h, "library.source", json!({"kind": "all"}));
    assert!(h.app.session.visible_cloned().contains(&photo), "All Photos still shows it");

    // one undo brings it back into the album
    exec(&mut h, "edit.undo", json!({}));
    assert!(in_album(&h, album, photo));
}

/// In an album the Photo menu offers Remove from Album with the Delete key, and Delete Photo (no
/// key) still deletes from the library, after asking; outside albums there is no Remove from Album.
#[test]
fn the_photo_menu_in_an_album_has_remove_from_album_on_the_delete_key() {
    let mut h = demo();
    let photo = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [photo.0]}));
    let items = photo_menu(&h);
    assert!(!items.iter().any(|(id, _)| id == "album.removePhotos"), "not outside albums: {items:?}");
    assert!(items.iter().any(|(id, sc)| id == "photo.delete" && sc.as_deref() == Some("Delete")), "{items:?}");

    let album = garden(&h);
    exec(&mut h, "library.source", json!({"kind": "album", "id": album}));
    let photo = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [photo.0]}));
    let items = photo_menu(&h);
    assert!(items.iter().any(|(id, sc)| id == "album.removePhotos" && sc.as_deref() == Some("Delete")), "{items:?}");
    assert!(items.iter().any(|(id, sc)| id == "photo.delete" && sc.is_none()), "{items:?}");

    // Photo ▸ Remove from Album (no params: the album on screen)
    crate::menubar::run_item(&mut h.app, "album.removePhotos", json!({})).unwrap();
    assert!(!in_album(&h, album, photo));
    assert!(!h.app.session.catalog.photo(photo).unwrap().deleted);

    // Photo ▸ Delete Photo still deletes from the library: it asks, then OK deletes
    let other = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [other.0]}));
    crate::menubar::run_item(&mut h.app, "photo.delete", json!({})).unwrap();
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ConfirmDelete { count: 1 }));
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.catalog.photo(other).unwrap().deleted, "Delete Photo removes it from the library");
    exec(&mut h, "library.source", json!({"kind": "all"}));
    assert!(!h.app.session.visible_cloned().contains(&other));
}

/// A smart album's photos follow its rules: Delete there neither removes nor deletes anything.
#[test]
fn the_delete_key_in_a_smart_album_deletes_nothing() {
    let mut h = demo();
    let r = exec(&mut h, "album.createSmart", json!({"name": "Rated", "rules": {"rating": 0, "ratingOp": "atLeast"}}));
    let smart = r["id"].as_u64().expect("smart album id");
    exec(&mut h, "library.source", json!({"kind": "album", "id": smart}));
    let photo = *h.app.session.visible_cloned().first().expect("the smart album shows photos");
    exec(&mut h, "library.select", json!({"ids": [photo.0]}));
    let r = h.request("ui.key", json!({"key": "delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.dialog, None);
    assert!(!h.app.session.catalog.photo(photo).unwrap().deleted);
    assert!(h.app.session.visible_cloned().contains(&photo));
    assert!(!photo_menu(&h).iter().any(|(id, _)| id == "album.removePhotos"));
}
