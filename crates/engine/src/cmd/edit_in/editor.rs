//! Talking to the external editor while a file is out for editing: launching it so that the
//! caller learns when editing is done (`--wait`), reusing one window (`--single-instance`), and
//! PhotoCraft's control-channel events (`events.subscribe` → `document.saved` /
//! `document.closed`).
//!
//! Everything is feature-detected, so an older PhotoCraft or any other editor still works: the
//! caller falls back to reloading when the app regains focus and to a cheap modification-time
//! poll of the files it handed over ([`Watched`]).
//!
//! - **Capabilities** ([`capabilities`]): a preset whose arguments already hold `--wait` waits; a
//!   PhotoCraft binary is asked once (`--version --json`, 5 s at most, cached) whether it takes
//!   `--wait` and `--single-instance` (`features`: `wait`, `singleInstance`). Builds without it
//!   stop at `--version` and print a plain line, so the probe never opens a window; that answer
//!   means neither. Other applications are never probed (an unknown GUI program might open one).
//! - **Events** ([`subscribe`]): one JSON-lines connection, `auth`, then `events.subscribe`; each
//!   pushed line `{"event": "document.saved"|"document.closed", "params": {path, format, docId}}`
//!   is handed to the caller. A channel without the method answers with an error: the caller
//!   learns that events are unsupported and keeps polling.
//!
//! Sources: own design; the event shape is the one PhotoCraft's control protocol documents for
//! external editor integration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::{Value, json};

use super::Control;

/// What an editor binary supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// `--wait`: the process exits when the files it opened are closed (0 = saved).
    pub wait: bool,
    /// `--single-instance`: a second launch hands its files to the running window.
    pub single_instance: bool,
}

static CAPS: Mutex<Option<HashMap<String, Caps>>> = Mutex::new(None);

/// Whether `app` (a preset's application) is a PhotoCraft binary.
pub fn is_photocraft(app: &str) -> bool {
    Path::new(app).file_stem().is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().contains("photocraft"))
}

/// What `app` with `args` supports (see the module docs).
pub fn capabilities(app: &str, args: &[String]) -> Caps {
    let given = Caps { wait: args.iter().any(|a| a == "--wait"), single_instance: args.iter().any(|a| a == "--single-instance") };
    if !is_photocraft(app) || cfg!(target_arch = "wasm32") {
        return given;
    }
    let mut cache = CAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let probed = *cache.get_or_insert_with(HashMap::new).entry(app.to_string()).or_insert_with(|| probe(app));
    Caps { wait: given.wait || probed.wait, single_instance: given.single_instance || probed.single_instance }
}

/// Parse PhotoCraft's `--version --json` answer (`app.capabilities`); anything else supports
/// neither.
pub fn caps_from_version(text: &str) -> Caps {
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
        return Caps::default();
    };
    let has = |f: &str| v.get("features").and_then(Value::as_array).is_some_and(|a| a.iter().any(|x| x.as_str() == Some(f)));
    Caps { wait: has("wait"), single_instance: has("singleInstance") }
}

#[cfg(not(target_arch = "wasm32"))]
fn probe(app: &str) -> Caps {
    use std::io::Read;
    let Ok(mut child) = std::process::Command::new(app)
        .args(["--version", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return Caps::default();
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(5) => std::thread::sleep(std::time::Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Caps::default();
            }
        }
    }
    let mut text = String::new();
    if let Some(o) = child.stdout.as_mut() {
        let _ = o.take(1 << 20).read_to_string(&mut text);
    }
    caps_from_version(&text)
}

#[cfg(target_arch = "wasm32")]
fn probe(_: &str) -> Caps {
    Caps::default()
}

/// The command line that opens `path` and waits for the editor to be done with it: the preset's
/// line with `--wait` (and `--single-instance` when supported) before the file. On macOS an app
/// name goes through `open -W`.
pub fn wait_command_line(app: &str, args: &[String], path: &Path, caps: Caps) -> Vec<String> {
    let mut extra: Vec<String> = Vec::new();
    if !args.iter().any(|a| a == "--wait") {
        extra.push("--wait".into());
    }
    if caps.single_instance && !args.iter().any(|a| a == "--single-instance" || a == "--new-instance") {
        extra.push("--single-instance".into());
    }
    extra.extend(args.iter().cloned());
    let mut line = super::command_line(app, &extra, path);
    if line.first().is_some_and(|p| p == "open") {
        line.insert(1, "-W".into());
    }
    line
}

/// One event from the editor.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorEvent {
    Saved(PathBuf),
    Closed(PathBuf),
}

/// An event line from the editor's control channel (`None`: a reply, or an event we don't use).
/// A relative path is taken as relative to `root` (the channel's automation read root).
pub fn parse_event(line: &str, root: &Path) -> Option<EditorEvent> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let kind = v.get("event").or_else(|| v.get("method")).and_then(Value::as_str)?;
    let params = v.get("params").or_else(|| v.get("data")).unwrap_or(&Value::Null);
    let p = PathBuf::from(params.get("path").and_then(Value::as_str).filter(|p| !p.is_empty())?);
    let p = if p.is_relative() { root.join(p) } else { p };
    match kind {
        "document.saved" => Some(EditorEvent::Saved(p)),
        "document.closed" => Some(EditorEvent::Closed(p)),
        _ => None,
    }
}

/// Subscribe to a PhotoCraft's control-channel events and hand each one to `on` until it returns
/// `false`, the connection ends or `stop` is set. `Err` when the channel can't be reached or
/// doesn't offer `events.subscribe` (an older PhotoCraft): the caller falls back to polling.
#[cfg(not(target_arch = "wasm32"))]
pub fn subscribe(c: &Control, stop: &std::sync::atomic::AtomicBool, mut on: impl FnMut(EditorEvent) -> bool) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::Ordering;
    let token = std::fs::read_to_string(&c.token_file).map_err(|e| format!("can't read the token file {}: {e}", c.token_file))?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], c.port));
    let stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5))
        .map_err(|e| format!("no PhotoCraft on port {}: {e}", c.port))?;
    // short reads, so `stop` is noticed
    stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).map_err(|e| e.to_string())?;
    let mut w = stream.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(stream);
    let root = PathBuf::from(&c.root);
    let mut line = String::new();
    // one line, up to 1 MiB, waiting at most `secs` (`None` = the stream ended)
    let mut read_line = |line: &mut String, secs: u64| -> Result<Option<()>, String> {
        line.clear();
        let start = std::time::Instant::now();
        loop {
            match r.read_line(line) {
                Ok(0) => return Ok(None),
                Ok(_) if line.ends_with('\n') => return Ok(Some(())),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.to_string()),
            }
            if line.len() > 1 << 20 {
                return Err("line too long".into());
            }
            if stop.load(Ordering::Relaxed) || (secs > 0 && start.elapsed().as_secs() >= secs) {
                return Err("stopped".into());
            }
        }
    };
    let mut call = |id: &str, method: &str, params: Value, line: &mut String| -> Result<Value, String> {
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        w.write_all(msg.as_bytes()).and_then(|_| w.write_all(b"\n")).map_err(|e| e.to_string())?;
        // events can arrive before the reply: skip lines that aren't it
        for _ in 0..64 {
            read_line(line, 30)?.ok_or("the connection closed")?;
            let v: Value = serde_json::from_str(line.trim()).map_err(|e| format!("bad reply ({e})"))?;
            if v.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }
            return if v["ok"].as_bool() == Some(true) { Ok(v["result"].clone()) } else { Err(v["error"].as_str().unwrap_or("refused").to_string()) };
        }
        Err("no reply".into())
    };
    call("auth", "auth", json!({"token": token.trim()}), &mut line)?;
    call("sub", "events.subscribe", json!({"events": ["document.saved", "document.closed"]}), &mut line)
        .map_err(|e| format!("events.subscribe: {e}"))?;
    loop {
        match read_line(&mut line, 0) {
            Ok(Some(())) => {
                if let Some(ev) = parse_event(&line, &root)
                    && !on(ev)
                {
                    return Ok(());
                }
            }
            Ok(None) => return Ok(()),
            Err(e) if e == "stopped" => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn subscribe(_: &Control, _: &std::sync::atomic::AtomicBool, _: impl FnMut(EditorEvent) -> bool) -> Result<(), String> {
    Err("not available in the browser".into())
}

/// Files out for editing, polled by modification time (the fallback when the editor can't tell
/// us it saved): `changed` reports a file once its time moved and then held still for one poll
/// (so a save still being written isn't read half-way).
#[derive(Debug, Default)]
pub struct Watched {
    files: HashMap<PathBuf, Entry>,
}

#[derive(Debug)]
struct Entry {
    ids: Vec<u64>,
    seen: Option<SystemTime>,
    pending: Option<SystemTime>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Most files watched at once.
const MAX_WATCHED: usize = 256;

impl Watched {
    /// Watch `path` for the photos `ids` (the time it has now is the baseline).
    pub fn add(&mut self, path: &Path, ids: &[u64]) {
        if self.files.len() >= MAX_WATCHED && !self.files.contains_key(path) {
            return;
        }
        let e = self.files.entry(path.to_path_buf()).or_insert_with(|| Entry { ids: Vec::new(), seen: mtime(path), pending: None });
        for id in ids {
            if !e.ids.contains(id) {
                e.ids.push(*id);
            }
        }
    }
    pub fn remove(&mut self, path: &Path) {
        self.files.remove(path);
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
    pub fn len(&self) -> usize {
        self.files.len()
    }
    /// The photos of `path` (an event names the file).
    pub fn ids(&self, path: &Path) -> Vec<u64> {
        self.files.get(path).map(|e| e.ids.clone()).unwrap_or_default()
    }
    /// Note that `path` was reloaded (e.g. on an event): the current time is the new baseline.
    pub fn mark_seen(&mut self, path: &Path) {
        if let Some(e) = self.files.get_mut(path) {
            e.seen = mtime(path);
            e.pending = None;
        }
    }
    /// One poll. → the photos whose files changed and settled since the last poll. A file that
    /// disappeared is forgotten.
    pub fn changed(&mut self) -> Vec<u64> {
        let mut out = Vec::new();
        self.files.retain(|p, e| {
            let Some(now) = mtime(p) else { return false };
            if Some(now) == e.seen {
                e.pending = None;
            } else if e.pending == Some(now) {
                e.seen = Some(now);
                e.pending = None;
                out.extend(e.ids.iter().copied());
            } else {
                e.pending = Some(now);
            }
            true
        });
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::edit_in::Control;

    #[test]
    fn version_probe_and_wait_line() {
        let c = caps_from_version(r#"{"app":"photocraft","features":["saveInPlace","events","wait","singleInstance"],"protocol":1}"#);
        assert_eq!(c, Caps { wait: true, single_instance: true });
        // a release without `--version --json` prints its plain version line
        assert_eq!(caps_from_version("photocraft 0.7.0 (abc123 2026-10-01)"), Caps::default());
        assert_eq!(caps_from_version(r#"{"features":"wait"}"#), Caps::default());
        assert_eq!(caps_from_version(r#"{"features":["wait"]}"#), Caps { wait: true, single_instance: false });
        let l = wait_command_line("/opt/photocraft", &[], Path::new("/a/b.psd"), c);
        if !cfg!(target_os = "macos") {
            assert_eq!(l, vec!["/opt/photocraft", "--wait", "--single-instance", "/a/b.psd"]);
        }
        let l = wait_command_line("/opt/photocraft", &["--new-instance".into(), "--wait".into()], Path::new("/a/b.psd"), c);
        assert_eq!(l.iter().filter(|a| *a == "--wait").count(), 1);
        assert!(!l.contains(&"--single-instance".to_string()));
        assert!(is_photocraft("/Applications/PhotoCraft.app/Contents/MacOS/photocraft") && is_photocraft("PhotoCraft"));
        assert!(!is_photocraft("gimp"));
        // non-PhotoCraft apps are never probed: only their own arguments count
        assert_eq!(capabilities("/usr/bin/gimp", &["--wait".into()]), Caps { wait: true, single_instance: false });
    }

    #[test]
    fn events_are_parsed_and_hostile_lines_ignored() {
        let root = Path::new("/root");
        assert_eq!(
            parse_event(r#"{"event":"document.saved","params":{"path":"/x/a.psd","format":"psd","docId":3}}"#, root),
            Some(EditorEvent::Saved("/x/a.psd".into()))
        );
        assert_eq!(parse_event(r#"{"method":"document.closed","params":{"path":"a.psd"}}"#, root), Some(EditorEvent::Closed("/root/a.psd".into())));
        for l in [
            "",
            "{",
            "null",
            r#"{"id":"sub","ok":true}"#,
            r#"{"event":"document.saved"}"#,
            r#"{"event":"document.saved","params":{"path":""}}"#,
            r#"{"event":"x","params":{"path":"/a"}}"#,
        ] {
            assert_eq!(parse_event(l, root), None, "{l}");
        }
    }

    #[test]
    fn watched_reports_a_settled_change_once() {
        let d = std::env::temp_dir().join(format!("edit-watch-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.tif");
        std::fs::write(&f, b"1").unwrap();
        let mut w = Watched::default();
        w.add(&f, &[7]);
        assert!(w.changed().is_empty());
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(later).unwrap();
        assert!(w.changed().is_empty(), "first sight: pending");
        assert_eq!(w.changed(), vec![7], "settled");
        assert!(w.changed().is_empty(), "once");
        std::fs::remove_file(&f).unwrap();
        w.changed();
        assert!(w.is_empty(), "a vanished file is forgotten");
    }

    /// A stand-in PhotoCraft control channel: answers `auth` and (if `events`) `events.subscribe`,
    /// then pushes `pushes`. → its port, and the requests it saw
    pub(crate) fn mock_editor(events: bool, pushes: Vec<String>) -> (u16, std::sync::mpsc::Receiver<Value>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in l.incoming().take(4) {
                let Ok(conn) = conn else { return };
                let mut w = conn.try_clone().unwrap();
                let (tx, pushes) = (tx.clone(), pushes.clone());
                std::thread::spawn(move || {
                    let mut r = BufReader::new(conn);
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 {
                        let v: Value = serde_json::from_str(line.trim()).unwrap();
                        line.clear();
                        let method = v["method"].as_str().unwrap_or_default().to_string();
                        let _ = tx.send(v.clone());
                        let ok = method != "events.subscribe" || events;
                        let reply = if ok {
                            json!({"id": v["id"], "ok": true, "result": {}})
                        } else {
                            json!({"id": v["id"], "ok": false, "error": "unknown method events.subscribe"})
                        };
                        // an event racing the reply is skipped by the client
                        if method == "events.subscribe" && ok {
                            writeln!(w, "{}", json!({"event": "document.opened", "params": {"path": "/x"}})).unwrap();
                        }
                        writeln!(w, "{reply}").unwrap();
                        if method == "events.subscribe" && ok {
                            for p in &pushes {
                                writeln!(w, "{p}").unwrap();
                            }
                            return;
                        }
                    }
                });
            }
        });
        (port, rx)
    }

    fn control(port: u16) -> Control {
        let dir = std::env::temp_dir().join(format!("edit-ev-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("token");
        std::fs::write(&token, "secret\n").unwrap();
        Control { port, token_file: token.to_string_lossy().to_string(), root: "/root".into() }
    }

    #[test]
    fn subscribe_delivers_saved_and_closed() {
        let saved = json!({"event": "document.saved", "params": {"path": "a/b.psd", "format": "psd", "docId": 1}}).to_string();
        let closed = json!({"event": "document.closed", "params": {"path": "/root/a/b.psd"}}).to_string();
        let (port, seen) = mock_editor(true, vec!["garbage".into(), saved, closed]);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut got = Vec::new();
        subscribe(&control(port), &stop, |e| {
            got.push(e);
            got.len() < 2
        })
        .unwrap();
        assert_eq!(got, vec![EditorEvent::Saved("/root/a/b.psd".into()), EditorEvent::Closed("/root/a/b.psd".into())]);
        assert_eq!(seen.recv().unwrap()["params"]["token"], "secret", "auth first, token trimmed");
        assert_eq!(seen.recv().unwrap()["method"], "events.subscribe");
    }

    #[test]
    fn an_older_editor_without_events_is_an_error() {
        let (port, _) = mock_editor(false, Vec::new());
        let stop = std::sync::atomic::AtomicBool::new(false);
        let e = subscribe(&control(port), &stop, |_| true).unwrap_err();
        assert!(e.contains("events.subscribe"), "{e}");
        // nothing listening
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(subscribe(&control(free), &stop, |_| true).is_err());
    }
}
