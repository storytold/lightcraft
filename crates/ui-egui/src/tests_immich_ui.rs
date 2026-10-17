//! Immich in the Import dialog and the key-storage prompt of Settings → Connections.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

fn app(dir: &std::path::Path) -> Headless {
    let mut s = lcc_engine::Session::new();
    s.remote.secrets_file = Some(dir.join("credentials.enc"));
    s.remote.credentials_prefs = Some(dir.join("credentials.json"));
    Headless::new(LccApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0)
}

fn immich_source(h: &Headless) -> Option<bool> {
    match &h.app.ui.dialog {
        Some(Dialog::Import { opts }) => Some(opts.immich),
        _ => None,
    }
}

// Feature: File ▸ Import from Immich opens the Import dialog on its Immich source (not a window
// of its own); the dialog switches between Files and Immich.
#[test]
fn import_from_immich_is_a_source_of_the_import_dialog() {
    let dir = std::env::temp_dir().join(format!("lcc-immich-ui-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = app(&dir);
    // keys in memory, so the dialog doesn't ask for a passphrase on hosts without a system keychain (FreeBSD CI)
    h.app.session.set_secret_store(std::sync::Arc::new(lcc_engine::remote::MemorySecrets::default()));
    let r = h.request("engine.execute", json!({"command": "file.importImmich", "params": {}}), T);
    assert_eq!(r["result"]["source"], "immich", "{r}");
    h.step();
    h.step();
    assert_eq!(immich_source(&h), Some(true));
    assert!(has(&h, "button:importSource:files") && has(&h, "button:importSource:immich"));
    assert!(has(&h, "button:immichOpenConnections"), "no server: offers to connect one");
    click(&mut h, "button:importSource:files");
    assert_eq!(immich_source(&h), Some(false));
    assert!(has(&h, "button:importChooseFiles"));
    click(&mut h, "button:importSource:immich");
    assert_eq!(immich_source(&h), Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

// Feature: keys kept in the encrypted file are unlocked with a passphrase prompt once per session.
#[test]
fn key_file_prompt_creates_and_unlocks_the_file() {
    let dir = std::env::temp_dir().join(format!("lcc-immich-ui-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = app(&dir);
    h.app.session.execute("credentials.useStore", &json!({"store": "file"})).unwrap();
    h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "connections"}}), T);
    h.step();
    h.step();
    assert!(has(&h, "credUnlockBox"), "the passphrase prompt shows");
    assert!(has(&h, "field:credPassphrase2"), "a new file asks twice");
    h.app.immich.type_passphrase("correct horse battery");
    click(&mut h, "button:credUnlock");
    let start = std::time::Instant::now();
    while h.app.session.execute("credentials.status", &json!({})).unwrap()["active"] != "encrypted file" {
        assert!(start.elapsed() < T, "never unlocked");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, "credUnlockBox"), "unlocked: no prompt");
    assert!(dir.join("credentials.enc").exists() || h.app.session.execute("credentials.status", &json!({})).unwrap()["file"]["unlocked"] == true);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tiny Immich server: version, user, key permissions (`perms`); everything else is empty.
#[allow(clippy::unwrap_used)]
fn mini_server(perms: Vec<&'static str>) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut r = BufReader::new(&mut s);
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or_default().split('?').next().unwrap_or_default().to_string();
            let mut len = 0usize;
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).is_err() || l.trim_end().is_empty() {
                    break;
                }
                if let Some((k, v)) = l.trim_end().split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            let v = match path.as_str() {
                "/api/server/version" => json!({"major": 3, "minor": 3, "patch": 1}),
                "/api/users/me" => json!({"id": "user-1", "email": "me@example.invalid", "name": "Me"}),
                "/api/api-keys/me" => json!({"id": "k", "name": "app", "permissions": perms}),
                "/api/search/metadata" => json!({"assets": {"total": 0, "count": 0, "items": [], "nextPage": null}}),
                _ => json!([]),
            };
            let b = v.to_string();
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{b}", b.len());
        }
    });
    format!("http://127.0.0.1:{port}")
}

// Feature: the server card stays short: only the permissions the key lacks (or one line saying
// none), the full list behind "Show permissions"; one status line for the latest sync; the album
// rule row in the sync rules.
#[test]
fn immich_card_is_compact() {
    let dir = std::env::temp_dir().join(format!("lcc-immich-ui-card-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = app(&dir);
    h.app.session.set_secret_store(std::sync::Arc::new(lcc_engine::remote::MemorySecrets::default()));
    let mut perms: Vec<&'static str> = lcc_immich::client::FEATURE_PERMISSIONS.iter().flat_map(|(_, p)| p.iter().copied()).collect();
    perms.extend(["map.read", "memory.read", "sharedLink.create", "sharedLink.read", "session.read", "workflow.read", "systemConfig.read"]);
    let url = mini_server(perms);
    let r = h.app.session.execute("immich.connect", &json!({"url": url, "apiKey": "ui-test-key"})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "connections"}}), T);
    h.step();
    h.step();
    assert!(has(&h, "label:immichPermsOk-0"), "one line: nothing missing");
    assert!(!has(&h, "label:immichPermsAll-0"), "the full list starts collapsed");
    assert!(!has(&h, "label:immichMissing-0"));
    click(&mut h, "button:immichPerms-0");
    assert!(has(&h, "label:immichPermsAll-0"), "Show permissions opens the list");
    click(&mut h, "button:immichPerms-0");
    assert!(!has(&h, "label:immichPermsAll-0"));
    click(&mut h, "button:immichSyncPanel-0");
    assert!(has(&h, "label:immichSyncStatus-0"));
    let text = h.painted_text().join("\n");
    assert!(text.contains("Albums ↔ albums"), "the album rule row");
    if let Some(p) = lcc_brand::env_os("TEST_SHOTS") {
        h.app.services.png = Some(Box::new(|img: &lcc_raster::Rgba8| {
            lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
        }));
        h.app.services.write = Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string())));
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("immich-card.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    // a key that lacks permissions lists only what is missing
    let url = mini_server(vec!["asset.read", "asset.view", "asset.download"]);
    h.app.session.execute("immich.disconnect", &json!({})).unwrap();
    assert_eq!(h.app.session.execute("immich.connect", &json!({"url": url, "apiKey": "ui-test-key"})).unwrap()["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "label:immichMissing-0") && !has(&h, "label:immichPermsOk-0"));
    let _ = std::fs::remove_dir_all(&dir);
}
