//! The SDK example plug-in (`sdk/examples/catalog-tools`), built to wasm32 by this test, installed
//! in a Plugin Manager and driven through every hook with a fake catalog.

use std::path::{Path, PathBuf};
use std::process::Command;

use dac_plugins::publish::{PluginPublisher, PublishItem, PublishProvider};
use dac_plugins::{HostApi, Manager, Permissions};
use serde_json::{Value, json};

struct FakeCatalog {
    calls: Vec<String>,
}

impl HostApi for FakeCatalog {
    fn catalog(&mut self, method: &str, _: &Value) -> Result<Value, String> {
        self.calls.push(method.into());
        Ok(json!({"photos": 42}))
    }
}

fn wasm32_installed() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    Command::new(rustc)
        .args(["--print", "target-libdir", "--target", "wasm32-unknown-unknown"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| Path::new(String::from_utf8_lossy(&o.stdout).trim()).exists())
}

/// Builds the example; `None` (test skipped) without the wasm32 target.
fn build_example() -> Option<Vec<u8>> {
    if !wasm32_installed() {
        eprintln!("skipped: the wasm32-unknown-unknown target is not installed (rustup target add wasm32-unknown-unknown)");
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/examples/catalog-tools");
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugin-example");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(&root)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .unwrap();
    assert!(status.success(), "building the example plug-in failed");
    Some(std::fs::read(target.join("wasm32-unknown-unknown/release/catalog_tools.wasm")).unwrap())
}

fn temp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugins-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn example_plugin_end_to_end() {
    let Some(wasm) = build_example() else { return };
    let dir = temp("example");
    let mut m = Manager::open(&dir).unwrap();
    let info = m.install_bytes(&wasm, Some(Permissions { catalog: true, ..Default::default() })).unwrap().info();
    assert_eq!(info["id"], "org.example.catalog-tools");
    assert_eq!(info["granted"]["catalog"], true);
    let id = "org.example.catalog-tools";
    let mut host = FakeCatalog { calls: vec![] };

    // menu command with its dialog defaults
    let r = m.run_command(id, "count", &json!({}), &[], &mut host).unwrap();
    assert_eq!(r["message"], "Photos: 42");
    assert_eq!(host.calls, vec!["catalog.stats"]);
    let r = m.run_command(id, "count", &json!({"label": "All"}), &[], &mut host).unwrap();
    assert_eq!(r["message"], "All: 42");
    assert!(m.get(id).unwrap().log.iter().any(|l| l.contains("counted photos for All")), "{:?}", m.get(id).unwrap().log);

    // own namespace
    let r = m.run_command(id, "mark", &json!({}), &["p1".into(), "p2".into()], &mut host).unwrap();
    assert_eq!(r["marked"], 2);

    // metadata provider
    let s = m.suggest_metadata(&[json!({"id": "p1", "fileName": "/x/Beach_sunset.jpg"})], &mut host);
    assert_eq!(s[0]["ok"]["suggestions"][0]["keywords"], json!(["beach", "sunset"]));

    // export hook
    let r = m.post_export(&[json!({"path": dir.join("out.jpg").display().to_string(), "photo": "p1"})], &mut host);
    assert_eq!(r[0]["ok"]["files"], 1, "{r:?}");

    // publish service through the trait boundary
    let mut publisher = PluginPublisher::new(&mut m, id).unwrap();
    assert_eq!(publisher.label(), "Example Folder");
    let item = PublishItem { path: "/tmp/a.jpg".into(), photo: "p1".into(), ..Default::default() };
    let r = publisher.upload(&mut host, &item).unwrap();
    assert_eq!(r.remote_id, "p1");

    // namespace data persisted; grants and enabled state survive a restart
    let data: Value = serde_json::from_slice(&std::fs::read(dir.join("data").join(format!("{id}.json"))).unwrap()).unwrap();
    assert_eq!(data["p1"]["marked"], true);
    assert_eq!(data["p1"]["lastExport"], dir.join("out.jpg").display().to_string());

    // revoking the catalog permission takes effect on the next call
    m.set_grant(id, Permissions::default()).unwrap();
    let e = m.run_command(id, "count", &json!({}), &[], &mut host).unwrap_err();
    assert!(e.to_string().contains("permission `catalog` not granted"), "{e}");
    drop(m);
    let mut m = Manager::open(&dir).unwrap();
    assert!(m.get(id).unwrap().enabled);
    assert!(m.get(id).unwrap().effective().is_empty());
    m.set_enabled(id, false).unwrap();
    assert!(m.run_command(id, "mark", &json!({}), &[], &mut host).is_err());
    m.uninstall(id).unwrap();
    assert!(!dir.join(format!("{id}.wasm")).exists());
    let _ = std::fs::remove_dir_all(&dir);
}
