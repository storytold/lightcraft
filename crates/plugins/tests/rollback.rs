//! Going back to the version an update replaced: the module and its settings come back, the grant
//! is cut back to what that version asks for (programs only from the user), and a rollback that
//! can't be written changes nothing.

use std::path::{Path, PathBuf};

use lcc_plugins::{Error, Manager, Manifest, NoHost, Permissions};
use serde_json::{Value, json};

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

const ID: &str = "test.rollback";

fn text(key: &str) -> Value {
    json!({"key": key, "label": key, "type": "text", "default": format!("{key}-default")})
}

/// Version `version` with settings `keys`, asking for `permissions`.
fn version(version: &str, keys: &[&str], permissions: Value) -> Vec<u8> {
    let settings: Vec<Value> = keys.iter().map(|k| text(k)).collect();
    module(&json!({
        "id": ID, "name": "Rollback", "version": version, "permissions": permissions, "settings": settings,
        "commands": [{"id": "go", "label": "Go"}],
    }))
}

fn folder(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-rollback-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn values(m: &mut Manager) -> Value {
    Value::Object(m.settings(ID, &mut NoHost).unwrap().values)
}

fn previous(dir: &Path) -> [PathBuf; 2] {
    [dir.join("previous").join(format!("{ID}.wasm")), dir.join("previous").join(format!("{ID}.json"))]
}

/// Every file below `dir` with its contents, to tell nothing changed.
#[cfg(unix)]
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(snapshot(&p));
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn an_update_keeps_the_version_it_replaces_and_going_back_restores_it() {
    let dir = folder("back");
    let v1 = version("1.0.0", &["a", "b"], json!({"catalog": true, "metadataWrite": true}));
    let v2 = version("1.1.0", &["b", "c"], json!({"catalog": true}));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes_by_user(&v1, Some(Permissions { catalog: true, metadata_write: true, ..Default::default() }), false).unwrap();
    assert_eq!(m.previous_version(ID), None, "a first install has nothing to go back to");
    m.set_settings(ID, &json!({"a": "A", "b": "B"}), &mut NoHost).unwrap();
    m.install_bytes(&v2, None, false).unwrap();
    assert_eq!(values(&mut m), json!({"b": "B", "c": "c-default"}), "the update pruned `a`");
    assert_eq!(m.previous_version(ID), Some("1.0.0"));
    m.set_settings(ID, &json!({"c": "C"}), &mut NoHost).unwrap();

    // kept across a restart; a module someone put in previous/ never loads
    std::fs::copy(dir.join(format!("{ID}.wasm")), dir.join("previous").join("other.wasm")).unwrap();
    drop(m);
    let mut m = Manager::open(&dir).unwrap();
    assert_eq!(m.previous_version(ID), Some("1.0.0"), "after a restart");
    assert_eq!(m.list().count(), 1, "nothing loads from previous/");
    assert!(m.failed.is_empty(), "{:?}", m.failed);

    let back = m.rollback(ID).unwrap();
    assert_eq!(back.plugin.manifest().version, "1.0.0");
    assert_eq!(back.log.back().map(String::as_str), Some("rolled back from 1.1.0 to 1.0.0"), "{:?}", back.log);
    assert_eq!(back.effective(), Permissions { catalog: true, ..Default::default() }, "the grant as it was, never more");
    assert_eq!(values(&mut m), json!({"a": "A", "b": "B"}), "its settings as they were before the update");
    let saved: Value = serde_json::from_slice(&std::fs::read(dir.join("settings").join(format!("{ID}.json"))).unwrap()).unwrap();
    assert_eq!(saved["settings"], json!({"a": "A", "b": "B"}), "and in its file");
    assert_eq!(std::fs::read(dir.join(format!("{ID}.wasm"))).unwrap(), v1, "the old module is the one in the folder");
    assert_eq!(m.previous_version(ID), None, "one level deep");
    assert!(previous(&dir).iter().all(|p| !p.exists()));
    let e = m.rollback(ID).unwrap_err();
    assert!(matches!(&e, Error::Params(t) if t.contains("no previous version")), "{e}");
    drop(m);
    let m = Manager::open(&dir).unwrap();
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0", "after a restart too");
}

#[test]
fn only_an_update_with_other_code_keeps_a_version() {
    let dir = folder("none");
    let v1 = version("1.0.0", &[], json!({}));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&v1, None, false).unwrap();
    m.install_bytes(&v1, None, false).unwrap();
    assert_eq!(m.previous_version(ID), None, "the same module again is a repair");
    let e = m.rollback(ID).unwrap_err();
    assert!(matches!(&e, Error::Params(t) if t.contains("no previous version")), "{e}");
    assert!(matches!(m.rollback("test.missing").unwrap_err(), Error::NotFound(_)));
    let mut m = Manager::in_memory();
    m.install_bytes(&v1, None, false).unwrap();
    m.install_bytes(&version("1.1.0", &[], json!({})), None, false).unwrap();
    assert!(m.rollback(ID).is_err(), "without a folder nothing is kept");
}

#[test]
fn removing_a_plugin_removes_its_previous_version_unless_its_data_is_kept() {
    let dir = folder("remove");
    let (v1, v2) = (version("1.0.0", &[], json!({})), version("2.0.0", &[], json!({})));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&v1, None, false).unwrap();
    m.install_bytes(&v2, None, false).unwrap();
    m.uninstall_with(ID, true).unwrap();
    assert!(previous(&dir).iter().all(|p| p.exists()), "kept with its data");
    m.install_bytes(&v2, None, false).unwrap();
    assert_eq!(m.previous_version(ID), Some("1.0.0"), "installed again, it can go back again");
    m.uninstall(ID).unwrap();
    assert!(previous(&dir).iter().all(|p| !p.exists()), "removed with the rest");
}

/// Programs the user granted: the rollback from the plug-in's page keeps them (those the old
/// version asks for), one from a command drops them, since that version's code is different.
#[test]
fn going_back_keeps_programs_only_from_the_plugins_page() {
    let programs = json!({"catalog": true, "programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
    let manifest = |v: &str| json!({"id": ID, "name": "Rollback", "version": v, "requires": ["programs"], "permissions": programs});
    let grant = Manifest::parse(manifest("1.0.0").to_string().as_bytes()).unwrap().permissions;
    let programs_of = |m: &Manager| m.get(ID).unwrap().effective().programs.len();
    for by_user in [true, false] {
        let dir = folder(if by_user { "page" } else { "command" });
        let mut m = Manager::open(&dir).unwrap();
        m.install_bytes_by_user(&module(&manifest("1.0.0")), Some(grant.clone()), false).unwrap();
        m.install_bytes_by_user(&module(&manifest("1.1.0")), None, false).unwrap();
        assert_eq!(programs_of(&m), 1);
        let back = if by_user { m.rollback_by_user(ID) } else { m.rollback(ID) }.unwrap();
        assert_eq!(back.plugin.manifest().version, "1.0.0");
        if by_user {
            assert_eq!(programs_of(&m), 1, "from the page: kept");
        } else {
            assert_eq!(programs_of(&m), 0, "from a command: dropped");
            assert!(m.get(ID).unwrap().log.iter().any(|l| l.contains("code is different")), "and the log says why");
        }
        assert!(m.get(ID).unwrap().effective().catalog, "the other permissions stay");
    }
}

/// A rollback that can't write the old module or its settings leaves the current version
/// installed and working, its files as they were.
#[cfg(unix)]
#[test]
fn a_rollback_that_cant_be_written_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path, mode: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    for read_only in ["", "settings"] {
        let dir = folder(&format!("read-only-{}", read_only.len()));
        let mut m = Manager::open(&dir).unwrap();
        m.install_bytes_by_user(
            &version("1.0.0", &["a"], json!({"catalog": true})),
            Some(Permissions { catalog: true, ..Default::default() }),
            false,
        )
        .unwrap();
        m.set_settings(ID, &json!({"a": "old"}), &mut NoHost).unwrap();
        m.install_bytes(&version("1.1.0", &["a"], json!({"catalog": true})), None, false).unwrap();
        m.set_settings(ID, &json!({"a": "new"}), &mut NoHost).unwrap();
        let before = snapshot(&dir);
        let locked = dir.join(read_only);
        mode(&locked, 0o555);
        let r = m.rollback(ID).map(|_| ());
        mode(&locked, 0o755);
        assert!(matches!(r, Err(Error::Io(_))), "read-only {read_only:?}: the error is reported: {r:?}");
        assert_eq!(snapshot(&dir), before, "read-only {read_only:?}: no file changed, none left behind");
        assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.1.0", "the current version stays");
        assert_eq!(values(&mut m), json!({"a": "new"}), "with its settings");
        assert!(m.get(ID).unwrap().effective().catalog);
        assert!(m.run_command(ID, "go", &json!({}), &[], &mut NoHost).is_ok(), "and it still runs");
        assert_eq!(m.previous_version(ID), Some("1.0.0"), "the rollback can be tried again");
        m.rollback(ID).unwrap();
        assert_eq!(values(&mut m), json!({"a": "old"}), "and then goes through");
    }
}

/// The old module is written but `plugins.json` can't be saved: the failure is reported, never
/// swallowed, and the module that runs is the one the folder holds.
#[cfg(unix)]
#[test]
fn a_rollback_whose_grant_cant_be_saved_says_so() {
    let dir = folder("save-fails");
    let mut m = Manager::open(&dir).unwrap();
    let catalog = Permissions { catalog: true, ..Default::default() };
    m.install_bytes_by_user(&version("1.0.0", &["a"], json!({"catalog": true})), Some(catalog), false).unwrap();
    m.install_bytes(&version("1.1.0", &["a"], json!({"catalog": true})), None, false).unwrap();
    // a folder where plugins.json goes: the atomic rename over it fails, the module's own write doesn't
    let state = dir.join("plugins.json");
    let saved = std::fs::read(&state).unwrap();
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    let r = m.rollback(ID).map(|_| ());
    std::fs::remove_dir(&state).unwrap();
    std::fs::write(&state, saved).unwrap();
    let Err(Error::Io(e)) = r else { panic!("the failed save is reported: {r:?}") };
    assert!(e.contains("plugins.json"), "and names the file: {e}");
    let on_disk = std::fs::read(dir.join(format!("{ID}.wasm"))).unwrap();
    assert_eq!(on_disk, version("1.0.0", &["a"], json!({"catalog": true})), "the old module was written");
    assert_eq!(m.get(ID).unwrap().plugin.manifest().version, "1.0.0", "the plug-in runs what the folder holds");
    assert!(m.get(ID).unwrap().effective().catalog, "with the grant it had");
}
