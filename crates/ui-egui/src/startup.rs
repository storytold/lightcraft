//! Startup dialog selection for first-run onboarding and What's New.
//!
//! On first launch (`seen == None`) show `FirstRun`. After an update (the stored previous
//! version differs from the running version) show `WhatsNew`. Both are skipped when the
//! version was already seen.
//!
//! `UiState::record_startup_dialog_seen` uses record-on-show semantics (documented there).

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
    fn ui_state_serde_round_trip_without_seen_version() {
        // New settings serialize without the field (None is skipped).
        let state = crate::state::UiState::default();
        let json = serde_json::to_string(&state).expect("serialize");
        assert!(!json.contains("seen_version"));
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
}
