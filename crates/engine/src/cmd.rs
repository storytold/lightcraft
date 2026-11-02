//! The command registry.
//!
//! Command ids group by area: `library.*` (view/selection), `photo.*` (rating, flags, rotate,
//! delete), `album.*`, `develop.*` (settings), `crop.*`, `mask.*`, `preset.*`, `version.*`,
//! `edit.*` (undo/redo), and queries (`catalog.query`, `photo.inspect`, `develop.get`…).

mod activity;
mod before;
pub(crate) mod browse;
mod color;
pub(crate) mod convert;
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
mod focus;
pub mod geocode;
pub mod keywords;
pub mod library;
pub mod lut_profiles;
pub mod manage;
pub mod mask_effect_files;
pub mod mask_effects;
mod masks;
mod merge;
pub mod metadata;
pub mod missing;
mod organize;
mod prefs;
mod preset_files;
pub mod previews;
mod print;
mod query;
pub mod recheck;
pub mod reload;
mod slideshow;
mod sync;
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
        v.extend(develop::specs());
        v.extend(color::specs());
        v.extend(curves::specs());
        v.extend(masks::specs());
        v.extend(organize::specs());
        v.extend(keywords::specs());
        v.extend(manage::specs());
        v.extend(previews::specs());
        v.extend(lut_profiles::specs());
        v.extend(mask_effects::specs());
        v.extend(cull::specs());
        v.extend(convert::specs());
        v.extend(convert::edit_specs());
        v.extend(merge::specs());
        v.extend(query::specs());
        v.extend(xmp::specs());
        v.extend(preset_files::specs());
        v.extend(prefs::specs());
        v.extend(slideshow::specs());
        v.extend(export::specs());
        v.extend(print::specs());
        v.extend(before::specs());
        v.extend(browse::specs());
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
        v.extend(focus::specs());
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

/// The call names its target photos in `ids`, whatever else it carries in `id` (`album.addPhotos`'s `id` is the album).
pub(crate) fn names_photo_ids(p: &Value) -> bool {
    p.get("ids").is_some_and(|v| !v.is_null())
}

/// For a command that opts into `explicit_targets` but whose `id` isn't a photo: [`Session::execute`] skipped the
/// selection check because the call carries an `id`, so the command makes it itself unless `ids` names the photos.
pub(crate) fn enabled_unless_ids(s: &Session, p: &Value, id: &str, enabled: Enabled) -> Result<()> {
    if names_photo_ids(p) {
        return Ok(());
    }
    enabled(s).map_err(|why| EngineError::Disabled(id.to_string(), why))
}

/// The photos `ids` names: `None` when it is missing or null (the command then falls back to the
/// selection, or its own default), else every entry as a photo id. An `ids` that isn't a list of
/// whole numbers is an error, never `None`: falling back would act on the selection instead of
/// the photos the call meant (`{"ids": 3}`, `{"ids": ["3"]}`).
pub(crate) fn photo_ids(p: &Value, cmd: &str) -> Result<Option<Vec<lightcraft_catalog::PhotoId>>> {
    match p.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().map(lightcraft_catalog::PhotoId).ok_or_else(|| bad(cmd, "`ids` must be photo ids (whole numbers)")))
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, "`ids` must be a list of photo ids")),
    }
}

/// The photo `id` names: `None` when it is missing or null (the command then uses the active
/// photo), else that id. An `id` that isn't a whole number is an error, never `None`: falling back
/// would act on the active photo instead of the one the call meant (`{"id": "5"}`).
pub(crate) fn photo_id(p: &Value, cmd: &str) -> Result<Option<lightcraft_catalog::PhotoId>> {
    match p.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(lightcraft_catalog::PhotoId(n))).ok_or_else(|| bad(cmd, "`id` must be a photo id (a whole number)")),
    }
}

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
pub(crate) fn f64_req(p: &Value, key: &str, cmd: &str) -> Result<f64> {
    p.get(key).and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("missing number `{key}`")))
}
/// A non-negative whole number that fits `usize`. Keep integer JSON exact; accept integral
/// floating-point values for compatibility, without truncating or saturating their conversion.
pub(crate) fn index_req(p: &Value, key: &str, cmd: &str) -> Result<usize> {
    let invalid = || bad(cmd, format!("`{key}` must be a non-negative integer"));
    if let Some(i) = p.get(key).and_then(Value::as_u64) {
        return usize::try_from(i).map_err(|_| invalid());
    }
    let i = f64_req(p, key, cmd)?;
    // The exclusive bound is exactly representable, unlike usize::MAX on 64-bit targets.
    let upper_bound = 2.0_f64.powi(usize::BITS as i32);
    if !(i.is_finite() && i >= 0.0 && i.fract() == 0.0 && i < upper_bound) {
        return Err(invalid());
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
pub(crate) fn point(p: &Value, key: &str) -> Option<lightcraft_geom::Point> {
    let a = p.get(key)?.as_array()?;
    Some(lightcraft_geom::Point::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::index_req;

    #[test]
    fn command_indices_preserve_integer_precision_and_check_usize_bounds() {
        for i in [0, 1, usize::MAX] {
            assert_eq!(index_req(&json!({"index": i}), "index", "test").unwrap(), i);
        }
        #[cfg(target_pointer_width = "64")]
        {
            let i = 9_007_199_254_740_993usize;
            assert_eq!(index_req(&json!({"index": i}), "index", "test").unwrap(), i, "integer JSON must not round through f64");
        }
        let upper_bound = 2.0_f64.powi(usize::BITS as i32);
        assert!(index_req(&json!({"index": upper_bound}), "index", "test").is_err(), "2^usize::BITS must not saturate");
        #[cfg(target_pointer_width = "32")]
        {
            assert!(index_req(&json!({"index": u64::from(u32::MAX) + 1}), "index", "test").is_err());
            assert_eq!(index_req(&json!({"index": f64::from(u32::MAX)}), "index", "test").unwrap(), usize::MAX);
        }
    }
}
