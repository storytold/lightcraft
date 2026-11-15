//! The Install Plug-in dialog: a plug-in this version can't run offers no Install button, and an
//! older version than the installed one is installed only through *Install Older Version*; a
//! plug-in that didn't load can be removed from its page in the Plug-in Manager; a stamped manifest
//! section that differs from `lcc_manifest` opens no dialog.
//!
//! Its own test binary: the Plug-in Manager's window state is the process's, and other UI tests
//! drawing frames at the same time would race it.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use lcc_ui_egui::headless::Headless;
use lcc_ui_egui::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn app() -> Headless {
    // install into memory, never into the real user's settings folder
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let services = Services { png: None, ..Default::default() };
    lcc_engine::scene_disk::use_build_dir();
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    Headless::new(app, [1200.0, 800.0], 1.0)
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// A plug-in with `manifest` that answers every call with `{}`, written to `dir/<name>.wasm`.
fn module_file(dir: &Path, name: &str, manifest: &Value) -> String {
    stamped_file(dir, name, manifest, None)
}

/// [`module_file`], stamped with `section` as its manifest section (true or not).
fn stamped_file(dir: &Path, name: &str, manifest: &Value, section: Option<&Value>) -> String {
    let manifest = manifest.to_string();
    let esc = |s: &str| s.replace('"', "\\\"");
    let section = section.map_or_else(String::new, |v| format!("(@custom \"{}\" \"{}\")", lcc_plugins::MANIFEST_SECTION, esc(&v.to_string())));
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192)))
  {section})"#,
        m = esc(&manifest),
        ml = manifest.len()
    );
    let path = dir.join(format!("{name}.wasm"));
    std::fs::write(&path, wat::parse_str(&src).unwrap()).unwrap();
    path.display().to_string()
}

fn has_widget(h: &mut Headless, id: &str) -> bool {
    let w = h.request("ui.widgets", json!({}), T);
    w["result"].as_array().into_iter().flatten().any(|x| x["id"] == id)
}

fn installed_version(h: &mut Headless, id: &str) -> Option<Value> {
    let l = exec(h, "plugin.list", json!({}));
    l["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).map(|p| p["version"].clone())
}

/// Clicks a widget and draws until the click has landed (press and release take frames).
fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

/// Opens the dialog for `path` (as File ▸ Plug-in Extras ▸ Install Plug-in… does) and draws it.
fn open_dialog(h: &mut Headless, path: &str) {
    exec(h, "plugins.install", json!({"path": path}));
    h.step();
    h.step();
}

fn folder() -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new("plugin-install-ui");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_dialog_refuses_what_this_version_cant_run_and_confirms_downgrades() {
    let dir = folder();
    let path = module_file(&dir, "needs", &json!({"id": "test.uineeds", "name": "Needs", "version": "1.0.0", "requires": ["test-missing-feature"]}));
    let mut h = app();
    open_dialog(&mut h, &path);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("doesn't have")), "the dialog says why: {text:?}");
    assert!(text.iter().any(|t| t.contains("test-missing-feature")), "and names the feature: {text:?}");
    assert!(!has_widget(&mut h, "button:pluginInstall"), "no Install button");
    click(&mut h, "button:pluginInstallCancel");
    assert!(installed_version(&mut h, "test.uineeds").is_none(), "nothing installed");

    // an older version installs only through Install Older Version (one test: the dialog's
    // state is the process's)
    let v2 = module_file(&dir, "v2", &json!({"id": "test.uiold", "name": "Old", "version": "2.0.0"}));
    let v1 = module_file(&dir, "v1", &json!({"id": "test.uiold", "name": "Old", "version": "1.0.0"}));
    exec(&mut h, "plugin.install", json!({"path": v2}));

    // Cancel installs nothing
    open_dialog(&mut h, &v1);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("1.0.0") && t.contains("2.0.0")), "both versions shown: {text:?}");
    assert!(text.iter().any(|t| t == "Install Older Version"), "{text:?}");
    click(&mut h, "button:pluginInstallCancel");
    assert_eq!(installed_version(&mut h, "test.uiold"), Some(json!("2.0.0")));

    // the button confirms the downgrade
    open_dialog(&mut h, &v1);
    click(&mut h, "button:pluginInstall");
    assert_eq!(installed_version(&mut h, "test.uiold"), Some(json!("1.0.0")));
    exec(&mut h, "plugin.uninstall", json!({"id": "test.uiold"}));

    // a plug-in that didn't load is listed with its id and a Remove button that removes it
    let failed = lcc_plugins::Failed {
        file: "test.uifailed.wasm".into(),
        error: "the plug-in needs `test-missing-feature`".into(),
        missing: vec!["test-missing-feature".into()],
        id: "test.uifailed".into(),
        path: None,
    };
    lcc_engine::cmd::plugins::manager().as_mut().unwrap().failed.push(failed);
    let row = |h: &mut Headless| exec(h, "plugin.list", json!({}))["failed"].as_array().unwrap().iter().find(|f| f["id"] == "test.uifailed").cloned();
    assert_eq!(row(&mut h).unwrap()["missing"], json!(["test-missing-feature"]));
    exec(&mut h, "plugins.manager", json!({"open": true}));
    h.step();
    h.step();
    click(&mut h, "pluginEntry:test.uifailed");
    click(&mut h, "button:pluginRemoveFailed:test.uifailed");
    click(&mut h, "button:pluginRemoveConfirm:test.uifailed");
    assert!(row(&mut h).is_none(), "removed");
    assert!(
        h.request("engine.execute", json!({"command": "plugin.uninstall", "params": {"id": "test.uifailed"}}), T)["ok"] == false,
        "gone for good"
    );

    // fields this version doesn't know are named, and it still installs
    let odd = module_file(&dir, "odd", &json!({"id": "test.uiodd", "name": "Odd", "version": "1.0.0", "hooks": {"exprt": true}}));
    open_dialog(&mut h, &odd);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("fields this version doesn't know") && t.contains("hooks.exprt")), "{text:?}");
    assert!(has_widget(&mut h, "button:pluginInstall"), "it still installs");
    click(&mut h, "button:pluginInstallCancel");

    // a value it can't use (a link that isn't https://) is named with why, and it still installs
    let link = module_file(&dir, "link", &json!({"id": "test.uilink", "name": "Link", "version": "1.0.0", "homepage": "http://example.com"}));
    open_dialog(&mut h, &link);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("values this version can't use") && t.contains("homepage: not an https:// link")), "{text:?}");
    assert!(has_widget(&mut h, "button:pluginInstall"), "it still installs");
    click(&mut h, "button:pluginInstallCancel");

    // the dialog lists lcc_manifest's permissions: a stamped section that hides one opens no dialog
    let asks = json!({"id": "test.uistamp", "name": "Stamp", "version": "1.0.0", "permissions": {"catalog": true}});
    let claims = json!({"id": "test.uistamp", "name": "Stamp", "version": "1.0.0"});
    let lying = stamped_file(&dir, "lying", &asks, Some(&claims));
    let r = h.request("engine.execute", json!({"command": "plugins.install", "params": {"path": lying}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("`permissions`"), "it says which fields differ: {r}");
    h.step();
    assert!(!has_widget(&mut h, "button:pluginInstall"), "no dialog to approve");
    assert!(installed_version(&mut h, "test.uistamp").is_none(), "nothing installed");
    // a true section opens the dialog as an unstamped file does
    let honest = stamped_file(&dir, "honest", &asks, Some(&asks));
    open_dialog(&mut h, &honest);
    assert!(has_widget(&mut h, "button:pluginInstall"), "a true section installs");
    click(&mut h, "button:pluginInstallCancel");
    let _ = std::fs::remove_dir_all(&dir);
}
