//! The Profile Manager's commands (ported from OneSlips, LR-PROF-MANAGE and LR-PROF-BODY): hide
//! profiles from the menus and the browser, and give camera bodies their own camera profile.

use std::collections::BTreeMap;

use lcc_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::profile_bodies;
use crate::profile_edits::{MAX_ENTRIES, MAX_NAME};
use crate::{Result, Session};

/// Every profile with its group and whether it is hidden (the user's names, groups and order),
/// the built-in looks removed from this install, and the camera bodies.
fn manager(s: &mut Session, _p: &Value) -> Result<Value> {
    let bodies = bodies(s, &json!({}))?;
    Ok(json!({
        "profiles": s.all_profiles(),
        "groups": s.profile_group_names(),
        "removed": s.profile_edits.removed,
        "bodies": bodies["bodies"],
        "camera": camera_profiles(s),
    }))
}

/// The imported camera profiles (`.dcp`), each with how many photos chose it and whether it is its camera's
/// default (the active copy for its model).
fn camera_profiles(s: &Session) -> Vec<Value> {
    let active: Vec<(std::path::PathBuf, lcc_raw::dcp::Dcp)> = crate::camera_profiles::active_dcps();
    lcc_raw::dcp::library()
        .into_iter()
        .map(|(id, d)| {
            let used = s.catalog.photos().filter(|p| p.develop.profile.camera == id).count();
            let default = active.iter().any(|(_, a)| *a == *d);
            json!({"id": id, "name": d.name, "model": d.model, "usedBy": used, "cameraDefault": default, "ownLook": d.has_look()})
        })
        .collect()
}

/// The file of imported camera profile `id` (`dcp:<file stem>`) in library folder `dir`.
fn camera_file(dir: &std::path::Path, id: &str, c: &str) -> Result<std::path::PathBuf> {
    let stem =
        id.strip_prefix("dcp:").filter(|st| !st.is_empty()).ok_or_else(|| bad(c, format!("`{id}` is not an imported camera profile (dcp:…)")))?;
    let entries = std::fs::read_dir(dir).map_err(|e| bad(c, format!("no imported camera profile `{id}` ({}: {e})", dir.display())))?;
    entries
        .flatten()
        .map(|e| e.path())
        .find(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp")) && f.file_stem().is_some_and(|n| n.to_string_lossy() == stem))
        .ok_or_else(|| bad(c, format!("no imported camera profile `{id}`")))
}

/// `profile.deleteCamera` (OneSlips, LR-PROF-IMPORT): an imported camera profile goes, with its copy as its camera's
/// default when it is that; photos that chose it go back to their camera's default (one undo step), and bodies that
/// started with it start with the camera's default.
fn delete_camera(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.deleteCamera";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing id"))?.to_string();
    let library = crate::camera_profiles::dcp_library_dir().ok_or_else(|| bad(C, "no camera-profiles folder"))?;
    let active = crate::camera_profiles::dir();
    let r = delete_camera_in(s, &id, &library, active.as_deref())?;
    // the decoder's lists as the folders now are
    lcc_raw::dcp::install_library(crate::camera_profiles::library_dcps());
    lcc_raw::dcp::install(crate::camera_profiles::active_dcps().into_iter().map(|(_, d)| d).collect());
    Ok(r)
}

/// [`delete_camera`] with the library folder `library` and the folder of the cameras' default copies `active`.
pub(crate) fn delete_camera_in(s: &mut Session, id: &str, library: &std::path::Path, active: Option<&std::path::Path>) -> Result<Value> {
    const C: &str = "profile.deleteCamera";
    let id = id.to_string();
    let file = camera_file(library, &id, C)?;
    let bytes = std::fs::read(&file).map_err(|e| bad(C, format!("{}: {e}", file.display())))?;
    let model = lcc_raw::dcp::parse(&bytes).map(|d| d.model).ok();
    std::fs::remove_file(&file).map_err(|e| bad(C, format!("{}: {e}", file.display())))?;
    // the camera's default copy of this very profile goes too: otherwise it keeps applying to every photo of the camera
    let mut was_default = false;
    if let (Some(model), Some(dir)) = (&model, active) {
        let copy = dir.join(crate::camera_profiles::dcp_file_name(model));
        if std::fs::read(&copy).is_ok_and(|b| b == bytes) {
            std::fs::remove_file(&copy).map_err(|e| bad(C, format!("{}: {e}", copy.display())))?;
            was_default = true;
        }
    }
    // (out of the decoder's list of profiles chosen per photo straight away)
    lcc_raw::dcp::install_library(lcc_raw::dcp::library().into_iter().filter(|(k, _)| *k != id).map(|(k, d)| (k, (*d).clone())).collect());
    let bodies_before = s.body_profiles.len();
    s.body_profiles.retain(|_, v| *v != id);
    let bodies = bodies_before.saturating_sub(s.body_profiles.len());
    let ops: Vec<Op> = s
        .catalog
        .photos()
        .filter(|ph| ph.develop.profile.camera == id)
        .map(|ph| {
            let mut d = (*ph.develop).clone();
            d.profile.camera.clear();
            (ph.id, d)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|(pid, d)| s.develop_op(pid, d, "Camera Profile"))
        .collect();
    let photos = ops.len();
    if photos > 0 {
        s.commit("Delete Camera Profile", Op::Batch { ops })?;
    }
    // photos of the camera decode again with what is left (the camera's default may have been this one)
    if let Some(model) = &model {
        let of_model: Vec<PhotoId> = s.catalog.photos().filter(|ph| lcc_raw::dcp::covers_model(model, &ph.meta.camera)).map(|ph| ph.id).collect();
        for pid in of_model {
            s.media.forget(pid);
        }
    }
    s.save_prefs()?;
    Ok(json!({"deleted": id, "photos": photos, "bodies": bodies, "wasCameraDefault": was_default}))
}

/// `profile.adoptForBody`: camera profile `profile` becomes body `body`'s (or photo `photo`'s body's) default, and
/// the body's photos still on their camera's default get it (one undo step).
fn adopt_for_body(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.adoptForBody";
    let id = str_param(p, "profile").or_else(|| str_param(p, "id")).unwrap_or("").trim().to_string();
    if id.is_empty() || !profile_bodies::known_profile(&id) {
        return Err(bad(C, format!("unknown camera profile `{id}`")));
    }
    let body = body_param(s, p, C)?;
    s.body_profiles.insert(body.clone(), id.clone());
    let ops: Vec<Op> = s
        .catalog
        .photos()
        .filter(|ph| ph.develop.profile.camera.is_empty() && profile_bodies::key_of(ph).as_deref() == Some(body.as_str()))
        .map(|ph| {
            let mut d = (*ph.develop).clone();
            d.profile.camera.clone_from(&id);
            (ph.id, d)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|(pid, d)| s.develop_op(pid, d, "Camera Profile"))
        .collect();
    let n = ops.len();
    if n > 0 {
        s.commit(C, Op::Batch { ops })?;
    }
    s.save_prefs()?;
    Ok(json!({"body": body, "profile": id, "changed": n}))
}

/// `profile.releaseFromBody`: body `body` no longer starts with camera profile `profile` (its photos keep theirs).
fn release_from_body(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.releaseFromBody";
    let id = str_param(p, "profile").or_else(|| str_param(p, "id")).ok_or_else(|| bad(C, "missing `profile`"))?.trim().to_string();
    let body = body_param(s, p, C)?;
    let released = s.body_profiles.get(&body) == Some(&id);
    if released {
        s.body_profiles.remove(&body);
        s.save_prefs()?;
    }
    Ok(json!({"body": body, "released": released}))
}

/// `profile.clearBodyDefault`: body `body` starts with its camera's default again.
fn clear_body_default(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.clearBodyDefault";
    let body = body_param(s, p, C)?;
    let had = s.body_profiles.remove(&body);
    s.save_prefs()?;
    Ok(json!({"body": body, "cleared": had}))
}

/// `profile.groupNew`: an empty group of the user's, kept in the group order until profiles are moved into it.
fn group_new(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.groupNew";
    let name = name_param(p, "name", C)?.ok_or_else(|| bad(C, "missing `name`"))?;
    if s.profile_group_names().contains(&name) {
        return Err(bad(C, format!("there is a group “{name}” already")));
    }
    if s.profile_edits.group_order.len() >= MAX_ENTRIES {
        return Err(bad(C, "too many groups"));
    }
    let mut order = s.profile_group_names();
    order.push(name.clone());
    s.profile_edits.group_order = order;
    s.save_prefs()?;
    Ok(json!({"group": name, "groups": s.profile_group_names()}))
}

/// `profile.groupMove`: group `group` moves `by` places (−1 up, 1 down) or `to` a place in the group order.
fn group_move(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.groupMove";
    let g = name_param(p, "group", C)?.ok_or_else(|| bad(C, "missing `group`"))?;
    let mut order = s.profile_group_names();
    let at = order.iter().position(|x| *x == g).ok_or_else(|| bad(C, format!("no profile group `{g}`")))?;
    let last = order.len().saturating_sub(1);
    let to = match (p.get("to").and_then(Value::as_u64), p.get("by").and_then(Value::as_i64)) {
        (Some(to), _) => usize::try_from(to).unwrap_or(usize::MAX).min(last),
        (None, Some(by)) => {
            let target = i64::try_from(at).unwrap_or(i64::MAX).saturating_add(by).clamp(0, i64::try_from(last).unwrap_or(i64::MAX));
            usize::try_from(target).unwrap_or(last)
        }
        _ => return Err(bad(C, "give `by` (places, −1 up, 1 down) or `to` (a place)")),
    };
    let moved = order.remove(at);
    order.insert(to.min(order.len()), moved);
    s.profile_edits.group_order = order;
    s.save_prefs()?;
    Ok(json!({"groups": s.profile_group_names()}))
}

/// `profile.place` (OneSlips, LR-PRE-MANAGE drag and drop): profiles `ids` move together before or after profile
/// `before` / `after`, or to the end of group `group`; placed by another profile, they join its group.
fn place(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.place";
    let list = p.get("ids").and_then(Value::as_array).ok_or_else(|| bad(C, "`ids` must be a list of profile ids"))?;
    if list.is_empty() || list.len() > MAX_ENTRIES {
        return Err(bad(C, "`ids` must name 1 or more profiles"));
    }
    let mut ids: Vec<String> = Vec::new();
    for v in list {
        let id = v.as_str().ok_or_else(|| bad(C, "`ids` must be a list of profile ids"))?;
        if !known(s, id) {
            return Err(bad(C, format!("unknown profile `{id}`")));
        }
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    let all = s.all_profiles();
    let (target, after) = match (str_param(p, "before"), str_param(p, "after")) {
        (Some(b), _) => (Some(b.to_string()), false),
        (None, Some(a)) => (Some(a.to_string()), true),
        (None, None) => (None, false),
    };
    let group = match (&target, name_param(p, "group", C).ok().flatten()) {
        (Some(t), _) => {
            if ids.contains(t) {
                return Err(bad(C, "a profile can't be placed by itself"));
            }
            all.iter().find(|x| x.id == *t).map(|x| x.group.clone()).ok_or_else(|| bad(C, format!("unknown profile `{t}`")))?
        }
        (None, Some(g)) => g,
        (None, None) => return Err(bad(C, "give `before`, `after` or `group`")),
    };
    let mut order: Vec<String> = all.iter().map(|x| x.id.clone()).filter(|x| !ids.contains(x)).collect();
    let at = match &target {
        Some(t) => order.iter().position(|x| x == t).map(|i| if after { i.saturating_add(1) } else { i }).unwrap_or(order.len()),
        // the end of the group: after its last profile, else at the end
        None => all
            .iter()
            .filter(|x| x.group == group && !ids.contains(&x.id))
            .filter_map(|x| order.iter().position(|o| *o == x.id))
            .max()
            .map_or(order.len(), |i| i.saturating_add(1)),
    };
    let at = at.min(order.len());
    for (k, id) in ids.iter().enumerate() {
        order.insert(at.saturating_add(k).min(order.len()), id.clone());
    }
    for id in &ids {
        let own = all.iter().find(|x| x.id == *id).map(|x| x.original_group.clone()).unwrap_or_default();
        if own == group {
            s.profile_edits.groups.remove(id);
        } else {
            s.profile_edits.groups.insert(id.clone(), group.clone());
        }
    }
    s.profile_edits.order = order;
    s.save_prefs()?;
    Ok(json!({"placed": ids, "group": group}))
}

/// A profile id the manager knows: a built-in look (removed ones too) or an imported LUT profile.
fn known(s: &Session, id: &str) -> bool {
    crate::presets::profile(id).is_some() || s.lut_profiles.iter().any(|x| x.id == id)
}

fn id_param(s: &Session, p: &Value, c: &str) -> Result<String> {
    let id = str_param(p, "id").ok_or_else(|| bad(c, "missing id"))?;
    if !known(s, id) {
        return Err(bad(c, format!("unknown profile `{id}`")));
    }
    Ok(id.to_string())
}

/// A name or group from `p[key]`: trimmed, not empty, at most [`MAX_NAME`] characters; `""` / null
/// clears (`None`).
fn name_param(p: &Value, key: &str, c: &str) -> Result<Option<String>> {
    match p.get(key) {
        None => Err(bad(c, format!("missing `{key}`"))),
        Some(Value::Null) => Ok(None),
        Some(Value::String(n)) => {
            let n = n.trim();
            if n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
                return Err(bad(c, format!("`{key}` must be at most {MAX_NAME} characters, without control characters")));
            }
            Ok((!n.is_empty()).then(|| n.to_string()))
        }
        Some(_) => Err(bad(c, format!("`{key}` must be a string"))),
    }
}

fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.rename";
    let id = id_param(s, p, C)?;
    match name_param(p, "name", C)? {
        Some(n) => s.profile_edits.names.insert(id.clone(), n),
        None => s.profile_edits.names.remove(&id),
    };
    s.save_prefs()?;
    Ok(json!({"id": id, "name": s.all_profiles().into_iter().find(|x| x.id == id).map(|x| x.name)}))
}

fn set_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.setGroup";
    let id = id_param(s, p, C)?;
    match name_param(p, "group", C)? {
        Some(g) => s.profile_edits.groups.insert(id.clone(), g),
        None => s.profile_edits.groups.remove(&id),
    };
    s.save_prefs()?;
    Ok(json!({"id": id, "group": s.all_profiles().into_iter().find(|x| x.id == id).map(|x| x.group)}))
}

fn reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.reorder";
    let ids = p.get("ids").and_then(Value::as_array).ok_or_else(|| bad(C, "`ids` must be a list of profile ids"))?;
    if ids.len() > MAX_ENTRIES {
        return Err(bad(C, "too many ids"));
    }
    let mut order: Vec<String> = Vec::new();
    for v in ids {
        let id = v.as_str().ok_or_else(|| bad(C, "`ids` must be a list of profile ids"))?;
        if !known(s, id) {
            return Err(bad(C, format!("unknown profile `{id}`")));
        }
        if !order.iter().any(|o| o == id) {
            order.push(id.to_string());
        }
    }
    // the ids given come first in that order, the rest keep theirs
    let rest: Vec<String> = s.profile_edits.order.iter().filter(|o| !order.contains(o)).cloned().collect();
    order.extend(rest);
    s.profile_edits.order = order;
    s.save_prefs()?;
    Ok(json!({"order": s.profile_edits.order}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.remove";
    let id = id_param(s, p, C)?;
    if crate::presets::profile(&id).is_none() {
        return Err(bad(C, "only a built-in look is removed here (an imported profile is deleted with its own command)"));
    }
    if !s.profile_edits.removed.contains(&id) {
        s.profile_edits.removed.push(id.clone());
    }
    s.profile_favorites.retain(|f| f != &id);
    s.profile_recent.retain(|f| f != &id);
    s.save_prefs()?;
    Ok(json!({"removed": id}))
}

fn restore_builtins(s: &mut Session, _p: &Value) -> Result<Value> {
    let n = s.profile_edits.removed.len();
    s.profile_edits.removed.clear();
    s.save_prefs()?;
    Ok(json!({"restored": n}))
}

/// The profiles of group `group` (as the manager shows it).
fn members(s: &Session, p: &Value, c: &str) -> Result<(String, Vec<crate::profile_edits::ShownProfile>)> {
    let g = name_param(p, "group", c)?.ok_or_else(|| bad(c, "missing `group`"))?;
    let list: Vec<_> = s.all_profiles().into_iter().filter(|x| x.group == g).collect();
    if list.is_empty() {
        return Err(bad(c, format!("no profile group `{g}`")));
    }
    Ok((g, list))
}

fn group_rename(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.groupRename";
    let (g, list) = members(s, p, C)?;
    let to = name_param(p, "name", C)?.ok_or_else(|| bad(C, "missing `name`"))?;
    for x in &list {
        s.profile_edits.groups.insert(x.id.clone(), to.clone());
    }
    s.save_prefs()?;
    Ok(json!({"group": g, "name": to, "profiles": list.len()}))
}

fn group_hide(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.groupHide";
    let (g, list) = members(s, p, C)?;
    let want = p.get("hidden").and_then(Value::as_bool).unwrap_or(!list.iter().all(|x| x.hidden));
    for x in &list {
        let on = s.profile_hidden.contains(&x.id);
        if want && !on {
            s.profile_hidden.push(x.id.clone());
        } else if !want {
            s.profile_hidden.retain(|h| *h != x.id);
        }
    }
    s.save_prefs()?;
    Ok(json!({"group": g, "hidden": want, "profiles": list.len()}))
}

fn group_remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.groupRemove";
    let (g, list) = members(s, p, C)?;
    // built-in looks are removed (restorable); imported LUT profiles are hidden (deleting their
    // files is the LUT import's own command)
    let (mut removed, mut hidden) = (0, 0);
    for x in &list {
        if x.kind == "look" {
            if !s.profile_edits.removed.contains(&x.id) {
                s.profile_edits.removed.push(x.id.clone());
            }
            removed += 1;
        } else if !s.profile_hidden.contains(&x.id) {
            s.profile_hidden.push(x.id.clone());
            hidden += 1;
        }
    }
    s.save_prefs()?;
    Ok(json!({"group": g, "removed": removed, "hidden": hidden}))
}

fn hide(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.hide";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing id"))?.to_string();
    // (camera profiles too, imported `dcp:…` and bundled `cal:…`: the Profile Manager hides them from
    // the camera profile choices)
    let camera = lcc_raw::dcp::library_get(&id).is_some() || lcc_raw::dcp::calibrated_look_exists(&id);
    if crate::presets::profile(&id).is_none() && !s.lut_profiles.iter().any(|x| x.id == id) && !camera {
        return Err(bad(C, format!("unknown profile `{id}`")));
    }
    let on = s.profile_hidden.contains(&id);
    let want = p.get("hidden").and_then(Value::as_bool).unwrap_or(!on);
    if want && !on {
        s.profile_hidden.push(id);
    } else if !want {
        s.profile_hidden.retain(|h| *h != id);
    }
    s.save_prefs()?;
    Ok(json!({"hidden": want}))
}

/// The camera bodies of the library's raws, each with its photo count and camera profile.
fn bodies(s: &mut Session, _p: &Value) -> Result<Value> {
    let mut found: BTreeMap<String, (String, String, usize)> = BTreeMap::new();
    for p in s.catalog.photos() {
        let Some(k) = s.body_key(p, false) else { continue };
        let body = k.rsplit_once('|').map_or(k.as_str(), |(_, b)| b).to_string();
        let e = found.entry(k).or_insert_with(|| (p.meta.camera.clone(), body, 0));
        e.2 = e.2.saturating_add(1);
    }
    // a body with a default but no photo in this library is listed too
    for k in s.body_profiles.keys() {
        if !found.contains_key(k) {
            let (model, body) = k.split_once('|').unwrap_or((k.as_str(), ""));
            found.insert(k.clone(), (model.to_string(), body.to_string(), 0));
        }
    }
    let list: Vec<Value> = found
        .into_iter()
        .map(|(k, (camera, body, photos))| json!({"body": k, "camera": camera, "serial": body, "photos": photos, "profile": s.body_profiles.get(&k)}))
        .collect();
    Ok(json!({"bodies": list}))
}

/// The body key a command names: `body` as `bodies` lists it, or the body of photo `photo` (default:
/// the active photo).
fn body_param(s: &Session, p: &Value, c: &str) -> Result<String> {
    if let Some(b) = str_param(p, "body") {
        let b = b.trim();
        if b.is_empty() || b.len() > 200 {
            return Err(bad(c, "`body` must be a body as profiles.bodies lists it"));
        }
        return Ok(b.to_string());
    }
    let id = match p.get("photo") {
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(c, "`photo` must be a photo id"))?),
        None => s.active().ok_or_else(|| bad(c, "no body given and no active photo"))?,
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(c, "no such photo"))?;
    s.body_key(photo, true).ok_or_else(|| bad(c, "this photo isn't a raw with a known camera body"))
}

fn set_body_default(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.setBodyDefault";
    let id = str_param(p, "id").unwrap_or("").trim().to_string();
    if !id.is_empty() && !profile_bodies::known_profile(&id) {
        return Err(bad(C, format!("unknown camera profile `{id}`")));
    }
    let body = body_param(s, p, C)?;
    if id.is_empty() {
        s.body_profiles.remove(&body);
    } else {
        s.body_profiles.insert(body.clone(), id.clone());
    }
    s.save_prefs()?;
    Ok(json!({"body": body, "profile": if id.is_empty() { Value::Null } else { json!(id) }}))
}

fn apply_body_defaults(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.applyBodyDefaults";
    let ids: Vec<PhotoId> = match p.get("ids") {
        None | Some(Value::Null) => s.catalog.photos().map(|x| x.id).collect(),
        Some(_) => s.targets(p)?,
    };
    let ops: Vec<Op> = ids
        .into_iter()
        .filter_map(|id| {
            let photo = s.catalog.photo(id)?;
            let want = s.body_default(photo)?.to_string();
            (photo.develop.profile.camera != want).then(|| {
                let mut d = (*photo.develop).clone();
                d.profile.camera = want;
                s.develop_op(id, d, "Camera Profile")
            })?
        })
        .collect();
    let n = ops.len();
    if n > 0 {
        s.commit(C, Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

fn profile_plan(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "import.profilePlan";
    let paths: Vec<String> = p
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(C, "`paths` must be a list of files or folders"))?
        .iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(|| bad(C, "`paths` must be a list of files or folders")))
        .collect::<Result<_>>()?;
    let choice = profile_bodies::ImportProfile::parse(p.get("cameraProfile")).map_err(|e| bad(C, e))?;
    let candidates = crate::import::scan(s, &paths);
    let rows = profile_bodies::plan(s, &candidates, &choice);
    Ok(json!({"rows": rows, "photos": rows.iter().map(|r| r.photos).sum::<usize>()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "import.profilePlan", "Camera Profiles for Import", [], None, "{paths: [file or folder], cameraProfile?: as library.import} → {rows: [{body (model|serial or model|file:XX, as cameraProfile.perBody takes it), camera, serial, photos, profile (\"\": the camera's default), profileName, how: auto|chosen|all}], photos}: the camera profile each body's new raws would get", always, profile_plan),
        cmd!(query "profiles.manager", "Manage Profiles", [], None, "{} → {profiles: [{id, name, originalName, group, originalGroup, kind: look|lut, hidden, builtinLabel}] (the user's names, groups and order), groups: [names in order, empty ones too], removed: [built-in look ids], bodies (as profiles.bodies), camera: [{id, name, model, usedBy, cameraDefault, ownLook}] (imported .dcp camera profiles)}", always, manager),
        cmd!("profile.rename", "Rename Profile", [], None, "{id, name: string (\"\" or null: its own name again)} → {id, name}", always, rename),
        cmd!(
            "profile.setGroup",
            "Move Profile to Group",
            [],
            None,
            "{id, group: string (\"\" or null: its own group again)} → {id, group}",
            always,
            set_group
        ),
        cmd!(
            "profile.reorder",
            "Reorder Profiles",
            [],
            None,
            "{ids: [profile id]}: these first, in this order; the others keep theirs → {order}",
            always,
            reorder
        ),
        cmd!(
            "profile.remove",
            "Remove Built-in Look",
            [],
            None,
            "{id: a built-in look}: out of this install's lists (photos using it keep it; profile.restoreBuiltins brings it back) → {removed}",
            always,
            remove
        ),
        cmd!(
            "profile.restoreBuiltins",
            "Restore Built-in Looks",
            [],
            None,
            "{}: every removed built-in look comes back → {restored}",
            always,
            restore_builtins
        ),
        cmd!(
            "profile.groupRename",
            "Rename Profile Group",
            [],
            None,
            "{group, name}: every profile of the group moves to `name` → {group, name, profiles}",
            always,
            group_rename
        ),
        cmd!(
            "profile.groupHide",
            "Hide Profile Group",
            [],
            None,
            "{group, hidden?: bool (toggles when omitted)} → {group, hidden, profiles}",
            always,
            group_hide
        ),
        cmd!(
            "profile.groupRemove",
            "Remove Profile Group",
            [],
            None,
            "{group}: its built-in looks are removed (restorable), its imported profiles hidden → {group, removed, hidden}",
            always,
            group_remove
        ),
        cmd!(
            "profile.hide",
            "Hide Profile",
            [],
            None,
            "{id (a look, lut:…, dcp:… or cal:…), hidden?: bool (toggles when omitted)}: out of the profile menu, the browser and the camera profile choices (still in profiles.manager; photos using it keep it) → {hidden}",
            always,
            hide
        ),
        cmd!(query "profiles.bodies", "Camera Bodies", [], None, "{} → {bodies: [{body (the key the other commands take), camera, serial (or file:XX for bodies that write none), photos, profile (its camera profile, or null)}]}", always, bodies),
        cmd!(
            "profile.setBodyDefault",
            "Set Body's Camera Profile",
            [],
            None,
            "{id: camera profile (dcp:…, cal:… or lc; \"\" clears), body? (from profiles.bodies) | photo? (default: the active photo)}: that body's raws start with it at import → {body, profile}",
            always,
            set_body_default
        ),
        cmd!(
            "profile.applyBodyDefaults",
            "Apply Body Camera Profiles",
            [],
            None,
            "{ids?} (default: every photo): photos of bodies with a camera profile get it, one undo step → {changed}",
            always,
            apply_body_defaults
        ),
        cmd!(
            "profile.deleteCamera",
            "Delete Imported Camera Profile",
            [],
            None,
            "{id: dcp:… (from profiles.manager `camera`)}: the imported .dcp goes, and its copy as its camera's default when it is that; photos that chose it go back to their camera's default (one undo step) → {deleted, photos, bodies, wasCameraDefault}",
            always,
            delete_camera
        ),
        cmd!(
            "profile.adoptForBody",
            "Adopt Camera Profile for Body",
            [],
            None,
            "{profile: camera profile (dcp:…, cal:… or lc), body? (from profiles.bodies) | photo? (default: the active photo)}: the body's default, and its photos still on the camera's default get it (one undo step) → {body, profile, changed}",
            always,
            adopt_for_body
        ),
        cmd!(
            "profile.releaseFromBody",
            "Remove Camera Profile from Body",
            [],
            None,
            "{profile, body? | photo?}: the body no longer starts with it (its photos keep theirs) → {body, released}",
            always,
            release_from_body
        ),
        cmd!(
            "profile.clearBodyDefault",
            "Clear Body's Camera Profile",
            [],
            None,
            "{body? | photo?}: the body's raws start with their camera's default again → {body, cleared}",
            always,
            clear_body_default
        ),
        cmd!(
            "profile.groupNew",
            "New Profile Group",
            [],
            None,
            "{name}: an empty group, kept in the group order → {group, groups}",
            always,
            group_new
        ),
        cmd!("profile.groupMove", "Move Profile Group", [], None, "{group, by?: places (−1 up, 1 down) | to?: place} → {groups}", always, group_move),
        cmd!(
            "profile.place",
            "Place Profiles",
            [],
            None,
            "{ids: [profile id], before?: id | after?: id | group?: name}: before or after another profile (joining its group), or at the end of a group, in one save → {placed, group}",
            always,
            place
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Profile Manager hides camera profiles too (out of the camera profile choices); an id no
    /// profile has is an error, never a hidden entry.
    #[test]
    fn camera_profiles_can_be_hidden() {
        let mut s = Session::with_demo();
        // a bundled calibration of a camera of its own (the registry is process-wide: add one and take it out again)
        let dcp = std::sync::Arc::new(lcc_raw::dcp::Dcp {
            model: "PMHIDE ONE".into(),
            name: "Hide Me".into(),
            copyright: String::new(),
            color: lcc_raw::ColorData::default(),
        });
        let others = || lcc_raw::dcp::calibrated().iter().filter(|c| c.make != "PMHIDE").map(|c| (**c).clone()).collect::<Vec<_>>();
        let mut list = others();
        list.push(lcc_raw::dcp::Calibrated {
            make: "PMHIDE".into(),
            names: vec!["PMHIDE ONE".into()],
            lens: None,
            looks: vec![("cal:pmhide-me".into(), dcp)],
            default: None,
            own_matrix_only: false,
        });
        lcc_raw::dcp::install_calibrated(list);
        let hidden = s.execute("profile.hide", &json!({"id": "cal:pmhide-me", "hidden": true}));
        let unknown = s.execute("profile.hide", &json!({"id": "dcp:pm-no-such-profile"}));
        lcc_raw::dcp::install_calibrated(others());
        assert_eq!(hidden.unwrap()["hidden"], true);
        assert!(s.profile_hidden.iter().any(|h| h == "cal:pmhide-me"));
        assert!(unknown.is_err());
    }
}
