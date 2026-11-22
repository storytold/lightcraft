//! Develop commands. All operate on the active photo unless `ids` are given (sync).

use lcc_catalog::{Op, PhotoId, Version};
use lcc_develop::{DevelopSettings, Preset, ProcessVersion, Section, SettingsGroup, SettingsPick, Treatment, Upright, WbMode, controls};
use lcc_geom::{CropGeometry, CropHandle, MIN_CROP_FRACTION, Point, Rect, crop_fit_angle, drag_crop};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_or, f64_req, has_active, has_clipboard, has_selection, index_req, ok, str_param};
use crate::{Result, Session, media::SourceLevel};

fn active(s: &Session, c: &str) -> Result<PhotoId> {
    s.active().ok_or_else(|| bad(c, "no active photo"))
}

/// Modify the active photo's settings with `f` and commit (or preview during an interaction).
/// Auto's tone and colour numbers onto a photo's settings.
fn set_auto_tone(d: &mut DevelopSettings, a: &lcc_pipeline::auto::AutoTone) {
    d.light.exposure = a.exposure;
    d.light.contrast = a.contrast;
    d.light.highlights = a.highlights;
    d.light.shadows = a.shadows;
    d.light.whites = a.whites;
    d.light.blacks = a.blacks;
    d.color.vibrance = a.vibrance;
    d.color.saturation = a.saturation;
}

fn edit(s: &mut Session, c: &str, label: &str, f: impl FnOnce(&mut DevelopSettings) -> Result<()>) -> Result<Value> {
    let id = active(s, c)?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    f(&mut d)?;
    s.set_develop(id, d, label)?;
    ok()
}

/// `develop.profile` with a camera profile (OneSlips, LR-PROF-LAYERS `dcp_profiles::choose`): it becomes the photo's
/// camera profile (`profile.camera`). One with a look of its own (tone curve, look table) comes with that look, "As
/// profile": the look goes back to the default, so no look of the app's is stacked on it. One without keeps the look.
fn choose_camera_profile(s: &mut Session, id: &str) -> Result<Value> {
    const C: &str = "develop.profile";
    let profile = crate::camera_profiles::choice_profile(id);
    if id != "lc" && profile.is_none() {
        return Err(bad(C, format!("unknown camera profile `{id}`")));
    }
    let own_look = profile.is_some_and(|d| d.has_look());
    edit(s, C, "Profile", |d| {
        d.profile.camera = id.to_string();
        if own_look {
            d.profile.id = lcc_develop::DEFAULT_LOOK.to_string();
        }
        Ok(())
    })
}

/// `d` (photo with source `info`, crop frame `dims`) with control `ctl` moved by `delta` from its
/// own value: white balance on the photo's own scale (As Shot read as the file's white first),
/// clamped to the control's range. Quick Develop and the live multi-photo edit use it, so every
/// photo moves by the same amount whatever its starting point.
fn nudge(info: &lcc_pipeline::SourceInfo, dims: (f64, f64), d: &DevelopSettings, ctl: &str, delta: f64, default: f64) -> DevelopSettings {
    let mut nd = d.clone();
    let wb = ctl == "wb.temp" || ctl == "wb.tint";
    if wb {
        lcc_pipeline::local::normalize_wb(&mut nd, info);
        if nd.wb.mode == WbMode::AsShot {
            nd.wb.temp = info.as_shot_temp;
            nd.wb.tint = info.as_shot_tint;
        }
    }
    let before = nd.crop.geometry;
    let v = controls::get(&nd, ctl).unwrap_or(default);
    controls::set(&mut nd, ctl, v + delta);
    refit_crop_angle(ctl == CROP_ANGLE, before, &mut nd, dims);
    if wb {
        nd.wb.mode = WbMode::Custom;
    }
    nd
}

fn section_param(p: &Value, c: &str) -> Result<Section> {
    let v = p.get("section").cloned().ok_or_else(|| bad(c, "missing `section`"))?;
    serde_json::from_value(v).map_err(|e| bad(c, e.to_string()))
}

pub(super) fn groups_param(p: &Value, c: &str) -> Result<Option<Vec<SettingsGroup>>> {
    match p.get("groups") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|e| bad(c, format!("invalid `groups`: {e}"))),
    }
}

/// `groups` (whole groups) and `settings` (single settings of a group: `light.exposure`,
/// `detail.sharpening`…; `develop.settingItems` lists them) as one pick; `None` when neither is given.
fn pick_param(p: &Value, c: &str) -> Result<Option<SettingsPick>> {
    let groups = groups_param(p, c)?;
    let settings: Option<Vec<String>> = match p.get("settings") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| bad(c, format!("invalid `settings`: {e}")))?),
    };
    if groups.is_none() && settings.is_none() {
        return Ok(None);
    }
    let settings = settings.unwrap_or_default();
    if let Some(unknown) = settings.iter().find(|id| lcc_develop::presets::find_item(id).is_none()) {
        return Err(bad(c, format!("unknown setting `{unknown}` (see develop.settingItems)")));
    }
    Ok(Some(SettingsPick { groups: groups.unwrap_or_default(), settings }.normalized()))
}

/// `masks` (Copy: which of the photo's masks, by id); `None` when not given.
fn mask_ids_param(p: &Value, c: &str) -> Result<Option<Vec<u32>>> {
    match p.get("masks") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad(c, format!("invalid mask id `{v}` (develop.get: masks[].id)"))))
            .collect::<Result<Vec<u32>>>()
            .map(Some),
        Some(_) => Err(bad(c, "`masks` must be a list of mask ids (develop.get: masks[].id)")),
    }
}

/// A preset group named like `name` (trimmed, any case) that exists already, else `name` trimmed:
/// typing "wedding " never makes a second "Wedding" group.
pub(crate) fn existing_preset_group(s: &Session, name: &str) -> String {
    let name = name.trim();
    let same = |g: &str| g.trim().eq_ignore_ascii_case(name);
    s.presets
        .iter()
        .map(|p| p.group.as_str())
        .chain(s.preset_folders.iter().map(String::as_str))
        .find(|g| same(g))
        .map(|g| g.trim().to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Remember the dialogs' choices (saved with the store or the library; a failed save is logged,
/// the command itself went through).
fn remember_dialogs(s: &mut Session) {
    if let Err(e) = s.save_prefs() {
        log::error!("dialogs: remembering the choices: {e}");
    }
}

/// The leaves of `a` that differ from `b` (same shape), as a partial JSON object: `None` when they
/// are equal. Used to store only what a batch-edit recipe names, so applying the saved preset
/// later never overrides the settings the recipe doesn't name.
fn partial_diff(a: &Value, b: &Value) -> Option<Value> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut out = serde_json::Map::new();
            for (k, v) in x {
                if let Some(d) = partial_diff(v, y.get(k).unwrap_or(&Value::Null)) {
                    out.insert(k.clone(), d);
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        (x, y) => (x != y).then(|| x.clone()),
    }
}

/// The partial settings a batch-edit recipe names, with the values the import would give (clamped
/// by the control specs). Each named control is compared with the same control at a value it is not
/// set to, so a control the recipe sets to its default (a reset) is stored too, and nothing else is.
/// `None` when the recipe names nothing that is stored, or a control that cannot be written.
fn recipe_partial(recipe: &[(String, f64)]) -> Option<Value> {
    let (mut named, mut other) = (DevelopSettings::default(), DevelopSettings::default());
    for (id, v) in recipe {
        if !controls::set(&mut named, id, *v) {
            return None;
        }
    }
    for (id, _) in recipe {
        let (spec, now) = (controls::find(id)?, controls::get(&named, id)?);
        if !controls::set(&mut other, id, if now > spec.min { spec.min } else { spec.max }) {
            return None;
        }
    }
    partial_diff(&named.to_json(), &other.to_json())
}

/// The longest name or group a saved recipe preset may have (characters).
const MAX_PRESET_TEXT: usize = 100;

pub(crate) fn crop_dims(s: &Session, id: PhotoId) -> (f64, f64) {
    let p = s.catalog.photo(id);
    let (w, h) = p.map(|p| (p.width.max(1) as f64, p.height.max(1) as f64)).unwrap_or((3.0, 2.0));
    let o = p.map(|p| p.develop.orientation).unwrap_or_default();
    if o.swaps_axes() { (h, w) } else { (w, h) }
}

/// Photo `id`'s frame as oriented with `orientation`; `None` when the catalog doesn't know its size.
fn oriented_dims(s: &Session, id: PhotoId, orientation: lcc_geom::Orientation) -> Option<(f64, f64)> {
    let p = s.catalog.photo(id)?;
    let (w, h) = (p.width as f64, p.height as f64);
    (w > 0.0 && h > 0.0).then_some(if orientation.swaps_axes() { (h, w) } else { (w, h) })
}

/// Paste, Paste from Previous and Sync Settings: when `partial` carries a crop, photo `id`'s pasted settings take
/// the crop `src` (made on a frame `from`) refitted to `id`'s frame as the paste orients it (the Crop group carries
/// the orientation), as Auto Sync does ([`crate::sync_same::synced_crop`], upstream #3148). Without both frames the
/// crop goes on as it is.
fn refit_pasted_crop(s: &Session, partial: &Value, src: &lcc_develop::Crop, from: Option<(f64, f64)>, id: PhotoId, pasted: &mut DevelopSettings) {
    if partial.get("crop").is_none() {
        return;
    }
    if let (Some(from), Some(to)) = (from, oriented_dims(s, id, pasted.orientation)) {
        pasted.crop = crate::sync_same::synced_crop(src, from, to);
    }
}

/// Sets photo `id`'s crop as `crop.set` does (`c` names the command in errors): `rect` `[x0, y0,
/// x1, y1]` normalized in the straightened frame (any order), `angle` in degrees (clamped to ±45°).
/// A new angle alone turns the crop as Straighten does; a rect is fitted inside the photo. One
/// History step ("Crop"). Returns the crop as set.
pub(crate) fn set_crop_of(s: &mut Session, id: PhotoId, c: &str, rect: Option<[f64; 4]>, angle: Option<f64>) -> Result<CropGeometry> {
    if s.catalog.photo(id).is_none() {
        return Err(bad(c, format!("no photo {}", id.0)));
    }
    let (w, h) = crop_dims(s, id);
    // the smallest crop a drag of the crop tool can make: below it an export comes out 1×1 (#1952)
    let min_side = MIN_CROP_FRACTION * w.min(h) * (1.0 - 1e-9);
    let too_small = |r: Rect| {
        (r.width() * w < min_side || r.height() * h < min_side)
            .then(|| bad(c, format!("rect is smaller than the smallest crop ({} % of the shorter side)", MIN_CROP_FRACTION * 100.0)))
    };
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    if let Some([x0, y0, x1, y1]) = rect {
        let r = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        if let Some(e) = too_small(r) {
            return Err(e);
        }
        d.crop.geometry.rect = r;
    }
    let angle = angle.map(|a| a.clamp(-45.0, 45.0));
    d.crop.geometry = match (rect, angle) {
        // a new angle alone turns the crop as Straighten does (issue #742)
        (None, Some(a)) => d.crop.geometry.with_angle(a, w, h),
        (_, a) => CropGeometry { angle: a.unwrap_or(d.crop.geometry.angle), ..d.crop.geometry }.constrained(w, h),
    };
    // a rect far larger than the photo is shrunk to fit it, and can come out a sliver
    if rect.is_some()
        && let Some(e) = too_small(d.crop.geometry.rect)
    {
        return Err(e);
    }
    let geometry = d.crop.geometry;
    s.set_develop(id, d, "Crop")?;
    Ok(geometry)
}

/// After a generic control write (`develop.set`, `develop.adjust`, …) that touched `crop.angle`:
/// refit the crop as `crop.straighten` does, from the crop it had `before`, so the box never takes in
/// canvas outside the photo (issue #742).
fn refit_crop_angle(touched: bool, before: CropGeometry, d: &mut DevelopSettings, (w, h): (f64, f64)) {
    if touched {
        d.crop.geometry = before.with_angle(d.crop.geometry.angle, w, h);
    }
}

const CROP_ANGLE: &str = "crop.angle";

/// Whether a white balance mode renders its own white instead of the numbers stored on the photo: `As Shot`
/// renders the source's as-shot white, and on a raw edited in Kelvin a named preset renders its own Kelvin / tint
/// ([`WbMode::preset`]). Either way a number written into `temp` / `tint` is stored but read by nothing, so the
/// panel shows the number while the render uses the mode's white. `Custom` reads its stored numbers, and so does
/// `Auto` on such a raw: its white is resolved and stored when the mode is set (`develop.wb`), not at render time.
fn wb_mode_ignores_its_numbers(mode: WbMode, info: &lcc_pipeline::SourceInfo) -> bool {
    mode == WbMode::AsShot || (info.raw && !info.relative_wb && mode.preset().is_some())
}

/// Whether `partial`'s white balance names one of the two numbers, and not both or neither: the only partial
/// [`prepare_photo_for_wb_partial`] prepares a photo for (so callers can skip reading the source's facts otherwise).
pub(crate) fn wb_partial_names_one_number(partial: &Value) -> bool {
    partial.get("wb").and_then(Value::as_object).is_some_and(|wb| wb.contains_key("temp") != wb.contains_key("tint"))
}

/// Bring a photo's settings onto the scale and mode a white balance partial names, so a one-field partial means
/// what it says (issue #2910, and its review). `None` when the partial needs no such preparation: it names both
/// numbers, neither, or a mode of its own. [`merge_settings_for_photo`] and `preset.apply` both start here, and
/// both finish with `normalize_wb` so the photo ends on the scale it is edited on.
pub(crate) fn prepare_photo_for_wb_partial(d: &DevelopSettings, partial: &Value, info: &lcc_pipeline::SourceInfo) -> Option<DevelopSettings> {
    if !wb_partial_names_one_number(partial) {
        return None;
    }
    let mut d = (*d).clone();
    let wb = partial.get("wb").and_then(Value::as_object);
    // a photo whose source is edited in Kelvin is the one with two scales to trip over
    let kelvin_source = info.raw && !info.relative_wb;
    // the scale the partial's numbers are written on: an unmentioned scale means the relative scale, as `merged` reads it
    let partial_scale = match wb.as_ref().and_then(|wb| wb.get("scale")).and_then(Value::as_str) {
        Some("kelvin") => lcc_develop::WbScale::Kelvin,
        _ => lcc_develop::WbScale::Legacy,
    };
    // put the photo onto the scale it is edited on: its values marked Kelvin, or a look still on the relative
    // scale converted to the Kelvin / tint that renders the same
    lcc_pipeline::local::normalize_wb(&mut d, info);
    // a mode that answers its own white would swallow the partial's number: write the white the photo really
    // renders as Custom values, so the number means what it says and the other field carries that white's look
    if !wb.is_some_and(|wb| wb.contains_key("mode")) && wb_mode_ignores_its_numbers(d.wb.mode, info) {
        let (t, tint) = lcc_pipeline::local::effective_wb(info, &d);
        (d.wb.mode, d.wb.temp, d.wb.tint) = (WbMode::Custom, t, tint);
    }
    if kelvin_source && partial_scale == lcc_develop::WbScale::Legacy {
        // and onto the partial's scale, so the field the partial leaves out is the photo's own look read there
        let as_shot = (info.as_shot_temp, info.as_shot_tint);
        let (kt, ktint) = lcc_pipeline::local::effective_wb(info, &d);
        if let Some((lt, ltint)) = lcc_pipeline::local::kelvin_to_legacy(kt, ktint, as_shot) {
            (d.wb.mode, d.wb.temp, d.wb.tint, d.wb.scale) = (WbMode::Custom, lt, ltint, lcc_develop::WbScale::Legacy);
        }
    }
    Some(d)
}

/// Merge a partial settings JSON into a photo's settings (issue #2910). `DevelopSettings::merged` keeps a white
/// balance that names no scale on the relative scale; on a photo edited in Kelvin the field the partial does not
/// name (a partial with only `temp`, or only `tint`) would then be read on the other scale, giving a white neither
/// the photo nor the partial asked for. A one-field white balance partial is therefore resolved against the photo
/// first: the photo's white balance is brought onto the partial's scale with the photo's as-shot white
/// (`kelvin_to_legacy` / `legacy_to_kelvin` via `normalize_wb`), the partial merges unchanged, and the result lands
/// back on the photo's editing scale (`normalize_wb`), where the kept field carries the look the photo had.
/// Partials naming both fields, or neither, mean the same as before. A photo whose white balance mode renders its
/// own white — `As Shot`, or a named preset on a raw edited in Kelvin — has that white written out as Custom values
/// first ([`wb_mode_ignores_its_numbers`]), because a mode that answers its own white would otherwise store the
/// partial's number without any render reading it; a partial that names a `mode` itself stays authoritative.
pub(crate) fn merge_settings_for_photo(
    d: &DevelopSettings,
    partial: &Value,
    info: &lcc_pipeline::SourceInfo,
) -> std::result::Result<DevelopSettings, serde_json::Error> {
    let prepared = prepare_photo_for_wb_partial(d, partial, info);
    let on_kelvin_scale = prepared.is_some() && info.raw && !info.relative_wb;
    let mut out = prepared.as_ref().unwrap_or(d).merged(partial)?;
    if on_kelvin_scale {
        // and the result back onto the scale this photo is edited on
        lcc_pipeline::local::normalize_wb(&mut out, info);
    }
    Ok(out)
}

/// `v` as a list of numbers; `None` unless it is an array of nothing but numbers.
fn numbers(v: Option<&Value>) -> Option<Vec<f64>> {
    v?.as_array()?.iter().map(Value::as_f64).collect()
}

/// A width × height lock stored as hundredths. Rejects non-finite, non-positive and
/// extreme (beyond 20:1) shapes, which would give an empty or unusable crop.
pub(crate) fn aspect_tuple(c: &str, x: f64, y: f64) -> Result<(u32, u32)> {
    let ratio = x / y;
    if !(x.is_finite()
        && y.is_finite()
        && x > 0.0
        && y > 0.0
        && ratio.is_finite()
        && (1.0 / lcc_geom::MAX_RATIO..=lcc_geom::MAX_RATIO).contains(&ratio))
    {
        return Err(bad(c, "aspect needs two positive numbers with a ratio between 1:20 and 20:1"));
    }
    let to = |v: f64| (v * 100.0).round();
    let (a, b) = (to(x), to(y));
    if a < 1.0 || b < 1.0 || a > f64::from(u32::MAX) || b > f64::from(u32::MAX) {
        return Err(bad(c, "aspect numbers are out of range"));
    }
    // what is stored is the rounded pair: `[1, 0.044]` is 22.7:1 once it is `(100, 4)`
    if !(1.0 / lcc_geom::MAX_RATIO..=lcc_geom::MAX_RATIO).contains(&(a / b)) {
        return Err(bad(c, "aspect needs two positive numbers with a ratio between 1:20 and 20:1"));
    }
    Ok((a as u32, b as u32))
}

/// How a new aspect ratio is turned (issue #2253).
#[derive(Clone, Copy, PartialEq)]
enum Turn {
    /// As the crop is now: a named ratio ("2x3", "original"…) lands landscape on a landscape crop
    /// and portrait on a portrait one (after X), as in Lightroom. A square crop follows the image.
    LikeCrop,
    /// As given: a custom `[w, h]` taller than wide is a portrait crop.
    AsGiven,
}

fn set_aspect(d: &mut DevelopSettings, w: f64, h: f64, aspect: Option<(u32, u32)>, turn: Turn) {
    let r = d.crop.geometry.rect_px(w, h);
    let landscape = if (r.width() - r.height()).abs() > 1e-6 * r.width().max(r.height()) { r.width() > r.height() } else { w >= h };
    let aspect = aspect.map(|(aw, ah)| if turn == Turn::LikeCrop && (aw >= ah) != landscape { (ah, aw) } else { (aw, ah) });
    d.crop.aspect = aspect;
    let a = aspect.map(|(aw, ah)| aw as f64 / ah as f64);
    let angle = d.crop.geometry.angle;
    d.crop.geometry = crop_fit_angle(w, h, angle, a);
}

/// Another photo is selected besides the active one (Crop to Same Aspect Ratio has someone to act on).
fn has_other_selected(s: &Session) -> std::result::Result<(), String> {
    let id = s.active().ok_or("no photo selected")?;
    if s.selection.ids.iter().any(|o| *o != id && s.catalog.photo(*o).is_some()) { Ok(()) } else { Err("select more than one photo".into()) }
}

/// Photo `id` is on an older rendering process than [`ProcessVersion::LATEST`].
fn outdated(s: &Session, id: PhotoId) -> bool {
    s.catalog.photo(id).is_some_and(|p| p.develop.process.is_outdated())
}

fn already_current() -> String {
    format!("already on the current process (version {})", ProcessVersion::LATEST.0)
}

/// Update to Current Process applies to the selection (the selected photos, else the active
/// photo): one of them is on an older rendering process. Calls that name their photos are
/// checked by the command instead (`explicit_targets`).
fn has_outdated_process(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    let sel = &s.selection;
    let targets = if sel.ids.is_empty() { sel.active.as_slice() } else { sel.ids.as_slice() };
    if targets.iter().any(|id| outdated(s, *id)) { Ok(()) } else { Err(already_current()) }
}

/// The photos a call names (`ids`, or `id`), each of which must be in the library; `None` when it
/// names none (the selection applies).
fn named_photos(s: &Session, p: &Value, c: &str) -> Result<Option<Vec<PhotoId>>> {
    if !super::names_photos(p) {
        return Ok(None);
    }
    let given = match p.get("ids").filter(|v| !v.is_null()) {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(bad(c, "`ids` must be a list of photo ids")),
        None => p.get("id").cloned().into_iter().collect(),
    };
    if given.is_empty() {
        return Err(bad(c, "`ids` names no photos"));
    }
    let ids = given
        .iter()
        .map(|v| match v.as_u64().map(PhotoId) {
            Some(id) if s.catalog.photo(id).is_some() => Ok(id),
            _ => Err(bad(c, format!("no such photo {v}"))),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(ids))
}

/// The user preset named by `p.id` (built-ins can't be changed).
/// Turn a preset's mark (At Import, hidden) on, off (`key`) or over; bumps the preset list's
/// generation so the mark is saved and the pickers rebuild.
fn toggle_mark(s: &mut Session, p: &Value, c: &str, key: &str, list: impl Fn(&mut Session) -> &mut Vec<String>) -> Result<Value> {
    let pid = str_param(p, "id").ok_or_else(|| bad(c, "missing id"))?.to_string();
    if !s.presets.iter().any(|x| x.id == pid) {
        return Err(bad(c, format!("unknown preset `{pid}`")));
    }
    let marks = list(s);
    let was = marks.contains(&pid);
    let on = bool_or(p, key, !was);
    marks.retain(|x| *x != pid);
    if on {
        marks.push(pid);
    }
    s.presets.bump();
    let mut out = serde_json::Map::new();
    out.insert(key.to_string(), Value::Bool(on));
    Ok(Value::Object(out))
}

impl Session {
    /// Built-in presets take their stock name and group, then the user's renames and moves
    /// ([`Session::preset_builtin_names`], [`Session::preset_builtin_groups`]).
    pub(crate) fn apply_builtin_edits(&mut self) {
        let stock: std::collections::HashMap<String, (String, String)> =
            crate::presets::builtin().into_iter().map(|b| (b.id, (b.name, b.group))).collect();
        let (names, groups) = (self.preset_builtin_names.clone(), self.preset_builtin_groups.clone());
        for p in self.presets.iter_mut().filter(|p| p.builtin) {
            if let Some((name, group)) = stock.get(&p.id) {
                p.name = names.get(&p.id).cloned().unwrap_or_else(|| name.clone());
                p.group = groups.get(&p.id).cloned().unwrap_or_else(|| group.clone());
            }
        }
        self.presets.bump();
    }
}

/// Give preset `p.id` a new name or group (`name`: which); a built-in one keeps the change in the
/// session's built-in edits, so it lasts and `preset.restoreBuiltins` can undo it.
fn edit_preset(s: &mut Session, p: &Value, c: &str, name: bool, value: String) -> Result<()> {
    let pid = str_param(p, "id").ok_or_else(|| bad(c, "missing id"))?.to_string();
    let pr = s.presets.iter_mut().find(|x| x.id == pid).ok_or_else(|| bad(c, format!("unknown preset `{pid}`")))?;
    if name {
        pr.name = value.clone();
    } else {
        pr.group = value.clone();
    }
    if pr.builtin {
        let edits = if name { &mut s.preset_builtin_names } else { &mut s.preset_builtin_groups };
        edits.insert(pid, value);
    }
    s.presets.bump();
    Ok(())
}

/// Every preset in `group` or a group under it.
fn in_group(p: &Preset, group: &str) -> bool {
    p.group == group || p.group.strip_prefix(group).is_some_and(|r| r.starts_with('/'))
}

/// `group` (or under it) moved to `new_group`, for user folders and every preset in it (built-in
/// ones keep it in the built-in edits).
fn regroup(s: &mut Session, old_group: &str, new_group: &str) {
    let swap = |g: &str| -> Option<String> {
        if g == old_group {
            Some(new_group.to_string())
        } else {
            g.strip_prefix(old_group).and_then(|r| r.strip_prefix('/')).map(|rest| format!("{new_group}/{rest}"))
        }
    };
    for f in &mut s.preset_folders {
        if let Some(n) = swap(f) {
            *f = n;
        }
    }
    let mut moved_builtin = Vec::new();
    for p in s.presets.iter_mut() {
        if let Some(n) = swap(&p.group) {
            if p.builtin {
                moved_builtin.push((p.id.clone(), n.clone()));
            }
            p.group = n;
        }
    }
    s.preset_builtin_groups.extend(moved_builtin);
    s.presets.bump();
}

fn user_preset<'a>(s: &'a mut Session, p: &Value, c: &str) -> Result<&'a mut Preset> {
    let pid = str_param(p, "id").ok_or_else(|| bad(c, "missing id"))?;
    let pr = s.presets.iter_mut().find(|x| x.id == pid).ok_or_else(|| bad(c, format!("unknown preset `{pid}`")))?;
    if pr.builtin {
        return Err(bad(c, "built-in presets can't be changed"));
    }
    Ok(pr)
}

/// Change version `p.index` of the active photo (`f` gets it and the current settings).
fn edit_version(s: &mut Session, p: &Value, c: &str, label: &str, f: impl FnOnce(&mut Version, std::sync::Arc<DevelopSettings>)) -> Result<Value> {
    let id = active(s, c)?;
    let i = index_req(p, "index", c)?;
    let ph = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
    let mut versions = ph.versions.clone();
    let v = versions.get_mut(i).ok_or_else(|| bad(c, "no such version"))?;
    f(v, ph.develop.clone());
    s.commit(label, Op::SetVersions { id, versions })?;
    ok()
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.set",
            "Set Develop Values",
            [],
            None,
            "{control?: controlId, value?: number, values?: {controlId: number}, ids?: [photo]}: see develop.controls; `crop.angle` turns the crop as crop.straighten does (kept if it still fits, else the largest of its aspect inside the photo)",
            has_active,
            |s, p| {
                let mut vals: Vec<(String, f64)> = Vec::new();
                if let (Some(c), Some(v)) = (str_param(p, "control"), p.get("value").and_then(Value::as_f64)) {
                    vals.push((c.to_string(), v));
                }
                if let Some(o) = p.get("values").and_then(Value::as_object) {
                    for (k, v) in o {
                        vals.push((k.clone(), v.as_f64().ok_or_else(|| bad("develop.set", format!("`{k}` must be a number")))?));
                    }
                }
                if vals.is_empty() {
                    return Err(bad("develop.set", "give `control`+`value` or `values`"));
                }
                for (k, _) in &vals {
                    if controls::find(k).is_none() {
                        return Err(bad("develop.set", format!("unknown control `{k}`")));
                    }
                }
                let label = if vals.len() == 1 { controls::find(&vals[0].0).map(|c| c.label).unwrap_or("Edit").to_string() } else { "Edit".into() };
                let angle = vals.iter().any(|(k, _)| k == CROP_ANGLE);
                let apply = |d: &mut DevelopSettings, info: &lcc_pipeline::SourceInfo, dims: (f64, f64)| {
                    let before = d.crop.geometry;
                    if vals.iter().any(|(k, _)| k == "wb.temp" || k == "wb.tint") {
                        lcc_pipeline::local::normalize_wb(d, info);
                        if d.wb.mode == WbMode::AsShot {
                            d.wb.temp = info.as_shot_temp;
                            d.wb.tint = info.as_shot_tint;
                        }
                    }
                    for (k, v) in &vals {
                        controls::set(d, k, *v);
                        if k == "wb.temp" || k == "wb.tint" {
                            d.wb.mode = WbMode::Custom;
                        }
                    }
                    refit_crop_angle(angle, before, d, dims);
                };
                if let Some(ids) = super::photo_ids(p, "develop.set")? {
                    let ops = ids
                        .iter()
                        .filter_map(|id| s.develop_of(*id).map(|d| (*id, d)))
                        .filter_map(|(id, d)| {
                            let mut d = (*d).clone();
                            let info = s.source_info(id);
                            apply(&mut d, &info, crop_dims(s, id));
                            s.develop_op(id, d, &label)
                        })
                        .collect();
                    s.commit(&label, Op::Batch { ops })?;
                    return ok();
                }
                let id = active(s, "develop.set")?;
                let info = s.source_info(id);
                let dims = crop_dims(s, id);
                edit(s, "develop.set", &label, |d| {
                    apply(d, &info, dims);
                    Ok(())
                })
            }
        ),
        cmd!("develop.adjust", "Nudge Develop Value", [], None, "{control, delta}", has_active, |s, p| {
            let c = str_param(p, "control").ok_or_else(|| bad("develop.adjust", "missing control"))?.to_string();
            let spec = controls::find(&c).ok_or_else(|| bad("develop.adjust", "unknown control"))?;
            let delta = f64_req(p, "delta", "develop.adjust")?;
            let id = active(s, "develop.adjust")?;
            let (info, dims) = (s.source_info(id), crop_dims(s, id));
            edit(s, "develop.adjust", spec.label, |d| {
                *d = nudge(&info, dims, d, &c, delta, spec.default);
                Ok(())
            })?;
            // a multi-photo interaction (develop.beginInteraction {ids}): the other photos move along,
            // live, each from its own value
            for other in s.interaction_others() {
                let Some(d) = s.develop_of(other) else { continue };
                let nd = nudge(&s.source_info(other), crop_dims(s, other), &d, &c, delta, spec.default);
                s.set_develop_silent(other, nd, spec.label)?;
            }
            ok()
        }),
        cmd!("develop.merge", "Apply Settings JSON", [], None, "{settings: partial DevelopSettings JSON, label?}", has_active, |s, p| {
            let mut partial = p.get("settings").cloned().ok_or_else(|| bad("develop.merge", "missing settings"))?;
            // a flat-field map given with the settings (their portable form) is stored with the edit
            s.absorb_settings(&mut partial);
            let label = str_param(p, "label").unwrap_or("Edit").to_string();
            let id = active(s, "develop.merge")?;
            let info = s.source_info(id);
            edit(s, "develop.merge", &label, |d| {
                *d = merge_settings_for_photo(d, &partial, &info).map_err(|e| bad("develop.merge", e.to_string()))?;
                Ok(())
            })
        }),
        cmd!("develop.reset", "Reset Edits", ["Photo"], Some("Cmd+Shift+R"), "{ids?}", has_selection, |s, p| {
            let ids = s.targets(p)?;
            let ops = ids
                .iter()
                .filter_map(|id| s.develop_of(*id).map(|d| (*id, d)))
                .filter_map(|(id, _d)| {
                    // back to the photo's import defaults when a default preset gave it its look
                    let look = s.catalog.photo(id).and_then(|p| p.import_look.clone());
                    let fresh = match look {
                        Some(l) => (*l).clone(),
                        None => {
                            // the photo's built-in defaults (raws: capture sharpening and colour noise
                            // reduction, embedded lens corrections), as at import, with the white balance
                            // of its current as-shot reference
                            let info = s.source_info(id);
                            let defaults = s.catalog.photo(id).map(|p| p.camera_defaults()).unwrap_or_default();
                            let mut fresh = DevelopSettings {
                                wb: lcc_develop::WhiteBalance {
                                    mode: WbMode::AsShot,
                                    temp: info.as_shot_temp,
                                    tint: info.as_shot_tint,
                                    ..Default::default()
                                },
                                ..defaults
                            };
                            lcc_pipeline::local::normalize_wb(&mut fresh, &info);
                            fresh
                        }
                    };
                    // a fresh start renders with the current process (a section or slider reset doesn't change it)
                    s.develop_op(id, DevelopSettings { process: ProcessVersion::LATEST, ..fresh }, "Reset")
                })
                .collect();
            s.commit("Reset", Op::Batch { ops })?;
            ok()
        }),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "develop.updateProcess",
                "Update to Current Process",
                ["Photo", "Develop Settings"],
                None,
                "{ids?}: moves the target photos (`ids`, else the selection) that are on an older rendering process to the current one, as one undo step (their look can change; the sliders keep their values; see docs/process-versions.md). Enabled while a selected photo is on an older process; named photos are checked themselves (each must exist, one must be on an older process) → {changed, process}",
                has_outdated_process,
                |s, p| {
                    const C: &str = "develop.updateProcess";
                    const LABEL: &str = "Update to Current Process";
                    let targets = match named_photos(s, p, C)? {
                        Some(ids) if !ids.iter().any(|id| outdated(s, *id)) => return Err(crate::EngineError::Disabled(C.into(), already_current())),
                        Some(ids) => ids,
                        None => s.targets(p)?,
                    };
                    let ops: Vec<Op> = targets
                        .into_iter()
                        .filter_map(|id| s.develop_of(id).map(|d| (id, d)))
                        .filter(|(_, d)| d.process.is_outdated())
                        .filter_map(|(id, d)| s.develop_op(id, DevelopSettings { process: ProcessVersion::LATEST, ..(*d).clone() }, LABEL))
                        .collect();
                    let n = ops.len();
                    if n > 0 {
                        s.commit(LABEL, Op::Batch { ops })?;
                    }
                    Ok(json!({"changed": n, "process": ProcessVersion::LATEST}))
                }
            )
        },
        cmd!(
            "develop.resetSection",
            "Reset Section",
            [],
            None,
            "{section: light|curve|color|mixer|grading|effects|vignette|grain|detail|optics|geometry}",
            has_active,
            |s, p| {
                let sec = section_param(p, "develop.resetSection")?;
                edit(s, "develop.resetSection", &format!("Reset {}", sec.label()), |d| {
                    d.reset_section(sec);
                    Ok(())
                })
            }
        ),
        cmd!("develop.resetControl", "Reset Control", [], None, "{control}", has_active, |s, p| {
            let c = str_param(p, "control").ok_or_else(|| bad("develop.resetControl", "missing control"))?.to_string();
            let spec = controls::find(&c).ok_or_else(|| bad("develop.resetControl", "unknown control"))?;
            let dims = crop_dims(s, active(s, "develop.resetControl")?);
            edit(s, "develop.resetControl", spec.label, |d| {
                let before = d.crop.geometry;
                controls::set(d, &c, spec.default);
                refit_crop_angle(c == CROP_ANGLE, before, d, dims);
                Ok(())
            })
        }),
        cmd!("develop.sectionEnabled", "Toggle Section", [], None, "{section: string, enabled: bool}", has_active, |s, p| {
            let sec = str_param(p, "section").ok_or_else(|| bad("develop.sectionEnabled", "missing section"))?.to_string();
            let on = bool_or(p, "enabled", true);
            edit(s, "develop.sectionEnabled", "Toggle Section", |d| {
                d.set_section_enabled(&sec, on);
                Ok(())
            })
        }),
        cmd!(
            "develop.autoBwMix",
            "Auto B&W Mix",
            [],
            None,
            "{}: sets the black & white mix from the photo's colours (and switches to B&W)",
            has_active,
            |s, _| {
                let id = active(s, "develop.autoBwMix")?;
                let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad("develop.autoBwMix", e))?;
                let info = s.source_info(id);
                let d = s.develop_of(id).unwrap_or_default();
                // Source plug-in filters (e.g. a negative conversion) change what the photo is
                let src = lcc_pipeline::plugin_filters::filtered_source(&src, &info, &d, 1024);
                let m = lcc_pipeline::auto::auto_bw_mix(&src, &info, &d);
                edit(s, "develop.autoBwMix", "Auto B&W Mix", |d| {
                    d.treatment = Treatment::Bw;
                    let b = &mut d.bw_mix;
                    [b.red, b.orange, b.yellow, b.green, b.aqua, b.blue, b.purple, b.magenta] = m;
                    Ok(())
                })?;
                Ok(json!({"mix": m}))
            }
        ),
        cmd!("develop.auto", "Auto Settings", ["Photo"], Some("Shift+A"), "{}", has_active, |s, _| {
            let id = active(s, "develop.auto")?;
            let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad("develop.auto", e))?;
            let info = s.source_info(id);
            let d = s.develop_of(id).unwrap_or_default();
            // Source plug-in filters (e.g. a negative conversion) change what the photo is
            let src = lcc_pipeline::plugin_filters::filtered_source(&src, &info, &d, 1024);
            let a = lcc_pipeline::auto::auto_tone(&src, &info, &d);
            // With Auto Sync on, every other selected photo gets its own Auto (worked out on its own pixels), not
            // the active photo's numbers (upstream #3808, in our own form); one undo step. A photo that can't
            // be read keeps its settings.
            if s.auto_sync_applies(id, "Auto") {
                let others: Vec<PhotoId> = s.selection.ids.iter().copied().filter(|x| *x != id).collect();
                let mut picks = vec![(id, a)];
                for x in others {
                    let Ok(xsrc) = s.source_now(x, SourceLevel::Thumb) else { continue };
                    let (xinfo, xd) = (s.source_info(x), s.develop_of(x).unwrap_or_default());
                    let xsrc = lcc_pipeline::plugin_filters::filtered_source(&xsrc, &xinfo, &xd, 1024);
                    picks.push((x, lcc_pipeline::auto::auto_tone(&xsrc, &xinfo, &xd)));
                }
                let mut ops: Vec<Op> = Vec::new();
                let mut done: Vec<PhotoId> = Vec::new();
                for (x, ax) in picks {
                    let mut nd = (*s.develop_of(x).unwrap_or_default()).clone();
                    set_auto_tone(&mut nd, &ax);
                    if let Some(op) = s.develop_op(x, nd, "Auto") {
                        ops.push(op);
                        done.push(x);
                    }
                }
                s.end_interaction()?;
                if !ops.is_empty() {
                    s.commit("Auto", Op::Batch { ops })?;
                }
                for x in done {
                    if let Some(d) = s.develop_of(x) {
                        let values = Session::auto_values(&d);
                        s.auto_applied.insert(x, values);
                    }
                }
            } else {
                edit(s, "develop.auto", "Auto", |d| {
                    set_auto_tone(d, &a);
                    Ok(())
                })?;
                if let Some(d) = s.develop_of(id) {
                    let values = Session::auto_values(&d);
                    s.auto_applied.insert(id, values);
                }
            }
            let mut r = serde_json::to_value(a).unwrap_or_default();
            if let Some(o) = r.as_object_mut() {
                o.insert("current".into(), json!(s.auto_is_current(id)));
            }
            Ok(r)
        }),
        cmd!(
            "develop.wb",
            "White Balance",
            [],
            None,
            "{mode: asShot|auto|daylight|cloudy|shade|tungsten|fluorescent|flash|custom, temp?, tint?}",
            has_active,
            |s, p| {
                let mode: WbMode =
                    serde_json::from_value(p.get("mode").cloned().unwrap_or(json!("custom"))).map_err(|e| bad("develop.wb", e.to_string()))?;
                let id = active(s, "develop.wb")?;
                let info = s.source_info(id);
                let (mut t, mut tint) = match mode {
                    WbMode::AsShot => (info.as_shot_temp, info.as_shot_tint),
                    WbMode::Auto => {
                        let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad("develop.wb", e))?;
                        lcc_pipeline::auto::auto_wb(&src, &info)
                    }
                    m => m
                        .preset()
                        .map(|(t, ti)| {
                            // presets are Kelvin on raws with that scale, relative to daylight (6500 K = as shot) elsewhere
                            if info.raw && !info.relative_wb { (t, ti) } else { (6500.0 * t / 5500.0, ti) }
                        })
                        .unwrap_or((info.as_shot_temp, info.as_shot_tint)),
                };
                t = f64_or(p, "temp", t);
                tint = f64_or(p, "tint", tint);
                // The resolved temperature/tint is stored, so rendering never depends on the mode; the
                // mode is kept for the UI's WB dropdown.
                edit(s, "develop.wb", "White Balance", |d| {
                    lcc_pipeline::local::normalize_wb(d, &info);
                    d.wb.mode = mode;
                    d.wb.temp = t.clamp(2000.0, 50000.0);
                    d.wb.tint = tint.clamp(-150.0, 150.0);
                    Ok(())
                })?;
                Ok(json!({"temp": t, "tint": tint}))
            }
        ),
        cmd!(
            "develop.wbPick",
            "White Balance Selector",
            [],
            Some("W"),
            "{x, y} normalized image coords of a neutral point, size?: side of the averaged square in pixels (1..=31, odd; default 5) → {applied: true, temp, tint}, or {applied: false, refused: bright|dark, message} for a clipped, near-white or near-black spot: nothing changes",
            has_active,
            |s, p| {
                let (x, y) = (f64_req(p, "x", "develop.wbPick")?, f64_req(p, "y", "develop.wbPick")?);
                if !x.is_finite() || !y.is_finite() {
                    return Err(bad("develop.wbPick", "x and y must be finite numbers"));
                }
                let half = wb_pick_half(p.get("size").and_then(Value::as_f64));
                let id = active(s, "develop.wbPick")?;
                let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad("develop.wbPick", e))?;
                let info = s.source_info(id);
                let d = s.develop_of(id).unwrap_or_default();
                // Sample a small neighbourhood in the oriented source.
                let o = src.oriented(d.orientation);
                let (cx, cy) = ((x.clamp(0.0, 1.0) * o.width as f64) as isize, (y.clamp(0.0, 1.0) * o.height as f64) as isize);
                let acc = match wb_sample(&o, cx, cy, half) {
                    WbSample::Neutral(c) => c,
                    // a white balance from a spot with no colour left is wrong: nothing changes and
                    // the selector stays armed (OneSlips, test notes 2026-10-25 note 13)
                    refused => {
                        let (why, message) = match refused {
                            WbSample::TooDark => ("dark", "This spot is too dark to read its colour: click a brighter neutral area"),
                            _ => ("bright", "This spot is too bright to read its colour: click a slightly darker neutral area"),
                        };
                        return Ok(json!({"applied": false, "refused": why, "message": message}));
                    }
                };
                let patch = lcc_raster::Rgb32f::filled(4, 4, acc);
                let (t, tint) = lcc_pipeline::auto::auto_wb(&patch, &info);
                edit(s, "develop.wbPick", "White Balance", |d| {
                    lcc_pipeline::local::normalize_wb(d, &info);
                    d.wb.mode = WbMode::Custom;
                    d.wb.temp = t;
                    d.wb.tint = tint;
                    Ok(())
                })?;
                Ok(json!({"applied": true, "temp": t, "tint": tint}))
            }
        ),
        cmd!(
            "develop.treatment",
            "Black & White",
            ["Photo", "Develop Settings"],
            Some("V"),
            "{bw?: bool} (toggles when omitted)",
            has_active,
            |s, p| {
                edit(s, "develop.treatment", "Treatment", |d| {
                    let bw = p.get("bw").and_then(Value::as_bool).unwrap_or(d.treatment != Treatment::Bw);
                    d.treatment = if bw { Treatment::Bw } else { Treatment::Color };
                    Ok(())
                })
            }
        ),
        cmd!(
            "develop.profile",
            "Set Profile",
            [],
            None,
            "{id: profile id (see profiles.list), or a camera profile (dcp:…, cal:…, lc: as develop.cameraProfile; one with a look of its own comes with it, \"As profile\": the look goes back to the default), amount?: 0..200}",
            has_active,
            |s, p| {
                let id = str_param(p, "id").ok_or_else(|| bad("develop.profile", "missing id"))?.to_string();
                if crate::camera_profiles::is_camera_choice(&id) {
                    return choose_camera_profile(s, &id);
                }
                if !crate::presets::PROFILES.iter().any(|x| x.id == id) && !s.lut_profiles.iter().any(|x| x.id == id) {
                    return Err(bad("develop.profile", format!("unknown profile `{id}`")));
                }
                edit(s, "develop.profile", "Profile", |d| {
                    d.profile.id = id.clone();
                    d.profile.amount = f64_or(p, "amount", d.profile.amount).clamp(0.0, 200.0);
                    Ok(())
                })?;
                s.note_profile_used(&id);
                ok()
            }
        ),
        cmd!(
            "develop.cameraProfile",
            "Set Camera Profile",
            [],
            None,
            "{id: \"\" (the camera's default), \"lc\" (the app's own colour), \"dcp:…\" (an imported .dcp) or \"cal:…\" (a Lightroom-matched calibrated look; both listed by camera.listDcp)}: how the raw is decoded",
            has_active,
            |s, p| {
                const C: &str = "develop.cameraProfile";
                let id = str_param(p, "id").unwrap_or("").trim().to_string();
                if !(id.is_empty() || id == "lc" || lcc_raw::dcp::library_get(&id).is_some() || lcc_raw::dcp::calibrated_look_exists(&id)) {
                    return Err(bad(C, format!("unknown camera profile `{id}`")));
                }
                edit(s, C, "Camera Profile", |d| {
                    d.profile.camera = id.clone();
                    Ok(())
                })
            }
        ),
        cmd!("profile.favorite", "Favorite Profile", [], None, "{id, favorite?: bool} (toggles when omitted)", always, |s, p| {
            let id = str_param(p, "id").ok_or_else(|| bad("profile.favorite", "missing id"))?.to_string();
            if crate::presets::profile(&id).is_none() && !s.lut_profiles.iter().any(|x| x.id == id) {
                return Err(bad("profile.favorite", format!("unknown profile `{id}`")));
            }
            let on = s.profile_favorites.contains(&id);
            let want = bool_or(p, "favorite", !on);
            if want && !on {
                s.profile_favorites.push(id);
            } else if !want {
                s.profile_favorites.retain(|f| *f != id);
            }
            Ok(json!({"favorite": want}))
        }),
        cmd!(
            "develop.curve",
            "Set Point Curve",
            [],
            None,
            "{channel: master|red|green|blue, points: [[x,y],...] in 0..1, mask?: id (that mask's local Curve instead of the Tone Curve)}",
            has_active,
            |s, p| {
                let ch = str_param(p, "channel").unwrap_or("master").to_string();
                let mask = super::curves::mask_param(p, "develop.curve")?;
                let mut pts: Vec<Point> = p
                    .get("points")
                    .and_then(Value::as_array)
                    .ok_or_else(|| bad("develop.curve", "missing points"))?
                    .iter()
                    .filter_map(|q| Some(Point::new(q.get(0)?.as_f64()?.clamp(0.0, 1.0), q.get(1)?.as_f64()?.clamp(0.0, 1.0))))
                    .collect();
                // stored in the order the curve runs, as the editor draws and drags them
                pts.sort_by(|a, b| a.x.total_cmp(&b.x));
                if let Some(m) = mask {
                    return edit(s, "develop.curve", "Mask Curve", |d| {
                        let c = super::curves::mask_curve_mut(d, m, "develop.curve")?;
                        *c.channel_mut(&ch).ok_or_else(|| bad("develop.curve", format!("unknown channel `{ch}`")))? = pts;
                        Ok(())
                    });
                }
                edit(s, "develop.curve", "Tone Curve", |d| {
                    match ch.as_str() {
                        "master" | "rgb" => d.curve.master = pts,
                        "red" => d.curve.red = pts,
                        "green" => d.curve.green = pts,
                        "blue" => d.curve.blue = pts,
                        other => return Err(bad("develop.curve", format!("unknown channel `{other}`"))),
                    }
                    Ok(())
                })
            }
        ),
        // ---- crop & rotate
        cmd!("crop.set", "Set Crop", [], None, "{rect?: [x0,y0,x1,y1] normalized in the straightened frame, angle?: degrees}", has_active, |s, p| {
            let id = active(s, "crop.set")?;
            let rect = match p.get("rect") {
                None | Some(Value::Null) => None,
                Some(v) => match v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<f64>>()).as_deref() {
                    Some(&[x0, y0, x1, y1]) => Some([x0, y0, x1, y1]),
                    _ => return Err(bad("crop.set", "rect needs 4 numbers")),
                },
            };
            set_crop_of(s, id, "crop.set", rect, p.get("angle").and_then(Value::as_f64))?;
            ok()
        }),
        cmd!(
            "crop.drag",
            "Drag Crop Handle",
            [],
            None,
            "{handle: topLeft|topRight|bottomRight|bottomLeft|top|right|bottom|left|move | 0..8, from: [x,y], to: [x,y] (normalized, straightened frame), start?: [x0,y0,x1,y1] (default: the current crop), keepRatio?: bool (default false)}: drags a crop handle; stops at the image edge, keeps the opposite edges, honours the aspect lock; keepRatio holds the frame's current proportions for this drag even with the lock off (the UI sends it while Shift is held)",
            has_active,
            |s, p| {
                let id = active(s, "crop.drag")?;
                let (w, h) = crop_dims(s, id);
                let handle = match p.get("handle") {
                    Some(Value::String(n)) => CropHandle::from_name(n),
                    Some(v) => v.as_u64().and_then(|i| u8::try_from(i).ok()).and_then(CropHandle::from_index),
                    None => None,
                }
                .ok_or_else(|| bad("crop.drag", "unknown handle"))?;
                let point = |k: &str| -> Result<Point> {
                    match numbers(p.get(k)).as_deref() {
                        Some([x, y]) if x.is_finite() && y.is_finite() => Ok(Point::new(*x, *y)),
                        _ => Err(bad("crop.drag", format!("`{k}` needs [x, y]"))),
                    }
                };
                let (from, to) = (point("from")?, point("to")?);
                let given = match (p.get("start"), numbers(p.get("start")).as_deref()) {
                    (None | Some(Value::Null), _) => None,
                    (_, Some(&[x0, y0, x1, y1])) if [x0, y0, x1, y1].iter().all(|v| v.is_finite()) => {
                        Some(Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
                    }
                    _ => return Err(bad("crop.drag", "`start` needs 4 numbers")),
                };
                let keep_ratio = bool_or(p, "keepRatio", false);
                edit(s, "crop.drag", "Crop", |d| {
                    let start = CropGeometry { rect: given.unwrap_or(d.crop.geometry.rect), angle: d.crop.geometry.angle };
                    // A locked crop keeps the shape it has: the lock stores a width:height tuple whose
                    // orientation is reinterpreted per image, so the frame itself is the truth.
                    // `keepRatio` (Shift held) asks for the same thing for one drag without touching the lock.
                    let ratio = (d.crop.aspect.is_some() || keep_ratio).then(|| start.rect_px(w, h).aspect()).filter(|a| a.is_finite() && *a > 0.0);
                    d.crop.geometry = drag_crop(start, handle, from, to, w, h, ratio);
                    Ok(())
                })
            }
        ),
        cmd!(
            "crop.straighten",
            "Straighten",
            [],
            None,
            "{angle: degrees -45..45}: keeps the crop if it still fits, else shrinks it about its centre (the largest crop of its aspect stays the largest)",
            has_active,
            |s, p| {
                let id = active(s, "crop.straighten")?;
                let (w, h) = crop_dims(s, id);
                let angle = f64_req(p, "angle", "crop.straighten")?.clamp(-45.0, 45.0);
                edit(s, "crop.straighten", "Straighten", |d| {
                    // keep the current crop if it still fits, else shrink it about its centre until it does
                    d.crop.geometry = d.crop.geometry.with_angle(angle, w, h);
                    Ok(())
                })
            }
        ),
        cmd!("crop.autoStraighten", "Auto Straighten", [], None, "{}: level the horizon (or plumb lines) with the crop angle", has_active, |s, _| {
            let id = active(s, "crop.autoStraighten")?;
            let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad("crop.autoStraighten", e))?;
            let info = s.source_info(id);
            let d = s.develop_of(id).unwrap_or_default();
            let aux = lcc_pipeline::source_stage::aux_source(&src, &info, &d);
            let Some(level) = lcc_pipeline::upright::level_degrees(aux.as_deref().unwrap_or(&src), &info, &d) else {
                return Ok(json!({"changed": false, "reason": "no dominant horizontal or vertical lines"}));
            };
            // Same convention as the crop angle (verified: a horizon descending 6° to the right
            // comes out level with angle = level ≈ −6°).
            let angle = level.clamp(-45.0, 45.0);
            s.execute("crop.straighten", &json!({"angle": (angle * 100.0).round() / 100.0}))?;
            Ok(json!({"changed": true, "angle": angle}))
        }),
        cmd!(
            "crop.aspect",
            "Crop Aspect",
            [],
            None,
            "{aspect: \"original\"|\"free\"|\"current\" (lock the crop's present shape)|\"toggle\" (lock ↔ free)|\"1x1\"|\"4x5\"|\"8.5x11\"|\"5x7\"|\"2x3\"|\"4x3\"|\"16x9\"|\"16x10\"|[w,h], orientation?: \"crop\"|\"given\"}: a named ratio takes the crop's present orientation (landscape, or portrait after X); [w,h] is used as given (taller than wide → portrait); `orientation` overrides either",
            has_active,
            |s, p| {
                let id = active(s, "crop.aspect")?;
                let (w, h) = crop_dims(s, id);
                // checked first, whatever the aspect: a bad value is an error on every path
                let turn = match p.get("orientation") {
                    Some(Value::String(o)) if o == "crop" => Turn::LikeCrop,
                    Some(Value::String(o)) if o == "given" => Turn::AsGiven,
                    Some(_) => return Err(bad("crop.aspect", "orientation is \"crop\" or \"given\"")),
                    None if matches!(p.get("aspect"), Some(Value::Array(_))) => Turn::AsGiven,
                    None => Turn::LikeCrop,
                };
                let locked = s.develop_of(id).is_some_and(|d| d.crop.aspect.is_some());
                let lock_current = match p.get("aspect").and_then(Value::as_str) {
                    Some("current") => true,
                    Some("toggle") => !locked,
                    _ => false,
                };
                if lock_current {
                    // lock to the rectangle as it is: no refit
                    return edit(s, "crop.aspect", "Lock Crop Aspect", |d| {
                        let r = d.crop.geometry.rect_px(w, h);
                        d.crop.aspect = Some(((r.width() * 100.0).round().max(1.0) as u32, (r.height() * 100.0).round().max(1.0) as u32));
                        Ok(())
                    });
                }
                let aspect = match p.get("aspect") {
                    Some(Value::String(a)) if a == "toggle" => None,
                    Some(Value::String(a)) if a == "free" => None,
                    Some(Value::String(a)) if a == "original" => Some(lcc_develop::Crop::original_aspect(w, h)),
                    Some(Value::String(a)) => {
                        let (x, y) = a.split_once('x').ok_or_else(|| bad("crop.aspect", "use WxH"))?;
                        let (x, y): (f64, f64) =
                            (x.parse().map_err(|_| bad("crop.aspect", "bad number"))?, y.parse().map_err(|_| bad("crop.aspect", "bad number"))?);
                        Some(aspect_tuple("crop.aspect", x, y)?)
                    }
                    Some(v @ Value::Array(_)) => match numbers(Some(v)).as_deref() {
                        Some(&[x, y]) => Some(aspect_tuple("crop.aspect", x, y)?),
                        _ => return Err(bad("crop.aspect", "use [w, h] with two numbers")),
                    },
                    _ => return Err(bad("crop.aspect", "missing aspect")),
                };
                edit(s, "crop.aspect", "Crop Aspect", |d| {
                    if aspect.is_none() {
                        d.crop.aspect = None;
                    } else {
                        set_aspect(d, w, h, aspect, turn);
                    }
                    Ok(())
                })
            }
        ),
        cmd!("crop.rotateAspect", "Rotate Crop Aspect", [], Some("X"), "{}", has_active, |s, _| {
            let id = active(s, "crop.rotateAspect")?;
            let (w, h) = crop_dims(s, id);
            edit(s, "crop.rotateAspect", "Rotate Crop", |d| {
                let r = d.crop.geometry.rect_px(w, h);
                // a degenerate crop has no aspect to rotate: fall back to the image's own
                let a = Some(r.height() / r.width()).filter(|a| a.is_finite() && *a > 0.0);
                d.crop.aspect = d.crop.aspect.map(|(x, y)| (y, x));
                let angle = d.crop.geometry.angle;
                d.crop.geometry = crop_fit_angle(w, h, angle, a);
                Ok(())
            })
        }),
        cmd!("crop.reset", "Reset Crop", [], Some("Cmd+Alt+R"), "{}", has_active, |s, _| {
            edit(s, "crop.reset", "Reset Crop", |d| {
                d.crop = Default::default();
                Ok(())
            })
        }),
        cmd!(
            "crop.asShot",
            "Crop As Shot",
            [],
            Some("Cmd+Alt+Shift+R"),
            "{}: the crop goes back to the decoded camera frame (the sensor's own crop, which decoding already applies): full frame, no angle, no ratio lock; flips stay",
            has_active,
            |s, _| {
                edit(s, "crop.asShot", "Crop As Shot", |d| {
                    d.crop.geometry = CropGeometry::default();
                    d.crop.aspect = None;
                    Ok(())
                })
            }
        ),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "crop.sameAspect",
                "Crop to Same Aspect Ratio",
                [],
                // Shift+A in the Classic keymap set (keymap_classic); the Alternative set gives it to Auto Settings
                None,
                "{ids?}: gives every other selected photo (or `ids`) the active photo's crop ratio: the largest centred crop of that ratio, as one undo step → {changed}. Needs an active photo and at least one other target",
                has_other_selected,
                |s, p| {
                    const C: &str = "crop.sameAspect";
                    let id = active(s, C)?;
                    let (w, h) = crop_dims(s, id);
                    let r = s.develop_of(id).unwrap_or_default().crop.geometry.rect_px(w, h);
                    // a crop of no size has no ratio to hand on
                    let (qa, qb) =
                        aspect_tuple(C, r.width(), r.height()).map_err(|_| bad(C, "the active photo's crop has no usable aspect ratio"))?;
                    let targets: Vec<PhotoId> = match named_photos(s, p, C)? {
                        Some(ids) => ids,
                        None => s.targets(p)?,
                    }
                    .into_iter()
                    .filter(|t| *t != id)
                    .collect();
                    let mut ops = Vec::new();
                    for t in targets {
                        let Some(mut d) = s.develop_of(t).map(|d| (*d).clone()) else { continue };
                        let (tw, th) = crop_dims(s, t);
                        d.crop.aspect = Some((qa, qb));
                        let angle = d.crop.geometry.angle;
                        d.crop.geometry = crop_fit_angle(tw, th, angle, Some(f64::from(qa) / f64::from(qb)));
                        ops.extend(s.develop_op(t, d, "Crop to Same Aspect Ratio"));
                    }
                    let n = ops.len();
                    if n > 0 {
                        s.commit("Crop to Same Aspect Ratio", Op::Batch { ops })?;
                    }
                    Ok(json!({"changed": n}))
                }
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "develop.setProcess",
                "Set Process Version",
                [],
                None,
                "{version: number, ids?}: puts the target photos (`ids`, else the selection) on rendering process `version` (1..=latest; unknown versions are refused), as one undo step. Their look can change; the sliders keep their values (docs/process-versions.md) → {changed, process}",
                has_selection,
                |s, p| {
                    const C: &str = "develop.setProcess";
                    let label = "Process Version";
                    let v = p
                        .get("version")
                        .and_then(|v| {
                            v.as_u64().or_else(|| v.as_f64().filter(|x| x.fract() == 0.0 && *x >= 0.0 && *x < f64::from(u32::MAX)).map(|x| x as u64))
                        })
                        .and_then(|v| u32::try_from(v).ok())
                        .map(ProcessVersion)
                        .filter(|v| v.is_known())
                        .ok_or_else(|| bad(C, format!("`version` must be a process version from 1 to {}", ProcessVersion::LATEST.0)))?;
                    let targets = match named_photos(s, p, C)? {
                        Some(ids) => ids,
                        None => s.targets(p)?,
                    };
                    let ops: Vec<Op> = targets
                        .into_iter()
                        .filter_map(|id| s.develop_of(id).map(|d| (id, d)))
                        .filter(|(_, d)| d.process != v)
                        .filter_map(|(id, d)| s.develop_op(id, DevelopSettings { process: v, ..(*d).clone() }, label))
                        .collect();
                    let n = ops.len();
                    if n > 0 {
                        s.commit(label, Op::Batch { ops })?;
                    }
                    Ok(json!({"changed": n, "process": v}))
                }
            )
        },
        // ---- HDR editing
        cmd!(
            "develop.hdr",
            "HDR",
            [],
            None,
            "{enabled?: bool (default: toggle), maxEv?: number 0..5 (headroom limit, stops above SDR white)}: HDR editing: highlights above SDR white for HDR exports and displays; every SDR render uses the SDR rendition (the hdr.sdr* controls)",
            has_active,
            |s, p| {
                let id = active(s, "develop.hdr")?;
                let cur = s.develop_of(id).unwrap_or_default().hdr;
                let enabled = bool_or(p, "enabled", !cur.enabled);
                let max_ev = match p.get("maxEv") {
                    None => None,
                    Some(v) => Some(v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad("develop.hdr", "`maxEv` must be a number"))?),
                };
                let label = if enabled { "HDR On" } else { "HDR Off" };
                edit(s, "develop.hdr", label, |d| {
                    d.hdr.enabled = enabled;
                    if let Some(m) = max_ev {
                        d.hdr.max_ev = m.clamp(0.0, lcc_develop::Hdr::MAX_EV_LIMIT);
                    }
                    Ok(())
                })?;
                Ok(json!({"enabled": enabled, "maxEv": s.develop_of(id).map(|d| d.hdr.max_ev)}))
            }
        ),
        cmd!(
            "develop.hslPick",
            "Pick an HSL Curve Point",
            [],
            None,
            "{channel: hue|sat|lum, x, y (normalized image coords)}: samples the photo's colour there and adds that hue to the channel's HSL curve, at the curve's current height there → {x}",
            has_active,
            |s, p| {
                let c = "develop.hslPick";
                let ch = str_param(p, "channel").ok_or_else(|| bad(c, "missing channel"))?.to_string();
                let (x, y) = (super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?);
                // the hue the curve receives: the photo with the curves themselves neutral (and
                // everything after the mixer, as Point Color reads it)
                let [_, _, h] = super::color::encoded_to_oklch(super::color::probe(s, c, x, y, |d| {
                    d.hsl_curves = Default::default();
                    super::color::after_mixer_neutral(d);
                })?);
                let hue_x = (h as f64 / std::f64::consts::TAU).rem_euclid(1.0);
                edit(s, c, "HSL Curves", |d| {
                    let pts = d.hsl_curves.channel_mut(&ch).ok_or_else(|| bad(c, format!("unknown channel `{ch}`")))?;
                    let at = lcc_pipeline::colorops::hsl_curve_at(pts, hue_x);
                    pts.push(Point::new(hue_x, at));
                    pts.sort_by(|a, b| a.x.total_cmp(&b.x));
                    Ok(())
                })?;
                Ok(json!({"channel": ch, "x": hue_x}))
            }
        ),
        cmd!(
            "develop.hslCurves",
            "Set HSL Curve",
            [],
            None,
            "{channel: hue|sat|lum, points: [[x,y],...]: x hue position 0..1 around the hue circle, y −1..1}",
            has_active,
            |s, p| {
                let ch = str_param(p, "channel").ok_or_else(|| bad("develop.hslCurves", "missing channel"))?.to_string();
                let mut pts: Vec<Point> = p
                    .get("points")
                    .and_then(Value::as_array)
                    .ok_or_else(|| bad("develop.hslCurves", "missing points"))?
                    .iter()
                    .filter_map(|q| Some(Point::new(q.get(0)?.as_f64()?.clamp(0.0, 1.0), q.get(1)?.as_f64()?.clamp(-1.0, 1.0))))
                    .collect();
                // stored in the order the curve runs, as the editor draws and drags them
                pts.sort_by(|a, b| a.x.total_cmp(&b.x));
                edit(s, "develop.hslCurves", "HSL Curves", |d| {
                    let c = d.hsl_curves.channel_mut(&ch).ok_or_else(|| bad("develop.hslCurves", format!("unknown channel `{ch}`")))?;
                    *c = pts;
                    Ok(())
                })
            }
        ),
        // ---- geometry: Upright
        cmd!(
            "geometry.upright",
            "Upright",
            [],
            None,
            "{mode: off|auto|guided|level|vertical|full, again?: bool}: analyses the photo (line segments → vanishing points) and stores the correction; run again to update after lens changes. `again` (a second click on the mode that is on) analyses it afresh and clears Guided's guides; the Transform sliders stay → {mode, transform, note (why the mode found nothing to correct, when it didn't)}",
            has_active,
            |s, p| {
                let mode: Upright =
                    serde_json::from_value(p.get("mode").cloned().unwrap_or(json!("auto"))).map_err(|e| bad("geometry.upright", e.to_string()))?;
                let id = active(s, "geometry.upright")?;
                let d = s.develop_of(id).unwrap_or_default();
                let (transform, note) = match mode {
                    Upright::Off | Upright::Guided => (None, None),
                    m => {
                        let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad("geometry.upright", e))?;
                        let info = s.source_info(id);
                        let aux = lcc_pipeline::source_stage::aux_source(&src, &info, &d);
                        let a = lcc_pipeline::upright::analyze_source(aux.as_deref().unwrap_or(&src), &info, &d);
                        (Some(lcc_pipeline::upright::solve(m, &a)), lcc_pipeline::upright::shortfall(m, &a))
                    }
                };
                let again = bool_or(p, "again", false);
                edit(s, "geometry.upright", "Upright", |d| {
                    // a second click on the mode analyses it afresh (OneSlips, test notes 2026-10-24 note 6);
                    // the Transform sliders stay as set (decision 2026-10-25 item 11)
                    if again && mode == Upright::Guided {
                        d.geometry.guides.clear();
                    }
                    d.geometry.upright = mode;
                    d.geometry.upright_transform = transform;
                    d.geometry.lrc_upright = None;
                    Ok(())
                })?;
                // say why when the mode can't do what it says, instead of nothing happening
                let note = note.map(|why| {
                    let what = match mode {
                        Upright::Level => "No level correction",
                        Upright::Vertical => "No vertical correction",
                        Upright::Full => "No full correction",
                        _ => "No Upright correction",
                    };
                    format!("{what}: {why}. Try Guided: draw lines along what should be vertical.")
                });
                Ok(json!({"mode": mode, "transform": transform, "note": note}))
            }
        ),
        cmd!(
            "geometry.guides",
            "Guided Upright Guides",
            [],
            None,
            "{guides: [[x0,y0,x1,y1], …] (≤ 4, normalized coords of the lens-corrected image), add?: bool (append one guide, dropping the oldest beyond 4)}: switches Upright to Guided",
            has_active,
            |s, p| {
                let parse = |v: &Value| -> Option<(Point, Point)> {
                    let a: Vec<f64> = v.as_array()?.iter().filter_map(Value::as_f64).collect();
                    (a.len() == 4 && a.iter().all(|x| x.is_finite())).then(|| (Point::new(a[0], a[1]), Point::new(a[2], a[3])))
                };
                let guides: Vec<(Point, Point)> =
                    p.get("guides").and_then(Value::as_array).map(|a| a.iter().filter_map(parse).collect()).unwrap_or_default();
                let add = bool_or(p, "add", false);
                edit(s, "geometry.guides", "Guided Upright", |d| {
                    if add {
                        d.geometry.guides.extend(guides);
                    } else {
                        d.geometry.guides = guides;
                    }
                    let n = d.geometry.guides.len();
                    if n > 4 {
                        d.geometry.guides.drain(..n - 4);
                    }
                    d.geometry.upright = Upright::Guided;
                    d.geometry.upright_transform = None;
                    d.geometry.lrc_upright = None;
                    Ok(())
                })
            }
        ),
        // ---- copy / paste / sync
        cmd!(
            "develop.copy",
            "Copy Edit Settings",
            ["Photo", "Develop Settings"],
            Some("Cmd+C"),
            "{groups?: [settingsGroup], settings?: [settingItem] (single settings of a group, develop.settingItems), masks?: [maskId] (with the masks group: only these of the photo's masks, by id (develop.get); default all)} (default: what was ticked last; at first all but crop, masks, remove): the group choice is remembered, the masks one is for this copy only → {groups, settings, masks? (the mask ids copied, when `masks` was given)}",
            has_active,
            |s, p| {
                let c = "develop.copy";
                let id = active(s, c)?;
                let pick = pick_param(p, c)?;
                let only_masks = mask_ids_param(p, c)?;
                let d = s.develop_of(id).unwrap_or_default();
                if let Some(only) = &only_masks {
                    if !pick.as_ref().unwrap_or(&s.dialogs.copy).has(SettingsGroup::Masks.id()) {
                        return Err(bad(c, "`masks` picks which masks to copy: add \"masks\" to `groups`"));
                    }
                    if let Some(unknown) = only.iter().find(|m| !d.masks.iter().any(|x| x.id == **m)) {
                        return Err(bad(c, format!("this photo has no mask {unknown} (develop.get: masks[].id)")));
                    }
                }
                if let Some(pick) = pick
                    && pick != s.dialogs.copy
                {
                    s.dialogs.copy = pick;
                    remember_dialogs(s);
                }
                // the whole settings: Paste Selected Settings can paste a group the copy left out (upstream #3148)
                let mut copied = d.to_json();
                if let Some(only) = &only_masks
                    && let Some(masks) = copied.get_mut("masks").and_then(Value::as_array_mut)
                {
                    // (in the photo's own order, whatever order they were named in)
                    masks.retain(|m| m.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).is_some_and(|mid| only.contains(&mid)));
                }
                // portable: pasted in another catalog, a flat-field map still applies
                s.clipboard = Some(s.portable(&copied));
                s.clipboard_dims = oriented_dims(s, id, d.orientation);
                s.clipboard_from = Some(id);
                let mut out = json!({"groups": s.dialogs.copy.groups, "settings": s.dialogs.copy.settings});
                if let Some(only) = only_masks
                    && let Some(o) = out.as_object_mut()
                {
                    let ids: Vec<u32> = d.masks.iter().map(|m| m.id).filter(|mid| only.contains(mid)).collect();
                    o.insert("masks".into(), json!(ids));
                }
                Ok(out)
            }
        ),
        cmd!(
            "develop.pastePrevious",
            "Paste Settings from Previous",
            ["Photo", "Develop Settings"],
            Some("Cmd+Alt+V"),
            "{ids?, groups?: [settingsGroup], settings?: [settingItem] (default: the copy choice)}: from the photo that was active before this one",
            has_active,
            |s, p| {
                let c = "develop.pastePrevious";
                let prev =
                    s.previous_active.filter(|id| s.catalog.photo(*id).is_some_and(|ph| !ph.deleted)).ok_or_else(|| bad(c, "no previous photo"))?;
                let pick = pick_param(p, c)?.unwrap_or_else(|| s.dialogs.copy.clone());
                let src = s.develop_of(prev).unwrap_or_default();
                let mut partial = pick.extract(&src);
                lcc_develop::presets::to_recipes(&mut partial);
                let from = oriented_dims(s, prev, src.orientation);
                let ids: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|id| *id != prev).collect();
                let link_source = link_copied_masks(s, &mut partial, Some(prev), &ids, "Paste from Previous");
                let mut ops = Vec::new();
                for id in ids {
                    let Some(d) = s.develop_of(id) else { continue };
                    let mut pasted = lcc_develop::apply_partial(&d, &partial, 1.0);
                    refit_pasted_crop(s, &partial, &src.crop, from, id, &mut pasted);
                    let next = crate::smart_masks::completed(s, id, pasted);
                    ops.extend(s.develop_op(id, next, "Paste from Previous"));
                }
                let n = ops.len();
                ops.extend(link_source);
                s.commit("Paste from Previous", Op::Batch { ops })?;
                Ok(json!({"changed": n, "from": prev.0}))
            }
        ),
        cmd!(
            "develop.paste",
            "Paste Edit Settings",
            ["Photo", "Develop Settings"],
            Some("Cmd+V"),
            "{ids?, groups?: [settingsGroup], settings?: [settingItem] (default: what Copy ticked; any part of the copied photo's settings, also one the copy left out: Paste Selected Settings)} → {changed, vignettingCalibrationSkipped: [{id, reason}] (a vignetting map left off a photo it doesn't fit)}",
            has_clipboard,
            |s, p| {
                let c = "develop.paste";
                let mut whole = s.clipboard.clone().ok_or_else(|| bad(c, "no settings copied"))?;
                // (copied in another catalog, the map comes with it: stored with the paste)
                s.absorb_settings(&mut whole);
                // the clipboard holds the copied photo's whole settings: what Copy ticked, or any part of them
                // Paste Selected Settings names, also a group the copy left out (upstream #3148)
                let pick = pick_param(p, c)?.unwrap_or_else(|| s.dialogs.copy.clone());
                let src = lcc_develop::apply_partial(&DevelopSettings::default(), &whole, 1.0);
                let mut clip = pick.extract(&src);
                // (the copied masks go as they were copied)
                if let (Some(m), Some(o)) = (whole.get("masks").cloned(), clip.as_object_mut())
                    && o.contains_key("masks")
                {
                    o.insert("masks".into(), m);
                }
                lcc_develop::presets::to_recipes(&mut clip);
                let from_dims = s.clipboard_dims;
                let ids = s.targets(p)?;
                let from = s.clipboard_from;
                let link_source = link_copied_masks(s, &mut clip, from, &ids, "Paste Settings");
                let mut ops = Vec::new();
                let mut vignetting_calibration_skipped = Vec::new();
                for id in ids {
                    let Some(d) = s.develop_of(id) else { continue };
                    let mut pasted = lcc_develop::apply_partial(&d, &clip, 1.0);
                    refit_pasted_crop(s, &clip, &src.crop, from_dims, id, &mut pasted);
                    // (a vignetting map from another lens stays off this photo)
                    if let Some(why) = s.keep_fitting_vignetting_calibration(id, &d, &mut pasted) {
                        vignetting_calibration_skipped.push(json!({"id": id.0, "reason": why}));
                    }
                    crate::upright_jobs::as_recipe(&d, &mut pasted);
                    let next = crate::smart_masks::completed(s, id, pasted);
                    ops.extend(s.develop_op(id, next, "Paste Settings"));
                }
                let n = ops.len();
                ops.extend(link_source);
                s.commit("Paste Settings", Op::Batch { ops })?;
                Ok(json!({"changed": n, "vignettingCalibrationSkipped": vignetting_calibration_skipped}))
            }
        ),
        cmd!(
            "develop.sync",
            "Sync Settings",
            [],
            None,
            "{groups?: [settingsGroup], settings?: [settingItem], controls?: [controlId], copies?: bool}: copy the active photo's settings to the other selected photos (with `copies`: to the master and every virtual copy of the active photo instead; Sync Copies): whole groups, single settings of a group (develop.settingItems) and single controls (e.g. [\"light.exposure\"]); with only `controls`, nothing else is copied (default: what Sync ticked last, at first all but crop, masks, remove; the choice is remembered) → {changed, vignettingCalibrationSkipped: [{id, reason}] (a vignetting map left off a photo it doesn't fit: another lens or camera, a zoom at another focal length, another image area)}",
            has_selection,
            |s, p| {
                let c = "develop.sync";
                let id = active(s, c)?;
                let controls: Vec<&'static str> = match p.get("controls") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .and_then(lcc_develop::controls::find)
                                .map(|spec| spec.id)
                                .ok_or_else(|| bad(c, format!("unknown control `{v}`")))
                        })
                        .collect::<Result<_>>()?,
                    Some(_) => return Err(bad(c, "controls must be a list of control ids")),
                };
                let pick = match pick_param(p, c)? {
                    Some(pick) => {
                        if pick != s.dialogs.sync {
                            s.dialogs.sync = pick.clone();
                            remember_dialogs(s);
                        }
                        pick
                    }
                    None if controls.is_empty() => s.dialogs.sync.clone(),
                    None => SettingsPick::default(),
                };
                let d = s.develop_of(id).unwrap_or_default();
                let mut partial = pick.extract(&d);
                lcc_develop::presets::to_recipes(&mut partial);
                let values: Vec<(&str, f64)> = controls.iter().filter_map(|k| lcc_develop::controls::get(&d, k).map(|v| (*k, v))).collect();
                let targets: Vec<PhotoId> = if p.get("copies").and_then(Value::as_bool).unwrap_or(false) {
                    let master = s.catalog.photo(id).map(|ph| ph.copy_of.unwrap_or(ph.id)).unwrap_or(id);
                    s.catalog
                        .photos()
                        .filter(|ph| !ph.deleted && ph.id != id && (ph.id == master || ph.copy_of == Some(master)))
                        .map(|ph| ph.id)
                        .collect()
                } else {
                    s.selection.ids.iter().copied().filter(|x| *x != id).collect()
                };
                let link_source = link_copied_masks(s, &mut partial, Some(id), &targets, "Sync Settings");
                let mut ops = Vec::new();
                let mut vignetting_calibration_skipped = Vec::new();
                // a crop goes on as what it means on each photo's own frame (upstream #3572; as Auto Sync does)
                let adims = oriented_dims(s, id, d.orientation);
                for x in targets.iter().copied() {
                    let Some(dd) = s.develop_of(x) else { continue };
                    let mut nd = lcc_develop::apply_partial(&dd, &partial, 1.0);
                    // (a vignetting map from another lens stays off this photo)
                    if let Some(why) = s.keep_fitting_vignetting_calibration(x, &dd, &mut nd) {
                        vignetting_calibration_skipped.push(json!({"id": x.0, "reason": why}));
                    }
                    refit_pasted_crop(s, &partial, &d.crop, adims, x, &mut nd);
                    for (k, v) in &values {
                        lcc_develop::controls::set(&mut nd, k, *v);
                    }
                    crate::upright_jobs::as_recipe(&dd, &mut nd);
                    let nd = crate::smart_masks::completed(s, x, nd);
                    ops.extend(s.develop_op(x, nd, "Sync Settings"));
                }
                let n = ops.len();
                ops.extend(link_source);
                s.commit("Sync Settings", Op::Batch { ops })?;
                Ok(json!({"changed": n, "vignettingCalibrationSkipped": vignetting_calibration_skipped}))
            }
        ),
        cmd!(
            "develop.settingItems",
            "List Settings to Copy",
            [],
            None,
            "{} → {groups: [{group, label, settings: [{id, label}]}], copy, sync, newPreset, presetGroup}: the settings groups and the single settings inside them that copy, sync and presets can pick, and what Copy, Sync and New Preset tick now",
            always,
            |s, _| {
                let groups: Vec<Value> = SettingsGroup::ALL
                    .iter()
                    .map(|g| {
                        json!({
                            "group": g.id(),
                            "label": g.label(),
                            "settings": g.items().iter().map(|i| json!({"id": i.id, "label": i.label})).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(json!({
                    "groups": groups,
                    "copy": s.dialogs.copy,
                    "sync": s.dialogs.sync,
                    "newPreset": s.dialogs.new_preset,
                    "presetGroup": s.dialogs.preset_group,
                }))
            }
        ),
        cmd!(
            "develop.quickAdjust",
            "Quick Develop",
            [],
            None,
            "{control: develop control id, delta, ids?}: add `delta` to the control on every target photo (each from its own value; Quick Develop) in one undo step → {changed}",
            has_selection,
            |s, p| {
                let c = "develop.quickAdjust";
                let ctl = str_param(p, "control").ok_or_else(|| bad(c, "missing control"))?.to_string();
                let spec = controls::find(&ctl).ok_or_else(|| bad(c, format!("unknown control `{ctl}`")))?;
                let delta = f64_req(p, "delta", c)?;
                let label = format!("Quick Develop: {}", spec.label);
                let ops: Vec<Op> = s
                    .targets(p)?
                    .into_iter()
                    .filter_map(|id| s.develop_of(id).map(|d| (id, d)))
                    .filter_map(|(id, d)| {
                        // each photo on its own scale: Kelvin (an old relative value converted first) or relative
                        let nd = nudge(&s.source_info(id), crop_dims(s, id), &d, &ctl, delta, spec.default);
                        s.develop_op(id, nd, &label)
                    })
                    .collect();
                let n = ops.len();
                if n > 0 {
                    s.commit(&label, Op::Batch { ops })?;
                }
                Ok(json!({"changed": n}))
            }
        ),
        cmd!(
            "develop.matchExposure",
            "Match Total Exposures",
            ["Photo", "Develop Settings"],
            None,
            "{ids?}: set each selected photo's Exposure so its total exposure (shutter × ISO ÷ aperture², plus the slider) equals the active photo's; photos without exposure data are skipped → {changed, skipped}",
            has_selection,
            |s, p| {
                let c = "develop.matchExposure";
                let reference = active(s, c)?;
                // log2 of the light the photo gathered (shutter s × ISO / f-number²)
                let gathered = |s: &Session, id: PhotoId| -> Option<f64> {
                    let m = &s.catalog.photo(id)?.meta;
                    let t = m.shutter_seconds()?;
                    let (n, iso) = (m.aperture? as f64, m.iso? as f64);
                    (n > 0.0 && iso > 0.0).then(|| (t * iso / (n * n)).log2())
                };
                let r_ev = gathered(s, reference).ok_or_else(|| bad(c, "the active photo has no shutter / aperture / ISO"))?;
                let r_slider = s.develop_of(reference).map(|d| d.light.exposure).unwrap_or(0.0);
                let ids: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|x| *x != reference).collect();
                let (mut ops, mut skipped) = (Vec::new(), Vec::new());
                for id in ids {
                    let (Some(ev), Some(d)) = (gathered(s, id), s.develop_of(id)) else {
                        skipped.push(id.0);
                        continue;
                    };
                    let mut nd = (*d).clone();
                    nd.light.exposure = (r_slider + r_ev - ev).clamp(-5.0, 5.0);
                    ops.extend(s.develop_op(id, nd, "Match Total Exposures"));
                }
                let n = ops.len();
                if n > 0 {
                    s.commit("Match Total Exposures", Op::Batch { ops })?;
                }
                Ok(json!({"changed": n, "skipped": skipped}))
            }
        ),
        cmd!(
            "develop.autoSync",
            "Auto Sync",
            ["Photo", "Develop Settings"],
            Some("Cmd+Alt+Shift+A"),
            "{on?: bool} (toggles when omitted): edits to the active photo also change the other selected photos (only the settings that changed; not spot removal / red eye) → {on}",
            always,
            |s, p| {
                s.auto_sync = p.get("on").and_then(Value::as_bool).unwrap_or(!s.auto_sync);
                Ok(json!({"on": s.auto_sync}))
            }
        ),
        // ---- presets
        cmd!(
            "preset.apply",
            "Apply Preset",
            [],
            None,
            "{id: presetId, amount?: 0..200 (percent), ids?, followAutoSync?: bool (true, as Develop's Presets panel: the photos a slider changes, the active photo, or every selected photo with Auto Sync on; default: the selection)}",
            has_selection,
            |s, p| {
                let pid = str_param(p, "id").ok_or_else(|| bad("preset.apply", "missing id"))?;
                let mut preset =
                    s.presets.iter().find(|x| x.id == pid).cloned().ok_or_else(|| bad("preset.apply", format!("unknown preset `{pid}`")))?;
                // a preset's flat-field map is stored with the edit that applies it
                s.absorb_settings(&mut preset.settings);
                let amount = f64_or(p, "amount", 100.0).clamp(0.0, 200.0) / 100.0;
                // `id` names the preset here: the photos are `ids`, else the selection, or (Develop's
                // Presets panel) the photos a slider would change
                // (an active photo outside the selection is not edited: the selection then, as before)
                let follow = bool_or(p, "followAutoSync", false) && s.active().is_some_and(|a| s.selection.ids.contains(&a));
                let ids =
                    if p.get("ids").is_none_or(Value::is_null) && follow { s.slider_targets() } else { s.targets(&json!({"ids": p.get("ids")}))? };
                // its masks applied to several photos in one go are linked: one link per mask of the preset
                let links: Vec<String> = if ids.len() > 1 {
                    let n = preset.settings.get("masks").and_then(Value::as_array).map_or(0, Vec::len);
                    (0..n).map(|k| crate::new_mask_link(k as u64)).collect()
                } else {
                    Vec::new()
                };
                let mut ops = Vec::new();
                for id in ids {
                    // the same composition the Presets panel previews (`Session::preset_applied`)
                    let Some(mut next) = s.preset_applied(id, &preset, amount) else { continue };
                    let own = |m: &&mut lcc_develop::Mask| m.preset.as_deref() == Some(preset.id.as_str());
                    for (m, l) in next.masks.iter_mut().filter(own).zip(&links) {
                        m.link = l.clone();
                    }
                    let next = crate::smart_masks::completed(s, id, next);
                    ops.extend(s.develop_op(id, next, &format!("Preset: {}", preset.name)));
                }
                s.commit(&format!("Preset: {}", preset.name), Op::Batch { ops })?;
                ok()
            }
        ),
        cmd!(
            "preset.create",
            "Create Preset",
            ["Photo"],
            None,
            "{name, group?, groups?: [settingsGroup], settings?: [settingItem]} (default: what New Preset ticked last; White Balance only once ticked): a group named like an existing one (any case, spaces trimmed) is that group; the choice is remembered → {id, group}",
            has_active,
            |s, p| {
                let c = "preset.create";
                let id = active(s, c)?;
                let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?.to_string();
                let typed = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).unwrap_or(crate::app_store::DEFAULT_PRESET_GROUP);
                let group = existing_preset_group(s, typed);
                let pick = pick_param(p, c)?.unwrap_or_else(|| s.dialogs.new_preset.clone());
                if pick.is_empty() {
                    return Err(bad(c, "tick at least one setting to save in the preset"));
                }
                if pick != s.dialogs.new_preset || group != s.dialogs.preset_group {
                    s.dialogs.new_preset = pick.clone();
                    s.dialogs.preset_group = group.clone();
                    remember_dialogs(s);
                }
                let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
                // a mask still under its automatic name ("Radial Gradient", "Groom Jamie - Clothes") is
                // saved unnamed, so applying the preset names it after the preset; a name the
                // photographer gave it ("Warmth") is kept
                for k in 0..d.masks.len() {
                    let Some(m) = d.masks.get(k).cloned() else { continue };
                    // (numbered too: "Brush 2" next to another Brush)
                    if lcc_develop::recipe::numbered_name(&m.name, &s.auto_mask_name(id, &m))
                        && let Some(m) = d.masks.get_mut(k)
                    {
                        m.name.clear();
                    }
                }
                let base = format!("user.{}", crate::presets::slug(&format!("{group}-{name}")));
                let mut pid = base.clone();
                let mut n = 2u32;
                while s.presets.iter().any(|x| x.id == pid) && n < 100_000 {
                    pid = format!("{base}-{n}");
                    n += 1;
                }
                // presets serve every catalog: a flat-field map goes with the preset
                let settings = s.portable(&pick.extract(&d));
                let preset = Preset { settings, ..Preset::from_settings(&pid, &name, &group, &d, &[]) };
                s.presets.push(preset);
                Ok(json!({"id": pid, "group": group}))
            }
        ),
        cmd!(
            "preset.saveRecipe",
            "Save Import Recipe as Preset",
            [],
            None,
            "{name (up to 100 characters), group?: default Import Recipes, recipe: [{id: a plain develop control (develop.controls; no white balance, geometry or crop), value}]} → {id, name}: save a batch-edit recipe (as library.import takes it) as a user preset, so it can be reused from the Preset list; only the settings the recipe names are stored, so applying it later never overrides the rest. The preset gets an id no other preset has; saving the same recipe under the same name and group again returns the preset that already is",
            always,
            |s, p| {
                const C: &str = "preset.saveRecipe";
                let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing name"))?.to_string();
                let group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).unwrap_or("Import Recipes").to_string();
                if name.chars().count() > MAX_PRESET_TEXT || group.chars().count() > MAX_PRESET_TEXT {
                    return Err(bad(C, format!("a name or group is at most {MAX_PRESET_TEXT} characters")));
                }
                let recipe = super::recipe_param(p, C)?;
                if recipe.is_empty() {
                    return Err(bad(C, "the recipe is empty"));
                }
                let settings = recipe_partial(&recipe).ok_or_else(|| bad(C, "the recipe sets nothing a preset can store"))?;
                // presets get their ids as imported ones do (`user.<group>-<name>`, then -2, -3…): an id from the list's
                // length repeats once a preset is deleted, and the first preset with an id is the one that gets applied
                let preset = Preset { id: String::new(), name, group, settings, favorite: false, builtin: false };
                let id = match s.add_presets(vec![preset.clone()]).into_iter().next() {
                    Some(id) => id,
                    // the same recipe under the same name and group is already a preset
                    None => s
                        .presets
                        .iter()
                        .find(|q| q.name == preset.name && q.group == preset.group && q.settings == preset.settings)
                        .map(|q| q.id.clone())
                        .ok_or_else(|| bad(C, "the preset could not be saved"))?,
                };
                Ok(json!({"id": id, "name": preset.name}))
            }
        ),
        cmd!("preset.delete", "Delete Preset", [], None, "{id}", always, |s, p| {
            let pid = str_param(p, "id").ok_or_else(|| bad("preset.delete", "missing id"))?;
            let i = s.presets.iter().position(|x| x.id == pid).ok_or_else(|| bad("preset.delete", "unknown preset"))?;
            if s.presets[i].builtin {
                return Err(bad("preset.delete", "built-in presets can't be deleted"));
            }
            s.presets.remove(i);
            s.preset_at_import.retain(|x| x.as_str() != pid);
            s.preset_hidden.retain(|x| x.as_str() != pid);
            ok()
        }),
        cmd!("preset.favorite", "Favorite Preset", [], None, "{id, favorite?: bool}", always, |s, p| {
            let pid = str_param(p, "id").ok_or_else(|| bad("preset.favorite", "missing id"))?;
            let pr = s.presets.iter_mut().find(|x| x.id == pid).ok_or_else(|| bad("preset.favorite", "unknown preset"))?;
            pr.favorite = bool_or(p, "favorite", !pr.favorite);
            ok()
        }),
        cmd!(
            "preset.forImport",
            "Mark Preset At Import",
            [],
            None,
            "{id, on?: bool}: the import pickers list presets marked At Import first",
            always,
            |s, p| { toggle_mark(s, p, "preset.forImport", "on", |s| &mut s.preset_at_import) }
        ),
        cmd!(
            "preset.hide",
            "Hide Preset",
            [],
            None,
            "{id, hidden?: bool}: out of the Presets panel and the pickers, kept in the Presets Manager",
            always,
            |s, p| { toggle_mark(s, p, "preset.hide", "hidden", |s| &mut s.preset_hidden) }
        ),
        cmd!(
            "preset.rename",
            "Rename Preset",
            [],
            None,
            "{id, name}: built-in presets too (preset.restoreBuiltins gives them their names back)",
            always,
            |s, p| {
                let c = "preset.rename";
                let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?.to_string();
                edit_preset(s, p, c, true, name)?;
                ok()
            }
        ),
        cmd!("preset.move", "Move Preset to Group", [], None, "{id, group}: built-in presets too", always, |s, p| {
            let c = "preset.move";
            let group = str_param(p, "group").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing group"))?;
            // (a group named like one there is is that group: never two "Wedding" groups)
            let group = existing_preset_group(s, group);
            edit_preset(s, p, c, false, group)?;
            ok()
        }),
        cmd!(
            "preset.hideGroup",
            "Hide Preset Group",
            [],
            None,
            "{group, hidden?: bool}: every preset in the group (and the groups under it) out of the Presets panel and the pickers, or back → {hidden, count}",
            always,
            |s, p| {
                let c = "preset.hideGroup";
                let group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).ok_or_else(|| bad(c, "missing group"))?;
                let ids: Vec<String> = s.presets.iter().filter(|q| in_group(q, group)).map(|q| q.id.clone()).collect();
                if ids.is_empty() {
                    return Err(bad(c, format!("no presets in `{group}`")));
                }
                let all_hidden = ids.iter().all(|id| s.preset_hidden.contains(id));
                let hidden = bool_or(p, "hidden", !all_hidden);
                s.preset_hidden.retain(|h| !ids.contains(h));
                if hidden {
                    s.preset_hidden.extend(ids.iter().cloned());
                }
                s.presets.bump();
                Ok(json!({"hidden": hidden, "count": ids.len()}))
            }
        ),
        cmd!(
            "preset.restoreBuiltins",
            "Restore Built-in Presets",
            [],
            None,
            "{}: built-in presets get their own names and groups back and are shown again → {restored}",
            always,
            |s, _p| {
                let builtin: std::collections::HashSet<String> = s.presets.iter().filter(|q| q.builtin).map(|q| q.id.clone()).collect();
                let before = s.preset_builtin_names.len() + s.preset_builtin_groups.len();
                let hidden = s.preset_hidden.len();
                s.preset_hidden.retain(|h| !builtin.contains(h));
                s.preset_builtin_names.clear();
                s.preset_builtin_groups.clear();
                s.apply_builtin_edits();
                Ok(json!({"restored": before + hidden - s.preset_hidden.len()}))
            }
        ),
        cmd!("preset.createGroup", "Create Preset Folder", [], None, "{name, parent?: group}", always, |s, p| {
            let c = "preset.createGroup";
            let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?;
            if name.contains('/') {
                return Err(bad(c, "folder name cannot contain slashes"));
            }
            let parent = str_param(p, "parent").map(str::trim).filter(|p| !p.is_empty());
            let group = match parent {
                Some(par) => format!("{par}/{name}"),
                None => name.to_string(),
            };
            if crate::presets::builtin().iter().any(|b| b.group == group) {
                return Err(bad(c, format!("`{group}` is a built-in group")));
            }
            if !s.preset_folders.iter().any(|f| f == &group) {
                s.preset_folders.push(group.clone());
                s.presets.bump();
            }
            Ok(json!({"group": group}))
        }),
        cmd!("preset.renameGroup", "Rename Preset Folder", [], None, "{group, name}: built-in groups too", always, |s, p| {
            let c = "preset.renameGroup";
            let old_group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).ok_or_else(|| bad(c, "missing group"))?.to_string();
            let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?;
            if name.contains('/') {
                return Err(bad(c, "folder name cannot contain slashes"));
            }
            let new_group = match old_group.rsplit_once('/') {
                Some((parent, _)) => format!("{parent}/{name}"),
                None => name.to_string(),
            };
            if old_group != new_group {
                regroup(s, &old_group, &new_group);
            }
            Ok(json!({"group": new_group}))
        }),
        cmd!("preset.moveGroup", "Move Preset Folder", [], None, "{group, parent?: group | null}: built-in groups too", always, |s, p| {
            let c = "preset.moveGroup";
            let old_group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).ok_or_else(|| bad(c, "missing group"))?.to_string();
            let parent = str_param(p, "parent").map(str::trim).filter(|p| !p.is_empty());
            if let Some(par) = parent
                && (par == old_group || par.starts_with(&format!("{old_group}/")))
            {
                return Err(bad(c, "cannot move a folder into itself or its subfolder"));
            }
            let leaf = old_group.rsplit_once('/').map(|(_, l)| l).unwrap_or(&old_group);
            let new_group = match parent {
                Some(par) => format!("{par}/{leaf}"),
                None => leaf.to_string(),
            };
            if old_group != new_group {
                regroup(s, &old_group, &new_group);
            }
            Ok(json!({"group": new_group}))
        }),
        cmd!(
            "preset.copy",
            "Copy Presets to Folder",
            [],
            None,
            "{ids: [presetId], group} → {ids: [new presetId], skipped}: user presets with the same settings under a new id, built-in presets included, unstarred; a preset identical to one already in the folder is skipped",
            always,
            |s, p| {
                let c = "preset.copy";
                let group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).ok_or_else(|| bad(c, "missing group"))?;
                let group = existing_preset_group(s, group);
                let ids = super::metadata_files::strings(p, "ids").filter(|v| !v.is_empty()).ok_or_else(|| bad(c, "missing ids"))?;
                let copies: Vec<Preset> = s
                    .presets
                    .iter()
                    .filter(|x| ids.contains(&x.id))
                    .map(|x| Preset {
                        id: String::new(),
                        name: x.name.clone(),
                        group: group.clone(),
                        settings: x.settings.clone(),
                        favorite: false,
                        builtin: false,
                    })
                    .collect();
                if copies.is_empty() {
                    return Err(bad(c, "none of these presets exist"));
                }
                let asked = copies.len();
                let added = s.add_presets(copies);
                let skipped = asked.saturating_sub(added.len());
                s.presets.bump();
                Ok(json!({"ids": added, "skipped": skipped}))
            }
        ),
        cmd!(
            "preset.deleteMany",
            "Delete Presets",
            [],
            None,
            "{ids: [presetId]} → {deleted, skipped}: user presets; built-in and unknown ids are skipped",
            always,
            |s, p| {
                let c = "preset.deleteMany";
                let ids = super::metadata_files::strings(p, "ids").filter(|v| !v.is_empty()).ok_or_else(|| bad(c, "missing ids"))?;
                let gone: std::collections::HashSet<&String> =
                    s.presets.iter().filter(|q| !q.builtin && ids.contains(&q.id)).map(|q| &q.id).collect();
                let gone: Vec<String> = gone.into_iter().cloned().collect();
                s.presets.retain(|q| !gone.contains(&q.id));
                s.preset_at_import.retain(|x| !gone.contains(x));
                s.preset_hidden.retain(|x| !gone.contains(x));
                s.presets.bump();
                Ok(json!({"deleted": gone.len(), "skipped": ids.len().saturating_sub(gone.len())}))
            }
        ),
        cmd!(
            "preset.deleteGroup",
            "Delete Preset Folder",
            [],
            None,
            "{group, expect?: how many presets it should hold}: refused when `expect` is given and wrong",
            always,
            |s, p| {
                let c = "preset.deleteGroup";
                let group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty()).ok_or_else(|| bad(c, "missing group"))?;
                if s.presets.iter().any(|q| q.builtin && in_group(q, group)) {
                    return Err(bad(c, "the group holds built-in presets, which can't be deleted: hide the group instead"));
                }
                if let Some(expect) = p.get("expect").and_then(Value::as_u64) {
                    let held = s.presets.iter().filter(|q| in_group(q, group)).count() as u64;
                    if held != expect {
                        return Err(bad(c, format!("the folder holds {held} presets, not {expect}: nothing deleted")));
                    }
                }
                let prefix = format!("{group}/");
                s.preset_folders.retain(|f| f != group && !f.starts_with(&prefix));
                let gone: Vec<String> = s.presets.iter().filter(|q| in_group(q, group)).map(|q| q.id.clone()).collect();
                let removed = gone.len();
                s.presets.retain(|q| !gone.contains(&q.id));
                s.preset_at_import.retain(|x| !gone.contains(x));
                s.preset_hidden.retain(|x| !gone.contains(x));
                s.presets.bump();
                Ok(json!({"deleted": removed}))
            }
        ),
        cmd!(
            "preset.update",
            "Update Preset with Current Settings",
            [],
            None,
            "{id, groups?: [settingsGroup], settings?: [settingItem] (default: the settings the preset already has)}: from the active photo; keeps the preset's name, group, place, favourite and marks; preset.undoUpdate puts the old settings back → {id, groups, settings}",
            has_active,
            |s, p| {
                let c = "preset.update";
                let id = active(s, c)?;
                let current = s.develop_of(id).unwrap_or_default();
                let pick = pick_param(p, c)?;
                let pr = user_preset(s, p, c)?;
                let pick = pick.unwrap_or_else(|| SettingsPick::of_partial(&pr.settings));
                if pick.is_empty() {
                    return Err(bad(c, "tick at least one setting to keep in the preset"));
                }
                let before = std::mem::replace(&mut pr.settings, pick.extract(&current));
                let pid = pr.id.clone();
                s.preset_undo = Some((pid.clone(), before));
                Ok(json!({"id": pid, "groups": pick.groups, "settings": pick.settings}))
            }
        ),
        cmd!(
            "preset.undoUpdate",
            "Undo Update Preset",
            [],
            None,
            "{}: put back the settings the last Update with Current Settings replaced",
            |s| if s.preset_undo.is_some() { Ok(()) } else { Err("no preset update to undo".into()) },
            |s, _| {
                let c = "preset.undoUpdate";
                let (pid, before) = s.preset_undo.take().ok_or_else(|| bad(c, "no preset update to undo"))?;
                let pr = s.presets.iter_mut().find(|x| x.id == pid).ok_or_else(|| bad(c, "the preset was deleted"))?;
                pr.settings = before;
                Ok(json!({"id": pid}))
            }
        ),
        cmd!(
            "preset.settings",
            "Preset Settings",
            [],
            None,
            "{id} → {groups, settings, ids}: what the preset holds, as the per-setting ticks (Update Preset's dialog starts from it)",
            always,
            |s, p| {
                let c = "preset.settings";
                let pid = str_param(p, "id").ok_or_else(|| bad(c, "missing id"))?;
                let pr = s.presets.iter().find(|x| x.id == pid).ok_or_else(|| bad(c, format!("unknown preset `{pid}`")))?;
                let pick = SettingsPick::of_partial(&pr.settings);
                Ok(json!({"groups": pick.groups, "settings": pick.settings, "ids": pick.to_ids()}))
            }
        ),
        // ---- versions & history
        cmd!("version.create", "Create Version", ["Photo", "Develop Settings"], None, "{name?}", has_active, |s, p| {
            let id = active(s, "version.create")?;
            let ph = s.catalog.photo(id).cloned().ok_or_else(|| bad("version.create", "no photo"))?;
            let mut versions = ph.versions.clone();
            let name = str_param(p, "name").map(str::to_string).unwrap_or_else(|| format!("Version {}", versions.len() + 1));
            versions.push(Version { name, created: (s.clock)(), settings: ph.develop.clone(), auto: false });
            s.commit("Create Version", Op::SetVersions { id, versions })?;
            ok()
        }),
        cmd!("version.restore", "Restore Version", [], None, "{index}", has_active, |s, p| {
            let id = active(s, "version.restore")?;
            let i = index_req(p, "index", "version.restore")?;
            let v = s.catalog.photo(id).and_then(|ph| ph.versions.get(i).cloned()).ok_or_else(|| bad("version.restore", "no such version"))?;
            s.set_develop(id, (*v.settings).clone(), &format!("Restore {}", v.name))?;
            ok()
        }),
        cmd!("version.delete", "Delete Version", [], None, "{index}", has_active, |s, p| {
            let id = active(s, "version.delete")?;
            let i = index_req(p, "index", "version.delete")?;
            let mut versions = s.catalog.photo(id).map(|ph| ph.versions.clone()).unwrap_or_default();
            if i >= versions.len() {
                return Err(bad("version.delete", "no such version"));
            }
            versions.remove(i);
            s.commit("Delete Version", Op::SetVersions { id, versions })?;
            ok()
        }),
        cmd!("version.rename", "Rename Version", [], None, "{index, name}", has_active, |s, p| {
            let c = "version.rename";
            let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?.to_string();
            edit_version(s, p, c, "Rename Version", |v, _| v.name = name)
        }),
        cmd!(
            "version.sync",
            "Sync Snapshots",
            ["Photo", "Develop Settings"],
            None,
            "{groups?: [settingsGroup], settings?: [settingItem]}: apply those settings of the active photo's current edit to each of its snapshots (versions), in one undo step (default: what Sync ticked last) → {changed}",
            has_active,
            |s, p| {
                let c = "version.sync";
                let id = active(s, c)?;
                let pick = pick_param(p, c)?.unwrap_or_else(|| s.dialogs.sync.clone());
                let ph = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
                if ph.versions.is_empty() {
                    return Err(bad(c, "the photo has no snapshots"));
                }
                let mut partial = pick.extract(&s.develop_of(id).unwrap_or_default());
                lcc_develop::presets::to_recipes(&mut partial);
                let mut versions = ph.versions.clone();
                for v in &mut versions {
                    v.settings = std::sync::Arc::new(lcc_develop::apply_partial(&v.settings, &partial, 1.0));
                }
                let n = versions.len();
                s.commit("Sync Snapshots", Op::SetVersions { id, versions })?;
                Ok(json!({"changed": n}))
            }
        ),
        cmd!("version.update", "Update Version with Current Settings", [], None, "{index}", has_active, |s, p| {
            edit_version(s, p, "version.update", "Update Version", |v, current| v.settings = current)
        }),
        cmd!("history.clear", "Clear History", [], None, "{}: keeps the current settings as the only step", has_active, |s, _| {
            let id = active(s, "history.clear")?;
            let d = s.develop_of(id).unwrap_or_default();
            let history = vec![lcc_catalog::HistoryStep { label: "Cleared History".into(), settings: d, batch: None }];
            s.commit("Clear History", Op::Batch { ops: vec![Op::SetHistory { id, history }, Op::SetHistoryCursor { id, cursor: None }] })?;
            ok()
        }),
        cmd!(
            "history.restore",
            "Go to History Step",
            [],
            None,
            "{index}: loads that step's settings and moves the History cursor there; later steps stay listed until a new edit",
            has_active,
            |s, p| {
                let c = "history.restore";
                let id = active(s, c)?;
                let i = index_req(p, "index", c)?;
                go_to_history_step(s, c, id, i)
            }
        ),
        cmd!("history.previous", "Previous History Step", [], None, "{}: steps the History cursor back one step", has_history_before, |s, _| {
            let c = "history.previous";
            let id = active(s, c)?;
            let i = s
                .catalog
                .photo(id)
                .and_then(|ph| ph.history_index())
                .and_then(|i| i.checked_sub(1))
                .ok_or_else(|| bad(c, "at the first History step"))?;
            go_to_history_step(s, c, id, i)
        }),
        cmd!("history.next", "Next History Step", [], None, "{}: steps the History cursor forward one step", has_history_after, |s, _| {
            let c = "history.next";
            let id = active(s, c)?;
            let ph = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
            let i = ph
                .history_index()
                .map(|i| i.saturating_add(1))
                .filter(|i| *i < ph.history.len())
                .ok_or_else(|| bad(c, "at the latest History step"))?;
            go_to_history_step(s, c, id, i)
        }),
        cmd!(
            "history.latest",
            "Return to Latest History Step",
            [],
            None,
            "{}: loads the latest step's settings (after editing at an earlier one)",
            has_history_after,
            |s, _| {
                let c = "history.latest";
                let id = active(s, c)?;
                let last = s.catalog.photo(id).and_then(|ph| ph.history.len().checked_sub(1)).ok_or_else(|| bad(c, "no history"))?;
                go_to_history_step(s, c, id, last)
            }
        ),
        cmd!(
            "history.editAt",
            "Edit at History Step",
            [],
            None,
            "{index, control?: controlId, value?: number, values?: {controlId: number}}: changes controls as of step `index`: on that step's group's section it replaces the group's later steps, otherwise it starts a new group there; every later group is kept and replayed on top (one undo step); the settings stay at the new step",
            has_active,
            |s, p| {
                let c = "history.editAt";
                let id = active(s, c)?;
                let i = index_req(p, "index", c)?;
                let base = s.catalog.photo(id).and_then(|ph| ph.history.get(i)).map(|h| h.settings.clone()).ok_or_else(|| bad(c, "no such step"))?;
                let mut vals: Vec<(String, f64)> = Vec::new();
                if let (Some(k), Some(v)) = (str_param(p, "control"), p.get("value").and_then(Value::as_f64)) {
                    vals.push((k.to_string(), v));
                }
                for (k, v) in p.get("values").and_then(Value::as_object).into_iter().flatten() {
                    vals.push((k.clone(), v.as_f64().ok_or_else(|| bad(c, format!("`{k}` must be a number")))?));
                }
                if vals.is_empty() {
                    return Err(bad(c, "give `control`+`value` or `values`"));
                }
                let mut d = (*base).clone();
                for (k, v) in &vals {
                    if controls::find(k).is_none() {
                        return Err(bad(c, format!("unknown control `{k}`")));
                    }
                    controls::set(&mut d, k, *v);
                }
                let label = match vals.as_slice() {
                    [(k, _)] => controls::find(k).map(|c| c.label).unwrap_or("Edit").to_string(),
                    _ => "Edit".into(),
                };
                let op = s.history_rewrite_op(id, i, &std::sync::Arc::new(d), &label).ok_or_else(|| bad(c, "no such step"))?;
                s.commit(&label, op)?;
                ok()
            }
        ),
        // ---- interactions (slider drags, brush strokes)
        cmd!(
            "develop.beginInteraction",
            "Begin Interaction",
            [],
            None,
            "{label, ids?}: ids: more photos develop.adjust moves along, live (a multi-photo edit; one undo step)",
            has_active,
            |s, p| {
                let ids: Vec<PhotoId> = p
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_u64).take(100_000).map(PhotoId).collect())
                    .unwrap_or_default();
                s.begin_interaction_with(str_param(p, "label").unwrap_or("Edit"), &ids)?;
                ok()
            }
        ),
        cmd!("develop.endInteraction", "End Interaction", [], None, "{}", always, |s, _| {
            s.end_interaction()?;
            ok()
        }),
        cmd!("develop.cancelInteraction", "Cancel Interaction", [], None, "{}", always, |s, _| {
            s.cancel_interaction()?;
            ok()
        }),
    ]
}

/// Load History step `i` of photo `id` and put the cursor there, without adding a step: the later
/// steps stay listed (one undo step).
fn go_to_history_step(s: &mut Session, c: &str, id: PhotoId, i: usize) -> Result<Value> {
    let ph = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
    let st = ph.history.get(i).cloned().ok_or_else(|| bad(c, "no such step"))?;
    let cursor = (i.saturating_add(1) < ph.history.len()).then_some(i);
    let label = format!("History: {}", st.label);
    let edited = Some((s.clock)());
    s.commit(
        &label,
        Op::Batch { ops: vec![Op::SetDevelop { id, settings: st.settings, label: label.clone(), edited }, Op::SetHistoryCursor { id, cursor }] },
    )?;
    ok()
}

fn has_history_before(s: &Session) -> std::result::Result<(), String> {
    let at = s.active().and_then(|id| s.catalog.photo(id)).and_then(|p| p.history_index());
    if at.is_some_and(|i| i > 0) { Ok(()) } else { Err("at the first History step".into()) }
}

fn has_history_after(s: &Session) -> std::result::Result<(), String> {
    let photo = s.active().and_then(|id| s.catalog.photo(id));
    if photo.is_some_and(|p| p.history_index().is_some_and(|i| i.saturating_add(1) < p.history.len())) {
        Ok(())
    } else {
        Err("at the latest History step".into())
    }
}

#[cfg(test)]
mod recipe_tests {
    use super::*;

    #[test]
    fn partial_diff_is_the_changed_leaves_only_and_a_recipe_that_stores_nothing_is_none() {
        // Equal settings differ nowhere, so there is nothing to store; a difference comes back with
        // its path only.
        {
            let a = json!({"light": {"exposure": 1.0, "contrast": 0.0}, "color": {"vibrance": 5.0}});
            assert_eq!(partial_diff(&a, &a), None);
            let b = json!({"light": {"exposure": 0.0, "contrast": 0.0}, "color": {"vibrance": 5.0}});
            assert_eq!(partial_diff(&a, &b), Some(json!({"light": {"exposure": 1.0}})));
            assert_eq!(partial_diff(&a, &json!({})), Some(a.clone()), "a leaf the other side lacks is a difference");
        }
        // A recipe that names nothing, or a control `set` would drop (a per-photo instance on fresh
        // settings), stores nothing: the command answers with an error, not an empty preset.
        {
            assert_eq!(recipe_partial(&[]), None);
            assert_eq!(recipe_partial(&recipe(&[("pointColor.0.hueShift", 5.0)])), None, "set returns false for it");
            assert_eq!(recipe_partial(&recipe(&[("nope.nope", 5.0)])), None);
            assert_eq!(recipe_partial(&recipe(&[("light.exposure", 0.5)])), Some(json!({"light": {"exposure": 0.5}})));
            // a reset to the control's default is stored
            assert_eq!(recipe_partial(&recipe(&[("light.exposure", 0.0)])), Some(json!({"light": {"exposure": 0.0}})));
        }
    }

    fn recipe(items: &[(&str, f64)]) -> Vec<(String, f64)> {
        items.iter().map(|(id, v)| (id.to_string(), *v)).collect()
    }
}

/// Half the side of `develop.wbPick`'s averaged square: `size` (pixels, rounded to the next odd
/// number, 1..=31; 5 when absent or not a finite number).
pub(crate) fn wb_pick_half(size: Option<f64>) -> isize {
    match size {
        Some(v) if v.is_finite() => ((v.clamp(1.0, 31.0).round() as isize) / 2).clamp(0, 15),
        _ => 2,
    }
}

/// A channel this close to the photo's top (its brightest value of that channel) has lost its colour.
/// Relative, as sources can be scene-referred (values past 1.0).
const WB_CLIPPED: f32 = 0.97;
/// A sample whose mean is this close to the top in some channel is near white.
const WB_NEAR_WHITE: f32 = 0.9;
/// Below this (linear, the brightest channel) a spot is noise, not colour.
const WB_DARK: f32 = 0.004;

/// What the White Balance Selector reads at a spot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WbSample {
    /// The mean colour of the sample.
    Neutral([f32; 3]),
    /// Clipped or near white: no colour left to read.
    TooBright,
    /// Near black: noise.
    TooDark,
}

/// The photo's top in each channel: white (1.0), or its brightest finite value where a scene-referred
/// source passes that; read on every other pixel.
fn wb_top(img: &lcc_raster::Rgb32f) -> [f32; 3] {
    let mut top = [1.0f32; 3];
    for c in img.data.iter().step_by(2) {
        for (t, v) in top.iter_mut().zip(c.iter()) {
            if v.is_finite() && *v > *t {
                *t = *v;
            }
        }
    }
    top
}

/// The square of half-side `half` around pixel (`cx`, `cy`) of `img` (clamped at the edges): its mean
/// colour, unless a quarter of it is clipped, its mean is near white, or it is near black
/// (OneSlips' White Balance Selector, test notes 2026-10-25 note 13).
pub(crate) fn wb_sample(img: &lcc_raster::Rgb32f, cx: isize, cy: isize, half: isize) -> WbSample {
    let top = wb_top(img);
    let half = half.clamp(0, 64);
    let (mut acc, mut n, mut clipped) = ([0.0f64; 3], 0usize, 0usize);
    for dy in -half..=half {
        for dx in -half..=half {
            let c = img.get_clamped(cx.saturating_add(dx), cy.saturating_add(dy));
            if !c.iter().all(|v| v.is_finite()) {
                continue;
            }
            if c.iter().zip(top.iter()).any(|(v, t)| *v >= WB_CLIPPED * t) {
                clipped += 1;
            }
            for (a, v) in acc.iter_mut().zip(c.iter()) {
                *a += f64::from(*v);
            }
            n += 1;
        }
    }
    if n == 0 {
        return WbSample::TooDark;
    }
    let mean = acc.map(|a| (a / n as f64) as f32);
    if clipped * 4 >= n || mean.iter().zip(top.iter()).any(|(m, t)| *m >= WB_NEAR_WHITE * t) {
        return WbSample::TooBright;
    }
    if mean[0].max(mean[1]).max(mean[2]) < WB_DARK {
        return WbSample::TooDark;
    }
    WbSample::Neutral(mean)
}

#[cfg(test)]
mod wb_sample_tests {
    use super::*;

    #[test]
    fn clipped_white_and_black_spots_are_refused_and_hostile_sizes_stay_in_range() {
        let mut img = lcc_raster::Rgb32f::filled(32, 32, [0.3, 0.25, 0.2]);
        assert!(matches!(wb_sample(&img, 16, 16, 2), WbSample::Neutral(_)));
        for p in img.data.iter_mut().take(32 * 16) {
            *p = [1.0, 1.0, 1.0];
        }
        assert_eq!(wb_sample(&img, 16, 2, 2), WbSample::TooBright);
        let dark = lcc_raster::Rgb32f::filled(8, 8, [0.001, 0.001, 0.001]);
        assert_eq!(wb_sample(&dark, 4, 4, 2), WbSample::TooDark);
        // a scene-referred photo whose top is past white: a dim grey card still reads
        let mut hdr = lcc_raster::Rgb32f::filled(32, 32, [0.9, 0.9, 0.9]);
        hdr.data[0] = [4.0, 4.0, 4.0];
        assert!(matches!(wb_sample(&hdr, 16, 16, 2), WbSample::Neutral(_)));
        // hostile input: NaN pixels and huge offsets don't panic
        let nan = lcc_raster::Rgb32f::filled(4, 4, [f32::NAN; 3]);
        assert_eq!(wb_sample(&nan, isize::MAX, isize::MIN, 1000), WbSample::TooDark);
        // hostile picker sizes stay in range
        for v in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1e300), Some(1e300), Some(0.0), Some(4.0), Some(31.0)] {
            let h = wb_pick_half(v);
            assert!((0..=15).contains(&h), "{v:?} -> {h}");
        }
        assert_eq!(wb_pick_half(Some(1.0)), 0);
        assert!(wb_pick_half(Some(15.0)) > wb_pick_half(None));
    }
}

/// Link the masks Paste, Sync Settings or Paste from Previous copy from photo `source` onto
/// `targets` in one go: each mask of `partial` gets the link its original on `source` has, else a
/// fresh one, written on `source` too (the returned op, part of the same undo step). Nothing is
/// linked when the copies land on fewer than two photos in all.
fn link_copied_masks(s: &Session, partial: &mut Value, source: Option<PhotoId>, targets: &[PhotoId], label: &str) -> Option<Op> {
    let others = targets.iter().filter(|t| Some(**t) != source).count();
    if others + usize::from(source.is_some_and(|id| s.catalog.photo(id).is_some())) < 2 {
        return None;
    }
    let masks = partial.get_mut("masks").and_then(Value::as_array_mut)?;
    let mut src = source.and_then(|id| s.develop_of(id)).map(|d| (*d).clone());
    let mut src_changed = false;
    for m in masks.iter_mut() {
        let Some(o) = m.as_object_mut() else { continue };
        if o.get("link").and_then(Value::as_str).is_some_and(|l| !l.is_empty()) {
            continue;
        }
        let mid = o.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok());
        let original = src.as_mut().and_then(|d| mid.and_then(|mid| d.masks.iter_mut().find(|x| x.id == mid)));
        let link = match original {
            Some(orig) if !orig.link.is_empty() => orig.link.clone(),
            Some(orig) => {
                orig.link = crate::new_mask_link(u64::from(orig.id));
                src_changed = true;
                orig.link.clone()
            }
            None => crate::new_mask_link(mid.map_or(0, u64::from)),
        };
        o.insert("link".into(), Value::String(link));
    }
    let (Some(id), Some(d), true) = (source, src, src_changed) else { return None };
    s.develop_op(id, d, label)
}
