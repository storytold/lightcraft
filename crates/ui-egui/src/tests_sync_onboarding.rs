//! Sync onboarding on a phone (fork-owned): the camera roll's review offers to keep adding new
//! photos and adds them in place; the Modern library's title lists the collections to show; the
//! Sync tab's collections fit a phone's width and tell each device's unfiled photos apart.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

type Answer = Arc<Mutex<Option<Sender<Result<Vec<String>, String>>>>>;

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn has(h: &Headless, id: &str) -> bool {
    rect(h, id).is_some()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

/// With `<PREFIX>_TEST_SHOTS=<dir>`, a picture of the screen as `<dir>/<name>.png` (to look at).
fn shot(h: &mut Headless, name: &str) {
    let Some(dir) = lcc_brand::env_os("TEST_SHOTS") else { return };
    h.app.services.png = Some(Box::new(|img: &lcc_raster::Rgba8| {
        lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
    }));
    h.app.services.write = Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string())));
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
}

fn phone(app: LccApp) -> Headless {
    let mut h = Headless::new(app, [400.0, 860.0], 1.0);
    h.request("ui.widgets", json!({}), T);
    h.settle(SETTLE);
    h
}

// Scenario: the camera roll is imported from the empty library's Camera Roll; the review adds the
// photos where they are (no Copy or Move), and offers to keep adding new ones, which turns the
// roll's watch on
#[test]
fn the_camera_roll_review_adds_in_place_and_offers_the_watch() {
    let dir = lcc_testkit::TempDir::new("lcc-sync-onboarding-roll");
    std::fs::create_dir_all(&dir).unwrap();
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[40, 90, 9, 255]; 64] };
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("IMG_1.png");
    std::fs::write(&photo, png).unwrap();

    let answer: Answer = Default::default();
    let a = answer.clone();
    let services = Services {
        png: None,
        // (a folder picker: Copy and Move would be offered for any other source)
        pick_folder: Some(Box::new(|| None)),
        camera_roll: Some(Box::new(move || {
            let (tx, rx) = channel();
            *a.lock().unwrap() = Some(tx);
            Ok(rx)
        })),
        ..Default::default()
    };
    let mut app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    app.ui.camera_roll_asked = true;
    let mut h = phone(app);
    h.app.run("file.addCameraRoll", json!({})).unwrap();
    answer.lock().unwrap().take().unwrap().send(Ok(vec![photo.to_string_lossy().to_string()])).unwrap();
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    h.settle(SETTLE);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review is open") };
    assert!(opts.camera_roll && !opts.copy && !opts.move_files, "the roll is added in place");
    assert!(has(&h, "check:importWatchCameraRoll"), "the review offers to keep adding new photos");
    assert!(has(&h, "label:importWatchCameraRollHelp"), "and says where they go");
    assert!(!has(&h, "button:importCopy") && !has(&h, "button:importMove"), "no Copy or Move for the roll");
    assert!(!h.app.ui.camera_roll_watch);
    click(&mut h, "check:importWatchCameraRoll");
    shot(&mut h, "sync-onboarding-camera-roll-review");
    assert!(h.app.ui.camera_roll_watch, "the box turns the roll's watch on");
    let _ = std::fs::remove_dir_all(&dir);
}

// Scenario: on a phone (Modern layout) the person taps the library's title, sees the collections
// (one inside a set) and taps one: the grid shows it, and the list goes away
#[test]
fn the_modern_title_lists_the_collections_and_shows_one() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let set = app.session.execute("album.create", &json!({"name": "Immich", "folder": true})).unwrap()["id"].as_u64().unwrap();
    let wedding = app.session.execute("album.create", &json!({"name": "Wedding", "parent": set})).unwrap()["id"].as_u64().unwrap();
    app.ui.modern.enabled = true;
    let mut h = phone(app);
    // (the demo opens in Develop)
    click(&mut h, "modern:mode:library");
    assert!(has(&h, "button:modernSources"), "the title is a button");
    click(&mut h, "button:modernSources");
    assert!(has(&h, "panel:modernSources"), "the list opens");
    let id = format!("button:modernSource-album{wedding}");
    assert!(has(&h, &id), "a collection inside a set is listed");
    let panel = rect(&h, "panel:modernSources").unwrap();
    assert!(panel.right() <= 400.0 + 0.5, "the list fits the phone: {panel:?}");
    shot(&mut h, "sync-onboarding-sources");
    click(&mut h, &id);
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Album(lcc_catalog::AlbumId(wedding)));
    assert!(!has(&h, "panel:modernSources"), "choosing closes the list");
    assert!(matches!(h.app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid), "the taps stay in the Library");
    // and back to everything
    click(&mut h, "button:modernSources");
    click(&mut h, "button:modernSource-all");
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::All);
}

// Scenario: two devices sync the whole catalog, with a few Immich albums; on a phone-wide Settings
// ▸ Sync every collection's choice stays on screen, the two devices' unfiled rows read apart, and
// the Immich albums fold under one line
#[test]
fn the_sync_collections_fit_a_phone_and_read_apart() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.modern.enabled = true;
    let mut collections = vec![
        json!({"id": "roll:b", "kind": "cameraRoll", "name": "Camera Roll - Fold7", "ownerName": "Fold7", "grant": "sync", "take": "previews", "most": "fullSync", "letters": "RP", "photos": 120}),
        json!({"id": "unfiled:a", "kind": "unfiled", "mine": true, "ownerName": "g1-cachy", "grant": "preview", "take": "none", "most": "none", "letters": "LN", "photos": 4}),
        json!({"id": "unfiled:b", "kind": "unfiled", "ownerName": "Fold7", "grant": "sync", "take": "none", "most": "fullSync", "letters": "RN", "photos": 9}),
    ];
    for i in 0..4 {
        collections.push(json!({"id": format!("immich:u:{i}"), "kind": "collection", "immich": true, "name": format!("A rather long Immich album name {i}"), "grant": "sync", "take": if i == 0 { "previews" } else { "none" }, "most": "fullQuality", "letters": "IN", "photos": 30}));
    }
    let status = json!({
        "name": "g1-cachy",
        "paired": [],
        "catalogue": {
            "photos": 10,
            "group": {"mode": "catalogue", "devices": [{"deviceId": "a", "name": "g1-cachy", "me": true}, {"deviceId": "b", "name": "Fold7", "online": true, "paired": true}]},
            "collections": collections,
        },
    });
    app.peer_sync.set_status_for_test(status);
    let mut h = phone(app);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "sync".into() });
    h.settle(SETTLE);
    for key in ["roll:b", "unfiled:a", "unfiled:b"] {
        let id = format!("combo:syncPick-{key}");
        crate::settings_screen::reveal(&mut h, &id);
        let r = rect(&h, &id).unwrap_or_else(|| panic!("no {id}"));
        assert!(r.right() <= 400.0 + 0.5, "{id} is on screen: {r:?}");
    }
    let names: Vec<String> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("text:syncCollection-unfiled")).map(|(w, _)| w.clone()).collect();
    assert_eq!(names.len(), 2, "both devices' unfiled rows: {names:?}");
    assert_eq!(crate::panels::peer_sync::unfiled_name("Fold7"), crate::i18n::tr_format!("Not in a collection on {device}", device = "Fold7"));
    // the albums are folded until asked for
    crate::settings_screen::reveal(&mut h, "text:syncImmichGroup");
    assert!(has(&h, "text:syncImmichGroup"));
    assert!(!has(&h, "combo:syncPick-immich:u:1"), "the albums start folded");
    click(&mut h, "text:syncImmichGroup");
    crate::settings_screen::reveal(&mut h, "combo:syncPick-immich:u:3");
    let r = rect(&h, "combo:syncPick-immich:u:3").expect("unfolded, every album has its choice");
    assert!(r.right() <= 400.0 + 0.5, "an album's choice is on screen: {r:?}");
    shot(&mut h, "sync-onboarding-collections");
}
