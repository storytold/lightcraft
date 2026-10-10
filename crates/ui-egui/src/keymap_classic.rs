//! The Classic keymap set (the default) beside the Alternative set (the keys declared on the
//! commands), keymap files, and the Classic-only steps of [`super::handle`]: module keys, Tab
//! presses held back from focus navigation. Kept out of the shared `shortcuts.rs` (U.5); a child
//! module of it, re-exported there.

use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Bindable, Keymap, assign, bindable, binding, matches, parse};
use crate::DacApp;

impl Bindable {
    /// The shortcut in the active keymap set: Classic's where [`CLASSIC`] names the command,
    /// else the declared one.
    pub fn default(&self) -> Option<&'static str> {
        default_in(active_set(), self)
    }
}

/// The keymap sets: Classic (the default) and the Alternative set (the keys declared on the
/// commands, which the app used before the Classic shell).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeymapSet {
    #[default]
    Classic,
    Alternative,
}

impl KeymapSet {
    pub fn parse(s: &str) -> Option<KeymapSet> {
        match s {
            "classic" | "Classic" => Some(KeymapSet::Classic),
            "alternative" | "Alternative" | "legacy" => Some(KeymapSet::Alternative),
            _ => None,
        }
    }
}

thread_local! {
    static ACTIVE: std::cell::Cell<KeymapSet> = const { std::cell::Cell::new(KeymapSet::Classic) };
}

/// The set the UI thread is using (follows [`crate::state::AppSettings::keymap_set`]).
pub fn active_set() -> KeymapSet {
    ACTIVE.with(|a| a.get())
}

pub fn set_active(set: KeymapSet) {
    ACTIVE.with(|a| a.set(set));
}

pub fn default_in(set: KeymapSet, b: &Bindable) -> Option<&'static str> {
    if set == KeymapSet::Classic
        && let Some((_, sc)) = CLASSIC.iter().find(|(id, _)| *id == b.id)
    {
        return *sc;
    }
    b.default
}

/// The Classic keymap: where it differs from the declared shortcuts (`None` = no key in Classic).
pub const CLASSIC: &[(&str, Option<&str>)] = &[
    // modules and views
    ("view.loupe", Some("E")),
    ("panel.edit", None),
    ("module.develop", Some("D")),
    ("view.detail", None),
    ("view.compare", Some("C")),
    ("module.library", Some("Cmd+Alt+1")),
    ("module.map", Some("Cmd+Alt+3")),
    ("module.book", Some("Cmd+Alt+4")),
    ("module.slideshow", Some("Cmd+Alt+5")),
    ("module.print", Some("Cmd+Alt+6")),
    ("module.web", Some("Cmd+Alt+7")),
    ("module.previous", Some("Cmd+Alt+Up")),
    // panels and screen
    ("panel.sides", Some("Tab")),
    ("panel.all", Some("Shift+Tab")),
    ("panel.top", Some("F5")),
    ("panel.bottom", Some("F6")),
    ("panel.left", Some("F7")),
    ("panel.right", Some("F8")),
    ("panel.toolbar", Some("T")),
    ("view.lightsOut", Some("L")),
    ("view.screenMode", Some("Shift+F")),
    ("view.screenModeNormal", Some("Cmd+Alt+F")),
    ("view.filterBar", None),
    // develop tools
    ("panel.crop", Some("R")),
    ("panel.remove", Some("Q")),
    ("panel.masking", Some("Shift+W")),
    ("tool.brush", Some("K")),
    ("tool.linear", Some("M")),
    ("tool.radial", Some("Shift+M")),
    ("tool.guidedUpright", Some("Shift+T")),
    ("panel.keywords", Some("Cmd+K")),
    // flags, collections
    ("photo.flagToggle", Some("`")),
    ("album.toggleTarget", Some("B")),
    ("library.showQuickCollection", Some("Cmd+B")),
    ("album.clearQuick", Some("Cmd+Alt+B")),
    ("album.saveQuick", Some("Cmd+Shift+B")),
    ("photo.copyMetadata", Some("Cmd+Alt+Shift+C")),
    ("photo.pasteMetadata", Some("Cmd+Alt+Shift+V")),
    // secondary window
    ("second.grid", Some("Shift+G")),
    ("second.loupe", Some("Shift+E")),
    ("second.compare", Some("Shift+C")),
    ("second.survey", Some("Shift+N")),
    ("second.slideshow", Some("Cmd+Shift+Enter")),
];

/// The largest keymap file read (a keymap is a few kilobytes).
const MAX_KEYMAP_FILE: u64 = 1 << 20;

/// `app.keymapSet {set: classic|alternative}` (no set: report the active one).
pub fn choose_set(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    if let Some(name) = p.get("set").and_then(Value::as_str) {
        let set = KeymapSet::parse(name).ok_or_else(|| format!("unknown keymap set: {name} (classic|alternative)"))?;
        app.ui.settings.keymap_set = set;
        set_active(set);
    }
    Ok(json!({"set": app.ui.settings.keymap_set}))
}

/// The keymap file: the set and the user's changes over it.
pub fn keymap_file(app: &DacApp) -> Value {
    json!({"format": "keymap", "version": 1, "set": app.ui.settings.keymap_set, "keymap": app.ui.settings.keymap})
}

/// `app.keymapExport {path}`.
pub fn export_file(app: &mut DacApp, path: &str) -> Result<Value, String> {
    let bytes = serde_json::to_vec_pretty(&keymap_file(app)).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("can't write {path}: {e}"))?;
    Ok(json!({"path": path, "count": app.ui.settings.keymap.len()}))
}

/// Apply a keymap file's contents: the set, then each entry through [`assign`] (so conflicts
/// move keys as in the editor). Bad entries are reported, the rest applied.
pub fn import_value(app: &mut DacApp, v: &Value) -> Result<Value, String> {
    let o = v.as_object().ok_or("a keymap file is a JSON object")?;
    if let Some(name) = o.get("set").and_then(Value::as_str) {
        let set = KeymapSet::parse(name).ok_or_else(|| format!("unknown keymap set: {name}"))?;
        app.ui.settings.keymap_set = set;
        set_active(set);
    }
    let entries = o.get("keymap").and_then(Value::as_object).ok_or("missing `keymap` object")?;
    let mut keymap = Keymap::new();
    let mut skipped = Vec::new();
    for (id, sc) in entries.iter().take(10_000) {
        let r = match sc {
            Value::String(s) if s.is_empty() => assign(&mut keymap, id, None),
            Value::String(s) => assign(&mut keymap, id, Some(s)),
            Value::Null => assign(&mut keymap, id, None),
            _ => Err("not a string".into()),
        };
        if let Err(e) = r {
            skipped.push(format!("{id}: {e}"));
        }
    }
    app.ui.settings.keymap = keymap;
    Ok(json!({"set": app.ui.settings.keymap_set, "count": app.ui.settings.keymap.len(), "skipped": skipped}))
}

/// `app.keymapImport {path}`.
pub fn import_file(app: &mut DacApp, path: &str) -> Result<Value, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("can't read {path}: {e}"))?.len();
    if len > MAX_KEYMAP_FILE {
        return Err(format!("{path} is too large for a keymap ({len} bytes)"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{path} is not a keymap file: {e}"))?;
    import_value(app, &v)
}

/// Keys the active set gives a command: the fixed bindings (aliases, ratings) yield to them
/// (Classic's ⇧E is the secondary loupe, not export).
pub(super) fn taken() -> Vec<(Modifiers, Key)> {
    if active_set() == KeymapSet::Classic { CLASSIC.iter().filter_map(|(_, sc)| sc.and_then(parse)).collect() } else { Vec::new() }
}

/// The module's own keys (Classic set): they come first and take their key from the global keymap.
pub(super) fn module_keys(module: crate::module::ModuleId) -> &'static [crate::module::ModuleKey] {
    if active_set() == KeymapSet::Classic { crate::module::get(module).keymap() } else { &[] }
}

/// Tab presses held back from egui's focus navigation fire the commands bound to them.
pub(super) fn deferred_tabs(deferred: &mut Vec<Modifiers>, keymap: &Keymap, fire: &mut Vec<String>) {
    for m in std::mem::take(deferred) {
        for b in bindable() {
            if let Some(sc) = binding(keymap, b.id, b.default())
                && let Some((bm, Key::Tab)) = parse(sc)
                && bm.shift == m.shift
                && bm.alt == m.alt
                && bm.command == m.command
            {
                fire.push(b.id.to_string());
            }
        }
    }
}

/// Module keys pressed this frame: their keys are `consumed` (the global keymap skips them).
pub(super) fn module_key_presses(
    i: &egui::InputState,
    keys: &[crate::module::ModuleKey],
    consumed: &mut Vec<(Modifiers, Key)>,
    aliased: &mut Vec<(&str, Value)>,
) {
    for (sc, id, params) in keys {
        if let Some((m, k)) = parse(sc)
            && matches(i, m, k)
        {
            consumed.push((m, k));
            aliased.push((id, serde_json::from_str(params).unwrap_or_default()));
        }
    }
}

/// Tab / ⇧Tab toggle panels: they must not also move keyboard focus into a field.
pub(super) fn keep_focus_on_panel_toggle(ctx: &egui::Context, fire: &[String]) {
    if fire.iter().any(|f| f == "panel.sides" || f == "panel.all") {
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
    }
}
