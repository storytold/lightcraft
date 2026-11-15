//! Develop tool profiles (fork-owned, own design): which of the Edit panel's groups and sliders
//! the right panel shows. Two built-in profiles that never change — Default (every group, as the
//! panel always was) and Witchcraft (the colourist's Primaries and HSL Curves, without Eboda's
//! Color Grading) — and the user's own, made in Settings ▸ Develop: any group switched off, any
//! slider or option of a group hidden, and one Custom group of chosen sliders in a chosen order.
//!
//! Saved with the app's settings (`ui.json`, `settings.toolProfiles`). A stored profile naming a
//! group, slider or option this build doesn't know keeps the rest: the unknown ids are dropped
//! when the settings are read, never an error. `view.toolProfile` picks the profile in use,
//! `view.toolProfileEdit` changes the user's own (the Settings page and the panel's row run them).

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;
use crate::menus::UiCommand;

/// The Default profile's id: every group and slider, as the Edit panel always had them.
pub const DEFAULT: &str = "default";
/// The Witchcraft profile's id: the colourist's palettes, Eboda's Color Grading away.
pub const WITCHCRAFT: &str = "witchcraft";
/// The Custom group's section id in the Edit panel.
pub const CUSTOM: &str = "custom";

/// At most this many profiles of the user's (a hand-edited file can't make the menu endless).
const MAX_PROFILES: usize = 100;
/// At most this many sliders in a Custom group.
const MAX_CUSTOM: usize = 200;
/// A profile name's longest, in characters.
const MAX_NAME: usize = 64;

/// The Edit panel's groups a profile switches on and off (section id, title), in the panel's order.
pub const GROUPS: &[(&str, &str)] = &[
    ("light", "Basic"),
    ("primaries", "Primaries"),
    ("hsl", "HSL Curves"),
    ("curve", "Tone Curve"),
    ("mixer", "Color Mixer"),
    ("grading", "Color Grading"),
    ("detail", "Detail"),
    ("retouch", "Retouch"),
    ("optics", "Lens Corrections"),
    ("geometry", "Transform"),
    ("effects", "Effects"),
    ("calibration", "Calibration"),
    ("pluginFilters", "Plug-in Filters"),
];

/// The options (rows that aren't sliders) a profile can hide: id, its group, label.
pub const OPTIONS: &[(&str, &str, &str)] = &[
    ("option:treatment", "light", "Auto · B&W · HDR"),
    ("option:wbSelector", "light", "White Balance Selector"),
    ("option:contrastMode", "light", "Contrast Mode"),
    ("option:aiDenoise", "detail", "AI Denoise"),
    ("option:hotPixels", "detail", "Remove Hot Pixels"),
    ("option:newPhotoDefault", "optics", "New photos start from"),
    ("option:vignetteStyle", "effects", "Vignette Style"),
];

/// The group of the Edit panel a develop control's slider sits in (`None`: not a slider of the
/// panel's groups, e.g. the crop angle).
pub fn group_of(control: &str) -> Option<&'static str> {
    let prefix = control.split('.').next().unwrap_or_default();
    // (the retouch person pickers are menus, not sliders)
    if control.ends_with("Person") {
        return None;
    }
    Some(match prefix {
        "profile" | "wb" | "light" | "hdr" | "color" | "effects" => "light",
        "curve" => "curve",
        "mixer" | "bw" => "mixer",
        "grading" => "grading",
        "primaries" => "primaries",
        "skin" | "retouch" => "retouch",
        "detail" | "enhance" => "detail",
        "optics" => "optics",
        "geometry" => "geometry",
        "vignette" | "grain" => "effects",
        "calibration" => "calibration",
        _ => return None,
    })
}

/// The sliders of a group, in the panel's order (the develop control specs).
pub fn sliders_of(group: &str) -> impl Iterator<Item = &'static lcc_develop::ControlSpec> + '_ {
    lcc_develop::controls::CONTROLS.iter().filter(move |c| group_of(c.id) == Some(group))
}

/// The options of a group (id, label).
pub fn options_of(group: &str) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    OPTIONS.iter().filter(move |(_, g, _)| *g == group).map(|(id, _, label)| (*id, *label))
}

/// Whether `id` is one of the panel's groups.
pub fn is_group(id: &str) -> bool {
    GROUPS.iter().any(|(g, _)| *g == id)
}

/// Whether `id` is a slider or option a profile can hide.
pub fn is_item(id: &str) -> bool {
    OPTIONS.iter().any(|(o, _, _)| *o == id) || is_slider(id)
}

/// Whether `id` is a slider of one of the panel's groups (what a Custom group can hold).
pub fn is_slider(id: &str) -> bool {
    lcc_develop::controls::CONTROLS.iter().any(|c| c.id == id) && group_of(id).is_some()
}

/// One tool profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProfile {
    pub id: String,
    pub name: String,
    /// The groups switched off (section ids of [`GROUPS`]).
    pub groups_off: Vec<String>,
    /// The sliders (control ids) and options ([`OPTIONS`]) hidden in their groups.
    pub hidden: Vec<String>,
    /// The Custom group's sliders, in order (empty: no Custom group).
    pub custom: Vec<String>,
    /// The Witchcraft palettes (Primaries, HSL Curves) show in either grading mode.
    pub witchcraft: bool,
}

impl ToolProfile {
    /// A stored profile, read field by field: a field of the wrong kind is left empty, an entry that
    /// isn't a string dropped (`None`: not an object).
    fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let text = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let list = |k: &str| -> Vec<String> {
            o.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
        };
        Some(ToolProfile {
            id: text("id"),
            name: text("name"),
            groups_off: list("groupsOff"),
            hidden: list("hidden"),
            custom: list("custom"),
            witchcraft: o.get("witchcraft").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    pub fn builtin(&self) -> bool {
        is_builtin(&self.id)
    }

    /// Whether the group shows (its own switch; the Witchcraft palettes also follow the grading mode).
    pub fn shows_group(&self, group: &str) -> bool {
        !self.groups_off.iter().any(|g| g == group)
    }

    /// Whether a slider or option shows in its group.
    pub fn shows_item(&self, item: &str) -> bool {
        !self.hidden.iter().any(|h| h == item)
    }

    /// The name shown for it (a built-in's translated).
    pub fn label(&self) -> String {
        if self.builtin() { crate::i18n::tr(&self.name).to_string() } else { self.name.clone() }
    }

    /// Unknown groups, sliders and options dropped, repeats removed, the name kept short.
    fn sanitize(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.groups_off.retain(|g| is_group(g) && seen.insert(g.clone()));
        let mut seen = std::collections::BTreeSet::new();
        self.hidden.retain(|h| is_item(h) && seen.insert(h.clone()));
        let mut seen = std::collections::BTreeSet::new();
        self.custom.retain(|c| is_slider(c) && seen.insert(c.clone()));
        self.custom.truncate(MAX_CUSTOM);
        self.name = clean_name(&self.name).unwrap_or_else(|| "Profile".into());
    }
}

/// The built-in profiles: Default, then Witchcraft.
static BUILTINS: LazyLock<[ToolProfile; 2]> = LazyLock::new(|| {
    [
        ToolProfile { id: DEFAULT.into(), name: "Default".into(), ..Default::default() },
        ToolProfile { id: WITCHCRAFT.into(), name: "Witchcraft".into(), groups_off: vec!["grading".into()], witchcraft: true, ..Default::default() },
    ]
});

pub fn is_builtin(id: &str) -> bool {
    id == DEFAULT || id == WITCHCRAFT
}

/// A name as typed, trimmed and cut to [`MAX_NAME`] characters; `None` when nothing is left.
fn clean_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Every tool profile and the one in use (the app's settings, `toolProfiles`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProfiles {
    /// The profile in use (empty or unknown: Default).
    pub active: String,
    /// The user's own profiles, in the order they were made.
    pub user: Vec<ToolProfile>,
}

impl<'de> Deserialize<'de> for ToolProfiles {
    /// Read leniently: a profile that isn't one is skipped, unknown ids inside one are dropped, and
    /// an unknown profile in use falls back to Default — a settings file never fails over them.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(Self::from_value(&v))
    }
}

impl ToolProfiles {
    pub fn from_value(v: &Value) -> Self {
        let user = v.get("user").and_then(Value::as_array).map(|a| a.iter().filter_map(ToolProfile::from_value).collect()).unwrap_or_default();
        let active = v.get("active").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut out = ToolProfiles { active, user };
        out.sanitize();
        out
    }

    fn sanitize(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.user.retain(|p| !p.id.is_empty() && !is_builtin(&p.id) && seen.insert(p.id.clone()));
        self.user.truncate(MAX_PROFILES);
        for p in &mut self.user {
            p.sanitize();
        }
        if self.find(&self.active).is_none() {
            self.active = DEFAULT.into();
        }
    }

    /// The profile in use.
    pub fn active(&self) -> &ToolProfile {
        self.find(&self.active).unwrap_or(&BUILTINS[0])
    }

    pub fn find(&self, id: &str) -> Option<&ToolProfile> {
        BUILTINS.iter().find(|p| p.id == id).or_else(|| self.user.iter().find(|p| p.id == id))
    }

    fn find_user(&mut self, id: &str) -> Result<&mut ToolProfile, String> {
        if is_builtin(id) {
            return Err(format!("the built-in tool profile `{id}` can't be changed: duplicate it to make your own"));
        }
        self.user.iter_mut().find(|p| p.id == id).ok_or_else(|| format!("no tool profile `{id}`"))
    }

    /// Every profile, built-ins first.
    pub fn all(&self) -> impl Iterator<Item = &ToolProfile> {
        BUILTINS.iter().chain(self.user.iter())
    }

    /// An id no profile has yet.
    fn new_id(&self) -> String {
        (1..).map(|n| format!("user-{n}")).find(|id| self.find(id).is_none()).unwrap_or_else(|| "user".into())
    }

    /// `base`, or `base 2`, `base 3`… — the first name no profile has.
    fn free_name(&self, base: &str) -> String {
        let taken = |n: &str| self.all().any(|p| p.name.eq_ignore_ascii_case(n) || p.label().eq_ignore_ascii_case(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }

    /// A summary for the commands' results.
    pub fn to_json(&self) -> Value {
        let profiles: Vec<Value> = self
            .all()
            .map(|p| {
                json!({"id": p.id, "name": p.label(), "builtin": p.builtin(), "groupsOff": p.groups_off, "hidden": p.hidden,
                       "custom": p.custom, "witchcraft": p.witchcraft})
            })
            .collect();
        json!({"active": self.active().id, "profiles": profiles})
    }
}

/// Whether the Edit panel shows a group under the profile in use (`grading_witchcraft`: the grading mode is Witchcraft,
/// which brings the Primaries and HSL Curves).
pub fn group_shown(app: &LccApp, group: &str) -> bool {
    let p = app.ui.settings.tool_profiles.active();
    if matches!(group, "primaries" | "hsl") && !(app.ui.settings.grading_mode == crate::state::GradingMode::Witchcraft || p.witchcraft) {
        return false;
    }
    p.shows_group(group)
}

/// Whether the Witchcraft palettes take their place in the Edit panel (either one may still be switched off).
pub fn witchcraft_shown(app: &LccApp) -> bool {
    app.ui.settings.grading_mode == crate::state::GradingMode::Witchcraft || app.ui.settings.tool_profiles.active().witchcraft
}

/// Whether the Edit panel shows a slider or option under the profile in use.
pub fn item_shown(app: &LccApp, item: &str) -> bool {
    let p = app.ui.settings.tool_profiles.active();
    // a slider in the Custom group moved there: it isn't drawn twice
    p.shows_item(item) && !p.custom.iter().any(|c| c == item)
}

/// The tool profile commands: pick one (also the Edit panel's row), and change the user's own.
pub const COMMANDS: &[UiCommand] = &[
    ("view.toolProfile", "Tool Profile", None, ""),
    ("view.toolProfileEdit", "Edit Tool Profiles", None, ""),
    ("section.custom", "Custom", None, ""),
];

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "view.toolProfile" => select(app, p),
        "view.toolProfileEdit" => edit(app, p),
        "section.custom" => {
            app.ui.right = crate::state::RightPanel::Edit;
            if p.get("solo").and_then(Value::as_bool) == Some(true) {
                app.ui.solo_section(CUSTOM);
            } else {
                app.ui.toggle_section(CUSTOM);
            }
            Ok(Value::Null)
        }
        _ => return None,
    })
}

/// `view.toolProfile {id?}`: with an id, use that profile; the profiles and the one in use either way.
fn select(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    if let Some(v) = p.get("id").filter(|v| !v.is_null()) {
        let Some(id) = v.as_str() else { return Err("view.toolProfile: id must be a string".into()) };
        use_profile(app, id)?;
    }
    Ok(app.ui.settings.tool_profiles.to_json())
}

fn use_profile(app: &mut LccApp, id: &str) -> Result<(), String> {
    let profiles = &mut app.ui.settings.tool_profiles;
    let Some(profile) = profiles.find(id) else {
        let ids: Vec<String> = profiles.all().map(|p| p.id.clone()).collect();
        return Err(format!("view.toolProfile: no tool profile `{id}` ({})", ids.join("|")));
    };
    let custom = !profile.custom.is_empty();
    profiles.active = id.to_string();
    // a Custom group is the profile's reason to be: it opens with it
    if custom && !app.ui.section_open(CUSTOM) {
        app.ui.open_sections.insert(0, CUSTOM.to_string());
    }
    Ok(())
}

fn str_param<'a>(p: &'a Value, key: &str) -> Result<&'a str, String> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| format!("view.toolProfileEdit: `{key}` is required"))
}

fn bool_param(p: &Value, key: &str) -> Result<bool, String> {
    p.get(key).and_then(Value::as_bool).ok_or_else(|| format!("view.toolProfileEdit: `{key}` (true|false) is required"))
}

/// `view.toolProfileEdit {op, …}`: `new {name?}`, `duplicate {id?, name?}` (both make the new profile the one in
/// use), `rename {id, name}`, `delete {id}`, `group {id, group, on}`, `item {id, item, on}`, `custom {id, add | remove
/// | up | down | items}`, `witchcraft {id, on}`. A built-in profile is refused: duplicate it first. `id` defaults to
/// the profile in use.
fn edit(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let op = str_param(p, "op")?;
    let profiles = &mut app.ui.settings.tool_profiles;
    let target = p.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| profiles.active().id.clone());
    match op {
        "new" | "duplicate" => {
            if profiles.user.len() >= MAX_PROFILES {
                return Err(format!("view.toolProfileEdit: at most {MAX_PROFILES} tool profiles"));
            }
            let mut profile = if op == "new" {
                ToolProfile::default()
            } else {
                profiles.find(&target).cloned().ok_or_else(|| format!("view.toolProfileEdit: no tool profile `{target}`"))?
            };
            let base = match p.get("name").and_then(Value::as_str).and_then(clean_name) {
                Some(n) => n,
                None if op == "new" => crate::i18n::tr("My Tools").to_string(),
                // (a copy takes its source's name and the first free number: "Witchcraft 2")
                None => profile.label(),
            };
            profile.id = profiles.new_id();
            profile.name = clean_name(&profiles.free_name(&base)).unwrap_or(base);
            let id = profile.id.clone();
            profiles.user.push(profile);
            use_profile(app, &id)?;
        }
        "rename" => {
            let name = clean_name(str_param(p, "name")?).ok_or("view.toolProfileEdit: the name is empty")?;
            if profiles.all().any(|q| q.id != target && (q.name.eq_ignore_ascii_case(&name) || q.label().eq_ignore_ascii_case(&name))) {
                return Err(format!("view.toolProfileEdit: a tool profile is already called “{name}”"));
            }
            profiles.find_user(&target)?.name = name;
        }
        "delete" => {
            profiles.find_user(&target)?;
            profiles.user.retain(|q| q.id != target);
            if profiles.active == target {
                profiles.active = DEFAULT.into();
            }
        }
        "group" => {
            let group = str_param(p, "group")?;
            if !is_group(group) {
                let all: Vec<&str> = GROUPS.iter().map(|(g, _)| *g).collect();
                return Err(format!("view.toolProfileEdit: no group `{group}` ({})", all.join("|")));
            }
            let on = bool_param(p, "on")?;
            let profile = profiles.find_user(&target)?;
            profile.groups_off.retain(|g| g != group);
            if !on {
                profile.groups_off.push(group.to_string());
            }
        }
        "item" => {
            let item = str_param(p, "item")?;
            if !is_item(item) {
                return Err(format!("view.toolProfileEdit: no slider or option `{item}`"));
            }
            let on = bool_param(p, "on")?;
            let profile = profiles.find_user(&target)?;
            profile.hidden.retain(|h| h != item);
            if !on {
                profile.hidden.push(item.to_string());
            }
        }
        "custom" => {
            let profile = profiles.find_user(&target)?;
            let slider = |key: &str| -> Result<Option<String>, String> {
                match p.get(key).and_then(Value::as_str) {
                    None => Ok(None),
                    Some(s) if is_slider(s) => Ok(Some(s.to_string())),
                    Some(s) => Err(format!("view.toolProfileEdit: no slider `{s}`")),
                }
            };
            if let Some(items) = p.get("items") {
                let Some(items) = items.as_array() else { return Err("view.toolProfileEdit: items must be a list of slider ids".into()) };
                let mut list = Vec::new();
                for i in items {
                    match i.as_str() {
                        Some(s) if is_slider(s) => {
                            if !list.iter().any(|x: &String| x == s) {
                                list.push(s.to_string());
                            }
                        }
                        _ => return Err(format!("view.toolProfileEdit: no slider `{i}`")),
                    }
                }
                list.truncate(MAX_CUSTOM);
                profile.custom = list;
            } else if let Some(s) = slider("add")? {
                if profile.custom.len() >= MAX_CUSTOM {
                    return Err(format!("view.toolProfileEdit: at most {MAX_CUSTOM} sliders in the Custom group"));
                }
                if !profile.custom.contains(&s) {
                    profile.custom.push(s);
                }
            } else if let Some(s) = slider("remove")? {
                profile.custom.retain(|c| *c != s);
            } else if let Some((s, up)) = slider("up")?.map(|s| (s, true)).or(slider("down")?.map(|s| (s, false))) {
                let Some(i) = profile.custom.iter().position(|c| *c == s) else {
                    return Err(format!("view.toolProfileEdit: `{s}` isn't in the Custom group"));
                };
                let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < profile.custom.len()) };
                if let Some(j) = j {
                    profile.custom.swap(i, j);
                }
            } else {
                return Err("view.toolProfileEdit: custom takes add, remove, up, down or items".into());
            }
            let custom = !profile.custom.is_empty();
            if custom && profiles.active == target && !app.ui.section_open(CUSTOM) {
                app.ui.open_sections.insert(0, CUSTOM.to_string());
            }
        }
        "witchcraft" => {
            let on = bool_param(p, "on")?;
            profiles.find_user(&target)?.witchcraft = on;
        }
        other => return Err(format!("view.toolProfileEdit: unknown op `{other}` (new|duplicate|rename|delete|group|item|custom|witchcraft)")),
    }
    Ok(app.ui.settings.tool_profiles.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_profiles_drop_what_this_build_does_not_know() {
        let v = json!({
            "active": "user-1",
            "user": [
                {"id": "user-1", "name": "  Mine  ", "groupsOff": ["grading", "nope", "grading"], "hidden": ["light.exposure", "gone.slider", "option:treatment", "option:nope"],
                 "custom": ["light.contrast", "removed.slider", "light.contrast", 7]},
                {"id": "default", "name": "Imposter"},
                {"id": "user-1", "name": "Twin"},
                "not a profile",
                {"id": "", "name": "No id"}
            ]
        });
        let p = ToolProfiles::from_value(&v);
        assert_eq!(p.user.len(), 1, "{p:?}");
        let mine = &p.user[0];
        assert_eq!(mine.name, "Mine");
        assert_eq!(mine.groups_off, ["grading"]);
        assert_eq!(mine.hidden, ["light.exposure", "option:treatment"]);
        assert_eq!(mine.custom, ["light.contrast"]);
        assert_eq!(p.active, "user-1");
        let p = ToolProfiles::from_value(&json!({"active": "missing", "user": 42}));
        assert_eq!(p.active().id, DEFAULT);
        let p = ToolProfiles::from_value(&json!("garbage"));
        assert_eq!(p.active().id, DEFAULT);
        let p: ToolProfiles = serde_json::from_str(r#"{"active":"witchcraft","user":[{"id":"u","custom":["light.exposure","x.y"]}]}"#).unwrap();
        assert_eq!(p.active().id, WITCHCRAFT);
        assert_eq!(p.user[0].custom, ["light.exposure"]);
    }

    #[test]
    fn every_group_has_its_sliders_and_every_option_a_group() {
        for (g, _) in GROUPS {
            if matches!(*g, "hsl" | "pluginFilters") {
                continue;
            }
            assert!(sliders_of(g).next().is_some() || options_of(g).next().is_some(), "{g} has nothing to hide");
        }
        for (_, g, _) in OPTIONS {
            assert!(is_group(g), "{g}");
        }
    }
}
