//! What a control does.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::devices::PageGroup;

/// A modifier held on a key or button while turning a dial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Modifier {
    /// Smaller steps (precise work).
    Fine,
    /// Bigger steps (sweeping changes).
    Coarse,
}

/// The action bound to a control. Stored in profiles (`control-surfaces.json`) and sent to
/// surfaces as JSON: `{"kind": "value", "target": "light.exposure"}`,
/// `{"kind": "command", "command": "photo.rate", "params": {"rating": 5}}`, …
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Binding {
    /// Turning moves this value (a dial); pressing a key bound to it makes the dials move it
    /// until pressed again (TactileBridge's adjustment dial toggle).
    Value { target: String },
    /// Run a LightCraft command (any engine or UI command id; `engine.commands` lists them).
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        params: Value,
    },
    /// Hold for smaller or bigger dial steps.
    Modifier { modifier: Modifier },
    /// Next / previous page of a group (both groups when `group` is missing).
    PageNext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<PageGroup>,
    },
    PagePrevious {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<PageGroup>,
    },
    /// Show a page by name.
    Page { page: String },
    /// Switch to a profile by name.
    Profile { profile: String },
    /// The next profile in the list.
    ProfileNext,
    /// Put the value the dial moves back to its default (a dial press).
    Reset,
    /// Dials change every selected photo, not just the active one (TactileBridge's Auto Sync;
    /// toggles).
    ApplyToSelected,
    /// Pages follow what LightCraft shows (toggles automatic switching).
    FollowContext,
}

impl Binding {
    pub fn value(target: &str) -> Binding {
        Binding::Value { target: target.to_string() }
    }

    pub fn command(command: &str) -> Binding {
        Binding::Command { command: command.to_string(), params: Value::Null }
    }

    pub fn command_with(command: &str, params: Value) -> Binding {
        Binding::Command { command: command.to_string(), params }
    }

    /// The value this binding moves or focuses, if any.
    pub fn target(&self) -> Option<&str> {
        match self {
            Binding::Value { target } => Some(target),
            _ => None,
        }
    }

    /// A short English name for the binding (editor lists, device labels when no better one is
    /// known).
    pub fn describe(&self) -> String {
        match self {
            Binding::Value { target } => crate::targets::target_spec(target).map_or_else(|| target.clone(), |t| t.label.to_string()),
            Binding::Command { command, params } => crate::icons::command_label(command, params),
            Binding::Modifier { modifier: Modifier::Fine } => "Fine".into(),
            Binding::Modifier { modifier: Modifier::Coarse } => "Coarse".into(),
            Binding::PageNext { group: Some(PageGroup::Keys) } => "Next Key Page".into(),
            Binding::PageNext { group: Some(PageGroup::Dials) } => "Next Dial Page".into(),
            Binding::PageNext { group: None } => "Next Page".into(),
            Binding::PagePrevious { group: Some(PageGroup::Keys) } => "Previous Key Page".into(),
            Binding::PagePrevious { group: Some(PageGroup::Dials) } => "Previous Dial Page".into(),
            Binding::PagePrevious { group: None } => "Previous Page".into(),
            Binding::Page { page } => page.clone(),
            Binding::Profile { profile } => profile.clone(),
            Binding::ProfileNext => "Next Profile".into(),
            Binding::Reset => "Reset Dial Value".into(),
            Binding::ApplyToSelected => "Apply to Selected".into(),
            Binding::FollowContext => "Follow Context".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bindings_round_trip_as_tagged_json() {
        let all = vec![
            Binding::value("light.exposure"),
            Binding::command_with("photo.rate", json!({"rating": 5})),
            Binding::command("edit.undo"),
            Binding::Modifier { modifier: Modifier::Fine },
            Binding::PageNext { group: Some(PageGroup::Dials) },
            Binding::PagePrevious { group: None },
            Binding::Page { page: "Cull".into() },
            Binding::Profile { profile: "Wedding".into() },
            Binding::ProfileNext,
            Binding::Reset,
            Binding::ApplyToSelected,
            Binding::FollowContext,
        ];
        for b in all {
            let v = serde_json::to_value(&b).unwrap();
            assert!(v["kind"].is_string(), "{v}");
            assert_eq!(serde_json::from_value::<Binding>(v).unwrap(), b);
        }
        assert_eq!(serde_json::to_value(Binding::command("edit.undo")).unwrap(), json!({"kind": "command", "command": "edit.undo"}));
        assert!(serde_json::from_value::<Binding>(json!({"kind": "nope"})).is_err());
    }
}
