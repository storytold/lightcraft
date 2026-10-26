//! The Delete dialog (issue #1706), as in Lightroom: Delete from Disk moves the files to the Trash
//! (here a temp folder, never the real one) and takes the photos out of the library; Remove moves
//! them to Recently Deleted; Cancel does nothing. When the files can't be moved, the dialog says
//! why and offers to remove the photos from the library instead. Photo ▸ Delete Rejected Photos…
//! asks the same about the rejected photos on screen.
// The tests that move files to a trash run where Delete from Disk is on (`trash::available`).
#![cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::trash::TrashRoot;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-delete-disk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
    let opts = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(p, lightcraft_engine::export::encode_image(&img, &opts).unwrap()).unwrap();
}

/// Three files imported from `dir`; the session's Trash is `bin`.
fn app(dir: &Path, bin: &Path) -> (Headless, Vec<PhotoId>) {
    let names = ["a.png", "b.png", "c.png"];
    for (i, n) in names.iter().enumerate() {
        png(&dir.join(n), i as u8 * 40);
    }
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.trash = TrashRoot::Folder(bin.to_path_buf());
    let paths: Vec<String> = names.iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
    let r = s.execute("library.import", &json!({"paths": paths, "mode": "add"})).unwrap();
    let mut ids: Vec<PhotoId> = r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).map(PhotoId).collect();
    ids.sort();
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (h, ids)
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn the_delete_dialog_deletes_from_disk_removes_or_cancels() {
    let (dir, bin) = (temp_dir("ok"), temp_dir("ok-bin"));
    let (mut h, ids) = app(&dir, &bin);
    exec(&mut h, "library.select", json!({"ids": [ids[0].0]}));

    // ⌫ asks, with Lightroom's three answers; Cancel changes nothing
    h.request("ui.key", json!({"key": "delete"}), T);
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1, rejected: false, error: None, .. })), "{:?}", h.app.ui.dialog);
    for b in ["button:dialogDeleteFromDisk", "button:dialogCancel", "button:dialogOk"] {
        assert!(h.app.widgets.iter().any(|(w, _)| w == b), "{b} is shown");
    }
    click(&mut h, "button:dialogCancel");
    assert_eq!(h.app.ui.dialog, None);
    assert!(dir.join("a.png").is_file());

    // Delete from Disk: the file goes to the Trash, the photo out of the library
    h.request("ui.key", json!({"key": "delete"}), T);
    h.step();
    click(&mut h, "button:dialogDeleteFromDisk");
    assert_eq!(h.app.ui.dialog, None, "done: the dialog closed");
    assert!(!dir.join("a.png").exists());
    assert!(bin.join("files/a.png").is_file());
    assert!(h.app.session.catalog.photo(ids[0]).is_none());

    // Remove (OK, the default): to Recently Deleted, the file stays
    exec(&mut h, "library.select", json!({"ids": [ids[1].0]}));
    h.request("ui.key", json!({"key": "delete"}), T);
    h.step();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.catalog.photo(ids[1]).unwrap().deleted);
    assert!(dir.join("b.png").is_file());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn when_the_trash_fails_the_dialog_says_why_and_offers_to_remove_instead() {
    let dir = temp_dir("fail");
    // the "Trash" is a file: nothing can go there
    let bin = dir.join("not-a-trash");
    let (mut h, ids) = app(&dir, &bin);
    std::fs::write(&bin, b"").unwrap();
    exec(&mut h, "library.select", json!({"ids": [ids[0].0, ids[1].0]}));
    h.request("ui.key", json!({"key": "delete"}), T);
    h.step();
    click(&mut h, "button:dialogDeleteFromDisk");
    let Some(Dialog::ConfirmDelete { count, ids: left, error: Some(e), .. }) = h.app.ui.dialog.clone() else {
        panic!("the dialog stays open with the error: {:?}", h.app.ui.dialog)
    };
    assert_eq!(count, 2);
    assert_eq!(left.len(), 2);
    assert!(e.contains("not deleted"), "{e}");
    assert!(dir.join("a.png").is_file() && dir.join("b.png").is_file(), "no file was touched");
    assert!(h.app.session.catalog.photo(ids[0]).is_some_and(|p| !p.deleted));
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "button:dialogDeleteFromDisk"), "no second try from here");
    // Remove from Library: the photos go to Recently Deleted, the files stay
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.catalog.photo(ids[0]).unwrap().deleted && h.app.session.catalog.photo(ids[1]).unwrap().deleted);
    assert!(dir.join("a.png").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn delete_rejected_photos_asks_about_the_rejected_ones() {
    let (dir, bin) = (temp_dir("rejected"), temp_dir("rejected-bin"));
    let (mut h, ids) = app(&dir, &bin);
    // nothing rejected: nothing to ask
    assert!(crate::menubar::run_item(&mut h.app, "dialog.deleteRejected", json!({})).is_err());
    exec(&mut h, "photo.reject", json!({"ids": [ids[0].0, ids[2].0]}));
    exec(&mut h, "library.select", json!({"ids": [ids[1].0]}));
    // ⌘⌫, as in Lightroom
    h.request("ui.key", json!({"key": "delete", "cmd": true}), T);
    h.step();
    let Some(Dialog::ConfirmDelete { count: 2, ids: asked, rejected: true, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    let mut asked = asked;
    asked.sort();
    assert_eq!(asked, vec![ids[0].0, ids[2].0], "the rejected photos, not the selection");
    click(&mut h, "button:dialogDeleteFromDisk");
    assert!(!dir.join("a.png").exists() && !dir.join("c.png").exists());
    assert!(dir.join("b.png").is_file());
    assert!(h.app.session.catalog.photo(ids[1]).is_some());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}

#[test]
fn delete_rejected_photos_in_an_album_only_takes_them_out_of_it() {
    let (dir, bin) = (temp_dir("album"), temp_dir("album-bin"));
    let (mut h, ids) = app(&dir, &bin);
    exec(&mut h, "album.create", json!({"name": "Trip"}));
    let album = h.app.session.catalog.albums().find(|a| a.name == "Trip").unwrap().id;
    exec(&mut h, "album.addPhotos", json!({"id": album.0, "ids": [ids[0].0, ids[1].0]}));
    exec(&mut h, "photo.reject", json!({"ids": [ids[0].0]}));
    exec(&mut h, "library.source", json!({"kind": "album", "id": album.0}));
    crate::menubar::run_item(&mut h.app, "dialog.deleteRejected", json!({})).unwrap();
    assert_eq!(h.app.ui.dialog, None, "no dialog in an album");
    let a = h.app.session.catalog.album(album).unwrap();
    assert!(!a.photos.contains(&ids[0]) && a.photos.contains(&ids[1]));
    assert!(h.app.session.catalog.photo(ids[0]).is_some_and(|p| !p.deleted), "still in the library");
    assert!(dir.join("a.png").is_file());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}
