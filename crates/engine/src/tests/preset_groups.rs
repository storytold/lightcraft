//! Preset groups made by name stay while empty and across reopening, and can be renamed, starred
//! and deleted; a group holding built-in presets can only be starred.

use std::path::PathBuf;

use lightcraft_develop::Preset;
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-preset-groups-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A session holding the user presets `a`, `b` and `c` in group "Mine"; returns their ids.
fn with_user_presets() -> (Session, Vec<String>) {
    let mut s = Session::new();
    let presets = ["a", "b", "c"].map(|name| Preset {
        id: String::new(),
        name: name.into(),
        group: "Mine".into(),
        settings: json!({"light": {"exposure": 0.5}}),
        favorite: false,
        builtin: false,
    });
    let ids = s.add_presets(presets.to_vec());
    (s, ids)
}

fn group_of(s: &Session, id: &str) -> String {
    s.presets.iter().find(|p| p.id == id).map(|p| p.group.clone()).unwrap()
}

#[test]
fn a_created_group_is_kept_empty_and_refuses_a_name_in_use() {
    let (mut s, _) = with_user_presets();
    let before = s.presets.len();
    let generation = s.presets.generation();
    assert_eq!(s.execute("preset.createGroup", &json!({"name": "  Empty  "})).unwrap()["name"], "Empty");
    assert!(s.presets.created_groups().contains("Empty"), "the name is trimmed");
    assert_eq!(s.presets.len(), before, "no preset was made or moved");
    assert_ne!(s.presets.generation(), generation, "saving and the panel see the change");
    for taken in ["Empty", "Mine"] {
        assert!(s.execute("preset.createGroup", &json!({"name": taken})).is_err(), "{taken}");
    }
    for bad in [json!({}), json!({"name": "   "}), json!({"name": "x".repeat(101)})] {
        assert!(s.execute("preset.createGroup", &bad).is_err(), "{bad}");
    }
}

#[test]
fn created_groups_survive_reopening_and_a_bad_entry_is_dropped() {
    let lib = temp_dir("reopen");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("preset.createGroup", &json!({"name": "Empty"})).unwrap();
    drop(s);

    let mut t = Session::new().with_fs();
    t.open_library(&lib, false).unwrap();
    assert!(t.presets.created_groups().contains("Empty"));
    t.execute("preset.deleteGroup", &json!({"name": "Empty"})).unwrap();
    drop(t);

    let mut u = Session::new().with_fs();
    u.open_library(&lib, false).unwrap();
    assert!(u.presets.created_groups().is_empty(), "a deleted group stays deleted");
    drop(u);

    // edited by hand: blank and overlong names don't become groups
    let long = "x".repeat(101);
    std::fs::write(lib.join("presets.json"), json!({"groups": ["  Kept ", "   ", long]}).to_string()).unwrap();
    let mut v = Session::new().with_fs();
    v.open_library(&lib, false).unwrap();
    assert_eq!(v.presets.created_groups().iter().collect::<Vec<_>>(), ["Kept"]);
}

#[test]
fn a_library_without_created_groups_writes_no_groups_field() {
    let lib = temp_dir("no-field");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("preset.createGroup", &json!({"name": "Empty"})).unwrap();
    s.execute("preset.deleteGroup", &json!({"name": "Empty"})).unwrap();
    drop(s);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(lib.join("presets.json")).unwrap()).unwrap();
    assert!(saved.get("groups").is_none(), "{saved}");
}

#[test]
fn delete_group_takes_its_user_presets_and_spares_built_in_groups() {
    let (mut s, ids) = with_user_presets();
    let builtin_group = s.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    assert!(s.execute("preset.deleteGroup", &json!({"name": builtin_group})).is_err());
    assert!(s.execute("preset.deleteGroup", &json!({"name": "No such group"})).is_err());
    assert_eq!(s.execute("preset.deleteGroup", &json!({"name": "Mine"})).unwrap()["deleted"], 3);
    assert!(!s.presets.iter().any(|p| ids.contains(&p.id)));
}

#[test]
fn rename_group_moves_its_presets_and_refuses_what_it_should() {
    let (mut s, ids) = with_user_presets();
    s.execute("preset.createGroup", &json!({"name": "Empty"})).unwrap();
    assert_eq!(s.execute("preset.renameGroup", &json!({"name": "Mine", "to": " Ours "})).unwrap()["renamed"], 3);
    assert!(ids.iter().all(|id| group_of(&s, id) == "Ours"));

    s.execute("preset.renameGroup", &json!({"name": "Empty", "to": "Still empty"})).unwrap();
    assert!(s.presets.created_groups().contains("Still empty"));
    assert!(!s.presets.created_groups().contains("Empty"), "the old name doesn't linger");

    assert!(s.execute("preset.renameGroup", &json!({"name": "Ours", "to": "Still empty"})).is_err(), "groups don't merge");
    assert!(s.execute("preset.renameGroup", &json!({"name": "No such group", "to": "x"})).is_err());
    assert!(s.execute("preset.renameGroup", &json!({"name": "Ours", "to": "  "})).is_err());
    let builtin_group = s.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    assert!(s.execute("preset.renameGroup", &json!({"name": builtin_group, "to": "Mine now"})).is_err());
    assert_eq!(s.execute("preset.renameGroup", &json!({"name": "Ours", "to": "Ours"})).unwrap()["renamed"], 0, "the same name changes nothing");
    assert!(ids.iter().all(|id| group_of(&s, id) == "Ours"));
}

#[test]
fn a_starred_group_follows_its_renaming_and_goes_with_its_deletion() {
    let (mut s, _) = with_user_presets();
    assert_eq!(s.execute("preset.favoriteGroup", &json!({"name": "Mine"})).unwrap()["favorite"], true);
    assert_eq!(s.execute("preset.favoriteGroup", &json!({"name": "Mine"})).unwrap()["favorite"], false, "it toggles");
    s.execute("preset.favoriteGroup", &json!({"name": "Mine", "favorite": true})).unwrap();
    s.execute("preset.renameGroup", &json!({"name": "Mine", "to": "Ours"})).unwrap();
    assert_eq!(s.presets.favorite_groups().iter().collect::<Vec<_>>(), ["Ours"]);
    s.execute("preset.deleteGroup", &json!({"name": "Ours"})).unwrap();
    assert!(s.presets.favorite_groups().is_empty());
    assert!(s.execute("preset.favoriteGroup", &json!({"name": "No such group"})).is_err());

    let builtin_group = s.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    s.execute("preset.favoriteGroup", &json!({"name": builtin_group})).unwrap();
    assert!(s.presets.favorite_groups().contains(&builtin_group), "built-in groups can be starred");
}

#[test]
fn starred_groups_survive_reopening() {
    let lib = temp_dir("starred");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let builtin_group = s.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    s.execute("preset.favoriteGroup", &json!({"name": builtin_group})).unwrap();
    drop(s);
    let mut t = Session::new().with_fs();
    t.open_library(&lib, false).unwrap();
    assert!(t.presets.favorite_groups().contains(&builtin_group));
}

#[test]
fn a_created_group_stays_after_its_last_preset_leaves() {
    let (mut s, ids) = with_user_presets();
    s.execute("preset.createGroup", &json!({"name": "Holding"})).unwrap();
    for group in ["Holding", "Elsewhere"] {
        for id in &ids {
            s.execute("preset.move", &json!({"id": id, "group": group})).unwrap();
        }
    }
    assert!(s.presets.created_groups().contains("Holding"));
    assert!(!s.presets.iter().any(|p| p.group == "Holding"));
}
