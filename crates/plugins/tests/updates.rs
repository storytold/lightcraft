//! Plug-in updates: a manifest's `updateUrl`, the per-plug-in update check (off by default, only
//! for a stamped plug-in signed by its pinned key), the result kept a day, and installing an
//! update: the feed's SHA-256, the file's own manifest and signer, and permission groups the feed
//! didn't declare. The feed parser's own cases: `src/updates.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lcc_plugins::{Error, Fetch, Manager, Permissions, SignerAnswer, UpdateCheck};
use plugin_stamp::Limits;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const ID: &str = "test.updates";
const FEED: &str = "https://feed.test/index.json";
const DAY: i64 = 24 * 60 * 60;

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

fn manifest(version: &str, permissions: Value) -> Value {
    json!({"id": ID, "name": "Updates", "version": version, "requires": ["updates"], "updateUrl": FEED, "permissions": permissions})
}

/// Version `version` asking for the catalog, stamped and signed with the key whose seed is all `key`.
fn signed(version: &str, key: u8) -> Vec<u8> {
    signed_with(manifest(version, json!({"catalog": true})), key)
}

fn signed_with(manifest: Value, key: u8) -> Vec<u8> {
    plugin_stamp::sign(&stamped_with(manifest), &[key; 32], &Limits::default()).unwrap()
}

fn stamped_with(manifest: Value) -> Vec<u8> {
    plugin_stamp::stamp(&module(&manifest), &Limits::default()).unwrap()
}

/// `module` signed by the key whose seed is all `key` without being stamped (the stamp tool won't):
/// a signature section over every byte before it, as the host reads one.
fn signed_unstamped(module: &[u8], key: u8) -> Vec<u8> {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let k = ed25519_dalek::SigningKey::from_bytes(&[key; 32]);
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let data = json!({"v": 1, "key": b64(k.verifying_key().as_bytes()), "sig": b64(&k.sign(module).to_bytes())}).to_string();
    let leb = |out: &mut Vec<u8>, mut n: usize| loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        out.push(if n == 0 { b } else { b | 0x80 });
        if n == 0 {
            break;
        }
    };
    let name = lcc_plugins::SIGNATURE_SECTION;
    let mut payload = Vec::new();
    leb(&mut payload, name.len());
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(data.as_bytes());
    let mut out = module.to_vec();
    out.push(0);
    leb(&mut out, payload.len());
    out.extend_from_slice(&payload);
    out
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A feed offering `file` as version `version`, declaring `permissions`.
fn feed(version: &str, file: &[u8], permissions: Value) -> Vec<u8> {
    json!({"updates-plugin": {"id": ID, "version": version, "url": "https://feed.test/updates.wasm", "sha256": sha(file),
        "permissions": permissions, "changelog": "https://feed.test/CHANGELOG.md", "notes": "Faster."}})
    .to_string()
    .into_bytes()
}

/// Serves URLs from a map and counts the requests.
#[derive(Debug, Default)]
struct Served {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    asked: Mutex<Vec<String>>,
}

impl Served {
    fn put(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.into(), body);
    }
    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl Fetch for Served {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        self.asked.lock().unwrap().push(url.into());
        let body = self.files.lock().unwrap().get(url).cloned().ok_or_else(|| format!("{url}: HTTP 404"))?;
        if body.len() as u64 > max { Err("too large".into()) } else { Ok(body) }
    }
}

fn folder(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-updates-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn install(m: &mut Manager, bytes: &[u8]) {
    let plugin = m.load(bytes).unwrap();
    m.install_loaded_as(plugin, bytes, None, false, false, &SignerAnswer::None).unwrap();
}

/// Runs the checks `update_checks` lists, as the engine does: fetched off the lock, then recorded.
fn run_checks(m: &mut Manager, id: Option<&str>, due: Option<i64>, at: i64) -> usize {
    let checks: Vec<UpdateCheck> = m.update_checks(id, due).unwrap();
    let n = checks.len();
    for c in checks {
        let fetched = m.fetcher().get(&c.url, lcc_plugins::updates::MAX_FEED_BYTES);
        m.record_check(&c, at, fetched).unwrap();
    }
    n
}

fn updates(m: &Manager) -> Value {
    m.get(ID).unwrap().updates_info(m.features())
}

/// AC-1: `updateUrl` is kept when it is an `https://` link, else dropped and named; `updates` is a
/// host feature.
#[test]
fn the_manifest_takes_an_https_update_url() {
    let m = Manager::in_memory();
    let p = m.load(&signed("1.0.0", 1)).unwrap();
    assert_eq!(p.manifest().update_url.as_deref(), Some(FEED));
    assert!(lcc_plugins::HOST_FEATURES.contains(&"updates"));
    for bad in [json!("http://feed.test/x.json"), json!("ftp://feed.test"), json!(7)] {
        let mut man = manifest("1.0.0", json!({}));
        man["updateUrl"] = bad.clone();
        let p = m.load(&module(&man)).unwrap();
        assert_eq!(p.manifest().update_url, None, "{bad}");
        assert!(p.manifest().dropped.iter().any(|d| d.starts_with("updateUrl: ")), "{bad}: {:?}", p.manifest().dropped);
        assert!(p.manifest().unknown_fields.is_empty(), "{:?}", p.manifest().unknown_fields);
    }
}

/// AC-4: only a stamped plug-in signed by its pinned key, with a feed, gets update checks; the
/// rest can't have them turned on and are never checked.
#[test]
fn only_stamped_signed_plugins_with_a_feed_get_update_checks() {
    let unstamped = module(&manifest("1.0.0", json!({})));
    let unsigned = stamped_with(manifest("1.0.0", json!({})));
    let mut no_feed = manifest("1.0.0", json!({}));
    no_feed.as_object_mut().unwrap().remove("updateUrl");
    let no_feed = signed_with(no_feed, 1);
    for (what, file, why) in
        [("unstamped", &unstamped, "isn't stamped"), ("unsigned", &unsigned, "isn't signed"), ("no feed", &no_feed, "no update feed")]
    {
        let mut m = Manager::in_memory();
        install(&mut m, file);
        let info = updates(&m);
        assert_eq!((info["check"].clone(), info["available"].clone()), (json!(false), json!(false)), "{what}: {info}");
        assert!(info["reason"].as_str().unwrap().contains(why), "{what}: {info}");
        let e = m.set_update_check(ID, true).unwrap_err();
        assert!(matches!(e, Error::Update(_)), "{what}: {e}");
        assert!(m.update_checks(Some(ID), None).is_err(), "{what}: not even on request");
        assert!(m.update_checks(None, None).unwrap().is_empty(), "{what}");
    }
    let mut m = Manager::in_memory();
    install(&mut m, &signed("1.0.0", 1));
    assert_eq!(updates(&m)["available"], json!(true));
    assert_eq!(updates(&m)["check"], json!(false), "off by default");
    assert!(m.update_checks(None, None).unwrap().is_empty(), "nothing is checked until the user turns it on");
    assert_eq!(m.update_checks(Some(ID), None).unwrap().len(), 1, "on request, whatever the setting");
    m.set_update_check(ID, true).unwrap();
    assert_eq!(m.update_checks(None, None).unwrap()[0].url, FEED);
}

/// AC-3: opening the manager checks a plug-in only when its last check is a day old; the result
/// within the day stands; the setting and result are kept in `plugins.json` across a restart.
#[test]
fn a_check_stands_for_a_day_and_is_kept_across_a_restart() {
    let dir = folder("day");
    let served = Arc::new(Served::default());
    let next = signed("1.1.0", 1);
    served.put(FEED, feed("1.1.0", &next, json!(["catalog"])));
    let mut m = Manager::open(&dir).unwrap();
    m.set_fetcher(served.clone());
    install(&mut m, &signed("1.0.0", 1));
    m.set_update_check(ID, true).unwrap();
    let now = 1_800_000_000;
    assert_eq!(run_checks(&mut m, None, Some(now), now), 1, "never checked: due");
    let info = updates(&m);
    assert_eq!((info["checkedAt"].clone(), info["offer"]["version"].clone()), (json!(now), json!("1.1.0")), "{info}");
    assert_eq!((info["offer"]["notes"].clone(), info["offer"]["adds"].clone()), (json!("Faster."), json!([])), "{info}");
    assert_eq!(run_checks(&mut m, None, Some(now + DAY - 1), now + DAY - 1), 0, "within the day");
    assert_eq!(served.asked(), 1);
    drop(m);
    let mut m = Manager::open(&dir).unwrap();
    m.set_fetcher(served.clone());
    assert_eq!(updates(&m)["check"], json!(true), "the setting is kept");
    assert_eq!(updates(&m)["offer"]["version"], json!("1.1.0"), "and the result, shown as it was");
    assert_eq!(m.update_checks(None, Some(now + 60)).unwrap().len(), 0);
    assert_eq!(m.update_checks(None, Some(now + DAY)).unwrap().len(), 1, "a day old: due");
    assert_eq!(m.update_checks(None, Some(now - 60)).unwrap().len(), 1, "from the future (clock set back): due");
    // a failed check is kept with why, and offers nothing
    served.put(FEED, b"<html>".to_vec());
    run_checks(&mut m, Some(ID), None, now + DAY);
    let info = updates(&m);
    assert!(info["error"].as_str().unwrap().contains("isn't JSON") && info["offer"].is_null(), "{info}");
    assert!(m.get(ID).unwrap().log.iter().any(|l| l.starts_with("update check failed")));
}

/// AC-6, AC-4: an update installs through the install path once its SHA-256, manifest and signer
/// hold up; anything else installs nothing and says why.
#[test]
fn an_update_installs_only_as_the_feed_and_the_pinned_key_say() {
    let dir = folder("install");
    let served = Arc::new(Served::default());
    let mut m = Manager::open(&dir).unwrap();
    m.set_fetcher(served.clone());
    install(&mut m, &signed("1.0.0", 1));
    m.set_update_check(ID, true).unwrap();
    assert!(matches!(m.update_offer(ID), Err(Error::Update(_))), "nothing found yet");
    let refused = |m: &mut Manager, offered: &[u8], file: &[u8]| {
        served.put(FEED, feed("1.1.0", offered, json!(["catalog"])));
        run_checks(m, Some(ID), None, 1);
        let e = m.install_update(ID, file, false, false).unwrap_err().to_string();
        assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0", "{e}");
        e
    };
    let good = signed("1.1.0", 1);
    assert!(refused(&mut m, &good, &signed("1.1.1", 1)).contains("SHA-256"));
    for (file, why) in [
        (stamped_with(manifest("1.1.0", json!({"catalog": true}))), "isn't signed"),
        (signed("1.1.0", 2), "not by the author key pinned"),
        (signed_unstamped(&module(&manifest("1.1.0", json!({"catalog": true}))), 1), "isn't stamped"),
        (signed("1.2.0", 1), "not test.updates 1.1.0"),
        // a section after the signature: it no longer covers the file
        ([signed("1.1.0", 1).as_slice(), &[0, 2, 1, b'x']].concat(), "signature doesn't hold up"),
    ] {
        let e = refused(&mut m, &file, &file);
        assert!(e.contains(why), "{why}: {e}");
    }
    assert_eq!(m.pin(ID).unwrap().signer().unwrap().fingerprint(), plugin_stamp::fingerprint(&plugin_stamp::public_key(&[1; 32])));
    // the real thing: installed, the old version kept to go back to, the result cleared
    served.put(FEED, feed("1.1.0", &good, json!(["catalog"])));
    run_checks(&mut m, Some(ID), None, 2);
    let installed = m.install_update(ID, &good, false, false).unwrap();
    assert_eq!(installed.plugin.manifest().version, "1.1.0");
    assert_eq!(m.previous_version(ID), Some("1.0.0"));
    assert!(updates(&m)["offer"].is_null() && updates(&m)["check"] == json!(true), "{}", updates(&m));
}

/// AC-6: a file asking for a permission group the feed didn't declare installs nothing and gives
/// the real diff, until the user says yes; new permissions aren't granted either way.
#[test]
fn undeclared_permission_groups_need_the_users_yes() {
    let served = Arc::new(Served::default());
    let mut m = Manager::in_memory();
    m.set_fetcher(served.clone());
    install(&mut m, &signed("1.0.0", 1));
    m.set_grant_by_user(ID, Permissions { catalog: true, ..Permissions::default() }).unwrap();
    let wider = signed_with(manifest("2.0.0", json!({"catalog": true, "network": ["api.feed.test"], "metadataWrite": true})), 1);
    served.put(FEED, feed("2.0.0", &wider, json!(["catalog", "network"])));
    run_checks(&mut m, Some(ID), None, 1);
    assert_eq!(updates(&m)["offer"]["adds"], json!(["network"]), "the row shows what the feed declares");
    match m.install_update(ID, &wider, false, false) {
        Err(Error::Undeclared { groups, requests }) => {
            assert_eq!(groups, ["metadataWrite"]);
            let names: Vec<String> = requests.iter().map(|r| r.describe()).collect();
            assert_eq!(names, ["metadataWrite", "network api.feed.test"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0");
    let i = m.install_update(ID, &wider, true, false).unwrap();
    assert_eq!(i.plugin.manifest().version, "2.0.0");
    assert_eq!(i.effective(), Permissions { catalog: true, ..Permissions::default() }, "new permissions aren't granted");
}

/// A module whose stamped section claims `manifest` but whose `lcc_manifest` traps: it fails the
/// moment it runs, so only checks that never run it can say anything about it.
fn trapping(manifest: &Value) -> Vec<u8> {
    let esc = |s: &str| s.bytes().map(|b| format!("\\{b:02x}")).collect::<String>();
    let src = format!(
        r#"(module
  (memory (export "memory") 2)
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (unreachable))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (unreachable))
  (@custom "{name}" "{m}"))"#,
        name = lcc_plugins::MANIFEST_SECTION,
        m = esc(&manifest.to_string()),
    );
    wat::parse_str(&src).unwrap()
}

/// AC-4, AC-6: an update is judged from its stamped section and signature before any of it runs:
/// another signer is refused and undeclared groups are asked about even for a file that traps when
/// it runs; only after the user's yes does it run (and here fail to load).
#[test]
fn an_update_is_judged_before_it_runs() {
    let served = Arc::new(Served::default());
    let mut m = Manager::in_memory();
    m.set_fetcher(served.clone());
    install(&mut m, &signed("1.0.0", 1));
    let offer = |m: &mut Manager, file: &[u8]| {
        served.put(FEED, feed("1.1.0", file, json!(["catalog"])));
        run_checks(m, Some(ID), None, 1);
    };
    let other_key = signed_unstamped(&trapping(&manifest("1.1.0", json!({"catalog": true}))), 2);
    offer(&mut m, &other_key);
    let e = m.install_update(ID, &other_key, true, false).unwrap_err().to_string();
    assert!(e.contains("not by the author key pinned"), "refused for its signer, not for what running it did: {e}");
    let undeclared = signed_unstamped(&trapping(&manifest("1.1.0", json!({"catalog": true, "metadataWrite": true}))), 1);
    offer(&mut m, &undeclared);
    assert!(matches!(m.install_update(ID, &undeclared, false, false), Err(Error::Undeclared { .. })), "asked before it runs");
    assert!(m.install_update(ID, &undeclared, true, false).is_err(), "it runs only after the yes, and fails to load");
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0");
}

/// AC-8: an update needing a host feature this app lacks is shown with what's missing, and not
/// installed.
#[test]
fn an_update_needing_a_missing_feature_is_shown_but_not_installable() {
    let served = Arc::new(Served::default());
    let mut m = Manager::in_memory();
    m.set_fetcher(served.clone());
    install(&mut m, &signed("1.0.0", 1));
    let next = signed("1.1.0", 1);
    let mut f: Value = serde_json::from_slice(&feed("1.1.0", &next, json!([]))).unwrap();
    f["updates-plugin"]["requires"] = json!(["updates", "teleport"]);
    served.put(FEED, f.to_string().into_bytes());
    run_checks(&mut m, Some(ID), None, 1);
    assert_eq!(updates(&m)["offer"]["missing"], json!(["teleport"]));
    assert!(matches!(m.update_offer(ID), Err(Error::Unsupported(missing)) if missing == ["teleport"]));
    assert!(m.install_update(ID, &next, false, false).is_err());
}
