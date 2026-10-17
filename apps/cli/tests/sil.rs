//! SIL (software-in-the-loop) harness: drives the real app — the desktop UI, engine and catalog,
//! run headless by `<cli> snapshot --library DIR --script -` — through Lightroom Classic-style
//! workflows on a library on disk, and checks invariants after every step (never exact outputs or
//! image quality). See docs/testing-sil.md.
//!
//! Environment: `SIL_SEED` (monkey seed), `SIL_STEPS` (monkey step budget, default 400),
//! `SIL_CORPUS` (folder of raws; default `<repo>/corpus/raw`, skipped when absent),
//! `SIL_OUT` (where reports and replay scripts go; default the system temp dir).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");
/// A step that takes longer than this is a deadlock as far as the harness is concerned.
const STEP_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- the driver

/// One running app process on a library, and the record of what was sent to it.
struct App {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<String>,
    stderr: PathBuf,
}

struct Sil {
    name: String,
    root: PathBuf,
    lib: PathBuf,
    app: Option<App>,
    /// `step | request | outcome` lines.
    report: Vec<String>,
    /// Every request sent, as a script `<cli> snapshot --library DIR --script FILE` replays.
    replay: Vec<String>,
    seed: Option<u64>,
    t0: Instant,
}

impl Sil {
    fn new(name: &str) -> Sil {
        let base = std::env::var_os("SIL_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("sil-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        let lib = root.join("library");
        Sil { name: name.into(), root, lib, app: None, report: vec![], replay: vec![], seed: None, t0: Instant::now() }
    }

    /// Start the app on the library (creating it on first start).
    fn start(&mut self) {
        assert!(self.app.is_none(), "already running");
        let n = self.report.len();
        let stderr = self.root.join(format!("stderr-{n}.log"));
        let mut child = Command::new(BIN)
            .args(["snapshot", "--library"])
            .arg(&self.lib)
            .args(["--script", "-", "--size", "1280x800"])
            .env(lcc_brand::env_var("CONFIG_DIR"), self.root.join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        self.replay.push(format!("# --- start: {BIN} snapshot --library {} --script -", self.lib.display()));
        self.report.push(format!("{:>7.1}s  start app on {}", self.t0.elapsed().as_secs_f64(), self.lib.display()));
        let stdin = child.stdin.take();
        self.app = Some(App { child, stdin, replies: rx, stderr });
    }

    /// End the session the way a user quits: EOF, then the process exits on its own.
    fn quit(&mut self) {
        let Some(mut app) = self.app.take() else { return };
        drop(app.stdin.take());
        let t = Instant::now();
        loop {
            match app.child.try_wait().unwrap() {
                Some(status) => {
                    // a failed request makes the exit status non-zero; a crash is a signal or a panic
                    let log = std::fs::read_to_string(&app.stderr).unwrap_or_default();
                    self.check(!log.contains("panicked at"), "the app panicked (see stderr)", &log_tail(&log));
                    self.check(status.code().is_some(), "the app was killed by a signal", &format!("{status:?}"));
                    self.report.push(format!("{:>7.1}s  quit: {status}", self.t0.elapsed().as_secs_f64()));
                    self.replay.push("# --- quit (EOF)".into());
                    return;
                }
                None if t.elapsed() > STEP_TIMEOUT => {
                    let _ = app.child.kill();
                    self.fail("the app did not exit after EOF (deadlock?)", "");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    fn reopen(&mut self) {
        self.quit();
        self.start();
    }

    /// Send one control request; the process must stay alive and answer within the timeout.
    /// Returns the whole reply (`ok`, `result` | `error`).
    fn request(&mut self, method: &str, params: Value) -> Value {
        let line = json!({"method": method, "params": params}).to_string();
        self.replay.push(line.clone());
        let ts = Instant::now();
        let app = self.app.as_mut().expect("app not running");
        let sent = app.stdin.as_mut().map(|s| writeln!(s, "{line}").and_then(|_| s.flush()));
        if !matches!(sent, Some(Ok(()))) {
            let log = std::fs::read_to_string(&app.stderr).unwrap_or_default();
            self.fail(&format!("the app is gone (write failed) at {line}"), &log_tail(&log));
        }
        let reply = match app.replies.recv_timeout(STEP_TIMEOUT) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => self.fail(&format!("no answer within {STEP_TIMEOUT:?} (deadlock?) to {line}"), ""),
            Err(RecvTimeoutError::Disconnected) => {
                let app = self.app.as_mut().unwrap();
                let status = app.child.wait().ok();
                let log = std::fs::read_to_string(&app.stderr).unwrap_or_default();
                self.fail(&format!("the app died ({status:?}) at {line}"), &log_tail(&log))
            }
        };
        let v: Value = match serde_json::from_str(&reply) {
            Ok(v) => v,
            Err(e) => self.fail(&format!("unparseable reply {reply:?}: {e}"), ""),
        };
        // every request either succeeds or fails cleanly with a message
        let ok = v.get("ok").and_then(Value::as_bool);
        let outcome = match ok {
            Some(true) => "ok".to_string(),
            Some(false) => {
                let err = v["error"].as_str().unwrap_or("");
                self.check(!err.trim().is_empty(), "an error reply without a message", &reply);
                self.check(!err.contains("panic"), "a command panicked (caught)", err);
                format!("error: {}", truncate(err, 160))
            }
            None => self.fail("a reply without `ok`", &reply),
        };
        self.report.push(format!(
            "{:>7.1}s  {:>6.0}ms  {} | {}",
            self.t0.elapsed().as_secs_f64(),
            ts.elapsed().as_secs_f64() * 1e3,
            truncate(&line, 200),
            outcome
        ));
        v
    }

    /// Run an engine or UI command; returns the reply.
    fn cmd(&mut self, id: &str, params: Value) -> Value {
        self.request("engine.execute", json!({"command": id, "params": params}))
    }

    /// Run a command that must succeed; returns its result.
    fn ok(&mut self, id: &str, params: Value) -> Value {
        let r = self.cmd(id, params.clone());
        if r["ok"] != true {
            self.fail(&format!("`{id}` {params} failed"), &r.to_string());
        }
        r["result"].clone()
    }

    fn check(&mut self, cond: bool, what: &str, detail: &str) {
        if !cond {
            self.fail(what, detail);
        }
    }

    /// Write the report and replay script, then fail the test.
    fn fail(&mut self, what: &str, detail: &str) -> ! {
        if let Some(app) = self.app.as_mut() {
            let _ = app.child.kill();
        }
        self.app = None;
        let report = self.root.join("report.txt");
        let replay = self.root.join("replay.jsonl");
        let _ = std::fs::write(&report, self.report.join("\n"));
        let _ = std::fs::write(&replay, self.replay.join("\n") + "\n");
        let tail: Vec<&String> = self.report.iter().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
        let seed = self.seed.map(|s| format!("\nreproduce: SIL_SEED={s} cargo xtask sil {}", self.name)).unwrap_or_default();
        panic!(
            "SIL `{}` invariant broken: {what}\n{detail}\n--- last steps ---\n{}\n--- full report: {}\n--- replay: {} (library {}){seed}",
            self.name,
            tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"),
            report.display(),
            replay.display(),
            self.lib.display(),
        );
    }

    fn finish(mut self) {
        self.quit();
        let _ = std::fs::write(self.root.join("report.txt"), self.report.join("\n"));
        eprintln!("SIL `{}`: {} steps in {:.1} s — passed", self.name, self.replay.len(), self.t0.elapsed().as_secs_f64());
        if std::env::var_os("SIL_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    // ------------------------------------------------------------ state queries and invariants

    /// Every photo of the library (not in Recently Deleted), sorted by id.
    fn photos(&mut self) -> Vec<Value> {
        let r = self.ok("catalog.query", json!({"filter": {}, "limit": 100_000}));
        let mut v = r["photos"].as_array().cloned().unwrap_or_default();
        v.sort_by_key(|p| p["id"].as_u64());
        v
    }

    fn ids(&mut self) -> Vec<u64> {
        self.photos().iter().filter_map(|p| p["id"].as_u64()).collect()
    }

    /// What the user would notice changing: each photo's catalog fields and develop settings, and
    /// the albums.
    fn fingerprint(&mut self) -> Value {
        let photos = self.photos();
        let mut out = vec![];
        for p in photos {
            let id = p["id"].clone();
            let dev = self.ok("develop.get", json!({"id": id}));
            out.push(json!({"photo": p, "develop": strip_volatile(dev)}));
        }
        let albums = self.ok("albums.list", json!({}));
        json!({"photos": out, "albums": albums})
    }

    /// Cheap consistency checks, run after every step.
    fn consistent(&mut self) {
        let state = self.ok("library.state", json!({}));
        let info = self.ok("library.info", json!({}));
        let all = self.ids();
        let n = info["photos"].as_u64().unwrap_or(0) as usize;
        self.check(n >= all.len(), "library.info counts fewer photos than the catalog lists", &format!("info {n}, listed {}", all.len()));
        let sel: Vec<u64> = state["selection"]["ids"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
        let stray: Vec<u64> = sel.iter().copied().filter(|i| !all.contains(i) && !self.deleted_ids().contains(i)).collect();
        self.check(stray.is_empty(), "the selection holds photos that don't exist", &format!("{stray:?} of {sel:?}"));
        if let Some(active) = state["selection"]["active"].as_u64() {
            self.check(sel.contains(&active), "the active photo is not selected", &state["selection"].to_string());
        }
        let view = self.ok("catalog.query", json!({"limit": 100_000}));
        let listed = view["photos"].as_array().map_or(0, Vec::len) as u64;
        self.check(
            state["visibleCount"].as_u64() == Some(listed) && view["total"].as_u64() == Some(listed),
            "the visible count disagrees with the photos the view lists",
            &format!("visibleCount {} listed {listed} total {}", state["visibleCount"], view["total"]),
        );
        let mut counts = vec![];
        album_counts(&self.ok("albums.list", json!({})), &mut counts);
        let total = info["photos"].as_u64().unwrap_or(0);
        let over: Vec<_> = counts.iter().filter(|(_, c)| *c > total).collect();
        self.check(over.is_empty(), "an album holds more photos than the library", &format!("{over:?} > {total}"));
    }

    fn deleted_ids(&mut self) -> Vec<u64> {
        let r = self.ok("catalog.query", json!({"filter": {"deleted": true}, "limit": 100_000}));
        r["photos"].as_array().into_iter().flatten().filter_map(|p| p["id"].as_u64()).collect()
    }

    /// Undo then redo returns to the same state (when there is something to undo).
    fn undo_redo_roundtrip(&mut self) {
        let before = self.fingerprint();
        let u = self.cmd("edit.undo", json!({}));
        if u["ok"] != true {
            return; // nothing to undo is a clean error
        }
        let r = self.cmd("edit.redo", json!({}));
        self.check(r["ok"] == true, "redo right after undo failed", &r.to_string());
        let after = self.fingerprint();
        if let Some(diff) = first_diff(&before, &after, "") {
            self.fail("undo then redo did not return to the same state", &diff);
        }
    }

    /// No long-running task is left behind: the activity stack empties.
    fn activity_drains(&mut self) {
        let t = Instant::now();
        loop {
            let r = self.ok("activity.list", json!({}));
            let tasks = r["tasks"].as_array().cloned().unwrap_or_default();
            if tasks.is_empty() {
                return;
            }
            if t.elapsed() > STEP_TIMEOUT {
                self.fail("the activity stack never emptied (stuck job)", &Value::from(tasks).to_string());
            }
            self.request("ui.settle", json!({"timeoutMs": 500}));
        }
    }

    /// Quit and reopen the library: everything the user did is still there.
    fn reopen_keeps_everything(&mut self) {
        self.activity_drains();
        let before = self.fingerprint();
        self.reopen();
        let after = self.fingerprint();
        if let Some(diff) = first_diff(&before, &after, "") {
            self.fail("reopening the library lost or changed something", &diff);
        }
    }

    /// Export `ids` and check that every file exists, is non-empty and decodes.
    fn export_decodes(&mut self, ids: &[u64], params: Value, decode: bool) -> Vec<PathBuf> {
        let dir = self.root.join(format!("export-{}", self.replay.len()));
        let mut p = params;
        p["dir"] = json!(dir.to_string_lossy());
        p["ids"] = json!(ids);
        let r = self.ok("app.export", p.clone());
        let files: Vec<PathBuf> = r["files"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str()).map(PathBuf::from).collect();
        self.check(files.len() == ids.len(), "export wrote a different number of files than photos", &r.to_string());
        for f in &files {
            let bytes = std::fs::read(f).unwrap_or_default();
            self.check(!bytes.is_empty(), "an exported file is missing or empty", &f.display().to_string());
            if decode && let Err(e) = lcc_codecs::decode(&bytes, Default::default()) {
                self.fail("an exported file does not decode", &format!("{}: {e}", f.display()));
            }
        }
        files
    }
}

fn album_counts(tree: &Value, out: &mut Vec<(String, u64)>) {
    for a in tree.as_array().into_iter().flatten() {
        out.push((a["name"].as_str().unwrap_or("").into(), a["count"].as_u64().unwrap_or(0)));
        album_counts(&a["children"], out);
    }
}

/// Fields that may legitimately change without the user doing anything (none known yet; kept as
/// the one place to list them).
fn strip_volatile(v: Value) -> Value {
    v
}

/// The first difference between two JSON values (floats compared to 1e-4), as `path: a != b`.
fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let (va, vb) = (x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
                if let Some(d) = first_diff(va, vb, &format!("{path}.{k}")) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: {} items != {} items", x.len(), y.len()));
            }
            x.iter().zip(y).enumerate().find_map(|(i, (va, vb))| first_diff(va, vb, &format!("{path}[{i}]")))
        }
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            ((x - y).abs() > 1e-4 * x.abs().max(1.0)).then(|| format!("{path}: {x} != {y}"))
        }
        _ => (a != b).then(|| format!("{path}: {} != {}", truncate(&a.to_string(), 300), truncate(&b.to_string(), 300))),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect::<String>() + "…" }
}

fn log_tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

// ---------------------------------------------------------------- test photos

/// A shoot folder of procedural JPEGs (small, so a run stays fast), plus up to `raws` raws from
/// the corpus when it is present.
fn shoot(root: &Path, jpegs: usize, raws: usize) -> (PathBuf, usize) {
    let dir = root.join("shoot");
    std::fs::create_dir_all(&dir).unwrap();
    for (i, scene) in lcc_scenes::demo_library().iter().cycle().take(jpegs).enumerate() {
        let img = scene.render_fit(480).to_srgb8();
        let jpg = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &Default::default()).unwrap();
        std::fs::write(dir.join(format!("IMG_{:04}.jpg", i + 1)), jpg).unwrap();
    }
    let mut n = jpegs;
    for raw in corpus_raws(raws) {
        let to = dir.join(raw.file_name().unwrap());
        if std::fs::copy(&raw, &to).is_ok() {
            n += 1;
        }
    }
    (dir, n)
}

/// The smallest raw of each format in the corpus (different decoders), at most `n`.
fn corpus_raws(n: usize) -> Vec<PathBuf> {
    if n == 0 {
        return vec![];
    }
    let dir = std::env::var_os("SIL_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw"));
    let mut files = vec![];
    collect_files(&dir, &mut files, 0);
    let mut by_ext: std::collections::BTreeMap<String, (u64, PathBuf)> = Default::default();
    for f in files {
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !["nef", "raf", "arw", "cr2", "dng", "orf", "pef", "rw2", "cr3"].contains(&ext.as_str()) {
            continue;
        }
        let len = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(u64::MAX);
        let e = by_ext.entry(ext).or_insert((len, f.clone()));
        if len < e.0 {
            *e = (len, f);
        }
    }
    let mut v: Vec<(u64, PathBuf)> = by_ext.into_values().collect();
    v.sort();
    v.into_iter().take(n).map(|(_, p)| p).collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 4 {
            collect_files(&p, out, depth + 1);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// Import the shoot (add mode) and wait until every file is in the library.
fn import(s: &mut Sil, dir: &Path, expect: usize) -> Vec<u64> {
    let r = s.ok("library.import", json!({"paths": [dir.to_string_lossy()], "mode": "add"}));
    s.activity_drains();
    let ids = s.ids();
    s.check(ids.len() == expect, "import did not add every file", &format!("{} of {expect}: {r}", ids.len()));
    ids
}

// ---------------------------------------------------------------- scenarios

/// A day in the life: import, cull, develop, sync, versions, collections, metadata, export,
/// XMP round trip, reopen.
#[test]
fn sil_workflow() {
    let mut s = Sil::new("workflow");
    let (dir, n) = shoot(&s.root, 8, 0);
    s.start();
    let ids = import(&mut s, &dir, n);
    s.consistent();

    // --- cull: rate, flag, label, reject, then filter on it
    s.ok("library.select", json!({"ids": [ids[0]]}));
    s.ok("photo.rate", json!({"rating": 5, "ids": [ids[0], ids[1]]}));
    s.ok("photo.rate", json!({"rating": 3, "ids": [ids[2]]}));
    s.ok("photo.flag", json!({"flag": "pick", "ids": [ids[0], ids[2]]}));
    s.ok("photo.reject", json!({"ids": [ids[7]]}));
    s.ok("photo.label", json!({"label": "red", "ids": [ids[1]]}));
    s.undo_redo_roundtrip();
    s.ok("library.filter", json!({"rating": 3, "ratingOp": "atLeast"}));
    let st = s.ok("library.state", json!({}));
    s.check(st["visibleCount"] == 3, "the rating filter shows the wrong photos", &st["visibleCount"].to_string());
    s.ok("library.filter", json!({"rating": 0, "flag": "pick"}));
    let st = s.ok("library.state", json!({}));
    s.check(st["visibleCount"] == 2, "the pick filter shows the wrong photos", &st["visibleCount"].to_string());
    s.consistent();
    s.ok("library.clearFilter", json!({}));
    s.consistent();

    // --- develop: auto, sliders, white-balance picker, crop, a mask, a preset
    s.ok("library.select", json!({"ids": [ids[0]], "active": ids[0]}));
    let r = s.request("ui.set", json!({"view": "detail"}));
    s.check(r["ok"] == true, "switching to Detail failed", &r.to_string());
    s.ok("develop.auto", json!({}));
    s.ok("develop.set", json!({"values": {"light.exposure": 0.7, "light.contrast": 20, "color.vibrance": 15}}));
    s.ok("develop.wbPick", json!({"x": 0.5, "y": 0.5}));
    s.ok("crop.set", json!({"rect": [0.1, 0.1, 0.9, 0.85], "angle": 2.0}));
    s.ok("mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.3, "ry": 0.2}));
    s.undo_redo_roundtrip();
    s.request("ui.settle", json!({"timeoutMs": 20000}));
    s.consistent();
    let a = s.ok("develop.get", json!({"id": ids[0]}));
    let preset = s.ok("preset.create", json!({"name": "SIL look"}));
    let pid = preset["id"].clone();
    s.check(!pid.is_null(), "preset.create returned no id", &preset.to_string());
    s.ok("preset.apply", json!({"id": pid, "ids": [ids[1]]}));
    let b = s.ok("develop.get", json!({"id": ids[1]}));
    s.check(b["light"]["exposure"] == a["light"]["exposure"], "a preset did not carry the exposure", &format!("{} vs {}", a["light"], b["light"]));

    // --- copy / paste, then Synchronize to a selection
    s.ok("develop.copy", json!({}));
    s.ok("develop.paste", json!({"ids": [ids[2]]}));
    let c = s.ok("develop.get", json!({"id": ids[2]}));
    s.check(c["light"] == a["light"], "paste did not carry the Light settings", &format!("{} vs {}", a["light"], c["light"]));
    s.ok("library.select", json!({"ids": [ids[0], ids[3], ids[4]], "active": ids[0]}));
    s.ok("develop.sync", json!({}));
    for &i in &ids[3..5] {
        let d = s.ok("develop.get", json!({"id": i}));
        s.check(d["light"] == a["light"] && d["color"] == a["color"], "Synchronize did not reach every selected photo", &i.to_string());
    }
    s.undo_redo_roundtrip();
    s.consistent();

    // --- versions: virtual copy with its own edits, history back and forth
    s.ok("library.select", json!({"ids": [ids[0]], "active": ids[0]}));
    let vc = s.ok("photo.virtualCopy", json!({}));
    let copy = vc["ids"][0].as_u64().unwrap_or(0);
    let has = s.ids().contains(&copy);
    s.check(has, "the virtual copy is not in the library", &vc.to_string());
    s.ok("develop.set", json!({"control": "light.exposure", "value": -1.5, "ids": [copy]}));
    let orig = s.ok("develop.get", json!({"id": ids[0]}));
    s.check(orig["light"] == a["light"], "editing a virtual copy changed its original", &orig["light"].to_string());
    s.ok("library.select", json!({"ids": [ids[0]], "active": ids[0]}));
    let h = s.ok("history.list", json!({}));
    let steps = h["history"].as_array().map_or(0, Vec::len);
    s.check(steps >= 3, "the history lost steps", &h.to_string());
    let now = s.ok("develop.get", json!({}));
    s.ok("history.previous", json!({}));
    s.ok("history.previous", json!({}));
    s.ok("history.next", json!({}));
    s.ok("history.next", json!({}));
    let back = s.ok("develop.get", json!({}));
    if let Some(d) = first_diff(&now, &back, "") {
        s.fail("history back two steps and forward two did not return to the same settings", &d);
    }
    s.consistent();

    // --- collections: album, add/remove, smart collection
    let album = s.ok("album.create", json!({"name": "SIL picks"}));
    let aid = album["id"].clone();
    s.ok("album.addPhotos", json!({"id": aid, "ids": [ids[0], ids[1], ids[2]]}));
    s.ok("album.removePhotos", json!({"id": aid, "ids": [ids[1]]}));
    s.ok("library.source", json!({"kind": "album", "id": aid}));
    let st = s.ok("library.state", json!({}));
    s.check(st["visibleCount"] == 2, "the album shows the wrong photos", &st.to_string());
    s.ok("album.createSmart", json!({"name": "SIL rated", "rules": {"rating": 3, "ratingOp": "atLeast"}}));
    s.consistent();
    s.ok("library.source", json!({"kind": "all"}));
    s.undo_redo_roundtrip();

    // --- keywords and metadata, synced to a selection
    s.ok("keyword.create", json!({"name": "silkeyword"}));
    s.ok("photo.setMeta", json!({"ids": [ids[0], ids[5]], "title": "SIL title", "addKeywords": ["silkeyword", "harbour"], "creator": "Tester"}));
    s.ok("library.filter", json!({"keyword": "silkeyword"}));
    let st = s.ok("library.state", json!({}));
    s.check(st["visibleCount"] == 2, "the keyword filter shows the wrong photos", &st["visibleCount"].to_string());
    s.ok("library.clearFilter", json!({}));
    s.undo_redo_roundtrip();
    s.consistent();

    // --- export: JPEG and TIFF decode; Original + Settings writes the file and its XMP
    s.export_decodes(&[ids[0], copy], json!({"format": "jpeg", "longEdge": 400, "quality": 80}), true);
    s.export_decodes(&[ids[1]], json!({"format": "tiff", "longEdge": 300}), true);
    let orig_files = s.export_decodes(&[ids[2]], json!({"preset": "Original + Settings"}), true);
    for f in orig_files {
        s.check(f.with_extension("xmp").is_file(), "Original + Settings wrote no XMP sidecar", &f.display().to_string());
    }
    s.activity_drains();

    // --- XMP: write, change in the catalog, read back
    let saved = s.ok("develop.get", json!({"id": ids[0]}));
    s.ok("photo.saveMetadataToFile", json!({"ids": [ids[0]]}));
    s.check(dir.join("IMG_0001.xmp").is_file(), "no XMP sidecar next to the original", "");
    s.ok("develop.set", json!({"control": "light.exposure", "value": -3.0, "ids": [ids[0]]}));
    let st = s.ok("photo.xmpStatus", json!({"ids": [ids[0]]}));
    s.check(st["photos"][0]["status"] == "changedInCatalog", "XMP status missed a catalog change", &st.to_string());
    // the app asks first, as Lightroom does: the user confirms the dialog
    let r = s.ok("photo.readMetadataFromFile", json!({"ids": [ids[0]]}));
    if r["confirm"] == "dialog" {
        let c = s.request("ui.dialog.confirm", json!({}));
        s.check(c["ok"] == true, "confirming Read Metadata from File failed", &c.to_string());
    }
    let back = s.ok("develop.get", json!({"id": ids[0]}));
    for group in ["light", "color", "crop", "wb", "effects"] {
        if let Some(d) = first_diff(&saved[group], &back[group], group) {
            s.fail("settings read back from XMP differ from those written", &d);
        }
    }

    // --- print / book / slideshow output
    let print = s.root.join("print.jpg");
    let r = s.cmd("print.render", json!({"path": print.to_string_lossy(), "ids": [ids[0], ids[1]]}));
    if r["ok"] == true {
        for p in r["result"]["pages"].as_array().into_iter().flatten() {
            let f = p["path"].as_str().unwrap_or("");
            s.check(std::fs::metadata(f).map(|m| m.len() > 0).unwrap_or(false), "a print page is missing or empty", f);
        }
    }

    // --- Recently Deleted: a re-import restores a deleted photo; emptying removes only the trash
    let live = |s: &mut Sil| {
        let trashed = s.deleted_ids();
        s.ids().into_iter().filter(|i| !trashed.contains(i)).collect::<Vec<u64>>()
    };
    let before = live(&mut s);
    // two photos of the shoot itself (not copies or versions made above)
    let picked: Vec<u64> = ids[3..].iter().copied().filter(|i| before.contains(i)).take(2).collect();
    if let [a, b] = picked[..] {
        s.ok("photo.delete", json!({"ids": [a, b]}));
        let r = s.ok("library.import", json!({"paths": [dir.to_string_lossy()], "mode": "add"}));
        s.activity_drains();
        let restored = r["restored"].as_array().map(|v| v.iter().filter_map(Value::as_u64).collect::<Vec<_>>()).unwrap_or_default();
        s.check(restored.contains(&a) && restored.contains(&b), "a re-import did not restore the deleted photos", &r.to_string());
        let now = live(&mut s);
        s.check(now == before, "a re-import of deleted photos added or lost photos", &format!("{now:?} vs {before:?}"));
        s.ok("photo.delete", json!({"ids": [a]}));
        let gone = s.deleted_ids();
        let r = s.ok("library.emptyRecentlyDeleted", json!({}));
        s.check(r["deleted"].as_u64() == Some(gone.len() as u64), "Empty Recently Deleted removed the wrong number", &r.to_string());
        let left = s.deleted_ids();
        s.check(left.is_empty(), "photos left in Recently Deleted", &format!("{left:?}"));
        let now = live(&mut s);
        let expect: Vec<u64> = before.iter().copied().filter(|i| *i != a).collect();
        s.check(now == expect, "emptying removed live photos", &format!("{now:?} vs {expect:?}"));
        let files = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        s.check(files >= n, "emptying deleted files on disk", &format!("{files} of {n}"));
        s.consistent();
    }

    // --- quit and reopen: everything is still there
    s.consistent();
    s.reopen_keeps_everything();
    s.consistent();
    s.activity_drains();
    s.finish();
}

/// Raws from the corpus (skipped without one): import, develop, export JPEG and DNG.
#[test]
fn sil_raw_workflow() {
    if corpus_raws(1).is_empty() {
        eprintln!("SIL raw: no corpus raws (cargo xtask corpus --download, or SIL_CORPUS) — skipped");
        return;
    }
    let mut s = Sil::new("raw");
    let (dir, n) = shoot(&s.root, 0, 3);
    s.start();
    let ids = import(&mut s, &dir, n);
    for &id in &ids {
        s.ok("library.select", json!({"ids": [id], "active": id}));
        s.ok("develop.auto", json!({}));
        s.ok("develop.set", json!({"values": {"light.shadows": 30, "detail.nrLuminance": 20}}));
        s.ok("develop.wb", json!({"mode": "custom", "temp": 5200, "tint": 5}));
        s.undo_redo_roundtrip();
    }
    s.export_decodes(&ids, json!({"format": "jpeg", "longEdge": 800}), true);
    s.export_decodes(&ids, json!({"format": "dng"}), false);
    s.ok("photo.saveMetadataToFile", json!({"ids": ids}));
    s.consistent();
    s.reopen_keeps_everything();
    s.finish();
}

// ---------------------------------------------------------------- monkey

/// xorshift64*: a tiny seeded generator, so a seed replays the same run everywhere.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

/// The monkey stops adding photos (virtual copies) once the library holds this many, so that
/// long runs keep each step, and the checks after it, inside the per-step timeout.
const MONKEY_MAX_PHOTOS: usize = 64;

/// One random user action (sometimes with hostile arguments).
fn random_action(r: &mut Rng, ids: &[u64], albums: &[u64], root: &Path, step: usize) -> (&'static str, Value) {
    let id = if ids.is_empty() || r.chance(5) { json!(r.pick(&[0u64, 999_999, u64::MAX])) } else { json!(r.pick(ids)) };
    let some: Vec<u64> = ids.iter().copied().filter(|_| r.chance(40)).collect();
    let hostile = r.chance(8);
    let num = |r: &mut Rng, lo: f64, hi: f64| if hostile { json!(r.pick(&[-1e30, 1e30, 0.0, -0.0, 1e-300])) } else { json!(r.f(lo, hi)) };
    let album = albums.first().map_or(json!(1), |a| json!(r.pick(&[*a, albums[albums.len() - 1], 424_242])));
    match r.below(33) {
        0 => ("library.select", json!({"ids": [id], "active": id})),
        1 => ("library.select", json!({"ids": some, "mode": r.pick(&["replace", "add", "toggle", "range"])})),
        2 => ("library.selectAll", json!({})),
        3 => ("photo.rate", json!({"rating": if hostile { json!(9) } else { json!(r.below(6)) }})),
        4 => ("photo.flag", json!({"flag": r.pick(&["pick", "reject", "none", "bogus"])})),
        5 => ("photo.label", json!({"label": r.pick(&["red", "yellow", "green", "blue", "purple", "none"])})),
        6 => ("library.filter", json!({"rating": r.below(6), "flag": r.pick(&[json!("pick"), json!(null), json!("reject")])})),
        7 => ("library.clearFilter", json!({})),
        8 => ("develop.auto", json!({})),
        9 => {
            let c = *r.pick(&[
                "light.exposure",
                "light.contrast",
                "light.highlights",
                "light.shadows",
                "color.vibrance",
                "effects.clarity",
                "detail.sharpenAmount",
                "nope.control",
            ]);
            ("develop.set", json!({"control": c, "value": num(r, -2.0, 2.0)}))
        }
        10 => ("develop.wbPick", json!({"x": num(r, 0.0, 1.0), "y": num(r, 0.0, 1.0)})),
        11 => ("crop.set", json!({"rect": [r.f(0.0, 0.4), r.f(0.0, 0.4), r.f(0.6, 1.0), r.f(0.6, 1.0)], "angle": num(r, -10.0, 10.0)})),
        12 => (
            "mask.add",
            json!({"kind": r.pick(&["radial", "linear", "brush", "luminanceRange"]), "center": [r.f(0.0, 1.0), r.f(0.0, 1.0)], "rx": 0.2, "ry": 0.2}),
        ),
        13 => ("develop.copy", json!({})),
        14 => ("develop.paste", json!({"ids": some})),
        15 => ("develop.sync", json!({})),
        16 => ("develop.reset", json!({})),
        17 if ids.len() < MONKEY_MAX_PHOTOS => ("photo.virtualCopy", json!({})),
        17 => ("library.selectAll", json!({})),
        18 => ("history.previous", json!({})),
        19 => ("history.next", json!({})),
        20 => ("edit.undo", json!({})),
        21 => ("edit.redo", json!({})),
        22 => ("album.create", json!({"name": format!("Monkey {step}")})),
        23 => ("album.addPhotos", json!({"id": album, "ids": some})),
        24 => ("album.removePhotos", json!({"id": album, "ids": some})),
        25 => (
            "photo.setMeta",
            json!({"title": if hostile { "\u{0}\u{202e}x".repeat(100) } else { format!("t{step}") }, "addKeywords": [format!("k{}", r.below(5))]}),
        ),
        26 => ("photo.rotateRight", json!({})),
        27 => ("photo.saveMetadataToFile", json!({})),
        28 => ("photo.delete", json!({"ids": [id]})),
        29 => ("photo.readMetadataFromFile", json!({})),
        30 => ("ui.dialog.confirm", json!({})),
        31 => ("ui.dialog.cancel", json!({})),
        _ => ("app.export", json!({"format": "jpeg", "longEdge": 200, "ids": [id], "dir": root.join("monkey-export").to_string_lossy()})),
    }
}

/// The monkey never asks for a photo-adding action once the library is at its size cap.
#[test]
fn sil_monkey_stops_adding_photos_past_the_cap() {
    let root = std::env::temp_dir();
    for n in [MONKEY_MAX_PHOTOS - 1, MONKEY_MAX_PHOTOS, MONKEY_MAX_PHOTOS * 4] {
        let ids: Vec<u64> = (1..=n as u64).collect();
        let mut r = Rng(0x5eed);
        let added = (0..5000).filter(|step| random_action(&mut r, &ids, &[], &root, *step).0 == "photo.virtualCopy").count();
        if n < MONKEY_MAX_PHOTOS {
            assert!(added > 0, "below the cap the monkey still makes virtual copies");
        } else {
            assert_eq!(added, 0, "{n} photos: the monkey added photos past its cap of {MONKEY_MAX_PHOTOS}");
        }
    }
}

/// Seeded random user actions; invariants after every step. `SIL_SEED` replays a run,
/// `SIL_STEPS` sets the budget.
#[test]
fn sil_monkey() {
    let seed = std::env::var("SIL_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1);
    let steps: usize = std::env::var("SIL_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(400);
    eprintln!("SIL monkey: SIL_SEED={seed} SIL_STEPS={steps}");
    let mut s = Sil::new("monkey");
    s.seed = Some(seed);
    let mut r = Rng(seed);
    let (dir, n) = shoot(&s.root, 6, 0);
    s.start();
    import(&mut s, &dir, n);
    let mut albums: Vec<u64> = vec![];
    for step in 0..steps {
        let ids = s.ids();
        let (id, params) = random_action(&mut r, &ids, &albums, &s.root, step);
        let reply = if id.starts_with("ui.") { s.request(id, params) } else { s.cmd(id, params) };
        if id == "album.create"
            && let Some(a) = reply["result"]["id"].as_u64()
        {
            albums.push(a);
        }
        s.consistent();
        if step % 20 == 19 {
            s.undo_redo_roundtrip();
        }
        if step % 60 == 59 {
            s.reopen_keeps_everything();
        }
    }
    s.activity_drains();
    s.reopen_keeps_everything();
    s.finish();
}
