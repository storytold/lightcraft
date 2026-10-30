//! Every action a control can be given, in categories: the editor's action picker and the
//! actions a surface plugin lists in its own app (the Logi Options+ actions panel), so a user can
//! put LightCraft actions on keys and dials there too.
//!
//! Each entry has a stable `key` (`v.light.exposure`, `c.photo.rate.rating-5`, `s.applyToSelected`)
//! that plugins store in their own profiles; it is made from the binding alone, so it stays the
//! same between LightCraft versions as long as the binding does.

use serde::Serialize;
use serde_json::Value;

use crate::binding::{Binding, Modifier};
use crate::devices::PageGroup;
use crate::targets::{TargetKind, all_targets};

/// One action.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Stable id (see the module docs).
    pub key: String,
    /// The category (English; hosts translate it).
    pub group: &'static str,
    /// The action's name (English; hosts translate it or use a better label of their own).
    pub label: String,
    /// The icon (`surface.icons` returns its SVG).
    pub icon: Option<&'static str>,
    /// A value (turned on a dial) rather than something a key does once.
    pub value: bool,
    pub binding: Binding,
}

/// Categories of key actions, in the order lists show them.
pub const COMMAND_GROUPS: &[&str] = &["Rating & Labels", "Browse & Select", "Edit", "Tools", "Masking", "View", "Photo", "Control Surface", "Other"];

/// The category of a key action.
pub fn command_group(b: &Binding) -> &'static str {
    let Binding::Command { command, .. } = b else { return "Control Surface" };
    let head = command.split('.').next().unwrap_or("");
    match command.as_str() {
        "photo.rate" | "photo.label" | "photo.flag" | "photo.pick" | "photo.reject" | "photo.unflag" => "Rating & Labels",
        "photo.rotateLeft" | "photo.rotateRight" => "Photo",
        "surface.maskPrevious" | "surface.maskNext" => "Masking",
        "surface.dialogConfirm" | "surface.dialogCancel" => "Other",
        _ => match head {
            "library" | "select" | "selection" => "Browse & Select",
            "edit" | "develop" | "settings" | "preset" => "Edit",
            "tool" | "crop" | "spot" | "remove" | "brush" => "Tools",
            "mask" => "Masking",
            "view" | "zoom" => "View",
            "photo" | "metadata" => "Photo",
            "surface" => "Control Surface",
            _ => "Other",
        },
    }
}

/// A short, safe slug of a JSON value for keys (`5`, `red`, `none`, `true`).
fn slug(v: &Value) -> String {
    let raw = match v {
        Value::Null => "none".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(32).collect()
}

/// The stable key of a binding.
pub fn key_of(b: &Binding) -> String {
    match b {
        Binding::Value { target } => format!("v.{target}"),
        Binding::Command { command, params } => {
            let mut k = format!("c.{command}");
            if let Value::Object(m) = params {
                let mut fields: Vec<_> = m.iter().collect();
                fields.sort_by(|a, b| a.0.cmp(b.0));
                for (name, v) in fields {
                    k.push('.');
                    k.extend(name.chars().filter(char::is_ascii_alphanumeric));
                    k.push('-');
                    k.push_str(&slug(v));
                }
            }
            k
        }
        Binding::Modifier { modifier: Modifier::Fine } => "s.fine".into(),
        Binding::Modifier { modifier: Modifier::Coarse } => "s.coarse".into(),
        Binding::PageNext { group } => format!("s.pageNext{}", group_suffix(*group)),
        Binding::PagePrevious { group } => format!("s.pagePrevious{}", group_suffix(*group)),
        Binding::Page { page } => format!("s.page.{}", slug(&Value::String(page.clone()))),
        Binding::Profile { profile } => format!("s.profile.{}", slug(&Value::String(profile.clone()))),
        Binding::ProfileNext => "s.profileNext".into(),
        Binding::Reset => "s.reset".into(),
        Binding::ApplyToSelected => "s.applyToSelected".into(),
        Binding::FollowContext => "s.followContext".into(),
    }
}

fn group_suffix(g: Option<PageGroup>) -> &'static str {
    match g {
        Some(PageGroup::Keys) => ".keys",
        Some(PageGroup::Dials) => ".dials",
        None => "",
    }
}

/// The surface's own actions (pages, modifiers, toggles).
fn surface_actions() -> Vec<Binding> {
    vec![
        Binding::PageNext { group: Some(PageGroup::Keys) },
        Binding::PagePrevious { group: Some(PageGroup::Keys) },
        Binding::PageNext { group: Some(PageGroup::Dials) },
        Binding::PagePrevious { group: Some(PageGroup::Dials) },
        Binding::Modifier { modifier: Modifier::Fine },
        Binding::Modifier { modifier: Modifier::Coarse },
        Binding::Reset,
        Binding::ApplyToSelected,
        Binding::FollowContext,
        Binding::ProfileNext,
    ]
}

/// Every action: the dial values (develop panels in order, then masks, brush, Remove and view),
/// then the key actions by category. Keys are unique.
pub fn entries() -> Vec<Entry> {
    let mut out: Vec<Entry> = all_targets()
        .into_iter()
        .map(|t| {
            let b = Binding::value(&t.id);
            Entry {
                key: key_of(&b),
                group: match t.kind {
                    TargetKind::Develop => t.group,
                    TargetKind::Mask => "Mask",
                    TargetKind::Ui => t.group,
                },
                label: t.label.to_string(),
                icon: crate::icons::icon_for(&b),
                value: true,
                binding: b,
            }
        })
        .collect();
    // key actions: TactileBridge's list as LightCraft maps it, then what the built-in profiles use
    let mut commands: Vec<Binding> = crate::tactilebridge::COMMANDS.iter().filter_map(|k| crate::tactilebridge::binding_of(k)).collect();
    for p in crate::defaults::profiles() {
        commands.extend(p.base.values().cloned());
        for pg in &p.pages {
            commands.extend(pg.bindings.values().cloned());
        }
    }
    commands.retain(|b| matches!(b, Binding::Command { .. }));
    commands.extend(surface_actions());
    let mut seen = std::collections::BTreeSet::new();
    for o in &out {
        seen.insert(o.key.clone());
    }
    let mut keyed: Vec<Entry> = Vec::new();
    for b in commands {
        let key = key_of(&b);
        if !seen.insert(key.clone()) {
            continue;
        }
        keyed.push(Entry { key, group: command_group(&b), label: b.describe(), icon: crate::icons::icon_for(&b), value: false, binding: b });
    }
    keyed.sort_by_key(|e| COMMAND_GROUPS.iter().position(|g| *g == e.group).unwrap_or(COMMAND_GROUPS.len()));
    out.extend(keyed);
    out
}

/// [`entries`], built once.
pub fn cached() -> &'static [Entry] {
    static ALL: std::sync::OnceLock<Vec<Entry>> = std::sync::OnceLock::new();
    ALL.get_or_init(entries)
}

/// The entry with this key.
pub fn find(key: &str) -> Option<Entry> {
    cached().iter().find(|e| e.key == key).cloned()
}

/// The entry of a binding, if the catalogue has it.
pub fn of(b: &Binding) -> Option<&'static Entry> {
    let key = key_of(b);
    cached().iter().find(|e| e.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_unique_stable_and_safe() {
        let all = entries();
        assert!(all.len() > 150, "{}", all.len());
        let mut keys: Vec<_> = all.iter().map(|e| e.key.as_str()).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "keys are unique");
        for k in &keys {
            assert!(!k.contains("___"), "{k}: Logi profiles split on ___");
            assert!(k.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c)), "{k}");
        }
        assert_eq!(key_of(&Binding::value("light.exposure")), "v.light.exposure");
        assert_eq!(key_of(&Binding::command_with("photo.rate", json!({"rating": 5}))), "c.photo.rate.rating-5");
        assert_eq!(key_of(&Binding::command_with("library.filter", json!({"label": null}))), "c.library.filter.label-none");
        assert_eq!(find("v.light.exposure").map(|e| e.value), Some(true));
        assert_eq!(find("c.edit.undo").map(|e| e.group), Some("Edit"));
        assert_eq!(find("s.applyToSelected").map(|e| e.group), Some("Control Surface"));
        assert!(find("nope").is_none());
    }

    #[test]
    fn most_actions_have_icons() {
        let all = entries();
        let with = all.iter().filter(|e| e.icon.is_some()).count();
        assert!(with * 2 > all.len(), "{with} of {}", all.len());
        // every group of key actions is a known category
        for e in all.iter().filter(|e| !e.value) {
            assert!(COMMAND_GROUPS.contains(&e.group), "{}", e.group);
        }
    }
}
