//! The SDK example plug-in (`sdk/examples/catalog-tools`), built to wasm32 by this test, installed
//! in a Plugin Manager and driven through every hook with a fake catalog.

use std::path::{Path, PathBuf};
use std::process::Command;

use lcc_plugins::publish::{PluginPublisher, PublishItem, PublishProvider};
use lcc_plugins::{HostApi, Manager, Permissions};
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

/// Builds the catalog-tools example; `None` (test skipped) without the wasm32 target.
fn build_example() -> Option<Vec<u8>> {
    build_sdk_example("catalog-tools")
}

/// Builds `sdk/examples/<name>` to wasm32; `None` (test skipped) without the wasm32 target.
fn build_sdk_example(name: &str) -> Option<Vec<u8>> {
    if !wasm32_installed() {
        eprintln!("skipped: the wasm32-unknown-unknown target is not installed (rustup target add wasm32-unknown-unknown)");
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/examples").join(name);
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-example-{name}"));
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
    Some(std::fs::read(target.join("wasm32-unknown-unknown/release").join(format!("{}.wasm", name.replace('-', "_")))).unwrap())
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

    // namespace data persisted (per library; none open here: the unnamed one); grants and enabled state survive a restart
    let data: Value = serde_json::from_slice(&std::fs::read(dir.join("data").join(format!("{id}.json"))).unwrap()).unwrap();
    assert_eq!(data["libraries"][""]["p1"]["marked"], true);
    assert_eq!(data["libraries"][""]["p1"]["lastExport"], dir.join("out.jpg").display().to_string());

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

/// A catalog of photos with numeric-string ids that records metadata writes.
struct FakeLibrary {
    writes: Vec<Value>,
}

impl HostApi for FakeLibrary {
    fn catalog(&mut self, method: &str, p: &Value) -> Result<Value, String> {
        match method {
            "photo.inspect" => {
                let id = p["id"].as_str().ok_or("id")?;
                Ok(json!({"id": id, "file_name": format!("IMG_{id}.JPG"), "captured": "2026-09-20T05:40:00", "meta": {"camera": "X2"}}))
            }
            other => Err(format!("unexpected {other}")),
        }
    }
    fn set_metadata(&mut self, p: &Value) -> Result<Value, String> {
        self.writes.push(p.clone());
        Ok(Value::Null)
    }
}

#[test]
fn batch_metadata_example_end_to_end() {
    let Some(wasm) = build_sdk_example("batch-metadata") else { return };
    let dir = temp("batch-metadata");
    let mut m = Manager::open(&dir).unwrap();
    let id = "org.example.batch-metadata";
    // installed with read access only: the write is refused by the host
    m.install_bytes(&wasm, Some(Permissions { catalog: true, ..Default::default() })).unwrap();
    let mut host = FakeLibrary { writes: vec![] };
    let sel: Vec<String> = vec!["7".into(), "8".into(), "9".into()];
    let e = m.run_command(id, "template", &json!({}), &sel, &mut host).unwrap_err();
    assert!(e.to_string().contains("metadataWrite"), "{e}");
    assert!(host.writes.is_empty());
    m.set_grant(id, Permissions { catalog: true, metadata_write: true, ..Default::default() }).unwrap();

    // titles from the dialog's default template; captions from a numbered one
    let r = m.run_command(id, "template", &json!({}), &sel, &mut host).unwrap();
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(host.writes[0], json!({"ids": ["7"], "title": "IMG_7 · 2026-09-20"}));
    host.writes.clear();
    let r = m.run_command(id, "template", &json!({"field": "caption", "template": "{camera} #{n}", "start": 10}), &sel, &mut host).unwrap();
    assert_eq!(r["changed"], 3);
    let captions: Vec<&Value> = host.writes.iter().map(|w| &w["caption"]).collect();
    assert_eq!(captions, [&json!("X2 #10"), &json!("X2 #11"), &json!("X2 #12")]);
    // the host checks dialog values against the manifest before the plug-in sees them
    assert!(m.run_command(id, "template", &json!({"field": "rating"}), &sel, &mut host).is_err());

    // keywords: one write for the whole selection, trimmed and de-duplicated
    host.writes.clear();
    m.run_command(id, "keywords", &json!({"keywords": "beach, Sunset, beach"}), &sel, &mut host).unwrap();
    assert_eq!(host.writes, vec![json!({"ids": ["7", "8", "9"], "addKeywords": ["beach", "Sunset"]})]);
    assert!(m.run_command(id, "keywords", &json!({"keywords": " , "}), &sel, &mut host).is_err());
    assert!(m.run_command(id, "keywords", &json!({"keywords": "x"}), &[], &mut host).is_err(), "nothing selected");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_tools_example_hook_and_destination() {
    let Some(wasm) = build_sdk_example("export-tools") else { return };
    let dir = temp("export-tools");
    let mut m = Manager::open(&dir).unwrap();
    let id = "org.example.export-tools";
    m.install_bytes(&wasm, None).unwrap();
    let file = dir.join("out.jpg");
    std::fs::write(&file, b"123456789").unwrap();
    let files = [json!({"path": file.display().to_string(), "photo": "5"})];
    let mut host = lcc_plugins::NoHost;

    // post-process hook: reads the lent (binary) file, records its checksum
    let r = m.post_export(&files, &mut host);
    assert_eq!(r[0]["ok"]["checksums"][0]["crc32"], "cbf43926", "{r:?}");
    let r = m.export_to(id, "webdav", &json!({"dryRun": true}), &[], &mut host).unwrap();
    assert_eq!(r["uploaded"], json!([]), "no files, nothing sent");

    // the destination: dry run lists the uploads; dialog values are checked first
    assert_eq!(m.export_destinations().len(), 1);
    let r = m.export_to(id, "webdav", &json!({"url": "https://dav.example.com/p/", "dryRun": true}), &files, &mut host).unwrap();
    assert_eq!(r["uploaded"][0]["url"], "https://dav.example.com/p/out.jpg", "{r}");
    assert!(m.export_to(id, "webdav", &json!({"dryRun": "yes"}), &files, &mut host).is_err());
    assert!(m.export_to(id, "nope", &json!({}), &files, &mut host).is_err());
    assert!(m.export_to(id, "webdav", &json!({"url": "ftp://x"}), &files, &mut host).is_err());
    // without the network grant, a real upload is refused by the host
    let e = m.export_to(id, "webdav", &json!({"url": "https://dav.example.com/p"}), &files, &mut host).unwrap_err();
    assert!(e.to_string().contains("network"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn publish_folder_example_publishes_and_removes() {
    let Some(wasm) = build_sdk_example("publish-folder") else { return };
    let dir = temp("publish-folder");
    let root = Path::new("/tmp/lcc-gallery");
    let mut m = Manager::open(&dir).unwrap();
    let id = "org.example.publish-folder";
    let grant = Permissions { fs: vec![lcc_plugins::FsRoot { path: root.display().to_string(), write: true }], ..Default::default() };
    m.install_bytes(&wasm, Some(grant)).unwrap();
    std::fs::create_dir_all(root).unwrap();
    let src = dir.join("render.jpg");
    std::fs::write(&src, [0u8, 1, 2, 255]).unwrap();
    let mut host = lcc_plugins::NoHost;
    let mut p = PluginPublisher::new(&mut m, id).unwrap();
    assert_eq!(p.label(), "Local Gallery");
    let item = PublishItem { path: src.display().to_string(), photo: format!("g{}", std::process::id()), title: "T".into(), ..Default::default() };
    let r = p.upload(&mut host, &item).unwrap();
    let published = root.join(&r.remote_id);
    assert_eq!(std::fs::read(&published).unwrap(), [0u8, 1, 2, 255], "binary copy");
    let index: Value = serde_json::from_str(&std::fs::read_to_string(root.join("index.json")).unwrap()).unwrap();
    assert_eq!(index[&r.remote_id]["title"], "T");
    p.delete(&mut host, &r.remote_id).unwrap();
    assert!(!published.exists());
    // a hostile remote id can't reach outside the folder
    assert!(p.delete(&mut host, "..").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn custom_fields_example_shows_and_edits_fields() {
    let Some(wasm) = build_sdk_example("custom-fields") else { return };
    let dir = temp("custom-fields");
    let mut m = Manager::open(&dir).unwrap();
    let id = "org.example.custom-fields";
    m.install_bytes(&wasm, None).unwrap();
    let mut host = lcc_plugins::NoHost;
    let f = m.metadata_fields("3");
    assert_eq!(f[0]["fields"].as_array().map(Vec::len), Some(4), "{f:?}");
    assert!(f[0]["fields"].as_array().unwrap().iter().all(|x| x["value"].is_null()));

    m.run_command(id, "set", &json!({"client": "ACME", "job": "J-1", "usage": "Commercial"}), &["3".into()], &mut host).unwrap();
    let value = |f: &[Value], k: &str| f[0]["fields"].as_array().unwrap().iter().find(|x| x["key"] == k).unwrap()["value"].clone();
    let f = m.metadata_fields("3");
    assert_eq!(value(&f, "client"), "ACME");
    assert_eq!(value(&f, "usage"), "Commercial");

    // the user edits a field; read-only and invalid values are refused
    m.set_metadata_field(id, "3", "job", &json!("J-2")).unwrap();
    assert!(m.set_metadata_field(id, "3", "delivered", &json!(true)).is_err());
    assert!(m.set_metadata_field(id, "3", "usage", &json!("Stolen")).is_err());
    assert!(m.set_metadata_field(id, "3", "nope", &json!("x")).is_err());
    assert_eq!(value(&m.metadata_fields("3"), "job"), "J-2");

    // the export hook fills the read-only field; the provider suggests the client keyword
    m.post_export(&[json!({"path": dir.join("a.jpg").display().to_string(), "photo": "3"})], &mut host);
    assert_eq!(value(&m.metadata_fields("3"), "delivered"), true);
    let s = m.suggest_metadata(&[json!({"id": "3"})], &mut host);
    assert_eq!(s[0]["ok"]["suggestions"][0]["keywords"], json!(["ACME"]), "{s:?}");
    m.set_metadata_field(id, "3", "client", &Value::Null).unwrap();
    assert!(value(&m.metadata_fields("3"), "client").is_null());

    // photo 3 of another library has none of these values (data is kept per library)
    m.set_library("other-library");
    assert!(value(&m.metadata_fields("3"), "job").is_null());
    m.run_command(id, "set", &json!({"client": "B", "job": "B-1", "usage": "Commercial"}), &["3".into()], &mut host).unwrap();
    assert_eq!(value(&m.metadata_fields("3"), "job"), "B-1");
    m.set_library("");
    assert_eq!(value(&m.metadata_fields("3"), "job"), "J-2");

    // survives a restart; disabled plug-ins show no fields
    drop(m);
    let mut m = Manager::open(&dir).unwrap();
    assert_eq!(value(&m.metadata_fields("3"), "job"), "J-2");
    m.set_enabled(id, false).unwrap();
    assert!(m.metadata_fields("3").is_empty());
    assert!(m.set_metadata_field(id, "3", "job", &json!("x")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
