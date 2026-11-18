//! Plug-in runs against a real session (the demo library), with modules assembled from
//! WebAssembly text.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;
use crate::catalog::PhotoId;
use crate::plugin::craft_plugin::bundle::Bundle;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-plugins-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A module that calls `calls` (command, params JSON) in order. A refused or failed call fails the
/// run when `strict`; otherwise its message becomes the output and the run goes on. `tail` is WAT
/// run after the calls (a trap, an endless loop), before returning 0.
fn module(calls: &[(&str, String)], strict: bool, tail: &str) -> Vec<u8> {
    let mut data = String::new();
    let mut body = String::new();
    let mut at = 0usize;
    for (command, params) in calls {
        let (c, p) = (at, at + command.len());
        at = p + params.len();
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        data.push_str(&format!(r#"(data (i32.const {c}) "{}") (data (i32.const {p}) "{}")"#, esc(command), esc(params)));
        let on_error = if strict {
            "(return (i32.const 1))".to_string()
        } else {
            "(call $set_output (i32.const 8192) (call $reply_read (i32.const 8192) (call $reply_len)))".to_string()
        };
        body.push_str(&format!(
            "(if (i32.ne (call $call (i32.const {c}) (i32.const {}) (i32.const {p}) (i32.const {})) (i32.const 0)) (then {on_error}))\n",
            command.len(),
            params.len()
        ));
    }
    let wat = format!(
        r#"(module
          (import "craft" "call" (func $call (param i32 i32 i32 i32) (result i32)))
          (import "craft" "reply_len" (func $reply_len (result i32)))
          (import "craft" "reply_read" (func $reply_read (param i32 i32) (result i32)))
          (import "craft" "set_output" (func $set_output (param i32 i32)))
          (memory (export "memory") 1)
          {data}
          (func (export "craft_run") (param $n i32) (result i32) {body} {tail} (i32.const 0)))"#
    );
    wat::parse_str(&wat).unwrap_or_else(|e| panic!("{e}\n{wat}"))
}

/// The `.craftplugin` of plug-in `id` for LightCraft, asking for `access`.
fn bundle(id: &str, access: &str, wasm: Vec<u8>) -> Vec<u8> {
    let manifest = json!({
        "id": id, "name": "Test Plug-in", "version": "1.0.0", "main": "plugin.wasm",
        "apps": {"lightcraft": {"api": 6}},
        "permissions": {"access": access},
        "commands": [{"id": "go", "label": "Go"}]
    });
    Bundle { manifest: manifest.to_string(), wasm, assets: vec![] }.encode()
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The demo library with plug-in `id` installed for the session (no network), and two photos.
fn with_plugin(id: &str, access: &str, wasm: Vec<u8>) -> (Session, PhotoId, PhotoId) {
    let mut s = Session::with_demo();
    s.plugins.network = false;
    let r = s.execute("plugin.install", &json!({"data": base64(&bundle(id, access, wasm)), "name": "test.craftplugin"})).unwrap();
    assert_eq!(r["installed"], true);
    assert_eq!(r["stored"], false, "without a store, a plug-in is kept for the session");
    let v = s.visible_cloned();
    (s, v[0], v[1])
}

fn rating(s: &Session, id: PhotoId) -> u8 {
    s.catalog.photo(id).unwrap().rating
}

fn labels(s: &Session) -> Vec<String> {
    s.undo.iter().map(|u| u.label.clone()).collect()
}

fn rate(id: PhotoId, stars: u8) -> (&'static str, String) {
    ("photo.rate", json!({"rating": stars, "ids": [id.0]}).to_string())
}

#[test]
fn base64_round_trips() {
    for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0, 255, 128, 7, 9]] {
        assert_eq!(super::base64_decode(&base64(bytes)).unwrap(), bytes);
    }
    assert!(super::base64_decode("not base64!").is_none());
}

#[test]
fn a_write_plugin_runs_real_commands_as_one_undo_step_named_after_it() {
    let mut s = Session::with_demo();
    let v = s.visible_cloned();
    let (a, b) = (v[0], v[1]);
    s.plugins.network = false;
    let wasm = module(&[rate(a, 4), rate(b, 2), ("photo.label", json!({"label": "red", "ids": [a.0]}).to_string())], true, "");
    s.execute("plugin.install", &json!({"data": base64(&bundle("io.example.rate", "write", wasm))})).unwrap();
    s.execute("photo.rate", &json!({"rating": 1, "ids": [b.0]})).unwrap();
    let (steps, label, was_a) = (labels(&s).len(), s.catalog.photo(a).unwrap().label, rating(&s, a));

    let r = s.execute("plugin.run", &json!({"id": "io.example.rate"})).unwrap();

    assert_eq!(r["calls"], 3, "{r}");
    assert_eq!((rating(&s, a), rating(&s, b)), (4, 2));
    let after = labels(&s);
    assert_eq!(after.len(), steps + 1, "three commands, one undo step");
    assert_eq!(after.last().map(String::as_str), Some("Test Plug-in"));

    assert_eq!(s.execute("edit.undo", &json!({})).unwrap()["undone"], "Test Plug-in");
    assert_eq!((rating(&s, a), rating(&s, b)), (was_a, 1), "one undo takes the whole run back");
    assert_eq!(s.catalog.photo(a).unwrap().label, label);
    assert_eq!(labels(&s).len(), steps, "the library's own steps are still there");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!((rating(&s, a), rating(&s, b)), (4, 2));
}

#[test]
fn a_run_that_changes_nothing_records_no_step_and_keeps_redo() {
    let (mut s, a, _) = with_plugin("io.example.reader", "write", module(&[("catalog.stats", "{}".into())], true, ""));
    s.execute("photo.rate", &json!({"rating": 3, "ids": [a.0]})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    let steps = labels(&s).len();

    s.execute("plugin.run", &json!({"id": "io.example.reader"})).unwrap();

    assert_eq!(labels(&s).len(), steps);
    assert!(!s.redo.is_empty(), "redo survives a run that edited nothing");
}

#[test]
fn a_read_plugin_that_edits_is_undone_and_refused() {
    let mut s = Session::with_demo();
    s.plugins.network = false;
    let a = s.visible_cloned()[0];
    let wasm = module(&[("photo.inspect", json!({"id": a.0}).to_string()), rate(a, 5)], false, "");
    s.execute("plugin.install", &json!({"data": base64(&bundle("io.example.peek", "read", wasm))})).unwrap();
    let (before, steps) = (rating(&s, a), labels(&s).len());

    let r = s.execute("plugin.run", &json!({"id": "io.example.peek"})).unwrap();

    let message = r["output"].as_str().unwrap();
    assert!(message.contains("may only read it"), "{message}");
    assert_eq!(rating(&s, a), before, "the edit is undone");
    assert_eq!(labels(&s).len(), steps, "nothing to undo");
    assert!(s.redo.is_empty(), "and nothing to redo");
}

#[test]
fn refused_commands_and_file_paths_are_refused() {
    for (call, why) in [
        (("file.open", r#"{"path":"/etc/hosts"}"#), "isn't available to plugins"),
        (("edit.undo", "{}"), "isn't available to plugins"),
        (("photo.delete", "{}"), "isn't available to plugins"),
        (("photo.deletePermanently", "{}"), "isn't available to plugins"),
        (("photo.moveToFolder", r#"{"folder":"/tmp"}"#), "isn't available to plugins"),
        (("photo.rename", r#"{"template":"x"}"#), "isn't available to plugins"),
        (("library.import", r#"{"paths":["/etc"]}"#), "isn't available to plugins"),
        (("library.info", "{}"), "isn't available to plugins"),
        (("app.export", r#"{"destination":"/tmp"}"#), "isn't available to plugins"),
        (("folder.rename", "{}"), "isn't available to plugins"),
        (("keyword.rename", r#"{"from":"a","to":"b"}"#), "isn't available to plugins"),
        (("preset.delete", "{}"), "isn't available to plugins"),
        (("develop.copy", "{}"), "isn't available to plugins"),
        (("plugin.list", "{}"), "isn't available to plugins"),
        (("library.source", r#"{"kind":"libraryFolder","path":"/home"}"#), "can't name files"),
        (("photo.setMeta", r#"{"title":"x","nested":{"file":"/etc/hosts"}}"#), "can't name files"),
    ] {
        let (mut s, a, _) = with_plugin("io.example.sneaky", "write", module(&[(call.0, call.1.into())], false, ""));
        let (before, steps, source) = (rating(&s, a), labels(&s).len(), s.source);
        let r = s.execute("plugin.run", &json!({"id": "io.example.sneaky"})).unwrap();
        let message = r["output"].as_str().unwrap_or_default();
        assert!(message.contains(why), "{}: {message}", call.0);
        assert_eq!((rating(&s, a), labels(&s).len(), s.source), (before, steps, source), "{}", call.0);
    }
}

#[test]
fn every_allowed_command_exists() {
    for id in super::ALLOWED {
        assert!(crate::find_command(id).is_some(), "{id} isn't a command");
    }
    // nothing of the history, the disk, the app or plug-ins themselves
    for id in super::ALLOWED {
        for refused in ["plugin.", "edit.", "folder.", "file.", "app.", "prefs.", "export.", "print.", "faces.", "denoise.", "merge."] {
            assert!(!id.starts_with(refused), "{id}");
        }
        assert!(!super::policy().refuses(id), "{id}");
    }
    assert!(super::policy().refuses("photo.deletePermanently") && super::policy().refuses("history.clear"));
}

#[test]
fn a_command_outside_the_manifest_allowlist_is_refused() {
    let mut s = Session::with_demo();
    s.plugins.network = false;
    let a = s.visible_cloned()[0];
    let manifest = json!({"id": "io.example.narrow", "name": "Narrow", "version": "1.0.0", "main": "p.wasm",
        "apps": {"lightcraft": {"api": 6}}, "permissions": {"access": "write", "commands": ["photo.flag"]}});
    let bytes = Bundle { manifest: manifest.to_string(), wasm: module(&[rate(a, 5)], false, ""), assets: vec![] }.encode();
    s.execute("plugin.install", &json!({"data": base64(&bytes)})).unwrap();
    let before = rating(&s, a);

    let r = s.execute("plugin.run", &json!({"id": "io.example.narrow"})).unwrap();

    assert!(r["output"].as_str().unwrap().contains("asked permission for"), "{r}");
    assert_eq!(rating(&s, a), before);
}

#[test]
fn a_trapping_or_endless_plugin_leaves_the_library_unchanged() {
    for (tail, fuel) in [("unreachable", None), ("(loop $l (br $l))", Some(200_000))] {
        let mut s = Session::with_demo();
        s.plugins.network = false;
        let v = s.visible_cloned();
        let (a, b) = (v[0], v[1]);
        let calls = [
            rate(a, 5),
            ("library.select", json!({"ids": [b.0], "active": b.0}).to_string()),
            ("library.sort", json!({"key": "rating", "ascending": false}).to_string()),
        ];
        s.execute("plugin.install", &json!({"data": base64(&bundle("io.example.crash", "write", module(&calls, false, tail)))})).unwrap();
        s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
        s.execute("photo.rate", &json!({"rating": 1, "ids": [b.0]})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        let (before, steps, selection, sort, redo) = (rating(&s, a), labels(&s), s.selection.clone(), s.sort, s.redo.len());
        let mut params = json!({"id": "io.example.crash"});
        if let Some(f) = fuel {
            params["fuel"] = json!(f);
        }

        let e = s.execute("plugin.run", &params).unwrap_err().to_string();

        assert!(e.contains("Test Plug-in"), "{e}");
        assert_eq!(rating(&s, a), before, "{tail}: the edits before the failure are rolled back");
        assert_eq!(labels(&s), steps);
        assert_eq!(s.redo.len(), redo, "{tail}: what could be redone still can");
        assert_eq!(s.selection, selection, "the selection is put back");
        assert_eq!(s.sort, sort, "the view is put back");
    }
}

#[test]
fn unknown_plugins_and_commands_do_not_run() {
    let (mut s, ..) = with_plugin("io.example.rate", "write", module(&[], true, ""));
    assert!(s.execute("plugin.run", &json!({"id": "io.example.nothing"})).unwrap_err().to_string().contains("no plug-in"));
    assert!(s.execute("plugin.run", &json!({"id": "io.example.rate", "command": "other"})).is_err());
    assert!(s.execute("plugin.run", &json!({})).is_err());
}

#[test]
fn settings_are_bounded_and_kept_per_plugin() {
    let mut all = Default::default();
    super::set_setting(&mut all, "a", "k", Some("{\"x\": 1}")).unwrap();
    assert_eq!(all["a"]["k"], json!({"x": 1}));
    assert!(super::set_setting(&mut all, "a", &"k".repeat(129), Some("1")).is_err());
    assert!(super::set_setting(&mut all, "a", "big", Some(&format!("\"{}\"", "x".repeat(64 * 1024)))).is_err());
    assert!(super::set_setting(&mut all, "a", "bad", Some("not json")).is_err());
    super::set_setting(&mut all, "a", "k", None).unwrap();
    assert!(!all.contains_key("a"));
}

#[test]
fn install_review_list_and_remove_through_the_store() {
    let dir = temp_dir("store");
    let mut s = Session::new();
    s.plugins.network = false;
    s.plugins.open_store(&dir);
    let bytes = bundle("io.example.stored", "write", module(&[], true, ""));
    let file = dir.join("stored.craftplugin");
    std::fs::write(&file, &bytes).unwrap();
    let path = file.to_string_lossy().to_string();

    // The confirmation: described, nothing written
    let review = s.execute("plugin.install", &json!({"path": path, "review": true})).unwrap();
    assert_eq!(review["installed"], false);
    assert_eq!(review["permissions"]["access"], "write");
    assert_eq!(review["kind"], "command");
    assert_eq!(review["sha256"], crate::plugin::craft_plugin::bundle::sha256_hex(&bytes));
    assert!(s.plugins.list().is_empty());
    assert!(!craft_plugin::store::plugins_dir(&dir).exists(), "nothing is written before the install");

    // Another bundle than the one reviewed isn't installed
    let e = s.execute("plugin.install", &json!({"path": path, "sha256": "0".repeat(64)})).unwrap_err();
    assert!(e.to_string().contains("reviewed"), "{e}");
    s.execute("plugin.install", &json!({"path": path, "review": true})).unwrap();
    let r = s.execute("plugin.install", &json!({"path": path, "sha256": review["sha256"]})).unwrap();
    assert_eq!((r["installed"].clone(), r["stored"].clone()), (json!(true), json!(true)));

    // A new session reads it back from the folder
    let mut again = Session::new();
    again.plugins.open_store(&dir);
    let list = again.execute("plugin.list", &json!({})).unwrap();
    assert_eq!(list["plugins"][0]["id"], "io.example.stored", "{list}");
    assert_eq!(list["plugins"][0]["source"]["type"], "file");
    assert_eq!(list["stored"], true);

    again.execute("plugin.remove", &json!({"id": "io.example.stored"})).unwrap();
    assert_eq!(again.execute("plugin.list", &json!({})).unwrap()["plugins"], json!([]));
    let mut third = Session::new();
    third.plugins.open_store(&dir);
    assert!(third.plugins.list().is_empty(), "removed from the folder");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_and_catalogs_are_kept_in_the_settings_folder() {
    let dir = temp_dir("prefs");
    let index = dir.join("index.json");
    std::fs::write(&index, json!({"schema": 1, "packages": []}).to_string()).unwrap();
    let url = format!("file://{}", index.to_string_lossy().replace('\\', "/"));
    let mut s = Session::new();
    s.plugins.open_store(&dir);
    s.execute("plugin.catalog.add", &json!({"url": url})).unwrap();
    s.plugins.prefs.settings.entry("io.example.x".into()).or_default().insert("k".into(), json!(1));
    s.plugins.save_prefs();

    let mut again = Session::new();
    again.plugins.open_store(&dir);
    assert_eq!(again.plugins.prefs.catalogs, vec![url]);
    assert_eq!(again.plugins.prefs.settings["io.example.x"]["k"], json!(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugins_for_other_apps_or_not_bundles_are_refused() {
    let mut s = Session::new();
    s.plugins.network = false;
    let manifest = json!({"id": "io.example.other", "name": "Other", "version": "1.0.0", "main": "p.wasm", "apps": {"figcraft": {"api": 6}}});
    let bytes = Bundle { manifest: manifest.to_string(), wasm: module(&[], true, ""), assets: vec![] }.encode();
    let e = s.execute("plugin.install", &json!({"data": base64(&bytes)})).unwrap_err().to_string();
    assert!(e.contains("figcraft"), "{e}");
    let e = s.execute("plugin.install", &json!({"data": base64(b"not a bundle")})).unwrap_err().to_string();
    assert!(e.contains("bundle"), "{e}");
    assert!(s.execute("plugin.install", &json!({})).is_err());
    assert!(s.plugins.list().is_empty());
}

#[test]
fn a_local_catalog_installs_its_package() {
    let dir = temp_dir("catalog");
    let bytes = bundle("io.example.listed", "write", module(&[], true, ""));
    let file = dir.join("listed.craftplugin");
    std::fs::write(&file, &bytes).unwrap();
    let url = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace('\\', "/"));
    let index = json!({"schema": 1, "packages": [{"id": "io.example.listed", "name": "Listed", "repo": "https://github.com/example/listed",
        "versions": [{"version": "1.0.0", "apps": {"lightcraft": {"api": 6}}, "url": url(&file),
            "sha256": crate::plugin::craft_plugin::bundle::sha256_hex(&bytes), "size": bytes.len()}]}]});
    let catalog = dir.join("index.json");
    std::fs::write(&catalog, index.to_string()).unwrap();
    let mut s = Session::new();
    s.plugins.network = false;

    s.execute("plugin.catalog.add", &json!({"url": url(&catalog)})).unwrap();
    let listed = s.execute("plugin.catalog.list", &json!({})).unwrap();
    let r = s.execute("plugin.install", &json!({"catalog": url(&catalog), "id": "io.example.listed"})).unwrap();

    assert_eq!(listed["packages"][0]["id"], "io.example.listed", "{listed}");
    assert_eq!(r["inCatalog"], true);
    assert_eq!(r["source"]["type"], "catalog");
    assert_eq!(s.execute("plugin.update", &json!({})).unwrap()["updates"], json!([]), "up to date");
    assert!(s.execute("plugin.catalog.add", &json!({"url": "https://example.com/index.json"})).is_err(), "GitHub's hosts only");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_repository_install_without_network_says_so_and_lists_no_download_left_behind() {
    let mut s = Session::new();
    s.plugins.network = false;
    let e = s.execute("plugin.install", &json!({"repo": "example/tools"})).unwrap_err().to_string();
    assert!(e.contains("no network access"), "{e}");
    assert!(s.activity.list().is_empty(), "the download's row goes when it ends");
}

#[test]
fn a_frontend_download_handed_over_installs_without_fetching_again() {
    let mut s = Session::new();
    s.plugins.network = false;
    let bytes = bundle("io.example.handed", "read", module(&[], true, ""));
    let request = json!({"repo": "example/handed"});
    let candidate = super::Candidate {
        bytes: bytes.clone(),
        source: craft_plugin::Source::Github { repo: "example/handed".into(), tag: "v1".into(), asset: "handed.craftplugin".into() },
        in_catalog: Some(false),
    };
    let sha = super::hold(&mut s, request.clone(), candidate);
    let mut review = request.clone();
    review["review"] = json!(true);
    review["sha256"] = json!(sha);
    let info = s.execute("plugin.install", &review).unwrap();
    assert_eq!((info["installed"].clone(), info["inCatalog"].clone()), (json!(false), json!(false)), "{info}");
    let mut go = request;
    go["sha256"] = json!(sha);
    let r = s.execute("plugin.install", &go).unwrap();
    assert_eq!(r["installed"], true, "{r}");
    assert_eq!(
        s.plugins.get("io.example.handed").unwrap().source,
        craft_plugin::Source::Github { repo: "example/handed".into(), tag: "v1".into(), asset: "handed.craftplugin".into() }
    );
}

#[test]
fn hostile_install_and_run_parameters_are_errors() {
    let mut s = Session::with_demo();
    s.plugins.network = false;
    for p in [
        json!(null),
        json!([1, 2]),
        json!({"data": 7}),
        json!({"data": "%%%"}),
        json!({"catalog": "file:///nowhere/index.json"}),
        json!({"catalog": "ftp://x", "id": "y"}),
        json!({"repo": "not a repo"}),
        json!({"path": "/nowhere/at/all.craftplugin"}),
    ] {
        assert!(s.execute("plugin.install", &p).is_err(), "{p}");
    }
    for p in [json!(null), json!({"id": 5}), json!({"id": "x", "params": "nope"}), json!({"id": "x", "fuel": -1})] {
        assert!(s.execute("plugin.run", &p).is_err(), "{p}");
    }
    let _: Value = s.execute("plugin.list", &json!({})).unwrap();
}
