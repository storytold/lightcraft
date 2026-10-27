//! The UI state and app settings (`UiState`: language, shortcuts, panels …) kept in one `ui.json`
//! for a host without a writer of its own: the mobile shells, in their sandbox. (The desktop host
//! has its own, which also follows the window and the library location.)
//!
//! Loaded at launch; saved a few seconds after something changes and when the app quits, since a
//! phone may stop the app without asking. A damaged file is set aside as `ui.json.corrupt-<unix
//! time>` and the defaults are used; one that can't be read is never overwritten this session.
//! Nothing is saved while the library couldn't be opened ("Continue Without Saving").

use std::path::{Path, PathBuf};

use crate::state::UiState;

/// How long after a change the settings are written (s, interface time).
const SAVE_AFTER: f64 = 3.0;

/// Where `ui.json` is and what was last written to it.
pub struct UiPrefs {
    path: PathBuf,
    written: Vec<u8>,
    checked: f64,
    /// The file couldn't be read at launch: it is left alone.
    keep_file: bool,
    /// The last write failed (reported once until one works).
    failing: bool,
}

impl UiPrefs {
    /// Read `path`: the writer, the saved state if there is one, and a message for the user when
    /// the file exists but can't be used.
    pub fn load(path: impl Into<PathBuf>) -> (UiPrefs, Option<UiState>, Option<String>) {
        let path = path.into();
        let mut prefs = UiPrefs { path, written: Vec::new(), checked: f64::NEG_INFINITY, keep_file: false, failing: false };
        let bytes = match std::fs::read(&prefs.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (prefs, None, None),
            Err(e) => {
                prefs.keep_file = true;
                let msg =
                    format!("The app settings ({}) couldn't be read: {e}. Defaults are used, and the file isn't overwritten.", prefs.path.display());
                return (prefs, None, Some(msg));
            }
        };
        match serde_json::from_slice::<UiState>(&bytes) {
            Ok(mut ui) => {
                // a panel layout that doesn't hold together is left out (the rest still loads)
                if ui.docking.as_ref().is_some_and(|w| w.validate().is_err()) {
                    ui.docking = None;
                }
                prefs.written = bytes;
                (prefs, Some(ui.sanitized()), None)
            }
            Err(e) => {
                let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let keep = prefs.path.with_file_name(format!("ui.json.corrupt-{secs}"));
                let what = match std::fs::rename(&prefs.path, &keep) {
                    Ok(()) => format!("it was kept as {}", keep.display()),
                    Err(r) => {
                        prefs.keep_file = true;
                        format!("it couldn't be set aside ({r}), so it isn't overwritten")
                    }
                };
                let msg = format!("The app settings ({}) are damaged ({e}); {what}. Defaults are used.", prefs.path.display());
                (prefs, None, Some(msg))
            }
        }
    }

    /// Each frame (`now`: interface time): write `ui` when it changed, at most every few seconds.
    /// `temporary` (the library couldn't be opened) writes nothing. A failed write is returned
    /// once, until a write works again.
    pub fn tick(&mut self, ui: &UiState, now: f64, temporary: bool) -> Option<String> {
        if now - self.checked < SAVE_AFTER {
            return None;
        }
        self.checked = now;
        self.save(ui, temporary)
    }

    /// Write `ui` now if it changed (the app is quitting); as [`UiPrefs::tick`].
    pub fn save(&mut self, ui: &UiState, temporary: bool) -> Option<String> {
        if self.keep_file || temporary || ui.docking.as_ref().is_some_and(|w| w.validate().is_err()) {
            return None;
        }
        let bytes = match serde_json::to_vec_pretty(ui) {
            Ok(b) => b,
            Err(e) => return self.failed(e.to_string()),
        };
        if bytes == self.written {
            return None;
        }
        match write_atomic(&self.path, &bytes) {
            Ok(()) => {
                self.written = bytes;
                self.failing = false;
                None
            }
            Err(e) => self.failed(e.to_string()),
        }
    }

    fn failed(&mut self, e: String) -> Option<String> {
        let first = !self.failing;
        self.failing = true;
        first.then(|| format!("Saving the app settings ({}) failed: {e}", self.path.display()))
    }
}

/// Write `bytes` to `path` through a temp file, fsynced and renamed over it: a crash or a full
/// disk leaves the old file or the new one, never a truncated one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-prefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn settings_survive_a_relaunch() {
        let d = dir("relaunch");
        let path = d.join("ui.json");
        let (mut prefs, saved, warning) = UiPrefs::load(&path);
        assert!(saved.is_none() && warning.is_none(), "nothing saved yet");
        let mut ui = UiState { language: crate::i18n::Locale::De, ..Default::default() };
        ui.settings.keymap.insert("view.loupe".into(), "Cmd+L".into());
        // not before a few seconds have passed since the last look
        assert!(prefs.tick(&ui, 0.0, false).is_none());
        assert!(path.exists(), "written");
        ui.language = crate::i18n::Locale::Ja;
        prefs.tick(&ui, 1.0, false);
        let (_, saved, _) = UiPrefs::load(&path);
        assert_eq!(saved.map(|u| u.language), Some(crate::i18n::Locale::De), "the next write waits a few seconds");
        assert!(prefs.save(&ui, false).is_none());
        let (_, saved, warning) = UiPrefs::load(&path);
        let saved = saved.expect("loaded");
        assert!(warning.is_none());
        assert_eq!(saved.language, crate::i18n::Locale::Ja);
        assert_eq!(saved.settings.keymap.get("view.loupe").map(String::as_str), Some("Cmd+L"), "a reassigned shortcut is kept");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_temporary_session_saves_nothing() {
        let d = dir("temporary");
        let path = d.join("ui.json");
        let (mut prefs, _, _) = UiPrefs::load(&path);
        assert!(prefs.save(&UiState::default(), true).is_none());
        assert!(!path.exists(), "Continue Without Saving writes nothing");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_the_defaults_used() {
        let d = dir("damaged");
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("ui.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let (mut prefs, saved, warning) = UiPrefs::load(&path);
        assert!(saved.is_none());
        assert!(warning.is_some_and(|w| w.contains("damaged")));
        let kept: Vec<String> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.iter().any(|n| n.starts_with("ui.json.corrupt-")), "{kept:?}");
        // the next save writes a good file in its place
        assert!(prefs.save(&UiState::default(), false).is_none());
        assert!(UiPrefs::load(&path).1.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}
