//! The command registry.
//!
//! Command ids group by area: `library.*` (view/selection), `photo.*` (rating, flags, rotate,
//! delete), `album.*`, `develop.*` (settings), `crop.*`, `mask.*`, `preset.*`, `version.*`,
//! `edit.*` (undo/redo), and queries (`catalog.query`, `photo.inspect`, `develop.get`…).

pub mod actions;
mod activity;
pub mod alternatives;
mod before;
pub(crate) mod browse;
#[cfg(not(target_arch = "wasm32"))]
pub mod catalog;
pub mod chart_profile;
mod collections;
mod color;
mod concepts;
pub(crate) mod convert;
#[cfg(not(target_arch = "wasm32"))]
pub mod credentials;
pub mod crop_aspects;
mod cull;
pub mod curves;
pub mod dcp_profiles;
pub(crate) mod denoise;
pub(crate) mod develop;
#[cfg(not(target_arch = "wasm32"))]
pub mod device_sync;
pub mod duplicates;
mod edit;
pub mod edit_in;
mod enhance;
mod export;
pub mod export_preset_files;
pub(crate) mod face_detect;
mod face_models;
mod face_recognize;
pub(crate) mod facets;
pub mod film_dust;
pub mod filters;
mod focus;
pub mod folder_metadata;
pub mod folders;
mod genfill;
pub mod geocode;
#[cfg(not(target_arch = "wasm32"))]
pub mod immich;
pub mod import_tab;
pub mod keywords;
mod lens_db;
pub mod levels;
pub mod library;
pub mod lut_profiles;
pub mod manage;
pub mod map;
pub mod mask_effect_files;
pub mod mask_effects;
mod masks;
mod merge;
pub mod metadata;
pub mod metadata_files;
pub mod missing;
mod ml;
pub(crate) mod organize;
mod people_recognize;
mod people_roles;
pub mod profile_manager;
pub(crate) mod retouch;
pub mod vignetting_calibration;
#[cfg(test)]
pub(crate) use organize::STACK_WORK;
#[cfg(not(target_arch = "wasm32"))]
pub mod marketplace;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod plugin_develop;
pub(crate) mod plugin_filters;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugin_jobs;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod plugin_library;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugin_programs;
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
pub mod read_originals;
pub mod recheck;
pub mod reload;
pub mod sensor;
mod slideshow;
pub(crate) mod source;
mod store;
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
/// A selection of photos that are there: one whose photos an undo took away (they stay selected
/// for the redo) is none, as [`Session::targets`] would be empty. What counts is what a command acts on
/// ([`Session::selected`]): the active photo only stands in when nothing else is selected.
pub fn has_selection(s: &Session) -> std::result::Result<(), String> {
    // (the rule of `Session::selected`, without making its list: this is asked for every command, every frame)
    let there = |id: &lcc_catalog::PhotoId| s.catalog.photo(*id).is_some();
    let any = if s.selection.ids.is_empty() { s.selection.active.iter().any(there) } else { s.selection.ids.iter().any(there) };
    if any { Ok(()) } else { Err("no photos selected".into()) }
}
/// Albums hold library photos: a selection of only browsed (Local) photos has nothing an album
/// can take (issue #684; Add to My Photos first).
pub fn has_library_selection(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.selected().iter().any(|id| s.catalog.photo(*id).is_none_or(|p| !p.local)) { Ok(()) } else { Err(LOCAL_ONLY.into()) }
}
/// Why an album command refused a Local-only selection.
pub const LOCAL_ONLY: &str = "only Local photos: albums hold catalog photos (Add to My Photos first)";
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
pub use masks::SPOT_SIZE;

pub fn command_specs() -> &'static [CommandSpec] {
    static SPECS: std::sync::OnceLock<Vec<CommandSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| {
        let mut v = Vec::new();
        v.extend(edit::specs());
        v.extend(library::specs());
        v.extend(alternatives::specs());
        v.extend(collections::specs());
        v.extend(develop::specs());
        v.extend(color::specs());
        v.extend(plugin_filters::specs());
        v.extend(curves::specs());
        v.extend(levels::specs());
        v.extend(crop_aspects::specs());
        v.extend(masks::specs());
        v.extend(retouch::specs());
        v.extend(retouch::retouch_specs());
        v.extend(genfill::specs());
        v.extend(concepts::specs());
        v.extend(people_roles::specs());
        v.extend(people_recognize::specs());
        v.extend(organize::specs());
        v.extend(keywords::specs());
        v.extend(manage::specs());
        v.extend(map::specs());
        v.extend(previews::specs());
        v.extend(lut_profiles::specs());
        v.extend(vignetting_calibration::specs());
        v.extend(lens_db::specs());
        v.extend(source::specs());
        v.extend(chart_profile::specs());
        v.extend(dcp_profiles::specs());
        v.extend(profile_manager::specs());
        v.extend(mask_effects::specs());
        v.extend(crate::machine_test::specs());
        v.extend(cull::specs());
        v.extend(crate::quality_scan::specs());
        v.extend(convert::specs());
        v.extend(convert::edit_specs());
        v.extend(merge::specs());
        v.extend(enhance::specs());
        v.extend(query::specs());
        v.extend(facets::specs());
        v.extend(quick::specs());
        v.extend(xmp::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(catalog::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(device_sync::specs());
        v.extend(preset_files::specs());
        v.extend(prefs::specs());
        v.extend(slideshow::specs());
        v.extend(export::specs());
        v.extend(crate::creations::specs());
        v.extend(print::specs());
        v.extend(before::specs());
        v.extend(browse::specs());
        v.extend(import_tab::specs());
        v.extend(folders::specs());
        v.extend(missing::specs());
        v.extend(duplicates::specs());
        v.extend(read_originals::specs());
        v.extend(recheck::specs());
        v.extend(sync::specs());
        v.extend(store::specs());
        v.extend(metadata::specs());
        v.extend(metadata_files::specs());
        v.extend(export_preset_files::specs());
        v.extend(geocode::specs());
        v.extend(filters::specs());
        v.extend(denoise::specs());
        v.extend(sensor::specs());
        v.extend(film_dust::specs());
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
        v.extend(marketplace::specs());
        #[cfg(not(target_arch = "wasm32"))]
        v.extend(update::specs());
        v.extend(focus::specs());
        v.extend(ml::specs());
        v.extend(folder_metadata::specs());
        v.extend(activity::specs());
        v.extend(video::specs());
        v
    })
}

pub fn find_command(id: &str) -> Option<&'static CommandSpec> {
    let id = canonical_id(id);
    command_specs().iter().find(|c| c.id == id.as_ref())
}

/// Collection command ids, the user-facing names: `collection.*` for `album.*`, `collections.list`
/// for `albums.list` and `dialog.newCollection`, `dialog.smartCollection`,
/// `dialog.newSmartCollection` for the dialogs' `…Album` ids. The UI calls them Collections (as
/// Lightroom Classic does) while the catalog, the journal and saved scripts keep the album ids, so
/// both spellings run the same command and old scripts, keymaps and journals keep working. Any
/// other id is returned as it is.
pub fn canonical_id(id: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if let Some(rest) = id.strip_prefix("collection.") {
        return Cow::Owned(format!("album.{rest}"));
    }
    match id {
        "collections.list" => Cow::Borrowed("albums.list"),
        "dialog.newCollection" => Cow::Borrowed("dialog.newAlbum"),
        "dialog.smartCollection" => Cow::Borrowed("dialog.smartAlbum"),
        "dialog.newSmartCollection" => Cow::Borrowed("dialog.newSmartAlbum"),
        _ => Cow::Borrowed(id),
    }
}

// ---------- param helpers

/// The call names its target photos (`ids` or `id`, not null) instead of using the selection.
pub(crate) fn names_photos(p: &Value) -> bool {
    ["ids", "id"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null()))
}

/// The photos `ids` names: `None` when it is missing or null (the command then falls back to the
/// selection, or its own default), else every entry as a photo id. An `ids` that isn't a list of
/// whole numbers is an error, never `None`: falling back would act on the selection instead of
/// the photos the call meant (`{"ids": 3}`, `{"ids": ["3"]}`).
pub(crate) fn photo_ids(p: &Value, cmd: &str) -> Result<Option<Vec<lcc_catalog::PhotoId>>> {
    match p.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().map(lcc_catalog::PhotoId).ok_or_else(|| bad(cmd, "`ids` must be photo ids (whole numbers)")))
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, "`ids` must be a list of photo ids")),
    }
}

/// The photo `id` names: `None` when it is missing or null (the command then uses the active
/// photo), else that id. An `id` that isn't a whole number is an error, never `None`: falling back
/// would act on the active photo instead of the one the call meant (`{"id": "5"}`).
pub(crate) fn photo_id(p: &Value, cmd: &str) -> Result<Option<lcc_catalog::PhotoId>> {
    match p.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(lcc_catalog::PhotoId(n))).ok_or_else(|| bad(cmd, "`id` must be a photo id (a whole number)")),
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
    if p.get(key).is_none_or(Value::is_null) {
        return Err(bad(cmd, format!("missing number `{key}`")));
    }
    index_opt(p, key, cmd).map(|i| i.unwrap_or_default())
}
/// Like [`index_req`], but absent / null is `None`; a supplied value that is not a whole, non-negative number
/// that fits `usize` (`0.9`, `-1`, `"3"`, `1e30`) is an error, never a default or a truncation. `2.0` is valid.
pub(crate) fn index_opt(p: &Value, key: &str, cmd: &str) -> Result<Option<usize>> {
    let v = match p.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let err = || bad(cmd, format!("`{key}` must be a non-negative integer, got {v}"));
    if let Some(n) = v.as_u64() {
        return usize::try_from(n).map(Some).map_err(|_| err());
    }
    match v.as_f64() {
        // 2^53 is the largest range where every integer is exactly representable
        Some(f) if v.is_f64() && f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= 9_007_199_254_740_992.0 => {
            usize::try_from(f as u64).map(Some).map_err(|_| err())
        }
        _ => Err(err()),
    }
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

/// A batch-edit recipe from a command's params: `[{id, value}, …]`, each `id` a develop control a
/// recipe may name ([`check_recipe`]). Empty when absent or null. A malformed item, a control a
/// recipe may not name, or more entries than there are controls is an error, so a typo is refused
/// rather than silently ignored. An id named twice counts once, the last value winning; the list
/// comes back in id order.
pub(crate) fn recipe_param(p: &Value, cmd: &str) -> Result<Vec<(String, f64)>> {
    let Some(v) = p.get("recipe").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let Some(items) = v.as_array() else {
        return Err(bad(cmd, "`recipe` must be a list of {id, value}"));
    };
    let most = lcc_develop::controls::CONTROLS.len();
    if items.len() > most {
        return Err(bad(cmd, format!("a recipe has one entry per control, at most {most} (it has {})", items.len())));
    }
    let mut by_id = std::collections::BTreeMap::new();
    for it in items {
        let id = it.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| bad(cmd, "each recipe item needs an `id`"))?;
        let value = it.get("value").and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("recipe `{id}` needs a numeric `value`")))?;
        by_id.insert(id.to_string(), value);
    }
    let recipe: Vec<(String, f64)> = by_id.into_iter().collect();
    check_recipe(&recipe).map_err(|e| bad(cmd, e))?;
    Ok(recipe)
}

/// Whether every control of `recipe` is one a batch-edit recipe may name, else why not. The rule is
/// on the control's spec: a plain control of the shared list, writable on any photo's settings
/// (`set` takes it), outside the Geometry section and not white balance.
///
/// - Per-photo instances (`pointColor.0.hueShift`, `redEye.0.pupilSize`) are not in the list; on a
///   photo without that instance a write is dropped.
/// - Geometry controls (`crop.angle`, `geometry.*`) need the photo's own size: the slider refits the
///   crop around a new angle, which a batch write cannot do.
/// - White balance means something else on each kind of file (Kelvin on a raw with matrices,
///   relative to the as-shot white on a rendered photo or a raw without them), so one temperature
///   cannot name the same look for every photo of a batch.
pub(crate) fn check_recipe(recipe: &[(String, f64)]) -> std::result::Result<(), String> {
    use lcc_develop::{Section, controls};
    let mut scratch = lcc_develop::DevelopSettings::default();
    for (id, value) in recipe {
        let Some(spec) = controls::CONTROLS.iter().find(|c| c.id == id.as_str()) else {
            return Err(format!(
                "unknown recipe control `{id}` (see develop.controls; a control that belongs to one photo, such as pointColor.0.hueShift, is not one)"
            ));
        };
        if spec.section == Section::Geometry {
            return Err(format!("`{id}` is a geometry control: a recipe does not crop, straighten or transform photos"));
        }
        if id.starts_with("wb.") {
            return Err(format!(
                "`{id}` is white balance: a recipe does not set it, because a temperature means something else on a raw than on a rendered photo"
            ));
        }
        if !controls::set(&mut scratch, id, *value) {
            return Err(format!("recipe control `{id}` cannot be set"));
        }
    }
    Ok(())
}
pub(crate) fn point(p: &Value, key: &str) -> Option<lcc_geom::Point> {
    let v = p.get(key)?;
    // `[x, y]`, or `{x, y}` as a Point serializes (the app sends those)
    let (x, y) = match v.as_array() {
        Some(a) => (a.first()?.as_f64()?, a.get(1)?.as_f64()?),
        None => (v.get("x")?.as_f64()?, v.get("y")?.as_f64()?),
    };
    Some(lcc_geom::Point::new(x, y))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::index_req;

    /// OneSlips ea89acca3: a point reads as `[x, y]` and as `{x, y}` (how a Point serializes), so a mask drawn with
    /// either lands where it was drawn, not at the default place.
    #[test]
    fn points_read_as_a_list_or_as_x_and_y() {
        let at = |v: serde_json::Value| super::point(&json!({"center": v}), "center").map(|p| (p.x, p.y));
        assert_eq!(at(json!([0.2, 0.7])), Some((0.2, 0.7)));
        assert_eq!(at(json!({"x": 0.2, "y": 0.7})), Some((0.2, 0.7)));
        for bad in [json!({"x": 0.2}), json!([0.2]), json!("0.2,0.7"), json!({"x": "a", "y": 1})] {
            assert_eq!(at(bad.clone()), None, "{bad}");
        }
        let mut s = crate::Session::with_demo();
        s.execute("mask.add", &json!({"kind": "radial", "center": {"x": 0.2, "y": 0.7}})).unwrap();
        let id = s.active().unwrap();
        let d = s.develop_of(id).unwrap();
        let shape = &d.masks.last().unwrap().components[0].shape;
        let lcc_develop::MaskShape::Radial { center, .. } = shape else { panic!("a radial: {shape:?}") };
        assert_eq!((center.x, center.y), (0.2, 0.7), "where it was drawn");
    }

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
