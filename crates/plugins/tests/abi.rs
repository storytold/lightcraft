//! The ABI against hand-written modules: hostile ones (infinite loops, memory bombs, forbidden
//! imports, bad manifests, lying pointers) must fail with an error, quickly; host calls are
//! checked against the grant.

use std::time::{Duration, Instant};

use dac_plugins::{Error, Limits, Manager, NoHost, Permissions, Plugin};
use serde_json::{Value, json};

/// A module implementing the ABI. `handle` is the body of `dac_handle(ptr, len) -> i64`;
/// `request` (a host request, JSON) is placed at offset 8192 for it to send.
fn module(manifest: &str, version: i32, handle: &str, extra: &str, request: &str) -> Vec<u8> {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (import "host" "log" (func $log (param i32 i32)))
  {extra}
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (data (i32.const 8192) "{r}")
  (func (export "dac_abi_version") (result i32) (i32.const {version}))
  (func (export "dac_manifest") (result i64)
    (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "dac_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1))
      (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "dac_handle") (param $ptr i32) (param $len i32) (result i64) (local $n i32)
    {handle})
)"#,
        m = esc(manifest),
        r = esc(request),
        mlen = manifest.len()
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}\n{src}"))
}

const MANIFEST: &str = r#"{"id":"test.plugin","name":"Test","permissions":{"catalog":true},"commands":[{"id":"go","label":"Go"}]}"#;

/// Sends the request at 8192, copies the reply to 16384 and returns it.
fn forward(request: &str) -> Vec<u8> {
    let handle = format!(
        "(local.set $n (call $call (i32.const 8192) (i32.const {len})))
         (call $response (i32.const 16384))
         (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 16384))",
        len = request.len()
    );
    module(MANIFEST, 1, &handle, "", request)
}

/// Returns a constant reply placed at 8192.
fn reply(json: &str) -> Vec<u8> {
    module(MANIFEST, 1, &format!("(i64.or (i64.shl (i64.const {}) (i64.const 32)) (i64.const 8192))", json.len()), "", json)
}

fn run(bytes: &[u8], limits: Limits) -> (dac_plugins::Result<Value>, Duration) {
    let t = Instant::now();
    let mut m = Manager::in_memory();
    m.set_limits(limits.clone());
    let r = Plugin::load(bytes, limits).and_then(|_| {
        m.install_bytes(bytes, None)?;
        m.run_command("test.plugin", "go", &json!({}), &[], &mut NoHost)
    });
    (r, t.elapsed())
}

#[test]
fn a_plain_reply_round_trips() {
    let (r, _) = run(&reply(r#"{"ok":{"x":1}}"#), Limits::default());
    assert_eq!(r.unwrap(), json!({"x": 1}));
    let (r, _) = run(&reply(r#"{"error":"nope"}"#), Limits::default());
    assert_eq!(r.unwrap_err(), Error::Failed("nope".into()));
}

#[test]
fn host_calls_follow_the_grant() {
    struct Cat;
    impl dac_plugins::HostApi for Cat {
        fn catalog(&mut self, _: &str, _: &Value) -> Result<Value, String> {
            Ok(json!({"photos": 3}))
        }
    }
    let bytes = forward(r#"{"method":"catalog.stats","params":{}}"#);
    let mut m = Manager::in_memory();
    m.install_bytes(&bytes, None).unwrap();
    // the plug-in returns the host's reply envelope as its own
    assert_eq!(m.run_command("test.plugin", "go", &json!({}), &[], &mut Cat).unwrap(), json!({"photos": 3}));
    m.set_grant("test.plugin", Permissions::default()).unwrap();
    let e = m.run_command("test.plugin", "go", &json!({}), &[], &mut Cat).unwrap_err();
    assert!(e.to_string().contains("permission `catalog`"), "{e}");
    // a grant beyond the request is cut back to it
    let g = m.set_grant("test.plugin", Permissions { catalog: true, metadata_write: true, network: vec!["x.com".into()], fs: vec![] }).unwrap();
    assert_eq!(g, Permissions { catalog: true, ..Default::default() });
}

#[test]
fn unrequested_capabilities_are_refused() {
    for (req, needle) in [
        (r#"{"method":"metadata.setStandard","params":{}}"#, "metadataWrite"),
        (r#"{"method":"http.request","params":{"url":"https://example.com/"}}"#, "network"),
        (r#"{"method":"fs.readText","params":{"path":"/etc/passwd"}}"#, "not inside a folder"),
        (r#"{"method":"fs.writeText","params":{"path":"/tmp/x","text":"y"}}"#, "not inside a folder"),
        (r#"{"method":"no.such","params":{}}"#, "unknown host method"),
        ("not json", "not JSON"),
    ] {
        let (r, _) = run(&forward(req), Limits::default());
        let e = r.unwrap_err().to_string();
        assert!(e.contains(needle), "{req}: {e}");
    }
}

#[test]
fn hostile_modules_fail_fast() {
    let limits = Limits { fuel: 50_000_000, wall_time_ms: 5_000, max_memory_bytes: 8 << 20, ..Limits::default() };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("infinite loop", module(MANIFEST, 1, "(loop $l (br $l)) (i64.const 0)", "", "")),
        ("memory bomb", module(MANIFEST, 1, "(drop (memory.grow (i32.const 60000))) (drop (memory.grow (i32.const 60000))) (i64.const 0)", "", "")),
        ("reply out of bounds", module(MANIFEST, 1, "(i64.or (i64.shl (i64.const 100) (i64.const 32)) (i64.const 0x7ffffff0))", "", "")),
        ("reply not json", reply("garbage")),
        ("unreachable", module(MANIFEST, 1, "unreachable", "", "")),
        ("deep recursion", module(MANIFEST, 1, "(call $rec) (i64.const 0)", "(func $rec (call $rec))", "")),
        ("host.call with a lying pointer", module(MANIFEST, 1, "(drop (call $call (i32.const 0x7ffffff0) (i32.const 100))) (i64.const 0)", "", "")),
        ("host.call flood", module(MANIFEST, 1, "(loop $l (drop (call $call (i32.const 8192) (i32.const 2))) (br $l)) (i64.const 0)", "", "{}")),
    ];
    for (name, bytes) in cases {
        let (r, t) = run(&bytes, limits.clone());
        assert!(r.is_err(), "{name} must fail");
        assert!(t < Duration::from_secs(10), "{name} took {t:?}");
    }
}

#[test]
fn bad_modules_are_rejected_on_load() {
    let wasi =
        module(MANIFEST, 1, "(i64.const 0)", r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#, "");
    assert!(matches!(Plugin::load(&wasi, Limits::default()), Err(Error::Abi(_))));
    assert!(matches!(Plugin::load(&module(MANIFEST, 2, "(i64.const 0)", "", ""), Limits::default()), Err(Error::Abi(_))));
    assert!(matches!(Plugin::load(&module(r#"{"id":"bad id","name":"x"}"#, 1, "(i64.const 0)", "", ""), Limits::default()), Err(Error::Manifest(_))));
    assert!(matches!(Plugin::load(b"\0asm garbage", Limits::default()), Err(Error::Module(_))));
    assert!(matches!(Plugin::load(b"MZ", Limits::default()), Err(Error::Module(_))));
    let big = Limits { max_module_bytes: 10, ..Limits::default() };
    assert!(matches!(Plugin::load(&reply("{}"), big), Err(Error::Module(_))));
}

#[test]
fn dialog_arguments_are_checked() {
    let manifest = r#"{"id":"test.plugin","name":"Test","commands":[{"id":"go","label":"Go","dialog":[{"key":"n","label":"N","type":"number","min":0,"max":1,"default":0.5}]}]}"#;
    let bytes = module(manifest, 1, "(i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192))", "", "{}");
    let mut m = Manager::in_memory();
    m.install_bytes(&bytes, None).unwrap();
    assert!(m.run_command("test.plugin", "go", &json!({"n": "x"}), &[], &mut NoHost).is_err());
    assert!(m.run_command("test.plugin", "nope", &json!({}), &[], &mut NoHost).is_err());
    assert!(m.run_command("test.plugin", "go", &json!({"n": 7}), &[], &mut NoHost).is_ok());
}
