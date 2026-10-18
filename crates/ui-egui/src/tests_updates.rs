//! #1553: Help ▸ Check for Updates… offers a newer release in a dialog whose Download opens the
//! release page, says when the running version is the latest, and never installs anything.
#![cfg(not(target_arch = "wasm32"))]

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

/// A local server answering every request with `body` as GitHub's latest release.
fn serve(body: String) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
            let _ = c.write_all(body.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/latest")
}

fn headless() -> (Headless, Arc<Mutex<Vec<String>>>) {
    headless_opening(Ok(()))
}

/// A headless app whose browser handoff records each URL and answers `result`.
fn headless_opening(result: Result<(), String>) -> (Headless, Arc<Mutex<Vec<String>>>) {
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    let services = Services {
        png: None,
        open_url: Some(Box::new(move |u: &str| {
            o.lock().unwrap().push(u.to_string());
            result.clone()
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    (h, opened)
}

/// Run the check against `endpoint` and step frames until the UI has shown its result.
fn check(h: &mut Headless, endpoint: &str) {
    let r = h.app.session.execute("app.checkForUpdates", &json!({"endpoint": endpoint})).unwrap();
    assert_eq!(r["started"], true, "{r}");
    let start = Instant::now();
    while h.app.session.updates.checking() || (h.app.ui.dialog.is_none() && h.app.ui.toast.is_none()) {
        assert!(start.elapsed() < T, "the check never ended");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.step();
    h.step();
}

#[test]
fn help_lists_check_for_updates() {
    let (h, _) = headless();
    let help = crate::menubar::menu_bar(&h.app).into_iter().find(|(t, _)| t == "Help").map(|(_, items)| items).unwrap_or_default();
    assert!(format!("{help:?}").contains("app.checkForUpdates"), "{help:?}");
}

#[test]
fn a_newer_release_is_offered_and_download_opens_its_page() {
    let (mut h, opened) = headless();
    let page = "https://github.com/storytold/lightcraft/releases/tag/v999.0.0";
    check(&mut h, &serve(json!({"tag_name": "v999.0.0", "html_url": page}).to_string()));
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::UpdateAvailable { latest: "999.0.0".into(), url: page.into() }));
    let text = h.painted_text();
    for want in ["Update Available", "LightCraft 999.0.0 is available.", "Download", "Later"] {
        assert!(text.iter().any(|t| t == want), "{want:?} not painted: {text:?}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Download closes the dialog");
    assert_eq!(opened.lock().unwrap().as_slice(), [page], "only the release page is opened; nothing is installed");

    // Later just closes it
    check(&mut h, &serve(json!({"tag_name": "v999.0.0", "html_url": page}).to_string()));
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(opened.lock().unwrap().len(), 1);
}

#[test]
fn a_browser_that_cannot_open_keeps_the_offer_and_says_why() {
    let (mut h, opened) = headless_opening(Err("no browser to open links with".into()));
    let page = "https://github.com/storytold/lightcraft/releases/tag/v999.0.0";
    check(&mut h, &serve(json!({"tag_name": "v999.0.0", "html_url": page}).to_string()));
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(opened.lock().unwrap().as_slice(), [page]);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::UpdateAvailable { .. })), "the offer stays so Download can be tried again");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("no browser to open links with"), "{toast:?}");
}

#[test]
fn the_latest_version_says_so_in_a_toast() {
    let (mut h, opened) = headless();
    check(&mut h, &serve(json!({"tag_name": "v0.0.1"}).to_string()));
    assert!(h.app.ui.dialog.is_none());
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert_eq!(toast, format!("LightCraft {} is the latest version.", env!("CARGO_PKG_VERSION")));
    assert!(opened.lock().unwrap().is_empty());
}
