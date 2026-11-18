//! LR-PRE-ONIMPORT / LR-PRE-MANAGE: presets marked At Import or hidden, built-in ones too, are
//! listed as such and kept with the library.

use serde_json::json;

use crate::Session;

#[test]
fn at_import_and_hidden_marks_toggle_and_are_listed() {
    let mut s = Session::with_demo();
    let id = s.presets.first().unwrap().id.clone();
    let g = s.presets.generation();
    assert_eq!(s.execute("preset.forImport", &json!({"id": id})).unwrap(), json!({"on": true}));
    assert!(s.presets.generation() != g, "a mark is a change of the preset list");
    assert_eq!(s.execute("preset.hide", &json!({"id": id, "hidden": true})).unwrap(), json!({"hidden": true}));
    let list = s.execute("presets.list", &json!({})).unwrap();
    let row = list.as_array().unwrap().iter().find(|p| p["id"] == json!(id)).unwrap().clone();
    assert_eq!((row["atImport"].clone(), row["hidden"].clone()), (json!(true), json!(true)));
    assert_eq!(s.execute("preset.forImport", &json!({"id": id})).unwrap(), json!({"on": false}));
    assert!(s.preset_at_import.is_empty() && s.preset_hidden == vec![id]);
    assert!(s.execute("preset.hide", &json!({"id": "no.such"})).is_err());
    assert!(s.execute("preset.forImport", &json!({})).is_err());
}

/// LR-PRE-MANAGE: built-in presets can be renamed and moved (alone or with their group), a whole
/// group hidden, and Restore Built-ins gives them back as shipped; the edits last in the library.
#[test]
fn builtin_edits_group_hiding_and_restore() {
    let dir = lcc_testkit::TempDir::new("lc-engine-builtin-edits");
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    let b = s.presets.iter().find(|p| p.builtin).unwrap().clone();
    s.execute("preset.rename", &json!({"id": b.id, "name": "Mine Now"})).unwrap();
    s.execute("preset.move", &json!({"id": b.id, "group": "Weddings"})).unwrap();
    let now = |s: &Session| s.presets.iter().find(|p| p.id == b.id).cloned().unwrap();
    assert_eq!((now(&s).name.as_str(), now(&s).group.as_str()), ("Mine Now", "Weddings"));
    // a group holding a built-in is renamed whole, never deleted
    s.execute("preset.renameGroup", &json!({"group": "Weddings", "name": "Weddings 2026"})).unwrap();
    assert_eq!(now(&s).group, "Weddings 2026");
    assert!(s.execute("preset.deleteGroup", &json!({"group": "Weddings 2026"})).is_err());
    let r = s.execute("preset.hideGroup", &json!({"group": "Weddings 2026"})).unwrap();
    assert_eq!(r, json!({"hidden": true, "count": 1}));
    assert!(s.preset_hidden.contains(&b.id));
    assert!(s.execute("preset.hideGroup", &json!({"group": "No Such"})).is_err());
    s.save_prefs().unwrap();
    s.persist().unwrap();
    drop(s);
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    assert_eq!((now(&s).name.as_str(), now(&s).group.as_str()), ("Mine Now", "Weddings 2026"), "kept with the library");
    let r = s.execute("preset.restoreBuiltins", &json!({})).unwrap();
    assert!(r["restored"].as_u64().unwrap() >= 3, "{r}");
    assert_eq!((now(&s).name, now(&s).group), (b.name.clone(), b.group.clone()));
    assert!(!s.preset_hidden.contains(&b.id));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hostile parameters: never a panic, always an error.
#[test]
fn manager_commands_refuse_bad_parameters() {
    let mut s = Session::with_demo();
    for (c, p) in [
        ("preset.rename", json!({"id": "x", "name": "y"})),
        ("preset.rename", json!({"id": 5})),
        ("preset.move", json!({"id": "x", "group": ""})),
        ("preset.hideGroup", json!({"group": null})),
        ("preset.renameGroup", json!({"group": "a", "name": "b/c"})),
        ("preset.moveGroup", json!({"group": "a", "parent": "a/b"})),
    ] {
        assert!(s.execute(c, &p).is_err(), "{c} {p}");
    }
}

/// LR-PRE-MANAGE: presets are copied (built-ins too) and deleted several at once; a folder delete can be guarded by
/// the count the user was shown.
#[test]
fn copy_delete_many_and_guarded_folder_delete() {
    let mut s = Session::with_demo();
    let b = s.presets.iter().find(|p| p.builtin).unwrap().clone();
    let r = s.execute("preset.copy", &json!({"ids": [b.id, "no.such"], "group": "Mine"})).unwrap();
    let new_ids: Vec<String> = r["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(new_ids.len(), 1);
    let copy = s.presets.iter().find(|p| p.id == new_ids[0]).unwrap().clone();
    assert!(!copy.builtin && !copy.favorite && copy.group == "Mine" && copy.settings == b.settings);
    // again: the same preset is already in the folder
    let r = s.execute("preset.copy", &json!({"ids": [b.id], "group": "mine"})).unwrap();
    assert_eq!((r["ids"].as_array().unwrap().len(), r["skipped"].clone()), (0, json!(1)));
    assert!(s.execute("preset.copy", &json!({"ids": ["no.such"], "group": "Mine"})).is_err());
    assert!(s.execute("preset.copy", &json!({"ids": [b.id]})).is_err());
    assert!(s.execute("preset.deleteGroup", &json!({"group": "Mine", "expect": 5})).is_err());
    assert!(s.presets.iter().any(|p| p.id == copy.id), "a wrong count deletes nothing");
    let r = s.execute("preset.deleteMany", &json!({"ids": [copy.id, b.id, "no.such"]})).unwrap();
    assert_eq!((r["deleted"].clone(), r["skipped"].clone()), (json!(1), json!(2)));
    assert!(s.presets.iter().any(|p| p.id == b.id), "built-ins stay");
    assert!(s.execute("preset.deleteMany", &json!({})).is_err());
}
