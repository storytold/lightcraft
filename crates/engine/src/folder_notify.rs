//! OS change notifications for folder polling (gap 37): FSEvents on macOS, inotify on Linux and
//! Android, kqueue on the BSDs, ReadDirectoryChangesW on Windows, through the pure-Rust `notify`
//! crate (CC0-1.0). They only say *when* to poll: a change in a watched folder starts a poll
//! a moment later ([`crate::folder_watch`] still decides what changed, from its listings), so the
//! poll's rules — scan only what changed, ask or add — stay the one path.
//!
//! Polling stays the fallback: a folder the OS can't watch (a network share whose server sends no
//! notifications, an inotify watch limit, a disk that went away) is listed in
//! [`OsWatch::unwatched`], and the app keeps polling every few minutes whatever the notifications
//! say, since a share often hears nothing of changes another computer makes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use notify::{EventKind, RecursiveMode, Watcher};

/// Watches on the library's folders (one level each, not recursive: subfolders that are the
/// library's are watched themselves).
pub struct OsWatch {
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<Vec<PathBuf>>,
    watched: Vec<String>,
    /// Folders the OS wouldn't watch (polling covers them).
    pub unwatched: Vec<String>,
}

impl OsWatch {
    /// Watch `dirs` (catalog spelling, `/`-separated). Fails only when the OS has no watcher at
    /// all; single folders that can't be watched are in [`OsWatch::unwatched`].
    pub fn start(dirs: &[String], cancel: &std::sync::atomic::AtomicBool) -> Result<OsWatch, String> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && !matches!(ev.kind, EventKind::Access(_))
            {
                let _ = tx.send(ev.paths);
            }
        })
        .map_err(|e| format!("folder notifications: {e}"))?;
        let mut watched = Vec::new();
        let mut unwatched = Vec::new();
        for d in dirs {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                unwatched.push(d.clone());
                continue;
            }
            match watcher.watch(Path::new(d), RecursiveMode::NonRecursive) {
                Ok(()) => watched.push(d.clone()),
                Err(e) => {
                    log::info!("folder notifications: {d}: {e} (polled instead)");
                    unwatched.push(d.clone());
                }
            }
        }
        Ok(OsWatch { _watcher: watcher, rx, watched, unwatched })
    }

    /// The watched folders something changed in since the last call.
    pub fn changed(&self) -> Vec<String> {
        let mut paths = Vec::new();
        while let Ok(p) = self.rx.try_recv() {
            paths.extend(p);
            // (a flood of events: what we have is enough to say "poll")
            if paths.len() > 100_000 {
                break;
            }
        }
        touched(&paths, &self.watched)
    }

    /// How many folders are watched.
    pub fn len(&self) -> usize {
        self.watched.len()
    }

    pub fn is_empty(&self) -> bool {
        self.watched.is_empty()
    }
}

/// Which of `watched` the event `paths` touch: a path in a watched folder, or the folder itself.
pub fn touched(paths: &[PathBuf], watched: &[String]) -> Vec<String> {
    let set: BTreeSet<&str> = watched.iter().map(String::as_str).collect();
    let mut out = BTreeSet::new();
    for p in paths {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/');
        if let Some(d) = find(&set, s) {
            out.insert(d.to_string());
        } else if let Some((parent, _)) = s.rsplit_once('/')
            && let Some(d) = find(&set, parent)
        {
            out.insert(d.to_string());
        }
    }
    out.into_iter().collect()
}

/// `s` in the set, comparing case-insensitively on case-insensitive systems' spellings too.
fn find<'a>(set: &BTreeSet<&'a str>, s: &str) -> Option<&'a str> {
    if let Some(d) = set.get(s) {
        return Some(d);
    }
    if cfg!(any(windows, target_os = "macos")) {
        return set.iter().find(|d| d.eq_ignore_ascii_case(s)).copied();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn events_map_to_their_watched_folder() {
        let watched = vec!["/p/trip".to_string(), "/p/2026".to_string()];
        let paths = [
            PathBuf::from("/p/trip/a.jpg"),
            PathBuf::from("/p/2026/11-20"),
            PathBuf::from("/p/2026/"),
            PathBuf::from("/elsewhere/x.jpg"),
            PathBuf::from(""),
            PathBuf::from("/"),
            PathBuf::from("/p/trip/sub/deep.jpg"),
        ];
        assert_eq!(touched(&paths, &watched), vec!["/p/2026".to_string(), "/p/trip".to_string()]);
        assert!(touched(&[], &watched).is_empty());
        assert!(touched(&paths, &[]).is_empty());
    }

    /// A file another app writes into a watched folder is reported (where the OS delivers
    /// notifications for temp folders; skipped otherwise).
    #[test]
    fn a_new_file_is_noticed() {
        let dir = std::env::temp_dir().join(format!("lcc-notify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // (macOS: /var is /private/var, and FSEvents reports the real path)
        #[cfg(not(windows))]
        let dir = std::fs::canonicalize(&dir).unwrap();
        let d = dir.to_string_lossy().replace('\\', "/");
        let missing = format!("{d}/not-there");
        let w = OsWatch::start(&[d.clone(), missing.clone()], &Default::default()).unwrap();
        assert_eq!(w.unwatched, vec![missing], "a folder that isn't there is left to polling");
        assert_eq!(w.len(), 1);
        std::fs::write(dir.join("new.jpg"), b"x").unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while seen.is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
            seen = w.changed();
        }
        let _ = std::fs::remove_dir_all(&dir);
        if seen.is_empty() {
            lcc_testkit::skip!(System, "no change notifications for the temp folder here");
        }
        assert_eq!(seen.len(), 1);
        assert!(seen[0].eq_ignore_ascii_case(&d) || std::path::Path::new(&seen[0]).ends_with(dir.file_name().unwrap()), "{seen:?}");
    }
}
