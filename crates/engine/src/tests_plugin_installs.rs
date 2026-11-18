//! Installing plug-ins and pixel filters safely: the size limit before anything is read, the
//! downgrade check for filters, and one id per module across both kinds.

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::plugins::tests::{module_file, turn};

/// A pixel filter `id` at `version`, written to `<name>.wasm` in a Temp folder of its own (removed
/// when the returned guard is dropped).
fn filter_file(name: &str, id: &str, version: &str) -> lcc_testkit::TempFile {
    let manifest = format!(r#"{{"id":"{id}","name":"Filter {name}","version":"{version}","kind":"filter","params":{{}}}}"#);
    let file = lcc_testkit::TempFile::new(&format!("lcc-engine-installs-file-{name}"), &format!("{name}.wasm"));
    std::fs::write(&file, crate::tests_plugin_filters::module(&manifest, None)).unwrap();
    file
}

fn listed(s: &mut Session, id: &str) -> Option<Value> {
    let l = s.execute("plugin.list", &json!({})).unwrap();
    l["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned()
}

#[test]
fn inspecting_a_file_over_the_size_limit_fails_as_installing_it_does() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = lcc_testkit::TempDir::new("lcc-engine-installs-big");
    let path = dir.join("big.wasm");
    let max = lcc_plugins::Limits::default().max_module_bytes as u64;
    // sparse: nothing is written, and nothing should be read
    std::fs::File::create(&path).unwrap().set_len(max + 1).unwrap();
    let why = format!("{} is {} bytes (limit {max})", path.display(), max + 1);
    let inspected = s.execute("plugin.inspectFile", &json!({"path": path})).unwrap_err().to_string();
    let installed = s.execute("plugin.install", &json!({"path": path})).unwrap_err().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(inspected.contains(&why), "{inspected}");
    assert!(installed.contains(&why), "{installed}");
}

#[test]
fn a_filter_older_than_the_installed_one_needs_confirming() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let id = "test.installs.olderfilter";
    let v110 = filter_file("older-1.10", id, "1.10");
    let v19 = filter_file("older-1.9", id, "1.9");
    let v1100 = filter_file("older-1.10.0", id, "1.10.0");
    s.execute("plugin.install", &json!({"path": v110.path()})).unwrap();

    let info = s.execute("plugin.inspectFile", &json!({"path": v19.path()})).unwrap();
    assert_eq!(info["downgrade"], true, "1.9 is older than 1.10: {info}");
    assert_eq!(s.execute("plugin.inspectFile", &json!({"path": v1100.path()})).unwrap()["downgrade"], false, "1.10.0 is 1.10 again");
    let e = s.execute("plugin.install", &json!({"path": v19.path()})).unwrap_err().to_string();
    assert!(e.contains("1.9") && e.contains("1.10") && e.contains("allowDowngrade"), "{e}");
    assert_eq!(listed(&mut s, id).unwrap()["version"], "1.10", "the installed filter is kept");

    let r = s.execute("plugin.install", &json!({"path": v19.path(), "allowDowngrade": true}));
    let after = listed(&mut s, id);
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    assert_eq!(r.unwrap()["version"], "1.9");
    assert_eq!(after.unwrap()["version"], "1.9");
}

#[test]
fn filter_versions_compare_as_numbers_or_semver() {
    use crate::filter_plugins::is_older;
    assert!(is_older("1.9", "1.10"));
    assert!(is_older("1", "1.0.1"));
    assert!(!is_older("1.0", "1"), "a missing part counts as 0");
    assert!(is_older("2.0.0-rc.1", "2.0.0"), "semver precedence");
    assert!(!is_older("2.0.0", "2.0.0-rc.1"));
    assert!(!is_older("beta", "1.0"), "free text can't be compared, and isn't a downgrade");
    assert!(!is_older("1.x", "2"));
}

#[test]
fn a_plugin_and_a_filter_never_share_an_id() {
    let _turn = turn();
    let mut s = Session::with_demo();
    // a plug-in installed: a filter with its id is refused
    let id = "test.installs.clash";
    let plugin = module_file("clash-plugin", &json!({"id": id, "name": "Clash Plug-in", "version": "1.0.0"}));
    let filter = filter_file("clash-filter", id, "1.0");
    s.execute("plugin.install", &json!({"path": plugin.path()})).unwrap();
    let conflict = s.execute("plugin.inspectFile", &json!({"path": filter.path()})).unwrap()["conflict"].clone();
    let refused = s.execute("plugin.install", &json!({"path": filter.path()}));
    let filter_kept = crate::filter_plugins::registry().get(id).is_some();
    let plugin_after = listed(&mut s, id);
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    assert!(conflict.as_str().is_some_and(|c| c.contains("Clash Plug-in")), "the dialog names the plug-in: {conflict}");
    let e = refused.unwrap_err().to_string();
    assert!(e.contains("Clash Plug-in") && e.contains("plug-in"), "{e}");
    assert!(!filter_kept, "no filter installed");
    assert_eq!(plugin_after.and_then(|p| p.get("kind").cloned()), None, "the plug-in is still the one listed");

    // a filter installed: a plug-in with its id is refused
    let id = "test.installs.clash2";
    let filter = filter_file("clash2-filter", id, "1.0");
    let plugin = module_file("clash2-plugin", &json!({"id": id, "name": "Clash Two", "version": "1.0.0"}));
    s.execute("plugin.install", &json!({"path": filter.path()})).unwrap();
    let conflict = s.execute("plugin.inspectFile", &json!({"path": plugin.path()})).unwrap()["conflict"].clone();
    let refused = s.execute("plugin.install", &json!({"path": plugin.path()}));
    let plugin_kept = crate::cmd::plugins::manager().as_ref().is_some_and(|m| m.get(id).is_some());
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    assert!(conflict.as_str().is_some_and(|c| c.contains("Filter clash2-filter")), "the dialog names the filter: {conflict}");
    let e = refused.unwrap_err().to_string();
    assert!(e.contains("Filter clash2-filter") && e.contains("pixel filter"), "{e}");
    assert!(!plugin_kept, "no plug-in installed");
}

/// A fresh folder for one test, removed when the test ends.
fn folder(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-engine-installs-{name}"))
}

fn filter_bytes(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"F","version":"{version}","kind":"filter","params":{{}}}}"#);
    crate::tests_plugin_filters::module(&manifest, None)
}

/// Of two filter modules with one id, the first by file name loads; the other is noted with why.
#[test]
fn of_two_filters_with_one_id_the_first_by_file_name_loads() {
    let dir = folder("dupfilters");
    std::fs::write(dir.join("a.wasm"), filter_bytes("test.installs.dupfilter", "1.0")).unwrap();
    std::fs::write(dir.join("b.wasm"), filter_bytes("test.installs.dupfilter", "2.0")).unwrap();
    let r = lcc_filters::Registry::new(lcc_filters::Limits::default());
    crate::filter_plugins::load_dir(&r, &dir);
    let second = dir.join("b.wasm").display().to_string();
    let noted = crate::filter_plugins::duplicate(&second);
    crate::filter_plugins::forget_duplicate(&second);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.get("test.installs.dupfilter").unwrap().manifest().version, "1.0", "the first by file name");
    let (id, why) = noted.expect("the other is noted");
    assert_eq!(id, "test.installs.dupfilter");
    assert!(why.contains("another module has this id") && why.contains("a.wasm"), "{why}");
}

/// A plug-in and a filter with one id in the folder: the first by file name stays loaded, whichever
/// kind it is, and the other is listed as a module that didn't load.
#[test]
fn a_plugin_and_a_filter_with_one_id_settle_on_the_first_file() {
    for plugin_first in [true, false] {
        let dir = folder(&format!("shared-{plugin_first}"));
        let id = "test.installs.shared";
        let (plugin_name, filter_name) = if plugin_first { ("a.wasm", "b.wasm") } else { ("b.wasm", "a.wasm") };
        let plugin = module_file("shared-plugin", &json!({"id": id, "name": "Shared", "version": "1.0.0"}));
        std::fs::copy(&plugin, dir.join(plugin_name)).unwrap();
        std::fs::write(dir.join(filter_name), filter_bytes(id, "1.0")).unwrap();
        let mut m = lcc_plugins::Manager::open(&dir).unwrap();
        let r = lcc_filters::Registry::new(lcc_filters::Limits::default());
        crate::filter_plugins::load_dir(&r, &dir);
        assert!(m.get(id).is_some() && r.get(id).is_some(), "each loader loads its own kind");
        crate::filter_plugins::settle_shared_ids(&mut m, &r);
        let filter_file = dir.join(filter_name).display().to_string();
        let noted = crate::filter_plugins::duplicate(&filter_file);
        crate::filter_plugins::forget_duplicate(&filter_file);
        let _ = std::fs::remove_dir_all(&dir);
        if plugin_first {
            assert!(m.get(id).is_some() && r.get(id).is_none(), "the plug-in stays loaded");
            assert!(noted.is_some_and(|(i, why)| i == id && why.contains("a.wasm")), "the filter is listed with why");
        } else {
            assert!(m.get(id).is_none() && r.get(id).is_some(), "the filter stays loaded");
            let row = m.failed.iter().find(|f| f.id == id).expect("the plug-in is listed as failed");
            assert!(row.error.contains("another module has this id") && row.error.contains("a.wasm"), "{}", row.error);
            assert_eq!(row.path.as_deref(), Some(dir.join("b.wasm").as_path()));
        }
    }
}

/// `plugin.list` gives a duplicate filter module its own id and why, `plugin.uninstall {file}`
/// removes only that file, and modules past the most the folder loads are counted.
#[test]
fn a_duplicate_filter_module_is_listed_and_removed_by_its_file() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = folder("listdup");
    let id = "test.installs.listdup";
    let loaded = dir.join("a.wasm");
    let extra = dir.join("b.wasm");
    std::fs::write(&extra, filter_bytes(id, "2.0")).unwrap();
    let f = lcc_filters::Filter::load(&filter_bytes(id, "1.0"), lcc_filters::Limits::default()).unwrap();
    crate::filter_plugins::registry().install(f.with_source(loaded.display().to_string()));
    let extra_file = extra.display().to_string();
    crate::filter_plugins::note_duplicate(&extra_file, id, &loaded.display().to_string());
    {
        let mut g = crate::cmd::plugins::manager();
        let m = g.get_or_insert_with(lcc_plugins::Manager::in_memory);
        // the plug-in manager fails to load a filter module; it names it by its file
        m.failed.push(lcc_plugins::Failed {
            file: extra_file.clone(),
            error: "no lcc_* exports".into(),
            missing: vec![],
            id: "b".into(),
            path: Some(extra.clone()),
        });
        m.skipped = vec![dir.join("z1.wasm"), dir.join("z2.wasm")];
    }
    let list = s.execute("plugin.list", &json!({})).unwrap();
    let removed = s.execute("plugin.uninstall", &json!({"file": extra_file}));
    let still = crate::filter_plugins::registry().get(id).is_some();
    let extra_gone = !extra.exists();
    crate::filter_plugins::registry().remove(id);
    {
        let mut g = crate::cmd::plugins::manager();
        let m = g.as_mut().unwrap();
        m.skipped.clear();
        m.failed.retain(|f| f.file != extra_file);
    }
    crate::filter_plugins::forget_duplicate(&extra_file);
    let _ = std::fs::remove_dir_all(&dir);

    let row = list["failed"].as_array().unwrap().iter().find(|f| f["file"] == extra_file.as_str()).cloned().expect("listed");
    assert_eq!(row["id"], id, "its own id: {row}");
    assert_eq!(row["kind"], "filter");
    assert!(row["error"].as_str().unwrap().contains("another module has this id"), "{row}");
    assert_eq!(list["skipped"], 2);
    removed.unwrap();
    assert!(extra_gone, "the file went");
    assert!(still, "the filter that loaded stays");
}

/// The install dialog learns which manifest fields this version doesn't know, for plug-ins and
/// filters alike; the install still goes ahead.
#[test]
fn inspect_names_unknown_manifest_fields() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let id = "test.installs.unknownfilter";
    let params = r#"{"a":{"type":"number","min":0,"max":1,"default":0,"label":"A","labl":"typo"}}"#;
    let manifest = format!(
        r#"{{"id":"{id}","name":"U","version":"1.0","kind":"filter","stage":"source","baseTonee":"neutral","analyzeArea":"frame","params":{params}}}"#
    );
    let dir = folder("unknown");
    let filter = dir.join("filter.wasm");
    std::fs::write(&filter, crate::tests_plugin_filters::module(&manifest, None)).unwrap();
    let info = s.execute("plugin.inspectFile", &json!({"path": filter})).unwrap();
    let installed = s.execute("plugin.install", &json!({"path": filter}));
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    let mut unknown: Vec<String> = serde_json::from_value(info["unknownFields"].clone()).unwrap();
    unknown.sort();
    assert_eq!(unknown, ["baseTonee", "params.a.labl"], "{info}");
    assert_eq!(installed.unwrap()["unknownFields"], json!(["baseTonee", "params.a.labl"]), "installed anyway");

    let plugin =
        module_file("unknown-plugin", &json!({"id": "test.installs.unknownplugin", "name": "U", "version": "1.0.0", "hooks": {"exprt": true}}));
    let info = s.execute("plugin.inspectFile", &json!({"path": plugin.path()})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(info["unknownFields"], json!(["hooks.exprt"]), "{info}");
}

/// `plugin.rollback` goes back to the version an update replaced (`plugin.list` →
/// `previousVersion`), and says so when no version is kept.
#[test]
fn rollback_goes_back_to_the_version_an_update_replaced() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = folder("rollback");
    let id = "test.installs.rollback";
    let v1 = module_file("rollback-1", &json!({"id": id, "name": "R", "version": "1.0.0"}));
    let v2 = module_file("rollback-2", &json!({"id": id, "name": "R", "version": "2.0.0"}));
    let before = crate::cmd::plugins::manager().replace(lcc_plugins::Manager::open(&dir).unwrap());
    s.execute("plugin.install", &json!({"path": v1.path()})).unwrap();
    let none = s.execute("plugin.rollback", &json!({"id": id})).map_err(|e| e.to_string());
    let first = listed(&mut s, id);
    s.execute("plugin.install", &json!({"path": v2.path()})).unwrap();
    let updated = listed(&mut s, id);
    let back = s.execute("plugin.rollback", &json!({"id": id}));
    let after = listed(&mut s, id);
    let log = s.execute("plugin.log", &json!({"id": id}));
    *crate::cmd::plugins::manager() = before;
    let _ = std::fs::remove_dir_all(&dir);

    assert!(none.as_ref().is_err_and(|e| e.contains("no previous version")), "{none:?}");
    assert_eq!(first.unwrap()["previousVersion"], Value::Null);
    assert_eq!(updated.unwrap()["previousVersion"], "1.0.0");
    assert_eq!(back.unwrap()["version"], "1.0.0");
    let after = after.unwrap();
    assert_eq!(after["version"], "1.0.0");
    assert_eq!(after["previousVersion"], Value::Null, "one level deep");
    assert!(log.unwrap().to_string().contains("rolled back from 2.0.0 to 1.0.0"));
}

/// `plugin.uninstall {id, keepData: true}` keeps the plug-in's settings for its next install;
/// without it they go.
#[test]
fn uninstall_keeps_the_settings_only_with_keep_data() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = folder("keepdata");
    let id = "test.installs.keepdata";
    let settings = json!([{"key": "t", "label": "T", "type": "text", "default": "d"}]);
    let module = module_file("keepdata", &json!({"id": id, "name": "K", "version": "1.0.0", "settings": settings}));
    let file = dir.join("settings").join(format!("{id}.json"));
    let before = crate::cmd::plugins::manager().replace(lcc_plugins::Manager::open(&dir).unwrap());
    s.execute("plugin.install", &json!({"path": module.path()})).unwrap();
    let set = crate::cmd::plugins::manager().as_mut().unwrap().set_settings(id, &json!({"t": "kept"}), &mut lcc_plugins::NoHost).map(|_| ());
    let kept = s.execute("plugin.uninstall", &json!({"id": id, "keepData": true})).map(|_| file.exists());
    s.execute("plugin.install", &json!({"path": module.path()})).unwrap();
    let back = crate::cmd::plugins::manager().as_mut().unwrap().settings(id, &mut lcc_plugins::NoHost).map(|v| v.values);
    let gone = s.execute("plugin.uninstall", &json!({"id": id})).map(|_| !file.exists());
    *crate::cmd::plugins::manager() = before;
    let _ = std::fs::remove_dir_all(&dir);
    set.unwrap();
    assert!(kept.unwrap(), "keepData keeps the settings file");
    assert_eq!(back.unwrap().get("t"), Some(&json!("kept")), "installed again, they're back");
    assert!(gone.unwrap(), "without keepData they go");
}

/// Removing a plug-in by its id never deletes a pixel filter module that has its id: the command
/// refuses and names the file to remove first.
#[test]
fn removing_a_plugin_by_id_leaves_a_filter_module_with_its_id() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = folder("byid");
    let id = "test.installs.byid";
    std::fs::copy(module_file("byid-plugin", &json!({"id": id, "name": "By Id", "version": "1.0.0"})), dir.join("a.wasm")).unwrap();
    let filter = dir.join(format!("{id}.wasm"));
    std::fs::write(&filter, filter_bytes(id, "1.0")).unwrap();
    let before = crate::cmd::plugins::manager().replace(lcc_plugins::Manager::open(&dir).unwrap());
    let refused = s.execute("plugin.uninstall", &json!({"id": id})).map_err(|e| e.to_string());
    let (plugin_kept, filter_kept) = (listed(&mut s, id).is_some(), filter.exists());
    *crate::cmd::plugins::manager() = before;
    let _ = std::fs::remove_dir_all(&dir);
    let e = refused.unwrap_err();
    assert!(e.contains("pixel filter module") && e.contains(&format!("{id}.wasm")), "{e}");
    assert!(filter_kept, "the filter module stays");
    assert!(plugin_kept, "and so does the plug-in, nothing half removed");
}

/// Updating a filter that was copied in under another name writes `<id>.wasm` and removes the old
/// file, so the old version can't load first at the next start.
#[test]
fn a_filter_update_replaces_a_module_copied_in_under_another_name() {
    let dir = folder("renamed");
    let id = "test.installs.renamed";
    let copied = dir.join("a-copy.wasm");
    std::fs::write(&copied, filter_bytes(id, "1.0")).unwrap();
    let limits = lcc_filters::Limits::default();
    let installed = lcc_filters::Filter::load(&filter_bytes(id, "1.0"), limits.clone()).unwrap().with_source(copied.display().to_string());
    let new = filter_bytes(id, "2.0");
    let written = crate::filter_plugins::write_into(&dir, lcc_filters::Filter::load(&new, limits.clone()).unwrap(), &new, Some(&installed));
    let dest = dir.join(format!("{id}.wasm"));
    let (copied_gone, dest_bytes) = (!copied.exists(), std::fs::read(&dest).ok());
    let r = lcc_filters::Registry::new(limits);
    crate::filter_plugins::load_dir(&r, &dir);
    let loaded = r.get(id).map(|f| f.manifest().version.clone());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(written.unwrap().source(), Some(dest.display().to_string().as_str()));
    assert_eq!(dest_bytes, Some(new), "the update is <id>.wasm");
    assert!(copied_gone, "the file it replaces went");
    assert_eq!(loaded.as_deref(), Some("2.0"), "the next start loads the update");
}

/// A plug-in module that failed to load still holds its id: a filter with that id is refused,
/// so removing the plug-in's file never takes the filter's id with it.
#[test]
fn a_plugin_that_failed_to_load_still_holds_its_id() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let id = "test.installs.failedclash";
    let plugin = module_file("failedclash-plugin", &json!({"id": id, "name": "Failed Clash", "version": "1.0.0"}));
    let filter = filter_file("failedclash-filter", id, "1.0");
    crate::cmd::plugins::manager().get_or_insert_with(lcc_plugins::Manager::in_memory).failed.push(lcc_plugins::Failed {
        file: plugin.display().to_string(),
        error: "needs a newer host".into(),
        missing: vec![],
        id: id.into(),
        path: Some(plugin.to_path_buf()),
    });
    let conflict = s.execute("plugin.inspectFile", &json!({"path": filter.path()})).unwrap()["conflict"].clone();
    let refused = s.execute("plugin.install", &json!({"path": filter.path()}));
    let filter_kept = crate::filter_plugins::registry().get(id).is_some();
    crate::filter_plugins::registry().remove(id);
    crate::cmd::plugins::manager().as_mut().unwrap().failed.retain(|f| f.id != id);
    assert!(conflict.is_string(), "the dialog names the clash: {conflict}");
    let e = refused.unwrap_err().to_string();
    assert!(e.contains(id) && e.contains("plug-in"), "{e}");
    assert!(!filter_kept, "no filter installed");
}

/// A plug-in's About fields (host feature `about`): known fields, so no unknown-field warning;
/// `plugin.inspectFile` and `plugin.list` give them, with Report a Problem's issues page for a
/// GitHub repository; a value the host can't use is dropped, named in the plug-in's log and
/// `dropped`, and the install still goes ahead.
#[test]
fn about_fields_are_listed_and_bad_ones_dropped() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let dir = folder("about");
    let id = "test.installs.about";
    let good = module_file(
        "about-good",
        &json!({"id": id, "name": "A", "version": "1.0.0", "requires": ["about", "pictures"],
            "homepage": "https://example.com/a", "repository": "https://github.com/me/plugins/tree/main/a",
            "changelog": "https://example.com/a/changes", "license": "MIT OR Apache-2.0"}),
    );
    let bad = module_file(
        "about-bad",
        &json!({"id": id, "name": "A", "version": "1.0.1", "homepage": "http://example.com", "repository": 7,
            "changelog": "javascript:alert(1)", "license": "MIT"}),
    );
    let before = crate::cmd::plugins::manager().replace(lcc_plugins::Manager::open(&dir).unwrap());
    let inspected = s.execute("plugin.inspectFile", &json!({"path": good.path()}));
    let installed = s.execute("plugin.install", &json!({"path": good.path()}));
    let first = listed(&mut s, id);
    let features = s.execute("plugin.list", &json!({})).map(|l| l["host"]["features"].clone());
    let inspected_bad = s.execute("plugin.inspectFile", &json!({"path": bad.path()}));
    let installed_bad = s.execute("plugin.install", &json!({"path": bad.path()}));
    let second = listed(&mut s, id);
    let log = s.execute("plugin.log", &json!({"id": id}));
    *crate::cmd::plugins::manager() = before;
    let _ = std::fs::remove_dir_all(&dir);

    let about = json!({"homepage": "https://example.com/a", "repository": "https://github.com/me/plugins/tree/main/a",
        "changelog": "https://example.com/a/changes", "license": "MIT OR Apache-2.0", "issues": "https://github.com/me/plugins/issues"});
    let inspected = inspected.unwrap();
    assert_eq!(inspected["manifest"]["about"], about, "{inspected}");
    assert_eq!((&inspected["unknownFields"], &inspected["dropped"]), (&json!([]), &json!([])), "all four are known and kept");
    assert_eq!(installed.unwrap()["about"], about, "a manifest that requires `about` and `pictures` installs");
    assert_eq!(first.unwrap()["about"], about);
    let features = features.unwrap();
    assert!(["about", "pictures"].iter().all(|f| features.as_array().unwrap().iter().any(|x| x == f)), "{features}");

    let inspected_bad = inspected_bad.unwrap();
    let mut dropped: Vec<String> = serde_json::from_value(inspected_bad["dropped"].clone()).unwrap();
    dropped.sort();
    assert_eq!(dropped, ["changelog: not an https:// link", "homepage: not an https:// link", "repository: not text"]);
    assert_eq!(inspected_bad["unknownFields"], json!([]), "dropped, not unknown");
    assert!(installed_bad.is_ok(), "a bad value never stops the install: {installed_bad:?}");
    assert_eq!(second.unwrap()["about"], json!({"license": "MIT"}), "only the value it can use");
    let log = log.unwrap().to_string();
    for field in ["homepage", "repository", "changelog"] {
        assert!(log.contains(&format!("manifest value dropped: {field}: ")), "the log names {field}: {log}");
    }
}

/// A pixel filter takes the same About fields with the same checks as a plug-in: shown in
/// `plugin.list` the same way, a value it can't use dropped and named in `plugin.inspectFile`.
#[test]
fn a_filters_about_fields_are_checked_as_a_plugins() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let id = "test.installs.filterabout";
    let manifest = format!(
        r#"{{"id":"{id}","name":"F","version":"1","kind":"filter","params":{{}},"homepage":"https://example.com/f","repository":"https://github.com/me/filters","changelog":7,"license":"MIT"}}"#
    );
    let dir = folder("filterabout");
    let file = dir.join("filter.wasm");
    std::fs::write(&file, crate::tests_plugin_filters::module(&manifest, None)).unwrap();
    let info = s.execute("plugin.inspectFile", &json!({"path": file})).unwrap();
    s.execute("plugin.install", &json!({"path": file})).unwrap();
    let row = listed(&mut s, id);
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let about = json!({"homepage": "https://example.com/f", "repository": "https://github.com/me/filters", "license": "MIT",
        "issues": "https://github.com/me/filters/issues"});
    assert_eq!(info["manifest"]["about"], about, "{info}");
    assert_eq!(info["dropped"], json!(["changelog: not text"]));
    // a value dropped for its type is still a known field (the lenient list), not an unknown one
    assert_eq!(info["unknownFields"], json!([]), "{info}");
    assert_eq!(row.unwrap()["about"], about);
}

/// Plug-ins and pixel filters keep the About checks in two copies (`lcc-filters` doesn't depend on
/// the plug-in host): both copies keep and drop the same values, with the same reasons.
#[test]
fn plugin_and_filter_about_checks_agree() {
    let huge = format!("https://example.com/{}", "a".repeat(lcc_plugins::about::MAX_LINK_BYTES));
    let values = [
        json!("https://example.com"),
        json!("https://github.com/me/x/tree/main/y"),
        json!("http://example.com"),
        json!("javascript:alert(1)"),
        json!("https://a@b.com/"),
        json!("https://éxample.com/"),
        json!(""),
        json!(null),
        json!(7),
        json!({"a": 1}),
        json!(huge),
        json!("MIT\nOR GPL"),
        json!("MIT\u{202E}LPG"),
        json!("x".repeat(lcc_plugins::about::MAX_LICENSE_BYTES + 1)),
    ];
    for field in lcc_plugins::about::ABOUT_FIELDS {
        for v in &values {
            let raw = json!({ *field: v });
            let (p, pd) = lcc_plugins::About::parse(&raw);
            let (f, fd) = lcc_filters::About::parse(&raw);
            assert_eq!((p.info(), pd), (f.info(), fd), "{raw}");
        }
    }
    for url in ["https://GitHub.com/a", "https://x.com:1/", "https://x.com:/", "https://a..b/"] {
        assert_eq!(lcc_plugins::about::link_host(url), lcc_filters::about::link_host(url), "{url}");
    }
}

/// A plug-in whose `lcc_manifest` returns `manifest` (or traps, when `None`), stamped with `section`,
/// written to `<name>.wasm` in a Temp folder of its own.
fn stamped_file(name: &str, manifest: Option<&Value>, section: &Value) -> lcc_testkit::TempFile {
    let esc = |s: &str| s.replace('"', "\\\"");
    let (data, body) = match manifest {
        Some(m) => {
            let m = m.to_string();
            (esc(&m), format!("(i64.or (i64.shl (i64.const {}) (i64.const 32)) (i64.const 16))", m.len()))
        }
        None => (String::new(), "(unreachable)".to_string()),
    };
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{data}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) {body})
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192)))
  (@custom "{name_}" "{section}"))"#,
        name_ = lcc_plugins::MANIFEST_SECTION,
        section = esc(&section.to_string()),
    );
    let file = lcc_testkit::TempFile::new(&format!("lcc-engine-installs-stamped-{name}"), &format!("{name}.wasm"));
    std::fs::write(&file, wat::parse_str(&src).unwrap()).unwrap();
    file
}

/// A stamped file inspects from its section without running (its `lcc_manifest` traps), with every
/// field an unstamped one has; `verify` reads `lcc_manifest`, and so does an unstamped file.
#[test]
fn a_stamped_file_inspects_from_its_section_without_running() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let manifest = json!({"id": "test.installs.stamped", "name": "Stamped", "version": "1.0.0", "permissions": {"catalog": true}});
    let traps = stamped_file("traps", None, &manifest);
    let info = s.execute("plugin.inspectFile", &json!({"path": traps.path()})).unwrap();
    assert_eq!((&info["source"], &info["verified"]), (&json!("section"), &json!(false)), "{info}");
    assert_eq!(info["manifest"]["id"], "test.installs.stamped");
    assert_eq!(info["manifest"]["permissions"]["catalog"], true);
    for field in ["installed", "missing", "versionError", "downgrade", "conflict", "unknownFields", "dropped"] {
        assert!(info.get(field).is_some(), "{field}: {info}");
    }
    let e = s.execute("plugin.inspectFile", &json!({"path": traps.path(), "verify": true})).unwrap_err().to_string();
    assert!(e.contains("trapped"), "verify runs lcc_manifest: {e}");
    assert!(s.execute("plugin.install", &json!({"path": traps.path()})).is_err(), "and installing does");
    assert!(listed(&mut s, "test.installs.stamped").is_none());

    let honest = stamped_file("honest", Some(&manifest), &manifest);
    let verified = s.execute("plugin.inspectFile", &json!({"path": honest.path(), "verify": true})).unwrap();
    assert_eq!((&verified["source"], &verified["verified"]), (&json!("export"), &json!(true)), "{verified}");
    let unverified = s.execute("plugin.inspectFile", &json!({"path": honest.path()})).unwrap();
    assert_eq!(unverified["manifest"], verified["manifest"], "a true section reads as lcc_manifest does");

    let plain = module_file("installs-unstamped", &json!({"id": "test.installs.unstamped", "name": "Plain", "version": "1.0.0"}));
    let info = s.execute("plugin.inspectFile", &json!({"path": plain.path()})).unwrap();
    assert_eq!((&info["source"], &info["verified"]), (&json!("export"), &json!(true)), "{info}");
    assert_eq!(s.execute("plugin.inspectFile", &json!({"path": plain.path(), "verify": true})).unwrap(), info, "verify changes nothing for it");
}

/// A section that hides a permission: inspected unverified it reads as it claims; verified, and
/// installed, it's refused naming the fields, and nothing is installed.
#[test]
fn a_section_that_differs_from_lcc_manifest_is_refused() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let asks = json!({"id": "test.installs.lying", "name": "Lying", "version": "1.0.0", "permissions": {"catalog": true}});
    let claims = json!({"id": "test.installs.lying", "name": "Lying", "version": "1.0.0"});
    let lying = stamped_file("lying", Some(&asks), &claims);
    let info = s.execute("plugin.inspectFile", &json!({"path": lying.path()})).unwrap();
    assert_eq!(info["verified"], false);
    assert!(info["manifest"]["permissions"]["catalog"] != true, "unverified, the claim: {info}");
    let verified = s.execute("plugin.inspectFile", &json!({"path": lying.path(), "verify": true})).unwrap_err().to_string();
    let installed = s.execute("plugin.install", &json!({"path": lying.path(), "grant": {"catalog": true}})).unwrap_err().to_string();
    assert!(verified.contains("`permissions`"), "{verified}");
    assert!(installed.contains("`permissions`"), "{installed}");
    assert!(listed(&mut s, "test.installs.lying").is_none(), "nothing installed");
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

/// `plugin.inspectFile` says who signed a file and whether that's the installed plug-in's pinned
/// signer; `plugin.install` pins the first signer and takes `acceptSigner` / `acceptUnsigned` for a
/// file that isn't signed by it; `plugin.list` shows the pin.
#[test]
fn signed_files_are_inspected_and_another_signer_installs_on_the_users_yes() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let id = "test.installs.signed";
    let dir = folder("signed");
    let version = |v: &str| {
        let f = module_file(&format!("signed-{v}"), &json!({"id": id, "name": "Signed", "version": v}));
        std::fs::read(f.path()).unwrap()
    };
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let (v1, fp1) = signed(&version("1.0.0"), 1);
    let (v2, fp2) = signed(&version("2.0.0"), 2);
    let (v1, v2) = (write("v1.wasm", &v1), write("v2.wasm", &v2));
    let invalid = write("bad.wasm", &[std::fs::read(&v2).unwrap().as_slice(), &[0, 2, 1, b'x']].concat());
    let unsigned = write("v3.wasm", &version("3.0.0"));
    let inspect = |s: &mut Session, p: &std::path::Path| s.execute("plugin.inspectFile", &json!({"path": p, "verify": true})).unwrap();

    let info = inspect(&mut s, &v1);
    assert_eq!(
        (&info["signature"], &info["signer"], &info["pinned"], &info["matchesPin"]),
        (&json!("valid"), &json!(fp1), &Value::Null, &Value::Null),
        "{info}"
    );
    s.execute("plugin.install", &json!({"path": v1})).unwrap();
    assert_eq!(listed(&mut s, id).unwrap()["signer"], fp1, "pinned at the first signed install");

    let info = inspect(&mut s, &v2);
    assert_eq!(info["signer"], fp2);
    let key2 = info["signerKey"].as_str().unwrap().to_string();
    assert_eq!(lcc_plugins::Signer::from_base64(&key2).unwrap().fingerprint(), fp2, "the full key behind the fingerprint");
    assert_eq!(info["pinned"], json!({"signer": fp1, "readable": true}), "{info}");
    assert_eq!(info["matchesPin"], false);
    let e = s.execute("plugin.install", &json!({"path": v2})).unwrap_err().to_string();
    assert!(e.contains(&fp1) && e.contains(&fp2) && e.contains("acceptSigner"), "{e}");
    assert!(s.execute("plugin.install", &json!({"path": v2, "acceptSigner": fp2, "acceptUnsigned": true})).is_err(), "one answer at a time");
    assert!(s.execute("plugin.install", &json!({"path": v2, "acceptSigner": 7})).is_err());
    let other_key = signed(&version("9.0.0"), 9).0;
    let other_key = s.execute("plugin.inspectFile", &json!({"path": write("other.wasm", &other_key)})).unwrap()["signerKey"].clone();
    assert!(s.execute("plugin.install", &json!({"path": v2, "acceptSigner": other_key})).is_err(), "another signer's full key");
    s.execute("plugin.install", &json!({"path": v2, "acceptSigner": key2})).unwrap();
    assert_eq!(listed(&mut s, id).unwrap()["signer"], fp2);
    assert_eq!(inspect(&mut s, &v2)["matchesPin"], true);

    let info = inspect(&mut s, &invalid);
    assert_eq!(info["signature"], "invalid", "{info}");
    assert!(info["signatureError"].as_str().unwrap().contains("last section"), "{info}");
    assert!(s.execute("plugin.install", &json!({"path": invalid, "acceptSigner": fp2})).unwrap_err().to_string().contains("plug-in signature"));

    assert_eq!(inspect(&mut s, &unsigned)["signature"], "none");
    assert!(s.execute("plugin.install", &json!({"path": unsigned})).unwrap_err().to_string().contains("acceptUnsigned"));
    s.execute("plugin.install", &json!({"path": unsigned, "acceptUnsigned": true})).unwrap();
    let after = listed(&mut s, id).unwrap();
    s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((&after["version"], &after["signer"]), (&json!("3.0.0"), &Value::Null), "installed unsigned, as the user said yes to");
}
