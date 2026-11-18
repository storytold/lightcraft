//! The install dialog and author signatures: it says who signed a plug-in (or that nobody did),
//! names both signers when a file isn't signed by the installed version's, and its Install Anyway
//! button is the user's yes; a file whose signature doesn't hold up can't be installed.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);
const ID: &str = "test.uisigned";

/// Version `version` of plug-in [`ID`]; it answers every call with `{}`.
fn module(version: &str) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{ID}","name":"Signed","version":"{version}"}}"#);
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192))))"#,
        m = manifest.replace('"', "\\\""),
        ml = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

/// `bytes` with an author signature section appended, signed with the key whose seed is all `seed`
/// (as `plugin-stamp sign` writes it), and that key's fingerprint.
fn signed(bytes: &[u8], seed: u8) -> (Vec<u8>, String) {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let data = json!({"v": 1, "key": b64(key.verifying_key().as_bytes()), "sig": b64(&key.sign(bytes).to_bytes())}).to_string();
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
    let mut out = bytes.to_vec();
    out.push(0);
    leb(&mut out, payload.len());
    out.extend_from_slice(&payload);
    let fingerprint = lcc_plugins::Signer::from_base64(&b64(key.verifying_key().as_bytes())).unwrap().fingerprint();
    (out, fingerprint)
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn shown(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(text))
}

/// Opens the install dialog for `path`, as File ▸ Plug-ins ▸ Install Plug-in… does once a file is chosen.
fn open_dialog(h: &mut Headless, path: &std::path::Path) {
    let r = h.request("engine.execute", json!({"command": "plugins.install", "params": {"path": path}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
}

fn installed(id: &str) -> Option<(String, Option<String>)> {
    let m = lcc_engine::cmd::plugins::manager();
    let p = m.as_ref()?.get(id)?;
    Some((p.plugin.manifest().version.clone(), p.pin().signer().map(|s| s.fingerprint())))
}

#[test]
fn the_install_dialog_names_the_signer_and_asks_when_it_changes() {
    // install into memory, never into the real user's settings folder
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let (v1, fp1) = signed(&module("1.0.0"), 1);
    lcc_engine::cmd::plugins::manager().as_mut().unwrap().install_bytes(&v1, None, false).unwrap();
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    h.settle(T);
    let dir = lcc_testkit::TempDir::new("lcc-ui-plugin-signing");
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };

    // another signer: both named, and Install Anyway is the yes
    let (v2, fp2) = signed(&module("2.0.0"), 2);
    open_dialog(&mut h, &write("v2.wasm", &v2));
    assert!(shown(&h, &format!("Signed by {fp2}")), "{:?}", h.painted_text());
    assert!(shown(&h, &format!("was signed by {fp1}, but this file is signed by {fp2}")), "{:?}", h.painted_text());
    assert!(shown(&h, "Install Anyway"), "{:?}", h.painted_text());
    assert!(h.click_as_user("button:pluginInstall"));
    assert_eq!(installed(ID), Some(("2.0.0".into(), Some(fp2.clone()))), "installed, and the new signer pinned");

    // the same signer again: said so, and a plain Install
    let (v3, _) = signed(&module("3.0.0"), 2);
    open_dialog(&mut h, &write("v3.wasm", &v3));
    assert!(shown(&h, &format!("Signed by {fp2}, the same author as the installed version")), "{:?}", h.painted_text());
    assert!(!shown(&h, "Install Anyway") && !shown(&h, "Install it only if"), "{:?}", h.painted_text());
    assert!(h.click_as_user("button:pluginInstall"));
    assert_eq!(installed(ID), Some(("3.0.0".into(), Some(fp2.clone()))));

    // a signature that doesn't hold up: why, and no install button
    let invalid = [v3.as_slice(), &[0, 2, 1, b'x']].concat();
    open_dialog(&mut h, &write("bad.wasm", &invalid));
    assert!(shown(&h, "It can't be installed:") && shown(&h, "last section"), "{:?}", h.painted_text());
    assert!(!has(&h, "button:pluginInstall"));
    assert!(h.click_as_user("button:pluginInstallCancel"));

    // unsigned over a signed one: said so, asked, and installed only on the click
    open_dialog(&mut h, &write("v4.wasm", &module("4.0.0")));
    assert!(shown(&h, "Not signed: nothing shows who made it"), "{:?}", h.painted_text());
    assert!(shown(&h, &format!("was signed by {fp2}, but this file isn't signed")), "{:?}", h.painted_text());
    let r = h.request("engine.execute", json!({"command": "plugin.install", "params": {"path": dir.join("v4.wasm")}}), T);
    assert_eq!(r["ok"], false, "a command without the answer is refused: {r}");
    assert!(h.click_as_user("button:pluginInstall"));
    assert_eq!(installed(ID), Some(("4.0.0".into(), None)), "installed unsigned, as the user said yes to");

    // a fresh unsigned install: said so, nothing to ask
    let _ = h.request("engine.execute", json!({"command": "plugin.uninstall", "params": {"id": ID}}), T);
    open_dialog(&mut h, &dir.join("v4.wasm"));
    assert!(shown(&h, "Not signed: nothing shows who made it"), "{:?}", h.painted_text());
    assert!(!shown(&h, "Install it only if") && !shown(&h, "Install Anyway"), "{:?}", h.painted_text());
    assert!(h.click_as_user("button:pluginInstall"));
    assert_eq!(installed(ID), Some(("4.0.0".into(), None)));
    let _ = h.request("engine.execute", json!({"command": "plugin.uninstall", "params": {"id": ID}}), T);
}
