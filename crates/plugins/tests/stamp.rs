//! The SDK's manifest stamp (`sdk/stamp`, `plugin-stamp`): the `photo-plugin.manifest` section it
//! writes equals what `lcc_manifest` returns, stamped plug-ins install and run as before, and a
//! module that can't answer is refused with both files left alone. The host reads the section
//! without running the module ([`Plugin::inspect`]) and refuses to load one whose section isn't
//! `lcc_manifest`'s answer.

use std::path::{Path, PathBuf};

use lcc_plugins::{MANIFEST_SECTION, Manager, ManifestSource, NoHost, Permissions, Plugin};
use plugin_stamp::{Error, Limits, SECTION};
use serde_json::{Value, json};

const MANIFEST: &str = r#"{"id":"test.stamp","name":"Stamp","version":"1.0.0","permissions":{"catalog":true},"commands":[{"id":"go","label":"Go"}]}"#;

fn esc(b: &[u8]) -> String {
    b.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// The packed `(len << 32) | ptr` an ABI export returns.
fn packed(ptr: usize, len: usize) -> String {
    format!("(i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const {ptr}))")
}

/// A plug-in with `data` at offset 16, `lcc_manifest` running `body`, and `extra` module fields
/// (custom sections, more exports).
fn module(data: &[u8], body: &str, extra: &str) -> Vec<u8> {
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (import "host" "log" (func $log (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{d}")
  (data (i32.const 32768) "{{\22ok\22:true}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) {body})
  (func (export "lcc_alloc") (param $n i32) (result i32) (i32.const 40000))
  (func (export "lcc_handle") (param $ptr i32) (param $len i32) (result i64) {reply})
  {extra}
)"#,
        d = esc(data),
        reply = packed(32768, 11),
    );
    wat::parse_str(&src).unwrap()
}

/// A plug-in whose `lcc_manifest` returns `manifest` as stored.
fn plain(manifest: &str, extra: &str) -> Vec<u8> {
    module(manifest.as_bytes(), &packed(16, manifest.len()), extra)
}

fn custom(name: &str, data: &[u8]) -> String {
    format!("(@custom \"{name}\" \"{}\")", esc(data))
}

fn temp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("stamp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn the_section_is_what_lcc_manifest_returns_even_when_built_at_run_time() {
    // the stored bytes say version "X.0.0"; lcc_manifest writes a '1' over the X before answering
    let stored = r#"{"id":"test.runtime","name":"Runtime","version":"X.0.0"}"#;
    let x = 16 + stored.find('X').unwrap();
    let body = format!("(i32.store8 (i32.const {x}) (i32.const 0x31)) {}", packed(16, stored.len()));
    let wasm = module(stored.as_bytes(), &body, "");
    let stamped = plugin_stamp::stamp(&wasm, &Limits::default()).unwrap();
    let section = plugin_stamp::read_section(&stamped).unwrap().expect("a section");
    assert_eq!(section, stored.replace('X', "1").as_bytes(), "the section holds the answer, not the stored bytes");
    let loaded = Plugin::load(&stamped, lcc_plugins::Limits::default()).unwrap();
    assert_eq!(loaded.manifest().version, "1.0.0", "and the host reads the same manifest from lcc_manifest");
    assert_eq!(plugin_stamp::check(&stamped, &Limits::default()).unwrap(), section, "the check agrees");
}

#[test]
fn stamping_again_replaces_the_section_and_keeps_everything_else() {
    let keep = custom("other", b"keep me");
    let unstamped = plain(MANIFEST, &keep);
    // the same module carrying two old sections of ours, one before and one after the other custom section
    let old = format!("{} {keep} {}", custom(SECTION, b"{\"old\":1}"), custom(SECTION, b"older"));
    let with_old = plain(MANIFEST, &old);
    assert!(matches!(plugin_stamp::read_section(&with_old), Err(Error::Duplicate(2))));

    let once = plugin_stamp::stamp(&unstamped, &Limits::default()).unwrap();
    assert!(once.starts_with(&unstamped), "every section of the module is kept as it was, in its place");
    assert_eq!(plugin_stamp::read_section(&once).unwrap(), Some(MANIFEST.as_bytes()));
    assert_eq!(plugin_stamp::stamp(&once, &Limits::default()).unwrap(), once, "stamping a stamped module changes nothing");
    assert_eq!(plugin_stamp::stamp(&with_old, &Limits::default()).unwrap(), once, "old sections are replaced by exactly one");
    assert_eq!(plugin_stamp::stamp(&unstamped, &Limits::default()).unwrap(), once, "the same input gives the same bytes");

    let p = Plugin::load(&once, lcc_plugins::Limits::default()).expect("the stamped module validates and loads");
    assert_eq!(p.id(), "test.stamp");
}

#[test]
fn modules_that_cannot_answer_are_refused_and_both_files_are_left_alone() {
    let not_utf8: &[u8] = b"{\"id\":\"\xff\xfe\"}";
    let looping = "(loop $l (br $l)) (i64.const 0)";
    let no_handle = format!(
        r#"(module (memory (export "memory") 1) (data (i32.const 16) "{}") (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) {}) (func (export "lcc_alloc") (param i32) (result i32) (i32.const 0)))"#,
        esc(MANIFEST.as_bytes()),
        packed(16, MANIFEST.len())
    );
    // a complete plug-in, but one that also imports a WASI function
    let wasi = format!(
        r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) {} (func (export "lcc_handle") (param i32 i32) (result i64) (i64.const 0)))"#,
        no_handle.strip_prefix("(module ").unwrap().strip_suffix(')').unwrap()
    );
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "no lcc_manifest",
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "lcc_abi_version") (result i32) (i32.const 1)))"#).unwrap(),
            "lcc_manifest",
        ),
        ("not a plug-in", wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap(), "lcc_abi_version"),
        ("a trap", module(b"{}", "unreachable", ""), "trapped"),
        ("a host call", module(b"{}", &format!("(call $log (i32.const 16) (i32.const 2)) {}", packed(16, 2)), ""), "host.log"),
        ("out of fuel", module(b"{}", looping, ""), "fuel"),
        ("oversized", module(b"{}", &packed(16, 70_000), ""), "70000 bytes"),
        ("outside memory", module(b"{}", &packed(200_000, 2), ""), "outside memory"),
        ("not UTF-8", module(not_utf8, &packed(16, not_utf8.len()), ""), "UTF-8"),
        ("not an object", plain("[1,2]", ""), "JSON object"),
        ("not JSON", plain("hello", ""), "isn't JSON"),
        ("not wasm", b"\0asm\x01\0\0\0\x01\x05".to_vec(), "section 0"),
        ("a WASI import", wat::parse_str(&wasi).unwrap(), "no WASI"),
        ("no lcc_handle", wat::parse_str(&no_handle).unwrap(), "lcc_handle"),
    ];
    let limits = Limits { fuel: 2_000_000, ..Limits::default() };
    for (what, wasm, needle) in cases {
        let err = plugin_stamp::stamp(&wasm, &limits).expect_err(what).to_string();
        assert!(err.contains(needle), "{what}: {err}");

        let dir = temp(&what.replace(' ', "-"));
        let (input, output) = (dir.join("in.wasm"), dir.join("out.wasm"));
        std::fs::write(&input, &wasm).unwrap();
        std::fs::write(&output, b"previous").unwrap();
        assert!(plugin_stamp::stamp_file(&input, Some(&output), &limits).is_err(), "{what}");
        assert!(plugin_stamp::stamp_file(&input, None, &limits).is_err(), "{what}");
        assert_eq!(std::fs::read(&input).unwrap(), wasm, "{what}: the input is left as it was");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous", "{what}: so is the output");
        assert_eq!(files_in(&dir), ["in.wasm", "out.wasm"], "{what}: no temporary file is left behind");
    }
}

#[test]
fn stamp_file_writes_the_output_or_the_input_in_place() {
    let dir = temp("files");
    let (input, output) = (dir.join("in.wasm"), dir.join("out.wasm"));
    let wasm = plain(MANIFEST, "");
    std::fs::write(&input, &wasm).unwrap();
    assert_eq!(plugin_stamp::stamp_file(&input, Some(&output), &Limits::default()).unwrap(), MANIFEST.as_bytes());
    assert_eq!(std::fs::read(&input).unwrap(), wasm, "-o leaves the input alone");
    let stamped = std::fs::read(&output).unwrap();
    assert_eq!(plugin_stamp::read_section(&stamped).unwrap(), Some(MANIFEST.as_bytes()));
    plugin_stamp::stamp_file(&input, None, &Limits::default()).unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), stamped, "without -o the input is stamped in place");
    assert_eq!(files_in(&dir), ["in.wasm", "out.wasm"]);
}

#[test]
fn a_failed_write_leaves_no_temporary_file_and_permissions_are_kept() {
    let dir = temp("write");
    let input = dir.join("in.wasm");
    let wasm = plain(MANIFEST, "");
    std::fs::write(&input, &wasm).unwrap();
    std::fs::create_dir(dir.join("folder")).unwrap();
    let err = plugin_stamp::stamp_file(&input, Some(&dir.join("folder")), &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Io(_)), "a folder as the output is an I/O error: {err}");
    assert!(dir.join("folder").is_dir(), "the folder is left as it was");
    assert_eq!(files_in(&dir), ["folder", "in.wasm"], "no temporary file is left behind");

    // a new output takes the input's permissions; a stamp in place keeps them
    let mut readonly = std::fs::metadata(&input).unwrap().permissions();
    readonly.set_readonly(true);
    std::fs::set_permissions(&input, readonly).unwrap();
    let output = dir.join("out.wasm");
    plugin_stamp::stamp_file(&input, Some(&output), &Limits::default()).unwrap();
    assert!(std::fs::metadata(&output).unwrap().permissions().readonly(), "the output is read-only like the input");
    let in_place = plugin_stamp::stamp_file(&input, None, &Limits::default());
    let now = std::fs::read(&input).unwrap();
    match in_place {
        // Windows may refuse to replace a read-only file: then the file is left as it was
        Err(_) if cfg!(windows) => assert_eq!(now, wasm),
        Err(e) => panic!("stamping a read-only file in place: {e}"),
        Ok(_) => assert!(plugin_stamp::read_section(&now).unwrap().is_some(), "stamped in place"),
    }
    assert!(std::fs::metadata(&input).unwrap().permissions().readonly(), "and it stays read-only");
    assert_eq!(files_in(&dir), ["folder", "in.wasm", "out.wasm"], "no temporary file is left behind");
    for f in [&input, &output] {
        let mut p = std::fs::metadata(f).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        std::fs::set_permissions(f, p).unwrap();
    }
}

#[test]
fn stamped_plugins_install_and_run_like_unstamped_ones() {
    let unstamped = plain(MANIFEST, "");
    let stamped = plugin_stamp::stamp(&unstamped, &Limits::default()).unwrap();
    let mut outputs = Vec::new();
    for (what, wasm) in [("stamped", &stamped), ("unstamped", &unstamped)] {
        let mut m = Manager::in_memory();
        let grant = Permissions { catalog: true, ..Default::default() };
        let info = m.install_bytes_by_user(wasm, Some(grant), false).unwrap_or_else(|e| panic!("{what}: {e}")).info();
        assert_eq!(info["id"], "test.stamp", "{what}");
        m.set_enabled("test.stamp", true).unwrap();
        let out = m.run_command("test.stamp", "go", &json!({}), &[], &mut NoHost).unwrap_or_else(|e| panic!("{what}: {e}"));
        outputs.push(out);
    }
    assert_eq!(outputs[0], outputs[1], "the command answers the same either way");
}

#[test]
fn check_passes_only_a_matching_section_and_writes_nothing() {
    let limits = Limits::default();
    let stamped = plugin_stamp::stamp(&plain(MANIFEST, ""), &limits).unwrap();
    assert_eq!(plugin_stamp::check(&stamped, &limits).unwrap(), MANIFEST.as_bytes());
    assert_eq!(plugin_stamp::check(&plain(MANIFEST, ""), &limits), Err(Error::Missing));

    // a section left over from an earlier build: the version and a command changed since
    let newer = MANIFEST.replace("1.0.0", "1.1.0").replace("\"Go\"", "\"Run\"");
    let stale = plain(&newer, &custom(SECTION, MANIFEST.as_bytes()));
    let err = plugin_stamp::check(&stale, &limits).unwrap_err();
    assert_eq!(err, Error::Stale(vec!["commands".into(), "version".into()]));
    assert!(err.to_string().contains("commands, version"), "{err}");
    let garbage = plain(MANIFEST, &custom(SECTION, b"not json"));
    assert_eq!(plugin_stamp::check(&garbage, &limits), Err(Error::Stale(vec![])));

    let twice = format!("{0} {0}", custom(SECTION, MANIFEST.as_bytes()));
    assert_eq!(plugin_stamp::check(&plain(MANIFEST, &twice), &limits), Err(Error::Duplicate(2)));

    let dir = temp("check");
    let file = dir.join("p.wasm");
    for wasm in [&stamped, &stale] {
        std::fs::write(&file, wasm).unwrap();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let _ = plugin_stamp::check_file(&file, &limits);
        assert_eq!(&std::fs::read(&file).unwrap(), wasm, "--check writes nothing");
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
        assert_eq!(files_in(&dir), ["p.wasm"]);
    }
    let section: Value = serde_json::from_slice(plugin_stamp::read_section(&stamped).unwrap().unwrap()).unwrap();
    assert_eq!(section["id"], "test.stamp");
}

#[test]
fn the_host_reads_the_section_the_tool_writes() {
    assert_eq!(MANIFEST_SECTION, SECTION, "one name on both sides");
}

#[test]
fn a_stamped_module_inspects_from_its_section_without_running() {
    // lcc_manifest traps, and so does the start function: inspecting must run neither
    let traps = module(b"", "(unreachable)", &format!("(func $start (unreachable)) (start $start) {}", custom(SECTION, MANIFEST.as_bytes())));
    let limits = lcc_plugins::Limits::default();
    let inspected = Plugin::inspect(&traps, limits.clone(), false).unwrap();
    assert_eq!(inspected.source, ManifestSource::Section);
    assert!(!inspected.verified(), "a section read without running is only the file's claim");
    assert_eq!(inspected.manifest.id, "test.stamp");
    assert!(inspected.manifest.permissions.catalog, "the section's permissions");
    let e = Plugin::inspect(&traps, limits.clone(), true).unwrap_err();
    assert!(matches!(e, lcc_plugins::Error::Trap(_)), "verify runs lcc_manifest: {e}");
    assert!(Plugin::load(&traps, limits.clone()).is_err(), "and loading runs it");

    let stamped = plugin_stamp::stamp(&plain(MANIFEST, ""), &Limits::default()).unwrap();
    let verified = Plugin::inspect(&stamped, limits.clone(), true).unwrap();
    assert_eq!((verified.source, verified.verified()), (ManifestSource::Export, true));
    let unverified = Plugin::inspect(&stamped, limits.clone(), false).unwrap();
    assert_eq!(unverified.manifest, verified.manifest, "a true section reads as lcc_manifest does");
    assert_eq!(unverified.digest, *Plugin::load(&stamped, limits).unwrap().digest(), "the digest is the module's");
}

#[test]
fn a_module_without_a_section_inspects_through_lcc_manifest() {
    let inspected = Plugin::inspect(&plain(MANIFEST, ""), lcc_plugins::Limits::default(), false).unwrap();
    assert_eq!((inspected.source, inspected.verified()), (ManifestSource::Export, true));
    assert_eq!(inspected.manifest.id, "test.stamp");
    let other = plain(MANIFEST, &custom("name", b"x"));
    assert_eq!(Plugin::inspect(&other, lcc_plugins::Limits::default(), false).unwrap().source, ManifestSource::Export, "other custom sections");
}

#[test]
fn a_section_inspect_still_refuses_what_install_would() {
    let section = custom(SECTION, MANIFEST.as_bytes());
    let wasi = format!(r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) {section})"#);
    let e = Plugin::inspect(&wat::parse_str(&wasi).unwrap(), lcc_plugins::Limits::default(), false).unwrap_err().to_string();
    assert!(e.contains("wasi_snapshot_preview1.fd_write"), "{e}");
    let no_handle = format!(
        r#"(module (memory (export "memory") 1)
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.const 0))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 0))
  (func (export "lcc_handle") (param i32) (result i64) (i64.const 0))
  {section})"#
    );
    let e = Plugin::inspect(&wat::parse_str(&no_handle).unwrap(), lcc_plugins::Limits::default(), false).unwrap_err().to_string();
    assert!(e.contains("lcc_handle"), "the wrong type for lcc_handle: {e}");
    let bad_json = plain(MANIFEST, &custom(SECTION, b"{\"id\":"));
    let e = Plugin::inspect(&bad_json, lcc_plugins::Limits::default(), false).unwrap_err();
    assert!(matches!(e, lcc_plugins::Error::Manifest(_)), "{e}");
}

/// A section that differs from `lcc_manifest` (it hides a permission and an older version): the
/// install, an update and start-up all refuse it, naming the fields.
#[test]
fn a_section_that_differs_from_lcc_manifest_is_refused_everywhere() {
    let claim = MANIFEST.replace(r#""catalog":true"#, r#""catalog":false"#).replace("1.0.0", "0.9.0");
    let lying = plain(MANIFEST, &custom(SECTION, claim.as_bytes()));
    let limits = lcc_plugins::Limits::default();
    let e = Plugin::load(&lying, limits.clone()).unwrap_err().to_string();
    assert!(e.contains("`permissions`, `version`") && e.contains(SECTION), "{e}");
    assert_eq!(Plugin::inspect(&lying, limits.clone(), true).unwrap_err().to_string(), e, "verify refuses it the same way");
    assert!(!Plugin::inspect(&lying, limits.clone(), false).unwrap().manifest.permissions.catalog, "unverified, it reads as it claims");
    let respaced = plain(MANIFEST, &custom(SECTION, MANIFEST.replace(',', ", ").as_bytes()));
    let e2 = Plugin::load(&respaced, limits.clone()).unwrap_err().to_string();
    assert!(e2.contains("byte for byte"), "the same fields written differently: {e2}");
    let not_json = plain(MANIFEST, &custom(SECTION, b"nope"));
    assert!(Plugin::load(&not_json, limits.clone()).unwrap_err().to_string().contains("same JSON object"));

    // a first install: nothing is installed
    let grant = Permissions { catalog: true, ..Default::default() };
    let mut m = Manager::in_memory();
    let e = m.install_bytes_by_user(&lying, Some(grant.clone()), true).unwrap_err().to_string();
    assert!(e.contains("`permissions`, `version`"), "{e}");
    assert_eq!(m.list().count(), 0, "nothing installed");

    // an update over a good install: the installed version stays as it was
    let dir = temp("lying");
    let mut m = Manager::open(&dir).unwrap();
    let good = plugin_stamp::stamp(&plain(MANIFEST, ""), &Limits::default()).unwrap();
    m.install_bytes_by_user(&good, Some(grant.clone()), false).unwrap();
    let newer = plain(&MANIFEST.replace("1.0.0", "1.1.0"), &custom(SECTION, MANIFEST.as_bytes()));
    let e = m.install_bytes_by_user(&newer, Some(grant), false).unwrap_err().to_string();
    assert!(e.contains("`version`"), "{e}");
    assert_eq!(m.get("test.stamp").unwrap().plugin.manifest().version, "1.0.0", "the installed plug-in is kept");
    let installed_file = dir.join("test.stamp.wasm");
    assert_eq!(std::fs::read(&installed_file).unwrap(), good, "and its file");

    // start-up: a module copied over the installed one is listed as failed, with the reason
    drop(m);
    std::fs::write(&installed_file, &lying).unwrap();
    let m = Manager::open(&dir).unwrap();
    assert!(m.get("test.stamp").is_none(), "it doesn't load");
    let failed = m.failed.iter().find(|f| f.id == "test.stamp").expect("listed as failed");
    assert!(failed.error.contains("`permissions`, `version`"), "{}", failed.error);
    let _ = std::fs::remove_dir_all(&dir);
}
