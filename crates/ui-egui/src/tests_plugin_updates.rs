//! Plug-in updates in the Plug-in Manager: a stamped, signed plug-in with a feed has a *Check for
//! updates* box (off by default); opening the window checks the feeds turned on; the update found
//! shows with its version, notes and the groups its feed declares; **Update** downloads and
//! installs it, and asks first when the file wants groups the feed didn't declare.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);
const ID: &str = "test.uiupdates";
const FEED: &str = "https://feed.test/ui-updates.json";

/// Serves feeds and files from a map; anything else is a 404.
#[derive(Debug, Default)]
struct Served(std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>);

impl lcc_plugins::Fetch for Served {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        let body = self.0.lock().unwrap().get(url).cloned().ok_or_else(|| format!("{url}: HTTP 404"))?;
        if body.len() as u64 > max { Err("too large".into()) } else { Ok(body) }
    }
}

/// Version `version` of plug-in [`ID`], stamped with its manifest and signed with the key whose
/// seed is all 7s, asking for `permissions`; it answers every call with `{}`.
fn module(version: &str, permissions: Value) -> Vec<u8> {
    let manifest =
        json!({"id": ID, "name": "Updated", "version": version, "requires": ["updates"], "updateUrl": FEED, "permissions": permissions}).to_string();
    let esc = manifest.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{esc}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192)))
  (@custom "{section}" "{esc}"))"#,
        ml = manifest.len(),
        section = lcc_plugins::MANIFEST_SECTION,
    );
    sign(&wat::parse_str(&src).unwrap())
}

/// `bytes` with an author signature section appended, as `plugin-stamp sign` writes it.
fn sign(bytes: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let data = json!({"v": 1, "key": b64(key.verifying_key().as_bytes()), "sig": b64(&key.sign(bytes).to_bytes())}).to_string();
    let leb = |out: &mut Vec<u8>, mut n: usize| loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        out.push(if n == 0 { b } else { b | 0x80 });
        if n == 0 {
            break;
        }
    };
    let name = lcc_plugins::SIGNATURE_SECTION;
    let mut payload = Vec::new();
    leb(&mut payload, name.len());
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(data.as_bytes());
    let mut out = bytes.to_vec();
    out.push(0);
    leb(&mut out, payload.len());
    out.extend_from_slice(&payload);
    out
}

fn sha256(bytes: &[u8]) -> String {
    let i = lcc_plugins::Plugin::inspect(bytes, lcc_plugins::Limits::default(), false).unwrap();
    i.digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn shown(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(text))
}

fn version() -> Option<String> {
    let m = lcc_engine::cmd::plugins::manager();
    Some(m.as_ref()?.get(ID)?.plugin.manifest().version.clone())
}

/// Runs frames until the update worker is done and its result is on screen.
fn finish(h: &mut Headless) {
    assert!(h.step_until(T, |_| !crate::panels::plugins::updates_running()), "the update worker finished");
    h.settle(T);
}

/// AC-3, AC-5, AC-6: off by default and not checked; turned on, the window's opening checks; the
/// row shows the update from the feed; Update asks about an undeclared group and installs on
/// Update Anyway; nothing new is granted.
#[test]
fn the_manager_checks_on_open_shows_the_update_and_asks_before_undeclared_groups() {
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let v1 = module("1.0.0", json!({"catalog": true}));
    let v2 = module("2.0.0", json!({"catalog": true, "metadataWrite": true, "crop": true}));
    let served = Arc::new(Served::default());
    // the feed declares crop but not metadataWrite
    let entry = json!({"id": ID, "version": "2.0.0", "url": "https://feed.test/ui-v2.wasm", "sha256": sha256(&v2),
        "permissions": ["catalog", "crop"], "changelog": "https://feed.test/CHANGELOG.md", "notes": "Writes <b>captions</b> now."});
    served.0.lock().unwrap().insert(FEED.into(), json!({"ui-updates": entry}).to_string().into_bytes());
    served.0.lock().unwrap().insert("https://feed.test/ui-v2.wasm".into(), v2);
    // the manager is the process's: its fetcher goes back to the network however the test ends
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(m) = lcc_engine::cmd::plugins::manager().as_mut() {
                m.set_fetcher(Arc::new(lcc_plugins::updates::Net));
                let _ = m.uninstall(ID);
            }
        }
    }
    let _restore = Restore;
    {
        let mut g = lcc_engine::cmd::plugins::manager();
        let m = g.as_mut().unwrap();
        m.set_fetcher(served.clone());
        m.install_bytes(&v1, None, false).unwrap();
    }
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    h.settle(T);
    let page = |h: &mut Headless, open: bool| {
        let r = h.request("engine.execute", json!({"command": "plugins.page", "params": {"id": ID, "tab": "about", "open": open}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(T);
    };

    // off by default: the box shows, and opening the window checks nothing
    page(&mut h, true);
    assert!(has(&h, &format!("pluginUpdateCheck:{ID}")), "a signed, stamped plug-in with a feed has the box");
    finish(&mut h);
    assert!(!shown(&h, "Version 2.0.0 is available"), "{:?}", h.painted_text());

    // turned on, then the window opened again: it checks and shows the update from the feed
    let r = h.request("engine.execute", json!({"command": "plugin.setUpdateCheck", "params": {"id": ID, "on": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let _ = h.request("engine.execute", json!({"command": "plugins.manager", "params": {"open": false}}), T);
    h.settle(T);
    page(&mut h, true);
    finish(&mut h);
    assert!(shown(&h, "Version 2.0.0 is available"), "{:?}", h.painted_text());
    assert!(shown(&h, "Writes <b>captions</b> now."), "the feed's note: {:?}", h.painted_text());
    assert!(shown(&h, "Change the crop of photos"), "the group its feed declares: {:?}", h.painted_text());
    assert!(has(&h, &format!("link:plugin-updateChangelog-{ID}")));
    assert_eq!(version().as_deref(), Some("1.0.0"), "nothing installs by itself");

    // Update: the file asks for a group the feed didn't declare, so it asks first
    assert!(h.click_as_user(&format!("button:pluginUpdate:{ID}")));
    finish(&mut h);
    assert!(shown(&h, "It asks for more than its update feed said:") && shown(&h, "Write photo metadata"), "{:?}", h.painted_text());
    assert_eq!(version().as_deref(), Some("1.0.0"), "nothing installed before the answer");
    assert!(h.click_as_user("button:pluginUpdateAnyway"));
    finish(&mut h);
    assert_eq!(version().as_deref(), Some("2.0.0"));
    let granted = lcc_engine::cmd::plugins::manager().as_ref().unwrap().get(ID).unwrap().effective().metadata_write;
    assert!(!granted, "new permissions aren't granted");
    assert!(!shown(&h, "Version 2.0.0 is available"), "the offer goes once installed: {:?}", h.painted_text());
}
