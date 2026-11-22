//! The Plug-in Marketplace window: opening it fetches the list; community entries show only when
//! asked for, with a warning; a page says which platforms an entry supports and offers nothing
//! where it doesn't; Install, Update and Remove work from the page; the automatic pass updates an
//! installed plug-in by itself only when it is turned on.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::marketplace as window;
use crate::tests_plugin_updates::{FETCHER_TURN, Served};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);
const TOOLS: &str = "test.uimarket.tools";
const LOOK: &str = "test.uimarket.look";
const ELSEWHERE: &str = "test.uimarket.elsewhere";

/// A plug-in module `id` at `version` that answers every call with `{}`.
fn plugin(id: &str, version: &str) -> Vec<u8> {
    let manifest =
        json!({"id": id, "name": "Market Tools", "version": version, "permissions": {"catalog": true, "network": ["api.example.org"]}}).to_string();
    let esc = manifest.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{esc}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192))))"#,
        ml = manifest.len(),
    );
    wat::parse_str(&src).unwrap()
}

struct Market {
    served: Arc<Served>,
    entries: Vec<Value>,
}

impl Market {
    fn list(&mut self, id: &str, version: &str, tier: &str, bytes: &[u8], more: Value) {
        let url = format!("https://releases.test/{id}/{version}/module.wasm");
        self.served.0.lock().unwrap().insert(url.clone(), bytes.to_vec());
        let mut e = json!({"id": id, "name": format!("Name of {id}"), "version": version, "kind": "plugin", "tier": tier, "url": url,
            "sha256": lcc_plugins::marketplace::sha256_hex(bytes), "size": bytes.len(), "summary": "Does a thing", "author": "Ann",
            "platforms": ["macos", "windows", "linux", "android"], "license": "MIT", "homepage": "https://example.org/tools"});
        for (k, v) in more.as_object().into_iter().flatten() {
            e[k] = v.clone();
        }
        self.entries.retain(|o| o["id"] != id);
        self.entries.push(e);
        let index = json!({"format": 1, "generated": "2026-11-23T04:17:09Z", "source": "https://market.test", "plugins": self.entries});
        self.served.0.lock().unwrap().insert(lcc_engine::cmd::marketplace::index_url().unwrap(), index.to_string().into_bytes());
    }
}

/// Puts the manager's fetcher back on the network and removes what the test installed, however
/// it ends.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        window::LIVE.set(false);
        if let Some(m) = lcc_engine::cmd::plugins::manager().as_mut() {
            m.set_fetcher(Arc::new(lcc_plugins::updates::Net));
            let _ = m.uninstall(TOOLS);
        }
        lcc_engine::cmd::marketplace::reset_for_test();
    }
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn shown(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(text))
}

fn version() -> Option<String> {
    let m = lcc_engine::cmd::plugins::manager();
    Some(m.as_ref()?.get(TOOLS)?.plugin.manifest().version.clone())
}

/// Runs frames until the marketplace's worker is done and its result is on screen.
fn finish(h: &mut Headless) {
    assert!(h.step_until(T, |_| !window::running()), "the marketplace worker finished");
    h.settle(T);
}

fn open(h: &mut Headless, open: bool, id: Option<&str>) {
    let mut p = json!({"open": open});
    if let Some(id) = id {
        p["id"] = json!(id);
    }
    let r = h.request("engine.execute", json!({"command": "plugins.marketplace", "params": p}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    finish(h);
}

#[test]
fn the_marketplace_lists_installs_updates_and_removes() {
    let _turn = FETCHER_TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    lcc_engine::cmd::plugins::use_in_memory_manager();
    lcc_engine::cmd::marketplace::reset_for_test();
    let mut market = Market { served: Arc::new(Served::default()), entries: Vec::new() };
    market.list(
        TOOLS,
        "1.0.0",
        "vetted",
        &plugin(TOOLS, "1.0.0"),
        json!({"permissions": ["catalog", "network"], "network": ["api.example.org"],
        "review": {"by": "bob", "date": "2026-11-01"}}),
    );
    market.list(LOOK, "1.0.0", "community", &plugin(LOOK, "1.0.0"), json!({}));
    market.list(ELSEWHERE, "1.0.0", "community", &plugin(ELSEWHERE, "1.0.0"), json!({"platforms": ["web"]}));
    let _restore = Restore;
    lcc_engine::cmd::plugins::manager().as_mut().unwrap().set_fetcher(market.served.clone());
    window::LIVE.set(true);
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(T);

    // opening fetches the list; only the vetted entry shows, and the page says who reviewed it
    open(&mut h, true, None);
    assert!(has(&h, &format!("marketplaceEntry:{TOOLS}")) && !has(&h, &format!("marketplaceEntry:{LOOK}")), "{:?}", h.painted_text());
    assert!(shown(&h, "2 community plug-ins aren't shown."), "{:?}", h.painted_text());
    assert!(shown(&h, "Reviewed by bob, a marketplace maintainer, on 2026-11-01."), "{:?}", h.painted_text());
    assert!(shown(&h, "Connect to the network: api.example.org"), "what it may ask for, before anything is downloaded: {:?}", h.painted_text());
    for p in ["macos", "windows", "linux", "android"] {
        assert!(has(&h, &format!("marketplacePlatform:{TOOLS}:{p}:yes")), "{p}");
    }

    // community entries on request, with a warning; one for another platform can't be installed
    assert!(h.click_as_user("marketplaceCommunity"));
    assert!(has(&h, &format!("marketplaceEntry:{LOOK}")) && shown(&h, "Community plug-ins passed automatic checks only"), "{:?}", h.painted_text());
    assert!(h.click_as_user(&format!("marketplaceEntry:{ELSEWHERE}")));
    let here = lcc_plugins::marketplace::platform();
    assert!(has(&h, &format!("marketplacePlatform:{ELSEWHERE}:{here}:no")), "{:?}", h.app.widgets.iter().map(|(w, _)| w).collect::<Vec<_>>());
    assert!(shown(&h, "It isn't available for") && shown(&h, "Passed automatic checks only."), "{:?}", h.painted_text());
    assert!(h.click_as_user(&format!("button:marketplaceInstall:{ELSEWHERE}")));
    finish(&mut h);
    assert!(lcc_engine::cmd::plugins::manager().as_ref().unwrap().get(ELSEWHERE).is_none(), "the button is off there");

    // search narrows the list
    let r = h.request("engine.execute", json!({"command": "plugins.marketplace", "params": {"open": true, "query": "uimarket.tools"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert!(has(&h, &format!("marketplaceEntry:{TOOLS}")) && !has(&h, &format!("marketplaceEntry:{LOOK}")));
    let _ = h.request("engine.execute", json!({"command": "plugins.marketplace", "params": {"open": true, "query": ""}}), T);
    h.settle(T);

    // Install: downloaded on the worker, installed with nothing granted
    assert!(h.click_as_user(&format!("marketplaceEntry:{TOOLS}")));
    assert!(h.click_as_user(&format!("button:marketplaceInstall:{TOOLS}")));
    finish(&mut h);
    assert_eq!(version().as_deref(), Some("1.0.0"), "{:?}", h.painted_text());
    let granted = lcc_engine::cmd::plugins::manager().as_ref().unwrap().get(TOOLS).unwrap().effective();
    assert!(granted.is_empty(), "installing grants nothing: {granted:?}");
    assert!(shown(&h, "Version 1.0.0 is installed") && has(&h, &format!("button:marketplaceRemove:{TOOLS}")), "{:?}", h.painted_text());

    // a new release is listed: the next opening offers it, and nothing installs by itself
    market.list(TOOLS, "1.1.0", "vetted", &plugin(TOOLS, "1.1.0"), json!({}));
    open(&mut h, false, None);
    open(&mut h, true, Some(TOOLS));
    assert_eq!(version().as_deref(), Some("1.0.0"));
    assert!(has(&h, "button:marketplaceUpdateAll") && has(&h, &format!("button:marketplaceUpdate:{TOOLS}")), "{:?}", h.painted_text());
    assert!(h.click_as_user(&format!("button:marketplaceUpdate:{TOOLS}")));
    finish(&mut h);
    assert_eq!(version().as_deref(), Some("1.1.0"));
    assert!(!has(&h, &format!("button:marketplaceUpdate:{TOOLS}")));

    // automatic updates: off, a newer release waits; on, the pass installs it by itself
    market.list(TOOLS, "1.2.0", "vetted", &plugin(TOOLS, "1.2.0"), json!({}));
    open(&mut h, false, None);
    window::look_again();
    h.settle(T);
    finish(&mut h);
    assert_eq!(version().as_deref(), Some("1.1.0"), "off by default");
    open(&mut h, true, Some(TOOLS));
    assert!(h.click_as_user("marketplaceAutoUpdate"));
    open(&mut h, false, None);
    window::look_again();
    h.settle(T);
    finish(&mut h);
    assert_eq!(version().as_deref(), Some("1.2.0"), "{:?}", h.painted_text());

    // Remove asks first
    open(&mut h, true, Some(TOOLS));
    assert!(h.click_as_user(&format!("button:marketplaceRemove:{TOOLS}")));
    assert!(shown(&h, "Remove Name of test.uimarket.tools?"), "{:?}", h.painted_text());
    assert_eq!(version().as_deref(), Some("1.2.0"), "nothing removed before the answer");
    assert!(h.click_as_user(&format!("button:marketplaceRemoveConfirm:{TOOLS}")));
    h.settle(T);
    assert_eq!(version(), None);
    assert!(has(&h, &format!("button:marketplaceInstall:{TOOLS}")));
}
