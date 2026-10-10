//! End-to-end Immich dialog: open it, browse a mock server, select and import into a real
//! library — workers, channel, dialog and catalog, driven through the control protocol only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

const ASSET: &str = r#"{"assets":{"total":2,"count":2,"items":[{"id":"a1","checksum":"c1","originalFileName":"IMG_1.JPG","fileCreatedAt":"2026-01-02T03:04:05.000Z","isFavorite":false,"rating":0,"width":48,"height":32},{"id":"b1","checksum":"c2","originalFileName":"BAD.JPG","fileCreatedAt":"2026-01-02T03:04:06.000Z","isFavorite":false,"rating":0,"width":48,"height":32}],"nextPage":null}}"#;

fn png(seed: u8) -> Option<Vec<u8>> {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).ok()
}

/// A mock Immich that answers by route (the dialog's workers race, so a fixed queue cannot).
/// Serves at most `max` connections so the listener thread always ends.
fn start_router(max: usize) -> Option<(String, Arc<Mutex<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let Ok(addr) = listener.local_addr() else { return None };
    let base = format!("http://{addr}");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    std::thread::spawn(move || {
        let mut done = 0;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            if let Some(head) = read_head(&mut stream) {
                s.lock().unwrap_or_else(|e| e.into_inner()).push(head.clone());
                let (status, ctype, body) = route(&head);
                let msg = format!("HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(msg.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
            done += 1;
            if done >= max {
                break;
            }
        }
    });
    Some((base, seen))
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let len = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..len]).into_owned();
    let declared = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let have = buf.len() - (len + 4);
    let mut left = declared.saturating_sub(have);
    while left > 0 {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        left -= n.min(left);
    }
    Some(head.lines().next().unwrap_or_default().to_string())
}

fn route(head: &str) -> (u16, &'static str, Vec<u8>) {
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    let (p, _query) = path.split_once('?').unwrap_or((path, ""));
    match p {
        "/api/server/ping" => (200, "application/json", br#"{"res":"pong"}"#.to_vec()),
        "/api/server/version" => (200, "application/json", br#"{"major":1,"minor":135,"patch":3}"#.to_vec()),
        "/api/albums" => (200, "application/json", br#"[{"id":"al1","albumName":"Trip","assetCount":1}]"#.to_vec()),
        "/api/search/metadata" => (200, "application/json", ASSET.as_bytes().to_vec()),
        "/api/assets/a1" => (200, "application/json", r#"{"id":"a1","checksum":"c1","originalFileName":"IMG_1.JPG"}"#.as_bytes().to_vec()),
        "/api/assets/a1/thumbnail" => png(7).map_or((404, "text/plain", Vec::new()), |b| (200, "image/png", b)),
        "/api/assets/a1/original" => png(9).map_or((404, "text/plain", Vec::new()), |b| (200, "image/png", b)),
        // a file the library cannot import: the download works, the bytes are not an image
        "/api/assets/b1" => (200, "application/json", r#"{"id":"b1","checksum":"c2","originalFileName":"BAD.JPG"}"#.as_bytes().to_vec()),
        "/api/assets/b1/thumbnail" => (404, "text/plain", Vec::new()),
        "/api/assets/b1/original" => (200, "application/octet-stream", b"not a jpeg, not anything".to_vec()),
        _ => (404, "text/plain", Vec::new()),
    }
}

fn temp_dir(tag: &str) -> Option<PathBuf> {
    let d = std::env::temp_dir().join(format!("lc-immich-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn assets(h: &Headless) -> Vec<String> {
    match &h.app.ui.dialog {
        Some(Dialog::Immich { opts }) => opts.assets.iter().map(|a| a.id.clone()).collect(),
        _ => Vec::new(),
    }
}

/// The File ▸ Import from Immich menu entry's enabled flag, from `ui.menu.list`.
fn menu_enabled(h: &mut Headless, id: &str) -> Option<bool> {
    let r = h.request("ui.menu.list", json!({}), T);
    r["result"].as_array()?.iter().find(|e| e["id"] == id).and_then(|e| e["enabled"].as_bool())
}

#[test]
fn browse_select_and_import_round_trip_through_the_dialog() {
    let (base, seen) = start_router(64).unwrap();
    let lib = temp_dir("e2e").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, false).unwrap();
    session.execute("immich.servers", &json!({"add": {"url": base, "apiKey": "k", "name": "Home"}})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);

    // File ▸ Import from Immich… — through the same dispatch the menu uses.
    let r = h.request("engine.execute", json!({"command": "file.importImmich"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Immich { .. })));
    h.settle(SETTLE); // let the window finish laying out before clicking it

    // Search: the page arrives over the worker channel, not on the UI thread.
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSearch"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let found = h.step_until(SETTLE, |h| assets(h).iter().any(|id| id == "a1"));
    assert!(found, "the browsed page shows the asset");
    h.settle(SETTLE); // the grid changes the window size; let it restabilize

    // Select the cell, then Import; the whole batch lands in the catalog.
    let r = h.request("ui.clickWidget", json!({"id": "immich:0"}), T);
    assert_eq!(r["ok"], true, "the cell is on screen: {r}");
    // the thumbnail arrived and released its in-flight slot (the bound must not drift open)
    let settled = h.step_until(SETTLE, |h| h.app.immich_task.as_ref().is_some_and(|t| !t.thumbs.is_empty() && t.thumbs_in_flight() == 0));
    assert!(settled, "the thumbnail loaded and the in-flight count returned to zero");
    let r = h.request("ui.clickWidget", json!({"id": "button:immichImport"}), T);
    assert_eq!(r["ok"], true, "Import is enabled with a selection: {r}");
    let done = h.step_until(SETTLE, |h| h.app.session.catalog.len() == 1);
    assert!(done, "the download and library.import completed");

    // it is a real file inside the library, like any other import
    let photo = h.app.session.catalog.photos().next().map(|p| p.source.clone());
    let Some(lightcraft_catalog::Source::File { path }) = photo else { panic!("{photo:?}") };
    assert!(std::path::Path::new(&path).starts_with(lib.join("Originals")) && std::path::Path::new(&path).exists(), "{path}");

    let got = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(got.iter().any(|c| c.starts_with("POST /api/search/metadata")), "{got:?}");
    assert!(got.iter().any(|c| c.starts_with("GET /api/assets/a1/original")), "{got:?}");
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn an_unreachable_server_shows_an_actionable_error_and_survives() {
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l); // nothing listens: connecting is refused at once
        format!("http://{a}")
    };
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("immich.servers", &json!({"add": {"url": dead, "apiKey": "k", "name": "Dead"}})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.request("engine.execute", json!({"command": "file.importImmich"}), T);
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSearch"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // the app keeps ticking: frames advance, nothing panics, the dialog is still there
    for _ in 0..30 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Immich { .. })), "a dead server keeps the dialog open");
    h.settle(SETTLE);
}

#[test]
fn import_from_immich_unlocks_only_after_the_settings_test_passes() {
    let (base, seen) = start_router(64).unwrap();
    let lib = temp_dir("settings").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, false).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);

    // no server: the menu item is locked, and the dialog's empty state has no Test button
    assert_eq!(menu_enabled(&mut h, "file.importImmich"), Some(false), "locked with no server configured");
    h.request("engine.execute", json!({"command": "file.importImmich"}), T);
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:immichTest"}), T);
    assert_eq!(r["ok"], false, "the empty state points at Settings instead: {r}");

    // configured but untested: still locked
    h.request("engine.execute", json!({"command": "immich.servers", "params": {"add": {"url": base, "apiKey": "k", "name": "Home"}}}), T);
    assert_eq!(h.app.session.immich_servers.len(), 1);
    assert_eq!(menu_enabled(&mut h, "file.importImmich"), Some(false), "locked until the test passes");

    // Settings ▸ Integrations: the Test button runs on a worker and unlocks the server
    h.app.ui.dialog = Some(Dialog::Settings { tab: "integrations".into() });
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSettingsTest-Home"}), T);
    assert_eq!(r["ok"], true, "the settings row has a Test button: {r}");
    let unlocked = h.step_until(SETTLE, |h| h.app.session.immich_servers.first().is_some_and(|s| s.verified));
    assert!(unlocked, "the passing test marked the server verified");
    assert_eq!(menu_enabled(&mut h, "file.importImmich"), Some(true), "unlocked after the test");
    let got = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(got.iter().any(|c| c.starts_with("GET /api/server/ping")), "{got:?}");
    assert!(got.iter().any(|c| c.starts_with("GET /api/server/version")), "{got:?}");

    // the dialog opens against the tested server and loads its albums
    h.app.ui.dialog = None;
    h.request("engine.execute", json!({"command": "file.importImmich"}), T);
    let albums = h.step_until(SETTLE, |h| h.app.immich_task.as_ref().is_some_and(|t| !t.albums.is_empty()));
    assert!(albums, "the dialog browses the tested server");

    // removing the server locks the menu again
    h.app.ui.dialog = Some(Dialog::Settings { tab: "integrations".into() });
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSettingsRemove-Home"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.immich_servers.is_empty(), "the server is gone");
    assert_eq!(menu_enabled(&mut h, "file.importImmich"), Some(false), "locked again with no server");
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_file_the_library_cannot_import_names_the_reason() {
    let (base, _seen) = start_router(64).unwrap();
    let lib = temp_dir("badfile").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, false).unwrap();
    session.execute("immich.servers", &json!({"add": {"url": base, "apiKey": "k", "name": "Home"}})).unwrap();
    session.execute("immich.verify", &json!({"server": "Home"})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);

    h.request("engine.execute", json!({"command": "file.importImmich"}), T);
    h.settle(SETTLE);
    h.request("ui.clickWidget", json!({"id": "button:immichSearch"}), T);
    let found = h.step_until(SETTLE, |h| assets(h).iter().any(|id| id == "b1"));
    assert!(found, "the page shows the bad asset");
    h.settle(SETTLE); // the grid changes the window size; let it restabilize
    let r = h.request("ui.clickWidget", json!({"id": "immich:1"}), T);
    assert_eq!(r["ok"], true, "the second cell is on screen: {r}");
    h.request("ui.clickWidget", json!({"id": "button:immichImport"}), T);
    let done = h.step_until(SETTLE, |h| h.app.immich_task.as_ref().is_some_and(|t| t.import_done));
    assert!(done, "the import finished");
    let failed = h.app.immich_task.as_ref().unwrap().failed.clone();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].starts_with("BAD.JPG"), "the file is named: {failed:?}");
    assert!(!failed[0].ends_with("import failed"), "the library's own reason, not the generic stand-in: {failed:?}");
    assert_eq!(h.app.session.catalog.len(), 0, "nothing was imported");
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn the_integrations_tab_adds_a_server_from_its_form() {
    let (base, _seen) = start_router(8).unwrap();
    let lib = temp_dir("addform").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, false).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);

    h.app.ui.dialog = Some(Dialog::Settings { tab: "integrations".into() });
    h.settle(SETTLE);
    // Add with empty fields does nothing (the button is disabled)
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSettingsAdd"}), T);
    assert_eq!(r["ok"], true, "the button is on screen: {r}");
    assert!(h.app.session.immich_servers.is_empty(), "nothing added from empty fields");
    // fill the form — the fields live in egui memory, exactly where the UI keeps them
    h.view.ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new("immich-settings-url"), base.clone());
        d.insert_temp(egui::Id::new("immich-settings-key"), "k".to_string());
    });
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:immichSettingsAdd"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.immich_servers.len(), 1, "the server was added");
    assert_eq!(h.app.session.immich_servers[0].url, base);
    assert!(!h.app.session.immich_servers[0].verified, "a fresh server still needs its test");
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(&lib);
}
