//! Immich in the Import dialog and the key-storage prompt of Settings → Connections.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{DacApp, Services};

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
    let mut s = dac_engine::Session::new();
    s.remote.secrets_file = Some(dir.join("credentials.enc"));
    s.remote.credentials_prefs = Some(dir.join("credentials.json"));
    Headless::new(DacApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0)
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
    let dir = std::env::temp_dir().join(format!("dac-immich-ui-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = app(&dir);
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
    let dir = std::env::temp_dir().join(format!("dac-immich-ui-key-{}", std::process::id()));
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
