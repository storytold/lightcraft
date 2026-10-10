//! Never-crash (P6.2): `actions.json` (hand-edited, imported from another machine, damaged) and
//! the arguments an action is played with are errors, never a panic; a full disk while saving
//! leaves the old file and no temp file.

use dac_actions::{Action, Param, Step, bind, parse, save, steps_from_journal, to_bytes};
use serde_json::{Map, Value, json};

fn sample() -> Vec<Action> {
    let mut a = Action::new("Warm & export");
    a.description = "two steps".into();
    a.steps = vec![
        Step { command: "develop.set".into(), params: json!({"light.exposure": "{{ev}}", "ids": [1, 2]}) },
        Step { command: "export.run".into(), params: json!({"preset": "{{preset}}", "dir": "/out/{{ev}}"}) },
    ];
    a.params = vec![
        Param { name: "ev".into(), default: json!(0.5), description: String::new() },
        Param { name: "preset".into(), default: Value::Null, description: "x".into() },
    ];
    vec![a, Action::new("Empty")]
}

#[test]
fn damaged_actions_files_never_panic() {
    let seed = String::from_utf8(to_bytes(&sample()).unwrap()).unwrap();
    assert!(parse(seed.as_bytes()).is_ok());
    dac_fuzzkit::run_json("actions.file", &[&seed], 3000, |s| {
        if let Ok(list) = parse(s.as_bytes()) {
            for a in &list {
                let mut args = Map::new();
                for (k, v) in [("ev", json!(1.5)), ("preset", json!("{{ev}}")), ("x", json!([[[]]]))] {
                    args.insert(k.into(), v);
                }
                let _ = bind(a, &args);
                let _ = bind(a, &Map::new());
            }
        }
    });
}

#[test]
fn hostile_journal_and_arguments_never_panic() {
    let deep = (0..200).fold(json!(1), |v, _| json!([v]));
    let entries: Vec<(String, Value)> = vec![
        ("develop.set".into(), json!({"ids": [1], "x": "{{a}}"})),
        ("actions.play".into(), json!({"name": "x"})),
        ("view.zoom".into(), json!(null)),
        ("x".repeat(10_000), deep.clone()),
    ];
    let steps = steps_from_journal(&entries, false, |_| true);
    let mut a = Action::new("rec");
    a.steps = steps;
    a.steps.push(Step { command: "develop.set".into(), params: json!({"deep": deep, "s": "{{".repeat(1000)}) });
    let _ = a.validate();
    let mut args = Map::new();
    args.insert("a".into(), json!("{{a}}"));
    let _ = bind(&a, &args);
}

#[test]
fn full_disk_save_leaves_the_old_file() {
    let dir = std::env::temp_dir().join(format!("dac-actions-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("actions.json");
    save(&path, &sample()).unwrap();
    let before = std::fs::read(&path).unwrap();
    // the temp file's name taken by a folder: the write fails part-way like a full disk would
    std::fs::create_dir_all(path.with_extension("json.tmp")).unwrap();
    assert!(save(&path, &[]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}
