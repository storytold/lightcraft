//! Never-crash (P6.2), beyond the ABI tests: mutated WebAssembly modules, manifests, replies
//! (commands and the publish hook), `plugins.json` and the per-plug-in store files are errors,
//! never a panic or a hang.

use dac_plugins::publish::{PluginPublisher, PublishItem, PublishProvider};
use dac_plugins::{Limits, Manager, Manifest, NamespaceStore, NoHost, Permissions, Plugin};
use serde_json::json;

const MANIFEST: &str = r#"{"id":"test.plugin","name":"Test","version":"1.0.0","permissions":{"catalog":true},"commands":[{"id":"go","label":"Go"}],"publish":{"id":"svc","name":"Svc"}}"#;

/// A module whose manifest is `manifest` and whose `dac_handle` returns `reply`.
fn module(manifest: &str, reply: &str) -> Vec<u8> {
    let esc = |s: &str| s.bytes().map(|b| format!("\\{b:02x}")).collect::<String>();
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (import "host" "log" (func $log (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (data (i32.const 32768) "{r}")
  (func (export "dac_abi_version") (result i32) (i32.const 1))
  (func (export "dac_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "dac_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "dac_handle") (param $ptr i32) (param $len i32) (result i64)
    (i64.or (i64.shl (i64.const {rlen}) (i64.const 32)) (i64.const 32768)))
)"#,
        m = esc(manifest),
        r = esc(reply),
        mlen = manifest.len(),
        rlen = reply.len()
    );
    wat::parse_str(&src).unwrap()
}

fn grant() -> Permissions {
    Permissions { catalog: true, ..Default::default() }
}

fn limits() -> Limits {
    Limits { wall_time_ms: 200, fuel: 5_000_000, ..Limits::default() }
}

#[test]
fn seeds_are_valid_plugins() {
    let mut m = Manager::in_memory();
    m.install_bytes(&module(MANIFEST, "{}"), Some(grant())).unwrap();
    m.set_enabled("test.plugin", true).unwrap();
    assert!(PluginPublisher::new(&mut m, "test.plugin").is_ok());
}

#[test]
fn mutated_modules_never_panic() {
    let seeds = [module(MANIFEST, r#"{"ok":true}"#), module(MANIFEST, r#"{"remoteId":"r1","url":"https://x"}"#)];
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    dac_fuzzkit::run("plugins.wasm", &refs, 400, |b| {
        if Plugin::load(b, limits()).is_err() {
            return;
        }
        let mut m = Manager::in_memory();
        m.set_limits(limits());
        let Ok(p) = m.install_bytes(b, Some(grant())) else { return };
        let id = p.plugin.id().to_string();
        let _ = m.set_enabled(&id, true);
        let _ = m.run_command(&id, "go", &json!({}), &["1".into()], &mut NoHost);
        let _ = m.post_export(&[json!({"path": "/x.jpg"})], &mut NoHost);
    });
}

#[test]
fn manifests_never_panic() {
    dac_fuzzkit::run_json("plugins.manifest", &[MANIFEST], 3000, |s| {
        if let Ok(m) = Manifest::parse(s.as_bytes()) {
            let _ = m.validate();
        }
    });
}

/// Replies of a plug-in's command and publish hooks (the plug-in is untrusted code).
#[test]
fn hostile_replies_never_panic() {
    let seeds = [r#"{"remoteId":"r1","url":"https://x/1"}"#, r#"{"result":{"photos":["1","2"],"metadata":[{"photo":"1","title":"t"}]}}"#];
    dac_fuzzkit::run_json("plugins.reply", &seeds, 150, |reply| {
        let mut m = Manager::in_memory();
        m.set_limits(limits());
        if m.install_bytes(&module(MANIFEST, reply), Some(grant())).is_err() {
            return;
        }
        let _ = m.set_enabled("test.plugin", true);
        let _ = m.run_command("test.plugin", "go", &json!({"a": 1}), &[], &mut NoHost);
        let _ = m.suggest_metadata(&[json!({"id": "1"})], &mut NoHost);
        if let Ok(mut p) = PluginPublisher::new(&mut m, "test.plugin") {
            let item = PublishItem { path: "/tmp/x.jpg".into(), photo: "1".into(), ..Default::default() };
            let _ = p.upload(&mut NoHost, &item);
            let _ = p.delete(&mut NoHost, "r1");
        }
    });
}

#[test]
fn plugin_state_and_store_files_never_panic() {
    let dir = std::env::temp_dir().join(format!("dac-plugins-robust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test.wasm"), module(MANIFEST, "{}")).unwrap();
    let state = r#"{"plugins":{"test.plugin":{"enabled":true,"grant":{"catalog":true,"fs":[{"path":"/x","write":true}]}}}}"#;
    dac_fuzzkit::run_json("plugins.state", &[state], 200, |s| {
        std::fs::write(dir.join("plugins.json"), s).unwrap();
        if let Ok(m) = Manager::open(&dir) {
            for p in m.list() {
                let _ = (p.effective(), p.info());
            }
        }
    });
    let store = r#"{"1":{"score":0.5,"tags":["a"]},"2":{}}"#;
    let file = dir.join("store.json");
    dac_fuzzkit::run_json("plugins.store", &[store], 1000, |s| {
        std::fs::write(&file, s).unwrap();
        if let Ok(mut st) = NamespaceStore::open(file.clone()) {
            for p in st.photos() {
                let _ = st.get(&p);
            }
            let _ = st.set("1", "k", json!([1, 2]));
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}
