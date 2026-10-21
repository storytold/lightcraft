//! Talking to the external editor while a file is out for editing: launching it so that the
//! caller learns when editing is done (`--wait`) and reusing one window (`--single-instance`),
//! and a cheap modification-time poll of the files handed over ([`Watched`]) for every other
//! editor.
//!
//! - **Capabilities** ([`capabilities`]): a PhotoCraft binary is asked once (`--version --json`,
//!   5 s at most, cached) whether it takes `--wait` and `--single-instance` (`features`: `wait`,
//!   `singleInstance`). Builds without it stop at `--version` and print a plain line, so the
//!   probe never opens a window (`--help` would: PhotoCraft has none and opens it as a file);
//!   that answer means neither. Other applications are never probed (an unknown GUI program
//!   might open a window).
//! - **Waiting** ([`wait_command_line`], [`run_and_wait`]): the editor started with `--wait`
//!   exits when the file is closed, and the caller reloads the photo then.
//! - **Polling** ([`Watched`]): a file whose modification time moved and then held still is
//!   reported once, so saves come back while the app keeps the focus.
//!
//! Sources: own design; the flags are the ones PhotoCraft's command line documents for external
//! editor integration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

/// What an editor binary supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// `--wait`: the process exits when the files it opened are closed (0 = saved).
    pub wait: bool,
    /// `--single-instance`: a second launch hands its files to the running window.
    pub single_instance: bool,
}

static CAPS: Mutex<Option<HashMap<String, Caps>>> = Mutex::new(None);

/// Whether `app` (the external editor setting) is a PhotoCraft binary.
pub fn is_photocraft(app: &str) -> bool {
    Path::new(app).file_stem().is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().contains("photocraft"))
}

/// What `app` supports (see the module docs; anything but PhotoCraft: nothing).
pub fn capabilities(app: &str) -> Caps {
    if !is_photocraft(app) || cfg!(target_arch = "wasm32") {
        return Caps::default();
    }
    let mut cache = CAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache.get_or_insert_with(HashMap::new).entry(app.to_string()).or_insert_with(|| probe(app))
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

/// The command line that opens `path` in `app` and waits for the editor to be done with it:
/// `--wait` (and `--single-instance` when supported) before the file. On macOS an application
/// name (no `/`) goes through `open -W -a <app> --args`.
pub fn wait_command_line(app: &str, path: &Path, caps: Caps) -> Vec<String> {
    let mut line: Vec<String> = if cfg!(target_os = "macos") && !app.contains('/') {
        vec!["open".into(), "-W".into(), "-a".into(), app.into(), "--args".into()]
    } else {
        vec![app.into()]
    };
    line.push("--wait".into());
    if caps.single_instance {
        line.push("--single-instance".into());
    }
    line.push(path.to_string_lossy().to_string());
    line
}

/// Start the editor and block until it exits. → whether it reported a save (exit code 0)
#[cfg(not(target_arch = "wasm32"))]
pub fn run_and_wait(line: &[String]) -> Result<bool, String> {
    let (prog, rest) = line.split_first().ok_or("no program")?;
    let status = std::process::Command::new(prog).args(rest).stdin(std::process::Stdio::null()).status().map_err(|e| format!("{prog}: {e}"))?;
    Ok(status.success())
}

#[cfg(target_arch = "wasm32")]
pub fn run_and_wait(_: &[String]) -> Result<bool, String> {
    Err("not available in the browser".into())
}

/// Files out for editing, polled by modification time (the fallback when the editor can't tell
/// us it saved): `changed` reports a file once its time moved and then held still for one poll
/// (so a save still being written isn't read half-way).
#[derive(Debug, Default)]
pub struct Watched {
    files: HashMap<PathBuf, Entry>,
    next_order: u64,
}

#[derive(Debug)]
struct Entry {
    ids: Vec<u64>,
    /// When it was added (the oldest goes first when the table is full).
    order: u64,
    /// The editor that had it exited: nothing more to wait for (it goes before any other).
    retired: bool,
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
            // a long session keeps receiving saves: make room by dropping a file whose editor has
            // exited, else the one that has been out the longest
            let victim = self.files.iter().min_by_key(|(_, e)| (!e.retired, e.order)).map(|(p, _)| p.clone());
            if let Some(v) = victim {
                self.files.remove(&v);
            }
        }
        let order = self.next_order;
        self.next_order = self.next_order.wrapping_add(1);
        let e = self.files.entry(path.to_path_buf()).or_insert_with(|| Entry {
            ids: Vec::new(),
            order,
            retired: false,
            seen: mtime(path),
            pending: None,
        });
        e.retired = false;
        for id in ids {
            if !e.ids.contains(id) {
                e.ids.push(*id);
            }
        }
    }
    /// The editor that had `path` exited and its save was read: it stays watched (a later save still
    /// comes back) but is the first to go when the table is full.
    pub fn retire(&mut self, path: &Path) {
        if let Some(e) = self.files.get_mut(path) {
            e.retired = true;
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

    #[test]
    fn version_probe_and_wait_line() {
        let c = caps_from_version(r#"{"app":"photocraft","features":["saveInPlace","events","wait","singleInstance"],"protocol":1}"#);
        assert_eq!(c, Caps { wait: true, single_instance: true });
        // a release without `--version --json` prints its plain version line
        assert_eq!(caps_from_version("photocraft 0.7.0 (abc123 2026-10-01)"), Caps::default());
        assert_eq!(caps_from_version(r#"{"features":"wait"}"#), Caps::default());
        assert_eq!(caps_from_version(r#"{"features":["wait"]}"#), Caps { wait: true, single_instance: false });
        let l = wait_command_line("/opt/photocraft", Path::new("/a/b.tif"), c);
        assert_eq!(l, vec!["/opt/photocraft", "--wait", "--single-instance", "/a/b.tif"]);
        let l = wait_command_line("/opt/photocraft", Path::new("/a/b.tif"), Caps { wait: true, single_instance: false });
        assert_eq!(l, vec!["/opt/photocraft", "--wait", "/a/b.tif"]);
        assert!(is_photocraft("/Applications/PhotoCraft.app/Contents/MacOS/photocraft") && is_photocraft("PhotoCraft"));
        assert!(!is_photocraft("gimp"));
        // other apps are never probed
        assert_eq!(capabilities("/usr/bin/gimp"), Caps::default());
    }

    /// A long editing session: the 257th copy is still watched; a retired file goes first, else the oldest.
    #[test]
    fn a_full_table_makes_room_for_new_copies() {
        let mut w = Watched::default();
        let p = |i: usize| PathBuf::from(format!("/no/such/dir/{i}.tif"));
        for i in 0..MAX_WATCHED {
            w.add(&p(i), &[i as u64]);
        }
        w.retire(&p(5));
        w.add(&p(1000), &[1000]);
        assert_eq!(w.len(), MAX_WATCHED);
        assert_eq!(w.ids(&p(1000)), vec![1000], "the new copy is watched");
        assert!(w.ids(&p(5)).is_empty(), "the retired one made room");
        assert_eq!(w.ids(&p(0)), vec![0]);
        w.add(&p(1001), &[1001]);
        assert!(w.ids(&p(0)).is_empty(), "then the oldest");
        assert_eq!(w.ids(&p(1001)), vec![1001]);
        assert_eq!(w.len(), MAX_WATCHED);
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
}
