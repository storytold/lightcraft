//! Keymap profiles: which set of default shortcuts the app uses.
//!
//! `Lightroom` (the default) is the keymap LightCraft has always shipped — the Lightroom-desktop
//! keys with Classic additions. `Classic` rebinds the same commands to Lightroom Classic's
//! defaults (Adobe's public shortcut list), so Classic muscle memory works without giving up the
//! desktop keys: switch profiles in Help ▸ Keyboard Shortcuts. The user's own rebindings
//! (`AppSettings::keymap`) sit above whichever profile is active. The full mapping, the
//! deliberate deviations and the Classic keys LightCraft has no command for live in
//! `docs/classic-keymap.md`.

pub const LIGHTROOM: &str = "lightroom";
pub const CLASSIC: &str = "classic";

/// A set of default shortcuts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyProfile {
    /// The shipped keymap (Lightroom-desktop keys with Classic additions).
    #[default]
    Lightroom,
    /// Lightroom Classic's defaults.
    Classic,
}

impl KeyProfile {
    pub const ALL: [KeyProfile; 2] = [KeyProfile::Lightroom, KeyProfile::Classic];

    pub const fn id(self) -> &'static str {
        match self {
            KeyProfile::Lightroom => LIGHTROOM,
            KeyProfile::Classic => CLASSIC,
        }
    }

    /// A saved `ui.json` value: an unknown name is the default keymap, never an error.
    pub fn parse_id(s: &str) -> KeyProfile {
        if s == CLASSIC { KeyProfile::Classic } else { KeyProfile::Lightroom }
    }

    pub const fn label(self) -> &'static str {
        match self {
            KeyProfile::Lightroom => "Lightroom",
            KeyProfile::Classic => "Lightroom Classic",
        }
    }
}

/// The Classic profile's defaults: `(command id, shortcut)`; `""` removes the declared one.
/// Sourced row by row from Adobe's public "Keyboard shortcuts for Lightroom Classic" (Library
/// and Develop tables); `docs/classic-keymap.md` maps each entry back to its table row and lists
/// what is deliberately not rebound.
pub const CLASSIC_OVERRIDES: &[(&str, &str)] = &[
    // Views: E loupe, D develop, C compare, L lights out, I info overlay, O people (in grids)
    ("view.detail", "E"),
    ("panel.edit", "D"),
    ("view.compare", "C"),
    ("view.lightsOut", "L"),
    ("view.infoOverlay", "I"),
    ("panel.info", ""),
    ("view.people", "O"),
    // Library, export and versions (New Album, Export with Previous and New Folder already
    // declare Classic's key)
    ("library.selectNone", "Cmd+D"),
    ("panel.presets", ""),
    ("dialog.export", "Cmd+Shift+E"),
    ("photo.editInPhotoCraft", "Cmd+E"),
    ("version.create", "Cmd+N"),
    ("dialog.createPreset", "Cmd+Shift+N"),
    // Develop settings (Paste from Previous and Auto Sync already declare Classic's key)
    ("develop.sync", "Cmd+Shift+S"),
    ("develop.auto", "Cmd+U"),
    ("develop.matchExposure", "Cmd+Alt+Shift+M"),
    // Develop tools: R crop, Q spots, K brush, M graduated, ⇧M radial, ⇧W masking, ⇧T upright
    // (K is Classic's Adjustment Brush; Classic gives its Keywords panel no key of its own)
    ("panel.keywords", ""),
    ("panel.crop", "R"),
    ("panel.remove", "Q"),
    ("tool.brush", "K"),
    ("tool.linear", "M"),
    ("tool.radial", "Shift+M"),
    ("tool.guidedUpright", "Shift+T"),
    ("panel.masking", "Shift+W"),
    ("view.toolOverlay", "H"),
    ("view.visualizeHdr", "Shift+H"),
    ("view.maskOverlayMode", "Alt+O"),
];

/// Secondary keys only the Classic profile adds: `(shortcut, command id, params)`.
pub const CLASSIC_ALIASES: &[(&str, &str, &str)] = &[
    // Classic's ⇧P flags and advances everywhere, not only in the grids
    ("Shift+P", "photo.flag", r#"{"flag": "pick", "advance": true}"#),
    // I cycles the info overlay; ⌘I (Ctrl+I on Windows) shows / hides it
    ("Cmd+I", "view.infoOverlay", "{}"),
    // Windows' second redo key
    ("Ctrl+Y", "edit.redo", "{}"),
];

/// The shortcut command `id` has under `profile` when the user hasn't changed it: the Classic
/// table first (`""` in it means none), else the declared default.
pub fn profile_default(profile: KeyProfile, id: &str, declared: Option<&'static str>) -> Option<&'static str> {
    if profile == KeyProfile::Classic {
        for (oid, sc) in CLASSIC_OVERRIDES {
            if *oid == id {
                return if sc.is_empty() { None } else { Some(sc) };
            }
        }
    }
    declared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::parse;

    #[test]
    fn unknown_profile_ids_are_the_default_keymap() {
        assert_eq!(KeyProfile::parse_id(""), KeyProfile::Lightroom);
        assert_eq!(KeyProfile::parse_id("what"), KeyProfile::Lightroom);
        assert_eq!(KeyProfile::parse_id(LIGHTROOM), KeyProfile::Lightroom);
        assert_eq!(KeyProfile::parse_id(CLASSIC), KeyProfile::Classic);
    }

    /// Every Classic override names a real command with a shortcut that parses.
    #[test]
    fn classic_overrides_are_real_commands_with_parsable_keys() {
        let ids: Vec<&str> = crate::shortcuts::bindable().iter().map(|b| b.id).collect();
        for (id, sc) in CLASSIC_OVERRIDES {
            assert!(ids.contains(id), "unknown command {id}");
            if !sc.is_empty() {
                assert!(parse(sc).is_some(), "{id}: {sc}");
            }
        }
        for (sc, id, params) in CLASSIC_ALIASES {
            assert!(ids.contains(id), "unknown command {id}");
            assert!(parse(sc).is_some(), "{id}: {sc}");
            assert!(serde_json::from_str::<serde_json::Value>(params).is_ok(), "alias {sc}: bad params");
        }
    }

    /// A Classic default replaces the declared one; `""` in the table removes it explicitly.
    #[test]
    fn profile_default_falls_through_and_removes() {
        assert_eq!(profile_default(KeyProfile::Lightroom, "panel.crop", Some("C")), Some("C"));
        assert_eq!(profile_default(KeyProfile::Classic, "panel.crop", Some("C")), Some("R"));
        assert_eq!(profile_default(KeyProfile::Classic, "panel.info", Some("I")), None);
        assert_eq!(profile_default(KeyProfile::Classic, "view.photoGrid", None), None);
    }

    /// No row repeats what the shipped profile already resolves to (a default declared twice),
    /// and `""` actually removes a key the shipped profile has.
    #[test]
    fn no_override_duplicates_the_shipped_default() {
        let mut wrong: Vec<String> = Vec::new();
        for (id, sc) in CLASSIC_OVERRIDES {
            let shipped = profile_default(KeyProfile::Lightroom, id, crate::shortcuts::find_bindable(id).expect("checked above").default);
            if sc.is_empty() {
                if shipped.is_none() {
                    wrong.push(format!("{id}: removes a key the shipped profile has not got"));
                }
            } else if shipped == Some(*sc) {
                wrong.push(format!("{id}: `{sc}` is already the shipped default"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("; "));
    }
}
