//! Plug-in permissions in the UI: the install dialog only lists what a plug-in may ask for, a
//! command the user starts asks for a permission it needs (Allow runs it again, Deny is
//! remembered), and only the user's own clicks allow: the control channel's are ignored, and say
//! so.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use lcc_ui_egui::headless::Headless;
use lcc_ui_egui::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn app() -> Headless {
    // install into memory, never into the real user's settings folder
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let services = Services { png: None, ..Default::default() };
    lcc_engine::scene_disk::use_build_dir();
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    for _ in 0..4 {
        h.step();
    }
    r["result"].clone()
}

/// A click through the control channel (`ui.clickWidget`): an agent's or a script's.
fn agent_click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "ui.clickWidget {id}: {r}");
    h.settle(T);
}

fn has_widget(h: &mut Headless, id: &str) -> bool {
    let w = h.request("ui.widgets", json!({}), T);
    w["result"].as_array().into_iter().flatten().any(|x| x["id"] == id)
}

fn widgets(h: &mut Headless) -> Vec<String> {
    let w = h.request("ui.widgets", json!({}), T);
    w["result"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str().map(str::to_string)).collect()
}

fn painted(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(text))
}

fn state(h: &mut Headless, id: &str, group: &str) -> Value {
    let list = exec(h, "plugin.list", json!({}));
    let p = list["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap_or(Value::Null);
    p["requests"].as_array().into_iter().flatten().find(|r| r["group"] == group).cloned().unwrap_or(Value::Null)
}

fn folder(name: &str) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("plugin-permissions-ui-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A plug-in `id` asking for `catalog` and `metadataWrite` whose command `go` sends `request`
/// and returns the reply, written to `dir/<id>.wasm`.
fn module_file(dir: &Path, id: &str, request: &str) -> String {
    let manifest = json!({"id": id, "name": "Asker", "version": "1.0.0", "permissions": {"catalog": true, "metadataWrite": true},
        "commands": [{"id": "go", "label": "Go"}]})
    .to_string();
    let esc = |s: &str| s.replace('"', "\\\"");
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (local $n i32)
    (local.set $n (call $call (i32.const 8192) (i32.const {rl})))
    (call $response (i32.const 16384))
    (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 16384))))"#,
        m = esc(&manifest),
        r = esc(request),
        ml = manifest.len(),
        rl = request.len()
    );
    let path = dir.join(format!("{id}.wasm"));
    std::fs::write(&path, wat::parse_str(&src).unwrap()).unwrap();
    path.display().to_string()
}

/// AC-4: the install dialog lists what the plug-in may ask for, with nothing to tick, and a fresh
/// install has nothing granted.
#[test]
fn the_install_dialog_only_lists_the_permissions() {
    let _turn = turn();
    let dir = folder("install");
    let id = "test.perm.install";
    let path = module_file(&dir, id, r#"{"method":"catalog.stats","params":{}}"#);
    let mut h = app();
    exec(&mut h, "plugins.install", json!({"path": path}));
    assert!(has_widget(&mut h, "button:pluginInstall"), "the dialog opened");
    assert!(
        painted(&h, "It may ask for these permissions") && painted(&h, "Read the catalog") && painted(&h, "Write photo metadata"),
        "{:?}",
        h.painted_text()
    );
    let ticks: Vec<String> = widgets(&mut h).into_iter().filter(|w| w.contains("ermission") || w.contains("checkbox")).collect();
    assert!(ticks.is_empty(), "nothing to tick: {ticks:?}");
    assert!(h.click_as_user("button:pluginInstall"), "Install on screen");
    assert_eq!(state(&mut h, id, "catalog")["state"], "never", "a fresh install has nothing granted");
    assert_eq!(state(&mut h, id, "metadataWrite")["state"], "never");
    exec(&mut h, "plugin.uninstall", json!({"id": id}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC-3: on the Permissions tab an agent's click on Allow changes nothing and says why; the
/// user's allows; an agent's Revoke works.
#[test]
fn only_the_users_click_allows_on_the_permissions_tab() {
    let _turn = turn();
    let dir = folder("tab");
    let id = "test.perm.tab";
    let path = module_file(&dir, id, r#"{"method":"catalog.stats","params":{}}"#);
    let mut h = app();
    exec(&mut h, "plugin.install", json!({"path": path}));
    exec(&mut h, "plugins.page", json!({"id": id, "tab": "permissions"}));
    assert!(painted(&h, "not asked yet"), "{:?}", h.painted_text());
    let allow = format!("pluginAllow:{id}:0");
    agent_click(&mut h, &allow);
    assert_eq!(state(&mut h, id, "catalog")["state"], "never", "an agent's click allows nothing");
    assert!(painted(&h, "Only you can allow a permission"), "it says why: {:?}", h.painted_text());
    assert!(h.click_as_user(&allow));
    assert_eq!(state(&mut h, id, "catalog")["state"], "allowed", "the user's click allows");
    agent_click(&mut h, &format!("pluginRevoke:{id}:0"));
    assert_eq!(state(&mut h, id, "catalog")["state"], "never", "revoking works from the control channel");
    exec(&mut h, "plugins.manager", json!({"open": false}));
    exec(&mut h, "plugin.uninstall", json!({"id": id}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC-6, AC-7: a command the user starts that needs a permission they were never asked about
/// shows a prompt; an agent's Allow is ignored, the user's grants it and runs the command again.
/// Deny is remembered: the tab shows the date, and the next run asks nothing.
#[test]
fn a_command_asks_for_what_it_needs_and_remembers_a_no() {
    let _turn = turn();
    let dir = folder("ask");
    let id = "test.perm.ask";
    let mut h = app();
    let photo = h.app.session.visible_cloned()[0].0;
    let request = json!({"method": "metadata.setStandard", "params": {"ids": [photo.to_string()], "title": "Asked for"}}).to_string();
    let path = module_file(&dir, id, &request);
    exec(&mut h, "plugin.install", json!({"path": path}));
    let run = |h: &mut Headless| {
        let _ = h.request("engine.execute", json!({"command": "plugins.runItem", "params": {"plugin": id, "command": "go"}}), T);
        h.settle(T);
    };
    run(&mut h);
    assert!(has_widget(&mut h, "button:pluginAskAllow"), "the prompt is up");
    assert!(painted(&h, "Asker asks to: Write photo metadata"), "it names the plug-in and the permission: {:?}", h.painted_text());
    agent_click(&mut h, "button:pluginAskAllow");
    assert!(has_widget(&mut h, "button:pluginAskAllow"), "an agent's Allow leaves the prompt up");
    assert_eq!(state(&mut h, id, "metadataWrite")["state"], "never", "and allows nothing");
    assert!(h.click_as_user("button:pluginAskAllow"));
    assert!(!has_widget(&mut h, "button:pluginAskAllow"), "answered");
    assert_eq!(state(&mut h, id, "metadataWrite")["state"], "allowed");
    let meta = exec(&mut h, "photo.inspect", json!({"id": photo}));
    assert_eq!(meta["meta"]["title"], "Asked for", "Allow ran the command again");
    // revoked, it asks again; Deny is remembered
    exec(&mut h, "plugin.revoke", json!({"id": id, "group": "metadataWrite"}));
    run(&mut h);
    assert!(has_widget(&mut h, "button:pluginAskDeny"), "asked again after a revoke");
    agent_click(&mut h, "button:pluginAskDeny");
    assert!(!has_widget(&mut h, "button:pluginAskDeny"), "answered");
    let denied = state(&mut h, id, "metadataWrite");
    assert_eq!(denied["state"], "denied", "{denied}");
    exec(&mut h, "plugins.page", json!({"id": id, "tab": "permissions"}));
    assert!(painted(&h, "denied on"), "the tab shows the date: {:?}", h.painted_text());
    assert!(has_widget(&mut h, &format!("pluginAllow:{id}:1")), "with Allow");
    exec(&mut h, "plugins.manager", json!({"open": false}));
    run(&mut h);
    assert!(!has_widget(&mut h, "button:pluginAskAllow"), "a denied permission isn't asked again");
    exec(&mut h, "plugin.uninstall", json!({"id": id}));
    let _ = std::fs::remove_dir_all(&dir);
}
