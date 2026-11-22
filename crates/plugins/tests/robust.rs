//! Never-crash (P6.2), beyond the ABI tests: mutated WebAssembly modules, manifests, replies
//! (commands and the publish hook), `plugins.json` and the per-plug-in store files are errors,
//! never a panic or a hang.

use lcc_plugins::publish::{PluginPublisher, PublishItem, PublishProvider};
use lcc_plugins::{Limits, Manager, Manifest, NamespaceStore, NoHost, Permissions, Plugin};
use serde_json::json;

const MANIFEST: &str = r#"{"id":"test.plugin","name":"Test","version":"1.0.0","permissions":{"catalog":true},"commands":[{"id":"go","label":"Go"}],"publish":{"id":"svc","name":"Svc"}}"#;

/// A module whose manifest is `manifest` and whose `lcc_handle` returns `reply`.
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
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "lcc_handle") (param $ptr i32) (param $len i32) (result i64)
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
fn mutated_modules_never_panic() {
    let seeds = [module(MANIFEST, r#"{"ok":true}"#), module(MANIFEST, r#"{"remoteId":"r1","url":"https://x"}"#)];
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    lcc_fuzzkit::run("plugins.wasm", &refs, 400, |b| {
        if Plugin::load(b, limits()).is_err() {
            return;
        }
        let mut m = Manager::in_memory();
        m.set_limits(limits());
        let Ok(p) = m.install_bytes_by_user(b, Some(grant()), false) else { return };
        let id = p.plugin.id().to_string();
        let _ = m.set_enabled(&id, true);
        let _ = m.run_command(&id, "go", &json!({}), &["1".into()], &mut NoHost);
        let _ = m.post_export(&[json!({"path": "/x.jpg"})], &mut NoHost);
    });
}

#[test]
fn manifests_never_panic() {
    let seeds = [MANIFEST, r#"{"id":"t","name":"T","version":"2.0.0-rc.1+b.7","requires":["jobs","settings"]}"#];
    lcc_fuzzkit::run_json("plugins.manifest", &seeds, 3000, |s| {
        if let Ok(m) = Manifest::parse(s.as_bytes()) {
            let _ = m.validate();
            let _ = (m.semver(), m.missing_features(&["jobs"]));
        }
    });
}

/// The SDK's manifest stamp (`sdk/stamp`) on mutated and malformed modules: errors, never a panic.
#[test]
fn stamped_sections_never_panic() {
    let sandbox = plugin_stamp::Limits { fuel: 5_000_000, ..Default::default() };
    let stamped = plugin_stamp::stamp(&module(MANIFEST, "{}"), &sandbox).unwrap();
    lcc_fuzzkit::run("plugins.stamp", &[stamped.as_slice()], 300, |b| {
        let _ = plugin_stamp::read_section(b);
        let _ = plugin_stamp::check(b, &sandbox);
        let _ = plugin_stamp::stamp(b, &sandbox);
    });
    for cut in 0..stamped.len() {
        let _ = plugin_stamp::read_section(&stamped[..cut]);
        let _ = plugin_stamp::check(&stamped[..cut], &sandbox);
    }
    let cut = &stamped[..stamped.len() - 1];
    assert!(plugin_stamp::read_section(cut).unwrap_err().to_string().contains("past the end"), "a truncated module");
    let hostile: [(&str, &[u8], &str); 4] = [
        ("a length over 32 bits", &[0, 0xff, 0xff, 0xff, 0xff, 0x7f], "bad length"),
        ("a length that never ends", &[0, 0x80], "bad length"),
        ("a name that isn't UTF-8", &[0, 3, 2, 0xff, 0xfe], "UTF-8"),
        ("a name longer than its section", &[0, 2, 9, b'x'], "runs past"),
    ];
    for (what, tail, needle) in hostile {
        let wasm = [stamped.as_slice(), tail].concat();
        let err = plugin_stamp::read_section(&wasm).expect_err(what).to_string();
        assert!(err.contains(needle), "{what}: {err}");
        assert!(plugin_stamp::check(&wasm, &sandbox).is_err(), "{what}");
        assert!(plugin_stamp::stamp(&wasm, &sandbox).is_err(), "{what}");
    }
    let twice = plugin_stamp::stamp(&module(MANIFEST, "{}"), &sandbox).unwrap();
    let section = &twice[module(MANIFEST, "{}").len()..];
    let doubled = [twice.as_slice(), section].concat();
    assert_eq!(plugin_stamp::read_section(&doubled), Err(plugin_stamp::Error::Duplicate(2)), "two sections");
    assert_eq!(plugin_stamp::check(&doubled, &sandbox), Err(plugin_stamp::Error::Duplicate(2)));
}

/// The host's own read of a stamped section ([`Plugin::inspect`], and [`Plugin::load`]'s check
/// against `lcc_manifest`) on mutated and malformed modules: errors, never a panic.
#[test]
fn inspected_sections_never_panic() {
    let limits = lcc_plugins::Limits { fuel: 5_000_000, ..Default::default() };
    let unstamped = module(MANIFEST, "{}");
    let stamped = plugin_stamp::stamp(&unstamped, &plugin_stamp::Limits::default()).unwrap();
    assert_eq!(Plugin::inspect(&stamped, limits.clone(), false).unwrap().source, lcc_plugins::ManifestSource::Section);
    lcc_fuzzkit::run("plugins.inspect", &[stamped.as_slice()], 300, |b| {
        let _ = Plugin::inspect(b, limits.clone(), false);
        let _ = Plugin::inspect(b, limits.clone(), true);
    });
    // the section goes last: a truncated module fails, or (cut at a section boundary) has no section left
    for cut in 0..stamped.len() {
        let read = Plugin::inspect(&stamped[..cut], limits.clone(), false);
        assert!(read.as_ref().map_or(true, |i| i.source == lcc_plugins::ManifestSource::Export), "cut at {cut}");
    }
    let section = |data: &[u8]| {
        let name = lcc_plugins::MANIFEST_SECTION.as_bytes();
        let mut payload = vec![u8::try_from(name.len()).unwrap()];
        payload.extend_from_slice(name);
        payload.extend_from_slice(data);
        let mut out = vec![0u8];
        let mut n = payload.len();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            out.push(if n == 0 { b } else { b | 0x80 });
            if n == 0 {
                break;
            }
        }
        out.extend_from_slice(&payload);
        out
    };
    let oversized = format!(r#"{{"id":"t","name":"T","version":"1.0.0","pad":"{}"}}"#, "x".repeat(lcc_plugins::manifest::MAX_MANIFEST_BYTES));
    let hostile: [(&str, Vec<u8>, &str); 6] = [
        ("a length over 32 bits", [unstamped.as_slice(), &[0, 0xff, 0xff, 0xff, 0xff, 0x7f]].concat(), "not a valid plug-in module"),
        ("a length past the end", [unstamped.as_slice(), &[0, 9, 1, b'x']].concat(), "not a valid plug-in module"),
        ("two sections", [stamped.as_slice(), &section(MANIFEST.as_bytes())].concat(), "2 photo-plugin.manifest sections"),
        ("an oversized section", [unstamped.as_slice(), &section(oversized.as_bytes())].concat(), "limit 65536"),
        ("invalid UTF-8", [unstamped.as_slice(), &section(b"{\"id\":\"\xff\"}")].concat(), "plug-in manifest"),
        ("invalid JSON", [unstamped.as_slice(), &section(b"{\"id\"")].concat(), "plug-in manifest"),
    ];
    for (what, wasm, needle) in hostile {
        let e = Plugin::inspect(&wasm, limits.clone(), false).expect_err(what).to_string();
        assert!(e.contains(needle), "{what}: {e}");
        assert!(Plugin::load(&wasm, limits.clone()).is_err(), "{what}: loading refuses it too");
    }
}

/// A custom section `name` holding `data`, with the shortest lengths.
fn custom_section(name: &str, data: &[u8]) -> Vec<u8> {
    let leb = |out: &mut Vec<u8>, mut n: usize| loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        out.push(if n == 0 { b } else { b | 0x80 });
        if n == 0 {
            break;
        }
    };
    let mut payload = Vec::new();
    leb(&mut payload, name.len());
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(data);
    let mut out = vec![0u8];
    leb(&mut out, payload.len());
    out.extend_from_slice(&payload);
    out
}

/// Author signature sections that don't hold up, on every path that reads one (`plugin-stamp`,
/// [`Plugin::inspect`], [`Plugin::load`], installing): each is "invalid" with why, never read as
/// unsigned, never installed, and mutated or truncated signed modules never panic.
#[test]
fn hostile_signature_sections_are_invalid_and_never_panic() {
    let limits = lcc_plugins::Limits { fuel: 5_000_000, ..Default::default() };
    let stamped = plugin_stamp::stamp(&module(MANIFEST, r#"{"said":"before"}"#), &plugin_stamp::Limits::default()).unwrap();
    let signed = plugin_stamp::sign(&stamped, &[5; 32], &plugin_stamp::Limits::default()).unwrap();
    assert!(matches!(Plugin::inspect(&signed, limits.clone(), false).unwrap().signature, lcc_plugins::Signature::Valid(_)));
    let sig = |data: &[u8]| custom_section(lcc_plugins::SIGNATURE_SECTION, data);
    // the section's JSON, as plugin-stamp wrote it: the end of the signed file
    let own: serde_json::Value = {
        let tail = &signed[stamped.len()..];
        let at = tail.iter().position(|&b| b == b'{').unwrap();
        serde_json::from_slice(&tail[at..]).unwrap()
    };
    let with = |field: &str, value: serde_json::Value| {
        let mut o = own.clone();
        o[field] = value;
        [stamped.as_slice(), &sig(o.to_string().as_bytes())].concat()
    };
    let without = |field: &str| {
        let mut o = own.clone();
        o.as_object_mut().unwrap().remove(field);
        [stamped.as_slice(), &sig(o.to_string().as_bytes())].concat()
    };
    let b64 = |bytes: &[u8]| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    };
    // what the code answers, changed after signing: the module is still a valid, current plug-in
    let mut changed = signed.clone();
    let at = changed.windows(6).position(|w| w == b"before").unwrap();
    changed[at] = b'B';
    // the section with its size written in five bytes instead of the fewest: not what the signer wrote
    let padded = {
        let name = lcc_plugins::SIGNATURE_SECTION.as_bytes();
        let data = own.to_string();
        let len = 1 + name.len() + data.len();
        let mut s = vec![0, (len & 0x7f) as u8 | 0x80, ((len >> 7) & 0x7f) as u8 | 0x80, 0x80, 0x80, 0];
        s.push(u8::try_from(name.len()).unwrap());
        s.extend_from_slice(name);
        s.extend_from_slice(data.as_bytes());
        [stamped.as_slice(), &s].concat()
    };
    let pad = format!(r#"{{"v":1,"pad":"{}"}}"#, "x".repeat(lcc_plugins::MAX_SIGNATURE_BYTES));
    let hostile: Vec<(&str, Vec<u8>, &str)> = vec![
        ("changed after signing", changed, "doesn't match the file"),
        ("a section after it", [signed.as_slice(), &custom_section("x", b"y")].concat(), "isn't the file's last section"),
        ("two signature sections", [signed.as_slice(), &sig(own.to_string().as_bytes())].concat(), "2 photo-plugin.signature sections"),
        ("an oversized section", [stamped.as_slice(), &sig(pad.as_bytes())].concat(), "limit 1024"),
        ("a 31-byte key", with("key", b64(&[7; 31]).into()), "isn't 32 bytes"),
        ("a 63-byte signature", with("sig", b64(&[7; 63]).into()), "isn't 64 bytes"),
        ("a key that isn't base64", with("key", "not base64!".into()), "isn't base64"),
        ("a signature that isn't base64", with("sig", "not base64!".into()), "`sig` isn't base64"),
        ("a padded section size", padded, "isn't the file's last section"),
        ("a signature that isn't a string", with("sig", json!(7)), "no `sig` string"),
        ("no key", without("key"), "no `key` string"),
        ("format version 2", with("v", json!(2)), "format version 2"),
        ("no format version", without("v"), "no format version"),
        ("invalid JSON", [stamped.as_slice(), &sig(b"{\"v\":1,")].concat(), "isn't JSON"),
        ("not an object", [stamped.as_slice(), &sig(b"[1]")].concat(), "isn't a JSON object"),
        ("invalid UTF-8", [stamped.as_slice(), &sig(b"\xff\xfe")].concat(), "isn't JSON"),
        ("another key named", with("key", b64(&plugin_stamp::public_key(&[6; 32])).into()), "doesn't match the file"),
    ];
    for (what, wasm, why) in &hostile {
        let lcc_plugins::Signature::Invalid(host) = Plugin::inspect(wasm, limits.clone(), false).expect(what).signature else {
            panic!("{what}: the host reads it as invalid")
        };
        assert!(host.contains(why), "{what}: {host}");
        assert!(matches!(Plugin::inspect(wasm, limits.clone(), true).expect(what).signature, lcc_plugins::Signature::Invalid(_)), "{what}: verified");
        assert!(matches!(plugin_stamp::signature(wasm), Ok(plugin_stamp::Signature::Invalid(_))), "{what}: the stamp tool agrees");
        let e = Plugin::load(wasm, limits.clone()).expect_err(what).to_string();
        assert!(e.starts_with("plug-in signature:") && e.contains(why), "{what}: {e}");
        let mut m = Manager::in_memory();
        assert!(m.install_bytes(wasm, None, false).is_err(), "{what}: never installed");
        assert!(m.list().next().is_none(), "{what}");
    }
    lcc_fuzzkit::run("plugins.signature", &[signed.as_slice()], 300, |b| {
        let _ = Plugin::inspect(b, limits.clone(), false);
        let _ = Plugin::load(b, limits.clone());
        let _ = plugin_stamp::signature(b);
    });
    // the signature goes last: a truncated module is no longer validly signed
    for cut in 0..signed.len() {
        let read = Plugin::inspect(&signed[..cut], limits.clone(), false);
        assert!(read.map_or(true, |i| i.signature.signer().is_none()), "cut at {cut}");
        assert!(!matches!(plugin_stamp::signature(&signed[..cut]), Ok(plugin_stamp::Signature::Valid { .. })), "cut at {cut}");
    }
}

/// `requires` and `version` from a hostile manifest: errors, never a panic.
#[test]
fn hostile_requires_and_versions_are_errors() {
    let huge: Vec<String> = (0..100_000).map(|i| format!("f{i}")).collect();
    let requires = [json!(huge), json!([1, true, null, {}]), json!(["é"]), json!(["a".repeat(10_000)]), json!(null), json!(7)];
    for r in requires {
        let m = json!({"id": "t", "name": "T", "version": "1.0.0", "requires": r}).to_string();
        let mut mgr = Manager::in_memory();
        assert!(Manifest::parse(m.as_bytes()).is_err() || m.len() > lcc_plugins::manifest::MAX_MANIFEST_BYTES, "{r}");
        if m.len() <= lcc_plugins::manifest::MAX_MANIFEST_BYTES {
            assert!(mgr.install_bytes(&module(&m, "{}"), None, false).is_err(), "{r}");
        }
    }
    let versions = [
        "1".repeat(60_000),
        "99999999999999999999999.0.0".into(),
        "1.18446744073709551616.0".into(),
        "１.２.３".into(),
        "1.0.0-é".into(),
        "1.0.0-".into(),
        "1.0.0+".into(),
        "\u{0}1.0.0".into(),
        "01.0.0".into(),
    ];
    for v in versions {
        let man = Manifest::parse(json!({"id": "t", "name": "T", "version": v}).to_string().as_bytes()).unwrap();
        assert!(man.semver().is_err(), "{v:?}");
        let m = json!({"id": "t", "name": "T", "version": v}).to_string();
        if m.len() <= lcc_plugins::manifest::MAX_MANIFEST_BYTES {
            assert!(Manager::in_memory().install_bytes(&module(&m, "{}"), None, false).is_err(), "{v:?}");
        }
    }
}

/// Replies of a plug-in's command and publish hooks (the plug-in is untrusted code).
#[test]
fn hostile_replies_never_panic() {
    let seeds = [r#"{"remoteId":"r1","url":"https://x/1"}"#, r#"{"result":{"photos":["1","2"],"metadata":[{"photo":"1","title":"t"}]}}"#];
    lcc_fuzzkit::run_json("plugins.reply", &seeds, 150, |reply| {
        let mut m = Manager::in_memory();
        m.set_limits(limits());
        if m.install_bytes_by_user(&module(MANIFEST, reply), Some(grant()), false).is_err() {
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
    let dir = std::env::temp_dir().join(format!("lcc-plugins-robust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test.wasm"), module(MANIFEST, "{}")).unwrap();
    let state = r#"{"plugins":{"test.plugin":{"enabled":true,"grant":{"catalog":true,"fs":[{"path":"/x","write":true}]},"checkUpdates":true,
        "lastCheck":{"at":5,"version":"1.0.0","offer":{"version":"1.1.0","url":"https://x.org/a.wasm","sha256":"00","permissions":["catalog"],"notes":"n"}}}}}"#;
    lcc_fuzzkit::run_json("plugins.state", &[state], 200, |s| {
        std::fs::write(dir.join("plugins.json"), s).unwrap();
        if let Ok(m) = Manager::open(&dir) {
            for p in m.list() {
                let _ = (p.effective(), p.info(), p.updates_info(m.features()));
            }
        }
    });
    let store = r#"{"1":{"score":0.5,"tags":["a"]},"2":{}}"#;
    let file = dir.join("store.json");
    lcc_fuzzkit::run_json("plugins.store", &[store], 1000, |s| {
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

/// AC-7: an update feed, every truncation and mutation of it, offers something or nothing and never
/// panics.
#[test]
fn hostile_update_feeds_never_panic() {
    let feed = json!({"geo": {"id": "test.plugin", "version": "1.1.0", "url": "https://x.org/geo.wasm",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "requires": ["settings"],
        "permissions": ["catalog", "network"], "changelog": "https://x.org/CHANGELOG.md", "notes": "Faster \u{e9}t\u{e9} lookups."}})
    .to_string();
    let read = |b: &[u8]| lcc_plugins::updates::read_feed(b, "test.plugin", "1.0.0", &mut Vec::new());
    assert!(matches!(read(feed.as_bytes()), Ok(Some(_))));
    for n in 0..feed.len() {
        let _ = read(&feed.as_bytes()[..n]);
    }
    lcc_fuzzkit::run("plugins.feed", &[feed.as_bytes()], 2000, |b| {
        if let Ok(Some(o)) = read(b) {
            assert!(o.notes.as_ref().is_none_or(|n| n.chars().count() <= lcc_plugins::updates::MAX_NOTES_CHARS));
            let _ = (o.adds(&Permissions::default()), o.missing(&[]));
        }
    });
}
