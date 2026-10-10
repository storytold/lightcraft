//! Actions (P4.5): record a sequence of commands, parameterise and save it, play it on the
//! selection (`actions.*`). The model, file format and argument binding live in `dac-actions`;
//! this module keeps the list on the [`Session`] and plays steps through [`Session::execute`], so
//! the desktop app, `app-cli`, the control channel and MCP all run the same thing.
//!
//! Recording copies the journal: `actions.record` remembers where the journal stood and
//! `actions.stop` turns the entries since then into steps ([`dac_actions::steps_from_journal`]:
//! selection, view and filter commands are dropped, and so are the photos calls named, so a step
//! applies to whatever is selected when the action plays). Playback stops at the first error and
//! reports which step failed; the steps that ran stay (each is its own undo step).
//!
//! Sources: own design, after PhotoCraft's `actions_cmds`.

use std::path::PathBuf;

use dac_actions::{Action, Param, Recording, Step};
use serde_json::{Map, Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

/// Session state for actions and Edit In presets (`Session::workflow`).
pub struct Workflow {
    pub actions: dac_actions::State,
    /// Where Edit In presets are kept (`editors.json`; `None` = in memory only).
    pub editors_file: Option<PathBuf>,
    /// Edit In presets the user saved (read from `editors_file` on first use).
    pub(crate) editors: Option<Vec<super::edit_in::EditorPreset>>,
}

impl Default for Workflow {
    /// Kept in the settings folder, except in unit tests and with `<PREFIX>_NO_PREFS`.
    fn default() -> Self {
        let dir = if cfg!(test) || dac_brand::env_is_set("NO_PREFS") { None } else { crate::config::config_dir() };
        Workflow {
            actions: dac_actions::State::with_file(dir.as_ref().map(|d| d.join("actions.json"))),
            editors_file: dir.map(|d| d.join("editors.json")),
            editors: None,
        }
    }
}

impl Workflow {
    /// In memory only, or kept in `dir` (tests, a portable setup).
    pub fn in_dir(dir: Option<PathBuf>) -> Self {
        Workflow {
            actions: dac_actions::State::with_file(dir.as_ref().map(|d| d.join("actions.json"))),
            editors_file: dir.map(|d| d.join("editors.json")),
            editors: None,
        }
    }
}

const C: &str = "actions";

fn name_param<'a>(p: &'a Value, cmd: &str) -> Result<&'a str> {
    let n = str_param(p, "name").map(str::trim).unwrap_or("");
    dac_actions::validate_name(n).map_err(|e| bad(cmd, e))?;
    Ok(n)
}

fn loaded(s: &mut Session, cmd: &str) -> Result<()> {
    s.workflow.actions.ensure_loaded().map_err(|e| bad(cmd, format!("the saved actions can't be read: {e}")))
}

fn summary(a: &Action) -> Value {
    json!({
        "name": a.name,
        "description": a.description,
        "steps": a.steps.len(),
        "params": a.params,
        "shortcut": a.shortcut,
        "perPhoto": a.per_photo,
    })
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    loaded(s, "actions.list")?;
    let w = &s.workflow.actions;
    Ok(json!({
        "actions": w.list.iter().map(summary).collect::<Vec<_>>(),
        "recording": w.recording.as_ref().map(|r| r.name.clone()),
    }))
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "actions.get";
    loaded(s, C)?;
    let n = name_param(p, C)?;
    let a = s.workflow.actions.find(n).ok_or_else(|| bad(C, format!("no action `{n}`")))?;
    serde_json::to_value(a).map_err(|e| bad(C, e.to_string()))
}

fn record(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "actions.record";
    let n = name_param(p, C)?.to_string();
    if let Some(r) = &s.workflow.actions.recording {
        return Err(bad(C, format!("already recording `{}`: stop it first", r.name)));
    }
    s.workflow.actions.recording = Some(Recording { name: n.clone(), start: s.journal.len(), keep_targets: bool_or(p, "keepTargets", false) });
    Ok(json!({"recording": n}))
}

fn stop(s: &mut Session, _: &Value) -> Result<Value> {
    const C: &str = "actions.stop";
    loaded(s, C)?;
    let r = s.workflow.actions.recording.take().ok_or_else(|| bad(C, "not recording"))?;
    // (the journal drops its oldest entries past 10 000; a recording that long keeps what is left)
    let start = r.start.min(s.journal.len());
    let entries = s.journal.get(start..).unwrap_or_default();
    let steps = dac_actions::steps_from_journal(entries, r.keep_targets, |id| super::find_command(id).is_some_and(|c| c.journal));
    if steps.is_empty() {
        return Err(bad(C, format!("nothing to save: `{}` recorded no edits", r.name)));
    }
    // re-recording an action keeps its description, parameters and shortcut
    let mut a = s.workflow.actions.find(&r.name).cloned().unwrap_or_else(|| Action::new(r.name.clone()));
    a.steps = steps;
    if a.validate().is_err() {
        // the new steps no longer use the old parameters' placeholders
        a.params.clear();
    }
    let n = a.steps.len();
    s.workflow.actions.upsert(a.clone()).map_err(|e| bad(C, e))?;
    Ok(json!({"name": a.name, "steps": n}))
}

fn cancel(s: &mut Session, _: &Value) -> Result<Value> {
    let r = s.workflow.actions.recording.take();
    Ok(json!({"cancelled": r.map(|r| r.name)}))
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "actions.save";
    loaded(s, C)?;
    let mut a: Action = match p.get("action") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| bad(C, format!("not an action: {e}")))?,
        None => {
            // update the fields given of an existing action
            let n = name_param(p, C)?;
            let mut a = s.workflow.actions.find(n).cloned().ok_or_else(|| bad(C, format!("no action `{n}`: give `action`")))?;
            if let Some(d) = str_param(p, "description") {
                a.description = d.to_string();
            }
            if let Some(v) = p.get("shortcut") {
                a.shortcut = v.as_str().filter(|x| !x.is_empty()).map(str::to_string);
            }
            if let Some(v) = p.get("perPhoto").and_then(Value::as_bool) {
                a.per_photo = v;
            }
            if let Some(v) = p.get("params") {
                a.params = serde_json::from_value::<Vec<Param>>(v.clone()).map_err(|e| bad(C, format!("bad `params`: {e}")))?;
            }
            if let Some(v) = p.get("steps") {
                a.steps = serde_json::from_value::<Vec<Step>>(v.clone()).map_err(|e| bad(C, format!("bad `steps`: {e}")))?;
            }
            a
        }
    };
    a.name = a.name.trim().to_string();
    if let Some(sc) = &a.shortcut
        && let Some(other) = s.workflow.actions.list.iter().find(|b| b.name != a.name && b.shortcut.as_deref() == Some(sc))
    {
        return Err(bad(C, format!("{sc} already plays `{}`", other.name)));
    }
    s.workflow.actions.upsert(a.clone()).map_err(|e| bad(C, e))?;
    Ok(summary(&a))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "actions.delete";
    let n = name_param(p, C)?.to_string();
    let gone = s.workflow.actions.remove(&n).map_err(|e| bad(C, e))?;
    if !gone {
        return Err(bad(C, format!("no action `{n}`")));
    }
    Ok(json!({"deleted": n}))
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "actions.play";
    loaded(s, C)?;
    let n = name_param(p, C)?.to_string();
    let a = s.workflow.actions.find(&n).cloned().ok_or_else(|| bad(C, format!("no action `{n}`")))?;
    let empty = Map::new();
    let args = match p.get("args") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err(bad(C, "`args` must be an object")),
    };
    let steps = dac_actions::bind(&a, args).map_err(|e| bad(C, e))?;
    let targets = s.targets(p);
    let per_photo = p.get("perPhoto").and_then(Value::as_bool).unwrap_or(a.per_photo);
    if per_photo && targets.is_empty() {
        return Err(bad(C, "no photos selected"));
    }
    s.workflow.actions.enter(&n).map_err(|e| bad(C, e))?;
    let before = s.selection.clone();
    let r = run_steps(s, &n, &steps, if per_photo { targets.clone() } else { Vec::new() });
    s.workflow.actions.leave();
    // a per-photo run hands back the selection it started with (photos it deleted aside)
    if per_photo {
        let mut sel = before;
        sel.ids.retain(|id| s.catalog.photo(*id).is_some());
        if sel.active.is_some_and(|id| s.catalog.photo(id).is_none()) {
            sel.active = sel.ids.first().copied();
        }
        s.selection = sel;
    }
    let ran = r?;
    Ok(json!({"name": n, "steps": steps.len(), "photos": if per_photo { targets.len() } else { 0 }, "ran": ran}))
}

/// Run `steps` once, or once per photo of `photos` with that photo alone selected. Stops at the
/// first error. → steps run
fn run_steps(s: &mut Session, name: &str, steps: &[Step], photos: Vec<dac_catalog::PhotoId>) -> Result<usize> {
    let rounds: Vec<Option<dac_catalog::PhotoId>> = if photos.is_empty() { vec![None] } else { photos.into_iter().map(Some).collect() };
    let mut ran = 0usize;
    for photo in rounds {
        if let Some(id) = photo {
            if s.catalog.photo(id).is_none() {
                continue;
            }
            s.selection = crate::Selection::single(id);
        }
        for (i, st) in steps.iter().enumerate() {
            if dac_actions::is_navigation(&st.command) && photo.is_some() {
                continue;
            }
            s.execute(&st.command, &st.params).map_err(|e| {
                let on = photo.map(|id| format!(" on photo {}", id.0)).unwrap_or_default();
                bad(C, format!("`{name}` stopped at step {} (`{}`){on}: {e}", i + 1, st.command))
            })?;
            ran += 1;
        }
    }
    Ok(ran)
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "actions.list", "Actions", [], None, "{} — the saved actions and the one being recorded → {actions: [{name, description, steps, params, shortcut, perPhoto}], recording}", always, list),
        cmd!(query "actions.get", "Get Action", [], None, "{name} — one action in full → {name, description, steps: [{command, params}], params: [{name, default, description}], shortcut, perPhoto}", always, get),
        cmd!(
            "actions.record",
            "Start Recording Action…",
            [],
            None,
            "{name, keepTargets?: false} — start recording the commands that follow into action `name` (selection, view and filter commands are left out; unless keepTargets the photos a call names are dropped, so steps apply to the selection when played) → {recording}",
            always,
            record
        ),
        cmd!(
            "actions.stop",
            "Stop Recording",
            [],
            None,
            "{} — save what was recorded since actions.record (re-recording keeps the description, parameters and shortcut) → {name, steps}",
            always,
            stop
        ),
        cmd!("actions.cancel", "Cancel Recording", [], None, "{} — stop recording without saving → {cancelled}", always, cancel),
        cmd!(
            "actions.save",
            "Save Action",
            [],
            None,
            "{action: {name, description?, steps: [{command, params}], params?: [{name, default?, description?}], shortcut?, perPhoto?: true}} to define or replace an action, or {name, description?, steps?, params?, shortcut?, perPhoto?} to change one; steps refer to a parameter as \"{{name}}\" (a whole string becomes the value, any JSON type) → the action's summary",
            always,
            save
        ),
        cmd!("actions.delete", "Delete Action", [], None, "{name} — delete a saved action → {deleted}", always, delete),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "actions.play",
                "Play Action",
                [],
                None,
                "{name, args?: {param: value}, ids?, perPhoto?} — run the action's steps; a per-photo action (the default) runs once for each target photo with that photo alone selected, then restores the selection. Stops at the first failing step → {name, steps, photos, ran}",
                always,
                play
            )
        },
    ]
}
