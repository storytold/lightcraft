//! The command registry.
//!
//! Command ids group by area: `library.*` (view/selection), `photo.*` (rating, flags, rotate,
//! delete), `album.*`, `develop.*` (settings), `crop.*`, `mask.*`, `preset.*`, `version.*`,
//! `edit.*` (undo/redo), and queries (`catalog.query`, `photo.inspect`, `develop.get`…).

mod activity;
mod before;
pub(crate) mod browse;
#[cfg(not(target_arch = "wasm32"))]
pub mod catalog;
mod collections;
mod color;
pub(crate) mod convert;
#[cfg(not(target_arch = "wasm32"))]
pub mod credentials;
mod cull;
pub mod curves;
pub(crate) mod denoise;
mod develop;
mod edit;
mod export;
pub(crate) mod face_detect;
mod face_models;
mod face_recognize;
pub mod filters;
pub mod folders;
#[cfg(not(target_arch = "wasm32"))]
pub mod immich;
pub mod keywords;
pub mod library;
pub mod lut_profiles;
pub mod manage;
mod masks;
mod merge;
pub mod metadata;
pub mod missing;
mod organize;
mod prefs;
mod preset_files;
pub mod previews;
mod query;
mod quick;
mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub mod web;
mod xmp;

use serde::Serialize;
use serde_json::Value;

use crate::{EngineError, Result, Session};

pub type Run = fn(&mut Session, &Value) -> Result<Value>;
pub type Enabled = fn(&Session) -> std::result::Result<(), String>;

pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Menu placement (`["Photo", "Rotate"]`); empty = not in menus.
    pub menu: &'static [&'static str],
    /// Default shortcut (`Cmd+Shift+C`), mapped per platform by the UI.
    pub shortcut: Option<&'static str>,
    /// Human/agent-readable parameter description.
    pub params: &'static str,
    pub enabled: Enabled,
    pub run: Run,
    /// Record in the journal (false for queries).
    pub journal: bool,
    /// `enabled` judges the selection (menus, shortcuts); a call that names its photos (`ids` /
    /// `id`, see [`names_photos`]) skips it, and the command validates those photos itself.
    /// Off unless a command opts in (`CommandSpec { explicit_targets: true, ..cmd!(…) }`).
    pub explicit_targets: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: Vec<&'static str>,
    pub shortcut: Option<&'static str>,
    pub params: &'static str,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

impl CommandSpec {
    pub fn info(&self, s: &Session) -> CommandInfo {
        let e = (self.enabled)(s);
        CommandInfo {
            id: self.id,
            label: self.label,
            menu: self.menu.to_vec(),
            shortcut: self.shortcut,
            params: self.params,
            enabled: e.is_ok(),
            disabled_reason: e.err(),
        }
    }
}

// ---------- enablement predicates

pub fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
pub fn has_active(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no photo selected".into())
}
pub fn has_selection(s: &Session) -> std::result::Result<(), String> {
    if s.selection.ids.is_empty() && s.selection.active.is_none() { Err("no photos selected".into()) } else { Ok(()) }
}
pub fn can_undo(s: &Session) -> std::result::Result<(), String> {
    if s.undo.is_empty() { Err("nothing to undo".into()) } else { Ok(()) }
}
pub fn can_redo(s: &Session) -> std::result::Result<(), String> {
    if s.redo.is_empty() { Err("nothing to redo".into()) } else { Ok(()) }
}
pub fn has_clipboard(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.clipboard.is_none() { Err("no settings copied".into()) } else { Ok(()) }
}

macro_rules! cmd {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr, $params:literal, $en:expr, $run:expr) => {
        $crate::cmd::CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc, params: $params, enabled: $en, run: $run, journal: true, explicit_targets: false }
    };
    (query $id:literal, $label:literal, [$($m:literal),*], $sc:expr, $params:literal, $en:expr, $run:expr) => {
        $crate::cmd::CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc, params: $params, enabled: $en, run: $run, journal: false, explicit_targets: false }
    };
}
pub(crate) use cmd;

pub fn command_specs() -> &'static [CommandSpec] {
    static SPECS: std::sync::OnceLock<Vec<CommandSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| {
        let mut v = Vec::new();
        v.extend(edit::specs());
        v.extend(library::specs());
        v.extend(collections::specs());
        v.extend(develop::specs());
        v.extend(color::specs());
        v.extend(curves::specs());
        v.extend(masks::specs());
        v.extend(organize::specs());
        v.extend(keywords::specs());
        v.extend(manage::specs());
        v.extend(previews::specs());
        v.extend(lut_profiles::specs());
        v.extend(cull::specs());
        v.extend(convert::specs());
        v.extend(convert::edit_specs());
        v.extend(merge::specs());
        v.extend(query::specs());
        v.extend(quick::specs());
        v.extend(xmp::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(catalog::specs());
        v.extend(preset_files::specs());
        v.extend(prefs::specs());
        v.extend(export::specs());
        v.extend(before::specs());
        v.extend(browse::specs());
        v.extend(folders::specs());
        v.extend(missing::specs());
        v.extend(sync::specs());
        v.extend(metadata::specs());
        v.extend(filters::specs());
        v.extend(denoise::specs());
        v.extend(face_models::specs());
        v.extend(face_detect::specs());
        v.extend(face_recognize::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(immich::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(web::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(credentials::specs());
        v.extend(activity::specs());
        v
    })
}

pub fn find_command(id: &str) -> Option<&'static CommandSpec> {
    command_specs().iter().find(|c| c.id == id)
}

// ---------- param helpers

/// The call names its target photos (`ids` or `id`, not null) instead of using the selection.
pub(crate) fn names_photos(p: &Value) -> bool {
    ["ids", "id"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null()))
}

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
pub(crate) fn f64_req(p: &Value, key: &str, cmd: &str) -> Result<f64> {
    p.get(key).and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("missing number `{key}`")))
}
pub(crate) fn f64_or(p: &Value, key: &str, d: f64) -> f64 {
    p.get(key).and_then(Value::as_f64).unwrap_or(d)
}
pub(crate) fn str_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}
pub(crate) fn bool_or(p: &Value, key: &str, d: bool) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(d)
}
pub(crate) fn ok() -> Result<Value> {
    Ok(Value::Null)
}
pub(crate) fn point(p: &Value, key: &str) -> Option<dac_geom::Point> {
    let a = p.get(key)?.as_array()?;
    Some(dac_geom::Point::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}
