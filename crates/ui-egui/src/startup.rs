//! Startup dialog selection for first-run onboarding and What's New.
//!
//! On first launch (`seen == None`) show `FirstRun`. After an update (the stored previous
//! version differs from the running version) show `WhatsNew`. Both are skipped when the
//! version was already seen.
//!
//! `UiState::record_startup_dialog_seen` uses record-on-show semantics (documented there): the
//! version is recorded when the dialog is presented in a UI pass, not when the logic tick
//! picks it.
//!
//! A settings file from before this existed migrates to [`LEGACY_SEEN_VERSION`], so an
//! existing installation is due What's New, never the first-run tour.

use crate::state::Dialog;

/// What to show at startup before the main UI takes over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDialog {
    /// First launch ever: show the onboarding walkthrough.
    FirstRun,
    /// The app was updated since the last launch: show What's New.
    WhatsNew,
}

/// Parse "1.2.3" / "1.2.3-rc.1" / "0.8.0" into (major, minor, patch), ignoring any
/// prerelease suffix. Returns `None` when not parseable.
pub fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split('-').next()?.trim();
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts[1].parse::<u64>().ok()?;
    let patch = parts[2].parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// The version a settings file from before the startup dialog existed migrates to: the file's
/// existence means an installation that has been run, so it is due What's New, never the
/// first-run tour. `None` stays reserved for no settings at all.
pub const LEGACY_SEEN_VERSION: &str = "0.0.0";

/// The legacy `seenVersion` migration both settings loaders share — the desktop settings file
/// (`lightcraft`'s `prefs`) and the browser's stored `ui.json` — applied to the parsed JSON
/// object before it deserializes into [`crate::state::UiState`]. A saved file from before the
/// startup dialog existed has no `seenVersion` key, and the file's existence means an
/// installation that has been run, due What's New — never the first-run tour. A file this
/// version saved before the dialog was presented carries an explicit `null` instead (that is
/// why `None` serializes) and is not migrated, and no file at all (no object) stays a first
/// run.
pub fn migrate_legacy_seen_version(fields: &mut serde_json::Map<String, serde_json::Value>) {
    if !fields.contains_key("seenVersion") {
        fields.insert("seenVersion".into(), serde_json::json!(LEGACY_SEEN_VERSION));
    }
}

/// The dialog to show for `seen` (the stored last-seen version, `None` = first run) against
/// `current` (the running version).
///
/// `FirstRun` only when `seen == None`. `WhatsNew` when the version changed. `None` when
/// identical.
///
/// Comparison goes through [`parse_version`]. When both sides parse, any numeric difference
/// (and a prerelease-vs-release string difference with the same numbers) is a change →
/// `WhatsNew`. When either side is unparseable, fall back to the string comparison: show
/// `WhatsNew` when the strings differ, `None` when identical — never `FirstRun` once
/// something was seen.
pub fn startup_dialog(seen: Option<&str>, current: &str) -> Option<StartupDialog> {
    let Some(seen) = seen else {
        return Some(StartupDialog::FirstRun);
    };
    if seen == current {
        return None;
    }
    // `seen` differs from `current`. `parse_version` confirms a real version-like change
    // when both sides parse; when either side is unparseable we fall back to the string
    // difference already established and show WhatsNew conservatively.
    match (parse_version(seen), parse_version(current)) {
        (Some(sv), Some(cv)) if sv != cv => Some(StartupDialog::WhatsNew),
        _ => Some(StartupDialog::WhatsNew),
    }
}

/// The dialog the first frame opens for `seen` (the stored last-seen version): [`Dialog::Welcome`]
/// on the first run ever, [`Dialog::WhatsNew`] after an update, none once the running version was
/// seen. A headless host (snapshots, scripted runs, tests: their UI state is session-local, with
/// no stored version behind it) never gains a modal.
pub fn open_at_startup(seen: Option<&str>, headless: bool) -> Option<Dialog> {
    if headless {
        return None;
    }
    match startup_dialog(seen, env!("CARGO_PKG_VERSION")) {
        Some(StartupDialog::FirstRun) => Some(Dialog::Welcome { page: 0 }),
        Some(StartupDialog::WhatsNew) => Some(Dialog::WhatsNew),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_plain() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn parse_version_prerelease() {
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.8.0-beta.2"), Some((0, 8, 0)));
    }

    #[test]
    fn parse_version_garbage() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("abc.2.3"), None);
    }

    #[test]
    fn startup_dialog_first_run() {
        assert_eq!(startup_dialog(None, "0.8.0"), Some(StartupDialog::FirstRun));
        assert_eq!(startup_dialog(None, "1.0.0"), Some(StartupDialog::FirstRun));
    }

    #[test]
    fn startup_dialog_equal() {
        assert_eq!(startup_dialog(Some("0.8.0"), "0.8.0"), None);
        assert_eq!(startup_dialog(Some("1.2.3"), "1.2.3"), None);
    }

    #[test]
    fn startup_dialog_version_changed() {
        assert_eq!(startup_dialog(Some("0.7.9"), "0.8.0"), Some(StartupDialog::WhatsNew));
    }

    #[test]
    fn startup_dialog_prerelease_vs_release() {
        assert_eq!(startup_dialog(Some("1.2.3-rc.1"), "1.2.3"), Some(StartupDialog::WhatsNew));
        assert_eq!(startup_dialog(Some("1.2.3"), "1.2.3-rc.1"), Some(StartupDialog::WhatsNew));
    }

    #[test]
    fn startup_dialog_unparseable_differing() {
        assert_eq!(startup_dialog(Some("garbage"), "0.8.0"), Some(StartupDialog::WhatsNew));
        assert_eq!(startup_dialog(Some("0.8.0"), "garbage"), Some(StartupDialog::WhatsNew));
    }

    #[test]
    fn startup_dialog_unparseable_identical() {
        assert_eq!(startup_dialog(Some("garbage"), "garbage"), None);
    }

    #[test]
    fn ui_state_serde_round_trip_with_seen_version() {
        let state = crate::state::UiState { seen_version: Some("0.8.0".to_string()), ..Default::default() };
        let json = serde_json::to_string(&state).expect("serialize");
        let back: crate::state::UiState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.seen_version, Some("0.8.0".to_string()));
    }

    #[test]
    fn ui_state_serde_serializes_an_unset_marker_as_null() {
        // A file this version saved before the startup dialog was presented carries an explicit
        // `null`, so the desktop loader can tell it apart from a pre-feature file (which lacks
        // the key and migrates): null stays `None` — the tour, not What's New.
        let state = crate::state::UiState::default();
        let json = serde_json::to_string(&state).expect("serialize");
        assert!(json.contains(r#""seenVersion":null"#), "{json}");
        let back: crate::state::UiState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.seen_version, None);
    }

    #[test]
    fn first_frame_opens_the_tour_or_whats_new_but_never_headless() {
        assert!(matches!(super::open_at_startup(None, false), Some(crate::state::Dialog::Welcome { page: 0 })));
        assert!(matches!(super::open_at_startup(Some("0.0.1"), false), Some(crate::state::Dialog::WhatsNew)));
        assert_eq!(super::open_at_startup(Some(env!("CARGO_PKG_VERSION")), false), None);
        // session-local state: nothing stored behind it, so no modal
        assert_eq!(super::open_at_startup(None, true), None);
    }

    #[test]
    fn ui_state_serde_older_json_without_field() {
        // Older ui.json without seen_version deserializes fine (None = first run).
        let json = r#"{"language":"en","view":"detail","left_panel":false}"#;
        let back: crate::state::UiState = serde_json::from_str(json).expect("deserialize");
        assert_eq!(back.seen_version, None);
    }

    /// A stored UiState serialized like both loaders receive it, with `seenVersion` rewritten.
    fn saved_ui(seen_version: serde_json::Value) -> serde_json::Value {
        let mut saved = serde_json::to_value(crate::state::UiState::default()).expect("serialize");
        saved.as_object_mut().expect("object").insert("seenVersion".into(), seen_version);
        saved
    }

    /// A settings file from before the startup dialog existed has no `seenVersion` key: the
    /// shared migration the desktop settings loader and the browser loader both run marks the
    /// installation as run (`LEGACY_SEEN_VERSION`), so it is due What's New, never the tour.
    #[test]
    fn legacy_settings_without_a_seen_version_get_whats_new() {
        let mut saved = saved_ui(serde_json::Value::Null);
        saved.as_object_mut().unwrap().remove("seenVersion");
        // without the migration it deserializes to a first run — exactly what the migration
        // exists to prevent for an existing installation
        let unmigrated: crate::state::UiState = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(unmigrated.seen_version, None);
        super::migrate_legacy_seen_version(saved.as_object_mut().unwrap());
        let ui: crate::state::UiState = serde_json::from_value(saved).unwrap();
        assert_eq!(ui.seen_version.as_deref(), Some(LEGACY_SEEN_VERSION));
        assert_eq!(startup_dialog(ui.seen_version.as_deref(), env!("CARGO_PKG_VERSION")), Some(StartupDialog::WhatsNew));
    }

    /// A file this version saved before the dialog was presented carries an explicit `null`
    /// (that is why `None` serializes): it is not migrated, so the tour still runs.
    #[test]
    fn an_explicit_null_seen_version_stays_a_first_run() {
        let mut saved = saved_ui(serde_json::Value::Null);
        super::migrate_legacy_seen_version(saved.as_object_mut().unwrap());
        assert_eq!(saved["seenVersion"], serde_json::Value::Null);
        let ui: crate::state::UiState = serde_json::from_value(saved).unwrap();
        assert_eq!(ui.seen_version, None);
        assert_eq!(startup_dialog(ui.seen_version.as_deref(), env!("CARGO_PKG_VERSION")), Some(StartupDialog::FirstRun));
    }

    /// A version already seen is left exactly as stored: no What's New, no tour.
    #[test]
    fn a_previously_seen_version_is_not_migrated() {
        let mut saved = saved_ui(serde_json::json!("0.14.0"));
        super::migrate_legacy_seen_version(saved.as_object_mut().unwrap());
        let ui: crate::state::UiState = serde_json::from_value(saved).unwrap();
        assert_eq!(ui.seen_version.as_deref(), Some("0.14.0"));
        assert_eq!(startup_dialog(Some("0.14.0"), "0.15.0"), Some(StartupDialog::WhatsNew));
    }
}
