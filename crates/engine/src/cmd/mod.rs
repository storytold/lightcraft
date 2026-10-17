//! The command registry.
//!
//! Command ids group by area: `library.*` (view/selection), `photo.*` (rating, flags, rotate,
//! delete), `album.*`, `develop.*` (settings), `crop.*`, `mask.*`, `preset.*`, `version.*`,
//! `edit.*` (undo/redo), and queries (`catalog.query`, `photo.inspect`, `develop.get`…).

pub mod actions;
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
pub mod crop_aspects;
mod cull;
pub mod curves;
pub(crate) mod denoise;
mod develop;
mod edit;
pub mod edit_in;
mod export;
pub(crate) mod face_detect;
mod face_models;
mod face_recognize;
pub mod filters;
mod focus;
pub mod folders;
pub mod geocode;
#[cfg(not(target_arch = "wasm32"))]
pub mod immich;
pub mod keywords;
mod lens_db;
pub mod library;
pub mod lut_profiles;
pub mod manage;
pub mod map;
pub mod mask_effect_files;
pub mod mask_effects;
mod masks;
mod merge;
pub mod metadata;
pub mod missing;
mod organize;
#[cfg(test)]
pub(crate) use organize::STACK_WORK;
pub(crate) mod plugin_filters;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugins;
/// The web build has no plug-ins: plug-in fields read from XMP have nowhere to go.
#[cfg(target_arch = "wasm32")]
pub mod plugins {
    /// No plug-ins folder on the web.
    pub fn plugins_dir() -> Option<std::path::PathBuf> {
        None
    }
    pub fn restore_fields(_s: &mut crate::Session, _items: &[(lcc_catalog::PhotoId, Vec<(String, String, String)>)]) -> usize {
        0
    }
}
mod prefs;
mod preset_files;
pub mod previews;
mod print;
#[cfg(not(target_arch = "wasm32"))]
pub mod publish;
mod query;
mod quick;
pub mod recheck;
mod slideshow;
mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub mod tether;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod video;
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
        let e = video_gate(s, self.id).and_then(|()| (self.enabled)(s));
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

/// Why a photo-only tool is disabled while a video clip is the active photo.
pub const NOT_FOR_VIDEO: &str = "Not available for video";

/// Commands that edit what a video can't take (plan §7: global grading only): crop and
/// geometry, masks, Remove / Heal spots, Red Eye and AI Denoise.
pub fn photo_only(id: &str) -> bool {
    ["mask.", "spot.", "redeye."].iter().any(|p| id.starts_with(p)) || matches!(id, "denoise.queue" | "denoise.toggle")
}

/// [`NOT_FOR_VIDEO`] for a [`photo_only`] command while the active photo is a video clip.
pub fn video_gate(s: &Session, id: &str) -> std::result::Result<(), String> {
    if photo_only(id) && s.active().is_some_and(|a| crate::video::adapter::is_video(s, a)) { Err(NOT_FOR_VIDEO.into()) } else { Ok(()) }
}

/// [`video_gate`] for one call: a [`photo_only`] command that names photos through `ids` is also
/// refused when any of them is a video clip, whatever the active photo is.
pub fn video_gate_call(s: &Session, id: &str, params: &serde_json::Value) -> std::result::Result<(), String> {
    video_gate(s, id)?;
    if !photo_only(id) {
        return Ok(());
    }
    let ids = params.get("ids").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_u64);
    match ids.map(crate::catalog::PhotoId).find(|p| crate::video::adapter::is_video(s, *p)) {
        Some(p) => Err(format!("{NOT_FOR_VIDEO} (photo {} is a video clip)", p.0)),
        None => Ok(()),
    }
}

pub fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
pub fn has_active(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no photo selected".into())
}
pub fn has_selection(s: &Session) -> std::result::Result<(), String> {
    if s.selection.ids.is_empty() && s.selection.active.is_none() { Err("no photos selected".into()) } else { Ok(()) }
}
/// Albums hold library photos: a selection of only browsed (Local) photos has nothing an album
/// can take (issue #684; Add to My Photos first).
pub fn has_library_selection(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.selected().iter().any(|id| s.catalog.photo(*id).is_none_or(|p| !p.local)) { Ok(()) } else { Err(LOCAL_ONLY.into()) }
}
/// Why an album command refused a Local-only selection.
pub const LOCAL_ONLY: &str = "only Local photos: albums hold library photos (Add to My Photos first)";
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
        v.extend(plugin_filters::specs());
        v.extend(curves::specs());
        v.extend(crop_aspects::specs());
        v.extend(masks::specs());
        v.extend(organize::specs());
        v.extend(keywords::specs());
        v.extend(manage::specs());
        v.extend(map::specs());
        v.extend(previews::specs());
        v.extend(lut_profiles::specs());
        v.extend(lens_db::specs());
        v.extend(mask_effects::specs());
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
        v.extend(slideshow::specs());
        v.extend(export::specs());
        v.extend(crate::creations::specs());
        v.extend(print::specs());
        v.extend(before::specs());
        v.extend(browse::specs());
        v.extend(folders::specs());
        v.extend(missing::specs());
        v.extend(recheck::specs());
        v.extend(sync::specs());
        v.extend(metadata::specs());
        v.extend(geocode::specs());
        v.extend(filters::specs());
        v.extend(denoise::specs());
        v.extend(face_models::specs());
        v.extend(face_detect::specs());
        v.extend(face_recognize::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(immich::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(publish::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(tether::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(web::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(credentials::specs());
        v.extend(actions::specs());
        v.extend(edit_in::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(plugins::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(update::specs());
        v.extend(focus::specs());
        v.extend(activity::specs());
        v.extend(video::specs());
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
/// A non-negative integer parameter: `index: -1` must be an error, not element 0 (`-1.0 as usize`
/// saturates to 0), just as `history.restore` already refuses a negative step index.
pub(crate) fn index_req(p: &Value, key: &str, cmd: &str) -> Result<usize> {
    let i = f64_req(p, key, cmd)?;
    if !(i.is_finite() && i >= 0.0) {
        return Err(bad(cmd, format!("`{key}` must be a non-negative integer")));
    }
    Ok(i as usize)
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
pub(crate) fn point(p: &Value, key: &str) -> Option<lcc_geom::Point> {
    let a = p.get(key)?.as_array()?;
    Some(lcc_geom::Point::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}
