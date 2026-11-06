//! Preset groups made by name: they may stay empty, and can be renamed, starred and deleted.

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::presets::MAX_GROUP_NAME;
use crate::{Result, Session};

/// The group name in `key`, trimmed.
fn group_param(p: &Value, key: &str, c: &str) -> Result<String> {
    let name = str_param(p, key).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, format!("missing {key}")))?;
    if name.chars().count() > MAX_GROUP_NAME {
        return Err(bad(c, format!("a group name is at most {MAX_GROUP_NAME} characters")));
    }
    Ok(name.to_string())
}

fn create_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "preset.createGroup";
    let name = group_param(p, "name", C)?;
    if s.presets.created_groups().contains(&name) || s.presets.iter().any(|x| x.group == name) {
        return Err(bad(C, format!("there is already a group named “{name}”")));
    }
    s.presets.add_created_group(name.clone());
    Ok(json!({"name": name}))
}

fn rename_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "preset.renameGroup";
    let name = group_param(p, "name", C)?;
    let to = group_param(p, "to", C)?;
    if s.presets.iter().any(|x| x.group == name && x.builtin) {
        return Err(bad(C, "a group holding built-in presets can't be renamed"));
    }
    let held = s.presets.iter().filter(|x| x.group == name).count();
    let created = s.presets.created_groups().contains(&name);
    if held == 0 && !created {
        return Err(bad(C, format!("unknown group “{name}”")));
    }
    if to == name {
        return Ok(json!({"name": to, "renamed": 0}));
    }
    // two groups don't merge by a slip of the keyboard: Move to Group does that
    if s.presets.created_groups().contains(&to) || s.presets.iter().any(|x| x.group == to) {
        return Err(bad(C, format!("there is already a group named “{to}”")));
    }
    if held > 0 {
        for x in s.presets.iter_mut().filter(|x| x.group == name) {
            x.group.clone_from(&to);
        }
    }
    if created {
        s.presets.remove_created_group(&name);
        s.presets.add_created_group(to.clone());
    }
    if s.presets.favorite_groups().contains(&name) {
        s.presets.set_favorite_group(&name, false);
        s.presets.set_favorite_group(&to, true);
    }
    Ok(json!({"name": to, "renamed": held}))
}

fn favorite_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "preset.favoriteGroup";
    let name = group_param(p, "name", C)?;
    if !s.presets.created_groups().contains(&name) && !s.presets.iter().any(|x| x.group == name) {
        return Err(bad(C, format!("unknown group “{name}”")));
    }
    let favorite = bool_or(p, "favorite", !s.presets.favorite_groups().contains(&name));
    s.presets.set_favorite_group(&name, favorite);
    Ok(json!({"name": name, "favorite": favorite}))
}

fn delete_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "preset.deleteGroup";
    let name = group_param(p, "name", C)?;
    if s.presets.iter().any(|x| x.group == name && x.builtin) {
        return Err(bad(C, "a group holding built-in presets can't be deleted"));
    }
    let held = s.presets.iter().filter(|x| x.group == name).count();
    if held == 0 && !s.presets.created_groups().contains(&name) {
        return Err(bad(C, format!("unknown group “{name}”")));
    }
    if held > 0 {
        s.presets.retain(|x| x.group != name);
    }
    s.presets.remove_created_group(&name);
    s.presets.set_favorite_group(&name, false);
    Ok(json!({"deleted": held}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "preset.createGroup",
            "New Preset Group",
            [],
            None,
            "{name (up to 100 characters)} → {name} — a group that stays listed and saved while it holds no preset; refused when a group has that name",
            always,
            create_group
        ),
        cmd!(
            "preset.renameGroup",
            "Rename Preset Group",
            [],
            None,
            "{name, to (up to 100 characters)} → {name, renamed} — the group and the user presets in it; refused for a group holding built-in presets, and when a group is already named `to`",
            always,
            rename_group
        ),
        cmd!(
            "preset.favoriteGroup",
            "Favorite Preset Group",
            [],
            None,
            "{name, favorite?: bool (default: the opposite of now)} → {name, favorite} — a starred group, built-in ones included, is listed before the others",
            always,
            favorite_group
        ),
        cmd!(
            "preset.deleteGroup",
            "Delete Preset Group",
            [],
            None,
            "{name} → {deleted} — the group and the user presets in it; refused for a group holding built-in presets",
            always,
            delete_group
        ),
    ]
}
