use super::{LibraryProblem, LightcraftApp, UiState};

/// Where the system keeps monitor profiles, for the Choose Profile dialog to start in: the user's
/// own profiles first (Linux: colord / DisplayCAL; macOS: ColorSync), then the system's.
pub(crate) fn display_profile_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    let user = |rel: &str| home.as_ref().map(|h| h.join(rel));
    let candidates: Vec<Option<std::path::PathBuf>> = if cfg!(target_os = "macos") {
        vec![user("Library/ColorSync/Profiles"), Some("/Library/ColorSync/Profiles".into())]
    } else if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        vec![Some(root.join("System32").join("spool").join("drivers").join("color"))]
    } else {
        let data = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(std::path::PathBuf::from).or_else(|| user(".local/share"));
        vec![data.map(|d| d.join("icc")), user(".color/icc"), Some("/usr/share/color/icc".into())]
    };
    candidates.into_iter().flatten().find(|d| d.is_dir())
}

pub(crate) fn config_dir() -> Option<std::path::PathBuf> {
    lightcraft_engine::config::config_dir()
}

/// Native geometry follows the same no-write modes as ui.json (#1839).
pub(crate) fn remember_native_window(in_memory: bool, keep_prefs_file: bool, has_prefs_path: bool) -> bool {
    !in_memory && !keep_prefs_file && has_prefs_path
}

/// The window at the size it was last used at, zoomed if it was (`window`), or at the usual size
/// when nothing is remembered. A zoomed window is given the size it goes back to. Where it
/// opens is the system's to say on macOS/Linux; Windows additionally restores monitor-aware
/// geometry through eframe's persistence when available.
pub(crate) fn as_remembered(builder: egui::ViewportBuilder, window: Option<lightcraft_ui_egui::window_place::WindowPlace>) -> egui::ViewportBuilder {
    let builder = builder.with_inner_size(window.map_or(lightcraft_ui_egui::window_place::DEFAULT_SIZE, |w| w.size));
    // (only ever asked to zoom: asked not to, macOS un-zooms a window whose size fills the screen
    // to a size of its own choosing)
    if window.is_some_and(|w| w.zoomed) { builder.with_maximized(true) } else { builder }
}

/// `<config>/ui.json`, the saved UI state and app settings — `None` when nothing is read or
/// written: `LIGHTCRAFT_NO_PREFS` (tests, scripts) or no home folder.
pub(crate) fn prefs_path() -> Option<std::path::PathBuf> {
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return None;
    }
    config_dir().map(|d| d.join("ui.json"))
}

/// The saved UI state and app settings (`<config>/ui.json`), if any, and a warning for the user
/// when the file exists but can't be used (issue #103): a damaged file is kept as
/// `ui.json.corrupt-<unix time>` first, so the next save can't lose the library location in it;
/// one that can't be read at all is not written this session (`keep_file`).
///
/// A `--memory` session reads the settings (so it looks like the user's app) but writes nothing
/// (issue #164), so a damaged file is left as it is there, not set aside.
pub(crate) fn load_prefs(in_memory: bool) -> (Option<UiState>, Option<String>, bool) {
    let Some(path) = prefs_path() else { return (None, None, false) };
    load_prefs_at(&path, !in_memory)
}

/// Compatibility recovery is limited to persisted docking, not runtime UiState commands. An
/// arrangement from an older docking version starts again from the default without a warning.
fn decode_saved_ui(bytes: &[u8]) -> Result<(UiState, bool), String> {
    let mut saved: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let fields = saved.as_object_mut().ok_or("settings must be an object")?;
    lightcraft_ui_egui::docking::drop_outdated(fields);
    let recovered =
        fields.get("docking").is_some_and(|value| match serde_json::from_value::<Option<lightcraft_ui_egui::docking::Workspace>>(value.clone()) {
            Ok(Some(workspace)) => lightcraft_ui_egui::docking::validate(&workspace).is_err(),
            Ok(None) => false,
            Err(_) => true,
        });
    if recovered {
        fields.remove("docking");
    }
    let ui = serde_json::from_value(saved).map_err(|error| error.to_string())?;
    Ok((ui, recovered))
}

pub(crate) fn load_prefs_at(path: &std::path::Path, set_aside_damaged: bool) -> (Option<UiState>, Option<String>, bool) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, None, false),
        Err(e) => {
            let msg = format!("The app settings ({}) couldn't be read: {e}. Defaults are used, and the file isn't overwritten.", path.display());
            return (None, Some(msg), true);
        }
    };
    match decode_saved_ui(&bytes) {
        Ok((ui, false)) => (Some(ui.sanitized()), None, false),
        Ok((ui, true)) => {
            let msg = format!(
                "The saved panel layout in {} couldn't be restored. Other settings were loaded; the original file is preserved and settings won't be saved this session.",
                path.display()
            );
            (Some(ui.sanitized()), Some(msg), true)
        }
        Err(e) if !set_aside_damaged => {
            let msg = format!("The app settings ({}) are damaged ({e}). Defaults are used; nothing is saved in this session.", path.display());
            (None, Some(msg), true)
        }
        Err(e) => {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let keep = path.with_file_name(format!("ui.json.corrupt-{secs}"));
            let kept = std::fs::rename(path, &keep);
            let what = match &kept {
                Ok(()) => format!("it was kept as {}", keep.display()),
                Err(r) => format!("it couldn't be set aside ({r})"),
            };
            let msg = format!(
                "The app settings ({}) are damaged ({e}); {what}. Defaults are used — reopen your library with Settings → Open Library… if it isn't shown.",
                path.display()
            );
            (None, Some(msg), kept.is_err())
        }
    }
}

/// Write `bytes` to `path` atomically: a temp file, fsynced, renamed over it (a crash or a full
/// disk leaves the old file or the new one, never a truncated one).
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
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
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Saves `ui.json` (app settings, incl. the library to open at launch): as soon as the library
/// changes, every few seconds when anything else changed, and at exit.
///
/// Nothing is written while the session is one that promises to save nothing: `--memory`
/// (`path` is `None`; issue #164), or the one running because the library couldn't be opened — the
/// question window and Continue Without Saving (issue #100; `app.openLibrary` ends it, and the
/// library it opened is saved then).
#[derive(Default)]
pub(crate) struct PrefsWriter {
    /// Where to write; `None` writes nothing (`--memory`, `LIGHTCRAFT_NO_PREFS`, no home folder).
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) written: Vec<u8>,
    pub(crate) library: String,
    pub(crate) checked: f64,
    /// The last write failed (reported once until a write works).
    pub(crate) failing: bool,
    /// `ui.json` couldn't be read at launch: never overwrite it this session.
    pub(crate) keep_file: bool,
    /// Follows the window, to remember the size it rests at (`UiState::window`).
    pub(crate) window: lightcraft_ui_egui::window_place::WindowWatch,
    /// When the window was first looked at: its watch goes by the clock from then on, not by
    /// the interface's time, which stands still while the window is hidden.
    pub(crate) window_since: Option<std::time::Instant>,
}

impl PrefsWriter {
    pub(crate) fn save(&mut self, app: &LightcraftApp) -> Result<(), String> {
        self.save_ui(&app.ui, app.library_problem.as_ref().is_some_and(LibraryProblem::saves_nothing))
    }

    /// Write `ui` if it changed since the last write; `temporary` (the library couldn't be
    /// opened) saves nothing.
    pub(crate) fn save_ui(&mut self, ui: &UiState, temporary: bool) -> Result<(), String> {
        if self.keep_file || temporary {
            return Ok(());
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        if let Some(workspace) = &ui.docking {
            lightcraft_ui_egui::docking::validate(workspace)?;
        }
        let bytes = serde_json::to_vec_pretty(ui).map_err(|e| e.to_string())?;
        if bytes == self.written {
            return Ok(());
        }
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| write_atomic(&path, &bytes))
            .map_err(|e| format!("saving the app settings failed: {e}"))?;
        self.written = bytes;
        self.library = ui.settings.library_path.clone();
        Ok(())
    }

    /// What to remember of the window as the app quits ([`WindowWatch::at_exit`]).
    ///
    /// [`WindowWatch::at_exit`]: lightcraft_ui_egui::window_place::WindowWatch::at_exit
    pub(crate) fn window_at_exit(
        &mut self,
        remembered: Option<lightcraft_ui_egui::window_place::WindowPlace>,
    ) -> Option<lightcraft_ui_egui::window_place::WindowPlace> {
        match self.window_since {
            Some(since) => self.window.at_exit(remembered, since.elapsed().as_secs_f64()),
            None => remembered,
        }
    }

    /// Per-frame host tick: records the window's placement while visible, asks for a repaint when
    /// a remembered size is due, and saves settings/library at most every three seconds or
    /// immediately after the library path changes; a failed save is reported once and retried.
    pub(crate) fn tick(&mut self, app: &mut LightcraftApp, ctx: &egui::Context) {
        // (asked for before the input is read: the context is not asked from inside its own lock)
        let zoom = ctx.zoom_factor();
        // The monotonic app clock: `i.time` stands still while the window is hidden.
        let now = app.tick_now();
        let (hidden, seen) = ctx.input(|i| {
            let hidden = i.viewport().visible() == Some(false);
            (hidden, lightcraft_ui_egui::window_place::Seen::of(i.viewport(), i.content_rect(), zoom))
        });
        // how the window is, to open like that next time (written with the settings below). What
        // the settings hold is checked first: a script may have set it (`ui.set`). A hidden
        // `run_logic` pass has no fresh content rect: keep the last known placement.
        let remembered = app.ui.window.and_then(lightcraft_ui_egui::window_place::WindowPlace::checked);
        let at = self.window_since.get_or_insert_with(std::time::Instant::now).elapsed().as_secs_f64();
        if !hidden {
            app.ui.window = self.window.note(remembered, &seen, at);
        }
        if let Some(wait) = self.window.waits(at) {
            // (a size is only remembered once the window has rested at it: look again then, though
            // nothing else asks for a frame)
            ctx.request_repaint_after(wait);
        }
        let moved = app.ui.settings.library_path != self.library;
        if !moved && now - self.checked < 3.0 {
            return;
        }
        self.checked = now;
        match self.save(app) {
            Ok(()) => self.failing = false,
            Err(e) => {
                if !self.failing {
                    log::warn!("{e}");
                    app.notices.push(format!("{e}. LightCraft keeps trying."));
                }
                self.failing = true;
                // don't retry every frame
                self.library = app.ui.settings.library_path.clone();
            }
        }
    }
}
