//! The SDK example plug-in (`sdk/examples/catalog-tools`), built to wasm32 by this test, installed
//! in a [`Manager`] and driven through every hook with a fake catalog.

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
        lcc_testkit::skip!(System, "the wasm32-unknown-unknown target is not installed (rustup target add wasm32-unknown-unknown)");
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
    let info = m.install_bytes_by_user(&wasm, Some(Permissions { catalog: true, ..Default::default() }), false).unwrap().info();
    assert_eq!(info["id"], "org.example.catalog-tools");
    assert_eq!(info["granted"]["catalog"], true);
    // `export_plugin!(package: …)`: what the manifest JSON leaves out comes from the crate's Cargo.toml
    let cargo = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/examples/catalog-tools/Cargo.toml")).unwrap();
    let version = cargo.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).unwrap();
    assert_eq!(info["version"], version, "the version is the crate's");
    assert_eq!(info["about"]["license"], "MIT", "so is the licence");
    assert_eq!(info["description"], "Example plug-in for the SDK", "the manifest's own description stays");
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
    assert!(e.to_string().contains("needs `catalog`, not granted"), "{e}");
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
    m.install_bytes_by_user(&wasm, Some(Permissions { catalog: true, ..Default::default() }), false).unwrap();
    let mut host = FakeLibrary { writes: vec![] };
    let sel: Vec<String> = vec!["7".into(), "8".into(), "9".into()];
    let e = m.run_command(id, "template", &json!({}), &sel, &mut host).unwrap_err();
    assert!(e.to_string().contains("metadataWrite"), "{e}");
    assert!(host.writes.is_empty());
    m.set_grant_by_user(id, Permissions { catalog: true, metadata_write: true, ..Default::default() }).unwrap();

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
    m.install_bytes(&wasm, None, false).unwrap();
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
    m.install_bytes_by_user(&wasm, Some(grant), false).unwrap();
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
    m.install_bytes(&wasm, None, false).unwrap();
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

/// What the stand-in converter does (`$1` is the job list of `<photo>\t<original>` lines): writes
/// `<original>-converted.tif` next to each original, and one `<photo>\t<made>` line per photo into
/// `result.txt` next to the list.
#[cfg(unix)]
const STAND_IN: &str = r#"d=$(dirname "$1"); t=$(printf '\t'); while IFS="$t" read -r p o; do m="${o%.*}-converted.tif"; : > "$m"; printf '%s\t%s\n' "$p" "$m"; done < "$1" > "$d/result.txt""#;

/// The app's side of `program.start`, with `/bin/sh` running [`STAND_IN`] as the installed program.
/// Its catalog: each photo's original is `<originals>/<id>.raw`; `library.add` calls are recorded.
#[cfg(unix)]
struct StandIn {
    installed: bool,
    runs: Vec<(lcc_plugins::StartRequest, std::process::Child)>,
    originals: std::path::PathBuf,
    added: Vec<Value>,
}

#[cfg(unix)]
impl HostApi for StandIn {
    fn catalog(&mut self, method: &str, p: &Value) -> Result<Value, String> {
        match (method, p["id"].as_str()) {
            ("photo.inspect", Some(id)) => Ok(json!({"id": id, "source": {"type": "file", "path": self.originals.join(format!("{id}.raw"))}})),
            _ => Err(format!("{method}: no catalog here")),
        }
    }

    fn library_add(&mut self, p: &Value) -> Result<Value, String> {
        self.added.push(p.clone());
        Ok(json!({"photo": format!("new{}", self.added.len()), "added": true, "reloaded": false}))
    }

    fn find_program(&mut self, decl: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
        if !self.installed {
            return vec![];
        }
        let bundle_id = if cfg!(target_os = "macos") { decl.mac_bundle_ids.first().cloned() } else { None };
        vec![lcc_plugins::Found { bundle_id, path: "/bin/sh".into(), version: Some("1.0".into()) }]
    }

    fn start_program(&mut self, run: lcc_plugins::StartRequest) -> Result<(), String> {
        match Command::new(&run.found.path).arg("-c").arg(STAND_IN).arg("stand-in").args(&run.args).spawn() {
            Ok(child) => {
                self.runs.push((run, child));
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(unix)]
#[test]
fn program_tools_example_starts_its_program_and_reads_what_it_made() {
    let Some(wasm) = build_sdk_example("program-tools") else { return };
    let dir = temp("program-tools");
    let mut m = Manager::open(&dir).unwrap();
    let id = "org.example.program-tools";
    let requested = m.install_bytes_by_user(&wasm, None, false).unwrap().info()["requested"].clone();
    assert_eq!(requested["programs"][0]["id"], "converter", "the manifest's programs are asked for");
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    let mut host = StandIn { installed: false, runs: vec![], originals: originals.clone(), added: vec![] };
    let photos = ["p1".to_string(), "p2".to_string()];
    let album = json!({"kind": "album", "id": "7", "name": "Trip"});

    // not granted yet: the example says so, and nothing starts
    let r = m.run_command(id, "send", &json!({}), &photos, &mut host).unwrap();
    assert!(r["message"].as_str().unwrap().contains("Allow"), "{r}");
    assert!(requested["catalog"] == true && requested["libraryAdd"] == true, "it asks to add its results: {requested}");
    let grant: Permissions = serde_json::from_value(json!({"catalog": true, "libraryAdd": true, "programs": requested["programs"]})).unwrap();
    m.set_grant_by_user(id, grant).unwrap();
    let r = m.run_command(id, "send", &json!({}), &photos, &mut host).unwrap();
    assert!(r["message"].as_str().unwrap().contains("not installed"), "{r}");
    assert!(host.runs.is_empty(), "nothing starts while the program isn't installed");

    // granted and installed, started from an album: the job list and the album are in the run's
    // folder before the program starts
    host.installed = true;
    let r = m.run_command_from(id, "send", &json!({}), &photos, &album, &mut host).unwrap();
    assert_eq!(r["message"], "Sent 2 photo(s) to Example Converter.");
    let (run, mut child) = host.runs.pop().unwrap();
    assert_eq!(r["run"], run.run());
    let job = run.dir.path().join("job.txt");
    assert_eq!(run.args, vec![job.display().to_string()], "{{tmpDir}} became the run's folder");
    let raw = |p: &str| originals.join(format!("{p}.raw")).display().to_string();
    assert_eq!(std::fs::read_to_string(&job).unwrap(), format!("p1\t{}\np2\t{}\n", raw("p1"), raw("p2")));
    assert_eq!(std::fs::read_to_string(run.dir.path().join("album.txt")).unwrap(), "7");

    // one run at a time per program
    assert!(m.run_command(id, "send", &json!({}), &photos, &mut host).is_err(), "a second start while the run is active fails");
    assert!(host.runs.is_empty());

    // the program exits: program.end adds what it made, stacked on each photo and in the album
    assert!(child.wait().unwrap().success());
    let mut call = m.program_end_call(id, run.run(), "converter", lcc_plugins::EndStatus::Exited, Some(0), run.dir.path()).unwrap();
    let outcome = call.run(&mut host);
    let ended = m.end(call, Some(outcome)).unwrap();
    assert_eq!(ended["converted"], json!(["p1", "p2"]));
    assert_eq!(ended["photos"], json!(["new1", "new2"]), "the new photos are reported");
    let made = |p: &str| originals.join(format!("{p}-converted.tif")).display().to_string();
    assert_eq!(
        host.added,
        vec![
            json!({"path": made("p1"), "from": "p1", "stack": true, "album": "7"}),
            json!({"path": made("p2"), "from": "p2", "stack": true, "album": "7"}),
        ],
        "each result is added from its photo's folder, stacked on the photo and in the album the command came from"
    );
    host.added.clear();

    // the run is over: the folder goes and a new start works
    let lcc_plugins::StartRequest { dir: run_dir, reservation, .. } = run;
    let folder = run_dir.path().to_path_buf();
    run_dir.remove();
    assert!(!folder.exists());
    drop(reservation);
    // started from a view that isn't an album: the results are only stacked
    let picks = json!({"kind": "other", "name": "picks"});
    m.run_command_from(id, "send", &json!({}), &photos, &picks, &mut host).unwrap();
    let (again, mut child) = host.runs.pop().unwrap();
    child.wait().unwrap();
    let mut call = m.program_end_call(id, again.run(), "converter", lcc_plugins::EndStatus::Exited, Some(0), again.dir.path()).unwrap();
    let outcome = call.run(&mut host);
    m.end(call, Some(outcome)).unwrap();
    assert_eq!(host.added.len(), 2);
    assert!(host.added.iter().all(|a| a["stack"] == true && a["album"].is_null()), "no album to add to: {:?}", host.added);
    // a program that exits with an error: nothing is recorded, and the log says why
    let mut call = m.program_end_call(id, again.run(), "converter", lcc_plugins::EndStatus::Exited, Some(2), again.dir.path()).unwrap();
    let outcome = call.run(&mut host);
    assert_eq!(m.end(call, Some(outcome)).unwrap()["converted"], json!([]), "a failed run converts nothing");
    assert!(m.get(id).unwrap().log.iter().any(|l| l.contains("exit code 2")), "the plug-in sees the exit code");
    again.dir.remove();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The keyword-picker example's catalog: a keyword tree, and the metadata writes it is asked for.
struct Keywords {
    writes: Vec<Value>,
}

impl HostApi for Keywords {
    fn catalog(&mut self, method: &str, _: &Value) -> Result<Value, String> {
        match method {
            "keyword.list" => Ok(json!([
                {"name": "Places", "path": "Places", "count": 0, "children": [
                    {"name": "Oslo", "path": "Places|Oslo", "count": 4, "children": []},
                    {"name": "Bergen", "path": "Places|Bergen", "count": 9, "children": []}]},
                {"name": "sunset", "path": "sunset", "count": 20, "children": []}
            ])),
            other => Err(format!("{other}: not in this test")),
        }
    }

    fn set_metadata(&mut self, params: &Value) -> Result<Value, String> {
        self.writes.push(params.clone());
        Ok(json!({}))
    }
}

/// The keyword-picker example: its settings decide which keywords its prepare step offers, the
/// dialog remembers the keyword (not *Remove it instead*), one keyword runs at once, none explains.
#[test]
fn keyword_picker_example_settings_remember_and_prepare() {
    use lcc_plugins::Prepared;
    let Some(wasm) = build_sdk_example("keyword-picker") else { return };
    let dir = temp("keyword-picker");
    let mut m = Manager::open(&dir).unwrap();
    let grant = Permissions { catalog: true, metadata_write: true, ..Default::default() };
    m.install_bytes_by_user(&wasm, Some(grant), false).unwrap();
    let id = "org.example.keyword-picker";
    let mut host = Keywords { writes: vec![] };
    let sel = ["1".to_string(), "2".to_string()];
    let options = |p: Prepared| match p {
        Prepared::Dialog { fields, error: None } => serde_json::to_value(&fields[0]).unwrap()["options"].clone(),
        other => panic!("a dialog: {other:?}"),
    };

    assert!(matches!(m.prepare_command(id, "apply", &[], &mut host).unwrap(), Prepared::Message(t) if t.contains("Select")), "no selection");
    assert_eq!(
        options(m.prepare_command(id, "apply", &sel, &mut host).unwrap()),
        json!(["sunset", "Places|Bergen", "Places|Oslo"]),
        "most used first"
    );
    m.set_settings(id, &json!({"minCount": 3, "order": "A to Z"}), &mut host).unwrap();
    let Prepared::Dialog { fields, .. } = m.prepare_command(id, "apply", &sel, &mut host).unwrap() else { panic!("a dialog") };
    assert_eq!(serde_json::to_value(&fields[0]).unwrap()["options"], json!(["Places|Bergen", "Places|Oslo", "sunset"]), "as the settings say");

    // run from the dialog: written, and the keyword (not `remove`) remembered
    let r = m.run_command(id, "apply", &json!({"keyword": "Places|Oslo", "remove": true}), &sel, &mut host).unwrap();
    assert!(r["message"].as_str().unwrap().contains("Removed"), "{r}");
    assert_eq!(host.writes.pop().unwrap(), json!({"ids": ["1", "2"], "removeKeywords": ["Places|Oslo"]}));
    assert_eq!(Value::Object(m.command_values(id, "apply", &fields)), json!({"keyword": "Places|Oslo", "remove": false}));
    assert!(m.run_command(id, "apply", &json!({"keyword": "Places|Oslo|Old"}), &sel, &mut host).is_err(), "not offered");

    // one keyword qualifies: it runs at once; none: the plug-in says why
    m.set_settings(id, &json!({"minCount": 10}), &mut host).unwrap();
    assert_eq!(m.prepare_command(id, "apply", &sel, &mut host).unwrap(), Prepared::Run(json!({"keyword": "sunset", "remove": false})));
    m.run_command(id, "apply", &Value::Null, &sel, &mut host).unwrap();
    assert_eq!(host.writes.pop().unwrap(), json!({"ids": ["1", "2"], "addKeywords": ["sunset"]}));
    m.set_settings(id, &json!({"minCount": 100}), &mut host).unwrap();
    let r = m.run_command(id, "apply", &Value::Null, &sel, &mut host).unwrap();
    assert_eq!(r["ran"], false, "{r}");
    assert!(host.writes.is_empty());
    drop(m);

    let mut m = Manager::open(&dir).unwrap();
    assert_eq!(m.settings(id, &mut lcc_plugins::NoHost).unwrap().values["minCount"], json!(100.0), "kept after a restart");
}

/// Counts what the job-tools example reports; cancelled once `cancel_after` photos were counted.
#[derive(Default)]
struct JobRow {
    items: std::cell::Cell<u64>,
    failed: std::cell::RefCell<Vec<String>>,
    last: std::cell::Cell<(u64, u64)>,
    cancel_after: Option<u64>,
}

impl lcc_plugins::JobReporter for JobRow {
    fn progress(&self, done: u64, total: u64, _: lcc_plugins::JobUnit) {
        self.last.set((done, total));
    }
    fn detail(&self, _: &str) {}
    fn item_done(&self) {
        self.items.set(self.items.get() + 1);
    }
    fn item_failed(&self, why: &str) {
        self.items.set(self.items.get() + 1);
        self.failed.borrow_mut().push(why.to_string());
    }
    fn cancelled(&self) -> bool {
        self.cancel_after.is_some_and(|n| self.items.get() >= n)
    }
}

/// A catalog that can't inspect photo 13.
struct Inspect;
impl HostApi for Inspect {
    fn catalog(&mut self, method: &str, p: &Value) -> Result<Value, String> {
        match p["id"].as_str() {
            Some("13") => Err("the file is missing".into()),
            _ => Ok(json!({"method": method})),
        }
    }
}

#[test]
fn job_tools_example_works_through_the_photos_and_stops_when_asked() {
    let Some(wasm) = build_sdk_example("job-tools") else { return };
    let mut m = Manager::in_memory();
    m.install_bytes_by_user(&wasm, Some(Permissions { catalog: true, ..Default::default() }), false).unwrap();
    let id = "org.example.job-tools";
    let photos: Vec<String> = (1..=25).map(|n| n.to_string()).collect();
    let run = |m: &mut Manager, row: &JobRow| {
        let lcc_plugins::Begun::Call(mut call) = m.begin_command(id, "check", &json!({}), &photos, &Value::Null, &mut Inspect).unwrap() else {
            panic!("check runs")
        };
        assert!(call.is_job());
        let outcome = call.run_with(&mut Inspect, row);
        m.end(*call, Some(outcome))
    };
    let row = JobRow::default();
    assert_eq!(run(&mut m, &row).unwrap(), json!({"message": "Checked 25 photos"}));
    assert_eq!(row.items.get(), 25, "every photo counted, over three calls");
    assert_eq!(row.last.get(), (25, 25));
    assert_eq!(*row.failed.borrow(), vec!["13: the file is missing".to_string()]);

    let row = JobRow { cancel_after: Some(12), ..JobRow::default() };
    assert_eq!(run(&mut m, &row).unwrap(), json!({"message": "Stopped after 12 of 25 photos"}), "it stops cleanly and says how far it got");
    assert_eq!(row.items.get(), 12);
}

/// Builds the SDK's stamp tool (`sdk/stamp`) on its own, from its own lockfile, as an author's CI does.
fn build_stamp_tool() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/stamp");
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugin-stamp-tool");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(&root)
        .args(["build", "--locked"])
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .unwrap();
    assert!(status.success(), "building plugin-stamp on its own failed");
    target.join("debug").join(format!("plugin-stamp{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn stamped_example_installs_and_the_tool_checks_it() {
    let Some(wasm) = build_example() else { return };
    let tool = build_stamp_tool();
    let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/stamp/Cargo.lock")).unwrap();
    for name in lock.lines().filter_map(|l| l.strip_prefix("name = \"")?.strip_suffix('"')) {
        assert!(
            !name.starts_with("lcc-") && !["rustls", "ureq", "reqwest", "hyper"].contains(&name),
            "the tool needs no app or network crate: {name}"
        );
    }
    let dir = temp("stamp-tool");
    let (raw, stamped) = (dir.join("raw.wasm"), dir.join("stamped.wasm"));
    std::fs::write(&raw, &wasm).unwrap();
    let run = |args: &[&Path]| Command::new(&tool).args(args).output().unwrap();

    let out = run(&[&raw, Path::new("-o"), &stamped]);
    assert!(out.status.success(), "stamping the example: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&raw).unwrap(), wasm, "-o leaves the build alone");
    let bytes = std::fs::read(&stamped).unwrap();

    // `export_plugin!(package: …)` fills the version in at run time; the section has it
    let section: Value = serde_json::from_slice(plugin_stamp::read_section(&bytes).unwrap().expect("a section")).unwrap();
    let cargo = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/examples/catalog-tools/Cargo.toml")).unwrap();
    let version = cargo.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).unwrap();
    assert_eq!(section["version"], version, "the section holds the version the package form filled in");

    let check = run(&[Path::new("--check"), &stamped]);
    assert!(check.status.success(), "--check passes the stamped file: {}", String::from_utf8_lossy(&check.stderr));
    let check = run(&[Path::new("--check"), &raw]);
    assert_eq!(check.status.code(), Some(1), "--check fails an unstamped file");
    assert!(String::from_utf8_lossy(&check.stderr).contains("wasn't stamped"), "and says why");
    assert_eq!(run(&[Path::new("--bogus")]).status.code(), Some(2), "a usage error");

    let mut m = Manager::in_memory();
    let info = m.install_bytes_by_user(&bytes, Some(Permissions { catalog: true, ..Default::default() }), false).unwrap().info();
    assert_eq!(info["id"], section["id"], "the stamped example installs as itself");
    assert_eq!(info["version"], section["version"]);
}

/// The tool's signing commands, as an author runs them: `keygen` makes a key once, `sign` signs a
/// stamped file, `--check` reports the signature, and stamping a signed file says to sign it again.
#[test]
fn the_stamp_tool_makes_a_key_signs_and_checks_the_signature() {
    let tool = build_stamp_tool();
    let dir = temp("stamp-sign");
    let run = |args: &[&Path]| Command::new(&tool).args(args).output().unwrap();
    let text = |o: &std::process::Output| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let manifest = r#"{"id":"test.cli.signed","name":"Signed","version":"1.0.0"}"#;
    let raw = wat::parse_str(format!(
        r#"(module (memory (export "memory") 1) (data (i32.const 16) "{m}") (data (i32.const 4096) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 8192))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 4096))))"#,
        m = manifest.replace('"', "\\\""),
        ml = manifest.len()
    ))
    .unwrap();
    let (key, plugin, signed) = (dir.join("author.key"), dir.join("p.wasm"), dir.join("signed.wasm"));
    std::fs::write(&plugin, &raw).unwrap();

    let out = run(&[Path::new("keygen"), &key]);
    assert!(out.status.success(), "keygen: {}", text(&out));
    let secret = plugin_stamp::secret_key_from_text(&std::fs::read_to_string(&key).unwrap()).unwrap();
    let fingerprint = plugin_stamp::fingerprint(&plugin_stamp::public_key(&secret));
    assert!(text(&out).contains(&fingerprint), "keygen prints the fingerprint: {}", text(&out));
    let again = run(&[Path::new("keygen"), &key]);
    assert_eq!(again.status.code(), Some(1), "a second keygen to the same file fails");
    assert!(text(&again).contains("never overwritten"), "{}", text(&again));
    assert_eq!(plugin_stamp::secret_key_from_text(&std::fs::read_to_string(&key).unwrap()).unwrap(), secret, "the key is kept");

    let unstamped = run(&[Path::new("sign"), Path::new("--key"), &key, &plugin]);
    assert_eq!(unstamped.status.code(), Some(1), "an unstamped file isn't signed: {}", text(&unstamped));
    assert_eq!(run(&[Path::new("sign"), &plugin]).status.code(), Some(2), "sign without --key is a usage error");
    assert!(run(&[&plugin]).status.success(), "stamp");
    let out = run(&[Path::new("sign"), Path::new("--key"), &key, &plugin, Path::new("-o"), &signed]);
    assert!(out.status.success() && text(&out).contains(&fingerprint), "sign: {}", text(&out));
    let check = run(&[Path::new("--check"), &signed]);
    assert!(check.status.success() && text(&check).contains(&format!("signed by {fingerprint}")), "--check: {}", text(&check));
    assert!(text(&run(&[Path::new("--check"), &plugin])).contains("not signed"), "the unsigned input");

    // changed after signing: --check fails and says why
    let tampered = dir.join("tampered.wasm");
    std::fs::write(&tampered, [std::fs::read(&signed).unwrap().as_slice(), &[0, 2, 1, b'x']].concat()).unwrap();
    let check = run(&[Path::new("--check"), &tampered]);
    assert_eq!(check.status.code(), Some(1), "{}", text(&check));
    assert!(text(&check).contains("invalid"), "{}", text(&check));

    // stamping the signed file again drops the signature, and says so
    let out = run(&[&signed]);
    assert!(out.status.success() && text(&out).contains("sign it again"), "{}", text(&out));
    assert!(text(&run(&[Path::new("--check"), &signed])).contains("not signed"));
}
