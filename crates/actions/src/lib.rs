//! Actions: recorded, parameterised sequences of engine commands (P4.5).
//!
//! An [`Action`] is a name plus a list of [`Step`]s (`command` id + JSON params), optional
//! [`Param`]s that steps refer to as `{{name}}`, an optional shortcut and whether it runs once per
//! photo of the selection. This crate holds the model, the file format (`actions.json`), the rules
//! for turning a command journal into steps ([`steps_from_journal`]) and for binding arguments
//! ([`bind`]); the engine (`crates/engine/src/cmd/actions.rs`) runs the steps through its command
//! registry, so the UI, CLI, control channel and MCP share one implementation.
//!
//! Sources: own design, after PhotoCraft's `actions_cmds` (record from the journal, replay through
//! the command registry, stop at the first error, bounded nesting).
//!
//! Everything here is input-hostile: files and arguments come from users and agents, so sizes and
//! nesting are capped and nothing panics.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Errors are plain messages a user or agent can act on.
pub type Result<T> = std::result::Result<T, String>;

/// Most actions kept.
pub const MAX_ACTIONS: usize = 1000;
/// Most steps in one action.
pub const MAX_STEPS: usize = 1000;
/// Largest `actions.json` read.
pub const MAX_FILE_BYTES: u64 = 8 << 20;
/// Deepest nesting of actions playing actions.
pub const MAX_NESTING: usize = 8;
/// Deepest JSON nesting walked when binding arguments.
const MAX_JSON_DEPTH: usize = 64;
/// File format version.
pub const VERSION: u32 = 1;

/// One recorded command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub command: String,
    #[serde(default = "empty_object")]
    pub params: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// A parameter of an action: steps refer to it as `{{name}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    /// Used when a run gives no value (`null` = the run must give one).
    #[serde(default)]
    pub default: Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A saved action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// A shortcut the desktop app binds to `actions.play {name}` (`Cmd+Alt+1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// Run the steps once for each selected photo (that photo alone selected), instead of once.
    #[serde(default = "yes")]
    pub per_photo: bool,
}

fn yes() -> bool {
    true
}

impl Action {
    pub fn new(name: impl Into<String>) -> Self {
        Action { name: name.into(), description: String::new(), steps: Vec::new(), params: Vec::new(), shortcut: None, per_photo: true }
    }

    /// Check what a user or agent may have written by hand.
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        if self.steps.len() > MAX_STEPS {
            return Err(format!("an action has at most {MAX_STEPS} steps ({} given)", self.steps.len()));
        }
        for (i, s) in self.steps.iter().enumerate() {
            if s.command.is_empty() || s.command.len() > 200 {
                return Err(format!("step {}: no command id", i + 1));
            }
            if !s.params.is_object() {
                return Err(format!("step {} (`{}`): params must be an object", i + 1, s.command));
            }
        }
        for (i, p) in self.params.iter().enumerate() {
            validate_name(&p.name).map_err(|e| format!("parameter {}: {e}", i + 1))?;
            if self.params.iter().take(i).any(|q| q.name == p.name) {
                return Err(format!("parameter `{}` is declared twice", p.name));
            }
        }
        // every placeholder must name a declared parameter
        for s in &self.steps {
            let mut names = Vec::new();
            placeholders(&s.params, 0, &mut names)?;
            if let Some(n) = names.iter().find(|n| !self.params.iter().any(|p| &p.name == *n)) {
                return Err(format!("step `{}` uses `{{{{{n}}}}}`, which is not a parameter of the action", s.command));
            }
        }
        Ok(())
    }
}

/// A name a user can see and type: 1–100 characters, no control characters.
pub fn validate_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() {
        return Err("the name is empty".into());
    }
    if n.chars().count() > 100 {
        return Err("the name is longer than 100 characters".into());
    }
    if n.chars().any(char::is_control) {
        return Err("the name contains control characters".into());
    }
    Ok(())
}

/// Commands that only move around the library (selection, view, filter, sort). They are left out
/// of a recording: an action replays edits, and a per-photo run chooses the photos itself.
pub fn is_navigation(id: &str) -> bool {
    matches!(
        id,
        "library.select"
            | "library.selectBy"
            | "library.selectAll"
            | "library.selectNone"
            | "library.next"
            | "library.previous"
            | "library.first"
            | "library.last"
            | "library.source"
            | "library.filter"
            | "library.clearFilter"
            | "library.sort"
            | "library.shuffle"
            | "library.browse"
            | "library.showQuickCollection"
            | "filter.applyPreset"
    ) || id.starts_with("view.")
}

/// May journal entry `id` become a step? `journaled` says whether the registry journals it.
/// Commands that edit actions are never recorded; `actions.play` is (an action may call another).
pub fn recordable(id: &str, journaled: bool) -> bool {
    if id == "actions.play" {
        return true;
    }
    journaled && !id.starts_with("actions.") && !is_navigation(id)
}

/// The steps a recording made: the journal entries since it started, minus what
/// [`recordable`] rejects. Unless `keep_targets`, the photos a call named (`ids` / `id`) are
/// dropped, so the step applies to whatever is selected when the action runs.
pub fn steps_from_journal(entries: &[(String, Value)], keep_targets: bool, journaled: impl Fn(&str) -> bool) -> Vec<Step> {
    entries
        .iter()
        .filter(|(id, _)| recordable(id, journaled(id)))
        .take(MAX_STEPS)
        .map(|(id, p)| {
            let mut params = if p.is_object() { p.clone() } else { empty_object() };
            if !keep_targets && let Some(o) = params.as_object_mut() {
                o.remove("ids");
                o.remove("id");
            }
            Step { command: id.clone(), params }
        })
        .collect()
}

/// The names of the `{{name}}` placeholders in `v`.
fn placeholders(v: &Value, depth: usize, out: &mut Vec<String>) -> Result<()> {
    if depth > MAX_JSON_DEPTH {
        return Err("params are nested too deeply".into());
    }
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(a) = rest.find("{{") {
                let after = rest.get(a + 2..).unwrap_or("");
                let Some(b) = after.find("}}") else { break };
                let name = after.get(..b).unwrap_or("").trim();
                if !name.is_empty() && !out.iter().any(|n| n == name) {
                    out.push(name.to_string());
                }
                rest = after.get(b + 2..).unwrap_or("");
            }
        }
        Value::Array(a) => {
            for x in a {
                placeholders(x, depth + 1, out)?;
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                placeholders(x, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The action's steps with `args` filled in. A string that is exactly `{{name}}` becomes the
/// argument's value (any JSON type: `{"value": "{{ev}}"}` with `ev = 0.5` gives a number); a
/// placeholder inside a longer string is replaced by the argument as text. A parameter the run
/// does not give takes its default; one with no default is an error, and so is an argument the
/// action does not declare.
pub fn bind(action: &Action, args: &Map<String, Value>) -> Result<Vec<Step>> {
    if let Some(k) = args.keys().find(|k| !action.params.iter().any(|p| &p.name == *k)) {
        return Err(format!("`{}` has no parameter `{k}`", action.name));
    }
    let mut values = Map::new();
    for p in &action.params {
        let v = args.get(&p.name).cloned().unwrap_or_else(|| p.default.clone());
        if v.is_null() {
            return Err(format!("`{}` needs a value for `{}`", action.name, p.name));
        }
        values.insert(p.name.clone(), v);
    }
    action.steps.iter().map(|s| Ok(Step { command: s.command.clone(), params: fill(&s.params, &values, 0)? })).collect()
}

fn fill(v: &Value, values: &Map<String, Value>, depth: usize) -> Result<Value> {
    if depth > MAX_JSON_DEPTH {
        return Err("params are nested too deeply".into());
    }
    Ok(match v {
        Value::String(s) => {
            let t = s.trim();
            if let Some(name) = t.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
                && !name.contains("{{")
                && let Some(x) = values.get(name.trim())
            {
                return Ok(x.clone());
            }
            let (mut out, mut rest) = (String::new(), s.as_str());
            while let Some(a) = rest.find("{{") {
                let after = rest.get(a + 2..).unwrap_or("");
                let Some(b) = after.find("}}") else { break };
                out.push_str(rest.get(..a).unwrap_or(""));
                let name = after.get(..b).unwrap_or("").trim();
                match values.get(name) {
                    Some(Value::String(x)) => out.push_str(x),
                    Some(x) => out.push_str(&x.to_string()),
                    None => out.push_str(rest.get(a..a + 2 + b + 2).unwrap_or("")),
                }
                rest = after.get(b + 2..).unwrap_or("");
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, values, depth + 1)).collect::<Result<_>>()?),
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, x) in o {
                m.insert(k.clone(), fill(x, values, depth + 1)?);
            }
            Value::Object(m)
        }
        other => other.clone(),
    })
}

/// On-disk form of the action list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionsFile {
    pub version: u32,
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// Read `actions.json`. A missing file is an empty list; a damaged one is an error (the caller
/// keeps the file rather than overwriting it).
pub fn load(path: &Path) -> Result<Vec<Action>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} is larger than {} MB", path.display(), MAX_FILE_BYTES >> 20));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse an actions file (also used to import one).
pub fn parse(bytes: &[u8]) -> Result<Vec<Action>> {
    let f: ActionsFile = serde_json::from_slice(bytes).map_err(|e| format!("not an actions file ({e})"))?;
    if f.version > VERSION {
        return Err(format!("written by a newer version (format {})", f.version));
    }
    let mut out: Vec<Action> = Vec::new();
    for a in f.actions.into_iter().take(MAX_ACTIONS) {
        a.validate().map_err(|e| format!("action `{}`: {e}", a.name))?;
        if out.iter().any(|b| b.name == a.name) {
            return Err(format!("action `{}` appears twice", a.name));
        }
        out.push(a);
    }
    Ok(out)
}

/// The file bytes for `actions`.
pub fn to_bytes(actions: &[Action]) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(&ActionsFile { version: VERSION, actions: actions.to_vec() }).map_err(|e| e.to_string())
}

/// Write `actions.json` atomically (a temporary file renamed over it).
pub fn save(path: &Path, actions: &[Action]) -> Result<()> {
    let bytes = to_bytes(actions)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// A recording in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub name: String,
    /// Journal length when it started.
    pub start: usize,
    /// Keep the photos calls named.
    pub keep_targets: bool,
}

/// The engine's action state: the list, where it is kept, a recording and the actions playing.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub list: Vec<Action>,
    /// `list` was read from `file` (it is read on first use).
    pub loaded: bool,
    /// Where the list is kept (`None` = in memory only).
    pub file: Option<PathBuf>,
    pub recording: Option<Recording>,
    /// Names of the actions playing, outermost first (cycle and depth checks).
    pub playing: Vec<String>,
}

impl State {
    /// A state that keeps its list in `file`.
    pub fn with_file(file: Option<PathBuf>) -> Self {
        State { file, ..State::default() }
    }

    /// Read the list on first use. A damaged file is reported and left alone (`loaded` stays
    /// false, so nothing is written over it).
    pub fn ensure_loaded(&mut self) -> Result<()> {
        if self.loaded {
            return Ok(());
        }
        if let Some(f) = &self.file {
            self.list = load(f)?;
        }
        self.loaded = true;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Action> {
        self.list.iter().find(|a| a.name == name.trim())
    }

    /// Add `a` or replace the action of the same name, and save.
    pub fn upsert(&mut self, a: Action) -> Result<()> {
        a.validate()?;
        self.ensure_loaded()?;
        match self.list.iter_mut().find(|b| b.name == a.name) {
            Some(b) => *b = a,
            None => {
                if self.list.len() >= MAX_ACTIONS {
                    return Err(format!("at most {MAX_ACTIONS} actions"));
                }
                self.list.push(a);
            }
        }
        self.persist()
    }

    /// Remove the action `name` and save; false when there is none.
    pub fn remove(&mut self, name: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let before = self.list.len();
        self.list.retain(|a| a.name != name.trim());
        if self.list.len() == before {
            return Ok(false);
        }
        self.persist()?;
        Ok(true)
    }

    pub fn persist(&self) -> Result<()> {
        match &self.file {
            Some(f) if self.loaded => save(f, &self.list),
            _ => Ok(()),
        }
    }

    /// Enter action `name` for playing; errors on a cycle or nesting too deep.
    pub fn enter(&mut self, name: &str) -> Result<()> {
        if self.playing.iter().any(|n| n == name) {
            return Err(format!("`{name}` plays itself (through {})", self.playing.join(" → ")));
        }
        if self.playing.len() >= MAX_NESTING {
            return Err(format!("actions nest more than {MAX_NESTING} deep"));
        }
        self.playing.push(name.to_string());
        Ok(())
    }

    pub fn leave(&mut self) {
        self.playing.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(c: &str, p: Value) -> Step {
        Step { command: c.into(), params: p }
    }

    #[test]
    fn recording_keeps_edits_and_drops_navigation_and_targets() {
        let j = vec![
            ("library.select".to_string(), json!({"ids": [1]})),
            ("develop.set".to_string(), json!({"ids": [1], "key": "light.exposure", "value": 1})),
            ("photo.rate".to_string(), json!({"rating": 3})),
            ("actions.record".to_string(), json!({})),
            ("actions.play".to_string(), json!({"name": "x"})),
            ("query.only".to_string(), json!({})),
        ];
        let s = steps_from_journal(&j, false, |id| id != "query.only");
        assert_eq!(
            s,
            vec![
                step("develop.set", json!({"key": "light.exposure", "value": 1})),
                step("photo.rate", json!({"rating": 3})),
                step("actions.play", json!({"name": "x"})),
            ]
        );
        let kept = steps_from_journal(&j, true, |_| true);
        assert_eq!(kept[0].params["ids"], json!([1]));
    }

    #[test]
    fn bind_fills_typed_and_inline_placeholders() {
        let mut a = Action::new("Brighten");
        a.params.push(Param { name: "ev".into(), default: json!(0.5), description: String::new() });
        a.params.push(Param { name: "tag".into(), default: Value::Null, description: String::new() });
        a.steps.push(step("develop.set", json!({"key": "light.exposure", "value": "{{ev}}"})));
        a.steps.push(step("keyword.add", json!({"keywords": ["edit-{{ tag }}"], "note": "{{tag}} at {{ev}}"})));
        a.validate().unwrap();
        let mut args = Map::new();
        assert!(bind(&a, &args).unwrap_err().contains("needs a value for `tag`"));
        args.insert("tag".into(), json!("night"));
        let s = bind(&a, &args).unwrap();
        assert_eq!(s[0].params["value"], json!(0.5));
        assert_eq!(s[1].params["note"], json!("night at 0.5"));
        assert_eq!(s[1].params["keywords"], json!(["edit-night"]));
        args.insert("nope".into(), json!(1));
        assert!(bind(&a, &args).is_err());
    }

    #[test]
    fn validate_rejects_undeclared_placeholders_and_bad_names() {
        let mut a = Action::new("x");
        a.steps.push(step("develop.set", json!({"value": "{{ev}}"})));
        assert!(a.validate().unwrap_err().contains("not a parameter"));
        assert!(Action::new("  ").validate().is_err());
        assert!(Action::new("a\u{7}b").validate().is_err());
        let mut b = Action::new("y");
        b.steps.push(step("x", json!(3)));
        assert!(b.validate().is_err());
    }

    #[test]
    fn file_round_trip_and_hostile_files() {
        let dir = std::env::temp_dir().join(format!("dac-actions-{}", std::process::id()));
        let f = dir.join("actions.json");
        let mut st = State::with_file(Some(f.clone()));
        let mut a = Action::new("One");
        a.steps.push(step("photo.rate", json!({"rating": 5})));
        st.upsert(a.clone()).unwrap();
        assert_eq!(load(&f).unwrap(), vec![a]);
        assert!(st.remove("One").unwrap());
        assert!(!st.remove("One").unwrap());
        std::fs::write(&f, b"{not json").unwrap();
        let mut broken = State::with_file(Some(f.clone()));
        assert!(broken.ensure_loaded().is_err());
        assert!(!broken.loaded);
        assert!(parse(br#"{"version": 99, "actions": []}"#).is_err());
        assert!(parse(br#"{"version": 1, "actions": [{"name": "a"}, {"name": "a"}]}"#).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nesting_is_bounded_and_cycles_found() {
        let mut st = State::default();
        st.enter("a").unwrap();
        assert!(st.enter("a").is_err());
        for i in 1..MAX_NESTING {
            st.enter(&format!("n{i}")).unwrap();
        }
        assert!(st.enter("deep").is_err());
        st.leave();
        assert_eq!(st.playing.len(), MAX_NESTING - 1);
    }

    #[test]
    fn deep_json_is_an_error_not_a_stack_overflow() {
        let mut v = json!("{{x}}");
        for _ in 0..200 {
            v = json!([v]);
        }
        let mut a = Action::new("deep");
        a.steps.push(step("s", json!({"v": v})));
        assert!(a.validate().is_err());
    }
}
