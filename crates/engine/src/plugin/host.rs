//! The sandbox's view of the session.

use craft_plugin::{Access, Manifest};
use serde_json::Value;

use super::{MAX_NOTE_CHARS, MAX_NOTES, MAX_RUN_STEPS, refusal, selection_json, set_setting, undo_all};
use crate::Session;

/// The sandbox's view of the session: every call is checked by [`refusal`] and run through
/// [`Session::execute`], like a menu, the CLI or MCP would; one that, from a read-only plug-in,
/// changes the library is undone and refused.
pub(super) struct EngineHost<'s> {
    s: &'s mut Session,
    manifest: &'s Manifest,
    /// [`Session::commits`] when the run started.
    start: u64,
    notes: Vec<String>,
}

impl<'s> EngineHost<'s> {
    pub(super) fn new(s: &'s mut Session, manifest: &'s Manifest, start: u64) -> Self {
        EngineHost { s, manifest, start, notes: Vec::new() }
    }

    /// What the run asked to show the user.
    pub(super) fn finish(self) -> Vec<String> {
        self.notes
    }

    /// Run `command` for a read-only plug-in. The run's history is empty while it runs (a read
    /// plug-in keeps no step), so a call that recorded a step edited the library: it is undone
    /// and refused.
    fn call_reading(&mut self, command: &str, params: &Value) -> Result<String, String> {
        let commits = self.s.commits();
        let r = self.s.execute(command, params);
        // (every edit of the catalog is an undo step; what LightCraft records for itself without
        // one, such as a raw's as-shot white read by a render, isn't the plug-in's edit)
        if self.s.commits() != commits || !self.s.undo.is_empty() {
            let undone = undo_all(self.s);
            let why = craft_plugin::check_read_only(self.manifest, command, true).err().unwrap_or_else(|| format!("`{command}` edits the library"));
            return Err(match undone {
                Ok(()) => why,
                Err(e) => format!("{why} (and it couldn't be put back: {e})"),
            });
        }
        r.map(|v| v.to_string()).map_err(|e| e.to_string())
    }
}

impl craft_plugin::Host for EngineHost<'_> {
    fn call(&mut self, command: &str, params: &str) -> Result<String, String> {
        let command = command.trim();
        let params: Value = serde_json::from_str(params).map_err(|e| format!("the params for `{command}` aren't JSON: {e}"))?;
        if let Some(why) = refusal(self.manifest, command, &params) {
            return Err(why);
        }
        if self.s.commits().wrapping_sub(self.start) >= MAX_RUN_STEPS {
            return Err(format!("a plug-in run may make at most {MAX_RUN_STEPS} edits; split the work into several runs"));
        }
        if self.manifest.permissions.access == Access::Write {
            self.s.execute(command, &params).map(|v| v.to_string()).map_err(|e| e.to_string())
        } else {
            self.call_reading(command, &params)
        }
    }

    fn selection(&mut self) -> String {
        selection_json(self.s).to_string()
    }

    fn notify(&mut self, message: &str) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(message.chars().take(MAX_NOTE_CHARS).collect());
        }
    }

    fn log(&mut self, line: &str) {
        // Also in the run's result (`logs`)
        log::info!("plug-in {}: {line}", self.manifest.id);
    }

    fn setting(&mut self, key: &str) -> Option<String> {
        Some(self.s.plugins.prefs.settings.get(&self.manifest.id)?.get(key)?.to_string())
    }

    fn set_setting(&mut self, key: &str, value: Option<&str>) -> Result<(), String> {
        set_setting(&mut self.s.plugins.prefs.settings, &self.manifest.id, key, value)
    }
}
