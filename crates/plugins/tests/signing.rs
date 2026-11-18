//! Author signatures: the SDK's `plugin-stamp` makes keys and signs a stamped plug-in (a
//! `photo-plugin.signature` section), the host reads the same signer, the first signed install pins
//! it, a file signed by another key or unsigned needs the user's yes, and a file that no longer
//! matches its pin doesn't load at start-up. Hostile signature sections: `robust.rs`.

use std::path::PathBuf;

use lcc_plugins::{Error, Manager, Permissions, Pin, Plugin, Signature, Signer, SignerAnswer};
use plugin_stamp::Limits;
use serde_json::{Value, json};

const ID: &str = "test.signing";

/// A module whose manifest is `manifest`; it answers every call with `{}`.
fn module(manifest: &Value) -> Vec<u8> {
    let manifest = manifest.to_string();
    let esc = |s: &str| s.bytes().map(|b| format!("\\{b:02x}")).collect::<String>();
    let src = format!(
        r#"(module
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (data (i32.const 32768) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 32768))))"#,
        m = esc(&manifest),
        ml = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

/// Version `version`, stamped, asking for the catalog.
fn stamped(version: &str) -> Vec<u8> {
    let raw = module(
        &json!({"id": ID, "name": "Signing", "version": version, "permissions": {"catalog": true}, "commands": [{"id": "go", "label": "Go"}]}),
    );
    plugin_stamp::stamp(&raw, &Limits::default()).unwrap()
}

/// Version `version`, stamped and signed with the key whose seed is all `key`.
fn signed(version: &str, key: u8) -> Vec<u8> {
    plugin_stamp::sign(&stamped(version), &[key; 32], &Limits::default()).unwrap()
}

/// The fingerprint of the key whose seed is all `key`, as the host shows it.
fn fp(key: u8) -> String {
    plugin_stamp::fingerprint(&plugin_stamp::public_key(&[key; 32]))
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn folder(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-signing-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn install(m: &mut Manager, bytes: &[u8], answer: &SignerAnswer) -> lcc_plugins::Result<String> {
    let plugin = m.load(bytes)?;
    m.install_loaded_as(plugin, bytes, None, false, false, answer).map(|i| i.plugin.manifest().version.clone())
}

fn pinned(m: &Manager) -> Option<String> {
    m.pin(ID).and_then(|p| p.signer()).map(|s| s.fingerprint())
}

fn plugins_json(dir: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("plugins.json")).unwrap()).unwrap()
}

#[test]
fn the_stamp_tool_signs_and_the_host_reads_the_same_signer() {
    let unsigned = stamped("1.0.0");
    let file = signed("1.0.0", 1);
    let public = plugin_stamp::public_key(&[1; 32]);
    assert_eq!(plugin_stamp::signature(&file).unwrap(), plugin_stamp::Signature::Valid { key: public });
    assert_eq!(plugin_stamp::signature(&unsigned).unwrap(), plugin_stamp::Signature::None);
    let inspected = Plugin::inspect(&file, lcc_plugins::Limits::default(), false).unwrap();
    let signer = inspected.signature.signer().expect("the host reads a valid signature");
    assert_eq!(signer.as_bytes(), &public, "the key the tool signed with");
    assert_eq!(signer.fingerprint(), plugin_stamp::fingerprint(&public), "both show the same fingerprint");
    assert_eq!(Plugin::inspect(&unsigned, lcc_plugins::Limits::default(), true).unwrap().signature, Signature::None);
    assert_eq!(Plugin::load(&file, lcc_plugins::Limits::default()).unwrap().signature().signer(), Some(signer), "a signed file loads");
    // the signature covers the stamped manifest too: the file's code and manifest are unchanged
    assert_eq!(plugin_stamp::read_section(&file).unwrap(), plugin_stamp::read_section(&unsigned).unwrap());
    assert!(file.starts_with(&unsigned), "signing only appends");
    assert_eq!(plugin_stamp::sign(&unsigned, &[1; 32], &Limits::default()).unwrap(), file, "the same input signs the same");
    // signing again replaces the signature: one section, the new key's
    let again = plugin_stamp::sign(&file, &[2; 32], &Limits::default()).unwrap();
    assert_eq!(plugin_stamp::signature(&again).unwrap(), plugin_stamp::Signature::Valid { key: plugin_stamp::public_key(&[2; 32]) });
    assert_eq!(again.len(), file.len(), "the old section went");
}

#[test]
fn signing_needs_a_current_stamp_and_stamping_drops_the_signature() {
    let raw = module(&json!({"id": ID, "name": "Signing", "version": "1.0.0"}));
    assert_eq!(plugin_stamp::sign(&raw, &[1; 32], &Limits::default()), Err(plugin_stamp::Error::Missing), "an unstamped file");
    // a stamp from another build of the plug-in: lcc_manifest answers differently
    let other = plugin_stamp::stamp(&module(&json!({"id": ID, "name": "Signing", "version": "2.0.0"})), &Limits::default()).unwrap();
    let section = plugin_stamp::read_section(&other).unwrap().unwrap().to_vec();
    let mut stale = raw.clone();
    stale.push(0);
    let name = plugin_stamp::SECTION.as_bytes();
    stale.push(u8::try_from(1 + name.len() + section.len()).unwrap());
    stale.push(u8::try_from(name.len()).unwrap());
    stale.extend_from_slice(name);
    stale.extend_from_slice(&section);
    assert!(matches!(plugin_stamp::sign(&stale, &[1; 32], &Limits::default()), Err(plugin_stamp::Error::Stale(_))), "a stale stamp");
    // stamping a signed file again: the signature goes, the stamp stays
    let restamped = plugin_stamp::stamp(&signed("1.0.0", 1), &Limits::default()).unwrap();
    assert_eq!(plugin_stamp::signature(&restamped).unwrap(), plugin_stamp::Signature::None);
    assert_eq!(restamped, stamped("1.0.0"));
}

#[test]
fn signing_a_file_writes_it_in_place_or_beside_it() {
    let dir = folder("file");
    let input = dir.join("p.wasm");
    std::fs::write(&input, stamped("1.0.0")).unwrap();
    let out = dir.join("signed.wasm");
    let public = plugin_stamp::sign_file(&input, Some(&out), &[3; 32], &Limits::default()).unwrap();
    assert_eq!(public, plugin_stamp::public_key(&[3; 32]));
    assert_eq!(std::fs::read(&input).unwrap(), stamped("1.0.0"), "-o leaves the input alone");
    assert_eq!(plugin_stamp::signature_file(&out).unwrap(), plugin_stamp::Signature::Valid { key: public });
    plugin_stamp::sign_file(&input, None, &[3; 32], &Limits::default()).unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), std::fs::read(&out).unwrap(), "in place");
}

#[test]
fn a_key_file_is_new_private_and_never_overwritten() {
    let dir = folder("keys");
    let path = dir.join("author.key");
    let public = plugin_stamp::new_key_file(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(plugin_stamp::SECRET_KEY_PREFIX), "{text}");
    let secret = plugin_stamp::secret_key_from_text(&text).unwrap();
    assert_eq!(plugin_stamp::public_key(&secret), public, "the file holds the key it reported");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "readable by its owner only");
    }
    let e = plugin_stamp::new_key_file(&path).unwrap_err().to_string();
    assert!(e.contains("never overwritten"), "{e}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "the key is still there");
    assert_ne!(plugin_stamp::new_key_file(&dir.join("second.key")).unwrap(), public, "each key is new");
    for bad in ["", "not a key", "photo-plugin-secret-key:!!!", "photo-plugin-secret-key:AAAA"] {
        assert!(matches!(plugin_stamp::secret_key_from_text(bad), Err(plugin_stamp::Error::Key(_))), "{bad:?}");
    }
    assert_eq!(plugin_stamp::secret_key_from_text(&format!("  {}\n\n", plugin_stamp::secret_key_text(&[9; 32]))).unwrap(), [9; 32]);
}

#[test]
fn the_first_signed_install_pins_its_key_and_the_same_signer_keeps_it() {
    let dir = folder("pin");
    let mut m = Manager::open(&dir).unwrap();
    install(&mut m, &signed("1.0.0", 1), &SignerAnswer::None).unwrap();
    assert_eq!(pinned(&m), Some(fp(1)), "pinned at the first signed install");
    let key = Signer::from_base64(plugins_json(&dir)["plugins"][ID]["signer"].as_str().expect("kept in plugins.json")).unwrap();
    assert_eq!(key.fingerprint(), fp(1));
    assert_eq!(m.get(ID).unwrap().info()["signer"], fp(1), "plugin.list shows it");
    assert!(m.get(ID).unwrap().log.iter().any(|l| l.contains(&fp(1))), "the log says so");
    // an update and a rollback by the same signer keep it, without asking
    install(&mut m, &signed("1.1.0", 1), &SignerAnswer::None).unwrap();
    assert_eq!(pinned(&m), Some(fp(1)));
    m.rollback(ID).unwrap();
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0");
    assert_eq!(pinned(&m), Some(fp(1)));
    // it holds across a restart
    drop(m);
    let mut m = Manager::open(&dir).unwrap();
    assert_eq!(pinned(&m), Some(fp(1)));
    // uninstalling removes it: a later install starts afresh
    m.uninstall(ID).unwrap();
    assert!(plugins_json(&dir)["plugins"].get(ID).is_none());
    install(&mut m, &signed("1.0.0", 2), &SignerAnswer::None).expect("a fresh install of another signer's file needs no yes");
    assert_eq!(pinned(&m), Some(fp(2)));
}

#[test]
fn another_signer_or_an_unsigned_file_installs_only_on_the_users_yes() {
    let mut m = Manager::in_memory();
    install(&mut m, &signed("1.0.0", 1), &SignerAnswer::None).unwrap();
    let e = install(&mut m, &signed("2.0.0", 2), &SignerAnswer::None).unwrap_err();
    assert!(matches!(e, Error::Signer(_)), "{e:?}");
    let e = e.to_string();
    assert!(e.contains(&fp(1)) && e.contains(&fp(2)) && e.contains("acceptSigner"), "names both signers and how to say yes: {e}");
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0", "nothing installed");
    assert_eq!(pinned(&m), Some(fp(1)), "the pin stays");
    // a yes to another fingerprint isn't a yes to this file
    assert!(matches!(install(&mut m, &signed("2.0.0", 2), &SignerAnswer::Signer(fp(3))), Err(Error::Signer(_))));
    assert!(matches!(install(&mut m, &signed("2.0.0", 2), &SignerAnswer::Unsigned), Err(Error::Signer(_))), "acceptUnsigned for a signed file");
    // the full key of another signer isn't a yes either; this file's is, as is its fingerprint
    let key = |k: u8| Signer::from_base64(&b64(&plugin_stamp::public_key(&[k; 32]))).unwrap().to_base64();
    assert!(matches!(install(&mut m, &signed("2.0.0", 2), &SignerAnswer::Signer(key(3))), Err(Error::Signer(_))), "another key");
    install(&mut m, &signed("1.5.0", 1), &SignerAnswer::None).unwrap();
    install(&mut m, &signed("2.0.0", 2), &SignerAnswer::Signer(key(2))).unwrap();
    assert_eq!(pinned(&m), Some(fp(2)), "the full key, as the dialog passes it");
    install(&mut m, &signed("2.0.1", 1), &SignerAnswer::Signer(fp(1))).unwrap();
    // the fingerprint as the user may type it: no spaces, upper case
    let typed = fp(2).replace(' ', "").to_uppercase();
    install(&mut m, &signed("2.1.0", 2), &SignerAnswer::Signer(typed)).unwrap();
    assert_eq!(pinned(&m), Some(fp(2)), "the new signer is pinned");
    assert!(m.get(ID).unwrap().log.iter().any(|l| l.contains("signer changed") && l.contains(&fp(2))));
    // an unsigned file over a signed one
    let e = install(&mut m, &stamped("3.0.0"), &SignerAnswer::None).unwrap_err().to_string();
    assert!(e.contains("isn't signed") && e.contains("acceptUnsigned"), "{e}");
    assert!(matches!(install(&mut m, &stamped("3.0.0"), &SignerAnswer::Signer(fp(2))), Err(Error::Signer(_))));
    install(&mut m, &stamped("3.0.0"), &SignerAnswer::Unsigned).unwrap();
    assert_eq!(m.pin(ID), Some(Pin::None), "no pin for an unsigned plug-in");
    // installed unsigned: its first signed version pins without asking
    install(&mut m, &signed("4.0.0", 3), &SignerAnswer::None).unwrap();
    assert_eq!(pinned(&m), Some(fp(3)));
}

#[test]
fn a_file_that_no_longer_matches_its_pin_doesnt_load_and_keeps_its_grant() {
    let dir = folder("startup");
    let mut m = Manager::open(&dir).unwrap();
    let plugin = m.load(&signed("1.0.0", 1)).unwrap();
    let grant = Permissions { catalog: true, ..Default::default() };
    m.install_loaded_as(plugin, &signed("1.0.0", 1), Some(grant.clone()), false, true, &SignerAnswer::None).unwrap();
    drop(m);
    // its data and settings, as a run would have left them
    let data = dir.join("data").join(format!("{ID}.json"));
    let settings = dir.join("settings").join(format!("{ID}.json"));
    for (f, text) in [(&data, r#"{"format":2,"libraries":{}}"#), (&settings, r#"{"format":1,"settings":{},"dialogs":{}}"#)] {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }
    let kept = || (std::fs::read(&data).ok(), std::fs::read(&settings).ok());
    let before = kept();
    let file = dir.join(format!("{ID}.wasm"));
    for (what, bytes, says) in [
        ("signed by another key", signed("1.0.0", 2), format!("it is signed by {}", fp(2))),
        ("unsigned", stamped("1.0.0"), "it isn't signed".to_string()),
        ("an invalid signature", [signed("1.0.0", 1).as_slice(), &[0, 2, 1, b'x']].concat(), "plug-in signature".to_string()),
    ] {
        std::fs::write(&file, &bytes).unwrap();
        let m = Manager::open(&dir).unwrap();
        assert!(m.get(ID).is_none(), "{what}: not loaded");
        let failed = m.failed.iter().find(|f| f.id == ID).unwrap_or_else(|| panic!("{what}: listed as failed"));
        assert!(failed.error.contains(&says), "{what}: {}", failed.error);
        if what != "an invalid signature" {
            assert!(failed.error.contains(&fp(1)), "{what}: names the pinned signer: {}", failed.error);
        }
        assert_eq!(kept(), before, "{what}: its data and settings are kept");
        assert_eq!(plugins_json(&dir)["plugins"][ID]["grant"]["catalog"], true, "{what}: and its grant");
    }
    // the user installs it again and says yes: it keeps the grant it had
    std::fs::write(&file, signed("1.0.0", 2)).unwrap();
    let mut m = Manager::open(&dir).unwrap();
    assert!(matches!(install(&mut m, &signed("1.0.0", 2), &SignerAnswer::None), Err(Error::Signer(_))), "still needs the yes");
    install(&mut m, &signed("1.0.0", 2), &SignerAnswer::Signer(fp(2))).unwrap();
    assert_eq!(m.get(ID).unwrap().grant, grant, "the grant was kept for it");
    assert_eq!(pinned(&m), Some(fp(2)));
}

#[test]
fn a_pin_that_cant_be_read_stops_loading_and_asks_on_install() {
    let dir = folder("unreadable");
    let mut m = Manager::open(&dir).unwrap();
    install(&mut m, &signed("1.0.0", 1), &SignerAnswer::None).unwrap();
    drop(m);
    let mut state = plugins_json(&dir);
    state["plugins"][ID]["signer"] = json!("not a key");
    std::fs::write(dir.join("plugins.json"), serde_json::to_vec(&state).unwrap()).unwrap();
    let mut m = Manager::open(&dir).unwrap();
    assert!(m.get(ID).is_none());
    assert!(m.failed.iter().any(|f| f.id == ID && f.error.contains("can't be read")), "{:?}", m.failed);
    let e = install(&mut m, &signed("1.0.0", 1), &SignerAnswer::None).unwrap_err().to_string();
    assert!(e.contains("can't be read") && e.contains(&fp(1)), "{e}");
    install(&mut m, &signed("1.0.0", 1), &SignerAnswer::Signer(fp(1))).unwrap();
    assert_eq!(pinned(&m), Some(fp(1)), "pinned again");
}

#[test]
fn a_plugin_installed_before_signatures_existed_pins_its_signer_at_start_up() {
    let dir = folder("before");
    // an entry with no signer: installed unsigned, or before pins were kept
    std::fs::write(dir.join(format!("{ID}.wasm")), signed("1.0.0", 1)).unwrap();
    // and the entry of a plug-in this start doesn't load (its module was moved away for now)
    let away = json!({"enabled": true, "grant": {"catalog": true}});
    std::fs::write(
        dir.join("plugins.json"),
        serde_json::to_vec(&json!({"plugins": {ID: {"enabled": true, "grant": {}}, "test.away": away}})).unwrap(),
    )
    .unwrap();
    let mut m = Manager::open(&dir).unwrap();
    assert!(m.get(ID).is_some_and(|i| i.enabled), "it loads as it did");
    assert_eq!(pinned(&m), Some(fp(1)), "its signer is pinned");
    let key = plugins_json(&dir)["plugins"][ID]["signer"].as_str().map(str::to_string);
    assert_eq!(key.map(|k| Signer::from_base64(&k).unwrap().fingerprint()), Some(fp(1)), "and written at once");
    let kept = &plugins_json(&dir)["plugins"]["test.away"];
    assert_eq!((&kept["enabled"], &kept["grant"]["catalog"]), (&away["enabled"], &json!(true)), "a save nobody asked for drops no entry");
    assert!(matches!(install(&mut m, &signed("1.1.0", 2), &SignerAnswer::None), Err(Error::Signer(_))), "and holds from now on");
    drop(m);
    // a file swapped in now, before any other change was saved, doesn't take its place
    std::fs::write(dir.join(format!("{ID}.wasm")), signed("1.0.0", 2)).unwrap();
    assert!(Manager::open(&dir).unwrap().get(ID).is_none(), "the pin holds across the restart");

    // a plugins.json that can't be read is never written over by those pins: its grants stay in it
    let dir = folder("before-unreadable");
    std::fs::write(dir.join(format!("{ID}.wasm")), signed("1.0.0", 1)).unwrap();
    std::fs::write(dir.join("plugins.json"), b"{ not json").unwrap();
    let m = Manager::open(&dir).unwrap();
    assert!(m.get(ID).is_some(), "it loads, disabled");
    assert_eq!(std::fs::read(dir.join("plugins.json")).unwrap(), b"{ not json");
}

/// AC-10: an update records the signer of the version it replaces; going back checks the kept file
/// against it, and asks as an install does.
#[test]
fn going_back_checks_the_kept_file_against_the_signer_recorded_with_it() {
    let setup = |name: &str| {
        let d = folder(name);
        let mut m = Manager::open(&d).unwrap();
        install(&mut m, &signed("1.0.0", 1), &SignerAnswer::None).unwrap();
        install(&mut m, &signed("2.0.0", 1), &SignerAnswer::None).unwrap();
        (d, m)
    };
    let (d, _m) = setup("rollback-recorded");
    let recorded: Value = serde_json::from_slice(&std::fs::read(d.join("previous").join(format!("{ID}.json"))).unwrap()).unwrap();
    assert_eq!(Signer::from_base64(recorded["signer"].as_str().unwrap()).unwrap().fingerprint(), fp(1), "recorded beside it");

    // swapped for another signer's file: refused naming both, then the user's yes
    let (d, mut m) = setup("rollback-other");
    std::fs::write(d.join("previous").join(format!("{ID}.wasm")), signed("1.0.0", 2)).unwrap();
    let e = m.rollback(ID).unwrap_err();
    assert!(matches!(e, Error::Signer(_)), "{e:?}");
    assert!(e.to_string().contains(&fp(1)) && e.to_string().contains(&fp(2)), "{e}");
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "2.0.0", "nothing changed");
    assert!(matches!(m.rollback_as(ID, false, &SignerAnswer::Signer(fp(3))), Err(Error::Signer(_))), "a yes to another key");
    m.rollback_as(ID, false, &SignerAnswer::Signer(fp(2))).unwrap();
    assert_eq!((m.get(ID).unwrap().plugin.manifest().version.as_str(), pinned(&m)), ("1.0.0", Some(fp(2))));
    assert!(m.get(ID).unwrap().log.iter().any(|l| l.contains("went back") && l.contains(&fp(2))), "{:?}", m.get(ID).unwrap().log);

    // swapped for an unsigned file
    let (d, mut m) = setup("rollback-unsigned");
    std::fs::write(d.join("previous").join(format!("{ID}.wasm")), stamped("1.0.0")).unwrap();
    assert!(m.rollback(ID).unwrap_err().to_string().contains("isn't signed"));
    m.rollback_as(ID, false, &SignerAnswer::Unsigned).unwrap();
    assert_eq!(m.pin(ID), Some(Pin::None));

    // with an invalid signature: refused, whatever the answer
    let (d, mut m) = setup("rollback-invalid");
    std::fs::write(d.join("previous").join(format!("{ID}.wasm")), [signed("1.0.0", 1).as_slice(), &[0, 2, 1, b'x']].concat()).unwrap();
    assert!(m.rollback_as(ID, false, &SignerAnswer::Signer(fp(1))).unwrap_err().to_string().contains("plug-in signature"));

    // kept unsigned, then signed by someone: that's a change too
    let d = folder("rollback-was-unsigned");
    let mut m = Manager::open(&d).unwrap();
    install(&mut m, &stamped("1.0.0"), &SignerAnswer::None).unwrap();
    install(&mut m, &signed("2.0.0", 1), &SignerAnswer::None).unwrap();
    std::fs::write(d.join("previous").join(format!("{ID}.wasm")), signed("1.0.0", 2)).unwrap();
    assert!(matches!(m.rollback(ID), Err(Error::Signer(_))));

    // kept before signers were recorded: goes back as it did
    let (d, mut m) = setup("rollback-legacy");
    let about = d.join("previous").join(format!("{ID}.json"));
    let mut v: Value = serde_json::from_slice(&std::fs::read(&about).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("signer");
    std::fs::write(&about, serde_json::to_vec(&v).unwrap()).unwrap();
    std::fs::write(d.join("previous").join(format!("{ID}.wasm")), signed("1.0.0", 2)).unwrap();
    m.rollback(ID).expect("no signer recorded: nothing to check against");
    assert_eq!(pinned(&m), Some(fp(2)));
}
