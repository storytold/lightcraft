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
    fuzz("actions.file", &[seed.as_bytes()], 5000, |s| {
        if let Ok(list) = parse(s) {
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

/// A tiny seeded mutator (this crate is standalone: no workspace dev-dependencies, so not
/// `dac-fuzzkit`). Bit flips, byte writes, truncation, deletion, duplication and inserted
/// tokens; `DAC_FUZZ_ITERS` raises the count.
fn fuzz(name: &str, seeds: &[&[u8]], default: usize, mut check: impl FnMut(&[u8])) {
    let mut x = name.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)) | 1;
    let mut next = move |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if n == 0 { 0 } else { (x % n as u64) as usize }
    };
    const TOKENS: &[&[u8]] = &[b"\0", b"\xff\xff\xff\xff", b"-1", b"1e309", b"null", b"[]", b"{}", b"\"\"", b"[[[[", b"99999999999999999999"];
    let iters = std::env::var("DAC_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(default);
    for s in seeds {
        check(s);
    }
    for i in 0..iters {
        let mut v = seeds[i % seeds.len()].to_vec();
        for _ in 0..1 + next(6) {
            let len = v.len();
            match next(6) {
                0 if len > 0 => {
                    let k = next(len);
                    v[k] ^= 1 << next(8);
                }
                1 if len > 0 => {
                    let k = next(len);
                    v[k] = next(256) as u8;
                }
                2 => v.truncate(next(len + 1)),
                3 if len > 0 => {
                    let a = next(len);
                    let b = (a + 1 + next(16)).min(len);
                    v.drain(a..b);
                }
                4 if len > 0 => {
                    let a = next(len);
                    let b = (a + 1 + next(64)).min(len);
                    let chunk = v[a..b].to_vec();
                    let at = next(v.len() + 1);
                    v.splice(at..at, chunk);
                }
                _ => {
                    let t = TOKENS[next(TOKENS.len())];
                    let at = next(len + 1);
                    v.splice(at..at, t.iter().copied());
                }
            }
        }
        check(&v);
    }
}
